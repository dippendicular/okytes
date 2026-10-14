mod common;
#[path = "../../../crates/transport-torrent/tests/swarm_support.rs"]
mod swarm_support;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use common::Fixture;
use okytes_core::desktop::DesktopService;
use okytes_torrent_types::{Input, Options, TorrentService};
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_api_previews_downloads_and_stops_indefinite_seeding() {
    let f = Fixture::new().await;
    let seed = swarm_support::Seeder::new(&f.root.path().join("seed"), 200001, None, false).await;
    let client = f.client(f.token.clone());
    let input = Input {
        metainfo: Some(STANDARD.encode(&seed.raw)),
        magnet: None,
        destination: None,
        options: Options {
            seed_seconds: None,
            initial_peers: vec![seed.session.listen_addr().unwrap().to_string()],
            ..Default::default()
        },
    };
    let metadata = client.inspect_torrent(input.clone()).await.unwrap();
    assert_eq!(metadata.files[0].path, "payload.bin");
    let id = client.add_torrent(input.clone()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if client.get(id.clone()).await.unwrap().state == "Seeding" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let diagnostics = client.torrent(id.clone()).await.unwrap();
    assert_eq!(diagnostics.activity, "seeding");
    assert_eq!(diagnostics.metadata.unwrap().info_hash, metadata.info_hash);
    assert!(diagnostics.verified.iter().all(|piece| *piece));
    tokio::time::timeout(Duration::from_secs(5), client.pause_all())
        .await
        .unwrap()
        .unwrap();
    let job = client.get(id.clone()).await.unwrap();
    assert_eq!(job.protocol, "torrent");
    assert_eq!(job.state, "Completed");
    assert_eq!(
        std::fs::read(std::path::Path::new(&job.destination).join("payload.bin")).unwrap(),
        seed.payload
    );
    assert_eq!(client.torrent(id).await.unwrap().activity, "stopped");
    let control = f
        .api
        .tokens
        .issue(okytes_daemon_api::contract::IssueToken {
            scope: okytes_daemon_api::contract::Scope::Control,
            lifetime_secs: 60,
        })
        .await
        .unwrap()
        .token;
    let mut outside = input;
    outside.destination = Some(
        f.root
            .path()
            .parent()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
    );
    assert!(f.client(control).add_torrent(outside).await.is_err());
    seed.session.stop().await;
    f.api.shutdown().await.unwrap();
}
