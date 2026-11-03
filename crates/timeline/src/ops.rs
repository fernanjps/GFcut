use std::path::PathBuf;

use uuid::Uuid;

use crate::{
    error::TimelineError,
    media::{MediaAsset, SilenceInterval},
    model::{Clip, Project, Track, TrackKind},
};

const MIN_CLIP_DURATION: f64 = 1e-4;

impl Project {
    /// Appends a new track to the project and returns its [`Uuid`].
    pub fn add_track(&mut self, name: impl Into<String>, kind: TrackKind) -> Uuid {
        let track = Track::new(name, kind);
        let id = track.id;
        self.tracks.push(track);
        id
    }

    /// Looks up a track by ID.
    pub fn track(&self, track_id: Uuid) -> Result<&Track, TimelineError> {
        self.tracks
            .iter()
            .find(|track| track.id == track_id)
            .ok_or(TimelineError::TrackNotFound(track_id))
    }

    /// Looks up a mutable track by ID.
    pub fn track_mut(&mut self, track_id: Uuid) -> Result<&mut Track, TimelineError> {
        self.tracks
            .iter_mut()
            .find(|track| track.id == track_id)
            .ok_or(TimelineError::TrackNotFound(track_id))
    }

    /// Finds a clip by ID across all tracks, returning `(track_id, &Clip)`.
    pub fn find_clip(&self, clip_id: Uuid) -> Result<(Uuid, &Clip), TimelineError> {
        for track in &self.tracks {
            if let Some(clip) = track.clips.iter().find(|c| c.id == clip_id) {
                return Ok((track.id, clip));
            }
        }
        Err(TimelineError::ClipNotFound(clip_id))
    }

    /// Inserts a clip onto `track_id` at `timeline_in` with the specified `duration`
    /// starting from `in_point` in the source file (`0.0` by default in typical MCP calls).
    pub fn add_clip(
        &mut self,
        track_id: Uuid,
        source_path: impl Into<PathBuf>,
        timeline_in: f64,
        in_point: f64,
        duration: f64,
    ) -> Result<Uuid, TimelineError> {
        if duration <= 0.0 {
            return Err(TimelineError::InvalidDuration(duration));
        }

        let clip = Clip::new(source_path, in_point, in_point + duration, timeline_in)?;
        let clip_id = clip.id;

        let track = self.track_mut(track_id)?;
        track.clips.push(clip);
        track.sort_clips();

        Ok(clip_id)
    }

    /// Appends a clip immediately after the last clip on `track_id`.
    pub fn append_clip(
        &mut self,
        track_id: Uuid,
        source_path: impl Into<PathBuf>,
        in_point: f64,
        duration: f64,
    ) -> Result<Uuid, TimelineError> {
        let timeline_in = self.track(track_id)?.duration();
        self.add_clip(track_id, source_path, timeline_in, in_point, duration)
    }

    /// Splits the clip identified by `clip_id` at `timeline_timestamp` into two contiguous clips.
    ///
    /// Returns `(left_clip_id, right_clip_id)`. The left clip retains the original `clip_id`
    /// so existing references remain valid, while the right half receives a new [`Uuid`].
    pub fn split_clip(
        &mut self,
        clip_id: Uuid,
        timeline_timestamp: f64,
    ) -> Result<(Uuid, Uuid), TimelineError> {
        for track in &mut self.tracks {
            if let Some(index) = track.clips.iter().position(|c| c.id == clip_id) {
                let clip = &track.clips[index];
                if !clip.can_split_at(timeline_timestamp) {
                    return Err(TimelineError::SplitOutOfBounds {
                        clip_id,
                        timestamp: timeline_timestamp,
                        timeline_in: clip.timeline_in,
                        timeline_out: clip.timeline_out,
                    });
                }

                let split_source_time = clip.timeline_to_source_time(timeline_timestamp);
                let mut right_clip = clip.clone();
                right_clip.id = Uuid::new_v4();
                right_clip.in_point = split_source_time;
                right_clip.timeline_in = timeline_timestamp;

                let right_id = right_clip.id;
                let left_clip = &mut track.clips[index];
                left_clip.out_point = split_source_time;
                left_clip.timeline_out = timeline_timestamp;
                let left_id = left_clip.id;

                track.clips.insert(index + 1, right_clip);
                return Ok((left_id, right_id));
            }
        }

        Err(TimelineError::ClipNotFound(clip_id))
    }

