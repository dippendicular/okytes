use okytes_sdk::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Error> {
    let root =
        std::env::current_dir().map_err(|error| Error::InvalidConfiguration(error.to_string()))?;
    let engine = EmbeddedEngine::open(EngineConfig::new(
        root.join(".okytes-state"),
        root.join("downloads"),
    ))
    .await?;
    let transfer = engine
        .submit(DownloadRequest::new("https://example.com/file.bin"))
        .await?;
    let result = transfer.wait().await?;
    println!("{}: {}", result.id, result.destination.display());
    engine.shutdown().await
}
