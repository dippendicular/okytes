#[path = "../../daemon/tests/common/mod.rs"]
mod common;
use common::Fixture;
use okytes_core::desktop::DesktopService;
use okytes_test_support::server::{ServerConfig, TestServer};
use std::time::Duration;

async fn command(fixture: &Fixture, args: &[&str]) -> serde_json::Value {
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_okytes-cli"))
        .args(["--json", "--daemon-url", &fixture.url, "--token-file"])
        .arg(fixture.root.path().join("daemon-admin.token"))
        .args(args)
        .env_remove("OKYTES_DAEMON_URL")
        .env_remove("OKYTES_DAEMON_TOKEN_FILE")
        .kill_on_drop(true)
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_str(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .last()
            .unwrap(),
    )
    .unwrap()
}
#[tokio::test]
async fn traffic_command_round_trips_live_daemon_preferences() {
    let fixture = Fixture::new().await;
    let path = fixture.root.path().join("traffic.json");
    let policy = okytes_core::traffic::TrafficPolicy {
        global_bps: Some(65536),
        timezone: "Asia/Dhaka".into(),
        queue_allowed: false,
        ..Default::default()
    };
    std::fs::write(&path, serde_json::to_vec(&policy).unwrap()).unwrap();
    let reply = command(&fixture, &["traffic", "--set", path.to_str().unwrap()]).await;
    assert_eq!(reply["global_bps"], 65536);
    assert_eq!(reply["queue_allowed"], false);
    assert_eq!(
        fixture
            .api
            .session
            .settings()
            .await
            .unwrap()
            .traffic
            .timezone,
        "Asia/Dhaka"
    );
}

#[tokio::test]
async fn daemon_cli_retains_legacy_json_records_and_integrity_options() {
    let fixture = Fixture::new().await;
    let server = TestServer::start(ServerConfig {
        file_size: 1024 * 1024,
        frame_delay: Duration::from_millis(10),
        ..Default::default()
    })
    .await
    .unwrap();
    let url = format!("http://{}/normal", server.addr);
    let record = command(
        &fixture,
        &[
            "add",
            &url,
            "--filename",
            "cli-remote.bin",
            "--sha256",
            &server.content_hash,
            "--conflict",
            "fail",
            "--run",
        ],
    )
    .await;
    assert_eq!(record["state"], "Completed");
    assert_eq!(record["verified_sha256"], server.content_hash);
    let id = record["id"].as_str().unwrap();
    let status = command(&fixture, &["status", id]).await;
    assert_eq!(status["id"], id);
    let history = command(&fixture, &["list"]).await;
    assert_eq!(history.as_array().unwrap().len(), 1);
    assert!(command(&fixture, &["config"])
        .await
        .get("max_concurrent_jobs")
        .is_some());
    assert_eq!(
        fixture.api.session.get(id.into()).await.unwrap().state,
        "Completed"
    );
    assert_eq!(
        fixture
            .client(fixture.token.clone())
            .list(0)
            .await
            .unwrap()
            .jobs
            .len(),
        1
    );
}
