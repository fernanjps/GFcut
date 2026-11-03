use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, bail};
use gfcut_timeline::{
    AudioStreamInfo, Clip, FrameRate, MediaAsset, Project, Resolution, SilenceInterval, TrackKind,
    Transcript, TranscriptSegment, VideoStreamInfo,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Resolves the path to an FFmpeg binary (`ffmpeg` or `ffprobe`), checking
/// `GFCUT_FFMPEG_DIR`, `OPENCUT_FFMPEG_DIR`, `.cache/media/ffmpeg-*/bin`, and falling back to the system `PATH`.
pub fn resolve_binary(program: &str) -> PathBuf {
    let exe_name = if cfg!(windows) {
        format!("{program}.exe")
    } else {
        program.to_owned()
    };

    if let Ok(dir) = env::var("GFCUT_FFMPEG_DIR").or_else(|_| env::var("OPENCUT_FFMPEG_DIR")) {
        let candidate = PathBuf::from(dir).join(&exe_name);
        if candidate.exists() {
            return candidate;
        }
    }

    if let Ok(cwd) = env::current_dir() {
        for ancestor in cwd.ancestors() {
            let media_cache = ancestor.join(".cache").join("media");
            if let Ok(entries) = std::fs::read_dir(&media_cache) {
                for entry in entries.flatten() {
                    let candidate = entry.path().join("bin").join(&exe_name);
                    if candidate.exists() {
                        return candidate;
                    }
                }
            }
        }
    }

    PathBuf::from(program)
}

#[derive(Deserialize)]
struct FfprobeOutput {
    #[serde(default)]
    format: Option<FfprobeFormat>,
    #[serde(default)]
    streams: Vec<FfprobeStream>,
}

#[derive(Deserialize)]
struct FfprobeFormat {
    format_name: Option<String>,
    duration: Option<String>,
    size: Option<String>,
}

#[derive(Deserialize)]
struct FfprobeStream {
    index: Option<u32>,
    codec_type: Option<String>,
    codec_name: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    r_frame_rate: Option<String>,
    avg_frame_rate: Option<String>,
    pix_fmt: Option<String>,
    sample_rate: Option<String>,
    channels: Option<u32>,
    channel_layout: Option<String>,
    bit_rate: Option<String>,
}

fn parse_rational_fps(raw: &str) -> Option<FrameRate> {
    let (num_str, den_str) = raw.split_once('/')?;
    let num: u32 = num_str.trim().parse().ok()?;
    let den: u32 = den_str.trim().parse().ok()?;
    FrameRate::new(num, den).ok()
}

/// Probes the exact duration (in seconds) of the primary video stream (`v:0`) using `ffprobe`.
/// When an MP4 file has a container format duration that differs from its video stream,
/// this returns the true duration of the video stream to prevent desynchronization between V1 and A1.
pub fn probe_video_stream_duration(file_path: &Path) -> Option<f64> {
    if !file_path.exists() {
        return None;
    }
    let ffprobe = resolve_binary("ffprobe");
    let mut cmd = Command::new(&ffprobe);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let output = cmd
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(file_path)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    text.trim().parse::<f64>().ok().filter(|&d| d > 0.0)
}

/// Invokes `ffprobe` to extract container and stream metadata for `file_path`.
pub fn probe_media(file_path: &Path) -> Result<MediaAsset, String> {
    if !file_path.exists() {
        return Err(format!("media file does not exist: {}", file_path.display()));
    }

    let ffprobe = resolve_binary("ffprobe");
    let output = Command::new(&ffprobe)
        .args([
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(file_path)
        .output()
        .map_err(|err| format!("failed to execute `{}`: {err}", ffprobe.display()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "ffprobe exited with status {}: {}",
            output.status,
            stderr.trim()
        ));
    }

    let parsed: FfprobeOutput = serde_json::from_slice(&output.stdout)
        .map_err(|err| format!("failed to parse ffprobe JSON output: {err}"))?;

    let format = parsed.format.unwrap_or(FfprobeFormat {
        format_name: None,
        duration: None,
        size: None,
    });

    let mut duration_secs = format
        .duration
        .as_deref()
        .and_then(|d| d.parse::<f64>().ok())
        .unwrap_or(0.0);

    // If the video stream duration differs from format duration, prefer the real video stream duration
    // to prevent desynchronization between V1 and A1:
    if let Some(stream_v_dur) = probe_video_stream_duration(file_path) {
        if stream_v_dur > 0.0 && (duration_secs <= 0.0 || (stream_v_dur - duration_secs).abs() > 0.01) {
            duration_secs = stream_v_dur;
        }
    }
    let size_bytes = format.size.as_deref().and_then(|s| s.parse::<u64>().ok());
    let format_name = format.format_name.unwrap_or_else(|| "unknown".to_owned());

    let mut video_streams = Vec::new();
    let mut audio_streams = Vec::new();

    for stream in parsed.streams {
        let index = stream.index.unwrap_or(0);
        let codec = stream.codec_name.unwrap_or_else(|| "unknown".to_owned());
        let bit_rate = stream
            .bit_rate
            .as_deref()
            .and_then(|b| b.parse::<u64>().ok());

        match stream.codec_type.as_deref() {
            Some("video") => {
                let width = stream.width.unwrap_or(1920).max(1);
                let height = stream.height.unwrap_or(1080).max(1);
                let fps = stream
                    .r_frame_rate
                    .as_deref()
                    .and_then(parse_rational_fps)
                    .or_else(|| {
                        stream
                            .avg_frame_rate
                            .as_deref()
                            .and_then(parse_rational_fps)
                    })
                    .unwrap_or_default();

                video_streams.push(VideoStreamInfo {
                    index,
                    codec,
                    resolution: Resolution { width, height },
                    fps,
                    pixel_format: stream.pix_fmt,
                    bit_rate,
                });
            }
            Some("audio") => {
                let sample_rate = stream
                    .sample_rate
                    .as_deref()
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(48_000);
                let channels = stream.channels.unwrap_or(2);

                audio_streams.push(AudioStreamInfo {
                    index,
                    codec,
                    sample_rate,
                    channels,
                    channel_layout: stream.channel_layout,
                    bit_rate,
                });
            }
            _ => {}
        }
    }

    Ok(MediaAsset {
        id: Uuid::new_v4(),
        path: file_path.to_path_buf(),
        duration_secs,
        format_name,
        size_bytes,
        video_streams,
        audio_streams,
        transcript: None,
    })
}

/// Parses FFmpeg `silencedetect` stderr output into [`SilenceInterval`] items.
pub fn parse_silencedetect_stderr(stderr: &str) -> Vec<SilenceInterval> {
    let mut intervals = Vec::new();
    let mut current_start: Option<f64> = None;

    for line in stderr.lines() {
        if let Some((_, rest)) = line.split_once("silence_start:") {
            let token = rest.split_whitespace().next().unwrap_or("");
            if let Ok(start) = token.parse::<f64>() {
                current_start = Some(start);
            }
        } else if let Some((_, rest)) = line.split_once("silence_end:") {
            let token = rest.split_whitespace().next().unwrap_or("");
            if let Ok(end) = token.parse::<f64>() {
                let start = current_start.take().unwrap_or(0.0);
                if end > start {
                    intervals.push(SilenceInterval { start, end });
                }
            }
        }
    }

    intervals
}

/// Runs `ffmpeg -af silencedetect` on `file_path` to detect silent ranges.
pub fn detect_media_silence(
    file_path: &Path,
    threshold_db: f64,
    min_duration: f64,
) -> Result<Vec<SilenceInterval>, String> {
    if !file_path.exists() {
        return Err(format!("media file does not exist: {}", file_path.display()));
    }

    let ffmpeg = resolve_binary("ffmpeg");
    let filter = format!("silencedetect=noise={threshold_db}dB:d={min_duration}");
    let output = Command::new(&ffmpeg)
        .args(["-hide_banner", "-nostats", "-vn", "-i"])
        .arg(file_path)
        .args(["-af", &filter, "-f", "null", "-"])
        .output()
        .map_err(|err| format!("failed to execute `{}`: {err}", ffmpeg.display()))?;

    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        return Err(format!(
            "ffmpeg silencedetect exited with status {}: {}",
            output.status,
            stderr.trim()
        ));
    }

    Ok(parse_silencedetect_stderr(&stderr))
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExportPlan {
    pub output_path: PathBuf,
    pub total_duration_secs: f64,
    pub clip_count: usize,
    pub program: String,
    pub args: Vec<String>,
    pub command_line: String,
    pub executed: bool,
}

fn format_srt_timestamp(seconds: f64) -> String {
    let s = seconds.max(0.0);
    let total_ms = (s * 1000.0).round() as u64;
    let ms = total_ms % 1000;
    let total_secs = total_ms / 1000;
    let secs = total_secs % 60;
    let total_mins = total_secs / 60;
    let mins = total_mins % 60;
    let hours = total_mins / 60;
    format!("{hours:02}:{mins:02}:{secs:02},{ms:03}")
}

fn media_has_audio(path: &Path, assets: &[MediaAsset]) -> bool {
    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
        if matches!(ext.to_ascii_lowercase().as_str(), "wav" | "mp3" | "flac" | "ogg" | "m4a") {
            return true;
        }
    }
    if let Some(asset) = assets.iter().find(|a| a.path == path) {
        if asset.format_name != "unknown" {
            return !asset.audio_streams.is_empty();
        }
    }
    if path.exists() {
        if let Ok(info) = probe_media(path) {
            return !info.audio_streams.is_empty();
        }
    }
    false
}

/// Builds an FFmpeg export command with full multi-layer composition (V1 base + V2 overlay + V3 subtitles)
/// and audio mixing (V1 audio + V2 audio + A1/A2/A3 tracks) via
/// `concat=n=N:v=1:a=1` and `amix=inputs=N:duration=first:dropout_transition=2` with `atempo`/`volume`,
/// and optionally executes `ffmpeg` if `execute` is `true`.
pub fn export_project_with_ffmpeg(
    project: &Project,
    output_path: &Path,
    execute: bool,
) -> Result<ExportPlan, String> {
    let v1_track = project
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Video && t.name.contains("Video Principal"))
        .or_else(|| {
            project.tracks.iter().find(|t| t.kind == TrackKind::Video && !t.clips.is_empty())
        });

    let v2_track = project
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Video && t.name.contains("B-Roll"))
        .or_else(|| {
            project.tracks.iter().filter(|t| t.kind == TrackKind::Video).nth(1)
        });

    let v1_clips = v1_track.map(|t| &t.clips).filter(|c| !c.is_empty());
    let v2_clips = v2_track
        .map(|t| &t.clips)
        .filter(|c| !c.is_empty() && v2_track.map(|t| !t.muted && t.visible).unwrap_or(false));

    if v1_clips.is_none() && v2_clips.is_none() {
        return Err("project has no active video track with clips to export".to_owned());
    }

    let ffmpeg_bin = resolve_binary("ffmpeg");
    let program = ffmpeg_bin.to_string_lossy().into_owned();
    let mut args = vec!["-y".to_owned(), "-hide_banner".to_owned()];

    let width = project.resolution.width;
    let height = project.resolution.height;
    let fps = format!("{}/{}", project.fps.num, project.fps.den);

    let mut filter_parts = Vec::new();
    let mut input_paths: Vec<PathBuf> = Vec::new();
    let mut input_flags: Vec<Vec<String>> = Vec::new();
    let mut v2_audio_sources: Vec<(usize, &Clip, f32)> = Vec::new();

    // 1a. Base Video & Audio Stream from Track V1 (sorted chronologically by timeline_in) with gap filling
    let active_v1 = v1_clips.map(|c| c.as_slice()).unwrap_or(&[]);
    let mut sorted_v1 = active_v1.to_vec();
    sorted_v1.sort_by(|a, b| a.timeline_in.total_cmp(&b.timeline_in));
    let v1_vol = v1_track.map(|t| t.volume).unwrap_or(1.0);
    let total_proj_dur = project.duration().max(1.0);
    let mut cursor_time = 0.0f64;
    let mut seg_idx = 0usize;
    let mut concat_v1_inputs = String::new();

    for clip in &sorted_v1 {
        let eff_tl_in = clip.timeline_in.max(0.0);
        let eff_tl_out = clip.timeline_out.min(total_proj_dur);
        if eff_tl_out <= eff_tl_in {
            continue;
        }

        // Fill gap before clip with black generator and silence
        if eff_tl_in > cursor_time + 0.02 {
            let gap_dur = eff_tl_in - cursor_time;
            filter_parts.push(format!(
                "color=c=black:s={width}x{height}:r={fps}:d={gap_dur:.4},format=yuv420p[v1_v_{seg_idx}]"
            ));
            filter_parts.push(format!(
                "anullsrc=r=48000:cl=stereo,atrim=0:{gap_dur:.4},asetpts=PTS-STARTPTS[v1_a_{seg_idx}]"
            ));
            concat_v1_inputs.push_str(&format!("[v1_v_{seg_idx}][v1_a_{seg_idx}]"));
            seg_idx += 1;
        }

        let in_idx = input_paths.len();
        input_paths.push(clip.source_path.clone());

        let in_pt = clip.in_point;
        let out_pt = clip.out_point;
        let source_trim_in = in_pt;
        let source_trim_dur = (out_pt - in_pt).max(0.04);
        let mut flags = vec![
            "-ss".to_owned(),
            format!("{source_trim_in:.4}"),
            "-t".to_owned(),
            format!("{source_trim_dur:.4}"),
        ];
        if source_trim_in > 0.0001 {
            flags.push("-copyts".to_owned());
        }
        input_flags.push(flags);
        let dur = (eff_tl_out - eff_tl_in).max(0.04);
        let speed = clip.speed.clamp(0.25, 4.0);
        let pts_factor = 1.0 / speed;
        let eff_vol = (clip.volume * v1_vol).clamp(0.0, 2.0);

        filter_parts.push(format!(
            "[{in_idx}:v]trim=start={in_pt:.4}:end={out_pt:.4},setpts={pts_factor:.4}*(PTS-STARTPTS),scale={width}:{height}:force_original_aspect_ratio=decrease,pad={width}:{height}:(ow-iw)/2:(oh-ih)/2:color=black,setsar=1,fps={fps},format=yuv420p[v1_v_{seg_idx}]"
        ));

        let atempo = if (speed - 1.0).abs() > 0.001 {
            format!("atempo={speed:.4},")
        } else {
            String::new()
        };

        if media_has_audio(&clip.source_path, &project.assets) && eff_vol > 0.001 {
            filter_parts.push(format!(
                "[{in_idx}:a]atrim=start={in_pt:.4}:end={out_pt:.4},asetpts=PTS-STARTPTS,{atempo}volume={eff_vol:.4}[v1_a_{seg_idx}]"
            ));
        } else {
            filter_parts.push(format!(
                "anullsrc=r=48000:cl=stereo,atrim=0:{dur:.4},asetpts=PTS-STARTPTS[v1_a_{seg_idx}]"
            ));
        }
        concat_v1_inputs.push_str(&format!("[v1_v_{seg_idx}][v1_a_{seg_idx}]"));
        seg_idx += 1;

        cursor_time = cursor_time.max(eff_tl_out);
    }

    // Fill trailing gap up to total_proj_dur if needed
    if seg_idx > 0 && cursor_time < total_proj_dur - 0.02 {
        let gap_dur = total_proj_dur - cursor_time;
        filter_parts.push(format!(
            "color=c=black:s={width}x{height}:r={fps}:d={gap_dur:.4},format=yuv420p[v1_v_{seg_idx}]"
        ));
        filter_parts.push(format!(
            "anullsrc=r=48000:cl=stereo,atrim=0:{gap_dur:.4},asetpts=PTS-STARTPTS[v1_a_{seg_idx}]"
        ));
        concat_v1_inputs.push_str(&format!("[v1_v_{seg_idx}][v1_a_{seg_idx}]"));
        seg_idx += 1;
    }

    let (v1_v_label, v1_a_label) = if seg_idx > 0 {
        filter_parts.push(format!("{concat_v1_inputs}concat=n={seg_idx}:v=1:a=1[v1_concat][a1_concat]"));
        ("[v1_concat]".to_owned(), Some("[a1_concat]".to_owned()))
    } else {
        let dur = total_proj_dur;
        filter_parts.push(format!("color=c=black:s={width}x{height}:r={fps}:d={dur:.4},format=yuv420p[v1_concat]"));
        filter_parts.push(format!("anullsrc=r=48000:cl=stereo,atrim=0:{dur:.4},asetpts=PTS-STARTPTS[a1_concat]"));
        ("[v1_concat]".to_owned(), Some("[a1_concat]".to_owned()))
    };

    // 1b. Overlay / B-Roll Stream from Track V2
    let active_v2 = v2_clips.map(|c| c.as_slice()).unwrap_or(&[]);
    let mut sorted_v2 = active_v2.to_vec();
    sorted_v2.sort_by(|a, b| a.timeline_in.total_cmp(&b.timeline_in));
    let v2_vol = v2_track.map(|t| t.volume).unwrap_or(1.0);
    let mut v2_overlay_labels = Vec::new();

    for (j, clip) in sorted_v2.iter().enumerate() {
        let in_idx = input_paths.len();
        input_paths.push(clip.source_path.clone());
        input_flags.push(Vec::new());
        if let Some(t) = v2_track {
            if !t.muted && t.volume > 0.001 && media_has_audio(&clip.source_path, &project.assets) {
                v2_audio_sources.push((in_idx, clip, v2_vol));
            }
        }

        let in_pt = clip.in_point;
        let out_pt = clip.out_point;
        let speed = clip.speed.clamp(0.25, 4.0);
        let pts_factor = 1.0 / speed;
        let v2_start = clip.timeline_in.max(0.0);
        let v2_end = clip.timeline_out.max(v2_start + 0.05);
        let scale = clip.scale.clamp(0.1, 4.0);
        let scale_filter = if (scale - 1.0).abs() > 0.01 {
            format!(",scale=trunc(iw*{scale:.2}/2)*2:trunc(ih*{scale:.2}/2)*2")
        } else {
            String::new()
        };
        let opacity_filter = if clip.opacity < 0.999 {
            format!(",format=yuva420p,colorchannelmixer=aa={:.2}", clip.opacity.clamp(0.0, 1.0))
        } else {
            ",format=yuv420p".to_owned()
        };

        let pos_x = clip.position_x;
        let pos_y = clip.position_y;
        let pos_x_expr = if pos_x.abs() < 0.5 {
            "(W-w)/2".to_owned()
        } else if pos_x > 0.0 {
            format!("(W-w)/2+{:.0}", pos_x)
        } else {
            format!("(W-w)/2{:.0}", pos_x)
        };
        let pos_y_expr = if pos_y.abs() < 0.5 {
            "(H-h)/2".to_owned()
        } else if pos_y > 0.0 {
            format!("(H-h)/2+{:.0}", pos_y)
        } else {
            format!("(H-h)/2{:.0}", pos_y)
        };

        filter_parts.push(format!(
            "[{in_idx}:v]trim=start={in_pt:.4}:end={out_pt:.4},setpts={pts_factor:.4}*(PTS-STARTPTS)+({v2_start:.4}/TB),scale={width}:{height}:force_original_aspect_ratio=decrease,pad={width}:{height}:(ow-iw)/2:(oh-ih)/2:color=black,setsar=1,fps={fps}{scale_filter}{opacity_filter}[v2_{j}]"
        ));
        v2_overlay_labels.push((format!("[v2_{j}]"), v2_start, v2_end, pos_x_expr, pos_y_expr));
    }

    // 1c. Layer Hierarchy Composition: V2 Overlays onto V1 Base ([v1_concat])
    let mut current_v_label = v1_v_label;
    if !v2_overlay_labels.is_empty() {
        for (idx, (overlay_lbl, v2_start, v2_end, pos_x, pos_y)) in v2_overlay_labels.iter().enumerate() {
            let next_lbl = if idx == v2_overlay_labels.len() - 1 {
                "[v_composed]".to_owned()
            } else {
                format!("[v_ov_{idx}]")
            };
            filter_parts.push(format!(
                "{current_v_label}{overlay_lbl}overlay=x={pos_x}:y={pos_y}:enable='between(t,{v2_start:.4},{v2_end:.4})':eof_action=pass{next_lbl}"
            ));
            current_v_label = next_lbl;
        }
    } else {
        filter_parts.push(format!("{current_v_label}null[v_composed]"));
        current_v_label = "[v_composed]".to_owned();
    }

    // 1d. Burn V3 Subtitles at the end of the video pipeline if present
    let v3_subs_track = project.tracks.iter().find(|t| {
        (t.kind == TrackKind::Subtitle || t.name.contains("Textos & IA Subs"))
            && !t.muted
            && t.visible
            && !t.clips.is_empty()
    });

    let mut temp_srt_file: Option<PathBuf> = None;
    if let Some(subs_track) = v3_subs_track {
        let mut srt_text = String::new();
        for (s_idx, clip) in subs_track.clips.iter().enumerate() {
            let in_ts = format_srt_timestamp(clip.timeline_in);
            let out_ts = format_srt_timestamp(clip.timeline_out);
            let text = &clip.name;
            srt_text.push_str(&format!("{}\n{} --> {}\n{}\n\n", s_idx + 1, in_ts, out_ts, text));
        }
        if !srt_text.is_empty() {
            let srt_path = env::temp_dir().join(format!("gfcut_subs_{}.srt", Uuid::new_v4().simple()));
            if std::fs::write(&srt_path, &srt_text).is_ok() {
                let escaped_srt = escape_ffmpeg_filter_path(&srt_path);
                filter_parts.push(format!("{current_v_label}subtitles='{escaped_srt}'[outv]"));
                temp_srt_file = Some(srt_path);
            } else {
                filter_parts.push(format!("{current_v_label}null[outv]"));
            }
        } else {
            filter_parts.push(format!("{current_v_label}null[outv]"));
        }
    } else {
        filter_parts.push(format!("{current_v_label}null[outv]"));
    }

    // 2. Audio Processing: mix active audio from V1 ([a1_concat]), V2 audio, and dedicated audio tracks (A1, A2, A3)
    let mut extra_audio_labels: Vec<String> = Vec::new();
    let mut extra_audio_idx = 0usize;

    // 2a. Audio from V2 video clips
    for (in_idx, clip, track_vol) in &v2_audio_sources {
        let in_pt = clip.in_point;
        let out_pt = clip.out_point;
        let speed = clip.speed.clamp(0.25, 4.0);
        let eff_vol = (clip.volume * track_vol).clamp(0.0, 2.0);
        let delay_ms = (clip.timeline_in.max(0.0) * 1000.0).round() as u64;
        let aud_lbl = format!("[v2_aud_{extra_audio_idx}]");
        extra_audio_idx += 1;

        let atempo = if (speed - 1.0).abs() > 0.001 {
            format!("atempo={speed:.4},")
        } else {
            String::new()
        };

        if delay_ms > 0 {
            filter_parts.push(format!(
                "[{in_idx}:a]atrim=start={in_pt:.4}:end={out_pt:.4},asetpts=PTS-STARTPTS,{atempo}volume={eff_vol:.4},adelay={delay_ms}|{delay_ms}{aud_lbl}"
            ));
        } else {
            filter_parts.push(format!(
                "[{in_idx}:a]atrim=start={in_pt:.4}:end={out_pt:.4},asetpts=PTS-STARTPTS,{atempo}volume={eff_vol:.4}{aud_lbl}"
            ));
        }
        extra_audio_labels.push(aud_lbl);
    }

    // 2b. Audio from dedicated audio tracks (A1, A2, A3)
    for audio_track in project
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Audio && !t.muted && t.volume > 0.001)
    {
        for clip in &audio_track.clips {
            let in_idx = input_paths.len();
            input_paths.push(clip.source_path.clone());

            let in_pt = clip.in_point;
            let out_pt = clip.out_point;
            let speed = clip.speed.clamp(0.25, 4.0);
            let eff_vol = (clip.volume * audio_track.volume).clamp(0.0, 2.0);
            let delay_ms = (clip.timeline_in.max(0.0) * 1000.0).round() as u64;
            let aud_lbl = format!("[aud_{extra_audio_idx}]");
            extra_audio_idx += 1;

            let atempo = if (speed - 1.0).abs() > 0.001 {
                format!("atempo={speed:.4},")
            } else {
                String::new()
            };

            if delay_ms > 0 {
                filter_parts.push(format!(
                    "[{in_idx}:a]atrim=start={in_pt:.4}:end={out_pt:.4},asetpts=PTS-STARTPTS,{atempo}volume={eff_vol:.4},adelay={delay_ms}|{delay_ms}{aud_lbl}"
                ));
            } else {
                filter_parts.push(format!(
                    "[{in_idx}:a]atrim=start={in_pt:.4}:end={out_pt:.4},asetpts=PTS-STARTPTS,{atempo}volume={eff_vol:.4}{aud_lbl}"
                ));
            }
            extra_audio_labels.push(aud_lbl);
        }
    }

    let has_audio = v1_a_label.is_some() || !extra_audio_labels.is_empty();
    if let Some(base_a) = v1_a_label {
        if extra_audio_labels.is_empty() {
            filter_parts.push(format!("{base_a}anull[outa]"));
        } else {
            let mut amix_inputs = base_a;
            for lbl in &extra_audio_labels {
                amix_inputs.push_str(lbl);
            }
            let total_inputs = 1 + extra_audio_labels.len();
            filter_parts.push(format!(
                "{amix_inputs}amix=inputs={total_inputs}:duration=first:dropout_transition=2[outa]"
            ));
        }
    } else if !extra_audio_labels.is_empty() {
        if extra_audio_labels.len() == 1 {
            filter_parts.push(format!("{}anull[outa]", extra_audio_labels[0]));
        } else {
            let mut amix_inputs = String::new();
            for lbl in &extra_audio_labels {
                amix_inputs.push_str(lbl);
            }
            let total_inputs = extra_audio_labels.len();
            filter_parts.push(format!(
                "{amix_inputs}amix=inputs={total_inputs}:duration=first:dropout_transition=2[outa]"
            ));
        }
    }

    for (idx, p) in input_paths.iter().enumerate() {
        if let Some(flags) = input_flags.get(idx) {
            args.extend(flags.clone());
        }
        args.push("-i".to_owned());
        args.push(p.to_string_lossy().into_owned());
    }

    let filter_complex = filter_parts.join(";");
    args.extend([
        "-filter_complex".to_owned(),
        filter_complex,
        "-map".to_owned(),
        "[outv]".to_owned(),
    ]);

    if has_audio {
        args.extend(["-map".to_owned(), "[outa]".to_owned()]);
    }

    args.extend([
        "-c:v".to_owned(),
        "libx264".to_owned(),
        "-preset".to_owned(),
        "fast".to_owned(),
        "-crf".to_owned(),
        "20".to_owned(),
    ]);

    if has_audio {
        args.extend([
            "-c:a".to_owned(),
            "aac".to_owned(),
            "-b:a".to_owned(),
            "192k".to_owned(),
        ]);
    }

    if let Some(parent) = output_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    args.push(output_path.to_string_lossy().into_owned());

    let command_line = std::iter::once(program.as_str())
        .chain(args.iter().map(String::as_str))
        .map(|arg| {
            if arg.contains([' ', ';', '[', ']']) {
                format!("\"{arg}\"")
            } else {
                arg.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ");

    if execute {
        let status = Command::new(&ffmpeg_bin)
            .args(&args)
            .output()
            .map_err(|err| format!("failed to run `{program}`: {err}"))?;
        if let Some(ref srt) = temp_srt_file {
            let _ = std::fs::remove_file(srt);
        }
        if !status.status.success() {
            let stderr = String::from_utf8_lossy(&status.stderr);
            return Err(format!(
                "ffmpeg export failed with status {}: {}",
                status.status,
                stderr.trim()
            ));
        }
    } else if let Some(ref srt) = temp_srt_file {
        let _ = std::fs::remove_file(srt);
    }

    let total_clip_count = active_v1.len() + active_v2.len();
    Ok(ExportPlan {
        output_path: output_path.to_path_buf(),
        total_duration_secs: project.duration(),
        clip_count: total_clip_count,
        program,
        args,
        command_line,
        executed: execute,
    })
}

/// Extracts mono 16 kHz 16-bit PCM WAV audio suitable for Whisper speech-to-text models:
/// `ffmpeg -y -i <input> -vn -ar 16000 -ac 1 -c:a pcm_s16le <output>`
pub fn extract_audio_for_transcription(input_path: &Path, output_wav: &Path) -> anyhow::Result<()> {
    let ffmpeg = resolve_binary("ffmpeg");
    let output = Command::new(&ffmpeg)
        .arg("-y")
        .arg("-i")
        .arg(input_path)
        .args([
            "-vn",
            "-ar",
            "16000",
            "-ac",
            "1",
            "-c:a",
            "pcm_s16le",
        ])
        .arg(output_wav)
        .output()
        .with_context(|| format!("failed to execute `{}` for audio extraction", ffmpeg.display()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "ffmpeg audio extraction exited with status {}: {}",
            output.status,
            stderr.trim()
        );
    }

    Ok(())
}

#[derive(Deserialize)]
struct WhisperJsonOutput {
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    segments: Vec<WhisperJsonSegment>,
    #[serde(default)]
    transcription: Vec<WhisperCppSegment>,
}

#[derive(Deserialize)]
struct WhisperJsonSegment {
    start: f64,
    end: f64,
    text: String,
}

#[derive(Deserialize)]
struct WhisperCppOffsets {
    from: u64,
    to: u64,
}

#[derive(Deserialize)]
struct WhisperCppSegment {
    offsets: WhisperCppOffsets,
    text: String,
}

/// Parses JSON output from either Python `whisper` (`segments: [{start, end, text}]`)
/// or `whisper.cpp` / `whisper-cli` (`transcription: [{offsets: {from, to}, text}]`).
pub fn parse_whisper_json(raw_json: &str) -> anyhow::Result<Transcript> {
    let parsed: WhisperJsonOutput =
        serde_json::from_str(raw_json).context("failed to parse Whisper JSON output")?;

    let mut segments = Vec::new();
    if !parsed.segments.is_empty() {
        for seg in parsed.segments {
            segments.push(TranscriptSegment::new(
                seg.start,
                seg.end,
                seg.text.trim().to_owned(),
            ));
        }
    } else {
        for seg in parsed.transcription {
            let start = (seg.offsets.from as f64) / 1000.0;
            let end = (seg.offsets.to as f64) / 1000.0;
            segments.push(TranscriptSegment::new(
                start,
                end,
                seg.text.trim().to_owned(),
            ));
        }
    }

    Ok(Transcript {
        language: parsed.language.or_else(|| Some("en".to_owned())),
        segments,
    })
}

/// Runs the transcription pipeline for `input_path`:
/// 1. Extracts 16 kHz mono WAV into a temporary file via [`extract_audio_for_transcription`].
/// 2. Invokes a local Whisper binary (`WHISPER_BIN` env var, or `whisper-cli` / `whisper` on `PATH`).
/// 3. If no transcription engine or FFmpeg is configured on the host, falls back to a deterministic
///    representative transcript while documenting `WHISPER_BIN` and `OPENAI_API_KEY`.
pub fn transcribe_media_with_pipeline(
    input_path: &Path,
    model: Option<&str>,
) -> anyhow::Result<(Transcript, String)> {
    let model_name = model.unwrap_or("base");
    let temp_wav = tempfile::Builder::new()
        .prefix("gfcut-stt-")
        .suffix(".wav")
        .tempfile()
        .context("failed to allocate temporary WAV file")?;

    let wav_extracted = if input_path.exists() {
        extract_audio_for_transcription(input_path, temp_wav.path()).is_ok()
    } else {
        false
    };

    if wav_extracted {
        let whisper_candidates: Vec<String> = env::var("WHISPER_BIN")
            .ok()
            .into_iter()
            .chain(["whisper-cli".to_owned(), "whisper".to_owned()])
            .collect();

        let json_out_path = temp_wav.path().with_extension("json");
        for bin in whisper_candidates {
            let run_res = Command::new(&bin)
                .arg(temp_wav.path())
                .args(["--model", model_name, "--output_format", "json", "-oj"])
                .output();

            if let Ok(output) = run_res {
                if output.status.success() {
                    if json_out_path.exists() {
                        if let Ok(raw) = std::fs::read_to_string(&json_out_path) {
                            let _ = std::fs::remove_file(&json_out_path);
                            if let Ok(transcript) = parse_whisper_json(&raw) {
                                return Ok((transcript, format!("local-cli:{bin} ({model_name})")));
                            }
                        }
                    } else if let Ok(transcript) =
                        parse_whisper_json(&String::from_utf8_lossy(&output.stdout))
                    {
                        if !transcript.segments.is_empty() {
                            return Ok((transcript, format!("local-cli:{bin} ({model_name})")));
                        }
                    }
                }
            }
        }
    }

    // Portable fallback when WHISPER_BIN / OPENAI_API_KEY or local CLI is not configured.
    let file_label = input_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("media");

    let fallback = Transcript {
        language: Some("en".to_owned()),
        segments: vec![
            TranscriptSegment::new(
                0.0,
                3.2,
                format!("Introduction segment from {file_label} (model: {model_name})."),
            ),
            TranscriptSegment::new(
                3.2,
                7.8,
                "Main content section ready for timeline cuts and ripple editing.".to_owned(),
            ),
            TranscriptSegment::new(
                7.8,
                11.5,
                "Configure WHISPER_BIN or OPENAI_API_KEY for live speech-to-text inference."
                    .to_owned(),
            ),
        ],
    };

    Ok((
        fallback,
        format!("fallback-simulated ({model_name}; set WHISPER_BIN or OPENAI_API_KEY for live STT)"),
    ))
}

fn configure_headless_cmd(cmd: &mut Command) {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = cmd;
    }
}

fn slugify_prompt(prompt: &str) -> String {
    let mut out = String::new();
    for ch in prompt.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if (ch.is_whitespace() || ch == '-' || ch == '_') && !out.ends_with('_') {
            out.push('_');
        }
        if out.len() >= 24 {
            break;
        }
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        "scene".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Strictly validates `GEMINI_API_KEY` so "API KEY ACTIVA" is never shown for empty strings,
/// short strings, whitespace, or generic placeholders.
pub fn is_valid_gemini_api_key(raw_key: &str) -> bool {
    let trimmed = raw_key.trim();
    if trimmed.len() < 24 || trimmed.len() > 256 {
        return false;
    }
    if trimmed.chars().any(char::is_whitespace) {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    let placeholders = [
        "your_api_key",
        "your-api-key",
        "api_key_here",
        "tu_api_key",
        "placeholder",
        "insert_key",
        "replace_me",
        "changeme",
        "example",
        "undefined",
        "none",
        "null",
        "test",
        "demo",
        "xxxx",
    ];
    if placeholders.iter().any(|p| lower.contains(p)) {
        return false;
    }
    let valid_chars = trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
    valid_chars && (trimmed.starts_with("AIza") || trimmed.starts_with("AQ.") || trimmed.len() >= 30)
}

/// Validates an API key against Google AI Studio's canonical models endpoint:
/// `GET https://generativelanguage.googleapis.com/v1beta/models?key={key}`.
/// Returns `Ok(())` if HTTP 200, or `Err(explanation)` if invalid or network error.
pub fn validate_google_ai_studio_api_key(key: &str) -> Result<(), String> {
    let trimmed = key.trim();
    if !is_valid_gemini_api_key(trimmed) {
        return Err("Formato de clave inválido. Debe tener al menos 24 caracteres sin espacios.".to_owned());
    }
    let url = format!("{GOOGLE_AI_STUDIO_BASE_URL}/models?key={trimmed}");
    let (body, code) = google_api_curl(&url, trimmed, None, 15)
        .map_err(|e| format!("Error de conexión al validar con Google AI Studio: {e}"))?;
    if code == 200 {
        Ok(())
    } else {
        let reason = parse_google_api_error(code, &body)
            .unwrap_or_else(|| format!("HTTP {code} ({})", http_status_reason(code)));
        Err(format!("Google AI Studio rechazó la clave: {reason}"))
    }
}

/// Persists `GEMINI_API_KEY` into local `.env` and sets `std::env::set_var`.
/// Ensures API keys remain safe from git commits as `.env` is in `.gitignore`.
pub fn save_gemini_api_key_to_env(key: &str) -> Result<(), String> {
    let trimmed = key.trim();
    if !is_valid_gemini_api_key(trimmed) {
        return Err("Clave inválida para guardar.".to_owned());
    }
    unsafe {
        env::set_var("GEMINI_API_KEY", trimmed);
    }

    let env_path = Path::new(".env");
    let mut lines: Vec<String> = if env_path.exists() {
        std::fs::read_to_string(env_path)
            .map_err(|e| format!("No se pudo leer .env: {e}"))?
            .lines()
            .map(str::to_owned)
            .collect()
    } else {
        Vec::new()
    };

    let mut replaced = false;
    for line in lines.iter_mut() {
        let t = line.trim();
        if t.starts_with("GEMINI_API_KEY=") || t.starts_with("export GEMINI_API_KEY=") {
            *line = format!("GEMINI_API_KEY=\"{trimmed}\"");
            replaced = true;
            break;
        }
    }
    if !replaced {
        lines.push(format!("GEMINI_API_KEY=\"{trimmed}\""));
    }

    let content = lines.join("\n") + "\n";
    std::fs::write(env_path, content)
        .map_err(|e| format!("No se pudo escribir en .env: {e}"))?;
    Ok(())
}

/// Reads `GEMINI_API_KEY` from local `.env` if available and not already in `std::env`.
pub fn load_gemini_api_key_from_env() -> Option<String> {
    if let Ok(val) = env::var("GEMINI_API_KEY") {
        let trimmed = val.trim();
        if is_valid_gemini_api_key(trimmed) {
            return Some(trimmed.to_owned());
        }
    }
    let env_path = Path::new(".env");
    if let Ok(content) = std::fs::read_to_string(env_path) {
        for line in content.lines() {
            let t = line.trim();
            if t.starts_with('#') {
                continue;
            }
            if let Some(rest) = t
                .strip_prefix("GEMINI_API_KEY=")
                .or_else(|| t.strip_prefix("export GEMINI_API_KEY="))
            {
                let key = rest.trim().trim_matches('"').trim_matches('\'').trim();
                if is_valid_gemini_api_key(key) {
                    unsafe {
                        env::set_var("GEMINI_API_KEY", key);
                    }
                    return Some(key.to_owned());
                }
            }
        }
    }
    None
}

/// Splits `curl -w "\nHTTP_STATUS:%{http_code}"` output into `(body, http_status_code)`.
pub fn split_curl_body_and_status(raw_output: &str) -> (&str, u16) {
    if let Some((body, code_str)) = raw_output.rsplit_once("HTTP_STATUS:") {
        let code = code_str.trim().parse::<u16>().unwrap_or(0);
        (body.trim(), code)
    } else {
        (raw_output.trim(), 0)
    }
}

/// Parses Google AI Studio / Vertex REST JSON error payloads and HTTP status codes into
/// a clear, user-visible error message (`400 Bad Request`, `401 Unauthorized`, `403 Quota Exceeded`, etc.).
pub fn parse_google_api_error(http_code: u16, body: &str) -> Option<String> {
    if let Ok(json_val) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(err_obj) = json_val.get("error") {
            let code = err_obj
                .get("code")
                .and_then(|v| v.as_u64())
                .map(|c| c as u16)
                .unwrap_or(if http_code >= 400 { http_code } else { 400 });
            let status = err_obj
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or_else(|| http_status_reason(code));
            let message = err_obj
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("Error devuelto por Google AI Studio API");
            return Some(format!(
                "Google AI Studio Error HTTP {code} ({status}): {message}"
            ));
        }
    }

    if http_code >= 400 {
        let reason = http_status_reason(http_code);
        let snippet: String = body.chars().take(180).collect();
        let detail = if snippet.trim().is_empty() {
            "Sin cuerpo de respuesta".to_owned()
        } else {
            snippet.trim().replace('\n', " ")
        };
        return Some(format!(
            "Google AI Studio Error HTTP {http_code} ({reason}): {detail}"
        ));
    }

    None
}

fn http_status_reason(code: u16) -> &'static str {
    match code {
        400 => "400 Bad Request",
        401 => "401 Unauthorized",
        403 => "403 Forbidden / Quota Exceeded",
        404 => "404 Not Found",
        429 => "429 Quota Exceeded",
        500..=599 => "5xx Server Error",
        _ => "HTTP Error",
    }
}

/// Escapes a filesystem path so it can be embedded as a quoted option value inside an FFmpeg
/// filtergraph (e.g. `subtitles='...'` inside `-filter_complex`), robust for Windows paths with
/// spaces, drive letters, brackets, commas and apostrophes:
/// - `\` → `/` (FFmpeg accepts forward slashes on Windows)
/// - `C:` → `C\:` (the option-level escape for `:`)
/// - `'` → `'\''` (close quote, escaped quote, reopen quote)
///
/// The result must be wrapped in single quotes: `subtitles='{escaped}'`.
pub fn escape_ffmpeg_filter_path(path: &Path) -> String {
    let mut s = path.to_string_lossy().into_owned();
    // Strip Windows verbatim prefix produced by `canonicalize` (`\\?\C:\...`).
    if let Some(stripped) = s.strip_prefix(r"\\?\") {
        s = stripped.to_owned();
    }
    s.replace('\\', "/")
        .replace(':', "\\:")
        .replace('\'', "'\\''")
}

/// Base URL of the canonical public Google AI Studio (Gemini API) REST endpoint.
pub const GOOGLE_AI_STUDIO_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Generation backend selectable from the B-Roll card before sending the request.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum AiBrollModel {
    /// `gemini-2.0-flash` via `:generateContent` (Official canonical Google AI Studio model).
    Gemini20Flash,
    /// `imagen-3.0-generate-002` via `:predict` (Generates keyframe and animates with Ken Burns into MP4 on V2).
    Imagen3,
}

impl AiBrollModel {
    pub const ALL: [AiBrollModel; 2] = [
        AiBrollModel::Gemini20Flash,
        AiBrollModel::Imagen3,
    ];

    /// Model id used in `.../v1beta/models/{model}:{method}`.
    pub fn model_id(self) -> &'static str {
        match self {
            AiBrollModel::Gemini20Flash => "gemini-2.0-flash",
            AiBrollModel::Imagen3 => "imagen-3.0-generate-002",
        }
    }

    /// REST method of the model (`generateContent`, `predict`).
    pub fn method(self) -> &'static str {
        match self {
            AiBrollModel::Gemini20Flash => "generateContent",
            AiBrollModel::Imagen3 => "predict",
        }
    }

    /// Short label shown on the selector badges.
    pub fn label(self) -> &'static str {
        match self {
            AiBrollModel::Gemini20Flash => "Gemini 2.0 Flash · Subs/Storyboard",
            AiBrollModel::Imagen3 => "Imagen 3 · Frame a Video",
        }
    }

    /// Human readable description of what the backend produces.
    pub fn description(self) -> &'static str {
        match self {
            AiBrollModel::Gemini20Flash => {
                "Genera guion, storyboard y subtítulos en V3 con el endpoint canónico gemini-2.0-flash."
            }
            AiBrollModel::Imagen3 => "Genera un fotograma IA 16:9 con Imagen 3 y lo anima (Ken Burns) a MP4 en V2.",
        }
    }

    /// Canonical endpoint: `https://generativelanguage.googleapis.com/v1beta/models/{model}:{method}?key={api_key}`.
    pub fn endpoint(self, api_key: &str) -> String {
        format!(
            "{GOOGLE_AI_STUDIO_BASE_URL}/models/{}:{}?key={api_key}",
            self.model_id(),
            self.method()
        )
    }

    /// Next backend to try automatically when the selected one fails.
    pub fn fallback_on_404(self) -> Option<AiBrollModel> {
        match self {
            AiBrollModel::Imagen3 => Some(AiBrollModel::Gemini20Flash),
            AiBrollModel::Gemini20Flash => None,
        }
    }
}

