//! Multi-segment HTTP downloader with resume support.
//!
//! Algorithm:
//!   1. HEAD or initial GET to learn Content-Length and Accept-Ranges.
//!   2. If server supports ranges AND content length is known, split into N segments.
//!   3. Each segment writes to a `.part-N` file alongside the destination.
//!   4. On resume, segment.written is read from existing part files.
//!   5. Once all segments complete, parts are concatenated into the destination file.
//!
//! For servers without range support, falls back to single-stream sequential download.

use crate::errors::{DownloadError, DownloadResult};
use crate::model::{DownloadOrigin, DownloadState, Segment};
use futures_util::StreamExt;
use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;

/// Snapshot of progress for the running download.
#[derive(Debug, Clone, Default)]
pub struct ProgressSnapshot {
    pub total: Option<u64>,
    pub written: u64,
    pub segment_count: usize,
    pub segment_progress: Vec<u64>,
    pub state: DownloadState,
    pub speed_bps: Option<f64>,
}

/// Persistent handle on disk for partial segment files.
pub struct SegmentWriter {
    pub path: PathBuf,
    pub file: Option<tokio::fs::File>,
}

impl SegmentWriter {
    pub async fn open(path: PathBuf, append: bool) -> std::io::Result<Self> {
        let file = if append {
            let f = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .await?;
            f
        } else {
            tokio::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&path)
                .await?
        };
        Ok(Self { path, file: Some(file) })
    }

    pub async fn write_chunk(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        if let Some(f) = self.file.as_mut() {
            f.write_all(bytes).await?;
        }
        Ok(())
    }

    pub async fn close(&mut self) -> std::io::Result<()> {
        if let Some(mut f) = self.file.take() {
            f.flush().await?;
            f.sync_all().await?;
        }
        Ok(())
    }
}

/// Compute segment boundaries for a file of total size `total`, split into `n` segments.
pub fn plan_segments(total: u64, n: usize) -> Vec<Segment> {
    let n = n.max(1);
    let chunk = total / n as u64;
    let mut segments = Vec::with_capacity(n);
    let mut offset = 0u64;
    for i in 0..n {
        let is_last = i == n - 1;
        let end = if is_last {
            total
        } else {
            offset + chunk
        };
        segments.push(Segment {
            index: i,
            start: offset,
            end,
            written: 0,
            retries: 0,
        });
        offset = end;
    }
    segments
}

/// Probe the remote URL to learn Content-Length and range support.
pub async fn probe(
    client: &reqwest::Client,
    origin: &DownloadOrigin,
    referer: Option<&str>,
    user_agent: Option<&str>,
) -> DownloadResult<(Option<u64>, bool)> {
    let url = origin.url();
    tracing::debug!(url, "probing remote");

    let mut req = client.get(url);
    if let Some(r) = referer {
        req = req.header("Referer", r);
    }
    if let Some(ua) = user_agent {
        req = req.header("User-Agent", ua);
    }
    // Use a Range header to probe. Many servers respond 206 with Content-Range even if
    // they don't advertise Accept-Ranges in their normal headers.
    req = req.header("Range", "bytes=0-0");
    let resp = req.send().await?;
    let status = resp.status();
    if !status.is_success() && status != reqwest::StatusCode::PARTIAL_CONTENT {
        return Err(DownloadError::Status {
            status: status.as_u16(),
            body: String::new(),
        });
    }

    let supports_ranges = resp
        .headers()
        .get("accept-ranges")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.eq_ignore_ascii_case("bytes"))
        .unwrap_or(status == reqwest::StatusCode::PARTIAL_CONTENT);

    let total = resp
        .headers()
        .get("content-range")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split('/').nth(1))
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|t| *t > 0)
        .or_else(|| {
            resp.headers()
                .get("content-length")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .filter(|t| *t > 0)
        });

    Ok((total, supports_ranges))
}

