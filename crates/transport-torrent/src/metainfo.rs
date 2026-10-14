//! Bounded bencode envelope validation, not a BitTorrent protocol engine.
//! Reject ambiguous paths before the maintained parser or storage is invoked.
use okytes_core::{
    engine::sanitize,
    error::{EngineError, EngineResult},
};
use okytes_torrent_types::{File, Metadata, Options};
use std::{collections::HashSet, ops::Range};

/// Maximum encoded metainfo accepted by files and magnet storage alike.
pub const MAX_META: usize = 512 * 1024;
/// Bounded metadata recipe used to construct private storage.
#[derive(Clone)]
pub struct Recipe {
    /// App metadata with validated names.
    pub metadata: Metadata,
    /// Original metainfo bytes, including the original hashed info dictionary.
    pub raw: Vec<u8>,
    /// File offsets in the torrent's logical payload.
    pub offsets: Vec<u64>,
    /// Exact number of payload bytes.
    pub total: u64,
}
enum Kind<'a> {
    Int(i64),
    Bytes(&'a [u8]),
    List(Vec<Value<'a>>),
    Dict(Vec<(&'a [u8], Value<'a>)>),
}
struct Value<'a> {
    kind: Kind<'a>,
    range: Range<usize>,
}
fn invalid(message: &str) -> EngineError {
    EngineError::Config(format!("torrent metadata: {message}"))
}
struct Parser<'a> {
    bytes: &'a [u8],
    at: usize,
    nodes: usize,
}
impl<'a> Parser<'a> {
    fn value(&mut self, depth: usize) -> EngineResult<Value<'a>> {
        self.nodes += 1;
        if depth > 16 || self.nodes > 65536 {
            return Err(invalid("nesting or element limit exceeded"));
        }
        let start = self.at;
        let Some(byte) = self.bytes.get(self.at).copied() else {
            return Err(invalid("truncated value"));
        };
        let kind = match byte {
            b'i' => {
                self.at += 1;
                let end = self.bytes[self.at..]
                    .iter()
                    .position(|b| *b == b'e')
                    .map(|n| self.at + n)
                    .ok_or_else(|| invalid("unterminated integer"))?;
                let text = std::str::from_utf8(&self.bytes[self.at..end])
                    .map_err(|_| invalid("invalid integer"))?;
                if text.len() > 20
                    || text.is_empty()
                    || text == "-0"
                    || (text.starts_with('0') && text.len() > 1)
                {
                    return Err(invalid("noncanonical integer"));
                }
                let number = text.parse().map_err(|_| invalid("integer out of range"))?;
                self.at = end + 1;
                Kind::Int(number)
            }
            b'l' => {
                self.at += 1;
                let mut values = vec![];
                while self.bytes.get(self.at) != Some(&b'e') {
                    values.push(self.value(depth + 1)?);
                }
                self.at += 1;
                Kind::List(values)
            }
            b'd' => {
                self.at += 1;
                let mut values = vec![];
                let mut previous: Option<&[u8]> = None;
                while self.bytes.get(self.at) != Some(&b'e') {
                    let key = self.value(depth + 1)?;
                    let Kind::Bytes(key) = key.kind else {
                        return Err(invalid("dictionary key is not bytes"));
                    };
                    if previous.is_some_and(|old| old >= key) {
                        return Err(invalid("duplicate or unsorted dictionary keys"));
                    }
                    previous = Some(key);
                    values.push((key, self.value(depth + 1)?));
                }
                self.at += 1;
                Kind::Dict(values)
            }
            b'0'..=b'9' => {
                let colon = self.bytes[self.at..]
                    .iter()
                    .position(|b| *b == b':')
                    .map(|n| self.at + n)
                    .ok_or_else(|| invalid("missing byte-string length"))?;
                let text = std::str::from_utf8(&self.bytes[self.at..colon])
                    .map_err(|_| invalid("invalid byte-string length"))?;
                if text.len() > 8 || (text.starts_with('0') && text.len() > 1) {
                    return Err(invalid("byte-string length out of bounds"));
                }
                let len: usize = text
                    .parse()
                    .map_err(|_| invalid("invalid byte-string length"))?;
                let end = (colon + 1)
                    .checked_add(len)
                    .filter(|end| *end <= self.bytes.len())
                    .ok_or_else(|| invalid("truncated byte string"))?;
                self.at = end;
                Kind::Bytes(&self.bytes[colon + 1..end])
            }
            _ => return Err(invalid("invalid bencode token")),
        };
        Ok(Value {
            kind,
            range: start..self.at,
        })
    }
}
impl<'a> Value<'a> {
    fn get(&self, key: &[u8]) -> Option<&Value<'a>> {
        if let Kind::Dict(values) = &self.kind {
            values.iter().find(|(k, _)| *k == key).map(|(_, v)| v)
        } else {
            None
        }
    }
    fn required(&self, key: &[u8]) -> EngineResult<&Value<'a>> {
        self.get(key)
            .ok_or_else(|| invalid("missing required field"))
    }
    fn bytes(&self) -> EngineResult<&'a [u8]> {
        if let Kind::Bytes(bytes) = self.kind {
            Ok(bytes)
        } else {
            Err(invalid("expected byte string"))
        }
    }
    fn integer(&self) -> EngineResult<u64> {
        if let Kind::Int(number) = self.kind {
            u64::try_from(number).map_err(|_| invalid("negative length"))
        } else {
            Err(invalid("expected integer"))
        }
    }
    fn list(&self) -> EngineResult<&[Value<'a>]> {
        if let Kind::List(values) = &self.kind {
            Ok(values)
        } else {
            Err(invalid("expected list"))
        }
    }
}
/// Safe portable path component; no renaming of hashed metadata is performed.
pub fn component(bytes: &[u8]) -> EngineResult<String> {
    let name = std::str::from_utf8(bytes).map_err(|_| invalid("non-UTF8 filename"))?;
    if name.is_empty()
        || name.len() > 200
        || !name.is_ascii()
        || name == "."
        || name == ".."
        || name.contains('~')
        || sanitize(name) != name
    {
        return Err(invalid("unsafe or unsupported filename"));
    }
    Ok(name.into())
}
/// Inspect a metainfo envelope with bounded allocation and exact original bytes.
pub fn validate(raw: &[u8]) -> EngineResult<Recipe> {
    if raw.is_empty() || raw.len() > MAX_META {
        return Err(invalid("metainfo must be 1..512 KiB"));
    }
    let mut parser = Parser {
        bytes: raw,
        at: 0,
        nodes: 0,
    };
    let top = parser.value(0)?;
    if parser.at != raw.len() {
        return Err(invalid("trailing data"));
    }
    let info = top.required(b"info")?;
    let _original_info = &raw[info.range.clone()];
    if info.get(b"meta version").is_some() || info.get(b"name.utf-8").is_some() {
        return Err(invalid(
            "v2 and alternate filename encodings are not supported",
        ));
    }
    let name = component(info.required(b"name")?.bytes()?)?;
    let piece_bytes = info.required(b"piece length")?.integer()?;
    if !(16384..=4 * 1024 * 1024).contains(&piece_bytes) {
        return Err(invalid("piece length exceeds supported bounds"));
    }
    let piece_hashes = info.required(b"pieces")?.bytes()?;
    if !piece_hashes.len().is_multiple_of(20) || piece_hashes.len() / 20 > 16384 {
        return Err(invalid("piece count or SHA-1 hash array is invalid"));
    }
    let mut files = vec![];
    let mut offsets = vec![];
    let mut total = 0u64;
    let mut paths = HashSet::new();
    let entries: Vec<_> = if let Some(list) = info.get(b"files") {
        let entries = list.list()?;
        if entries.is_empty() || entries.len() > 64 {
            return Err(invalid("file count exceeds 64"));
        }
        entries.iter().collect()
    } else {
        vec![info]
    };
    for (index, file) in entries.iter().enumerate() {
        if file.get(b"path.utf-8").is_some()
            || file.get(b"symlink path").is_some()
            || file.get(b"attr").is_some()
        {
            return Err(invalid("alternate and symlink paths are forbidden"));
        }
        let path = if info.get(b"files").is_some() {
            let components = file.required(b"path")?.list()?;
            if components.is_empty() || components.len() > 16 {
                return Err(invalid("path depth exceeds 16"));
            }
            components
                .iter()
                .map(|v| v.bytes().and_then(component))
                .collect::<EngineResult<Vec<_>>>()?
                .join("/")
        } else {
            name.clone()
        };
        let key = path.to_ascii_lowercase();
        if path.len() > 2048 || !paths.insert(key) {
            return Err(invalid("duplicate or oversized path"));
        }
        let bytes = file.required(b"length")?.integer()?;
        offsets.push(total);
        total = total
            .checked_add(bytes)
            .filter(|n| *n <= 64 * 1024 * 1024 * 1024)
            .ok_or_else(|| invalid("payload exceeds 64 GiB"))?;
        files.push(File {
            index: index as u32,
            path,
            bytes,
            selected: true,
        });
    }
    for a in &paths {
        for b in &paths {
            if a != b && b.starts_with(&format!("{a}/")) {
                return Err(invalid("file/directory path collision"));
            }
        }
    }
    if total == 0 || total.div_ceil(piece_bytes) != (piece_hashes.len() / 20) as u64 {
        return Err(invalid("piece hashes do not cover the payload"));
    }
    let mut trackers = vec![];
    if let Some(announce) = top.get(b"announce") {
        if !announce.bytes()?.is_empty() {
            trackers.push(tracker(announce.bytes()?)?);
        }
    }
    if let Some(tiers) = top.get(b"announce-list") {
        for tier in tiers.list()? {
            for announce in tier.list()? {
                let url = tracker(announce.bytes()?)?;
                if !trackers.contains(&url) {
                    trackers.push(url);
                }
            }
        }
    }
    if trackers.len() > 16 {
        return Err(invalid("tracker count exceeds 16"));
    }
    let parsed = librqbit::torrent_from_bytes(raw)
        .map_err(|_| invalid("maintained parser rejected metainfo"))?;
    Ok(Recipe {
        metadata: Metadata {
            info_hash: parsed.info_hash.as_string(),
            name,
            files,
            pieces: (piece_hashes.len() / 20) as u32,
            piece_bytes: piece_bytes as u32,
            trackers,
        },
        raw: raw.into(),
        offsets,
        total,
    })
}
fn tracker(bytes: &[u8]) -> EngineResult<String> {
    let value = std::str::from_utf8(bytes).map_err(|_| invalid("invalid tracker URL"))?;
    let url = url::Url::parse(value).map_err(|_| invalid("invalid tracker URL"))?;
    if value.len() > 2048
        || !matches!(url.scheme(), "http" | "https" | "udp")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(invalid("unsafe tracker URL"));
    }
    Ok(url.to_string())
}
/// Validate resource limits and privacy choices before any network operation.
pub fn options(options: &Options) -> EngineResult<()> {
    if !(1..=32).contains(&options.peers)
        || options.initial_peers.len() > 32
        || options.files.as_ref().is_some_and(|files| {
            files.is_empty()
                || files.len() > 64
                || files.iter().collect::<HashSet<_>>().len() != files.len()
        })
        || [options.download_bps, options.upload_bps]
            .iter()
            .any(|rate| rate.is_some_and(|n| n == 0 || n > 1 << 40))
        || options
            .seed_ratio
            .is_some_and(|r| !r.is_finite() || !(0.0..=100.0).contains(&r))
        || options.seed_seconds.is_some_and(|s| s > 365 * 86400)
    {
        return Err(invalid("invalid torrent resource or seeding options"));
    }
    let p = &options.privacy;
    if p.peer_proxy
        .as_ref()
        .is_some_and(|value| value.len() > 2048)
    {
        return Err(invalid("proxy URL exceeds 2048 bytes"));
    }
    if p.encryption != "plaintext" {
        return Err(invalid("peer encryption is not supported"));
    }
    if p.listen.parse::<std::net::SocketAddr>().is_err()
        || p.bootstrap_nodes
            .as_ref()
            .is_some_and(|nodes| nodes.len() > 32 || nodes.iter().any(|n| n.len() > 200))
    {
        return Err(invalid("invalid listen/bootstrap configuration"));
    }
    if p.bind_device.as_ref().is_some_and(|name| {
        name.is_empty()
            || name.len() > 64
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-.:".contains(&b))
    }) {
        return Err(invalid("invalid network device name"));
    }
    if cfg!(windows) && p.bind_device.is_some() {
        return Err(invalid("device binding is supported on Linux/macOS only"));
    }
    if let Some(proxy) = &p.peer_proxy {
        let parsed = url::Url::parse(proxy).map_err(|_| invalid("invalid SOCKS proxy"))?;
        if parsed.scheme() != "socks5"
            || parsed.host().is_none()
            || parsed.port().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.path() != "" && parsed.path() != "/"
            || parsed.query().is_some()
        {
            return Err(invalid(
                "SOCKS proxy must not contain credentials or query parameters",
            ));
        }
    }
    for peer in &options.initial_peers {
        peer.parse::<std::net::SocketAddr>()
            .map_err(|_| invalid("invalid initial peer"))?;
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unsafe_names_and_oversized_envelopes_fail_before_parse() {
        for name in [
            "..",
            "/root",
            "C:\\file",
            "CON",
            "file/other",
            "file\\other",
            "x.",
            "x ",
        ] {
            assert!(component(name.as_bytes()).is_err(), "{name}");
        }
        assert!(validate(&vec![b'x'; MAX_META + 1]).is_err());
    }
    proptest::proptest! {#[test] fn arbitrary_envelopes_never_panic(bytes in proptest::collection::vec(proptest::num::u8::ANY,0..4096)){let _=validate(&bytes);}}
}
