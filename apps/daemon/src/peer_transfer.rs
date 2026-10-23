//! Filesystem boundary for peer transfers.
//!
//! Sender paths never cross the wire. Receivers write only beneath a private
//! staging directory, reject links and special files at every boundary, sync
//! verified bytes, and publish the complete tree with one rename.

use okytes_core::peer::{Entry, Hash, Manifest, MAX_CHUNK};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
};
use uuid::Uuid;

/// Payload chunks are deliberately small and independent from control frames.
pub const CHUNK_SIZE: usize = MAX_CHUNK;

/// A validated local source tree bound to one immutable wire manifest.
pub struct Source {
    manifest: Manifest,
    files: BTreeMap<String, PathBuf>,
}

impl Source {
    /// Enumerate selected regular files/directories without following links.
    pub fn open(paths: &[PathBuf]) -> io::Result<Self> {
        if paths.is_empty() {
            return Err(io::Error::other("select at least one source"));
        }
        let mut files = BTreeMap::new();
        let mut entries = Vec::new();
        for path in paths {
            let name = path
                .file_name()
                .and_then(|v| v.to_str())
                .ok_or_else(|| io::Error::other("source name is not portable UTF-8"))?;
            enumerate(path, name, &mut entries, &mut files)?;
        }
        let manifest = Manifest::new(entries).map_err(io::Error::other)?;
        Ok(Self { manifest, files })
    }

    /// Immutable metadata offered to the receiver.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Read one bounded chunk, detecting source replacement or mutation.
    pub fn read(&self, path: &str, offset: u64) -> io::Result<Vec<u8>> {
        let local = self
            .files
            .get(path)
            .ok_or_else(|| io::Error::other("unknown source file"))?;
        let expected = self
            .manifest
            .entries
            .iter()
            .find_map(|entry| match entry {
                Entry::File {
                    path: p,
                    size,
                    sha256,
                } if p == path => Some((*size, sha256)),
                _ => None,
            })
            .ok_or_else(|| io::Error::other("source manifest mismatch"))?;
        let metadata = fs::symlink_metadata(local)?;
        if !metadata.file_type().is_file() || metadata.len() != expected.0 || offset > expected.0 {
            return Err(io::Error::other("source changed after approval"));
        }
        let mut file = File::open(local)?;
        file.seek(SeekFrom::Start(offset))?;
        let remaining = expected.0 - offset;
        let limit = remaining.min(CHUNK_SIZE as u64) as usize;
        let mut bytes = vec![0; limit];
        file.read_exact(&mut bytes)?;
        if offset + bytes.len() as u64 == expected.0 {
            let current = digest_file(local)?;
            if &current != expected.1 {
                return Err(io::Error::other("source changed after approval"));
            }
        }
        Ok(bytes)
    }
}

fn enumerate(
    local: &Path,
    relative: &str,
    entries: &mut Vec<Entry>,
    files: &mut BTreeMap<String, PathBuf>,
) -> io::Result<()> {
    let metadata = fs::symlink_metadata(local)?;
    let kind = metadata.file_type();
    if kind.is_symlink() {
        return Err(io::Error::other("symbolic links cannot be transferred"));
    }
    if kind.is_file() {
        let digest = digest_file(local)?;
        entries.push(Entry::File {
            path: relative.replace('\\', "/"),
            size: metadata.len(),
            sha256: digest,
        });
        files.insert(relative.replace('\\', "/"), local.to_path_buf());
        return Ok(());
    }
    if !kind.is_dir() {
        return Err(io::Error::other(
            "only regular files and directories are supported",
        ));
    }
    entries.push(Entry::Directory {
        path: relative.replace('\\', "/"),
    });
    let mut children = fs::read_dir(local)?.collect::<Result<Vec<_>, _>>()?;
    children.sort_by_key(|entry| entry.file_name());
    for child in children {
        let name = child
            .file_name()
            .into_string()
            .map_err(|_| io::Error::other("source name is not portable UTF-8"))?;
        enumerate(&child.path(), &format!("{relative}/{name}"), entries, files)?;
    }
    Ok(())
}

fn digest_file(path: &Path) -> io::Result<Hash> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; CHUNK_SIZE];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Hash::try_from(format!("{:x}", hasher.finalize())).map_err(io::Error::other)
}

/// Crash-resumable receiver for one approved request.
#[derive(Debug)]
pub struct Receiver {
    manifest: Manifest,
    staging: PathBuf,
    final_path: PathBuf,
}

