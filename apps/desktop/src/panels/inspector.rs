use gpui::{
    AnyElement, Context, Entity, FocusHandle, KeyDownEvent, Window, div, hsla, prelude::*, px,
};
use uuid::Uuid;

use crate::{
    components::{
        Badge, BadgeVariant, Button, ButtonSize, ButtonVariant, Label, Separator,
        render_scrollbar_rail,
    },
    project_state::{ProjectState, TRACK_NAME_V3_SUBS, format_timecode},
    theme::ActiveTheme,
};
use gfcut_timeline::TrackKind;

pub(crate) struct Inspector {
    project_state: Entity<ProjectState>,
    is_editing_text: bool,
    editing_clip_id: Option<Uuid>,
    text_draft: String,
    focus_handle: FocusHandle,
    section_text_open: bool,
    section_transform_open: bool,
    section_audio_open: bool,
    section_speed_open: bool,
}

impl Inspector {
    pub(crate) fn new(project_state: Entity<ProjectState>, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();

        cx.observe(&project_state, |this, _, cx| {
            let sel_id = this.project_state.read(cx).selected_clip_id();
            if this.editing_clip_id != sel_id {
                this.is_editing_text = false;
                this.editing_clip_id = sel_id;
                this.text_draft.clear();
            }
            cx.notify();
        })
        .detach();

        Self {
            project_state,
            is_editing_text: false,
            editing_clip_id: None,
            text_draft: String::new(),
            focus_handle,
            section_text_open: true,
            section_transform_open: true,
            section_audio_open: true,
            section_speed_open: true,
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.is_editing_text {
            return;
        }

        let key = event.keystroke.key.as_str();
        match key {
            "backspace" => {
                self.text_draft.pop();
                cx.notify();
            }
            "space" | " " => {
                self.text_draft.push(' ');
                cx.notify();
            }
            "enter" => {
                let draft = self.text_draft.trim().to_owned();
                self.project_state.update(cx, |s, cx| {
                    s.update_selected_clip_subtitle_text(&draft, cx);
                });
                self.is_editing_text = false;
                cx.notify();
            }
            "escape" => {
                self.is_editing_text = false;
                cx.notify();
            }
            _ => {
                if let Some(ch) = &event.keystroke.key_char {
                    if !event.keystroke.modifiers.control && !event.keystroke.modifiers.platform {
                        self.text_draft.push_str(ch);
                        cx.notify();
                    }
                } else if key.chars().count() == 1
                    && !event.keystroke.modifiers.control
                    && !event.keystroke.modifiers.platform
                {
                    self.text_draft.push_str(key);
                    cx.notify();
                }
            }
        }
    }
}

impl Render for Inspector {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = window.theme().colors;
        let state = self.project_state.read(cx);

        if state.inspector_collapsed() {
            return div()
                .id("gfcut-inspector-collapsed")
                .w(px(0.0))
                .h_full()
                .overflow_hidden()
                .into_any_element();
        }

        let project = state.project();

        let total_clips: usize = project.tracks.iter().map(|t| t.clips.len()).sum();
        let resolution_str = format!(
            "{} × {} px",
            project.resolution.width, project.resolution.height
        );
        let fps_str = format!(
            "{:.2} fps ({}/{})",
            project.fps.as_f64(),
            project.fps.num,
            project.fps.den
        );
        let duration_str = format_timecode(project.duration());
        let playhead_str = format_timecode(state.playhead_time());
        let path_str = state
            .project_path()
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("demo.gfcut.json")
            .to_owned();

        let selected_clip = state
            .selected_clip_id()
            .and_then(|id| project.find_clip(id).ok().map(|(_, c)| c.clone()));

        let state_pos_zero = self.project_state.clone();
        let state_pos_back = self.project_state.clone();
        let state_pos_fwd = self.project_state.clone();

        let state_spd_05 = self.project_state.clone();
        let state_spd_10 = self.project_state.clone();
        let state_spd_15 = self.project_state.clone();
        let state_spd_20 = self.project_state.clone();

