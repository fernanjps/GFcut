use std::{ffi::OsStr, path::PathBuf, sync::Arc};

use gpui::{
    AnyElement, Context, Entity, FocusHandle, FontWeight, KeyDownEvent, ObjectFit, RenderImage,
    SharedString, StyledImage, Window, black, div, hsla, img, prelude::*, px,
};

use crate::{
    components::{
        Badge, BadgeVariant, Button, ButtonSize, ButtonVariant, Label,
        render_scrollbar_rail,
    },
    project_state::{AiBrollModel, ProjectState, WorkspaceMode, format_timecode},
    theme::ActiveTheme,
    thumbnail_service::procedural_thumbnail_render_image,
};
use gfcut_mcp::ffmpeg::AVAILABLE_TTS_VOICES;
use gfcut_timeline::TrackKind;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BrowserTab {
    Medios,
    ModoIa,
    Efectos,
    Textos,
    Transiciones,
}

impl BrowserTab {
    const ALL: [Self; 5] = [
        Self::Medios,
        Self::ModoIa,
        Self::Efectos,
        Self::Textos,
        Self::Transiciones,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Medios => "📁 Medios",
            Self::ModoIa => "🤖 Modo IA",
            Self::Efectos => "✨ Efectos",
            Self::Textos => "T Textos",
            Self::Transiciones => "🔗 Transiciones",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MediaFilter {
    Todos,
    Video,
    Audio,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ModoIaSubTab {
    VideoEscenas,
    VozLocucion,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActiveTextInput {
    None,
    TtsScript,
    BrollPrompt,
}

struct MediaCardItem {
    name: String,
    path: PathBuf,
    duration_secs: f64,
    resolution_badge: String,
    fps_badge: String,
    is_audio: bool,
    has_transcript: bool,
    thumbnail: Arc<RenderImage>,
}

pub(crate) struct Browser {
    focus_handle: FocusHandle,
    project_state: Entity<ProjectState>,
    active_tab: BrowserTab,
    last_workspace_mode: WorkspaceMode,
    media_filter: MediaFilter,
    modo_ia_subtab: ModoIaSubTab,
    active_input: ActiveTextInput,
    tts_script: String,
    selected_voice_id: &'static str,
    broll_prompt: String,
    broll_duration_secs: f64,
    broll_model: AiBrollModel,
}

impl Browser {
    pub(crate) fn new(project_state: Entity<ProjectState>, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        cx.observe(&project_state, |this, state_ent, cx| {
            let mode = state_ent.read(cx).workspace_mode();
            if mode != this.last_workspace_mode {
                this.last_workspace_mode = mode;
                this.active_tab = match mode {
                    WorkspaceMode::Edicion => BrowserTab::Medios,
                    WorkspaceMode::Audio => {
                        this.modo_ia_subtab = ModoIaSubTab::VozLocucion;
                        BrowserTab::ModoIa
                    }
                    WorkspaceMode::Color | WorkspaceMode::Efectos => BrowserTab::Efectos,
                };
            }
            cx.notify();
        })
        .detach();

        Self {
            focus_handle,
            project_state,
            active_tab: BrowserTab::Medios,
            last_workspace_mode: WorkspaceMode::Edicion,
            media_filter: MediaFilter::Todos,
            modo_ia_subtab: ModoIaSubTab::VideoEscenas,
            active_input: ActiveTextInput::None,
            tts_script: "Bienvenidos a GFCut Studio, edición inteligente asistida por IA.".to_owned(),
            selected_voice_id: "alvaro",
            broll_prompt: "Ciudad futurista cyberpunk al atardecer con luces de neón, 4K".to_owned(),
            broll_duration_secs: 4.0,
            broll_model: AiBrollModel::Imagen3,
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.active_input == ActiveTextInput::None {
            return;
        }
        cx.stop_propagation();

        let key = event.keystroke.key.as_str();
        let target = match self.active_input {
            ActiveTextInput::TtsScript => &mut self.tts_script,
            ActiveTextInput::BrollPrompt => &mut self.broll_prompt,
            ActiveTextInput::None => return,
        };

        match key {
            "backspace" => {
                target.pop();
                cx.notify();
            }
            "space" | " " => {
                target.push(' ');
                cx.notify();
            }
            "escape" | "enter" => {
                self.active_input = ActiveTextInput::None;
                cx.notify();
            }
            _ => {
                if let Some(ch) = &event.keystroke.key_char {
                    if !event.keystroke.modifiers.control && !event.keystroke.modifiers.platform {
                        target.push_str(ch);
                        cx.notify();
                    }
                } else if key.chars().count() == 1
                    && !event.keystroke.modifiers.control
                    && !event.keystroke.modifiers.platform
                {
                    target.push_str(key);
                    cx.notify();
                }
            }
        }
    }

    fn collect_media_cards(&self, cx: &Context<Self>) -> Vec<MediaCardItem> {
        let state = self.project_state.read(cx);
        let project = state.project();
        let default_fps = format!("{:.0} FPS", project.fps.as_f64());
        let default_res =
            resolution_badge_label(project.resolution.width, project.resolution.height);

        let mut items: Vec<MediaCardItem> = Vec::new();

        for asset in &project.assets {
            let name = asset
                .path
                .file_name()
                .and_then(OsStr::to_str)
                .unwrap_or("Media")
                .to_owned();

            let is_audio = if is_video_file(&asset.path) || !asset.video_streams.is_empty() {
                false
            } else if is_audio_file(&asset.path) || !asset.audio_streams.is_empty() {
                true
            } else {
                false
            };

            let (res_badge, fps_badge) = if let Some(vs) = asset.video_streams.first() {
                (
                    resolution_badge_label(vs.resolution.width, vs.resolution.height),
                    format!("{:.0} FPS", vs.fps.as_f64()),
                )
            } else if is_audio {
                let sr = asset
                    .audio_streams
                    .first()
                    .map(|a| format!("{}kHz", a.sample_rate / 1000))
                    .unwrap_or_else(|| "WAV HQ".to_owned());
                ("AUDIO".to_owned(), sr)
            } else {
                (default_res.clone(), default_fps.clone())
            };

            let thumbnail = state
                .thumbnail_for_path(&asset.path)
                .unwrap_or_else(|| procedural_thumbnail_render_image(&name, is_audio));

            items.push(MediaCardItem {
                name,
                path: asset.path.clone(),
                duration_secs: asset.duration_secs,
                resolution_badge: res_badge,
                fps_badge,
                is_audio,
                has_transcript: asset.transcript.is_some(),
                thumbnail,
            });
        }

        for track in &project.tracks {
            for clip in &track.clips {
                if clip.source_path.extension().and_then(OsStr::to_str) == Some("preset") {
                    continue;
                }
                if !items.iter().any(|item| item.path == clip.source_path) {
                    let is_audio = if is_video_file(&clip.source_path) {
                        false
                    } else if is_audio_file(&clip.source_path) || track.kind == TrackKind::Audio {
                        true
                    } else {
                        false
                    };
                    let thumbnail = state
                        .thumbnail_for_path(&clip.source_path)
                        .unwrap_or_else(|| {
                            procedural_thumbnail_render_image(&clip.name, is_audio)
                        });

                    items.push(MediaCardItem {
                        name: clip.name.clone(),
                        path: clip.source_path.clone(),
                        duration_secs: clip.source_duration(),
                        resolution_badge: if is_audio {
                            "AUDIO".to_owned()
                        } else {
                            default_res.clone()
                        },
                        fps_badge: if is_audio {
                            "48kHz".to_owned()
                        } else {
                            default_fps.clone()
                        },
                        is_audio,
                        has_transcript: false,
                        thumbnail,
                    });
                }
            }
        }

        items
            .into_iter()
            .filter(|item| match self.media_filter {
                MediaFilter::Todos => true,
                MediaFilter::Video => !item.is_audio,
                MediaFilter::Audio => item.is_audio,
            })
            .collect()
    }

    fn render_api_key_dialog(&self, window: &Window) -> AnyElement {
        let colors = window.theme().colors;
        let state_paste = self.project_state.clone();
        let state_preset_key = self.project_state.clone();
        let state_close = self.project_state.clone();

        div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .p(px(10.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(colors.destructive)
            .bg(hsla(0.02, 0.65, 0.16, 0.95))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(11.5))
                            .font_weight(FontWeight::BOLD)
                            .text_color(colors.foreground)
                            .child("🔑 Configurar GEMINI_API_KEY (Google AI Studio)"),
                    )
                    .child(
                        Button::new("close-api-key-modal", "✕")
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::XSmall)
                            .on_click(move |_, _, cx| {
                                state_close.update(cx, |s, cx| s.dismiss_api_key_modal(cx));
                            }),
                    ),
            )
            .child(
                Label::new(
                    "Pega tu GEMINI_API_KEY desde el portapapeles o activa directamente tu clave de Google AI Studio:",
                )
                .muted(),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(
                        Button::new(
                            "paste-gemini-key-btn",
                            "📋 Pegar GEMINI_API_KEY del Portapapeles",
                        )
                        .variant(ButtonVariant::Default)
                        .size(ButtonSize::XSmall)
                        .on_click(move |_, _, cx| {
                            if let Some(item) = cx.read_from_clipboard() {
                                if let Some(text) = item.text() {
                                    state_paste.update(cx, |s, cx| {
                                        s.set_gemini_api_key(text, cx);
                                    });
                                }
                            }
                        }),
                    )
                    .child(
                        Button::new(
                            "open-full-settings-from-browser-btn",
                            "⚙ Abrir Ajustes de API Key (Validar en Google)",
                        )
                        .variant(ButtonVariant::Secondary)
                        .size(ButtonSize::XSmall)
                        .on_click(move |_, _, cx| {
                            state_preset_key.update(cx, |s, cx| {
                                s.open_settings_modal(cx);
                            });
                        }),
                    ),
            )
            .into_any_element()
    }

    fn render_broll_generator_card(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = window.theme().colors;
        let is_editing = self.active_input == ActiveTextInput::BrollPrompt;
        let prompt_val = self.broll_prompt.clone();
        let dur_val = self.broll_duration_secs;
        let ai_busy = self.project_state.read(cx).ai_task_running();
        let has_key = self.project_state.read(cx).has_gemini_api_key();
        let selected_model = self.broll_model;

        let state_gen = self.project_state.clone();
        let state_toggle_key = self.project_state.clone();
        let focus_for_input = self.focus_handle.clone();

        // Model selector badges (distinct accent per backend).
        let model_accent = |m: AiBrollModel| match m {
            AiBrollModel::Gemini20Flash => hsla(0.62, 0.80, 0.55, 1.0),
            AiBrollModel::Imagen3 => hsla(0.83, 0.70, 0.60, 1.0),
        };
        let model_badges: Vec<AnyElement> = AiBrollModel::ALL
            .into_iter()
            .map(|m| {
                let is_sel = m == selected_model;
                let accent = model_accent(m);
                div()
                    .id(SharedString::from(format!("broll-model-{}", m.model_id())))
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .items_center()
                    .px(px(4.0))
                    .py(px(4.0))
                    .rounded(px(4.0))
                    .border_1()
                    .border_color(if is_sel { accent } else { colors.border })
                    .bg(if is_sel {
                        accent.opacity(0.22)
                    } else {
                        colors.background
                    })
                    .cursor_pointer()
                    .when(!ai_busy, |this| {
                        this.on_click(cx.listener(move |this, _, _, cx| {
                            this.broll_model = m;
                            cx.notify();
                        }))
                    })
                    .child(
                        div()
                            .text_size(px(9.5))
                            .font_weight(if is_sel {
                                FontWeight::BOLD
                            } else {
                                FontWeight::MEDIUM
                            })
                            .text_color(if is_sel { accent } else { colors.muted_foreground })
                            .whitespace_nowrap()
                            .child(m.label()),
                    )
                    .child(
                        div()
                            .text_size(px(8.0))
                            .text_color(colors.muted_foreground)
                            .whitespace_nowrap()
                            .child(m.model_id()),
                    )
                    .into_any_element()
            })
            .collect();

        let busy_banner = match selected_model {
            AiBrollModel::Gemini20Flash => "⏳ Generando guion y subtítulos con Gemini 2.0 Flash...",
            AiBrollModel::Imagen3 => "⏳ Generando fotograma con Imagen 3 y animándolo a MP4...",
        };
        let submit_label = match selected_model {
            AiBrollModel::Gemini20Flash => "✨ Generar Storyboard y Subs en V3",
            AiBrollModel::Imagen3 => "🖼 Generar Frame Imagen 3 → MP4 en V2",
        };

        div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .p(px(8.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(hsla(0.62, 0.78, 0.48, 0.85))
            .bg(colors.card)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(11.0))
                            .font_weight(FontWeight::BOLD)
                            .child("🎬 Generar Escena IA (Google AI Studio)"),
                    )
                    .child(
                        Button::new(
                            "toggle-gemini-key-modal-btn",
                            if has_key {
                                "🔑 API KEY ACTIVA"
                            } else {
                                "🔑 SIN API KEY"
                            },
                        )
                        .variant(if has_key {
                            ButtonVariant::Secondary
                        } else {
                            ButtonVariant::Destructive
                        })
                        .size(ButtonSize::XSmall)
                        .on_click(move |_, _, cx| {
                            state_toggle_key.update(cx, |s, cx| s.toggle_api_key_modal(cx));
                        }),
                    ),
            )
            .child(
                // Model selector: Veo 2 | Imagen 3 | Gemini 2.5 Flash
                div()
                    .flex()
                    .flex_col()
                    .gap(px(3.0))
                    .child(div().flex().gap(px(4.0)).children(model_badges))
                    .child(
                        div()
                            .text_size(px(9.0))
                            .text_color(colors.muted_foreground)
                            .child(selected_model.description()),
                    ),
            )
            .child(
                // Interactive Prompt Box
                div()
                    .id("broll-prompt-input-box")
                    .px(px(8.0))
                    .py(px(5.0))
                    .rounded(px(4.0))
                    .bg(colors.background)
                    .border_1()
                    .border_color(if is_editing {
                        colors.primary
                    } else {
                        colors.border
                    })
                    .cursor_text()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.active_input = ActiveTextInput::BrollPrompt;
                        window.focus(&focus_for_input);
                        cx.notify();
                    }))
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(colors.foreground)
                            .child(if is_editing {
                                format!("{}|", self.broll_prompt)
                            } else {
                                self.broll_prompt.clone()
                            }),
                    ),
            )
            .child(
                // Preset Prompt Chips + Paste Button
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(4.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(3.0))
                            .child(
                                Button::new("broll-preset-cyber", "Cyberpunk")
                                    .variant(ButtonVariant::Outline)
                                    .size(ButtonSize::XSmall)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.broll_prompt =
                                            "Ciudad futurista cyberpunk al atardecer con neón 4K"
                                                .to_owned();
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("broll-preset-drone", "Dron")
                                    .variant(ButtonVariant::Outline)
                                    .size(ButtonSize::XSmall)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.broll_prompt =
                                            "Toma aérea cinematográfica de montañas con niebla"
                                                .to_owned();
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("broll-paste-btn", "📋 Pegar")
                                    .variant(ButtonVariant::Ghost)
                                    .size(ButtonSize::XSmall)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if let Some(clip) = cx.read_from_clipboard() {
                                            if let Some(txt) = clip.text() {
                                                let trimmed = txt.trim();
                                                if !trimmed.is_empty() {
                                                    this.broll_prompt = trimmed.to_owned();
                                                    cx.notify();
                                                }
                                            }
                                        }
                                    })),
                            ),
                    )
                    .child(
                        // Duration pills: 3s / 5s / 8s
                        div()
                            .flex()
                            .items_center()
                            .gap(px(3.0))
                            .children([3.0_f64, 5.0, 8.0].into_iter().map(|d| {
                                let is_sel = (dur_val - d).abs() < 0.1;
                                Button::new(
                                    SharedString::from(format!("broll-dur-{d:.0}")),
                                    format!("{d:.0}s"),
                                )
                                .variant(if is_sel {
                                    ButtonVariant::Default
                                } else {
                                    ButtonVariant::Outline
                                })
                                .size(ButtonSize::XSmall)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.broll_duration_secs = d;
                                    cx.notify();
                                }))
                            })),
                    ),
            )
            .when(ai_busy, |this| {
                this.child(
                    div()
                        .px(px(8.0))
                        .py(px(5.0))
                        .rounded(px(4.0))
                        .bg(hsla(0.11, 0.85, 0.22, 0.92))
                        .border_1()
                        .border_color(hsla(0.11, 0.90, 0.50, 0.95))
                        .text_size(px(10.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(colors.foreground)
                        .child(busy_banner),
                )
            })
            .child(
                Button::new(
                    "generate-ai-broll-submit",
                    if ai_busy { "⏳ Generando..." } else { submit_label },
                )
                .variant(ButtonVariant::Default)
                .size(ButtonSize::Small)
                .disabled(ai_busy)
                .on_click(move |_, _, cx| {
                    let p = prompt_val.clone();
                    state_gen.update(cx, |s, cx| {
                        s.generate_ai_broll_scene(p, dur_val, selected_model, cx);
                    });
                }),
            )
            .into_any_element()
    }

    fn render_tts_generator_card(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = window.theme().colors;
        let is_editing = self.active_input == ActiveTextInput::TtsScript;
        let script_val = self.tts_script.clone();
        let ai_busy = self.project_state.read(cx).ai_task_running();
        let state_tts = self.project_state.clone();
        let state_tts_prev = self.project_state.clone();
        let focus_for_input = self.focus_handle.clone();
        let selected_vid = self.selected_voice_id;

        let voice_pills: Vec<AnyElement> = AVAILABLE_TTS_VOICES
            .iter()
            .map(|v| {
                let is_sel = v.id == selected_vid;
                let vid = v.id;
                Button::new(
                    SharedString::from(format!("tts-voice-{}", v.id)),
                    v.label,
                )
                .variant(if is_sel {
                    ButtonVariant::Default
                } else {
                    ButtonVariant::Outline
                })
                .size(ButtonSize::XSmall)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.selected_voice_id = vid;
                    cx.notify();
                }))
                .into_any_element()
            })
            .collect();

        let voice_desc = AVAILABLE_TTS_VOICES
            .iter()
            .find(|v| v.id == selected_vid)
            .map(|v| v.description)
            .unwrap_or("Voz neuronal de alta definición");

        div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .p(px(8.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(hsla(0.43, 0.78, 0.42, 0.85))
            .bg(colors.card)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(11.0))
                            .font_weight(FontWeight::BOLD)
                            .child("🎙 Locución & Voz IA (Pista A1 + Sub V3)"),
                    )
                    .child(
                        Button::new("tts-paste-clipboard", "📋 Pegar")
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::XSmall)
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(clip) = cx.read_from_clipboard() {
                                    if let Some(txt) = clip.text() {
                                        let trimmed = txt.trim();
                                        if !trimmed.is_empty() {
                                            this.tts_script = trimmed.to_owned();
                                            cx.notify();
                                        }
                                    }
                                }
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(3.0))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(3.0))
                            .children(voice_pills),
                    )
                    .child(
                        div()
                            .text_size(px(8.5))
                            .text_color(colors.muted_foreground)
                            .child(voice_desc),
                    ),
            )
            .child(
                div()
                    .id("tts-script-input-box")
                    .px(px(8.0))
                    .py(px(5.0))
                    .rounded(px(4.0))
                    .bg(colors.background)
                    .border_1()
                    .border_color(if is_editing {
                        colors.primary
                    } else {
                        colors.border
                    })
                    .cursor_text()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.active_input = ActiveTextInput::TtsScript;
                        window.focus(&focus_for_input);
                        cx.notify();
                    }))
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(colors.foreground)
                            .child(if is_editing {
                                format!("{}|", self.tts_script)
                            } else {
                                self.tts_script.clone()
                            }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(4.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(3.0))
                            .child(
                                Button::new("tts-preset-intro", "Intro")
                                    .variant(ButtonVariant::Outline)
                                    .size(ButtonSize::XSmall)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.tts_script =
                                            "Bienvenidos a GFCut Studio, edición inteligente con IA."
                                                .to_owned();
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("tts-preset-outro", "Outro")
                                    .variant(ButtonVariant::Outline)
                                    .size(ButtonSize::XSmall)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.tts_script =
                                            "Suscríbete y activa la campana para más tutoriales."
                                                .to_owned();
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(4.0))
                            .child(
                                Button::new(
                                    "preview-tts-submit",
                                    "▶ Probar Voz",
                                )
                                .variant(ButtonVariant::Secondary)
                                .size(ButtonSize::XSmall)
                                .disabled(ai_busy)
                                .on_click({
                                    let s_prev = script_val.clone();
                                    let v_prev = selected_vid.to_string();
                                    move |_, _, cx| {
                                        let txt = s_prev.clone();
                                        let v_id = v_prev.clone();
                                        state_tts_prev.update(cx, |s, cx| {
                                            s.preview_tts_voice(txt, Some(v_id), cx);
                                        });
                                    }
                                }),
                            )
                            .child(
                                Button::new(
                                    "generate-tts-submit",
                                    if ai_busy {
                                        "🎙 Sintetizando..."
                                    } else {
                                        "📥 Añadir a A1"
                                    },
                                )
                                .variant(ButtonVariant::Default)
                                .size(ButtonSize::XSmall)
                                .disabled(ai_busy)
                                .on_click({
                                    let s_gen = script_val.clone();
                                    let v_gen = selected_vid.to_string();
                                    move |_, _, cx| {
                                        let txt = s_gen.clone();
                                        let v_id = v_gen.clone();
                                        state_tts.update(cx, |s, cx| {
                                            s.generate_tts_voiceover(txt, Some(v_id), cx);
                                        });
                                    }
                                }),
                            ),
                    ),
            )
            .into_any_element()
    }
}

