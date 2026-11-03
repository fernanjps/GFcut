use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{error::TimelineError, media::MediaAsset};

pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// Rational frame rate representation (`num / den`), avoiding floating-point drift
/// for NTSC rates such as `24000/1001` (23.976 fps) or `30000/1001` (29.97 fps).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FrameRate {
    pub num: u32,
    pub den: u32,
}

impl FrameRate {
    pub const FPS_24: Self = Self { num: 24, den: 1 };
    pub const FPS_25: Self = Self { num: 25, den: 1 };
    pub const FPS_30: Self = Self { num: 30, den: 1 };
    pub const FPS_60: Self = Self { num: 60, den: 1 };
    pub const FPS_23_976: Self = Self {
        num: 24_000,
        den: 1_001,
    };
    pub const FPS_29_97: Self = Self {
        num: 30_000,
        den: 1_001,
    };

    pub fn new(num: u32, den: u32) -> Result<Self, TimelineError> {
        if num == 0 || den == 0 {
            return Err(TimelineError::InvalidFrameRate { num, den });
        }
        Ok(Self { num, den })
    }

    pub fn as_f64(self) -> f64 {
        f64::from(self.num) / f64::from(self.den)
    }

    /// Converts a timestamp in seconds to the nearest frame index.
    pub fn seconds_to_frames(self, seconds: f64) -> i64 {
        (seconds * self.as_f64()).round() as i64
    }

    /// Converts a frame index back to seconds.
    pub fn frames_to_seconds(self, frames: i64) -> f64 {
        (frames as f64) / self.as_f64()
    }
}

impl Default for FrameRate {
    fn default() -> Self {
        Self::FPS_30
    }
}

/// Output canvas resolution in pixels.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Resolution {
    pub width: u32,
    pub height: u32,
}

impl Resolution {
    pub const HD_720P: Self = Self {
        width: 1280,
        height: 720,
    };
    pub const FULL_HD_1080P: Self = Self {
        width: 1920,
        height: 1080,
    };
    pub const UHD_4K: Self = Self {
        width: 3840,
        height: 2160,
    };
    pub const VERTICAL_1080P: Self = Self {
        width: 1080,
        height: 1920,
    };

    pub fn new(width: u32, height: u32) -> Result<Self, TimelineError> {
        if width == 0 || height == 0 {
            return Err(TimelineError::InvalidResolution { width, height });
        }
        Ok(Self { width, height })
    }
}

impl Default for Resolution {
    fn default() -> Self {
        Self::FULL_HD_1080P
    }
}

/// Kind of media a track holds.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    Video,
    Audio,
    Subtitle,
}

/// A single media segment placed on a [`Track`].
///
/// * `in_point` / `out_point`: trim bounds within `source_path` (in seconds).
/// * `timeline_in` / `timeline_out`: position and span on the timeline (in seconds).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Clip {
    #[schemars(with = "String")]
    pub id: Uuid,
    pub name: String,
    pub source_path: PathBuf,
    pub in_point: f64,
    pub out_point: f64,
    pub timeline_in: f64,
    pub timeline_out: f64,
    #[serde(default = "default_speed")]
    pub speed: f64,
    #[serde(default = "default_zero_f32")]
    pub position_x: f32,
    #[serde(default = "default_zero_f32")]
    pub position_y: f32,
    #[serde(default = "default_unit_f32")]
    pub scale: f32,
    #[serde(default = "default_unit_f32")]
    pub opacity: f32,
    #[serde(default = "default_unit_f32")]
    pub volume: f32,
}

fn default_speed() -> f64 {
    1.0
}

fn default_zero_f32() -> f32 {
    0.0
}

fn default_unit_f32() -> f32 {
    1.0
}

impl Clip {
    pub fn new(
        source_path: impl Into<PathBuf>,
        in_point: f64,
        out_point: f64,
        timeline_in: f64,
    ) -> Result<Self, TimelineError> {
        if in_point < 0.0 || out_point <= in_point {
            return Err(TimelineError::InvalidTimeRange {
                start: in_point,
                end: out_point,
            });
        }
        if timeline_in < 0.0 {
            return Err(TimelineError::InvalidTimeRange {
                start: timeline_in,
                end: timeline_in,
            });
        }

        let source_path = source_path.into();
        let name = source_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("Clip")
            .to_owned();
        let duration = out_point - in_point;

        Ok(Self {
            id: Uuid::new_v4(),
            name,
            source_path,
            in_point,
            out_point,
            timeline_in,
            timeline_out: timeline_in + duration,
            speed: 1.0,
            position_x: 0.0,
            position_y: 0.0,
            scale: 1.0,
            opacity: 1.0,
            volume: 1.0,
        })
    }

