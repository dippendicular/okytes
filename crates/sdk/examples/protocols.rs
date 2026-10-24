use okytes_sdk::prelude::*;
use okytes_sdk::TorrentRequest;

#[tokio::main]
async fn main() -> Result<(), Error> {
    let state = std::path::absolute(".okytes-sdk-state")
        .map_err(|e| Error::InvalidConfiguration(e.to_string()))?;
    let output =
        std::path::absolute("downloads").map_err(|e| Error::InvalidConfiguration(e.to_string()))?;
    let engine = EmbeddedEngine::open(EngineConfig::new(state, output).concurrency(4)).await?;

    let ftp = engine
        .submit(DownloadRequest::ftp(
            "ftps://files.example.invalid/release.bin",
            "account",
            "password",
        ))
        .await?;
    ftp.priority(10).await?;
    let _torrent = engine
        .inspect_torrent(TorrentRequest {
            magnet: Some("magnet:?xt=urn:btih:0000000000000000000000000000000000000000".into()),
            metainfo: None,
            destination: None,
            options: Default::default(),
        })
        .await?;
    let _media = engine
        .inspect_media("https://media.example.invalid/manifest.mpd")
        .await?;
    engine.shutdown().await
}