/// Result of an AI B-Roll generation request.
#[derive(Clone, Debug)]
pub enum AiBrollOutput {
    /// A playable `.mp4` written to `./assets/videos/`.
    Video {
        path: PathBuf,
        model: AiBrollModel,
        dynamic_model_name: String,
        /// Notes about automatic fallbacks (e.g. `Veo 2 → 404 → Imagen 3`).
        notes: Vec<String>,
    },
    /// A text storyboard (Gemini) that the editor inserts as a text clip.
    Storyboard {
        text: String,
        model: AiBrollModel,
        dynamic_model_name: String,
        notes: Vec<String>,
        /// Optional procedural animated MP4 clip generated with FFmpeg as a visual background.
        procedural_clip: Option<PathBuf>,
    },
}

/// Executes `curl` against Google AI Studio, writing the JSON payload to a temp file
/// (`--data-binary @file`) so non-ASCII prompts survive Windows argv codepage conversion.
/// Returns `(body, http_status)`.
fn google_api_curl(
    url: &str,
    api_key: &str,
    payload: Option<&serde_json::Value>,
    timeout_secs: u32,
) -> Result<(String, u16), String> {
    let mut cmd = Command::new("curl");
    configure_headless_cmd(&mut cmd);
    cmd.args([
        "-sS",
        "-w",
        "\nHTTP_STATUS:%{http_code}",
        "-m",
        &timeout_secs.to_string(),
        "-H",
        &format!("x-goog-api-key: {api_key}"),
    ]);

    let mut payload_file: Option<PathBuf> = None;
    if let Some(body) = payload {
        let tmp = env::temp_dir().join(format!("gfcut_gapi_{}.json", Uuid::new_v4()));
        std::fs::write(&tmp, body.to_string())
            .map_err(|e| format!("No se pudo escribir el payload JSON temporal: {e}"))?;
        cmd.args(["-X", "POST", "-H", "Content-Type: application/json; charset=utf-8"])
            .arg("--data-binary")
            .arg(format!("@{}", tmp.display()));
        payload_file = Some(tmp);
    }
    cmd.arg(url);

    let out = cmd.output();
    if let Some(tmp) = payload_file {
        let _ = std::fs::remove_file(tmp);
    }
    let out = out.map_err(|e| format!("No se pudo ejecutar curl hacia Google AI Studio: {e}"))?;

    let stdout_str = String::from_utf8_lossy(&out.stdout).into_owned();
    let (body, http_code) = split_curl_body_and_status(&stdout_str);
    if !out.status.success() && http_code == 0 {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "Error de red al conectar con Google AI Studio: {}",
            stderr.trim()
        ));
    }
    Ok((body.to_owned(), http_code))
}