/// Download a single segment to its `.part-<index>` file. Resumes from `segment.written` if any.
pub async fn download_segment(
    client: &reqwest::Client,
    origin: &DownloadOrigin,
    segment: &Arc<Mutex<Segment>>,
    part_path: &Path,
    referer: Option<&str>,
    user_agent: Option<&str>,
    max_retries: u32,
    initial_backoff: Duration,
    cancel: tokio_util::sync::CancellationToken,
    progress: Arc<dyn Fn(usize, u64) + Send + Sync + 'static>,
) -> DownloadResult<()> {
    let (start, end, written) = {
        let s = segment.lock();
        (s.start, s.end, s.written)
    };

    let mut writer = SegmentWriter::open(part_path.to_path_buf(), written > 0).await?;
    // Seek the writer to where we left off, if any.
    if written > 0 && writer.file.is_some() {
        // tokio::fs::File is opened in append mode when resuming — no need to seek.
    }

    let url = origin.url().to_string();
    let mut range_start = start + written;
    let range_end = end.saturating_sub(1);
    if range_start > range_end {
        writer.close().await?;
        let mut s = segment.lock();
        s.written = s.len();
        return Ok(());
    }

    let mut backoff = initial_backoff;
    let mut attempts = 0u32;
    // Bytes of this segment already on disk, kept across retries.
    let mut done = written;
    loop {
        attempts += 1;
        let mut req = client.get(&url);
        if let Some(r) = referer {
            req = req.header("Referer", r);
        }
        if let Some(ua) = user_agent {
            req = req.header("User-Agent", ua);
        }
        req = req.header(
            "Range",
            format!("bytes={}-{}", range_start, range_end),
        );

        tracing::debug!(attempt = attempts, range = ?(range_start, range_end), "fetching segment");
        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) if attempts <= max_retries => {
                tracing::warn!(?e, "segment request failed, retrying after {:?}", backoff);
                tokio::time::sleep(backoff).await;
                backoff *= 2;
                continue;
            }
            Err(e) => return Err(DownloadError::Http(e)),
        };

        let status = resp.status();
        if !(status.is_success() || status == reqwest::StatusCode::PARTIAL_CONTENT) {
            let body = resp.text().await.unwrap_or_default();
            if attempts <= max_retries && status.is_server_error() {
                tracing::warn!(status = ?status, "server error, retrying after {:?}", backoff);
                tokio::time::sleep(backoff).await;
                backoff *= 2;
                continue;
            }
            return Err(DownloadError::Status {
                status: status.as_u16(),
                body,
            });
        }

        let mut stream = resp.bytes_stream();
        let mut local_written = done;
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    writer.close().await?;
                    let mut s = segment.lock();
                    s.written = local_written;
                    return Err(DownloadError::Cancelled);
                }
                chunk = stream.next() => {
                    match chunk {
                        Some(Ok(bytes)) => {
                            let n = bytes.len();
                            writer.write_chunk(&bytes).await?;
                            local_written += n as u64;
                            {
                                let mut s = segment.lock();
                                s.written = local_written;
                            }
                            progress(segment_index_from_lock(&segment), local_written);
                        }
                        Some(Err(e)) => {
                            if attempts <= max_retries {
                                // Keep the part file open: the retry appends to it.
                                tracing::warn!(?e, "stream chunk error, retrying segment");
                                tokio::time::sleep(backoff).await;
                                backoff *= 2;
                                // Continue loop will re-request from `local_written` since written was updated.
                                // Reset range_start to resume from where we are.
                                done = local_written;
                                range_start = start + local_written;
                                break;
                            }
                            writer.close().await?;
                            return Err(DownloadError::Http(e));
                        }
                        None => {
                            writer.close().await?;
                            // Segment complete.
                            let mut s = segment.lock();
                            s.written = s.len();
                            return Ok(());
                        }
                    }
                }
            }
        }
    }
}

fn segment_index_from_lock(s: &Arc<Mutex<Segment>>) -> usize {
    s.lock().index
}

/// Merge all `.part-<n>` files into the destination in order, then delete the part files.
pub async fn merge_segments(
    dest: &Path,
    part_paths: &[PathBuf],
) -> std::io::Result<()> {
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut out = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(dest)
        .await?;
    for part in part_paths {
        let f = tokio::fs::File::open(part).await?;
        let mut reader = ReaderStream::new(f);
        while let Some(chunk) = reader.next().await {
            let chunk = chunk?;
            out.write_all(&chunk).await?;
        }
        out.flush().await?;
    }
    out.sync_all().await?;
    for part in part_paths {
        let _ = tokio::fs::remove_file(part).await;
    }
    Ok(())
}

/// Helper to compute part-file path for a download destination and segment index.
pub fn part_path(dest: &Path, index: usize) -> PathBuf {
    let mut p = dest.to_path_buf();
    let stem = dest
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let ext = dest
        .extension()
        .map(|s| format!(".{}", s.to_string_lossy()))
        .unwrap_or_default();
    let new_name = format!("{}.part-{}{}", stem, index, ext);
    p.set_file_name(new_name);
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_segments_balanced() {
        let segs = plan_segments(1000, 4);
        assert_eq!(segs.len(), 4);
        assert_eq!(segs[0].start, 0);
        assert_eq!(segs[0].end, 250);
        assert_eq!(segs[3].end, 1000);
        // Total coverage check.
        let total: u64 = segs.iter().map(|s| s.len()).sum();
        assert_eq!(total, 1000);
    }

    #[test]
    fn plan_segments_odd() {
        let segs = plan_segments(1001, 4);
        let total: u64 = segs.iter().map(|s| s.len()).sum();
        assert_eq!(total, 1001);
        // Last segment absorbs the remainder.
        assert!(segs[3].len() >= segs[0].len());
    }

    #[test]
    fn part_path_format() {
        let p = part_path(&PathBuf::from("/tmp/movie.mp4"), 2);
        assert_eq!(p.file_name().unwrap(), "movie.part-2.mp4");
    }
}
