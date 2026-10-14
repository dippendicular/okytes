use okytes_test_support::{
    file_server::{FileServer, Remote},
    server::{ServerConfig, TestServer},
};
#[tokio::test]
async fn cli_file_login_and_media_inspection_download_real_data() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("jobs.db");
    let credentials = temp.path().join("login.json");
    std::fs::write(
        &credentials,
        r#"{"username":"user","password":"password","confirmed_host_key":null}"#,
    )
    .unwrap();
    let file = (0..65536).map(|n| (n % 251) as u8).collect::<Vec<_>>();
    let ftp = FileServer::ftp(Remote::new(file.clone()), false)
        .await
        .unwrap();
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_okytes-cli"))
        .args([
            "--db",
            database.to_str().unwrap(),
            "--json",
            "add",
            &format!("ftp://{}/file.bin", ftp.address),
            "--output",
            temp.path().to_str().unwrap(),
            "--filename",
            "cli-ftp.bin",
            "--credentials",
            credentials.to_str().unwrap(),
            "--run",
        ])
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read(temp.path().join("cli-ftp.bin")).unwrap(),
        file
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("password"));
    let server = TestServer::start(ServerConfig::default()).await.unwrap();
    let mut data = vec![0u8; 188 * 8];
    for packet in data.chunks_mut(188) {
        packet[0] = 0x47;
        packet[3] = 0x10;
    }
    server.fixture("/fixtures/data.ts", data.clone()).unwrap();
    server
        .fixture(
            "/fixtures/cli.m3u8",
            "#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:4,\ndata.ts\n#EXT-X-ENDLIST\n",
        )
        .unwrap();
    let source = format!("hls+{}", server.url("/fixtures/cli.m3u8"));
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_okytes-cli"))
        .args([
            "--db",
            database.to_str().unwrap(),
            "--json",
            "media-inspect",
            &source,
        ])
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let choices: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(choices.as_array().unwrap().len(), 1);
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_okytes-cli"))
        .args([
            "--db",
            database.to_str().unwrap(),
            "add",
            &source,
            "--output",
            temp.path().to_str().unwrap(),
            "--filename",
            "cli-media.ts",
            "--run",
        ])
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read(temp.path().join("cli-media.ts")).unwrap(),
        data
    );
}