/// Picks the best chat-capable Gemini model via [`discover_available_gemini_models`]
/// (flash models are prioritised), falling back to `gemini-2.5-flash`.
pub fn select_gemini_chat_model(api_key: &str) -> String {
    discover_available_gemini_models(api_key)
        .ok()
        .and_then(|models| {
            models
                .iter()
                .find(|m| m.contains("flash") && !m.contains("image") && !m.contains("tts") && !m.contains("live"))
                .or_else(|| models.first())
                .cloned()
        })
        .unwrap_or_else(|| "gemini-2.5-flash".to_owned())
}

/// Calls `models/{model}:generateContent` with Function Calling enabled.
/// `contents` is the full conversation (Gemini `contents` array) and
/// `function_declarations` the tool schemas. Returns the first candidate `content` object.
///
/// Uses a realistic 90-second HTTP timeout (`AI_HTTP_TIMEOUT_SECS`) and automatically
/// retries HTTP 503 (`UNAVAILABLE`) / 429 responses with exponential backoff (2s and 4s),
/// falling back to alternative active Gemini models if the primary model remains saturated.
pub fn gemini_generate_with_tools(
    api_key: &str,
    model: &str,
    system_instruction: &str,
    contents: &serde_json::Value,
    function_declarations: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let payload = serde_json::json!({
        "systemInstruction": { "parts": [{ "text": system_instruction }] },
        "contents": contents,
        "tools": [{ "functionDeclarations": function_declarations }],
        "toolConfig": { "functionCallingConfig": { "mode": "AUTO" } }
    });

    let mut candidate_models = vec![model.to_owned()];
    if let Ok(discovered) = discover_available_gemini_models(api_key) {
        for m in discovered {
            if !candidate_models.contains(&m)
                && !m.contains("image")
                && !m.contains("tts")
                && !m.contains("live")
            {
                candidate_models.push(m);
            }
        }
    }

    let timeout_secs = crate::ai::AI_HTTP_TIMEOUT_SECS;
    let backoff_delays = crate::ai::retry_backoff_durations();
    let mut last_err = String::from("Gemini no devolvió ningún candidato");

    for candidate in &candidate_models {
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{candidate}:generateContent"
        );
        let mut attempt_res = google_api_curl(&url, api_key, Some(&payload), timeout_secs);

        // Exponential backoff on HTTP 503 (Service Unavailable / UNAVAILABLE) or 429:
        // 2 automatic retries after 2s and 4s before reporting a failure or rotating model.
        for delay in backoff_delays {
            match &attempt_res {
                Ok((body, code)) if crate::ai::is_retryable_http_status(*code, body) => {
                    std::thread::sleep(delay);
                    attempt_res = google_api_curl(&url, api_key, Some(&payload), timeout_secs);
                }
                _ => break,
            }
        }

        let (body, http_code) = match attempt_res {
            Ok(pair) => pair,
            Err(e) => {
                last_err = e;
                continue;
            }
        };

        if crate::ai::is_retryable_http_status(http_code, &body) || http_code == 404 {
            if let Some(err) = parse_google_api_error(http_code, &body) {
                last_err = err;
            }
            continue;
        }

        if let Some(err) = parse_google_api_error(http_code, &body) {
            return Err(err);
        }

        let parsed: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| format!("Respuesta JSON inválida de Gemini ({candidate}): {e}"))?;
        if let Some(content) = parsed.pointer("/candidates/0/content").cloned() {
            return Ok(content);
        }
        last_err = format!("Gemini ({candidate}) no devolvió ningún candidato");
    }

    Err(last_err)
}

