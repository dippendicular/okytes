use okytes_core::{
    engine::JobStore,
    state::{JobState, SegmentState},
    types::{ConflictStrategy, JobRecord, RemoteIdentity, SegmentRecord},
};
use okytes_persistence::SqliteStore;
use proptest::prelude::*;
use uuid::Uuid;
fn job(id: Uuid) -> JobRecord {
    JobRecord {
        protocol: "http".into(),
        capture_id: None,
        requires_session: false,
        use_server_filename: false,
        verified_sha256: None,
        id,
        url: "https://example.com/file".parse().unwrap(),
        output_path: "file".into(),
        partial_path: "partial".into(),
        total_size: Some(1024),
        remote_identity: RemoteIdentity {
            etag: Some("\"v1\"".into()),
            last_modified: None,
            content_length: Some(1024),
        },
        state: JobState::Queued,
        expected_hash: None,
        conflict_strategy: ConflictStrategy::Fail,
        headers: vec![("Authorization".into(), "SECRET".into())],
        priority: 0,
        bytes_downloaded: 0,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        retry_count: 0,
        error_message: None,
    }
}
proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]
    #[test] fn checkpoint_recovery_roundtrip_is_idempotent(bytes in 0u64..=1024) {
        let runtime=tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let dir=tempfile::tempdir().unwrap();let path=dir.path().join("store.sqlite");let store=SqliteStore::open(&path).unwrap();let id=Uuid::new_v4();store.save_job(&job(id)).await.unwrap();
            let segment=SegmentRecord {id:Uuid::new_v4(),job_id:id,start:0,end:1023,bytes_downloaded:bytes,state:SegmentState::Active,last_checkpoint:chrono::Utc::now()};
            store.save_segments(std::slice::from_ref(&segment)).await.unwrap();store.checkpoint_segment(&segment).await.unwrap();drop(store);
            let reopened=SqliteStore::open(&path).unwrap();let loaded=reopened.load_segments(id).await.unwrap();assert_eq!(loaded.len(),1);assert_eq!(loaded[0].bytes_downloaded,bytes);assert!(reopened.load_job(id).await.unwrap().unwrap().headers.is_empty());
        });
    }
}
#[tokio::test]
async fn control_transition_cannot_be_overwritten_by_stale_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(dir.path().join("db")).unwrap();
    let id = Uuid::new_v4();
    let old = job(id);
    store.save_job(&old).await.unwrap();
    store.transition_job(id, JobState::Paused).await.unwrap();
    assert!(store.save_job(&old).await.is_err());
    assert_eq!(
        store.load_job(id).await.unwrap().unwrap().state,
        JobState::Paused
    );
}
#[test]
fn process_lock_has_single_owner_and_is_released_on_drop() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let owner = SqliteStore::acquire_engine(&path).unwrap();
    assert!(SqliteStore::acquire_engine(&path).is_err());
    drop(owner);
    assert!(SqliteStore::acquire_engine(&path).is_ok());
}
