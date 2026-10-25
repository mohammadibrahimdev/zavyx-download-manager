//! Standalone Native Messaging host binary.
//!
//! Used by the browser extension to launch the host without depending on the GUI.
//! The browser launches this binary directly with stdin/stdout pipes.
//!
//! Every start and exit is appended to `host.log` in the Zavyx DM data folder, so a
//! broken browser connection can be diagnosed without a debugger.

use std::io::Write;
use std::sync::Arc;
use zavyx_dm_core::{EngineConfig, ProgressCallback, Queue};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive("zavyx=info".parse()?))
        // stdout carries the browser protocol; any log line there corrupts it.
        .with_writer(std::io::stderr)
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    log_line(&format!("start pid={} args={:?}", std::process::id(), args));
    let result = run().await;
    match &result {
        Ok(()) => log_line("exit: browser closed the connection"),
        Err(e) => log_line(&format!("exit with error: {:#}", e)),
    }
    result
}

async fn run() -> anyhow::Result<()> {
    let cfg = EngineConfig::load_or_create()?;
    let progress: ProgressCallback = Arc::new(|id, snap| {
        tracing::debug!(?id, state = ?snap.state, written = snap.written, "progress");
    });
    let queue = Arc::new(Queue::new(cfg, progress)?);

    // Pump loop in a background task.
    let q2 = queue.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(500));
        loop {
            interval.tick().await;
            q2.pump();
        }
    });

    let mut host = zavyx_dm_native_messaging::Host::new();
    zavyx_dm_native_messaging::run_loop(&mut host, queue).await
}

/// Append a timestamped line to host.log, keeping the file small.
fn log_line(msg: &str) {
    let Ok(dir) = EngineConfig::state_dir() else { return };
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("host.log");
    if std::fs::metadata(&path).map(|m| m.len() > 256 * 1024).unwrap_or(false) {
        let _ = std::fs::remove_file(&path);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{} {}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"), msg);
    }
}
