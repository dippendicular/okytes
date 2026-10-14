//! Explicit supported HLS/DASH subset. Unsupported timing/layouts fail closed.
use super::*;
fn join(base: &Url, uri: &str) -> Result<Url, TransportError> {
    if uri.len() > 8192 {
        return Err(error(base, "resource URI exceeds 8192 bytes"));
    }
    let url = base.join(uri).map_err(|e| error(base, e))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.as_str().len() > 8192
        || !url.username().is_empty()
        || url.password().is_some()
        || (base.scheme() == "https" && url.scheme() != "https")
    {
        return Err(error(
            base,
            "unsupported resource URL, credentials or HTTPS downgrade",
        ));
    }
    Ok(url)
}
fn part(
    base: &Url,
    uri: &str,
    range: Option<(u64, u64)>,
    format: Format,
) -> Result<Part, TransportError> {
    Ok(Part {
        url: join(base, uri)?,
        range,
        identity: RemoteIdentity::empty(),
        length: 0,
        offset: 0,
        format,
        key: None,
        prepared: None,
    })
}
fn hls(bytes: &[u8], url: &Url) -> Result<m3u8_rs::Playlist, TransportError> {
    let text = std::str::from_utf8(bytes).map_err(|e| error(url, e))?;
    if !text.trim_start_matches('\u{feff}').starts_with("#EXTM3U") {
        return Err(error(url, "HLS EXTM3U header required"));
    }
    let mut segments = 0usize;
    let mut variants = 0usize;
    let mut tracks = 0usize;
    for line in text.lines() {
        if line.len() > 8192 {
            return Err(error(url, "HLS line exceeds 8192 bytes"));
        }
        if line.starts_with("#EXTINF:") {
            segments += 1;
        }
        if line.starts_with("#EXT-X-STREAM-INF:") {
            variants += 1;
        }
        if line.starts_with("#EXT-X-MEDIA:") {
            tracks += 1;
        }
        if segments > 1024 || variants > 128 || tracks > 128 {
            return Err(error(url, "HLS segment or selection count exceeds bounds"));
        }
        if line.starts_with("#EXT-X-KEY:") || line.starts_with("#EXT-X-SESSION-KEY:") {
            if !line.contains("METHOD=NONE") && !line.contains("METHOD=AES-128") {
                return Err(error(
                    url,
                    "DRM/SAMPLE-AES and unsupported encryption are refused",
                ));
            }
            if line.contains("KEYFORMAT=") && !line.contains("KEYFORMAT=\"identity\"") {
                return Err(error(
                    url,
                    "DRM key formats (including FairPlay) are refused",
                ));
            }
        }
        if [
            "#EXT-X-GAP",
            "#EXT-X-SKIP",
            "#EXT-X-PART:",
            "#EXT-X-PRELOAD-HINT",
        ]
        .iter()
        .any(|tag| line.starts_with(tag))
        {
            return Err(error(url, "gaps and low-latency/live HLS are unsupported"));
        }
    }
    m3u8_rs::parse_playlist_res(bytes)
        .map_err(|e| error(url, format!("invalid HLS manifest: {e:?}")))
}
fn dash(bytes: &[u8], url: &Url) -> Result<dash_mpd::MPD, TransportError> {
    let text = std::str::from_utf8(bytes).map_err(|e| error(url, e))?;
    if text.contains("ContentProtection")
        || ["widevine", "playready", "fairplay", "cenc:pssh"]
            .iter()
            .any(|s| text.to_lowercase().contains(s))
    {
        return Err(error(url, "DRM/content-protected DASH is refused"));
    }
    if text.to_lowercase().contains("<!doctype")
        || text.to_lowercase().contains("<!entity")
        || text.contains("xlink:")
    {
        return Err(error(
            url,
            "external XML entities and XLink are unsupported",
        ));
    }
    let mpd = dash_mpd::parse(text).map_err(|e| error(url, e))?;
    if mpd.mpdtype.as_deref().is_some_and(|t| t != "static") || mpd.periods.len() != 1 {
        return Err(error(
            url,
            "only static single-period DASH VOD is supported",
        ));
    }
    Ok(mpd)
}
/// Selection metadata is distinct from format support; downloads validate the
/// selected child before creation proceeds to transfer.
pub(crate) fn choices(
    kind: &str,
    source: &Url,
    base: &Url,
    bytes: &[u8],
) -> Result<Vec<MediaChoice>, TransportError> {
    let mut choices = Vec::new();
    if kind == "hls" {
        match hls(bytes, base)? {
            m3u8_rs::Playlist::MasterPlaylist(master) => {
                for (index, v) in master
                    .variants
                    .iter()
                    .enumerate()
                    .filter(|(_, v)| !v.is_i_frame)
                {
                    let url = join(base, &v.uri)?;
                    let resolution = v
                        .resolution
                        .as_ref()
                        .map(|r| format!("{}×{}", r.width, r.height))
                        .unwrap_or_else(|| "video".into());
                    choices.push(MediaChoice {
                        id: index.to_string(),
                        label: format!(
                            "{resolution} · {} bit/s · {}",
                            v.bandwidth,
                            v.codecs.as_deref().unwrap_or("unspecified codecs")
                        ),
                        url: media_url("hls", &url, None),
                        kind: "video".into(),
                        extension: "ts".into(),
                    });
                }
                for (index, a) in master.alternatives.iter().enumerate() {
                    if let Some(ref uri) = a.uri {
                        let kind = match a.media_type {
                            m3u8_rs::AlternativeMediaType::Audio => "audio",
                            m3u8_rs::AlternativeMediaType::Subtitles => "subtitles",
                            _ => continue,
                        };
                        choices.push(MediaChoice {
                            id: format!("track-{index}"),
                            label: format!(
                                "{} · {}",
                                a.name,
                                a.language.as_deref().unwrap_or("und")
                            ),
                            url: media_url("hls", &join(base, uri)?, None),
                            kind: kind.into(),
                            extension: if kind == "subtitles" { "vtt" } else { "mp4" }.into(),
                        });
                    }
                }
            }
            m3u8_rs::Playlist::MediaPlaylist(p) => {
                if !p.end_list {
                    return Err(error(
                        base,
                        "live HLS is unsupported; an ENDLIST VOD is required",
                    ));
                }
                choices.push(MediaChoice {
                    id: "default".into(),
                    label: "Original media playlist".into(),
                    url: source.to_string(),
                    kind: "video".into(),
                    extension: if p.segments.iter().any(|s| s.map.is_some()) {
                        "mp4"
                    } else if p
                        .segments
                        .first()
                        .is_some_and(|s| s.uri.split('?').next().unwrap_or("").ends_with(".vtt"))
                    {
                        "vtt"
                    } else {
                        "ts"
                    }
                    .into(),
                });
            }
        }
    } else {
        let mpd = dash(bytes, base)?;
        for (ai, a) in mpd.periods[0].adaptations.iter().enumerate() {
            for (ri, r) in a.representations.iter().enumerate() {
                let id = format!("{ai}:{ri}");
                let mime = r
                    .mimeType
                    .as_deref()
                    .or(a.mimeType.as_deref())
                    .unwrap_or("");
                let kind = if mime.starts_with("audio/") {
                    "audio"
                } else if mime == "text/vtt" {
                    "subtitles"
                } else {
                    "video"
                };
                choices.push(MediaChoice {
                    id: id.clone(),
                    label: format!(
                        "{} · {} bit/s · {} · {}",
                        r.id.as_deref().unwrap_or(&id),
                        r.bandwidth.unwrap_or(0),
                        a.lang.as_deref().unwrap_or("und"),
                        r.codecs
                            .as_deref()
                            .or(a.codecs.as_deref())
                            .unwrap_or("unspecified codecs")
                    ),
                    url: media_url("dash", base, Some(&id)),
                    kind: kind.into(),
                    extension: if kind == "subtitles" { "vtt" } else { "mp4" }.into(),
                });
            }
        }
    }
    if choices.is_empty() || choices.len() > 128 {
        return Err(error(base, "manifest must expose 1–128 selectable tracks"));
    }
    Ok(choices)
}
pub(crate) async fn resolve(
    transport: &MediaTransport,
    kind: &str,
    source: &Url,
    base: &Url,
    bytes: &[u8],
    headers: &[(String, String)],
) -> Result<(Vec<Part>, String, Vec<Bytes>), TransportError> {
    if kind == "dash" {
        let (parts, extension) = dash_parts(source, base, &dash(bytes, base)?)?;
        return Ok((parts, extension, Vec::new()));
    }
    let auth_root = physical(source)?.1;
    let mut documents = Vec::new();
    let (playlist, base) = match hls(bytes, base)? {
        m3u8_rs::Playlist::MediaPlaylist(p) => (p, base.clone()),
        m3u8_rs::Playlist::MasterPlaylist(master) => {
            if !master.session_key.is_empty() {
                return Err(error(base,"session keys are unsupported; use a media playlist with explicit authorized AES-128 keys"));
            }
            let selected = master
                .variants
                .iter()
                .filter(|v| !v.is_i_frame)
                .max_by_key(|v| v.bandwidth)
                .ok_or_else(|| error(base, "master has no media variants"))?;
            let child = join(base, &selected.uri)?;
            let (data, final_url) = transport
                .document(
                    &child,
                    headers_for(&auth_root, &child, headers),
                    1024 * 1024,
                )
                .await?;
            let p = match hls(&data, &final_url)? {
                m3u8_rs::Playlist::MediaPlaylist(p) => p,
                _ => return Err(error(base, "nested master playlists are unsupported")),
            };
            documents.push(data);
            (p, final_url)
        }
    };
    if !playlist.end_list
        || playlist.target_duration == 0
        || playlist.segments.is_empty()
        || playlist.i_frames_only
    {
        return Err(error(&base, "only complete nonempty HLS VOD is supported"));
    }
    if playlist.discontinuity_sequence != 0 || playlist.segments.iter().any(|s| s.discontinuity) {
        return Err(error(
            &base,
            "HLS discontinuities require remuxing and are unsupported",
        ));
    }
    let has_map = playlist.segments.iter().any(|s| s.map.is_some());
    let vtt = playlist
        .segments
        .iter()
        .all(|s| s.uri.split('?').next().unwrap_or("").ends_with(".vtt"));
    let format = if vtt {
        Format::Vtt
    } else if has_map {
        Format::Mp4
    } else {
        Format::Ts
    };
    let extension = if vtt {
        "vtt"
    } else if has_map {
        "mp4"
    } else {
        "ts"
    };
    let mut parts = Vec::new();
    let mut previous_range: Option<(String, u64)> = None;
    let mut previous_map: Option<(String, Option<(u64, u64)>)> = None;
    let mut keys: std::collections::HashMap<Url, [u8; 16]> = std::collections::HashMap::new();
    for (index, segment) in playlist.segments.iter().enumerate() {
        if !segment.duration.is_finite() || segment.duration <= 0.0 {
            return Err(error(&base, "invalid segment duration"));
        }
        let mut key = None;
        if let Some(ref encryption) = segment.key {
            match encryption.method {
                m3u8_rs::KeyMethod::None => {}
                m3u8_rs::KeyMethod::AES128 => {
                    if encryption
                        .keyformat
                        .as_deref()
                        .is_some_and(|f| f != "identity")
                    {
                        return Err(error(&base, "DRM key formats are refused"));
                    }
                    let key_url = join(
                        &base,
                        encryption
                            .uri
                            .as_deref()
                            .ok_or_else(|| error(&base, "AES key URI required"))?,
                    )?;
                    let value = if let Some(key) = keys.get(&key_url) {
                        *key
                    } else {
                        if keys.len() >= 128 {
                            return Err(error(&base, "key rotation exceeds 128 keys"));
                        }
                        let (bytes, _) = transport
                            .document(&key_url, headers_for(&auth_root, &key_url, headers), 16)
                            .await?;
                        let value: [u8; 16] = bytes.as_ref().try_into().map_err(|_| {
                            error(&base, "AES-128 key must contain exactly 16 bytes")
                        })?;
                        keys.insert(key_url, value);
                        value
                    };
                    let mut iv = [0u8; 16];
                    if let Some(ref explicit) = encryption.iv {
                        let hex = explicit.trim_start_matches("0x").trim_start_matches("0X");
                        if hex.is_empty()
                            || hex.len() > 32
                            || !hex.bytes().all(|b| b.is_ascii_hexdigit())
                        {
                            return Err(error(&base, "invalid AES IV"));
                        }
                        let padded = format!("{hex:0>32}");
                        for (i, slot) in iv.iter_mut().enumerate() {
                            *slot = u8::from_str_radix(&padded[i * 2..i * 2 + 2], 16)
                                .map_err(|e| error(&base, e))?;
                        }
                    } else {
                        let sequence = playlist
                            .media_sequence
                            .checked_add(index as u64)
                            .ok_or_else(|| error(&base, "media sequence overflow"))?;
                        iv[8..].copy_from_slice(&sequence.to_be_bytes());
                    }
                    key = Some((value, iv));
                }
                _ => return Err(error(&base, "DRM and unsupported encryption are refused")),
            }
        }
        if let Some(ref map) = segment.map {
            let range = map
                .byte_range
                .as_ref()
                .map(|r| {
                    let offset = r
                        .offset
                        .ok_or_else(|| error(&base, "init byte range requires explicit offset"))?;
                    range_end(&base, offset, r.length).map(|end| (offset, end))
                })
                .transpose()?;
            let descriptor = (map.uri.clone(), range);
            if previous_map.as_ref() != Some(&descriptor) {
                if previous_map.is_some() {
                    return Err(error(
                        &base,
                        "changing initialization segments require remuxing",
                    ));
                }
                if key.is_some() && segment.key.as_ref().is_some_and(|k| k.iv.is_none()) {
                    return Err(error(
                        &base,
                        "encrypted initialization requires explicit IV",
                    ));
                }
                let mut init = part(&base, &map.uri, range, Format::Mp4Init)?;
                init.key = key;
                parts.push(init);
                previous_map = Some(descriptor);
            }
        } else if has_map {
            return Err(error(
                &base,
                "every fragmented MP4 segment requires initialization",
            ));
        }
        let range = if let Some(ref r) = segment.byte_range {
            let offset = match r.offset {
                Some(n) => n,
                None => previous_range
                    .as_ref()
                    .filter(|(uri, _)| uri == &segment.uri)
                    .map(|(_, end)| *end)
                    .ok_or_else(|| {
                        error(
                            &base,
                            "implicit byte range requires the same preceding resource",
                        )
                    })?,
            };
            let end = range_end(&base, offset, r.length)?;
            previous_range = Some((segment.uri.clone(), end + 1));
            Some((offset, end))
        } else {
            previous_range = None;
            None
        };
        let mut p = part(&base, &segment.uri, range, format)?;
        p.key = key;
        parts.push(p);
        if parts.len() > 1024 {
            return Err(error(&base, "media exceeds 1024 source segments"));
        }
    }
    Ok((parts, extension.into(), documents))
}
fn range_end(url: &Url, start: u64, length: u64) -> Result<u64, TransportError> {
    if length == 0 {
        return Err(error(url, "empty byte range"));
    }
    start
        .checked_add(length - 1)
        .filter(|n| *n < u64::MAX)
        .ok_or_else(|| error(url, "byte range overflow"))
}
fn explicit_range(url: &Url, value: Option<&str>) -> Result<Option<(u64, u64)>, TransportError> {
    value
        .map(|value| {
            let (a, b) = value
                .split_once('-')
                .ok_or_else(|| error(url, "invalid DASH byte range"))?;
            let start = a.parse::<u64>().map_err(|e| error(url, e))?;
            let end = b.parse::<u64>().map_err(|e| error(url, e))?;
            if start > end || end == u64::MAX {
                return Err(error(url, "invalid DASH byte range"));
            }
            Ok((start, end))
        })
        .transpose()
}
fn base_url(parent: &Url, bases: &[dash_mpd::BaseURL]) -> Result<Url, TransportError> {
    if bases.len() > 1 {
        return Err(error(parent, "multiple CDN BaseURLs are unsupported"));
    }
    match bases.first() {
        Some(base) => join(parent, &base.base),
        None => Ok(parent.clone()),
    }
}
fn dash_parts(
    source: &Url,
    root: &Url,
    mpd: &dash_mpd::MPD,
) -> Result<(Vec<Part>, String), TransportError> {
    let period = &mpd.periods[0];
    let selected = url::form_urlencoded::parse(source.fragment().unwrap_or("").as_bytes())
        .find(|(key, _)| key == "representation")
        .map(|(_, v)| v.into_owned());
    let candidates = period
        .adaptations
        .iter()
        .enumerate()
        .flat_map(|(ai, a)| {
            a.representations
                .iter()
                .enumerate()
                .map(move |(ri, r)| (ai, ri, a, r))
        })
        .collect::<Vec<_>>();
    let (ai, ri, a, r) = candidates
        .iter()
        .copied()
        .filter(|(ai, ri, _, _)| {
            selected
                .as_ref()
                .is_none_or(|id| id == &format!("{ai}:{ri}"))
        })
        .max_by_key(|(_, _, _, r)| r.bandwidth.unwrap_or(0))
        .ok_or_else(|| error(root, "selected DASH representation not found"))?;
    let _ = (ai, ri);
    let base = base_url(
        &base_url(
            &base_url(&base_url(root, &mpd.base_url)?, &period.BaseURL)?,
            &a.BaseURL,
        )?,
        &r.BaseURL,
    )?;
    let mime = r
        .mimeType
        .as_deref()
        .or(a.mimeType.as_deref())
        .unwrap_or("");
    let subtitle = mime == "text/vtt";
    if !subtitle && !matches!(mime, "video/mp4" | "audio/mp4" | "application/mp4") {
        return Err(error(
            root,
            "only fragmented MP4 (video/audio) and text/vtt DASH are supported",
        ));
    }
    if !subtitle {
        let codecs = r.codecs.as_deref().or(a.codecs.as_deref()).unwrap_or("");
        if !codecs.split(',').all(|c| {
            ["avc1", "avc3", "hvc1", "hev1", "mp4a"]
                .iter()
                .any(|prefix| c.trim().starts_with(prefix))
        }) {
            return Err(error(
                root,
                "supported DASH codecs are AVC, HEVC and AAC in fragmented MP4",
            ));
        }
    }
    let format = if subtitle { Format::Vtt } else { Format::Mp4 };
    let mut parts = Vec::new();
    if let Some(list) = r
        .SegmentList
        .as_ref()
        .or(a.SegmentList.as_ref())
        .or(period.SegmentList.as_ref())
    {
        if list.href.is_some() {
            return Err(error(root, "external SegmentLists are unsupported"));
        }
        if let Some(ref init) = list.Initialization {
            parts.push(part(
                &base,
                init.sourceURL.as_deref().unwrap_or(""),
                explicit_range(root, init.range.as_deref())?,
                Format::Mp4Init,
            )?);
        } else if !subtitle {
            return Err(error(root, "DASH initialization required"));
        }
        for segment in &list.segment_urls {
            parts.push(part(
                &base,
                segment.media.as_deref().unwrap_or(""),
                explicit_range(root, segment.mediaRange.as_deref())?,
                format,
            )?);
            if parts.len() > 1024 {
                return Err(error(root, "segment count exceeds 1024"));
            }
        }
    } else if let Some(template) = r
        .SegmentTemplate
        .as_ref()
        .or(a.SegmentTemplate.as_ref())
        .or(period.SegmentTemplate.as_ref())
    {
        if template.timescale == Some(0) {
            return Err(error(root, "DASH timescale must be positive"));
        }
        if template.presentationTimeOffset.unwrap_or(0) != 0 {
            return Err(error(root, "presentation time offsets require remuxing"));
        }
        let id = r.id.as_deref().unwrap_or("");
        let bandwidth = r.bandwidth.unwrap_or(0);
        let mut number = template.startNumber.unwrap_or(1);
        if let Some(ref init) = template.initialization {
            parts.push(part(
                &base,
                &expand(root, init, id, bandwidth, number, 0)?,
                None,
                Format::Mp4Init,
            )?);
        } else if !subtitle {
            return Err(error(root, "DASH initialization required"));
        }
        let media = template
            .media
            .as_deref()
            .ok_or_else(|| error(root, "DASH segment media template required"))?;
        if let Some(ref timeline) = template.SegmentTimeline {
            let mut time = 0u64;
            for s in &timeline.segments {
                time = s.t.unwrap_or(time);
                let repeat = s.r.unwrap_or(0);
                if !(0..=1023).contains(&repeat) || s.d == 0 {
                    return Err(error(
                        root,
                        "unbounded/negative timeline repeats are unsupported",
                    ));
                }
                for _ in 0..=repeat {
                    parts.push(part(
                        &base,
                        &expand(root, media, id, bandwidth, number, time)?,
                        None,
                        format,
                    )?);
                    number = number
                        .checked_add(1)
                        .ok_or_else(|| error(root, "segment number overflow"))?;
                    time = time
                        .checked_add(s.d)
                        .ok_or_else(|| error(root, "segment time overflow"))?;
                    if parts.len() > 1024 {
                        return Err(error(root, "segment count exceeds 1024"));
                    }
                }
            }
        } else {
            let duration = template
                .duration
                .ok_or_else(|| error(root, "DASH segment duration or timeline required"))?;
            let scale = template.timescale.unwrap_or(1);
            let total = period
                .duration
                .or(mpd.mediaPresentationDuration)
                .ok_or_else(|| error(root, "bounded VOD duration required"))?
                .as_secs_f64();
            let count = (total * scale as f64 / duration).ceil();
            if !count.is_finite()
                || !(1.0..=1023.0).contains(&count)
                || duration <= 0.0
                || duration.fract() != 0.0
                || scale == 0
            {
                return Err(error(root, "invalid or excessive DASH duration"));
            }
            for i in 0..count as u64 {
                parts.push(part(
                    &base,
                    &expand(
                        root,
                        media,
                        id,
                        bandwidth,
                        number,
                        i.checked_mul(duration as u64)
                            .ok_or_else(|| error(root, "segment time overflow"))?,
                    )?,
                    None,
                    format,
                )?);
                number = number
                    .checked_add(1)
                    .ok_or_else(|| error(root, "segment number overflow"))?;
            }
        }
    } else if subtitle {
        parts.push(part(&base, "", None, Format::Vtt)?);
    } else {
        return Err(error(
            root,
            "SegmentBase/indexed and unsegmented DASH are unsupported",
        ));
    }
    Ok((parts, if subtitle { "vtt" } else { "mp4" }.into()))
}
fn expand(
    url: &Url,
    template: &str,
    id: &str,
    bandwidth: u64,
    number: u64,
    time: u64,
) -> Result<String, TransportError> {
    let mut result = String::new();
    let mut rest = template;
    while let Some(index) = rest.find('$') {
        result.push_str(&rest[..index]);
        rest = &rest[index + 1..];
        if rest.starts_with('$') {
            result.push('$');
            rest = &rest[1..];
            continue;
        }
        let end = rest
            .find('$')
            .ok_or_else(|| error(url, "unterminated DASH template"))?;
        let token = &rest[..end];
        rest = &rest[end + 1..];
        let (name, format) = token
            .split_once('%')
            .map(|(a, b)| (a, Some(b)))
            .unwrap_or((token, None));
        if name == "RepresentationID" {
            if format.is_some() {
                return Err(error(url, "formatted representation ID unsupported"));
            }
            result.push_str(id);
        } else {
            let value = match name {
                "Number" => number,
                "Time" => time,
                "Bandwidth" => bandwidth,
                _ => return Err(error(url, "unknown DASH template identifier")),
            };
            let width = if let Some(format) = format {
                let width = format
                    .strip_prefix('0')
                    .and_then(|s| s.strip_suffix('d'))
                    .ok_or_else(|| error(url, "only %0Nd DASH formatting supported"))?
                    .parse::<usize>()
                    .map_err(|e| error(url, e))?;
                if width > 20 {
                    return Err(error(url, "template width exceeds 20"));
                }
                width
            } else {
                0
            };
            result.push_str(&format!("{value:0width$}"));
        }
    }
    result.push_str(rest);
    if result.len() > 8192 {
        return Err(error(url, "expanded URI too long"));
    }
    Ok(result)
}
