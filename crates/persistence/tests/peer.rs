use okytes_core::peer::{Direction, Entry, Hash, Manifest, State};
use okytes_persistence::SqliteStore;
use uuid::Uuid;

fn manifest(size: u64) -> Manifest {
    Manifest::new(vec![
        Entry::Directory {
            path: "empty".into(),
        },
        Entry::File {
            path: "file.txt".into(),
            size,
            sha256: Hash::digest(b"file"),
        },
    ])
    .unwrap()
}
fn peer() -> Hash {
    Hash::digest(b"authenticated test peer certificate")
}

#[tokio::test]
async fn offers_survive_restart_without_creating_runnable_http_jobs() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("profile.sqlite");
    let store = SqliteStore::open(&path).unwrap();
    let id = Uuid::new_v4();
    let request = store
        .peer_offer(
            id,
            peer(),
            Direction::Inbound,
            manifest(5 * 1024 * 1024 * 1024),
        )
        .await
        .unwrap();
    assert_eq!(request.state, State::AwaitingApproval);
    assert_eq!(request.revision, 0);
    assert!(store.page(0).await.unwrap().is_empty());
    drop(store);
    let store = SqliteStore::open(&path).unwrap();
    assert_eq!(store.peer_request(id).await.unwrap().unwrap(), request);
    assert_eq!(store.peer_requests(0).await.unwrap(), vec![request]);
    assert!(store.peer_request(Uuid::new_v4()).await.unwrap().is_none());
}

#[tokio::test]
async fn repeated_offer_is_idempotent_but_changed_peer_or_manifest_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(dir.path().join("profile.sqlite")).unwrap();
    let id = Uuid::new_v4();
    store
        .peer_offer(id, peer(), Direction::Inbound, manifest(10))
        .await
        .unwrap();
    let approved = store
        .peer_transition(id, peer(), 0, State::Negotiating)
        .await
        .unwrap();
    assert_eq!(
        store
            .peer_offer(id, peer(), Direction::Inbound, manifest(10))
            .await
            .unwrap(),
        approved
    );
    assert!(store
        .peer_offer(
            id,
            Hash::digest(b"another certificate"),
            Direction::Inbound,
            manifest(10)
        )
        .await
        .is_err());
    assert!(store
        .peer_offer(id, peer(), Direction::Outbound, manifest(10))
        .await
        .is_err());
    assert!(store
        .peer_offer(id, peer(), Direction::Inbound, manifest(11))
        .await
        .is_err());
    assert_eq!(store.peer_requests(0).await.unwrap().len(), 1);
}

#[tokio::test]
async fn concurrent_approval_and_rejection_commit_only_one_decision() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(dir.path().join("profile.sqlite")).unwrap();
    let id = Uuid::new_v4();
    store
        .peer_offer(id, peer(), Direction::Inbound, manifest(1))
        .await
        .unwrap();
    let (accepted, rejected) = tokio::join!(
        store.peer_transition(id, peer(), 0, State::Negotiating),
        store.peer_transition(id, peer(), 0, State::Rejected)
    );
    assert_ne!(accepted.is_ok(), rejected.is_ok());
    let current = store.peer_request(id).await.unwrap().unwrap();
    assert_eq!(current.revision, 1);
    assert_eq!(
        store
            .peer_transition(id, peer(), 0, current.state)
            .await
            .unwrap(),
        current
    );
    assert_eq!(
        store
            .peer_transition(id, peer(), 1, current.state)
            .await
            .unwrap(),
        current
    );
}

#[tokio::test]
async fn stale_messages_cannot_reopen_cancelled_or_rejected_requests() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(dir.path().join("profile.sqlite")).unwrap();
    let id = Uuid::new_v4();
    store
        .peer_offer(id, peer(), Direction::Inbound, manifest(1))
        .await
        .unwrap();
    store
        .peer_transition(id, peer(), 0, State::Negotiating)
        .await
        .unwrap();
    store
        .peer_transition(id, peer(), 1, State::Cancelled)
        .await
        .unwrap();
    assert!(store
        .peer_transition(id, peer(), 0, State::Negotiating)
        .await
        .is_err());
    assert!(store
        .peer_transition(id, peer(), 2, State::Negotiating)
        .await
        .is_err());
    assert!(store
        .peer_transition(id, Hash::digest(b"wrong peer"), 2, State::Cancelled)
        .await
        .is_err());
    assert_eq!(
        store.peer_request(id).await.unwrap().unwrap().state,
        State::Cancelled
    );
}

