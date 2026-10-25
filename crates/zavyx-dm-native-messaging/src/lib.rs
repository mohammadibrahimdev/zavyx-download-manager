//! Native Messaging protocol implementation.
//!
//! Spec (Chrome/Edge/Brave/Opera MV3):
//!   - 4-byte little-endian length prefix, then JSON payload of that many bytes.
//!   - stdin receives messages from the extension.
//!   - stdout sends messages to the extension.
//!   - stderr is ignored by the browser but useful for logging.
//!
//! Firefox WebExtension uses the same protocol, just registered via a different
//! manifest location (postbox.json / native-messaging-hosts/).

use zavyx_dm_core::{DownloadJob, DownloadOrigin, EngineConfig, Queue};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

#[derive(Debug, thiserror::Error)]
pub enum NmError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("message too large: {len} bytes")]
    TooLarge { len: u32 },
    #[error("invalid message: {0}")]
    Invalid(String),
}

pub type NmResult<T> = Result<T, NmError>;

/// One message from the extension.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IncomingMessage {
    Ping,
    AddDownload {
        url: String,
        referer: Option<String>,
        user_agent: Option<String>,
        filename: Option<String>,
        convert_to: Option<String>,
    },
    AddBatchRange {
        template: String,
        start: u64,
        end: u64,
        step: u64,
    },
    AddBatchList {
        template: String,
        items: Vec<String>,
    },
    AddVideo {
        url: String,
        #[serde(default)]
        page_url: String,
        kind: Option<String>, // "youtube", "vimeo", "twitter", "tiktok", "generic", "page"
        /// yt-dlp format selector (quality); `None` means best.
        #[serde(default)]
        format: Option<String>,
    },
    AddHls {
        url: String,
        #[serde(default)]
        page_url: String,
    },
    CancelDownload { id: String },
    PauseDownload { id: String },
    ResumeDownload { id: String },
    ListDownloads,
    GetProgress { id: String },
    SetConfig {
        download_dir: Option<String>,
        max_concurrent_downloads: Option<usize>,
        max_segments: Option<usize>,
    },
    GetConfig,
    /// Bring the desktop app window to the front (launching it if needed).
    ShowWindow,
    Quit,
}

impl IncomingMessage {
    /// Messages that need the desktop app running, so the host launches it.
    fn wants_app(&self) -> bool {
        matches!(
            self,
            Self::AddDownload { .. }
                | Self::AddBatchRange { .. }
                | Self::AddBatchList { .. }
                | Self::AddVideo { .. }
                | Self::AddHls { .. }
                | Self::ShowWindow
        )
    }
}

