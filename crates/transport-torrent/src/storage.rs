//! Private regular-file storage. Each write reaches sync_data before the
//! maintained verifier can acknowledge a piece in the serialized SQLite writer.
use crate::metainfo::{self, Recipe};
use librqbit::StorageResult as Result;
use okytes_core::error::{EngineError, EngineResult};
fn invalid(message: &str) -> EngineError {
    EngineError::Config(message.into())
}
use librqbit::storage::{BoxStorageFactory, StorageFactory, StorageFactoryExt, TorrentStorage};
use librqbit::{ManagedTorrentShared, TorrentMetadata};
use okytes_core::types::JobId;
use okytes_persistence::SqliteStore;
use okytes_torrent_types::Options;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

pub(crate) struct Data {
    pub id: JobId,
    pub root: PathBuf,
    pub store: Arc<SqliteStore>,
    pub options: Options,
    pub recipe: Mutex<Option<Arc<Recipe>>>,
    pub verified: Mutex<Vec<bool>>,
    pub stopped: AtomicBool,
    pub complete: AtomicBool,
    pub sealed: AtomicBool,
    pub quota_exhausted: AtomicBool,
    pub io_gate: std::sync::RwLock<()>,
}
impl Data {
    pub fn recipe(&self) -> EngineResult<Arc<Recipe>> {
        self.recipe
            .lock()
            .map_err(|_| invalid("metadata lock failed"))?
            .clone()
            .ok_or_else(|| invalid("metadata is not resolved"))
    }
    pub fn bits(&self) -> EngineResult<Vec<bool>> {
        Ok(self
            .verified
            .lock()
            .map_err(|_| invalid("bitmap lock failed"))?
            .clone())
    }
    pub fn selected_bytes(&self) -> EngineResult<u64> {
        Ok(self
            .recipe()?
            .metadata
            .files
            .iter()
            .filter(|f| f.selected)
            .map(|f| f.bytes)
            .sum())
    }
    pub fn progress(&self) -> EngineResult<u64> {
        let recipe = self.recipe()?;
        let bits = self.bits()?;
        let size = recipe.metadata.piece_bytes as u64;
        let mut bytes = 0;
        for file in &recipe.metadata.files {
            if !file.selected {
                continue;
            }
            let start = recipe.offsets[file.index as usize];
            let end = start + file.bytes;
            for piece in start / size..end.div_ceil(size) {
                if bits.get(piece as usize) == Some(&true) {
                    bytes += end.min((piece + 1) * size) - start.max(piece * size);
                }
            }
        }
        Ok(bytes)
    }
    pub fn refresh_complete(&self) -> EngineResult<()> {
        self.complete.store(
            self.progress()? == self.selected_bytes()?,
            Ordering::Release,
        );
        Ok(())
    }
}
#[derive(Clone)]
pub(crate) struct Factory(pub Arc<Data>);
impl StorageFactory for Factory {
    type Storage = Storage;
    fn create(&self, _: &ManagedTorrentShared, metadata: &TorrentMetadata) -> Result<Storage> {
        let mut recipe = metainfo::validate(&metadata.torrent_bytes)?;
        if let Some(selected) = &self.0.options.files {
            if selected
                .iter()
                .any(|index| *index as usize >= recipe.metadata.files.len())
            {
                return Err(invalid("selected file index is outside metainfo").into());
            }
            for file in &mut recipe.metadata.files {
                file.selected = selected.contains(&file.index);
            }
        }
        if metadata.file_infos.len() != recipe.metadata.files.len() {
            return Err(invalid("library file mapping differs from validated metainfo").into());
        }
        for (actual, expected) in metadata.file_infos.iter().zip(&recipe.metadata.files) {
            if actual
                .relative_filename
                .to_string_lossy()
                .replace('\\', "/")
                != expected.path
                || actual.len != expected.bytes
            {
                return Err(
                    invalid("library path interpretation differs from approved metadata").into(),
                );
            }
        }
        *self
            .0
            .verified
            .lock()
            .map_err(|_| invalid("bitmap lock failed"))? =
            vec![false; recipe.metadata.pieces as usize];
        *self
            .0
            .recipe
            .lock()
            .map_err(|_| invalid("metadata lock failed"))? = Some(Arc::new(recipe));
        Ok(Storage {
            data: self.0.clone(),
            files: Mutex::new(Arc::new(vec![])),
        })
    }
    fn clone_box(&self) -> BoxStorageFactory {
        self.clone().boxed()
    }
}
pub(crate) struct Storage {
    data: Arc<Data>,
    files: Mutex<Arc<Vec<Mutex<File>>>>,
}
fn safe_parent(root: &Path, relative: &Path) -> Result<()> {
    let mut path = root.to_owned();
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            return Err(invalid("unsafe storage path").into());
        };
        path.push(name);
        if path.exists() {
            let meta = std::fs::symlink_metadata(&path)?;
            if meta.file_type().is_symlink() {
                return Err(invalid("symlink in storage path").into());
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if meta.file_attributes() & 0x400 != 0 {
                    return Err(invalid("reparse point in storage path").into());
                }
            }
        } else {
            std::fs::create_dir(&path)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
            }
        }
    }
    Ok(())
}
impl TorrentStorage for Storage {
    fn init(&mut self, _: &ManagedTorrentShared, _: &TorrentMetadata) -> Result<()> {
        let _gate = self
            .data
            .io_gate
            .read()
            .map_err(|_| invalid("IO gate failed"))?;
        if self.data.stopped.load(Ordering::Acquire) {
            return Err(invalid("torrent stopped").into());
        }
        let recipe = self.data.recipe()?;
        std::fs::create_dir_all(&self.data.root)?;
        let root_meta = std::fs::symlink_metadata(&self.data.root)?;
        if root_meta.file_type().is_symlink() {
            return Err(invalid("staging root is a symlink").into());
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if root_meta.file_attributes() & 0x400 != 0 {
                return Err(invalid("staging root is a reparse point").into());
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.data.root, std::fs::Permissions::from_mode(0o700))?;
        }
        let mut files = vec![];
        for file in &recipe.metadata.files {
            let relative = Path::new(&file.path);
            if let Some(parent) = relative.parent() {
                safe_parent(&self.data.root, parent)?;
            }
            let path = self.data.root.join(relative);
            let mut options = OpenOptions::new();
            options.read(true).write(true).create(true).truncate(false);
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
                options.custom_flags(0x00200000);
            }
            let handle = options.open(path)?;
            let meta = handle.metadata()?;
            if !meta.is_file() {
                return Err(invalid("staging path is not a regular file").into());
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if meta.file_attributes() & 0x400 != 0 {
                    return Err(invalid("staging file is a reparse point").into());
                }
            }
            #[cfg(windows)]
            if !librqbit::storage::filesystem::mark_file_sparse(&handle) {
                return Err(invalid(
                    "private torrent storage requires a sparse-capable filesystem",
                )
                .into());
            }
            if meta.len() > file.bytes {
                handle.set_len(file.bytes)?;
            }
            handle.sync_data()?;
            files.push(Mutex::new(handle));
        }
        *self
            .files
            .lock()
            .map_err(|_| invalid("storage lock failed"))? = Arc::new(files);
        Ok(())
    }
    fn pread_exact(&self, index: usize, offset: u64, buf: &mut [u8]) -> Result<()> {
        let _gate = self
            .data
            .io_gate
            .read()
            .map_err(|_| invalid("IO gate failed"))?;
        if self.data.stopped.load(Ordering::Acquire) {
            return Err(invalid("torrent stopped").into());
        }
        let files = self
            .files
            .lock()
            .map_err(|_| invalid("storage lock failed"))?
            .clone();
        let mut file = files
            .get(index)
            .ok_or_else(|| invalid("invalid file index"))?
            .lock()
            .map_err(|_| invalid("file lock failed"))?;
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(buf)?;
        Ok(())
    }
    fn pwrite_all(&self, index: usize, offset: u64, buf: &[u8]) -> Result<()> {
        let _gate = self
            .data
            .io_gate
            .read()
            .map_err(|_| invalid("IO gate failed"))?;
        if self.data.sealed.load(Ordering::Acquire) || self.data.stopped.load(Ordering::Acquire) {
            return Err(invalid("verified storage is sealed or stopped").into());
        }
        let recipe = self.data.recipe()?;
        let expected = recipe
            .metadata
            .files
            .get(index)
            .ok_or_else(|| invalid("invalid file index"))?
            .bytes;
        if offset
            .checked_add(buf.len() as u64)
            .is_none_or(|end| end > expected)
        {
            return Err(invalid("write exceeds validated file length").into());
        }
        let files = self
            .files
            .lock()
            .map_err(|_| invalid("storage lock failed"))?
            .clone();
        let mut file = files
            .get(index)
            .ok_or_else(|| invalid("invalid file index"))?
            .lock()
            .map_err(|_| invalid("file lock failed"))?;
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(buf)?;
        file.sync_data()?;
        Ok(())
    }
    fn ensure_file_length(&self, index: usize, length: u64) -> Result<()> {
        let _gate = self
            .data
            .io_gate
            .read()
            .map_err(|_| invalid("IO gate failed"))?;
        if self.data.stopped.load(Ordering::Acquire) {
            return Err(invalid("torrent stopped").into());
        }
        let files = self
            .files
            .lock()
            .map_err(|_| invalid("storage lock failed"))?
            .clone();
        let file = files
            .get(index)
            .ok_or_else(|| invalid("invalid file index"))?
            .lock()
            .map_err(|_| invalid("file lock failed"))?;
        file.set_len(length)?;
        file.sync_data()?;
        Ok(())
    }
    fn remove_file(&self, _: usize, _: &Path) -> Result<()> {
        Ok(())
    }
    fn remove_directory_if_empty(&self, _: &Path) -> Result<()> {
        Ok(())
    }
    fn take(&self) -> Result<Box<dyn TorrentStorage>> {
        Ok(Box::new(Storage {
            data: self.data.clone(),
            files: Mutex::new(std::mem::replace(
                &mut *self
                    .files
                    .lock()
                    .map_err(|_| invalid("storage lock failed"))?,
                Arc::new(vec![]),
            )),
        }))
    }
    fn on_piece_completed(&self, piece: librqbit_core::lengths::ValidPieceIndex) -> Result<()> {
        let _gate = self
            .data
            .io_gate
            .read()
            .map_err(|_| invalid("IO gate failed"))?;
        if self.data.stopped.load(Ordering::Acquire) {
            return Err(invalid("torrent stopped before piece acknowledgement").into());
        }
        self.data
            .store
            .commit_torrent_piece(self.data.id, piece.get_usize())?;
        let mut bits = self
            .data
            .verified
            .lock()
            .map_err(|_| invalid("bitmap lock failed"))?;
        *bits
            .get_mut(piece.get_usize())
            .ok_or_else(|| invalid("verified index exceeds metainfo"))? = true;
        drop(bits);
        self.data.refresh_complete()?;
        Ok(())
    }
}
