use okytes_core::{
    transport::Transport,
    types::{EngineConfig, RemoteIdentity},
};
use okytes_test_support::server::{ServerConfig, TestServer};
use okytes_transport_http::HttpTransport;
#[tokio::test]
async fn head_reads_resource_header_and_weak_etags_never_reach_if_range() {
    let server = TestServer::start(ServerConfig::default()).await.unwrap();
    let client = HttpTransport::new(&EngineConfig::default()).unwrap();
    let probe = client
        .probe(&server.url("/normal").parse().unwrap(), &[])
        .await
        .unwrap();
    assert_eq!(probe.content_length, Some(1024 * 1024));
    let url = server.url("/weak-etag").parse().unwrap();
    let weak = RemoteIdentity {
        etag: Some("W/\"weak\"".into()),
        last_modified: None,
        content_length: Some(1024 * 1024),
    };
    let _ = client.fetch(&url, &[], Some((0, 99)), &weak).await;
    assert!(server.requests().iter().all(|r| r.if_range.is_none()));
    server.shutdown().await;
}
#[tokio::test]
async fn tls_handshake_and_non_http_redirect_targets_fail_closed() {
    let server = TestServer::start(ServerConfig::default()).await.unwrap();
    let config = EngineConfig {
        read_timeout_secs: 1,
        ..Default::default()
    };
    let client = HttpTransport::new(&config).unwrap();
    let url = server
        .url("/normal")
        .replace("http:", "https:")
        .parse()
        .unwrap();
    assert!(client.probe(&url, &[]).await.is_err());
    assert!(client
        .probe(&"file:///etc/passwd".parse().unwrap(), &[])
        .await
        .is_err());
    server.shutdown().await;
}
