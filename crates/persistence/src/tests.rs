use super::*;
use okytes_core::types::{ConflictStrategy, RemoteIdentity};
use std::sync::atomic::{AtomicBool, Ordering};
use uuid::Uuid;

static PROGRESS_WAITING: AtomicBool = AtomicBool::new(false);

fn progress_busy(attempt: i32) -> bool {
    PROGRESS_WAITING.store(true, Ordering::Release);
    std::thread::sleep(std::time::Duration::from_millis(1));
    attempt < 5000
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn torrent_progress_cannot_overwrite_another_connections_cancellation() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("profile.sqlite");
    let store = SqliteStore::open(&path).unwrap();
    let id = Uuid::new_v4();
    let mut job = JobRecord {
        protocol: "torrent".into(),
        capture_id: None,
        requires_session: false,
        use_server_filename: false,
        verified_sha256: None,
        id,
        url: "magnet:?xt=urn:btih:0123456789012345678901234567890123456789"
            .parse()
            .unwrap(),
        output_path: root.path().join("output"),
        partial_path: root.path().join("partial"),
        total_size: Some(128),
        remote_identity: RemoteIdentity {
            etag: None,
            last_modified: None,
            content_length: Some(128),
        },
        state: JobState::Downloading,
        expected_hash: None,
        conflict_strategy: ConflictStrategy::Fail,
        headers: vec![],
        priority: 0,
        bytes_downloaded: 0,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        retry_count: 0,
        error_message: None,
    };
    store.create_torrent(&job, "{}", &[]).await.unwrap();
    PROGRESS_WAITING.store(false, Ordering::Release);
    store
        .call(|conn| conn.busy_handler(Some(progress_busy)).map_err(db))
        .await
        .unwrap();

    let mut controller = Connection::open(&path).unwrap();
    let tx = controller
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let progress = tokio::spawn({
        let store = store.clone();
        async move { store.torrent_progress(id, 32, 128).await }
    });
    // A real SQLite busy callback proves the progress writer reached the held
    // write lock; no timing guess determines which record it read. Without an
    // IMMEDIATE transaction it already read Downloading before blocking here.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !PROGRESS_WAITING.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    job.state = JobState::Cancelled;
    save_job(&tx, &job).unwrap();
    tx.execute("UPDATE torrent_jobs SET descriptor=json_set(descriptor,'$.stop_requested',json('true')) WHERE job_id=?1",[id.to_string()]).unwrap();
    tx.commit().unwrap();
    progress.await.unwrap().unwrap();
    let current = store.load_job(id).await.unwrap().unwrap();
    assert_eq!(current.state, JobState::Cancelled);
    assert_eq!(current.bytes_downloaded, 32);
    assert!(store.torrent_stop_requested(id).await.unwrap());
}
