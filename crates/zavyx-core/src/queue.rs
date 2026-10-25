//! Queue + scheduler. Holds pending downloads, dispatches N concurrent workers,
//! handles batch-sequence expansion, scheduled start times, and pause/resume.

use crate::config::EngineConfig;
use crate::downloader::{
    download_segment, merge_segments, part_path, plan_segments, probe, ProgressSnapshot,
};
use crate::errors::{DownloadError, DownloadResult};
use crate::model::{
    ConvertProfile, DownloadId, DownloadJob, DownloadOrigin, DownloadState,
};
use chrono::Utc;
use parking_lot::Mutex;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

pub type ProgressCallback = Arc<dyn Fn(DownloadId, ProgressSnapshot) + Send + Sync + 'static>;

struct Inner {
    /// Pending + paused downloads awaiting dispatch, in FIFO order.
    pending: VecDeque<DownloadId>,
    /// All known downloads by ID.
    jobs: HashMap<DownloadId, DownloadJob>,
    /// Cancellation tokens for active downloads.
    cancellations: HashMap<DownloadId, CancellationToken>,
    /// IDs currently being downloaded.
    active: HashMap<DownloadId, tokio::task::JoinHandle<DownloadResult<()>>>,
}

impl Inner {
    fn new() -> Self {
        Self {
            pending: VecDeque::new(),
            jobs: HashMap::new(),
            cancellations: HashMap::new(),
            active: HashMap::new(),
        }
    }
}

pub struct Queue {
    inner: Arc<Mutex<Inner>>,
    client: reqwest::Client,
    cfg: EngineConfig,
    progress: ProgressCallback,
}

