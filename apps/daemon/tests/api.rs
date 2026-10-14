use okytes_core::desktop::DesktopService;
use okytes_daemon_api::contract::{Action, IssueToken, IssuedToken, Scope, Submit};
use okytes_test_support::server::{ServerConfig, TestServer};
use reqwest::{Method, StatusCode};
use sha2::{Digest, Sha256};
use std::time::Duration;

mod common;
use common::Fixture;

#[tokio::test]
async fn every_endpoint_rejects_missing_credentials_and_enforces_all_scopes() {
    let f = Fixture::new().await;
    let http = reqwest::Client::new();
    let read = f
        .api
        .tokens
        .issue(IssueToken {
            scope: Scope::ReadOnly,
            lifetime_secs: 60,
        })
        .await
        .unwrap()
        .token;
    let control = f
        .api
        .tokens
        .issue(IssueToken {
            scope: Scope::Control,
            lifetime_secs: 60,
        })
        .await
        .unwrap()
        .token;
    let paths = [
        (Method::GET, "v1/records", Scope::ReadOnly),
        (Method::GET, "v1/records/not-a-job", Scope::ReadOnly),
        (Method::GET, "v1/config", Scope::ReadOnly),
        (Method::PUT, "v1/config", Scope::Admin),
        (Method::GET, "v1/jobs", Scope::ReadOnly),
        (Method::POST, "v1/jobs", Scope::Control),
        (Method::POST, "v1/submit", Scope::Control),
        (Method::GET, "v1/jobs/not-a-job", Scope::ReadOnly),
        (Method::POST, "v1/jobs/not-a-job/control", Scope::Control),
        (Method::POST, "v1/pause-all", Scope::Control),
        (Method::GET, "v1/settings", Scope::ReadOnly),
        (Method::PUT, "v1/settings", Scope::Admin),
        (Method::POST, "v1/capture", Scope::Control),
        (Method::GET, "v1/events", Scope::ReadOnly),
        (Method::GET, "v1/tokens", Scope::Admin),
        (Method::POST, "v1/tokens", Scope::Admin),
        (Method::DELETE, "v1/tokens/not-a-token", Scope::Admin),
        (Method::GET, "v1/openapi.json", Scope::ReadOnly),
        (Method::GET, "not-a-route", Scope::ReadOnly),
    ];
    for (method, path, required) in paths {
        let url = format!("{}{path}", f.url);
        assert_eq!(
            http.request(method.clone(), &url)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED,
            "{path}"
        );
        for (scope, token) in [
            (Scope::ReadOnly, &read),
            (Scope::Control, &control),
            (Scope::Admin, &f.token),
        ] {
            let response = http
                .request(method.clone(), &url)
                .bearer_auth(token)
                .json(&serde_json::json!({}))
                .send()
                .await
                .unwrap();
            if scope < required {
                assert_eq!(response.status(), StatusCode::FORBIDDEN, "{path}");
            } else {
                assert_ne!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
                assert_ne!(response.status(), StatusCode::FORBIDDEN, "{path}");
            }
        }
    }
}

#[tokio::test]
async fn tokens_expire_revoke_and_are_persisted_as_hashes_only() {
    let f = Fixture::new().await;
    let admin = f.client(f.token.clone());
    let issued: IssuedToken = admin
        .request(
            Method::POST,
            "v1/tokens",
            Some(&IssueToken {
                scope: Scope::Control,
                lifetime_secs: 2,
            }),
        )
        .await
        .unwrap();
    let client = f.client(issued.token.clone());
    assert!(client.list(0).await.is_ok());
    tokio::time::sleep(Duration::from_millis(2200)).await;
    assert!(client.list(0).await.is_err());
    let issued: IssuedToken = admin
        .request(
            Method::POST,
            "v1/tokens",
            Some(&IssueToken {
                scope: Scope::ReadOnly,
                lifetime_secs: 60,
            }),
        )
        .await
        .unwrap();
    let client = f.client(issued.token.clone());
    assert!(client.list(0).await.is_ok());
    let revoked: bool = admin
        .request(
            Method::DELETE,
            &format!("v1/tokens/{}", issued.info.id),
            None::<&()>,
        )
        .await
        .unwrap();
    assert!(revoked);
    assert!(client.list(0).await.is_err());
    let store = okytes_persistence::SqliteStore::open(f.root.path().join("okytes.db")).unwrap();
    let value = store.preference("daemon-auth-v1").await.unwrap().unwrap();
    assert!(!value.contains(&f.token));
    assert!(!value.contains(&issued.token));
    assert!(value.contains("hash"));
}

