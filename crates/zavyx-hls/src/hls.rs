//! HLS m3u8 playlist parser. Handles:
//!   - Master playlists (variant selection by bandwidth)
//!   - Media playlists (segments)
//!   - EXT-X-BYTERANGE
//!   - EXT-X-KEY (encryption metadata)
//!   - EXTINF durations
//!   - Relative URL resolution

use std::time::Duration;
use url::Url;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct HlsSegment {
    /// URI of the segment (absolute after resolution).
    pub uri: String,
    /// Byte range (offset, length) if EXT-X-BYTERANGE present.
    pub byte_range: Option<(u64, u64)>,
    /// Duration in seconds.
    pub duration: f64,
    /// Sequence number from EXT-X-MEDIA-SEQUENCE.
    pub sequence: u64,
    /// Title hint from EXTINF (rarely present).
    pub title: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct HlsPlaylist {
    pub version: Option<u32>,
    pub is_master: bool,
    pub variants: Vec<HlsVariant>,
    pub segments: Vec<HlsSegment>,
    pub media_sequence: u64,
    pub is_endlist: bool,
    pub target_duration: Option<Duration>,
    pub keys: Vec<HlsKey>,
    /// Base URL used for relative resolution.
    pub base_url: Option<Url>,
}

#[derive(Debug, Clone, Default)]
pub struct HlsVariant {
    pub uri: String,
    pub bandwidth: u64,
    pub resolution: Option<String>,
    pub codecs: Option<String>,
    pub frame_rate: Option<f64>,
    pub average_bandwidth: Option<u64>,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct HlsKey {
    pub method: String,
    pub uri: Option<String>,
    pub iv: Option<String>,
    pub key_format: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum HlsError {
    #[error("malformed playlist: {0}")]
    Malformed(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("URL error: {0}")]
    Url(#[from] url::ParseError),
}

pub type HlsResult<T> = Result<T, HlsError>;

/// Parse a playlist from text. `base_url` is the URL the playlist was fetched from,
/// used to resolve relative segment URIs.
pub fn parse_playlist(text: &str, base_url: Option<&str>) -> HlsResult<HlsPlaylist> {
    let mut pl = HlsPlaylist::default();
    if let Some(b) = base_url {
        pl.base_url = Some(Url::parse(b)?);
    }

    let mut lines = text.lines().peekable();
    let header = lines.next().ok_or_else(|| HlsError::Malformed("empty playlist".into()))?;
    let header = header.trim();
    if header != "#EXTM3U" {
        return Err(HlsError::Malformed(format!(
            "missing #EXTM3U header, got: {:?}",
            header
        )));
    }

    let mut current_byte_offset: Option<(u64, u64)> = None;
    let mut last_key: HlsKey = HlsKey::default();

    while let Some(line) = lines.next() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('#') {
            // Tag.
            if line.starts_with("#EXT-X-VERSION:") {
                pl.version = line[15..].trim().parse().ok();
            } else if line.starts_with("#EXT-X-TARGETDURATION:") {
                let v: f64 = line[22..].trim().parse().unwrap_or(0.0);
                pl.target_duration = Some(Duration::from_secs_f64(v));
            } else if line.starts_with("#EXT-X-MEDIA-SEQUENCE:") {
                pl.media_sequence = line[21..].trim().parse().unwrap_or(0);
            } else if line.starts_with("#EXTINF:") {
                // #EXTINF:<duration>,<title>
                let rest = &line[8..];
                let (dur_str, title) = match rest.find(',') {
                    Some(idx) => (rest[..idx].trim(), Some(rest[idx + 1..].trim().to_string())),
                    None => (rest.trim(), None),
                };
                let dur: f64 = dur_str.parse().unwrap_or(0.0);

                // The next non-comment line may be an #EXT-X-BYTERANGE tag (with optional
                // repetition), then the segment URI. Skip any tags in between.
                let mut byte_range = current_byte_offset.take();
                let seg_line = loop {
                    let Some(next) = lines.next() else {
                        return Err(HlsError::Malformed("EXTINF without segment URI".into()));
                    };
                    let next = next.trim();
                    if next.is_empty() {
                        continue;
                    }
                    if next.starts_with("#EXT-X-BYTERANGE:") {
                        let spec = &next[17..].trim();
                        let mut parts = spec.splitn(2, '@');
                        let len: u64 = parts.next().unwrap_or("0").trim().parse().unwrap_or(0);
                        let off: Option<u64> = parts.next().and_then(|s| s.trim().parse().ok());
                        byte_range = Some((off.unwrap_or(0), len));
                        continue;
                    }
                    if next.starts_with('#') {
                        // Unknown tag between EXTINF and URI; ignore.
                        continue;
                    }
                    break next.to_string();
                };
                let seq = pl.media_sequence + pl.segments.len() as u64;
                let resolved = resolve_url(&seg_line, pl.base_url.as_ref());
                pl.segments.push(HlsSegment {
                    uri: resolved,
                    byte_range,
                    duration: dur,
                    sequence: seq,
                    title,
                });
            } else if line.starts_with("#EXT-X-BYTERANGE:") {
                let spec = &line[17..].trim();
                let mut parts = spec.splitn(2, '@');
                let len: u64 = parts.next().unwrap_or("0").trim().parse().unwrap_or(0);
                let off: Option<u64> = parts.next().and_then(|s| s.trim().parse().ok());
                current_byte_offset = Some((off.unwrap_or(0), len));
            } else if line.starts_with("#EXT-X-STREAM-INF:") {
                // Master playlist variant.
                pl.is_master = true;
                let attrs = &line[18..];
                let mut v = HlsVariant::default();
                for kv in attrs.split(',') {
                    let mut it = kv.splitn(2, '=');
                    let key = it.next().unwrap_or("").trim();
                    let value = it.next().unwrap_or("").trim().trim_matches('"');
                    match key {
                        "BANDWIDTH" => v.bandwidth = value.parse().unwrap_or(0),
                        "AVERAGE-BANDWIDTH" => v.average_bandwidth = value.parse().ok(),
                        "RESOLUTION" => v.resolution = Some(value.to_string()),
                        "CODECS" => v.codecs = Some(value.to_string()),
                        "FRAME-RATE" => v.frame_rate = value.parse().ok(),
                        "NAME" => v.name = Some(value.to_string()),
                        _ => {}
                    }
                }
                // Next line is the variant URI.
                let Some(vline) = lines.next() else {
                    return Err(HlsError::Malformed("STREAM-INF without URI".into()));
                };
                let vline = vline.trim();
                if vline.is_empty() || vline.starts_with('#') {
                    return Err(HlsError::Malformed("STREAM-INF without URI".into()));
                }
                v.uri = resolve_url(vline, pl.base_url.as_ref());
                pl.variants.push(v);
            } else if line.starts_with("#EXT-X-KEY:") {
                let attrs = &line[11..];
                let mut k = HlsKey::default();
                for kv in split_attrs(attrs) {
                    let mut it = kv.splitn(2, '=');
                    let key = it.next().unwrap_or("").trim();
                    let value = it.next().unwrap_or("").trim().trim_matches('"');
                    match key {
                        "METHOD" => k.method = value.to_string(),
                        "URI" => k.uri = Some(resolve_url(value, pl.base_url.as_ref())),
                        "IV" => k.iv = Some(value.to_string()),
                        "KEYFORMAT" => k.key_format = Some(value.to_string()),
                        _ => {}
                    }
                }
                last_key = k.clone();
                pl.keys.push(k);
            } else if line == "#EXT-X-ENDLIST" {
                pl.is_endlist = true;
            } else {
                // Unknown tag, ignore.
            }
        } else {
            // Bare URI? Treat as a segment if not master.
            if !pl.is_master {
                let seq = pl.media_sequence + pl.segments.len() as u64;
                let resolved = resolve_url(line, pl.base_url.as_ref());
                pl.segments.push(HlsSegment {
                    uri: resolved,
                    byte_range: current_byte_offset.take(),
                    duration: 0.0,
                    sequence: seq,
                    title: None,
                });
            }
        }
    }

    Ok(pl)
}

/// Split HLS attribute string, respecting quoted values that may contain commas.
fn split_attrs(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut in_quote = false;
    for c in s.chars() {
        match c {
            '"' => {
                in_quote = !in_quote;
                buf.push(c);
            }
            ',' if !in_quote => {
                let trimmed = buf.trim().to_string();
                if !trimmed.is_empty() {
                    out.push(trimmed);
                }
                buf.clear();
            }
            _ => buf.push(c),
        }
    }
    let trimmed = buf.trim().to_string();
    if !trimmed.is_empty() {
        out.push(trimmed);
    }
    out
}

/// Resolve a possibly-relative URI against the playlist's base URL.
fn resolve_url(uri: &str, base: Option<&Url>) -> String {
    if let Some(b) = base {
        if let Ok(joined) = b.join(uri) {
            return joined.to_string();
        }
    }
    uri.to_string()
}

impl HlsPlaylist {
    /// Pick the highest-bandwidth variant from a master playlist.
    pub fn best_variant(&self) -> Option<&HlsVariant> {
        self.variants.iter().max_by_key(|v| v.bandwidth)
    }

    /// Total duration of all segments in seconds.
    pub fn total_duration(&self) -> f64 {
        self.segments.iter().map(|s| s.duration).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_media_playlist() {
        let text = r#"#EXTM3U
#EXT-X-VERSION:3
#EXT-X-TARGETDURATION:10
#EXT-X-MEDIA-SEQUENCE:0
#EXTINF:9.00,
seg-0.ts
#EXTINF:9.00,
seg-1.ts
#EXTINF:9.00,
seg-2.ts
#EXT-X-ENDLIST
"#;
        let pl = parse_playlist(text, Some("https://example.com/playlist.m3u8")).unwrap();
        assert!(!pl.is_master);
        assert_eq!(pl.segments.len(), 3);
        assert_eq!(pl.segments[0].uri, "https://example.com/seg-0.ts");
        assert!((pl.segments[0].duration - 9.0).abs() < 1e-3);
        assert!(pl.is_endlist);
        assert!((pl.total_duration() - 27.0).abs() < 1e-3);
    }

    #[test]
    fn parse_master_playlist() {
        let text = r#"#EXTM3U
#EXT-X-STREAM-INF:BANDWIDTH=1000000,RESOLUTION=640x360,CODECS="avc1.42c01e,mp4a.40.2"
low.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=3000000,RESOLUTION=1280x720,CODECS="avc1.640028,mp4a.40.2"
high.m3u8
"#;
        let pl = parse_playlist(text, Some("https://example.com/master.m3u8")).unwrap();
        assert!(pl.is_master);
        assert_eq!(pl.variants.len(), 2);
        assert_eq!(pl.best_variant().unwrap().bandwidth, 3000000);
        assert_eq!(pl.best_variant().unwrap().uri, "https://example.com/high.m3u8");
    }

    #[test]
    fn parse_byterange() {
        let text = r#"#EXTM3U
#EXT-X-TARGETDURATION:10
#EXT-X-MEDIA-SEQUENCE:0
#EXTINF:9.00,
#EXT-X-BYTERANGE:100@0
chunk.ts
#EXTINF:9.00,
#EXT-X-BYTERANGE:100@100
chunk.ts
"#;
        let pl = parse_playlist(text, Some("https://example.com/playlist.m3u8")).unwrap();
        assert_eq!(pl.segments.len(), 2);
        assert_eq!(pl.segments[0].byte_range, Some((0, 100)));
        assert_eq!(pl.segments[1].byte_range, Some((100, 100)));
    }
}
