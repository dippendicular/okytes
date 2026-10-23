use okytes_core::desktop::{AddInput, DesktopService};
use okytes_desktop_service::Session;
use std::{process::Stdio, time::Duration};

mod common;
use common::Fixture;

#[tokio::test]
async fn occupied_listener_fails_before_provisioning_profile_or_credentials() {
    let root = tempfile::tempdir().unwrap();
    let profile = root.path().join("unprovisioned");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = listener.local_addr().unwrap().to_string();
    let child = tokio::process::Command::new(env!("CARGO_BIN_EXE_okytes-daemon"))
        .args(["--bind", &endpoint, "--data-dir"])
        .arg(&profile)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(15), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("choose a free --bind address"));
    assert!(
        !profile.exists(),
        "port conflict provisioned a queue profile"
    );
}

#[tokio::test]
async fn concurrent_clients_share_admission_and_embedded_contenders_cannot_take_ownership() {
    let fixture = Fixture::new().await;
    let database = fixture.root.path().join("okytes.db");
    assert!(
        Session::open(&database, &fixture.root.path().join("downloads"))
            .await
            .is_err()
    );
    let first = fixture.client(fixture.token.clone());
    let second = fixture.client(fixture.token.clone());
    // A closed admission window keeps the fixture deterministic without network
    // access. Both clients still submit through the actual authenticated server.
    let mut settings = fixture.api.session.settings().await.unwrap();
    settings.traffic.queue_allowed = false;
    fixture.api.session.set_settings(settings).await.unwrap();
    let input = AddInput {
        urls: vec!["https://example.invalid/manual-download.bin".into()],
        destination: None,
        allow_duplicates: false,
    };
    let (left, right) = tokio::join!(first.add(input.clone()), second.add(input));
    assert_ne!(
        left.is_ok(),
        right.is_ok(),
        "exactly one duplicate admission must succeed"
    );
    let (left, right) = tokio::join!(first.list(0), second.list(0));
    let left = left.unwrap();
    let right = right.unwrap();
    assert_eq!(left.jobs.len(), 1);
    assert_eq!(left.jobs[0].id, right.jobs[0].id);
    assert_eq!(left.jobs[0].state, "Queued");
}
