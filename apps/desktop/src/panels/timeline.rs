use std::sync::Arc;

use gpui::{
    AnyElement, AppContext, ClickEvent, Context, CursorStyle, DragMoveEvent, Entity, FontWeight, MouseButton,
    MouseDownEvent, ObjectFit, Pixels, RenderImage, ScrollDelta, ScrollWheelEvent, SharedString, StyledImage,
    Window, black, div, hsla, img, prelude::*, px, relative,
};
use gfcut_timeline::{Clip, Track, TrackKind};
use uuid::Uuid;

use crate::{
    components::{
        Badge, BadgeVariant, Button, ButtonSize, ButtonVariant, Label, Separator,
        render_scrollbar_rail,
    },
    project_state::{
        ProjectState, TRACK_NAME_A1_VOICE, TRACK_NAME_A2_BGM, TRACK_NAME_A3_SFX,
        TRACK_NAME_V1_MAIN, TRACK_NAME_V2_BROLL, TRACK_NAME_V3_SUBS, format_timecode,
    },
    theme::{ActiveTheme, SemanticColors},
};

const TRACK_HEADER_WIDTH_PX: f32 = 236.0;
const VU_METER_WIDTH_PX: f32 = 46.0;

#[derive(Clone, Copy, Debug)]
struct PlayheadScrubDrag;

