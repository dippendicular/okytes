use okytes_core::desktop::DesktopService;
use okytes_daemon_api::contract::{IssueToken, Scope};
use okytes_test_support::server::{ServerConfig, TestServer};
use reqwest::{Client, StatusCode};
use sha2::{Digest, Sha256};
use std::time::Duration;
mod common;
use common::Fixture;

#[tokio::test]
async fn browser_surface_is_disabled_unless_the_owner_explicitly_enables_it() {
    let f = Fixture::new().await;
    assert!(f.root.path().is_dir());
    assert!(!Client::new()
        .get(format!("{}quick", f.url))
        .send()
        .await
        .unwrap()
        .status()
        .is_success());
    assert!(f.client(f.token.clone()).quick_pair().await.is_err());
}

async fn paired(fixture: &Fixture) -> (Client, String, String) {
    let native = fixture.client(fixture.token.clone());
    let pairing = native.quick_pair().await.unwrap();
    assert!(!pairing.url.contains(&pairing.code));
    let http = Client::new();
    let response = http
        .post(format!("{}quick/session", fixture.url))
        .header("origin", fixture.url.trim_end_matches('/'))
        .header("x-okytes-quick", "1")
        .json(&serde_json::json!({"code":pairing.code}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(
        cookie.contains("HttpOnly")
            && cookie.contains("SameSite=Strict")
            && cookie.contains("Max-Age=900")
    );
    let cookie = cookie.split(';').next().unwrap().to_owned();
    let text = response.text().await.unwrap();
    assert!(
        !text.contains(&fixture.token) && !text.contains(&pairing.code) && !text.contains(&cookie)
    );
    let csrf = serde_json::from_str::<serde_json::Value>(&text).unwrap()["csrf"]
        .as_str()
        .unwrap()
        .to_owned();
    let repeated = http
        .post(format!("{}quick/session", fixture.url))
        .header("x-okytes-quick", "1")
        .json(&serde_json::json!({"code":pairing.code}))
        .send()
        .await
        .unwrap();
    assert_eq!(repeated.status(), StatusCode::UNAUTHORIZED);
    (http, cookie, csrf)
}

#[tokio::test]
async fn local_assets_pairing_scopes_origin_host_csrf_and_logout_fail_closed() {
    let f = Fixture::quick().await;
    let (http, cookie, csrf) = paired(&f).await;
    let page = http.get(format!("{}quick", f.url)).send().await.unwrap();
    assert_eq!(page.status(), StatusCode::OK);
    assert!(page.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .contains("frame-ancestors 'none'"));
    assert_eq!(page.headers()["x-content-type-options"], "nosniff");
    for route in ["v1/jobs", "v1/events"] {
        assert_eq!(
            http.get(format!("{}{route}", f.url))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            http.get(format!("{}{route}", f.url))
                .header("cookie", &cookie)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            http.get(format!("{}{route}", f.url))
                .header("cookie", &cookie)
                .header("x-okytes-quick", "1")
                .header("x-okytes-csrf", &csrf)
                .header("origin", "https://evil.invalid")
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            http.get(format!("{}{route}", f.url))
                .header("cookie", &cookie)
                .header("x-okytes-quick", "1")
                .header("x-okytes-csrf", &csrf)
                .header("host", "rebound.invalid")
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
    }
    let request = || {
        http.get(format!("{}v1/jobs", f.url))
            .header("cookie", &cookie)
            .header("x-okytes-quick", "1")
            .header("x-okytes-csrf", &csrf)
    };
    assert_eq!(request().send().await.unwrap().status(), StatusCode::OK);
    for route in ["v1/config", "v1/tokens", "v1/records", "v1/settings"] {
        assert_eq!(
            http.get(format!("{}{route}", f.url))
                .header("cookie", &cookie)
                .header("x-okytes-quick", "1")
                .header("x-okytes-csrf", &csrf)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
    }
    let control = f
        .api
        .tokens
        .issue(IssueToken {
            scope: Scope::Control,
            lifetime_secs: 60,
        })
        .await
        .unwrap();
    assert!(f.client(control.token).quick_pair().await.is_err());
    let response = http
        .post(format!("{}quick/logout", f.url))
        .header("cookie", &cookie)
        .header("x-okytes-quick", "1")
        .header("x-okytes-csrf", &csrf)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        request().send().await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn quick_download_uses_real_engine_survives_client_close_and_redacts_source_secrets() {
    let f = Fixture::quick().await;
    let server = TestServer::start(ServerConfig {
        file_size: 2 * 1024 * 1024,
        frame_delay: Duration::from_millis(10),
        ..Default::default()
    })
    .await
    .unwrap();
    let (http, cookie, csrf) = paired(&f).await;
    let input = serde_json::json!({"urls":[format!("http://{}/normal?signature=PRIVATE", server.addr)],"destination":null,"allow_duplicates":false});
    let response = http
        .post(format!("{}v1/jobs", f.url))
        .header("cookie", &cookie)
        .header("x-okytes-quick", "1")
        .header("x-okytes-csrf", &csrf)
        .json(&input)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let ids: Vec<String> = response.json().await.unwrap();
    let repeated = http
        .post(format!("{}v1/jobs", f.url))
        .header("cookie", &cookie)
        .header("x-okytes-quick", "1")
        .header("x-okytes-csrf", &csrf)
        .json(&input)
        .send()
        .await
        .unwrap();
    assert!(!repeated.status().is_success());
    drop(http);
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let job = f.api.session.get(ids[0].clone()).await.unwrap();
            assert_ne!(job.state, "Failed");
            if job.state == "Completed" {
                let bytes = std::fs::read(job.destination).unwrap();
                assert_eq!(format!("{:x}", Sha256::digest(bytes)), server.content_hash);
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    let page = Client::new()
        .get(format!("{}v1/jobs", f.url))
        .header("cookie", &cookie)
        .header("x-okytes-quick", "1")
        .header("x-okytes-csrf", &csrf)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(!page.contains("PRIVATE") && !page.contains("signature="));
    assert!(page.contains(&ids[0]));
}

#[tokio::test]
async fn quick_rejects_invalid_urls_destinations_large_inputs_and_revoked_sessions() {
    let f = Fixture::quick().await;
    let (http, cookie, csrf) = paired(&f).await;
    let submit = |input| {
        http.post(format!("{}v1/jobs", f.url))
            .header("cookie", &cookie)
            .header("x-okytes-quick", "1")
            .header("x-okytes-csrf", &csrf)
            .json(&input)
    };
    for url in [
        "file:///etc/passwd",
        "javascript:alert(1)",
        "ftp://example.invalid/file",
        "http://user:pass@example.invalid/file",
        "not a URL",
    ] {
        assert!(!submit(
            serde_json::json!({"urls":[url],"destination":null,"allow_duplicates":false})
        )
        .send()
        .await
        .unwrap()
        .status()
        .is_success());
    }
    let outside = tempfile::tempdir().unwrap();
    assert!(!submit(serde_json::json!({"urls":["https://example.invalid/a"],"destination":outside.path(),"allow_duplicates":false})).send().await.unwrap().status().is_success());
    assert!(!submit(serde_json::json!({"urls":vec!["https://example.invalid/a";11],"destination":null,"allow_duplicates":false})).send().await.unwrap().status().is_success());
    assert!(!submit(
        serde_json::json!({"urls":["x".repeat(100000)],"destination":null,"allow_duplicates":false})
    )
    .send()
    .await
    .unwrap()
    .status()
    .is_success());
    let browser = f
        .api
        .tokens
        .list()
        .await
        .into_iter()
        .find(|info| info.scope == Scope::Control)
        .unwrap();
    f.api.tokens.revoke(&browser.id).await.unwrap();
    assert_eq!(
        http.get(format!("{}v1/jobs", f.url))
            .header("cookie", &cookie)
            .header("x-okytes-quick", "1")
            .header("x-okytes-csrf", &csrf)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
}
