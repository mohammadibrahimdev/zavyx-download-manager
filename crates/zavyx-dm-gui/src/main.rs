//! Zavyx Download Manager — desktop app built on egui.
//!
//! Layout (classic download-manager style):
//!   - Top: menu bar + large toolbar (Add URL, Resume, Stop, Stop All, Delete, Options)
//!   - Left: categories (by file type, finished/unfinished)
//!   - Center: downloads table (name, size, status, time left, speed, last try)
//!   - Bottom: status bar with totals and the author's GitHub link
//!
//! All download state is owned by `Queue`, driven by a background tokio runtime.
//! The browser extension reaches this app through `zavyx_dm_native_messaging::serve_app`.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use anyhow::Result;
use chrono::Local;
use egui::{Align, Color32, Layout, RichText, Vec2};
use egui_extras::{Column, TableBuilder};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use zavyx_dm_core::{DownloadId, DownloadJob, DownloadOrigin, DownloadState, EngineConfig, Queue};
use zavyx_dm_native_messaging::{filename_from_url, IncomingMessage, APP_ADDR};

const APP_NAME: &str = "Zavyx Download Manager";
const AUTHOR: &str = "Mohammad Ibrahim";
const GITHUB_PROFILE: &str = "https://github.com/mohammadibrahimdev";
const REPO_URL: &str = "https://github.com/mohammadibrahimdev/zavyx-download-manager";
const RELEASES_URL: &str = "https://github.com/mohammadibrahimdev/zavyx-download-manager/releases";
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Brand colour ("Zavyx violet").
const ACCENT: Color32 = Color32::from_rgb(109, 74, 255);
const ACCENT_DARK: Color32 = Color32::from_rgb(91, 52, 230);
const OK_GREEN: Color32 = Color32::from_rgb(34, 160, 90);
const ERR_RED: Color32 = Color32::from_rgb(210, 60, 60);

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive("zavyx=info".parse()?))
        .init();

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    // Enter the runtime on the GUI thread too: button handlers call
    // `queue.pump()`, which uses `tokio::spawn` and needs a runtime context.
    let _rt_guard = rt.enter();

    // Single instance: if Zavyx DM is already running, bring it forward and exit.
    let listener = match rt.block_on(tokio::net::TcpListener::bind(APP_ADDR)) {
        Ok(l) => Some(l),
        Err(_) => {
            if notify_running_instance() {
                return Ok(());
            }
            None // Port held by something else; run without browser integration.
        }
    };

    let cfg = EngineConfig::load_or_create()?;
    let progress: zavyx_dm_core::ProgressCallback = Arc::new(|id, snap| {
        tracing::debug!(?id, state = ?snap.state, "progress");
    });
    let queue = Arc::new(Queue::new(cfg, progress)?);
    for job in load_saved_jobs() {
        queue.restore(job);
    }

    let q2 = queue.clone();
    rt.spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(500));
        loop {
            interval.tick().await;
            q2.pump();
        }
    });

    // Save the download list whenever it changes. Runs off the UI thread so it
    // keeps working while the window is hidden or minimized.
    let q3 = queue.clone();
    rt.spawn(async move {
        let mut last_sig = jobs_signature(&q3.list());
        let mut interval = tokio::time::interval(Duration::from_secs(2));
        loop {
            interval.tick().await;
            let jobs = q3.list();
            let sig = jobs_signature(&jobs);
            if sig != last_sig {
                save_jobs(&jobs);
                last_sig = sig;
            }
        }
    });

    // Requests from the browser extension bring the window to the front.
    let ctx_cell: Arc<OnceLock<egui::Context>> = Arc::new(OnceLock::new());
    if let Some(listener) = listener {
        let cell = ctx_cell.clone();
        let hook: zavyx_dm_native_messaging::AppHook = Arc::new(move |msg: &IncomingMessage| {
            let Some(ctx) = cell.get() else { return };
            if !matches!(msg, IncomingMessage::ListDownloads | IncomingMessage::GetProgress { .. } | IncomingMessage::GetConfig) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
            ctx.request_repaint();
        });
        rt.spawn(zavyx_dm_native_messaging::serve_app(queue.clone(), listener, hook));
    }

    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1040.0, 620.0])
        .with_min_inner_size([720.0, 420.0])
        .with_title(APP_NAME);
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/logo-256.png")) {
        viewport = viewport.with_icon(icon);
    }
    let native_options = eframe::NativeOptions { viewport, ..Default::default() };

    eframe::run_native(
        APP_NAME,
        native_options,
        Box::new(move |cc| {
            let _ = ctx_cell.set(cc.egui_ctx.clone());
            Ok(Box::new(ZavyxDmApp::new(cc, queue)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe error: {}", e))?;

    Ok(())
}

/// Ask an already-running Zavyx DM to show its window. Returns true if it answered.
fn notify_running_instance() -> bool {
    let Ok(mut s) = std::net::TcpStream::connect(APP_ADDR) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
    if s.write_all(b"{\"type\":\"show_window\"}\n").is_err() {
        return false;
    }
    let mut buf = [0u8; 64];
    matches!(s.read(&mut buf), Ok(n) if n > 0)
}

// --- Persistence of the download list ---

fn jobs_file() -> Option<PathBuf> {
    EngineConfig::state_dir().ok().map(|d| d.join("downloads.json"))
}

fn load_saved_jobs() -> Vec<DownloadJob> {
    jobs_file()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Cheap fingerprint of the list, to skip saving when nothing changed.
fn jobs_signature(jobs: &[DownloadJob]) -> u64 {
    jobs.iter().fold(jobs.len() as u64, |acc, j| {
        let id = j.id.as_u128() as u64;
        acc.wrapping_mul(31).wrapping_add(id ^ (j.state as u64 * 7 + j.written))
    })
}

fn save_jobs(jobs: &[DownloadJob]) {
    let Some(path) = jobs_file() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string_pretty(jobs) {
        let _ = std::fs::write(path, json);
    }
}

// --- Categories ---

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Category {
    All,
    Compressed,
    Documents,
    Music,
    Programs,
    Video,
    Other,
    Unfinished,
    Finished,
}

impl Category {
    const TYPES: [Category; 7] = [
        Category::All,
        Category::Compressed,
        Category::Documents,
        Category::Music,
        Category::Programs,
        Category::Video,
        Category::Other,
    ];

    fn label(self) -> &'static str {
        match self {
            Category::All => "📥  All Downloads",
            Category::Compressed => "📁  Compressed",
            Category::Documents => "📄  Documents",
            Category::Music => "🎵  Music",
            Category::Programs => "💾  Programs",
            Category::Video => "🎬  Video",
            Category::Other => "📦  Other",
            Category::Unfinished => "⏳  Unfinished",
            Category::Finished => "✅  Finished",
        }
    }

    fn matches(self, job: &DownloadJob) -> bool {
        match self {
            Category::All => true,
            Category::Unfinished => job.state != DownloadState::Completed,
            Category::Finished => job.state == DownloadState::Completed,
            c => file_category(job) == c,
        }
    }
}

fn file_category(job: &DownloadJob) -> Category {
    if matches!(
        job.origin,
        DownloadOrigin::YtDlp { .. } | DownloadOrigin::Hls { .. } | DownloadOrigin::Dash { .. } | DownloadOrigin::HtmlVideo { .. }
    ) {
        return Category::Video;
    }
    let name = display_name(job).to_lowercase();
    let ext = name.rsplit('.').next().unwrap_or("");
    match ext {
        "zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "xz" | "iso" | "tgz" => Category::Compressed,
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "txt" | "epub" | "csv" | "rtf" => Category::Documents,
        "mp3" | "wav" | "flac" | "aac" | "m4a" | "ogg" | "wma" | "opus" => Category::Music,
        "exe" | "msi" | "apk" | "dmg" | "deb" | "rpm" | "appimage" | "msix" => Category::Programs,
        "mp4" | "mkv" | "webm" | "avi" | "mov" | "flv" | "m4v" | "wmv" | "ts" | "3gp" => Category::Video,
        _ => Category::Other,
    }
}

// --- Dialog state ---

#[derive(Default)]
struct AddUrlDialog {
    open: bool,
    url: String,
    file_name: String,
    /// File name was typed by the user; stop auto-filling it from the URL.
    name_edited: bool,
    folder: String,
    referer: String,
    user_agent: String,
}

#[derive(Default)]
struct AddVideoDialog {
    open: bool,
    url: String,
    quality: usize,
    folder: String,
}

const VIDEO_QUALITIES: [(&str, Option<&str>); 6] = [
    ("Best available", None),
    ("1080p", Some("bv*[height<=1080]+ba/b[height<=1080]")),
    ("720p", Some("bv*[height<=720]+ba/b[height<=720]")),
    ("480p", Some("bv*[height<=480]+ba/b[height<=480]")),
    ("360p", Some("bv*[height<=360]+ba/b[height<=360]")),
    ("Audio only", Some("bestaudio/best")),
];

#[derive(Default)]
struct AddStreamDialog {
    open: bool,
    url: String,
    file_name: String,
}

struct AddBatchDialog {
    open: bool,
    template: String,
    start: u64,
    end: u64,
    step: u64,
}

impl Default for AddBatchDialog {
    fn default() -> Self {
        Self { open: false, template: String::new(), start: 1, end: 10, step: 1 }
    }
}

/// Smoothed transfer rate for one download.
struct SpeedTrack {
    bytes: u64,
    at: Instant,
    bps: f64,
}

struct ZavyxDmApp {
    queue: Arc<Queue>,
    config: EngineConfig,
    jobs: Vec<DownloadJob>,
    last_refresh: Instant,
    speeds: HashMap<DownloadId, SpeedTrack>,
    /// Last state seen per job, to detect completions.
    seen_states: HashMap<DownloadId, DownloadState>,
    finished_popups: Vec<DownloadId>,
    category: Category,
    selected: Option<DownloadId>,
    search: String,
    add_url: AddUrlDialog,
    add_video: AddVideoDialog,
    add_stream: AddStreamDialog,
    add_batch: AddBatchDialog,
    options_open: bool,
    options_saved: bool,
    about_open: bool,
    dark_mode: bool,
}

impl ZavyxDmApp {
    fn new(cc: &eframe::CreationContext<'_>, queue: Arc<Queue>) -> Self {
        let config = queue.config().clone();
        apply_theme(&cc.egui_ctx, false);
        let jobs = queue.list();
        let seen_states = jobs.iter().map(|j| (j.id, j.state)).collect();
        Self {
            queue,
            config,
            jobs,
            last_refresh: Instant::now(),
            speeds: HashMap::new(),
            seen_states,
            finished_popups: Vec::new(),
            category: Category::All,
            selected: None,
            search: String::new(),
            add_url: AddUrlDialog::default(),
            add_video: AddVideoDialog::default(),
            add_stream: AddStreamDialog::default(),
            add_batch: AddBatchDialog::default(),
            options_open: false,
            options_saved: false,
            about_open: false,
            dark_mode: false,
        }
    }

    fn refresh(&mut self) {
        if self.last_refresh.elapsed() < Duration::from_millis(400) {
            return;
        }
        self.last_refresh = Instant::now();
        let mut jobs = self.queue.list();
        jobs.sort_by_key(|j| j.created_at);

        let now = Instant::now();
        for job in &jobs {
            let entry = self.speeds.entry(job.id).or_insert(SpeedTrack { bytes: job.written, at: now, bps: 0.0 });
            let dt = now.duration_since(entry.at).as_secs_f64();
            if job.state.is_active() && dt > 0.0 {
                let instant = job.written.saturating_sub(entry.bytes) as f64 / dt;
                entry.bps = if entry.bps == 0.0 { instant } else { entry.bps * 0.7 + instant * 0.3 };
            } else if !job.state.is_active() {
                entry.bps = 0.0;
            }
            entry.bytes = job.written;
            entry.at = now;

            let prev = self.seen_states.insert(job.id, job.state);
            if job.state == DownloadState::Completed && prev.is_some_and(|p| p != DownloadState::Completed) {
                self.finished_popups.push(job.id);
            }
        }
        self.jobs = jobs;
    }

    fn speed(&self, id: DownloadId) -> f64 {
        self.speeds.get(&id).map(|s| s.bps).unwrap_or(0.0)
    }

    fn job(&self, id: DownloadId) -> Option<&DownloadJob> {
        self.jobs.iter().find(|j| j.id == id)
    }

    fn after_change(&mut self) {
        self.queue.pump();
        self.last_refresh = Instant::now() - Duration::from_secs(1);
        self.refresh();
    }

    // --- Actions ---

    fn open_add_url(&mut self) {
        self.add_url = AddUrlDialog { open: true, folder: self.config.download_dir.display().to_string(), ..Default::default() };
        if let Some(url) = clipboard_url() {
            self.add_url.url = url;
        }
    }

    fn open_add_video(&mut self) {
        self.add_video = AddVideoDialog { open: true, folder: self.config.download_dir.display().to_string(), ..Default::default() };
        if let Some(url) = clipboard_url() {
            self.add_video.url = url;
        }
    }

    fn submit_url(&mut self) {
        let url = self.add_url.url.trim().to_string();
        if url.is_empty() {
            return;
        }
        let name = if self.add_url.file_name.trim().is_empty() { filename_from_url(&url) } else { self.add_url.file_name.trim().to_string() };
        let folder = PathBuf::from(self.add_url.folder.trim());
        let job = DownloadJob {
            origin: DownloadOrigin::Direct { url },
            destination: self.unique_destination(&folder, &name),
            filename: None,
            referer: non_empty(&self.add_url.referer),
            user_agent: non_empty(&self.add_url.user_agent),
            ..new_job()
        };
        self.queue.add(job);
        self.add_url.open = false;
        self.after_change();
    }

    fn submit_video(&mut self) {
        let url = self.add_video.url.trim().to_string();
        if url.is_empty() {
            return;
        }
        let format = VIDEO_QUALITIES[self.add_video.quality].1.map(str::to_string);
        let job = DownloadJob {
            origin: DownloadOrigin::YtDlp { url, format },
            destination: PathBuf::from(self.add_video.folder.trim()),
            ..new_job()
        };
        self.queue.add(job);
        self.add_video.open = false;
        self.after_change();
    }

    fn submit_stream(&mut self) {
        let url = self.add_stream.url.trim().to_string();
        if url.is_empty() {
            return;
        }
        let mut name = if self.add_stream.file_name.trim().is_empty() {
            filename_from_url(&url).trim_end_matches(".m3u8").trim_end_matches(".mpd").to_string()
        } else {
            self.add_stream.file_name.trim().to_string()
        };
        if !name.to_lowercase().ends_with(".mp4") {
            name.push_str(".mp4");
        }
        let job = DownloadJob {
            origin: DownloadOrigin::Hls { url, page_url: String::new() },
            destination: self.unique_destination(&self.config.download_dir.clone(), &name),
            ..new_job()
        };
        self.queue.add(job);
        self.add_stream.open = false;
        self.after_change();
    }

    fn submit_batch(&mut self) {
        let template = self.add_batch.template.trim().to_string();
        if template.is_empty() {
            return;
        }
        let job = DownloadJob {
            origin: DownloadOrigin::Direct { url: template.clone() },
            destination: self.config.download_dir.join("batch"),
            sequence: Some(zavyx_dm_core::SequenceSpec::Range {
                template,
                start: self.add_batch.start,
                end: self.add_batch.end.max(self.add_batch.start),
                step: self.add_batch.step.max(1),
            }),
            ..new_job()
        };
        self.queue.add(job);
        self.add_batch.open = false;
        self.after_change();
    }

    /// `folder/name`, adding " (1)", " (2)"... if the file or another job already uses it.
    fn unique_destination(&self, folder: &Path, name: &str) -> PathBuf {
        let (stem, ext) = match name.rfind('.') {
            Some(i) if i > 0 => (&name[..i], &name[i..]),
            _ => (name, ""),
        };
        let taken = |p: &Path| p.exists() || self.jobs.iter().any(|j| j.destination == p);
        let mut candidate = folder.join(name);
        let mut n = 1;
        while taken(&candidate) {
            candidate = folder.join(format!("{} ({}){}", stem, n, ext));
            n += 1;
        }
        candidate
    }

    fn resume(&mut self, id: DownloadId) {
        self.queue.resume(id);
        self.after_change();
    }

    fn stop(&mut self, id: DownloadId) {
        self.queue.pause(id);
        self.after_change();
    }

    fn stop_all(&mut self) {
        for j in self.jobs.clone() {
            if j.state.is_active() || matches!(j.state, DownloadState::Queued | DownloadState::Pending) {
                self.queue.pause(j.id);
            }
        }
        self.after_change();
    }

    fn resume_all(&mut self) {
        for j in self.jobs.clone() {
            if matches!(j.state, DownloadState::Paused | DownloadState::Error | DownloadState::Failed) {
                self.queue.resume(j.id);
            }
        }
        self.after_change();
    }

    fn delete(&mut self, id: DownloadId) {
        self.queue.remove(id);
        if self.selected == Some(id) {
            self.selected = None;
        }
        self.after_change();
        save_jobs(&self.jobs);
    }

    fn delete_completed(&mut self) {
        for j in self.jobs.clone() {
            if j.state == DownloadState::Completed {
                self.queue.remove(j.id);
            }
        }
        self.after_change();
        save_jobs(&self.jobs);
    }
}

impl eframe::App for ZavyxDmApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.refresh();
        ctx.request_repaint_after(Duration::from_millis(500));

        if ctx.input(|i| i.key_pressed(egui::Key::Delete)) && !ctx.wants_keyboard_input() {
            if let Some(id) = self.selected {
                self.delete(id);
            }
        }

        self.menu_bar(ctx);
        self.toolbar(ctx);
        self.status_bar(ctx);
        self.sidebar(ctx);
        egui::CentralPanel::default().show(ctx, |ui| self.downloads_table(ui));

        self.add_url_window(ctx);
        self.add_video_window(ctx);
        self.add_stream_window(ctx);
        self.add_batch_window(ctx);
        self.options_window(ctx);
        self.about_window(ctx);
        self.finished_window(ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        save_jobs(&self.queue.list());
    }
}

// --- Panels ---

impl ZavyxDmApp {
    fn menu_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("menu").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("Tasks", |ui| {
                    if ui.button("➕ Add URL…").clicked() {
                        self.open_add_url();
                        ui.close_menu();
                    }
                    if ui.button("🎬 Add video (YouTube, etc.)…").clicked() {
                        self.open_add_video();
                        ui.close_menu();
                    }
                    if ui.button("📡 Add stream (m3u8 / mpd)…").clicked() {
                        self.add_stream = AddStreamDialog { open: true, ..Default::default() };
                        ui.close_menu();
                    }
                    if ui.button("📑 Add batch download…").clicked() {
                        self.add_batch = AddBatchDialog { open: true, ..Default::default() };
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Exit").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                ui.menu_button("Downloads", |ui| {
                    if ui.button("▶ Resume all").clicked() {
                        self.resume_all();
                        ui.close_menu();
                    }
                    if ui.button("⏹ Stop all").clicked() {
                        self.stop_all();
                        ui.close_menu();
                    }
                    if ui.button("✔ Delete completed").clicked() {
                        self.delete_completed();
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("📂 Open download folder").clicked() {
                        open_path(&self.config.download_dir);
                        ui.close_menu();
                    }
                });
                ui.menu_button("View", |ui| {
                    if ui.checkbox(&mut self.dark_mode, "Dark mode").changed() {
                        apply_theme(ctx, self.dark_mode);
                        ui.close_menu();
                    }
                });
                ui.menu_button("Help", |ui| {
                    if ui.button("🔄 Check for updates").clicked() {
                        ctx.open_url(egui::OpenUrl::new_tab(RELEASES_URL));
                        ui.close_menu();
                    }
                    if ui.button("🐙 Source code on GitHub").clicked() {
                        ctx.open_url(egui::OpenUrl::new_tab(REPO_URL));
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("ℹ About Zavyx DM").clicked() {
                        self.about_open = true;
                        ui.close_menu();
                    }
                });
            });
        });
    }

    fn toolbar(&mut self, ctx: &egui::Context) {
        let sel = self.selected.and_then(|id| self.job(id)).map(|j| (j.id, j.state));
        let can_resume = sel.is_some_and(|(_, s)| matches!(s, DownloadState::Paused | DownloadState::Error | DownloadState::Failed));
        let can_stop = sel.is_some_and(|(_, s)| s.is_active() || matches!(s, DownloadState::Queued | DownloadState::Pending));

        egui::TopBottomPanel::top("toolbar")
            .frame(egui::Frame::side_top_panel(&ctx.style()).inner_margin(egui::Margin::symmetric(10.0, 6.0)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(Vec2::splat(40.0), egui::Sense::hover());
                    paint_logo(ui.painter(), rect);
                    ui.vertical(|ui| {
                        ui.add_space(2.0);
                        ui.label(RichText::new("Zavyx DM").size(20.0).strong().color(ACCENT));
                        ui.label(RichText::new("Download Manager").size(11.0).weak());
                    });
                    ui.add_space(12.0);
                    ui.separator();

                    if tool_button(ui, "➕", "Add URL", true).clicked() {
                        self.open_add_url();
                    }
                    if tool_button(ui, "🎬", "Video", true).clicked() {
                        self.open_add_video();
                    }
                    ui.separator();
                    if tool_button(ui, "▶", "Resume", can_resume).clicked() {
                        if let Some((id, _)) = sel {
                            self.resume(id);
                        }
                    }
                    if tool_button(ui, "⏸", "Stop", can_stop).clicked() {
                        if let Some((id, _)) = sel {
                            self.stop(id);
                        }
                    }
                    if tool_button(ui, "⏹", "Stop All", true).clicked() {
                        self.stop_all();
                    }
                    if tool_button(ui, "🗑", "Delete", sel.is_some()).on_hover_text("Remove from list (the file stays on disk)").clicked() {
                        if let Some((id, _)) = sel {
                            self.delete(id);
                        }
                    }
                    if tool_button(ui, "✔", "Clear Done", true).on_hover_text("Remove all finished downloads from the list").clicked() {
                        self.delete_completed();
                    }
                    ui.separator();
                    if tool_button(ui, "⚙", "Options", true).clicked() {
                        self.config = EngineConfig::load_or_create().unwrap_or_else(|_| self.config.clone());
                        self.options_saved = false;
                        self.options_open = true;
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.search).hint_text("🔍 Search downloads").desired_width(180.0));
                    });
                });
            });
    }

    fn sidebar(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("categories").resizable(false).exact_width(180.0).show(ctx, |ui| {
            ui.add_space(8.0);
            ui.label(RichText::new("CATEGORIES").size(11.0).weak().strong());
            ui.add_space(4.0);
            for c in Category::TYPES {
                self.category_entry(ui, c);
            }
            ui.add_space(10.0);
            ui.label(RichText::new("STATUS").size(11.0).weak().strong());
            ui.add_space(4.0);
            self.category_entry(ui, Category::Unfinished);
            self.category_entry(ui, Category::Finished);
        });
    }

    fn category_entry(&mut self, ui: &mut egui::Ui, c: Category) {
        let count = self.jobs.iter().filter(|j| c.matches(j)).count();
        let text = format!("{}  ({})", c.label(), count);
        let selected = self.category == c;
        let resp = ui
            .with_layout(Layout::top_down_justified(Align::LEFT), |ui| {
                ui.add(egui::SelectableLabel::new(selected, RichText::new(text).size(14.0)))
            })
            .inner;
        if resp.clicked() {
            self.category = c;
        }
    }

    fn status_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            let active = self.jobs.iter().filter(|j| j.state.is_active()).count();
            let done = self.jobs.iter().filter(|j| j.state == DownloadState::Completed).count();
            let total_speed: f64 = self.jobs.iter().filter(|j| j.state.is_active()).map(|j| self.speed(j.id)).sum();
            ui.horizontal(|ui| {
                ui.label(format!("{} downloads", self.jobs.len()));
                ui.separator();
                ui.label(format!("{} active", active));
                ui.separator();
                ui.label(format!("{} finished", done));
                ui.separator();
                ui.label(format!("⬇ {}", format_speed(total_speed)));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.hyperlink_to("GitHub", GITHUB_PROFILE);
                    ui.label(RichText::new(format!("Zavyx DM v{} · by {}", VERSION, AUTHOR)).weak());
                });
            });
        });
    }

    fn downloads_table(&mut self, ui: &mut egui::Ui) {
        let search = self.search.to_lowercase();
        let rows: Vec<DownloadJob> = self
            .jobs
            .iter()
            .filter(|j| self.category.matches(j))
            .filter(|j| search.is_empty() || display_name(j).to_lowercase().contains(&search) || j.origin.url().to_lowercase().contains(&search))
            .cloned()
            .collect();

        if rows.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() / 3.0);
                    ui.label(RichText::new("No downloads here yet").size(18.0).weak());
                    ui.add_space(6.0);
                    ui.label(RichText::new("Click “Add URL”, or use the Zavyx DM button in your browser.").weak());
                });
            });
            return;
        }

        enum RowAction {
            Select(DownloadId),
            Open(DownloadId),
            OpenFolder(DownloadId),
            Resume(DownloadId),
            Stop(DownloadId),
            Delete(DownloadId),
            CopyUrl(String),
        }
        let mut actions = Vec::new();

        TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .sense(egui::Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::remainder().at_least(160.0).clip(true))
            .column(Column::exact(85.0))
            .column(Column::exact(150.0))
            .column(Column::exact(75.0))
            .column(Column::exact(90.0))
            .column(Column::exact(110.0))
            .header(26.0, |mut h| {
                for title in ["File Name", "Size", "Status", "Time Left", "Speed", "Last Try"] {
                    h.col(|ui| {
                        ui.strong(title);
                    });
                }
            })
            .body(|body| {
                body.rows(28.0, rows.len(), |mut row| {
                    let job = &rows[row.index()];
                    let speed = self.speed(job.id);
                    row.set_selected(self.selected == Some(job.id));

                    row.col(|ui| {
                        ui.label(file_icon(job));
                        ui.label(display_name(job)).on_hover_text(job.origin.url());
                    });
                    row.col(|ui| {
                        ui.label(job.total_size.map(human_bytes).unwrap_or_else(|| if job.written > 0 { human_bytes(job.written) } else { "—".into() }));
                    });
                    row.col(|ui| status_cell(ui, job));
                    row.col(|ui| {
                        let left = job.total_size.map(|t| t.saturating_sub(job.written)).unwrap_or(0);
                        ui.label(if job.state.is_active() && left > 0 { format_eta(speed, left) } else { String::new() });
                    });
                    row.col(|ui| {
                        ui.label(if job.state.is_active() { format_speed(speed) } else { String::new() });
                    });
                    row.col(|ui| {
                        let when = job.started_at.unwrap_or(job.created_at).with_timezone(&Local);
                        ui.label(when.format("%b %d, %H:%M").to_string());
                    });

                    let resp = row.response();
                    if resp.clicked() {
                        actions.push(RowAction::Select(job.id));
                    }
                    if resp.double_clicked() {
                        actions.push(if job.state == DownloadState::Completed { RowAction::Open(job.id) } else { RowAction::Resume(job.id) });
                    }
                    resp.context_menu(|ui| {
                        actions.push(RowAction::Select(job.id));
                        let done = job.state == DownloadState::Completed;
                        if ui.add_enabled(done, egui::Button::new("📄 Open")).clicked() {
                            actions.push(RowAction::Open(job.id));
                            ui.close_menu();
                        }
                        if ui.button("📂 Open folder").clicked() {
                            actions.push(RowAction::OpenFolder(job.id));
                            ui.close_menu();
                        }
                        ui.separator();
                        let resumable = matches!(job.state, DownloadState::Paused | DownloadState::Error | DownloadState::Failed);
                        if ui.add_enabled(resumable, egui::Button::new("▶ Resume")).clicked() {
                            actions.push(RowAction::Resume(job.id));
                            ui.close_menu();
                        }
                        if ui.add_enabled(job.state.is_active() || job.state == DownloadState::Queued, egui::Button::new("⏸ Stop")).clicked() {
                            actions.push(RowAction::Stop(job.id));
                            ui.close_menu();
                        }
                        if ui.button("🔗 Copy address").clicked() {
                            actions.push(RowAction::CopyUrl(job.origin.url().to_string()));
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("🗑 Delete from list").clicked() {
                            actions.push(RowAction::Delete(job.id));
                            ui.close_menu();
                        }
                    });
                });
            });

        for a in actions {
            match a {
                RowAction::Select(id) => self.selected = Some(id),
                RowAction::Open(id) => {
                    if let Some(j) = self.job(id) {
                        open_path(&j.destination);
                    }
                }
                RowAction::OpenFolder(id) => {
                    if let Some(j) = self.job(id) {
                        reveal_path(&j.destination);
                    }
                }
                RowAction::Resume(id) => self.resume(id),
                RowAction::Stop(id) => self.stop(id),
                RowAction::Delete(id) => self.delete(id),
                RowAction::CopyUrl(url) => ui.ctx().copy_text(url),
            }
        }
    }
}

