//! Loopback-only FTP/FTPS and SSH/SFTP fault-injection fixtures.
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    net::TcpListener,
    sync::RwLock,
    task::JoinHandle,
};
/// Deterministic remote content, identity changes and transfer failure controls.
pub struct Remote {
    /// File bytes exposed as `/file.bin`.
    pub bytes: RwLock<Vec<u8>>,
    /// Last-modified version.
    pub version: AtomicU64,
    /// Fail transfers once they reach this offset; u64::MAX disables failure.
    pub drop_at: AtomicU64,
    /// Recorded REST/SFTP read offsets.
    pub offsets: tokio::sync::Mutex<Vec<u64>>,
}
impl Remote {
    /// Construct a repeatable file fixture.
    pub fn new(bytes: Vec<u8>) -> Arc<Self> {
        Arc::new(Self {
            bytes: RwLock::new(bytes),
            version: AtomicU64::new(1),
            drop_at: AtomicU64::new(u64::MAX),
            offsets: tokio::sync::Mutex::new(Vec::new()),
        })
    }
}
/// Running fixture; dropping it stops admission and all open FTP connections.
pub struct FileServer {
    /// Loopback address.
    pub address: std::net::SocketAddr,
    /// Trusted FTPS certificate when TLS is enabled.
    pub certificate: Option<rustls::pki_types::CertificateDer<'static>>,
    /// SSH fingerprint when serving SFTP.
    pub fingerprint: Option<String>,
    task: JoinHandle<()>,
}
impl Drop for FileServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}
type Socket = Box<dyn Io>;
async fn reply(socket: &mut BufReader<Socket>, value: &str) -> std::io::Result<()> {
    socket.get_mut().write_all(value.as_bytes()).await?;
    socket.get_mut().flush().await
}
impl FileServer {
    /// Start a passive FTP or explicit FTPS server with fixed `user`/`password` login.
    pub async fn ftp(remote: Arc<Remote>, tls: bool) -> std::io::Result<Self> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let (certificate, acceptor) = if tls {
            let rcgen::CertifiedKey { cert, signing_key } =
                rcgen::generate_simple_self_signed(vec!["localhost".into()])
                    .map_err(std::io::Error::other)?;
            let certificate = cert.der().clone();
            let key = rustls::pki_types::PrivatePkcs8KeyDer::from(signing_key.serialize_der());
            let config = rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(vec![certificate.clone()], key.into())
                .map_err(std::io::Error::other)?;
            (
                Some(certificate),
                Some(tokio_rustls::TlsAcceptor::from(Arc::new(config))),
            )
        } else {
            (None, None)
        };
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let task = tokio::spawn(async move {
            let mut tasks = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted=listener.accept()=>{let Ok((socket,_))=accepted else{break};let remote=remote.clone();let acceptor=acceptor.clone();tasks.spawn(async move{let _=serve_ftp(Box::new(socket),remote,acceptor).await;});}
                    Some(_)=tasks.join_next()=>{}
                }
            }
        });
        Ok(Self {
            address,
            certificate,
            fingerprint: None,
            task,
        })
    }
    /// Start an SSH/SFTP server, optionally using a supplied host key.
    pub async fn sftp(
        remote: Arc<Remote>,
        key: Option<russh::keys::PrivateKey>,
    ) -> Result<Self, russh::Error> {
        let key = match key {
            Some(key) => key,
            None => russh::keys::PrivateKey::random(
                &mut rand_new::rng(),
                russh::keys::Algorithm::Ed25519,
            )
            .map_err(|_| russh::Error::Inconsistent)?,
        };
        let fingerprint = key
            .public_key()
            .fingerprint(russh::keys::HashAlg::Sha256)
            .to_string();
        let config = Arc::new(russh::server::Config {
            keys: vec![key],
            auth_rejection_time: Duration::ZERO,
            auth_rejection_time_initial: Some(Duration::ZERO),
            ..Default::default()
        });
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let task = tokio::spawn(async move {
            let mut tasks = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted=listener.accept()=>{let Ok((socket,_))=accepted else{break};let remote=remote.clone();let config=config.clone();tasks.spawn(async move{if let Ok(session)=russh::server::run_stream(config,socket,Ssh{remote,channels:HashMap::new()}).await{let _=session.await;}});}
                    Some(_)=tasks.join_next()=>{}
                }
            }
        });
        Ok(Self {
            address,
            certificate: None,
            fingerprint: Some(fingerprint),
            task,
        })
    }
}
async fn serve_ftp(
    socket: Socket,
    remote: Arc<Remote>,
    acceptor: Option<tokio_rustls::TlsAcceptor>,
) -> std::io::Result<()> {
    let mut socket = BufReader::new(socket);
    reply(&mut socket, "220 Okytes fixture\r\n").await?;
    let mut passive = None;
    let mut offset = 0;
    let mut logged = false;
    let mut username = String::new();
    let mut protected = false;
    loop {
        let mut line = String::new();
        if socket.read_line(&mut line).await? == 0 {
            return Ok(());
        }
        let (command, arg) = line
            .trim_end()
            .split_once(' ')
            .unwrap_or((line.trim_end(), ""));
        match command {
            "AUTH" => {
                let Some(acceptor) = &acceptor else {
                    reply(&mut socket, "502 No TLS\r\n").await?;
                    continue;
                };
                reply(&mut socket, "234 Start TLS\r\n").await?;
                let raw = socket.into_inner();
                socket = BufReader::new(Box::new(acceptor.accept(raw).await?));
            }
            "PBSZ" | "TYPE" | "NOOP" => reply(&mut socket, "200 OK\r\n").await?,
            "PROT" => {
                protected = arg == "P";
                reply(&mut socket, "200 OK\r\n").await?;
            }
            "USER" => {
                username = arg.into();
                reply(&mut socket, "331 Password\r\n").await?;
            }
            "PASS" => {
                logged = (username == "user" && arg == "password") || username == "anonymous";
                reply(
                    &mut socket,
                    if logged {
                        "230 Logged in\r\n"
                    } else {
                        "530 Bad credentials\r\n"
                    },
                )
                .await?;
            }
            _ if !logged => reply(&mut socket, "530 Login required\r\n").await?,
            "SIZE" => {
                reply(
                    &mut socket,
                    &format!("213 {}\r\n", remote.bytes.read().await.len()),
                )
                .await?
            }
            "MDTM" => {
                reply(
                    &mut socket,
                    &format!(
                        "213 {}\r\n",
                        20260101000000u64 + remote.version.load(Ordering::SeqCst)
                    ),
                )
                .await?
            }
            "REST" => {
                offset = arg.parse().unwrap_or(0);
                remote.offsets.lock().await.push(offset);
                reply(&mut socket, "350 Offset accepted\r\n").await?;
            }
            "EPSV" => {
                let listener = TcpListener::bind("127.0.0.1:0").await?;
                reply(
                    &mut socket,
                    &format!(
                        "229 Entering Extended Passive Mode (|||{}|)\r\n",
                        listener.local_addr()?.port()
                    ),
                )
                .await?;
                passive = Some(listener);
            }
            "RETR" => {
                let Some(listener) = passive.take() else {
                    reply(&mut socket, "425 No data connection\r\n").await?;
                    continue;
                };
                reply(&mut socket, "150 Data follows\r\n").await?;
                let (data, _) = listener.accept().await?;
                let mut data: Socket = if protected {
                    Box::new(
                        acceptor
                            .as_ref()
                            .ok_or_else(|| std::io::Error::other("TLS missing"))?
                            .accept(data)
                            .await?,
                    )
                } else {
                    Box::new(data)
                };
                let bytes = remote.bytes.read().await.clone();
                let limit = remote
                    .drop_at
                    .load(Ordering::SeqCst)
                    .min(bytes.len() as u64) as usize;
                let start = (offset as usize).min(limit);
                for chunk in bytes[start..limit].chunks(8192) {
                    if data.write_all(chunk).await.is_err() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
                let _ = data.shutdown().await;
                reply(
                    &mut socket,
                    if limit < bytes.len() {
                        "426 Connection dropped\r\n"
                    } else {
                        "226 Complete\r\n"
                    },
                )
                .await?;
            }
            "QUIT" => {
                reply(&mut socket, "221 Goodbye\r\n").await?;
                return Ok(());
            }
            _ => reply(&mut socket, "502 Unsupported\r\n").await?,
        }
    }
}
struct Ssh {
    remote: Arc<Remote>,
    channels: HashMap<russh::ChannelId, russh::Channel<russh::server::Msg>>,
}
impl russh::server::Handler for Ssh {
    type Error = russh::Error;
    async fn auth_password(
        &mut self,
        user: &str,
        password: &str,
    ) -> Result<russh::server::Auth, Self::Error> {
        Ok(if user == "user" && password == "password" {
            russh::server::Auth::Accept
        } else {
            russh::server::Auth::Reject {
                proceed_with_methods: None,
                partial_success: false,
            }
        })
    }
    async fn channel_open_session(
        &mut self,
        channel: russh::Channel<russh::server::Msg>,
        reply: russh::server::ChannelOpenHandle,
        _: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        self.channels.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }
    async fn subsystem_request(
        &mut self,
        id: russh::ChannelId,
        name: &str,
        session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        if name != "sftp" {
            session.channel_failure(id)?;
            return Ok(());
        }
        let channel = self
            .channels
            .remove(&id)
            .ok_or(russh::Error::Inconsistent)?;
        session.channel_success(id)?;
        let remote = self.remote.clone();
        tokio::spawn(async move {
            russh_sftp::server::run(channel.into_stream(), Sftp { remote }).await;
        });
        Ok(())
    }
}
struct Sftp {
    remote: Arc<Remote>,
}
use russh_sftp::protocol::{Attrs, Data, FileAttributes, Handle, OpenFlags, Status, StatusCode};
impl Sftp {
    async fn attributes(&self) -> FileAttributes {
        FileAttributes {
            size: Some(self.remote.bytes.read().await.len() as u64),
            mtime: Some(self.remote.version.load(Ordering::SeqCst) as u32),
            ..Default::default()
        }
    }
}
impl russh_sftp::server::Handler for Sftp {
    type Error = StatusCode;
    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }
    async fn stat(&mut self, id: u32, _: String) -> Result<Attrs, Self::Error> {
        Ok(Attrs {
            id,
            attrs: self.attributes().await,
        })
    }
    async fn fstat(&mut self, id: u32, _: String) -> Result<Attrs, Self::Error> {
        Ok(Attrs {
            id,
            attrs: self.attributes().await,
        })
    }
    async fn open(
        &mut self,
        id: u32,
        path: String,
        _: OpenFlags,
        _: FileAttributes,
    ) -> Result<Handle, Self::Error> {
        if path != "/file.bin" {
            return Err(StatusCode::NoSuchFile);
        }
        Ok(Handle { id, handle: path })
    }
    async fn read(
        &mut self,
        id: u32,
        _: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, Self::Error> {
        tokio::time::sleep(Duration::from_millis(2)).await;
        self.remote.offsets.lock().await.push(offset);
        if offset >= self.remote.drop_at.load(Ordering::SeqCst) {
            return Err(StatusCode::ConnectionLost);
        }
        let bytes = self.remote.bytes.read().await;
        let start = usize::try_from(offset).map_err(|_| StatusCode::Failure)?;
        if start >= bytes.len() {
            return Err(StatusCode::Eof);
        }
        Ok(Data {
            id,
            data: bytes[start..bytes.len().min(start + len as usize).min(start + 8192)].to_vec(),
        })
    }
    async fn close(&mut self, id: u32, _: String) -> Result<Status, Self::Error> {
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: String::new(),
            language_tag: String::new(),
        })
    }
}