#[derive(Clone, Copy, Debug)]
pub(crate) struct TimelineScrollDrag {
    pub(crate) lane_width: f32,
    pub(crate) max_scroll_x: f32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ClipDrag {
    pub(crate) clip_id: Uuid,
    pub(crate) initial_track_id: Uuid,
    pub(crate) initial_timeline_in: f64,
    pub(crate) _clip_duration: f64,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ActiveClipDragState {
    pub(crate) drag: ClipDrag,
    pub(crate) start_x: Pixels,
    pub(crate) start_y: Pixels,
    pub(crate) target_track_id: Uuid,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ClipTrimDrag {
    pub(crate) clip_id: Uuid,
    pub(crate) edge: crate::timeline_edit::TrimEdge,
    pub(crate) initial: crate::timeline_edit::TrimResult,
    pub(crate) source_duration: Option<f64>,
}

/// Converts a window-space X coordinate inside the timeline track/ruler area into a timeline timestamp,
/// taking into account horizontal scroll and `pixels_per_second`.
pub(crate) fn window_x_to_timeline_time_zoomed(
    mouse_x: Pixels,
    scroll_x: f32,
    px_per_sec: f32,
) -> f64 {
    let lane_left = TRACK_HEADER_WIDTH_PX;
    let x_in_lane = f32::from(mouse_x) - lane_left;
    let safe_pps = px_per_sec.max(1.0);
    (((x_in_lane + scroll_x) / safe_pps) as f64).max(0.0)
}

/// Computes pixel `(left_px, width_px)` for placing a clip inside the lane with zoom and scroll.
pub(crate) fn clip_layout_pixels(
    timeline_in: f64,
    timeline_out: f64,
    scroll_x: f32,
    px_per_sec: f32,
) -> (f32, f32) {
    let left_px = (timeline_in * px_per_sec as f64) as f32 - scroll_x;
    let width_px = ((timeline_out - timeline_in) * px_per_sec as f64).max(4.0) as f32;
    (left_px, width_px)
}

/// Computes the pixel X position inside the lane for drawing the vertical playhead needle.
pub(crate) fn playhead_pixel_x(playhead_time: f64, scroll_x: f32, px_per_sec: f32) -> f32 {
    (playhead_time * px_per_sec as f64) as f32 - scroll_x
}

/// Converts a window-space X coordinate inside the timeline track/ruler area into a timeline timestamp.
#[allow(dead_code)]
fn window_x_to_timeline_time(
    mouse_x: Pixels,
    window_width: Pixels,
    timeline_span: f64,
) -> f64 {
    let lane_left = px(TRACK_HEADER_WIDTH_PX);
    let right_margin = px(VU_METER_WIDTH_PX);
    let lane_width = (window_width - lane_left - right_margin).max(px(1.0));
    let fraction = ((mouse_x - lane_left) / lane_width).clamp(0.0, 1.0);
    (fraction as f64) * timeline_span.max(1.0)
}

/// Computes normalized `(left_fraction, width_fraction)` in `[0.0, 1.0]` for placing
/// a clip spanning `[timeline_in, timeline_out]` inside a timeline of length `total_span`.
#[allow(dead_code)]
pub(crate) fn clip_layout_fractions(
    timeline_in: f64,
    timeline_out: f64,
    total_span: f64,
) -> (f32, f32) {
    let safe_span = total_span.max(1.0);
    let start = timeline_in.clamp(0.0, safe_span);
    let end = timeline_out.clamp(start, safe_span);

    let left = (start / safe_span) as f32;
    let width = ((end - start) / safe_span).max(0.01) as f32;
    let clamped_width = width.min((1.0 - left).max(0.01));

    (left, clamped_width)
}

/// Computes normalized `playhead_fraction` in `[0.0, 1.0]` for drawing the vertical playhead line.
#[allow(dead_code)]
pub(crate) fn playhead_fraction(playhead_time: f64, total_span: f64) -> f32 {
    let safe_span = total_span.max(1.0);
    (playhead_time / safe_span).clamp(0.0, 1.0) as f32
}

/// Returns the short studio code (`V3`, `V2`, `V1`, `A1`, `A2`, `A3`) and badge hue for a track.
fn track_studio_badge_spec(track: &Track, row_idx: usize) -> (&'static str, f32) {
    if track.name == TRACK_NAME_V3_SUBS || track.kind == TrackKind::Subtitle {
        ("V3", 0.76) // Purple / Violet
    } else if track.name == TRACK_NAME_V2_BROLL {
        ("V2", 0.62) // Indigo / Blue
    } else if track.name == TRACK_NAME_V1_MAIN {
        ("V1", 0.56) // Cyan / Blue
    } else if track.name == TRACK_NAME_A1_VOICE {
        ("A1", 0.43) // Emerald / Teal
    } else if track.name == TRACK_NAME_A2_BGM {
        ("A2", 0.50) // Cyan-Slate
    } else if track.name == TRACK_NAME_A3_SFX {
        ("A3", 0.47) // Teal
    } else {
        match track.kind {
            TrackKind::Video => ("V1", 0.56),
            TrackKind::Audio => {
                if row_idx.is_multiple_of(2) {
                    ("A1", 0.43)
                } else {
                    ("A2", 0.50)
                }
            }
            TrackKind::Subtitle => ("V3", 0.76),
        }
    }
}

pub(crate) struct Timeline {
    project_state: Entity<ProjectState>,
    active_clip_drag: Option<ActiveClipDragState>,
    active_trim_drag: Option<(ClipTrimDrag, Pixels)>,
}

impl Timeline {
    pub(crate) fn new(project_state: Entity<ProjectState>, cx: &mut Context<Self>) -> Self {
        cx.observe(&project_state, |_, _, cx| {
            cx.notify();
        })
        .detach();

        Self {
            project_state,
            active_clip_drag: None,
            active_trim_drag: None,
        }
    }

    fn on_scrub_drag_move(
        &mut self,
        event: &DragMoveEvent<PlayheadScrubDrag>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pps = self.project_state.read(cx).pixels_per_second();
        let scroll_x = self.project_state.read(cx).timeline_scroll_x();
        let new_time = window_x_to_timeline_time_zoomed(event.event.position.x, scroll_x, pps);

        self.project_state.update(cx, |state, cx| {
            state.set_playhead_time(new_time, cx);
        });
    }

    fn on_scrollbar_drag_move(
        &mut self,
        event: &DragMoveEvent<TimelineScrollDrag>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let drag = *event.drag(cx);
        let click_in_bar = f32::from(event.event.position.x) - TRACK_HEADER_WIDTH_PX;
        let frac = (click_in_bar / drag.lane_width.max(1.0)).clamp(0.0, 1.0);
        let target_scroll = (frac * drag.max_scroll_x).clamp(0.0, drag.max_scroll_x);

        self.project_state.update(cx, |state, cx| {
            state.set_timeline_scroll_x(target_scroll, cx);
        });
    }

    fn on_clip_drag_move(
        &mut self,
        event: &DragMoveEvent<ClipDrag>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let drag = *event.drag(cx);
        let (start_x, start_y) = match &self.active_clip_drag {
            Some(active) if active.drag.clip_id == drag.clip_id => (active.start_x, active.start_y),
            _ => {
                let sx = event.event.position.x;
                let sy = event.event.position.y;
                self.active_clip_drag = Some(ActiveClipDragState {
                    drag,
                    start_x: sx,
                    start_y: sy,
                    target_track_id: drag.initial_track_id,
                });
                (sx, sy)
            }
        };

        let px_per_sec = self.project_state.read(cx).pixels_per_second();
        let delta_px = f32::from(event.event.position.x - start_x);
        let delta_secs = (delta_px / px_per_sec.max(0.001)) as f64;
        let mut target_in = (drag.initial_timeline_in + delta_secs).max(0.0);

        if !event.event.modifiers.shift {
            let snap_points = self.project_state.read(cx).snap_points(Some(drag.clip_id));
            let (snapped_in, _) = crate::timeline_edit::snap_clip_move(
                target_in,
                drag._clip_duration,
                crate::timeline_edit::SNAP_TOLERANCE_PX,
                px_per_sec,
                &snap_points,
            );
            target_in = snapped_in;
        }

        // Evaluate vertical cursor movement across compatible track rows
        let project = self.project_state.read(cx).project().clone();
        let mut target_track_id = drag.initial_track_id;

        if let Ok((src_track_id, _)) = project.find_clip(drag.clip_id) {
            let tracks = &project.tracks;
            if let Some(src_row) = tracks.iter().position(|t| t.id == src_track_id) {
                let src_kind = tracks[src_row].kind;
                let delta_y = f32::from(event.event.position.y - start_y);
                let row_step = 40.0;
                let delta_row = (delta_y / row_step).round() as i32;
                let candidate_row = ((src_row as i32) + delta_row).clamp(0, (tracks.len().saturating_sub(1)) as i32) as usize;
                let candidate_track = &tracks[candidate_row];
                if candidate_track.kind == src_kind && !candidate_track.locked {
                    target_track_id = candidate_track.id;
                }
            }
        }

        if let Some(active) = &mut self.active_clip_drag {
            active.target_track_id = target_track_id;
        }

        self.project_state.update(cx, |state, cx| {
            state.preview_move_clip_cross_track(drag.clip_id, target_track_id, target_in, cx);
        });
    }

    fn on_clip_drop(
        &mut self,
        drag: &ClipDrag,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (start_x, target_track_id) = match self.active_clip_drag.take() {
            Some(active) if active.drag.clip_id == drag.clip_id => (active.start_x, active.target_track_id),
            _ => (window.mouse_position().x, drag.initial_track_id),
        };

        let px_per_sec = self.project_state.read(cx).pixels_per_second();
        let delta_px = f32::from(window.mouse_position().x - start_x);
        let delta_secs = (delta_px / px_per_sec.max(0.001)) as f64;
        let mut final_in = (drag.initial_timeline_in + delta_secs).max(0.0);

        let snap_points = self.project_state.read(cx).snap_points(Some(drag.clip_id));
        let (snapped_in, _) = crate::timeline_edit::snap_clip_move(
            final_in,
            drag._clip_duration,
            crate::timeline_edit::SNAP_TOLERANCE_PX,
            px_per_sec,
            &snap_points,
        );
        final_in = snapped_in;

        let clip_id = drag.clip_id;
        let initial_in = drag.initial_timeline_in;
        let initial_track_id = drag.initial_track_id;

        self.project_state.update(cx, |state, cx| {
            state.commit_move_clip_cross_track(
                clip_id,
                initial_track_id,
                initial_in,
                target_track_id,
                final_in,
                cx,
            );
        });
    }

    fn on_trim_drag_move(
        &mut self,
        event: &DragMoveEvent<ClipTrimDrag>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let drag = *event.drag(cx);
        let start_x = match self.active_trim_drag {
            Some((active, sx)) if active.clip_id == drag.clip_id && active.edge == drag.edge => sx,
            _ => {
                let sx = event.event.position.x;
                self.active_trim_drag = Some((drag, sx));
                sx
            }
        };

        let px_per_sec = self.project_state.read(cx).pixels_per_second();
        let delta_px = f32::from(event.event.position.x - start_x);
        let delta_secs = (delta_px / px_per_sec.max(0.001)) as f64;

        let base_time = match drag.edge {
            crate::timeline_edit::TrimEdge::Left => drag.initial.timeline_in,
            crate::timeline_edit::TrimEdge::Right => drag.initial.timeline_out,
        };
        let mut target_time = (base_time + delta_secs).max(0.0);

        // Snap unless Shift is held
        if !event.event.modifiers.shift {
            let snap_points = self.project_state.read(cx).snap_points(Some(drag.clip_id));
            target_time = crate::timeline_edit::apply_timeline_snapping(
                target_time,
                crate::timeline_edit::SNAP_TOLERANCE_PX,
                px_per_sec,
                &snap_points,
            );
        }

        self.project_state.update(cx, |state, cx| {
            state.preview_trim_clip(drag.clip_id, drag.edge, target_time, cx);
        });
    }

    fn on_trim_drop(
        &mut self,
        drag: &ClipTrimDrag,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let start_x = match self.active_trim_drag.take() {
            Some((active, sx)) if active.clip_id == drag.clip_id && active.edge == drag.edge => sx,
            _ => window.mouse_position().x,
        };

        let px_per_sec = self.project_state.read(cx).pixels_per_second();
        let delta_px = f32::from(window.mouse_position().x - start_x);
        let delta_secs = (delta_px / px_per_sec.max(0.001)) as f64;

        let base_time = match drag.edge {
            crate::timeline_edit::TrimEdge::Left => drag.initial.timeline_in,
            crate::timeline_edit::TrimEdge::Right => drag.initial.timeline_out,
        };
        let mut target_time = (base_time + delta_secs).max(0.0);

        let snap_points = self.project_state.read(cx).snap_points(Some(drag.clip_id));
        target_time = crate::timeline_edit::apply_timeline_snapping(
            target_time,
            crate::timeline_edit::SNAP_TOLERANCE_PX,
            px_per_sec,
            &snap_points,
        );

        let final_result = {
            let state = self.project_state.read(cx);
            let clip = state.project().find_clip(drag.clip_id).ok().map(|(_, c)| c.clone());
            match clip {
                Some(c) => crate::timeline_edit::compute_trim(
                    &c,
                    drag.edge,
                    target_time,
                    drag.source_duration,
                ),
                None => return,
            }
        };

        self.project_state.update(cx, |state, cx| {
            state.commit_trim_clip(drag.clip_id, drag.initial, final_result, cx);
        });
    }
}

impl Render for Timeline {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = window.theme();
        let colors = theme.colors;
        let state = self.project_state.read(cx);
        let project = state.project();

        let timeline_span = state.timeline_span();
        let playhead_time = state.playhead_time();
        let selected_clip_id = state.selected_clip_id();
        let is_playing = state.is_playing();
        let ai_busy = state.ai_task_running();
        let active_trim_guide = state.active_trim_guide();

        if state.timeline_collapsed() {
            let state_expand = self.project_state.clone();
            let total_clips: usize = project.tracks.iter().map(|t| t.clips.len()).sum();
            return div()
                .id("gfcut-timeline-panel-collapsed")
                .flex()
                .flex_none()
                .items_center()
                .justify_between()
                .w_full()
                .h(px(32.0))
                .px(px(14.0))
                .bg(colors.card)
                .border_t_1()
                .border_color(colors.border)
                .text_color(colors.foreground)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .child(
                            div()
                                .text_size(px(11.5))
                                .font_weight(FontWeight::BOLD)
                                .text_color(colors.muted_foreground)
                                .child("🎞 Línea de Tiempo (Oculta)"),
                        )
                        .child(
                            Badge::new(format!(
                                "{} pistas • {} clips",
                                project.tracks.len(),
                                total_clips
                            ))
                            .variant(BadgeVariant::Outline),
                        )
                        .child(
                            Badge::new(format!("⏱ {}", format_timecode(playhead_time)))
                                .variant(BadgeVariant::Secondary),
                        ),
                )
                .child(
                    Button::new("timeline-expand-btn", "▴ Mostrar Línea de Tiempo (Ctrl+T)")
                        .variant(ButtonVariant::Secondary)
                        .size(ButtonSize::XSmall)
                        .on_click(move |_, _, cx| {
                            state_expand.update(cx, |s, cx| s.toggle_timeline(cx));
                        }),
                )
                .into_any_element();
        }

        let fps_val = project.fps.as_f64();
        let px_per_sec = state.pixels_per_second();
        let scroll_x = state.timeline_scroll_x();

        let lane_left = px(TRACK_HEADER_WIDTH_PX);
        let right_margin = px(VU_METER_WIDTH_PX);
        let lane_width = (window.viewport_size().width - lane_left - right_margin).max(px(1.0));
        let lane_width_f32 = f32::from(lane_width);
        let total_content_width = (timeline_span * px_per_sec as f64) as f32;
        let max_scroll_x = (total_content_width - lane_width_f32).max(0.0);
        let clamped_scroll_x = scroll_x.clamp(0.0, max_scroll_x);

        let ph_pixel_x = playhead_pixel_x(playhead_time, clamped_scroll_x, px_per_sec);

        let track_rows: Vec<AnyElement> = project
            .tracks
            .iter()
            .enumerate()
            .map(|(row_idx, track)| {
                render_track_row(
                    row_idx,
                    track,
                    timeline_span,
                    ph_pixel_x,
                    clamped_scroll_x,
                    px_per_sec,
                    lane_width_f32,
                    selected_clip_id,
                    state,
                    self.project_state.clone(),
                    window,
                )
            })
            .collect();

        let state_zoom_out = self.project_state.clone();
        let zoom_out_btn = Button::new("timeline-zoom-out-btn", "🔍 -")
            .variant(ButtonVariant::Ghost)
            .size(ButtonSize::XSmall)
            .on_click(move |_, _, cx| {
                state_zoom_out.update(cx, |s, cx| s.zoom_out_timeline(cx));
            });

        let state_zoom_in = self.project_state.clone();
        let zoom_in_btn = Button::new("timeline-zoom-in-btn", "🔍 +")
            .variant(ButtonVariant::Ghost)
            .size(ButtonSize::XSmall)
            .on_click(move |_, _, cx| {
                state_zoom_in.update(cx, |s, cx| s.zoom_in_timeline(cx));
            });

        let state_zoom_fit = self.project_state.clone();
        let zoom_fit_btn = Button::new("timeline-zoom-fit-btn", "⇲ Ajustar")
            .variant(ButtonVariant::Outline)
            .size(ButtonSize::XSmall)
            .on_click(move |_, _, cx| {
                state_zoom_fit.update(cx, |s, cx| s.zoom_fit_timeline(lane_width_f32, cx));
            });

        let has_any_clips = project.tracks.iter().any(|t| !t.clips.is_empty());

        // Left Editing Tools
        let is_cursor_active =
            state.active_tool() == crate::project_state::TimelineTool::Select;
        let _ = state;
        let state_cursor = self.project_state.clone();
        let cursor_btn = Button::new("timeline-tool-cursor-btn", "➤ Cursor (V)")
            .variant(if is_cursor_active {
                ButtonVariant::Default
            } else {
                ButtonVariant::Ghost
            })
            .size(ButtonSize::XSmall)
            .on_click(move |_, _, cx| {
                state_cursor.update(cx, |s, cx| {
                    s.set_active_tool(crate::project_state::TimelineTool::Select, cx);
                });
            });

        let state_split = self.project_state.clone();
        let split_btn = Button::new("timeline-split-btn", "✂ Cortar [S]")
            .variant(ButtonVariant::Secondary)
            .size(ButtonSize::XSmall)
            .on_click(move |_, _, cx| {
                state_split.update(cx, |s, cx| s.split_clip_at_playhead(cx));
            });

        let state_snap = self.project_state.clone();
        let snap_btn = Button::new("timeline-snap-zero-btn", "⏮ A 0s")
            .variant(ButtonVariant::Outline)
            .size(ButtonSize::XSmall)
            .on_click(move |_, _, cx| {
                state_snap.update(cx, |s, cx| s.set_playhead_time(0.0, cx));
            });

        let is_ripple = state.is_ripple_mode();
        let state_ripple = self.project_state.clone();
        let ripple_btn = Button::new(
            "timeline-ripple-toggle-btn",
            if is_ripple { "⇥ Ripple: ON" } else { "⇥ Ripple: OFF" },
        )
        .variant(if is_ripple {
            ButtonVariant::Default
        } else {
            ButtonVariant::Outline
        })
        .size(ButtonSize::XSmall)
        .on_click(move |_, _, cx| {
            state_ripple.update(cx, |s, cx| s.toggle_ripple_mode(cx));
        });

        let state_delete = self.project_state.clone();
        let delete_btn = Button::new("timeline-delete-btn", "🗑 Borrar")
            .variant(ButtonVariant::Destructive)
            .size(ButtonSize::XSmall)
            .disabled(!has_any_clips)
            .on_click(move |_, _, cx| {
                state_delete.update(cx, |s, cx| {
                    let ripple = s.is_ripple_mode();
                    s.delete_selected_clip(ripple, cx);
                });
            });

        // Right Integrated AI Action Buttons
        let state_ai_subs = self.project_state.clone();
        let ai_subs_btn = Button::new(
            "timeline-ai-subtitles-btn",
            if ai_busy {
                "✨ Procesando IA..."
            } else {
                "✨ Auto Subtítulos IA"
            },
        )
        .variant(ButtonVariant::Default)
        .size(ButtonSize::XSmall)
        .disabled(ai_busy)
        .on_click(move |_, _, cx| {
            state_ai_subs.update(cx, |s, cx| s.generate_ai_subtitles(cx));
        });

        let state_ai_silence = self.project_state.clone();
        let ai_silence_btn = Button::new("timeline-ai-silence-btn", "✂ Limpiar Silencios IA")
            .variant(ButtonVariant::Secondary)
            .size(ButtonSize::XSmall)
            .disabled(ai_busy)
            .on_click(move |_, _, cx| {
                state_ai_silence.update(cx, |s, cx| s.clean_silences_ai(cx));
            });

        let state_collapse = self.project_state.clone();
        let state_wheel = self.project_state.clone();

        div()
            .id("gfcut-timeline-panel")
            .flex()
            .flex_col()
            .flex_1()
            .size_full()
            .bg(colors.card)
            .text_color(colors.card_foreground)
            .overflow_hidden()
            .on_scroll_wheel(move |event: &ScrollWheelEvent, window, cx| {
                let ctrl_or_cmd = event.modifiers.control || event.modifiers.platform;
                let (dx, dy) = match event.delta {
                    ScrollDelta::Pixels(p) => (f32::from(p.x), f32::from(p.y)),
                    ScrollDelta::Lines(p) => (p.x * 20.0, p.y * 20.0),
                };
                if ctrl_or_cmd {
                    let zoom_factor: f32 = if dy > 0.0 || dx > 0.0 {
                        1.15
                    } else if dy < 0.0 || dx < 0.0 {
                        0.85
                    } else {
                        1.0
                    };
                    if (zoom_factor - 1.0).abs() > 0.01 {
                        let cursor_x = f32::from(window.mouse_position().x) - TRACK_HEADER_WIDTH_PX;
                        state_wheel.update(cx, |s, cx| {
                            let pps = s.pixels_per_second();
                            let cur_scroll = s.timeline_scroll_x();
                            let focal_time = if cursor_x >= 0.0 {
                                (((cursor_x + cur_scroll) / pps) as f64).max(0.0)
                            } else {
                                s.playhead_time()
                            };
                            s.zoom_timeline(zoom_factor, Some(focal_time), lane_width_f32, cx);
                        });
                    }
                } else {
                    let delta = if dx.abs() > dy.abs() {
                        -dx
                    } else {
                        -dy * 1.5
                    };
                    if delta.abs() > 0.01 {
                        state_wheel.update(cx, |s, cx| {
                            s.scroll_timeline_by(delta, lane_width_f32, cx);
                        });
                    }
                }
            })
            .on_drag_move::<ClipDrag>(cx.listener(Timeline::on_clip_drag_move))
            .on_drag_move::<TimelineScrollDrag>(cx.listener(Timeline::on_scrollbar_drag_move))
            .on_drop(cx.listener(Timeline::on_clip_drop))
            .on_drag_move::<ClipTrimDrag>(cx.listener(Timeline::on_trim_drag_move))
            .on_drop(cx.listener(Timeline::on_trim_drop))
            .child(
                // 1. GFCut Top AI & Editing Toolbar
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .h(px(36.0))
                    .px(px(10.0))
                    .border_b_1()
                    .border_color(colors.border)
                    .bg(colors.sidebar)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(5.0))
                            .child(cursor_btn)
                            .child(split_btn)
                            .child(snap_btn)
                            .child(ripple_btn)
                            .child(delete_btn),
                    )
                    .child(
                        // Center Digital Timecode + FPS Readout + Zoom Controls
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .px(px(8.0))
                            .py(px(3.0))
                            .rounded(px(5.0))
                            .bg(colors.background)
                            .border_1()
                            .border_color(colors.border)
                            .child(
                                div()
                                    .text_size(px(13.0))
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(colors.foreground)
                                    .child(format!("⏱ {}", format_timecode(playhead_time))),
                            )
                            .child(
                                div()
                                    .px(px(5.0))
                                    .py(px(1.0))
                                    .rounded(px(3.0))
                                    .bg(hsla(0.43, 0.78, 0.22, 0.90))
                                    .text_color(hsla(0.43, 0.90, 0.78, 1.0))
                                    .text_size(px(9.5))
                                    .font_weight(FontWeight::BOLD)
                                    .child(format!("{fps_val:.2} FPS")),
                            )
                            .child(Separator::vertical())
                            .child(zoom_out_btn)
                            .child(
                                div()
                                    .px(px(4.0))
                                    .py(px(1.0))
                                    .rounded(px(2.0))
                                    .bg(colors.muted.opacity(0.3))
                                    .text_size(px(10.0))
                                    .font_weight(FontWeight::BOLD)
                                    .child(format!("{px_per_sec:.0} px/s")),
                            )
                            .child(zoom_in_btn)
                            .child(zoom_fit_btn),
                    )
                    .child(
                        // Right AI Action Toolbar & Minimize Button
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .child(ai_silence_btn)
                            .child(ai_subs_btn)
                            .child(
                                Button::new("timeline-toolbar-collapse-btn", "▾ Minimizar")
                                    .variant(ButtonVariant::Ghost)
                                    .size(ButtonSize::XSmall)
                                    .on_click(move |_, _, cx| {
                                        state_collapse.update(cx, |s, cx| s.toggle_timeline(cx));
                                    }),
                            ),
                    ),
            )
            .child(
                // 2. Main Timeline Body: [Tracks + Ruler] on Left, [VU Master Meter] on Right
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_hidden()
                    .child(
                        div()
                            .relative()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .overflow_hidden()
                            .child(
                                // Time Ruler Row
                                div()
                                    .flex()
                                    .flex_none()
                                    .items_center()
                                    .h(px(24.0))
                                    .border_b_1()
                                    .border_color(colors.border)
                                    .bg(colors.background)
                                    .child(
                                        div()
                                            .flex()
                                            .flex_none()
                                            .items_center()
                                            .justify_between()
                                            .w(px(TRACK_HEADER_WIDTH_PX))
                                            .h_full()
                                            .px(px(10.0))
                                            .border_r_1()
                                            .border_color(colors.border)
                                            .bg(colors.sidebar)
                                            .child(Label::new("PISTAS (V3..V1 / A1..A3)").muted()),
                                    )
                                    .child(render_time_ruler(
                                        timeline_span,
                                        playhead_time,
                                        ph_pixel_x,
                                        clamped_scroll_x,
                                        px_per_sec,
                                        lane_width_f32,
                                        self.project_state.clone(),
                                        window,
                                        cx,
                                    )),
                            )
                            .child(
                                // Stratified 6-Track Rows with Visible Scrollbar
                                div()
                                    .flex()
                                    .flex_1()
                                    .min_h_0()
                                    .w_full()
                                    .child(
                                        div()
                                            .id("timeline-tracks-scrollable")
                                            .flex()
                                            .flex_col()
                                            .flex_1()
                                            .min_w_0()
                                            .h_full()
                                            .overflow_y_scroll()
                                            .children(track_rows),
                                    )
                                    .child(render_scrollbar_rail(&colors, 52.0)),
                            )
                            .child(render_horizontal_scrollbar(
                                clamped_scroll_x,
                                max_scroll_x,
                                lane_width_f32,
                                total_content_width,
                                self.project_state.clone(),
                                &colors,
                                cx,
                            ))
                            .child(render_timeline_visual_guides(
                                ph_pixel_x,
                                active_trim_guide,
                                clamped_scroll_x,
                                px_per_sec,
                                window,
                            )),
                    )
                    .child(render_vu_master_meter(is_playing, playhead_time, window)),
            )
            .into_any_element()
    }
}

