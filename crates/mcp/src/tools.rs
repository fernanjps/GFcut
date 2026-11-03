use std::path::{Path, PathBuf};

use gfcut_timeline::{Project, SilenceInterval, TrackKind};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    ffmpeg::{
        detect_media_silence, export_project_with_ffmpeg, probe_media,
        transcribe_media_with_pipeline,
    },
    protocol::{CallToolResult, ToolDefinition, input_schema_for},
};

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct GetTimelineArgs {
    /// Optional path to the `.gfcut.json` file. Uses the server's default project path if omitted.
    #[serde(default)]
    pub project_path: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct AddClipArgs {
    /// Optional path to the `.gfcut.json` file. Uses the server's default project path if omitted.
    #[serde(default)]
    pub project_path: Option<PathBuf>,
    /// Target track UUID. If omitted, uses the first Video track in the project.
    #[serde(default)]
    #[schemars(with = "Option<String>")]
    pub track_id: Option<Uuid>,
    /// Path to the source media file.
    pub file_path: PathBuf,
    /// Timeline start position in seconds. If omitted, appends at the end of the track.
    #[serde(default)]
    pub start_time: Option<f64>,
    /// Source media in-point in seconds (defaults to `0.0`).
    #[serde(default)]
    pub in_point: Option<f64>,
    /// Duration of the clip in seconds.
    pub duration: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SplitClipArgs {
    /// Optional path to the `.gfcut.json` file. Uses the server's default project path if omitted.
    #[serde(default)]
    pub project_path: Option<PathBuf>,
    /// UUID of the clip to split.
    #[schemars(with = "String")]
    pub clip_id: Uuid,
    /// Timeline timestamp in seconds where the cut should occur.
    pub timestamp: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct RemoveSilenceArgs {
    /// Optional path to the `.gfcut.json` file. Uses the server's default project path if omitted.
    #[serde(default)]
    pub project_path: Option<PathBuf>,
    /// UUID of the track to remove silence from.
    #[schemars(with = "String")]
    pub track_id: Uuid,
    /// Silence noise floor threshold in dB (e.g. `-35.0`). Defaults to `-35.0`.
    #[serde(default)]
    pub threshold_db: Option<f64>,
    /// Minimum silence duration in seconds (defaults to `0.4`).
    #[serde(default)]
    pub min_silence_duration: Option<f64>,
    /// Optional explicit timeline silence intervals `[start, end]`. If omitted, `ffmpeg silencedetect`
    /// is run automatically on each clip's `source_path`.
    #[serde(default)]
    pub intervals: Option<Vec<SilenceInterval>>,
    /// Whether to ripple-delete gaps after excising silence (defaults to `true`).
    #[serde(default)]
    pub ripple: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct InspectMediaArgs {
    /// Path to the media file to probe via `ffprobe`.
    pub file_path: PathBuf,
    /// Optional path to a `.gfcut.json` file; if provided and present, caches the probed asset in `project.assets`.
    #[serde(default)]
    pub project_path: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct TranscribeMediaArgs {
    /// Path to the video or audio file to transcribe.
    pub file_path: String,
    /// Whisper model identifier (e.g. `"tiny"`, `"base"`, `"whisper-1"`).
    #[serde(default)]
    pub model: Option<String>,
    /// Optional path to the `.gfcut.json` file. Uses the server's default project path if omitted.
    #[serde(default)]
    pub project_path: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ExportProjectArgs {
    /// Optional path to the `.gfcut.json` file. Uses the server's default project path if omitted.
    #[serde(default)]
    pub project_path: Option<PathBuf>,
    /// Destination path for the rendered video file (e.g. `output.mp4`).
    pub output_path: PathBuf,
    /// When `true`, executes `ffmpeg` immediately. When `false` (default), generates and returns the exact FFmpeg export plan/command.
    #[serde(default)]
    pub execute: Option<bool>,
}

/// Returns the catalog of MCP tools exposed by GFCut.
pub fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "get_timeline".to_owned(),
            description: "Reads the `.gfcut.json` project file and returns the current timeline state (tracks, clips, resolution, FPS, and duration).".to_owned(),
            input_schema: input_schema_for::<GetTimelineArgs>(),
        },
        ToolDefinition {
            name: "add_clip".to_owned(),
            description: "Inserts a media clip into a track on the timeline and atomically persists the updated `.gfcut.json` project.".to_owned(),
            input_schema: input_schema_for::<AddClipArgs>(),
        },
        ToolDefinition {
            name: "split_clip".to_owned(),
            description: "Splits an existing clip into two contiguous clips at the given timeline timestamp (seconds) and atomically updates the project file.".to_owned(),
            input_schema: input_schema_for::<SplitClipArgs>(),
        },
        ToolDefinition {
            name: "remove_silence".to_owned(),
            description: "Excises silent intervals from a track (using either provided intervals or FFmpeg `silencedetect` at `threshold_db`) and performs a ripple delete.".to_owned(),
            input_schema: input_schema_for::<RemoveSilenceArgs>(),
        },
        ToolDefinition {
            name: "inspect_media".to_owned(),
            description: "Invokes `ffprobe` on a media file and returns its duration, container format, resolution, frame rate, and audio/video stream metadata.".to_owned(),
            input_schema: input_schema_for::<InspectMediaArgs>(),
        },
        ToolDefinition {
            name: "transcribe_media".to_owned(),
            description: "Extracts 16 kHz mono WAV audio via FFmpeg, transcribes speech with timestamps (via `WHISPER_BIN` / Whisper CLI or fallback), attaches the `Transcript` to the `MediaAsset` in `.gfcut.json`, and returns LLM-readable segments.".to_owned(),
            input_schema: input_schema_for::<TranscribeMediaArgs>(),
        },
        ToolDefinition {
            name: "export_project".to_owned(),
            description: "Generates the FFmpeg render command/filtergraph for the current timeline and optionally executes FFmpeg to produce the output video.".to_owned(),
            input_schema: input_schema_for::<ExportProjectArgs>(),
        },
    ]
}

fn load_or_init_project(path: &Path) -> Result<Project, String> {
    if path.exists() {
        Project::load_from_file(path).map_err(|e| e.to_string())
    } else {
        let project = Project::default();
        project
            .save_to_file_atomic(path)
            .map_err(|e| e.to_string())?;
        Ok(project)
    }
}

pub fn dispatch_tool(
    default_project_path: &Path,
    name: &str,
    arguments: Option<Value>,
) -> CallToolResult {
    let args_val = arguments.unwrap_or_else(|| serde_json::json!({}));

    match name {
        "get_timeline" => {
            let args: GetTimelineArgs = match serde_json::from_value(args_val) {
                Ok(a) => a,
                Err(e) => return CallToolResult::tool_error(format!("invalid arguments: {e}")),
            };
            let path = args
                .project_path
                .as_deref()
                .unwrap_or(default_project_path);
            match load_or_init_project(path) {
                Ok(project) => CallToolResult::success_json(&project),
                Err(err) => CallToolResult::tool_error(err),
            }
        }

        "add_clip" => {
            let args: AddClipArgs = match serde_json::from_value(args_val) {
                Ok(a) => a,
                Err(e) => return CallToolResult::tool_error(format!("invalid arguments: {e}")),
            };
            let path = args
                .project_path
                .as_deref()
                .unwrap_or(default_project_path);

            let mut project = match load_or_init_project(path) {
                Ok(p) => p,
                Err(e) => return CallToolResult::tool_error(e),
            };

            let track_id = match args.track_id {
                Some(id) => id,
                None => project
                    .tracks
                    .iter()
                    .find(|t| t.kind == TrackKind::Video)
                    .map(|t| t.id)
                    .unwrap_or_else(|| project.add_track("Video Track 1", TrackKind::Video)),
            };

            let in_point = args.in_point.unwrap_or(0.0);
            let result = match args.start_time {
                Some(timeline_in) => project.add_clip(
                    track_id,
                    &args.file_path,
                    timeline_in,
                    in_point,
                    args.duration,
                ),
                None => project.append_clip(track_id, &args.file_path, in_point, args.duration),
            };

            let clip_id = match result {
                Ok(id) => id,
                Err(e) => return CallToolResult::tool_error(e.to_string()),
            };

            if let Err(e) = project.save_to_file_atomic(path) {
                return CallToolResult::tool_error(e.to_string());
            }

            let (_, clip) = project.find_clip(clip_id).expect("clip was just added");
            CallToolResult::success_json(&serde_json::json!({
                "clip_id": clip_id,
                "track_id": track_id,
                "clip": clip,
                "project_duration": project.duration(),
            }))
        }

        "split_clip" => {
            let args: SplitClipArgs = match serde_json::from_value(args_val) {
                Ok(a) => a,
                Err(e) => return CallToolResult::tool_error(format!("invalid arguments: {e}")),
            };
            let path = args
                .project_path
                .as_deref()
                .unwrap_or(default_project_path);

            let mut project = match load_or_init_project(path) {
                Ok(p) => p,
                Err(e) => return CallToolResult::tool_error(e),
            };

            let (left_clip_id, right_clip_id) =
                match project.split_clip(args.clip_id, args.timestamp) {
                    Ok(ids) => ids,
                    Err(e) => return CallToolResult::tool_error(e.to_string()),
                };

            if let Err(e) = project.save_to_file_atomic(path) {
                return CallToolResult::tool_error(e.to_string());
            }

            CallToolResult::success_json(&serde_json::json!({
                "left_clip_id": left_clip_id,
                "right_clip_id": right_clip_id,
                "split_at": args.timestamp,
                "project_duration": project.duration(),
            }))
        }

        "remove_silence" => {
            let args: RemoveSilenceArgs = match serde_json::from_value(args_val) {
                Ok(a) => a,
                Err(e) => return CallToolResult::tool_error(format!("invalid arguments: {e}")),
            };
            let path = args
                .project_path
                .as_deref()
                .unwrap_or(default_project_path);

            let mut project = match load_or_init_project(path) {
                Ok(p) => p,
                Err(e) => return CallToolResult::tool_error(e),
            };

            let threshold_db = args.threshold_db.unwrap_or(-35.0);
            let min_duration = args.min_silence_duration.unwrap_or(0.4);
            let ripple = args.ripple.unwrap_or(true);

            let timeline_intervals = match args.intervals {
                Some(explicit) => explicit,
                None => {
                    let track = match project.track(args.track_id) {
                        Ok(t) => t,
                        Err(e) => return CallToolResult::tool_error(e.to_string()),
                    };

                    let mut mapped_intervals = Vec::new();
                    for clip in &track.clips {
                        let source_silences = match detect_media_silence(
                            &clip.source_path,
                            threshold_db,
                            min_duration,
                        ) {
                            Ok(s) => s,
                            Err(e) => return CallToolResult::tool_error(e),
                        };

                        for s in source_silences {
                            let clipped_start = s.start.max(clip.in_point);
                            let clipped_end = s.end.min(clip.out_point);
                            if clipped_end > clipped_start {
                                let tl_start = clip.timeline_in
                                    + (clipped_start - clip.in_point) / clip.speed;
                                let tl_end =
                                    clip.timeline_in + (clipped_end - clip.in_point) / clip.speed;
                                mapped_intervals.push(SilenceInterval {
                                    start: tl_start,
                                    end: tl_end,
                                });
                            }
                        }
                    }
                    mapped_intervals
                }
            };

            let removed_segments = match project.remove_silence_intervals(
                args.track_id,
                &timeline_intervals,
                ripple,
            ) {
                Ok(count) => count,
                Err(e) => return CallToolResult::tool_error(e.to_string()),
            };

            if let Err(e) = project.save_to_file_atomic(path) {
                return CallToolResult::tool_error(e.to_string());
            }

            CallToolResult::success_json(&serde_json::json!({
                "track_id": args.track_id,
                "removed_segments": removed_segments,
                "ripple": ripple,
                "project_duration": project.duration(),
            }))
        }

        "inspect_media" => {
            let args: InspectMediaArgs = match serde_json::from_value(args_val) {
                Ok(a) => a,
                Err(e) => return CallToolResult::tool_error(format!("invalid arguments: {e}")),
            };

            let asset = match probe_media(&args.file_path) {
                Ok(asset) => asset,
                Err(err) => return CallToolResult::tool_error(err),
            };

            let project_path = args
                .project_path
                .as_deref()
                .unwrap_or(default_project_path);
            if project_path.exists() {
                if let Ok(mut project) = Project::load_from_file(project_path) {
                    project.upsert_asset(asset.clone());
                    let _ = project.save_to_file_atomic(project_path);
                }
            }

            CallToolResult::success_json(&asset)
        }

        "transcribe_media" => {
            let args: TranscribeMediaArgs = match serde_json::from_value(args_val) {
                Ok(a) => a,
                Err(e) => return CallToolResult::tool_error(format!("invalid arguments: {e}")),
            };

            let project_path = args
                .project_path
                .as_deref()
                .unwrap_or(default_project_path);

            let mut project = match load_or_init_project(project_path) {
                Ok(p) => p,
                Err(e) => return CallToolResult::tool_error(e),
            };

            let media_path = PathBuf::from(&args.file_path);
            let (transcript, engine) =
                match transcribe_media_with_pipeline(&media_path, args.model.as_deref()) {
                    Ok(res) => res,
                    Err(err) => return CallToolResult::tool_error(err.to_string()),
                };

            let inferred_duration = transcript
                .segments
                .last()
                .map(|s| s.end)
                .unwrap_or(0.0);

            let asset_id = if let Ok(probed) = probe_media(&media_path) {
                project.upsert_asset(probed)
            } else {
                project.ensure_asset_for_path(&media_path, inferred_duration)
            };

            if let Err(err) =
                project.attach_transcript_and_save(&asset_id, transcript.clone(), project_path)
            {
                return CallToolResult::tool_error(err.to_string());
            }

            let readable_segments: Vec<String> = transcript
                .segments
                .iter()
                .map(|seg| format!("[{:.2}s - {:.2}s] {}", seg.start, seg.end, seg.text))
                .collect();

            CallToolResult::success_json(&serde_json::json!({
                "asset_id": asset_id,
                "file_path": args.file_path,
                "engine": engine,
                "language": transcript.language,
                "segment_count": transcript.segments.len(),
                "readable_segments": readable_segments,
                "transcript": transcript,
            }))
        }

        "export_project" => {
            let args: ExportProjectArgs = match serde_json::from_value(args_val) {
                Ok(a) => a,
                Err(e) => return CallToolResult::tool_error(format!("invalid arguments: {e}")),
            };
            let path = args
                .project_path
                .as_deref()
                .unwrap_or(default_project_path);

            let project = match load_or_init_project(path) {
                Ok(p) => p,
                Err(e) => return CallToolResult::tool_error(e),
            };

            let execute = args.execute.unwrap_or(false);
            match export_project_with_ffmpeg(&project, &args.output_path, execute) {
                Ok(plan) => CallToolResult::success_json(&plan),
                Err(err) => CallToolResult::tool_error(err),
            }
        }

        unknown => CallToolResult::tool_error(format!("unknown tool: {unknown}")),
    }
}
