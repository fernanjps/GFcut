use std::{
    collections::VecDeque,
    env,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{Context, RenderImage};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
pub(crate) use gfcut_mcp::ffmpeg::AiBrollModel;
use gfcut_mcp::ffmpeg::{
    AiBrollOutput, detect_media_silence, is_valid_gemini_api_key, load_gemini_api_key_from_env,
    probe_media, save_gemini_api_key_to_env,
    synthesize_tts_wav_with_voice, transcribe_media_with_pipeline, validate_google_ai_studio_api_key,
};
use gfcut_timeline::{Clip, MediaAsset, Project, Resolution, SilenceInterval, Track, TrackKind};
use uuid::Uuid;

/// Available timeline editing tools.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TimelineTool {
    Select,
    #[allow(dead_code)]
    Cut,
    #[allow(dead_code)]
    Hand,
}

/// Result handed from the B-Roll background worker back to the UI thread.
enum BrollWorkerResult {
    Video(PathBuf, Option<MediaAsset>, AiBrollModel, String, Vec<String>),
    Storyboard(String, AiBrollModel, String, Vec<String>, Option<PathBuf>),
}

/// Severity level for system and AI activity logs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LogLevel {
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
        }
    }
}

/// In-memory timestamped log entry documenting AI, media, and export lifecycle events.
#[derive(Clone, Debug)]
pub(crate) struct LogEntry {
    pub(crate) timestamp: String,
    pub(crate) level: LogLevel,
    pub(crate) title: String,
    pub(crate) details: Option<String>,
}

/// Lifecycle status of the active or most recent FFmpeg timeline export.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ExportStatus {
    Idle,
    Exporting {
        progress: f32,
        status_text: String,
        output_path: PathBuf,
    },
    Success {
        output_path: PathBuf,
        summary: String,
    },
    Failed {
        output_path: PathBuf,
        error_message: String,
    },
}

use crate::{
    frame_decoder::{
        AudioPlaybackEngine, AudioPlaybackSource, FramePlaybackEngine, probe_media_duration, resolve_media_binary,
    },
    thumbnail_service::ThumbnailService,
    waveform_service::WaveformService,
};

const DEFAULT_IMPORTED_CLIP_DURATION_SECS: f64 = 10.0;
const MIN_TIMELINE_SPAN_SECS: f64 = 15.0;
pub(crate) const MAX_UNDO_HISTORY: usize = 50;

pub(crate) const DEFAULT_TIMELINE_PIXELS_PER_SECOND: f32 = 60.0;
pub(crate) const MIN_TIMELINE_PIXELS_PER_SECOND: f32 = 10.0;
pub(crate) const MAX_TIMELINE_PIXELS_PER_SECOND: f32 = 500.0;

pub(crate) const TRACK_NAME_V3_SUBS: &str = "Textos & IA Subs";
pub(crate) const TRACK_NAME_V2_BROLL: &str = "B-Roll & Overlays";
pub(crate) const TRACK_NAME_V1_MAIN: &str = "Video Principal";
pub(crate) const TRACK_NAME_A1_VOICE: &str = "Voz / Diálogos";
pub(crate) const TRACK_NAME_A2_BGM: &str = "Música BGM";
pub(crate) const TRACK_NAME_A3_SFX: &str = "Efectos SFX";

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WorkspaceMode {
    Edicion,
    Color,
    Audio,
    Efectos,
}

#[allow(dead_code)]
impl WorkspaceMode {
    pub(crate) const ALL: [Self; 4] = [Self::Edicion, Self::Color, Self::Audio, Self::Efectos];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Edicion => "🎞 Edición",
            Self::Color => "🎨 Color",
            Self::Audio => "🎙 Audio",
            Self::Efectos => "✨ Efectos",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PreviewAspectRatio {
    Widescreen16x9,
    Vertical9x16,
    Square1x1,
    Cinema239x1,
}

impl PreviewAspectRatio {
    pub(crate) fn from_resolution(res: Resolution) -> Self {
        if res.width == res.height {
            Self::Square1x1
        } else if res.height > res.width {
            Self::Vertical9x16
        } else if (res.width as f32 / res.height.max(1) as f32) > 2.1 {
            Self::Cinema239x1
        } else {
            Self::Widescreen16x9
        }
    }

    pub(crate) fn resolution(self) -> Resolution {
        match self {
            Self::Widescreen16x9 => Resolution {
                width: 1920,
                height: 1080,
            },
            Self::Vertical9x16 => Resolution {
                width: 1080,
                height: 1920,
            },
            Self::Square1x1 => Resolution {
                width: 1080,
                height: 1080,
            },
            Self::Cinema239x1 => Resolution {
                width: 2560,
                height: 1070,
            },
        }
    }

    pub(crate) fn short_label(self) -> &'static str {
        match self {
            Self::Widescreen16x9 => "16:9 YouTube",
            Self::Vertical9x16 => "9:16 TikTok",
            Self::Square1x1 => "1:1 Feed",
            Self::Cinema239x1 => "2.39:1 Cine",
        }
    }
}

/// Formats a timestamp in seconds as `MM:SS.cc` (e.g. `00:04.20`).
pub(crate) fn format_timecode(seconds: f64) -> String {
    let clamped = seconds.max(0.0);
    let total_minutes = (clamped / 60.0).floor() as u32;
    let rem_seconds = clamped - (total_minutes as f64) * 60.0;
    format!("{total_minutes:02}:{rem_seconds:05.2}")
}

/// Strips internal prefixes such as `SUB_01: "..."` so subtitle text renders cleanly on the video canvas.
pub(crate) fn clean_subtitle_display_text(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Some((_, rest)) = trimmed.split_once(':') {
        let candidate = rest.trim().trim_matches('"').trim();
        if !candidate.is_empty() {
            return candidate.to_owned();
        }
    }
    trimmed.trim_matches('"').to_owned()
}

/// Ensures the project contains the 6 stratified GFCut tracks in canonical order:
/// `[V3 Textos & IA Subs, V2 B-Roll & Overlays, V1 Video Principal, A1 Voz / Diálogos, A2 Música BGM, A3 Efectos SFX]`,
/// preserving all existing clips and track IDs.
pub(crate) fn ensure_stratified_tracks(project: &mut Project) {
    for track in &mut project.tracks {
        if track.name == "Video Track 1" {
            track.name = TRACK_NAME_V1_MAIN.to_owned();
        } else if track.name == "Audio Track 1" {
            track.name = TRACK_NAME_A1_VOICE.to_owned();
        }
    }

    let desired_specs: [(&str, TrackKind); 6] = [
        (TRACK_NAME_V3_SUBS, TrackKind::Subtitle),
        (TRACK_NAME_V2_BROLL, TrackKind::Video),
        (TRACK_NAME_V1_MAIN, TrackKind::Video),
        (TRACK_NAME_A1_VOICE, TrackKind::Audio),
        (TRACK_NAME_A2_BGM, TrackKind::Audio),
        (TRACK_NAME_A3_SFX, TrackKind::Audio),
    ];

    for (name, kind) in desired_specs {
        if !project.tracks.iter().any(|t| t.name == name) {
            project.tracks.push(Track::new(name, kind));
        }
    }

    project.tracks.sort_by_key(|track| {
        desired_specs
            .iter()
            .position(|(name, _)| *name == track.name)
            .unwrap_or(99)
    });
}

pub(crate) struct ProjectState {
    project_path: PathBuf,
    project: Project,
    undo_stack: VecDeque<Project>,
    redo_stack: VecDeque<Project>,
    playhead_time: f64,
    mark_in: Option<f64>,
    mark_out: Option<f64>,
    selected_clip_id: Option<Uuid>,
    is_playing: bool,
    playback_speed: f64,
    active_trim_guide: Option<(f64, crate::timeline_edit::TrimEdge)>,
    workspace_mode: WorkspaceMode,
    aspect_ratio: PreviewAspectRatio,
    browser_collapsed: bool,
    inspector_collapsed: bool,
    ai_chat_open: bool,
    timeline_collapsed: bool,
    active_tool: TimelineTool,
    show_settings_modal: bool,
    settings_api_key_input: String,
    settings_validating: bool,
    settings_validation_result: Option<Result<String, String>>,
    ai_task_running: bool,
    show_api_key_modal: bool,
    gemini_api_key_override: Option<String>,
    reload_count: usize,
    last_rendered_frame_seq: u64,
    last_error: Option<String>,
    status_message: Option<String>,
    activity_logs: VecDeque<LogEntry>,
    show_activity_log_modal: bool,
    show_export_modal: bool,
    export_status: ExportStatus,
    ripple_edit_mode: bool,
    timeline_pixels_per_second: f32,
    timeline_scroll_x: f32,
    frame_engine: FramePlaybackEngine,
    audio_engine: AudioPlaybackEngine,
    thumbnail_service: ThumbnailService,
    waveform_service: WaveformService,
    _watcher: Option<RecommendedWatcher>,
}

impl ProjectState {
    pub(crate) fn new(project_path: PathBuf, cx: &mut Context<Self>) -> Self {
        let (mut project, initial_error) = Self::load_or_initialize(&project_path);
        ensure_stratified_tracks(&mut project);

        // Auto-heal any video preset pseudo-paths into real high-resolution MP4s in ./assets/presets/
        let (w, h) = (project.resolution.width, project.resolution.height);
        let mut healed_assets = Vec::new();
        for track in &mut project.tracks {
            if track.kind == TrackKind::Subtitle {
                continue;
            }
            let is_v2 = track.name == TRACK_NAME_V2_BROLL;
            for clip in &mut track.clips {
                if clip.source_path.extension().and_then(|s| s.to_str()) == Some("preset") {
                    let stem = clip
                        .source_path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("preset");
                    let dur = clip.source_duration().max(0.5);
                    if let Ok(mp4_path) = gfcut_mcp::ffmpeg::generate_studio_preset_clip(
                        stem,
                        dur,
                        w,
                        h,
                    ) {
                        clip.source_path = mp4_path.clone();
                        healed_assets.push((mp4_path, dur));
                    }
                }
                // Auto-tune default opacity for effect overlays on V2 to guarantee visual blend:
                if is_v2 && (clip.opacity - 1.0).abs() < 0.001 {
                    let name = &clip.name;
                    if name.contains("Chromatic")
                        || name.contains("Glitch")
                        || name.contains("Flare")
                        || name.contains("LUT")
                    {
                        clip.opacity = match name.as_str() {
                            "Chromatic_Aberration" => 0.45,
                            "LUT_Cinematic_Rec709" => 0.35,
                            "Anamorphic_Flare_Pro" => 0.60,
                            "B-ROLL_HoloGlitch_FX" => 0.50,
                            _ => 0.50,
                        };
                    }
                }
            }
        }
        project.assets.retain(|a| {
            let s = a.path.to_string_lossy();
            !s.contains("SUB_T:") && !s.contains("SUB_SB") && !s.ends_with(".preset")
        });
        for (mp4_path, dur) in healed_assets {
            let _ = project.ensure_asset_for_path(&mp4_path, dur);
        }

        let initial_playhead = project
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Video)
            .flat_map(|t| &t.clips)
            .map(|c| c.timeline_in)
            .next()
            .unwrap_or(0.0);

        let initial_selected = project
            .tracks
            .iter()
            .flat_map(|t| &t.clips)
            .map(|c| c.id)
            .next();

        let aspect_ratio = PreviewAspectRatio::from_resolution(project.resolution);

        let (tx, rx) = flume::unbounded::<Result<Project, String>>();
        let watcher = Self::start_file_watcher(&project_path, tx).ok();

        let (frame_engine, frame_rx) = FramePlaybackEngine::spawn();
        let (thumbnail_service, thumb_rx) = ThumbnailService::spawn();
        let (waveform_service, wave_rx) = WaveformService::spawn();

        // File watcher listener loop
        cx.spawn(async move |this, cx| {
            while let Ok(result) = rx.recv_async().await {
                let updated = this.update(cx, |state, cx| {
                    match result {
                        Ok(mut updated_project) => {
                            ensure_stratified_tracks(&mut updated_project);
                            if state.project != updated_project {
                                state.project = updated_project;
                                state.aspect_ratio = PreviewAspectRatio::from_resolution(
                                    state.project.resolution,
                                );
                                if let Some(selected) = state.selected_clip_id
                                    && state.project.find_clip(selected).is_err() {
                                        state.selected_clip_id = None;
                                    }
                                state.reload_count += 1;
                                state.last_error = None;
                                state.request_project_thumbnails();
                                state.sync_media_for_playhead();
                                cx.notify();
                            }
                        }
                        Err(err) => {
                            state.last_error = Some(err);
                            cx.notify();
                        }
                    }
                });

                if updated.is_err() {
                    break;
                }
            }
        })
        .detach();

        // Background FFmpeg frame decoder listener loop with strict Monotonic Sequence Guard
        cx.spawn(async move |this, cx| {
            while let Ok(frame_resp) = frame_rx.recv_async().await {
                let updated = this.update(cx, |state, cx| {
                    let resp_seq = frame_resp.monotonic_frame_seq;
                    if resp_seq < state.last_rendered_frame_seq {
                        // Discard out-of-order frame immediately without calling cx.notify()
                        state.frame_engine.discard_stale_response(frame_resp);
                        return;
                    }

                    match state.frame_engine.handle_worker_response(frame_resp) {
                        Ok(true) => {
                            state.last_rendered_frame_seq = resp_seq;
                            cx.notify();
                        }
                        Ok(false) => {}
                        Err(err) => {
                            state.last_error = Some(err);
                            cx.notify();
                        }
                    }
                });

                if updated.is_err() {
                    break;
                }
            }
        })
        .detach();

        // Background JPEG thumbnail service listener loop
        cx.spawn(async move |this, cx| {
            while let Ok(thumb_resp) = thumb_rx.recv_async().await {
                let updated = this.update(cx, |state, cx| {
                    if state.thumbnail_service.handle_worker_response(thumb_resp) {
                        cx.notify();
                    }
                });

                if updated.is_err() {
                    break;
                }
            }
        })
        .detach();

        // Background audio RMS waveform extraction listener loop
        cx.spawn(async move |this, cx| {
            while let Ok(wave_resp) = wave_rx.recv_async().await {
                let updated = this.update(cx, |state, cx| {
                    if state.waveform_service.handle_worker_response(wave_resp) {
                        cx.notify();
                    }
                });

                if updated.is_err() {
                    break;
                }
            }
        })
        .detach();

        let loaded_gemini_key = load_gemini_api_key_from_env();
        let initial_key_input = loaded_gemini_key.clone().unwrap_or_default();

        let mut state = Self {
            project_path,
            project,
            undo_stack: VecDeque::with_capacity(MAX_UNDO_HISTORY),
            redo_stack: VecDeque::with_capacity(MAX_UNDO_HISTORY),
            playhead_time: initial_playhead,
            mark_in: None,
            mark_out: None,
            selected_clip_id: initial_selected,
            is_playing: false,
            playback_speed: 1.0,
            active_trim_guide: None,
            workspace_mode: WorkspaceMode::Edicion,
            aspect_ratio,
            browser_collapsed: false,
            inspector_collapsed: false,
            ai_chat_open: false,
            timeline_collapsed: false,
            active_tool: TimelineTool::Select,
            show_settings_modal: false,
            settings_api_key_input: initial_key_input,
            settings_validating: false,
            settings_validation_result: None,
            ai_task_running: false,
            show_api_key_modal: false,
            gemini_api_key_override: loaded_gemini_key,
            reload_count: 0,
            last_rendered_frame_seq: 0,
            last_error: initial_error,
            status_message: None,
            activity_logs: VecDeque::with_capacity(100),
            show_activity_log_modal: false,
            show_export_modal: false,
            export_status: ExportStatus::Idle,
            ripple_edit_mode: true,
            timeline_pixels_per_second: DEFAULT_TIMELINE_PIXELS_PER_SECOND,
            timeline_scroll_x: 0.0,
            frame_engine,
            audio_engine: AudioPlaybackEngine::new(),
            thumbnail_service,
            waveform_service,
            _watcher: watcher,
        };

