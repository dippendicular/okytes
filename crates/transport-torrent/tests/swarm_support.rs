//! Local generated swarm fixture. Only the tracker response is a test fixture;
//! every BitTorrent peer and verifier is the maintained library.
#![allow(dead_code)]
use librqbit::storage::{
    filesystem::FilesystemStorageFactory, BoxStorageFactory, StorageFactory, StorageFactoryExt,
    TorrentStorage,
};
use librqbit::{
    create_torrent, spawn_utils::BlockingSpawner, AddTorrent, AddTorrentOptions,
    CreateTorrentOptions, ListenerOptions, ManagedTorrentShared, Session, SessionOptions,
    TorrentMetadata,
};
use okytes_core::traffic::{TrafficController, TrafficPolicy};
use okytes_persistence::SqliteStore;
use okytes_transport_torrent::Adapter;
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

pub struct Tracker {
    pub url: String,
    pub peers: Arc<Mutex<Vec<SocketAddr>>>,
    pub announces: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Tracker {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Tracker {
    pub async fn new() -> Self {
        use axum::{response::IntoResponse, routing::get, Router};
        let peers = Arc::new(Mutex::new(Vec::<SocketAddr>::new()));
        let announces = Arc::new(AtomicUsize::new(0));
        let p = peers.clone();
        let count = announces.clone();
        let router = Router::new().route(
            "/announce",
            get(move || {
                let p = p.clone();
                let count = count.clone();
                async move {
                    count.fetch_add(1, Ordering::Relaxed);
                    let mut compact = vec![];
                    for peer in p.lock().unwrap().iter() {
                        if let std::net::IpAddr::V4(ip) = peer.ip() {
                            compact.extend_from_slice(&ip.octets());
                            compact.extend_from_slice(&peer.port().to_be_bytes());
                        }
                    }
                    let mut bytes = format!("d8:intervali1e5:peers{}:", compact.len()).into_bytes();
                    bytes.extend(compact);
                    bytes.push(b'e');
                    ([(axum::http::header::CONTENT_TYPE, "text/plain")], bytes).into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/announce", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            url,
            peers,
            announces,
            task,
        }
    }
}
pub fn adapter(root: &Path, policy: TrafficPolicy) -> (Arc<Adapter>, Arc<SqliteStore>) {
    let store = Arc::new(SqliteStore::open(root.join("jobs.sqlite")).unwrap());
    let (events, _) = tokio::sync::broadcast::channel(64);
    let adapter = Adapter::new(
        store.clone(),
        root.join("private"),
        root.join("out"),
        Arc::new(TrafficController::new(policy.clone()).unwrap()),
        policy,
        events,
    )
    .unwrap();
    (adapter, store)
}
pub struct Seeder {
    pub session: Arc<Session>,
    pub raw: Vec<u8>,
    pub magnet: String,
    pub payload: Vec<u8>,
    pub source: PathBuf,
    pub bad: Arc<AtomicBool>,
    pub bad_reads: Arc<AtomicUsize>,
}
impl Seeder {
    pub async fn new(root: &Path, size: usize, tracker: Option<&Tracker>, corrupt: bool) -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let source = root.join("generated");
        std::fs::create_dir_all(&source).unwrap();
        let payload = (0..size)
            .map(|i| (i.wrapping_mul(31) % 251) as u8)
            .collect::<Vec<_>>();
        std::fs::write(source.join("payload.bin"), &payload).unwrap();
        let meta = create_torrent(
            &source,
            CreateTorrentOptions {
                name: Some("generated"),
                piece_length: Some(16384),
                trackers: tracker.map(|t| vec![t.url.clone()]).unwrap_or_default(),
            },
            &BlockingSpawner::new(2),
        )
        .await
        .unwrap();
        let raw = meta.as_bytes().unwrap().to_vec();
        let bad = Arc::new(AtomicBool::new(false));
        let bad_reads = Arc::new(AtomicUsize::new(0));
        let session = Session::new_with_opts(
            source.clone(),
            SessionOptions {
                dht: None,
                disable_local_service_discovery: true,
                listen: Some(ListenerOptions {
                    listen_addr: "127.0.0.1:0".parse().unwrap(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let handle = session
            .add_torrent(
                AddTorrent::from_bytes(raw.clone()),
                Some(AddTorrentOptions {
                    overwrite: true,
                    output_folder: Some(source.to_string_lossy().into_owned()),
                    disable_pex: true,
                    storage_factory: Some(
                        BadFactory {
                            bad: bad.clone(),
                            reads: bad_reads.clone(),
                        }
                        .boxed(),
                    ),
                    ..Default::default()
                }),
            )
            .await
            .unwrap()
            .into_handle()
            .unwrap();
        handle.wait_until_initialized().await.unwrap();
        assert!(handle.stats().finished);
        bad.store(corrupt, Ordering::Release);
        if let Some(tracker) = tracker {
            tracker
                .peers
                .lock()
                .unwrap()
                .push(session.listen_addr().unwrap());
        }
        Self {
            session,
            raw,
            magnet: meta.as_magnet().to_string(),
            payload,
            source,
            bad,
            bad_reads,
        }
    }
}
#[derive(Clone)]
struct BadFactory {
    bad: Arc<AtomicBool>,
    reads: Arc<AtomicUsize>,
}
impl StorageFactory for BadFactory {
    type Storage = BadStorage;
    fn create(
        &self,
        shared: &ManagedTorrentShared,
        meta: &TorrentMetadata,
    ) -> anyhow::Result<BadStorage> {
        Ok(BadStorage {
            inner: Box::new(FilesystemStorageFactory {}.create(shared, meta)?),
            bad: self.bad.clone(),
            reads: self.reads.clone(),
        })
    }
    fn clone_box(&self) -> BoxStorageFactory {
        self.clone().boxed()
    }
}
struct BadStorage {
    inner: Box<dyn TorrentStorage>,
    bad: Arc<AtomicBool>,
    reads: Arc<AtomicUsize>,
}
impl TorrentStorage for BadStorage {
    fn init(
        &mut self,
        shared: &ManagedTorrentShared,
        meta: &TorrentMetadata,
    ) -> anyhow::Result<()> {
        self.inner.init(shared, meta)
    }
    fn pread_exact(&self, index: usize, offset: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        self.inner.pread_exact(index, offset, buf)?;
        if self.bad.load(Ordering::Acquire) && !buf.is_empty() {
            buf[0] ^= 1;
            self.reads.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }
    fn pwrite_all(&self, index: usize, offset: u64, buf: &[u8]) -> anyhow::Result<()> {
        self.inner.pwrite_all(index, offset, buf)
    }
    fn ensure_file_length(&self, index: usize, len: u64) -> anyhow::Result<()> {
        self.inner.ensure_file_length(index, len)
    }
    fn remove_file(&self, index: usize, path: &Path) -> anyhow::Result<()> {
        self.inner.remove_file(index, path)
    }
    fn remove_directory_if_empty(&self, path: &Path) -> anyhow::Result<()> {
        self.inner.remove_directory_if_empty(path)
    }
    fn take(&self) -> anyhow::Result<Box<dyn TorrentStorage>> {
        Ok(Box::new(Self {
            inner: self.inner.take()?,
            bad: self.bad.clone(),
            reads: self.reads.clone(),
        }))
    }
}
