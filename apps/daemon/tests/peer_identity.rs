use okytes_core::peer::Hash;
use okytes_daemon::peer_identity::{ApprovedPeer, Identity, ALPN, SERVER_NAME};
use okytes_persistence::SqliteStore;
use rustls::pki_types::ServerName;
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tokio_rustls::{TlsAcceptor, TlsConnector};
use uuid::Uuid;

fn approved(identity: &Identity) -> ApprovedPeer {
    ApprovedPeer::confirm(identity.certificate(), &identity.fingerprint()).unwrap()
}

#[test]
fn protected_identity_survives_restart_and_rejects_corruption() {
    let root = tempfile::tempdir().unwrap();
    let one = Identity::open(root.path()).unwrap();
    let two = Identity::open(root.path()).unwrap();
    assert_eq!(one.certificate(), two.certificate());
    assert_eq!(one.fingerprint(), two.fingerprint());
    let other = tempfile::tempdir().unwrap();
    let different = Identity::open(other.path()).unwrap();
    assert_ne!(one.fingerprint(), different.fingerprint());
    assert!(ApprovedPeer::confirm(one.certificate(), &different.fingerprint()).is_err());
    assert!(ApprovedPeer::confirm(b"not DER", &Hash::digest(b"not DER")).is_err());
    assert!(ApprovedPeer::confirm(&vec![0; 4097], &Hash::digest(&vec![0; 4097])).is_err());
    let path = root.path().join("peer-identity-v1.key");
    assert!(std::fs::metadata(&path).unwrap().len() <= 512);
    // Explicit test corruption is never repaired or silently re-paired.
    std::fs::write(&path, b"broken").unwrap();
    assert!(Identity::open(root.path()).is_err());
    assert_eq!(std::fs::read(path).unwrap(), b"broken");
}

async fn exchange(server: rustls::ServerConfig, client: rustls::ClientConfig) -> bool {
    tokio::time::timeout(Duration::from_secs(5), async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut tls = TlsAcceptor::from(Arc::new(server))
                .accept(tcp)
                .await
                .map_err(|_| ())?;
            if tls.get_ref().1.alpn_protocol() != Some(ALPN) {
                return Err(());
            }
            let mut input = [0; 4];
            tls.read_exact(&mut input).await.map_err(|_| ())?;
            if input != *b"ping" {
                return Err(());
            }
            tls.write_all(b"pong").await.map_err(|_| ())?;
            Ok::<_, ()>(())
        };
        let client = async move {
            let tcp = TcpStream::connect(address).await.unwrap();
            let mut tls = TlsConnector::from(Arc::new(client))
                .connect(ServerName::try_from(SERVER_NAME).unwrap(), tcp)
                .await
                .map_err(|_| ())?;
            if tls.get_ref().1.alpn_protocol() != Some(ALPN) {
                return Err(());
            }
            assert_eq!(
                tls.get_ref().1.protocol_version(),
                Some(rustls::ProtocolVersion::TLSv1_3)
            );
            tls.write_all(b"ping").await.map_err(|_| ())?;
            let mut reply = [0; 4];
            tls.read_exact(&mut reply).await.map_err(|_| ())?;
            if reply != *b"pong" {
                return Err(());
            }
            Ok::<_, ()>(())
        };
        let (server, client) = tokio::join!(server, client);
        server.is_ok() && client.is_ok()
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn real_tls_requires_both_approved_identities_and_protocol() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let c = tempfile::tempdir().unwrap();
    let a = Identity::open(a.path()).unwrap();
    let b = Identity::open(b.path()).unwrap();
    let c = Identity::open(c.path()).unwrap();
    assert!(
        exchange(
            b.server(&approved(&a)).unwrap(),
            a.client(&approved(&b)).unwrap()
        )
        .await
    );
    // Client substitution and server substitution fail normal rustls verification.
    assert!(
        !exchange(
            b.server(&approved(&a)).unwrap(),
            c.client(&approved(&b)).unwrap()
        )
        .await
    );
    assert!(
        !exchange(
            c.server(&approved(&a)).unwrap(),
            a.client(&approved(&b)).unwrap()
        )
        .await
    );
    let mut wrong_protocol = a.client(&approved(&b)).unwrap();
    wrong_protocol.alpn_protocols = vec![b"http/1.1".to_vec()];
    assert!(!exchange(b.server(&approved(&a)).unwrap(), wrong_protocol).await);
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut roots = rustls::RootCertStore::empty();
    roots.add(b.certificate().to_vec().into()).unwrap();
    let mut anonymous = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    anonymous.alpn_protocols = vec![ALPN.to_vec()];
    assert!(!exchange(b.server(&approved(&a)).unwrap(), anonymous).await);
}

