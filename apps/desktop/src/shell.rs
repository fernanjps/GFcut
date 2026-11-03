use std::path::PathBuf;

use gpui::{
    AnyElement, Context, Entity, FocusHandle, FontWeight, KeyDownEvent, Window, div,
    hsla, prelude::*, px,
};

use crate::{
    components::{Badge, BadgeVariant, Button, ButtonSize, ButtonVariant, Label, Separator},
    panels::{AiChat, Browser, Inspector, Preview, Timeline},
    project_state::{ProjectState, format_timecode},
    theme::ActiveTheme,
};

pub(crate) struct Shell {
    focus_handle: FocusHandle,
    project_state: Entity<ProjectState>,
    browser: Entity<Browser>,
    preview: Entity<Preview>,
    inspector: Entity<Inspector>,
    timeline: Entity<Timeline>,
    ai_chat: Entity<AiChat>,
}

impl Shell {
    pub(crate) fn new(project_path: PathBuf, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        let project_state = cx.new(|cx| ProjectState::new(project_path, cx));

        cx.observe(&project_state, |_, _, cx| {
            cx.notify();
        })
        .detach();

        let browser = cx.new(|cx| Browser::new(project_state.clone(), cx));
        let preview = cx.new(|cx| Preview::new(project_state.clone(), cx));
        let inspector = cx.new(|cx| Inspector::new(project_state.clone(), cx));
        let timeline = cx.new(|cx| Timeline::new(project_state.clone(), cx));
        let ai_chat = cx.new(|cx| AiChat::new(project_state.clone(), cx));

        Self {
            focus_handle,
            project_state,
            browser,
            preview,
            inspector,
            timeline,
            ai_chat,
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let modifiers = &event.keystroke.modifiers;
        let ctrl_or_cmd = modifiers.control || modifiers.platform;

        let is_export_open = self.project_state.read(cx).show_export_modal();
        if is_export_open && key == "escape" {
            self.project_state.update(cx, |s, cx| {
                if !matches!(s.export_status(), crate::project_state::ExportStatus::Exporting { .. }) {
                    s.dismiss_export_modal(cx);
                }
            });
            return;
        }

        let is_settings_open = self.project_state.read(cx).show_settings_modal();
        if is_settings_open {
            match key {
                "escape" => {
                    self.project_state.update(cx, |s, cx| s.dismiss_settings_modal(cx));
                    return;
                }
                "enter" => {
                    self.project_state.update(cx, |s, cx| s.validate_and_save_settings_api_key(cx));
                    return;
                }
                "backspace" => {
                    self.project_state.update(cx, |s, cx| s.settings_key_backspace(cx));
                    return;
                }
                "v" | "V" if ctrl_or_cmd => {
                    if let Some(item) = cx.read_from_clipboard() {
                        if let Some(text) = item.text() {
                            self.project_state.update(cx, |s, cx| {
                                s.set_settings_api_key_input(text, cx);
                            });
                        }
                    }
                    return;
                }
                _ => {
                    if let Some(ch) = &event.keystroke.key_char {
                        if !ctrl_or_cmd {
                            self.project_state.update(cx, |s, cx| {
                                s.settings_key_append(ch, cx);
                            });
                        }
                    }
                    return;
                }
            }
        }

        if ctrl_or_cmd {
            match key {
                "z" | "Z" if modifiers.shift => {
                    self.project_state.update(cx, |state, cx| {
                        state.redo(cx);
                    });
                    return;
                }
                "z" | "Z" => {
                    self.project_state.update(cx, |state, cx| {
                        state.undo(cx);
                    });
                    return;
                }
                "y" | "Y" => {
                    self.project_state.update(cx, |state, cx| {
                        state.redo(cx);
                    });
                    return;
                }
                "+" | "=" => {
                    self.project_state.update(cx, |state, cx| {
                        state.zoom_in_timeline(cx);
                    });
                    return;
                }
                "-" | "_" => {
                    self.project_state.update(cx, |state, cx| {
                        state.zoom_out_timeline(cx);
                    });
                    return;
                }
                "0" => {
                    let viewport_w = f32::from(window.viewport_size().width) - 280.0;
                    self.project_state.update(cx, |state, cx| {
                        state.zoom_fit_timeline(viewport_w.max(400.0), cx);
                    });
                    return;
                }
                "t" | "T" => {
                    self.project_state.update(cx, |state, cx| {
                        state.toggle_timeline(cx);
                    });
                    return;
                }
                "b" | "B" => {
                    self.project_state.update(cx, |state, cx| {
                        state.toggle_browser(cx);
                    });
                    return;
                }
                "k" | "K" => {
                    self.project_state.update(cx, |state, cx| {
                        state.toggle_ai_chat(cx);
                    });
                    return;
                }
                "e" | "E" => {
                    self.project_state.update(cx, |state, cx| {
                        state.prompt_and_export_video(cx);
                    });
                    return;
                }
                _ => {}
            }
        }

        match key {
            "v" | "V" => {
                self.project_state.update(cx, |state, cx| {
                    state.set_active_tool(crate::project_state::TimelineTool::Select, cx);
                });
            }
            "s" | "S" => {
                self.project_state.update(cx, |state, cx| {
                    state.split_clip_at_playhead(cx);
                });
            }
            "i" | "I" => {
                self.project_state.update(cx, |state, cx| {
                    state.set_mark_in_at_playhead(cx);
                });
            }
            "o" | "O" => {
                self.project_state.update(cx, |state, cx| {
                    state.set_mark_out_at_playhead(cx);
                });
            }
            "backspace" | "delete" => {
                self.project_state.update(cx, |state, cx| {
                    let ripple = state.is_ripple_mode();
                    state.delete_selected_clip(ripple, cx);
                });
            }
            " " | "space" => {
                self.project_state.update(cx, |state, cx| {
                    state.toggle_playback(cx);
                });
            }
            "left" => {
                self.project_state.update(cx, |state, cx| {
                    state.step_playhead(-0.5, cx);
                });
            }
            "right" => {
                self.project_state.update(cx, |state, cx| {
                    state.step_playhead(0.5, cx);
                });
            }
            _ => {}
        }
    }

    fn render_settings_modal(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = window.theme().colors;
        let state = self.project_state.read(cx);
        let has_key = state.has_gemini_api_key();
        let validating = state.is_settings_validating();
        let input_val = state.settings_api_key_input().to_owned();
        let val_result = state.settings_validation_result();

        let state_close = self.project_state.clone();
        let state_validate = self.project_state.clone();
        let state_paste = self.project_state.clone();
        let state_clear = self.project_state.clone();
        let state_folder = self.project_state.clone();

        let masked_key = if input_val.len() > 10 {
            format!(
                "{}...{}",
                &input_val[..6.min(input_val.len())],
                &input_val[input_val.len().saturating_sub(4)..]
            )
        } else {
            "••••••••".to_owned()
        };

        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .bg(hsla(0.0, 0.0, 0.0, 0.70))
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .w(px(520.0))
                    .max_w(px(520.0))
                    .gap(px(12.0))
                    .p(px(16.0))
                    .rounded(px(8.0))
                    .bg(colors.card)
                    .border_1()
                    .border_color(colors.border)
                    .child(
                        // Header
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.0))
                                    .child(
                                        div()
                                            .text_size(px(14.0))
                                            .font_weight(FontWeight::BOLD)
                                            .text_color(colors.foreground)
                                            .child("⚙ Ajustes • Google AI Studio & API Keys"),
                                    ),
                            )
                            .child(
                                Button::new("close-settings-modal-x", "✕")
                                    .variant(ButtonVariant::Ghost)
                                    .size(ButtonSize::XSmall)
                                    .on_click(move |_, _, cx| {
                                        state_close.update(cx, |s, cx| s.dismiss_settings_modal(cx));
                                    }),
                            ),
                    )
                    .child(
                        Label::new(
                            "Configura tu GEMINI_API_KEY para habilitar generación de B-Roll (Veo / Imagen 3 / Gemini 2.5 Flash), transcripciones neuronales y visión artificial."
                        )
                        .muted(),
                    )
                    .child(
                        // Git safety badge / callout
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .p(px(8.0))
                            .rounded(px(5.0))
                            .bg(colors.sidebar)
                            .border_1()
                            .border_color(colors.border)
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(colors.muted_foreground)
                                    .child("🔒 Seguro para Git: La clave se guarda exclusivamente en tu archivo local .env (en .gitignore) y nunca se sube al repositorio."),
                            ),
                    )
                    .child(
                        // Current status badge
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(if has_key {
                                Badge::new(format!("✓ GEMINI_API_KEY ACTIVA ({masked_key})"))
                                    .variant(BadgeVariant::Default)
                            } else {
                                Badge::new("⚠ Sin clave activa (Modo offline / fallback)")
                                    .variant(BadgeVariant::Outline)
                            }),
                    )
                    .child(
                        // Input box + actions
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.0))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .p(px(8.0))
                                    .rounded(px(6.0))
                                    .border_1()
                                    .border_color(if input_val.is_empty() {
                                        colors.border
                                    } else {
                                        hsla(0.68, 0.82, 0.52, 1.0)
                                    })
                                    .bg(colors.background)
                                    .child(
                                        div()
                                            .text_size(px(11.5))
                                            .text_color(if input_val.is_empty() {
                                                colors.muted_foreground
                                            } else {
                                                colors.foreground
                                            })
                                            .child(if input_val.is_empty() {
                                                "Escribe o pega aquí tu clave de Google AI Studio (AIzaSy... o AQ....)".to_owned()
                                            } else {
                                                input_val.clone()
                                            }),
                                    )
                                    .child(
                                        Button::new("settings-clear-input-btn", "✕")
                                            .variant(ButtonVariant::Ghost)
                                            .size(ButtonSize::XSmall)
                                            .on_click(move |_, _, cx| {
                                                state_clear.update(cx, |s, cx| {
                                                    s.set_settings_api_key_input(String::new(), cx);
                                                });
                                            }),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .child(
                                        Button::new(
                                            "settings-paste-btn",
                                            "📋 Pegar del Portapapeles",
                                        )
                                        .variant(ButtonVariant::Secondary)
                                        .size(ButtonSize::XSmall)
                                        .on_click(move |_, _, cx| {
                                            if let Some(item) = cx.read_from_clipboard() {
                                                if let Some(text) = item.text() {
                                                    state_paste.update(cx, |s, cx| {
                                                        s.set_settings_api_key_input(text, cx);
                                                    });
                                                }
                                            }
                                        }),
                                    ),
                            ),
                    )
                    .when_some(val_result, |this, res| {
                        match res {
                            Ok(msg) => this.child(
                                div()
                                    .p(px(8.0))
                                    .rounded(px(5.0))
                                    .bg(hsla(0.35, 0.65, 0.15, 0.9))
                                    .child(
                                        div()
                                            .text_size(px(11.5))
                                            .text_color(hsla(0.35, 0.90, 0.85, 1.0))
                                            .child(msg.clone()),
                                    ),
                            ),
                            Err(err) => this.child(
                                div()
                                    .p(px(8.0))
                                    .rounded(px(5.0))
                                    .bg(hsla(0.02, 0.65, 0.15, 0.9))
                                    .child(
                                        div()
                                            .text_size(px(11.5))
                                            .text_color(hsla(0.02, 0.90, 0.85, 1.0))
                                            .child(format!("✕ {err}")),
                                    ),
                            ),
                        }
                    })
                    .child(
                        // Footer action buttons
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .mt(px(4.0))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.0))
                                    .child(
                                        Button::new("settings-cancel-btn", "Cerrar")
                                            .variant(ButtonVariant::Ghost)
                                            .size(ButtonSize::Small)
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.project_state.update(cx, |s, cx| {
                                                    s.dismiss_settings_modal(cx);
                                                });
                                            })),
                                    )
                                    .child(
                                        Button::new("settings-open-media-btn", "📁 Abrir Carpeta de Medios")
                                            .variant(ButtonVariant::Secondary)
                                            .size(ButtonSize::Small)
                                            .on_click(move |_, _, cx| {
                                                state_folder.update(cx, |s, cx| {
                                                    s.open_project_media_folder(cx);
                                                });
                                            }),
                                    ),
                            )
                            .child(
                                Button::new(
                                    "settings-validate-save-btn",
                                    if validating {
                                        "⏳ Validando con Google..."
                                    } else {
                                        "✓ Validar y Guardar en .env"
                                    },
                                )
                                .variant(ButtonVariant::Default)
                                .size(ButtonSize::Small)
                                .disabled(validating)
                                .on_click(move |_, _, cx| {
                                    state_validate.update(cx, |s, cx| {
                                        s.validate_and_save_settings_api_key(cx);
                                    });
                                }),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_activity_log_modal(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = window.theme().colors;
        let state = self.project_state.read(cx);
        let logs: Vec<crate::project_state::LogEntry> = state.activity_logs().iter().cloned().collect();
        let log_count = logs.len();

        let state_close = self.project_state.clone();
        let state_close_footer = self.project_state.clone();
        let state_clear = self.project_state.clone();

        let log_items: Vec<AnyElement> = if logs.is_empty() {
            vec![
                div()
                    .p(px(24.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(colors.muted_foreground)
                    .text_size(px(12.0))
                    .child("No hay eventos registrados aún. Las operaciones de IA, exportación y edición aparecerán aquí en tiempo real.")
                    .into_any_element(),
            ]
        } else {
            logs.into_iter()
                .map(|entry| {
                    let level_badge = match entry.level {
                        crate::project_state::LogLevel::Info => {
                            Badge::new(entry.level.label()).variant(BadgeVariant::Secondary)
                        }
                        crate::project_state::LogLevel::Warn => {
                            Badge::new(entry.level.label()).variant(BadgeVariant::Outline)
                        }
                        crate::project_state::LogLevel::Error => {
                            Badge::new(entry.level.label()).variant(BadgeVariant::Destructive)
                        }
                    };

                    div()
                        .flex()
                        .flex_col()
                        .gap(px(3.0))
                        .p(px(8.0))
                        .rounded(px(4.0))
                        .bg(colors.sidebar)
                        .border_1()
                        .border_color(colors.border)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap(px(8.0))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(6.0))
                                        .child(
                                            div()
                                                .px(px(5.0))
                                                .py(px(1.0))
                                                .rounded(px(3.0))
                                                .bg(colors.muted)
                                                .text_color(colors.muted_foreground)
                                                .text_size(px(9.5))
                                                .font_weight(FontWeight::BOLD)
                                                .child(entry.timestamp),
                                        )
                                        .child(level_badge)
                                        .child(
                                            div()
                                                .text_size(px(11.5))
                                                .font_weight(FontWeight::BOLD)
                                                .text_color(colors.foreground)
                                                .child(entry.title),
                                        ),
                                ),
                        )
                        .when_some(entry.details, |this, details| {
                            this.child(
                                div()
                                    .text_size(px(10.5))
                                    .text_color(colors.muted_foreground)
                                    .child(details),
                            )
                        })
                        .into_any_element()
                })
                .collect()
        };

        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .bg(hsla(0.0, 0.0, 0.0, 0.65))
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .w(px(640.0))
                    .max_w(px(640.0))
                    .h(px(480.0))
                    .max_h(px(480.0))
                    .gap(px(10.0))
                    .p(px(16.0))
                    .rounded(px(8.0))
                    .bg(colors.card)
                    .border_1()
                    .border_color(colors.border)
                    .child(
                        // Header
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.0))
                                    .child(
                                        div()
                                            .text_size(px(14.0))
                                            .font_weight(FontWeight::BOLD)
                                            .text_color(colors.foreground)
                                            .child("📋 Registro de Actividad & Consola del Sistema"),
                                    )
                                    .child(
                                        Badge::new(format!("{log_count} eventos"))
                                            .variant(BadgeVariant::Secondary),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .child(
                                        Button::new("clear-activity-logs-btn", "Limpiar")
                                            .variant(ButtonVariant::Ghost)
                                            .size(ButtonSize::XSmall)
                                            .on_click(move |_, _, cx| {
                                                state_clear.update(cx, |s, cx| s.clear_activity_logs(cx));
                                            }),
                                    )
                                    .child(
                                        Button::new("close-activity-logs-modal-x", "✕")
                                            .variant(ButtonVariant::Ghost)
                                            .size(ButtonSize::XSmall)
                                            .on_click(move |_, _, cx| {
                                                state_close.update(cx, |s, cx| s.dismiss_activity_log(cx));
                                            }),
                                    ),
                            ),
                    )
                    .child(
                        Label::new("Historial de operaciones de IA en la nube, generación de B-Roll, exportación FFmpeg y mutaciones del proyecto.")
                            .muted(),
                    )
                    .child(
                        // Scrollable list container
                        div()
                            .id("activity-logs-scroll-list")
                            .flex()
                            .flex_col()
                            .flex_1()
                            .gap(px(6.0))
                            .overflow_y_scroll()
                            .p(px(4.0))
                            .rounded(px(4.0))
                            .bg(colors.background)
                            .border_1()
                            .border_color(colors.border)
                            .children(log_items),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(px(10.0))
                                    .text_color(colors.muted_foreground)
                                    .child("● Los eventos se almacenan en memoria y documentan rutas y fallbacks de red."),
                            )
                            .child(
                                Button::new("close-activity-logs-footer-btn", "Cerrar")
                                    .variant(ButtonVariant::Secondary)
                                    .size(ButtonSize::Small)
                                    .on_click(move |_, _, cx| {
                                        state_close_footer.update(cx, |s, cx| s.dismiss_activity_log(cx));
                                    }),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_export_modal(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = window.theme().colors;
        let state = self.project_state.read(cx);
        let export_status = state.export_status().clone();

        let card_content = match &export_status {
            crate::project_state::ExportStatus::Idle => {
                div().child("Iniciando exportación...")
            }
            crate::project_state::ExportStatus::Exporting { progress, status_text, output_path } => {
                let prog_val = *progress;
                let pct = (prog_val * 100.0).clamp(0.0, 100.0);
                let out_display = output_path.display().to_string();
                let state_close = self.project_state.clone();

                div()
                    .flex()
                    .flex_col()
                    .gap(px(16.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.0))
                                    .child(
                                        div()
                                            .text_size(px(16.0))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(colors.foreground)
                                            .child("Exportando Proyecto GFCut..."),
                                    )
                                    .child(
                                        Badge::new("FFmpeg").variant(BadgeVariant::Secondary),
                                    ),
                            )
                            .child(
                                Button::new("close-exporting-modal-x", "✕")
                                    .variant(ButtonVariant::Ghost)
                                    .size(ButtonSize::XSmall)
                                    .on_click(move |_, _, cx| {
                                        state_close.update(cx, |s, cx| s.dismiss_export_modal(cx));
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.0))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .text_size(px(13.0))
                                            .text_color(colors.foreground)
                                            .child("Procesando capas V1, V2 y mezcla de audio con FFmpeg..."),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(14.0))
                                            .font_weight(FontWeight::BOLD)
                                            .text_color(colors.primary)
                                            .child(format!("{pct:.0}%")),
                                    ),
                            )
                            .child(
                                div()
                                    .w_full()
                                    .h(px(12.0))
                                    .bg(colors.secondary)
                                    .rounded(px(6.0))
                                    .overflow_hidden()
                                    .child(
                                        div()
                                            .h_full()
                                            .w(gpui::DefiniteLength::Fraction(prog_val.clamp(0.01, 1.0)))
                                            .bg(colors.primary)
                                            .rounded(px(6.0)),
                                    ),
                            )
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(colors.muted_foreground)
                                    .child(status_text.clone()),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .p(px(8.0))
                            .rounded(px(4.0))
                            .bg(colors.sidebar)
                            .border_1()
                            .border_color(colors.border)
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(colors.muted_foreground)
                                    .child(format!("📁 Destino: {out_display}")),
                            ),
                    )
            }
            crate::project_state::ExportStatus::Success { output_path, summary } => {
                let out_display = output_path.display().to_string();
                let path_to_open = output_path.clone();
                let state_close_x = self.project_state.clone();
                let state_close_btn = self.project_state.clone();
                let state_open_vid = self.project_state.clone();

                div()
                    .flex()
                    .flex_col()
                    .gap(px(16.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.0))
                                    .child(
                                        div()
                                            .text_size(px(16.0))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(colors.foreground)
                                            .child("¡Exportación completada con éxito!"),
                                    )
                                    .child(
                                        Badge::new("✓ Listo").variant(BadgeVariant::Default),
                                    ),
                            )
                            .child(
                                Button::new("close-success-modal-x", "✕")
                                    .variant(ButtonVariant::Ghost)
                                    .size(ButtonSize::XSmall)
                                    .on_click(move |_, _, cx| {
                                        state_close_x.update(cx, |s, cx| s.dismiss_export_modal(cx));
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.0))
                            .p(px(12.0))
                            .rounded(px(6.0))
                            .bg(colors.sidebar)
                            .border_1()
                            .border_color(colors.border)
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(colors.foreground)
                                    .child(format!("📁 Archivo generado: {out_display}")),
                            )
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(colors.muted_foreground)
                                    .child(summary.clone()),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_end()
                            .gap(px(8.0))
                            .child(
                                Button::new("open-exported-video-btn", "📁 Abrir Video")
                                    .variant(ButtonVariant::Default)
                                    .size(ButtonSize::Small)
                                    .on_click(move |_, _, cx| {
                                        state_open_vid.update(cx, |s, _| {
                                            s.open_exported_video_file(&path_to_open);
                                        });
                                    }),
                            )
                            .child(
                                Button::new("close-export-success-btn", "Cerrar")
                                    .variant(ButtonVariant::Secondary)
                                    .size(ButtonSize::Small)
                                    .on_click(move |_, _, cx| {
                                        state_close_btn.update(cx, |s, cx| s.dismiss_export_modal(cx));
                                    }),
                            ),
                    )
            }
            crate::project_state::ExportStatus::Failed { output_path, error_message } => {
                let out_display = output_path.display().to_string();
                let state_close_x = self.project_state.clone();
                let state_close_btn = self.project_state.clone();
                let state_retry = self.project_state.clone();

                div()
                    .flex()
                    .flex_col()
                    .gap(px(16.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.0))
                                    .child(
                                        div()
                                            .text_size(px(16.0))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(colors.destructive)
                                            .child("Error en la Exportación"),
                                    )
                                    .child(
                                        Badge::new("Fallo FFmpeg").variant(BadgeVariant::Destructive),
                                    ),
                            )
                            .child(
                                Button::new("close-fail-modal-x", "✕")
                                    .variant(ButtonVariant::Ghost)
                                    .size(ButtonSize::XSmall)
                                    .on_click(move |_, _, cx| {
                                        state_close_x.update(cx, |s, cx| s.dismiss_export_modal(cx));
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.0))
                            .p(px(12.0))
                            .rounded(px(6.0))
                            .bg(colors.sidebar)
                            .border_1()
                            .border_color(colors.destructive)
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(colors.destructive)
                                    .child(format!("Fallo exportando a: {out_display}")),
                            )
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(colors.muted_foreground)
                                    .child(error_message.clone()),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_end()
                            .gap(px(8.0))
                            .child(
                                Button::new("retry-export-btn", "🔄 Reintentar")
                                    .variant(ButtonVariant::Default)
                                    .size(ButtonSize::Small)
                                    .on_click(move |_, _, cx| {
                                        state_retry.update(cx, |s, cx| s.retry_export(cx));
                                    }),
                            )
                            .child(
                                Button::new("close-export-fail-btn", "Cerrar")
                                    .variant(ButtonVariant::Secondary)
                                    .size(ButtonSize::Small)
                                    .on_click(move |_, _, cx| {
                                        state_close_btn.update(cx, |s, cx| s.dismiss_export_modal(cx));
                                    }),
                            ),
                    )
            }
        };

        div()
            .id("export-modal-backdrop")
            .absolute()
            .inset_0()
            .bg(hsla(0.0, 0.0, 0.0, 0.65))
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .id("export-modal-card")
                    .w(px(540.0))
                    .bg(colors.card)
                    .border_1()
                    .border_color(colors.border)
                    .rounded(px(8.0))
                    .p(px(20.0))
                    .child(card_content),
            )
            .into_any_element()
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = window.theme().colors;
        let state = self.project_state.read(cx);
        let project = state.project();
        let show_settings_modal = state.show_settings_modal();
        let show_activity_log = state.show_activity_log();
        let show_export_modal = state.show_export_modal();

        let resolution_label = format!(
            "{}×{}",
            project.resolution.width, project.resolution.height
        );
        let fps_label = format!("{:.2} FPS", project.fps.as_f64());
        let playhead_label = format!(
            "{} / {}",
            format_timecode(state.playhead_time()),
            format_timecode(project.duration())
        );

        let state_clear_notice = self.project_state.clone();
        let sync_badge = if state.last_error().is_some() {
            Button::new("header-notice-btn", "⚠ Notice [✕]")
                .variant(ButtonVariant::Destructive)
                .size(ButtonSize::XSmall)
                .on_click(move |_, _, cx| {
                    state_clear_notice.update(cx, |s, cx| s.clear_last_error(cx));
                })
                .into_any_element()
        } else {
            Badge::new(format!("MCP Live ({})", state.reload_count()))
                .variant(BadgeVariant::Secondary)
                .into_any_element()
        };

        let is_browser_collapsed = state.browser_collapsed();
        let state_toggle_browser = self.project_state.clone();
        let browser_toggle_btn = Button::new(
            "header-browser-toggle-btn",
            if is_browser_collapsed { "📁 Medios" } else { "📁 Medios (Activo)" },
        )
        .variant(if !is_browser_collapsed {
            ButtonVariant::Secondary
        } else {
            ButtonVariant::Ghost
        })
        .size(ButtonSize::XSmall)
        .on_click(move |_, _, cx| {
            state_toggle_browser.update(cx, |s, cx| s.toggle_browser(cx));
        });

        let is_tl_collapsed = state.timeline_collapsed();
        let state_toggle_tl = self.project_state.clone();
        let timeline_toggle_btn = Button::new(
            "header-timeline-toggle-btn",
            if is_tl_collapsed { "🎞 Timeline" } else { "🎞 Timeline (Activa)" },
        )
        .variant(if !is_tl_collapsed {
            ButtonVariant::Secondary
        } else {
            ButtonVariant::Ghost
        })
        .size(ButtonSize::XSmall)
        .on_click(move |_, _, cx| {
            state_toggle_tl.update(cx, |s, cx| s.toggle_timeline(cx));
        });

        let is_insp_collapsed = state.inspector_collapsed();
        let state_toggle_insp = self.project_state.clone();
        let inspector_toggle_btn = Button::new(
            "header-inspector-toggle-btn",
            if is_insp_collapsed { "⚙ Inspector" } else { "⚙ Inspector (Activo)" },
        )
        .variant(if !is_insp_collapsed {
            ButtonVariant::Secondary
        } else {
            ButtonVariant::Ghost
        })
        .size(ButtonSize::XSmall)
        .on_click(move |_, _, cx| {
            state_toggle_insp.update(cx, |s, cx| s.toggle_inspector(cx));
        });

        let is_chat_open = state.ai_chat_open();
        let state_toggle_chat = self.project_state.clone();
        let ai_chat_toggle_btn = Button::new(
            "header-ai-chat-toggle-btn",
            if is_chat_open { "💬 Asistente IA (Activo)" } else { "💬 Asistente IA" },
        )
        .variant(if is_chat_open { ButtonVariant::Secondary } else { ButtonVariant::Ghost })
        .size(ButtonSize::XSmall)
        .on_click(move |_, _, cx| {
            state_toggle_chat.update(cx, |s, cx| s.toggle_ai_chat(cx));
        });

        let can_undo = state.can_undo();
        let can_redo = state.can_redo();
        let undo_label = if can_undo {
            format!("↶ Deshacer ({})", state.undo_count())
        } else {
            "↶ Deshacer".to_owned()
        };
        let redo_label = if can_redo {
            format!("↷ Rehacer ({})", state.redo_count())
        } else {
            "↷ Rehacer".to_owned()
        };

        let state_undo = self.project_state.clone();
        let undo_btn = Button::new("header-undo-btn", undo_label)
            .variant(if can_undo {
                ButtonVariant::Secondary
            } else {
                ButtonVariant::Ghost
            })
            .size(ButtonSize::XSmall)
            .on_click(move |_, _, cx| {
                state_undo.update(cx, |s, cx| s.undo(cx));
            });

        let state_redo = self.project_state.clone();
        let redo_btn = Button::new("header-redo-btn", redo_label)
            .variant(if can_redo {
                ButtonVariant::Secondary
            } else {
                ButtonVariant::Ghost
            })
            .size(ButtonSize::XSmall)
            .on_click(move |_, _, cx| {
                state_redo.update(cx, |s, cx| s.redo(cx));
            });

        let state_export = self.project_state.clone();
        let export_btn = Button::new("header-export-btn", "⚡ Exportar Video")
            .variant(ButtonVariant::Default)
            .size(ButtonSize::Small)
            .on_click(move |_, _, cx| {
                state_export.update(cx, |s, cx| s.prompt_and_export_video(cx));
            });

        let state_settings = self.project_state.clone();
        let settings_btn = Button::new("header-settings-btn", "⚙ Ajustes")
            .variant(if state.show_settings_modal() {
                ButtonVariant::Secondary
            } else {
                ButtonVariant::Ghost
            })
            .size(ButtonSize::XSmall)
            .on_click(move |_, _, cx| {
                state_settings.update(cx, |s, cx| s.toggle_settings_modal(cx));
            });

        let state_log = self.project_state.clone();

        div()
            .id("gfcut-shell")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .flex()
            .flex_col()
            .size_full()
            .bg(colors.background)
            .text_color(colors.foreground)
            .child(
                // Top GFCut Header Bar - Clean, Pro Studio Layout
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .h(px(40.0))
                    .px(px(14.0))
                    .border_b_1()
                    .border_color(colors.border)
                    .bg(colors.card)
                    .child(
                        // Left Session: Brand, Project Title & Undo/Redo
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .px(px(7.0))
                                    .py(px(2.5))
                                    .rounded(px(4.0))
                                    .bg(hsla(0.68, 0.82, 0.52, 1.0))
                                    .text_color(colors.foreground)
                                    .text_size(px(11.0))
                                    .font_weight(FontWeight::BOLD)
                                    .child("🎬 GFCut"),
                            )
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .font_weight(FontWeight::BOLD)
                                    .child(project.name.clone()),
                            )
                            .child(Separator::vertical())
                            .child(undo_btn)
                            .child(redo_btn),
                    )
                    .child(
                        // Center Session: Workspace Panel Visibility Switchers
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .p(px(2.5))
                            .rounded(px(6.0))
                            .bg(colors.background)
                            .border_1()
                            .border_color(colors.border)
                            .child(browser_toggle_btn)
                            .child(timeline_toggle_btn)
                            .child(inspector_toggle_btn)
                            .child(ai_chat_toggle_btn),
                    )
                    .child(
                        // Right Session: HUD Telemetry & Primary Actions
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .child(Badge::new(playhead_label).variant(BadgeVariant::Secondary))
                                    .child(
                                        Badge::new(format!("{} @ {}", resolution_label, fps_label))
                                            .variant(BadgeVariant::Outline),
                                    )
                                    .child(sync_badge),
                            )
                            .child(Separator::vertical())
                            .child(settings_btn)
                            .child(Separator::vertical())
                            .child(export_btn),
                    ),
            )
            .child(
                // Upper Studio Workspace: [Media Browser | Program Preview | Inspector]
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .border_b_1()
                    .border_color(colors.border)
                    .child(self.browser.clone())
                    .child(self.preview.clone())
                    .child(self.inspector.clone())
                    .child(self.ai_chat.clone()),
            )
            .child(
                div()
                    .when(is_tl_collapsed, |d| d.flex_none().h(px(32.0)))
                    .when(!is_tl_collapsed, |d| d.flex_none().h(px(300.0)))
                    .flex()
                    .flex_col()
                    .w_full()
                    .child(self.timeline.clone()),
            )
            .child(
                // Bottom Studio Telemetry Footer Bar
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .h(px(22.0))
                    .px(px(12.0))
                    .border_t_1()
                    .border_color(colors.border)
                    .bg(colors.sidebar)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .child(
                                Label::new(format!(
                                    "{} @ {} • ● FFmpeg + GPU RenderImage Active",
                                    resolution_label, fps_label
                                ))
                                .muted(),
                            )
                            .when_some(
                                state.status_message().map(str::to_owned),
                                |this, status_msg| {
                                    this.child(
                                        Badge::new(status_msg).variant(BadgeVariant::Secondary),
                                    )
                                },
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .child(
                                Button::new(
                                    "bottom-activity-logs-btn",
                                    format!("📋 Registro ({})", state.activity_logs().len()),
                                )
                                .variant(if state.show_activity_log() {
                                    ButtonVariant::Secondary
                                } else {
                                    ButtonVariant::Ghost
                                })
                                .size(ButtonSize::XSmall)
                                .on_click(move |_, _, cx| {
                                    state_log.update(cx, |s, cx| s.toggle_activity_log(cx));
                                }),
                            )
                            .child(
                                Label::new("MCP JSON-RPC Ready • Guardado atómico activo").muted(),
                            ),
                    ),
            )
            .when(show_settings_modal, |this| {
                this.child(self.render_settings_modal(window, cx))
            })
            .when(show_activity_log, |this| {
                this.child(self.render_activity_log_modal(window, cx))
            })
            .when(show_export_modal, |this| {
                this.child(self.render_export_modal(window, cx))
            })
    }
}