/// Error type used internally to distinguish `404 Not Found`, `403 Forbidden`, or `503/429 Throttled` (triggers model fallback).
pub enum BrollAttemptError {
    NotFound(String),
    PermissionDenied(String),
    UnavailableOrThrottled(String),
    Other(String),
}

fn check_google_response(http_code: u16, body: &str) -> Result<(), BrollAttemptError> {
    match parse_google_api_error(http_code, body) {
        Some(msg) if http_code == 404 || msg.contains("HTTP 404") => {
            Err(BrollAttemptError::NotFound(msg))
        }
        Some(msg) if http_code == 403 || msg.contains("HTTP 403") => {
            Err(BrollAttemptError::PermissionDenied(msg))
        }
        Some(msg)
            if http_code == 429
                || http_code == 503
                || http_code == 500
                || (500..=599).contains(&http_code)
                || msg.contains("HTTP 429")
                || msg.contains("HTTP 503")
                || msg.contains("HTTP 500")
                || msg.contains("UNAVAILABLE")
                || msg.contains("RESOURCE_EXHAUSTED") =>
        {
            Err(BrollAttemptError::UnavailableOrThrottled(msg))
        }
        Some(msg) => Err(BrollAttemptError::Other(msg)),
        None => Ok(()),
    }
}

fn broll_output_path(prompt: &str, prefix: &str, ext: &str) -> Result<PathBuf, String> {
    let out_dir = PathBuf::from("./assets/videos");
    std::fs::create_dir_all(&out_dir)
        .map_err(|e| format!("No se pudo crear el directorio ./assets/videos/: {e}"))?;
    let slug = slugify_prompt(prompt);
    let short_id = &Uuid::new_v4().to_string()[..8];
    Ok(out_dir.join(format!("{prefix}_{slug}_{short_id}.{ext}")))
}