fn is_audio_file(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "wav" | "mp3" | "flac" | "ogg" | "aac" | "m4a" | "wma" | "opus" | "aiff"
            )
        })
}

fn is_video_file(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "mp4" | "mov" | "mkv" | "webm" | "avi" | "m4v" | "ts" | "flv"
            )
        })
}

fn resolution_badge_label(width: u32, height: u32) -> String {
    let max_dim = width.max(height);
    let min_dim = width.min(height);
    if max_dim >= 3840 {
        "4K UHD".to_owned()
    } else if min_dim >= 1440 {
        "1440p".to_owned()
    } else if min_dim >= 1080 {
        "1080p".to_owned()
    } else if min_dim >= 720 {
        "720p".to_owned()
    } else {
        format!("{width}×{height}")
    }
}

fn truncate_card_title(title: &str, max_chars: usize) -> String {
    if title.chars().count() <= max_chars {
        title.to_owned()
    } else {
        let prefix: String = title.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{prefix}…")
    }
}

impl Render for Browser {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.project_state.read(cx).browser_collapsed() {
            return div()
                .id("gfcut-browser-panel-collapsed")
                .w(px(0.0))
                .h_full()
                .overflow_hidden()
                .into_any_element();
        }

        let colors = window.theme().colors;
        let active_tab = self.active_tab;
        let current_filter = self.media_filter;
        let show_api_modal = self.project_state.read(cx).show_api_key_modal();
        let media_cards = self.collect_media_cards(cx);

