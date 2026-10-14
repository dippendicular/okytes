//! Generated payloads and maintained librqbit peers only. No public discovery.
use base64::{engine::general_purpose::STANDARD, Engine as _};
use librqbit::{
    create_torrent, spawn_utils::BlockingSpawner, AddTorrent, AddTorrentOptions,
    CreateTorrentOptions, ListenerOptions, Session, SessionOptions,
};
use okytes_core::{
    state::JobState,
    store::JobStore,
    traffic::{TrafficController, TrafficPolicy},
};
use okytes_persistence::SqliteStore;
use okytes_torrent_types::{Input, Options, TorrentService};
use okytes_transport_torrent::Adapter;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_download_selective_magnet_and_durable_verification() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("generated");
    std::fs::create_dir(&source).unwrap();
    let payload: Vec<u8> = (0..200000).map(|i| (i * 17 % 251) as u8).collect();
    std::fs::write(source.join("chosen.bin"), &payload).unwrap();
    std::fs::write(source.join("other.bin"), vec![13; 23000]).unwrap();
    let store = Arc::new(SqliteStore::open(temp.path().join("jobs.sqlite")).unwrap());
    let (events, _) = tokio::sync::broadcast::channel(64);
    let adapter = Adapter::new(
        store.clone(),
        temp.path().join("private"),
        temp.path().join("out"),
        Arc::new(TrafficController::new(TrafficPolicy::default()).unwrap()),
        TrafficPolicy::default(),
        events,
    )
    .unwrap();
    let meta = create_torrent(
        &source,
        CreateTorrentOptions {
            name: Some("generated"),
            piece_length: Some(16384),
            ..Default::default()
        },
        &BlockingSpawner::new(2),
    )
    .await
    .unwrap();
    let raw = meta.as_bytes().unwrap();
    let seed = Session::new_with_opts(
        source.clone(),
        SessionOptions {
            dht: None,
            disable_local_service_discovery: true,
            listen: Some(ListenerOptions {
                listen_addr: "127.0.0.1:0".parse().unwrap(),
                ..Default::default()
            }),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let handle = seed
        .add_torrent(
            AddTorrent::from_bytes(raw.clone()),
            Some(AddTorrentOptions {
                output_folder: Some(source.to_string_lossy().into_owned()),
                overwrite: true,
                disable_pex: true,
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    handle.wait_until_initialized().await.unwrap();
    assert!(handle.stats().finished);
    for magnet in [false, true] {
        let mut options = Options {
            initial_peers: vec![seed.listen_addr().unwrap().to_string()],
            ..Default::default()
        };
        let preview = adapter
            .inspect_torrent(Input {
                metainfo: Some(STANDARD.encode(&raw)),
                magnet: None,
                destination: None,
                options: options.clone(),
            })
            .await
            .unwrap();
        options.files = Some(vec![
            preview
                .files
                .iter()
                .find(|f| f.path == "chosen.bin")
                .unwrap()
                .index,
        ]);
        let input = Input {
            magnet: magnet.then(|| meta.as_magnet().to_string()),
            metainfo: (!magnet).then(|| STANDARD.encode(&raw)),
            destination: None,
            options,
        };
        let id = adapter.add_torrent(input).await.unwrap().parse().unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(20), adapter.execute_job(id))
            .await
            .unwrap()
            .unwrap();
        let job = store.load_job(id).await.unwrap().unwrap();
        assert_eq!(job.state, JobState::Completed);
        assert_eq!(
            std::fs::read(job.output_path.join("chosen.bin")).unwrap(),
            payload
        );
        assert!(!job.output_path.join("other.bin").exists());
        let diagnostics = adapter.torrent(id.to_string()).await.unwrap();
        assert_eq!(diagnostics.activity, "stopped");
        assert!(diagnostics.verified.iter().any(|v| *v));
        assert_eq!(job.bytes_downloaded, payload.len() as u64);
    }
    seed.stop().await;
}