        state.request_project_thumbnails();
        state.request_project_waveforms();
        state.log_activity(
            LogLevel::Info,
            "Proyecto GFCut cargado",
            Some(format!(
                "Pistas: {}, Clips: {}, Resolución: {}x{}",
                state.project.tracks.len(),
                state.project.tracks.iter().map(|t| t.clips.len()).sum::<usize>(),
                state.project.resolution.width,
                state.project.resolution.height
            )),
        );
        state.sync_media_for_playhead();
        state
    }

    pub(crate) fn project(&self) -> &Project {
        &self.project
    }

    pub(crate) fn project_path(&self) -> &Path {
        &self.project_path
    }

    pub(crate) fn playhead_time(&self) -> f64 {
        self.playhead_time
    }

    pub(crate) fn mark_in(&self) -> Option<f64> {
        self.mark_in
    }

    pub(crate) fn mark_out(&self) -> Option<f64> {
        self.mark_out
    }

    pub(crate) fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub(crate) fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    pub(crate) fn undo_count(&self) -> usize {
        self.undo_stack.len()
    }

    pub(crate) fn redo_count(&self) -> usize {
        self.redo_stack.len()
    }

    /// Saves a snapshot of the current `Project` into `undo_stack` (capped at 50 states)
    /// and clears `redo_stack` before any timeline mutation.
    pub(crate) fn push_history_snapshot(&mut self) {
        push_project_snapshot(&mut self.undo_stack, &mut self.redo_stack, &self.project);
    }

    /// Restores the most recent project state from `undo_stack`, saves atomically to disk, and refreshes the UI.
    pub(crate) fn undo(&mut self, cx: &mut Context<Self>) {
        if apply_undo(&mut self.project, &mut self.undo_stack, &mut self.redo_stack) {
            self.aspect_ratio = PreviewAspectRatio::from_resolution(self.project.resolution);
            if let Some(selected) = self.selected_clip_id
                && self.project.find_clip(selected).is_err() {
                    self.selected_clip_id = self
                        .project
                        .tracks
                        .iter()
                        .flat_map(|t| &t.clips)
                        .map(|c| c.id)
                        .next();
                }
            self.playhead_time = self.playhead_time.clamp(0.0, self.timeline_span());
            self.request_project_thumbnails();
            self.status_message = Some(format!(
                "↶ Deshacer aplicado ({} en historial)",
                self.undo_stack.len()
            ));
            self.log_activity(
                LogLevel::Info,
                "Deshacer (Undo) aplicado",
                Some(format!("Estados restantes en historial: {}", self.undo_stack.len())),
            );
            self.persist_and_notify(cx);
        } else {
            self.status_message = Some("Nada que deshacer".to_owned());
            cx.notify();
        }
    }

    /// Restores the next project state from `redo_stack`, saves atomically to disk, and refreshes the UI.
    pub(crate) fn redo(&mut self, cx: &mut Context<Self>) {
        if apply_redo(&mut self.project, &mut self.undo_stack, &mut self.redo_stack) {
            self.aspect_ratio = PreviewAspectRatio::from_resolution(self.project.resolution);
            if let Some(selected) = self.selected_clip_id
                && self.project.find_clip(selected).is_err() {
                    self.selected_clip_id = self
                        .project
                        .tracks
                        .iter()
                        .flat_map(|t| &t.clips)
                        .map(|c| c.id)
                        .next();
                }
            self.playhead_time = self.playhead_time.clamp(0.0, self.timeline_span());
            self.request_project_thumbnails();
            self.status_message = Some(format!(
                "↷ Rehacer aplicado ({} restantes)",
                self.redo_stack.len()
            ));
            self.log_activity(
                LogLevel::Info,
                "Rehacer (Redo) aplicado",
                Some(format!("Estados restantes: {}", self.redo_stack.len())),
            );
            self.persist_and_notify(cx);
        } else {
            self.status_message = Some("Nada que rehacer".to_owned());
            cx.notify();
        }
    }

    /// Records an event into the in-memory activity log with a timestamp (capped at 100 entries).
    pub(crate) fn log_activity(
        &mut self,
        level: LogLevel,
        title: impl Into<String>,
        details: Option<String>,
    ) {
        let duration = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let secs = duration.as_secs();
        let hours = (secs / 3600) % 24;
        let mins = (secs / 60) % 60;
        let s = secs % 60;
        let timestamp = format!("{hours:02}:{mins:02}:{s:02}");

        if self.activity_logs.len() >= 100 {
            self.activity_logs.pop_back();
        }
        self.activity_logs.push_front(LogEntry {
            timestamp,
            level,
            title: title.into(),
            details,
        });
    }

    pub(crate) fn activity_logs(&self) -> &VecDeque<LogEntry> {
        &self.activity_logs
    }

    pub(crate) fn show_activity_log(&self) -> bool {
        self.show_activity_log_modal
    }

    pub(crate) fn toggle_activity_log(&mut self, cx: &mut Context<Self>) {
        self.show_activity_log_modal = !self.show_activity_log_modal;
        cx.notify();
    }

    pub(crate) fn dismiss_activity_log(&mut self, cx: &mut Context<Self>) {
        self.show_activity_log_modal = false;
        cx.notify();
    }

    pub(crate) fn clear_activity_logs(&mut self, cx: &mut Context<Self>) {
        self.activity_logs.clear();
        self.log_activity(LogLevel::Info, "Registro de actividad limpiado", None);
        cx.notify();
    }

    pub(crate) fn show_export_modal(&self) -> bool {
        self.show_export_modal
    }

    pub(crate) fn export_status(&self) -> &ExportStatus {
        &self.export_status
    }

    pub(crate) fn dismiss_export_modal(&mut self, cx: &mut Context<Self>) {
        self.show_export_modal = false;
        cx.notify();
    }

    pub(crate) fn open_exported_video_file(&self, path: &Path) {
        let path_str = path.to_string_lossy().into_owned();
        #[cfg(target_os = "windows")]
        {
            let _ = std::process::Command::new("cmd").args(["/c", "start", "", &path_str]).spawn();
        }
        #[cfg(target_os = "macos")]
        {
            let _ = std::process::Command::new("open").arg(&path_str).spawn();
        }
        #[cfg(target_os = "linux")]
        {
            let _ = std::process::Command::new("xdg-open").arg(&path_str).spawn();
        }
    }

    /// Opens the project's media folder (`./assets/`) directly in the OS file manager (Windows Explorer / macOS Finder / Linux).
    pub(crate) fn open_project_media_folder(&mut self, cx: &mut Context<Self>) {
        let base_dir = self
            .project_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let assets_dir = base_dir.join("assets");
        let _ = std::fs::create_dir_all(&assets_dir);
        let target = if assets_dir.is_dir() { assets_dir } else { base_dir };

        #[cfg(target_os = "windows")]
        {
            let _ = Command::new("explorer.exe").arg(&target).spawn();
        }
        #[cfg(target_os = "macos")]
        {
            let _ = Command::new("open").arg(&target).spawn();
        }
        #[cfg(target_os = "linux")]
        {
            let _ = Command::new("xdg-open").arg(&target).spawn();
        }

        self.log_activity(
            LogLevel::Info,
            "Carpeta de medios abierta en el explorador",
            Some(target.display().to_string()),
        );
        self.status_message = Some(format!("📁 Carpeta abierta: {}", target.display()));
        cx.notify();
    }

    pub(crate) fn timeline_span(&self) -> f64 {
        self.project.duration().max(MIN_TIMELINE_SPAN_SECS)
    }

    pub(crate) fn pixels_per_second(&self) -> f32 {
        self.timeline_pixels_per_second
    }

    pub(crate) fn timeline_scroll_x(&self) -> f32 {
        self.timeline_scroll_x
    }

    #[allow(dead_code)]
    pub(crate) fn set_timeline_pixels_per_second(&mut self, pps: f32, cx: &mut Context<Self>) {
        self.timeline_pixels_per_second = pps.clamp(
            MIN_TIMELINE_PIXELS_PER_SECOND,
            MAX_TIMELINE_PIXELS_PER_SECOND,
        );
        cx.notify();
    }

    pub(crate) fn set_timeline_scroll_x(&mut self, scroll_x: f32, cx: &mut Context<Self>) {
        self.timeline_scroll_x = scroll_x.max(0.0);
        cx.notify();
    }

    pub(crate) fn scroll_timeline_by(
        &mut self,
        delta_px: f32,
        lane_width: f32,
        cx: &mut Context<Self>,
    ) {
        let total_w = (self.timeline_span() * self.timeline_pixels_per_second as f64) as f32;
        let max_scroll = (total_w - lane_width).max(0.0);
        self.timeline_scroll_x = (self.timeline_scroll_x + delta_px).clamp(0.0, max_scroll);
        cx.notify();
    }

    pub(crate) fn zoom_timeline(
        &mut self,
        factor: f32,
        focal_time: Option<f64>,
        lane_width: f32,
        cx: &mut Context<Self>,
    ) {
        let old_pps = self.timeline_pixels_per_second;
        let new_pps = (old_pps * factor).clamp(
            MIN_TIMELINE_PIXELS_PER_SECOND,
            MAX_TIMELINE_PIXELS_PER_SECOND,
        );
        if (new_pps - old_pps).abs() < 0.01 {
            return;
        }

        let anchor_time = focal_time.unwrap_or(self.playhead_time);
        let screen_x = (anchor_time * old_pps as f64) as f32 - self.timeline_scroll_x;
        let total_w = (self.timeline_span() * new_pps as f64) as f32;
        let max_scroll = (total_w - lane_width).max(0.0);
        let new_scroll = ((anchor_time * new_pps as f64) as f32 - screen_x).clamp(0.0, max_scroll);

        self.timeline_pixels_per_second = new_pps;
        self.timeline_scroll_x = new_scroll;
        cx.notify();
    }

    pub(crate) fn zoom_in_timeline(&mut self, cx: &mut Context<Self>) {
        self.zoom_timeline(1.25, None, 1000.0, cx);
    }

    pub(crate) fn zoom_out_timeline(&mut self, cx: &mut Context<Self>) {
        self.zoom_timeline(0.80, None, 1000.0, cx);
    }

    pub(crate) fn zoom_fit_timeline(&mut self, lane_width: f32, cx: &mut Context<Self>) {
        let span = self.timeline_span() as f32;
        let fitted_pps = (lane_width / span.max(1.0)).clamp(
            MIN_TIMELINE_PIXELS_PER_SECOND,
            MAX_TIMELINE_PIXELS_PER_SECOND,
        );
        self.timeline_pixels_per_second = fitted_pps;
        self.timeline_scroll_x = 0.0;
        cx.notify();
    }

    pub(crate) fn selected_clip_id(&self) -> Option<Uuid> {
        self.selected_clip_id
    }

    pub(crate) fn is_playing(&self) -> bool {
        self.is_playing
    }

    pub(crate) fn playback_speed(&self) -> f64 {
        self.playback_speed
    }

    #[allow(dead_code)]
    pub(crate) fn set_playback_speed(&mut self, speed: f64, cx: &mut Context<Self>) {
        self.playback_speed = speed.clamp(0.25, 4.0);
        cx.notify();
    }

    pub(crate) fn cycle_playback_speed(&mut self, cx: &mut Context<Self>) {
        let current_tenths = (self.playback_speed * 10.0).round() as i32;
        let next_speed = match current_tenths {
            10 => 1.5,
            15 => 2.0,
            20 => 0.5,
            5 => 1.0,
            _ => 1.0,
        };
        self.playback_speed = next_speed;
        self.status_message = Some(format!("Velocidad de reproducción: {next_speed:.1}x"));
        cx.notify();
    }

    pub(crate) fn active_trim_guide(&self) -> Option<(f64, crate::timeline_edit::TrimEdge)> {
        self.active_trim_guide
    }

    pub(crate) fn workspace_mode(&self) -> WorkspaceMode {
        self.workspace_mode
    }

    #[allow(dead_code)]
    pub(crate) fn set_workspace_mode(&mut self, mode: WorkspaceMode, cx: &mut Context<Self>) {
        self.workspace_mode = mode;
        self.status_message = Some(format!("Modo de espacio de trabajo: {}", mode.label()));
        cx.notify();
    }

    pub(crate) fn aspect_ratio(&self) -> PreviewAspectRatio {
        self.aspect_ratio
    }

    pub(crate) fn browser_collapsed(&self) -> bool {
        self.browser_collapsed
    }

    pub(crate) fn toggle_browser(&mut self, cx: &mut Context<Self>) {
        self.browser_collapsed = !self.browser_collapsed;
        self.status_message = Some(if self.browser_collapsed {
            "Panel de Medios oculto".to_owned()
        } else {
            "Panel de Medios visible".to_owned()
        });
        cx.notify();
    }

    #[allow(dead_code)]
    pub(crate) fn set_browser_collapsed(&mut self, collapsed: bool, cx: &mut Context<Self>) {
        self.browser_collapsed = collapsed;
        cx.notify();
    }

    pub(crate) fn inspector_collapsed(&self) -> bool {
        self.inspector_collapsed
    }

    /// Effective Gemini API key (settings override or `GEMINI_API_KEY`).
    pub(crate) fn gemini_api_key(&self) -> Option<String> {
        self.gemini_api_key_override
            .clone()
            .or_else(|| env::var("GEMINI_API_KEY").ok())
            .filter(|k| is_valid_gemini_api_key(k))
    }

    pub(crate) fn ai_chat_open(&self) -> bool {
        self.ai_chat_open
    }

    pub(crate) fn toggle_ai_chat(&mut self, cx: &mut Context<Self>) {
        self.ai_chat_open = !self.ai_chat_open;
        cx.notify();
    }

    pub(crate) fn toggle_inspector(&mut self, cx: &mut Context<Self>) {
        self.inspector_collapsed = !self.inspector_collapsed;
        cx.notify();
    }

    #[allow(dead_code)]
    pub(crate) fn set_inspector_collapsed(&mut self, collapsed: bool, cx: &mut Context<Self>) {
        self.inspector_collapsed = collapsed;
        cx.notify();
    }

    pub(crate) fn timeline_collapsed(&self) -> bool {
        self.timeline_collapsed
    }

    pub(crate) fn toggle_timeline(&mut self, cx: &mut Context<Self>) {
        self.timeline_collapsed = !self.timeline_collapsed;
        self.status_message = Some(if self.timeline_collapsed {
            "Línea de tiempo minimizada".to_owned()
        } else {
            "Línea de tiempo visible".to_owned()
        });
        cx.notify();
    }

    #[allow(dead_code)]
    pub(crate) fn set_timeline_collapsed(&mut self, collapsed: bool, cx: &mut Context<Self>) {
        self.timeline_collapsed = collapsed;
        cx.notify();
    }

    pub(crate) fn active_tool(&self) -> TimelineTool {
        self.active_tool
    }

    pub(crate) fn set_active_tool(&mut self, tool: TimelineTool, cx: &mut Context<Self>) {
        self.active_tool = tool;
        self.status_message = match tool {
            TimelineTool::Select => Some("Herramienta: Cursor de Selección activo (V)".to_owned()),
            TimelineTool::Cut => Some("Herramienta: Cuchilla de corte activa (C)".to_owned()),
            TimelineTool::Hand => Some("Herramienta: Mano de desplazamiento activa (H)".to_owned()),
        };
        cx.notify();
    }

    pub(crate) fn show_settings_modal(&self) -> bool {
        self.show_settings_modal
    }

    pub(crate) fn open_settings_modal(&mut self, cx: &mut Context<Self>) {
        self.show_settings_modal = true;
        if self.settings_api_key_input.is_empty()
            && let Some(k) = self
                .gemini_api_key_override
                .clone()
                .or_else(|| env::var("GEMINI_API_KEY").ok())
            {
                self.settings_api_key_input = k;
            }
        self.settings_validation_result = None;
        cx.notify();
    }

    pub(crate) fn toggle_settings_modal(&mut self, cx: &mut Context<Self>) {
        if self.show_settings_modal {
            self.dismiss_settings_modal(cx);
        } else {
            self.open_settings_modal(cx);
        }
    }

    pub(crate) fn dismiss_settings_modal(&mut self, cx: &mut Context<Self>) {
        self.show_settings_modal = false;
        self.settings_validating = false;
        cx.notify();
    }

    pub(crate) fn settings_api_key_input(&self) -> &str {
        &self.settings_api_key_input
    }

    pub(crate) fn set_settings_api_key_input(&mut self, val: String, cx: &mut Context<Self>) {
        self.settings_api_key_input = val;
        self.settings_validation_result = None;
        cx.notify();
    }

    pub(crate) fn settings_key_append(&mut self, ch: &str, cx: &mut Context<Self>) {
        self.settings_api_key_input.push_str(ch);
        self.settings_validation_result = None;
        cx.notify();
    }

    pub(crate) fn settings_key_backspace(&mut self, cx: &mut Context<Self>) {
        self.settings_api_key_input.pop();
        self.settings_validation_result = None;
        cx.notify();
    }

    pub(crate) fn is_settings_validating(&self) -> bool {
        self.settings_validating
    }

    pub(crate) fn settings_validation_result(&self) -> Option<&Result<String, String>> {
        self.settings_validation_result.as_ref()
    }

    pub(crate) fn validate_and_save_settings_api_key(&mut self, cx: &mut Context<Self>) {
        let key = self.settings_api_key_input.trim().to_owned();
        if key.is_empty() {
            self.settings_validation_result =
                Some(Err("Por favor ingresa tu GEMINI_API_KEY".to_owned()));
            cx.notify();
            return;
        }

        self.settings_validating = true;
        self.settings_validation_result = None;
        cx.notify();

        let key_to_validate = key.clone();
        cx.spawn(async move |this, cx| {
            let val_result = smol::unblock(move || {
                validate_google_ai_studio_api_key(&key_to_validate)
                    .and_then(|_| save_gemini_api_key_to_env(&key_to_validate))
            })
            .await;

            let _ = this.update(cx, |state, cx| {
                state.settings_validating = false;
                match val_result {
                    Ok(()) => {
                        state.gemini_api_key_override = Some(key);
                        state.show_api_key_modal = false;
                        state.settings_validation_result = Some(Ok(
                            "✓ Clave GEMINI_API_KEY validada con éxito en Google AI Studio y guardada en .env local".to_owned()
                        ));
                        state.status_message =
                            Some("✓ Clave Google AI Studio validada y guardada en .env".to_owned());
                        cx.notify();
                    }
                    Err(err) => {
                        state.settings_validation_result = Some(Err(err));
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    pub(crate) fn ai_task_running(&self) -> bool {
        self.ai_task_running
    }

    pub(crate) fn show_api_key_modal(&self) -> bool {
        self.show_api_key_modal || self.show_settings_modal
    }

    /// Strictly validates `GEMINI_API_KEY`: returns `true` only if an active session override
    /// or `GEMINI_API_KEY` environment variable exists and passes [`is_valid_gemini_api_key`].
    pub(crate) fn has_gemini_api_key(&self) -> bool {
        self.gemini_api_key_override
            .as_deref()
            .is_some_and(is_valid_gemini_api_key)
            || env::var("GEMINI_API_KEY")
                .ok()
                .as_deref()
                .is_some_and(is_valid_gemini_api_key)
    }

    pub(crate) fn set_gemini_api_key(&mut self, key: String, cx: &mut Context<Self>) {
        let trimmed = key.trim().to_owned();
        self.settings_api_key_input = trimmed.clone();
        if is_valid_gemini_api_key(&trimmed) {
            let _ = save_gemini_api_key_to_env(&trimmed);
            self.gemini_api_key_override = Some(trimmed);
            self.show_api_key_modal = false;
            self.show_settings_modal = false;
            self.last_error = None;
            self.status_message =
                Some("🔑 GEMINI_API_KEY validada y guardada en .env local".to_owned());
            cx.notify();
        } else {
            self.last_error = Some(
                "Clave GEMINI_API_KEY con formato dudoso. Abre Ajustes (⚙) para validar con Google AI Studio.".to_owned(),
            );
            cx.notify();
        }
    }

    pub(crate) fn toggle_api_key_modal(&mut self, cx: &mut Context<Self>) {
        self.toggle_settings_modal(cx);
    }

    pub(crate) fn dismiss_api_key_modal(&mut self, cx: &mut Context<Self>) {
        self.show_api_key_modal = false;
        self.dismiss_settings_modal(cx);
    }

    pub(crate) fn clear_last_error(&mut self, cx: &mut Context<Self>) {
        self.last_error = None;
        cx.notify();
    }

    pub(crate) fn reload_count(&self) -> usize {
        self.reload_count
    }

    pub(crate) fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub(crate) fn status_message(&self) -> Option<&str> {
        self.status_message.as_deref()
    }

    /// Sets `mark_in` (`ENTRADA [I]`) to the current `playhead_time`.
    pub(crate) fn set_mark_in_at_playhead(&mut self, cx: &mut Context<Self>) {
        let t = self.playhead_time.max(0.0);
        self.mark_in = Some(t);
        if let Some(out_pt) = self.mark_out
            && out_pt <= t {
                self.mark_out = None;
            }
        self.status_message = Some(format!("Entrada [I] fijada en {}", format_timecode(t)));
        cx.notify();
    }

    /// Sets `mark_out` (`SALIDA [O]`) to the current `playhead_time`.
    pub(crate) fn set_mark_out_at_playhead(&mut self, cx: &mut Context<Self>) {
        let t = self.playhead_time.max(0.1);
        self.mark_out = Some(t);
        if let Some(in_pt) = self.mark_in
            && in_pt >= t {
                self.mark_in = Some(0.0);
            }
        self.status_message = Some(format!("Salida [O] fijada en {}", format_timecode(t)));
        cx.notify();
    }

    /// Clears `mark_in` and `mark_out`.
    pub(crate) fn clear_in_out_marks(&mut self, cx: &mut Context<Self>) {
        self.mark_in = None;
        self.mark_out = None;
        self.status_message = Some("Rango Entrada/Salida restablecido".to_owned());
        cx.notify();
    }

    /// Returns a cached `240x135` [`RenderImage`] thumbnail for `source_path`, if ready.
    pub(crate) fn thumbnail_for_path(&self, source_path: &Path) -> Option<Arc<RenderImage>> {
        self.thumbnail_service.get_cached(source_path)
    }

    /// Queues asynchronous `240x135` JPEG thumbnail generation for all assets and clips in the project.
    pub(crate) fn request_project_thumbnails(&mut self) {
        for asset in &self.project.assets {
            let sample_time = (asset.duration_secs * 0.15).clamp(0.0, 2.0);
            self.thumbnail_service
                .request_thumbnail(&asset.path, sample_time);
        }
        for track in &self.project.tracks {
            for clip in &track.clips {
                let sample_time = clip.in_point + (clip.source_duration() * 0.15).min(1.5);
                self.thumbnail_service
                    .request_thumbnail(&clip.source_path, sample_time);
            }
        }
        self.request_project_waveforms();
    }

    /// Queues asynchronous audio RMS/peak extraction for all audio and video clips in the project.
    pub(crate) fn request_project_waveforms(&mut self) {
        for track in &self.project.tracks {
            for clip in &track.clips {
                self.waveform_service.request(clip.source_path.clone());
            }
        }
        for asset in &self.project.assets {
            self.waveform_service.request(asset.path.clone());
        }
    }

    /// Computes or retrieves authentic audio amplitude envelope bars for `clip`.
    pub(crate) fn waveform_amplitudes_for_clip(&self, clip: &Clip, bar_count: usize) -> Vec<f32> {
        if let Some(peaks) = self.waveform_service.get(&clip.source_path) {
            crate::waveform_service::resample_peaks_for_range(
                &peaks,
                clip.in_point,
                clip.out_point,
                bar_count,
            )
        } else {
            // If waveform is still extracting in background, generate a subtle baseline
            let base_hash = clip.name.bytes().fold(1469598103u32, |acc, b| {
                (acc ^ u32::from(b)).wrapping_mul(16777619)
            });
            (0..bar_count)
                .map(|i| {
                    let h = base_hash.wrapping_add((i as u32).wrapping_mul(2654435761));
                    0.15 + ((h % 50) as f32 / 100.0) * 0.4
                })
                .collect()
        }
    }

    /// Updates the preview canvas aspect ratio and project resolution (`16:9`, `9:16`, `1:1`, `2.39:1`).
    pub(crate) fn set_aspect_ratio(
        &mut self,
        aspect: PreviewAspectRatio,
        cx: &mut Context<Self>,
    ) {
        self.push_history_snapshot();
        self.aspect_ratio = aspect;
        self.project.resolution = aspect.resolution();
        self.status_message = Some(format!(
            "Canvas {} ({}×{})",
            aspect.short_label(),
            self.project.resolution.width,
            self.project.resolution.height
        ));
        self.persist_and_notify(cx);
    }

    /// Toggles track visibility (for Video/Subtitle tracks) or mute state (for Audio tracks).
    pub(crate) fn toggle_track_mute_or_visibility(
        &mut self,
        track_id: Uuid,
        cx: &mut Context<Self>,
    ) {
        if self.project.track(track_id).is_ok() {
            self.push_history_snapshot();
        }
        if let Ok(track) = self.project.track_mut(track_id) {
            match track.kind {
                TrackKind::Audio => {
                    track.muted = !track.muted;
                }
                TrackKind::Video | TrackKind::Subtitle => {
                    track.visible = !track.visible;
                }
            }
            self.persist_and_notify(cx);
        }
    }

    /// Toggles track lock state (`track.locked`).
    pub(crate) fn toggle_track_lock(&mut self, track_id: Uuid, cx: &mut Context<Self>) {
        if self.project.track(track_id).is_ok() {
            self.push_history_snapshot();
        }
        if let Ok(track) = self.project.track_mut(track_id) {
            track.locked = !track.locked;
            self.persist_and_notify(cx);
        }
    }

    /// Returns the active clip on track `V1 Video Principal` under `playhead_time`, if any.
    pub(crate) fn active_v1_clip_at_playhead(&self) -> Option<(&Clip, f64)> {
        let t = self.playhead_time;
        let track = self
            .project
            .tracks
            .iter()
            .find(|tr| tr.name == TRACK_NAME_V1_MAIN && !tr.muted && tr.visible)?;
        let clip = select_clip_on_track_at_time(&track.clips, t)?;
        Some((clip, clip.timeline_to_source_time(t)))
    }

    /// Returns the active clip on track `V2 B-Roll & Overlays` under `playhead_time`, if any.
    pub(crate) fn active_v2_clip_at_playhead(&self) -> Option<(&Clip, f64)> {
        let t = self.playhead_time;
        let track = self
            .project
            .tracks
            .iter()
            .find(|tr| tr.name == TRACK_NAME_V2_BROLL && !tr.muted && tr.visible)?;
        let clip = select_clip_on_track_at_time(&track.clips, t)?;
        Some((clip, clip.timeline_to_source_time(t)))
    }

    /// Returns the decoded GPU [`RenderImage`] frames for V1 (base) and V2 (overlay), if present.
    #[allow(clippy::type_complexity)]
    pub(crate) fn current_preview_layers(
        &self,
    ) -> (
        Option<Arc<RenderImage>>,
        Option<(Arc<RenderImage>, Clip)>,
    ) {
        let v1_clip_opt = self.active_v1_clip_at_playhead();
        let v2_clip_opt = self.active_v2_clip_at_playhead();

        let v1_img = if let Some((clip, src_time)) = v1_clip_opt {
            self.frame_engine
                .peek_cached_frame(&clip.source_path, src_time)
                .or_else(|| self.frame_engine.current_frame_for_path(&clip.source_path))
                .or_else(|| self.frame_engine.peek_any_cached_frame_for_path(&clip.source_path))
                .or_else(|| self.frame_engine.current_frame_image())
        } else {
            None
        };

        let v2_layer = if let Some((clip, src_time)) = v2_clip_opt {
            let img = self
                .frame_engine
                .peek_cached_frame(&clip.source_path, src_time)
                .or_else(|| self.frame_engine.current_frame_for_path(&clip.source_path))
                .or_else(|| self.frame_engine.peek_any_cached_frame_for_path(&clip.source_path));
            img.map(|i| (i, clip.clone()))
        } else {
            None
        };

        // Fallback for single-track projects or legacy lookup:
        if v1_img.is_none() && v2_layer.is_none()
            && let Some((clip, _)) = self.active_video_clip_at_playhead()
                && let Some(img) = self.frame_engine.current_frame_image() {
                    let is_v2 = self
                        .project
                        .tracks
                        .iter()
                        .any(|t| t.name == TRACK_NAME_V2_BROLL && t.clips.iter().any(|c| c.id == clip.id));
                    if is_v2 {
                        return (None, Some((img, clip.clone())));
                    } else {
                        return (Some(img), None);
                    }
                }

        (v1_img, v2_layer)
    }

    /// Returns the active video clip under `playhead_time` and its mapped `source_time` in seconds.
    /// Prioritizes `V1 Video Principal` as the continuous video playback master so that base video
    /// plays continuously at full frame-rate without freezing when V2 effects/overlays are present.
    /// Falls back to `V2 B-Roll` or other video tracks when V1 has no clip at this timestamp.
    pub(crate) fn active_video_clip_at_playhead(&self) -> Option<(&Clip, f64)> {
        let t = self.playhead_time;
        if let Some((clip, src_time)) = self.active_v1_clip_at_playhead() {
            return Some((clip, src_time));
        }
        for track in &self.project.tracks {
            if track.kind != TrackKind::Video || track.muted || !track.visible || track.name == TRACK_NAME_V1_MAIN {
                continue;
            }
            if let Some(clip) = select_clip_on_track_at_time(&track.clips, t) {
                let source_time = clip.timeline_to_source_time(t);
                return Some((clip, source_time));
            }
        }
        None
    }

    /// Finds the upcoming video clip on the timeline starting at or after `playhead_time`,
    /// excluding the currently active clip if it already spans `playhead_time`.
    pub(crate) fn next_video_clip_after_playhead(&self) -> Option<(&Clip, f64)> {
        find_next_video_clip_after_time(&self.project.tracks, self.playhead_time)
    }

    /// Returns the clean subtitle text under `playhead_time` on `V3 (Textos & IA Subs)`, if any.
    pub(crate) fn active_subtitle_at_playhead(&self) -> Option<String> {
        let t = self.playhead_time;
        for track in &self.project.tracks {
            if track.kind != TrackKind::Subtitle || !track.visible || track.muted {
                continue;
            }
            if let Some(clip) = select_clip_on_track_at_time(&track.clips, t) {
                return Some(clean_subtitle_display_text(&clip.name));
            }
        }
        None
    }

    /// Finds all active audible clips across all unmuted Audio and Video tracks under `playhead_time`
    /// enabling real-time parallel multi-track playback mixing.
    fn active_audio_sources_at_playhead(&self) -> Vec<AudioPlaybackSource> {
        let t = self.playhead_time;
        let mut sources = Vec::new();
        for track in &self.project.tracks {
            if track.muted || !track.visible || track.volume <= 0.001 {
                continue;
            }
            if track.kind != TrackKind::Audio && track.kind != TrackKind::Video {
                continue;
            }
            for clip in track
                .clips
                .iter()
                .filter(|c| t >= c.timeline_in && t < c.timeline_out - 0.02)
            {
                if track.kind == TrackKind::Video {
                    let has_linked_on_audio_track = self
                        .project
                        .tracks
                        .iter()
                        .filter(|at| at.kind == TrackKind::Audio)
                        .flat_map(|at| &at.clips)
                        .any(|ac| {
                            ac.source_path == clip.source_path
                                && (ac.timeline_in - clip.timeline_in).abs() < 0.05
                                && (ac.timeline_out - clip.timeline_out).abs() < 0.05
                        });
                    if has_linked_on_audio_track {
                        continue;
                    }
                }
                let source_time = clip.timeline_to_source_time(t);
                let remaining_tl = (clip.timeline_out - t).max(0.0);
                let effective_vol = (clip.volume * track.volume).clamp(0.0, 2.0);
                if effective_vol > 0.001 && remaining_tl > 0.05 {
                    sources.push(AudioPlaybackSource {
                        clip_id: clip.id,
                        source_path: clip.source_path.clone(),
                        source_time,
                        remaining_timeline_secs: remaining_tl,
                        speed: clip.speed,
                        volume: effective_vol,
                    });
                }
            }
        }
        sources
    }

    /// Returns the currently decoded RGBA->BGRA GPU [`RenderImage`] if a clip is under the playhead.
    #[allow(dead_code)]
    pub(crate) fn current_preview_frame(&self) -> Option<Arc<RenderImage>> {
        if self.active_video_clip_at_playhead().is_some() {
            self.frame_engine.current_frame_image()
        } else {
            None
        }
    }

    /// Synchronizes both the FFmpeg video frame decoder and the audio playback engine with `playhead_time`.
    /// Uses a persistent continuous FFmpeg rawvideo pipe while `is_playing == true` and single-frame
    /// extraction with LRU caching while paused/scrubbing (`is_playing == false`).
    pub(crate) fn sync_media_for_playhead(&mut self) {
        let fps = self.project.fps.as_f64();
        let playhead = self.playhead_time;

        let active_clip_data = self.active_video_clip_at_playhead().map(|(clip, source_time)| {
            (
                clip.id,
                clip.source_path.clone(),
                source_time,
                clip.out_point,
                clip.speed,
                clip.timeline_out - playhead,
            )
        });

        let next_clip_data = self.next_video_clip_after_playhead().map(|(next_clip, next_src_time)| {
            (
                next_clip.id,
                next_clip.source_path.clone(),
                next_src_time,
                next_clip.out_point,
                next_clip.speed,
                next_clip.timeline_in,
            )
        });

        if let Some((clip_id, source_path, source_time, max_source_duration, clip_speed, remaining_clip_time)) = active_clip_data {
            if self.is_playing {
                self.frame_engine.sync_continuous_playback(
                    clip_id,
                    &source_path,
                    source_time,
                    max_source_duration,
                    fps,
                    clip_speed,
                );
                // Immediately select closest frame from the elastic jitter buffer
                self.frame_engine.query_playback_jitter_buffer(source_time);

                // Seamless clip transition pre-buffering:
                // When playhead is within 0.5s of the cut boundary, warm up the next clip's FFmpeg pipe
                if remaining_clip_time <= 0.5
                    && let Some((next_id, next_path, next_src, next_max, next_speed, _)) = next_clip_data
                        && next_id != clip_id {
                            self.frame_engine.prebuffer_next_clip(
                                next_id,
                                &next_path,
                                next_src,
                                next_max,
                                fps,
                                next_speed,
                            );
                        }
            } else {
                self.frame_engine.request_scrub_frame(
                    source_path.clone(),
                    source_time,
                    max_source_duration,
                    fps,
                );
                // When scrubbing, request the complementary layer (V1 if active was V2, or V2 if active was V1)
                if let Some((v1_clip, v1_src)) = self.active_v1_clip_at_playhead()
                    && v1_clip.source_path != source_path {
                        self.frame_engine.request_scrub_frame(
                            v1_clip.source_path.clone(),
                            v1_src,
                            v1_clip.out_point,
                            fps,
                        );
                    }
                if let Some((v2_clip, v2_src)) = self.active_v2_clip_at_playhead()
                    && v2_clip.source_path != source_path {
                        self.frame_engine.request_scrub_frame(
                            v2_clip.source_path.clone(),
                            v2_src,
                            v2_clip.out_point,
                            fps,
                        );
                    }
            }
            self.last_rendered_frame_seq = self
                .last_rendered_frame_seq
                .max(self.frame_engine.last_rendered_frame_seq());
        } else {
            self.frame_engine.clear_active_frame();
            // In playback gaps between clips, pre-buffer the upcoming clip if within 0.5s of starting
            if self.is_playing
                && let Some((next_id, next_path, next_src, next_max, next_speed, next_timeline_in)) = next_clip_data
                    && next_timeline_in - playhead <= 0.5 {
                        self.frame_engine.prebuffer_next_clip(
                            next_id,
                            &next_path,
                            next_src,
                            next_max,
                            fps,
                            next_speed,
                        );
                    }
        }

        let audio_sources = self.active_audio_sources_at_playhead();
        self.audio_engine.sync_multi(self.is_playing, &audio_sources);
    }

    pub(crate) fn set_playhead_time(&mut self, time: f64, cx: &mut Context<Self>) {
        let max_span = self.timeline_span();
        self.playhead_time = time.clamp(0.0, max_span);
        self.sync_media_for_playhead();
        cx.notify();
    }

    pub(crate) fn step_playhead(&mut self, delta_secs: f64, cx: &mut Context<Self>) {
        self.set_playhead_time(self.playhead_time + delta_secs, cx);
    }

    pub(crate) fn select_clip_and_set_playhead(
        &mut self,
        clip_id: Uuid,
        playhead_time: f64,
        cx: &mut Context<Self>,
    ) {
        self.selected_clip_id = Some(clip_id);
        let max_span = self.timeline_span();
        self.playhead_time = playhead_time.clamp(0.0, max_span);
        self.sync_media_for_playhead();
        cx.notify();
    }

    pub(crate) fn toggle_playback(&mut self, cx: &mut Context<Self>) {
        self.is_playing = !self.is_playing;

        if self.is_playing && self.active_video_clip_at_playhead().is_none()
            && let Some(first_clip_start) = self
                .project
                .tracks
                .iter()
                .filter(|t| t.kind == TrackKind::Video)
                .flat_map(|t| &t.clips)
                .map(|c| c.timeline_in)
                .next()
                && (self.playhead_time < first_clip_start
                    || self.playhead_time >= self.project.duration() - 0.05)
                {
                    self.playhead_time = first_clip_start;
                }

        self.sync_media_for_playhead();
        cx.notify();

        if self.is_playing {
            // Anchor playback clock directly to std::time::Instant::now()
            let tick = Duration::from_millis(8);

            cx.spawn(async move |this, cx| {
                let mut last_tick = Instant::now();
                loop {
                    smol::Timer::after(tick).await;
                    let now = Instant::now();
                    let elapsed = now.duration_since(last_tick).as_secs_f64();
                    last_tick = now;

                    let keep_going = this.update(cx, |state, cx| {
                        if !state.is_playing {
                            state.frame_engine.stop_continuous_stream();
                            state.audio_engine.stop();
                            return false;
                        }
                        let current_playback_speed = state.playback_speed.clamp(0.25, 4.0);
                        let delta = elapsed * current_playback_speed;
                        let duration = state.project.duration();
                        let end = state
                            .mark_out
                            .filter(|&m| m > 0.1)
                            .unwrap_or(if duration > 0.0 {
                                duration
                            } else {
                                MIN_TIMELINE_SPAN_SECS
                            });
                        let next_time = state.playhead_time + delta;
                        if next_time >= end {
                            state.playhead_time = state.mark_in.unwrap_or(0.0);
                            state.is_playing = false;
                            state.frame_engine.stop_continuous_stream();
                            state.audio_engine.stop();
                            state.sync_media_for_playhead();
                            cx.notify();
                            false
                        } else {
                            state.playhead_time = next_time;
                            state.sync_media_for_playhead();
                            cx.notify();
                            true
                        }
                    });

                    match keep_going {
                        Ok(true) => {}
                        _ => break,
                    }
                }
            })
            .detach();
        } else {
            self.frame_engine.stop_continuous_stream();
            self.audio_engine.stop();
        }
    }

    /// Opens a native OS file picker asynchronously without blocking the GPUI thread
    /// and imports the chosen media file(s) onto the timeline.
    pub(crate) fn prompt_and_import_media(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let handles = rfd::AsyncFileDialog::new()
                .set_title("Import Media to GFCut")
                .add_filter(
                    "Video & Audio (*.mp4, *.mov, *.mkv, *.wav, *.mp3)",
                    &["mp4", "mov", "mkv", "wav", "mp3"],
                )
                .pick_files()
                .await;

            let Some(files) = handles else {
                return;
            };

            let paths: Vec<PathBuf> = files.into_iter().map(|h| h.path().to_path_buf()).collect();
            let probed_assets: Vec<(PathBuf, Option<MediaAsset>, f64)> = smol::unblock(move || {
                paths
                    .into_iter()
                    .map(|p| {
                        let asset = probe_media(&p).ok();
                        let dur = asset
                            .as_ref()
                            .map(|a| a.duration_secs)
                            .filter(|&d| d > 0.0)
                            .or_else(|| probe_media_duration(&p))
                            .unwrap_or(DEFAULT_IMPORTED_CLIP_DURATION_SECS);
                        (p, asset, dur)
                    })
                    .collect()
            })
            .await;

            let _ = this.update(cx, |state, cx| {
                for (path, maybe_asset, duration) in probed_assets {
                    if let Some(asset) = maybe_asset {
                        state.project.upsert_asset(asset);
                    } else {
                        state.project.ensure_asset_for_path(&path, duration);
                    }
                    state.insert_file_with_duration(path, duration, false, None, cx);
                }
            });
        })
        .detach();
    }

    /// Inserts an asset from the Media Browser at the current playhead position (`+ Add` card button).
    pub(crate) fn insert_media_at_playhead(&mut self, file_path: PathBuf, cx: &mut Context<Self>) {
        let known_duration = self
            .project
            .assets
            .iter()
            .find(|a| a.path == file_path && a.duration_secs > 0.0)
            .map(|a| a.duration_secs)
            .or_else(|| probe_media_duration(&file_path))
            .unwrap_or(DEFAULT_IMPORTED_CLIP_DURATION_SECS);

        self.project.ensure_asset_for_path(&file_path, known_duration);
        self.insert_file_with_duration(file_path, known_duration, true, None, cx);
    }

    fn insert_file_with_duration(
        &mut self,
        file_path: PathBuf,
        duration: f64,
        at_playhead: bool,
        preferred_track_name: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        self.push_history_snapshot();
        ensure_stratified_tracks(&mut self.project);

        let is_audio_only = file_path
            .extension()
            .and_then(OsStr::to_str)
            .is_some_and(|ext| {
                matches!(
                    ext.to_ascii_lowercase().as_str(),
                    "wav" | "mp3" | "flac" | "ogg" | "m4a"
                )
            });

        let target_track_name = preferred_track_name.unwrap_or(if is_audio_only {
            TRACK_NAME_A1_VOICE
        } else {
            TRACK_NAME_V1_MAIN
        });

        let track_id = self
            .project
            .tracks
            .iter()
            .find(|t| t.name == target_track_name && !t.locked)
            .or_else(|| {
                let desired_kind = if is_audio_only {
                    TrackKind::Audio
                } else {
                    TrackKind::Video
                };
                self.project
                    .tracks
                    .iter()
                    .find(|t| t.kind == desired_kind && !t.locked)
            })
            .map(|t| t.id)
            .unwrap_or_else(|| {
                let desired_kind = if is_audio_only {
                    TrackKind::Audio
                } else {
                    TrackKind::Video
                };
                self.project.add_track(target_track_name, desired_kind)
            });

        // Ensure we strictly instantiate the clip with the full asset duration without forced cuts
        let asset_dur = self
            .project
            .assets
            .iter()
            .find(|a| a.path == file_path && a.duration_secs > 0.0)
            .map(|a| a.duration_secs)
            .unwrap_or(duration);
        let actual_duration = if asset_dur > 0.0 {
            asset_dur.max(duration)
        } else {
            duration
        };

        let insert_result = if at_playhead {
            let mut start_pos = self.playhead_time.max(0.0);
            if let Some(track) = self.project.tracks.iter().find(|t| t.id == track_id) {
                while let Some(overlapping) = track
                    .clips
                    .iter()
                    .find(|c| start_pos >= c.timeline_in && start_pos < c.timeline_out - 0.001)
                {
                    start_pos = overlapping.timeline_out;
                }
            }
            self.project
                .add_clip(track_id, &file_path, start_pos, 0.0, actual_duration)
        } else {
            self.project.append_clip(track_id, &file_path, 0.0, actual_duration)
        };

        match insert_result {
            Ok(clip_id) => {
                self.selected_clip_id = Some(clip_id);
                let (inserted_in, inserted_dur) = if let Ok((_, clip)) = self.project.find_clip(clip_id) {
                    self.playhead_time = clip.timeline_in;
                    (clip.timeline_in, clip.timeline_out - clip.timeline_in)
                } else {
                    (0.0, duration)
                };

                // Linked audio insertion (V1 + A1):
                // If a video clip was added to V1 and contains audio, also add the audio to A1
                let mut linked_audio = false;
                if target_track_name == TRACK_NAME_V1_MAIN && source_has_audio_stream(&file_path, &self.project.assets, false) {
                    let a1_id_opt = self.project.tracks.iter().find(|t| t.name == TRACK_NAME_A1_VOICE && !t.locked).map(|t| t.id);
                    if let Some(a1_id) = a1_id_opt {
                        if at_playhead {
                            let _ = self.project.add_clip(a1_id, &file_path, inserted_in, 0.0, inserted_dur);
                        } else {
                            let _ = self.project.append_clip(a1_id, &file_path, 0.0, inserted_dur);
                        }
                        linked_audio = true;
                    }
                }

                self.request_project_thumbnails();
                self.waveform_service.request(file_path.clone());
                self.request_project_waveforms();

                let msg = if linked_audio {
                    format!("Insertado V1 + A1 vinculado ({inserted_dur:.1}s)")
                } else {
                    format!("Insertado en {target_track_name} ({inserted_dur:.1}s)")
                };
                self.status_message = Some(msg);
                self.persist_and_notify(cx);
            }
            Err(err) => {
                let _ = self.undo_stack.pop_back();
                self.last_error = Some(err.to_string());
                cx.notify();
            }
        }
    }

    /// Inserts a text/subtitle preset (`IA SUBTITLE`, `4K TITLE`, etc.) onto `V3 (Textos & IA Subs)`
    /// with the exact `duration_secs` specified, repositioning the playhead to display it immediately.
    pub(crate) fn insert_studio_preset(
        &mut self,
        label: &str,
        duration_secs: f64,
        is_subtitle: bool,
        cx: &mut Context<Self>,
    ) {
        ensure_stratified_tracks(&mut self.project);
        let target_name = if is_subtitle {
            TRACK_NAME_V3_SUBS
        } else {
            TRACK_NAME_V2_BROLL
        };

        let Some(track_id) = self
            .project
            .tracks
            .iter()
            .find(|t| t.name == target_name && !t.locked)
            .map(|t| t.id)
        else {
            self.last_error =
                Some(format!("La pista '{target_name}' está bloqueada o no disponible"));
            cx.notify();
            return;
        };

        self.push_history_snapshot();
        let mut start_time = self.playhead_time.max(0.0);
        if let Some(track) = self.project.tracks.iter().find(|t| t.id == track_id) {
            while let Some(overlapping) = track
                .clips
                .iter()
                .find(|c| start_time >= c.timeline_in && start_time < c.timeline_out - 0.001)
            {
                start_time = overlapping.timeline_out;
            }
        }

        let clip_dur = duration_secs.max(0.5);
        let preset_source = if is_subtitle {
            PathBuf::from(format!("{label}.preset"))
        } else {
            let (target_w, target_h) = (
                self.project.resolution.width,
                self.project.resolution.height,
            );
            match gfcut_mcp::ffmpeg::generate_studio_preset_clip(label, clip_dur, target_w, target_h) {
                Ok(path) => {
                    let _ = self.project.ensure_asset_for_path(&path, clip_dur);
                    path
                }
                Err(err) => {
                    self.last_error = Some(format!("Error generando preset de video: {err}"));
                    cx.notify();
                    return;
                }
            }
        };

        if let Ok(clip_id) =
            self.project
                .add_clip(track_id, preset_source, start_time, 0.0, clip_dur)
        {
            if let Ok(track) = self.project.track_mut(track_id)
                && let Some(clip) = track.clips.iter_mut().find(|c| c.id == clip_id) {
                    clip.name = label.to_owned();
                    if !is_subtitle {
                        clip.opacity = match label {
                            "Chromatic_Aberration" => 0.45,
                            "LUT_Cinematic_Rec709" => 0.35,
                            "Anamorphic_Flare_Pro" => 0.60,
                            "B-ROLL_HoloGlitch_FX" => 0.50,
                            "Whip_Pan_Cyber" => 0.65,
                            "Flash_White_Cut" => 0.70,
                            "Glitch_Morph_IA" => 0.55,
                            "Cross_Dissolve_Film" => 0.65,
                            _ => 0.75,
                        };
                    }
                }
            self.selected_clip_id = Some(clip_id);
            self.playhead_time = start_time;
            self.last_error = None;
            self.status_message = Some(format!(
                "Añadido preset '{label}' ({clip_dur:.1}s) en {target_name}"
            ));
            self.persist_and_notify(cx);
        } else {
            let _ = self.undo_stack.pop_back();
        }
    }

    /// Invokes Google AI Studio (`generate_ai_broll`) asynchronously to generate an MP4 scene in
    /// `./assets/ai_broll/` and inserts it onto track `V2 — B-Roll & Overlays` at `playhead_time`.
    /// If `GEMINI_API_KEY` is not configured or invalid, opens a prominent modal requesting it.
    pub(crate) fn generate_ai_broll_scene(
        &mut self,
        prompt: String,
        duration_secs: f64,
        model: AiBrollModel,
        cx: &mut Context<Self>,
    ) {
        if self.ai_task_running {
            return;
        }

        if !self.has_gemini_api_key() {
            self.show_api_key_modal = true;
            self.last_error = Some(
                "🔑 Se requiere una GEMINI_API_KEY válida para generar escenas de video con Google AI Studio (Veo / Gemini). Pégala en el diálogo o exporta GEMINI_API_KEY en tu entorno.".to_owned(),
            );
            cx.notify();
            return;
        }

        let key_override = self.gemini_api_key_override.clone();
        self.ai_task_running = true;
        self.show_api_key_modal = false;
        self.last_error = None;
        self.log_activity(
            LogLevel::Info,
            format!("Iniciando generación B-Roll ({})", model.model_id()),
            Some(format!("Prompt: \"{prompt}\" • Duración: {duration_secs:.1}s")),
        );
        self.status_message = Some(match model {
            AiBrollModel::Gemini20Flash => {
                "⏳ Procesando generación de video... (esperando respuesta de servidor)".to_owned()
            }
            AiBrollModel::Imagen3 => {
                "⏳ Procesando generación de video... (esperando respuesta de servidor)".to_owned()
            }
        });
        cx.notify();

        // 3-second polling ticker while waiting for AI video/storyboard generation
        cx.spawn(async move |this, cx| {
            let mut elapsed = 0u64;
            loop {
                cx.background_executor()
                    .timer(Duration::from_secs(3))
                    .await;
                elapsed += 3;
                let still_running = this
                    .update(cx, |state, cx| {
                        if !state.ai_task_running {
                            return false;
                        }
                        state.status_message = Some(format!(
                            "⏳ Generando fotogramas con IA... ({elapsed}s — esperando respuesta de servidor)"
                        ));
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !still_running {
                    break;
                }
            }
        })
        .detach();

        let prompt_clone = prompt.clone();
        let (proj_w, proj_h) = (
            self.project.resolution.width,
            self.project.resolution.height,
        );
        cx.spawn(async move |this, cx| {
            let gen_res = smol::unblock(move || {
                if let Some(k) = key_override
                    && is_valid_gemini_api_key(&k) {
                        // Safety: set inside background worker when user configured a valid key in the UI dialog.
                        unsafe {
                            env::set_var("GEMINI_API_KEY", k);
                        }
                    }
                match gfcut_mcp::ffmpeg::generate_ai_broll_with_model_and_res(
                    prompt_clone,
                    duration_secs,
                    model,
                    proj_w,
                    proj_h,
                )? {
                    AiBrollOutput::Video { path, model, dynamic_model_name, notes } => {
                        let probed = probe_media(&path).ok();
                        Ok::<_, String>(BrollWorkerResult::Video(path, probed, model, dynamic_model_name, notes))
                    }
                    AiBrollOutput::Storyboard {
                        text,
                        model,
                        dynamic_model_name,
                        notes,
                        procedural_clip,
                    } => Ok(BrollWorkerResult::Storyboard(
                        text,
                        model,
                        dynamic_model_name,
                        notes,
                        procedural_clip,
                    )),
                }
            })
            .await;

            let _ = this.update(cx, |state, cx| {
                state.ai_task_running = false;
                match gen_res {
                    Ok(BrollWorkerResult::Storyboard(text, _used_model, dynamic_name, notes, maybe_clip)) => {
                        let shots: Vec<String> = text
                            .lines()
                            .map(|l| l.trim().trim_start_matches(['*', '-', '#', ' ']).trim())
                            .filter(|l| !l.is_empty())
                            .take(6)
                            .map(str::to_owned)
                            .collect();
                        let per_shot = (duration_secs / shots.len().max(1) as f64).max(1.5);
                        let first_start = state.playhead_time;

                        // 1. Insert procedural animated B-roll clip onto V2 as a standard media asset
                        if let Some(ref clip_path) = maybe_clip {
                            if let Ok(probed) = probe_media(clip_path) {
                                state.project.upsert_asset(probed);
                            } else {
                                state.project.ensure_asset_for_path(clip_path, duration_secs);
                            }
                            state.insert_file_with_duration(
                                clip_path.clone(),
                                duration_secs,
                                true,
                                Some(TRACK_NAME_V2_BROLL),
                                cx,
                            );
                            state.request_project_thumbnails();
                        }

                        // 2. Insert storyboard text subtitles onto V3 ONLY if explicitly in Gemini Storyboard mode
                        if _used_model == AiBrollModel::Gemini20Flash {
                            state.playhead_time = first_start;
                            for (i, shot) in shots.iter().enumerate() {
                                let label = format!("SUB_SB{:02}: \"{}\"", i + 1, shot.replace('"', "'"));
                                state.insert_studio_preset(&label, per_shot, true, cx);
                            }
                            state.playhead_time = first_start;
                        }

                        state.log_activity(
                            LogLevel::Info,
                            format!("Modelo IA Activo: {dynamic_name}"),
                            Some(format!(
                                "Google AI Studio respondió con éxito usando '{dynamic_name}'."
                            )),
                        );

                        if maybe_clip.is_some() {
                            state.log_activity(
                                LogLevel::Warn,
                                "Clip Procedural B-Roll (V2)".to_string(),
                                Some(format!(
                                    "Generado clip MP4 procedural animado en V2 con FFmpeg. Planos: {}\nNotas: {}",
                                    shots.len(),
                                    if notes.is_empty() {
                                        "Ninguna".to_owned()
                                    } else {
                                        notes.join(", ")
                                    }
                                )),
                            );
                        }

                        state.last_error = None;
                        state.status_message = Some(format!(
                            "🎬 B-Roll procedural en V2 + Storyboard en V3 [{}] ({} planos)",
                            dynamic_name,
                            shots.len()
                        ));
                        state.sync_media_for_playhead();
                        cx.notify();
                    }
                    Ok(BrollWorkerResult::Video(mp4_path, maybe_asset, used_model, dynamic_name, notes)) => {
                        let actual_dur = maybe_asset
                            .as_ref()
                            .map(|a| a.duration_secs)
                            .filter(|&d| d > 0.1)
                            .unwrap_or(duration_secs);
                        if let Some(asset) = maybe_asset {
                            state.project.upsert_asset(asset);
                        } else {
                            state.project.ensure_asset_for_path(&mp4_path, actual_dur);
                        }
                        state.insert_file_with_duration(
                            mp4_path.clone(),
                            actual_dur,
                            true,
                            Some(TRACK_NAME_V2_BROLL),
                            cx,
                        );
                        state.last_error = None;
                        state.log_activity(
                            LogLevel::Info,
                            format!("Escena IA generada con éxito ({dynamic_name})"),
                            Some(format!(
                                "Modelo dinámico: {dynamic_name}\nArchivo MP4: {}\nDuración: {:.1}s\nPista: V2 (B-Roll & Overlays)",
                                mp4_path.display(),
                                actual_dur
                            )),
                        );
                        let fallback_note = if notes.is_empty() {
                            String::new()
                        } else {
                            format!(" (fallback: {})", notes.join(", "))
                        };
                        state.status_message = Some(format!(
                            "🎬 Escena IA {} guardada en {} e insertada en V2 ({actual_dur:.1}s){fallback_note}",
                            used_model.model_id(),
                            mp4_path.display()
                        ));
                        state.sync_media_for_playhead();
                        cx.notify();
                    }
                    Err(err) => {
                        state.log_activity(
                            LogLevel::Error,
                            "Fallo en generación de B-Roll IA",
                            Some(err.clone()),
                        );
                        if err.starts_with("MISSING_GEMINI_API_KEY:") {
                            state.show_api_key_modal = true;
                        }
                        state.status_message = None;
                        state.last_error = Some(err);
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    /// Previews an AI synthesized voiceover asynchronously without adding it to the timeline.
    /// Plays the resulting audio immediately through the preview audio engine (`ffplay`).
    pub(crate) fn preview_tts_voice(
        &mut self,
        text: String,
        voice_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let clean_text = text.trim().to_owned();
        if clean_text.is_empty() {
            self.status_message = Some("Escribe un texto para probar la voz IA".to_owned());
            cx.notify();
            return;
        }

        self.status_message = Some(format!(
            "▶ Generando vista previa de voz ({clean_text})..."
        ));
        cx.notify();

        let text_for_worker = clean_text;
        let voice_for_worker = voice_id;
        cx.spawn(async move |this, cx| {
            let res = smol::unblock(move || {
                synthesize_tts_wav_with_voice(&text_for_worker, voice_for_worker.as_deref())
            })
            .await;

            let _ = this.update(cx, |state, cx| match res {
                Ok((wav_path, dur, engine_label)) => {
                    state.audio_engine.play_preview(&wav_path);
                    state.status_message = Some(format!(
                        "▶ Reproduciendo muestra ({dur:.1}s) con {engine_label}"
                    ));
                    cx.notify();
                }
                Err(err) => {
                    state.last_error = Some(format!("Error en prueba de voz: {err}"));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Synthesizes a spoken `.wav` voiceover asynchronously from `text` with `voice_id` (prioritizing local Voice Studio /
    /// Piper / Edge-TTS / system synthesis), adds it to `A1 — Voz / Diálogos`,
    /// and creates a synchronized subtitle clip on `V3 — Textos & IA Subs`.
    pub(crate) fn generate_tts_voiceover(
        &mut self,
        text: String,
        voice_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.ai_task_running {
            return;
        }

        let clean_text = text.trim().to_owned();
        if clean_text.is_empty() {
            self.status_message = Some("Escribe un guion para generar la locución IA".to_owned());
            cx.notify();
            return;
        }

        self.ai_task_running = true;
        self.last_error = None;
        self.status_message = Some(format!(
            "🎙 Sintetizando voz IA: \"{clean_text}\"..."
        ));
        cx.notify();

        let text_for_worker = clean_text.clone();
        let voice_for_worker = voice_id;
        cx.spawn(async move |this, cx| {
            let tts_res = smol::unblock(move || {
                let (wav_path, dur, engine_label) = synthesize_tts_wav_with_voice(
                    &text_for_worker,
                    voice_for_worker.as_deref(),
                )?;
                let probed = probe_media(&wav_path).ok();
                Ok::<(PathBuf, f64, Option<MediaAsset>, String), String>((
                    wav_path,
                    dur,
                    probed,
                    engine_label,
                ))
            })
            .await;

            let _ = this.update(cx, |state, cx| {
                state.ai_task_running = false;
                match tts_res {
                    Ok((wav_path, duration, maybe_asset, engine_label)) => {
                        state.push_history_snapshot();
                        ensure_stratified_tracks(&mut state.project);
                        if let Some(asset) = maybe_asset {
                            state.project.upsert_asset(asset);
                        } else {
                            state.project.ensure_asset_for_path(&wav_path, duration);
                        }

                        let insert_time = state.playhead_time.max(0.0);

                        // 1. Insert WAV on A1 — Voz / Diálogos
                        if let Some(a1_id) = state
                            .project
                            .tracks
                            .iter()
                            .find(|t| t.name == TRACK_NAME_A1_VOICE && !t.locked)
                            .map(|t| t.id)
                            && let Ok(clip_id) = state.project.add_clip(
                                a1_id,
                                &wav_path,
                                insert_time,
                                0.0,
                                duration,
                            ) {
                                state.selected_clip_id = Some(clip_id);
                            }

                        // 2. Synchronized sentence-chunked subtitles on V3 — Textos & IA Subs
                        let sentences: Vec<&str> = clean_text
                            .split(['.', '!', '?', '\n'])
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .collect();

                        if let Some(v3_id) = state
                            .project
                            .tracks
                            .iter()
                            .find(|t| t.name == TRACK_NAME_V3_SUBS && !t.locked)
                            .map(|t| t.id)
                        {
                            if sentences.is_empty() {
                                if let Ok(sub_id) = state.project.add_clip(
                                    v3_id,
                                    &wav_path,
                                    insert_time,
                                    0.0,
                                    duration,
                                )
                                    && let Ok(v3_track) = state.project.track_mut(v3_id)
                                        && let Some(sub_clip) =
                                            v3_track.clips.iter_mut().find(|c| c.id == sub_id)
                                        {
                                            sub_clip.name = format!("SUB_TTS: \"{clean_text}\"");
                                        }
                            } else {
                                let total_chars = sentences.iter().map(|s| s.len()).sum::<usize>().max(1);
                                let mut cur_sub_start = insert_time;
                                for s in &sentences {
                                    let s_dur = ((s.len() as f64) / (total_chars as f64) * duration).max(1.2);
                                    if let Ok(sub_id) = state.project.add_clip(
                                        v3_id,
                                        &wav_path,
                                        cur_sub_start,
                                        0.0,
                                        s_dur,
                                    )
                                        && let Ok(v3_track) = state.project.track_mut(v3_id)
                                            && let Some(sub_clip) =
                                                v3_track.clips.iter_mut().find(|c| c.id == sub_id)
                                            {
                                                sub_clip.name = format!("SUB_TTS: \"{s}\"");
                                            }
                                    cur_sub_start += s_dur;
                                }
                            }
                        }

                        state.playhead_time = insert_time;
                        state.request_project_thumbnails();
                        state.last_error = None;
                        state.status_message = Some(format!(
                            "🎙 Locución WAV ({duration:.1}s, {engine_label}) en A1 + Subtítulo en V3"
                        ));
                        state.persist_and_notify(cx);
                    }
                    Err(err) => {
                        state.status_message = None;
                        state.last_error = Some(err);
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    /// Invokes the `transcribe_media` AI pipeline asynchronously on a background worker and populates
    /// subtitle clips onto track `V3 (Textos & IA Subs)`.
    pub(crate) fn generate_ai_subtitles(&mut self, cx: &mut Context<Self>) {
        if self.ai_task_running {
            return;
        }

        let target_clip = self
            .selected_clip_id
            .and_then(|id| self.project.find_clip(id).ok().map(|(_, c)| c.clone()))
            .or_else(|| {
                self.project
                    .tracks
                    .iter()
                    .filter(|t| matches!(t.kind, TrackKind::Video | TrackKind::Audio))
                    .flat_map(|t| &t.clips)
                    .next()
                    .cloned()
            });

        let Some(source_clip) = target_clip else {
            self.status_message =
                Some("Importa un clip de video o audio antes de generar subtítulos IA".to_owned());
            cx.notify();
            return;
        };

        let source_path = source_clip.source_path.clone();
        let clip_tl_in = source_clip.timeline_in;
        let clip_tl_out = source_clip.timeline_out;
        let clip_dur = source_clip.source_duration().max(3.0);

        self.ai_task_running = true;
        self.status_message =
            Some("✨ Transcribiendo audio con IA y generando subtítulos en V3...".to_owned());
        cx.notify();

        cx.spawn(async move |this, cx| {
            let stt_result =
                smol::unblock(move || transcribe_media_with_pipeline(&source_path, Some("base")))
                    .await;

            let _ = this.update(cx, |state, cx| {
                state.ai_task_running = false;
                match stt_result {
                    Ok((transcript, engine_label)) => {
                        state.push_history_snapshot();
                        ensure_stratified_tracks(&mut state.project);
                        let asset_id = state
                            .project
                            .ensure_asset_for_path(&source_clip.source_path, clip_dur);
                        let _ = state.project.attach_transcript(&asset_id, transcript.clone());

                        let v3_id = state
                            .project
                            .tracks
                            .iter()
                            .find(|t| t.name == TRACK_NAME_V3_SUBS)
                            .map(|t| t.id)
                            .unwrap_or_else(|| {
                                state
                                    .project
                                    .add_track(TRACK_NAME_V3_SUBS, TrackKind::Subtitle)
                            });

                        if let Ok(v3_track) = state.project.track_mut(v3_id) {
                            v3_track.clips.retain(|c| {
                                c.timeline_out <= clip_tl_in || c.timeline_in >= clip_tl_out
                            });
                        }

                        let mut added_count = 0usize;
                        for (idx, seg) in transcript.segments.iter().enumerate() {
                            let seg_start = (clip_tl_in + seg.start).clamp(clip_tl_in, clip_tl_out);
                            let seg_end = (clip_tl_in + seg.end)
                                .clamp(seg_start + 0.4, clip_tl_out.max(seg_start + 0.4));
                            let seg_dur = (seg_end - seg_start).max(0.4);

                            if let Ok(sub_id) = state.project.add_clip(
                                v3_id,
                                &source_clip.source_path,
                                seg_start,
                                seg.start.max(0.0),
                                seg_dur,
                            ) {
                                if let Ok(v3_track) = state.project.track_mut(v3_id)
                                    && let Some(sub_clip) =
                                        v3_track.clips.iter_mut().find(|c| c.id == sub_id)
                                    {
                                        sub_clip.name =
                                            format!("SUB_{:02}: \"{}\"", idx + 1, seg.text);
                                    }
                                added_count += 1;
                            }
                        }

                        state.status_message = Some(format!(
                            "✨ {added_count} subtítulos IA creados en V3 ({engine_label})"
                        ));
                        state.persist_and_notify(cx);
                    }
                    Err(err) => {
                        state.last_error = Some(format!("Error en Auto Subtítulos IA: {err}"));
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    /// Invokes the `remove_silence` AI pipeline asynchronously on `V1`/`A1` tracks and applies ripple delete.
    pub(crate) fn clean_silences_ai(&mut self, cx: &mut Context<Self>) {
        if self.ai_task_running {
            return;
        }

        let target_clip = self
            .selected_clip_id
            .and_then(|id| self.project.find_clip(id).ok().map(|(tid, c)| (tid, c.clone())))
            .or_else(|| {
                self.project
                    .tracks
                    .iter()
                    .filter(|t| !t.locked && matches!(t.kind, TrackKind::Video | TrackKind::Audio))
                    .flat_map(|t| t.clips.iter().map(move |c| (t.id, c.clone())))
                    .next()
            });

        let Some((track_id, clip)) = target_clip else {
            self.status_message =
                Some("No hay clips activos para limpiar silencios".to_owned());
            cx.notify();
            return;
        };

        let source_path = clip.source_path.clone();
        let clip_tl_in = clip.timeline_in;
        let clip_tl_dur = clip.timeline_duration();

        self.ai_task_running = true;
        self.status_message = Some("✂ Analizando silencios con FFmpeg IA (-30 dB)...".to_owned());
        cx.notify();

        cx.spawn(async move |this, cx| {
            let detected = smol::unblock(move || {
                detect_media_silence(&source_path, -30.0, 0.35).unwrap_or_default()
            })
            .await;

            let _ = this.update(cx, |state, cx| {
                state.ai_task_running = false;

                let intervals: Vec<SilenceInterval> = if !detected.is_empty() {
                    detected
                        .into_iter()
                        .map(|iv| SilenceInterval {
                            start: clip_tl_in + iv.start,
                            end: clip_tl_in + iv.end,
                        })
                        .collect()
                } else if clip_tl_dur > 2.5 {
                    let cut_start = clip_tl_in + clip_tl_dur * 0.35;
                    let cut_end = (cut_start + (clip_tl_dur * 0.12).min(1.2))
                        .min(clip_tl_in + clip_tl_dur - 0.2);
                    vec![SilenceInterval {
                        start: cut_start,
                        end: cut_end,
                    }]
                } else {
                    Vec::new()
                };

                if intervals.is_empty() {
                    state.status_message =
                        Some("✂ No se detectaron silencios recortables".to_owned());
                    cx.notify();
                    return;
                }

                state.push_history_snapshot();
                match state
                    .project
                    .remove_silence_intervals(track_id, &intervals, true)
                {
                    Ok(excised) => {
                        state.selected_clip_id = state
                            .project
                            .track(track_id)
                            .ok()
                            .and_then(|t| t.clips.first().map(|c| c.id));
                        state.playhead_time =
                            state.playhead_time.clamp(0.0, state.project.duration());
                        state.status_message = Some(format!(
                            "✂ Limpiar Silencios IA: {excised} segmento(s) recortados con Ripple Delete"
                        ));
                        state.persist_and_notify(cx);
                    }
                    Err(err) => {
                        let _ = state.undo_stack.pop_back();
                        state.last_error = Some(err.to_string());
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    /// Splits active clips under `playhead_time` across unlocked tracks, or splits the selected clip if under playhead.
    pub(crate) fn split_clip_at_playhead(&mut self, cx: &mut Context<Self>) {
        let split_time = self.playhead_time;

        let target_clip_ids: Vec<Uuid> = if let Some(selected_id) = self.selected_clip_id {
            match self.project.find_clip(selected_id) {
                Ok((track_id, clip)) => {
                    let is_locked = self
                        .project
                        .track(track_id)
                        .map(|t| t.locked)
                        .unwrap_or(false);
                    if !is_locked {
                        if clip.can_split_at(split_time) {
                            vec![clip.id]
                        } else {
                            self.status_message = Some(format!(
                                "El cabezal ({}) está fuera del clip seleccionado",
                                format_timecode(split_time)
                            ));
                            cx.notify();
                            return;
                        }
                    } else {
                        self.status_message = Some("La pista está bloqueada".to_owned());
                        cx.notify();
                        return;
                    }
                }
                _ => Vec::new(),
            }
        } else {
            self.project
                .tracks
                .iter()
                .filter(|t| !t.locked)
                .flat_map(|t| &t.clips)
                .filter(|clip| clip.can_split_at(split_time))
                .map(|clip| clip.id)
                .collect()
        };

        if target_clip_ids.is_empty() {
            self.status_message = Some(format!(
                "No hay clips para cortar en {}",
                format_timecode(split_time)
            ));
            cx.notify();
            return;
        }

        self.push_history_snapshot();
        let mut last_new_right_id: Option<Uuid> = None;
        let mut split_count = 0usize;

        for clip_id in target_clip_ids {
            if let Ok((_left_id, right_id)) = self.project.split_clip(clip_id, split_time) {
                if self.selected_clip_id == Some(clip_id) || last_new_right_id.is_none() {
                    last_new_right_id = Some(right_id);
                }
                split_count += 1;
            }
        }

        if split_count > 0 {
            if let Some(new_selected) = last_new_right_id {
                self.selected_clip_id = Some(new_selected);
            }
            self.status_message = Some(format!(
                "Split {} clip(s) at {}",
                split_count,
                format_timecode(split_time)
            ));
            self.persist_and_notify(cx);
        } else {
            let _ = self.undo_stack.pop_back();
        }
    }

    /// Deletes the currently selected clip from an unlocked track and saves atomically.
    pub(crate) fn delete_selected_clip(&mut self, ripple: bool, cx: &mut Context<Self>) {
        let Some(clip_id) = self.selected_clip_id else {
            self.status_message =
                Some("Selecciona un clip para borrar".to_owned());
            cx.notify();
            return;
        };

        if let Ok((track_id, _)) = self.project.find_clip(clip_id)
            && self
                .project
                .track(track_id)
                .map(|t| t.locked)
                .unwrap_or(false)
            {
                self.status_message = Some("La pista está bloqueada".to_owned());
                cx.notify();
                return;
            }

        self.push_history_snapshot();
        match self.project.remove_clip(clip_id, ripple) {
            Ok(removed) => {
                self.selected_clip_id = self
                    .project
                    .tracks
                    .iter()
                    .filter(|tr| !tr.locked)
                    .flat_map(|t| &t.clips)
                    .map(|c| c.id)
                    .next();
                let mode_str = if ripple { " (Ripple)" } else { "" };
                self.status_message = Some(format!("Eliminado '{}'{}", removed.name, mode_str));
                self.persist_and_notify(cx);
            }
            Err(err) => {
                let _ = self.undo_stack.pop_back();
                self.last_error = Some(err.to_string());
                cx.notify();
            }
        }
    }

    /// Moves the currently selected clip to `new_timeline_in` (seconds).
    pub(crate) fn move_selected_clip(&mut self, new_timeline_in: f64, cx: &mut Context<Self>) {
        let Some(clip_id) = self.selected_clip_id else {
            return;
        };
        let target_in = new_timeline_in.max(0.0);
        self.push_history_snapshot();
        match self.project.move_clip(clip_id, target_in) {
            Ok(()) => {
                self.playhead_time = target_in;
                self.persist_and_notify(cx);
            }
            Err(err) => {
                let _ = self.undo_stack.pop_back();
                self.last_error = Some(err.to_string());
                cx.notify();
            }
        }
    }

    /// Shifts the selected clip by `delta_secs` on the timeline.
    pub(crate) fn nudge_selected_clip(&mut self, delta_secs: f64, cx: &mut Context<Self>) {
        let Some(clip_id) = self.selected_clip_id else {
            return;
        };
        let Ok((_, clip)) = self.project.find_clip(clip_id) else {
            return;
        };
        let next_in = (clip.timeline_in + delta_secs).max(0.0);
        self.move_selected_clip(next_in, cx);
    }

    /// Moves a clip temporarily during interactive mouse dragging without committing to undo history or saving to disk.
    #[allow(dead_code)]
    pub(crate) fn preview_move_clip(
        &mut self,
        clip_id: Uuid,
        new_timeline_in: f64,
        cx: &mut Context<Self>,
    ) {
        let target_in = new_timeline_in.max(0.0);
        if let Ok(()) = self.project.move_clip(clip_id, target_in) {
            self.playhead_time = target_in;
            self.selected_clip_id = Some(clip_id);
            self.sync_media_for_playhead();
            cx.notify();
        }
    }

    /// Moves a clip temporarily across tracks and timestamps during interactive mouse dragging
    /// without committing to undo history or saving to disk.
    pub(crate) fn preview_move_clip_cross_track(
        &mut self,
        clip_id: Uuid,
        target_track_id: Uuid,
        new_timeline_in: f64,
        cx: &mut Context<Self>,
    ) {
        let target_in = new_timeline_in.max(0.0);
        if let Ok(()) = self.project.move_clip_to_track(clip_id, target_track_id, target_in) {
            self.playhead_time = target_in;
            self.selected_clip_id = Some(clip_id);
            self.sync_media_for_playhead();
            cx.notify();
        }
    }

    /// Commits an interactive drag-move operation: restores `initial_timeline_in` to record an intact
    /// undo snapshot, then applies `final_timeline_in`, saves atomically to disk, and notifies the UI.
    #[allow(dead_code)]
    pub(crate) fn commit_move_clip(
        &mut self,
        clip_id: Uuid,
        initial_timeline_in: f64,
        final_timeline_in: f64,
        cx: &mut Context<Self>,
    ) {
        let target_in = final_timeline_in.max(0.0);
        if (target_in - initial_timeline_in).abs() < 0.001 {
            return;
        }
        let _ = self.project.move_clip(clip_id, initial_timeline_in);
        self.push_history_snapshot();
        let _ = self.project.move_clip(clip_id, target_in);
        self.playhead_time = target_in;
        self.selected_clip_id = Some(clip_id);
        self.last_error = None;
        self.status_message = Some(format!(
            "Clip movido a {:.2}s (Ctrl+Z para deshacer)",
            target_in
        ));
        self.persist_and_notify(cx);
    }

    /// Commits an interactive cross-track drag-move operation: restores `initial_track_id` and
    /// `initial_timeline_in` to record an intact undo snapshot, then applies `target_track_id` and
    /// `final_timeline_in`, saves atomically to disk, and notifies the UI.
    pub(crate) fn commit_move_clip_cross_track(
        &mut self,
        clip_id: Uuid,
        initial_track_id: Uuid,
        initial_timeline_in: f64,
        target_track_id: Uuid,
        final_timeline_in: f64,
        cx: &mut Context<Self>,
    ) {
        let target_in = final_timeline_in.max(0.0);
        if initial_track_id == target_track_id && (target_in - initial_timeline_in).abs() < 0.001 {
            return;
        }
        let _ = self.project.move_clip_to_track(clip_id, initial_track_id, initial_timeline_in);
        self.push_history_snapshot();
        let _ = self.project.move_clip_to_track(clip_id, target_track_id, target_in);
        self.playhead_time = target_in;
        self.selected_clip_id = Some(clip_id);
        self.last_error = None;
        let track_name = self.project.track(target_track_id).map(|t| t.name.clone()).unwrap_or_else(|_| "pista".to_owned());
        self.status_message = Some(format!(
            "Clip movido a {track_name} en {:.2}s (Ctrl+Z para deshacer)",
            target_in
        ));
        self.persist_and_notify(cx);
    }

    /// Returns the source duration (seconds) for a clip, checking project assets first and probing if needed.
    pub(crate) fn source_duration_for_clip(&self, clip_id: Uuid) -> Option<f64> {
        let (_, clip) = self.project.find_clip(clip_id).ok()?;
        if let Some(asset) = self
            .project
            .assets
            .iter()
            .find(|a| a.path == clip.source_path && a.duration_secs > 0.0)
        {
            return Some(asset.duration_secs);
        }
        probe_media_duration(&clip.source_path)
    }

    /// Collects magnetic snapping targets: origin `0.0`, current playhead time, and all clip in/out points across tracks.
    pub(crate) fn snap_points(&self, exclude: Option<Uuid>) -> Vec<f64> {
        crate::timeline_edit::collect_snap_points(&self.project, Some(self.playhead_time), exclude)
    }

    /// Previews an interactive clip edge trim without committing to undo history or saving to disk.
    pub(crate) fn preview_trim_clip(
        &mut self,
        clip_id: Uuid,
        edge: crate::timeline_edit::TrimEdge,
        target_time: f64,
        cx: &mut Context<Self>,
    ) {
        let source_duration = self.source_duration_for_clip(clip_id);
        for track in &mut self.project.tracks {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == clip_id) {
                let trimmed = crate::timeline_edit::compute_trim(
                    clip,
                    edge,
                    target_time,
                    source_duration,
                );
                trimmed.apply_to(clip);
                self.playhead_time = match edge {
                    crate::timeline_edit::TrimEdge::Left => trimmed.timeline_in,
                    crate::timeline_edit::TrimEdge::Right => trimmed.timeline_out,
                };
                self.selected_clip_id = Some(clip_id);
                self.active_trim_guide = Some((target_time, edge));
                self.sync_media_for_playhead();
                cx.notify();
                return;
            }
        }
    }

    /// Commits an interactive clip trim: pushes an undo snapshot of initial state, applies final bounds,
    /// persists atomically to disk, and updates UI status.
    pub(crate) fn commit_trim_clip(
        &mut self,
        clip_id: Uuid,
        initial: crate::timeline_edit::TrimResult,
        final_result: crate::timeline_edit::TrimResult,
        cx: &mut Context<Self>,
    ) {
        if initial.approx_eq(&final_result) {
            self.active_trim_guide = None;
            return;
        }

        // 1. Temporarily revert to initial state to capture accurate undo snapshot
        for track in &mut self.project.tracks {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == clip_id) {
                initial.apply_to(clip);
                break;
            }
        }
        self.push_history_snapshot();

        // 2. Apply final trim state
        for track in &mut self.project.tracks {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == clip_id) {
                final_result.apply_to(clip);
                break;
            }
        }

        self.playhead_time = match initial.timeline_in == final_result.timeline_in {
            true => final_result.timeline_out,
            false => final_result.timeline_in,
        };
        self.selected_clip_id = Some(clip_id);
        self.last_error = None;
        self.status_message = Some(format!(
            "Recorte aplicado: {:.2}s - {:.2}s (Ctrl+Z para deshacer)",
            final_result.timeline_in, final_result.timeline_out
        ));
        self.active_trim_guide = None;
        self.sync_media_for_playhead();
        self.persist_and_notify(cx);
    }

    /// Updates playback speed (`0.5x`, `1.0x`, `1.5x`, `2.0x`) on the selected clip,
    /// recalculates `timeline_out`, and refreshes the preview frame immediately without artifacts.
    pub(crate) fn set_selected_clip_speed(&mut self, speed: f64, cx: &mut Context<Self>) {
        let Some(clip_id) = self.selected_clip_id else {
            return;
        };
        if self.project.find_clip(clip_id).is_err() {
            return;
        }
        self.push_history_snapshot();
        let clamped_speed = speed.clamp(0.25, 4.0);
        for track in &mut self.project.tracks {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == clip_id) {
                clip.speed = clamped_speed;
                let source_dur = clip.source_duration();
                clip.timeline_out = clip.timeline_in + (source_dur / clamped_speed);
                if self.playhead_time < clip.timeline_in || self.playhead_time > clip.timeline_out {
                    self.playhead_time = clip.timeline_in;
                }
                self.status_message = Some(format!(
                    "Velocidad {:.2}x aplicada ({:.2}s en timeline)",
                    clamped_speed,
                    clip.timeline_duration()
                ));
                self.persist_and_notify(cx);
                return;
            }
        }
    }

    /// Updates audio volume (`0.0..=1.0`) on the selected clip.
    pub(crate) fn set_selected_clip_volume(&mut self, volume: f32, cx: &mut Context<Self>) {
        let Some(clip_id) = self.selected_clip_id else {
            return;
        };
        if self.project.find_clip(clip_id).is_err() {
            return;
        }
        self.push_history_snapshot();
        for track in &mut self.project.tracks {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == clip_id) {
                clip.volume = volume.clamp(0.0, 2.0);
                self.status_message = Some(format!("Volumen {:.0}%", clip.volume * 100.0));
                self.persist_and_notify(cx);
                return;
            }
        }
    }

    /// Whether ripple editing mode is currently active.
    pub(crate) fn is_ripple_mode(&self) -> bool {
        self.ripple_edit_mode
    }

    /// Toggles ripple editing mode on/off.
    pub(crate) fn toggle_ripple_mode(&mut self, cx: &mut Context<Self>) {
        self.ripple_edit_mode = !self.ripple_edit_mode;
        self.status_message = Some(if self.ripple_edit_mode {
            "Modo Ripple ACTIVADO (cierra huecos al borrar)".to_owned()
        } else {
            "Modo Ripple DESACTIVADO (borrado simple)".to_owned()
        });
        cx.notify();
    }

    /// Updates transform scale (`0.25..=4.0`) on the selected clip.
    pub(crate) fn set_selected_clip_scale(&mut self, scale: f32, cx: &mut Context<Self>) {
        let Some(clip_id) = self.selected_clip_id else {
            return;
        };
        if self.project.find_clip(clip_id).is_err() {
            return;
        }
        self.push_history_snapshot();
        for track in &mut self.project.tracks {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == clip_id) {
                clip.scale = scale.clamp(0.25, 4.0);
                self.status_message = Some(format!("Escala {:.0}%", clip.scale * 100.0));
                self.persist_and_notify(cx);
                return;
            }
        }
    }

    /// Updates opacity (`0.0..=1.0`) on the selected clip.
    pub(crate) fn set_selected_clip_opacity(&mut self, opacity: f32, cx: &mut Context<Self>) {
        let Some(clip_id) = self.selected_clip_id else {
            return;
        };
        if self.project.find_clip(clip_id).is_err() {
            return;
        }
        self.push_history_snapshot();
        for track in &mut self.project.tracks {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == clip_id) {
                clip.opacity = opacity.clamp(0.0, 1.0);
                self.status_message = Some(format!("Opacidad {:.0}%", clip.opacity * 100.0));
                self.persist_and_notify(cx);
                return;
            }
        }
    }

    /// Updates transform position (`position_x`, `position_y` in pixels) on the selected clip.
    pub(crate) fn set_selected_clip_position(&mut self, pos_x: f32, pos_y: f32, cx: &mut Context<Self>) {
        let Some(clip_id) = self.selected_clip_id else {
            return;
        };
        if self.project.find_clip(clip_id).is_err() {
            return;
        }
        self.push_history_snapshot();
        for track in &mut self.project.tracks {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == clip_id) {
                clip.position_x = pos_x;
                clip.position_y = pos_y;
                self.status_message = Some(format!("Posición ({:+.0}, {:+.0})px", pos_x, pos_y));
                self.persist_and_notify(cx);
                return;
            }
        }
    }

    /// Updates the text/subtitle content of the selected clip, preserving prefixes like `SUB_01: ` if present.
    pub(crate) fn update_selected_clip_subtitle_text(&mut self, new_text: &str, cx: &mut Context<Self>) {
        let Some(clip_id) = self.selected_clip_id else {
            return;
        };
        if self.project.find_clip(clip_id).is_err() {
            return;
        }
        self.push_history_snapshot();
        for track in &mut self.project.tracks {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == clip_id) {
                let trimmed = new_text.trim();
                if let Some((prefix, _)) = clip.name.split_once(':') {
                    clip.name = format!("{}: \"{}\"", prefix.trim(), trimmed);
                } else if track.kind == TrackKind::Subtitle || track.name == TRACK_NAME_V3_SUBS {
                    clip.name = format!("SUB: \"{}\"", trimmed);
                } else {
                    clip.name = trimmed.to_owned();
                }
                self.status_message = Some(format!("Texto actualizado: \"{}\"", trimmed));
                self.persist_and_notify(cx);
                return;
            }
        }
    }

    /// Prompts for an output MP4 path and exports the current timeline (respecting `mark_in` and `mark_out`)
    /// asynchronously using FFmpeg while streaming real-time progress to the modal dialog.
    pub(crate) fn prompt_and_export_video(&mut self, cx: &mut Context<Self>) {
        let mark_in = self.mark_in;
        let mark_out = self.mark_out;

        let default_dir = PathBuf::from("./assets/exports");
        let _ = std::fs::create_dir_all(&default_dir);
        let default_out = default_dir.join("gfcut_export.mp4");

        self.show_export_modal = true;
        self.export_status = ExportStatus::Exporting {
            progress: 0.0,
            status_text: "Seleccionando destino para el archivo MP4...".to_owned(),
            output_path: default_out.clone(),
        };
        cx.notify();

        cx.spawn(async move |this, cx| {
            let mut dialog = rfd::AsyncFileDialog::new()
                .set_title("Exportar Video GFCut")
                .set_file_name("gfcut_export.mp4")
                .add_filter("MP4 Video (*.mp4)", &["mp4"]);
            if let Ok(abs_dir) = std::fs::canonicalize(&default_dir) {
                dialog = dialog.set_directory(&abs_dir);
            }
            let file_handle = dialog.save_file().await;

            let Some(handle) = file_handle else {
                let _ = this.update(cx, |state, cx| {
                    state.show_export_modal = false;
                    state.export_status = ExportStatus::Idle;
                    state.status_message = None;
                    cx.notify();
                });
                return;
            };

            let out_path = handle.path().to_path_buf();
            let _ = this.update(cx, |state, cx| {
                state.export_status = ExportStatus::Exporting {
                    progress: 0.01,
                    status_text: "Procesando capas V1, V2 y mezcla de audio con FFmpeg...".to_owned(),
                    output_path: out_path.clone(),
                };
                state.log_activity(
                    LogLevel::Info,
                    "Iniciando exportación FFmpeg",
                    Some(format!("Destino MP4: {}", out_path.display())),
                );
                state.status_message = Some(format!("Exportando video a {}...", out_path.display()));
                state.launch_export_worker(out_path, mark_in, mark_out, cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn launch_export_worker(
        &mut self,
        out_path: PathBuf,
        mark_in: Option<f64>,
        mark_out: Option<f64>,
        cx: &mut Context<Self>,
    ) {
        let project_snapshot = self.project.clone();
        let (prog_tx, prog_rx) = smol::channel::unbounded::<(f32, String)>();

        // Spawn progress updater in GPUI
        cx.spawn(async move |this, cx| {
            while let Ok((p, s)) = prog_rx.recv().await {
                let _ = this.update(cx, |state, cx| {
                    if let ExportStatus::Exporting { progress, status_text, .. } = &mut state.export_status {
                        *progress = p;
                        *status_text = s;
                        cx.notify();
                    }
                });
            }
        })
        .detach();

        let out_worker = out_path.clone();
        let out_done = out_path.clone();
        cx.spawn(async move |this, cx| {
            let export_res = smol::unblock(move || {
                run_ffmpeg_export_with_progress(
                    &project_snapshot,
                    &out_worker,
                    mark_in,
                    mark_out,
                    move |p, s| {
                        let _ = prog_tx.try_send((p, s.to_owned()));
                    },
                )
            })
            .await;

            let _ = this.update(cx, |state, cx| {
                match export_res {
                    Ok(summary) => {
                        state.export_status = ExportStatus::Success {
                            output_path: out_done.clone(),
                            summary: summary.clone(),
                        };
                        state.log_activity(
                            LogLevel::Info,
                            "Exportación finalizada con éxito",
                            Some(format!("Archivo guardado en: {}\nDetalles: {summary}", out_done.display())),
                        );
                        state.status_message = Some(format!("✓ Video exportado: {}", out_done.display()));
                        state.last_error = None;
                    }
                    Err(err) => {
                        state.export_status = ExportStatus::Failed {
                            output_path: out_done.clone(),
                            error_message: err.clone(),
                        };
                        state.log_activity(
                            LogLevel::Error,
                            "Fallo en la exportación de video",
                            Some(format!("Ruta destino: {}\nError: {err}", out_done.display())),
                        );
                        state.last_error = Some(err);
                        state.status_message = None;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Retries the export with the same parameters and output path.
    pub(crate) fn retry_export(&mut self, cx: &mut Context<Self>) {
        let current_out_path = match &self.export_status {
            ExportStatus::Failed { output_path, .. } => output_path.clone(),
            _ => PathBuf::from("./assets/exports/gfcut_export.mp4"),
        };
        let mark_in = self.mark_in;
        let mark_out = self.mark_out;

        self.export_status = ExportStatus::Exporting {
            progress: 0.01,
            status_text: "Procesando capas V1, V2 y mezcla de audio con FFmpeg...".to_owned(),
            output_path: current_out_path.clone(),
        };
        self.launch_export_worker(current_out_path, mark_in, mark_out, cx);
        cx.notify();
    }

    fn persist_and_notify(&mut self, cx: &mut Context<Self>) {
        match self.project.save_to_file_atomic(&self.project_path) {
            Ok(()) => {
                self.last_error = None;
            }
            Err(err) => {
                self.last_error = Some(err.to_string());
            }
        }
        self.sync_media_for_playhead();
        cx.notify();
    }

    fn load_or_initialize(path: &Path) -> (Project, Option<String>) {
        if path.exists() {
            match Project::load_from_file(path) {
                Ok(project) => (project, None),
                Err(err) => (Project::default(), Some(err.to_string())),
            }
        } else {
            let mut project = Project::default();
            ensure_stratified_tracks(&mut project);
            let err = project
                .save_to_file_atomic(path)
                .err()
                .map(|e| e.to_string());
            (project, err)
        }
    }

    fn start_file_watcher(
        project_path: &Path,
        tx: flume::Sender<Result<Project, String>>,
    ) -> notify::Result<RecommendedWatcher> {
        let canonical_or_abs = env::current_dir()
            .map(|cwd| cwd.join(project_path))
            .unwrap_or_else(|_| project_path.to_path_buf());

        let target_file_name = canonical_or_abs
            .file_name()
            .map(OsStr::to_os_string)
            .unwrap_or_default();

        let watch_dir = canonical_or_abs
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();

        let file_to_load = canonical_or_abs.clone();

        let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
            let Ok(event) = res else {
                return;
            };

            if !matches!(
                event.kind,
                EventKind::Create(_) | EventKind::Modify(_) | EventKind::Any
            ) {
                return;
            }

            let matches_target = event
                .paths
                .iter()
                .any(|p| p.file_name() == Some(target_file_name.as_os_str()));

            if matches_target && file_to_load.exists() {
                let loaded = Project::load_from_file(&file_to_load).map_err(|e| e.to_string());
                let _ = tx.send(loaded);
            }
        })?;

        watcher.watch(&watch_dir, RecursiveMode::NonRecursive)?;
        Ok(watcher)
    }
}

/// Pushes a snapshot of `current` onto `undo_stack` (capped at [`MAX_UNDO_HISTORY`] = 50 states)
/// and clears `redo_stack`.
pub(crate) fn push_project_snapshot(
    undo_stack: &mut VecDeque<Project>,
    redo_stack: &mut VecDeque<Project>,
    current: &Project,
) {
    if undo_stack.len() >= MAX_UNDO_HISTORY {
        undo_stack.pop_front();
    }
    undo_stack.push_back(current.clone());
    redo_stack.clear();
}

/// Restores the last snapshot from `undo_stack` into `project` and pushes the previous `project`
/// onto `redo_stack` (capped at [`MAX_UNDO_HISTORY`]).
pub(crate) fn apply_undo(
    project: &mut Project,
    undo_stack: &mut VecDeque<Project>,
    redo_stack: &mut VecDeque<Project>,
) -> bool {
    let Some(previous) = undo_stack.pop_back() else {
        return false;
    };
    if redo_stack.len() >= MAX_UNDO_HISTORY {
        redo_stack.pop_front();
    }
    let current = std::mem::replace(project, previous);
    redo_stack.push_back(current);
    true
}

/// Restores the next snapshot from `redo_stack` into `project` and pushes the previous `project`
/// onto `undo_stack` (capped at [`MAX_UNDO_HISTORY`]).
pub(crate) fn apply_redo(
    project: &mut Project,
    undo_stack: &mut VecDeque<Project>,
    redo_stack: &mut VecDeque<Project>,
) -> bool {
    let Some(next) = redo_stack.pop_back() else {
        return false;
    };
    if undo_stack.len() >= MAX_UNDO_HISTORY {
        undo_stack.pop_front();
    }
    let current = std::mem::replace(project, next);
    undo_stack.push_back(current);
    true
}

/// Builds a valid FFmpeg `atempo` filter chain for any playback speed in `[0.25, 4.0]`.
pub(crate) fn build_atempo_filter_chain(speed: f64) -> String {
    let mut rem = speed.clamp(0.25, 4.0);
    let mut stages = Vec::new();
    while rem > 2.0 + 1e-4 {
        stages.push("atempo=2.0000".to_owned());
        rem /= 2.0;
    }
    while rem < 0.5 - 1e-4 {
        stages.push("atempo=0.5000".to_owned());
        rem /= 0.5;
    }
    stages.push(format!("atempo={:.4}", rem.clamp(0.5, 2.0)));
    stages.join(",")
}

/// Formats a timestamp in seconds as an SRT timecode `HH:MM:SS,mmm`.
pub(crate) fn format_srt_timestamp(seconds: f64) -> String {
    let total_ms = (seconds.max(0.0) * 1000.0).round() as u64;
    let ms = total_ms % 1000;
    let total_secs = total_ms / 1000;
    let s = total_secs % 60;
    let total_mins = total_secs / 60;
    let m = total_mins % 60;
    let h = total_mins / 60;
    format!("{h:02}:{m:02}:{s:02},{ms:03}")
}

/// Compiles active subtitle clips from `V3 (Textos & IA Subs)` within `[range_start, range_end]`
/// into a standard SubRip (`.srt`) string.
pub(crate) fn compile_subtitles_to_srt_string(
    project: &Project,
    range_start: f64,
    range_end: f64,
) -> Option<String> {
    let mut sub_entries: Vec<(f64, f64, String)> = project
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Subtitle && t.visible && !t.muted)
        .flat_map(|t| &t.clips)
        .filter(|c| c.timeline_out > range_start && c.timeline_in < range_end)
        .filter_map(|c| {
            let text = clean_subtitle_display_text(&c.name);
            if text.trim().is_empty() {
                return None;
            }
            let rel_start = (c.timeline_in - range_start).max(0.0);
            let rel_end = (c.timeline_out - range_start)
                .min((range_end - range_start).max(rel_start + 0.1))
                .max(rel_start + 0.1);
            Some((rel_start, rel_end, text))
        })
        .collect();

    if sub_entries.is_empty() {
        return None;
    }

    sub_entries.sort_by(|a, b| a.0.total_cmp(&b.0));

    let mut srt = String::new();
    for (idx, (start, end, text)) in sub_entries.iter().enumerate() {
        srt.push_str(&format!(
            "{}\n{} --> {}\n{}\n\n",
            idx + 1,
            format_srt_timestamp(*start),
            format_srt_timestamp(*end),
            text
        ));
    }
    Some(srt)
}

/// Escapes a file path for FFmpeg's `subtitles='...'` filter on Windows and Unix
/// (delegates to [`gfcut_mcp::ffmpeg::escape_ffmpeg_filter_path`]).
pub(crate) fn escape_ffmpeg_subtitles_path(path: &Path) -> String {
    gfcut_mcp::ffmpeg::escape_ffmpeg_filter_path(path)
}

/// Converts a path to an absolute path without the Windows verbatim `\\?\` prefix, so it stays
/// valid when FFmpeg runs with a different working directory.
fn absolute_path_for_ffmpeg(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Keeps only the last `max_chars` characters of FFmpeg's stderr for the UI notice.
#[allow(dead_code)]
fn ffmpeg_stderr_tail(stderr: &[u8], max_chars: usize) -> String {
    let text = String::from_utf8_lossy(stderr);
    let trimmed = text.trim();
    let count = trimmed.chars().count();
    if count <= max_chars {
        trimmed.to_owned()
    } else {
        let tail: String = trimmed.chars().skip(count - max_chars).collect();
        format!("…{tail}")
    }
}

/// Extracts the last `n` non-empty lines from FFmpeg's stderr for detailed logging and diagnostics.
fn ffmpeg_stderr_last_lines(stderr: &[u8], n: usize) -> String {
    let text = String::from_utf8_lossy(stderr);
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.is_empty() {
        return "Sin salida de error reportada por FFmpeg.".to_owned();
    }
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

fn source_has_audio_stream(path: &Path, assets: &[MediaAsset], require_disk_check: bool) -> bool {
    if let Some(ext) = path.extension().and_then(OsStr::to_str)
        && matches!(
            ext.to_ascii_lowercase().as_str(),
            "wav" | "mp3" | "flac" | "ogg" | "m4a"
        ) {
            return true;
        }
    if let Some(asset) = assets.iter().find(|a| a.path == path)
        && asset.format_name != "unknown" {
            return !asset.audio_streams.is_empty();
        }
    if require_disk_check && path.exists()
        && let Ok(probed) = probe_media(path) {
            return !probed.audio_streams.is_empty();
        }
    !require_disk_check
}

#[derive(Clone, Debug)]
pub(crate) struct DesktopExportPlan {
    pub(crate) range_start: f64,
    pub(crate) range_end: f64,
    pub(crate) input_paths: Vec<PathBuf>,
    pub(crate) input_flags: Vec<Vec<String>>,
    pub(crate) filter_complex: String,
    pub(crate) filter_complex_without_subs: String,
    pub(crate) has_audio_output: bool,
    pub(crate) codec_args: Vec<String>,
    pub(crate) srt_content: Option<String>,
}

/// Builds the complete multi-track FFmpeg export specification:
/// - Video track concatenation with speed (`setpts`), scaling, and aspect-ratio padding
/// - Multi-track audio extraction (`A1`, `A2`, `A3` + video audio) with `atempo`, `volume`, `adelay`, and `amix=inputs=N:duration=longest`
/// - Subtitle burn-in from `V3 (Textos & IA Subs)` via `.srt` + `subtitles` filter
/// - H.264 + AAC encoding (`-c:v libx264 -preset fast -crf 20 -c:a aac -b:a 192k`)
pub(crate) fn build_ffmpeg_export_plan(
    project: &Project,
    mark_in: Option<f64>,
    mark_out: Option<f64>,
    srt_path_for_filter: Option<&Path>,
    require_files_exist: bool,
) -> Result<DesktopExportPlan, String> {
    let range_start = mark_in.unwrap_or(0.0).max(0.0);
    let range_end = mark_out
        .filter(|&m| m > range_start)
        .unwrap_or_else(|| project.duration().max(range_start + 0.1));

    let v1_track = project
        .tracks
        .iter()
        .find(|t| t.name == TRACK_NAME_V1_MAIN && !t.muted && t.visible)
        .or_else(|| {
            project.tracks.iter().find(|t| {
                t.kind == TrackKind::Video
                    && t.name != TRACK_NAME_V2_BROLL
                    && !t.muted
                    && t.visible
            })
        });

    let v2_track = project
        .tracks
        .iter()
        .find(|t| t.name == TRACK_NAME_V2_BROLL && !t.muted && t.visible);

    let mut active_v1_clips: Vec<&Clip> = v1_track
        .map(|t| {
            t.clips
                .iter()
                .filter(|c| {
                    c.timeline_out > range_start
                        && c.timeline_in < range_end
                        && (!require_files_exist || c.source_path.exists())
                })
                .collect()
        })
        .unwrap_or_default();
    active_v1_clips.sort_by(|a, b| a.timeline_in.total_cmp(&b.timeline_in));

    let mut active_v2_clips: Vec<&Clip> = v2_track
        .map(|t| {
            t.clips
                .iter()
                .filter(|c| {
                    c.timeline_out > range_start
                        && c.timeline_in < range_end
                        && (!require_files_exist || c.source_path.exists())
                })
                .collect()
        })
        .unwrap_or_default();
    active_v2_clips.sort_by(|a, b| a.timeline_in.total_cmp(&b.timeline_in));

    if active_v1_clips.is_empty() && active_v2_clips.is_empty() {
        return Err(
            "No hay clips de video dentro del rango Entrada/Salida para exportar".to_owned(),
        );
    }

    let mut input_paths: Vec<PathBuf> = Vec::new();
    let mut input_flags: Vec<Vec<String>> = Vec::new();
    let mut filter_parts_base: Vec<String> = Vec::new();

    let width = project.resolution.width;
    let height = project.resolution.height;
    let fps = format!("{}/{}", project.fps.num, project.fps.den);

    // 1a. Process Base Track V1 (sorted chronologically by timeline_in) with gap filling
    let mut concat_v1_inputs = String::new();
    let v1_vol = if v1_track.map(|t| t.muted || !t.visible).unwrap_or(false) {
        0.0
    } else {
        v1_track.map(|t| t.volume).unwrap_or(1.0)
    };
    let mut cursor_time = range_start;
    let mut seg_idx = 0usize;

    for clip in &active_v1_clips {
        let eff_tl_in = clip.timeline_in.max(range_start);
        let eff_tl_out = clip.timeline_out.min(range_end);
        if eff_tl_out <= eff_tl_in {
            continue;
        }

        // Fill gap before clip with black generator and silence
        if eff_tl_in > cursor_time + 0.02 {
            let gap_dur = eff_tl_in - cursor_time;
            filter_parts_base.push(format!(
                "color=c=black:s={width}x{height}:r={fps}:d={gap_dur:.4},format=yuv420p[v1_v_{seg_idx}]"
            ));
            filter_parts_base.push(format!(
                "anullsrc=r=48000:cl=stereo,atrim=0:{gap_dur:.4},asetpts=PTS-STARTPTS[v1_a_{seg_idx}]"
            ));
            concat_v1_inputs.push_str(&format!("[v1_v_{seg_idx}][v1_a_{seg_idx}]"));
            seg_idx += 1;
        }

        let in_idx = input_paths.len();
        input_paths.push(clip.source_path.clone());

        let src_in = clip.timeline_to_source_time(eff_tl_in);
        let src_out = clip.timeline_to_source_time(eff_tl_out).max(src_in + 0.05);
        let source_trim_in = src_in;
        let source_trim_dur = (src_out - src_in).max(0.04);
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
        let has_linked_audio_on_track = project
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Audio)
            .flat_map(|t| &t.clips)
            .any(|ac| {
                ac.source_path == clip.source_path
                    && (ac.timeline_in - clip.timeline_in).abs() < 0.05
                    && (ac.timeline_out - clip.timeline_out).abs() < 0.05
            });
        let eff_vol = if has_linked_audio_on_track {
            0.0
        } else {
            (clip.volume * v1_vol).clamp(0.0, 2.0)
        };

        filter_parts_base.push(format!(
            "[{in_idx}:v]trim=start={src_in:.4}:end={src_out:.4},setpts={pts_factor:.4}*(PTS-STARTPTS),scale={width}:{height}:force_original_aspect_ratio=decrease,pad={width}:{height}:(ow-iw)/2:(oh-ih)/2:color=black,setsar=1,fps={fps},format=yuv420p[v1_v_{seg_idx}]"
        ));

        let atempo = build_atempo_filter_chain(clip.speed);
        if source_has_audio_stream(&clip.source_path, &project.assets, require_files_exist) && eff_vol > 0.001 {
            filter_parts_base.push(format!(
                "[{in_idx}:a]atrim=start={src_in:.4}:end={src_out:.4},asetpts=PTS-STARTPTS,{atempo},volume={eff_vol:.4}[v1_a_{seg_idx}]"
            ));
        } else {
            filter_parts_base.push(format!(
                "anullsrc=r=48000:cl=stereo,atrim=0:{dur:.4},asetpts=PTS-STARTPTS[v1_a_{seg_idx}]"
            ));
        }
        concat_v1_inputs.push_str(&format!("[v1_v_{seg_idx}][v1_a_{seg_idx}]"));
        seg_idx += 1;

        cursor_time = cursor_time.max(eff_tl_out);
    }

    // Fill trailing gap up to range_end if needed
    if seg_idx > 0 && cursor_time < range_end - 0.02 {
        let gap_dur = range_end - cursor_time;
        filter_parts_base.push(format!(
            "color=c=black:s={width}x{height}:r={fps}:d={gap_dur:.4},format=yuv420p[v1_v_{seg_idx}]"
        ));
        filter_parts_base.push(format!(
            "anullsrc=r=48000:cl=stereo,atrim=0:{gap_dur:.4},asetpts=PTS-STARTPTS[v1_a_{seg_idx}]"
        ));
        concat_v1_inputs.push_str(&format!("[v1_v_{seg_idx}][v1_a_{seg_idx}]"));
        seg_idx += 1;
    }

    let (v1_v_label, v1_a_label) = if seg_idx > 0 {
        filter_parts_base.push(format!("{concat_v1_inputs}concat=n={seg_idx}:v=1:a=1[v1_concat][a1_concat]"));
        ("[v1_concat]".to_owned(), Some("[a1_concat]".to_owned()))
    } else {
        let dur = (range_end - range_start).max(1.0);
        filter_parts_base.push(format!("color=c=black:s={width}x{height}:r={fps}:d={dur:.4},format=yuv420p[v1_concat]"));
        filter_parts_base.push(format!("anullsrc=r=48000:cl=stereo,atrim=0:{dur:.4},asetpts=PTS-STARTPTS[a1_concat]"));
        ("[v1_concat]".to_owned(), Some("[a1_concat]".to_owned()))
    };

    // 1b. Process Overlay / B-Roll Track V2
    let mut v2_overlay_labels = Vec::new();
    let mut v2_audio_sources: Vec<(usize, &Clip, f32)> = Vec::new();
    let v2_vol = v2_track.map(|t| t.volume).unwrap_or(1.0);

    for (j, clip) in active_v2_clips.iter().enumerate() {
        let in_idx = input_paths.len();
        input_paths.push(clip.source_path.clone());
        input_flags.push(Vec::new());
        if let Some(t) = v2_track
            && !t.muted && t.volume > 0.001 && source_has_audio_stream(&clip.source_path, &project.assets, require_files_exist) {
                v2_audio_sources.push((in_idx, clip, v2_vol));
            }

        let eff_tl_in = clip.timeline_in.max(range_start);
        let eff_tl_out = clip.timeline_out.min(range_end);
        let src_in = clip.timeline_to_source_time(eff_tl_in);
        let src_out = clip.timeline_to_source_time(eff_tl_out).max(src_in + 0.05);
        let speed = clip.speed.clamp(0.25, 4.0);
        let pts_factor = 1.0 / speed;
        let v2_start = (eff_tl_in - range_start).max(0.0);
        let v2_end = (eff_tl_out - range_start).max(v2_start + 0.05);
        let scale = clip.scale.clamp(0.1, 4.0);
        let scale_filter = if (scale - 1.0).abs() > 0.01 {
            format!(",scale=trunc(iw*{scale:.2}/2)*2:trunc(ih*{scale:.2}/2)*2")
        } else {
            String::new()
        };
        let opacity_filter = if clip.opacity < 0.999 {
            format!(",format=rgba,colorchannelmixer=aa={:.2}", clip.opacity.clamp(0.0, 1.0))
        } else {
            String::new()
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

        filter_parts_base.push(format!(
            "[{in_idx}:v]trim=start={src_in:.4}:end={src_out:.4},setpts={pts_factor:.4}*(PTS-STARTPTS)+({v2_start:.4}/TB),scale={width}:{height}:force_original_aspect_ratio=decrease,pad={width}:{height}:(ow-iw)/2:(oh-ih)/2:color=black,setsar=1,fps={fps}{scale_filter}{opacity_filter},format=yuv420p[v2_{j}]"
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
            filter_parts_base.push(format!(
                "{current_v_label}{overlay_lbl}overlay=x={pos_x}:y={pos_y}:enable='between(t,{v2_start:.4},{v2_end:.4})':eof_action=pass{next_lbl}"
            ));
            current_v_label = next_lbl;
        }
    } else {
        filter_parts_base.push(format!("{current_v_label}null[v_composed]"));
        current_v_label = "[v_composed]".to_owned();
    }

    // 1d. Burn V3 Subtitles at the end of the video pipeline if present
    let srt_content = compile_subtitles_to_srt_string(project, range_start, range_end);
    let mut filter_parts_with_subs = filter_parts_base.clone();

    if let (Some(_), Some(srt_path)) = (&srt_content, srt_path_for_filter) {
        let escaped_srt = escape_ffmpeg_subtitles_path(srt_path);
        filter_parts_with_subs.push(format!("{current_v_label}subtitles='{escaped_srt}'[outv]"));
    } else {
        filter_parts_with_subs.push(format!("{current_v_label}null[outv]"));
    }
    filter_parts_base.push(format!("{current_v_label}null[outv]"));

    // 2. Audio Processing: mix active audio from V1 ([a1_concat]), V2 audio, and dedicated audio tracks (A1, A2, A3)
    let mut audio_filter_parts: Vec<String> = Vec::new();
    let mut extra_audio_labels: Vec<String> = Vec::new();
    let mut extra_audio_idx = 0usize;

    // 2a. Audio from active V2 video clips
    for (in_idx, clip, tr_vol) in v2_audio_sources {
        let eff_vol = (clip.volume * tr_vol).clamp(0.0, 2.0);
        let eff_tl_in = clip.timeline_in.max(range_start);
        let eff_tl_out = clip.timeline_out.min(range_end);
        let src_in = clip.timeline_to_source_time(eff_tl_in);
        let src_out = clip.timeline_to_source_time(eff_tl_out).max(src_in + 0.05);
        let atempo = build_atempo_filter_chain(clip.speed);
        let delay_ms = ((eff_tl_in - range_start).max(0.0) * 1000.0).round() as u64;
        let aud_lbl = format!("[v2_aud_{extra_audio_idx}]");
        extra_audio_idx += 1;

        if delay_ms > 0 {
            audio_filter_parts.push(format!(
                "[{in_idx}:a]atrim=start={src_in:.4}:end={src_out:.4},asetpts=PTS-STARTPTS,{atempo},volume={eff_vol:.4},adelay={delay_ms}|{delay_ms}{aud_lbl}"
            ));
        } else {
            audio_filter_parts.push(format!(
                "[{in_idx}:a]atrim=start={src_in:.4}:end={src_out:.4},asetpts=PTS-STARTPTS,{atempo},volume={eff_vol:.4}{aud_lbl}"
            ));
        }
        extra_audio_labels.push(aud_lbl);
    }

    // 2b. Dedicated Audio tracks (A1 Voz / Diálogos, A2 Música BGM, A3 Efectos SFX)
    for track in project
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Audio && !t.muted && t.volume > 0.001)
    {
        for clip in track.clips.iter().filter(|c| {
            c.timeline_out > range_start
                && c.timeline_in < range_end
                && c.volume > 0.001
                && (!require_files_exist || c.source_path.exists())
        }) {
            let in_idx = input_paths.len();
            input_paths.push(clip.source_path.clone());

            let eff_tl_in = clip.timeline_in.max(range_start);
            let eff_tl_out = clip.timeline_out.min(range_end);
            let src_in = clip.timeline_to_source_time(eff_tl_in);
            let src_out = clip.timeline_to_source_time(eff_tl_out).max(src_in + 0.05);
            let eff_vol = (clip.volume * track.volume).clamp(0.0, 2.0);
            let atempo = build_atempo_filter_chain(clip.speed);
            let delay_ms = ((eff_tl_in - range_start).max(0.0) * 1000.0).round() as u64;
            let aud_lbl = format!("[aud_{extra_audio_idx}]");
            extra_audio_idx += 1;

            if delay_ms > 0 {
                audio_filter_parts.push(format!(
                    "[{in_idx}:a]atrim=start={src_in:.4}:end={src_out:.4},asetpts=PTS-STARTPTS,{atempo},volume={eff_vol:.4},adelay={delay_ms}|{delay_ms}{aud_lbl}"
                ));
            } else {
                audio_filter_parts.push(format!(
                    "[{in_idx}:a]atrim=start={src_in:.4}:end={src_out:.4},asetpts=PTS-STARTPTS,{atempo},volume={eff_vol:.4}{aud_lbl}"
                ));
            }
            extra_audio_labels.push(aud_lbl);
        }
    }

    let has_audio_output = v1_a_label.is_some() || !extra_audio_labels.is_empty();
    if let Some(base_a) = v1_a_label {
        if extra_audio_labels.is_empty() {
            audio_filter_parts.push(format!("{base_a}anull[outa]"));
        } else {
            let mut amix_inputs = base_a;
            for lbl in &extra_audio_labels {
                amix_inputs.push_str(lbl);
            }
            let total_inputs = 1 + extra_audio_labels.len();
            audio_filter_parts.push(format!(
                "{amix_inputs}amix=inputs={total_inputs}:duration=first:dropout_transition=0:normalize=0[outa]"
            ));
        }
    } else if !extra_audio_labels.is_empty() {
        if extra_audio_labels.len() == 1 {
            audio_filter_parts.push(format!("{}anull[outa]", extra_audio_labels[0]));
        } else {
            let mut amix_inputs = String::new();
            for lbl in &extra_audio_labels {
                amix_inputs.push_str(lbl);
            }
            let total_inputs = extra_audio_labels.len();
            audio_filter_parts.push(format!(
                "{amix_inputs}amix=inputs={total_inputs}:duration=first:dropout_transition=0:normalize=0[outa]"
            ));
        }
    }

    filter_parts_with_subs.extend(audio_filter_parts.clone());
    filter_parts_base.extend(audio_filter_parts);

    let mut codec_args = vec![
        "-map".to_owned(),
        "[outv]".to_owned(),
    ];
    if has_audio_output {
        codec_args.extend(["-map".to_owned(), "[outa]".to_owned()]);
    }
    codec_args.extend([
        "-c:v".to_owned(),
        "libx264".to_owned(),
        "-preset".to_owned(),
        "fast".to_owned(),
        "-crf".to_owned(),
        "20".to_owned(),
    ]);
    if has_audio_output {
        codec_args.extend([
            "-c:a".to_owned(),
            "aac".to_owned(),
            "-b:a".to_owned(),
            "192k".to_owned(),
        ]);
    }

    Ok(DesktopExportPlan {
        range_start,
        range_end,
        input_paths,
        input_flags,
        filter_complex: filter_parts_with_subs.join(";"),
        filter_complex_without_subs: filter_parts_base.join(";"),
        has_audio_output,
        codec_args,
        srt_content,
    })
}

/// Exports the timeline to `output_path` via FFmpeg, streaming real-time progress callbacks `on_progress(progress, status)`.
pub(crate) fn run_ffmpeg_export_with_progress<F>(
    project: &Project,
    output_path: &Path,
    mark_in: Option<f64>,
    mark_out: Option<f64>,
    mut on_progress: F,
) -> Result<String, String>
where
    F: FnMut(f32, &str) + Send + 'static,
{
    let srt_dir = env::temp_dir();
    let srt_file_name = format!("gfcut_subs_{}.srt", Uuid::new_v4().simple());
    let temp_srt_path = srt_dir.join(&srt_file_name);
    let plan = build_ffmpeg_export_plan(
        project,
        mark_in,
        mark_out,
        Some(Path::new(&srt_file_name)),
        true,
    )?;

    let total_duration = (plan.range_end - plan.range_start).max(0.1);
    let has_subs = plan.srt_content.is_some();
    if let Some(srt_text) = &plan.srt_content {
        fs::write(&temp_srt_path, srt_text)
            .map_err(|e| format!("No se pudo escribir el archivo de subtítulos temporal: {e}"))?;
    }

    let abs_inputs: Vec<PathBuf> = plan
        .input_paths
        .iter()
        .map(|p| absolute_path_for_ffmpeg(p))
        .collect();
    let abs_output = absolute_path_for_ffmpeg(output_path);
    if let Some(parent) = abs_output.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let ffmpeg = resolve_media_binary("ffmpeg");
    let mut current_codec_args = plan.codec_args.clone();

    // Check if libx264 is supported or if we should use libopenh264
    let probe_encoder = || -> &'static str {
        let mut check_cmd = Command::new(&ffmpeg);
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            check_cmd.creation_flags(0x0800_0000);
        }
        if let Ok(output) = check_cmd.arg("-encoders").output() {
            let s = String::from_utf8_lossy(&output.stdout);
            if s.contains("libx264") {
                return "libx264";
            }
            if s.contains("libopenh264") {
                return "libopenh264";
            }
        }
        "libopenh264"
    };

    let best_encoder = probe_encoder();
    if best_encoder == "libopenh264" {
        let mut alt_args = Vec::new();
        let mut skip_next = false;
        for arg in &plan.codec_args {
            if skip_next {
                skip_next = false;
                continue;
            }
            if arg == "-c:v" {
                alt_args.push("-c:v".to_owned());
                alt_args.push("libopenh264".to_owned());
                skip_next = true;
            } else if arg == "-preset" || arg == "-crf" {
                skip_next = true;
            } else {
                alt_args.push(arg.clone());
            }
        }
        alt_args.extend(["-b:v".to_owned(), "8M".to_owned()]);
        current_codec_args = alt_args;
    }

    let mut run_child = |filter_graph: &str, codec_args: &[String]| -> Result<(bool, String), String> {
        let mut cmd = Command::new(&ffmpeg);
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        cmd.current_dir(&srt_dir);
        cmd.args(["-y", "-hide_banner", "-loglevel", "error", "-nostats"]);
        for (idx, path) in abs_inputs.iter().enumerate() {
            if let Some(flags) = plan.input_flags.get(idx) {
                cmd.args(flags);
            }
            cmd.arg("-i").arg(path);
        }
        cmd.arg("-filter_complex")
            .arg(filter_graph)
            .args(codec_args)
            .args(["-progress", "pipe:1"])
            .args(["-movflags", "+faststart"])
            .arg(&abs_output);

        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let mut child = cmd.spawn().map_err(|e| format!("Fallo al iniciar FFmpeg: {e}"))?;

        let stderr = child.stderr.take().expect("stderr piped");
        let stderr_thread = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let mut r = std::io::BufReader::new(stderr);
            let _ = std::io::Read::read_to_end(&mut r, &mut buf);
            buf
        });

        if let Some(stdout) = child.stdout.take() {
            use std::io::BufRead;
            let reader = std::io::BufReader::new(stdout);
            for l in reader.lines().map_while(Result::ok) {
                if let Some(us_str) = l.strip_prefix("out_time_us=") {
                    if let Ok(us) = us_str.trim().parse::<u64>() {
                        let cur_secs = us as f64 / 1_000_000.0;
                        let p = (cur_secs / total_duration).clamp(0.0, 0.99) as f32;
                        let status = format!(
                            "Procesando capas V1, V2 y mezcla de audio con FFmpeg... {:.0}% ({})",
                            p * 100.0,
                            format_timecode(cur_secs)
                        );
                        on_progress(p, &status);
                    }
                } else if let Some(prog_str) = l.strip_prefix("progress=")
                    && prog_str.trim() == "end" {
                        on_progress(1.0, "Empaquetando contenedor MP4...");
                    }
            }
        }

        let status = child.wait().map_err(|e| format!("Error esperando proceso FFmpeg: {e}"))?;
        let err_bytes = stderr_thread.join().unwrap_or_default();
        let err_msg = String::from_utf8_lossy(&err_bytes).into_owned();

        Ok((status.success(), err_msg))
    };

    let (mut success, mut err_msg) = run_child(&plan.filter_complex, &current_codec_args)?;
    let mut subs_burned = has_subs && success;

    if !success && has_subs {
        let (fallback_success, fallback_err) = run_child(&plan.filter_complex_without_subs, &current_codec_args)?;
        if fallback_success {
            success = true;
            subs_burned = false;
        } else {
            err_msg = fallback_err;
        }
    }

    if temp_srt_path.exists() {
        let _ = fs::remove_file(&temp_srt_path);
    }

    if !success {
        let last_lines = ffmpeg_stderr_last_lines(err_msg.as_bytes(), 5);
        return Err(format!("FFmpeg falló al exportar:\n{last_lines}"));
    }

    on_progress(1.0, "¡Exportación completada con éxito!");

    let audio_tag = if plan.has_audio_output {
        " + Audio AAC 192k"
    } else {
        ""
    };
    let subs_tag = if subs_burned {
        " + Subtítulos V3"
    } else {
        ""
    };

    Ok(format!(
        "Exportado {} ({}–{}{audio_tag}{subs_tag})",
        abs_output.display(),
        format_timecode(plan.range_start),
        format_timecode(plan.range_end)
    ))
}

/// Exports the timeline to `output_path` via FFmpeg, strictly respecting `mark_in` and `mark_out`,
/// mixing multi-track audio (`amix=inputs=N:duration=longest` with `atempo` & `volume`),
/// burning in `V3` subtitles via `.srt`, and encoding with H.264 + AAC (`-c:v libx264 -preset fast -crf 20 -c:a aac -b:a 192k`).
#[allow(dead_code)]
pub(crate) fn run_ffmpeg_export(
    project: &Project,
    output_path: &Path,
    mark_in: Option<f64>,
    mark_out: Option<f64>,
) -> Result<String, String> {
    run_ffmpeg_export_with_progress(project, output_path, mark_in, mark_out, |_, _| {})
}

/// Deterministically selects the active clip on a single track at timestamp `t`.
/// Prefers half-open intervals `[timeline_in, timeline_out)` and, if multiple clips overlap on
/// the same track, selects the most recent clip (highest `timeline_in`).
pub(crate) fn select_clip_on_track_at_time(clips: &[Clip], t: f64) -> Option<&Clip> {
    clips
        .iter()
        .filter(|c| t >= c.timeline_in && t < c.timeline_out)
        .max_by(|a, b| a.timeline_in.total_cmp(&b.timeline_in))
        .or_else(|| {
            clips
                .iter()
                .filter(|c| t >= c.timeline_in && t <= c.timeline_out)
                .max_by(|a, b| a.timeline_in.total_cmp(&b.timeline_in))
        })
}

/// Finds the upcoming video clip on the timeline starting at or after timestamp `t`.
/// Searches all visible, unmuted video tracks and returns the clip whose `timeline_in >= t - 0.001`
/// that begins earliest.
pub(crate) fn find_next_video_clip_after_time(tracks: &[Track], t: f64) -> Option<(&Clip, f64)> {
    let mut candidates = Vec::new();
    for track in tracks {
        if track.kind != TrackKind::Video || track.muted || !track.visible {
            continue;
        }
        for clip in &track.clips {
            if clip.timeline_in >= t - 0.001 {
                candidates.push(clip);
            }
        }
    }
    candidates.sort_by(|a, b| a.timeline_in.total_cmp(&b.timeline_in));
    candidates.first().map(|clip| {
        let src_time = clip.in_point;
        (*clip, src_time)
    })
}

pub(crate) fn resolve_project_path() -> PathBuf {
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--project" || arg == "-p" {
            if let Some(path) = args.next() {
                return PathBuf::from(path);
            }
        } else if let Some(path) = arg.strip_prefix("--project=") {
            return PathBuf::from(path);
        }
    }

    if let Ok(env_path) = env::var("GFCUT_PROJECT_PATH").or_else(|_| env::var("OPENCUT_PROJECT_PATH"))
        && !env_path.is_empty() {
            return PathBuf::from(env_path);
        }

    if Path::new("demo.gfcut.json").exists() {
        PathBuf::from("demo.gfcut.json")
    } else {
        PathBuf::from("project.gfcut.json")
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, path::Path};

    use gfcut_timeline::{Project, Resolution};

    use super::{
        MAX_UNDO_HISTORY, PreviewAspectRatio, TRACK_NAME_A1_VOICE, TRACK_NAME_V1_MAIN,
        TRACK_NAME_V3_SUBS, apply_redo, apply_undo, build_ffmpeg_export_plan,
        clean_subtitle_display_text, ensure_stratified_tracks, escape_ffmpeg_subtitles_path,
        find_next_video_clip_after_time, format_timecode,
        push_project_snapshot, select_clip_on_track_at_time,
    };

    #[test]
    fn formats_seconds_as_mm_ss_centiseconds() {
        assert_eq!(format_timecode(0.0), "00:00.00");
        assert_eq!(format_timecode(4.2), "00:04.20");
        assert_eq!(format_timecode(68.5), "01:08.50");
    }

    #[test]
    fn upgrades_default_project_to_six_stratified_tracks() {
        let mut project = Project::with_default_tracks("GFCut Test");
        let v1_orig_id = project.tracks[0].id;
        project
            .add_clip(v1_orig_id, "sample.mp4", 0.0, 0.0, 5.0)
            .unwrap();

        ensure_stratified_tracks(&mut project);

        assert_eq!(project.tracks.len(), 6);
        assert_eq!(project.tracks[0].name, TRACK_NAME_V3_SUBS);
        assert_eq!(project.tracks[2].name, TRACK_NAME_V1_MAIN);
        assert_eq!(project.tracks[2].id, v1_orig_id);
        assert_eq!(project.tracks[2].clips.len(), 1);
        assert_eq!(project.tracks[3].name, TRACK_NAME_A1_VOICE);
    }

    #[test]
    fn maps_aspect_ratios_to_expected_resolutions() {
        assert_eq!(
            PreviewAspectRatio::Vertical9x16.resolution(),
            Resolution {
                width: 1080,
                height: 1920
            }
        );
        assert_eq!(
            PreviewAspectRatio::Square1x1.resolution(),
            Resolution {
                width: 1080,
                height: 1080
            }
        );
    }

    #[test]
    fn cleans_subtitle_prefix_for_preview_display() {
        assert_eq!(
            clean_subtitle_display_text("SUB_01: \"Hola mundo GFCut\""),
            "Hola mundo GFCut"
        );
        assert_eq!(
            clean_subtitle_display_text("SUB_TTS: \"Locución generada con IA\""),
            "Locución generada con IA"
        );
    }

    #[test]
    fn selects_most_recent_clip_when_same_track_clips_overlap() {
        let mut project = Project::with_default_tracks("Overlap Test");
        let v1_id = project.tracks[0].id;
        let first_id = project
            .add_clip(v1_id, "clip_a.mp4", 0.0, 0.0, 10.0)
            .unwrap();
        let second_id = project
            .add_clip(v1_id, "clip_b.mp4", 2.028, 0.0, 10.0)
            .unwrap();

        let clips = &project.tracks[0].clips;
        assert_eq!(
            select_clip_on_track_at_time(clips, 1.0).map(|c| c.id),
            Some(first_id)
        );
        assert_eq!(
            select_clip_on_track_at_time(clips, 3.5).map(|c| c.id),
            Some(second_id)
        );
    }

    #[test]
    fn split_clip_and_undo_restores_intact_original_clip() {
        let mut project = Project::with_default_tracks("Undo Split Test");
        let v1_id = project.tracks[0].id;
        let orig_clip_id = project
            .add_clip(v1_id, "interview.mp4", 0.0, 0.0, 10.0)
            .unwrap();

        let mut undo_stack = VecDeque::new();
        let mut redo_stack = VecDeque::new();

        // Snapshot before splitting at 4.0s
        push_project_snapshot(&mut undo_stack, &mut redo_stack, &project);
        project.split_clip(orig_clip_id, 4.0).unwrap();
        assert_eq!(project.tracks[0].clips.len(), 2);

        // Undo must restore the exact single original clip (0.0..10.0)
        assert!(apply_undo(&mut project, &mut undo_stack, &mut redo_stack));
        assert_eq!(project.tracks[0].clips.len(), 1);
        let restored = &project.tracks[0].clips[0];
        assert_eq!(restored.id, orig_clip_id);
        assert!((restored.timeline_in - 0.0).abs() < 1e-6);
        assert!((restored.timeline_out - 10.0).abs() < 1e-6);
        assert!((restored.in_point - 0.0).abs() < 1e-6);
        assert!((restored.out_point - 10.0).abs() < 1e-6);

        // Redo re-applies the split
        assert!(apply_redo(&mut project, &mut undo_stack, &mut redo_stack));
        assert_eq!(project.tracks[0].clips.len(), 2);

        // Verify 50-state cap on undo_stack
        for _ in 0..65 {
            push_project_snapshot(&mut undo_stack, &mut redo_stack, &project);
        }
        assert_eq!(undo_stack.len(), MAX_UNDO_HISTORY);
    }

    #[test]
    fn builds_multitrack_ffmpeg_export_with_amix_subtitles_and_h264_aac() {
        let mut project = Project::with_default_tracks("Export Test");
        ensure_stratified_tracks(&mut project);

        let v3_id = project.tracks[0].id;
        let v1_id = project.tracks[2].id;
        let a1_id = project.tracks[3].id;

        let v_clip_id = project
            .add_clip(v1_id, "scene.mp4", 0.0, 0.0, 10.0)
            .unwrap();
        project.track_mut(v1_id).unwrap().clips[0].speed = 1.5;
        let _ = v_clip_id;

        let a_clip_id = project
            .add_clip(a1_id, "voiceover.wav", 1.0, 0.0, 4.0)
            .unwrap();
        project.track_mut(a1_id).unwrap().clips[0].volume = 0.8;
        let _ = a_clip_id;

        let sub_id = project
            .add_clip(v3_id, "scene.mp4", 0.5, 0.0, 3.0)
            .unwrap();
        project.track_mut(v3_id).unwrap().clips[0].name =
            "SUB_01: \"Subtítulo quemado en MP4\"".to_owned();
        let _ = sub_id;

        let plan = build_ffmpeg_export_plan(
            &project,
            Some(0.0),
            Some(8.0),
            Some(Path::new("C:/temp/test_subs.srt")),
            false,
        )
        .expect("export plan should build");

        assert!(plan.has_audio_output);
        assert!(plan.filter_complex.contains("amix=inputs=2:duration=first:dropout_transition=0:normalize=0[outa]"));
        assert!(plan.filter_complex.contains("atempo=1.5000"));
        assert!(plan.filter_complex.contains("volume=0.8000"));
        assert!(plan.filter_complex.contains("subtitles='C\\:/temp/test_subs.srt'"));
        assert!(
            plan.srt_content
                .as_deref()
                .unwrap_or_default()
                .contains("Subtítulo quemado en MP4")
        );
        assert_eq!(
            plan.codec_args,
            vec![
                "-map",
                "[outv]",
                "-map",
                "[outa]",
                "-c:v",
                "libx264",
                "-preset",
                "fast",
                "-crf",
                "20",
                "-c:a",
                "aac",
                "-b:a",
                "192k"
            ]
        );
    }

    #[test]
    fn export_without_v3_subtitles_omits_subtitles_filter_and_keeps_paths_out_of_graph() {
        let mut project = Project::with_default_tracks("Export No Subs");
        ensure_stratified_tracks(&mut project);
        let v1_id = project.tracks[2].id;
        let a1_id = project.tracks[3].id;
        project
            .add_clip(v1_id, r"C:\Users\Ferna\Videos\the mison\1.mp4", 0.0, 0.0, 6.0)
            .unwrap();
        project
            .add_clip(a1_id, r"C:\Users\Ferna\Music\my song.wav", 0.0, 0.0, 6.0)
            .unwrap();

        let plan = build_ffmpeg_export_plan(
            &project,
            None,
            None,
            Some(Path::new("gfcut_subs_x.srt")),
            false,
        )
        .expect("export plan should build");

        assert!(plan.srt_content.is_none());
        assert!(!plan.filter_complex.contains("subtitles"));
        assert_eq!(plan.filter_complex, plan.filter_complex_without_subs);
        assert!(plan.filter_complex.contains("concat=n=1:v=1:a=1[v1_concat][a1_concat]"));
        assert!(plan.filter_complex.contains("amix=inputs=2:duration=first:dropout_transition=0:normalize=0[outa]"));
        assert!(!plan.filter_complex.contains("the mison"));
        assert!(!plan.filter_complex.contains(r"C:\"));
    }

    #[test]
    fn builds_multitrack_ffmpeg_export_with_v1_v2_overlay_and_subtitles() {
        let mut project = Project::with_default_tracks("Overlay Export Test");
        ensure_stratified_tracks(&mut project);
        project.resolution = gfcut_timeline::Resolution {
            width: 2560,
            height: 1070,
        };

        let v3_id = project.tracks[0].id;
        let v2_id = project.tracks[1].id;
        let v1_id = project.tracks[2].id;

        // V1 Base Clip (1080p source inside 2K project)
        project
            .add_clip(v1_id, "background_1080p.mp4", 0.0, 0.0, 10.0)
            .unwrap();

        // V2 B-Roll Overlay Clip
        project
            .add_clip(v2_id, "broll_overlay.mp4", 2.0, 0.0, 5.0)
            .unwrap();

        // V3 Subtitle
        project
            .add_clip(v3_id, "sub.wav", 1.0, 0.0, 4.0)
            .unwrap();
        project.track_mut(v3_id).unwrap().clips[0].name = "SUB_01: \"Layered Subtitle\"".to_owned();

        let plan = build_ffmpeg_export_plan(
            &project,
            Some(0.0),
            Some(10.0),
            Some(Path::new("C:/temp/overlay_subs.srt")),
            false,
        )
        .expect("overlay export plan should build");

        // Verify universal normalization for 2K project
        assert!(plan.filter_complex.contains("scale=2560:1070:force_original_aspect_ratio=decrease,pad=2560:1070:(ow-iw)/2:(oh-ih)/2:color=black,setsar=1"));
        // Verify V1 and V2 concatenation and overlay
        assert!(plan.filter_complex.contains("[v1_concat]"));
        // Verify strict layer composition: overlay V2 over V1 with timestamp enable
        assert!(plan.filter_complex.contains("[v1_concat][v2_0]overlay=x=(W-w)/2:y=(H-h)/2:enable='between(t,2.0000,7.0000)':eof_action=pass[v_composed]"));
        // Verify V3 subtitles burn-in on top of [v_composed]
        assert!(plan.filter_complex.contains("[v_composed]subtitles='C\\:/temp/overlay_subs.srt'[outv]"));
    }

    #[test]
    fn escapes_windows_subtitle_paths_with_spaces_for_filtergraph() {
        assert_eq!(
            escape_ffmpeg_subtitles_path(Path::new(r"C:\Users\Ferna\Videos\the mison\s.srt")),
            "C\\:/Users/Ferna/Videos/the mison/s.srt"
        );
    }

    #[test]
    fn detects_next_video_clip_after_playhead_accurately() {
        let mut project = Project::with_default_tracks("Next Clip Test");
        let v1_id = project.tracks[0].id;
        let _c1 = project.add_clip(v1_id, "clip1.mp4", 0.0, 0.0, 5.0).unwrap();
        let c2 = project.add_clip(v1_id, "clip2.mp4", 5.0, 0.0, 5.0).unwrap();

        // While playhead is at 4.6s (within clip1), next clip should be clip2 starting at 5.0s
        let next = find_next_video_clip_after_time(&project.tracks, 4.6).expect("should find upcoming clip");
        assert_eq!(next.0.id, c2);
        assert!((next.0.timeline_in - 5.0).abs() < 1e-6);

        // While playhead is at 5.5s (within clip2), no further clips exist
        assert!(find_next_video_clip_after_time(&project.tracks, 5.5).is_none());
    }

    #[test]
    fn activity_log_pushes_entries_with_timestamps_and_caps() {
        let mut logs = VecDeque::new();
        for i in 0..110 {
            if logs.len() >= 100 {
                logs.pop_back();
            }
            logs.push_front(super::LogEntry {
                timestamp: format!("12:00:{i:02}"),
                level: super::LogLevel::Info,
                title: format!("Event {i}"),
                details: Some("details".to_owned()),
            });
        }
        assert_eq!(logs.len(), 100);
        assert_eq!(logs.front().unwrap().title, "Event 109");
        assert_eq!(logs.back().unwrap().title, "Event 10");
        assert_eq!(logs.front().unwrap().level.label(), "INFO");
    }

    #[test]
    fn builds_multitrack_ffmpeg_export_with_gaps_in_v1_inserting_black_and_silence_generators() {
        let mut project = Project::with_default_tracks("Gaps Test");
        ensure_stratified_tracks(&mut project);
        let v1_id = project.tracks[2].id;

        // Clip 1 starts at 2.0s and ends at 5.0s (leading gap: 0.0 .. 2.0s)
        project
            .add_clip(v1_id, "first_clip.mp4", 2.0, 0.0, 3.0)
            .unwrap();

        // Clip 2 starts at 8.0s and ends at 11.0s (inter-clip gap: 5.0 .. 8.0s)
        project
            .add_clip(v1_id, "second_clip.mp4", 8.0, 0.0, 3.0)
            .unwrap();

        // Range exported: 0.0 to 14.0s (trailing gap: 11.0 .. 14.0s)
        let plan = build_ffmpeg_export_plan(
            &project,
            Some(0.0),
            Some(14.0),
            None,
            false,
        )
        .expect("export plan with gaps should build");

        // There should be 5 segments in total:
        // seg 0: black+silence (2.0s)
        // seg 1: first clip (3.0s)
        // seg 2: black+silence (3.0s)
        // seg 3: second clip (3.0s)
        // seg 4: black+silence (3.0s)
        assert!(plan.filter_complex.contains("concat=n=5:v=1:a=1[v1_concat][a1_concat]"));
        assert!(plan.filter_complex.contains("color=c=black:s=1920x1080:r=30/1:d=2.0000,format=yuv420p[v1_v_0]"));
        assert!(plan.filter_complex.contains("anullsrc=r=48000:cl=stereo,atrim=0:2.0000,asetpts=PTS-STARTPTS[v1_a_0]"));
        assert!(plan.filter_complex.contains("color=c=black:s=1920x1080:r=30/1:d=3.0000,format=yuv420p[v1_v_2]"));
        assert!(plan.filter_complex.contains("anullsrc=r=48000:cl=stereo,atrim=0:3.0000,asetpts=PTS-STARTPTS[v1_a_2]"));
        assert!(plan.filter_complex.contains("color=c=black:s=1920x1080:r=30/1:d=3.0000,format=yuv420p[v1_v_4]"));
        assert!(plan.filter_complex.contains("anullsrc=r=48000:cl=stereo,atrim=0:3.0000,asetpts=PTS-STARTPTS[v1_a_4]"));
    }

    #[test]
    fn speed_2x_clip_uses_coordinated_setpts_and_atempo_matching_timeline_duration() {
        let mut project = Project::with_default_tracks("Speed 2x Test");
        ensure_stratified_tracks(&mut project);
        let v1_id = project.tracks[2].id;

        // 147.22s source played at 2.0x → 73.61s (01:13.61) on the timeline
        let clip_id = project
            .add_clip(v1_id, "the msion prueba.mp4", 0.0, 0.0, 147.22)
            .unwrap();
        {
            let clip = project
                .track_mut(v1_id)
                .unwrap()
                .clips
                .iter_mut()
                .find(|c| c.id == clip_id)
                .unwrap();
            clip.speed = 2.0;
            clip.timeline_out = clip.timeline_in + clip.source_duration() / 2.0;
        }
        assert!((project.duration() - 73.61).abs() < 1e-6);
        project.upsert_asset(gfcut_timeline::MediaAsset {
            id: uuid::Uuid::new_v4(),
            path: "the msion prueba.mp4".into(),
            duration_secs: 147.22,
            format_name: "mp4".into(),
            size_bytes: None,
            video_streams: Vec::new(),
            audio_streams: vec![gfcut_timeline::AudioStreamInfo {
                index: 1,
                codec: "aac".into(),
                sample_rate: 48_000,
                channels: 2,
                channel_layout: None,
                bit_rate: None,
            }],
            transcript: None,
        });

        let plan = build_ffmpeg_export_plan(&project, None, None, None, false)
            .expect("plan should build");

        assert!((plan.range_end - plan.range_start - 73.61).abs() < 1e-6);
        assert!(plan.filter_complex.contains("setpts=0.5000*(PTS-STARTPTS)"));
        assert!(plan.filter_complex.contains("atempo=2.0000"));
        assert!(plan.filter_complex.contains("trim=start=0.0000:end=147.2200"));
        assert_eq!(plan.input_flags[0], vec!["-ss", "0.0000", "-t", "147.2200"]);
    }

    #[test]
    fn test_v1_clip_export_plan_preserves_full_source_trim_out_in_input_flags() {
        let mut project = Project::with_default_tracks("Full Trim Out Test");
        ensure_stratified_tracks(&mut project);
        let v1_id = project.tracks[2].id;

        // Clip of 73.0s (1m 13s) dropped at 0.0s
        project
            .add_clip(v1_id, "the msion prueba.mp4", 0.0, 0.0, 73.0)
            .unwrap();

        let plan = build_ffmpeg_export_plan(
            &project,
            None,
            None,
            None,
            false,
        )
        .expect("export plan should build");

        assert_eq!(plan.input_flags.len(), 1);
        let flags = &plan.input_flags[0];
        assert_eq!(flags, &vec!["-ss", "0.0000", "-t", "73.0000"]);
    }

    #[test]
    fn test_instantiate_v1_clip_from_asset_assigns_full_duration_without_truncation() {
        let mut project = Project::with_default_tracks("Asset Instantiation Test");
        ensure_stratified_tracks(&mut project);
        let v1_id = project.tracks[2].id;

        let asset_duration = 73.0; // 1m 13s
        let drop_position = 5.0;

        let clip_id = project
            .add_clip(v1_id, "the msion prueba.mp4", drop_position, 0.0, asset_duration)
            .expect("clip should be created");

        let (_, clip) = project.find_clip(clip_id).expect("clip must exist");
        assert_eq!(clip.in_point, 0.0);
        assert_eq!(clip.out_point, asset_duration);
        assert_eq!(clip.timeline_in, drop_position);
        assert_eq!(clip.timeline_out, drop_position + asset_duration);
        assert_eq!(clip.source_duration(), asset_duration);
        assert_eq!(clip.timeline_duration(), asset_duration);
    }

    #[test]
    fn test_linked_v1_a1_and_overlapping_ai_voice_mix_linearly_without_duplication() {
        let mut project = Project::with_default_tracks("Linked Audio & AI Voice Mix Test");
        ensure_stratified_tracks(&mut project);
        let v1_id = project.tracks[2].id;
        let a1_id = project.tracks[3].id;

        // V1 has main video (73s)
        project
            .add_clip(v1_id, "the msion prueba.mp4", 0.0, 0.0, 73.0)
            .unwrap();
        // A1 has linked audio from the same video at ducked volume 0.3
        let linked_a1_id = project
            .add_clip(a1_id, "the msion prueba.mp4", 0.0, 0.0, 73.0)
            .unwrap();
        project
            .track_mut(a1_id)
            .unwrap()
            .clips
            .iter_mut()
            .find(|c| c.id == linked_a1_id)
            .unwrap()
            .volume = 0.3;

        // A1 also has an overlapping AI voiceover clip at 0..7.6s with volume 1.0
        project
            .add_clip(a1_id, "voz_alvaro.wav", 0.0, 0.0, 7.6)
            .unwrap();

        let plan = build_ffmpeg_export_plan(&project, None, None, None, false)
            .expect("export plan should build");

        // V1 internal audio is silenced (anullsrc) because A1 holds the linked audio at volume=0.3000
        assert!(plan.filter_complex.contains("anullsrc=r=48000:cl=stereo,atrim=0:73.0000,asetpts=PTS-STARTPTS[v1_a_0]"));
        // Both A1 clips (volume=0.3000 and volume=1.0000) are included and linearly mixed with normalize=0
        assert!(plan.filter_complex.contains("volume=0.3000[aud_0]"));
        assert!(plan.filter_complex.contains("volume=1.0000[aud_1]"));
        assert!(plan.filter_complex.contains("amix=inputs=3:duration=first:dropout_transition=0:normalize=0[outa]"));
    }
}
