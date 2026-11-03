use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::model::{FrameRate, Resolution};

/// Metadata extracted from a media file via `inspect_media` (e.g. `ffprobe`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MediaAsset {
    #[schemars(with = "String")]
    pub id: Uuid,
    pub path: PathBuf,
    pub duration_secs: f64,
    pub format_name: String,
    pub size_bytes: Option<u64>,
    pub video_streams: Vec<VideoStreamInfo>,
    pub audio_streams: Vec<AudioStreamInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<Transcript>,
}

impl MediaAsset {
    /// Creates a minimal asset entry when full `ffprobe` metadata is not yet available.
    pub fn minimal(path: impl Into<PathBuf>, duration_secs: f64) -> Self {
        Self {
            id: Uuid::new_v4(),
            path: path.into(),
            duration_secs,
            format_name: "unknown".to_owned(),
            size_bytes: None,
            video_streams: Vec::new(),
            audio_streams: Vec::new(),
            transcript: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct VideoStreamInfo {
    pub index: u32,
    pub codec: String,
    pub resolution: Resolution,
    pub fps: FrameRate,
    pub pixel_format: Option<String>,
    pub bit_rate: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AudioStreamInfo {
    pub index: u32,
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u32,
    pub channel_layout: Option<String>,
    pub bit_rate: Option<u64>,
}

/// Time-aligned transcription produced by `transcribe_media` (e.g. Whisper).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Transcript {
    pub language: Option<String>,
    pub segments: Vec<TranscriptSegment>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TranscriptSegment {
    #[serde(default = "Uuid::new_v4")]
    #[schemars(with = "String")]
    pub id: Uuid,
    pub start: f64,
    pub end: f64,
    pub text: String,
}

impl TranscriptSegment {
    pub fn new(start: f64, end: f64, text: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            start,
            end,
            text: text.into(),
        }
    }
}

/// Detected silence interval in seconds `[start, end]`, used by `remove_silence`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SilenceInterval {
    pub start: f64,
    pub end: f64,
}

impl SilenceInterval {
    pub fn duration(&self) -> f64 {
        (self.end - self.start).max(0.0)
    }
}