impl Receiver {
    /// Open or recover a private staging tree beneath an existing destination.
    pub fn open(
        root: &Path,
        request: Uuid,
        final_name: &str,
        manifest: Manifest,
    ) -> io::Result<Self> {
        manifest.validate().map_err(io::Error::other)?;
        if request.is_nil() || !safe_component(final_name) {
            return Err(io::Error::other("invalid peer destination"));
        }
        let root = fs::canonicalize(root)?;
        reject_link(&root)?;
        let staging = root.join(format!(".okytes-peer-{request}.part"));
        let final_path = root.join(final_name);
        if final_path.exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "destination exists",
            ));
        }
        match fs::create_dir(&staging) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => reject_link(&staging)?,
            Err(error) => return Err(error),
        }
        for entry in &manifest.entries {
            match entry {
                Entry::Directory { path } => create_directory(&staging, path)?,
                Entry::File { path, size, .. } => {
                    let target = wire_path(&staging, path)?;
                    ensure_parent_chain(
                        &staging,
                        target
                            .parent()
                            .ok_or_else(|| io::Error::other("missing parent"))?,
                    )?;
                    match OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&target)
                    {
                        Ok(file) => {
                            file.sync_data()?;
                        }
                        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                            let meta = fs::symlink_metadata(&target)?;
                            if !meta.file_type().is_file() || meta.len() > *size {
                                return Err(io::Error::other("unsafe or oversized partial file"));
                            }
                        }
                        Err(error) => return Err(error),
                    }
                }
            }
        }
        Ok(Self {
            manifest,
            staging,
            final_path,
        })
    }

    /// Durable offset available for requesting the next chunk after restart.
    pub fn offset(&self, path: &str) -> io::Result<u64> {
        let (target, size, _) = self.file(path)?;
        match fs::symlink_metadata(target) {
            Ok(meta) if meta.file_type().is_file() && meta.len() <= size => Ok(meta.len()),
            Ok(_) => Err(io::Error::other("unsafe or oversized partial file")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
            Err(error) => Err(error),
        }
    }

    /// Reconcile a staged file to the last committed database offset. Bytes
    /// synced just before a crash but not checkpointed are discarded safely.
    pub fn resume(&self, path: &str, durable: u64) -> io::Result<u64> {
        let (target, size, _) = self.file(path)?;
        if durable > size {
            return Err(io::Error::other("durable offset exceeds manifest"));
        }
        let metadata = fs::symlink_metadata(&target)?;
        if !metadata.file_type().is_file() || metadata.len() < durable {
            return Err(io::Error::other(
                "partial file is behind durable checkpoint",
            ));
        }
        if metadata.len() > durable {
            OpenOptions::new()
                .write(true)
                .open(&target)?
                .set_len(durable)?;
        }
        Ok(durable)
    }

    /// Append exactly the requested bounded chunk and sync it before returning.
    pub fn write(&self, path: &str, offset: u64, bytes: &[u8]) -> io::Result<u64> {
        if bytes.is_empty() || bytes.len() > CHUNK_SIZE {
            return Err(io::Error::other("invalid peer payload chunk"));
        }
        let (target, size, _) = self.file(path)?;
        if offset
            .checked_add(bytes.len() as u64)
            .is_none_or(|end| end > size)
        {
            return Err(io::Error::other("peer payload exceeds manifest"));
        }
        ensure_parent_chain(
            &self.staging,
            target
                .parent()
                .ok_or_else(|| io::Error::other("missing parent"))?,
        )?;
        let mut options = OpenOptions::new();
        options.write(true).create(true);
        let mut file = options.open(&target)?;
        if file.metadata()?.len() != offset {
            return Err(io::Error::other("peer payload offset mismatch"));
        }
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(bytes)?;
        file.sync_data()?;
        Ok(offset + bytes.len() as u64)
    }

    /// Verify every byte, sync directories, and atomically publish the tree.
    pub fn publish(self) -> io::Result<PathBuf> {
        for entry in &self.manifest.entries {
            if let Entry::File { path, size, sha256 } = entry {
                let (target, _, _) = self.file(path)?;
                let meta = fs::symlink_metadata(&target)?;
                if !meta.file_type().is_file()
                    || meta.len() != *size
                    || digest_file(&target)? != *sha256
                {
                    return Err(io::Error::other("peer payload integrity check failed"));
                }
            }
        }
        sync_directory(&self.staging)?;
        fs::rename(&self.staging, &self.final_path)?;
        if let Some(parent) = self.final_path.parent() {
            sync_directory(parent)?;
        }
        Ok(self.final_path)
    }

    fn file(&self, path: &str) -> io::Result<(PathBuf, u64, &Hash)> {
        let (size, hash) = self
            .manifest
            .entries
            .iter()
            .find_map(|entry| match entry {
                Entry::File {
                    path: item,
                    size,
                    sha256,
                } if item == path => Some((*size, sha256)),
                _ => None,
            })
            .ok_or_else(|| io::Error::other("unknown peer payload path"))?;
        Ok((wire_path(&self.staging, path)?, size, hash))
    }
}

fn wire_path(root: &Path, value: &str) -> io::Result<PathBuf> {
    let mut result = root.to_path_buf();
    for part in value.split('/') {
        if !safe_component(part) {
            return Err(io::Error::other("unsafe peer path"));
        }
        result.push(part);
    }
    Ok(result)
}

fn safe_component(value: &str) -> bool {
    !value.is_empty()
        && Path::new(value)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
        && !value.contains(['/', '\\'])
}

fn reject_link(path: &Path) -> io::Result<()> {
    let kind = fs::symlink_metadata(path)?.file_type();
    if kind.is_symlink() || !kind.is_dir() {
        Err(io::Error::other("unsafe destination boundary"))
    } else {
        Ok(())
    }
}

fn create_directory(root: &Path, wire: &str) -> io::Result<()> {
    let target = wire_path(root, wire)?;
    let parent = target
        .parent()
        .ok_or_else(|| io::Error::other("missing parent"))?;
    ensure_parent_chain(root, parent)?;
    match fs::create_dir(&target) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => reject_link(&target),
        Err(error) => Err(error),
    }
}

fn ensure_parent_chain(root: &Path, parent: &Path) -> io::Result<()> {
    if !parent.starts_with(root) {
        return Err(io::Error::other("destination escaped staging"));
    }
    let mut current = root.to_path_buf();
    for component in parent
        .strip_prefix(root)
        .map_err(|_| io::Error::other("destination escaped staging"))?
        .components()
    {
        current.push(component);
        reject_link(&current)?;
    }
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}
