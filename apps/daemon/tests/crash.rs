use okytes_core::{desktop::DesktopService, state::JobState, types::JobRecord};
use okytes_daemon_api::{contract::Submit, Client};
use okytes_test_support::server::{ServerConfig, TestServer};
use sha2::{Digest, Sha256};
use std::{path::Path, process::Stdio, time::Duration};
async fn start(root: &Path, port: u16) -> (tokio::process::Child, Client) {
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_okytes-daemon"))
        .args(["--bind", &format!("127.0.0.1:{port}"), "--data-dir"])
        .arg(root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let file = root.join("daemon-admin.token");
    let client = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            assert!(
                child.try_wait().unwrap().is_none(),
                "daemon exited before becoming ready"
            );
            if file.exists() {
                if let Ok(client) = Client::from_file(format!("http://127.0.0.1:{port}/"), &file) {
                    if client.list(0).await.is_ok() {
                        break client;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
    (child, client)
}
#[tokio::test]
async fn forced_daemon_kill_restarts_from_durable_bytes_and_finishes_identically() {
    let root = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let server = TestServer::start(ServerConfig {
        file_size: 8 * 1024 * 1024,
        frame_delay: Duration::from_millis(30),
        ..Default::default()
    })
    .await
    .unwrap();
    let (mut child, client) = start(root.path(), port).await;
    let id = client
        .submit(Submit {
            url: format!("http://{}/normal", server.addr),
            destination: None,
            filename: Some("crash.bin".into()),
            sha256: Some(server.content_hash.clone()),
            conflict: "fail".into(),
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let job = client.get(id.clone()).await.unwrap();
            assert_ne!(job.state, "Completed", "fixture finished before kill");
            if job.bytes > 0.0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    child.kill().await.unwrap();
    child.wait().await.unwrap();
    drop(client);
    let (mut child, client) = start(root.path(), port).await;
    tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            let record: JobRecord = client
                .request(
                    reqwest::Method::GET,
                    &format!("v1/records/{id}"),
                    None::<&()>,
                )
                .await
                .unwrap();
            assert_ne!(record.state, JobState::Failed, "{:?}", record.error_message);
            if record.state == JobState::Completed {
                assert_eq!(
                    record.verified_sha256.as_deref(),
                    Some(server.content_hash.as_str())
                );
                assert_eq!(
                    format!(
                        "{:x}",
                        Sha256::digest(std::fs::read(record.output_path).unwrap())
                    ),
                    server.content_hash
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
    child.kill().await.unwrap();
    child.wait().await.unwrap();
    assert!(server.requests().iter().any(|request| request
        .range
        .as_deref()
        .is_some_and(|range| !range.starts_with("bytes=0-"))));
}

#[tokio::test]
async fn remote_plaintext_and_incomplete_tls_configuration_are_refused_before_provisioning() {
    let root = tempfile::tempdir().unwrap();
    for args in [
        vec!["--bind", "0.0.0.0:17841"],
        vec!["--bind", "0.0.0.0:17841", "--remote"],
    ] {
        let result = tokio::process::Command::new(env!("CARGO_BIN_EXE_okytes-daemon"))
            .args(args)
            .arg("--data-dir")
            .arg(root.path())
            .kill_on_drop(true)
            .output()
            .await
            .unwrap();
        assert!(!result.status.success());
        assert!(!root.path().join("daemon-admin.token").exists());
    }
}
