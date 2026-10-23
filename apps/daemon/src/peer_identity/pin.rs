//! Restrictive wrappers: exact approved DER AND normal webpki validation.
//! All signature checks delegate to rustls. No verification assertion is minted
//! here, no validation failure is overridden and client auth stays mandatory.
use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, ServerName, UnixTime},
    server::danger::{ClientCertVerified, ClientCertVerifier},
    DigitallySignedStruct, DistinguishedName, Error, SignatureScheme,
};
use std::sync::Arc;

fn check(
    pin: &[u8],
    cert: &CertificateDer<'_>,
    intermediates: &[CertificateDer<'_>],
) -> Result<(), Error> {
    if pin != cert.as_ref() || !intermediates.is_empty() {
        return Err(Error::InvalidCertificate(
            rustls::CertificateError::ApplicationVerificationFailure,
        ));
    }
    Ok(())
}

#[derive(Debug)]
pub(super) struct ServerPin {
    pin: Vec<u8>,
    verifier: Arc<dyn ServerCertVerifier>,
}
impl ServerPin {
    pub(super) fn new(pin: Vec<u8>, verifier: Arc<dyn ServerCertVerifier>) -> Self {
        Self { pin, verifier }
    }
}
impl ServerCertVerifier for ServerPin {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        check(&self.pin, end_entity, intermediates)?;
        self.verifier
            .verify_server_cert(end_entity, intermediates, name, ocsp, now)
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.verifier.verify_tls12_signature(message, cert, dss)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.verifier.verify_tls13_signature(message, cert, dss)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.verifier.supported_verify_schemes()
    }
}

#[derive(Debug)]
pub(super) struct ClientPin {
    pins: Vec<Vec<u8>>,
    verifier: Arc<dyn ClientCertVerifier>,
}
impl ClientPin {
    pub(super) fn many(pins: Vec<Vec<u8>>, verifier: Arc<dyn ClientCertVerifier>) -> Self {
        Self { pins, verifier }
    }
}
impl ClientCertVerifier for ClientPin {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        self.verifier.root_hint_subjects()
    }
    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        now: UnixTime,
    ) -> Result<ClientCertVerified, Error> {
        if !intermediates.is_empty()
            || !self
                .pins
                .iter()
                .any(|pin| pin.as_slice() == end_entity.as_ref())
        {
            return Err(Error::InvalidCertificate(
                rustls::CertificateError::ApplicationVerificationFailure,
            ));
        }
        self.verifier
            .verify_client_cert(end_entity, intermediates, now)
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.verifier.verify_tls12_signature(message, cert, dss)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.verifier.verify_tls13_signature(message, cert, dss)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.verifier.supported_verify_schemes()
    }
}