        let state_vol_0 = self.project_state.clone();
        let state_vol_50 = self.project_state.clone();
        let state_vol_100 = self.project_state.clone();

        let state_scale_50 = self.project_state.clone();
        let state_scale_75 = self.project_state.clone();
        let state_scale_100 = self.project_state.clone();
        let state_scale_150 = self.project_state.clone();

        let state_opac_25 = self.project_state.clone();
        let state_opac_50 = self.project_state.clone();
        let state_opac_75 = self.project_state.clone();
        let state_opac_100 = self.project_state.clone();

        let state_pos_center = self.project_state.clone();
        let state_pos_pip_tr = self.project_state.clone();
        let state_pos_pip_br = self.project_state.clone();
        let state_pos_pip_bl = self.project_state.clone();
        let (proj_w, proj_h) = (project.resolution.width, project.resolution.height);

        let is_subtitle_or_text = state.selected_clip_id().and_then(|id| {
            project.find_clip(id).ok().map(|(t_id, _)| {
                project
                    .tracks
                    .iter()
                    .any(|t| t.id == t_id && (t.kind == TrackKind::Subtitle || t.name == TRACK_NAME_V3_SUBS))
            })
        }).unwrap_or(false);

        let is_editing = self.is_editing_text;
        let text_draft = self.text_draft.clone();
        let focus_for_edit = self.focus_handle.clone();
        let sec_text_open = self.section_text_open;
        let sec_trans_open = self.section_transform_open;
        let sec_audio_open = self.section_audio_open;
        let sec_speed_open = self.section_speed_open;

        let state_collapse = self.project_state.clone();
        let state_clear_err = self.project_state.clone();
        let last_err_str = state.last_error().map(str::to_owned);
        let status_msg_str = state.status_message().map(str::to_owned);

