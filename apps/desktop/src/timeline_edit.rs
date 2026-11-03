//! Pure, GPUI-independent timeline editing math: interactive edge trimming and magnetic snapping.
//! Kept free of UI types so it can be unit-tested deterministically.

use gfcut_timeline::{Clip, Project};
use uuid::Uuid;

/// Shortest clip duration (timeline seconds) a trim operation is allowed to produce.
pub(crate) const MIN_TRIM_CLIP_SECS: f64 = 0.1;
/// Visual snapping distance (pixels) to the playhead and neighbouring clip edges.
pub(crate) const SNAP_TOLERANCE_PX: f32 = 8.0;
/// Width (pixels) of the interactive trim zones on each side of a clip block.
pub(crate) const TRIM_HANDLE_WIDTH_PX: f32 = 6.0;

/// Which edge of a clip is being trimmed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TrimEdge {
    Left,
    Right,
}

/// Resulting source/timeline bounds of a clip after a trim.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TrimResult {
    pub(crate) in_point: f64,
    pub(crate) out_point: f64,
    pub(crate) timeline_in: f64,
    pub(crate) timeline_out: f64,
}

impl TrimResult {
    pub(crate) fn from_clip(clip: &Clip) -> Self {
        Self {
            in_point: clip.in_point,
            out_point: clip.out_point,
            timeline_in: clip.timeline_in,
            timeline_out: clip.timeline_out,
        }
    }

    pub(crate) fn apply_to(self, clip: &mut Clip) {
        clip.in_point = self.in_point;
        clip.out_point = self.out_point;
        clip.timeline_in = self.timeline_in;
        clip.timeline_out = self.timeline_out;
    }

    pub(crate) fn approx_eq(&self, other: &Self) -> bool {
        const EPS: f64 = 5e-4;
        (self.in_point - other.in_point).abs() < EPS
            && (self.out_point - other.out_point).abs() < EPS
            && (self.timeline_in - other.timeline_in).abs() < EPS
            && (self.timeline_out - other.timeline_out).abs() < EPS
    }
}

/// Computes the trimmed bounds of `original` when `edge` is dragged to `target_time` (timeline seconds).
///
/// * Left edge: moves `timeline_in` and `in_point` together (respecting `speed`), keeping
///   `timeline_out`/`out_point` fixed, so the material after the cut does not shift.
///   Clamped so `in_point >= 0` and the clip keeps at least [`MIN_TRIM_CLIP_SECS`].
/// * Right edge: moves `timeline_out` and `out_point` together. When `source_duration` is known,
///   `out_point` never exceeds the real length of the source file. `None` means the clip has no
///   finite source (e.g. a synthetic text/subtitle clip) and can be extended freely.
pub(crate) fn compute_trim(
    original: &Clip,
    edge: TrimEdge,
    target_time: f64,
    source_duration: Option<f64>,
) -> TrimResult {
    let speed = original.speed.max(0.01);
    match edge {
        TrimEdge::Left => {
            let min_t = (original.timeline_in - original.in_point / speed).max(0.0);
            let max_t = (original.timeline_out - MIN_TRIM_CLIP_SECS).max(min_t);
            let t = target_time.clamp(min_t, max_t);
            let in_point = (original.in_point + (t - original.timeline_in) * speed).max(0.0);
            TrimResult {
                in_point,
                out_point: original.out_point,
                timeline_in: t,
                timeline_out: original.timeline_out,
            }
        }
        TrimEdge::Right => {
            let min_t = original.timeline_in + MIN_TRIM_CLIP_SECS;
            let effective_src = match source_duration {
                Some(src) => Some(src.max(original.out_point)),
                None => None,
            };
            let max_t = match effective_src {
                Some(src) => original.timeline_out + (src - original.out_point).max(0.0) / speed,
                None => f64::INFINITY,
            }
            .max(min_t);
            let t = target_time.clamp(min_t, max_t);
            let out_point = original.out_point + (t - original.timeline_out) * speed;
            TrimResult {
                in_point: original.in_point,
                out_point,
                timeline_in: original.timeline_in,
                timeline_out: t,
            }
        }
    }
}

/// Collects magnetic snap targets: timeline origin, the given playhead position and the
/// `timeline_in`/`timeline_out` of every clip on every track except `exclude`.
pub(crate) fn collect_snap_points(
    project: &Project,
    playhead: Option<f64>,
    exclude: Option<Uuid>,
) -> Vec<f64> {
    let mut points = vec![0.0];
    points.extend(playhead);
    for track in &project.tracks {
        for clip in &track.clips {
            if Some(clip.id) == exclude {
                continue;
            }
            points.push(clip.timeline_in);
            points.push(clip.timeline_out);
        }
    }
    points.sort_by(f64::total_cmp);
    points.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
    points
}

