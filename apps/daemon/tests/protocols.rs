use okytes_core::desktop::{Action, DesktopService, FileCredentials, Submit};
use okytes_test_support::{
    file_server::{FileServer, Remote},
    server::{ServerConfig, TestServer},
};
use sha2::{Digest, Sha256};
use std::time::Duration;
mod common;
use common::Fixture;
#[tokio::test]
async fn daemon_routes_file_credentials_and_media_selection_to_real_transports() {
    let fixture = Fixture::new().await;
    let client = fixture.client(fixture.token.clone());
    let bytes = (0..262144).map(|n| (n % 251) as u8).collect::<Vec<_>>();
    let remote = Remote::new(bytes.clone());
    let ftp = FileServer::ftp(remote, false).await.unwrap();
    let id = client
        .submit(Submit {
            credentials: Some(FileCredentials {
                username: "user".into(),
                password: "password".into(),
                confirmed_host_key: None,
            }),
            url: format!("ftp://{}/file.bin", ftp.address),
            destination: None,
            filename: Some("ftp.bin".into()),
            sha256: Some(format!("{:x}", Sha256::digest(&bytes))),
            conflict: "fail".into(),
        })
        .await
        .unwrap();
    client.control(id.clone(), Action::Pause).await.unwrap();
    client
        .authorize_file(
            id.clone(),
            FileCredentials {
                username: "user".into(),
                password: "password".into(),
                confirmed_host_key: None,
            },
        )
        .await
        .unwrap();
    client.control(id.clone(), Action::Resume).await.unwrap();
    wait(&client, &id).await;
    assert_eq!(
        std::fs::read(client.get(id).await.unwrap().destination).unwrap(),
        bytes
    );
    let media = TestServer::start(ServerConfig::default()).await.unwrap();
    let mut data = vec![0u8; 188 * 16];
    for packet in data.chunks_mut(188) {
        packet[0] = 0x47;
        packet[3] = 0x10;
    }
    media.fixture("/fixtures/media.ts", data.clone()).unwrap();
    media
        .fixture(
            "/fixtures/vod.m3u8",
            "#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:4,\nmedia.ts\n#EXT-X-ENDLIST\n",
        )
        .unwrap();
    let url = format!("hls+{}", media.url("/fixtures/vod.m3u8"));
    let choices = client.inspect_media(url.clone()).await.unwrap();
    assert_eq!(choices.len(), 1);
    let id = client
        .submit(Submit {
            credentials: None,
            url,
            destination: None,
            filename: Some("media.ts".into()),
            sha256: Some(format!("{:x}", Sha256::digest(&data))),
            conflict: "fail".into(),
        })
        .await
        .unwrap();
    wait(&client, &id).await;
    let job = client.get(id).await.unwrap();
    assert_eq!(job.protocol, "hls+http");
    assert_eq!(std::fs::read(job.destination).unwrap(), data);
    assert!(fixture
        .api
        .session
        .engine_page(0)
        .await
        .unwrap()
        .iter()
        .all(|record| record.headers.is_empty()));
    let database = std::fs::read(fixture.root.path().join("okytes.db")).unwrap();
    assert!(!database.windows(8).any(|bytes| bytes == b"password"));
}
async fn wait(client: &okytes_daemon_api::Client, id: &str) {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let job = client.get(id.into()).await.unwrap();
            assert_ne!(job.state, "Failed", "{:?}", job.error);
            if job.state == "Completed" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}
