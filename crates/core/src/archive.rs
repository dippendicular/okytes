//! Remote ZIP preview. Reads only the archive tail and central directory with
//! byte-range requests, so users can inspect contents before downloading.
//! Everything is bounded; nothing is decompressed or written to disk.
use crate::{
    desktop::{ZipEntry, ZipListing},
    error::{EngineError, EngineResult},
    transport::Transport,
};
use futures::StreamExt;
use url::Url;

/// End record (22 bytes) plus the largest possible comment.
const TAIL: u64 = 22 + 65535;
/// ZIP64 locator (20 bytes) sits directly before the end record.
const LOCATOR: u64 = 20;
const MAX_DIRECTORY: u64 = 32 * 1024 * 1024;
const MAX_LISTED: usize = 5000;

fn invalid(message: &str) -> EngineError {
    EngineError::Config(format!("ZIP preview: {message}"))
}
fn u16_at(b: &[u8], at: usize) -> Option<u64> {
    b.get(at..at + 2)
        .map(|s| u64::from(u16::from_le_bytes([s[0], s[1]])))
}
fn u32_at(b: &[u8], at: usize) -> Option<u64> {
    b.get(at..at + 4)
        .map(|s| u64::from(u32::from_le_bytes([s[0], s[1], s[2], s[3]])))
}
fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    b.get(at..at + 8)
        .map(|s| u64::from_le_bytes(s.try_into().unwrap_or([0; 8])))
}

async fn read(
    transport: &dyn Transport,
    url: &Url,
    identity: &crate::types::RemoteIdentity,
    start: u64,
    end: u64,
) -> EngineResult<Vec<u8>> {
    let response = transport
        .fetch(url, &[], Some((start, end)), identity)
        .await
        .map_err(|e| invalid(&e.to_string()))?;
    let limit = (end - start + 1) as usize;
    let mut bytes = Vec::with_capacity(limit.min(1 << 20));
    let mut body = response.body;
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|e| invalid(&e.to_string()))?;
        if bytes.len() + chunk.len() > limit {
            return Err(invalid("server returned more data than requested"));
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.len() != limit {
        return Err(invalid("server returned a truncated range"));
    }
    Ok(bytes)
}

fn dos_time(date: u64, time: u64) -> Option<String> {
    let (year, month, day) = (1980 + (date >> 9), (date >> 5) & 15, date & 31);
    if date == 0 || !(1..=12).contains(&month) || day == 0 {
        return None;
    }
    Some(format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}",
        time >> 11,
        (time >> 5) & 63
    ))
}

/// Parse central-directory records. Pure function over bytes for clarity.
fn entries(directory: &[u8], expected: u64) -> EngineResult<(Vec<ZipEntry>, u64)> {
    let mut at = 0usize;
    let mut found = 0u64;
    let mut listed = Vec::new();
    while at + 46 <= directory.len() && found < expected {
        if u32_at(directory, at) != Some(0x0201_4b50) {
            return Err(invalid("central directory is corrupt"));
        }
        let field = |offset| u16_at(directory, at + offset).unwrap_or(0);
        let flags = field(8);
        let (name_len, extra_len, comment_len) =
            (field(28) as usize, field(30) as usize, field(32) as usize);
        let mut compressed = u32_at(directory, at + 20).unwrap_or(0);
        let mut size = u32_at(directory, at + 24).unwrap_or(0);
        let name_start = at + 46;
        let extra_start = name_start + name_len;
        let next = extra_start + extra_len + comment_len;
        if next > directory.len() {
            return Err(invalid("central directory is truncated"));
        }
        // ZIP64 extra field lists only the values saturated in the record.
        let mut extra = &directory[extra_start..extra_start + extra_len];
        while extra.len() >= 4 {
            let id = u16_at(extra, 0).unwrap_or(0);
            let len = u16_at(extra, 2).unwrap_or(0) as usize;
            let data = extra.get(4..4 + len).unwrap_or(&[]);
            if id == 1 {
                let mut cursor = 0;
                if size == 0xFFFF_FFFF {
                    size = u64_at(data, cursor).unwrap_or(size);
                    cursor += 8;
                }
                if compressed == 0xFFFF_FFFF {
                    compressed = u64_at(data, cursor).unwrap_or(compressed);
                }
            }
            extra = extra.get(4 + len..).unwrap_or(&[]);
        }
        if listed.len() < MAX_LISTED {
            let raw = &directory[name_start..extra_start];
            // Bit 11 marks UTF-8; legacy CP437 names are shown lossily.
            let name = String::from_utf8_lossy(raw).into_owned();
            listed.push(ZipEntry {
                directory: name.ends_with('/'),
                name,
                size: size as f64,
                compressed: compressed as f64,
                method: field(10) as u32,
                encrypted: flags & 1 == 1,
                modified: dos_time(field(14), field(12)),
            });
        }
        found += 1;
        at = next;
    }
    Ok((listed, found))
}

