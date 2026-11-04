---
applyTo: "**/*.{rs,toml,json,md}"
---

# Contexto Técnico del Proyecto — GFCut Studio

**GFCut Studio** es un editor de video no lineal (NLE) profesional, nativo y asistido por Inteligencia Artificial, construido íntegramente en **Rust (Edición 2024)** utilizando **GPUI (`0.2.2`)** para el renderizado de interfaz acelerado por GPU, **FFmpeg / FFprobe / FFplay** para el procesamiento multimedia en tiempo real, y el protocolo **Model Context Protocol (MCP)** para la edición autónoma mediante agentes e integración con **Google AI Studio (Gemini & Imagen 3)**.

---

## 1. Arquitectura del Workspace (`Cargo.toml`)

El repositorio está organizado como un workspace modular de Cargo con tres paquetes principales:

- **`apps/desktop` (`gfcut-desktop`)**: Aplicación gráfica NLE de escritorio basada en GPUI.
  - `src/main.rs`: Punto de entrada GPUI, configuración de ventana y carga de proyecto.
  - `src/shell.rs`: Contenedor maestro de la interfaz, barra superior de estado, conmutadores de paneles, atajos globales y modales (Exportación, Ajustes de API Key, Registro de Actividad).
  - `src/project_state.rs`: Estado reactivo central, reloj de reproducción anclado a `Instant`, pila Undo/Redo (50 niveles), sincronización de audio/video, construcción del plan de exportación FFmpeg (`build_ffmpeg_export_plan`) y despachadores asíncronos de IA.
  - `src/frame_decoder.rs`: Motor de decodificación continua de video (`FramePlaybackEngine`) con `-hwaccel auto`, descarte de fotogramas retrasados (frame dropping), caché LRU en memoria, pre-buffering de cortes y motor de mezcla de audio multipista en tiempo real (`AudioPlaybackEngine`).
  - `src/timeline_edit.rs`: Matemática pura para recorte de bordes (trimming), movimiento de clips entre pistas y ajuste magnético (snapping).
  - `src/thumbnail_service.rs` & `src/waveform_service.rs`: Extracción asíncrona de miniaturas JPEG/BGRA y picos de forma de onda de audio.
  - `src/panels/`:
    - `timeline.rs`: Línea de tiempo interactiva de 6 pistas, regla de tiempo con zoom centrado en el cursor, scroll horizontal continuo, VU Meter maestro y barra de herramientas.
    - `preview.rs`: Monitor de programa con composición en vivo (`V1` base + `V2` PiP/Overlay + `V3` subtítulos) y controles de transporte.
    - `browser.rs`: Explorador de medios locales, generador de escenas B-Roll IA, estudio de locución TTS con preescucha y biblioteca de efectos/transiciones.
    - `inspector.rs`: Inspector colapsable por secciones para transformar escala, posición, opacidad, velocidad (`0.5x`–`2.0x`), volumen y texto de subtítulos.
    - `ai_chat.rs`: Panel lateral "Chat Asistente IA" (`Ctrl+K`) con caja de texto multilínea auto-ajustable y ejecución directa (modo agente) de las 15 herramientas MCP mediante Function Calling de Gemini.

- **`crates/mcp` (`gfcut-mcp`)**: Servidor MCP JSON-RPC 2.0 sobre Stdio y motor multimedia compartido.
  - `src/ffmpeg.rs`: Sondeo de medios (`probe_media`), exportación multicapa con FFmpeg, detección de silencios, transcripción STT (Whisper), síntesis de voz neuronal (`Edge-TTS` / `Windows SAPI5`) y generación de B-Roll IA / procedural.
  - `src/ai.rs`: Configuración de red para IA, timeout extendido (`AI_HTTP_TIMEOUT_SECS = 90s`) y retroceso exponencial ante errores HTTP `503 Service Unavailable` / `429 Too Many Requests`.
  - `src/tools.rs`: Definición de esquemas JSON y ejecución de las 15 herramientas MCP sobre el archivo de proyecto.
  - `src/server.rs` & `src/protocol.rs`: Transporte y tipos del protocolo MCP.

- **`crates/timeline` (`gfcut-timeline`)**: Modelo de dominio puro y serialización sin dependencias de UI.
  - `src/model.rs`: Entidades `Project`, `Track`, `TrackKind`, `Clip`, `MediaAsset`, `Resolution`, `FrameRate`.
  - `src/ops.rs`: Operaciones de edición (`add_clip`, `split_clip`, `ripple_delete_clip`, `move_clip_to_track`, `remove_intervals_ripple`).
  - `src/storage.rs`: Lectura y escritura atómica (`save_to_file_atomic`) mediante archivo temporal y renombrado seguro.

---

## 2. Reglas de Ingeniería y Estándares de Código

### A. Calidad de Compilación (Cero Errores y Cero Warnings)
1. Antes de dar por completada cualquier tarea, verifica siempre en la raíz del proyecto:
   - `cargo check --workspace` (0 errores)
   - `cargo clippy --workspace` (0 advertencias)
   - `cargo test --workspace` (100% de los tests unitarios y de integración aprobados)
