//! Standalone Native Messaging host binary.
//!
//! Used by the browser extension to launch the host without depending on the GUI.
//! The browser launches this binary directly with stdin/stdout pipes.

use zavyx_core::{EngineConfig, Queue, ProgressCallback};
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive("zavyx=info".parse()?))
        // stdout carries the browser protocol; any log line there corrupts it.
        .with_writer(std::io::stderr)
        .init();

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

    let mut host = zavyx_native_messaging::Host::new();
    zavyx_native_messaging::run_loop(&mut host, queue).await?;
    Ok(())
}