#[tokio::test]
async fn concurrent_offer_replays_share_one_record_and_invalid_metadata_never_lands() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(dir.path().join("profile.sqlite")).unwrap();
    let id = Uuid::new_v4();
    let (first, second) = tokio::join!(
        store.peer_offer(id, peer(), Direction::Outbound, manifest(1)),
        store.peer_offer(id, peer(), Direction::Outbound, manifest(1))
    );
    assert_eq!(first.unwrap(), second.unwrap());
    assert!(store
        .peer_offer(Uuid::nil(), peer(), Direction::Inbound, manifest(1))
        .await
        .is_err());
    let bad = Manifest {
        entries: vec![Entry::Directory {
            path: "../escape".into(),
        }],
    };
    assert!(store
        .peer_offer(Uuid::new_v4(), peer(), Direction::Inbound, bad)
        .await
        .is_err());
    assert_eq!(store.peer_requests(0).await.unwrap().len(), 1);
    assert!(store
        .peer_transition(id, peer(), 0, State::Transferring)
        .await
        .is_err());
    assert!(store
        .peer_transition(Uuid::new_v4(), peer(), 0, State::Cancelled)
        .await
        .is_err());
}

#[tokio::test]
async fn history_is_paged_and_bounded_and_migrates_an_existing_profile() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("profile.sqlite");
    // Simulate the pre-M10 schema with a real legacy preference.
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE jobs(id TEXT PRIMARY KEY, record TEXT NOT NULL, priority INTEGER NOT NULL, state TEXT NOT NULL);CREATE TABLE segments(id TEXT PRIMARY KEY, job_id TEXT NOT NULL, record TEXT NOT NULL);CREATE TABLE settings(key TEXT PRIMARY KEY,value TEXT NOT NULL);INSERT INTO settings VALUES ('legacy','kept');PRAGMA user_version=1;").unwrap();
    drop(conn);
    let store = SqliteStore::open(&path).unwrap();
    assert_eq!(
        store.preference("legacy").await.unwrap().as_deref(),
        Some("kept")
    );
    for _ in 0..65 {
        store
            .peer_offer(Uuid::new_v4(), peer(), Direction::Inbound, manifest(1))
            .await
            .unwrap();
    }
    assert_eq!(store.peer_requests(0).await.unwrap().len(), 64);
    assert_eq!(store.peer_requests(64).await.unwrap().len(), 1);
    assert!(store.peer_requests(65).await.unwrap().is_empty());
    assert!(store.peer_requests(4097).await.is_err());
    // Fill the remainder directly to exercise the hard admission cap cheaply.
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("WITH RECURSIVE n(i) AS (SELECT 65 UNION ALL SELECT i+1 FROM n WHERE i<4095) INSERT INTO peer_requests SELECT 'cap-'||i,'{}' FROM n;").unwrap();
    drop(conn);
    assert!(store
        .peer_offer(Uuid::new_v4(), peer(), Direction::Inbound, manifest(1))
        .await
        .is_err());
}

#[tokio::test]
async fn payload_checkpoints_are_bounded_idempotent_and_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("profile.sqlite");
    let store = SqliteStore::open(&path).unwrap();
    let id = Uuid::new_v4();
    store
        .peer_offer(id, peer(), Direction::Inbound, manifest(600_000))
        .await
        .unwrap();
    assert!(store
        .peer_checkpoint(id, peer(), "file.txt".into(), 0, 1)
        .await
        .is_err());
    store
        .peer_transition(id, peer(), 0, State::Negotiating)
        .await
        .unwrap();
    store
        .peer_transition(id, peer(), 1, State::Transferring)
        .await
        .unwrap();
    assert_eq!(
        store
            .peer_checkpoint(id, peer(), "file.txt".into(), 0, 262_144)
            .await
            .unwrap(),
        262_144
    );
    assert_eq!(
        store
            .peer_checkpoint(id, peer(), "file.txt".into(), 0, 262_144)
            .await
            .unwrap(),
        262_144
    );
    assert!(store
        .peer_checkpoint(id, peer(), "file.txt".into(), 0, 262_145)
        .await
        .is_err());
    assert!(store
        .peer_checkpoint(id, peer(), "empty".into(), 0, 1)
        .await
        .is_err());
    assert!(store
        .peer_checkpoint(
            id,
            Hash::digest(b"other peer"),
            "file.txt".into(),
            262_144,
            262_145,
        )
        .await
        .is_err());
    drop(store);

    let store = SqliteStore::open(&path).unwrap();
    let offsets = store.peer_offsets(id, peer()).await.unwrap();
    assert_eq!(offsets.get("file.txt"), Some(&262_144));
    assert!(store
        .peer_offsets(id, Hash::digest(b"other peer"))
        .await
        .is_err());
    store
        .peer_transition(id, peer(), 2, State::Paused)
        .await
        .unwrap();
    assert!(store
        .peer_checkpoint(id, peer(), "file.txt".into(), 262_144, 262_145)
        .await
        .is_err());
}