    /// Removes a clip by ID. If `ripple` is `true`, all subsequent clips on the same track
    /// shift earlier by the removed clip's timeline duration to close the gap.
    pub fn remove_clip(&mut self, clip_id: Uuid, ripple: bool) -> Result<Clip, TimelineError> {
        for track in &mut self.tracks {
            if let Some(index) = track.clips.iter().position(|c| c.id == clip_id) {
                let removed = track.clips.remove(index);
                if ripple {
                    let shift = removed.timeline_duration();
                    for subsequent in &mut track.clips[index..] {
                        subsequent.timeline_in = (subsequent.timeline_in - shift).max(0.0);
                        subsequent.timeline_out = (subsequent.timeline_out - shift).max(0.0);
                    }
                }
                return Ok(removed);
            }
        }

        Err(TimelineError::ClipNotFound(clip_id))
    }

    /// Moves a clip to `new_timeline_in`, preserving its duration.
    pub fn move_clip(&mut self, clip_id: Uuid, new_timeline_in: f64) -> Result<(), TimelineError> {
        if new_timeline_in < 0.0 {
            return Err(TimelineError::InvalidTimeRange {
                start: new_timeline_in,
                end: new_timeline_in,
            });
        }

        for track in &mut self.tracks {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == clip_id) {
                let duration = clip.timeline_duration();
                clip.timeline_in = new_timeline_in;
                clip.timeline_out = new_timeline_in + duration;
                track.sort_clips();
                return Ok(());
            }
        }

