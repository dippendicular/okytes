use okytes_core::{
    engine::Engine,
    service::DownloadService,
    transport::Transport,
    types::{ConflictStrategy, DownloadRequest, EngineConfig},
};
use okytes_persistence::{files::LocalFiles, SqliteStore};
use okytes_test_support::server::{ServerConfig, TestServer};
use okytes_transport_http::HttpTransport;
use okytes_transport_media::MediaTransport;
use sha2::{Digest, Sha256};
use std::sync::Arc;
fn ts(n: usize) -> Vec<u8> {
    let mut data = vec![0u8; 188 * n];
    for packet in data.chunks_mut(188) {
        packet[0] = 0x47;
        packet[1] = 0x40;
        packet[3] = 0x10;
    }
    data
}
fn mp4(kind: &str, payload: &[u8]) -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(&((payload.len() + 8) as u32).to_be_bytes());
    data.extend_from_slice(kind.as_bytes());
    data.extend_from_slice(payload);
    data
}
fn hls(uris: &[&str]) -> String {
    format!(
        "#EXTM3U\n#EXT-X-TARGETDURATION:4\n{}#EXT-X-ENDLIST\n",
        uris.iter()
            .map(|uri| format!("#EXTINF:4,\n{uri}\n"))
            .collect::<String>()
    )
}
async fn server() -> TestServer {
    TestServer::start(ServerConfig::default()).await.unwrap()
}
fn media() -> Arc<MediaTransport> {
    MediaTransport::new(Arc::new(
        HttpTransport::new(&EngineConfig::default()).unwrap(),
    ))
}
async fn download(
    server: &TestServer,
    path: &str,
    expected: &[u8],
) -> okytes_core::types::JobRecord {
    let temp = tempfile::tempdir().unwrap();
    let config = EngineConfig {
        database_path: temp.path().join("jobs.db"),
        ..Default::default()
    };
    let store = Arc::new(SqliteStore::open(&config.database_path).unwrap());
    let engine = Engine::new(config, media(), store, Arc::new(LocalFiles)).unwrap();
    let source = format!(
        "{}+{}",
        if path.contains(".mpd") { "dash" } else { "hls" },
        server.url(path)
    );
    let id = engine
        .add(DownloadRequest {
            url: source.parse().unwrap(),
            output_dir: temp.path().into(),
            filename: Some("output.bin".into()),
            expected_hash: Some(format!("{:x}", Sha256::digest(expected))),
            conflict_strategy: ConflictStrategy::Fail,
            headers: vec![],
            priority: 0,
        })
        .await
        .unwrap();
    engine.execute_job(id).await.unwrap();
    let job = engine.status(id).await.unwrap();
    assert_eq!(tokio::fs::read(&job.output_path).await.unwrap(), expected);
    job
}
#[tokio::test]
async fn hls_vod_byte_ranges_master_and_separate_audio_selection() {
    let server = server().await;
    let first = ts(4);
    let second = ts(5);
    let full = [first.clone(), second.clone()].concat();
    server.fixture("/fixtures/shared.ts", full.clone()).unwrap();
    server.fixture("/fixtures/video.m3u8",format!("#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:4,\n#EXT-X-BYTERANGE:{}@0\nshared.ts\n#EXTINF:4,\n#EXT-X-BYTERANGE:{}\nshared.ts\n#EXT-X-ENDLIST\n",first.len(),second.len())).unwrap();
    server
        .fixture("/fixtures/audio.m3u8", hls(&["shared.ts"]))
        .unwrap();
    server.fixture("/fixtures/master.m3u8","#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"English\",LANGUAGE=\"en\",URI=\"audio.m3u8\"\n#EXT-X-STREAM-INF:BANDWIDTH=100000,RESOLUTION=320x180,AUDIO=\"a\"\nvideo.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=200000,RESOLUTION=640x360,AUDIO=\"a\"\nvideo.m3u8\n").unwrap();
    let choices = media()
        .inspect(&format!("hls+{}", server.url("/fixtures/master.m3u8")))
        .await
        .unwrap();
    assert_eq!(choices.len(), 3);
    assert_eq!(choices[2].kind, "audio");
    download(&server, "/fixtures/master.m3u8", &full).await;
    download(&server, "/fixtures/audio.m3u8", &full).await;
}
#[tokio::test]
async fn hls_authorized_aes128_and_fragmented_mp4_init_assemble() {
    use cbc::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
    let server = server().await;
    let key = [7u8; 16];
    let iv = [0u8; 16];
    let plain = ts(7);
    let ciphertext = cbc::Encryptor::<aes::Aes128>::new(&key.into(), &iv.into())
        .encrypt_padded_vec_mut::<Pkcs7>(&plain);
    server.fixture("/fixtures/key", key.to_vec()).unwrap();
    server
        .fixture("/fixtures/encrypted.ts", ciphertext)
        .unwrap();
    server.fixture("/fixtures/aes.m3u8","#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXT-X-KEY:METHOD=AES-128,URI=\"key\"\n#EXTINF:4,\nencrypted.ts\n#EXT-X-ENDLIST\n").unwrap();
    download(&server, "/fixtures/aes.m3u8", &plain).await;
    let init = [mp4("ftyp", b"isom"), mp4("moov", &mp4("mvex", &[]))].concat();
    let segment = [
        mp4("moof", &mp4("mfhd", &[0; 8])),
        mp4("mdat", b"synthetic payload"),
    ]
    .concat();
    server.fixture("/fixtures/init.mp4", init.clone()).unwrap();
    server
        .fixture("/fixtures/one.m4s", segment.clone())
        .unwrap();
    server.fixture("/fixtures/fmp4.m3u8","#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:4\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:4,\none.m4s\n#EXT-X-ENDLIST\n").unwrap();
    download(&server, "/fixtures/fmp4.m3u8", &[init, segment].concat()).await;
}
#[tokio::test]
async fn dash_segment_list_timeline_template_quality_and_subtitle_selection() {
    let server = server().await;
    let init = mp4("moov", &mp4("mvex", &[]));
    let a = [mp4("moof", &mp4("mfhd", &[0; 8])), mp4("mdat", b"one")].concat();
    let b = [mp4("moof", &mp4("mfhd", &[0; 8])), mp4("mdat", b"two")].concat();
    for (path, data) in [
        ("/fixtures/init.mp4", init.clone()),
        ("/fixtures/v-001.m4s", a.clone()),
        ("/fixtures/v-002.m4s", b.clone()),
    ] {
        server.fixture(path, data).unwrap();
    }
    server.fixture("/fixtures/list.mpd",r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static"><Period><AdaptationSet mimeType="video/mp4" codecs="avc1.42E01E"><Representation id="v" bandwidth="100"><SegmentList><Initialization sourceURL="init.mp4"/><SegmentURL media="v-001.m4s"/><SegmentURL media="v-002.m4s"/></SegmentList></Representation></AdaptationSet></Period></MPD>"#).unwrap();
    download(
        &server,
        "/fixtures/list.mpd",
        &[init.clone(), a.clone(), b.clone()].concat(),
    )
    .await;
    server.fixture("/fixtures/timeline.mpd",r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static"><Period><AdaptationSet mimeType="video/mp4" codecs="avc1.42E01E"><SegmentTemplate initialization="init.mp4" media="$RepresentationID$-$Number%03d$.m4s" startNumber="1"><SegmentTimeline><S t="0" d="4" r="1"/></SegmentTimeline></SegmentTemplate><Representation id="v" bandwidth="100"/></AdaptationSet><AdaptationSet mimeType="audio/mp4" codecs="mp4a.40.2" lang="en"><SegmentList><Initialization sourceURL="init.mp4"/><SegmentURL media="v-001.m4s"/></SegmentList><Representation id="a" bandwidth="50"/></AdaptationSet><AdaptationSet mimeType="text/vtt" lang="en"><Representation id="sub"><BaseURL>sub.vtt</BaseURL></Representation></AdaptationSet></Period></MPD>"#).unwrap();
    let choices = media()
        .inspect(&format!("dash+{}", server.url("/fixtures/timeline.mpd")))
        .await
        .unwrap();
    assert_eq!(
        choices.iter().map(|c| c.kind.as_str()).collect::<Vec<_>>(),
        vec!["video", "audio", "subtitles"]
    );
    download(
        &server,
        "/fixtures/timeline.mpd",
        &[init.clone(), a.clone(), b].concat(),
    )
    .await;
    download(
        &server,
        "/fixtures/timeline.mpd#representation=1:0",
        &[init, a].concat(),
    )
    .await;
    server
        .fixture(
            "/fixtures/sub.vtt",
            "WEBVTT\n\n00:00:00.000 --> 00:00:01.000\nSubtitle\n",
        )
        .unwrap();
    download(
        &server,
        "/fixtures/timeline.mpd#representation=2:0",
        b"WEBVTT\n\n00:00:00.000 --> 00:00:01.000\nSubtitle\n\n",
    )
    .await;
}
#[tokio::test]
async fn webvtt_sidecars_have_one_header_and_preserve_absolute_timestamps() {
    let server = server().await;
    server
        .fixture(
            "/fixtures/one.vtt",
            "WEBVTT\n\n00:00:00.000 --> 00:00:01.000\nOne\n",
        )
        .unwrap();
    server
        .fixture(
            "/fixtures/two.vtt",
            "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nTwo\n",
        )
        .unwrap();
    server
        .fixture("/fixtures/subtitles.m3u8", hls(&["one.vtt", "two.vtt"]))
        .unwrap();
    download(
        &server,
        "/fixtures/subtitles.m3u8",
        b"WEBVTT\n\n00:00:00.000 --> 00:00:01.000\nOne\n\n00:00:01.000 --> 00:00:02.000\nTwo\n\n",
    )
    .await;
}
#[tokio::test]
async fn drm_live_discontinuities_bad_ranges_and_missing_segments_refused() {
    let server = server().await;
    let transport = media();
    for (index,text) in ["#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:4,\na.ts\n","#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://fairplay\"\n#EXTINF:4,\na.ts\n#EXT-X-ENDLIST\n","#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXT-X-DISCONTINUITY\n#EXTINF:4,\na.ts\n#EXT-X-ENDLIST\n","#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:4,\n#EXT-X-BYTERANGE:10\na.ts\n#EXT-X-ENDLIST\n","#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:4,\nmissing.ts\n#EXT-X-ENDLIST\n"].iter().enumerate(){let path=format!("/fixtures/refuse-{index}.m3u8");server.fixture(&path,text.to_string()).unwrap();let url=format!("hls+{}",server.url(&path)).parse().unwrap();assert!(transport.probe(&url,&[]).await.is_err());}
    for drm in [
        "urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed",
        "urn:uuid:9a04f079-9840-4286-ab92-e65be0885f95",
    ] {
        server.fixture("/fixtures/drm.mpd",format!("<MPD><Period><AdaptationSet><ContentProtection schemeIdUri=\"{drm}\"/></AdaptationSet></Period></MPD>")).unwrap();
        assert!(transport
            .inspect(&format!("dash+{}", server.url("/fixtures/drm.mpd")))
            .await
            .unwrap_err()
            .to_string()
            .contains("DRM"));
    }
}
#[tokio::test]
async fn corrupted_segment_fails_before_any_payload_is_published() {
    use futures::TryStreamExt;
    let server = server().await;
    server
        .fixture("/fixtures/corrupt.ts", vec![0u8; 188])
        .unwrap();
    server
        .fixture("/fixtures/corrupt.m3u8", hls(&["corrupt.ts"]))
        .unwrap();
    let transport = media();
    let url = format!("hls+{}", server.url("/fixtures/corrupt.m3u8"))
        .parse()
        .unwrap();
    let probe = transport.probe(&url, &[]).await.unwrap();
    let response = transport
        .fetch(&url, &[], None, &probe.remote_identity)
        .await
        .unwrap();
    assert!(response.body.try_collect::<Vec<_>>().await.is_err());
}
#[tokio::test]
async fn immutable_plans_for_the_same_url_do_not_replace_each_other() {
    use futures::TryStreamExt;
    let server = server().await;
    let transport = media();
    let first = ts(2);
    let second = ts(3);
    server.fixture("/fixtures/a.ts", first.clone()).unwrap();
    server.fixture("/fixtures/b.ts", second.clone()).unwrap();
    server
        .fixture("/fixtures/versions.m3u8", hls(&["a.ts"]))
        .unwrap();
    let url = format!("hls+{}", server.url("/fixtures/versions.m3u8"))
        .parse()
        .unwrap();
    let a = transport.probe(&url, &[]).await.unwrap();
    server
        .fixture("/fixtures/versions.m3u8", hls(&["b.ts"]))
        .unwrap();
    let b = transport.probe(&url, &[]).await.unwrap();
    assert_ne!(a.remote_identity, b.remote_identity);
    for (identity, expected) in [(a.remote_identity, first), (b.remote_identity, second)] {
        let response = transport.fetch(&url, &[], None, &identity).await.unwrap();
        assert_eq!(
            response
                .body
                .try_collect::<Vec<_>>()
                .await
                .unwrap()
                .concat(),
            expected
        );
    }
}
