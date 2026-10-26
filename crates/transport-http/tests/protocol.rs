use okytes_core::{
    transport::Transport,
    types::{EngineConfig, RemoteIdentity},
};
use okytes_test_support::server::{ServerConfig, TestServer};
use okytes_transport_http::HttpTransport;

#[tokio::test]
async fn completed_probes_reuse_the_same_http_connection() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpListener;
    use tokio::time::{timeout, Duration};

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/file", listener.local_addr().unwrap())
        .parse()
        .unwrap();
    // Accept exactly one socket: the second probe must arrive on that socket.
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut socket = BufReader::new(socket);
        for _ in 0..2 {
            let mut line = String::new();
            socket.read_line(&mut line).await.unwrap();
            assert!(line.starts_with("HEAD /file HTTP/1.1"));
            loop {
                line.clear();
                assert_ne!(socket.read_line(&mut line).await.unwrap(), 0);
                if line == "\r\n" {
                    break;
                }
            }
            socket
                .get_mut()
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nAccept-Ranges: bytes\r\nETag: \"stable\"\r\n\r\n",
                )
                .await
                .unwrap();
        }
    });
    let client = HttpTransport::new(&EngineConfig::default()).unwrap();
    timeout(Duration::from_secs(5), async {
        for _ in 0..2 {
            let probe = client.probe(&url, &[]).await.unwrap();
            assert_eq!(probe.content_length, Some(100));
        }
        server.await.unwrap();
    })
    .await
    .expect("both probes should finish on one connection");
}
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