    /// Duration in the source media (`out_point - in_point`).
    pub fn source_duration(&self) -> f64 {
        (self.out_point - self.in_point).max(0.0)
    }

    /// Duration on the timeline (`timeline_out - timeline_in`).
    pub fn timeline_duration(&self) -> f64 {
        (self.timeline_out - self.timeline_in).max(0.0)
    }

    /// Returns `true` if `timestamp` falls strictly inside `(timeline_in, timeline_out)`.
    pub fn can_split_at(&self, timestamp: f64) -> bool {
        const EPSILON: f64 = 1e-6;
        timestamp > self.timeline_in + EPSILON && timestamp < self.timeline_out - EPSILON
    }

    /// Maps a timeline timestamp to the corresponding timestamp in the source file,
    /// strictly clamped to `[in_point, out_point]` even when `speed != 1.0`.
    pub fn timeline_to_source_time(&self, timeline_timestamp: f64) -> f64 {
        let relative = (timeline_timestamp - self.timeline_in).max(0.0);
        let mapped = self.in_point + relative * self.speed.max(0.01);
        mapped.clamp(self.in_point, self.out_point)
    }
}

/// An ordered track of clips (e.g. `"Video Track 1"`, `"Audio Track 1"`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Track {
    #[schemars(with = "String")]
    pub id: Uuid,
    pub name: String,
    pub kind: TrackKind,
    pub clips: Vec<Clip>,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub locked: bool,
    #[serde(default = "default_true")]
    pub visible: bool,
    #[serde(default = "default_unit_f32")]
    pub volume: f32,
}

fn default_true() -> bool {
    true
}

impl Track {
    pub fn new(name: impl Into<String>, kind: TrackKind) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            kind,
            clips: Vec::new(),
            muted: false,
            locked: false,
            visible: true,
            volume: 1.0,
        }
    }

    /// End timestamp of the last clip on this track, or `0.0` if empty.
    pub fn duration(&self) -> f64 {
        self.clips
            .iter()
            .map(|clip| clip.timeline_out)
            .fold(0.0_f64, f64::max)
    }

    /// Sorts clips chronologically by `timeline_in`.
    pub fn sort_clips(&mut self) {
        self.clips
            .sort_by(|a, b| a.timeline_in.total_cmp(&b.timeline_in));
    }
}

/// Canonical GFCut project structure (`.gfcut.json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Project {
    pub version: u32,
    #[schemars(with = "String")]
    pub id: Uuid,
    pub name: String,
    pub fps: FrameRate,
    pub resolution: Resolution,
    pub tracks: Vec<Track>,
    #[serde(default)]
    pub assets: Vec<MediaAsset>,
}

impl Project {
    pub fn new(name: impl Into<String>, fps: FrameRate, resolution: Resolution) -> Self {
        Self {
            version: CURRENT_SCHEMA_VERSION,
            id: Uuid::new_v4(),
            name: name.into(),
            fps,
            resolution,
            tracks: Vec::new(),
            assets: Vec::new(),
        }
    }

    /// Creates a project pre-populated with `Video Track 1` and `Audio Track 1`.
    pub fn with_default_tracks(name: impl Into<String>) -> Self {
        let mut project = Self::new(name, FrameRate::default(), Resolution::default());
        project.add_track("Video Track 1", TrackKind::Video);
        project.add_track("Audio Track 1", TrackKind::Audio);
        project
    }

    /// Total duration of the timeline across all tracks in seconds.
    pub fn duration(&self) -> f64 {
        self.tracks
            .iter()
            .map(Track::duration)
            .fold(0.0_f64, f64::max)
    }
}

impl Default for Project {
    fn default() -> Self {
        Self::with_default_tracks("Untitled Project")
    }
}
