//! Zavyx HLS/DASH parsing utilities.

pub mod dash;
pub mod hls;

pub use dash::{parse_mpd, Mpd, MpdError, MpdResult, Period, AdaptationSet, Representation, SegmentTemplate, SegmentTimeline};
pub use hls::{parse_playlist, HlsError, HlsKey, HlsPlaylist, HlsResult, HlsSegment, HlsVariant};
