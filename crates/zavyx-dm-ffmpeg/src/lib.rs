//! ffmpeg subprocess wrapper.
//!
//! Provides:
//!   - Download an HLS/DASH stream by URL into a container (uses `-c copy`)
//!   - Convert a file to a different container/codec profile
//!   - Probe file metadata

use std::path::{Path, PathBuf};
use std::process::Stdio;
use thiserror::Error;
use tokio::io::AsyncBufReadExt;

#[derive(Debug, Error)]
pub enum FfmpegError {
    #[error("ffmpeg not found at: {0}")]
    NotFound(PathBuf),
    #[error("ffmpeg failed (status {0})")]
    Failed(i32),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

pub type FfmpegResult<T> = Result<T, FfmpegError>;

/// Download a stream by URL (HLS m3u8 or DASH mpd) directly to a file.
/// Uses `-c copy` so it's basically a mux.
pub async fn download_stream(
    ffmpeg_path: &Path,
    url: &str,
    dest: &Path,
    extra_args: &[&str],
) -> FfmpegResult<()> {
    if !ffmpeg_path.exists() {
        return Err(FfmpegError::NotFound(ffmpeg_path.to_path_buf()));
    }
    let parent = dest.parent().unwrap_or_else(|| Path::new("."));
    tokio::fs::create_dir_all(parent).await.ok();
    let mut cmd = tokio::process::Command::new(ffmpeg_path);
    cmd.arg("-y")
        .arg("-i").arg(url)
        .args(extra_args)
        .arg("-c").arg("copy")
        .arg(dest);
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    cmd.kill_on_drop(true);
    let mut child = cmd.spawn()?;
    let stderr = child.stderr.take().expect("piped");
    let mut reader = tokio::io::BufReader::new(stderr).lines();
    while let Ok(Some(line)) = reader.next_line().await {
        tracing::debug!("ffmpeg: {}", line);
    }
    let status = child.wait().await?;
    if !status.success() {
        return Err(FfmpegError::Failed(status.code().unwrap_or(-1)));
    }
    Ok(())
}

/// Convert a file to a new container/codec.
pub async fn convert(
    ffmpeg_path: &Path,
    src: &Path,
    dest_container: &str,
    video_codec: Option<&str>,
    audio_codec: Option<&str>,
    extra_args: &[&str],
) -> FfmpegResult<()> {
    if !ffmpeg_path.exists() {
        return Err(FfmpegError::NotFound(ffmpeg_path.to_path_buf()));
    }
    let dest = src.with_extension(dest_container);
    let mut cmd = tokio::process::Command::new(ffmpeg_path);
    cmd.arg("-y").arg("-i").arg(src);
    if let Some(v) = video_codec {
        cmd.arg("-c:v").arg(v);
    } else {
        cmd.arg("-c:v").arg("copy");
    }
    if let Some(a) = audio_codec {
        cmd.arg("-c:a").arg(a);
    } else {
        cmd.arg("-c:a").arg("copy");
    }
    cmd.args(extra_args).arg(&dest);
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    cmd.kill_on_drop(true);
    let mut child = cmd.spawn()?;
    let stderr = child.stderr.take().expect("piped");
    let mut reader = tokio::io::BufReader::new(stderr).lines();
    while let Ok(Some(line)) = reader.next_line().await {
        tracing::debug!("ffmpeg: {}", line);
    }
    let status = child.wait().await?;
    if !status.success() {
        return Err(FfmpegError::Failed(status.code().unwrap_or(-1)));
    }
    Ok(())
}

/// Quick probe: is this binary path a working ffmpeg?
pub async fn is_working_ffmpeg(ffmpeg_path: &Path) -> bool {
    if !ffmpeg_path.exists() {
        return false;
    }
    tokio::process::Command::new(ffmpeg_path)
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}
