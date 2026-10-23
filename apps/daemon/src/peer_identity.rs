//! Protected native device credentials and exact-peer mutual TLS configuration.
//! No listener or automatic trust bootstrap. Callers hold the profile lease and
//! approve fingerprints through a trusted native surface before using a pin.
use okytes_core::peer::Hash;
use rcgen::{CertificateParams, KeyPair, PKCS_ED25519};
use rustls::{
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    ClientConfig, RootCertStore, ServerConfig,
};
use std::{io, path::Path, sync::Arc};
mod pin;

/// TLS hostname used by the peer protocol, independent of socket addressing.
pub const SERVER_NAME: &str = "okytes-peer.invalid";
/// Required application protocol for authenticated peer sessions.
pub const ALPN: &[u8] = b"okytes-peer/1";

fn invalid() -> io::Error {
    io::Error::other("peer identity or certificate approval failed")
}

/// Private native identity. Deliberately has no Debug or serialization API.
pub struct Identity {
    key: Vec<u8>,
    certificate: Vec<u8>,
}
impl Identity {
    /// Refresh a native trust slot before configuring an outbound connection.
    /// A previously built configuration is not an authorization cache.
    pub async fn client_for(
        &self,
        store: &okytes_persistence::SqliteStore,
        slot: uuid::Uuid,
    ) -> io::Result<ClientConfig> {
        self.client(&Self::current_peer(store, slot).await?)
    }
    /// Refresh a native trust slot before configuring an incoming peer session.
    /// Existing sessions require an additional per-operation revocation check.
    pub async fn server_for(
        &self,
        store: &okytes_persistence::SqliteStore,
        slot: uuid::Uuid,
    ) -> io::Result<ServerConfig> {
        self.server(&Self::current_peer(store, slot).await?)
    }
    /// Build one bounded incoming TLS snapshot for every currently approved
    /// identity. The authenticated leaf fingerprint must be resolved again
    /// through `peer_trust_by_fingerprint` before each application command.
    pub async fn server_for_trusted(
        &self,
        store: &okytes_persistence::SqliteStore,
    ) -> io::Result<ServerConfig> {
        let records = store.peer_active_trusts().await.map_err(|_| invalid())?;
        if records.is_empty() {
            return Err(invalid());
        }
        let peers = records
            .iter()
            .map(|record| ApprovedPeer::confirm(&record.certificate, &record.fingerprint))
            .collect::<io::Result<Vec<_>>>()?;
        self.server_many(&peers)
    }
    async fn current_peer(
        store: &okytes_persistence::SqliteStore,
        slot: uuid::Uuid,
    ) -> io::Result<ApprovedPeer> {
        let record = store
            .peer_trust(slot)
            .await
            .map_err(|_| invalid())?
            .filter(|record| !record.revoked)
            .ok_or_else(invalid)?;
        ApprovedPeer::confirm(&record.certificate, &record.fingerprint)
    }
    /// Load or provision a protected native key under the leased profile. Never
    /// replace corrupt/existing keys or follow links through the secret helpers.
    pub fn open(directory: &Path) -> io::Result<Self> {
        let path = directory.join("peer-identity-v1.key");
        let key = match okytes_browser_bridge::read_secret(&path) {
            Ok(key) => key,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let key = KeyPair::generate_for(&PKCS_ED25519)
                    .map_err(|_| invalid())?
                    .serialize_der();
                match okytes_browser_bridge::write_secret(&path, &key) {
                    Ok(()) => key,
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        okytes_browser_bridge::read_secret(&path)?
                    }
                    Err(_) => return Err(invalid()),
                }
            }
            Err(_) => return Err(invalid()),
        };
        let pair = KeyPair::from_pkcs8_der_and_sign_algo(
            &PrivatePkcs8KeyDer::from(key.clone()),
            &PKCS_ED25519,
        )
        .map_err(|_| invalid())?;
        let mut params = CertificateParams::new(vec![SERVER_NAME.into()]).map_err(|_| invalid())?;
        params.not_before = rcgen::date_time_ymd(2020, 1, 1);
        params.not_after = rcgen::date_time_ymd(2050, 1, 1);
        params.extended_key_usages = vec![
            rcgen::ExtendedKeyUsagePurpose::ServerAuth,
            rcgen::ExtendedKeyUsagePurpose::ClientAuth,
        ];
        let certificate = params
            .self_signed(&pair)
            .map_err(|_| invalid())?
            .der()
            .to_vec();
        let result = Self { key, certificate };
        // Validate key/certificate matching before exposing an identity.
        result.client(&ApprovedPeer::confirm(
            result.certificate(),
            &result.fingerprint(),
        )?)?;
        Ok(result)
    }
    /// Public certificate suitable for independent fingerprint comparison.
    pub fn certificate(&self) -> &[u8] {
        &self.certificate
    }
    /// SHA-256 of the complete canonical DER certificate.
    pub fn fingerprint(&self) -> Hash {
        Hash::digest(&self.certificate)
    }
    fn key(&self) -> PrivateKeyDer<'static> {
        PrivatePkcs8KeyDer::from(self.key.clone()).into()
    }
    /// TLS 1.3 configuration trusting only an explicitly approved remote peer.
    /// This is a trust snapshot; callers must recheck revocation per operation.
    pub fn client(&self, peer: &ApprovedPeer) -> io::Result<ClientConfig> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let verifier = rustls::client::WebPkiServerVerifier::builder_with_provider(
            Arc::new(peer.roots()?),
            provider.clone(),
        )
        .build()
        .map_err(|_| invalid())?;
        let mut config = ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|_| invalid())?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(pin::ServerPin::new(
                peer.certificate.clone(),
                verifier,
            )))
            .with_client_auth_cert(
                vec![CertificateDer::from(self.certificate.clone())],
                self.key(),
            )
            .map_err(|_| invalid())?;
        config.alpn_protocols = vec![ALPN.to_vec()];
        config.resumption = rustls::client::Resumption::disabled();
        config.enable_early_data = false;
        Ok(config)
    }
    /// Mandatory client-certificate verification, restricted to one approved pin.
    pub fn server(&self, peer: &ApprovedPeer) -> io::Result<ServerConfig> {
        self.server_many(std::slice::from_ref(peer))
    }
    fn server_many(&self, peers: &[ApprovedPeer]) -> io::Result<ServerConfig> {
        if peers.is_empty() || peers.len() > 64 {
            return Err(invalid());
        }
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut roots = RootCertStore::empty();
        for peer in peers {
            roots
                .add(CertificateDer::from(peer.certificate.clone()))
                .map_err(|_| invalid())?;
        }
        let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
            Arc::new(roots),
            provider.clone(),
        )
        .build()
        .map_err(|_| invalid())?;
        let mut config = ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|_| invalid())?
            .with_client_cert_verifier(Arc::new(pin::ClientPin::many(
                peers.iter().map(|peer| peer.certificate.clone()).collect(),
                verifier,
            )))
            .with_single_cert(
                vec![CertificateDer::from(self.certificate.clone())],
                self.key(),
            )
            .map_err(|_| invalid())?;
        config.alpn_protocols = vec![ALPN.to_vec()];
        config.send_tls13_tickets = 0;
        config.session_storage = Arc::new(rustls::server::NoServerSessionStorage {});
        Ok(config)
    }
}

/// Exact certificate approved by independent fingerprint comparison. This type
/// does not itself implement a pairing UI or confer transfer acceptance.
pub struct ApprovedPeer {
    certificate: Vec<u8>,
}
impl ApprovedPeer {
    /// Call only after the native user independently confirmed this full digest.
    /// Refuse changed DER, oversized data and malformed trust anchors.
    pub fn confirm(certificate: &[u8], confirmed: &Hash) -> io::Result<Self> {
        if certificate.is_empty()
            || certificate.len() > 4096
            || Hash::digest(certificate) != *confirmed
        {
            return Err(invalid());
        }
        let result = Self {
            certificate: certificate.to_vec(),
        };
        result.roots()?;
        Ok(result)
    }
    fn roots(&self) -> io::Result<RootCertStore> {
        let mut roots = RootCertStore::empty();
        roots
            .add(CertificateDer::from(self.certificate.clone()))
            .map_err(|_| invalid())?;
        Ok(roots)
    }
}
