use gpui::{
    AnyElement, Context, Entity, FontWeight, ObjectFit, SharedString, StyledImage, Window, black,
    div, hsla, img, prelude::*, px, relative,
};

use crate::{
    components::{Badge, BadgeVariant, Button, ButtonSize, ButtonVariant, Label},
    project_state::{PreviewAspectRatio, ProjectState, format_timecode},
    theme::ActiveTheme,
};

pub(crate) struct Preview {
    project_state: Entity<ProjectState>,
}

impl Preview {
    pub(crate) fn new(project_state: Entity<ProjectState>, cx: &mut Context<Self>) -> Self {
        cx.observe(&project_state, |_, _, cx| {
            cx.notify();
        })
        .detach();

        Self { project_state }
    }
}

impl Render for Preview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = window.theme().colors;
        let state = self.project_state.read(cx);
        let project = state.project();
        let playhead = state.playhead_time();
        let duration = project.duration();
        let is_playing = state.is_playing();
        let current_aspect = state.aspect_ratio();
        let mark_in = state.mark_in();
        let mark_out = state.mark_out();

        let active_clip_info = state
            .active_video_clip_at_playhead()
            .map(|(clip, source_time)| (clip.name.clone(), source_time));
        let active_subtitle = state.active_subtitle_at_playhead();
        let (v1_frame, v2_frame_and_clip) = state.current_preview_layers();
        let active_v2_clip = state.active_v2_clip_at_playhead().map(|(c, _)| c.clone());

        let aspect_options = [
            PreviewAspectRatio::Widescreen16x9,
            PreviewAspectRatio::Vertical9x16,
            PreviewAspectRatio::Square1x1,
            PreviewAspectRatio::Cinema239x1,
        ];

        let aspect_buttons: Vec<AnyElement> = aspect_options
            .into_iter()
            .map(|aspect| {
                let is_active = aspect == current_aspect;
                let state_aspect = self.project_state.clone();
                let btn_id = SharedString::from(format!("preview-aspect-{:?}", aspect));
                Button::new(btn_id, aspect.short_label())
                    .variant(if is_active {
                        ButtonVariant::Default
                    } else {
                        ButtonVariant::Ghost
                    })
                    .size(ButtonSize::XSmall)
                    .on_click(move |_, _, cx| {
                        state_aspect.update(cx, |s, cx| {
                            s.set_aspect_ratio(aspect, cx);
                        });
                    })
                    .into_any_element()
            })
            .collect();

        let (canvas_w_frac, canvas_h_frac) = match current_aspect {
            PreviewAspectRatio::Widescreen16x9 => (1.0_f32, 1.0_f32),
            PreviewAspectRatio::Vertical9x16 => (0.42_f32, 0.98_f32),
            PreviewAspectRatio::Square1x1 => (0.64_f32, 0.96_f32),
            PreviewAspectRatio::Cinema239x1 => (1.0_f32, 0.76_f32),
        };

        // Interactive ENTRADA [I] and SALIDA [O] Buttons
        let mark_in_label = match mark_in {
            Some(t) => format!("ENTRADA [I] {}", format_timecode(t)),
            None => "ENTRADA [I]".to_owned(),
        };
        let state_mark_in = self.project_state.clone();
        let mark_in_btn = Button::new("preview-mark-in-btn", mark_in_label)
            .variant(if mark_in.is_some() {
                ButtonVariant::Default
            } else {
                ButtonVariant::Outline
            })
            .size(ButtonSize::XSmall)
            .on_click(move |_, _, cx| {
                state_mark_in.update(cx, |s, cx| s.set_mark_in_at_playhead(cx));
            });

        let mark_out_label = match mark_out {
            Some(t) => format!("SALIDA [O] {}", format_timecode(t)),
            None => "SALIDA [O]".to_owned(),
        };
        let state_mark_out = self.project_state.clone();
        let mark_out_btn = Button::new("preview-mark-out-btn", mark_out_label)
            .variant(if mark_out.is_some() {
                ButtonVariant::Default
            } else {
                ButtonVariant::Outline
            })
            .size(ButtonSize::XSmall)
            .on_click(move |_, _, cx| {
                state_mark_out.update(cx, |s, cx| s.set_mark_out_at_playhead(cx));
            });

        let has_marks = mark_in.is_some() || mark_out.is_some();
        let state_clear_marks = self.project_state.clone();
        let clear_marks_btn = has_marks.then(|| {
            Button::new("preview-clear-marks-btn", "✕ Rango")
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::XSmall)
                .on_click(move |_, _, cx| {
                    state_clear_marks.update(cx, |s, cx| s.clear_in_out_marks(cx));
                })
        });

        let state_jump_start = self.project_state.clone();
        let jump_start_btn = Button::new("preview-jump-start", "⏮")
            .variant(ButtonVariant::Ghost)
            .size(ButtonSize::XSmall)
            .on_click(move |_, _, cx| {
                state_jump_start.update(cx, |s, cx| s.set_playhead_time(0.0, cx));
            });

        let state_back = self.project_state.clone();
        let step_back_btn = Button::new("preview-step-back", "⏪ -1s")
            .variant(ButtonVariant::Outline)
            .size(ButtonSize::XSmall)
            .on_click(move |_, _, cx| {
                state_back.update(cx, |s, cx| s.step_playhead(-1.0, cx));
            });

        let state_play = self.project_state.clone();
        let play_btn = Button::new(
            "preview-play-toggle",
            if is_playing { "⏸ Pause" } else { "▶ Play" },
        )
        .variant(ButtonVariant::Default)
        .size(ButtonSize::Small)
        .on_click(move |_, _, cx| {
            state_play.update(cx, |s, cx| s.toggle_playback(cx));
        });

        let state_fwd = self.project_state.clone();
        let step_fwd_btn = Button::new("preview-step-fwd", "+1s ⏩")
            .variant(ButtonVariant::Outline)
            .size(ButtonSize::XSmall)
            .on_click(move |_, _, cx| {
                state_fwd.update(cx, |s, cx| s.step_playhead(1.0, cx));
            });

        let state_jump_end = self.project_state.clone();
        let jump_end_btn = Button::new("preview-jump-end", "⏭")
            .variant(ButtonVariant::Ghost)
            .size(ButtonSize::XSmall)
            .on_click(move |_, _, cx| {
                state_jump_end.update(cx, |s, cx| {
                    let end = s.project().duration();
                    s.set_playhead_time(end, cx);
                });
            });

        let current_speed = state.playback_speed();
        let state_speed = self.project_state.clone();
        let speed_btn = Button::new(
            "preview-playback-speed-toggle",
            format!("{current_speed:.1}x"),
        )
        .variant(ButtonVariant::Outline)
        .size(ButtonSize::XSmall)
        .on_click(move |_, _, cx| {
            state_speed.update(cx, |s, cx| s.cycle_playback_speed(cx));
        });

        let state_split_quick = self.project_state.clone();
        let quick_split_btn = Button::new("preview-quick-split", "✂ Cortar [S]")
            .variant(ButtonVariant::Secondary)
            .size(ButtonSize::XSmall)
            .on_click(move |_, _, cx| {
                state_split_quick.update(cx, |s, cx| s.split_clip_at_playhead(cx));
            });

        let live_badge_bg = if is_playing {
            hsla(0.42, 0.78, 0.24, 0.88)
        } else {
            hsla(0.0, 0.0, 0.12, 0.85)
        };
        let live_badge_fg = if is_playing {
            hsla(0.42, 0.90, 0.76, 1.0)
        } else {
            colors.muted_foreground
        };
        let live_status_text = if is_playing {
            "● EN VIVO REPRODUCIENDO"
        } else {
            "● MONITOR PROGRAMA"
        };

        div()
            .flex()
            .flex_col()
            .flex_1()
            .h_full()
            .p(px(10.0))
            .gap(px(8.0))
            .bg(colors.background)
            .overflow_hidden()
            .child(
                // Top Player Bar: Sequence Title + [⚡ Aspect Ratio Selector]
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .child(Label::new(format!("Programa: {}", project.name)))
                            .child(Badge::new("REC.709").variant(BadgeVariant::Outline)),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(3.0))
                            .p(px(2.0))
                            .rounded(px(6.0))
                            .bg(colors.card)
                            .border_1()
                            .border_color(colors.border)
                            .children(aspect_buttons),
                    ),
            )
            .child(
                // Outer Viewport Stage
                div()
                    .relative()
                    .flex()
                    .flex_1()
                    .w_full()
                    .min_h_0()
                    .items_center()
                    .justify_center()
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(colors.border)
                    .bg(hsla(0.0, 0.0, 0.04, 1.0))
                    .overflow_hidden()
                    .child(
                        // Aspect-Framed Video Canvas
                        div()
                            .relative()
                            .flex()
                            .w(relative(canvas_w_frac))
                            .h(relative(canvas_h_frac))
                            .items_center()
                            .justify_center()
                            .bg(black())
                            .border_1()
                            .border_color(colors.border)
                            .rounded(px(4.0))
                            .overflow_hidden()
                            .when_some(v1_frame.as_ref(), |this, frame_image| {
                                this.child(
                                    div()
                                        .relative()
                                        .flex()
                                        .size_full()
                                        .items_center()
                                        .justify_center()
                                        .child(
                                            img(frame_image.clone())
                                                .size_full()
                                                .object_fit(ObjectFit::Contain),
                                        ),
                                )
                            })
                            .when_some(v2_frame_and_clip.as_ref(), |this, (ov_image, clip)| {
                                let scale_val = clip.scale.clamp(0.1, 4.0);
                                let opac_val = clip.opacity.clamp(0.0, 1.0);
                                this.child(
                                    div()
                                        .absolute()
                                        .inset_0()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(
                                            div()
                                                .relative()
                                                .flex()
                                                .w(relative(scale_val))
                                                .h(relative(scale_val))
                                                .left(px(clip.position_x))
                                                .top(px(clip.position_y))
                                                .opacity(opac_val)
                                                .items_center()
                                                .justify_center()
                                                .child(
                                                    img(ov_image.clone())
                                                        .size_full()
                                                        .object_fit(ObjectFit::Contain),
                                                ),
                                        ),
                                )
                            })
                            .when_some(active_v2_clip.as_ref(), |this, v2_clip| {
                                let opac = v2_clip.opacity.clamp(0.0, 1.0);
                                let name = &v2_clip.name;
                                if name.contains("LUT") || name.contains("Color") {
                                    this.child(
                                        div()
                                            .absolute()
                                            .inset_0()
                                            .bg(hsla(0.52, 0.45, 0.18, 0.28 * opac)),
                                    )
                                } else if name.contains("Chromatic") || name.contains("Glitch") {
                                    this.child(
                                        div()
                                            .absolute()
                                            .inset_0()
                                            .bg(hsla(0.5, 0.9, 0.4, 0.12 * opac))
                                            .child(
                                                div()
                                                    .absolute()
                                                    .left_0()
                                                    .right_0()
                                                    .top(relative(0.35))
                                                    .h(px(4.0))
                                                    .bg(hsla(0.50, 0.95, 0.55, 0.65 * opac)),
                                            )
                                            .child(
                                                div()
                                                    .absolute()
                                                    .left_0()
                                                    .right_0()
                                                    .top(relative(0.68))
                                                    .h(px(6.0))
                                                    .bg(hsla(0.85, 0.95, 0.55, 0.65 * opac)),
                                            ),
                                    )
                                } else if name.contains("Flare") {
                                    this.child(
                                        div()
                                            .absolute()
                                            .inset_0()
                                            .child(
                                                div()
                                                    .absolute()
                                                    .left_0()
                                                    .right_0()
                                                    .top(relative(0.44))
                                                    .h(px(24.0))
                                                    .bg(hsla(0.55, 0.90, 0.50, 0.25 * opac)),
                                            )
                                            .child(
                                                div()
                                                    .absolute()
                                                    .left_0()
                                                    .right_0()
                                                    .top(relative(0.48))
                                                    .h(px(6.0))
                                                    .bg(hsla(0.54, 0.98, 0.72, 0.85 * opac)),
                                            ),
                                    )
                                } else if name.contains("Flash") {
                                    this.child(
                                        div()
                                            .absolute()
                                            .inset_0()
                                            .bg(hsla(0.0, 0.0, 1.0, 0.40 * opac)),
                                    )
                                } else {
                                    this
                                }
                            })
                            .when(v1_frame.is_none() && v2_frame_and_clip.is_none(), |this| {
                                this.child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .items_center()
                                        .gap(px(4.0))
                                        .child(
                                            Label::new(
                                                "Sin clip de video activo en la posición del playhead",
                                            )
                                            .muted(),
                                        ),
                                )
                            })
                            .when_some(active_subtitle, |this, sub_text| {
                                this.child(
                                    // V3 Subtitle Overlay on Bottom Center of Video Canvas
                                    div()
                                        .absolute()
                                        .bottom(px(38.0))
                                        .left(px(20.0))
                                        .right(px(20.0))
                                        .flex()
                                        .justify_center()
                                        .child(
                                            div()
                                                .px(px(14.0))
                                                .py(px(6.0))
                                                .rounded(px(6.0))
                                                .bg(hsla(0.0, 0.0, 0.0, 0.0))
                                                .text_color(hsla(0.0, 0.0, 1.0, 0.98))
                                                .text_size(px(14.5))
                                                .font_weight(FontWeight::BOLD)
                                                .child(sub_text),
                                        ),
                                )
                            }),
                    )
                    .child(
                        // Top-Left HUD Overlay Badges ("● EN VIVO REPRODUCIENDO" + Resolution)
                        div()
                            .absolute()
                            .top(px(8.0))
                            .left(px(10.0))
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .child(
                                div()
                                    .px(px(7.0))
                                    .py(px(2.5))
                                    .rounded(px(4.0))
                                    .bg(live_badge_bg)
                                    .text_color(live_badge_fg)
                                    .text_size(px(9.5))
                                    .font_weight(FontWeight::BOLD)
                                    .child(live_status_text),
                            )
                            .child(
                                div()
                                    .px(px(7.0))
                                    .py(px(2.5))
                                    .rounded(px(4.0))
                                    .bg(hsla(0.0, 0.0, 0.08, 0.85))
                                    .text_color(colors.foreground)
                                    .text_size(px(9.5))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(format!(
                                        "{} × {} ({})",
                                        project.resolution.width,
                                        project.resolution.height,
                                        current_aspect.short_label()
                                    )),
                            ),
                    )
                    .child(
                        // Bottom-Right HUD Timecode Overlay
                        div()
                            .absolute()
                            .bottom(px(8.0))
                            .right(px(10.0))
                            .px(px(10.0))
                            .py(px(4.0))
                            .rounded(px(5.0))
                            .bg(hsla(0.0, 0.0, 0.05, 0.88))
                            .border_1()
                            .border_color(colors.border)
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .child(
                                div()
                                    .text_size(px(13.0))
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(colors.foreground)
                                    .child(format_timecode(playhead)),
                            )
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(colors.muted_foreground)
                                    .child(format!("/ {}", format_timecode(duration))),
                            ),
                    ),
            )
            .child(
                // GFCut Transport Bar with Interactive Mark In [I] & Mark Out [O]
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .h(px(34.0))
                    .px(px(10.0))
                    .rounded(px(6.0))
                    .bg(colors.card)
                    .border_1()
                    .border_color(colors.border)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(4.0))
                            .child(mark_in_btn)
                            .child(mark_out_btn)
                            .when_some(clear_marks_btn, |this, btn| this.child(btn))
                            .child(quick_split_btn),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(4.0))
                            .child(jump_start_btn)
                            .child(step_back_btn)
                            .child(play_btn)
                            .child(step_fwd_btn)
                            .child(jump_end_btn)
                            .child(speed_btn),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .when_some(active_clip_info, |this, (clip_name, _)| {
                                this.child(
                                    Badge::new(clip_name).variant(BadgeVariant::Secondary),
                                )
                            })
                            .child({
                                let h = project.resolution.height;
                                let res_label = if h >= 2160 {
                                    "4K UHD"
                                } else if h >= 1440 {
                                    "2K QHD"
                                } else if h >= 1080 {
                                    "1080p FHD"
                                } else {
                                    "720p HD"
                                };
                                Badge::new(format!(
                                    "{:.0} FPS • {res_label} ({}x{})",
                                    project.fps.as_f64(),
                                    project.resolution.width,
                                    project.resolution.height
                                ))
                                .variant(BadgeVariant::Outline)
                            }),
                    ),
            )
    }
}
