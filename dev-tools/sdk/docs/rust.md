# Rust SDK

Add `okytes-sdk` and Tokio to an application. `EmbeddedEngine::open` requires absolute, isolated state and destination paths and obtains the existing exclusive engine lease. A second owner fails; dropping a `TransferHandle` does not cancel its durable job.

```rust
use okytes_sdk::prelude::*;

# async fn run() -> Result<(), Error> {
let engine = EmbeddedEngine::open(EngineConfig::new("/var/lib/my-app/okytes", "/var/lib/my-app/downloads")).await?;
let transfer = engine.submit(DownloadRequest::new("https://example.org/archive.tar.zst").expected_sha256("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")).await?;
let result = transfer.wait().await?;
engine.shutdown().await?;
# Ok(()) }
```

`EngineConfig` controls concurrency, retries, connection/read timeouts, global and per-transfer rate limits, and bounded graceful shutdown. `DownloadRequest::ftp` and `DownloadRequest::sftp` make credential and host-key requirements explicit. Torrent submission/inspection and media inspection have separate typed methods. Completed handles return `TransferResult` with destination, protocol, durable bytes, expected SHA-256 verification state and remote identity metadata.

Credentials remain memory-only. LAN peer execution remains daemon-owned because it requires an authenticated listener and receiver approval. `DaemonClient` is the distinct lightweight remote type; call `capabilities()` and use only advertised protocols. `request_sdk` preserves structured daemon errors and `updates` exposes cursor-aware `Changed`/`Resync` events.