        Err(TimelineError::ClipNotFound(clip_id))
    }

    /// Moves a clip to `new_timeline_in` and optionally re-assigns it to `target_track_id`.
    /// Preserves duration and validates track kind compatibility and lock status.
    pub fn move_clip_to_track(
        &mut self,
        clip_id: Uuid,
        target_track_id: Uuid,
        new_timeline_in: f64,
    ) -> Result<(), TimelineError> {
        if new_timeline_in < 0.0 {
            return Err(TimelineError::InvalidTimeRange {
                start: new_timeline_in,
                end: new_timeline_in,
            });
        }

        // Find source track and clip index
        let (src_track_idx, clip_idx, src_track_kind) = self
            .tracks
            .iter()
            .enumerate()
            .find_map(|(t_idx, track)| {
                track
                    .clips
                    .iter()
                    .position(|c| c.id == clip_id)
                    .map(|c_idx| (t_idx, c_idx, track.kind))
            })
            .ok_or(TimelineError::ClipNotFound(clip_id))?;

        // Find target track
        let (dst_track_idx, dst_track_kind, is_locked) = self
            .tracks
            .iter()
            .enumerate()
            .find(|(_, t)| t.id == target_track_id)
            .map(|(t_idx, t)| (t_idx, t.kind, t.locked))
            .ok_or(TimelineError::TrackNotFound(target_track_id))?;

        if is_locked {
            // Cannot move into locked track
            return Ok(());
        }

        // Compatibility check: Video can move to Video; Audio to Audio; Subtitle to Subtitle
        if src_track_kind != dst_track_kind {
            // Incompatible track kinds
            return Ok(());
        }

        if src_track_idx == dst_track_idx {
            let clip = &mut self.tracks[src_track_idx].clips[clip_idx];
            let duration = clip.timeline_duration();
            clip.timeline_in = new_timeline_in;
            clip.timeline_out = new_timeline_in + duration;
            self.tracks[src_track_idx].sort_clips();
            return Ok(());
        }

        // Move across tracks
        let mut clip = self.tracks[src_track_idx].clips.remove(clip_idx);
        let duration = clip.timeline_duration();
        clip.timeline_in = new_timeline_in;
        clip.timeline_out = new_timeline_in + duration;

        self.tracks[dst_track_idx].clips.push(clip);
        self.tracks[dst_track_idx].sort_clips();
        self.tracks[src_track_idx].sort_clips();

        Ok(())
    }

    /// Trims a clip's `in_point` and `out_point` in the source media and updates `timeline_out`.
    pub fn trim_clip(
        &mut self,
        clip_id: Uuid,
        new_in_point: f64,
        new_out_point: f64,
    ) -> Result<(), TimelineError> {
        if new_in_point < 0.0 || new_out_point <= new_in_point {
            return Err(TimelineError::InvalidTimeRange {
                start: new_in_point,
                end: new_out_point,
            });
        }

        for track in &mut self.tracks {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == clip_id) {
                clip.in_point = new_in_point;
                clip.out_point = new_out_point;
                let timeline_duration = (new_out_point - new_in_point) / clip.speed;
                clip.timeline_out = clip.timeline_in + timeline_duration;
                return Ok(());
            }
        }

        Err(TimelineError::ClipNotFound(clip_id))
    }

    /// Removes silent intervals (expressed in timeline seconds) from `track_id` and
    /// optionally ripple-closes the resulting gaps so the remaining speech/action plays seamlessly.
    ///
    /// Returns the number of silent segments excised.
    pub fn remove_silence_intervals(
        &mut self,
        track_id: Uuid,
        intervals: &[SilenceInterval],
        ripple: bool,
    ) -> Result<usize, TimelineError> {
        let track = self.track_mut(track_id)?;

        // Normalize and sort valid intervals chronologically.
        let mut normalized: Vec<SilenceInterval> = intervals
            .iter()
            .copied()
            .filter(|i| i.end - i.start > MIN_CLIP_DURATION && i.end > 0.0)
            .map(|i| SilenceInterval {
                start: i.start.max(0.0),
                end: i.end,
            })
            .collect();
        normalized.sort_by(|a, b| a.start.total_cmp(&b.start));

        let mut removed_count = 0usize;

        for interval in normalized {
            let mut next_clips = Vec::with_capacity(track.clips.len() + 1);
            for clip in track.clips.drain(..) {
                // Case 1: Clip does not overlap the silence interval at all.
                if clip.timeline_out <= interval.start + MIN_CLIP_DURATION
                    || clip.timeline_in >= interval.end - MIN_CLIP_DURATION
                {
                    next_clips.push(clip);
                    continue;
                }

                removed_count += 1;

                // Keep the left portion if silence starts after clip.timeline_in
                if interval.start > clip.timeline_in + MIN_CLIP_DURATION {
                    let mut left = clip.clone();
                    left.out_point = clip.timeline_to_source_time(interval.start);
                    left.timeline_out = interval.start;
                    next_clips.push(left);
                }

                // Keep the right portion if silence ends before clip.timeline_out
                if interval.end < clip.timeline_out - MIN_CLIP_DURATION {
                    let mut right = clip.clone();
                    right.id = Uuid::new_v4();
                    right.in_point = clip.timeline_to_source_time(interval.end);
                    right.timeline_in = interval.end;
                    next_clips.push(right);
                }
            }
            track.clips = next_clips;
        }

        if ripple {
            let mut cursor = 0.0_f64;
            for clip in &mut track.clips {
                let duration = clip.timeline_duration();
                clip.timeline_in = cursor;
                clip.timeline_out = cursor + duration;
                cursor += duration;
            }
        }

        Ok(removed_count)
    }

    /// Registers or updates a probed/transcribed [`MediaAsset`] in the project catalog.
    pub fn upsert_asset(&mut self, asset: MediaAsset) -> Uuid {
        let id = asset.id;
        if let Some(existing) = self.assets.iter_mut().find(|a| a.path == asset.path) {
            *existing = asset;
            existing.id
        } else {
            self.assets.push(asset);
            id
        }
    }

    /// Finds an existing [`MediaAsset`] by path or registers a minimal asset entry, returning its [`Uuid`].
    pub fn ensure_asset_for_path(
        &mut self,
        path: impl Into<PathBuf>,
        fallback_duration_secs: f64,
    ) -> Uuid {
        let path = path.into();
        if let Some(existing) = self.assets.iter().find(|a| a.path == path) {
            return existing.id;
        }
        let asset = MediaAsset::minimal(path, fallback_duration_secs);
        let id = asset.id;
        self.assets.push(asset);
        id
    }

    /// Attaches a [`Transcript`] to the specified `asset_id` in the project's asset catalog.
    pub fn attach_transcript(
        &mut self,
        asset_id: &Uuid,
        transcript: crate::media::Transcript,
    ) -> Result<(), TimelineError> {
        let asset = self
            .assets
            .iter_mut()
            .find(|a| &a.id == asset_id)
            .ok_or(TimelineError::AssetNotFound(*asset_id))?;
        asset.transcript = Some(transcript);
        Ok(())
    }

    /// Attaches a [`Transcript`] to `asset_id` and atomically persists the project to `project_path`.
    pub fn attach_transcript_and_save(
        &mut self,
        asset_id: &Uuid,
        transcript: crate::media::Transcript,
        project_path: impl AsRef<std::path::Path>,
    ) -> Result<(), TimelineError> {
        self.attach_transcript(asset_id, transcript)?;
        self.save_to_file_atomic(project_path)
    }
}