// --- Dialog windows ---

impl ZavyxDmApp {
    fn add_url_window(&mut self, ctx: &egui::Context) {
        if !self.add_url.open {
            return;
        }
        let mut open = true;
        let mut submit = false;
        let mut cancel = false;
        dialog("Add new download", &mut open).show(ctx, |ui| {
            egui::Grid::new("add-url").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                ui.label("Address:");
                let r = ui.add(egui::TextEdit::singleline(&mut self.add_url.url).hint_text("https://example.com/file.zip").desired_width(380.0));
                if r.changed() && !self.add_url.name_edited {
                    let url = self.add_url.url.trim();
                    self.add_url.file_name = if url.is_empty() { String::new() } else { filename_from_url(url) };
                }
                if self.add_url.url.is_empty() {
                    r.request_focus();
                }
                ui.end_row();

                ui.label("Save as:");
                if ui.add(egui::TextEdit::singleline(&mut self.add_url.file_name).desired_width(380.0)).changed() {
                    self.add_url.name_edited = true;
                }
                ui.end_row();

                ui.label("Save to:");
                folder_picker(ui, &mut self.add_url.folder);
                ui.end_row();
            });
            ui.add_space(4.0);
            egui::CollapsingHeader::new("Advanced").show(ui, |ui| {
                egui::Grid::new("add-url-adv").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                    ui.label("Referer:");
                    ui.add(egui::TextEdit::singleline(&mut self.add_url.referer).desired_width(320.0));
                    ui.end_row();
                    ui.label("User-Agent:");
                    ui.add(egui::TextEdit::singleline(&mut self.add_url.user_agent).desired_width(320.0));
                    ui.end_row();
                });
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if primary_button(ui, "⬇  Start Download").clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    submit = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
        });
        if submit {
            self.submit_url();
        }
        if !open || cancel {
            self.add_url.open = false;
        }
    }

    fn add_video_window(&mut self, ctx: &egui::Context) {
        if !self.add_video.open {
            return;
        }
        let mut open = true;
        let mut submit = false;
        let mut cancel = false;
        dialog("Download video", &mut open).show(ctx, |ui| {
            ui.label("Works with YouTube, Facebook, Instagram, TikTok, X/Twitter, Vimeo and 1000+ other sites.");
            ui.add_space(6.0);
            egui::Grid::new("add-video").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                ui.label("Video page:");
                ui.add(egui::TextEdit::singleline(&mut self.add_video.url).hint_text("https://www.youtube.com/watch?v=…").desired_width(380.0));
                ui.end_row();
                ui.label("Quality:");
                egui::ComboBox::from_id_salt("quality")
                    .selected_text(VIDEO_QUALITIES[self.add_video.quality].0)
                    .show_ui(ui, |ui| {
                        for (i, (label, _)) in VIDEO_QUALITIES.iter().enumerate() {
                            ui.selectable_value(&mut self.add_video.quality, i, *label);
                        }
                    });
                ui.end_row();
                ui.label("Save to:");
                folder_picker(ui, &mut self.add_video.folder);
                ui.end_row();
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if primary_button(ui, "⬇  Download Video").clicked() {
                    submit = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
        });
        if submit {
            self.submit_video();
        }
        if !open || cancel {
            self.add_video.open = false;
        }
    }

    fn add_stream_window(&mut self, ctx: &egui::Context) {
        if !self.add_stream.open {
            return;
        }
        let mut open = true;
        let mut submit = false;
        let mut cancel = false;
        dialog("Download stream", &mut open).show(ctx, |ui| {
            ui.label("Paste an HLS (.m3u8) or DASH (.mpd) link. It is saved as an .mp4 file.");
            ui.add_space(6.0);
            egui::Grid::new("add-stream").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                ui.label("Stream link:");
                ui.add(egui::TextEdit::singleline(&mut self.add_stream.url).desired_width(380.0));
                ui.end_row();
                ui.label("File name:");
                ui.add(egui::TextEdit::singleline(&mut self.add_stream.file_name).hint_text("optional").desired_width(380.0));
                ui.end_row();
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if primary_button(ui, "⬇  Download").clicked() {
                    submit = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
        });
        if submit {
            self.submit_stream();
        }
        if !open || cancel {
            self.add_stream.open = false;
        }
    }

    fn add_batch_window(&mut self, ctx: &egui::Context) {
        if !self.add_batch.open {
            return;
        }
        let mut open = true;
        let mut submit = false;
        let mut cancel = false;
        dialog("Batch download", &mut open).show(ctx, |ui| {
            ui.label("Download a numbered series of files. Put {i} where the number goes, or {i:02} for 01, 02, …");
            ui.add_space(6.0);
            egui::Grid::new("add-batch").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                ui.label("Address:");
                ui.add(egui::TextEdit::singleline(&mut self.add_batch.template).hint_text("https://example.com/photo_{i:02}.jpg").desired_width(380.0));
                ui.end_row();
                ui.label("Numbers:");
                ui.horizontal(|ui| {
                    ui.label("from");
                    ui.add(egui::DragValue::new(&mut self.add_batch.start));
                    ui.label("to");
                    ui.add(egui::DragValue::new(&mut self.add_batch.end));
                    ui.label("step");
                    ui.add(egui::DragValue::new(&mut self.add_batch.step).range(1..=1000));
                });
                ui.end_row();
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if primary_button(ui, "⬇  Add All").clicked() {
                    submit = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
        });
        if submit {
            self.submit_batch();
        }
        if !open || cancel {
            self.add_batch.open = false;
        }
    }

    fn options_window(&mut self, ctx: &egui::Context) {
        if !self.options_open {
            return;
        }
        let mut open = true;
        dialog("Options", &mut open).min_width(480.0).show(ctx, |ui| {
            ui.heading("General");
            egui::Grid::new("opt-general").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                ui.label("Download folder:");
                let mut folder = self.config.download_dir.display().to_string();
                if folder_picker(ui, &mut folder) {
                    self.config.download_dir = PathBuf::from(folder);
                }
                ui.end_row();
            });
            ui.add_space(8.0);
            ui.heading("Connection");
            egui::Grid::new("opt-conn").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                ui.label("Downloads at the same time:");
                ui.add(egui::DragValue::new(&mut self.config.max_concurrent_downloads).range(1..=32));
                ui.end_row();
                ui.label("Connections per download:");
                ui.add(egui::DragValue::new(&mut self.config.max_segments).range(1..=32));
                ui.end_row();
                ui.label("Retries on error:");
                ui.add(egui::DragValue::new(&mut self.config.max_retries).range(0..=20));
                ui.end_row();
            });
            ui.add_space(8.0);
            ui.heading("Video");
            ui.checkbox(&mut self.config.auto_ytdlp, "Use yt-dlp for YouTube and other video sites");
            ui.checkbox(&mut self.config.auto_convert_streams, "Convert streams (m3u8 / mpd) to mp4");
            egui::Grid::new("opt-tools").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                ui.label("yt-dlp:");
                tool_path_field(ui, &mut self.config.ytdlp_path);
                ui.end_row();
                ui.label("ffmpeg:");
                tool_path_field(ui, &mut self.config.ffmpeg_path);
                ui.end_row();
            });
            ui.add_space(8.0);
            ui.heading("Browser integration");
            ui.label("Install the Zavyx DM extension in Chrome, Edge, Brave, Opera or Firefox to send downloads and videos here with one click.");
            ui.hyperlink_to("How to install the extension", format!("{}#browser-extension", REPO_URL));

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if primary_button(ui, "💾  Save").clicked() {
                    self.options_saved = self.config.save().is_ok();
                }
                if ui.button("Reset to defaults").clicked() {
                    self.config = EngineConfig::default();
                }
                if self.options_saved {
                    ui.label(RichText::new("Saved. Restart Zavyx DM to apply connection settings.").color(OK_GREEN));
                }
            });
        });
        if !open {
            self.options_open = false;
        }
    }

    fn about_window(&mut self, ctx: &egui::Context) {
        if !self.about_open {
            return;
        }
        let mut open = true;
        dialog("About Zavyx DM", &mut open).show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(72.0), egui::Sense::hover());
                paint_logo(ui.painter(), rect);
                ui.add_space(6.0);
                ui.label(RichText::new(APP_NAME).size(20.0).strong());
                ui.label(format!("Version {}", VERSION));
                ui.add_space(6.0);
                ui.label("Free and open-source download manager for Windows.");
                ui.label(format!("Made by {}", AUTHOR));
                ui.hyperlink_to("github.com/mohammadibrahimdev", GITHUB_PROFILE);
                ui.add_space(6.0);
                ui.hyperlink_to("Source code & releases", REPO_URL);
                ui.label(RichText::new("MIT License").weak());
            });
        });
        if !open {
            self.about_open = false;
        }
    }

    fn finished_window(&mut self, ctx: &egui::Context) {
        let Some(&id) = self.finished_popups.first() else {
            return;
        };
        let Some(job) = self.job(id).cloned() else {
            self.finished_popups.remove(0);
            return;
        };
        let mut open = true;
        let mut close = false;
        dialog("Download complete", &mut open).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("✅").size(28.0).color(OK_GREEN));
                ui.vertical(|ui| {
                    ui.label(RichText::new(display_name(&job)).strong());
                    if let Some(t) = job.total_size {
                        ui.label(human_bytes(t));
                    }
                    ui.label(RichText::new(job.destination.display().to_string()).weak().size(11.0));
                });
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if primary_button(ui, "Open").clicked() {
                    open_path(&job.destination);
                    close = true;
                }
                if ui.button("Open folder").clicked() {
                    reveal_path(&job.destination);
                    close = true;
                }
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
        });
        if !open || close {
            self.finished_popups.remove(0);
        }
    }
}

