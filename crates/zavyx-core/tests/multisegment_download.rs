//! Integration test: actually download a file using the multi-segment engine.
//!
//! Spins up a tiny HTTP server in-process that supports range requests,
//! then downloads through the queue and verifies the bytes match.

use zavyx_core::{DownloadJob, DownloadOrigin, EngineConfig, Queue, DownloadState};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn end_to_end_multi_segment_download() {
    // Create a 1 MB random-ish payload.
    let payload: Vec<u8> = (0..1_000_000).map(|i| (i % 256) as u8).collect();

    // Spin up a tiny HTTP server that supports Range.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let payload_arc = Arc::new(payload.clone());
    let server = tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(c) => c,
                Err(_) => break,
            };
            let payload = payload_arc.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 8192];
                let _ = sock.read(&mut buf).await;
                let req = String::from_utf8_lossy(&buf);
                // Look for a Range: bytes=A-B header.
                let range_start_end = req
                    .lines()
                    .find(|l| l.to_lowercase().starts_with("range: bytes="))
                    .and_then(|l| {
                        let s = l.split('=').nth(1)?.trim();
                        let mut parts = s.splitn(2, '-');
                        let start: u64 = parts.next()?.parse().ok()?;
                        let end_str = parts.next()?.trim();
                        let end: u64 = if end_str.is_empty() {
                            payload.len() as u64 - 1
                        } else {
                            end_str.parse().ok()?
                        };
                        Some((start, end))
                    });
                let (start, end) = range_start_end.unwrap_or((0, payload.len() as u64 - 1));
                let end = end.min(payload.len() as u64 - 1);
                let chunk = &payload[start as usize..=end as usize];
                let body_len = chunk.len();
                let total = payload.len();
                let status = if range_start_end.is_some() { "206 Partial Content" } else { "200 OK" };
                let resp = format!(
                    "HTTP/1.1 {}\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nContent-Range: bytes {}-{}/{}\r\nConnection: close\r\n\r\n",
                    status, body_len, start, end, total
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.write_all(chunk).await;
                let _ = sock.flush().await;
                let _ = sock.shutdown().await;
            });
        }
    });

    // Configure the queue with 4 segments and no concurrency limit.
    let mut cfg = EngineConfig::default();
    cfg.max_segments = 4;
    cfg.max_concurrent_downloads = 4;
    cfg.max_retries = 1;
    cfg.retry_backoff_ms = 50;
    let dest = std::env::temp_dir().join(format!("zavyx_test_{}.bin", uuid::Uuid::new_v4()));
    cfg.download_dir = dest.parent().unwrap().to_path_buf();

    let progress: zavyx_core::ProgressCallback = Arc::new(|_, _| {});
    let queue = Queue::new(cfg, progress).unwrap();

    let mut job = DownloadJob::default();
    job.origin = DownloadOrigin::Direct { url: format!("http://127.0.0.1:{}/", port) };
    job.destination = dest.clone();
    job.filename = Some("zavyx_test.bin".to_string());

    let id = queue.add(job);
    queue.pump();

    // Wait up to 30s for completion.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        queue.pump();
        if let Some(snap) = queue.progress(id) {
            if snap.state.is_terminal() {
                break;
            }
        }
        if std::time::Instant::now() > deadline {
            panic!("download did not complete in 30s");
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    // Verify the file matches.
    let actual = std::fs::read(&dest).unwrap();
    assert_eq!(actual.len(), payload.len(), "file size mismatch");
    assert_eq!(&actual[..], &payload[..], "byte content mismatch");

    // Snapshot should report completed.
    let snap = queue.progress(id).unwrap();
    assert_eq!(snap.state, DownloadState::Completed, "expected completed, got {:?}", snap.state);
    assert_eq!(snap.written, payload.len() as u64);
    assert_eq!(snap.total, Some(payload.len() as u64));

    // Cleanup.
    let _ = std::fs::remove_file(&dest);
    server.abort();
}

// Need a uuid for test temp files. We re-export it from zavyx-core.
extern crate uuid;

#[test]
fn default_jobs_get_unique_ids() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _g = rt.enter();
    let q = zavyx_core::Queue::new(zavyx_core::EngineConfig::default(), std::sync::Arc::new(|_, _| {})).unwrap();
    let a = q.add(zavyx_core::DownloadJob::default());
    let b = q.add(zavyx_core::DownloadJob::default());
    assert!(!a.is_nil());
    assert_ne!(a, b);
    assert_eq!(q.list().len(), 2);
}