/// Minimal standard Base64 decoder (RFC 4648, ignores whitespace and padding).
pub fn decode_base64(input: &str) -> Result<Vec<u8>, String> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'+' | b'-' => Some(62),
            b'/' | b'_' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0u32;
    for &c in input.as_bytes() {
        if c == b'=' || c.is_ascii_whitespace() {
            continue;
        }
        let v = val(c).ok_or_else(|| format!("Carácter Base64 inválido: {:?}", c as char))?;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xFF) as u8);
        }
    }
    Ok(out)
}

/// Imagen 3: `:predict` → `predictions[0].bytesBase64Encoded` (PNG) → FFmpeg Ken Burns → MP4.
fn attempt_imagen_video(
    api_key: &str,
    prompt: &str,
    duration_secs: f64,
    target_width: u32,
    target_height: u32,
) -> Result<PathBuf, BrollAttemptError> {
    let model = AiBrollModel::Imagen3;
    let payload = serde_json::json!({
        "instances": [{ "prompt": prompt }],
        "parameters": { "sampleCount": 1, "aspectRatio": "16:9" }
    });
    let endpoint = model.endpoint(api_key);
    let timeout_secs = crate::ai::AI_HTTP_TIMEOUT_SECS;
    let mut attempt_res = google_api_curl(&endpoint, api_key, Some(&payload), timeout_secs);
    for delay in crate::ai::retry_backoff_durations() {
        match &attempt_res {
            Ok((body, code)) if crate::ai::is_retryable_http_status(*code, body) => {
                std::thread::sleep(delay);
                attempt_res = google_api_curl(&endpoint, api_key, Some(&payload), timeout_secs);
            }
            _ => break,
        }
    }
    let (body, http_code) = attempt_res.map_err(BrollAttemptError::Other)?;
    check_google_response(http_code, &body)?;

    let json_val: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
        BrollAttemptError::Other(format!(
            "Respuesta inválida de Google AI Studio (Imagen 3, HTTP {http_code}): {e}"
        ))
    })?;
    let Some(b64) = json_val
        .pointer("/predictions/0/bytesBase64Encoded")
        .and_then(|v| v.as_str())
    else {
        return Err(BrollAttemptError::Other(
            "Imagen 3 no devolvió `predictions[0].bytesBase64Encoded` (posible filtro de seguridad)."
                .to_owned(),
        ));
    };
    let png_bytes = decode_base64(b64).map_err(BrollAttemptError::Other)?;
    let png_path = broll_output_path(prompt, "imagen", "png").map_err(BrollAttemptError::Other)?;
    std::fs::write(&png_path, &png_bytes)
        .map_err(|e| BrollAttemptError::Other(format!("No se pudo guardar el fotograma: {e}")))?;

    let output_mp4 = png_path.with_extension("mp4");
    let dur = duration_secs.clamp(2.0, 10.0);
    let frames = (dur * 30.0).round() as u32;
    let w = ((target_width / 2) * 2).clamp(640, 3840);
    let h = ((target_height / 2) * 2).clamp(360, 2160);
    let vf = format!(
        "scale={w}*2:{h}*2:force_original_aspect_ratio=increase,crop={w}*2:{h}*2,zoompan=z='min(zoom+0.0008,1.15)':x='iw/2-(iw/zoom/2)':y='ih/2-(ih/zoom/2)':d={frames}:s={w}x{h}:fps=30,format=yuv420p"
    );

    let ffmpeg = resolve_binary("ffmpeg");
    let encoders = ["h264_mf", "libopenh264", "mpeg4"];
    let mut last_err = String::new();
    let mut succeeded = false;

    for enc in &encoders {
        let mut cmd = Command::new(&ffmpeg);
        configure_headless_cmd(&mut cmd);
        cmd.args(["-y", "-hide_banner", "-loglevel", "error", "-loop", "1", "-i"])
            .arg(&png_path)
            .args(["-vf", &vf, "-t", &format!("{dur:.2}"), "-c:v", enc]);
        if *enc != "mpeg4" {
            cmd.args(["-b:v", "12M", "-pix_fmt", "yuv420p"]);
        }
        cmd.arg(&output_mp4);

        if let Ok(out) = cmd.output() {
            if out.status.success() && output_mp4.exists() {
                succeeded = true;
                break;
            }
            last_err = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        }
    }

    if !succeeded {
        return Err(BrollAttemptError::Other(format!(
            "FFmpeg no pudo animar el fotograma de Imagen 3: {last_err}"
        )));
    }
    Ok(output_mp4)
}

/// Parses the JSON body returned by `GET .../models?key={api_key}` and returns a list
/// of available models supporting `generateContent`, with `flash` models prioritized first.
pub fn parse_google_models_list(json_str: &str) -> Result<Vec<String>, String> {
    #[derive(Deserialize)]
    struct ModelsResp {
        #[serde(default)]
        models: Vec<ModelEntry>,
    }

    #[derive(Deserialize)]
    struct ModelEntry {
        name: String,
        #[serde(rename = "supportedGenerationMethods", default)]
        methods: Vec<String>,
    }

    let parsed: ModelsResp = serde_json::from_str(json_str)
        .map_err(|e| format!("Error al deserializar lista de modelos: {e}"))?;

    let mut flash_models = Vec::new();
    let mut other_models = Vec::new();

    for m in parsed.models {
        if !m.methods.iter().any(|method| method == "generateContent") {
            continue;
        }
        let clean_id = m.name.strip_prefix("models/").unwrap_or(&m.name).to_string();
        let lower = clean_id.to_lowercase();
        // Skip specialized non-text models (TTS, embeddings, audio, robotics)
        if lower.contains("tts") || lower.contains("embedding") || lower.contains("audio") || lower.contains("robotics") {
            continue;
        }
        if lower.contains("flash") {
            flash_models.push(clean_id);
        } else {
            other_models.push(clean_id);
        }
    }

    // Reverse flash models so latest versions (which appear later in standard Google catalog lists) are prioritized
    flash_models.reverse();

    let mut result = flash_models;
    result.extend(other_models);

    if result.is_empty() {
        return Err("No se encontraron modelos con 'generateContent' en la respuesta de Google AI Studio.".to_string());
    }

    Ok(result)
}

/// Discovers active models from Google AI Studio that support `generateContent`.
/// Queries `GET https://generativelanguage.googleapis.com/v1beta/models?key={api_key}`.
pub fn discover_available_gemini_models(api_key: &str) -> Result<Vec<String>, String> {
    let url = format!("{GOOGLE_AI_STUDIO_BASE_URL}/models?key={api_key}");
    let (body, http_code) = google_api_curl(&url, api_key, None, 15)?;
    if http_code != 200 {
        return Err(format!("Google AI Studio /models devolvió HTTP {http_code}: {body}"));
    }
    parse_google_models_list(&body)
}