// --- Widgets & helpers ---

fn new_job() -> DownloadJob {
    DownloadJob { id: zavyx_dm_core::DownloadId::new_v4(), created_at: chrono::Utc::now(), ..Default::default() }
}

fn non_empty(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

fn clipboard_url() -> Option<String> {
    let text = arboard::Clipboard::new().ok()?.get_text().ok()?;
    let text = text.trim();
    (text.starts_with("http://") || text.starts_with("https://")).then(|| text.to_string())
}

fn dialog<'a>(title: &'a str, open: &'a mut bool) -> egui::Window<'a> {
    egui::Window::new(title)
        .open(open)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
}

/// Text field plus "Browse…" button. Returns true when the value changed.
fn folder_picker(ui: &mut egui::Ui, folder: &mut String) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        changed |= ui.add(egui::TextEdit::singleline(folder).desired_width(300.0)).changed();
        if ui.button("📂 Browse…").clicked() {
            if let Some(dir) = rfd::FileDialog::new().set_directory(&*folder).pick_folder() {
                *folder = dir.display().to_string();
                changed = true;
            }
        }
    });
    changed
}

fn tool_path_field(ui: &mut egui::Ui, path: &mut PathBuf) {
    let mut text = path.display().to_string();
    ui.horizontal(|ui| {
        if ui.add(egui::TextEdit::singleline(&mut text).desired_width(300.0)).changed() {
            *path = PathBuf::from(&text);
        }
        if ui.button("Browse…").clicked() {
            if let Some(file) = rfd::FileDialog::new().add_filter("Program", &["exe"]).pick_file() {
                *path = file;
            }
        }
    });
}

