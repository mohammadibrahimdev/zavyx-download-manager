//! Zavyx DM core — clean-room reimplementation of IDM-style download manager behavior.

pub mod config;
pub mod downloader;
pub mod errors;
pub mod model;
pub mod queue;

pub use config::EngineConfig;
pub use downloader::{
    download_segment, merge_segments, part_path, plan_segments, probe, ProgressSnapshot,
};
pub use errors::{DownloadError, DownloadResult};
pub use model::{
    ConvertProfile, DownloadId, DownloadJob, DownloadOrigin, DownloadState, Segment,
    SequenceSpec,
};
pub use queue::{Queue, ProgressCallback};