impl Queue {
    pub fn new(cfg: EngineConfig, progress: ProgressCallback) -> anyhow::Result<Self> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::ACCEPT, "*/*".parse().unwrap());
        let client = reqwest::Client::builder()
            .default_headers(headers)
            .pool_idle_timeout(Duration::from_secs(60))
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self {
            inner: Arc::new(Mutex::new(Inner::new())),
            client,
            cfg,
            progress,
        })
    }

    pub fn config(&self) -> &EngineConfig {
        &self.cfg
    }

    /// Add a job. If it has a `sequence` spec, it is expanded into N child jobs.
    pub fn add(&self, mut job: DownloadJob) -> DownloadId {
        // Callers often start from `DownloadJob::default()`; give it a real identity.
        if job.id.is_nil() {
            job.id = uuid::Uuid::new_v4();
        }
        if job.created_at == chrono::DateTime::<Utc>::default() {
            job.created_at = Utc::now();
        }
        if let Some(seq) = job.sequence.take() {
            let urls = seq.expand();
            tracing::info!(count = urls.len(), "expanded sequence download");
            let mut first_id = None;
            for url in urls {
                // Derive a filename from the URL and append it to the parent destination
                // (which is treated as a directory for batch downloads).
                let child_dest = derive_batch_dest(&job.destination, &url);
                let child = DownloadJob {
                    id: uuid::Uuid::new_v4(),
                    origin: DownloadOrigin::Direct { url },
                    destination: child_dest,
                    filename: None,
                    referer: job.referer.clone(),
                    user_agent: job.user_agent.clone(),
                    extra_headers: job.extra_headers.clone(),
                    created_at: Utc::now(),
                    started_at: None,
                    finished_at: None,
                    total_size: None,
                    written: 0,
                    segment_count: self.cfg.max_segments,
                    segments: Vec::new(),
                    state: DownloadState::Queued,
                    last_error: None,
                    scheduled_for: job.scheduled_for,
                    convert_to: job.convert_to.clone(),
                    sequence: None,
                };
                let id = child.id;
                let mut inner = self.inner.lock();
                inner.jobs.insert(id, child);
                inner.pending.push_back(id);
                if first_id.is_none() {
                    first_id = Some(id);
                }
            }
            return first_id.unwrap_or(job.id);
        }

        job.state = DownloadState::Queued;
        let id = job.id;
        let mut inner = self.inner.lock();
        inner.jobs.insert(id, job);
        inner.pending.push_back(id);
        id
    }

    pub fn pause(&self, id: DownloadId) {
        let mut inner = self.inner.lock();
        if let Some(tok) = inner.cancellations.get(&id) {
            tok.cancel();
        }
        if let Some(job) = inner.jobs.get_mut(&id) {
            job.state = DownloadState::Paused;
        }
    }

    pub fn cancel(&self, id: DownloadId) {
        let mut inner = self.inner.lock();
        if let Some(tok) = inner.cancellations.get(&id) {
            tok.cancel();
        }
        if let Some(job) = inner.jobs.get_mut(&id) {
            job.state = DownloadState::Cancelled;
        }
        inner.pending.retain(|p| *p != id);
    }

    pub fn resume(&self, id: DownloadId) {
        let mut inner = self.inner.lock();
        if let Some(job) = inner.jobs.get_mut(&id) {
            if matches!(job.state, DownloadState::Paused | DownloadState::Error | DownloadState::Failed) {
                job.state = DownloadState::Queued;
                inner.pending.push_back(id);
            }
        }
    }

    /// Drop a job from the list, stopping it first if it is running.
    pub fn remove(&self, id: DownloadId) {
        let mut inner = self.inner.lock();
        if let Some(tok) = inner.cancellations.remove(&id) {
            tok.cancel();
        }
        inner.pending.retain(|p| *p != id);
        inner.jobs.remove(&id);
    }

    /// Re-insert a job saved by a previous session. Unfinished jobs come back
    /// paused so the user decides when to resume them.
    pub fn restore(&self, mut job: DownloadJob) {
        if !job.state.is_terminal() {
            job.state = DownloadState::Paused;
        }
        self.inner.lock().jobs.insert(job.id, job);
    }

    pub fn snapshot(&self, id: DownloadId) -> Option<DownloadJob> {
        self.inner.lock().jobs.get(&id).cloned()
    }

    pub fn list(&self) -> Vec<DownloadJob> {
        self.inner.lock().jobs.values().cloned().collect()
    }

    pub fn progress(&self, id: DownloadId) -> Option<ProgressSnapshot> {
        let inner = self.inner.lock();
        let job = inner.jobs.get(&id)?;
        Some(ProgressSnapshot {
            total: job.total_size,
            written: job.written,
            segment_count: job.segment_count,
            segment_progress: job.segments.iter().map(|s| s.written).collect(),
            state: job.state,
            speed_bps: job.avg_speed(),
        })
    }

    /// Try to dispatch up to `max_concurrent` downloads. Called periodically by the engine loop.
    pub fn pump(&self) {
        let mut to_dispatch = Vec::new();
        {
            let mut inner = self.inner.lock();
            let capacity = self.cfg.max_concurrent_downloads.saturating_sub(inner.active.len());
            for _ in 0..capacity {
                let Some(id) = inner.pending.pop_front() else {
                    break;
                };
                let Some(job) = inner.jobs.get_mut(&id) else {
                    continue;
                };
                if job.state.is_terminal() {
                    continue;
                }
                if let Some(when) = job.scheduled_for {
                    if when > Utc::now() {
                        inner.pending.push_back(id);
                        continue;
                    }
                }
                job.state = DownloadState::Preparing;
                job.started_at = Some(Utc::now());
                to_dispatch.push(id);
            }
        }
        for id in to_dispatch {
            let _ = self.spawn_worker(id);
        }
    }

    fn spawn_worker(&self, id: DownloadId) -> anyhow::Result<()> {
        let client = self.client.clone();
        let cfg = self.cfg.clone();
        let progress = self.progress.clone();
        let inner = self.inner.clone();
        let cancel = CancellationToken::new();

        let job = {
            let mut inner = inner.lock();
            if !inner.jobs.contains_key(&id) {
                return Ok(());
            }
            inner.cancellations.insert(id, cancel.clone());
            inner.jobs.get(&id).cloned().unwrap()
        };

        let handle = tokio::spawn(async move {
            run_download(client, cfg, inner.clone(), progress, cancel, id, job).await
        });

        let mut inner = self.inner.lock();
        inner.active.insert(id, handle);
        Ok(())
    }

    /// Mark a download as finished (success or error). Called from within run_download.
    pub fn mark_finished(&self, id: DownloadId, result: DownloadResult<()>) {
        let mut inner = self.inner.lock();
        inner.cancellations.remove(&id);
        inner.active.remove(&id);
        if let Some(job) = inner.jobs.get_mut(&id) {
            job.finished_at = Some(Utc::now());
            match result {
                Ok(()) => {
                    job.state = DownloadState::Completed;
                }
                Err(e) => {
                    job.last_error = Some(e.to_string());
                    job.state = match e {
                        DownloadError::Cancelled => DownloadState::Cancelled,
                        _ => DownloadState::Failed,
                    };
                }
            }
        }
    }
}

