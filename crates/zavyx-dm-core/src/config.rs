//! Engine configuration. Persisted as TOML in the user's config dir.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineConfig {
    /// Default download directory.
    pub download_dir: PathBuf,
    /// Number of concurrent downloads (queue worker slots).
    pub max_concurrent_downloads: usize,
    /// Number of segments per multi-segment download.
    pub max_segments: usize,
    /// Per-segment max retries.
    pub max_retries: u32,
    /// Initial retry backoff in milliseconds.
    pub retry_backoff_ms: u64,
    /// Per-connection speed limit (bytes/sec), 0 = unlimited.
    pub speed_limit_per_segment: u64,
    /// Global speed limit (bytes/sec), 0 = unlimited.
    pub global_speed_limit: u64,
    /// Default user-agent.
    pub user_agent: String,
    /// Path to yt-dlp binary.
    pub ytdlp_path: PathBuf,
    /// Path to ffmpeg binary.
    pub ffmpeg_path: PathBuf,
    /// Whether to use yt-dlp for known video sites automatically.
    pub auto_ytdlp: bool,
    /// Whether to auto-convert HLS/DASH to mp4 after download.
    pub auto_convert_streams: bool,
}

impl Default for EngineConfig {
    fn default() -> Self {
        let download_dir = directories::UserDirs::new()
            .and_then(|d| d.download_dir().map(|p| p.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("./downloads"));
        Self {
            download_dir,
            max_concurrent_downloads: 4,
            max_segments: 8,
            max_retries: 5,
            retry_backoff_ms: 1000,
            speed_limit_per_segment: 0,
            global_speed_limit: 0,
            user_agent: "Zavyx DM/0.1".to_string(),
            ytdlp_path: find_tool("yt-dlp").unwrap_or_else(|| PathBuf::from("yt-dlp")),
            ffmpeg_path: find_tool("ffmpeg").unwrap_or_else(|| PathBuf::from("ffmpeg")),
            auto_ytdlp: true,
            auto_convert_streams: true,
        }
    }
}

impl EngineConfig {
    /// Load from the user's config dir, creating a default if absent.
    pub fn load_or_create() -> anyhow::Result<Self> {
        migrate_from_v0_1();
        let path = Self::config_path()?;
        if path.exists() {
            let raw = std::fs::read_to_string(&path)?;
            let mut cfg: Self = toml::from_str(&raw)?;
            // A saved bare name ("yt-dlp") that isn't on PATH would make video
            // downloads fail; prefer the copy shipped next to the app.
            for (path, name) in [(&mut cfg.ytdlp_path, "yt-dlp"), (&mut cfg.ffmpeg_path, "ffmpeg")] {
                if !path.is_file() && which(&path.to_string_lossy()).is_none() {
                    if let Some(found) = find_tool(name) {
                        *path = found;
                    }
                }
            }
            Ok(cfg)
        } else {
            let cfg = Self::default();
            cfg.save()?;
            Ok(cfg)
        }
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::config_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let raw = toml::to_string_pretty(self)?;
        std::fs::write(path, raw)?;
        Ok(())
    }

    pub fn config_path() -> anyhow::Result<PathBuf> {
        let proj = directories::ProjectDirs::from("io.github", "mohammadibrahimdev", "Zavyx DM")
            .ok_or_else(|| anyhow::anyhow!("Cannot determine config directory"))?;
        Ok(proj.config_dir().join("config.toml"))
    }

    pub fn state_dir() -> anyhow::Result<PathBuf> {
        let proj = directories::ProjectDirs::from("io.github", "mohammadibrahimdev", "Zavyx DM")
            .ok_or_else(|| anyhow::anyhow!("Cannot determine state directory"))?;
        Ok(proj.data_local_dir().to_path_buf())
    }
}

/// v0.1.0 stored its settings and download list under the app name "Zavyx".
/// Copy them to the "Zavyx DM" folders once, if the new ones don't exist yet.
fn migrate_from_v0_1() {
    let (Some(old), Some(new)) = (
        directories::ProjectDirs::from("io.github", "mohammadibrahimdev", "Zavyx"),
        directories::ProjectDirs::from("io.github", "mohammadibrahimdev", "Zavyx DM"),
    ) else {
        return;
    };
    for (from, to) in [
        (old.config_dir().join("config.toml"), new.config_dir().join("config.toml")),
        (old.data_local_dir().join("downloads.json"), new.data_local_dir().join("downloads.json")),
    ] {
        if from.is_file() && !to.exists() {
            if let Some(dir) = to.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::copy(&from, &to);
        }
    }
}

/// Find a helper tool: next to the running executable first (bundled with the
/// installer), then on PATH.
fn find_tool(name: &str) -> Option<PathBuf> {
    let exe_name = if cfg!(windows) { format!("{}.exe", name) } else { name.to_string() };
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(&exe_name)))
        .filter(|p| p.is_file())
        .or_else(|| which(name))
}

/// Tiny PATH-based binary lookup, avoids pulling in the `which` crate.
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name).with_extension(
            if cfg!(windows) { "exe" } else { "" },
        );
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}
