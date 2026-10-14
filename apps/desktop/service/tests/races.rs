use okytes_core::{
    desktop::{Action, AddInput, DesktopService},
    types::EngineConfig,
};
use okytes_desktop_service::Session;
use okytes_persistence::SqliteStore;
use okytes_test_support::server::{ServerConfig, TestServer};
use std::time::Duration;

#[tokio::test]
async fn pause_cancel_races_cannot_resurrect_jobs_and_lag_resync_is_authoritative() {
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("jobs.db");
    let store = SqliteStore::open(&database).unwrap();
    store
        .set_config(&EngineConfig {
            channel_capacity: 2,
            ..Default::default()
        })
        .await
        .unwrap();
    let server = TestServer::start(ServerConfig {
        file_size: 4 * 1024 * 1024,
        frame_delay: Duration::from_millis(30),
        ..Default::default()
    })
    .await
    .unwrap();
    let session = Session::open(&database, root.path()).await.unwrap();
    let mut events = session.subscribe().await.unwrap();
    let ids = session
        .add(AddInput {
            urls: (0..16)
                .map(|n| format!("http://{}/race-{n}.bin", server.addr))
                .collect(),
            destination: None,
            allow_duplicates: false,
        })
        .await
        .unwrap();
    for id in &ids {
        let (pause, cancel) = tokio::join!(
            session.control(id.clone(), Action::Pause),
            session.control(id.clone(), Action::Cancel)
        );
        pause.unwrap();
        cancel.unwrap();
        assert_eq!(session.get(id.clone()).await.unwrap().state, "Cancelled");
        assert!(session.control(id.clone(), Action::Resume).await.is_err());
    }
    assert!(matches!(
        events.recv().await,
        Err(tokio::sync::broadcast::error::RecvError::Lagged(_))
    ));
    let snapshot = session.list(0).await.unwrap();
    assert_eq!(snapshot.jobs.len(), 16);
    assert!(snapshot.jobs.iter().all(|job| job.state == "Cancelled"));
    assert!(snapshot.next.is_none());
    for id in ids {
        session.control(id, Action::Remove).await.unwrap();
    }
    assert!(session.list(0).await.unwrap().jobs.is_empty());
    let ids = session
        .add(AddInput {
            urls: (0..100)
                .map(|n| format!("http://{}/bulk-{n}.bin", server.addr))
                .collect(),
            destination: None,
            allow_duplicates: false,
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), session.pause_all())
        .await
        .unwrap()
        .unwrap();
    let snapshot = session.list(0).await.unwrap();
    assert_eq!(snapshot.jobs.len(), 100);
    assert!(snapshot.jobs.iter().all(|job| job.state == "Paused"));
    for id in ids {
        session.control(id, Action::Remove).await.unwrap();
    }
}