        // Top 4-Tab Bar: [Medios, Textos, Efectos, Transiciones]
        let tab_buttons: Vec<AnyElement> = BrowserTab::ALL
            .into_iter()
            .map(|tab| {
                let is_active = tab == active_tab;
                let btn_id = SharedString::from(format!("browser-tab-{:?}", tab));
                Button::new(btn_id, tab.label())
                    .variant(if is_active {
                        ButtonVariant::Default
                    } else {
                        ButtonVariant::Ghost
                    })
                    .size(ButtonSize::XSmall)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.active_tab = tab;
                        this.active_input = ActiveTextInput::None;
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect();

        let state_for_import = self.project_state.clone();

        // Filter buttons: [TODOS, VIDEO, AUDIO]
        let filter_specs = [
            (MediaFilter::Todos, "TODOS"),
            (MediaFilter::Video, "VIDEO"),
            (MediaFilter::Audio, "AUDIO"),
        ];
        let filter_buttons: Vec<AnyElement> = filter_specs
            .into_iter()
            .map(|(f, label)| {
                let is_active = f == current_filter;
                Button::new(SharedString::from(format!("media-filter-{label}")), label)
                    .variant(if is_active {
                        ButtonVariant::Default
                    } else {
                        ButtonVariant::Outline
                    })
                    .size(ButtonSize::XSmall)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.media_filter = f;
                        this.active_tab = BrowserTab::Medios;
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect();

        let api_modal_el = show_api_modal.then(|| self.render_api_key_dialog(window));
        let last_err_str = self
            .project_state
            .read(cx)
            .last_error()
            .map(str::to_owned);
        let state_clear_err = self.project_state.clone();

        let text_presets: &[(&'static str, &'static str, &'static str, f64, bool)] = &[
            (
                "SUB_01: \"El futuro es hoy\"",
                "IA SUBTITLE",
                "00:03.50",
                3.5,
                true,
            ),
            ("LowerThird_Cyberpunk", "4K TITLE", "00:04.00", 4.0, true),
            ("Neon_Headline_Opener", "60 FPS", "00:05.00", 5.0, true),
            ("Caption_TikTok_Dynamic", "9:16 SUB", "00:03.00", 3.0, true),
        ];

        let content_area: AnyElement = match active_tab {
            BrowserTab::Medios => {
                let state_import = state_for_import.clone();
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .gap(px(8.0))
                    .child(
                        // Medios Header: Import Button + Filter Strip
                        div()
                            .flex()
                            .flex_none()
                            .flex_col()
                            .gap(px(6.0))
                            .child(
                                Button::new(
                                    "import-media-btn",
                                    "📁 Importar Medios Locales (Videos, Audios, Imágenes)",
                                )
                                .variant(ButtonVariant::Default)
                                .size(ButtonSize::Small)
                                .on_click(move |_, _, cx| {
                                    state_import.update(cx, |state, cx| {
                                        state.prompt_and_import_media(cx);
                                    });
                                }),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(4.0))
                                    .children(filter_buttons),
                            ),
                    )
                    .child(render_medios_grid(
                        &media_cards,
                        self.project_state.clone(),
                        window,
                    ))
                    .into_any_element()
            }
            BrowserTab::ModoIa => {
                let current_subtab = self.modo_ia_subtab;
                let subtab_selector = div()
                    .flex()
                    .items_center()
                    .gap(px(3.0))
                    .p(px(2.0))
                    .rounded(px(6.0))
                    .bg(colors.card)
                    .border_1()
                    .border_color(colors.border)
                    .child(
                        Button::new("modo-ia-tab-video", "🎬 Video & Escenas IA")
                            .variant(if current_subtab == ModoIaSubTab::VideoEscenas {
                                ButtonVariant::Default
                            } else {
                                ButtonVariant::Ghost
                            })
                            .size(ButtonSize::XSmall)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.modo_ia_subtab = ModoIaSubTab::VideoEscenas;
                                this.active_input = ActiveTextInput::None;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("modo-ia-tab-audio", "🎙 Locución & Voz IA")
                            .variant(if current_subtab == ModoIaSubTab::VozLocucion {
                                ButtonVariant::Default
                            } else {
                                ButtonVariant::Ghost
                            })
                            .size(ButtonSize::XSmall)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.modo_ia_subtab = ModoIaSubTab::VozLocucion;
                                this.active_input = ActiveTextInput::None;
                                cx.notify();
                            })),
                    );

                let card_content = match current_subtab {
                    ModoIaSubTab::VideoEscenas => self.render_broll_generator_card(window, cx),
                    ModoIaSubTab::VozLocucion => self.render_tts_generator_card(window, cx),
                };

                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .gap(px(8.0))
                    .child(subtab_selector)
                    .child(card_content)
                    .into_any_element()
            }
            BrowserTab::Efectos => div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .gap(px(8.0))
                .child(
                    div()
                        .p(px(8.0))
                        .rounded(px(6.0))
                        .bg(colors.card)
                        .border_1()
                        .border_color(colors.border)
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(2.0))
                                .child(
                                    div()
                                        .font_weight(FontWeight::BOLD)
                                        .child(Label::new("✨ Efectos de Video & Filtros")),
                                )
                                .child(
                                    Label::new(
                                        "Se componen y fusionan sobre la escena en V2 con opacidad ajustable en el Inspector.",
                                    )
                                    .muted(),
                                ),
                        ),
                )
                .child(render_preset_grid(
                    &[
                        ("B-ROLL_HoloGlitch_FX", "OVERLAY V2", "00:04.00", 4.0, false),
                        ("Anamorphic_Flare_Pro", "LENS FX V2", "00:03.50", 3.5, false),
                        ("LUT_Cinematic_Rec709", "COLOR V2", "00:05.00", 5.0, false),
                        ("Chromatic_Aberration", "GLITCH V2", "00:02.50", 2.5, false),
                    ],
                    self.project_state.clone(),
                    window,
                ))
                .into_any_element(),
            BrowserTab::Textos => div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .gap(px(8.0))
                .child(
                    div()
                        .p(px(8.0))
                        .rounded(px(6.0))
                        .bg(colors.card)
                        .border_1()
                        .border_color(colors.border)
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(2.0))
                                .child(
                                    div()
                                        .font_weight(FontWeight::BOLD)
                                        .child(Label::new("T Títulos & Subtítulos")),
                                )
                                .child(
                                    Label::new(
                                        "Plantillas tipográficas para la pista V3 con edición de contenido en el Inspector.",
                                    )
                                    .muted(),
                                ),
                        ),
                )
                .child(render_preset_grid(
                    text_presets,
                    self.project_state.clone(),
                    window,
                ))
                .into_any_element(),
            BrowserTab::Transiciones => div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .gap(px(8.0))
                .child(
                    div()
                        .p(px(8.0))
                        .rounded(px(6.0))
                        .bg(colors.card)
                        .border_1()
                        .border_color(colors.border)
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(2.0))
                                .child(
                                    div()
                                        .font_weight(FontWeight::BOLD)
                                        .child(Label::new("🔗 Transiciones de Escena")),
                                )
                                .child(
                                    Label::new(
                                        "Cortes dinámicos y transiciones de video entre escenas.",
                                    )
                                    .muted(),
                                ),
                        ),
                )
                .child(render_preset_grid(
                    &[
                        ("Whip_Pan_Cyber", "TRANS V2", "00:01.00", 1.0, false),
                        ("Flash_White_Cut", "IMPACT V2", "00:00.80", 0.8, false),
                        ("Glitch_Morph_IA", "OPTICAL V2", "00:01.50", 1.5, false),
                        ("Cross_Dissolve_Film", "DISSOLVE V2", "00:01.20", 1.2, false),
                    ],
                    self.project_state.clone(),
                    window,
                ))
                .into_any_element(),
        };

        div()
            .id("gfcut-browser-panel")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .flex()
            .flex_col()
            .w_1_4()
            .h_full()
            .p(px(10.0))
            .gap(px(8.0))
            .border_r_1()
            .border_color(colors.sidebar_border)
            .bg(colors.sidebar)
            .text_color(colors.sidebar_foreground)
            .overflow_hidden()
            .child(
                // 1. Studio Top Tab Bar: [Medios, Modo IA, Efectos, Textos, Transiciones]
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .gap(px(3.0))
                    .p(px(3.0))
                    .rounded(px(6.0))
                    .bg(colors.background)
                    .border_1()
                    .border_color(colors.border)
                    .children(tab_buttons),
            )
            .when_some(api_modal_el, |this, modal| this.child(modal))
            .when_some(last_err_str, |this, err| {
                this.child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_none()
                        .max_h(px(70.0))
                        .overflow_hidden()
                        .gap(px(3.0))
                        .p(px(6.0))
                        .rounded(px(5.0))
                        .bg(hsla(0.0, 0.85, 0.25, 0.35))
                        .border_1()
                        .border_color(hsla(0.0, 0.85, 0.55, 0.80))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(Badge::new("Notice / Error").variant(BadgeVariant::Destructive))
                                .child(
                                    Button::new("browser-clear-err-btn", "✕")
                                        .variant(ButtonVariant::Ghost)
                                        .size(ButtonSize::XSmall)
                                        .on_click(move |_, _, cx| {
                                             state_clear_err.update(cx, |s, cx| s.clear_last_error(cx));
                                        }),
                                ),
                        )
                        .child(
                            div()
                                .text_size(px(10.0))
                                .text_color(colors.foreground)
                                .child(err),
                        ),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .id("gfcut-browser-scroll-area")
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .pr(px(4.0))
                            .overflow_y_scroll()
                            .child(content_area),
                    )
                    .child(render_scrollbar_rail(&colors, 60.0)),
            )
            .into_any_element()
    }
}

