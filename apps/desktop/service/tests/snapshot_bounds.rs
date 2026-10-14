use okytes_core::{
    desktop::DesktopService,
    state::{JobState, SegmentState},
    store::JobStore,
    types::{ConflictStrategy, JobRecord, RemoteIdentity, SegmentRecord},
};
use okytes_desktop_service::Session;
use okytes_persistence::SqliteStore;
#[tokio::test]
async fn media_diagnostics_paginate_by_encoded_size_without_losing_jobs() {
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("jobs.db");
    let session = Session::open(&database, root.path()).await.unwrap();
    let store = SqliteStore::open(&database).unwrap();
    for index in 0..128 {
        let id = uuid::Uuid::new_v4();
        let now = chrono::Utc::now();
        let job = JobRecord {
            protocol: "hls+http".into(),
            capture_id: None,
            requires_session: false,
            use_server_filename: false,
            verified_sha256: None,
            id,
            url: format!("hls+http://127.0.0.1:9/media-{index}.m3u8")
                .parse()
                .unwrap(),
            output_path: root.path().join(format!("media-{index}.ts")),
            partial_path: root.path().join(format!("{id}.part")),
            total_size: Some(1024),
            remote_identity: RemoteIdentity::empty(),
            state: JobState::Paused,
            expected_hash: None,
            conflict_strategy: ConflictStrategy::Fail,
            headers: vec![],
            priority: 0,
            bytes_downloaded: 0,
            created_at: now,
            updated_at: now,
            retry_count: 0,
            error_message: None,
        };
        store.save_job(&job).await.unwrap();
        let segments = (0..1024)
            .map(|start| SegmentRecord {
                id: uuid::Uuid::new_v4(),
                job_id: id,
                start,
                end: start,
                bytes_downloaded: 0,
                state: SegmentState::Pending,
                last_checkpoint: now,
            })
            .collect::<Vec<_>>();
        store.save_segments(&segments).await.unwrap();
    }
    let mut offset = 0;
    let mut ids = std::collections::HashSet::new();
    let mut pages = 0;
    loop {
        let page = session.list(offset).await.unwrap();
        assert!(serde_json::to_vec(&page).unwrap().len() <= 4 * 1024 * 1024);
        pages += 1;
        for job in page.jobs {
            assert_eq!(job.segments.len(), 1024);
            assert!(ids.insert(job.id));
        }
        let Some(next) = page.next else { break };
        assert!(next > offset);
        offset = next;
    }
    assert_eq!(ids.len(), 128);
    assert!(pages > 1);
    session.shutdown().await.unwrap();
}
