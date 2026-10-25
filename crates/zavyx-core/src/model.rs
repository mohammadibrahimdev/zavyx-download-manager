//! Core data model for downloads, segments, and queue state.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

/// Unique download identifier.
pub type DownloadId = Uuid;

/// Lifecycle state of a single download.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DownloadState {
    /// Created but not yet queued for download.
    #[default]
    Pending,
    /// In the queue, waiting for a worker slot.
    Queued,
    /// Headers fetched, segment plan computed.
    Preparing,
    /// Actively downloading one or more segments.
    Downloading,
    /// All segments done, being merged.
    Merging,
    /// User paused the download.
    Paused,
    /// Server doesn't support ranges — single-stream fallback.
    SingleStream,
    /// Finished successfully.
    Completed,
    /// Transient error, will be retried.
    Error,
    /// Permanent failure (e.g. 404, 403).
    Failed,
    /// User cancelled.
    Cancelled,
}

impl DownloadState {
    pub fn is_terminal(self) -> bool {
        matches!(self, DownloadState::Completed | DownloadState::Failed | DownloadState::Cancelled)
    }
    pub fn is_active(self) -> bool {
        matches!(self, DownloadState::Preparing | DownloadState::Downloading | DownloadState::Merging | DownloadState::SingleStream)
    }
}

/// A single byte-range slice of a download.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    /// Zero-based index of this segment in the parent download.
    pub index: usize,
    /// Inclusive start byte offset.
    pub start: u64,
    /// Exclusive end byte offset.
    pub end: u64,
    /// Bytes written so far for this segment.
    pub written: u64,
    /// Number of consecutive failures; used for retry/backoff.
    pub retries: u32,
}

impl Segment {
    pub fn len(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }
    pub fn remaining(&self) -> u64 {
        self.len().saturating_sub(self.written)
    }
    pub fn is_complete(&self) -> bool {
        self.written >= self.len()
    }
}

/// Where a download came from — drives different code paths.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DownloadOrigin {
    /// Plain direct HTTP(S) URL.
    Direct { url: String },
    /// Discovered by the browser extension's media sniffer.
    Sniffed { url: String, page_url: String, mime: Option<String> },
    /// Generic `<video src=...>` from the content script.
    HtmlVideo { url: String, page_url: String },
    /// HLS playlist (.m3u8). Will be expanded into sub-segments at run time.
    Hls { url: String, page_url: String },
    /// DASH manifest (.mpd).
    Dash { url: String, page_url: String },
    /// Handed off to yt-dlp (YouTube, social, streaming).
    YtDlp { url: String, format: Option<String> },
}

impl Default for DownloadOrigin {
    fn default() -> Self {
        DownloadOrigin::Direct { url: String::new() }
    }
}

impl DownloadOrigin {
    pub fn url(&self) -> &str {
        match self {
            DownloadOrigin::Direct { url } => url,
            DownloadOrigin::Sniffed { url, .. } => url,
            DownloadOrigin::HtmlVideo { url, .. } => url,
            DownloadOrigin::Hls { url, .. } => url,
            DownloadOrigin::Dash { url, .. } => url,
            DownloadOrigin::YtDlp { url, .. } => url,
        }
    }
}

/// A unit of work for the download engine.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DownloadJob {
    pub id: DownloadId,
    pub origin: DownloadOrigin,
    /// Final on-disk destination for the merged file.
    pub destination: PathBuf,
    /// Suggested filename (used if `destination` is a directory).
    pub filename: Option<String>,
    /// Optional referer to send with HTTP requests.
    pub referer: Option<String>,
    /// Optional user-agent override.
    pub user_agent: Option<String>,
    /// Optional extra headers (one per line, "Name: value").
    pub extra_headers: Option<String>,
    /// Timestamps.
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    /// Total content length, if known. None = unknown (chunked/streaming).
    pub total_size: Option<u64>,
    /// How many bytes have been durably written across all segments.
    pub written: u64,
    /// Number of segments the file is split into. 1 = single-stream.
    pub segment_count: usize,
    /// Per-segment progress.
    pub segments: Vec<Segment>,
    /// Current lifecycle state.
    pub state: DownloadState,
    /// Last error message, if any.
    pub last_error: Option<String>,
    /// Optional scheduled start time (for the scheduler).
    pub scheduled_for: Option<DateTime<Utc>>,
    /// Post-download action: convert via ffmpeg.
    pub convert_to: Option<ConvertProfile>,
    /// Optional sequence pattern (for batch downloads like `file_{01..20}.zip`).
    pub sequence: Option<SequenceSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConvertProfile {
    /// e.g. "mp3", "mp4", "mkv", "webm".
    pub container: String,
    /// Optional ffmpeg stream specifiers (e.g. "aac", "libx264").
    pub audio_codec: Option<String>,
    pub video_codec: Option<String>,
}

/// A batch-sequence specification, expanded by the engine at queue time.
///
/// Examples:
///   { template: "https://example.com/file_{i:02}.zip", start: 1, end: 20, step: 1 }
///   { template: "https://example.com/{a,b,c}.zip", list: ["a","b","c"] }
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SequenceSpec {
    Range {
        template: String,
        start: u64,
        end: u64,
        step: u64,
    },
    List {
        template: String,
        items: Vec<String>,
    },
}

impl SequenceSpec {
    /// Expand the sequence into a list of concrete URLs.
    pub fn expand(&self) -> Vec<String> {
        match self {
            SequenceSpec::Range { template, start, end, step } => {
                let mut out = Vec::new();
                let mut i = *start;
                while i <= *end {
                    let s = template.replace("{i}", &i.to_string());
                    let s = regex_replace_padded(&s, i);
                    out.push(s);
                    i = i.saturating_add(*step);
                }
                out
            }
            SequenceSpec::List { template, items } => {
                items.iter().map(|item| template.replace("{}", item)).collect()
            }
        }
    }
}

/// Replace `{i:0N}` with zero-padded number, e.g. template `file_{i:02}.zip`, n=3 → `file_03.zip`.
fn regex_replace_padded(template: &str, n: u64) -> String {
    let re = regex::Regex::new(r"\{i:(\d+)\}").unwrap();
    re.replace_all(template, |caps: &regex::Captures| {
        let width: usize = caps[1].parse().unwrap_or(0);
        format!("{:0width$}", n, width = width)
    })
    .into_owned()
}

impl DownloadJob {
    /// Percentage complete, or None if total size is unknown.
    pub fn percent(&self) -> Option<f64> {
        self.total_size
            .filter(|t| *t > 0)
            .map(|t| (self.written as f64 / t as f64) * 100.0)
    }

    /// Compute average speed in bytes/sec since start.
    pub fn avg_speed(&self) -> Option<f64> {
        let started = self.started_at?;
        let elapsed = Utc::now()
            .signed_duration_since(started)
            .to_std()
            .ok()?
            .as_secs_f64();
        if elapsed < 0.1 {
            return None;
        }
        Some(self.written as f64 / elapsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_sequence_expands() {
        let s = SequenceSpec::Range {
            template: "https://x/{i:02}.zip".to_string(),
            start: 1,
            end: 3,
            step: 1,
        };
        assert_eq!(
            s.expand(),
            vec!["https://x/01.zip", "https://x/02.zip", "https://x/03.zip"]
        );
    }

    #[test]
    fn list_sequence_expands() {
        let s = SequenceSpec::List {
            template: "https://x/{}.zip".to_string(),
            items: vec!["a".into(), "b".into(), "c".into()],
        };
        assert_eq!(
            s.expand(),
            vec!["https://x/a.zip", "https://x/b.zip", "https://x/c.zip"]
        );
    }
}