#[tokio::test]
async fn a_new_certificate_with_the_same_private_key_requires_new_approval() {
    use rustls::pki_types::PrivatePkcs8KeyDer;
    let a_root = tempfile::tempdir().unwrap();
    let b_root = tempfile::tempdir().unwrap();
    let a = Identity::open(a_root.path()).unwrap();
    let b = Identity::open(b_root.path()).unwrap();
    let altered = |root: &std::path::Path| {
        let key = okytes_browser_bridge::read_secret(&root.join("peer-identity-v1.key")).unwrap();
        let pair = rcgen::KeyPair::from_pkcs8_der_and_sign_algo(
            &PrivatePkcs8KeyDer::from(key.clone()),
            &rcgen::PKCS_ED25519,
        )
        .unwrap();
        let mut params = rcgen::CertificateParams::new(vec![SERVER_NAME.into()]).unwrap();
        params.not_before = rcgen::date_time_ymd(2020, 1, 1);
        params.not_after = rcgen::date_time_ymd(2050, 1, 1);
        params.extended_key_usages = vec![
            rcgen::ExtendedKeyUsagePurpose::ServerAuth,
            rcgen::ExtendedKeyUsagePurpose::ClientAuth,
        ];
        params.serial_number = Some(999u64.into());
        (params.self_signed(&pair).unwrap().der().to_vec(), key)
    };
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let (cert, key) = altered(b_root.path());
    assert_ne!(cert, b.certificate());
    let mut server = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert.into()], PrivatePkcs8KeyDer::from(key).into())
        .unwrap();
    server.alpn_protocols = vec![ALPN.to_vec()];
    assert!(!exchange(server, a.client(&approved(&b)).unwrap()).await);
    let (cert, key) = altered(a_root.path());
    assert_ne!(cert, a.certificate());
    let mut roots = rustls::RootCertStore::empty();
    roots.add(b.certificate().to_vec().into()).unwrap();
    let mut client = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_root_certificates(roots)
        .with_client_auth_cert(vec![cert.into()], PrivatePkcs8KeyDer::from(key).into())
        .unwrap();
    client.alpn_protocols = vec![ALPN.to_vec()];
    assert!(!exchange(b.server(&approved(&a)).unwrap(), client).await);
}

#[tokio::test]
async fn explicit_trust_revision_and_revocation_survive_restart() {
    let root = tempfile::tempdir().unwrap();
    let remote = tempfile::tempdir().unwrap();
    let identity = Identity::open(remote.path()).unwrap();
    let local = Identity::open(root.path()).unwrap();
    let path = root.path().join("profile.sqlite");
    let store = SqliteStore::open(&path).unwrap();
    let slot = Uuid::new_v4();
    assert!(store.peer_trust(slot).await.unwrap().is_none());
    assert!(local.client_for(&store, slot).await.is_err());
    assert!(store
        .peer_approve(
            slot,
            identity.certificate().to_vec(),
            Hash::digest(b"wrong approval"),
            None
        )
        .await
        .is_err());
    let trusted = store
        .peer_approve(
            slot,
            identity.certificate().to_vec(),
            identity.fingerprint(),
            None,
        )
        .await
        .unwrap();
    assert!(!trusted.revoked);
    assert_eq!(trusted.revision, 0);
    local.client_for(&store, slot).await.unwrap();
    local.server_for(&store, slot).await.unwrap();
    let (replace, revoke) = tokio::join!(
        store.peer_approve(
            slot,
            identity.certificate().to_vec(),
            identity.fingerprint(),
            Some(0)
        ),
        store.peer_revoke(slot, 0)
    );
    assert_ne!(replace.is_ok(), revoke.is_ok());
    let current = store.peer_trust(slot).await.unwrap().unwrap();
    let revoked = store.peer_revoke(slot, current.revision).await.unwrap();
    assert!(revoked.revoked);
    assert_eq!(
        store.peer_revoke(slot, current.revision).await.unwrap(),
        revoked
    );
    drop(store);
    let store = SqliteStore::open(&path).unwrap();
    assert_eq!(store.peer_trust(slot).await.unwrap().unwrap(), revoked);
    assert!(local.client_for(&store, slot).await.is_err());
    assert!(local.server_for(&store, slot).await.is_err());
    assert!(store
        .peer_approve(
            slot,
            identity.certificate().to_vec(),
            identity.fingerprint(),
            None
        )
        .await
        .is_err());
    let replacement = tempfile::tempdir().unwrap();
    let replacement = Identity::open(replacement.path()).unwrap();
    assert!(store
        .peer_approve(
            slot,
            replacement.certificate().to_vec(),
            replacement.fingerprint(),
            Some(0)
        )
        .await
        .is_err());
    let renewed = store
        .peer_approve(
            slot,
            replacement.certificate().to_vec(),
            replacement.fingerprint(),
            Some(revoked.revision),
        )
        .await
        .unwrap();
    assert!(!renewed.revoked);
    assert_ne!(renewed.fingerprint, trusted.fingerprint);
    // Old revocation cannot revoke the new approved identity.
    assert!(store.peer_revoke(slot, revoked.revision).await.is_err());
    ApprovedPeer::confirm(&renewed.certificate, &renewed.fingerprint).unwrap();
}

