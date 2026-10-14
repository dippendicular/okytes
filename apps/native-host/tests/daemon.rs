#[path = "../../daemon/tests/common/mod.rs"]
mod common;
use common::Fixture;
use okytes_browser_bridge::{
    protocol::{Capture, Operation, Request, Response},
    read_frame, write_frame,
};
use okytes_core::desktop::DesktopService;
use okytes_test_support::server::{ServerConfig, TestServer};
use sha2::{Digest, Sha256};
use std::{process::Stdio, time::Duration};

#[tokio::test]
async fn browser_host_uses_configured_daemon_and_disconnect_cannot_cancel_it() {
    let f = Fixture::new().await;
    let origin = "chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/";
    std::fs::write(
        f.root.path().join("browser-hosts.json"),
        serde_json::to_vec(&[origin]).unwrap(),
    )
    .unwrap();
    let source = TestServer::start(ServerConfig {
        file_size: 4 * 1024 * 1024,
        frame_delay: Duration::from_millis(15),
        ..Default::default()
    })
    .await
    .unwrap();
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_okytes-native-host"))
        .arg(origin)
        .env("OKYTES_DATA_DIR", f.root.path())
        .env("OKYTES_DAEMON_URL", &f.url)
        .env(
            "OKYTES_DAEMON_TOKEN_FILE",
            f.root.path().join("daemon-admin.token"),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let request = Request {
        version: 1,
        id: uuid::Uuid::new_v4().to_string(),
        operation: Operation::Capture(Capture {
            url: format!("http://{}/normal", source.addr),
            filename: Some("daemon-host.bin".into()),
            sha256: Some(source.content_hash.clone()),
            headers: vec![],
        }),
    };
    write_frame(child.stdin.as_mut().unwrap(), &request)
        .await
        .unwrap();
    let reply: Response = read_frame(child.stdout.as_mut().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert!(reply.ok);
    let id = reply.job_id.unwrap();
    child.kill().await.unwrap();
    child.wait().await.unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let job = f.client(f.token.clone()).get(id.clone()).await.unwrap();
            assert_ne!(job.state, "Failed");
            if job.state == "Completed" {
                assert_eq!(
                    format!(
                        "{:x}",
                        Sha256::digest(std::fs::read(job.destination).unwrap())
                    ),
                    source.content_hash
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(f.api.session.list(0).await.unwrap().jobs.len(), 1);
}