/// Renders continuous visual guides across the timeline:
/// 1. A continuous vertical playhead line from the time ruler through track A3.
/// 2. An active trim guide line (dotted vertical line) during interactive clip edge trimming.
fn render_timeline_visual_guides(
    ph_pixel_x: f32,
    active_trim_guide: Option<(f64, crate::timeline_edit::TrimEdge)>,
    scroll_x: f32,
    px_per_sec: f32,
    window: &Window,
) -> AnyElement {
    let colors = window.theme().colors;

    div()
        .id("timeline-visual-guides-overlay")
        .absolute()
        .left(px(TRACK_HEADER_WIDTH_PX))
        .right(px(7.0))
        .top_0()
        .bottom_0()
        .overflow_hidden()
        // 1. Continuous Vertical Playhead Line across the entire timeline (Ruler to A3)
        .child(
            div()
                .absolute()
                .left(px(ph_pixel_x))
                .top_0()
                .bottom_0()
                .w(px(2.0))
                .bg(hsla(0.52, 0.95, 0.60, 0.85)),
        )
        // 2. Active Trim Guide during drag: Dotted Vertical Guide Line with Timecode Badge
        .when_some(active_trim_guide, |this, (trim_time, edge)| {
            let trim_x = (trim_time * px_per_sec as f64) as f32 - scroll_x;
            let edge_label = match edge {
                crate::timeline_edit::TrimEdge::Left => "◀| Recorte In",
                crate::timeline_edit::TrimEdge::Right => "|▶ Recorte Out",
            };
            this.child(
                div()
                    .absolute()
                    .left(px(trim_x))
                    .top_0()
                    .bottom_0()
                    .w(px(2.0))
                    .child(
                        // Top indicator badge
                        div()
                            .absolute()
                            .top(px(2.0))
                            .left(px(4.0))
                            .px(px(4.0))
                            .py(px(1.0))
                            .rounded(px(3.0))
                            .bg(hsla(0.12, 0.95, 0.45, 0.95))
                            .text_color(colors.foreground)
                            .text_size(px(9.0))
                            .font_weight(FontWeight::BOLD)
                            .whitespace_nowrap()
                            .child(format!("{edge_label}: {}", format_timecode(trim_time))),
                    )
                    .child(
                        // Vertical dotted/dashed line
                        div()
                            .flex()
                            .flex_col()
                            .justify_between()
                            .w(px(2.0))
                            .h_full()
                            .children((0..40).map(|_| {
                                div()
                                    .w(px(2.0))
                                    .h(px(5.0))
                                    .bg(hsla(0.12, 0.95, 0.65, 0.90))
                            })),
                    ),
            )
        })
        .into_any_element()
}

