//! Error types for the core engine.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DownloadError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("URL parse error: {0}")]
    Url(#[from] url::ParseError),
    // The body is kept for debugging but left out of the message: servers often
    // send a whole HTML page, which would flood the UI.
    #[error("Server returned HTTP {status}")]
    Status { status: u16, body: String },
    #[error("Server does not support range requests (no Accept-Ranges header)")]
    RangeNotSupported,
    #[error("Content length unknown; cannot multi-segment")]
    LengthUnknown,
    #[error("Segment {index} out of bounds")]
    SegmentOutOfBounds { index: usize },
    #[error("Download was cancelled by the user")]
    Cancelled,
    #[error("Exceeded retry budget for segment {index}")]
    RetryExhausted { index: usize },
    #[error("Invalid download origin / URL")]
    InvalidOrigin,
    #[error("Configuration error: {0}")]
    Config(String),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Other: {0}")]
    Other(String),
}

pub type DownloadResult<T> = Result<T, DownloadError>;