fn render_medios_grid(
    cards: &[MediaCardItem],
    project_state: Entity<ProjectState>,
    window: &Window,
) -> AnyElement {
    let colors = window.theme().colors;

    if cards.is_empty() {
        return div()
            .flex()
            .flex_col()
            .flex_1()
            .items_center()
            .justify_center()
            .p(px(16.0))
            .child(
                Label::new("Sin medios en este filtro. Pulsa '+ Import Media' o cambia a TODOS.")
                    .muted(),
            )
            .into_any_element();
    }

    let mut rows: Vec<AnyElement> = Vec::new();
    for (row_idx, pair) in cards.chunks(2).enumerate() {
        let mut row_children: Vec<AnyElement> = Vec::with_capacity(2);
        for (col_idx, item) in pair.iter().enumerate() {
            let idx = row_idx * 2 + col_idx;
            row_children.push(render_single_media_card(
                idx,
                item,
                project_state.clone(),
                window,
            ));
        }
        if row_children.len() == 1 {
            row_children.push(div().flex_1().into_any_element());
        }

        rows.push(
            div()
                .flex()
                .flex_none()
                .w_full()
                .gap(px(8.0))
                .children(row_children)
                .into_any_element(),
        );
    }

    div()
        .flex()
        .flex_col()
        .flex_1()
        .w_full()
        .gap(px(8.0))
        .overflow_hidden()
        .border_color(colors.border)
        .children(rows)
        .into_any_element()
}

