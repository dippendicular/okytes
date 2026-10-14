mod common;
use common::Fixture;
use okytes_core::desktop::DesktopService;
use okytes_test_support::server::{ServerConfig, TestServer};
use sha2::{Digest, Sha256};
use std::time::Duration;

#[tokio::test]
#[ignore = "requires Node.js and built TypeScript SDK; CI runs this explicitly"]
async fn generated_sdk_operates_against_a_real_running_daemon() {
    let fixture = Fixture::new().await;
    let server = TestServer::start(ServerConfig {
        file_size: 4 * 1024 * 1024,
        frame_delay: Duration::from_millis(15),
        ..Default::default()
    })
    .await
    .unwrap();
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../packages/sdk-typescript/tests/live.mjs");
    let result = tokio::time::timeout(
        Duration::from_secs(45),
        tokio::process::Command::new("node")
            .arg(script)
            .env("OKYTES_SDK_URL", &fixture.url)
            .env(
                "OKYTES_SDK_TOKEN_FILE",
                fixture.root.path().join("daemon-admin.token"),
            )
            .env(
                "OKYTES_SDK_SOURCE",
                format!("http://{}/normal", server.addr),
            )
            .env("OKYTES_SDK_SHA", &server.content_hash)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        result.status.success(),
        "SDK failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let evidence: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    let id = evidence["id"].as_str().unwrap();
    assert_eq!(
        fixture
            .client(fixture.token.clone())
            .get(id.into())
            .await
            .unwrap()
            .state,
        "Completed"
    );
    assert_eq!(fixture.api.session.list(0).await.unwrap().jobs.len(), 1);
    assert_eq!(
        format!(
            "{:x}",
            Sha256::digest(std::fs::read(evidence["destination"].as_str().unwrap()).unwrap())
        ),
        server.content_hash
    );
}
