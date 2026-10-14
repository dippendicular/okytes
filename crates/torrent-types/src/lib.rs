//! Portable torrent app contract. No sockets, filesystem access or P2P engine.
use okytes_core::desktop::Reply;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Explicit peer discovery and network configuration.
#[derive(Debug, Clone, Serialize, Deserialize, TS, utoipa::ToSchema)]
#[serde(default, deny_unknown_fields)]
#[ts(export)]
#[schema(as=TorrentPrivacy)]
pub struct Privacy {
    /// Enable public DHT discovery; off by default.
    pub dht: bool,
    /// Exchange peer addresses; off by default.
    pub pex: bool,
    /// Enable local multicast discovery; off by default.
    pub local_discovery: bool,
    /// DHT bootstrap overrides; empty means no bootstrap network traffic.
    pub bootstrap_nodes: Option<Vec<String>>,
    /// Listening address; loopback by default. No UPnP mapping is requested.
    pub listen: String,
    /// Supported on Linux/macOS only; interface name, not a filesystem path.
    pub bind_device: Option<String>,
    /// Optional SOCKS5 peer proxy, without embedded credentials.
    pub peer_proxy: Option<String>,
    /// Only plaintext is supported by the pinned engine. Other values fail.
    pub encryption: String,
}
impl Default for Privacy {
    fn default() -> Self {
        Self {
            dht: false,
            pex: false,
            local_discovery: false,
            bootstrap_nodes: None,
            listen: "127.0.0.1:0".into(),
            bind_device: None,
            peer_proxy: None,
            encryption: "plaintext".into(),
        }
    }
}

/// Torrent options persisted together with the job.
#[derive(Debug, Clone, Serialize, Deserialize, TS, utoipa::ToSchema)]
#[serde(default, deny_unknown_fields)]
#[ts(export)]
#[schema(as=TorrentOptions)]
pub struct Options {
    /// Null selects all files; otherwise a nonempty list of file indices.
    pub files: Option<Vec<u32>>,
    /// Per-torrent download cap, in bytes/second.
    #[ts(type = "number | null")]
    pub download_bps: Option<u64>,
    /// Per-torrent upload cap, in bytes/second.
    #[ts(type = "number | null")]
    pub upload_bps: Option<u64>,
    /// Maximum upload/download ratio; null adds no ratio restriction.
    pub seed_ratio: Option<f64>,
    /// Seconds to seed after completion; zero stops at completion. Null is explicit unlimited time.
    #[ts(type = "number | null")]
    pub seed_seconds: Option<u64>,
    /// Bounded peer connections, 1..32.
    pub peers: u32,
    /// Discovery and socket settings.
    pub privacy: Privacy,
    /// Explicit initial peer addresses, maximum 32; useful for local/manual swarms.
    pub initial_peers: Vec<String>,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            files: None,
            download_bps: None,
            upload_bps: None,
            seed_ratio: None,
            seed_seconds: Some(0),
            peers: 16,
            privacy: Default::default(),
            initial_peers: vec![],
        }
    }
}

/// Exactly one source must be supplied. Metainfo is base64, maximum 512 KiB decoded.
#[derive(Debug, Clone, Serialize, Deserialize, TS, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[ts(export)]
#[schema(as=TorrentInput)]
pub struct Input {
    /// Magnet URI, no larger than 8192 bytes.
    pub magnet: Option<String>,
    /// Base64 .torrent contents.
    pub metainfo: Option<String>,
    /// Absolute destination override; null uses profile settings.
    pub destination: Option<String>,
    /// Validated file/discovery/seeding preferences.
    #[serde(default)]
    pub options: Options,
}
/// One metainfo file, independent of HTTP segment semantics.
#[derive(Debug, Clone, Serialize, Deserialize, TS, utoipa::ToSchema)]
#[ts(export)]
#[schema(as=TorrentFile)]
pub struct File {
    /// Stable file index.
    pub index: u32,
    /// Validated relative filename.
    pub path: String,
    /// File length; bounded below JavaScript's exact integer limit.
    #[ts(type = "number")]
    pub bytes: u64,
    /// Whether this file is selected for publication.
    pub selected: bool,
}
/// Metadata preview, with no file creation or transfer admission.
#[derive(Debug, Clone, Serialize, Deserialize, TS, utoipa::ToSchema)]
#[ts(export)]
#[schema(as=TorrentMetadata)]
pub struct Metadata {
    /// v1 info hash, verified during resolution.
    pub info_hash: String,
    /// Safe torrent display name.
    pub name: String,
    /// Bounded file list.
    pub files: Vec<File>,
    /// Total number of verified-piece slots.
    pub pieces: u32,
    /// Piece size in bytes.
    pub piece_bytes: u32,
    /// Configured tracker endpoints, sanitized for display.
    pub trackers: Vec<String>,
}
/// Bounded live torrent diagnostics.
#[derive(Debug, Clone, Serialize, Deserialize, TS, utoipa::ToSchema)]
#[ts(export)]
#[schema(as=TorrentSnapshot)]
pub struct Snapshot {
    /// Unified job UUID.
    pub id: String,
    /// Resolving, downloading, paused, publishing, seeding, stopped or failed.
    pub activity: String,
    /// Metadata once resolved.
    pub metadata: Option<Metadata>,
    /// Verified and durable pieces, never HTTP byte segments.
    pub verified: Vec<bool>,
    /// Serialized maintained-engine peer statistics; count remains bounded.
    pub peers: String,
    /// Last actionable adapter error.
    pub error: Option<String>,
    /// Actual live upload counter, or its last recorded value after stopping.
    #[ts(type = "number")]
    pub uploaded_bytes: u64,
    /// Active options.
    pub options: Options,
    /// Tracker discovery state; does not imply successful announce telemetry.
    pub tracker_status: String,
}
/// Functions implemented by embedded and daemon app backends.
pub trait TorrentService: Send + Sync {
    /// Validate or resolve metadata without creating output files.
    fn inspect_torrent(&self, input: Input) -> Reply<'_, Metadata>;
    /// Add a durable unified job; metadata resolution runs in the queue owner.
    fn add_torrent(&self, input: Input) -> Reply<'_, String>;
    /// Read bounded protocol diagnostics.
    fn torrent(&self, id: String) -> Reply<'_, Snapshot>;
}
