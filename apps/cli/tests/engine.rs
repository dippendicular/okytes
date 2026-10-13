use okytes_core::{
    engine::{hash_file, Engine, JobStore},
    service::DownloadService,
    state::JobState,
    types::{ConflictStrategy, DownloadRequest, EngineConfig},
};
use okytes_persistence::SqliteStore;
use okytes_test_support::server::{ServerConfig, TestServer};
use okytes_transport_http::HttpTransport;
use std::{sync::Arc, time::Duration};

fn setup(dir: &std::path::Path) -> (Arc<Engine>, Arc<SqliteStore>) {
    let config = EngineConfig {
        database_path: dir.join("db.sqlite"),
        min_segment_size: 64 * 1024,
        max_segment_size: 128 * 1024,
        max_retries: 3,
        retry_base_delay_ms: 1,
        max_connections_per_origin: 2,
        ..Default::default()
    };
    let store = Arc::new(SqliteStore::open(&config.database_path).unwrap());
    (
        Arc::new(
            Engine::new(
                config.clone(),
                Arc::new(HttpTransport::new(&config).unwrap()),
                store.clone(),
                Arc::new(okytes_persistence::files::LocalFiles),
            )
            .unwrap(),
        ),
        store,
    )
}
async fn add(
    engine: &Engine,
    server: &TestServer,
    dir: &std::path::Path,
    path: &str,
) -> uuid::Uuid {
    engine
        .add(DownloadRequest {
            url: server.url(path).parse().unwrap(),
            output_dir: dir.into(),
            filename: Some("result.bin".into()),
            expected_hash: Some(server.content_hash.clone()),
            conflict_strategy: ConflictStrategy::Fail,
            headers: vec![],
            priority: 0,
        })
        .await
        .unwrap()
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_downloads_cover_supported_server_behaviors() {
    for path in [
        "/normal",
        "/no-range",
        "/weak-etag",
        "/redirect",
        "/head-unsupported",
        "/chunked",
        "/mid-disconnect",
        "/high-latency",
        "/slow",
        "/throttle-429",
        "/throttle-503",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let server = TestServer::start(ServerConfig::default()).await.unwrap();
        let (engine, _) = setup(dir.path());
        let id = add(&engine, &server, dir.path(), path).await;
        engine
            .execute_job(id)
            .await
            .unwrap_or_else(|e| panic!("{path}: {e}"));
        let job = engine.status(id).await.unwrap();
        assert_eq!(job.state, JobState::Completed);
        assert_eq!(
            hash_file(&job.output_path).await.unwrap(),
            server.content_hash
        );
        assert!(!job.partial_path.exists());
        assert!(server
            .requests()
            .iter()
            .all(|r| r.if_range.as_ref().is_none_or(|v| !v.starts_with("W/"))));
        assert!(server.peak_connections() <= 2);
        server.shutdown().await;
    }
}
#[tokio::test]
async fn malformed_ranges_changed_identity_and_corruption_never_publish() {
    for path in [
        "/range-ignored",
        "/wrong-content-length",
        "/invalid-headers",
        "/wrong-range",
        "/changed-etag",
        "/expired",
        "/corrupted",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let server = TestServer::start(ServerConfig::default()).await.unwrap();
        let (engine, _) = setup(dir.path());
        let id = add(&engine, &server, dir.path(), path).await;
        assert!(engine.execute_job(id).await.is_err(), "{path}");
        assert_eq!(engine.status(id).await.unwrap().state, JobState::Failed);
        assert!(!dir.path().join("result.bin").exists());
        server.shutdown().await;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pause_resume_and_changed_validator_rejection() {
    for (path, changed) in [
        ("/normal", false),
        ("/normal", true),
        ("/missing-etag", true),
        ("/changed-modified", true),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let server = TestServer::start(ServerConfig {
            file_size: 4 * 1024 * 1024,
            frame_delay: Duration::from_millis(10),
            ..Default::default()
        })
        .await
        .unwrap();
        let (engine, store) = setup(dir.path());
        let id = add(&engine, &server, dir.path(), path).await;
        let runner = engine.clone();
        let task = tokio::spawn(async move { runner.execute_job(id).await });
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if engine.status(id).await.unwrap().bytes_downloaded > 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        engine.pause(id).await.unwrap();
        task.await.unwrap().unwrap();
        assert_eq!(engine.status(id).await.unwrap().state, JobState::Paused);
        let before = store.load_segments(id).await.unwrap();
        assert!(before.iter().any(|s| s.bytes_downloaded > 0));
        if changed {
            server.set_version(2);
        }
        engine.resume(id).await.unwrap();
        let result = engine.execute_job(id).await;
        if changed {
            assert!(result.is_err());
            assert!(!dir.path().join("result.bin").exists());
            let after = store.load_segments(id).await.unwrap();
            assert_eq!(
                before.iter().map(|s| s.bytes_downloaded).sum::<u64>(),
                after.iter().map(|s| s.bytes_downloaded).sum::<u64>()
            );
        } else {
            result.unwrap();
            assert_eq!(
                hash_file(&dir.path().join("result.bin")).await.unwrap(),
                server.content_hash
            );
        }
        server.shutdown().await;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_during_transfer_releases_body_and_partial() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(ServerConfig {
        frame_delay: Duration::from_millis(10),
        ..Default::default()
    })
    .await
    .unwrap();
    let (engine, _) = setup(dir.path());
    let id = add(&engine, &server, dir.path(), "/slow").await;
    let runner = engine.clone();
    let task = tokio::spawn(async move { runner.execute_job(id).await });
    while engine.status(id).await.unwrap().bytes_downloaded == 0 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    engine.cancel(id).await.unwrap();
    task.await.unwrap().unwrap();
    let job = engine.status(id).await.unwrap();
    assert_eq!(job.state, JobState::Cancelled);
    assert!(!job.partial_path.exists());
    assert!(!job.output_path.exists());
    server.shutdown().await;
}
#[tokio::test]
async fn filename_sanitization_conflicts_and_invalid_input() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(ServerConfig::default()).await.unwrap();
    let (engine, _) = setup(dir.path());
    for name in ["../../outside", "CON.txt", "a:b", "..", "NUL. "] {
        let id = engine
            .add(DownloadRequest {
                url: server.url("/normal").parse().unwrap(),
                output_dir: dir.path().into(),
                filename: Some(name.into()),
                expected_hash: None,
                conflict_strategy: ConflictStrategy::Rename,
                headers: vec![],
                priority: 0,
            })
            .await
            .unwrap();
        let job = engine.status(id).await.unwrap();
        assert_eq!(
            job.output_path.parent(),
            Some(std::fs::canonicalize(dir.path()).unwrap().as_path())
        );
        assert!(!job
            .output_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains(':'));
    }
    std::fs::write(dir.path().join("result.bin"), b"existing").unwrap();
    let invalid = engine
        .add(DownloadRequest {
            url: server.url("/normal").parse().unwrap(),
            output_dir: dir.path().into(),
            filename: Some("result.bin".into()),
            expected_hash: None,
            conflict_strategy: ConflictStrategy::Fail,
            headers: vec![],
            priority: 0,
        })
        .await;
    assert!(invalid.is_err());
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn origin_cap_is_shared_across_jobs_and_redirects() {
    use futures::{stream, StreamExt};
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(ServerConfig {
        frame_delay: Duration::from_millis(5),
        ..Default::default()
    })
    .await
    .unwrap();
    let (engine, _) = setup(dir.path());
    let mut ids = vec![];
    for n in 0..6 {
        ids.push(
            engine
                .add(DownloadRequest {
                    url: server
                        .url(if n % 2 == 0 { "/redirect" } else { "/normal" })
                        .parse()
                        .unwrap(),
                    output_dir: dir.path().into(),
                    filename: Some(format!("job-{n}")),
                    expected_hash: Some(server.content_hash.clone()),
                    conflict_strategy: ConflictStrategy::Fail,
                    headers: vec![],
                    priority: 0,
                })
                .await
                .unwrap(),
        );
    }
    let mut jobs = stream::iter(ids)
        .map(|id| engine.execute_job(id))
        .buffer_unordered(4);
    while let Some(result) = jobs.next().await {
        result.unwrap();
    }
    assert!(server.peak_connections() <= 2);
    server.shutdown().await;
}

#[tokio::test]
async fn published_file_recovery_requires_the_persisted_verified_hash() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(ServerConfig::default()).await.unwrap();
    let (engine, _) = setup(dir.path());
    let id = add(&engine, &server, dir.path(), "/normal").await;
    engine.execute_job(id).await.unwrap();
    let mut job = engine.status(id).await.unwrap();
    job.state = JobState::Verifying;
    let store = Arc::new(SqliteStore::open(dir.path().join("crash-snapshot.sqlite")).unwrap());
    store.save_job(&job).await.unwrap();
    let config = EngineConfig::default();
    let recovered = Engine::new(
        config.clone(),
        Arc::new(HttpTransport::new(&config).unwrap()),
        store,
        Arc::new(okytes_persistence::files::LocalFiles),
    )
    .unwrap();
    recovered.execute_job(id).await.unwrap();
    assert_eq!(
        recovered.status(id).await.unwrap().state,
        JobState::Completed
    );
    assert_eq!(
        hash_file(&job.output_path).await.unwrap(),
        server.content_hash
    );
    server.shutdown().await;
}

#[tokio::test]
async fn conflict_created_after_add_is_never_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(ServerConfig::default()).await.unwrap();
    let (engine, _) = setup(dir.path());
    let id = add(&engine, &server, dir.path(), "/normal").await;
    std::fs::write(dir.path().join("result.bin"), b"keep").unwrap();
    assert!(engine.execute_job(id).await.is_err());
    assert_eq!(
        std::fs::read(dir.path().join("result.bin")).unwrap(),
        b"keep"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn orphan_preallocation_and_server_filename_are_recovered_safely() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(ServerConfig::default()).await.unwrap();
    let (engine, _) = setup(dir.path());
    let id = engine
        .add(DownloadRequest {
            url: server.url("/disposition").parse().unwrap(),
            output_dir: dir.path().into(),
            filename: None,
            expected_hash: Some(server.content_hash.clone()),
            conflict_strategy: ConflictStrategy::Fail,
            headers: vec![],
            priority: 0,
        })
        .await
        .unwrap();
    let job = engine.status(id).await.unwrap();
    std::fs::write(&job.partial_path, b"preallocation without a checkpoint").unwrap();
    engine.execute_job(id).await.unwrap();
    let job = engine.status(id).await.unwrap();
    assert_eq!(
        job.output_path.parent(),
        Some(std::fs::canonicalize(dir.path()).unwrap().as_path())
    );
    assert_eq!(
        hash_file(&job.output_path).await.unwrap(),
        server.content_hash
    );
    assert!(job
        .output_path
        .file_name()
        .unwrap()
        .to_string_lossy()
        .contains("CON"));
    assert!(!job.partial_path.exists());
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn improving_throughput_splits_relinquished_tails_without_overlap() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(ServerConfig {
        file_size: 8 * 1024 * 1024,
        ..Default::default()
    })
    .await
    .unwrap();
    let (engine, store) = setup(dir.path());
    let id = add(&engine, &server, dir.path(), "/accelerating").await;
    engine.execute_job(id).await.unwrap();
    let segments = store.load_segments(id).await.unwrap();
    assert!(
        segments.len() > 1,
        "improving throughput should admit a stealing worker"
    );
    assert!(segments.len() <= 8);
    let ranges: Vec<_> = segments
        .iter()
        .map(|s| okytes_core::range::ByteRange {
            start: s.start,
            end: s.end,
        })
        .collect();
    okytes_core::range::validate_ranges(&ranges, 8 * 1024 * 1024).unwrap();
    assert_eq!(
        hash_file(&dir.path().join("result.bin")).await.unwrap(),
        server.content_hash
    );
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancellation_interrupts_an_origin_permit_wait() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(ServerConfig {
        frame_delay: Duration::from_secs(2),
        ..Default::default()
    })
    .await
    .unwrap();
    let config = EngineConfig {
        database_path: dir.path().join("db"),
        max_connections_per_origin: 1,
        max_segments_per_job: 1,
        min_segment_size: 64 * 1024,
        max_segment_size: 128 * 1024,
        ..Default::default()
    };
    let store = Arc::new(SqliteStore::open(&config.database_path).unwrap());
    let engine = Arc::new(
        Engine::new(
            config.clone(),
            Arc::new(HttpTransport::new(&config).unwrap()),
            store,
            Arc::new(okytes_persistence::files::LocalFiles),
        )
        .unwrap(),
    );
    let first = add(&engine, &server, dir.path(), "/normal").await;
    let e = engine.clone();
    let first_task = tokio::spawn(async move { e.execute_job(first).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !server.requests().iter().any(|r| r.range.is_some()) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let second = add(&engine, &server, dir.path(), "/normal").await;
    let e = engine.clone();
    let second_task = tokio::spawn(async move { e.execute_job(second).await });
    while engine.status(second).await.unwrap().state != JobState::Probing {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    engine.cancel(second).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), second_task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        engine.status(second).await.unwrap().state,
        JobState::Cancelled
    );
    engine.cancel(first).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), first_task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    server.shutdown().await;
}