/// Returns the snap point closest to `target_time` if it lies within `tolerance_px` on screen
/// (converted to seconds with `px_per_sec`), otherwise `target_time` unchanged.
pub(crate) fn apply_timeline_snapping(
    target_time: f64,
    tolerance_px: f32,
    px_per_sec: f32,
    snap_points: &[f64],
) -> f64 {
    nearest_snap(target_time, tolerance_px, px_per_sec, snap_points)
        .map(|(point, _)| point)
        .unwrap_or(target_time)
}

fn nearest_snap(
    target_time: f64,
    tolerance_px: f32,
    px_per_sec: f32,
    snap_points: &[f64],
) -> Option<(f64, f64)> {
    let tolerance_secs = f64::from(tolerance_px) / f64::from(px_per_sec.max(0.001));
    snap_points
        .iter()
        .map(|&p| (p, (p - target_time).abs()))
        .filter(|&(_, dist)| dist <= tolerance_secs)
        .min_by(|a, b| a.1.total_cmp(&b.1))
}

/// Snaps a whole clip being moved: whichever of its start (`target_in`) or end
/// (`target_in + duration`) is closest to a snap point wins. Returns `(new_timeline_in, snapped_at)`.
pub(crate) fn snap_clip_move(
    target_in: f64,
    duration: f64,
    tolerance_px: f32,
    px_per_sec: f32,
    snap_points: &[f64],
) -> (f64, Option<f64>) {
    let start = nearest_snap(target_in, tolerance_px, px_per_sec, snap_points);
    let end = nearest_snap(target_in + duration, tolerance_px, px_per_sec, snap_points);
    match (start, end) {
        (Some((ps, ds)), Some((pe, de))) => {
            if ds <= de {
                (ps.max(0.0), Some(ps))
            } else {
                ((pe - duration).max(0.0), Some(pe))
            }
        }
        (Some((ps, _)), None) => (ps.max(0.0), Some(ps)),
        (None, Some((pe, _))) => ((pe - duration).max(0.0), Some(pe)),
        (None, None) => (target_in.max(0.0), None),
    }
}

/// Deletes a clip from a list of clips and shifts all subsequent clips earlier
/// by the duration of the removed clip to close the gap (Ripple Delete).
/// Returns `Some((removed_clip, shifted_count))` if the clip was found and removed.
#[allow(dead_code)]
pub(crate) fn apply_ripple_delete_to_clips(
    clips: &mut Vec<Clip>,
    clip_id: Uuid,
) -> Option<(Clip, usize)> {
    let index = clips.iter().position(|c| c.id == clip_id)?;
    let removed = clips.remove(index);
    let shift = removed.timeline_duration();
    let shifted_count = clips.len() - index;
    for subsequent in &mut clips[index..] {
        subsequent.timeline_in = (subsequent.timeline_in - shift).max(0.0);
        subsequent.timeline_out = (subsequent.timeline_out - shift).max(0.0);
    }
    Some((removed, shifted_count))
}

#[cfg(test)]
mod tests {
    use gfcut_timeline::{Clip, Project};

    use super::{
        MIN_TRIM_CLIP_SECS, TrimEdge, apply_ripple_delete_to_clips, apply_timeline_snapping,
        collect_snap_points, compute_trim, snap_clip_move,
    };

    fn clip(in_point: f64, out_point: f64, timeline_in: f64, speed: f64) -> Clip {
        let mut c = Clip::new("a.mp4", in_point, out_point, timeline_in).unwrap();
        c.speed = speed;
        c.timeline_out = timeline_in + (out_point - in_point) / speed;
        c
    }

    #[test]
    fn left_trim_moves_in_point_and_keeps_tail_fixed() {
        let c = clip(2.0, 8.0, 10.0, 1.0); // timeline [10..16]
        let r = compute_trim(&c, TrimEdge::Left, 11.5, Some(20.0));
        assert!((r.timeline_in - 11.5).abs() < 1e-9);
        assert!((r.in_point - 3.5).abs() < 1e-9);
        assert!((r.timeline_out - 16.0).abs() < 1e-9);
        assert!((r.out_point - 8.0).abs() < 1e-9);
    }

