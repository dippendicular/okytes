use okytes_core::desktop::{Action, AddInput, DesktopService};
use okytes_desktop_service::Session;
use okytes_test_support::server::{ServerConfig, TestServer};
use sha2::{Digest, Sha256};
use std::time::Duration;

async fn wait(session: &Session, id: &str, state: &str) {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let job = session.get(id.into()).await.unwrap();
            if job.state == state {
                return;
            }
            assert_ne!(job.state, "Failed", "{:?}", job.error);
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn queue_download_pause_resume_cancel_and_restart_use_real_engine() {
    let root = tempfile::tempdir().unwrap();
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
    let input = AddInput {
        urls: vec![format!("http://{}/normal", server.addr)],
        destination: None,
        allow_duplicates: false,
    };
    let id = session.add(input.clone()).await.unwrap().remove(0);
    assert!(session
        .add(input)
        .await
        .unwrap_err()
        .to_string()
        .contains("duplicate"));
    wait(&session, &id, "Downloading").await;
    session.control(id.clone(), Action::Pause).await.unwrap();
    assert_eq!(session.get(id.clone()).await.unwrap().state, "Paused");
    assert_eq!(session.get(id.clone()).await.unwrap().workers, 0);
    session
        .control(id.clone(), Action::Rename("renamed.bin".into()))
        .await
        .unwrap();
    assert!(session
        .control(id.clone(), Action::Rename("../escape".into()))
        .await
        .is_err());
    session.control(id.clone(), Action::Resume).await.unwrap();
    wait(&session, &id, "Completed").await;
    let job = session.get(id.clone()).await.unwrap();
    let bytes = tokio::fs::read(&job.destination).await.unwrap();
    assert_eq!(format!("{:x}", Sha256::digest(bytes)), server.content_hash);
    session.control(id.clone(), Action::Restart).await.unwrap();
    let page = session.list(0).await.unwrap();
    let restarted = page.jobs.iter().find(|j| j.id != id).unwrap().id.clone();
    wait(&session, &restarted, "Downloading").await;
    session
        .control(restarted.clone(), Action::Cancel)
        .await
        .unwrap();
    assert_eq!(
        session.get(restarted.clone()).await.unwrap().state,
        "Cancelled"
    );
    session.control(restarted, Action::Remove).await.unwrap();
    session.control(id, Action::Remove).await.unwrap();
    assert!(std::path::Path::new(&job.destination).exists());
    assert!(session.list(0).await.unwrap().jobs.is_empty());
}

#[tokio::test]
async fn settings_persist_and_inputs_are_bounded() {
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("jobs.db");
    let session = Session::open(&database, root.path()).await.unwrap();
    let mut settings = session.settings().await.unwrap();
    settings.concurrency = 0;
    assert!(session.set_settings(settings.clone()).await.is_err());
    settings.concurrency = 1;
    settings.theme = "dark".into();
    session.set_settings(settings).await.unwrap();
    for urls in [
        vec![],
        vec!["https://example.com".into(); 101],
        vec!["file:///secret".into()],
        vec!["https://user:secret@example.com".into()],
    ] {
        assert!(session
            .add(AddInput {
                urls,
                destination: None,
                allow_duplicates: true
            })
            .await
            .is_err());
    }
    assert!(session
        .control("bad-id".into(), Action::Pause)
        .await
        .is_err());
    assert!(session.list(u32::MAX).await.is_err());
    drop(session);
    // Runner uses weak ownership and releases its session between ticks.
    tokio::time::sleep(Duration::from_millis(150)).await;
    let session = Session::open(&database, root.path()).await.unwrap();
    assert_eq!(session.settings().await.unwrap().theme, "dark");
}

#[tokio::test]
async fn full_streams_show_live_durable_bytes_without_creating_resume_checkpoints() {
    let root = tempfile::tempdir().unwrap();
    let server = TestServer::start(ServerConfig {
        file_size: 4 * 1024 * 1024,
        frame_delay: Duration::from_millis(20),
        ..Default::default()
    })
    .await
    .unwrap();
    let session = Session::open(&root.path().join("jobs.db"), root.path())
        .await
        .unwrap();
    for endpoint in ["no-range", "weak-etag", "chunked"] {
        let id = session
            .add(AddInput {
                urls: vec![format!("http://{}/{endpoint}", server.addr)],
                destination: None,
                allow_duplicates: false,
            })
            .await
            .unwrap()
            .remove(0);
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let job = session.get(id.clone()).await.unwrap();
                if job.state == "Downloading"
                    && job.bytes > 0.0
                    && job.bytes < 4.0 * 1024.0 * 1024.0
                {
                    assert!(job.workers > 0);
                    assert!(
                        job.segments.is_empty(),
                        "full-stream display bytes must not become resumable segment checkpoints"
                    );
                    break;
                }
                assert_ne!(job.state, "Completed", "missed live progress");
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap();
        wait(&session, &id, "Completed").await;
        let job = session.get(id.clone()).await.unwrap();
        assert_eq!(
            format!(
                "{:x}",
                Sha256::digest(tokio::fs::read(job.destination).await.unwrap())
            ),
            server.content_hash
        );
    }
}