#[tokio::test]
async fn real_client_controls_downloads_and_survives_client_disconnect() {
    let f = Fixture::new().await;
    let source = TestServer::start(ServerConfig {
        file_size: 4 * 1024 * 1024,
        frame_delay: Duration::from_millis(15),
        ..Default::default()
    })
    .await
    .unwrap();
    let client = f.client(f.token.clone());
    let id = client
        .submit(Submit {
            url: format!("http://{}/normal", source.addr),
            destination: None,
            filename: Some("daemon.bin".into()),
            sha256: Some(source.content_hash.clone()),
            conflict: "fail".into(),
        })
        .await
        .unwrap();
    client.control(id.clone(), Action::Pause).await.unwrap();
    assert_eq!(client.get(id.clone()).await.unwrap().state, "Paused");
    client.control(id.clone(), Action::Resume).await.unwrap();
    drop(client);
    let client = f.client(f.token.clone());
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let job = client.get(id.clone()).await.unwrap();
            assert_ne!(job.state, "Failed", "{:?}", job.error);
            if job.state == "Completed" {
                let bytes = std::fs::read(job.destination).unwrap();
                assert_eq!(format!("{:x}", Sha256::digest(bytes)), source.content_hash);
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn host_origin_body_and_flood_limits_fail_closed() {
    let f = Fixture::new().await;
    let http = reqwest::Client::new();
    for (name, value) in [
        ("Host", "attacker.example"),
        ("Origin", "https://attacker.example"),
        ("Origin", "null"),
    ] {
        assert_eq!(
            http.get(format!("{}v1/jobs", f.url))
                .bearer_auth(&f.token)
                .header(name, value)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
    }
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let authority = f.url.trim_start_matches("http://").trim_end_matches('/');
    let mut socket = tokio::net::TcpStream::connect(authority).await.unwrap();
    socket.write_all(format!("POST /v1/jobs HTTP/1.1\r\nHost: {authority}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: 1048577\r\nConnection: close\r\n\r\n",f.token).as_bytes()).await.unwrap();
    let mut response = Vec::new();
    socket.read_to_end(&mut response).await.unwrap();
    assert!(
        response.starts_with(b"HTTP/1.1 413"),
        "oversized content length must be rejected before reading its body"
    );
    assert!(http
        .post(format!("{}v1/jobs", f.url))
        .bearer_auth(&f.token)
        .header("Content-Type", "application/json")
        .body("{")
        .send()
        .await
        .unwrap()
        .status()
        .is_client_error());
    let mut limited = false;
    for _ in 0..605 {
        if http
            .get(format!("{}v1/jobs", f.url))
            .bearer_auth(&f.token)
            .send()
            .await
            .unwrap()
            .status()
            == StatusCode::TOO_MANY_REQUESTS
        {
            limited = true;
            break;
        }
    }
    assert!(limited);
}

#[tokio::test]
async fn event_stream_is_authenticated_and_revocation_closes_existing_subscribers() {
    use futures::StreamExt;
    let f = Fixture::new().await;
    let issued = f
        .api
        .tokens
        .issue(IssueToken {
            scope: Scope::ReadOnly,
            lifetime_secs: 60,
        })
        .await
        .unwrap();
    let response = reqwest::Client::new()
        .get(format!("{}v1/events", f.url))
        .bearer_auth(&issued.token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut stream = response.bytes_stream();
    let first = stream.next().await.unwrap().unwrap();
    assert!(String::from_utf8_lossy(&first).contains("Resync"));
    f.api.tokens.revoke(&issued.info.id).await.unwrap();
    let next = tokio::time::timeout(Duration::from_secs(3), stream.next())
        .await
        .unwrap();
    assert!(next.is_none());
}

#[tokio::test]
async fn slow_headers_do_not_block_other_clients_and_control_destinations_are_scoped() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let f = Fixture::new().await;
    let authority = f.url.trim_start_matches("http://").trim_end_matches('/');
    let mut socket = tokio::net::TcpStream::connect(authority).await.unwrap();
    socket
        .write_all(b"GET /v1/jobs HTTP/1.1\r\n")
        .await
        .unwrap();
    assert!(f.client(f.token.clone()).list(0).await.is_ok());
    let mut bytes = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), socket.read_to_end(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    let issued = f
        .api
        .tokens
        .issue(IssueToken {
            scope: Scope::Control,
            lifetime_secs: 60,
        })
        .await
        .unwrap();
    let client = f.client(issued.token);
    for destination in [
        f.root.path().to_string_lossy().into_owned(),
        f.root
            .path()
            .join("downloads/../escape")
            .to_string_lossy()
            .into_owned(),
    ] {
        assert!(client
            .submit(Submit {
                url: "http://127.0.0.1/file".into(),
                destination: Some(destination),
                filename: None,
                sha256: None,
                conflict: "rename".into()
            })
            .await
            .is_err());
    }
}

#[tokio::test]
async fn concurrent_controls_and_graceful_shutdown_preserve_queue_ownership() {
    let f = Fixture::new().await;
    let source = TestServer::start(ServerConfig {
        file_size: 8 * 1024 * 1024,
        frame_delay: Duration::from_millis(30),
        ..Default::default()
    })
    .await
    .unwrap();
    let client = f.client(f.token.clone());
    let id = client
        .submit(Submit {
            url: format!("http://{}/normal", source.addr),
            destination: None,
            filename: None,
            sha256: None,
            conflict: "rename".into(),
        })
        .await
        .unwrap();
    let (pause, cancel) = tokio::join!(
        client.control(id.clone(), Action::Pause),
        client.control(id.clone(), Action::Cancel)
    );
    assert!(pause.is_ok() || cancel.is_ok());
    assert_eq!(client.get(id.clone()).await.unwrap().state, "Cancelled");
    assert!(client.control(id, Action::Resume).await.is_err());
    let id = client
        .submit(Submit {
            url: format!("http://{}/normal", source.addr),
            destination: None,
            filename: None,
            sha256: None,
            conflict: "rename".into(),
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while client.get(id.clone()).await.unwrap().bytes == 0.0 {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    f.api.shutdown().await.unwrap();
    assert!(client
        .add(okytes_core::desktop::AddInput {
            urls: vec!["http://127.0.0.1/file".into()],
            destination: None,
            allow_duplicates: true
        })
        .await
        .is_err());
    let job = f.api.session.get(id).await.unwrap();
    assert_eq!(job.state, "Queued");
    assert!(job.bytes > 0.0);
    assert_eq!(job.workers, 0);
}

#[tokio::test]
async fn admin_configuration_updates_queue_preferences_and_rejects_database_redirection() {
    let f = Fixture::new().await;
    let client = f.client(f.token.clone());
    let mut config: okytes_core::types::EngineConfig = client
        .request(Method::GET, "v1/config", None::<&()>)
        .await
        .unwrap();
    config.max_concurrent_jobs = 2;
    config.default_output_dir = f.root.path().join("new-downloads");
    config.database_path = f.root.path().join("wrong.db");
    client
        .request::<()>(Method::PUT, "v1/config", Some(&config))
        .await
        .unwrap();
    assert_eq!(client.settings().await.unwrap().concurrency, 2);
    assert_eq!(
        client.settings().await.unwrap().destination,
        config.default_output_dir.to_string_lossy()
    );
    let saved: okytes_core::types::EngineConfig = client
        .request(Method::GET, "v1/config", None::<&()>)
        .await
        .unwrap();
    assert_eq!(saved.database_path, f.root.path().join("okytes.db"));
    assert!(!f.root.path().join("wrong.db").exists());
}
