//! OS adapters. Reviewer note: Windows FFI is limited to creating an owner-only
//! DACL and freeing its allocation. Each call documents pointer lifetimes.
use std::{
    io,
    path::{Path, PathBuf},
};

#[cfg(unix)]
mod native {
    use super::*;
    use std::os::unix::{
        fs::MetadataExt,
        fs::{OpenOptionsExt, PermissionsExt},
    };
    pub type Stream = tokio::net::UnixStream;
    pub struct Listener {
        inner: tokio::net::UnixListener,
        path: PathBuf,
    }
    impl Drop for Listener {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
    impl Listener {
        pub async fn accept(&self) -> io::Result<Stream> {
            self.inner.accept().await.map(|(stream, _)| stream)
        }
    }
    pub fn endpoint(directory: &Path) -> io::Result<PathBuf> {
        Ok(std::fs::canonicalize(directory)?.join("browser-ipc/bridge.sock"))
    }
    pub async fn connect(path: &Path) -> io::Result<Stream> {
        Stream::connect(path).await
    }
    pub async fn listen(path: &Path) -> io::Result<Listener> {
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("invalid socket path"))?;
        std::fs::create_dir_all(parent)?;
        if std::fs::symlink_metadata(parent)?.file_type().is_symlink() {
            return Err(io::Error::other("unsafe socket directory"));
        }
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        if path.exists() {
            if Stream::connect(path).await.is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "bridge already running",
                ));
            }
            // Protected directory and the desktop's profile lease bound stale cleanup.
            std::fs::remove_file(path)?;
        }
        let inner = tokio::net::UnixListener::bind(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        Ok(Listener {
            inner,
            path: path.into(),
        })
    }
    pub fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(bytes)?;
        file.sync_all()
    }
    pub fn read_private(path: &Path) -> io::Result<Vec<u8>> {
        use std::io::Read;
        let meta = std::fs::symlink_metadata(path)?;
        if !meta.is_file() || meta.mode() & 0o077 != 0 || meta.len() > 512 {
            return Err(io::Error::other("unsafe token file"));
        }
        let file = std::fs::File::open(path)?;
        let opened = file.metadata()?;
        if opened.ino() != meta.ino() || opened.dev() != meta.dev() {
            return Err(io::Error::other("token changed while opening"));
        }
        let mut bytes = Vec::new();
        file.take(513).read_to_end(&mut bytes)?;
        Ok(bytes)
    }
}

#[cfg(windows)]
mod native {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::{
        os::windows::{ffi::OsStrExt, fs::MetadataExt, fs::OpenOptionsExt},
        sync::Mutex,
    };
    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions};
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::{
            Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW, SetFileSecurityW,
            DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, SECURITY_ATTRIBUTES,
        },
    };
    /// Server/client wrappers share AsyncRead/Write through a boxed stream.
    pub type Stream = Box<dyn crate::IoStream>;
    pub struct Listener {
        pending: Mutex<Option<NamedPipeServer>>,
        path: PathBuf,
    }
    fn wide(value: &std::ffi::OsStr) -> Vec<u16> {
        value.encode_wide().chain(Some(0)).collect()
    }
    struct Descriptor(*mut std::ffi::c_void);
    impl Descriptor {
        fn new() -> io::Result<Self> {
            let value = wide(std::ffi::OsStr::new("D:P(A;;GA;;;OW)"));
            let mut descriptor = std::ptr::null_mut();
            // SAFETY: terminated UTF-16 input and writable output live through the
            // call. Windows allocates the descriptor, freed by Descriptor::drop.
            if unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    value.as_ptr(),
                    1,
                    &mut descriptor,
                    std::ptr::null_mut(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(Self(descriptor))
        }
    }
    impl Drop for Descriptor {
        fn drop(&mut self) {
            // SAFETY: pointer originates from the SDDL conversion allocation and
            // is freed once after every API using it has returned.
            unsafe {
                LocalFree(self.0);
            }
        }
    }
    fn pipe(path: &Path, first: bool) -> io::Result<NamedPipeServer> {
        let descriptor = Descriptor::new()?;
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: 0,
        };
        // SAFETY: attributes and its descriptor are valid during synchronous
        // CreateNamedPipe; Windows copies the security descriptor into the pipe.
        unsafe {
            ServerOptions::new()
                .first_pipe_instance(first)
                .reject_remote_clients(true)
                .create_with_security_attributes_raw(path, &mut attributes as *mut _ as *mut _)
        }
    }
    pub fn endpoint(directory: &Path) -> io::Result<PathBuf> {
        let directory = std::fs::canonicalize(directory)?
            .to_string_lossy()
            .to_lowercase();
        let digest = Sha256::digest(directory.as_bytes());
        Ok(PathBuf::from(format!(
            "\\\\.\\pipe\\okytes-browser-{}",
            crate::hex(&digest)
        )))
    }
    pub async fn connect(path: &Path) -> io::Result<Stream> {
        for _ in 0..20 {
            match ClientOptions::new().open(path) {
                Ok(stream) => return Ok(Box::new(stream)),
                Err(error) if error.raw_os_error() == Some(231) => {
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await
                }
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(io::ErrorKind::WouldBlock, "bridge busy"))
    }
    pub async fn listen(path: &Path) -> io::Result<Listener> {
        Ok(Listener {
            pending: Mutex::new(Some(pipe(path, true)?)),
            path: path.into(),
        })
    }
    impl Listener {
        pub async fn accept(&self) -> io::Result<Stream> {
            let stream = self
                .pending
                .lock()
                .map_err(|_| io::Error::other("pipe lock"))?
                .take()
                .ok_or_else(|| io::Error::other("pipe missing"))?;
            stream.connect().await?;
            let next = pipe(&self.path, false)?;
            *self
                .pending
                .lock()
                .map_err(|_| io::Error::other("pipe lock"))? = Some(next);
            Ok(Box::new(stream))
        }
    }
    pub fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(0)
            .open(path)?;
        let descriptor = Descriptor::new()?;
        let name = wide(path.as_os_str());
        // SAFETY: path and descriptor are live during the synchronous call.
        // Apply a protected owner-only DACL before writing any secret bytes.
        if unsafe {
            SetFileSecurityW(
                name.as_ptr(),
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                descriptor.0,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        file.write_all(bytes)?;
        file.sync_all()
    }
    pub fn read_private(path: &Path) -> io::Result<Vec<u8>> {
        use std::io::Read;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(0x00200000)
            .open(path)?;
        let meta = file.metadata()?;
        if !meta.is_file() || meta.file_attributes() & 0x400 != 0 || meta.len() > 512 {
            return Err(io::Error::other("unsafe token file"));
        }
        let mut bytes = Vec::new();
        file.take(513).read_to_end(&mut bytes)?;
        Ok(bytes)
    }
}
pub use native::*;