#[tokio::test]
async fn trust_registry_rejects_invalid_and_unbounded_admission() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path().join("profile.sqlite")).unwrap();
    let identity = Identity::open(root.path()).unwrap();
    assert!(store
        .peer_approve(
            Uuid::nil(),
            identity.certificate().to_vec(),
            identity.fingerprint(),
            None
        )
        .await
        .is_err());
    assert!(store
        .peer_approve(Uuid::new_v4(), vec![], Hash::digest(b""), None)
        .await
        .is_err());
    let large = vec![0; 4097];
    assert!(store
        .peer_approve(Uuid::new_v4(), large.clone(), Hash::digest(&large), None)
        .await
        .is_err());
    assert!(store.peer_revoke(Uuid::new_v4(), 0).await.is_err());
    for value in 0u8..64 {
        let certificate = vec![value.saturating_add(1)];
        store
            .peer_approve(
                Uuid::new_v4(),
                certificate.clone(),
                Hash::digest(&certificate),
                None,
            )
            .await
            .unwrap();
    }
    let certificate = vec![65];
    assert!(store
        .peer_approve(
            Uuid::new_v4(),
            certificate.clone(),
            Hash::digest(&certificate),
            None
        )
        .await
        .is_err());
}

#[tokio::test]
async fn incoming_tls_snapshot_accepts_each_active_pin_and_rechecks_revocation() {
    let directory = tempfile::tempdir().unwrap();
    let local_dir = directory.path().join("local");
    let first_dir = directory.path().join("first");
    let second_dir = directory.path().join("second");
    std::fs::create_dir_all(&local_dir).unwrap();
    std::fs::create_dir_all(&first_dir).unwrap();
    std::fs::create_dir_all(&second_dir).unwrap();
    let local = Identity::open(&local_dir).unwrap();
    let first = Identity::open(&first_dir).unwrap();
    let second = Identity::open(&second_dir).unwrap();
    let store = SqliteStore::open(directory.path().join("trust.sqlite")).unwrap();
    let first_slot = Uuid::new_v4();
    let second_slot = Uuid::new_v4();
    store
        .peer_approve(
            first_slot,
            first.certificate().to_vec(),
            first.fingerprint(),
            None,
        )
        .await
        .unwrap();
    store
        .peer_approve(
            second_slot,
            second.certificate().to_vec(),
            second.fingerprint(),
            None,
        )
        .await
        .unwrap();
    local.server_for_trusted(&store).await.unwrap();
    assert_eq!(store.peer_active_trusts().await.unwrap().len(), 2);
    assert_eq!(
        store
            .peer_trust_by_fingerprint(first.fingerprint())
            .await
            .unwrap()
            .unwrap()
            .id,
        first_slot
    );
    store.peer_revoke(first_slot, 0).await.unwrap();
    assert!(store
        .peer_trust_by_fingerprint(first.fingerprint())
        .await
        .unwrap()
        .is_none());
    assert_eq!(store.peer_active_trusts().await.unwrap().len(), 1);
    local.server_for_trusted(&store).await.unwrap();
}
