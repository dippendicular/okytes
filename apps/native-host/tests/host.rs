use okytes_browser_bridge::{
    protocol::{Capture, Operation, Request, Response},
    provision, read_frame, serve, write_frame, Handler,
};
use okytes_core::desktop::DesktopService;
use okytes_desktop_service::Session;
use okytes_test_support::server::{ServerConfig, TestServer};
use sha2::{Digest, Sha256};
use std::{process::Stdio, sync::Arc, time::Duration};

#[tokio::test]
async fn real_host_frames_capture_then_process_kill_leaves_engine_running() {
    let root = tempfile::tempdir().unwrap();
    provision(root.path()).unwrap();
    let origin = "chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/";
    std::fs::write(
        root.path().join("browser-hosts.json"),
        serde_json::to_vec(&[origin]).unwrap(),
    )
    .unwrap();
    let server = TestServer::start(ServerConfig {
        file_size: 4 * 1024 * 1024,
        frame_delay: Duration::from_millis(15),
        ..Default::default()
    })
    .await
    .unwrap();
    let session = Session::open(&root.path().join("jobs.db"), root.path())
        .await
        .unwrap();
    let handler: Arc<dyn Handler> = session.clone();
    let bridge = serve(root.path(), handler).await.unwrap();
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_okytes-native-host"))
        .arg(origin)
        .env("OKYTES_DATA_DIR", root.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let request = Request {
        version: 1,
        id: uuid::Uuid::new_v4().to_string(),
        operation: Operation::Capture(Capture {
            url: format!("http://{}/normal", server.addr),
            filename: Some("host.bin".into()),
            sha256: Some(server.content_hash.clone()),
            headers: vec![],
        }),
    };
    write_frame(child.stdin.as_mut().unwrap(), &request)
        .await
        .unwrap();
    let response: Response = tokio::time::timeout(
        Duration::from_secs(15),
        read_frame(child.stdout.as_mut().unwrap()),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert!(response.ok);
    let id = response.job_id.unwrap();
    child.kill().await.unwrap();
    child.wait().await.unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let job = session.get(id.clone()).await.unwrap();
            assert_ne!(job.state, "Failed", "{:?}", job.error);
            if job.state == "Completed" {
                assert_eq!(
                    format!(
                        "{:x}",
                        Sha256::digest(std::fs::read(job.destination).unwrap())
                    ),
                    server.content_hash
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    bridge.abort();
    let _ = bridge.await;
}