    #[test]
    fn left_trim_cannot_reveal_media_before_source_start() {
        let c = clip(2.0, 8.0, 10.0, 1.0);
        let r = compute_trim(&c, TrimEdge::Left, 0.0, Some(20.0));
        assert!((r.timeline_in - 8.0).abs() < 1e-9);
        assert!(r.in_point.abs() < 1e-9);
    }

    #[test]
    fn right_trim_is_clamped_to_real_source_duration_and_speed() {
        let c = clip(0.0, 4.0, 0.0, 2.0); // timeline [0..2], source [0..4] at 2x
        let r = compute_trim(&c, TrimEdge::Right, 50.0, Some(6.0));
        // 2 extra source seconds at 2x = 1 extra timeline second.
        assert!((r.timeline_out - 3.0).abs() < 1e-9);
        assert!((r.out_point - 6.0).abs() < 1e-9);
    }

    #[test]
    fn trims_never_collapse_below_minimum_duration() {
        let c = clip(0.0, 5.0, 0.0, 1.0);
        let l = compute_trim(&c, TrimEdge::Left, 99.0, Some(5.0));
        assert!((l.timeline_out - l.timeline_in - MIN_TRIM_CLIP_SECS).abs() < 1e-9);
        let r = compute_trim(&c, TrimEdge::Right, -5.0, Some(5.0));
        assert!((r.timeline_out - r.timeline_in - MIN_TRIM_CLIP_SECS).abs() < 1e-9);
    }

    #[test]
    fn snapping_respects_pixel_tolerance() {
        let points = [0.0, 5.0, 10.0];
        // 100 px/s → 8 px = 0.08 s.
        assert_eq!(apply_timeline_snapping(5.05, 8.0, 100.0, &points), 5.0);
        assert_eq!(apply_timeline_snapping(5.20, 8.0, 100.0, &points), 5.20);
    }

    #[test]
    fn clip_move_snaps_by_nearest_edge_and_collects_points() {
        let mut project = Project::with_default_tracks("Snap");
        let v1 = project.tracks[0].id;
        let a = project.add_clip(v1, "a.mp4", 0.0, 0.0, 4.0).unwrap();
        let b = project.add_clip(v1, "b.mp4", 6.0, 0.0, 2.0).unwrap();

        let points = collect_snap_points(&project, Some(12.0), Some(b));
        assert_eq!(points, vec![0.0, 4.0, 12.0]);
        assert!(!collect_snap_points(&project, None, Some(a)).contains(&4.0));

        // Clip of 2s dragged so its start lands at 4.05 → start snaps to 4.0.
        let (new_in, at) = snap_clip_move(4.05, 2.0, 8.0, 100.0, &points);
        assert!((new_in - 4.0).abs() < 1e-9);
        assert_eq!(at, Some(4.0));
        // End (9.96 + 2 = 11.96) close to playhead 12.0 → end snaps.
        let (new_in, at) = snap_clip_move(9.96, 2.0, 8.0, 100.0, &points);
        assert!((new_in - 10.0).abs() < 1e-9);
        assert_eq!(at, Some(12.0));
    }

    #[test]
    fn ripple_delete_removes_clip_and_shifts_tail_left() {
        let mut clips = vec![
            clip(0.0, 4.0, 0.0, 1.0),  // [0..4]
            clip(0.0, 3.0, 5.0, 1.0),  // [5..8], dur = 3
            clip(0.0, 2.0, 9.0, 1.0),  // [9..11]
            clip(0.0, 5.0, 15.0, 1.0), // [15..20]
        ];
        let target_id = clips[1].id;
        let res = apply_ripple_delete_to_clips(&mut clips, target_id);
        assert!(res.is_some());
        let (removed, shifted_count) = res.unwrap();
        assert_eq!(removed.id, target_id);
        assert_eq!(shifted_count, 2);
        assert_eq!(clips.len(), 3);
        // Clip 0 untouched
        assert_eq!(clips[0].timeline_in, 0.0);
        assert_eq!(clips[0].timeline_out, 4.0);
        // Clip 2 shifted by 3.0: 9.0 -> 6.0
        assert_eq!(clips[1].timeline_in, 6.0);
        assert_eq!(clips[1].timeline_out, 8.0);
        // Clip 3 shifted by 3.0: 15.0 -> 12.0
        assert_eq!(clips[2].timeline_in, 12.0);
        assert_eq!(clips[2].timeline_out, 17.0);
    }
}
