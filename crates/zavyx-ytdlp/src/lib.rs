//! yt-dlp subprocess wrapper.
//!
//! Provides:
//!   - List formats for a URL (parse JSON output)
//!   - Download a URL to a destination path
//!   - Stream progress events to a callback
//!
//! Requires yt-dlp on PATH (or set the binary path explicitly).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

#[derive(Debug, Error)]
pub enum YtDlpError {
    #[error("yt-dlp not found at: {0}")]
    NotFound(PathBuf),
    #[error("yt-dlp failed (status {0})")]
    Failed(i32),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type YtDlpResult<T> = Result<T, YtDlpError>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoFormat {
    pub format_id: String,
    pub ext: String,
    pub resolution: Option<String>,
    pub fps: Option<f64>,
    pub vcodec: Option<String>,
    pub acodec: Option<String>,
    pub filesize: Option<u64>,
    pub tbr: Option<f64>, // total bitrate
    pub format_note: Option<String>, // e.g. "1080p"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoInfo {
    pub id: String,
    pub title: String,
    pub uploader: Option<String>,
    pub webpage_url: String,
    pub duration: Option<f64>,
    pub thumbnail: Option<String>,
    pub formats: Vec<VideoFormat>,
}

/// Discover available formats for a URL by running `yt-dlp -J`.
pub async fn list_formats(ytdlp_path: &Path, url: &str) -> YtDlpResult<VideoInfo> {
    if !ytdlp_path.exists() {
        return Err(YtDlpError::NotFound(ytdlp_path.to_path_buf()));
    }
    let output = tokio::process::Command::new(ytdlp_path)
        .arg("--no-playlist")
        .arg("--no-warnings")
        .arg("-J")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await?;
    if !output.status.success() {
        return Err(YtDlpError::Failed(output.status.code().unwrap_or(-1)));
    }
    let info: VideoInfo = serde_json::from_slice(&output.stdout)?;
    Ok(info)
}

/// Download progress events.
#[derive(Debug, Clone, Serialize)]
pub enum DownloadProgress {
    Resolving,
    Downloading { downloaded: u64, total: Option<u64>, speed: Option<f64> },
    Muxing,
    Completed,
    Error(String),
}

/// Download a URL with optional format selector. Streams progress to the provided callback.
pub async fn download<F>(
    ytdlp_path: &Path,
    url: &str,
    format: Option<&str>,
    dest_dir: &Path,
    template: Option<&str>,
    mut progress: F,
) -> YtDlpResult<()>
where
    F: FnMut(DownloadProgress) + Send + 'static,
{
    if !ytdlp_path.exists() {
        return Err(YtDlpError::NotFound(ytdlp_path.to_path_buf()));
    }
    let template = template.unwrap_or("%(title).200s [%(id)s].%(ext)s");
    let dest = dest_dir.join(template);

    let mut cmd = tokio::process::Command::new(ytdlp_path);
    cmd.arg("--no-playlist")
        .arg("--newline")
        .arg("--no-color")
        .arg("--no-progress")
        .arg("-o").arg(&dest);
    if let Some(f) = format {
        cmd.arg("-f").arg(f);
    }
    cmd.arg(url);
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.kill_on_drop(true);

    let mut child = cmd.spawn()?;
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");

    // Channel for stdout lines (yt-dlp prints progress like "[download]  12.3% of ...").
    let (tx, mut rx) = mpsc::channel::<String>(64);

    // stdout task — parses lines and sends progress events via the channel.
    let stdout_task = tokio::spawn(async move {
        let mut reader = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            let _ = tx.send(line).await;
        }
    });
    // stderr task — log to tracing only.
    let stderr_task = tokio::spawn(async move {
        let mut reader = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            tracing::info!("yt-dlp: {}", line);
        }
    });

    // Drain the channel and call progress on each line.
    while let Some(line) = rx.recv().await {
        if let Some(p) = parse_progress_line(&line) {
            progress(p);
        }
    }

    let status = child.wait().await?;
    let _ = tokio::join!(stdout_task, stderr_task);
    if !status.success() {
        return Err(YtDlpError::Failed(status.code().unwrap_or(-1)));
    }
    progress(DownloadProgress::Completed);
    Ok(())
}

fn parse_progress_line(line: &str) -> Option<DownloadProgress> {
    let line = line.trim();
    if !line.starts_with("[download]") {
        return None;
    }
    // Extract percent and ETA.
    let line = &line[10..].trim_start();
    // Try to extract percentage.
    let pct = line
        .split_whitespace()
        .next()
        .and_then(|s| s.trim_end_matches('%').parse::<f64>().ok());

    if pct.is_some() {
        Some(DownloadProgress::Downloading {
            downloaded: 0,
            total: None,
            speed: None,
        })
    } else {
        None
    }
}

/// Suggest a good default format selector.
pub fn default_format_selector() -> &'static str {
    // Prefer progressive (single-file) up to 720p, else best.
    "best[height<=720][ext=mp4]/best[ext=mp4]/best"
}
