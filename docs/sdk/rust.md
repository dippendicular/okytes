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

`DownloadRequest` accepts implemented HTTP/HTTPS, FTP/FTPS/SFTP and supported media URLs through the existing service boundary. Credentials remain memory-only. LAN peer execution remains daemon-owned because it requires an authenticated listener and receiver approval. `DaemonClient` is the distinct remote type; call `capabilities()` and use only advertised protocols.
