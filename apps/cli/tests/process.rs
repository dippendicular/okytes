use okytes_core::{
    engine::{hash_file, Engine, JobStore},
    service::DownloadService,
    state::JobState,
    types::{ConflictStrategy, DownloadRequest, EngineConfig},
};
use okytes_persistence::SqliteStore;
use okytes_test_support::server::{ServerConfig, TestServer};
use okytes_transport_http::HttpTransport;
use std::{
    process::{Child, Command, Stdio},
    sync::Arc,
    time::Duration,
};

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn command(db: &std::path::Path) -> Command {
    // Release validation can exercise the installed executable with the same
    // crash-recovery and changed-validator fixtures as the workspace binary.
    let binary = std::env::var_os("OKYTES_INSTALLED_CLI")
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_okytes-cli").into());
    let mut c = Command::new(binary);
    c.arg("--db").arg(db).arg("--json");
    c
}
async fn fixture(
    dir: &std::path::Path,
    server: &TestServer,
) -> (Engine, Arc<SqliteStore>, uuid::Uuid) {
    let config = EngineConfig {
        database_path: dir.join("db.sqlite"),
        max_segment_size: 512 * 1024,
        min_segment_size: 64 * 1024,
        max_retries: 1,
        retry_base_delay_ms: 1,
        ..Default::default()
    };
    let store = Arc::new(SqliteStore::open(&config.database_path).unwrap());
    store.set_config(&config).await.unwrap();
    let engine = Engine::new(
        config.clone(),
        Arc::new(HttpTransport::new(&config).unwrap()),
        store.clone(),
        Arc::new(okytes_persistence::files::LocalFiles),
    )
    .unwrap();
    let id = engine
        .add(DownloadRequest {
            url: server.url("/normal").parse().unwrap(),
            output_dir: dir.into(),
            filename: Some("result.bin".into()),
            expected_hash: Some(server.content_hash.clone()),
            conflict_strategy: ConflictStrategy::Fail,
            headers: vec![],
            priority: 0,
        })
        .await
        .unwrap();
    (engine, store, id)
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn forced_process_kill_resumes_identically_and_changed_etag_is_rejected() {
    for changed in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("db.sqlite");
        let server = TestServer::start(ServerConfig {
            file_size: 8 * 1024 * 1024,
            frame_delay: Duration::from_millis(10),
            ..Default::default()
        })
        .await
        .unwrap();
        let (engine, store, id) = fixture(dir.path(), &server).await;
        let mut runner = Process(
            command(&db)
                .arg("run")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if engine.status(id).await.unwrap().bytes_downloaded >= 256 * 1024 {
                    break;
                }
                assert!(runner.0.try_wait().unwrap().is_none());
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        // std::process::Child::kill is SIGKILL on Unix, TerminateProcess on Windows.
        runner.0.kill().unwrap();
        assert!(!runner.0.wait().unwrap().success());
        let before = store
            .load_segments(id)
            .await
            .unwrap()
            .iter()
            .map(|s| s.bytes_downloaded)
            .sum::<u64>();
        assert!(before > 0);
        if changed {
            server.set_version(2);
        }
        let output = command(&db).arg("run").output().unwrap();
        if changed {
            assert!(!output.status.success());
            assert_eq!(engine.status(id).await.unwrap().state, JobState::Failed);
            assert!(!dir.path().join("result.bin").exists());
            assert_eq!(
                before,
                store
                    .load_segments(id)
                    .await
                    .unwrap()
                    .iter()
                    .map(|s| s.bytes_downloaded)
                    .sum::<u64>()
            );
        } else {
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                hash_file(&dir.path().join("result.bin")).await.unwrap(),
                server.content_hash
            );
        }
        server.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn separate_cli_controls_and_exclusive_runner_ownership() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("db.sqlite");
    let server = TestServer::start(ServerConfig {
        file_size: 8 * 1024 * 1024,
        frame_delay: Duration::from_millis(10),
        ..Default::default()
    })
    .await
    .unwrap();
    let (engine, _, id) = fixture(dir.path(), &server).await;
    let mut runner = Process(
        command(&db)
            .arg("run")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if engine.status(id).await.unwrap().bytes_downloaded > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let duplicate = command(&db).arg("run").output().unwrap();
    assert!(!duplicate.status.success());
    let paused = command(&db)
        .arg("pause")
        .arg(id.to_string())
        .output()
        .unwrap();
    assert!(
        paused.status.success(),
        "{}",
        String::from_utf8_lossy(&paused.stderr)
    );
    assert!(runner.0.wait().unwrap().success());
    assert_eq!(engine.status(id).await.unwrap().state, JobState::Paused);
    assert!(command(&db)
        .arg("resume")
        .arg(id.to_string())
        .arg("--run")
        .status()
        .unwrap()
        .success());
    assert_eq!(
        hash_file(&dir.path().join("result.bin")).await.unwrap(),
        server.content_hash
    );
    server.shutdown().await;
}

fn peak_memory(id: u32) -> u64 {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string(format!("/proc/{id}/status")).unwrap_or_default();
        status
            .lines()
            .find_map(|line| {
                line.strip_prefix("VmHWM:")
                    .and_then(|s| s.split_whitespace().next())
                    .and_then(|s| s.parse::<u64>().ok())
            })
            .unwrap_or(0)
            * 1024
    }
    #[cfg(target_os = "windows")]
    {
        let output = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!("(Get-Process -Id {id}).PeakWorkingSet64"),
            ])
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .unwrap_or(0)
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        let _ = id;
        0
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cli_json_queue_config_and_cancel_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("db.sqlite");
    let server = TestServer::start(ServerConfig::default()).await.unwrap();
    let (engine, store, id) = fixture(dir.path(), &server).await;
    let queued = command(&db)
        .args(["queue", &id.to_string(), "17"])
        .output()
        .unwrap();
    assert!(queued.status.success());
    assert_eq!(engine.status(id).await.unwrap().priority, 17);
    let listed = command(&db).arg("list").output().unwrap();
    let records: Vec<okytes_core::types::JobRecord> =
        serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(records[0].id, id);
    let mut config = store.config().await.unwrap();
    config.max_concurrent_jobs = 2;
    let settings = dir.path().join("settings.json");
    std::fs::write(&settings, serde_json::to_vec(&config).unwrap()).unwrap();
    assert!(command(&db)
        .arg("config")
        .arg("--set")
        .arg(&settings)
        .output()
        .unwrap()
        .status
        .success());
    assert_eq!(store.config().await.unwrap().max_concurrent_jobs, 2);
    config.max_concurrent_jobs = 0;
    std::fs::write(&settings, serde_json::to_vec(&config).unwrap()).unwrap();
    let rejected = command(&db)
        .arg("config")
        .arg("--set")
        .arg(&settings)
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    let error: serde_json::Value = serde_json::from_slice(&rejected.stderr).unwrap();
    assert!(error["error"].is_string());
    assert_eq!(store.config().await.unwrap().max_concurrent_jobs, 2);
    assert!(command(&db)
        .arg("cancel")
        .arg(id.to_string())
        .output()
        .unwrap()
        .status
        .success());
    assert_eq!(engine.status(id).await.unwrap().state, JobState::Cancelled);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "writes and verifies a 2 GiB file; run explicitly for milestone acceptance"]
