use okytes_transport_torrent::metainfo;
fn multi(names: &[&str]) -> Vec<u8> {
    let mut bytes = b"d4:infod5:filesl".to_vec();
    for name in names {
        bytes.extend_from_slice(b"d6:lengthi16384e4:pathl");
        bytes.extend_from_slice(format!("{}:{name}", name.len()).as_bytes());
        bytes.extend_from_slice(b"ee");
    }
    bytes.extend_from_slice(b"e4:name4:test12:piece lengthi16384e6:pieces");
    bytes.extend_from_slice(format!("{}:", names.len() * 20).as_bytes());
    bytes.extend(vec![0; names.len() * 20]);
    bytes.extend_from_slice(b"ee");
    bytes
}
#[test]
fn hostile_paths_duplicates_and_counts_are_rejected() {
    assert!(metainfo::validate(&multi(&["safe.bin"])).is_ok());
    for path in [
        "..",
        "../escape",
        "/escape",
        "C:\\escape",
        "\\\\host\\share",
        "CON",
        "NUL.txt",
        "trailing.",
        "trailing ",
        "a/b",
        "a\\b",
    ] {
        assert!(metainfo::validate(&multi(&[path])).is_err(), "{path}");
    }
    assert!(metainfo::validate(&multi(&["a.bin", "a.bin"])).is_err());
    assert!(metainfo::validate(&multi(&["A.bin", "a.bin"])).is_err());
    assert!(metainfo::validate(&multi(&[&"x".repeat(201)])).is_err());
    assert!(metainfo::validate(&multi(&vec!["a"; 65])).is_err());
    assert!(metainfo::validate(&[b'l'; 10000]).is_err());
    let raw = multi(&["safe.bin"]);
    let zero =
        String::from_utf8_lossy(&raw).replace("12:piece lengthi16384e", "12:piece lengthi0e");
    assert!(metainfo::validate(zero.as_bytes()).is_err());
}

#[tokio::test]
async fn rejected_metadata_creates_no_job_or_output() {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use okytes_core::{
        store::JobStore,
        traffic::{TrafficController, TrafficPolicy},
    };
    use okytes_torrent_types::{Input, TorrentService};
    use std::sync::Arc;
    let root = tempfile::tempdir().unwrap();
    let store =
        Arc::new(okytes_persistence::SqliteStore::open(root.path().join("jobs.sqlite")).unwrap());
    let (events, _) = tokio::sync::broadcast::channel(8);
    let adapter = okytes_transport_torrent::Adapter::new(
        store.clone(),
        root.path().join("private"),
        root.path().join("output"),
        Arc::new(TrafficController::new(TrafficPolicy::default()).unwrap()),
        TrafficPolicy::default(),
        events,
    )
    .unwrap();
    let request = Input {
        magnet: None,
        metainfo: Some(STANDARD.encode(multi(&["../outside"]))),
        options: Default::default(),
        destination: None,
    };
    assert!(adapter.add_torrent(request).await.is_err());
    assert!(store.list_jobs(None).await.unwrap().is_empty());
    assert!(!root.path().join("private").exists());
    assert!(!root.path().join("output").exists());
}