        let scrollable_content = div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .when_some(last_err_str, |this, err| {
                this.child(
                    div()
                        .flex()
                        .flex_col()
                        .max_h(px(70.0))
                        .overflow_hidden()
                        .gap(px(3.0))
                        .p(px(6.0))
                        .rounded(px(5.0))
                        .border_1()
                        .border_color(colors.destructive)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(Badge::new("Notice").variant(BadgeVariant::Destructive))
                                .child(
                                    Button::new("insp-clear-err", "✕")
                                        .variant(ButtonVariant::Ghost)
                                        .size(ButtonSize::XSmall)
                                        .on_click(move |_, _, cx| {
                                            state_clear_err
                                                .update(cx, |s, cx| s.clear_last_error(cx));
                                        }),
                                 ),
                        )
                        .child(
                            div()
                                .text_size(px(10.0))
                                .text_color(colors.muted_foreground)
                                .child(err),
                        ),
                )
                .child(Separator::horizontal())
            })
            .when_some(status_msg_str.clone(), |this, msg| {
                this.child(Badge::new(msg).variant(BadgeVariant::Secondary))
            })
            .when_some(selected_clip, |this, clip| {
                let editor_box = if is_editing {
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .p(px(8.0))
                        .rounded(px(6.0))
                        .border_1()
                        .border_color(colors.primary)
                        .bg(colors.background)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .text_size(px(11.0))
                                        .font_weight(gpui::FontWeight::BOLD)
                                        .text_color(colors.foreground)
                                        .child("✏ Editando Subtítulo / Título"),
                                )
                                .child(Badge::new("En Edición").variant(BadgeVariant::Default)),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .min_h(px(34.0))
                                .px(px(8.0))
                                .py(px(4.0))
                                .rounded(px(4.0))
                                .bg(colors.card)
                                .border_1()
                                .border_color(colors.border)
                                .child(
                                    div()
                                        .text_size(px(11.5))
                                        .text_color(if text_draft.is_empty() {
                                            colors.muted_foreground
                                        } else {
                                            colors.foreground
                                        })
                                        .child(if text_draft.is_empty() {
                                            "Escribe el texto aquí... (Enter guardar)".to_owned()
                                        } else {
                                            format!("{} ▍", text_draft)
                                        }),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(5.0))
                                .child(
                                    Button::new("sub-save-btn", "💾 Guardar [Enter]")
                                        .variant(ButtonVariant::Default)
                                        .size(ButtonSize::XSmall)
                                        .on_click(cx.listener(|this, _, _window, cx| {
                                            let draft = this.text_draft.trim().to_owned();
                                            this.project_state.update(cx, |s, cx| {
                                                s.update_selected_clip_subtitle_text(&draft, cx);
                                            });
                                            this.is_editing_text = false;
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Button::new("sub-cancel-btn", "✕ Cancelar [Esc]")
                                        .variant(ButtonVariant::Ghost)
                                        .size(ButtonSize::XSmall)
                                        .on_click(cx.listener(|this, _, _window, cx| {
                                            this.is_editing_text = false;
                                            cx.notify();
                                        })),
                                ),
                        )
                } else {
                    let clean_text = if let Some(stripped) = clip.name.strip_prefix("SUB_") {
                        if let Some((_, text)) = stripped.split_once(": ") {
                            text.to_owned()
                        } else {
                            clip.name.clone()
                        }
                    } else {
                        clip.name.clone()
                    };

                    let raw_name_upper = clip.name.clone();
                    let raw_name_lower = clip.name.clone();
                    let state_upper = self.project_state.clone();
                    let state_lower = self.project_state.clone();
                    let edit_text_init = clean_text.clone();
                    let focus_to_window = focus_for_edit.clone();

                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .p(px(8.0))
                        .rounded(px(6.0))
                        .border_1()
                        .border_color(if is_subtitle_or_text {
                            hsla(0.76, 0.68, 0.45, 0.9)
                        } else {
                            colors.border
                        })
                        .bg(colors.sidebar.opacity(0.4))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .text_size(px(11.0))
                                        .font_weight(gpui::FontWeight::BOLD)
                                        .text_color(if is_subtitle_or_text {
                                            hsla(0.76, 0.85, 0.75, 1.0)
                                        } else {
                                            colors.foreground
                                        })
                                        .child(if is_subtitle_or_text {
                                            "💬 Subtítulo / Texto (V3)"
                                        } else {
                                            "🏷 Título del Clip"
                                        }),
                                )
                                .child(
                                    Button::new("sub-edit-btn", "✏ Editar")
                                        .variant(ButtonVariant::Outline)
                                        .size(ButtonSize::XSmall)
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.is_editing_text = true;
                                            this.text_draft = edit_text_init.clone();
                                            window.focus(&focus_to_window);
                                            cx.notify();
                                        })),
                                ),
                        )
                        .child(
                            div()
                                .p(px(6.0))
                                .rounded(px(4.0))
                                .bg(colors.background)
                                .border_1()
                                .border_color(colors.border)
                                .text_size(px(11.0))
                                .text_color(colors.foreground)
                                .child(clip.name.clone()),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(4.0))
                                .child(
                                    Button::new("sub-upper-btn", "ABC")
                                        .variant(ButtonVariant::Ghost)
                                        .size(ButtonSize::XSmall)
                                        .on_click(move |_, _, cx| {
                                            let upper = raw_name_upper.to_uppercase();
                                            state_upper.update(cx, |s, cx| {
                                                s.update_selected_clip_subtitle_text(&upper, cx);
                                            });
                                        }),
                                )
                                .child(
                                    Button::new("sub-lower-btn", "abc")
                                        .variant(ButtonVariant::Ghost)
                                        .size(ButtonSize::XSmall)
                                        .on_click(move |_, _, cx| {
                                            let lower = raw_name_lower.to_lowercase();
                                            state_lower.update(cx, |s, cx| {
                                                s.update_selected_clip_subtitle_text(&lower, cx);
                                            });
                                        }),
                                ),
                        )
                };

                this.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .p(px(6.0))
                        .rounded(px(5.0))
                        .bg(colors.sidebar.opacity(0.3))
                        .border_1()
                        .border_color(colors.border)
                        .child(Label::new("Clip Seleccionado"))
                        .child(Badge::new(clip.name.clone()).variant(BadgeVariant::Default)),
                )
                // Section 1: Subtítulos & Título
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.0))
                        .p(px(6.0))
                        .rounded(px(6.0))
                        .bg(colors.sidebar.opacity(0.25))
                        .border_1()
                        .border_color(colors.border)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .text_size(px(11.0))
                                        .font_weight(gpui::FontWeight::BOLD)
                                        .text_color(colors.foreground)
                                        .child("💬 Subtítulos & Título"),
                                )
                                .child(
                                    Button::new("insp-sec-toggle-text", if sec_text_open { "▼" } else { "▶" })
                                        .variant(ButtonVariant::Ghost)
                                        .size(ButtonSize::XSmall)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.section_text_open = !this.section_text_open;
                                            cx.notify();
                                        })),
                                ),
                        )
                        .when(sec_text_open, |this| this.child(editor_box)),
                )
                // Section 2: Transformación & Video
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .p(px(6.0))
                        .rounded(px(6.0))
                        .bg(colors.sidebar.opacity(0.25))
                        .border_1()
                        .border_color(colors.border)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .text_size(px(11.0))
                                        .font_weight(gpui::FontWeight::BOLD)
                                        .text_color(colors.foreground)
                                        .child("📐 Transformación & Video"),
                                )
                                .child(
                                    Button::new("insp-sec-toggle-trans", if sec_trans_open { "▼" } else { "▶" })
                                        .variant(ButtonVariant::Ghost)
                                        .size(ButtonSize::XSmall)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.section_transform_open = !this.section_transform_open;
                                            cx.notify();
                                        })),
                                ),
                        )
                        .when(sec_trans_open, |this| {
                            this.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        Label::new(format!("Escala ({:.0}%)", clip.scale * 100.0)).muted(),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(px(4.0))
                                            .child(
                                                Button::new("insp-scale-50", "50%")
                                                    .variant(if (clip.scale - 0.50).abs() < 0.05 {
                                                        ButtonVariant::Default
                                                    } else {
                                                        ButtonVariant::Outline
                                                    })
                                                    .size(ButtonSize::XSmall)
                                                    .on_click(move |_, _, cx| {
                                                        state_scale_50.update(cx, |s, cx| {
                                                            s.set_selected_clip_scale(0.50, cx)
                                                        });
                                                    }),
                                            )
                                            .child(
                                                Button::new("insp-scale-75", "75%")
                                                    .variant(if (clip.scale - 0.75).abs() < 0.05 {
                                                        ButtonVariant::Default
                                                    } else {
                                                        ButtonVariant::Outline
                                                    })
                                                    .size(ButtonSize::XSmall)
                                                    .on_click(move |_, _, cx| {
                                                        state_scale_75.update(cx, |s, cx| {
                                                            s.set_selected_clip_scale(0.75, cx)
                                                        });
                                                    }),
                                            )
                                            .child(
                                                Button::new("insp-scale-100", "100%")
                                                    .variant(if (clip.scale - 1.0).abs() < 0.05 {
                                                        ButtonVariant::Default
                                                    } else {
                                                        ButtonVariant::Outline
                                                    })
                                                    .size(ButtonSize::XSmall)
                                                    .on_click(move |_, _, cx| {
                                                        state_scale_100.update(cx, |s, cx| {
                                                            s.set_selected_clip_scale(1.0, cx)
                                                        });
                                                    }),
                                            )
                                            .child(
                                                Button::new("insp-scale-150", "150%")
                                                    .variant(if (clip.scale - 1.50).abs() < 0.05 {
                                                        ButtonVariant::Default
                                                    } else {
                                                        ButtonVariant::Outline
                                                    })
                                                    .size(ButtonSize::XSmall)
                                                    .on_click(move |_, _, cx| {
                                                        state_scale_150.update(cx, |s, cx| {
                                                            s.set_selected_clip_scale(1.50, cx)
                                                        });
                                                    }),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        Label::new(format!("Opacidad ({:.0}%)", clip.opacity * 100.0)).muted(),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(px(4.0))
                                            .child(
                                                Button::new("insp-opac-25", "25%")
                                                    .variant(if (clip.opacity - 0.25).abs() < 0.05 {
                                                        ButtonVariant::Default
                                                    } else {
                                                        ButtonVariant::Outline
                                                    })
                                                    .size(ButtonSize::XSmall)
                                                    .on_click(move |_, _, cx| {
                                                        state_opac_25.update(cx, |s, cx| {
                                                            s.set_selected_clip_opacity(0.25, cx)
                                                        });
                                                    }),
                                            )
                                            .child(
                                                Button::new("insp-opac-50", "50%")
                                                    .variant(if (clip.opacity - 0.50).abs() < 0.05 {
                                                        ButtonVariant::Default
                                                    } else {
                                                        ButtonVariant::Outline
                                                    })
                                                    .size(ButtonSize::XSmall)
                                                    .on_click(move |_, _, cx| {
                                                        state_opac_50.update(cx, |s, cx| {
                                                            s.set_selected_clip_opacity(0.50, cx)
                                                        });
                                                    }),
                                            )
                                            .child(
                                                Button::new("insp-opac-75", "75%")
                                                    .variant(if (clip.opacity - 0.75).abs() < 0.05 {
                                                        ButtonVariant::Default
                                                    } else {
                                                        ButtonVariant::Outline
                                                    })
                                                    .size(ButtonSize::XSmall)
                                                    .on_click(move |_, _, cx| {
                                                        state_opac_75.update(cx, |s, cx| {
                                                            s.set_selected_clip_opacity(0.75, cx)
                                                        });
                                                    }),
                                            )
                                            .child(
                                                Button::new("insp-opac-100", "100%")
                                                    .variant(if (clip.opacity - 1.0).abs() < 0.05 {
                                                        ButtonVariant::Default
                                                    } else {
                                                        ButtonVariant::Outline
                                                    })
                                                    .size(ButtonSize::XSmall)
                                                    .on_click(move |_, _, cx| {
                                                        state_opac_100.update(cx, |s, cx| {
                                                            s.set_selected_clip_opacity(1.0, cx)
                                                        });
                                                    }),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(4.0))
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .justify_between()
                                            .child(Label::new("Posición").muted())
                                            .child(
                                                div()
                                                    .text_size(px(10.0))
                                                    .text_color(colors.muted_foreground)
                                                    .child(format!("({:+.0}, {:+.0}) px", clip.position_x, clip.position_y)),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(px(4.0))
                                            .child(
                                                Button::new("insp-pos-center", "Centro")
                                                    .variant(if clip.position_x.abs() < 1.0 && clip.position_y.abs() < 1.0 {
                                                        ButtonVariant::Default
                                                    } else {
                                                        ButtonVariant::Outline
                                                    })
                                                    .size(ButtonSize::XSmall)
                                                    .on_click(move |_, _, cx| {
                                                        state_pos_center.update(cx, |s, cx| {
                                                            s.set_selected_clip_position(0.0, 0.0, cx)
                                                        });
                                                    }),
                                            )
                                            .child(
                                                Button::new("insp-pos-tr", "Sup. Der. PiP")
                                                    .variant(if clip.position_x > 1.0 && clip.position_y < -1.0 {
                                                        ButtonVariant::Default
                                                    } else {
                                                        ButtonVariant::Outline
                                                    })
                                                    .size(ButtonSize::XSmall)
                                                    .on_click(move |_, _, cx| {
                                                        state_pos_pip_tr.update(cx, |s, cx| {
                                                            let ox = (proj_w as f32 * 0.25).round();
                                                            let oy = -(proj_h as f32 * 0.25).round();
                                                            s.set_selected_clip_position(ox, oy, cx);
                                                            if let Some(c_id) = s.selected_clip_id() {
                                                                if let Ok((_, c)) = s.project().find_clip(c_id) {
                                                                    if (c.scale - 1.0).abs() < 0.01 {
                                                                        s.set_selected_clip_scale(0.50, cx);
                                                                    }
                                                                }
                                                            }
                                                        });
                                                    }),
                                            )
                                            .child(
                                                Button::new("insp-pos-br", "Inf. Der.")
                                                    .variant(if clip.position_x > 1.0 && clip.position_y > 1.0 {
                                                        ButtonVariant::Default
                                                    } else {
                                                        ButtonVariant::Outline
                                                    })
                                                    .size(ButtonSize::XSmall)
                                                    .on_click(move |_, _, cx| {
                                                        state_pos_pip_br.update(cx, |s, cx| {
                                                            let ox = (proj_w as f32 * 0.25).round();
                                                            let oy = (proj_h as f32 * 0.25).round();
                                                            s.set_selected_clip_position(ox, oy, cx);
                                                            if let Some(c_id) = s.selected_clip_id() {
                                                                if let Ok((_, c)) = s.project().find_clip(c_id) {
                                                                    if (c.scale - 1.0).abs() < 0.01 {
                                                                        s.set_selected_clip_scale(0.50, cx);
                                                                    }
                                                                }
                                                            }
                                                        });
                                                    }),
                                            )
                                            .child(
                                                Button::new("insp-pos-bl", "Inf. Izq.")
                                                    .variant(if clip.position_x < -1.0 && clip.position_y > 1.0 {
                                                        ButtonVariant::Default
                                                    } else {
                                                        ButtonVariant::Outline
                                                    })
                                                    .size(ButtonSize::XSmall)
                                                    .on_click(move |_, _, cx| {
                                                        state_pos_pip_bl.update(cx, |s, cx| {
                                                            let ox = -(proj_w as f32 * 0.25).round();
                                                            let oy = (proj_h as f32 * 0.25).round();
                                                            s.set_selected_clip_position(ox, oy, cx);
                                                            if let Some(c_id) = s.selected_clip_id() {
                                                                if let Ok((_, c)) = s.project().find_clip(c_id) {
                                                                    if (c.scale - 1.0).abs() < 0.01 {
                                                                        s.set_selected_clip_scale(0.50, cx);
                                                                    }
                                                                }
                                                            }
                                                        });
                                                    }),
                                            ),
                                    ),
                            )
                        }),
                )
                // Section 3: Audio & Volumen
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .p(px(6.0))
                        .rounded(px(6.0))
                        .bg(colors.sidebar.opacity(0.25))
                        .border_1()
                        .border_color(colors.border)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .text_size(px(11.0))
                                        .font_weight(gpui::FontWeight::BOLD)
                                        .text_color(colors.foreground)
                                        .child("🔊 Audio & Volumen"),
                                )
                                .child(
                                    Button::new("insp-sec-toggle-audio", if sec_audio_open { "▼" } else { "▶" })
                                        .variant(ButtonVariant::Ghost)
                                        .size(ButtonSize::XSmall)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.section_audio_open = !this.section_audio_open;
                                            cx.notify();
                                        })),
                                ),
                        )
                        .when(sec_audio_open, |this| {
                            this.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        Label::new(format!("Volumen ({:.0}%)", clip.volume * 100.0)).muted(),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(px(4.0))
                                            .child(
                                                Button::new("insp-vol-0", "Silenciar")
                                                    .variant(if clip.volume <= 0.02 {
                                                        ButtonVariant::Default
                                                    } else {
                                                        ButtonVariant::Outline
                                                    })
                                                    .size(ButtonSize::XSmall)
                                                    .on_click(move |_, _, cx| {
                                                        state_vol_0.update(cx, |s, cx| {
                                                            s.set_selected_clip_volume(0.0, cx)
                                                        });
                                                    }),
                                            )
                                            .child(
                                                Button::new("insp-vol-50", "50%")
                                                    .variant(if (clip.volume - 0.5).abs() < 0.05 {
                                                        ButtonVariant::Default
                                                    } else {
                                                        ButtonVariant::Outline
                                                    })
                                                    .size(ButtonSize::XSmall)
                                                    .on_click(move |_, _, cx| {
                                                        state_vol_50.update(cx, |s, cx| {
                                                            s.set_selected_clip_volume(0.5, cx)
                                                        });
                                                    }),
                                            )
                                            .child(
                                                Button::new("insp-vol-100", "100%")
                                                    .variant(if (clip.volume - 1.0).abs() < 0.05 {
                                                        ButtonVariant::Default
                                                    } else {
                                                        ButtonVariant::Outline
                                                    })
                                                    .size(ButtonSize::XSmall)
                                                    .on_click(move |_, _, cx| {
                                                        state_vol_100.update(cx, |s, cx| {
                                                            s.set_selected_clip_volume(1.0, cx)
                                                        });
                                                    }),
                                            ),
                                    ),
                            )
                        }),
                )
                // Section 4: Tiempo & Velocidad
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .p(px(6.0))
                        .rounded(px(6.0))
                        .bg(colors.sidebar.opacity(0.25))
                        .border_1()
                        .border_color(colors.border)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .text_size(px(11.0))
                                        .font_weight(gpui::FontWeight::BOLD)
                                        .text_color(colors.foreground)
                                        .child("⏱ Tiempo & Recorte"),
                                )
                                .child(
                                    Button::new("insp-sec-toggle-speed", if sec_speed_open { "▼" } else { "▶" })
                                        .variant(ButtonVariant::Ghost)
                                        .size(ButtonSize::XSmall)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.section_speed_open = !this.section_speed_open;
                                            cx.notify();
                                        })),
                                ),
                        )
                        .when(sec_speed_open, |this| {
                            this.child(inspector_row(
                                "Línea de Tiempo",
                                format!("{:.2}s – {:.2}s", clip.timeline_in, clip.timeline_out),
                            ))
                            .child(inspector_row(
                                "Recorte Origen",
                                format!("{:.2}s – {:.2}s", clip.in_point, clip.out_point),
                            ))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(Label::new("Desplazar").muted())
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(px(4.0))
                                            .child(
                                                Button::new("insp-pos-zero", "0.0s")
                                                    .variant(ButtonVariant::Outline)
                                                    .size(ButtonSize::XSmall)
                                                    .on_click(move |_, _, cx| {
                                                        state_pos_zero
                                                            .update(cx, |s, cx| s.move_selected_clip(0.0, cx));
                                                    }),
                                            )
                                            .child(
                                                Button::new("insp-pos-back", "-0.5s")
                                                    .variant(ButtonVariant::Outline)
                                                    .size(ButtonSize::XSmall)
                                                    .on_click(move |_, _, cx| {
                                                        state_pos_back.update(cx, |s, cx| {
                                                            s.nudge_selected_clip(-0.5, cx)
                                                        });
                                                    }),
                                            )
                                            .child(
                                                Button::new("insp-pos-fwd", "+0.5s")
                                                    .variant(ButtonVariant::Outline)
                                                    .size(ButtonSize::XSmall)
                                                    .on_click(move |_, _, cx| {
                                                        state_pos_fwd
                                                            .update(cx, |s, cx| s.nudge_selected_clip(0.5, cx));
                                                    }),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(Label::new(format!("Velocidad ({:.1}x)", clip.speed)).muted())
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(px(4.0))
                                            .child(
                                                Button::new("insp-spd-05", "0.5x")
                                                    .variant(if (clip.speed - 0.5).abs() < 0.05 {
                                                        ButtonVariant::Default
                                                    } else {
                                                        ButtonVariant::Outline
                                                    })
                                                    .size(ButtonSize::XSmall)
                                                    .on_click(move |_, _, cx| {
                                                        state_spd_05.update(cx, |s, cx| {
                                                            s.set_selected_clip_speed(0.5, cx)
                                                        });
                                                    }),
                                            )
                                            .child(
                                                Button::new("insp-spd-10", "1x")
                                                    .variant(if (clip.speed - 1.0).abs() < 0.05 {
                                                        ButtonVariant::Default
                                                    } else {
                                                        ButtonVariant::Outline
                                                    })
                                                    .size(ButtonSize::XSmall)
                                                    .on_click(move |_, _, cx| {
                                                        state_spd_10.update(cx, |s, cx| {
                                                            s.set_selected_clip_speed(1.0, cx)
                                                        });
                                                    }),
                                            )
                                            .child(
                                                Button::new("insp-spd-15", "1.5x")
                                                    .variant(if (clip.speed - 1.5).abs() < 0.05 {
                                                        ButtonVariant::Default
                                                    } else {
                                                        ButtonVariant::Outline
                                                    })
                                                    .size(ButtonSize::XSmall)
                                                    .on_click(move |_, _, cx| {
                                                        state_spd_15.update(cx, |s, cx| {
                                                            s.set_selected_clip_speed(1.5, cx)
                                                        });
                                                    }),
                                            )
                                            .child(
                                                Button::new("insp-spd-20", "2x")
                                                    .variant(if (clip.speed - 2.0).abs() < 0.05 {
                                                        ButtonVariant::Default
                                                    } else {
                                                        ButtonVariant::Outline
                                                    })
                                                    .size(ButtonSize::XSmall)
                                                    .on_click(move |_, _, cx| {
                                                        state_spd_20.update(cx, |s, cx| {
                                                            s.set_selected_clip_speed(2.0, cx)
                                                        });
                                                    }),
                                            ),
                                    ),
                            )
                        }),
                )
                .child(Separator::horizontal())
            })
            .child(Label::new("Project Details"))
            .child(inspector_row("Name", project.name.clone()))
            .child(inspector_row("Resolution", resolution_str))
            .child(inspector_row("Frame Rate", fps_str))
            .child(inspector_row("Playhead", playhead_str))
            .child(inspector_row(
                "Export Range [I/O]",
                format!(
                    "{} – {}",
                    format_timecode(state.mark_in().unwrap_or(0.0)),
                    format_timecode(state.mark_out().unwrap_or_else(|| project.duration()))
                ),
            ))
            .child(inspector_row("Duration", duration_str))
            .child(inspector_row("Total Clips", total_clips.to_string()))
            .child(inspector_row("Watched File", path_str));

        div()
            .id("gfcut-inspector-panel")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .flex()
            .flex_col()
            .w_1_4()
            .h_full()
            .p(px(12.0))
            .gap(px(8.0))
            .border_l_1()
            .border_color(colors.border)
            .bg(colors.card)
            .text_color(colors.card_foreground)
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .child(Label::new("Inspector"))
                            .child(
                                Badge::new(format!("v{}", project.version))
                                    .variant(BadgeVariant::Outline),
                            ),
                    )
                    .child(
                        Button::new("inspector-collapse-btn", "❯")
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::XSmall)
                            .on_click(move |_, _, cx| {
                                state_collapse.update(cx, |s, cx| s.toggle_inspector(cx));
                            }),
                    ),
            )
            .child(Separator::horizontal())
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .id("gfcut-inspector-scroll-area")
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .pr(px(4.0))
                            .overflow_y_scroll()
                            .child(scrollable_content),
                    )
                    .child(render_scrollbar_rail(&colors, 64.0)),
            )
            .into_any_element()
    }
}

fn inspector_row(label: &'static str, value: String) -> AnyElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(8.0))
        .child(Label::new(label).muted())
        .child(Label::new(value))
        .into_any_element()
}