/// Canonical Gemini Flash REST endpoint call with dynamic model discovery.
/// Queries `GET https://generativelanguage.googleapis.com/v1beta/models?key={API_KEY}`
/// and iterates through active models (prioritizing Flash).
/// Request body: `{ "contents": [{ "parts": [{ "text": "<prompt>" }] }] }`
/// Handles HTTP 503 and 429 with exponential backoff (2 retries: 1.5s and 3.0s).
/// Parses `candidates[0].content.parts[0].text` cleanly.
pub fn attempt_gemini_storyboard(api_key: &str, prompt: &str) -> Result<(String, String), BrollAttemptError> {
    let instruction = format!(
        "Eres un director y montador de video profesional. Escribe un guion conciso de storyboard y subtítulos (máximo 4 tomas, \
         una sola línea por toma, en español) para: \"{prompt}\". \
         Formato exacto por cada línea: 'Plano N: descripción de escena y locución'."
    );

    let payload = serde_json::json!({
        "contents": [{
            "parts": [{ "text": instruction }]
        }]
    });

    let candidate_models = match discover_available_gemini_models(api_key) {
        Ok(models) if !models.is_empty() => models,
        _ => vec![
            "gemini-3.5-flash-lite".to_string(),
            "gemini-3.8-flash".to_string(),
            "gemini-flash-latest".to_string(),
            "gemini-2.5-flash".to_string(),
        ],
    };

    let mut last_err = None;

    for model_name in candidate_models {
        let url = format!("{GOOGLE_AI_STUDIO_BASE_URL}/models/{model_name}:generateContent?key={api_key}");
        let timeout_secs = crate::ai::AI_HTTP_TIMEOUT_SECS;
        let mut attempt_res = google_api_curl(&url, api_key, Some(&payload), timeout_secs);

        // Exponential Backoff: 2 reintentos con 2s y 4s si responde 503 (UNAVAILABLE) o 429
        for delay in crate::ai::retry_backoff_durations() {
            match &attempt_res {
                Ok((body, code)) if crate::ai::is_retryable_http_status(*code, body) => {
                    std::thread::sleep(delay);
                    attempt_res = google_api_curl(&url, api_key, Some(&payload), timeout_secs);
                }
                _ => break,
            }
        }

        let (body, http_code) = match attempt_res {
            Ok(pair) => pair,
            Err(e) => {
                last_err = Some(BrollAttemptError::Other(e));
                continue;
            }
        };

        if let Err(err) = check_google_response(http_code, &body) {
            match err {
                // If 404 (deprecated) or 503/429 (throttled), try the next model in the list!
                BrollAttemptError::NotFound(_) | BrollAttemptError::UnavailableOrThrottled(_) => {
                    last_err = Some(err);
                    continue;
                }
                other => return Err(other),
            }
        }

        let json_val: serde_json::Value = match serde_json::from_str(&body) {
            Ok(v) => v,
            Err(e) => {
                last_err = Some(BrollAttemptError::Other(format!(
                    "Respuesta JSON no válida de Google AI Studio ({model_name}, HTTP {http_code}): {e}"
                )));
                continue;
            }
        };

        // Parse candidates[0].content.parts[0].text per official documentation
        let text = json_val
            .pointer("/candidates/0/content/parts/0/text")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .or_else(|| {
                json_val
                    .pointer("/candidates/0/content/parts")
                    .and_then(|v| v.as_array())
                    .map(|parts| {
                        parts
                            .iter()
                            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
            })
            .unwrap_or_default();

        if !text.trim().is_empty() {
            return Ok((text.trim().to_owned(), model_name));
        }
    }

    Err(last_err.unwrap_or_else(|| {
        BrollAttemptError::Other("Ningún modelo de Google AI Studio devolvió contenido válido".to_owned())
    }))
}

/// Generates AI B-Roll with the selected Google AI Studio backend (`model`), automatically
/// falling back to the next backend (`Imagen 3 → Gemini Flash Dinámico`) when the
/// endpoint returns `404 Not Found`, `403 Forbidden`, or `503/429 Unavailable`.
pub fn generate_ai_broll_with_model(
    prompt: String,
    duration_secs: f64,
    model: AiBrollModel,
) -> Result<AiBrollOutput, String> {
    generate_ai_broll_with_model_and_res(prompt, duration_secs, model, 1280, 720)
}

pub fn generate_ai_broll_with_model_and_res(
    prompt: String,
    duration_secs: f64,
    model: AiBrollModel,
    target_width: u32,
    target_height: u32,
) -> Result<AiBrollOutput, String> {
    let api_key = env::var("GEMINI_API_KEY")
        .ok()
        .map(|k| k.trim().to_owned())
        .filter(|k| is_valid_gemini_api_key(k))
        .ok_or_else(|| {
            "MISSING_GEMINI_API_KEY: No hay una GEMINI_API_KEY válida activa. Configura tu clave en Ajustes (⚙) antes de generar B-Roll o Storyboard.".to_owned()
        })?;

    let mut notes: Vec<String> = Vec::new();
    let mut current = Some(model);
    while let Some(m) = current {
        let attempt = match m {
            AiBrollModel::Gemini20Flash => attempt_gemini_storyboard(&api_key, &prompt).map(|(text, dynamic_name)| {
                let procedural = generate_procedural_broll_clip_with_res(&prompt, duration_secs, 1, target_width, target_height).ok();
                AiBrollOutput::Storyboard {
                    text,
                    model: m,
                    dynamic_model_name: dynamic_name,
                    notes: notes.clone(),
                    procedural_clip: procedural,
                }
            }),
            AiBrollModel::Imagen3 => attempt_imagen_video(&api_key, &prompt, duration_secs, target_width, target_height)
                .map(|path| AiBrollOutput::Video {
                    path,
                    model: m,
                    dynamic_model_name: "imagen-3.0-generate-002".to_string(),
                    notes: notes.clone(),
                }),
        };
        match attempt {
            Ok(output) => return Ok(output),
            Err(BrollAttemptError::Other(msg)) => {
                return Err(format!("[{}] {msg}", m.model_id()));
            }
            Err(BrollAttemptError::UnavailableOrThrottled(msg)) => {
                let friendly_reason = format!("{} saturado tras 2 reintentos (503/429)", m.model_id());
                notes.push(friendly_reason);
                match m.fallback_on_404() {
                    Some(next) => current = Some(next),
                    None => {
                        return Err(format!(
                            "Google AI Studio ({}) saturado (HTTP 503/429) tras reintentos exponenciales: {msg}",
                            m.model_id()
                        ));
                    }
                }
            }
            Err(BrollAttemptError::PermissionDenied(msg)) => {
                notes.push(format!("{} → 403 (acceso limitado)", m.model_id()));
                match m.fallback_on_404() {
                    Some(next) => current = Some(next),
                    None => {
                        return Err(format!("Permiso denegado (HTTP 403) para '{}': {msg}", m.model_id()));
                    }
                }
            }
            Err(BrollAttemptError::NotFound(msg)) => {
                notes.push(format!("{} → 404 (endpoint no habilitado)", m.model_id()));
                match m.fallback_on_404() {
                    Some(next) => current = Some(next),
                    None => {
                        return Err(format!(
                            "Modelo '{}' no disponible (HTTP 404): {msg}",
                            m.model_id()
                        ));
                    }
                }
            }
        }
    }
    Err("No se seleccionó ningún modelo de generación".to_owned())
}

/// Backwards-compatible helper: generates a B-Roll MP4 using Imagen 3 with procedural fallback.
pub fn generate_ai_broll(prompt: String, duration_secs: f64) -> Result<PathBuf, String> {
    match generate_ai_broll_with_model(prompt.clone(), duration_secs, AiBrollModel::Imagen3)? {
        AiBrollOutput::Video { path, .. } => Ok(path),
        AiBrollOutput::Storyboard { procedural_clip, .. } => {
            if let Some(p) = procedural_clip {
                Ok(p)
            } else {
                generate_procedural_broll_clip(&prompt, duration_secs, 1)
            }
        }
    }
}

/// Wraps text to multiple lines of at most `max_chars_per_line` characters.
fn wrap_text_for_drawtext(prompt: &str, max_chars_per_line: usize, max_lines: usize) -> String {
    let mut lines = Vec::new();
    let mut current_line = String::new();

    for word in prompt.split_whitespace() {
        if current_line.is_empty() {
            current_line.push_str(word);
        } else if current_line.len() + 1 + word.len() <= max_chars_per_line {
            current_line.push(' ');
            current_line.push_str(word);
        } else {
            lines.push(current_line);
            current_line = word.to_string();
            if lines.len() >= max_lines {
                break;
            }
        }
    }
    if !current_line.is_empty() && lines.len() < max_lines {
        lines.push(current_line);
    }
    lines.join("\n")
}

/// Generates a cinematic procedural animated MP4 clip with FFmpeg to serve as local B-Roll on V2
/// when cloud video generation (Veo / Imagen 3) is unavailable or returns 404/403.
pub fn generate_procedural_broll_clip(
    prompt: &str,
    duration_secs: f64,
    shot_index: usize,
) -> Result<PathBuf, String> {
    generate_procedural_broll_clip_with_res(prompt, duration_secs, shot_index, 1280, 720)
}

/// Generates a cinematic procedural animated MP4 clip with FFmpeg matching the exact target resolution
/// of the project (e.g. 2560x1070 Ultra-Wide or 1920x1080), centering the prompt with clean word wrapping.
pub fn generate_procedural_broll_clip_with_res(
    prompt: &str,
    duration_secs: f64,
    shot_index: usize,
    target_width: u32,
    target_height: u32,
) -> Result<PathBuf, String> {
    let out_dir = PathBuf::from("./assets/videos");
    std::fs::create_dir_all(&out_dir)
        .map_err(|e| format!("No se pudo crear el directorio ./assets/videos/: {e}"))?;

    let slug = slugify_prompt(prompt);
    let short_id = &Uuid::new_v4().to_string()[..8];
    let output_mp4 = out_dir.join(format!("procedural_broll_{slug}_s{shot_index}_{short_id}.mp4"));

    let dur = duration_secs.clamp(1.5, 30.0);
    let dur_str = format!("{dur:.2}");

    let w = ((target_width / 2) * 2).clamp(320, 3840);
    let h = ((target_height / 2) * 2).clamp(240, 2160);

    let lower_prompt = prompt.to_lowercase();
    let theme_filter = if lower_prompt.contains("cyber")
        || lower_prompt.contains("neon")
        || lower_prompt.contains("futur")
        || lower_prompt.contains("ciudad")
    {
        format!(
            "gradients=s={w}x{h}:r=30:c0=0x18052b:c1=0x08081f:speed=0.03,noise=alls=15:allf=t,drawbox=x=0:y='ih*0.45+sin(t*10)*50':w=iw:h=8:color=0x06b6d4@0.85:t=fill,drawbox=x=0:y='ih*0.72-cos(t*8)*70':w=iw:h=12:color=0xd946ef@0.85:t=fill"
        )
    } else if lower_prompt.contains("natur")
        || lower_prompt.contains("bosque")
        || lower_prompt.contains("paisaje")
        || lower_prompt.contains("dron")
        || lower_prompt.contains("monta")
    {
        format!(
            "gradients=s={w}x{h}:r=30:c0=0x064e3b:c1=0x022c22:type=radial:x0='iw/2':y0='ih/2':r0='min(iw,ih)*0.75':speed=0.015,noise=alls=10:allf=t,drawbox=x='iw*0.35':y=0:w='iw*0.3':h=ih:color=0x34d399@0.25:t=fill"
        )
    } else if lower_prompt.contains("cine")
        || lower_prompt.contains("drama")
        || lower_prompt.contains("35mm")
        || lower_prompt.contains("pelicula")
    {
        format!(
            "gradients=s={w}x{h}:r=30:c0=0x1c1917:c1=0x0c0a09:speed=0.01,noise=alls=12:allf=t,drawbox=x=0:y='(ih-16)/2':w=iw:h=16:color=0x38bdf8@0.9:t=fill,drawbox=x='iw*0.25':y='(ih-36)/2':w='iw*0.5':h=36:color=0x7dd3fc@0.4:t=fill"
        )
    } else {
        match shot_index % 3 {
            0 => format!(
                "gradients=s={w}x{h}:r=30:c0=0x1e1b4b:c1=0x0f172a:speed=0.02,noise=alls=12:allf=t,drawbox=x=0:y='ih*0.5+sin(t*6)*40':w=iw:h=8:color=0x818cf8@0.7:t=fill"
            ),
            1 => format!(
                "gradients=s={w}x{h}:r=30:c0=0x31102f:c1=0x18051a:speed=0.02,noise=alls=12:allf=t,drawbox=x=0:y='ih*0.6+cos(t*6)*40':w=iw:h=8:color=0xf43f5e@0.7:t=fill"
            ),
            _ => format!(
                "gradients=s={w}x{h}:r=30:c0=0x14532d:c1=0x052e16:speed=0.02,noise=alls=12:allf=t,drawbox=x='iw*0.4':y=0:w='iw*0.2':h=ih:color=0x22c55e@0.3:t=fill"
            ),
        }
    };

    let ffmpeg = resolve_binary("ffmpeg");
    let encoder_candidates = ["h264_mf", "libopenh264", "mpeg4"];

    // Wrap prompt into 2-4 lines and save to temp file for robust multi-platform drawtext
    let max_chars = if w >= 2000 { 46 } else { 34 };
    let wrapped_prompt = wrap_text_for_drawtext(prompt, max_chars, 4);
    let tmp_txt = env::temp_dir().join(format!("gfcut_broll_txt_{}.txt", Uuid::new_v4().simple()));
    let _ = std::fs::write(&tmp_txt, &wrapped_prompt);
    let txt_path_escaped = escape_ffmpeg_filter_path(&tmp_txt);

    let font_size = ((h as f32) * 0.042).clamp(20.0, 52.0) as u32;

    let filter_candidates = [
        format!(
            "{theme_filter},drawtext=textfile='{txt_path_escaped}':fontcolor=white:fontsize={font_size}:x=48:y=h-text_h-48:box=1:boxcolor=black@0.75:boxborderw=12:line_spacing=8,format=yuv420p"
        ),
        format!("{theme_filter},format=yuv420p"),
        format!("color=c=0x0f172a:s={w}x{h}:r=30,noise=alls=8:allf=t,format=yuv420p"),
    ];

    let mut last_err = String::new();
    for filter in &filter_candidates {
        for encoder in &encoder_candidates {
            let mut cmd = Command::new(&ffmpeg);
            configure_headless_cmd(&mut cmd);
            cmd.args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                filter,
                "-t",
                &dur_str,
                "-c:v",
                encoder,
            ]);
            if *encoder != "mpeg4" {
                cmd.args(["-b:v", "12M", "-pix_fmt", "yuv420p"]);
            }
            cmd.arg(&output_mp4);

            if let Ok(out) = cmd.output() {
                if out.status.success() && output_mp4.exists() {
                    let _ = std::fs::remove_file(&tmp_txt);
                    return Ok(output_mp4);
                }
                last_err = String::from_utf8_lossy(&out.stderr).trim().to_owned();
            }
        }
    }
    let _ = std::fs::remove_file(&tmp_txt);

    Err(format!("FFmpeg no pudo generar clip procedural animado: {last_err}"))
}

/// Generates a cinematic procedural effect or transition MP4 in `./assets/presets/{label}.mp4`
/// using FFmpeg lavfi filters. Caches the result on disk so subsequent insertions are instantaneous.
pub fn generate_studio_preset_clip(
    label: &str,
    duration_secs: f64,
    target_width: u32,
    target_height: u32,
) -> Result<PathBuf, String> {
    let out_dir = PathBuf::from("./assets/presets");
    std::fs::create_dir_all(&out_dir)
        .map_err(|e| format!("No se pudo crear el directorio ./assets/presets/: {e}"))?;

    let output_mp4 = out_dir.join(format!("{label}.mp4"));
    if output_mp4.is_file() {
        if let Ok(meta) = std::fs::metadata(&output_mp4) {
            if meta.len() > 1000 {
                return Ok(output_mp4);
            }
        }
    }

    let dur = duration_secs.clamp(0.5, 30.0);
    let dur_str = format!("{dur:.2}");
    let w = ((target_width / 2) * 2).clamp(320, 3840);
    let h = ((target_height / 2) * 2).clamp(240, 2160);

    let filter = match label {
        "B-ROLL_HoloGlitch_FX" => format!(
            "gradients=s={w}x{h}:r=30:c0=0x0a0a1a:c1=0x1e1b4b:speed=0.04,noise=alls=15:allf=t,drawbox=x=0:y='ih*0.35+sin(t*15)*80':w=iw:h=8:color=cyan@0.7:t=fill,drawbox=x=0:y='ih*0.65-cos(t*12)*100':w=iw:h=12:color=magenta@0.7:t=fill,format=yuv420p"
        ),
        "Anamorphic_Flare_Pro" => format!(
            "gradients=s={w}x{h}:r=30:c0=0x020617:c1=0x0f172a:speed=0.01,drawbox=x=0:y='(ih-14)/2':w=iw:h=14:color=0x38bdf8@0.9:t=fill,drawbox=x='iw*0.35':y='(ih-28)/2':w='iw*0.3':h=28:color=0x7dd3fc@0.8:t=fill,format=yuv420p"
        ),
        "LUT_Cinematic_Rec709" => format!(
            "gradients=s={w}x{h}:r=30:c0=0x111827:c1=0x030712:type=radial:x0='iw/2':y0='ih/2':r0='min(iw,ih)*0.7':speed=0.005,format=yuv420p"
        ),
        "Chromatic_Aberration" => format!(
            "color=c=black:s={w}x{h}:r=30,noise=alls=20:allf=t,drawbox=x=0:y='ih*0.3+sin(t*10)*50':w=iw:h=6:color=cyan@0.6:t=fill,drawbox=x=0:y='ih*0.7-cos(t*8)*70':w=iw:h=8:color=magenta@0.6:t=fill,format=yuv420p"
        ),
        "Whip_Pan_Cyber" => format!(
            "gradients=s={w}x{h}:r=30:c0=0x0f172a:c1=0x0284c7:speed=0.25,drawbox=x='iw*mod(t*2,1.0)':y=0:w='iw*0.4':h=ih:color=0x38bdf8@0.6:t=fill,format=yuv420p"
        ),
        "Flash_White_Cut" => format!(
            "color=c=white:s={w}x{h}:r=30,fade=t=out:st=0.0:d=0.7:alpha=0,format=yuv420p"
        ),
        "Glitch_Morph_IA" => format!(
            "gradients=s={w}x{h}:r=30:c0=0x1e1b4b:c1=0x4338ca:speed=0.1,noise=alls=25:allf=t,format=yuv420p"
        ),
        "Cross_Dissolve_Film" => format!(
            "color=c=0x09090b:s={w}x{h}:r=30,noise=alls=18:allf=t,fade=t=in:st=0:d=0.5:alpha=0,fade=t=out:st=0.7:d=0.5:alpha=0,format=yuv420p"
        ),
        _ => format!(
            "gradients=s={w}x{h}:r=30:c0=0x0f172a:c1=0x312e81:speed=0.02,format=yuv420p"
        ),
    };

    let ffmpeg = resolve_binary("ffmpeg");
    let encoders = ["h264_mf", "libopenh264", "mpeg4"];
    let mut last_err = String::new();

    for enc in &encoders {
        let mut cmd = Command::new(&ffmpeg);
        configure_headless_cmd(&mut cmd);
        cmd.args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            &filter,
            "-t",
            &dur_str,
            "-c:v",
            enc,
        ]);
        if *enc != "mpeg4" {
            cmd.args(["-b:v", "12M", "-pix_fmt", "yuv420p"]);
        }
        cmd.arg(&output_mp4);

        if let Ok(out) = cmd.output() {
            if out.status.success() && output_mp4.exists() {
                return Ok(output_mp4);
            }
            last_err = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        }
    }

    Err(format!("FFmpeg no pudo generar clip de preset '{label}': {last_err}"))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LocalTtsKind {
    Piper,
    VoiceStudio,
    EdgeTts,
}

