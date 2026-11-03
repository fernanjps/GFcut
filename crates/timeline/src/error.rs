use std::path::PathBuf;

use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum TimelineError {
    #[error("track not found: {0}")]
    TrackNotFound(Uuid),

    #[error("clip not found: {0}")]
    ClipNotFound(Uuid),

    #[error("media asset not found: {0}")]
    AssetNotFound(Uuid),

    #[error("invalid frame rate: {num}/{den} (both numerator and denominator must be > 0)")]
    InvalidFrameRate { num: u32, den: u32 },

    #[error("invalid resolution: {width}x{height} (both dimensions must be > 0)")]
    InvalidResolution { width: u32, height: u32 },

    #[error("invalid time range: start ({start}s) must be non-negative and strictly less than end ({end}s)")]
    InvalidTimeRange { start: f64, end: f64 },

    #[error("invalid duration: {0}s (must be > 0)")]
    InvalidDuration(f64),

    #[error("invalid speed multiplier: {0} (must be > 0)")]
    InvalidSpeed(f64),

    #[error("split timestamp {timestamp}s is outside clip {clip_id} bounds ({timeline_in}s..{timeline_out}s)")]
    SplitOutOfBounds {
        clip_id: Uuid,
        timestamp: f64,
        timeline_in: f64,
        timeline_out: f64,
    },

    #[error("failed to read or write project file at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to serialize or deserialize project JSON: {0}")]
    Serialization(#[from] serde_json::Error),
}
