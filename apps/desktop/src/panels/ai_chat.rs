//! "Chat Asistente" side panel: natural-language control of the editor through Gemini
//! Function Calling in Direct Agent Mode. Each tool call returned by the model is executed
//! immediately on [`ProjectState`] without manual confirmation and confirmed in the conversation history.

use std::time::Duration;

use gpui::{
    Context, Entity, FocusHandle, KeyDownEvent, MouseButton, Window, div, hsla, prelude::*, px,
};
use serde_json::{Value, json};

use crate::{
    components::{Button, ButtonSize, ButtonVariant, Label, Separator, render_scrollbar_rail},
    project_state::{ProjectState, TRACK_NAME_V1_MAIN, format_timecode},
    theme::ActiveTheme,
};
use gfcut_timeline::TrackKind;

/// Maximum model ↔ tool round-trips per user prompt so multi-step trailer/montage edits
/// can chain `timeline_get_state` + splits + ripple deletes + speed + TTS + titles in one turn.
const MAX_TOOL_ROUNDS: usize = 10;

/// Minimum height of the bottom chat input box (1 line).
pub(crate) const INPUT_MIN_HEIGHT_PX: f32 = 38.0;
/// Maximum height of the bottom chat input box (4 lines; scrolls vertically beyond this).
pub(crate) const INPUT_MAX_HEIGHT_PX: f32 = 110.0;
/// Approximate character columns per visual line inside the 340px side panel input box.
pub(crate) const INPUT_WRAP_COLS: usize = 34;

const SYSTEM_PROMPT: &str = "Eres el Agente de Montaje autónomo de GFCut Studio, un editor de video profesional. \
Responde siempre en español y opera en MODO AGENTE DE EJECUCIÓN DIRECTA: \
NUNCA pidas confirmación manual antes de ejecutar herramientas. \
Cuando el usuario envíe una orden de montaje (ej. 'mejora el video tipo trailer', 'hazlo más dinámico', \
'corta y elimina partes lentas', 'agrega narración'): \
1) Llama primero a `timeline_get_state` para conocer la duración y los clips en V1/A1. \
2) Invoca inmediatamente en secuencia las herramientas locales de MCP registradas \
(`timeline_split_clip`, `timeline_ripple_delete`, `timeline_set_clip_speed`, `tts_generate_speech`, \
`timeline_add_title`, `timeline_set_clip_volume`, etc.) aplicando los cambios directamente sobre el proyecto. \
3) Al finalizar las llamadas, resume en 1 o 2 frases las acciones aplicadas. Los tiempos están en segundos.";

