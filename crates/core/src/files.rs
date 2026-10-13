//! Platform boundary for private partial creation and no-follow opens.
use crate::error::EngineResult;
use std::{future::Future, path::Path, pin::Pin};
/// Platform file access implementation; OS flags stay outside core.
pub trait PartialFiles: Send + Sync {
    /// Create a unique partial with restrictive permissions.
    fn create(
        &self,
        path: &Path,
    ) -> Pin<Box<dyn Future<Output = EngineResult<tokio::fs::File>> + Send + '_>>;
    /// Open an existing regular partial without following symlinks.
    fn open(
        &self,
        path: &Path,
    ) -> Pin<Box<dyn Future<Output = EngineResult<tokio::fs::File>> + Send + '_>>;
}
