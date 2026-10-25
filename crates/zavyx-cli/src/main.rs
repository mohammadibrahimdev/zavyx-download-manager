//! Zavyx CLI.
//!
//! Provides:
//!   - `zavyx download <url>` — queue a direct download
//!   - `zavyx batch <template> <start> <end>` — batch range
//!   - `zavyx video <url>` — download a video (yt-dlp if known, else direct)
//!   - `zavyx hls <url>` — download an HLS stream via ffmpeg
//!   - `zavyx list` — list all known downloads
//!   - `zavyx config` — print/edit config
//!   - `zavyx host` — run as a Native Messaging host (stdin/stdout loop)
//!   - `zavyx --version`
//!
//! The GUI is a separate binary `Zavyx`; this CLI is for power users and the
//! browser extension's Native Messaging bridge.

use anyhow::Result;
use clap::{Parser, Subcommand};
use zavyx_core::{
    DownloadJob, DownloadOrigin, DownloadState, EngineConfig, Queue, SequenceSpec,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(
    name = "zavyx",
    version,
    about = "Zavyx — clean-room reimplementation of IDM-style download manager behavior",
    long_about = "Zavyx is a Rust-based download manager with multi-segment acceleration, \
                  queue/scheduler, batch downloads, HLS/DASH support, yt-dlp integration, \
                  and a browser extension bridge."
)]
struct Cli {
    /// Disable colored output.
    #[arg(long, global = true)]
    no_color: bool,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Download a direct URL.
    Download {
        url: String,
        /// Output file or directory.
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Override the suggested filename.
        #[arg(long)]
        filename: Option<String>,
        /// Send this referer header.
        #[arg(long)]
        referer: Option<String>,
        /// Override user agent.
        #[arg(long)]
        user_agent: Option<String>,
        /// Block until the download finishes.
        #[arg(long)]
        wait: bool,
    },
    /// Download a batch of URLs from a template with a numeric range.
    Batch {
        /// Template, e.g. "https://example.com/file_{i:02}.zip".
        template: String,
        start: u64,
        end: u64,
        /// Step size. Default 1.
        #[arg(short = 's', long, default_value = "1")]
        step: u64,
        /// Output directory.
        #[arg(short, long)]
        out: Option<PathBuf>,
        #[arg(long)]
        wait: bool,
    },
    /// Download a video from a known video site (uses yt-dlp).
    Video {
        url: String,
        /// yt-dlp format selector (e.g. "best[height<=720]").
        #[arg(short, long)]
        format: Option<String>,
        #[arg(short, long)]
        out: Option<PathBuf>,
        #[arg(long)]
        wait: bool,
    },
    /// Download an HLS (.m3u8) or DASH (.mpd) stream.
    Hls {
        url: String,
        /// Output file (.mp4 recommended).
        #[arg(short, long)]
        out: Option<PathBuf>,
        #[arg(long)]
        wait: bool,
    },
    /// List all known downloads in the queue.
    List,
    /// Cancel a download by ID.
    Cancel { id: String },
    /// Pause a download by ID.
    Pause { id: String },
    /// Resume a paused/failed download.
    Resume { id: String },
    /// Show or edit configuration.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Run as a Native Messaging host (stdin/stdout loop).
    Host,
}

#[derive(Subcommand, Debug)]
enum ConfigAction {
    /// Print the current configuration.
    Show,
    /// Set one or more fields.
    Set {
        #[arg(long)]
        download_dir: Option<PathBuf>,
        #[arg(long)]
        max_concurrent: Option<usize>,
        #[arg(long)]
        max_segments: Option<usize>,
    },
    /// Open the config file path in the system text editor (just prints the path).
    Path,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
    let cfg = EngineConfig::load_or_create()?;
    let queue = Arc::new(Queue::new(cfg.clone(), Arc::new(|_, _| {}))?);

