//! Native partial access; permissions and no-follow flags stay outside core.
use okytes_core::{
    error::{EngineError, EngineResult},
    files::PartialFiles,
};
use std::{future::Future, path::Path, pin::Pin};
/// Local private partial-file adapter.
pub struct LocalFiles;
impl LocalFiles {
    fn access(
        path: &Path,
        create: bool,
    ) -> Pin<Box<dyn Future<Output = EngineResult<tokio::fs::File>> + Send>> {
        let path = path.to_owned();
        Box::pin(async move {
            let location = path.clone();
            let result = tokio::task::spawn_blocking(move || -> std::io::Result<std::fs::File> {
                let mut options = std::fs::OpenOptions::new();
                options.read(true).write(true).create_new(create);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options
                        .mode(0o600)
                        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
                }
                #[cfg(windows)]
                {
                    use std::os::windows::fs::OpenOptionsExt;
                    options.custom_flags(0x00200000); // FILE_FLAG_OPEN_REPARSE_POINT
                }
                let file = options.open(&path)?;
                let metadata = file.metadata()?;
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    if metadata.file_attributes() & 0x400 != 0 {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            "partial path is a reparse point",
                        ));
                    }
                }
                if !metadata.is_file() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "partial is not a regular file",
                    ));
                }
                Ok(file)
            })
            .await
            .map_err(|e| EngineError::Scheduler(e.to_string()))?;
            Ok(tokio::fs::File::from_std(result.map_err(|source| {
                EngineError::FileIo {
                    path: location,
                    source,
                }
            })?))
        })
    }
}
impl PartialFiles for LocalFiles {
    fn create(
        &self,
        path: &Path,
    ) -> Pin<Box<dyn Future<Output = EngineResult<tokio::fs::File>> + Send + '_>> {
        Self::access(path, true)
    }
    fn open(
        &self,
        path: &Path,
    ) -> Pin<Box<dyn Future<Output = EngineResult<tokio::fs::File>> + Send + '_>> {
        Self::access(path, false)
    }
}
