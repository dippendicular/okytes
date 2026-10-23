use okytes_desktop_service::Session;
use okytes_persistence::SqliteStore;

#[tokio::test]
async fn rejected_engine_contender_does_not_create_or_migrate_database() {
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("jobs.db");
    // Model a native owner between acquiring its OS lease and opening SQLite.
    let owner = SqliteStore::acquire_engine(&database).unwrap();
    assert!(Session::open(&database, root.path()).await.is_err());
    assert!(
        !database.exists(),
        "losing contender opened a writable store"
    );
    drop(owner);
    let session = Session::open(&database, root.path()).await.unwrap();
    session.shutdown().await.unwrap();
}