/// Large icon-over-label toolbar button.
fn tool_button(ui: &mut egui::Ui, icon: &str, label: &str, enabled: bool) -> egui::Response {
    let size = Vec2::new(66.0, 50.0);
    let sense = if enabled { egui::Sense::click() } else { egui::Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    let visuals = ui.visuals();
    let fg = if enabled { visuals.text_color() } else { visuals.weak_text_color().gamma_multiply(0.6) };
    if enabled && resp.hovered() {
        ui.painter().rect_filled(rect, 6.0, ACCENT.gamma_multiply(0.15));
    }
    let icon_color = if enabled { ACCENT } else { fg };
    ui.painter().text(rect.center_top() + Vec2::new(0.0, 16.0), egui::Align2::CENTER_CENTER, icon, egui::FontId::proportional(22.0), icon_color);
    ui.painter().text(rect.center_bottom() - Vec2::new(0.0, 9.0), egui::Align2::CENTER_CENTER, label, egui::FontId::proportional(12.0), fg);
    if enabled {
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        resp
    }
}

fn primary_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(egui::Button::new(RichText::new(text).color(Color32::WHITE).strong()).fill(ACCENT).min_size(Vec2::new(130.0, 28.0)))
}

fn status_cell(ui: &mut egui::Ui, job: &DownloadJob) {
    let pct = job.percent().unwrap_or(0.0) as f32 / 100.0;
    match job.state {
        DownloadState::Completed => {
            ui.label(RichText::new("✔ Complete").color(OK_GREEN));
        }
        DownloadState::Failed | DownloadState::Error => {
            let r = ui.label(RichText::new("⚠ Error").color(ERR_RED));
            if let Some(e) = &job.last_error {
                r.on_hover_text(e);
            }
        }
        DownloadState::Cancelled => {
            ui.label(RichText::new("Stopped").weak());
        }
        DownloadState::Paused => {
            ui.add(egui::ProgressBar::new(pct).desired_width(80.0).desired_height(8.0).rounding(3.0).fill(Color32::from_gray(150)));
            ui.label(RichText::new(format!("Paused {:.0}%", pct * 100.0)).weak());
        }
        DownloadState::Queued | DownloadState::Pending => {
            ui.label(RichText::new("Queued").weak());
        }
        DownloadState::Preparing => {
            ui.label("Connecting…");
        }
        DownloadState::Merging => {
            ui.label("Finishing…");
        }
        DownloadState::Downloading | DownloadState::SingleStream => {
            if job.total_size.is_some() {
                ui.add(egui::ProgressBar::new(pct).desired_width(80.0).desired_height(8.0).rounding(3.0).fill(ACCENT));
                ui.label(format!("{:.1}%", pct * 100.0));
            } else {
                ui.add(egui::ProgressBar::new(0.0).desired_width(80.0).desired_height(8.0).rounding(3.0).animate(true).fill(ACCENT));
                ui.label("Downloading");
            }
        }
    }
}