/// Run a download to completion.
///
/// Origin dispatch:
///   - Direct/Sniffed/HtmlVideo → multi-segment HTTP download
///   - Hls → delegate to zavyx-hls crate (in real impl; here we shell out to ffmpeg)
///   - Dash → same
///   - YtDlp → spawn yt-dlp subprocess
async fn run_download(
    client: reqwest::Client,
    cfg: EngineConfig,
    inner: Arc<Mutex<Inner>>,
    progress: ProgressCallback,
    cancel: CancellationToken,
    id: DownloadId,
    job: DownloadJob,
) -> DownloadResult<()> {
    let result = run_download_inner(client, cfg, inner.clone(), progress, cancel, id, job).await;

    // Report back into the queue.
    {
        let mut inner = inner.lock();
        inner.cancellations.remove(&id);
        inner.active.remove(&id);
        if let Some(j) = inner.jobs.get_mut(&id) {
            match &result {
                // `pause()` stops the worker via cancellation; keep it resumable.
                Err(DownloadError::Cancelled) if j.state == DownloadState::Paused => {}
                Ok(()) => {
                    j.finished_at = Some(Utc::now());
                    j.state = DownloadState::Completed;
                }
                Err(e) => {
                    j.finished_at = Some(Utc::now());
                    j.last_error = Some(e.to_string());
                    j.state = match e {
                        DownloadError::Cancelled => DownloadState::Cancelled,
                        _ => DownloadState::Failed,
                    };
                }
            }
        }
    }
    result
}