async fn multi_gigabyte_transfer_with_hundreds_queued_has_bounded_memory() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("db.sqlite");
    let size = 2usize * 1024 * 1024 * 1024 + 1024;
    let server = TestServer::start(ServerConfig {
        file_size: size,
        ..Default::default()
    })
    .await
    .unwrap();
    let (engine, _, first) = fixture(dir.path(), &server).await;
    engine.pause(first).await.unwrap();
    for n in 0..400 {
        engine
            .add(DownloadRequest {
                url: server.url("/normal").parse().unwrap(),
                output_dir: dir.path().into(),
                filename: Some(format!("queued-{n}")),
                expected_hash: None,
                conflict_strategy: ConflictStrategy::Fail,
                headers: vec![],
                priority: 0,
            })
            .await
            .unwrap();
    }
    let mut runner = Process(
        command(&db)
            .arg("add")
            .arg(server.url("/normal"))
            .arg("--output")
            .arg(dir.path())
            .arg("--filename")
            .arg("large.bin")
            .arg("--sha256")
            .arg(&server.content_hash)
            .arg("--run")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut peak = 0;
    let deadline = std::time::Instant::now() + Duration::from_secs(600);
    loop {
        peak = peak.max(peak_memory(runner.0.id()));
        if let Some(status) = runner.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "stress transfer exceeded ten minutes"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(peak > 0, "platform memory measurement unavailable");
    assert!(
        peak < 128 * 1024 * 1024,
        "peak resident memory {peak} exceeds 128 MiB"
    );
    assert_eq!(
        std::fs::metadata(dir.path().join("large.bin"))
            .unwrap()
            .len(),
        size as u64
    );
    assert_eq!(
        hash_file(&dir.path().join("large.bin")).await.unwrap(),
        server.content_hash
    );
    eprintln!("stress evidence: bytes={size}, queued=400, peak_resident_bytes={peak}");
    server.shutdown().await;
}
