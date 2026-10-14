use std::{process::Stdio, time::Duration};
#[tokio::test]
async fn configured_remote_listener_requires_tls_trust_and_authentication() {
    let root = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert = root.path().join("cert.pem");
    let key = root.path().join("key.pem");
    std::fs::write(&cert, certified.cert.pem()).unwrap();
    std::fs::write(&key, certified.signing_key.serialize_pem()).unwrap();
    let authority = format!("localhost:{port}");
    let url = format!("https://{authority}/v1/jobs");
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_okytes-daemon"))
        .args([
            "--remote",
            "--bind",
            &format!("127.0.0.1:{port}"),
            "--host",
            &authority,
            "--data-dir",
        ])
        .arg(root.path())
        .arg("--cert")
        .arg(cert)
        .arg("--key")
        .arg(key)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let http = reqwest::Client::builder()
        .add_root_certificate(
            reqwest::Certificate::from_pem(certified.cert.pem().as_bytes()).unwrap(),
        )
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            assert!(child.try_wait().unwrap().is_none(), "TLS daemon exited");
            if let Ok(response) = http.get(&url).send().await {
                assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        reqwest::Client::new().get(&url).send().await.is_err(),
        "an untrusted certificate must fail"
    );
    let token = String::from_utf8(
        okytes_browser_bridge::read_secret(&root.path().join("daemon-admin.token")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        http.get(&url)
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::OK
    );
    assert!(
        reqwest::get(format!("http://127.0.0.1:{port}/v1/jobs"))
            .await
            .is_err(),
        "TLS listener cannot answer plaintext HTTP"
    );
    child.kill().await.unwrap();
    child.wait().await.unwrap();
}
