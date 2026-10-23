//! Long-lived deterministic fixture for native desktop end-to-end tests.
use okytes_test_support::server::{ServerConfig, TestServer};
use std::time::Duration;
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = TestServer::start(ServerConfig {
        file_size: 8 * 1024 * 1024,
        frame_delay: Duration::from_millis(
            std::env::var("OKYTES_FIXTURE_FRAME_MS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|value| *value <= 1000)
                .unwrap_or(25),
        ),
        ..Default::default()
    })
    .await?;
    println!(
        "{{\"url\":\"http://{}/normal\",\"sha256\":\"{}\"}}",
        server.addr, server.content_hash
    );
    tokio::signal::ctrl_c().await?;
    Ok(())
}