async fn run_download_inner(
    client: reqwest::Client,
    cfg: EngineConfig,
    inner: Arc<Mutex<Inner>>,
    progress: ProgressCallback,
    cancel: CancellationToken,
    id: DownloadId,
    job: DownloadJob,
) -> DownloadResult<()> {
    // Dispatch on origin.
    match &job.origin {
        DownloadOrigin::YtDlp { url, format } => {
            let report = move |ev: YtdlpEvent| {
                let mut inner = inner.lock();
                let Some(j) = inner.jobs.get_mut(&id) else { return };
                match ev {
                    YtdlpEvent::Progress { written, total } => {
                        j.state = DownloadState::Downloading;
                        j.written = written;
                        if total.is_some() {
                            j.total_size = total;
                        }
                    }
                    YtdlpEvent::Saved(path) => {
                        j.filename = path.file_name().map(|n| n.to_string_lossy().to_string());
                        if let Ok(meta) = std::fs::metadata(&path) {
                            j.total_size = Some(meta.len());
                            j.written = meta.len();
                        }
                        j.destination = path;
                    }
                }
            };
            return run_ytdlp(&cfg, url, format.as_deref(), &job.destination, cancel, report).await;
        }
        DownloadOrigin::Hls { url, .. } | DownloadOrigin::Dash { url, .. } => {
            // For HLS/DASH we shell out to ffmpeg which is robust and well-tested.
            return run_ffmpeg_stream(&cfg, url, &job.destination, cancel).await;
        }
        _ => { /* fall through to multi-segment HTTP path */ }
    }

    // Multi-segment HTTP path.
    let (total, supports_ranges) = probe(
        &client,
        &job.origin,
        job.referer.as_deref(),
        job.user_agent.as_deref().or(Some(cfg.user_agent.as_str())),
    )
    .await?;

    // Ensure the destination's parent directory exists (segment workers will write
    // part files alongside the destination).
    if let Some(parent) = job.destination.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }

    // Update job metadata.
    {
        let mut inner = inner.lock();
        if let Some(j) = inner.jobs.get_mut(&id) {
            j.total_size = total;
            j.state = DownloadState::Downloading;
        }
    }

    let dest = job.destination.clone();
    let segments_arc = if let (Some(total), true) = (total, supports_ranges) {
        // Plan segments.
        let segs = plan_segments(total, cfg.max_segments);
        let segs: Vec<Arc<Mutex<crate::model::Segment>>> =
            segs.into_iter().map(|s| Arc::new(Mutex::new(s))).collect();
        // Check for existing part files (resume).
        for (i, s) in segs.iter().enumerate() {
            let part = part_path(&dest, i);
            if let Ok(metadata) = std::fs::metadata(&part) {
                let len = metadata.len();
                if len <= s.lock().len() {
                    s.lock().written = len;
                }
            }
        }
        segs
    } else {
        // Single-stream fallback.
        let s = crate::model::Segment {
            index: 0,
            start: 0,
            end: total.unwrap_or(0),
            written: 0,
            retries: 0,
        };
        vec![Arc::new(Mutex::new(s))]
    };

    let segment_count = segments_arc.len();
    {
        let mut inner = inner.lock();
        if let Some(j) = inner.jobs.get_mut(&id) {
            j.segment_count = segment_count;
            j.segments = segments_arc
                .iter()
                .map(|s| s.lock().clone())
                .collect();
        }
    }

    // Spawn one tokio task per segment. Each segment worker updates its own segment state
    // in the shared queue under `inner`. The UI / native-messaging layer polls the queue
    // at its own cadence — we don't push per-chunk progress events (lock contention would
    // be too high).
    let mut handles = Vec::new();
    for seg in segments_arc.iter().cloned() {
        let client = client.clone();
        let origin = job.origin.clone();
        let part = part_path(&dest, seg.lock().index);
        let referer = job.referer.clone();
        let ua = job.user_agent.clone().or(Some(cfg.user_agent.clone()));
        let max_retries = cfg.max_retries;
        let backoff = Duration::from_millis(cfg.retry_backoff_ms);
        let cancel = cancel.clone();
        let inner = inner.clone();
        let id = id;
        let seg_idx = seg.lock().index;
        // Lightweight per-chunk callback: just update the shared segment state, no events.
        let progress_cb: Arc<dyn Fn(usize, u64) + Send + Sync + 'static> = Arc::new(move |_idx, written| {
            let mut inner = inner.lock();
            let Some(j) = inner.jobs.get_mut(&id) else {
                return;
            };
            if let Some(s) = j.segments.get_mut(seg_idx) {
                s.written = written;
            }
            j.written = j.segments.iter().map(|s| s.written).sum();
        });
        let h = tokio::spawn(async move {
            download_segment(
                &client,
                &origin,
                &seg,
                &part,
                referer.as_deref(),
                ua.as_deref(),
                max_retries,
                backoff,
                cancel,
                progress_cb,
            )
            .await
        });
        handles.push(h);
    }

    // Wait for all segments. If any fails, the download is failed (partial state is preserved for resume).
    let mut first_err: Option<DownloadError> = None;
    for h in handles {
        match h.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                first_err.get_or_insert(e);
            }
            Err(e) => {
                first_err.get_or_insert(DownloadError::Other(format!("join error: {}", e)));
            }
        }
    }
    if let Some(e) = first_err {
        return Err(e);
    }

    // Merge segments into final file.
    {
        let mut inner = inner.lock();
        if let Some(j) = inner.jobs.get_mut(&id) {
            j.state = DownloadState::Merging;
        }
    }
    let part_paths: Vec<PathBuf> = (0..segment_count).map(|i| part_path(&dest, i)).collect();
    merge_segments(&dest, &part_paths).await?;

    // Optional post-download convert.
    if let Some(profile) = &job.convert_to {
        run_ffmpeg_convert(&cfg, &dest, profile).await?;
    }

    // Final state update.
    {
        let mut inner = inner.lock();
        if let Some(j) = inner.jobs.get_mut(&id) {
            j.written = j.total_size.unwrap_or(j.written);
        }
    }
    Ok(())
}

/// What yt-dlp reports while it runs.
enum YtdlpEvent {
    Progress { written: u64, total: Option<u64> },
    /// Final file path after merging/moving.
    Saved(PathBuf),
}