fn render_single_media_card(
    idx: usize,
    item: &MediaCardItem,
    project_state: Entity<ProjectState>,
    window: &Window,
) -> AnyElement {
    let colors = window.theme().colors;
    let state_add = project_state;
    let path_clone = item.path.clone();

    let add_btn = Button::new(
        SharedString::from(format!("browser-card-add-{idx}")),
        "+ Add",
    )
    .variant(ButtonVariant::Secondary)
    .size(ButtonSize::XSmall)
    .on_click(move |_, _, cx| {
        state_add.update(cx, |s, cx| {
            s.insert_media_at_playhead(path_clone.clone(), cx);
        });
    });

    let duration_badge = format_timecode(item.duration_secs);
    let display_name = truncate_card_title(&item.name, 16);

    let emerald_bg = hsla(0.43, 0.78, 0.22, 0.92);
    let emerald_fg = hsla(0.43, 0.90, 0.78, 1.0);
    let cyan_bg = hsla(0.58, 0.82, 0.32, 0.92);
    let dark_pill_bg = hsla(0.0, 0.0, 0.04, 0.85);

    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .rounded(px(6.0))
        .bg(colors.card)
        .border_1()
        .border_color(colors.border)
        .overflow_hidden()
        .child(
            div()
                .relative()
                .w_full()
                .h(px(74.0))
                .bg(black())
                .overflow_hidden()
                .child(
                    img(item.thumbnail.clone())
                        .size_full()
                        .object_fit(ObjectFit::Cover),
                )
                .child(
                    div()
                        .absolute()
                        .top(px(5.0))
                        .left(px(5.0))
                        .px(px(5.0))
                        .py(px(1.5))
                        .rounded(px(3.0))
                        .bg(emerald_bg)
                        .text_color(emerald_fg)
                        .text_size(px(9.0))
                        .font_weight(FontWeight::BOLD)
                        .child(item.fps_badge.clone()),
                )
                .when(item.has_transcript, |this| {
                    this.child(
                        div()
                            .absolute()
                            .top(px(5.0))
                            .right(px(5.0))
                            .px(px(5.0))
                            .py(px(1.5))
                            .rounded(px(3.0))
                            .bg(hsla(0.76, 0.75, 0.45, 0.92))
                            .text_color(colors.primary_foreground)
                            .text_size(px(9.0))
                            .font_weight(FontWeight::BOLD)
                            .child("IA SUBS"),
                    )
                })
                .child(
                    div()
                        .absolute()
                        .bottom(px(5.0))
                        .left(px(5.0))
                        .px(px(5.0))
                        .py(px(1.5))
                        .rounded(px(3.0))
                        .bg(cyan_bg)
                        .text_color(colors.foreground)
                        .text_size(px(9.0))
                        .font_weight(FontWeight::BOLD)
                        .child(item.resolution_badge.clone()),
                )
                .child(
                    div()
                        .absolute()
                        .bottom(px(5.0))
                        .right(px(5.0))
                        .px(px(5.0))
                        .py(px(1.5))
                        .rounded(px(3.0))
                        .bg(dark_pill_bg)
                        .text_color(colors.foreground)
                        .text_size(px(9.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(duration_badge),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(4.0))
                .px(px(7.0))
                .py(px(5.0))
                .child(
                    div()
                        .text_size(px(11.0))
                        .font_weight(FontWeight::MEDIUM)
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .child(display_name),
                )
                .child(add_btn),
        )
        .into_any_element()
}

fn render_preset_grid(
    presets: &[(&'static str, &'static str, &'static str, f64, bool)],
    project_state: Entity<ProjectState>,
    window: &Window,
) -> AnyElement {
    let colors = window.theme().colors;
    let mut rows: Vec<AnyElement> = Vec::new();

    for (row_idx, pair) in presets.chunks(2).enumerate() {
        let mut row_cards: Vec<AnyElement> = Vec::with_capacity(2);
        for (col_idx, (title, tag, dur_str, dur_secs, is_sub)) in
            pair.iter().copied().enumerate()
        {
            let idx = row_idx * 2 + col_idx;
            let state_add = project_state.clone();
            let thumb = procedural_thumbnail_render_image(title, false);

            let card = div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .rounded(px(6.0))
                .bg(colors.card)
                .border_1()
                .border_color(colors.border)
                .overflow_hidden()
                .child(
                    div()
                        .relative()
                        .w_full()
                        .h(px(64.0))
                        .bg(black())
                        .overflow_hidden()
                        .child(img(thumb).size_full().object_fit(ObjectFit::Cover))
                        .child(
                            div()
                                .absolute()
                                .bottom(px(5.0))
                                .left(px(5.0))
                                .px(px(5.0))
                                .py(px(1.5))
                                .rounded(px(3.0))
                                .bg(hsla(0.75, 0.72, 0.42, 0.90))
                                .text_color(colors.foreground)
                                .text_size(px(9.0))
                                .font_weight(FontWeight::BOLD)
                                .child(tag),
                        )
                        .child(
                            div()
                                .absolute()
                                .bottom(px(5.0))
                                .right(px(5.0))
                                .px(px(5.0))
                                .py(px(1.5))
                                .rounded(px(3.0))
                                .bg(hsla(0.0, 0.0, 0.05, 0.85))
                                .text_color(colors.foreground)
                                .text_size(px(9.5))
                                .child(dur_str),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .px(px(7.0))
                        .py(px(4.0))
                        .child(
                            div()
                                .text_size(px(10.5))
                                .font_weight(FontWeight::MEDIUM)
                                .whitespace_nowrap()
                                .child(truncate_card_title(title, 15)),
                        )
                        .child(
                            Button::new(
                                SharedString::from(format!("preset-add-{tag}-{idx}")),
                                "+ Add",
                            )
                            .variant(ButtonVariant::Secondary)
                            .size(ButtonSize::XSmall)
                            .on_click(move |_, _, cx| {
                                state_add.update(cx, |s, cx| {
                                    s.insert_studio_preset(title, dur_secs, is_sub, cx);
                                });
                            }),
                        ),
                )
                .into_any_element();

            row_cards.push(card);
        }

        rows.push(
            div()
                .flex()
                .flex_none()
                .w_full()
                .gap(px(8.0))
                .children(row_cards)
                .into_any_element(),
        );
    }

    div()
        .flex()
        .flex_col()
        .flex_1()
        .w_full()
        .gap(px(8.0))
        .children(rows)
        .into_any_element()
}