fn render_horizontal_scrollbar(
    scroll_x: f32,
    max_scroll_x: f32,
    lane_width: f32,
    total_content_width: f32,
    project_state: Entity<ProjectState>,
    colors: &SemanticColors,
    cx: &mut Context<Timeline>,
) -> AnyElement {
    if max_scroll_x <= 0.0 || total_content_width <= lane_width {
        return div().id("timeline-scrollbar-empty").h(px(8.0)).into_any_element();
    }

    let ratio = (lane_width / total_content_width.max(1.0)).clamp(0.05, 1.0);
    let thumb_width = (lane_width * ratio).clamp(32.0, lane_width);
    let available_travel = (lane_width - thumb_width).max(1.0);
    let scroll_fraction = (scroll_x / max_scroll_x).clamp(0.0, 1.0);
    let thumb_left = available_travel * scroll_fraction;

    let drag = TimelineScrollDrag {
        lane_width,
        max_scroll_x,
    };

    let state_click = project_state;
    div()
        .id("timeline-h-scrollbar-rail")
        .flex_none()
        .h(px(10.0))
        .w_full()
        .pl(px(TRACK_HEADER_WIDTH_PX))
        .pr(px(VU_METER_WIDTH_PX))
        .bg(colors.background.opacity(0.55))
        .border_t_1()
        .border_color(colors.border.opacity(0.35))
        .child(
            div()
                .id("timeline-h-scrollbar-track")
                .relative()
                .w_full()
                .h_full()
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, move |event: &MouseDownEvent, _window, cx| {
                    let click_in_bar = f32::from(event.position.x) - TRACK_HEADER_WIDTH_PX;
                    let frac = (click_in_bar / lane_width.max(1.0)).clamp(0.0, 1.0);
                    let target_scroll = frac * max_scroll_x;
                    state_click.update(cx, |s, cx| {
                        s.set_timeline_scroll_x(target_scroll, cx);
                    });
                })
                .on_drag(drag, |_, _, _, cx| cx.new(|_| gpui::Empty))
                .on_drag_move::<TimelineScrollDrag>(cx.listener(Timeline::on_scrollbar_drag_move))
                .child(
                    div()
                        .id("timeline-h-scrollbar-thumb")
                        .absolute()
                        .left(px(thumb_left))
                        .w(px(thumb_width))
                        .h(px(8.0))
                        .top(px(1.0))
                        .rounded(px(4.0))
                        .bg(colors.muted_foreground.opacity(0.60))
                        .hover(|s| s.bg(colors.muted_foreground.opacity(0.95))),
                ),
        )
        .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn render_time_ruler(
    timeline_span: f64,
    playhead_time: f64,
    ph_pixel_x: f32,
    scroll_x: f32,
    px_per_sec: f32,
    lane_width: f32,
    project_state: Entity<ProjectState>,
    window: &Window,
    cx: &mut Context<Timeline>,
) -> AnyElement {
    let colors = window.theme().colors;

    let step_secs = if px_per_sec >= 300.0 {
        0.5
    } else if px_per_sec >= 150.0 {
        1.0
    } else if px_per_sec >= 70.0 {
        2.0
    } else if px_per_sec >= 35.0 {
        5.0
    } else if px_per_sec >= 15.0 {
        10.0
    } else if px_per_sec >= 8.0 {
        30.0
    } else {
        60.0
    };

    let start_time = ((scroll_x / px_per_sec).max(0.0) as f64).floor();
    let end_time = (((scroll_x + lane_width) / px_per_sec) as f64 + step_secs).min(timeline_span.max(600.0));

    let first_tick = (start_time / step_secs).floor() as i64;
    let last_tick = (end_time / step_secs).ceil() as i64;

    let tick_elements: Vec<AnyElement> = (first_tick..=last_tick)
        .filter_map(|idx| {
            let t = (idx as f64) * step_secs;
            if t < 0.0 {
                return None;
            }
            let x = (t * px_per_sec as f64) as f32 - scroll_x;
            if x < -10.0 || x > lane_width + 10.0 {
                return None;
            }
            let is_major = idx % 2 == 0;
            Some(
                div()
                    .absolute()
                    .left(px(x))
                    .top_0()
                    .bottom_0()
                    .flex()
                    .flex_col()
                    .justify_between()
                    .child(
                        div()
                            .w(px(1.0))
                            .h(if is_major { px(10.0) } else { px(5.0) })
                            .bg(if is_major { colors.border } else { colors.border.opacity(0.4) }),
                    )
                    .when(is_major, |this| {
                        this.child(
                            div()
                                .pl(px(3.0))
                                .pb(px(2.0))
                                .text_size(px(9.0))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(colors.muted_foreground)
                                .whitespace_nowrap()
                                .child(format_timecode(t)),
                        )
                    })
                    .into_any_element(),
            )
        })
        .collect();

    let state_click = project_state;
    div()
        .id("timeline-time-ruler")
        .relative()
        .flex()
        .flex_1()
        .h_full()
        .items_center()
        .overflow_hidden()
        .cursor_ew_resize()
        .on_mouse_down(MouseButton::Left, move |event: &MouseDownEvent, _window, cx| {
            let exact_time = window_x_to_timeline_time_zoomed(event.position.x, scroll_x, px_per_sec);
            state_click.update(cx, |s, cx| s.set_playhead_time(exact_time, cx));
        })
        .on_drag(PlayheadScrubDrag, |_, _, _, cx| cx.new(|_| gpui::Empty))
        .on_drag_move::<PlayheadScrubDrag>(cx.listener(Timeline::on_scrub_drag_move))
        .children(tick_elements)
        .child(
            // Playhead Cap Indicator on Ruler
            div()
                .absolute()
                .left(px(ph_pixel_x))
                .top_0()
                .bottom_0()
                .flex()
                .items_center()
                .child(div().w(px(2.0)).h_full().bg(hsla(0.56, 0.95, 0.65, 1.0)))
                .child(
                    div()
                        .px(px(4.0))
                        .py(px(1.0))
                        .rounded(px(3.0))
                        .bg(hsla(0.56, 0.90, 0.48, 1.0))
                        .text_color(colors.foreground)
                        .text_size(px(9.0))
                        .font_weight(FontWeight::BOLD)
                        .child(format!("{} (Playhead)", format_timecode(playhead_time))),
                ),
        )
        .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn render_track_row(
    row_idx: usize,
    track: &Track,
    timeline_span: f64,
    ph_pixel_x: f32,
    scroll_x: f32,
    px_per_sec: f32,
    lane_width: f32,
    selected_clip_id: Option<Uuid>,
    state_ref: &ProjectState,
    project_state: Entity<ProjectState>,
    window: &Window,
) -> AnyElement {
    let _ = timeline_span;
    let _ = lane_width;
    let colors = window.theme().colors;
    let (track_code, badge_hue) = track_studio_badge_spec(track, row_idx);

    let clip_blocks: Vec<AnyElement> = track
        .clips
        .iter()
        .map(|clip| {
            let is_selected = selected_clip_id == Some(clip.id);
            let thumb = if track.kind == TrackKind::Video {
                state_ref.thumbnail_for_path(&clip.source_path)
            } else {
                None
            };
            let clip_duration = (clip.timeline_out - clip.timeline_in).max(0.1);
            let clip_width_px = (clip_duration * px_per_sec as f64).max(4.0) as f32;
            let bar_count = ((clip_width_px / 3.5).round() as usize).clamp(16, 256);
            let waveform_amps = if track.kind == TrackKind::Audio {
                Some(state_ref.waveform_amplitudes_for_clip(clip, bar_count))
            } else {
                None
            };
            let source_duration = state_ref.source_duration_for_clip(clip.id);
            let has_contiguous_prev = track
                .clips
                .iter()
                .any(|c| c.id != clip.id && (c.timeline_out - clip.timeline_in).abs() < 0.005);
            let has_contiguous_next = track
                .clips
                .iter()
                .any(|c| c.id != clip.id && (clip.timeline_out - c.timeline_in).abs() < 0.005);

            render_clip_block(
                clip,
                track.id,
                track.kind,
                badge_hue,
                thumb,
                waveform_amps,
                source_duration,
                scroll_x,
                px_per_sec,
                is_selected,
                has_contiguous_prev,
                has_contiguous_next,
                project_state.clone(),
                window,
            )
        })
        .collect();

    let track_id = track.id;
    let state_vis = project_state.clone();
    let state_lock = project_state.clone();
    let state_lane_click = project_state;

    let vis_or_mute_label = match track.kind {
        TrackKind::Audio => {
            if track.muted {
                "🔇"
            } else {
                "🔊"
            }
        }
        TrackKind::Video | TrackKind::Subtitle => {
            if track.visible {
                "👁"
            } else {
                "🚫"
            }
        }
    };

    let lock_label = if track.locked { "🔒" } else { "🔓" };
    let lane_id = SharedString::from(format!("track-lane-{}", track.id));

    div()
        .flex()
        .flex_col()
        .flex_none()
        .h(px(38.0))
        .w_full()
        .child(
            div()
                .flex()
                .items_center()
                .flex_1()
                .w_full()
                .child(
                    // Left Track Header: [Code Badge (V3/V2/V1/A1/A2/A3)] + [Track Name] + [Eye/Mute] + [Lock]
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .flex_none()
                        .w(px(TRACK_HEADER_WIDTH_PX))
                        .h_full()
                        .px(px(8.0))
                        .overflow_hidden()
                        .border_r_1()
                        .border_color(colors.border)
                        .bg(colors.sidebar)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .overflow_hidden()
                                .child(
                                    div()
                                        .px(px(5.0))
                                        .py(px(1.5))
                                        .rounded(px(3.0))
                                        .bg(hsla(badge_hue, 0.72, 0.36, 0.92))
                                        .text_color(colors.foreground)
                                        .text_size(px(9.5))
                                        .font_weight(FontWeight::BOLD)
                                        .child(track_code),
                                )
                                .child(
                                    div()
                                        .text_size(px(11.0))
                                        .font_weight(FontWeight::MEDIUM)
                                        .whitespace_nowrap()
                                        .text_color(if track.muted || !track.visible {
                                             colors.muted_foreground
                                        } else {
                                            colors.foreground
                                        })
                                        .child(track.name.clone()),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(2.0))
                                .child(
                                    Button::new(
                                        SharedString::from(format!("track-vis-{track_id}")),
                                        vis_or_mute_label,
                                    )
                                    .variant(ButtonVariant::Ghost)
                                    .size(ButtonSize::XSmall)
                                    .on_click(move |_, _, cx| {
                                        state_vis.update(cx, |s, cx| {
                                            s.toggle_track_mute_or_visibility(track_id, cx);
                                        });
                                    }),
                                )
                                .child(
                                    Button::new(
                                        SharedString::from(format!("track-lock-{track_id}")),
                                        lock_label,
                                    )
                                    .variant(ButtonVariant::Ghost)
                                    .size(ButtonSize::XSmall)
                                    .on_click(move |_, _, cx| {
                                        state_lock.update(cx, |s, cx| {
                                            s.toggle_track_lock(track_id, cx);
                                        });
                                    }),
                                ),
                        ),
                )
                .child(
                    // Right Track Clip Lane + Playhead Needle
                    div()
                        .id(lane_id)
                        .relative()
                        .flex_1()
                        .h_full()
                        .bg(colors.background)
                        .overflow_hidden()
                        .on_mouse_down(
                            MouseButton::Left,
                            move |event: &MouseDownEvent, _window, cx| {
                                let clicked_time = window_x_to_timeline_time_zoomed(
                                    event.position.x,
                                    scroll_x,
                                    px_per_sec,
                                );
                                state_lane_click.update(cx, |s, cx| {
                                    s.set_playhead_time(clicked_time, cx);
                                });
                            },
                        )
                        .children(clip_blocks)
                        .child(
                            // Vertical Playhead Needle piercing this track
                            div()
                                .absolute()
                                .left(px(ph_pixel_x))
                                .top_0()
                                .bottom_0()
                                .w(px(2.0))
                                .bg(hsla(0.56, 0.95, 0.65, 0.95)),
                        ),
                ),
        )
        .child(Separator::horizontal())
        .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn render_clip_block(
    clip: &Clip,
    track_id: Uuid,
    kind: TrackKind,
    hue: f32,
    thumbnail: Option<Arc<RenderImage>>,
    waveform_amplitudes: Option<Vec<f32>>,
    source_duration: Option<f64>,
    scroll_x: f32,
    px_per_sec: f32,
    is_selected: bool,
    has_contiguous_prev: bool,
    has_contiguous_next: bool,
    project_state: Entity<ProjectState>,
    window: &Window,
) -> AnyElement {
    let colors = window.theme().colors;
    let (left_px, width_px) =
        clip_layout_pixels(clip.timeline_in, clip.timeline_out, scroll_x, px_per_sec);

    let bg_color = match kind {
        TrackKind::Subtitle => hsla(0.76, 0.68, if is_selected { 0.42 } else { 0.32 }, 0.92),
        TrackKind::Video => hsla(hue, 0.65, if is_selected { 0.28 } else { 0.20 }, 0.92),
        TrackKind::Audio => hsla(hue, 0.68, if is_selected { 0.22 } else { 0.15 }, 0.92),
    };

    let border_color = if is_selected {
        hsla(0.56, 0.95, 0.68, 1.0)
    } else {
        hsla(hue, 0.70, 0.45, 0.85)
    };

    let clip_id = clip.id;
    let clip_in = clip.timeline_in;
    let clip_out = clip.timeline_out;
    let element_id = SharedString::from(format!("clip-{clip_id}"));
    let meta_text = format!(
        "{:.1}s–{:.1}s • {:.1}x",
        clip.timeline_in, clip.timeline_out, clip.speed
    );

    let inner_content: AnyElement = match kind {
        TrackKind::Video => {
            // Video Clip Block: Left Thumbnail + Title & Metadata + Selected Badge
            div()
                .flex()
                .items_center()
                .size_full()
                .gap(px(6.0))
                .overflow_hidden()
                .when_some(thumbnail, |this, thumb_img| {
                    this.child(
                        div()
                            .flex_none()
                            .w(px(46.0))
                            .h_full()
                            .rounded(px(3.0))
                            .bg(black())
                            .overflow_hidden()
                            .child(img(thumb_img).size_full().object_fit(ObjectFit::Cover)),
                    )
                })
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .justify_center()
                        .min_w_0()
                        .flex_1()
                        .overflow_hidden()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap(px(4.0))
                                .child(
                                    div()
                                        .text_size(px(10.5))
                                        .font_weight(FontWeight::BOLD)
                                        .whitespace_nowrap()
                                        .child(clip.name.clone()),
                                )
                                .when(is_selected, |this| {
                                    this.child(
                                        div()
                                            .px(px(4.0))
                                            .py(px(0.5))
                                            .rounded(px(2.0))
                                            .bg(hsla(0.56, 0.90, 0.45, 1.0))
                                            .text_size(px(8.0))
                                            .font_weight(FontWeight::BOLD)
                                            .child("SELECCIONADO"),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .text_size(px(8.5))
                                .text_color(colors.muted_foreground)
                                .whitespace_nowrap()
                                .child(format!("{meta_text} • Rec.709")),
                        ),
                )
                .into_any_element()
        }
        TrackKind::Audio => {
            // Audio Clip Block: Label Row + Real RMS/Peak Waveform Bars
            let amp_count = waveform_amplitudes.as_ref().map(|v| v.len()).unwrap_or(48);
            let waveform_bars = render_audio_waveform_bars(waveform_amplitudes.as_deref(), amp_count);
            div()
                .relative()
                .flex()
                .flex_col()
                .justify_between()
                .size_full()
                .px(px(6.0))
                .py(px(2.0))
                .overflow_hidden()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(4.0))
                        .child(
                            div()
                                .text_size(px(10.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(hsla(0.45, 0.90, 0.78, 1.0))
                                .whitespace_nowrap()
                                .child(clip.name.clone()),
                        )
                        .child(
                            div()
                                .px(px(4.0))
                                .rounded(px(2.0))
                                .bg(hsla(0.45, 0.80, 0.22, 0.85))
                                .text_color(hsla(0.45, 0.90, 0.78, 1.0))
                                .text_size(px(8.0))
                                .font_weight(FontWeight::BOLD)
                                .child("IA ENHANCED -14 LUFS"),
                        ),
                )
                .child(waveform_bars)
                .into_any_element()
        }
        TrackKind::Subtitle => {
            // Subtitle / Text Block on V3
            div()
                .flex()
                .items_center()
                .justify_between()
                .size_full()
                .px(px(6.0))
                .overflow_hidden()
                .child(
                    div()
                        .text_size(px(10.0))
                        .font_weight(FontWeight::BOLD)
                        .whitespace_nowrap()
                        .child(clip.name.clone()),
                )
                .into_any_element()
        }
    };

    let clip_drag = ClipDrag {
        clip_id,
        initial_track_id: track_id,
        initial_timeline_in: clip_in,
        _clip_duration: (clip_out - clip_in).max(0.1),
    };

    let initial_trim = crate::timeline_edit::TrimResult::from_clip(clip);
    let left_trim_drag = ClipTrimDrag {
        clip_id,
        edge: crate::timeline_edit::TrimEdge::Left,
        initial: initial_trim,
        source_duration,
    };
    let right_trim_drag = ClipTrimDrag {
        clip_id,
        edge: crate::timeline_edit::TrimEdge::Right,
        initial: initial_trim,
        source_duration,
    };

    let handle_w = crate::timeline_edit::TRIM_HANDLE_WIDTH_PX;

    div()
        .id(element_id)
        .relative()
        .absolute()
        .left(px(left_px))
        .w(px(width_px))
        .top(px(3.0))
        .bottom(px(3.0))
        .when(has_contiguous_prev && has_contiguous_next, |this| this.rounded(px(0.0)))
        .when(has_contiguous_prev && !has_contiguous_next, |this| this.rounded_l(px(0.0)).rounded_r(px(3.0)))
        .when(!has_contiguous_prev && has_contiguous_next, |this| this.rounded_l(px(3.0)).rounded_r(px(0.0)))
        .when(!has_contiguous_prev && !has_contiguous_next, |this| this.rounded(px(3.0)))
        .when(is_selected, |this| this.border_2())
        .when(!is_selected, |this| this.border_1())
        .border_color(border_color)
        .bg(bg_color)
        .text_color(colors.foreground)
        .cursor_move()
        .overflow_hidden()
        .on_drag(clip_drag, |_, _, _, cx| cx.new(|_| gpui::Empty))
        .on_click(move |event: &ClickEvent, _window, cx| {
            cx.stop_propagation();
            let raw_time = window_x_to_timeline_time_zoomed(
                event.position().x,
                scroll_x,
                px_per_sec,
            );
            let clamped_playhead = raw_time.clamp(clip_in, clip_out);
            project_state.update(cx, |state, cx| {
                state.select_clip_and_set_playhead(clip_id, clamped_playhead, cx);
            });
        })
        .child(
            div()
                .size_full()
                .px(px(handle_w))
                .py(px(2.0))
                .overflow_hidden()
                .child(inner_content),
        )
        // Left Trim Handle [↔]
        .child(
            div()
                .id(SharedString::from(format!("trim-l-{clip_id}")))
                .absolute()
                .left_0()
                .top_0()
                .bottom_0()
                .w(px(handle_w))
                .cursor(CursorStyle::ResizeLeftRight)
                .flex()
                .items_center()
                .justify_start()
                .on_drag(left_trim_drag, |_, _, _, cx| cx.new(|_| gpui::Empty))
                .child(
                    div()
                        .h_full()
                        .w(px(2.5))
                        .rounded_l(px(2.0))
                        .bg(if is_selected {
                            hsla(0.56, 0.95, 0.72, 0.95)
                        } else {
                            hsla(0.0, 0.0, 1.0, 0.35)
                        }),
                ),
        )
        // Right Trim Handle [↔]
        .child(
            div()
                .id(SharedString::from(format!("trim-r-{clip_id}")))
                .absolute()
                .right_0()
                .top_0()
                .bottom_0()
                .w(px(handle_w))
                .cursor(CursorStyle::ResizeLeftRight)
                .flex()
                .items_center()
                .justify_end()
                .on_drag(right_trim_drag, |_, _, _, cx| cx.new(|_| gpui::Empty))
                .child(
                    div()
                        .h_full()
                        .w(px(2.5))
                        .rounded_r(px(2.0))
                        .bg(if is_selected {
                            hsla(0.56, 0.95, 0.72, 0.95)
                        } else {
                            hsla(0.0, 0.0, 1.0, 0.35)
                        }),
                ),
        )
        // 1px Cut Seam on right edge: crisp dark grey divider between abutting clips
        .child(
            div()
                .id(SharedString::from(format!("cut-seam-{clip_id}")))
                .absolute()
                .right_0()
                .top_0()
                .bottom_0()
                .w(px(1.0))
                .bg(if has_contiguous_next {
                    hsla(0.0, 0.0, 0.12, 0.95)
                } else {
                    hsla(0.0, 0.0, 0.22, 0.85)
                }),
        )
        .into_any_element()
}

/// Renders authentic audio waveform bars using extracted RMS/peak amplitudes.
fn render_audio_waveform_bars(amplitudes: Option<&[f32]>, count: usize) -> AnyElement {
    let bars: Vec<AnyElement> = (0..count)
        .map(|i| {
            let frac = amplitudes
                .and_then(|a| a.get(i).copied())
                .unwrap_or(0.20);
            let bar_color = if i % 3 == 0 {
                hsla(0.48, 0.88, 0.62, 0.82)
            } else {
                hsla(0.43, 0.85, 0.52, 0.72)
            };

            div()
                .flex_1()
                .h(relative(frac.clamp(0.08, 1.0)))
                .rounded(px(1.0))
                .bg(bar_color)
                .into_any_element()
        })
        .collect();

    div()
        .flex()
        .items_end()
        .w_full()
        .h(px(16.0))
        .gap(px(1.5))
        .children(bars)
        .into_any_element()
}

/// Renders the right-hand studio VU Master Audio Meter (`L / R` + dB scale).
fn render_vu_master_meter(is_playing: bool, playhead_time: f64, window: &Window) -> AnyElement {
    let colors = window.theme().colors;
    let pulse = if is_playing {
        ((playhead_time * 11.0).sin().abs() as f32 * 0.35) + 0.50
    } else {
        0.18
    };
    let left_level = pulse.clamp(0.1, 0.92);
    let right_level = (pulse * 0.93).clamp(0.1, 0.92);

    div()
        .flex()
        .flex_col()
        .flex_none()
        .items_center()
        .justify_between()
        .w(px(VU_METER_WIDTH_PX))
        .h_full()
        .py(px(6.0))
        .px(px(4.0))
        .border_l_1()
        .border_color(colors.border)
        .bg(colors.sidebar)
        .child(
            div()
                .text_size(px(9.0))
                .font_weight(FontWeight::BOLD)
                .text_color(colors.muted_foreground)
                .child("L  VU  R"),
        )
        .child(
            div()
                .flex()
                .flex_1()
                .items_end()
                .justify_center()
                .gap(px(5.0))
                .py(px(4.0))
                .w_full()
                .child(
                    div()
                        .flex()
                        .items_end()
                        .w(px(6.0))
                        .h_full()
                        .rounded(px(2.0))
                        .bg(colors.background)
                        .overflow_hidden()
                        .child(
                            div()
                                .w_full()
                                .h(relative(left_level))
                                .bg(hsla(0.43, 0.85, 0.52, 0.92)),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_end()
                        .w(px(6.0))
                        .h_full()
                        .rounded(px(2.0))
                        .bg(colors.background)
                        .overflow_hidden()
                        .child(
                            div()
                                .w_full()
                                .h(relative(right_level))
                                .bg(hsla(0.48, 0.85, 0.56, 0.92)),
                        ),
                ),
        )
        .child(
            div()
                .text_size(px(8.5))
                .font_weight(FontWeight::BOLD)
                .text_color(hsla(0.43, 0.88, 0.68, 1.0))
                .child(if is_playing { "-2.4 dB" } else { "-∞ dB" }),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{
        clip_layout_fractions, clip_layout_pixels, playhead_fraction, playhead_pixel_x,
        window_x_to_timeline_time_zoomed,
    };
    use gpui::px;

    #[test]
    fn computes_proportional_clip_positions() {
        let (left, width) = clip_layout_fractions(5.0, 15.0, 20.0);
        assert!((left - 0.25).abs() < 1e-5);
        assert!((width - 0.50).abs() < 1e-5);
    }

    #[test]
    fn clamps_out_of_bounds_clip_fractions() {
        let (left, width) = clip_layout_fractions(-2.0, 30.0, 20.0);
        assert!((left - 0.0).abs() < 1e-5);
        assert!((width - 1.0).abs() < 1e-5);
    }

    #[test]
    fn computes_normalized_playhead_fraction() {
        assert!((playhead_fraction(4.2, 8.4) - 0.5).abs() < 1e-5);
        assert!((playhead_fraction(-1.0, 10.0) - 0.0).abs() < 1e-5);
        assert!((playhead_fraction(25.0, 10.0) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn computes_zoomed_clip_layout_pixels_with_scroll() {
        let (left_px, width_px) = clip_layout_pixels(2.0, 6.0, 50.0, 100.0);
        // left = 2.0s * 100px/s - 50px = 150px
        assert!((left_px - 150.0).abs() < 1e-5);
        // width = (6.0 - 2.0) * 100px/s = 400px
        assert!((width_px - 400.0).abs() < 1e-5);
    }

    #[test]
    fn computes_zoomed_playhead_pixel_x() {
        let ph_px = playhead_pixel_x(3.5, 50.0, 60.0);
        // 3.5s * 60px/s - 50px = 210 - 50 = 160px
        assert!((ph_px - 160.0).abs() < 1e-5);
    }

    #[test]
    fn converts_window_x_to_timeline_time_zoomed() {
        // Track header width = 236px. Mouse at 336px -> x_in_lane = 100px.
        // scroll_x = 50px, pps = 100px/s -> time = (100 + 50) / 100 = 1.5s
        let time = window_x_to_timeline_time_zoomed(px(336.0), 50.0, 100.0);
        assert!((time - 1.5).abs() < 1e-5);
    }
}