// Markers we ask yt-dlp to print so its output is easy to parse.
const YTDLP_PROGRESS: &str = "ZAVYX-PROGRESS ";
const YTDLP_SAVED: &str = "ZAVYX-SAVED ";

/// Spawn yt-dlp as a subprocess. `dest` is a folder (yt-dlp names the file from
/// the video title) or a full file path.
async fn run_ytdlp(
    cfg: &EngineConfig,
    url: &str,
    format: Option<&str>,
    dest: &Path,
    cancel: CancellationToken,
    report: impl Fn(YtdlpEvent) + Send + Sync + 'static,
) -> DownloadResult<()> {
    use std::process::Stdio;
    use tokio::io::AsyncBufReadExt;
    if !cfg.ytdlp_path.exists() {
        return Err(DownloadError::Config(format!(
            "yt-dlp not found at: {}",
            cfg.ytdlp_path.display()
        )));
    }
    let output = if dest.is_dir() || dest.extension().is_none() {
        dest.join("%(title).150B [%(id)s].%(ext)s")
    } else {
        dest.to_path_buf()
    };
    let mut cmd = tokio::process::Command::new(&cfg.ytdlp_path);
    cmd.arg("--no-playlist")
        .arg("--newline")
        .arg("--no-color")
        .arg("--windows-filenames")
        .arg("--progress")
        .arg("--progress-template")
        .arg(format!(
            "download:{}%(progress.downloaded_bytes)s %(progress.total_bytes)s %(progress.total_bytes_estimate)s",
            YTDLP_PROGRESS
        ))
        .arg("--print")
        .arg(format!("after_move:{}%(filepath)s", YTDLP_SAVED))
        .arg("-o")
        .arg(&output);
    if cfg.ffmpeg_path.is_file() {
        cmd.arg("--ffmpeg-location").arg(&cfg.ffmpeg_path);
    }
    if let Some(fmt) = format {
        cmd.arg("-f").arg(fmt);
    }
    cmd.arg(url);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.stdin(Stdio::null());
    cmd.kill_on_drop(true);
    #[cfg(windows)]
    {
        // Don't flash a console window when started from the GUI.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().map_err(|e| DownloadError::Other(format!("spawn yt-dlp: {}", e)))?;

    let report = Arc::new(report);
    let last_error = Arc::new(Mutex::new(None::<String>));
    let mut readers = Vec::new();
    for pipe in [
        Box::new(child.stdout.take().expect("piped")) as Box<dyn tokio::io::AsyncRead + Send + Unpin>,
        Box::new(child.stderr.take().expect("piped")),
    ] {
        let report = report.clone();
        let last_error = last_error.clone();
        readers.push(tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(pipe).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(rest) = line.strip_prefix(YTDLP_PROGRESS) {
                    let nums: Vec<Option<u64>> = rest.split(' ').map(|n| n.parse::<f64>().ok().map(|v| v as u64)).collect();
                    if let Some(Some(written)) = nums.first() {
                        let total = nums.get(1).copied().flatten().or(nums.get(2).copied().flatten());
                        report(YtdlpEvent::Progress { written: *written, total });
                    }
                } else if let Some(path) = line.strip_prefix(YTDLP_SAVED) {
                    report(YtdlpEvent::Saved(PathBuf::from(path.trim())));
                } else if line.starts_with("ERROR:") {
                    *last_error.lock() = Some(line.trim_start_matches("ERROR:").trim().to_string());
                } else {
                    tracing::debug!("yt-dlp: {}", line);
                }
            }
        }));
    }

    let status = tokio::select! {
        status = child.wait() => status.map_err(|e| DownloadError::Other(format!("yt-dlp wait: {}", e)))?,
        _ = cancel.cancelled() => {
            let _ = child.kill().await;
            return Err(DownloadError::Cancelled);
        }
    };
    for r in readers {
        let _ = r.await;
    }
    if !status.success() {
        let msg = last_error.lock().take().unwrap_or_else(|| format!("yt-dlp exited with status {}", status));
        return Err(DownloadError::Other(msg));
    }
    Ok(())
}

