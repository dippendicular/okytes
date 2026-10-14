use okytes_browser_bridge::{
    forward,
    protocol::{Capture, Operation, Request},
    provision, serve, Handler,
};
use okytes_core::desktop::DesktopService;
use okytes_desktop_service::Session;
use okytes_test_support::server::{ServerConfig, TestServer};
use sha2::{Digest, Sha256};
use std::{sync::Arc, time::Duration};

#[tokio::test]
async fn authenticated_handoff_survives_disconnect_is_idempotent_and_never_persists_credentials() {
    let root = tempfile::tempdir().unwrap();
    let server = TestServer::start(ServerConfig {
        file_size: 2 * 1024 * 1024,
        frame_delay: Duration::from_millis(10),
        ..Default::default()
    })
    .await
    .unwrap();
    let db = root.path().join("jobs.db");
    let session = Session::open(&db, root.path()).await.unwrap();
    provision(root.path()).unwrap();
    let handler: Arc<dyn Handler> = session.clone();
    let listener = serve(root.path(), handler).await.unwrap();
    let request = Request {
        version: 1,
        id: uuid::Uuid::new_v4().to_string(),
        operation: Operation::Capture(Capture {
            url: format!("http://{}/session-protected", server.addr),
            filename: Some("browser.bin".into()),
            sha256: Some(server.content_hash.clone()),
            headers: vec![
                (
                    "Cookie".into(),
                    "odm_test_cookie=NEVER_PERSIST_COOKIE_93".into(),
                ),
                (
                    "Authorization".into(),
                    "Bearer NEVER_PERSIST_AUTH_93".into(),
                ),
            ],
        }),
    };
    let response = forward(root.path(), request.clone()).await.unwrap();
    assert!(response.ok);
    assert_eq!(
        forward(root.path(), request.clone()).await.unwrap().job_id,
        response.job_id
    );
    // Destroy the entire bridge while the queue-owned transfer is still running.
    listener.abort();
    let _ = listener.await;
    let id = response.job_id.unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let job = session.get(id.clone()).await.unwrap();
            assert_ne!(job.state, "Failed", "{:?}", job.error);
            if job.state == "Completed" {
                let bytes = tokio::fs::read(job.destination).await.unwrap();
                assert_eq!(format!("{:x}", Sha256::digest(bytes)), server.content_hash);
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(session.list(0).await.unwrap().jobs.len(), 1);
    assert!(server.requests().iter().all(|request| request.has_session));
    drop(session);
    // Close SQLite's writer and release its OS locks before scanning persisted
    // files. Windows otherwise rejects raw reads of the live shared-memory file.
    tokio::time::sleep(Duration::from_millis(250)).await;
    for entry in std::fs::read_dir(root.path()).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() && path.file_name().unwrap() != "browser.bin" {
            let bytes = std::fs::read(&path).unwrap();
            let data = String::from_utf8_lossy(&bytes);
            assert!(!data.contains("NEVER_PERSIST_COOKIE_93"));
            assert!(!data.contains("NEVER_PERSIST_AUTH_93"));
        }
    }
    let session = Session::open(&db, root.path()).await.unwrap();
    if let Operation::Capture(capture) = request.operation {
        assert_eq!(session.capture(request.id, capture).await.unwrap(), id);
    }
    assert_eq!(session.list(0).await.unwrap().jobs.len(), 1);
}

#[tokio::test]
async fn credential_jobs_require_recapture_after_desktop_restart() {
    use okytes_core::desktop::Action;
    let root = tempfile::tempdir().unwrap();
    let server = TestServer::start(ServerConfig {
        file_size: 8 * 1024 * 1024,
        frame_delay: Duration::from_millis(30),
        ..Default::default()
    })
    .await
    .unwrap();
    let db = root.path().join("jobs.db");
    let session = Session::open(&db, root.path()).await.unwrap();
    let id = session
        .capture(
            uuid::Uuid::new_v4().to_string(),
            Capture {
                url: format!("http://{}/session-protected", server.addr),
                filename: Some("protected.bin".into()),
                sha256: None,
                headers: vec![
                    ("Cookie".into(), "test=memory-only".into()),
                    ("Authorization".into(), "Bearer memory-only".into()),
                ],
            },
        )
        .await
        .unwrap();
    session.control(id.clone(), Action::Pause).await.unwrap();
    assert!(session
        .control(id.clone(), Action::Restart)
        .await
        .unwrap_err()
        .to_string()
        .contains("browser"));
    drop(session);
    tokio::time::sleep(Duration::from_millis(250)).await;
    let previous = server.requests().len();
    let session = Session::open(&db, root.path()).await.unwrap();
    session.control(id.clone(), Action::Resume).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let job = session.get(id.clone()).await.unwrap();
            if job.state == "Failed" {
                assert!(job.error.unwrap().contains("session expired"));
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        server.requests().len(),
        previous,
        "must fail before sending unauthenticated HTTP"
    );
}

#[tokio::test]
async fn session_headers_never_cross_an_origin_redirect() {
    let root = tempfile::tempdir().unwrap();
    let original = TestServer::start(ServerConfig::default()).await.unwrap();
    let other = TestServer::start(ServerConfig::default()).await.unwrap();
    let session = Session::open(&root.path().join("jobs.db"), root.path())
        .await
        .unwrap();
    let id = session
        .capture(
            uuid::Uuid::new_v4().to_string(),
            Capture {
                url: format!(
                    "http://{}/cross-origin?to=http://{}/normal",
                    original.addr, other.addr
                ),
                filename: Some("redirect.bin".into()),
                sha256: Some(other.content_hash.clone()),
                headers: vec![
                    ("Cookie".into(), "test=origin-only".into()),
                    ("Authorization".into(), "Bearer origin-only".into()),
                ],
            },
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let job = session.get(id.clone()).await.unwrap();
            assert_ne!(job.state, "Failed", "{:?}", job.error);
            if job.state == "Completed" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert!(original
        .requests()
        .iter()
        .all(|request| request.has_session));
    assert!(!other.requests().is_empty());
    assert!(other.requests().iter().all(|request| !request.has_session));
}
