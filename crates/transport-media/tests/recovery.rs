use okytes_core::{
    engine::Engine,
    service::DownloadService,
    state::JobState,
    store::JobStore,
    types::{ConflictStrategy, DownloadRequest, EngineConfig},
};
use okytes_persistence::{files::LocalFiles, SqliteStore};
use okytes_test_support::server::{ServerConfig, TestServer};
use okytes_transport_http::HttpTransport;
use okytes_transport_media::MediaTransport;
use sha2::{Digest, Sha256};
use std::{sync::Arc, time::Duration};
fn setup(database: &std::path::Path) -> (Engine, Arc<SqliteStore>) {
    let config = EngineConfig {
        database_path: database.into(),
        max_concurrent_jobs: 1,
        fixed_concurrency: Some(1),
        ..Default::default()
    };
    let store = Arc::new(SqliteStore::open(database).unwrap());
    let transport = MediaTransport::new(Arc::new(HttpTransport::new(&config).unwrap()));
    (
        Engine::new(config, transport, store.clone(), Arc::new(LocalFiles)).unwrap(),
        store,
    )
}
#[test]
fn transfer_child() {
    let Some(database) = std::env::var_os("OKYTES_M7_CHILD_DB") else {
        return;
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let (engine, _) = setup(std::path::Path::new(&database));
        engine
            .execute_job(
                std::env::var("OKYTES_M7_CHILD_ID")
                    .unwrap()
                    .parse()
                    .unwrap(),
            )
            .await
            .unwrap();
    });
}
#[tokio::test]
async fn process_kill_resumes_source_segment_checkpoints_and_rejects_changed_manifest() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("jobs.db");
    let (engine, store) = setup(&database);
    let server = TestServer::start(ServerConfig {
        frame_delay: Duration::from_millis(3),
        ..Default::default()
    })
    .await
    .unwrap();
    let mut segment = vec![0u8; 188 * 8192];
    for packet in segment.chunks_mut(188) {
        packet[0] = 0x47;
        packet[3] = 0x10;
    }
    server
        .fixture("/fixtures/data.ts", segment.clone())
        .unwrap();
    server
        .fixture(
            "/fixtures/crash.m3u8",
            format!(
                "#EXTM3U\n#EXT-X-TARGETDURATION:4\n{}#EXT-X-ENDLIST\n",
                "#EXTINF:4,\ndata.ts\n".repeat(4)
            ),
        )
        .unwrap();
    let output = [
        segment.clone(),
        segment.clone(),
        segment.clone(),
        segment.clone(),
    ]
    .concat();
    let id = engine
        .add(DownloadRequest {
            url: format!("hls+{}", server.url("/fixtures/crash.m3u8"))
                .parse()
                .unwrap(),
            output_dir: temp.path().into(),
            filename: Some("recovered.ts".into()),
            expected_hash: Some(format!("{:x}", Sha256::digest(&output))),
            conflict_strategy: ConflictStrategy::Fail,
            headers: vec![],
            priority: 0,
        })
        .await
        .unwrap();
    let spawn = || {
        std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "transfer_child", "--nocapture"])
            .env("OKYTES_M7_CHILD_DB", &database)
            .env("OKYTES_M7_CHILD_ID", id.to_string())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap()
    };
    let mut child = spawn();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
    loop {
        let segments = store.load_segments(id).await.unwrap();
        if segments.iter().any(|s| s.is_complete()) {
            assert_eq!(segments.len(), 4);
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no durable source segment checkpoint"
        );
        assert!(
            child.try_wait().unwrap().is_none(),
            "child exited before checkpoint"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(!temp.path().join("recovered.ts").exists());
    server.fixture("/fixtures/crash.m3u8",format!("#EXTM3U\n#EXT-X-TARGETDURATION:4\n# comment changes immutable manifest\n{}#EXT-X-ENDLIST\n","#EXTINF:4,\ndata.ts\n".repeat(4))).unwrap();
    let result = engine.execute_job(id).await.unwrap_err();
    assert!(matches!(
        result,
        okytes_core::error::EngineError::IdentityMismatch { .. }
    ));
    assert!(!temp.path().join("recovered.ts").exists());
    server
        .fixture(
            "/fixtures/crash.m3u8",
            format!(
                "#EXTM3U\n#EXT-X-TARGETDURATION:4\n{}#EXT-X-ENDLIST\n",
                "#EXTINF:4,\ndata.ts\n".repeat(4)
            ),
        )
        .unwrap();
    engine.resume(id).await.unwrap();
    let mut resumed = spawn();
    assert!(tokio::task::spawn_blocking(move || resumed.wait())
        .await
        .unwrap()
        .unwrap()
        .success());
    let job = engine.status(id).await.unwrap();
    assert_eq!(job.state, JobState::Completed);
    assert_eq!(std::fs::read(&job.output_path).unwrap(), output);
}
