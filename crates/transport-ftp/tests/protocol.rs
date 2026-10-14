use futures::TryStreamExt;
use okytes_core::{
    transport::Transport,
    types::{EngineConfig, RemoteIdentity},
};
use okytes_test_support::file_server::{FileServer, Remote};
use okytes_transport_ftp::FileTransport;
use std::sync::atomic::Ordering;
use url::Url;
fn content() -> Vec<u8> {
    (0..262144).map(|n| (n % 251) as u8).collect()
}
fn login() -> Vec<(String, String)> {
    vec![
        ("okytes-user".into(), "user".into()),
        ("okytes-password".into(), "password".into()),
    ]
}
#[tokio::test]
async fn one_connection_cap_reuses_ftp_controls_and_handles_empty_files() {
    let temp = tempfile::tempdir().unwrap();
    let server = FileServer::ftp(Remote::new(Vec::new()), false)
        .await
        .unwrap();
    let config = EngineConfig {
        max_connections_total: 1,
        max_connections_per_origin: 1,
        ..Default::default()
    };
    let transport = FileTransport::new(&config, temp.path().join("known_hosts")).unwrap();
    let url = Url::parse(&format!("ftp://{}/file.bin", server.address)).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let probe = transport.probe(&url, &login()).await.unwrap();
        assert_eq!(probe.content_length, Some(0));
        let response = transport
            .fetch(&url, &login(), None, &probe.remote_identity)
            .await
            .unwrap();
        assert_eq!(response.content_length, Some(0));
        assert!(response
            .body
            .try_collect::<Vec<_>>()
            .await
            .unwrap()
            .is_empty());
        transport.probe(&url, &login()).await.unwrap();
    })
    .await
    .unwrap();
}
async fn body(
    transport: &FileTransport,
    url: &Url,
    headers: &[(String, String)],
    range: (u64, u64),
    identity: &RemoteIdentity,
) -> Result<Vec<u8>, okytes_core::error::TransportError> {
    let response = transport.fetch(url, headers, Some(range), identity).await?;
    Ok(response.body.try_collect::<Vec<_>>().await?.concat())
}
#[tokio::test]
async fn ftp_rest_resume_reuse_and_identity_guard() {
    let temp = tempfile::tempdir().unwrap();
    let remote = Remote::new(content());
    let server = FileServer::ftp(remote.clone(), false).await.unwrap();
    let url = Url::parse(&format!("ftp://{}/file.bin", server.address)).unwrap();
    let transport =
        FileTransport::new(&EngineConfig::default(), temp.path().join("known_hosts")).unwrap();
    let probe = transport.probe(&url, &login()).await.unwrap();
    assert_eq!(
        body(
            &transport,
            &url,
            &login(),
            (65536, 262143),
            &probe.remote_identity
        )
        .await
        .unwrap(),
        content()[65536..]
    );
    assert!(remote.offsets.lock().await.contains(&65536));
    assert_eq!(
        transport
            .probe(&url, &login())
            .await
            .unwrap()
            .remote_identity,
        probe.remote_identity
    );
    remote.version.fetch_add(1, Ordering::SeqCst);
    assert!(transport
        .fetch(
            &url,
            &login(),
            Some((65536, 262143)),
            &probe.remote_identity
        )
        .await
        .is_err());
    let mut bad = login();
    bad[1].1 = "wrong".into();
    assert!(transport.probe(&url, &bad).await.is_err());
}
#[tokio::test]
async fn ftps_validates_certificate_and_protects_resumed_data() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let temp = tempfile::tempdir().unwrap();
    let remote = Remote::new(content());
    let server = FileServer::ftp(remote, true).await.unwrap();
    let url = Url::parse(&format!(
        "ftps://localhost:{}/file.bin",
        server.address.port()
    ))
    .unwrap();
    let untrusted =
        FileTransport::new(&EngineConfig::default(), temp.path().join("known_hosts")).unwrap();
    assert!(untrusted.probe(&url, &login()).await.is_err());
    let mut roots = rustls::RootCertStore::empty();
    roots.add(server.certificate.clone().unwrap()).unwrap();
    let transport = FileTransport::with_roots(
        &EngineConfig::default(),
        temp.path().join("known_hosts"),
        roots,
    )
    .unwrap();
    let probe = transport.probe(&url, &login()).await.unwrap();
    assert_eq!(
        body(
            &transport,
            &url,
            &login(),
            (32768, 262143),
            &probe.remote_identity
        )
        .await
        .unwrap(),
        content()[32768..]
    );
}
#[tokio::test]
async fn ftp_dropped_connection_never_returns_success() {
    let temp = tempfile::tempdir().unwrap();
    let remote = Remote::new(content());
    remote.drop_at.store(65536, Ordering::SeqCst);
    let server = FileServer::ftp(remote, false).await.unwrap();
    let url = Url::parse(&format!("ftp://{}/file.bin", server.address)).unwrap();
    let transport =
        FileTransport::new(&EngineConfig::default(), temp.path().join("known_hosts")).unwrap();
    let probe = transport.probe(&url, &login()).await.unwrap();
    assert!(body(
        &transport,
        &url,
        &login(),
        (0, 262143),
        &probe.remote_identity
    )
    .await
    .is_err());
}
#[tokio::test]
async fn sftp_unknown_key_requires_confirmation_resume_and_changed_key_refused() {
    let temp = tempfile::tempdir().unwrap();
    let remote = Remote::new(content());
    let server = FileServer::sftp(remote.clone(), None).await.unwrap();
    let url = Url::parse(&format!("sftp://{}/file.bin", server.address)).unwrap();
    let known = temp.path().join("known_hosts");
    let transport = FileTransport::new(&EngineConfig::default(), known.clone()).unwrap();
    let error = transport
        .probe(&url, &login())
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains(server.fingerprint.as_ref().unwrap()),
        "{error}"
    );
    assert!(!known.exists());
    let mut confirmed = login();
    confirmed.push((
        "okytes-host-key".into(),
        server.fingerprint.clone().unwrap(),
    ));
    let probe = transport.probe(&url, &confirmed).await.unwrap();
    assert!(known.exists());
    assert_eq!(
        body(
            &transport,
            &url,
            &login(),
            (49152, 262143),
            &probe.remote_identity
        )
        .await
        .unwrap(),
        content()[49152..]
    );
    assert!(remote.offsets.lock().await.contains(&49152));
    let other =
        russh::keys::PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519).unwrap();
    std::fs::write(
        &known,
        format!(
            "[127.0.0.1]:{} {}\n",
            server.address.port(),
            other.public_key().to_openssh().unwrap()
        ),
    )
    .unwrap();
    assert!(transport.probe(&url, &confirmed).await.is_err());
}
#[tokio::test]
async fn sftp_bad_credentials_changed_file_and_failed_read() {
    let temp = tempfile::tempdir().unwrap();
    let remote = Remote::new(content());
    let server = FileServer::sftp(remote.clone(), None).await.unwrap();
    let url = Url::parse(&format!("sftp://{}/file.bin", server.address)).unwrap();
    let transport =
        FileTransport::new(&EngineConfig::default(), temp.path().join("known_hosts")).unwrap();
    let mut credentials = login();
    credentials.push((
        "okytes-host-key".into(),
        server.fingerprint.clone().unwrap(),
    ));
    let probe = transport.probe(&url, &credentials).await.unwrap();
    let mut bad = login();
    bad[1].1 = "wrong".into();
    assert!(transport.probe(&url, &bad).await.is_err());
    remote.drop_at.store(65536, Ordering::SeqCst);
    assert!(body(
        &transport,
        &url,
        &credentials,
        (0, 262143),
        &probe.remote_identity
    )
    .await
    .is_err());
    remote.version.fetch_add(1, Ordering::SeqCst);
    assert!(transport
        .fetch(
            &url,
            &credentials,
            Some((65536, 262143)),
            &probe.remote_identity
        )
        .await
        .is_err());
}