fn display_name(job: &DownloadJob) -> String {
    if let Some(name) = &job.filename {
        return name.clone();
    }
    match &job.origin {
        DownloadOrigin::YtDlp { url, .. } => format!("Video: {}", url),
        _ => job
            .destination
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| job.origin.url().to_string()),
    }
}

fn file_icon(job: &DownloadJob) -> &'static str {
    match file_category(job) {
        Category::Compressed => "📁",
        Category::Documents => "📄",
        Category::Music => "🎵",
        Category::Programs => "💾",
        Category::Video => "🎬",
        _ => "📦",
    }
}

/// Draw the Zavyx DM "Z" logo in `rect`.
fn paint_logo(painter: &egui::Painter, rect: egui::Rect) {
    let k = rect.width() / 256.0;
    let p = |x: f32, y: f32| rect.min + Vec2::new(x * k, y * k);
    painter.rect_filled(rect, 56.0 * k, ACCENT);
    painter.rect_filled(egui::Rect::from_min_max(p(0.0, 128.0), p(256.0, 256.0)), egui::Rounding { nw: 0.0, ne: 0.0, sw: 56.0 * k, se: 56.0 * k }, ACCENT_DARK.gamma_multiply(0.5));
    let white = Color32::WHITE;
    painter.rect_filled(egui::Rect::from_min_max(p(64.0, 64.0), p(192.0, 98.0)), 0.0, white);
    painter.rect_filled(egui::Rect::from_min_max(p(64.0, 160.0), p(192.0, 194.0)), 0.0, white);
    painter.add(egui::Shape::convex_polygon(vec![p(146.0, 98.0), p(192.0, 98.0), p(110.0, 160.0), p(64.0, 160.0)], white, egui::Stroke::NONE));
}