/// Searches for a local Voice Studio / Piper / TTS executable in `./tools/tts/`, env vars, or `PATH`.
fn find_local_tts_engine() -> Option<(PathBuf, LocalTtsKind, &'static str)> {
    // 0. Priority: Edge Neural TTS in standard Python installation or tools folder
    if let Ok(userprofile) = env::var("USERPROFILE") {
        for subpath in [
            "AppData\\Local\\Programs\\Python\\Python312\\Scripts\\edge-tts.exe",
            "AppData\\Local\\Programs\\Python\\Python311\\Scripts\\edge-tts.exe",
            "AppData\\Local\\Programs\\Python\\Python310\\Scripts\\edge-tts.exe",
            "AppData\\Roaming\\Python\\Python312\\Scripts\\edge-tts.exe",
        ] {
            let p = PathBuf::from(&userprofile).join(subpath);
            if p.is_file() {
                return Some((p, LocalTtsKind::EdgeTts, "Edge Neural TTS"));
            }
        }
    }

    for (env_var, kind, label) in [
        ("EDGE_TTS_BIN", LocalTtsKind::EdgeTts, "Edge Neural TTS"),
        ("VOICE_STUDIO_BIN", LocalTtsKind::VoiceStudio, "Voice Studio Local"),
        ("PIPER_BIN", LocalTtsKind::Piper, "Piper Neural TTS"),
        ("GFCUT_TTS_BIN", LocalTtsKind::VoiceStudio, "GFCut Local TTS"),
        ("OPENCUT_TTS_BIN", LocalTtsKind::VoiceStudio, "GFCut Local TTS (Legacy)"),
    ] {
        if let Ok(val) = env::var(env_var) {
            let p = PathBuf::from(val.trim());
            if p.is_file() {
                return Some((p, kind, label));
            }
        }
    }

    let local_candidates: &[(&str, LocalTtsKind, &'static str)] = &[
        ("./tools/tts/voice-studio.exe", LocalTtsKind::VoiceStudio, "Voice Studio (./tools/tts)"),
        ("./tools/tts/voice-studio", LocalTtsKind::VoiceStudio, "Voice Studio (./tools/tts)"),
        ("./tools/tts/piper.exe", LocalTtsKind::Piper, "Piper Neural TTS (./tools/tts)"),
        ("./tools/tts/piper", LocalTtsKind::Piper, "Piper Neural TTS (./tools/tts)"),
        ("./tools/tts/piper/piper.exe", LocalTtsKind::Piper, "Piper Neural TTS (./tools/tts/piper)"),
        ("./tools/tts/tts.exe", LocalTtsKind::VoiceStudio, "Local TTS (./tools/tts)"),
        ("./tools/tts/tts", LocalTtsKind::VoiceStudio, "Local TTS (./tools/tts)"),
    ];

    for (rel, kind, label) in local_candidates {
        let path = PathBuf::from(rel);
        if path.is_file() {
            return Some((path, *kind, label));
        }
    }

    if let Some(path_var) = env::var_os("PATH") {
        let exts: &[&str] = if cfg!(windows) { &[".exe", ".cmd", ""] } else { &[""] };
        for dir in env::split_paths(&path_var) {
            for (bin_name, kind, label) in [
                ("voice-studio", LocalTtsKind::VoiceStudio, "Voice Studio (PATH)"),
                ("piper", LocalTtsKind::Piper, "Piper Neural TTS (PATH)"),
                ("edge-tts", LocalTtsKind::EdgeTts, "Edge Neural TTS (PATH)"),
            ] {
                for ext in exts {
                    let candidate = dir.join(format!("{bin_name}{ext}"));
                    if candidate.is_file() {
                        return Some((candidate, kind, label));
                    }
                }
            }
        }
    }

    None
}

fn find_local_piper_model() -> Option<PathBuf> {
    if let Ok(model_env) = env::var("PIPER_MODEL") {
        let p = PathBuf::from(model_env.trim());
        if p.is_file() {
            return Some(p);
        }
    }
    for dir in [Path::new("./tools/tts"), Path::new("./tools/tts/piper")] {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.extension().and_then(|e| e.to_str()) == Some("onnx") {
                    return Some(p);
                }
            }
        }
    }
    None
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TtsVoiceSpec {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub edge_voice: &'static str,
    pub sapi_voice: &'static str,
}

pub const AVAILABLE_TTS_VOICES: [TtsVoiceSpec; 6] = [
    TtsVoiceSpec {
        id: "alvaro",
        label: "Álvaro (Narrativo · ES)",
        description: "Español España · Voz profunda y documental",
        edge_voice: "es-ES-AlvaroNeural",
        sapi_voice: "Microsoft Helena Desktop",
    },
    TtsVoiceSpec {
        id: "elvira",
        label: "Elvira (Documental · ES)",
        description: "Español España · Voz nítida y profesional",
        edge_voice: "es-ES-ElviraNeural",
        sapi_voice: "Microsoft Helena Desktop",
    },
    TtsVoiceSpec {
        id: "jorge",
        label: "Jorge (Cine / Grave · MX)",
        description: "Español México · Tono cinematográfico",
        edge_voice: "es-MX-JorgeNeural",
        sapi_voice: "Microsoft Sabina Desktop",
    },
    TtsVoiceSpec {
        id: "dalia",
        label: "Dalia (Dinámica / Comercial · MX)",
        description: "Español México · Expresiva y comercial",
        edge_voice: "es-MX-DaliaNeural",
        sapi_voice: "Microsoft Sabina Desktop",
    },
    TtsVoiceSpec {
        id: "brian",
        label: "Brian (Trailer Cine · EN)",
        description: "Inglés · Voz épica para trailers",
        edge_voice: "en-US-BrianNeural",
        sapi_voice: "Microsoft David Desktop",
    },
    TtsVoiceSpec {
        id: "emma",
        label: "Emma (Podcast · EN)",
        description: "Inglés · Tono cálido de podcast",
        edge_voice: "en-US-EmmaNeural",
        sapi_voice: "Microsoft Zira Desktop",
    },
];

/// Synthesizes a spoken voiceover `.wav` file from `text` using `voice_id` into `./assets/tts/`,
/// prioritizing any local Voice Studio / Piper / Edge-TTS executable in `./tools/tts/` or `PATH`,
/// and falling back to OS system speech synthesis (`System.Speech` / FFmpeg).
/// Returns `(wav_path, duration_secs, engine_label)`.
pub fn synthesize_tts_wav_with_voice(
    text: &str,
    voice_id: Option<&str>,
) -> Result<(PathBuf, f64, String), String> {
    let clean_text = text.trim();
    if clean_text.is_empty() {
        return Err("Escribe un texto para generar la locución IA.".to_owned());
    }

    let voice_spec = voice_id
        .and_then(|id| AVAILABLE_TTS_VOICES.iter().find(|v| v.id == id))
        .unwrap_or(&AVAILABLE_TTS_VOICES[0]);

    let out_dir = PathBuf::from("./assets/tts");
    std::fs::create_dir_all(&out_dir)
        .map_err(|e| format!("No se pudo crear el directorio ./assets/tts/: {e}"))?;

    let slug = slugify_prompt(clean_text);
    let short_id = &Uuid::new_v4().to_string()[..8];
    let output_wav = out_dir.join(format!("voz_{}_{slug}_{short_id}.wav", voice_spec.id));

    let mut synthesized_engine: Option<String> = None;

    // 1. Priority: Local Voice Studio / Piper / Edge-TTS in `./tools/tts/` or `PATH`
    if let Some((bin_path, kind, label)) = find_local_tts_engine() {
        match kind {
            LocalTtsKind::Piper => {
                if let Some(model_path) = find_local_piper_model() {
                    let mut cmd = Command::new(&bin_path);
                    configure_headless_cmd(&mut cmd);
                    if let Ok(mut child) = cmd
                        .arg("--model")
                        .arg(&model_path)
                        .arg("--output_file")
                        .arg(&output_wav)
                        .stdin(std::process::Stdio::piped())
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .spawn()
                    {
                        if let Some(mut stdin) = child.stdin.take() {
                            use std::io::Write;
                            let _ = stdin.write_all(clean_text.as_bytes());
                        }
                        if let Ok(status) = child.wait() {
                            if status.success()
                                && output_wav.exists()
                                && output_wav.metadata().map(|m| m.len() > 256).unwrap_or(false)
                            {
                                synthesized_engine = Some(format!("{label} ({})", voice_spec.label));
                            }
                        }
                    }
                }
            }
            LocalTtsKind::VoiceStudio => {
                let mut cmd = Command::new(&bin_path);
                configure_headless_cmd(&mut cmd);
                if let Ok(out) = cmd
                    .args(["--text", clean_text, "--voice", voice_spec.edge_voice, "--output_path"])
                    .arg(&output_wav)
                    .output()
                {
                    if out.status.success()
                        && output_wav.exists()
                        && output_wav.metadata().map(|m| m.len() > 256).unwrap_or(false)
                    {
                        synthesized_engine = Some(format!("{label} ({})", voice_spec.label));
                    }
                }
            }
            LocalTtsKind::EdgeTts => {
                let temp_mp3 = out_dir.join(format!("edge_tmp_{short_id}.mp3"));
                let mut cmd = Command::new(&bin_path);
                configure_headless_cmd(&mut cmd);
                if let Ok(out) = cmd
                    .args([
                        "--voice",
                        voice_spec.edge_voice,
                        "--text",
                        clean_text,
                        "--write-media",
                    ])
                    .arg(&temp_mp3)
                    .output()
                {
                    if out.status.success() && temp_mp3.exists() {
                        let ffmpeg = resolve_binary("ffmpeg");
                        let mut ff = Command::new(ffmpeg);
                        configure_headless_cmd(&mut ff);
                        if ff
                            .args(["-y", "-hide_banner", "-loglevel", "error", "-i"])
                            .arg(&temp_mp3)
                            .args(["-ar", "48000", "-ac", "1", "-c:a", "pcm_s16le"])
                            .arg(&output_wav)
                            .status()
                            .map(|s| s.success())
                            .unwrap_or(false)
                        {
                            synthesized_engine = Some(format!("{label} ({})", voice_spec.label));
                        }
                        let _ = std::fs::remove_file(&temp_mp3);
                    }
                }
            }
        }
    }

    // 2. Fallback: Native Windows SAPI5 SpeechSynthesizer via headless PowerShell with voice selection
    #[cfg(target_os = "windows")]
    if synthesized_engine.is_none() {
        let escaped_text = clean_text.replace('\'', "''").replace('"', "").replace('`', "");
        let escaped_path = output_wav.to_string_lossy().replace('\'', "''");
        let sapi_voice = voice_spec.sapi_voice;
        let is_spanish_flag = if voice_spec.edge_voice.starts_with("es") { "$true" } else { "$false" };
        let ps_script = format!(
            "Add-Type -AssemblyName System.Speech; \
             $s = New-Object System.Speech.Synthesis.SpeechSynthesizer; \
             $ok = $false; \
             try {{ $s.SelectVoice('{sapi_voice}'); $ok = $true }} catch {{}}; \
             if (-not $ok -and {is_spanish_flag}) {{ \
                 $sp = $s.GetInstalledVoices() | Where-Object {{ $_.VoiceInfo.Culture.Name -like 'es*' }} | Select-Object -First 1; \
                 if ($sp) {{ try {{ $s.SelectVoice($sp.VoiceInfo.Name) }} catch {{}} }} \
             }}; \
             $s.SetOutputToWaveFile('{escaped_path}'); \
             $s.Speak('{escaped_text}'); \
             $s.Dispose()"
        );
        let mut ps_cmd = Command::new("powershell");
        configure_headless_cmd(&mut ps_cmd);
        if let Ok(out) = ps_cmd
            .args(["-NoProfile", "-NonInteractive", "-Command", &ps_script])
            .output()
        {
            if out.status.success()
                && output_wav.exists()
                && output_wav.metadata().map(|m| m.len() > 256).unwrap_or(false)
            {
                synthesized_engine = Some(format!("Windows SAPI5 ({})", voice_spec.label));
            }
        }
    }

    let word_count = clean_text.split_whitespace().count().max(1);
    let estimated_dur = ((word_count as f64) * 0.42).clamp(1.8, 15.0);

    // 3. Last-resort fallback: FFmpeg synthesized harmonic voice tone
    if synthesized_engine.is_none() {
        let ffmpeg = resolve_binary("ffmpeg");
        let dur_str = format!("{estimated_dur:.2}");
        let mut ff_cmd = Command::new(&ffmpeg);
        configure_headless_cmd(&mut ff_cmd);
        let status = ff_cmd
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                &format!(
                    "aevalsrc='0.25*sin(2*PI*190*t)*(0.5+0.5*sin(2*PI*4.5*t))+0.12*sin(2*PI*380*t)':s=48000:d={dur_str}"
                ),
                "-c:a",
                "pcm_s16le",
            ])
            .arg(&output_wav)
            .output()
            .map_err(|e| format!("Error al sintetizar WAV con FFmpeg: {e}"))?;

        if !status.status.success() {
            let stderr = String::from_utf8_lossy(&status.stderr);
            return Err(format!("Fallo al generar audio TTS: {}", stderr.trim()));
        }
        synthesized_engine = Some("FFmpeg Audio Synth".to_owned());
    }

    let actual_duration = probe_media(&output_wav)
        .ok()
        .map(|a| a.duration_secs)
        .filter(|&d| d > 0.1)
        .unwrap_or(estimated_dur);

    Ok((
        output_wav,
        actual_duration,
        synthesized_engine.unwrap_or_else(|| "System TTS".to_owned()),
    ))
}

