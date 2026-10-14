mod swarm_support;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use okytes_core::{
    service::DownloadService, state::JobState, store::JobStore, traffic::TrafficPolicy,
};
use okytes_torrent_types::{Input, Options, TorrentService};
use std::{
    sync::atomic::Ordering,
    time::{Duration, Instant},
};
use swarm_support::*;
fn input(seed: &Seeder, options: Options, magnet: bool) -> Input {
    Input {
        magnet: magnet.then(|| seed.magnet.clone()),
        metainfo: (!magnet).then(|| STANDARD.encode(&seed.raw)),
        options,
        destination: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn separate_controller_stops_a_live_owner_and_clears_cancelled_pieces() {
    let root = tempfile::tempdir().unwrap();
    let seed = Seeder::new(&root.path().join("seed"), 262144, None, false).await;
    let (owner, store) = adapter(root.path(), TrafficPolicy::default());
    let (controller, _) = adapter(root.path(), TrafficPolicy::default());
    let id: uuid::Uuid = owner
        .add_torrent(input(
            &seed,
            Options {
                download_bps: Some(65536),
                initial_peers: vec![seed.session.listen_addr().unwrap().to_string()],
                ..Default::default()
            },
            false,
        ))
        .await
        .unwrap()
        .parse()
        .unwrap();
    let worker = tokio::spawn({
        let owner = owner.clone();
        async move { owner.execute_job(id).await }
    });
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if store
                .torrent_pieces(id, 16)
                .await
                .unwrap()
                .iter()
                .any(|bit| *bit)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    controller.cancel(id).await.unwrap();
    worker.await.unwrap().unwrap();
    let job = store.load_job(id).await.unwrap().unwrap();
    assert_eq!(job.state, JobState::Cancelled);
    assert!(!job.partial_path.exists());
    assert!(store
        .torrent_pieces(id, 16)
        .await
        .unwrap()
        .iter()
        .all(|bit| !*bit));
    assert!(!store.torrent_active(id).await.unwrap());
    seed.session.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dropping_a_worker_quiesces_before_resume() {
    let root = tempfile::tempdir().unwrap();
    let seed = Seeder::new(&root.path().join("seed"), 131072, None, false).await;
    let (owner, store) = adapter(root.path(), TrafficPolicy::default());
    let id: uuid::Uuid = owner
        .add_torrent(input(
            &seed,
            Options {
                download_bps: Some(65536),
                initial_peers: vec![seed.session.listen_addr().unwrap().to_string()],
                ..Default::default()
            },
            false,
        ))
        .await
        .unwrap()
        .parse()
        .unwrap();
    let worker = tokio::spawn({
        let owner = owner.clone();
        async move { owner.execute_job(id).await }
    });
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if store.load_job(id).await.unwrap().unwrap().bytes_downloaded > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    worker.abort();
    assert!(worker.await.unwrap_err().is_cancelled());
    owner.pause(id).await.unwrap();
    assert!(!store.torrent_active(id).await.unwrap());
    owner.resume(id).await.unwrap();
    tokio::time::timeout(Duration::from_secs(15), owner.execute_job(id))
        .await
        .unwrap()
        .unwrap();
    let job = store.load_job(id).await.unwrap().unwrap();
    assert_eq!(
        std::fs::read(job.output_path.join("payload.bin")).unwrap(),
        seed.payload
    );
    seed.session.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn seeding_ratio_and_time_bound_real_uploads() {
    use librqbit::{AddTorrent, AddTorrentOptions, Session, SessionOptions};
    let root = tempfile::tempdir().unwrap();
    let seed = Seeder::new(&root.path().join("seed"), 262144, None, false).await;
    for ratio in [Some(0.25), None] {
        let profile = root
            .path()
            .join(if ratio.is_some() { "ratio" } else { "time" });
        std::fs::create_dir(&profile).unwrap();
        let (adapter, store) = adapter(
            &profile,
            TrafficPolicy {
                upload_bps: Some(32768),
                ..Default::default()
            },
        );
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let mut options = Options {
            seed_seconds: Some(4),
            seed_ratio: ratio,
            upload_bps: Some(65536),
            initial_peers: vec![seed.session.listen_addr().unwrap().to_string()],
            ..Default::default()
        };
        options.privacy.listen = address.to_string();
        let id: uuid::Uuid = adapter
            .add_torrent(input(&seed, options, false))
            .await
            .unwrap()
            .parse()
            .unwrap();
        let worker = tokio::spawn({
            let adapter = adapter.clone();
            async move { adapter.execute_job(id).await }
        });
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if store.load_job(id).await.unwrap().unwrap().state == JobState::Completed {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let leecher = Session::new_with_opts(
            profile.join("leecher"),
            SessionOptions {
                dht: None,
                disable_local_service_discovery: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let handle = leecher
            .add_torrent(
                AddTorrent::from_bytes(seed.raw.clone()),
                Some(AddTorrentOptions {
                    initial_peers: Some(vec![address]),
                    disable_pex: true,
                    ..Default::default()
                }),
            )
            .await
            .unwrap()
            .into_handle()
            .unwrap();
        handle.wait_until_initialized().await.unwrap();
        let start = Instant::now();
        tokio::time::timeout(Duration::from_secs(8), worker)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let uploaded = adapter
            .torrent(id.to_string())
            .await
            .unwrap()
            .uploaded_bytes;
        assert!(uploaded > 0, "real leecher must receive payload");
        assert!(uploaded <= 5 * 32768, "global upload bucket bound");
        assert!(!handle.stats().finished);
        if ratio.is_some() {
            assert!(uploaded <= 65536, "ratio hard cap");
        } else {
            assert!(
                start.elapsed() >= Duration::from_secs(3),
                "time rule must allow selected seed interval"
            );
        }
        leecher.stop().await;
    }
    seed.session.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tracker_magnet_pause_resume_and_shared_limit() {
    let root = tempfile::tempdir().unwrap();
    let tracker = Tracker::new().await;
    let seed = Seeder::new(&root.path().join("seed"), 262144, Some(&tracker), false).await;
    let (adapter, store) = adapter(
        root.path(),
        TrafficPolicy {
            global_bps: Some(131072),
            ..Default::default()
        },
    );
    let id: uuid::Uuid = adapter
        .add_torrent(input(
            &seed,
            Options {
                download_bps: Some(65536),
                ..Default::default()
            },
            true,
        ))
        .await
        .unwrap()
        .parse()
        .unwrap();
    let worker = tokio::spawn({
        let adapter = adapter.clone();
        async move { adapter.execute_job(id).await }
    });
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if store.load_job(id).await.unwrap().unwrap().bytes_downloaded > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    adapter.pause(id).await.unwrap();
    worker.await.unwrap().unwrap();
    assert_eq!(
        store.load_job(id).await.unwrap().unwrap().state,
        JobState::Paused
    );
    assert!(tracker.announces.load(Ordering::Relaxed) > 0);
    adapter.resume(id).await.unwrap();
    let before = Instant::now();
    tokio::time::timeout(Duration::from_secs(15), adapter.execute_job(id))
        .await
        .unwrap()
        .unwrap();
    assert!(before.elapsed() >= Duration::from_secs(1));
    let job = store.load_job(id).await.unwrap().unwrap();
    assert_eq!(
        std::fs::read(job.output_path.join("payload.bin")).unwrap(),
        seed.payload
    );
    seed.session.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn corrupted_upload_is_not_published_and_recovers() {
    let root = tempfile::tempdir().unwrap();
    let seed = Seeder::new(&root.path().join("seed"), 131072, None, true).await;
    let (adapter, store) = adapter(root.path(), TrafficPolicy::default());
    let id: uuid::Uuid = adapter
        .add_torrent(input(
            &seed,
            Options {
                initial_peers: vec![seed.session.listen_addr().unwrap().to_string()],
                ..Default::default()
            },
            false,
        ))
        .await
        .unwrap()
        .parse()
        .unwrap();
    let worker = tokio::spawn({
        let adapter = adapter.clone();
        async move { adapter.execute_job(id).await }
    });
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if seed.bad_reads.load(Ordering::Relaxed) > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let job = store.load_job(id).await.unwrap().unwrap();
    assert!(!job.output_path.exists());
    assert_eq!(job.bytes_downloaded, 0);
    assert!(adapter
        .torrent(id.to_string())
        .await
        .unwrap()
        .verified
        .iter()
        .all(|b| !*b));
    adapter.pause(id).await.unwrap();
    worker.await.unwrap().unwrap();
    seed.bad.store(false, Ordering::Release);
    adapter.resume(id).await.unwrap();
    tokio::time::timeout(Duration::from_secs(20), adapter.execute_job(id))
        .await
        .unwrap()
        .unwrap();
    let job = store.load_job(id).await.unwrap().unwrap();
    assert_eq!(
        std::fs::read(job.output_path.join("payload.bin")).unwrap(),
        seed.payload
    );
    seed.session.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn torrent_crash_child() {
    let Some(root) = std::env::var_os("OKYTES_TORRENT_TEST_ROOT") else {
        return;
    };
    let id = std::env::var("OKYTES_TORRENT_TEST_ID")
        .unwrap()
        .parse()
        .unwrap();
    let root = std::path::PathBuf::from(root);
    let _lease = std::sync::Arc::new(
        okytes_persistence::SqliteStore::acquire_engine(&root.join("jobs.sqlite")).unwrap(),
    );
    let (adapter, _) = adapter(&root, TrafficPolicy::default());
    adapter.retain_lease(_lease.clone()).unwrap();
    adapter.execute_job(id).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn process_kill_rechecks_and_resumes_durable_pieces() {
    let root = tempfile::tempdir().unwrap();
    let seed = Seeder::new(&root.path().join("seed"), 1048576, None, false).await;
    let (adapter, store) = adapter(root.path(), TrafficPolicy::default());
    let id: uuid::Uuid = adapter
        .add_torrent(input(
            &seed,
            Options {
                download_bps: Some(131072),
                initial_peers: vec![seed.session.listen_addr().unwrap().to_string()],
                ..Default::default()
            },
            false,
        ))
        .await
        .unwrap()
        .parse()
        .unwrap();
    let spawn = || {
        std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "torrent_crash_child", "--nocapture"])
            .env("OKYTES_TORRENT_TEST_ROOT", root.path())
            .env("OKYTES_TORRENT_TEST_ID", id.to_string())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap()
    };
    let mut child = spawn();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let bits = store.torrent_pieces(id, 64).await.unwrap();
            if bits.iter().any(|b| *b) {
                assert!(bits.iter().any(|b| !*b));
                break;
            }
            assert!(
                child.try_wait().unwrap().is_none(),
                "child exited before crash point"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    let mut resumed = spawn();
    tokio::time::timeout(Duration::from_secs(25), async {
        loop {
            if let Some(status) = resumed.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    let job = store.load_job(id).await.unwrap().unwrap();
    assert_eq!(job.state, JobState::Completed);
    assert_eq!(
        std::fs::read(job.output_path.join("payload.bin")).unwrap(),
        seed.payload
    );
    assert!(store
        .torrent_pieces(id, 64)
        .await
        .unwrap()
        .iter()
        .all(|b| *b));
    seed.session.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn isolated_dht_resolves_magnet_without_trackers_or_initial_peers() {
    use librqbit::{
        AddTorrent, AddTorrentOptions, DhtSessionConfig, ListenerOptions, Session, SessionOptions,
    };
    use std::str::FromStr;
    let root = tempfile::tempdir().unwrap();
    let seed = Seeder::new(&root.path().join("seed"), 200001, None, false).await;
    let meta = okytes_transport_torrent::metainfo::validate(&seed.raw).unwrap();
    let cancel = tokio_util::sync::CancellationToken::new();
    let bootstrap = librqbit::dht::DhtState::with_config(librqbit::dht::DhtConfig {
        peer_id: Some(librqbit::dht::Id20::from_str(&meta.metadata.info_hash).unwrap()),
        bootstrap_addrs: Some(vec![]),
        listen_addr: Some("127.0.0.1:0".parse().unwrap()),
        cancellation_token: Some(cancel.clone()),
        ..Default::default()
    })
    .await
    .unwrap();
    let serving = Session::new_with_opts(
        seed.source.clone(),
        SessionOptions {
            dht: Some(DhtSessionConfig {
                bootstrap_addrs: Some(vec![bootstrap.listen_addr().to_string()]),
                port: None,
                persistence: None,
            }),
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
    let handle = serving
        .add_torrent(
            AddTorrent::from_bytes(seed.raw.clone()),
            Some(AddTorrentOptions {
                overwrite: true,
                output_folder: Some(seed.source.to_string_lossy().into_owned()),
                disable_pex: true,
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    handle.wait_until_initialized().await.unwrap();
    // Keep the maintained discovery stream alive to announce this generated seed.
    let mut announced = serving.get_dht().unwrap().get_peers(
        librqbit::dht::Id20::from_str(&meta.metadata.info_hash).unwrap(),
        serving.listen_addr().map(|addr| addr.port()),
    );
    use futures::StreamExt;
    let announcing = tokio::spawn(async move { while announced.next().await.is_some() {} });
    tokio::time::sleep(Duration::from_secs(2)).await;
    let (adapter, store) = adapter(root.path(), TrafficPolicy::default());
    let mut options = Options::default();
    options.privacy.dht = true;
    options.privacy.bootstrap_nodes = Some(vec![bootstrap.listen_addr().to_string()]);
    let id: uuid::Uuid = adapter
        .add_torrent(input(&seed, options, true))
        .await
        .unwrap()
        .parse()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(30), adapter.execute_job(id))
        .await
        .unwrap()
        .unwrap();
    let job = store.load_job(id).await.unwrap().unwrap();
    assert_eq!(
        std::fs::read(job.output_path.join("payload.bin")).unwrap(),
        seed.payload
    );
    announcing.abort();
    cancel.cancel();
    serving.stop().await;
    seed.session.stop().await;
}
