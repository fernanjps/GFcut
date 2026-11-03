pub mod error;
pub mod media;
pub mod model;
pub mod ops;
pub mod storage;

pub use error::TimelineError;
pub use media::{AudioStreamInfo, MediaAsset, SilenceInterval, Transcript, TranscriptSegment, VideoStreamInfo};
pub use model::{
    CURRENT_SCHEMA_VERSION, Clip, FrameRate, Project, Resolution, Track, TrackKind,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_project_and_splits_clip_accurately() {
        let mut project = Project::with_default_tracks("Demo Edit");
        let video_track_id = project.tracks[0].id;

        let clip_id = project
            .add_clip(video_track_id, "intro.mp4", 0.0, 2.0, 10.0)
            .expect("clip should be added");

        assert!((project.duration() - 10.0).abs() < 1e-6);

        let (left_id, right_id) = project
            .split_clip(clip_id, 4.0)
            .expect("split inside clip bounds should succeed");

        assert_eq!(left_id, clip_id);
        assert_ne!(left_id, right_id);

        let track = project.track(video_track_id).unwrap();
        assert_eq!(track.clips.len(), 2);

        // Left clip: timeline [0..4], source [2..6]
        assert!((track.clips[0].timeline_in - 0.0).abs() < 1e-6);
        assert!((track.clips[0].timeline_out - 4.0).abs() < 1e-6);
        assert!((track.clips[0].in_point - 2.0).abs() < 1e-6);
        assert!((track.clips[0].out_point - 6.0).abs() < 1e-6);

        // Right clip: timeline [4..10], source [6..12]
        assert!((track.clips[1].timeline_in - 4.0).abs() < 1e-6);
        assert!((track.clips[1].timeline_out - 10.0).abs() < 1e-6);
        assert!((track.clips[1].in_point - 6.0).abs() < 1e-6);
        assert!((track.clips[1].out_point - 12.0).abs() < 1e-6);
    }

    #[test]
    fn removes_silence_intervals_and_ripples_timeline() {
        let mut project = Project::with_default_tracks("Podcast Cut");
        let track_id = project.tracks[0].id;

        project
            .add_clip(track_id, "interview.mp4", 0.0, 0.0, 20.0)
            .unwrap();

        // Remove two silent segments: [2.0..5.0] (3s) and [10.0..15.0] (5s) -> remaining 12s
        let silences = [
            SilenceInterval {
                start: 2.0,
                end: 5.0,
            },
            SilenceInterval {
                start: 10.0,
                end: 15.0,
            },
        ];

        let excised = project
            .remove_silence_intervals(track_id, &silences, true)
            .expect("silence removal should succeed");

        assert_eq!(excised, 2);

        let track = project.track(track_id).unwrap();
        assert_eq!(track.clips.len(), 3);
        assert!((project.duration() - 12.0).abs() < 1e-6);

        // Segment 1: source [0..2], timeline [0..2]
        assert!((track.clips[0].in_point - 0.0).abs() < 1e-6);
        assert!((track.clips[0].out_point - 2.0).abs() < 1e-6);
        assert!((track.clips[0].timeline_in - 0.0).abs() < 1e-6);
        assert!((track.clips[0].timeline_out - 2.0).abs() < 1e-6);

        // Segment 2: source [5..10], timeline [2..7]
        assert!((track.clips[1].in_point - 5.0).abs() < 1e-6);
        assert!((track.clips[1].out_point - 10.0).abs() < 1e-6);
        assert!((track.clips[1].timeline_in - 2.0).abs() < 1e-6);
        assert!((track.clips[1].timeline_out - 7.0).abs() < 1e-6);

        // Segment 3: source [15..20], timeline [7..12]
        assert!((track.clips[2].in_point - 15.0).abs() < 1e-6);
        assert!((track.clips[2].out_point - 20.0).abs() < 1e-6);
        assert!((track.clips[2].timeline_in - 7.0).abs() < 1e-6);
        assert!((track.clips[2].timeline_out - 12.0).abs() < 1e-6);
    }

    #[test]
    fn roundtrips_project_via_atomic_file_save() {
        let mut project = Project::with_default_tracks("Atomic Save Test");
        let track_id = project.tracks[0].id;
        project
            .add_clip(track_id, "broll.mov", 0.0, 1.5, 4.5)
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.gfcut.json");

        project.save_to_file_atomic(&file_path).unwrap();
        let loaded = Project::load_from_file(&file_path).unwrap();

        assert_eq!(project, loaded);
    }

    #[test]
    fn moves_clip_between_compatible_tracks_across_timeline() {
        let mut project = Project::with_default_tracks("Cross-Track Move Test");
        let v1_id = project.tracks[0].id;
        let v2_id = project.add_track("Video Track 2", TrackKind::Video);

        let clip_id = project
            .add_clip(v1_id, "broll.mp4", 0.0, 0.0, 5.0)
            .unwrap();

        assert_eq!(project.track(v1_id).unwrap().clips.len(), 1);
        assert_eq!(project.track(v2_id).unwrap().clips.len(), 0);

        // Move clip from v1 to v2 at 3.0s
        project
            .move_clip_to_track(clip_id, v2_id, 3.0)
            .expect("cross track move should succeed");

        assert_eq!(project.track(v1_id).unwrap().clips.len(), 0);
        let v2_track = project.track(v2_id).unwrap();
        assert_eq!(v2_track.clips.len(), 1);
        assert_eq!(v2_track.clips[0].id, clip_id);
        assert!((v2_track.clips[0].timeline_in - 3.0).abs() < 1e-6);
        assert!((v2_track.clips[0].timeline_out - 8.0).abs() < 1e-6);
    }

    #[test]
    fn clamps_timeline_to_source_time_when_speed_varies() {
        let mut clip = Clip::new("sample.mp4", 2.0, 10.0, 0.0).unwrap();
        clip.speed = 2.0;
        clip.timeline_out = 4.0; // (10.0 - 2.0) / 2.0

        assert!((clip.timeline_to_source_time(0.0) - 2.0).abs() < 1e-6);
        assert!((clip.timeline_to_source_time(2.0) - 6.0).abs() < 1e-6);
        // Even past timeline_out, source time must never exceed out_point (10.0)
        assert!((clip.timeline_to_source_time(4.0) - 10.0).abs() < 1e-6);
        assert!((clip.timeline_to_source_time(9.0) - 10.0).abs() < 1e-6);
    }
}