/// Wraps `text` into visual lines of at most `max_cols` characters, respecting both
/// explicit `Shift+Enter` newlines (`\n`) and word/character boundaries so no character
/// is ever clipped horizontally outside the input box.
pub(crate) fn wrap_chat_input_lines(text: &str, max_cols: usize) -> Vec<String> {
    let cols = max_cols.max(8);
    if text.is_empty() {
        return vec![String::new()];
    }

    let mut out = Vec::new();
    for raw_line in text.split('\n') {
        if raw_line.is_empty() {
            out.push(String::new());
            continue;
        }

        let mut current = String::new();
        let mut current_len = 0usize;

        for word in raw_line.split(' ') {
            let word_len = word.chars().count();
            if current_len == 0 {
                if word_len <= cols {
                    current.push_str(word);
                    current_len = word_len;
                } else {
                    let mut chunk = String::new();
                    let mut chunk_len = 0usize;
                    for ch in word.chars() {
                        chunk.push(ch);
                        chunk_len += 1;
                        if chunk_len == cols {
                            out.push(std::mem::take(&mut chunk));
                            chunk_len = 0;
                        }
                    }
                    current = chunk;
                    current_len = chunk_len;
                }
            } else if current_len + 1 + word_len <= cols {
                current.push(' ');
                current.push_str(word);
                current_len += 1 + word_len;
            } else {
                out.push(std::mem::take(&mut current));
                if word_len <= cols {
                    current.push_str(word);
                    current_len = word_len;
                } else {
                    let mut chunk = String::new();
                    let mut chunk_len = 0usize;
                    for ch in word.chars() {
                        chunk.push(ch);
                        chunk_len += 1;
                        if chunk_len == cols {
                            out.push(std::mem::take(&mut chunk));
                            chunk_len = 0;
                        }
                    }
                    current = chunk;
                    current_len = chunk_len;
                }
            }
        }
        out.push(current);
    }

    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// Computes the dynamic height of `#ai-chat-input` between `38px` (1 line) and `110px` (4+ lines).
pub(crate) fn compute_input_box_height(text: &str) -> f32 {
    let visual_lines = wrap_chat_input_lines(text, INPUT_WRAP_COLS).len().max(1);
    let extra_lines = visual_lines.saturating_sub(1).min(3) as f32;
    (INPUT_MIN_HEIGHT_PX + extra_lines * 24.0).clamp(INPUT_MIN_HEIGHT_PX, INPUT_MAX_HEIGHT_PX)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChatRole {
    User,
    Assistant,
    Tool,
    Error,
}

#[derive(Clone, Debug)]
pub(crate) struct ChatMessage {
    pub(crate) role: ChatRole,
    pub(crate) text: String,
}

pub(crate) struct AiChat {
    project_state: Entity<ProjectState>,
    focus_handle: FocusHandle,
    input: String,
    messages: Vec<ChatMessage>,
    /// Raw Gemini `contents` history (user / model / functionResponse turns).
    contents: Vec<Value>,
    busy: bool,
    progress_status: Option<String>,
    elapsed_secs: u64,
    model: Option<String>,
}

impl AiChat {
    pub(crate) fn new(project_state: Entity<ProjectState>, cx: &mut Context<Self>) -> Self {
        cx.observe(&project_state, |_, _, cx| cx.notify()).detach();
        Self {
            project_state,
            focus_handle: cx.focus_handle(),
            input: String::new(),
            messages: vec![ChatMessage {
                role: ChatRole::Assistant,
                text: "Hola 👋 Dime qué quieres editar. Ej: \"mejora el video tipo trailer\", \
                       \"corta el clip en el segundo 10\", \"pon el clip a 2x\", \"genera una voz que diga Bienvenidos\"."
                    .to_owned(),
            }],
            contents: Vec::new(),
            busy: false,
            progress_status: None,
            elapsed_secs: 0,
            model: None,
        }
    }

    fn push(&mut self, role: ChatRole, text: impl Into<String>) {
        self.messages.push(ChatMessage { role, text: text.into() });
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        let ctrl = ks.modifiers.control || ks.modifiers.platform;
        match ks.key.as_str() {
            "enter" => {
                if ks.modifiers.shift {
                    self.input.push('\n');
                } else {
                    self.send(cx);
                }
            }
            "backspace" => {
                self.input.pop();
            }
            "escape" => self.input.clear(),
            "space" => self.input.push(' '),
            "v" | "V" if ctrl => {
                if let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) {
                    self.input.push_str(&text);
                }
            }
            _ => {
                if ctrl {
                    return;
                }
                if let Some(ch) = &ks.key_char {
                    self.input.push_str(ch);
                }
            }
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn send(&mut self, cx: &mut Context<Self>) {
        let prompt = self.input.trim().to_owned();
        if prompt.is_empty() || self.busy {
            return;
        }
        self.input.clear();
        self.push(ChatRole::User, prompt.clone());

        let Some(api_key) = self.project_state.read(cx).gemini_api_key() else {
            self.push(
                ChatRole::Error,
                "Falta la GEMINI_API_KEY. Configúrala en ⚙ Ajustes para usar el asistente.",
            );
            cx.notify();
            return;
        };

        self.contents.push(json!({ "role": "user", "parts": [{ "text": prompt }] }));
        self.busy = true;
        self.elapsed_secs = 0;
        self.progress_status = Some(
            "Procesando generación de video... (esperando respuesta de servidor)".to_owned(),
        );
        cx.notify();

        // Reactive 3-second polling ticker updating the status bar while waiting for Gemini / media tools
        cx.spawn(async move |this, cx| {
            let mut elapsed = 0u64;
            loop {
                cx.background_executor()
                    .timer(Duration::from_secs(3))
                    .await;
                elapsed += 3;
                let keep_polling = this
                    .update(cx, |chat, cx| {
                        if !chat.busy {
                            return false;
                        }
                        chat.elapsed_secs = elapsed;
                        let stage = if elapsed >= 9 {
                            "Generando fotogramas con IA..."
                        } else {
                            "Procesando generación de video..."
                        };
                        chat.progress_status = Some(format!(
                            "{stage} (esperando respuesta de servidor • {elapsed}s)"
                        ));
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !keep_polling {
                    break;
                }
            }
        })
        .detach();

        let cached_model = self.model.clone();
        cx.spawn(async move |this, cx| {
            let model = match cached_model {
                Some(m) => m,
                None => {
                    let key = api_key.clone();
                    let m = smol::unblock(move || gfcut_mcp::ai::select_gemini_chat_model(&key)).await;
                    let _ = this.update(cx, |chat, _| chat.model = Some(m.clone()));
                    m
                }
            };

            for round in 0..MAX_TOOL_ROUNDS {
                let Ok(contents) = this.update(cx, |chat, cx| {
                    if round > 0 {
                        chat.progress_status = Some(format!(
                            "Aplicando herramientas de montaje (paso {}/{})...",
                            round + 1,
                            MAX_TOOL_ROUNDS
                        ));
                        cx.notify();
                    }
                    Value::Array(chat.contents.clone())
                }) else {
                    return;
                };
                let key = api_key.clone();
                let mdl = model.clone();
                let response = smol::unblock(move || {
                    gfcut_mcp::ai::gemini_generate_with_tools(
                        &key,
                        &mdl,
                        SYSTEM_PROMPT,
                        &contents,
                        &function_declarations(),
                    )
                })
                .await;

                let content = match response {
                    Ok(c) => c,
                    Err(err) => {
                        let _ = this.update(cx, |chat, cx| {
                            chat.push(ChatRole::Error, format!("Error de Gemini: {err}"));
                            chat.busy = false;
                            chat.progress_status = None;
                            cx.notify();
                        });
                        return;
                    }
                };

                let parts = content.get("parts").and_then(Value::as_array).cloned().unwrap_or_default();
                let calls: Vec<(String, Value)> = parts
                    .iter()
                    .filter_map(|p| p.get("functionCall"))
                    .map(|fc| {
                        (
                            fc.get("name").and_then(Value::as_str).unwrap_or_default().to_owned(),
                            fc.get("args").cloned().unwrap_or_else(|| json!({})),
                        )
                    })
                    .collect();
                let text: String = parts
                    .iter()
                    .filter_map(|p| p.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n");

                let finished = this
                    .update(cx, |chat, cx| {
                        chat.contents.push(json!({ "role": "model", "parts": parts }));
                        if !text.trim().is_empty() {
                            chat.push(ChatRole::Assistant, text.trim().to_owned());
                        }
                        if calls.is_empty() {
                            chat.busy = false;
                            chat.progress_status = None;
                            cx.notify();
                            return true;
                        }
                        let mut responses = Vec::new();
                        for (name, args) in &calls {
                            let result = chat
                                .project_state
                                .update(cx, |state, cx| execute_tool(state, name, args, cx));
                            let ok = result.get("error").is_none();
                            chat.push(
                                ChatRole::Tool,
                                format!(
                                    "{} {name} → {}",
                                    if ok { "✅" } else { "⚠" },
                                    result.get("message").or(result.get("error")).and_then(Value::as_str).unwrap_or("ok")
                                ),
                            );
                            responses.push(json!({
                                "functionResponse": { "name": name, "response": result }
                            }));
                        }
                        chat.contents.push(json!({ "role": "user", "parts": responses }));
                        cx.notify();
                        false
                    })
                    .unwrap_or(true);
                if finished {
                    return;
                }
            }

            let _ = this.update(cx, |chat, cx| {
                chat.push(ChatRole::Error, "Se alcanzó el límite de pasos de herramientas.");
                chat.busy = false;
                chat.progress_status = None;
                cx.notify();
            });
        })
        .detach();
    }
}

/// Gemini `functionDeclarations` exposed to the model (15 editor tools).
pub(crate) fn function_declarations() -> Value {
    let t = |name: &str, desc: &str, props: Value, required: &[&str]| {
        if props.as_object().is_some_and(|o| o.is_empty()) {
            json!({ "name": name, "description": desc })
        } else {
            json!({ "name": name, "description": desc,
                    "parameters": { "type": "object", "properties": props, "required": required } })
        }
    };
    let secs = |d: &str| json!({ "type": "number", "description": d });
    json!([
        t("timeline_get_state", "Devuelve pistas, clips (id, inicio, fin, velocidad), cabezal y clip seleccionado.", json!({}), &[]),
        t("timeline_set_playhead", "Mueve el cabezal de reproducción.", json!({ "seconds": secs("Tiempo en segundos") }), &["seconds"]),
        t("timeline_select_clip_at", "Selecciona el clip de video (V1 por defecto) bajo un tiempo dado.",
          json!({ "seconds": secs("Tiempo en segundos"), "track": { "type": "string", "enum": ["video", "audio"] } }), &["seconds"]),
        t("timeline_split_clip", "Corta el clip en el tiempo indicado (o en el cabezal si se omite).",
          json!({ "at_seconds": secs("Punto de corte en segundos") }), &[]),
        t("timeline_ripple_delete", "Elimina el clip seleccionado (o el que esté en at_seconds) cerrando el hueco.",
          json!({ "at_seconds": secs("Tiempo dentro del clip a borrar") }), &[]),
        t("timeline_delete_clip", "Elimina el clip seleccionado (o el de at_seconds) dejando el hueco.",
          json!({ "at_seconds": secs("Tiempo dentro del clip a borrar") }), &[]),
        t("timeline_set_clip_speed", "Cambia la velocidad del clip seleccionado (0.25 a 4.0).",
          json!({ "speed": secs("Multiplicador, ej. 2.0") }), &["speed"]),
        t("timeline_set_clip_volume", "Cambia el volumen del clip seleccionado (0.0 a 2.0).",
          json!({ "volume": secs("1.0 = 100%") }), &["volume"]),
        t("timeline_nudge_clip", "Desplaza el clip seleccionado en la línea de tiempo.",
          json!({ "delta_seconds": secs("Positivo = derecha") }), &["delta_seconds"]),
        t("tts_generate_speech", "Genera una locución en español y la inserta en A1 en el cabezal.",
          json!({ "text": { "type": "string" },
                  "voice": { "type": "string", "enum": ["alvaro", "elvira", "jorge", "dalia"] } }), &["text"]),
        t("subtitles_generate", "Transcribe el clip seleccionado y crea subtítulos en V3.", json!({}), &[]),
        t("timeline_add_title", "Inserta un título/subtítulo de texto en V3 en el cabezal.",
          json!({ "text": { "type": "string" }, "duration": secs("Duración en segundos") }), &["text"]),
        t("timeline_undo", "Deshace la última acción.", json!({}), &[]),
        t("timeline_redo", "Rehace la última acción deshecha.", json!({}), &[]),
        t("export_video", "Abre el diálogo para exportar el proyecto a MP4.", json!({}), &[]),
    ])
}

fn num(args: &Value, key: &str) -> Option<f64> {
    args.get(key).and_then(Value::as_f64)
}

fn ok(message: String) -> Value {
    json!({ "ok": true, "message": message })
}

fn err(message: &str) -> Value {
    json!({ "error": message })
}

/// Selects the clip under `t` on the preferred track kind (V1 first for video). Returns whether one was found.
fn select_clip_at(state: &mut ProjectState, t: f64, kind: TrackKind, cx: &mut Context<ProjectState>) -> bool {
    let found = {
        let project = state.project();
        let mut tracks: Vec<_> = project.tracks.iter().filter(|tr| tr.kind == kind).collect();
        tracks.sort_by_key(|tr| tr.name != TRACK_NAME_V1_MAIN);
        tracks
            .iter()
            .find_map(|tr| tr.clips.iter().find(|c| t >= c.timeline_in && t < c.timeline_out).map(|c| c.id))
    };
    match found {
        Some(id) => {
            state.select_clip_and_set_playhead(id, t, cx);
            true
        }
        None => false,
    }
}

/// Ensures a clip is selected on `kind` (either already selected, under `playhead_time`, or the first clip on V1).
fn ensure_clip_selected(state: &mut ProjectState, kind: TrackKind, cx: &mut Context<ProjectState>) -> bool {
    if state.selected_clip_id().is_some() {
        return true;
    }
    if select_clip_at(state, state.playhead_time(), kind, cx) {
        return true;
    }
    let first = {
        let project = state.project();
        let mut tracks: Vec<_> = project.tracks.iter().filter(|tr| tr.kind == kind && !tr.locked).collect();
        tracks.sort_by_key(|tr| tr.name != TRACK_NAME_V1_MAIN);
        tracks.iter().find_map(|tr| tr.clips.first().map(|c| (c.id, c.timeline_in)))
    };
    if let Some((id, t_in)) = first {
        state.select_clip_and_set_playhead(id, t_in, cx);
        true
    } else {
        false
    }
}

/// Executes a model-requested tool directly against the live [`ProjectState`].
pub(crate) fn execute_tool(
    state: &mut ProjectState,
    name: &str,
    args: &Value,
    cx: &mut Context<ProjectState>,
) -> Value {
    match name {
        "timeline_get_state" => {
            let project = state.project();
            let tracks: Vec<Value> = project
                .tracks
                .iter()
                .map(|tr| {
                    json!({
                        "name": tr.name,
                        "kind": format!("{:?}", tr.kind),
                        "clips": tr.clips.iter().map(|c| json!({
                            "id": c.id.to_string(), "name": c.name,
                            "start": c.timeline_in, "end": c.timeline_out,
                            "speed": c.speed, "volume": c.volume,
                        })).collect::<Vec<_>>(),
                    })
                })
                .collect();
            json!({
                "ok": true,
                "message": format!("{} pistas, duración {}", tracks.len(), format_timecode(project.duration())),
                "playhead": state.playhead_time(),
                "selected_clip_id": state.selected_clip_id().map(|id| id.to_string()),
                "duration": project.duration(),
                "tracks": tracks,
            })
        }
        "timeline_set_playhead" => match num(args, "seconds") {
            Some(s) => {
                state.set_playhead_time(s.max(0.0), cx);
                ok(format!("Cabezal en {}", format_timecode(s.max(0.0))))
            }
            None => err("Falta 'seconds'"),
        },
        "timeline_select_clip_at" => {
            let Some(s) = num(args, "seconds") else { return err("Falta 'seconds'") };
            let kind = if args.get("track").and_then(Value::as_str) == Some("audio") {
                TrackKind::Audio
            } else {
                TrackKind::Video
            };
            if select_clip_at(state, s, kind, cx) {
                ok(format!("Clip seleccionado en {}", format_timecode(s)))
            } else {
                err("No hay clip en ese tiempo")
            }
        }
        "timeline_split_clip" => {
            let t = num(args, "at_seconds").unwrap_or_else(|| state.playhead_time());
            if !select_clip_at(state, t, TrackKind::Video, cx) && state.selected_clip_id().is_none() {
                return err("No hay clip bajo el punto de corte");
            }
            state.set_playhead_time(t, cx);
            state.split_clip_at_playhead(cx);
            ok(format!("Clip cortado en {}", format_timecode(t)))
        }
        "timeline_ripple_delete" | "timeline_delete_clip" => {
            if let Some(t) = num(args, "at_seconds") {
                if !select_clip_at(state, t, TrackKind::Video, cx) {
                    return err("No hay clip en ese tiempo");
                }
            } else if !ensure_clip_selected(state, TrackKind::Video, cx) {
                return err("No hay clip seleccionado");
            }
            if state.selected_clip_id().is_none() {
                return err("No hay clip seleccionado");
            }
            let ripple = name == "timeline_ripple_delete";
            state.delete_selected_clip(ripple, cx);
            ok(if ripple { "Clip eliminado (ripple)".into() } else { "Clip eliminado".into() })
        }
        "timeline_set_clip_speed" => {
            let Some(sp) = num(args, "speed") else { return err("Falta 'speed'") };
            if !ensure_clip_selected(state, TrackKind::Video, cx) {
                return err("No hay clip seleccionado");
            }
            let sp = sp.clamp(0.25, 4.0);
            state.set_selected_clip_speed(sp, cx);
            ok(format!("Velocidad {sp:.2}x"))
        }
        "timeline_set_clip_volume" => {
            let Some(v) = num(args, "volume") else { return err("Falta 'volume'") };
            if !ensure_clip_selected(state, TrackKind::Video, cx)
                && !ensure_clip_selected(state, TrackKind::Audio, cx)
            {
                return err("No hay clip seleccionado");
            }
            state.set_selected_clip_volume(v.clamp(0.0, 2.0) as f32, cx);
            ok(format!("Volumen {:.0}%", v.clamp(0.0, 2.0) * 100.0))
        }
        "timeline_nudge_clip" => {
            let Some(d) = num(args, "delta_seconds") else { return err("Falta 'delta_seconds'") };
            if !ensure_clip_selected(state, TrackKind::Video, cx) {
                return err("No hay clip seleccionado");
            }
            state.nudge_selected_clip(d, cx);
            ok(format!("Clip desplazado {d:+.2}s"))
        }
        "tts_generate_speech" => {
            let Some(text) = args.get("text").and_then(Value::as_str).filter(|t| !t.trim().is_empty()) else {
                return err("Falta 'text'");
            };
            let voice = args.get("voice").and_then(Value::as_str).map(str::to_owned);
            state.generate_tts_voiceover(text.to_owned(), voice, cx);
            ok("Generando locución en A1 (segundo plano)".into())
        }
        "subtitles_generate" => {
            state.generate_ai_subtitles(cx);
            ok("Generando subtítulos en V3 (segundo plano)".into())
        }
        "timeline_add_title" => {
            let Some(text) = args.get("text").and_then(Value::as_str) else { return err("Falta 'text'") };
            let dur = num(args, "duration").unwrap_or(3.0).clamp(0.5, 60.0);
            let label = format!("SUB_T: \"{}\"", text.replace('"', "'"));
            state.insert_studio_preset(&label, dur, true, cx);
            ok(format!("Título insertado ({dur:.1}s)"))
        }
        "timeline_undo" => {
            state.undo(cx);
            ok("Deshecho".into())
        }
        "timeline_redo" => {
            state.redo(cx);
            ok("Rehecho".into())
        }
        "export_video" => {
            state.prompt_and_export_video(cx);
            ok("Diálogo de exportación abierto".into())
        }
        other => err(&format!("Herramienta desconocida: {other}")),
    }
}

impl Render for AiChat {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = window.theme().colors;
        let (chat_open, ai_task_running, state_status) = {
            let st = self.project_state.read(cx);
            (
                st.ai_chat_open(),
                st.ai_task_running(),
                st.status_message().map(str::to_owned),
            )
        };
        if !chat_open {
            return div().id("gfcut-ai-chat-collapsed").w(px(0.0)).h_full().into_any_element();
        }

        let history = self.messages.iter().enumerate().map(|(i, m)| {
            let (bg, label) = match m.role {
                ChatRole::User => (colors.primary.opacity(0.25), "Tú"),
                ChatRole::Assistant => (colors.sidebar.opacity(0.6), "Asistente"),
                ChatRole::Tool => (hsla(0.43, 0.6, 0.25, 0.45), "Herramienta"),
                ChatRole::Error => (colors.destructive.opacity(0.3), "Error"),
            };
            div()
                .id(("chat-msg", i))
                .flex()
                .flex_col()
                .w_full()
                .min_w_0()
                .gap(px(2.0))
                .p(px(6.0))
                .rounded(px(6.0))
                .bg(bg)
                .when(m.role == ChatRole::User, |d| d.ml(px(16.0)))
                .child(div().text_size(px(9.5)).text_color(colors.muted_foreground).child(label))
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .text_size(px(11.5))
                        .text_color(colors.foreground)
                        .child(m.text.clone()),
                )
        });

        let active_progress: Option<String> = if self.busy {
            self.progress_status.clone().or_else(|| {
                Some("Procesando generación de video... (esperando respuesta de servidor)".to_owned())
            })
        } else if ai_task_running {
            state_status.or_else(|| {
                Some("Procesando generación de video... (esperando respuesta de servidor)".to_owned())
            })
        } else {
            None
        };

        let focus = self.focus_handle.clone();
        let is_focused = focus.is_focused(window);
        let placeholder = self.input.is_empty();
        let display_text = if placeholder {
            if self.busy {
                "Procesando comando…".to_owned()
            } else {
                "Escribe un comando… (Enter enviar, Shift+Enter línea)".to_owned()
            }
        } else {
            format!("{}{}", self.input, if is_focused { "▍" } else { "" })
        };

        let wrapped_lines = wrap_chat_input_lines(&display_text, INPUT_WRAP_COLS);
        let input_box_h = if placeholder {
            INPUT_MIN_HEIGHT_PX
        } else {
            compute_input_box_height(&self.input)
        };

        div()
            .id("gfcut-ai-chat-panel")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .flex()
            .flex_col()
            .w(px(340.0))
            .flex_none()
            .h_full()
            .p(px(10.0))
            .gap(px(8.0))
            .border_l_1()
            .border_color(colors.border)
            .bg(colors.card)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(Label::new("💬 Chat Asistente"))
                    .child(
                        Button::new("ai-chat-close", "✕")
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::XSmall)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.project_state.update(cx, |s, cx| s.toggle_ai_chat(cx));
                            })),
                    ),
            )
            .child(
                div()
                    .text_size(px(9.5))
                    .text_color(colors.muted_foreground)
                    .child(format!(
                        "Modelo: {} · 15 herramientas · Modo Agente",
                        self.model.as_deref().unwrap_or("auto")
                    )),
            )
            .child(Separator::horizontal())
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .child(
                        div()
                            .id("ai-chat-scroll")
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap(px(6.0))
                            .overflow_y_scroll()
                            .children(history),
                    )
                    .child(render_scrollbar_rail(&colors, 64.0)),
            )
            .when_some(active_progress, |panel, status_text| {
                let pct = ((self.elapsed_secs % 30) as f32 / 30.0).clamp(0.18, 0.92);
                panel.child(
                    div()
                        .id("ai-chat-progress-banner")
                        .flex()
                        .flex_col()
                        .w_full()
                        .min_w_0()
                        .gap(px(4.0))
                        .p(px(6.0))
                        .rounded(px(5.0))
                        .border_1()
                        .border_color(colors.primary.opacity(0.45))
                        .bg(colors.primary.opacity(0.12))
                        .child(
                            div()
                                .w_full()
                                .min_w_0()
                                .text_size(px(10.0))
                                .text_color(colors.foreground)
                                .child(format!("⏳ {status_text}")),
                        )
                        .child(
                            div()
                                .w_full()
                                .h(px(3.0))
                                .rounded(px(2.0))
                                .bg(colors.border)
                                .child(
                                    div()
                                        .w(px(290.0 * pct))
                                        .h_full()
                                        .rounded(px(2.0))
                                        .bg(colors.primary),
                                ),
                        ),
                )
            })
            .child(
                div()
                    .flex()
                    .items_end()
                    .w_full()
                    .min_w_0()
                    .gap(px(6.0))
                    .child(
                        div()
                            .id("ai-chat-input")
                            .flex()
                            .flex_col()
                            .flex_1()
                            .w_full()
                            .min_w_0()
                            .min_h(px(INPUT_MIN_HEIGHT_PX))
                            .h(px(input_box_h))
                            .max_h(px(INPUT_MAX_HEIGHT_PX))
                            .overflow_y_scroll()
                            .overflow_x_hidden()
                            .px(px(8.0))
                            .py(px(6.0))
                            .rounded(px(5.0))
                            .border_1()
                            .border_color(if is_focused { colors.primary } else { colors.border })
                            .bg(colors.background)
                            .text_size(px(11.5))
                            .text_color(if placeholder { colors.muted_foreground } else { colors.foreground })
                            .on_mouse_down(MouseButton::Left, move |_, window, _| window.focus(&focus))
                            .children(wrapped_lines.into_iter().map(|line| {
                                div()
                                    .w_full()
                                    .min_w_0()
                                    .line_height(px(18.0))
                                    .child(if line.is_empty() { " ".to_owned() } else { line })
                            })),
                    )
                    .child(
                        Button::new("ai-chat-send", if self.busy { "…" } else { "Enviar" })
                            .variant(ButtonVariant::Default)
                            .size(ButtonSize::Small)
                            .on_click(cx.listener(|this, _, _, cx| this.send(cx))),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        INPUT_MAX_HEIGHT_PX, INPUT_MIN_HEIGHT_PX, INPUT_WRAP_COLS, compute_input_box_height,
        function_declarations, wrap_chat_input_lines,
    };

    #[test]
    fn exposes_fifteen_function_declarations_with_unique_names() {
        let decls = function_declarations();
        let arr = decls.as_array().expect("array");
        assert_eq!(arr.len(), 15);
        let mut names: Vec<&str> = arr.iter().map(|d| d["name"].as_str().unwrap()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 15);
        assert!(names.contains(&"timeline_split_clip"));
        assert!(names.contains(&"timeline_ripple_delete"));
        assert!(names.contains(&"tts_generate_speech"));
    }

    #[test]
    fn input_box_height_grows_from_38px_to_110px_cap() {
        assert_eq!(compute_input_box_height(""), INPUT_MIN_HEIGHT_PX);
        assert_eq!(compute_input_box_height("cortar video"), 38.0);
        // 2 lines via Shift+Enter
        assert_eq!(compute_input_box_height("linea 1\nlinea 2"), 62.0);
        // 3 lines
        assert_eq!(compute_input_box_height("l1\nl2\nl3"), 86.0);
        // 4 or more lines capped at 110px
        assert_eq!(compute_input_box_height("l1\nl2\nl3\nl4"), INPUT_MAX_HEIGHT_PX);
        let long_paragraph = "mejora el video tipo trailer cortando las partes lentas, aplicando ripple delete y generando una locucion epica en espanol para la introduccion del proyecto";
        assert_eq!(compute_input_box_height(long_paragraph), INPUT_MAX_HEIGHT_PX);
    }

    #[test]
    fn wrap_chat_input_lines_prevents_horizontal_overflow_and_preserves_shift_enter_newlines() {
        let text = "mejora el video tipo trailer con cortes dinamicos y narracion\nsegunda linea tras shift enter";
        let lines = wrap_chat_input_lines(text, INPUT_WRAP_COLS);
        assert!(lines.len() >= 3);
        for line in &lines {
            assert!(
                line.chars().count() <= INPUT_WRAP_COLS,
                "line exceeded wrap width: {line:?}"
            );
        }
    }
}