2. Mantén la compatibilidad con **Rust 2024**. Evita `unwrap()` en rutas de ejecución de usuario; propaga errores descriptivos en español hacia `state.last_error` y regístralos en `state.log_activity(...)`.
3. Nunca bloquees el hilo principal de GPUI con operaciones de disco pesadas, procesos de FFmpeg o peticiones HTTP. Usa siempre `smol::unblock` o hilos de trabajo en segundo plano.

### B. Jerarquía Estratificada de 6 Pistas Canónicas
Todo proyecto debe preservar el orden estratificado garantizado por `ensure_stratified_tracks`:
1. `V3 — Textos & IA Subs` (`TrackKind::Subtitle`)
2. `V2 — B-Roll & Overlays` (`TrackKind::Video`)
3. `V1 — Video Principal` (`TrackKind::Video`)
4. `A1 — Voz / Diálogos` (`TrackKind::Audio`)
5. `A2 — Música BGM` (`TrackKind::Audio`)
6. `A3 — Efectos SFX` (`TrackKind::Audio`)

### C. Fidelidad de Duración, Reproducción y Mezcla de Audio/Video
1. **Sondeo Preciso (`probe_media`)**:
   - Al importar un archivo de video, extrae la duración real del stream principal (`ffprobe -v error -select_streams v:0 -show_entries stream=duration`). Si difiere de la duración del contenedor, prioriza la duración del stream de video para evitar cortes prematuros o desincronización entre `V1` y `A1`.
2. **Inserción Completa sin Truncamiento**:
   - Al insertar un activo en `V1`, asigna `in_point = 0.0`, `out_point = asset.duration_secs`, `timeline_in = drop_position` y `timeline_out = drop_position + asset.duration_secs`. Nunca apliques límites arbitrarios de duración.
3. **Mezcla de Audio Multipista y Visibilidad de Pistas**:
   - En previsualización (`active_audio_sources_at_playhead`) y en exportación (`build_ffmpeg_export_plan`), una pista solo debe emitir audio si `!track.muted && track.visible && track.volume > 0.001`. Ocultar una pista de video con el icono del ojo (`👁`) debe silenciar también su audio.
   - En la exportación con FFmpeg, utiliza `amix=inputs={N}:duration=first:dropout_transition=0:normalize=0` para garantizar una mezcla lineal auténtica sin caídas ni saltos bruscos de volumen (ducking involuntario) cuando entra o sale una locución de IA.
4. **Multiplicadores de Velocidad Coordinados**:
   - Cuando un clip tenga `speed != 1.0`, coordina siempre el filtro de video (`setpts=(1.0/speed)*PTS`) con el filtro de audio (`atempo=speed`) y pasa `-ss {src_in} -t {src_dur}` a los argumentos de entrada de FFmpeg.

### D. Síntesis de Voz (TTS) y Subtítulos
1. **Codificación UTF-8 Estricta**:
   - Tanto al invocar `edge-tts` como el fallback `System.Speech.Synthesis.SpeechSynthesizer` de Windows (PowerShell), pasa siempre el guion mediante un archivo temporal codificado en **UTF-8** (`-f archivo.txt` o `[System.IO.File]::ReadAllText(..., [System.Text.Encoding]::UTF8)`). Esto evita que tildes, eñes o signos especiales se corrompan en la consola de Windows o provoquen lecturas con fonética inglesa.
2. **Prohibición de Ruido Sintético**:
   - Nunca utilices generadores de ondas senoidales (`aevalsrc`) como fallback de voz. Si todos los motores de síntesis fallan, devuelve un `Err` claro indicando al usuario que revise su conexión o las voces instaladas en el sistema.

### E. Integración con Gemini AI, MCP y Ergonomía de Interfaz
1. **Resiliencia HTTP y Descubrimiento de Modelos**:
   - Utiliza `discover_available_gemini_models` para seleccionar modelos activos de Google AI Studio.
   - Mantén un timeout HTTP de **90 segundos** (`AI_HTTP_TIMEOUT_SECS`) y aplica retroceso exponencial automático (2s y 4s) ante respuestas HTTP `503 (UNAVAILABLE)` o `429`.
2. **Modo Agente en el Chat Asistente (`ai_chat.rs`)**:
   - Cuando el usuario solicite ediciones en lenguaje natural, el modelo debe invocar directamente las herramientas MCP (`timeline_split_clip`, `timeline_ripple_delete`, `tts_generate_speech`, etc.) sobre `project_state` sin pedir confirmación manual por cada paso.
3. **Controles de Entrada y Navegación**:
   - Todos los campos de entrada de texto (`ai_chat.rs`, `browser.rs`, `inspector.rs`) deben soportar pegado directo con `Ctrl+V` / `Cmd+V`.
   - En el timeline, los eventos `.on_drag_move::<TimelineScrollDrag>` deben estar registrados en el contenedor principal del panel para no perder el foco vertical al arrastrar la barra de desplazamiento.