/// One message from the host to the extension.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutgoingMessage {
    Pong { version: String },
    DownloadAdded { id: String },
    DownloadList { items: Vec<DownloadSummary> },
    Progress {
        id: String,
        state: String,
        total: Option<u64>,
        written: u64,
        speed_bps: Option<f64>,
    },
    Config { config: EngineConfigSnapshot },
    Error { message: String },
    Ok,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadSummary {
    pub id: String,
    pub url: String,
    pub filename: Option<String>,
    pub state: String,
    pub total: Option<u64>,
    pub written: u64,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl From<&DownloadJob> for DownloadSummary {
    fn from(j: &DownloadJob) -> Self {
        Self {
            id: j.id.to_string(),
            url: j.origin.url().to_string(),
            filename: j.filename.clone(),
            state: format!("{:?}", j.state).to_lowercase(),
            total: j.total_size,
            written: j.written,
            created_at: j.created_at,
            started_at: j.started_at,
            finished_at: j.finished_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineConfigSnapshot {
    pub download_dir: String,
    pub max_concurrent_downloads: usize,
    pub max_segments: usize,
    pub max_retries: u32,
    pub ytdlp_path: String,
    pub ffmpeg_path: String,
    pub auto_ytdlp: bool,
    pub auto_convert_streams: bool,
}

impl From<&EngineConfig> for EngineConfigSnapshot {
    fn from(c: &EngineConfig) -> Self {
        Self {
            download_dir: c.download_dir.to_string_lossy().to_string(),
            max_concurrent_downloads: c.max_concurrent_downloads,
            max_segments: c.max_segments,
            max_retries: c.max_retries,
            ytdlp_path: c.ytdlp_path.to_string_lossy().to_string(),
            ffmpeg_path: c.ffmpeg_path.to_string_lossy().to_string(),
            auto_ytdlp: c.auto_ytdlp,
            auto_convert_streams: c.auto_convert_streams,
        }
    }
}

/// Native Messaging host. Owns the read/write halves of stdin/stdout.
pub struct Host {
    reader: BufReader<tokio::io::Stdin>,
    writer: Arc<Mutex<tokio::io::Stdout>>,
}

impl Host {
    pub fn new() -> Self {
        Self {
            reader: BufReader::new(tokio::io::stdin()),
            writer: Arc::new(Mutex::new(tokio::io::stdout())),
        }
    }

    /// Read one length-prefixed message.
    pub async fn read_message(&mut self) -> NmResult<Option<IncomingMessage>> {
        match self.read_raw().await? {
            Some(v) => Ok(Some(serde_json::from_value(v)?)),
            None => Ok(None),
        }
    }

    /// Read one length-prefixed message as untyped JSON, so the caller can pick
    /// out the extension's `seq` and survive messages it can't parse.
    pub async fn read_raw(&mut self) -> NmResult<Option<serde_json::Value>> {
        let mut len_buf = [0u8; 4];
        // Read exactly 4 bytes; EOF means the browser closed the channel.
        let read = self.reader.read_exact(&mut len_buf).await;
        match read {
            Ok(_) => {}
            // EOF or a closed pipe both mean the browser went away.
            Err(e) if matches!(e.kind(), io::ErrorKind::UnexpectedEof | io::ErrorKind::BrokenPipe) => return Ok(None),
            Err(e) => return Err(NmError::Io(e)),
        }
        let len = u32::from_le_bytes(len_buf);
        if len > 16 * 1024 * 1024 {
            return Err(NmError::TooLarge { len });
        }
        let mut payload = vec![0u8; len as usize];
        if len > 0 {
            self.reader.read_exact(&mut payload).await?;
        }
        Ok(Some(serde_json::from_slice(&payload)?))
    }

    /// Write one length-prefixed message.
    pub async fn write_message(&self, msg: &OutgoingMessage) -> NmResult<()> {
        self.write_reply(msg, None).await
    }

    /// Write one length-prefixed message, echoing the request's `seq` so the
    /// extension can match the reply to its pending promise.
    pub async fn write_reply(&self, msg: &OutgoingMessage, seq: Option<serde_json::Value>) -> NmResult<()> {
        let payload = encode_reply(msg, seq)?;
        let len = payload.len() as u32;
        let len_bytes = len.to_le_bytes();
        let mut stdout = self.writer.lock().await;
        stdout.write_all(&len_bytes).await?;
        stdout.write_all(&payload).await?;
        stdout.flush().await?;
        Ok(())
    }
}

impl Default for Host {
    fn default() -> Self {
        Self::new()
    }
}

/// Main native-messaging loop. Reads messages from the extension, dispatches on the
/// queue, and writes responses back.
///
/// Requests are handed to the running desktop app (launching it for new
/// downloads) so they show up in its window and outlive the browser's port.
/// `queue` is only a fallback for when the app can't be reached.
pub async fn run_loop(host: &mut Host, queue: Arc<Queue>) -> anyhow::Result<()> {
    let mut app: Option<AppLink> = None;
    while let Some(raw) = host.read_raw().await? {
        let seq = raw.get("seq").cloned();
        let outgoing = match serde_json::from_value::<IncomingMessage>(raw) {
            Ok(IncomingMessage::Ping) => pong(),
            Ok(msg) => match forward_to_app(&mut app, &msg).await {
                Some(out) => out,
                None => handle_message(msg, &queue)
                    .await
                    .unwrap_or_else(|e| OutgoingMessage::Error { message: e.to_string() }),
            },
            Err(e) => OutgoingMessage::Error { message: format!("invalid message: {}", e) },
        };
        host.write_reply(&outgoing, seq).await?;
    }
    Ok(())
}

fn pong() -> OutgoingMessage {
    OutgoingMessage::Pong {
        version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

/// Serialize a reply, adding `seq` when the request carried one.
pub fn encode_reply(msg: &OutgoingMessage, seq: Option<serde_json::Value>) -> serde_json::Result<Vec<u8>> {
    let mut value = serde_json::to_value(msg)?;
    if let (Some(seq), Some(obj)) = (seq, value.as_object_mut()) {
        obj.insert("seq".into(), seq);
    }
    serde_json::to_vec(&value)
}

// --- Link between the browser host and the desktop app ---

/// Loopback address the desktop app listens on for requests from the host.
pub const APP_ADDR: &str = "127.0.0.1:47391";

/// File name of the desktop app, expected next to the host binary.
pub const APP_EXE: &str = if cfg!(windows) { "ZavyxDM.exe" } else { "ZavyxDM" };

/// Called by the app for each request before it is handled (e.g. to focus the window).
pub type AppHook = Arc<dyn Fn(&IncomingMessage) + Send + Sync>;

/// One newline-delimited JSON connection to the running desktop app.
struct AppLink {
    lines: tokio::io::Lines<BufReader<tokio::net::tcp::OwnedReadHalf>>,
    writer: tokio::net::tcp::OwnedWriteHalf,
}

impl AppLink {
    async fn connect() -> Option<Self> {
        let stream = tokio::net::TcpStream::connect(APP_ADDR).await.ok()?;
        let (rd, writer) = stream.into_split();
        Some(Self { lines: BufReader::new(rd).lines(), writer })
    }

    async fn request(&mut self, msg: &IncomingMessage) -> Option<OutgoingMessage> {
        let mut buf = serde_json::to_vec(msg).ok()?;
        buf.push(b'\n');
        self.writer.write_all(&buf).await.ok()?;
        let line = self.lines.next_line().await.ok()??;
        serde_json::from_str(&line).ok()
    }
}

/// Send `msg` to the desktop app, launching it first when the request needs it.
/// Returns `None` when the app isn't reachable.
async fn forward_to_app(link: &mut Option<AppLink>, msg: &IncomingMessage) -> Option<OutgoingMessage> {
    let mut launched = false;
    for _ in 0..2 {
        if link.is_none() {
            *link = AppLink::connect().await;
        }
        if link.is_none() && msg.wants_app() && !launched {
            launched = true;
            if launch_app() {
                for _ in 0..100 {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    *link = AppLink::connect().await;
                    if link.is_some() {
                        break;
                    }
                }
            }
        }
        let conn = link.as_mut()?;
        if let Some(out) = conn.request(msg).await {
            return Some(out);
        }
        // Stale connection (app restarted); reconnect once.
        *link = None;
    }
    None
}

/// Start the desktop app detached from the browser's pipes.
fn launch_app() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let app = exe.with_file_name(APP_EXE);
    let mut cmd = std::process::Command::new(&app);
    // The app must not inherit the host's stdio: that is the browser's message pipe.
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Windows children inherit every inheritable handle, not just the ones
        // passed above. If the app kept the browser's pipe ends open, the
        // browser would never notice this host exiting and the extension hangs.
        stop_std_handle_inheritance();
        // Leave the browser's job object so the app survives the host exiting.
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        if cmd.creation_flags(CREATE_BREAKAWAY_FROM_JOB).spawn().is_ok() {
            return true;
        }
        cmd.creation_flags(0);
    }
    match cmd.spawn() {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!(?app, error = %e, "failed to launch app");
            false
        }
    }
}

#[cfg(windows)]
fn stop_std_handle_inheritance() {
    extern "system" {
        fn GetStdHandle(which: u32) -> isize;
        fn SetHandleInformation(handle: isize, mask: u32, flags: u32) -> i32;
    }
    const HANDLE_FLAG_INHERIT: u32 = 1;
    // STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE
    for which in [-10i32, -11, -12] {
        // SAFETY: plain Win32 calls on this process's own standard handles.
        unsafe {
            let h = GetStdHandle(which as u32);
            if h != 0 && h != -1 {
                SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

/// Serve host requests in the desktop app until the listener fails.
pub async fn serve_app(queue: Arc<Queue>, listener: tokio::net::TcpListener, hook: AppHook) {
    loop {
        let Ok((stream, peer)) = listener.accept().await else {
            continue;
        };
        if !peer.ip().is_loopback() {
            continue;
        }
        let queue = queue.clone();
        let hook = hook.clone();
        tokio::spawn(async move {
            let (rd, mut wr) = stream.into_split();
            let mut lines = BufReader::new(rd).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let out = match serde_json::from_str::<IncomingMessage>(&line) {
                    // The browser must never be able to close the app.
                    Ok(IncomingMessage::Quit) => OutgoingMessage::Ok,
                    Ok(msg) => {
                        hook(&msg);
                        handle_message(msg, &queue)
                            .await
                            .unwrap_or_else(|e| OutgoingMessage::Error { message: e.to_string() })
                    }
                    Err(e) => OutgoingMessage::Error { message: format!("invalid message: {}", e) },
                };
                let mut buf = serde_json::to_vec(&out).unwrap_or_default();
                buf.push(b'\n');
                if wr.write_all(&buf).await.is_err() {
                    break;
                }
            }
        });
    }
}

/// Dispatch one incoming message.
pub async fn handle_message(msg: IncomingMessage, queue: &Arc<Queue>) -> anyhow::Result<OutgoingMessage> {
    match msg {
        IncomingMessage::Ping => Ok(pong()),
        IncomingMessage::ShowWindow => Ok(OutgoingMessage::Ok),
        IncomingMessage::AddDownload { url, referer, user_agent, filename, convert_to } => {
            let dest_dir = queue.config().download_dir.clone();
            let filename = filename.unwrap_or_else(|| filename_from_url(&url));
            let dest = unique_path(dest_dir.join(&filename));
            let job = DownloadJob {
                id: uuid::Uuid::new_v4(),
                origin: DownloadOrigin::Direct { url: url.clone() },
                destination: dest,
                filename: Some(filename),
                referer,
                user_agent,
                extra_headers: None,
                created_at: chrono::Utc::now(),
                started_at: None,
                finished_at: None,
                total_size: None,
                written: 0,
                segment_count: queue.config().max_segments,
                segments: Vec::new(),
                state: zavyx_dm_core::DownloadState::Queued,
                last_error: None,
                scheduled_for: None,
                convert_to: convert_to.map(|c| zavyx_dm_core::ConvertProfile {
                    container: c,
                    audio_codec: None,
                    video_codec: None,
                }),
                sequence: None,
            };
            let id = queue.add(job);
            Ok(OutgoingMessage::DownloadAdded { id: id.to_string() })
        }
        IncomingMessage::AddBatchRange { template, start, end, step } => {
            let mut job = DownloadJob::default();
            job.origin = DownloadOrigin::Direct { url: template.clone() };
            job.destination = queue.config().download_dir.join("batch");
            job.sequence = Some(zavyx_dm_core::SequenceSpec::Range {
                template,
                start,
                end,
                step,
            });
            let id = queue.add(job);
            Ok(OutgoingMessage::DownloadAdded { id: id.to_string() })
        }
        IncomingMessage::AddBatchList { template, items } => {
            let mut job = DownloadJob::default();
            job.origin = DownloadOrigin::Direct { url: template.clone() };
            job.destination = queue.config().download_dir.join("batch");
            job.sequence = Some(zavyx_dm_core::SequenceSpec::List { template, items });
            let id = queue.add(job);
            Ok(OutgoingMessage::DownloadAdded { id: id.to_string() })
        }
        IncomingMessage::AddVideo { url, page_url, kind, format } => {
            // Decide routing: known video site → yt-dlp, generic → direct.
            let cfg = queue.config();
            let is_known = is_known_video_site(&url) || kind.is_some();
            let origin = if cfg.auto_ytdlp && is_known {
                DownloadOrigin::YtDlp { url: url.clone(), format: format.filter(|f| !f.is_empty()) }
            } else {
                DownloadOrigin::HtmlVideo { url: url.clone(), page_url: page_url.clone() }
            };
            let mut job = DownloadJob::default();
            // yt-dlp names its own output inside a folder; direct video needs a file path.
            job.destination = match origin {
                DownloadOrigin::YtDlp { .. } => cfg.download_dir.clone(),
                _ => unique_path(cfg.download_dir.join(filename_from_url(&url))),
            };
            job.origin = origin;
            job.referer = if page_url.is_empty() { None } else { Some(page_url) };
            let id = queue.add(job);
            Ok(OutgoingMessage::DownloadAdded { id: id.to_string() })
        }
        IncomingMessage::AddHls { url, page_url } => {
            let cfg = queue.config();
            let filename = format!("{}.mp4", filename_from_url(&url).trim_end_matches(".m3u8"));
            let mut job = DownloadJob::default();
            job.origin = DownloadOrigin::Hls { url: url.clone(), page_url };
            job.destination = cfg.download_dir.join(filename);
            job.convert_to = if cfg.auto_convert_streams {
                Some(zavyx_dm_core::ConvertProfile {
                    container: "mp4".into(),
                    audio_codec: None,
                    video_codec: None,
                })
            } else {
                None
            };
            let id = queue.add(job);
            Ok(OutgoingMessage::DownloadAdded { id: id.to_string() })
        }
        IncomingMessage::CancelDownload { id } => {
            if let Ok(uuid) = uuid::Uuid::parse_str(&id) {
                queue.cancel(uuid);
            }
            Ok(OutgoingMessage::Ok)
        }
        IncomingMessage::PauseDownload { id } => {
            if let Ok(uuid) = uuid::Uuid::parse_str(&id) {
                queue.pause(uuid);
            }
            Ok(OutgoingMessage::Ok)
        }
        IncomingMessage::ResumeDownload { id } => {
            if let Ok(uuid) = uuid::Uuid::parse_str(&id) {
                queue.resume(uuid);
            }
            Ok(OutgoingMessage::Ok)
        }
        IncomingMessage::ListDownloads => {
            let items = queue
                .list()
                .iter()
                .map(|j| DownloadSummary::from(j))
                .collect();
            Ok(OutgoingMessage::DownloadList { items })
        }
        IncomingMessage::GetProgress { id } => {
            let uuid = uuid::Uuid::parse_str(&id).map_err(|e| anyhow::anyhow!("invalid uuid: {}", e))?;
            let snap = queue.progress(uuid).ok_or_else(|| anyhow::anyhow!("no such download"))?;
            Ok(OutgoingMessage::Progress {
                id,
                state: format!("{:?}", snap.state).to_lowercase(),
                total: snap.total,
                written: snap.written,
                speed_bps: snap.speed_bps,
            })
        }
        IncomingMessage::SetConfig { download_dir, max_concurrent_downloads, max_segments } => {
            let cfg_path = EngineConfig::config_path().ok();
            let mut cfg = EngineConfig::load_or_create().unwrap_or_default();
            if let Some(d) = download_dir {
                cfg.download_dir = PathBuf::from(d);
            }
            if let Some(m) = max_concurrent_downloads {
                cfg.max_concurrent_downloads = m;
            }
            if let Some(m) = max_segments {
                cfg.max_segments = m;
            }
            cfg.save().ok();
            let _ = cfg_path;
            Ok(OutgoingMessage::Ok)
        }
        IncomingMessage::GetConfig => {
            let cfg = queue.config();
            Ok(OutgoingMessage::Config { config: EngineConfigSnapshot::from(cfg) })
        }
        IncomingMessage::Quit => {
            tracing::info!("Quit message received, exiting loop");
            std::process::exit(0);
        }
    }
}

/// Heuristic check: is this URL one yt-dlp supports?
fn is_known_video_site(url: &str) -> bool {
    let host = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|s| s.to_lowercase()))
        .unwrap_or_default();
    matches!(
        host.as_str(),
        s if s.contains("youtube.com")
            || s.contains("youtu.be")
            || s.contains("vimeo.com")
            || s.contains("dailymotion.com")
            || s.contains("twitch.tv")
            || s.contains("tiktok.com")
            || s.contains("instagram.com")
            || s.contains("twitter.com")
            || s.contains("x.com")
            || s.contains("facebook.com")
            || s.contains("v.qq.com")
            || s.contains("bilibili.com")
            || s.contains("pinterest.com")
            || s.contains("reddit.com")
            || s.contains("soundcloud.com")
    )
}

/// `path`, or `name (1).ext`, `name (2).ext`... if a file already exists there.
fn unique_path(path: PathBuf) -> PathBuf {
    if !path.exists() {
        return path;
    }
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    (1..)
        .map(|n| path.with_file_name(format!("{} ({}){}", stem, n, ext)))
        .find(|p| !p.exists())
        .unwrap_or(path)
}

/// Derive a sane filename from a URL's last path segment.
pub fn filename_from_url(url: &str) -> String {
    let parsed = url::Url::parse(url).ok();
    let path = parsed
        .as_ref()
        .map(|u| u.path().to_string())
        .unwrap_or_default();
    let seg = path.rsplit('/').next().unwrap_or("");
    let seg = percent_encoding::percent_decode_str(seg)
        .decode_utf8_lossy()
        .to_string();
    if seg.is_empty() || seg.is_empty() {
        return "download.bin".to_string();
    }
    if !seg.contains('.') {
        format!("{}.bin", seg)
    } else {
        seg
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_from_youtube_url() {
        // YouTube URLs are not filename-shaped, fall back.
        let s = filename_from_url("https://www.youtube.com/watch?v=dQw4w9WgXcQ");
        assert!(!s.is_empty());
    }

    #[test]
    fn filename_from_direct_url() {
        let s = filename_from_url("https://example.com/path/to/file.zip");
        assert_eq!(s, "file.zip");
    }

    #[test]
    fn known_video_sites_match() {
        assert!(is_known_video_site("https://www.youtube.com/watch?v=x"));
        assert!(is_known_video_site("https://youtu.be/abc"));
        assert!(is_known_video_site("https://www.tiktok.com/@user/video/123"));
        assert!(!is_known_video_site("https://example.com/video.mp4"));
    }

    #[test]
    fn reply_echoes_seq() {
        let bytes = encode_reply(&pong(), Some(serde_json::json!("abc"))).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["type"], "pong");
        assert_eq!(v["seq"], "abc");
    }

    #[test]
    fn video_without_page_url_parses() {
        let msg: IncomingMessage =
            serde_json::from_str(r#"{"type":"add_video","url":"https://youtu.be/x","seq":"1"}"#).unwrap();
        assert!(matches!(msg, IncomingMessage::AddVideo { .. }));
    }
}