/// Use ffmpeg to download an HLS or DASH stream by treating the manifest URL as input.
async fn run_ffmpeg_stream(
    cfg: &EngineConfig,
    url: &str,
    dest: &Path,
    cancel: CancellationToken,
) -> DownloadResult<()> {
    use std::process::Stdio;
    if !cfg.ffmpeg_path.exists() {
        return Err(DownloadError::Config(format!(
            "ffmpeg not found at: {}",
            cfg.ffmpeg_path.display()
        )));
    }
    let mut cmd = tokio::process::Command::new(&cfg.ffmpeg_path);
    cmd.arg("-y")
        .arg("-i").arg(url)
        .arg("-c").arg("copy")
        .arg(dest);
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    cmd.kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| DownloadError::Other(format!("spawn ffmpeg: {}", e)))?;
    let status = tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            let _ = child.start_kill();
            return Err(DownloadError::Cancelled);
        }
        r = child.wait() => r.map_err(|e| DownloadError::Other(format!("ffmpeg wait: {}", e)))?,
    };
    if !status.success() {
        return Err(DownloadError::Other(format!("ffmpeg exited with status {}", status)));
    }
    Ok(())
}

/// Use ffmpeg to convert a downloaded file to a different container/codec.
async fn run_ffmpeg_convert(
    cfg: &EngineConfig,
    src: &Path,
    profile: &ConvertProfile,
) -> DownloadResult<()> {
    use std::process::Stdio;
    if !cfg.ffmpeg_path.exists() {
        return Err(DownloadError::Config(format!(
            "ffmpeg not found at: {}",
            cfg.ffmpeg_path.display()
        )));
    }
    let dest = src.with_extension(&profile.container);
    let mut cmd = tokio::process::Command::new(&cfg.ffmpeg_path);
    cmd.arg("-y").arg("-i").arg(src);
    if let Some(v) = &profile.video_codec {
        cmd.arg("-c:v").arg(v);
    } else {
        cmd.arg("-c:v").arg("copy");
    }
    if let Some(a) = &profile.audio_codec {
        cmd.arg("-c:a").arg(a);
    } else {
        cmd.arg("-c:a").arg("copy");
    }
    cmd.arg(&dest);
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    cmd.kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| DownloadError::Other(format!("spawn ffmpeg convert: {}", e)))?;
    let status = child.wait().await.map_err(|e| DownloadError::Other(format!("ffmpeg convert wait: {}", e)))?;
    if !status.success() {
        return Err(DownloadError::Other(format!("ffmpeg convert exited with status {}", status)));
    }
    Ok(())
}

// --- helpers ---

/// Derive the destination file path for a batch-expanded download.
///
/// If `parent` is an existing directory, the child file goes inside it
/// with the URL's last path segment as filename. If `parent` doesn't exist
/// (yet) we still treat it as a directory and create it lazily at download
/// time. Falls back to `download_<i>.bin` if the URL has no path segment.
fn derive_batch_dest(parent: &Path, url: &str) -> PathBuf {
    let filename = url::Url::parse(url)
        .ok()
        .and_then(|u| {
            let path = u.path();
            let seg = path.rsplit('/').next().unwrap_or("");
            if seg.is_empty() {
                None
            } else {
                Some(seg.to_string())
            }
        })
        .unwrap_or_else(|| format!("download_{}.bin", url.len() % 1000));
    parent.join(filename)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pause_then_resume_requeues() {
        let cfg = EngineConfig::default();
        let progress: ProgressCallback = Arc::new(|_, _| {});
        let q = Queue::new(cfg, progress).unwrap();
        let mut job = DownloadJob::default();
        job.destination = PathBuf::from("./test.tmp");
        let id = q.add(job);
        q.pause(id);
        let snap = q.snapshot(id).unwrap();
        assert_eq!(snap.state, DownloadState::Paused);
        q.resume(id);
        let snap = q.snapshot(id).unwrap();
        assert_eq!(snap.state, DownloadState::Queued);
    }

    #[test]
    fn cancel_marks_terminal() {
        let cfg = EngineConfig::default();
        let progress: ProgressCallback = Arc::new(|_, _| {});
        let q = Queue::new(cfg, progress).unwrap();
        let mut job = DownloadJob::default();
        job.destination = PathBuf::from("./test.tmp");
        let id = q.add(job);
        q.cancel(id);
        let snap = q.snapshot(id).unwrap();
        assert!(snap.state.is_terminal());
    }
}