/// List a remote ZIP's entries using at most three bounded range requests.
pub async fn zip_listing(transport: &dyn Transport, url: &Url) -> EngineResult<ZipListing> {
    let probe = transport
        .probe(url, &[])
        .await
        .map_err(|e| invalid(&e.to_string()))?;
    let size = probe
        .content_length
        .ok_or_else(|| invalid("the server did not report the file size"))?;
    if !probe.accepts_ranges {
        return Err(invalid(
            "the server does not support range requests, so the archive cannot be inspected without downloading it",
        ));
    }
    if size < 22 {
        return Err(invalid("the file is too small to be a ZIP archive"));
    }
    let url = &probe.final_url;
    let identity = &probe.remote_identity;
    let tail_start = size.saturating_sub(TAIL + LOCATOR);
    let tail = read(transport, url, identity, tail_start, size - 1).await?;
    let end = (0..=tail.len().saturating_sub(22))
        .rev()
        .find(|&i| u32_at(&tail, i) == Some(0x0605_4b50))
        .ok_or_else(|| invalid("no ZIP end record found; this is probably not a ZIP file"))?;
    let mut total = u16_at(&tail, end + 10).unwrap_or(0);
    let mut directory_size = u32_at(&tail, end + 12).unwrap_or(0);
    let mut directory_offset = u32_at(&tail, end + 16).unwrap_or(0);
    let comment_len = u16_at(&tail, end + 20).unwrap_or(0) as usize;
    let comment = tail
        .get(end + 22..end + 22 + comment_len)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .filter(|c| !c.is_empty());
    if total == 0xFFFF || directory_size == 0xFFFF_FFFF || directory_offset == 0xFFFF_FFFF {
        let locator = end
            .checked_sub(LOCATOR as usize)
            .filter(|&i| u32_at(&tail, i) == Some(0x0706_4b50))
            .ok_or_else(|| invalid("ZIP64 locator is missing"))?;
        let record_at = u64_at(&tail, locator + 8).unwrap_or(u64::MAX);
        if record_at.saturating_add(56) > size {
            return Err(invalid("ZIP64 record is out of range"));
        }
        let record = read(transport, url, identity, record_at, record_at + 55).await?;
        if u32_at(&record, 0) != Some(0x0606_4b50) {
            return Err(invalid("ZIP64 end record is corrupt"));
        }
        total = u64_at(&record, 32).unwrap_or(0);
        directory_size = u64_at(&record, 40).unwrap_or(0);
        directory_offset = u64_at(&record, 48).unwrap_or(0);
    }
    if directory_size == 0 {
        return Ok(ZipListing {
            archive_size: size as f64,
            total_entries: 0.0,
            truncated: false,
            comment,
            entries: vec![],
        });
    }
    if directory_size > MAX_DIRECTORY || directory_offset.saturating_add(directory_size) > size {
        return Err(invalid("central directory is too large or out of range"));
    }
    // Small archives: the directory often already sits inside the tail read.
    let directory = if directory_offset >= tail_start {
        let from = (directory_offset - tail_start) as usize;
        tail[from..from + directory_size as usize].to_vec()
    } else {
        read(
            transport,
            url,
            identity,
            directory_offset,
            directory_offset + directory_size - 1,
        )
        .await?
    };
    let (entries, found) = entries(&directory, total)?;
    Ok(ZipListing {
        archive_size: size as f64,
        total_entries: found as f64,
        truncated: (found as usize) > entries.len(),
        comment,
        entries,
    })
}