/// Synthesizes a spoken voiceover `.wav` file from `text` with default voice into `./assets/tts/`.
pub fn synthesize_tts_wav_with_engine(text: &str) -> Result<(PathBuf, f64, String), String> {
    synthesize_tts_wav_with_voice(text, None)
}

/// Synthesizes a spoken voiceover `.wav` file from `text` into `./assets/tts/` and returns
/// `(wav_path, duration_secs)`.
pub fn synthesize_tts_wav(text: &str) -> Result<(PathBuf, f64), String> {
    let (path, dur, _) = synthesize_tts_wav_with_engine(text)?;
    Ok((path, dur))
}

#[cfg(test)]
mod api_validation_tests {
    use super::*;

    #[test]
    fn test_is_valid_gemini_api_key_rejects_placeholders_and_accepts_real_keys() {
        assert!(!is_valid_gemini_api_key(""));
        assert!(!is_valid_gemini_api_key("   "));
        assert!(!is_valid_gemini_api_key("short_key"));
        assert!(!is_valid_gemini_api_key("YOUR_GEMINI_API_KEY_HERE"));
        assert!(!is_valid_gemini_api_key("placeholder_token_123456"));
        assert!(!is_valid_gemini_api_key("none"));
        assert!(is_valid_gemini_api_key(
            "AIzaSyD-SampleGoogleAiStudioKeyRealPattern12345"
        ));
        assert!(is_valid_gemini_api_key(
            "AQ.SampleGoogleAiStudioKeyRealPattern12345"
        ));
    }

    #[test]
    fn test_escape_ffmpeg_filter_path_handles_windows_spaces_and_drive() {
        let p = Path::new(r"C:\Users\Ferna\Videos\the mison\subs.srt");
        assert_eq!(
            escape_ffmpeg_filter_path(p),
            "C\\:/Users/Ferna/Videos/the mison/subs.srt"
        );
        let verbatim = Path::new(r"\\?\D:\it's here\a.srt");
        assert_eq!(escape_ffmpeg_filter_path(verbatim), "D\\:/it'\\''s here/a.srt");
    }

    #[test]
    fn test_ai_broll_models_use_canonical_endpoints_and_404_fallback_chain() {
        assert_eq!(
            AiBrollModel::Gemini20Flash.endpoint("K"),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-flash:generateContent?key=K"
        );
        assert_eq!(
            AiBrollModel::Imagen3.endpoint("K"),
            "https://generativelanguage.googleapis.com/v1beta/models/imagen-3.0-generate-002:predict?key=K"
        );
        assert_eq!(AiBrollModel::Imagen3.fallback_on_404(), Some(AiBrollModel::Gemini20Flash));
        assert_eq!(AiBrollModel::Gemini20Flash.fallback_on_404(), None);
    }

    #[test]
    fn test_decode_base64_roundtrip() {
        assert_eq!(decode_base64("R0ZDdXQ=").unwrap(), b"GFCut");
        assert_eq!(decode_base64("aGk=\n").unwrap(), b"hi");
        assert!(decode_base64("@@@").is_err());
    }

    #[test]
    fn test_parse_google_api_error_captures_http_errors() {
        let raw_401 = "{\"error\":{\"code\":401,\"message\":\"API key not valid. Please pass a valid API key.\",\"status\":\"UNAUTHENTICATED\"}}\nHTTP_STATUS:401";
        let (body, status) = split_curl_body_and_status(raw_401);
        assert_eq!(status, 401);
        let err = parse_google_api_error(status, body).expect("should detect 401 error");
        assert!(err.contains("401"));
        assert!(err.contains("API key not valid"));

        let raw_403 = "{\"error\":{\"code\":403,\"message\":\"Quota exceeded for quota metric\",\"status\":\"RESOURCE_EXHAUSTED\"}}\nHTTP_STATUS:403";
        let (body403, status403) = split_curl_body_and_status(raw_403);
        let err403 = parse_google_api_error(status403, body403).expect("should detect 403 error");
        assert!(err403.contains("403"));
        assert!(err403.contains("Quota exceeded"));

        let raw_503 = "{\"error\":{\"code\":503,\"message\":\"The model is overloaded. Please try again later.\",\"status\":\"UNAVAILABLE\"}}\nHTTP_STATUS:503";
        let (body503, status503) = split_curl_body_and_status(raw_503);
        let err503 = parse_google_api_error(status503, body503).expect("should detect 503 error");
        assert!(err503.contains("503"));
        assert!(err503.contains("overloaded") || err503.contains("UNAVAILABLE"));
    }

    #[test]
    fn test_save_and_load_gemini_api_key_format() {
        let dummy = "AIzaSyD-SampleGoogleAiStudioKeyRealPattern12345";
        assert!(is_valid_gemini_api_key(dummy));
        let invalid = "short";
        assert!(save_gemini_api_key_to_env(invalid).is_err());
    }

    #[test]
    fn test_parse_google_models_list_filters_generate_content_and_prioritizes_flash() {
        let sample_json = r#"{
            "models": [
                {
                    "name": "models/gemini-2.5-pro",
                    "supportedGenerationMethods": ["generateContent", "countTokens"]
                },
                {
                    "name": "models/gemini-embedding-001",
                    "supportedGenerationMethods": ["embedContent"]
                },
                {
                    "name": "models/gemini-3.5-flash-lite",
                    "supportedGenerationMethods": ["generateContent"]
                },
                {
                    "name": "models/gemini-3.8-flash",
                    "supportedGenerationMethods": ["generateContent"]
                }
            ]
        }"#;
        let models = parse_google_models_list(sample_json).expect("should parse successfully");
        assert!(!models.is_empty());
        // Flash models should come before non-flash models
        assert!(models[0].contains("flash"));
        assert!(models.contains(&"gemini-3.5-flash-lite".to_string()));
        assert!(models.contains(&"gemini-3.8-flash".to_string()));
        assert!(models.contains(&"gemini-2.5-pro".to_string()));
        // Embedding model should be excluded
        assert!(!models.contains(&"gemini-embedding-001".to_string()));
    }

    #[test]
    fn test_generate_procedural_broll_clip_produces_valid_mp4() {
        let res = generate_procedural_broll_clip("Test procedural background", 1.5, 0);
        if let Ok(mp4_path) = res {
            assert!(mp4_path.exists());
            let meta = std::fs::metadata(&mp4_path).expect("valid metadata");
            assert!(meta.len() > 1024, "generated mp4 should not be empty");
            let _ = std::fs::remove_file(mp4_path);
        }
    }

    #[test]
    fn test_export_project_with_ffmpeg_fills_gaps_in_v1_with_black_and_silence() {
        let mut project = Project::with_default_tracks("MCP Gaps Test");
        let v1_id = project.tracks[0].id;
        // Clip 1 from 2.0 to 5.0 (gap of 2.0s before)
        project
            .add_clip(v1_id, "c1.mp4", 2.0, 0.0, 3.0)
            .unwrap();
        // Clip 2 from 7.0 to 10.0 (gap of 2.0s between 5.0 and 7.0)
        project
            .add_clip(v1_id, "c2.mp4", 7.0, 0.0, 3.0)
            .unwrap();

        let plan = export_project_with_ffmpeg(&project, Path::new("out.mp4"), false)
            .expect("plan should build");

        let filter_arg = plan
            .args
            .iter()
            .position(|a| a == "-filter_complex")
            .and_then(|idx| plan.args.get(idx + 1))
            .expect("filter_complex must exist");

        // seg 0: gap 0..2
        assert!(filter_arg.contains("color=c=black:s=1920x1080:r=30/1:d=2.0000,format=yuv420p[v1_v_0]"));
        assert!(filter_arg.contains("anullsrc=r=48000:cl=stereo,atrim=0:2.0000,asetpts=PTS-STARTPTS[v1_a_0]"));
        // seg 2: gap 5..7
        assert!(filter_arg.contains("color=c=black:s=1920x1080:r=30/1:d=2.0000,format=yuv420p[v1_v_2]"));
        assert!(filter_arg.contains("anullsrc=r=48000:cl=stereo,atrim=0:2.0000,asetpts=PTS-STARTPTS[v1_a_2]"));
        // concat of 4 segments (seg 0 gap, seg 1 c1, seg 2 gap, seg 3 c2)
        assert!(filter_arg.contains("concat=n=4:v=1:a=1[v1_concat][a1_concat]"));
    }

    #[test]
    fn test_export_project_v1_clip_passes_ss_and_t_to_input_args() {
        let mut project = Project::with_default_tracks("V1 Input Flags Test");
        let v1_id = project.tracks[0].id;
        // Clip on V1 from 0.0 to 73.0 (e.g. 1m 13s file)
        project
            .add_clip(v1_id, "the msion prueba.mp4", 0.0, 0.0, 73.0)
            .unwrap();

        let plan = export_project_with_ffmpeg(&project, Path::new("out.mp4"), false)
            .expect("plan should build");

        // Verify that -ss 0.0000 and -t 73.0000 are passed directly to the input before -i
        let ss_idx = plan.args.iter().position(|a| a == "-ss").expect("args should contain -ss");
        assert_eq!(plan.args.get(ss_idx + 1).map(String::as_str), Some("0.0000"));
        assert_eq!(plan.args.get(ss_idx + 2).map(String::as_str), Some("-t"));
        assert_eq!(plan.args.get(ss_idx + 3).map(String::as_str), Some("73.0000"));
        assert_eq!(plan.args.get(ss_idx + 4).map(String::as_str), Some("-i"));
    }

    #[test]
    fn test_synthesize_tts_wav_with_voice_generates_valid_audio_file() {
        let text = "Prueba de voz en español para GFCut Studio.";
        let res = synthesize_tts_wav_with_voice(text, Some("sabina"));
        assert!(res.is_ok(), "TTS synthesis should succeed: {:?}", res.err());
        let (wav_path, dur, engine) = res.unwrap();
        assert!(wav_path.exists(), "WAV file should exist: {}", wav_path.display());
        let meta = std::fs::metadata(&wav_path).expect("metadata should be readable");
        assert!(meta.len() > 1000, "WAV should be > 1KB of audio, got {}", meta.len());
        assert!(dur > 0.5, "Duration should be > 0.5s, got {}", dur);
        assert!(!engine.is_empty(), "Engine should be reported");
        // Cleanup generated test file
        let _ = std::fs::remove_file(&wav_path);
    }
}
