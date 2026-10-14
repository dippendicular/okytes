use okytes_core::{
    desktop::{Action, AddInput, DesktopService},
    traffic::{ScheduleWindow, TrafficPolicy},
};
use okytes_desktop_service::Session;
use okytes_test_support::server::{ServerConfig, TestServer};
use std::time::{Duration, Instant};

#[test]
fn overlapping_windows_are_restrictive_and_job_overrides_are_independent() {
    let first = uuid::Uuid::new_v4();
    let other = uuid::Uuid::new_v4();
    let policy = TrafficPolicy {
        global_bps: Some(1000),
        per_job_bps: Some(500),
        queue_allowed: false,
        job_limits: vec![okytes_core::traffic::JobLimit {
            id: first.to_string(),
            bps: Some(100),
        }],
        windows: vec![
            ScheduleWindow {
                days: vec![0],
                start_minute: 0,
                end_minute: 1440,
                global_bps: Some(700),
                queue_allowed: Some(true),
            },
            ScheduleWindow {
                days: vec![0],
                start_minute: 1200,
                end_minute: 1440,
                global_bps: Some(250),
                queue_allowed: Some(false),
            },
        ],
        ..Default::default()
    };
    policy.validate().unwrap();
    let at = "2026-10-12T22:00:00Z".parse().unwrap();
    assert_eq!(policy.effective(at, first).global_bps, Some(250));
    assert!(!policy.effective(at, first).queue_allowed);
    assert_eq!(policy.effective(at, first).job_bps, Some(100));
    assert_eq!(policy.effective(at, other).job_bps, Some(500));
    let earlier = "2026-10-12T10:00:00Z".parse().unwrap();
    assert!(policy.effective(earlier, first).queue_allowed);
    assert_eq!(policy.effective(earlier, first).global_bps, Some(700));
    let mut invalid = policy;
    invalid.timezone = "Unknown/Nowhere".into();
    assert!(invalid.validate().is_err());
}

async fn completed(session: &Session, id: &str) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let job = session.get(id.into()).await.unwrap();
            assert_ne!(job.state, "Failed", "{:?}", job.error);
            if job.state == "Completed" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn closed_queue_policy_survives_restart_and_opens_without_resuming_user_pauses() {
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join("jobs.sqlite");
    let origin = TestServer::start(ServerConfig {
        file_size: 256 * 1024,
        frame_delay: Duration::from_millis(10),
        ..Default::default()
    })
    .await
    .unwrap();
    let session = Session::open(&db, root.path()).await.unwrap();
    let mut settings = session.settings().await.unwrap();
    settings.traffic = TrafficPolicy {
        queue_allowed: true,
        timezone: "Asia/Dhaka".into(),
        windows: vec![ScheduleWindow {
            days: vec![0, 1, 2, 3, 4, 5, 6],
            start_minute: 0,
            end_minute: 1440,
            global_bps: Some(65536),
            queue_allowed: Some(false),
        }],
        ..Default::default()
    };
    session.set_settings(settings.clone()).await.unwrap();
    let id = session
        .add(AddInput {
            urls: vec![origin.url("/normal")],
            destination: None,
            allow_duplicates: false,
        })
        .await
        .unwrap()
        .remove(0);
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(session.get(id.clone()).await.unwrap().state, "Queued");
    assert!(origin.requests().is_empty());
    session.control(id.clone(), Action::Pause).await.unwrap();
    session.shutdown().await.unwrap();
    drop(session);
    tokio::time::sleep(Duration::from_millis(150)).await;
    let session = Session::open(&db, root.path()).await.unwrap();
    assert_eq!(
        serde_json::to_value(session.settings().await.unwrap().traffic).unwrap(),
        serde_json::to_value(settings.traffic).unwrap()
    );
    let mut settings = session.settings().await.unwrap();
    settings.traffic.queue_allowed = true;
    settings.traffic.windows.clear();
    session.set_settings(settings).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(session.get(id.clone()).await.unwrap().state, "Paused");
    session.control(id.clone(), Action::Resume).await.unwrap();
    completed(&session, &id).await;
    assert_eq!(
        okytes_core::engine::hash_file(std::path::Path::new(
            &session.get(id).await.unwrap().destination
        ))
        .await
        .unwrap(),
        origin.content_hash
    );
    session.shutdown().await.unwrap();
}
#[tokio::test]
async fn aggregate_payload_limit_is_shared_by_real_jobs_and_live_updates_release_waits() {
    let root = tempfile::tempdir().unwrap();
    let origin = TestServer::start(ServerConfig {
        file_size: 128 * 1024,
        ..Default::default()
    })
    .await
    .unwrap();
    let session = Session::open(&root.path().join("jobs.sqlite"), root.path())
        .await
        .unwrap();
    let mut settings = session.settings().await.unwrap();
    settings.traffic.global_bps = Some(128 * 1024);
    session.set_settings(settings).await.unwrap();
    let start = Instant::now();
    let ids = session
        .add(AddInput {
            urls: vec![origin.url("/a"), origin.url("/b")],
            destination: None,
            allow_duplicates: false,
        })
        .await
        .unwrap();
    for id in &ids {
        completed(&session, id).await;
    }
    let elapsed = start.elapsed().as_secs_f64();
    assert!(
        elapsed >= 1.8,
        "global limit multiplied by job count: {elapsed}"
    );
    assert!(
        elapsed < 3.0,
        "payload rate more than 33% below target: {elapsed}"
    );
    let mut settings = session.settings().await.unwrap();
    settings.traffic.global_bps = Some(1);
    session.set_settings(settings).await.unwrap();
    let id = session
        .add(AddInput {
            urls: vec![origin.url("/c")],
            destination: None,
            allow_duplicates: false,
        })
        .await
        .unwrap()
        .remove(0);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut settings = session.settings().await.unwrap();
    settings.traffic.global_bps = None;
    session.set_settings(settings).await.unwrap();
    completed(&session, &id).await;
    session.shutdown().await.unwrap();
}
