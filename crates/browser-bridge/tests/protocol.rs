use okytes_browser_bridge::{
    forward,
    protocol::{Capture, Operation, Request, Response, MAX_FRAME},
    provision, read_frame, serve, write_frame, Handler,
};
use std::{future::Future, pin::Pin, sync::Arc};

struct Echo;
impl Handler for Echo {
    fn handle(&self, request: Request) -> Pin<Box<dyn Future<Output = Response> + Send + '_>> {
        Box::pin(async move { Response::reply(request.id, Ok(None)) })
    }
}
fn ping() -> Request {
    Request {
        version: 1,
        id: uuid::Uuid::new_v4().to_string(),
        operation: Operation::Ping,
    }
}

#[tokio::test]
async fn framing_rejects_oversized_truncated_malformed_and_unknown_fields() {
    for bytes in [
        ((MAX_FRAME + 1) as u32).to_le_bytes().to_vec(),
        vec![1, 0],
        vec![1, 0, 0, 0, b'{'],
    ] {
        assert!(read_frame::<Request>(&mut bytes.as_slice()).await.is_err());
    }
    let mut bytes = Vec::new();
    write_frame(&mut bytes, &serde_json::json!({"version":1,"id":uuid::Uuid::new_v4(),"operation":{"kind":"Ping"},"extra":"secret"})).await.unwrap();
    assert!(read_frame::<Request>(&mut bytes.as_slice()).await.is_err());
    assert!(read_frame::<Request>(&mut &b""[..])
        .await
        .unwrap()
        .is_none());
}

#[test]
fn validation_blocks_paths_credentials_reserved_headers_and_versions() {
    let mut request = ping();
    request.version = 2;
    assert!(request.validate().is_err());
    request.version = 1;
    for url in [
        "file:///etc/passwd",
        "https://user:pass@example.com/a",
        "not a URL",
    ] {
        request.operation = Operation::Capture(Capture {
            url: url.into(),
            filename: None,
            sha256: None,
            headers: vec![],
        });
        assert!(request.validate().is_err());
    }
    request.operation = Operation::Capture(Capture {
        url: "https://example.com/a".into(),
        filename: None,
        sha256: None,
        headers: vec![("Range".into(), "bytes=1-2".into())],
    });
    assert!(request.validate().is_err());
}

#[tokio::test]
async fn private_token_mutual_authentication_and_unavailable_fallback() {
    let root = tempfile::tempdir().unwrap();
    provision(root.path()).unwrap();
    let first = std::fs::read(root.path().join("browser-token.json")).unwrap();
    provision(root.path()).unwrap();
    assert_eq!(
        first,
        std::fs::read(root.path().join("browser-token.json")).unwrap()
    );
    assert!(forward(root.path(), ping()).await.is_err());
    let server = serve(root.path(), Arc::new(Echo)).await.unwrap();
    let request = ping();
    let response = forward(root.path(), request.clone()).await.unwrap();
    assert!(response.ok);
    assert_eq!(response.id, request.id);
    server.abort();
    let _ = server.await;
}