fn apply_theme(ctx: &egui::Context, dark: bool) {
    let mut v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.selection.bg_fill = ACCENT.gamma_multiply(if dark { 0.6 } else { 0.25 });
    v.selection.stroke = egui::Stroke::new(1.0_f32, ACCENT);
    v.hyperlink_color = ACCENT;
    v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, ACCENT);
    if !dark {
        v.panel_fill = Color32::from_rgb(247, 247, 251);
        v.extreme_bg_color = Color32::WHITE;
        v.faint_bg_color = Color32::from_rgb(242, 240, 255);
    }
    ctx.set_visuals(v);
    ctx.style_mut(|s| {
        s.spacing.item_spacing = Vec2::new(8.0, 5.0);
        s.spacing.button_padding = Vec2::new(10.0, 4.0);
    });
}

/// Open a file (or folder) with its default program.
fn open_path(path: &Path) {
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer").arg(path).spawn();
    #[cfg(not(windows))]
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
}

/// Show a file selected in its folder (or open the folder itself).
fn reveal_path(path: &Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        if path.is_file() {
            let _ = std::process::Command::new("explorer").raw_arg(format!("/select,\"{}\"", path.display())).spawn();
            return;
        }
    }
    let dir = if path.is_dir() { path } else { path.parent().unwrap_or(path) };
    open_path(dir);
}

fn human_bytes(n: u64) -> String {
    if n < 1024 {
        return format!("{} B", n);
    }
    let n = n as f64;
    let (v, u) = if n < 1024.0 * 1024.0 {
        (n / 1024.0, "KB")
    } else if n < 1024.0 * 1024.0 * 1024.0 {
        (n / (1024.0 * 1024.0), "MB")
    } else {
        (n / (1024.0 * 1024.0 * 1024.0), "GB")
    };
    format!("{:.1} {}", v, u)
}

fn format_speed(bps: f64) -> String {
    if bps > 1.0 {
        format!("{}/s", human_bytes(bps as u64))
    } else {
        "—".to_string()
    }
}

fn format_eta(speed: f64, remaining: u64) -> String {
    if speed <= 1.0 {
        return "—".to_string();
    }
    let secs = (remaining as f64 / speed) as u64;
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{}h {}m", h, m)
    } else if m > 0 {
        format!("{}m {}s", m, s)
    } else {
        format!("{}s", s)
    }
}