    // For CLI commands, we want to run synchronously. Set up a tokio runtime.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    // Default to no persistent state for the CLI; persistent state lives in the GUI runtime.
    // All work runs inside the runtime so queue.pump() / tokio::spawn work.
    rt.block_on(async move {
        match cli.command {
            Commands::Download { url, out, filename, referer, user_agent, wait } => {
                let dest = out.unwrap_or_else(|| cfg.download_dir.clone());
                let job = DownloadJob {
                    id: uuid::Uuid::new_v4(),
                    origin: DownloadOrigin::Direct { url: url.clone() },
                    destination: dest,
                    filename,
                    referer,
                    user_agent,
                    extra_headers: None,
                    created_at: chrono::Utc::now(),
                    started_at: None,
                    finished_at: None,
                    total_size: None,
                    written: 0,
                    segment_count: cfg.max_segments,
                    segments: Vec::new(),
                    state: DownloadState::Queued,
                    last_error: None,
                    scheduled_for: None,
                    convert_to: None,
                    sequence: None,
                };
                let id = queue.add(job);
                println!("Queued download {}", id);
                queue.pump();
                if wait {
                    wait_for_completion(queue.clone(), id).await;
                } else {
                    // Even without --wait, give the queue a tick so work actually starts.
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    queue.pump();
                }
            }
            Commands::Batch { template, start, end, step, out, wait } => {
                let mut job = DownloadJob::default();
                job.origin = DownloadOrigin::Direct { url: template.clone() };
                job.destination = out.unwrap_or_else(|| cfg.download_dir.join("batch"));
                job.sequence = Some(SequenceSpec::Range {
                    template,
                    start,
                    end,
                    step,
                });
                let id = queue.add(job);
                println!("Queued batch {}", id);
                queue.pump();
                if wait {
                    wait_for_completion(queue.clone(), id).await;
                } else {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    queue.pump();
                }
            }
            Commands::Video { url, format, out, wait } => {
                let mut job = DownloadJob::default();
                job.origin = DownloadOrigin::YtDlp { url, format };
                job.destination = out.unwrap_or_else(|| cfg.download_dir.clone());
                let id = queue.add(job);
                println!("Queued video download {}", id);
                queue.pump();
                if wait {
                    wait_for_completion(queue.clone(), id).await;
                }
            }
            Commands::Hls { url, out, wait } => {
                let mut job = DownloadJob::default();
                job.origin = DownloadOrigin::Hls { url, page_url: String::new() };
                job.destination = out.unwrap_or_else(|| cfg.download_dir.join("stream.mp4"));
                let id = queue.add(job);
                println!("Queued HLS stream {}", id);
                queue.pump();
                if wait {
                    wait_for_completion(queue.clone(), id).await;
                }
            }
            Commands::List => {
                let items = queue.list();
                println!("{:<40} {:<14} {:>12} {:>10}", "ID", "STATE", "WRITTEN", "TOTAL");
                for j in items {
                    println!(
                        "{:<40} {:<14} {:>12} {:>10}",
                        j.id,
                        format!("{:?}", j.state).to_lowercase(),
                        j.written,
                        j.total_size.unwrap_or(0),
                    );
                }
            }
            Commands::Cancel { id } => {
                let uuid = uuid::Uuid::parse_str(&id)?;
                queue.cancel(uuid);
                println!("Cancelled {}", id);
            }
            Commands::Pause { id } => {
                let uuid = uuid::Uuid::parse_str(&id)?;
                queue.pause(uuid);
                println!("Paused {}", id);
            }
            Commands::Resume { id } => {
                let uuid = uuid::Uuid::parse_str(&id)?;
                queue.resume(uuid);
                println!("Resumed {}", id);
            }
            Commands::Config { action } => match action {
                ConfigAction::Show => {
                    let raw = toml::to_string_pretty(&cfg).unwrap_or_default();
                    println!("{}", raw);
                }
                ConfigAction::Set { download_dir, max_concurrent, max_segments } => {
                    let mut cfg = cfg.clone();
                    if let Some(d) = download_dir {
                        cfg.download_dir = d;
                    }
                    if let Some(m) = max_concurrent {
                        cfg.max_concurrent_downloads = m;
                    }
                    if let Some(m) = max_segments {
                        cfg.max_segments = m;
                    }
                    cfg.save()?;
                    println!("Configuration updated.");
                }
                ConfigAction::Path => {
                    let path = EngineConfig::config_path()?;
                    println!("{}", path.display());
                }
            },
            Commands::Host => {
                let mut host = zavyx_native_messaging::Host::new();
                zavyx_native_messaging::run_loop(&mut host, queue.clone())
                    .await
                    .expect("native messaging loop failed");
            }
        }
        Ok::<(), anyhow::Error>(())
    })?;

    Ok(())
}

/// Block until a download reaches a terminal state.
async fn wait_for_completion(queue: Arc<Queue>, id: zavyx_core::DownloadId) {
    let mut interval = tokio::time::interval(Duration::from_millis(250));
    loop {
        interval.tick().await;
        queue.pump();
        if let Some(snap) = queue.progress(id) {
            match snap.state {
                DownloadState::Completed => {
                    println!("Completed.");
                    return;
                }
                DownloadState::Failed | DownloadState::Cancelled => {
                    println!("Terminal state: {:?}", snap.state);
                    return;
                }
                _ => {
                    println!(
                        "{:50} {:>10} / {:>10}",
                        format!("{:?}", snap.state),
                        snap.written,
                        snap.total.unwrap_or(0),
                    );
                }
            }
        } else {
            println!("Download not found.");
            return;
        }
    }
}
