# GFCut Studio 🎬

Un editor de video profesional, libre y de alto rendimiento construido desde cero en **Rust** con **GPUI** y composición multicapa acelerada por **FFmpeg**.

[![License: MIT](https://img.shields.io/badge/license-MIT-green?style=flat)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-brightgreen.svg)](https://www.rust-lang.org/)
[![GPUI](https://img.shields.io/badge/GUI-GPUI-blue.svg)](https://github.com/zed-industries/zed)

---

## 🏗️ Arquitectura del Espacio de Trabajo (Workspace)

El workspace de Cargo está estructurado de manera modular y desacoplada:

```
c:\OpenCut-main/
├── apps/
│   └── desktop/               # gfcut-desktop (Binario GUI de escritorio)
│       └── src/
│           ├── main.rs        # Entrada GPUI, configuración de ventana "GFCut Studio"
│           ├── shell.rs       # Estructura principal, barra superior, atajos y modales
│           ├── project_state.rs # Estado reactivo, reloj en tiempo real, Undo/Redo (50 niveles)
│           ├── frame_decoder.rs # Decodificación en streaming mediante pipes FFmpeg & pre-buffering
│           └── panels/        # Paneles modulares: Timeline, Preview, Browser, Inspector
│
├── crates/
│   ├── mcp/                   # gfcut-mcp (Librería y Servidor MCP / Motor FFmpeg)
│   │   ├── src/ffmpeg.rs      # Exportación multicapa, filtros, TTS local, Google AI Studio
│   │   ├── src/tools.rs       # Catálogo de herramientas MCP para agentes autónomos
│   │   └── src/server.rs      # Protocolo MCP Stdio para orquestación IA
│   │
│   └── timeline/              # gfcut-timeline (Modelo de datos puro y operaciones de montaje)
│       ├── src/model.rs       # Entidades: Project, Track, Clip, Asset, Transcripción
│       ├── src/ops.rs         # Mutaciones: cortes, trims, cross-track moves, ripple delete
│       └── src/storage.rs     # Persistencia JSON atómica con checksums (.gfcut.json)
│
├── assets/
│   ├── videos/                # Activos de video estándar y B-Rolls procedurales generados
│   ├── exports/               # Destino por defecto de renderizados finales MP4
│   └── audio/                 # Pistas de voz y música
└── demo.gfcut.json            # Proyecto de demostración stratified por defecto
```

---

## 🌟 Características Principales

### 1. Jerarquía Estratificada de 6 Pistas Canónicas
GFCut implementa una estructura fija y ordenada de pistas para flujos de trabajo profesionales:
- **`V3` Textos & IA Subs**: Títulos, subtítulos quemados y tarjetas de storyboard.
- **`V2` B-Roll & Overlays**: Planos de inserción, sobreimpresiones y clips procedurales generados por IA.
- **`V1` Video Principal**: Video maestro base y tomas primarias.
- **`A1` Voz / Diálogos**: Pista de diálogo principal, locuciones y síntesis de voz (TTS).
- **`A2` Música BGM**: Banda sonora y música de fondo.
- **`A3` Efectos SFX**: Efectos sonoros y foley.

### 2. Motor de Exportación Multicapa Real
- **Composición Vertical de Video**:
  - Normalización unificada de resolución (e.g. 2K Ultra-Wide `2560x1070` o Full HD `1920x1080`).
  - Concatenación de clips en `V1`.
  - Composición de `V2` (B-Roll) con marcas de tiempo exactas calculadas mediante `enable='between(t, start, end)'` usando el filtro `overlay`.
  - Quemado de subtítulos de `V3` al final de la cadena de video.
- **Fusión y Mezcla de Audio**:
  - Normalización independiente de volumen por clip y pista.
  - Sincronización precisa con `adelay`.
  - Mezcla estéreo unificada mediante `amix=inputs=N:duration=first:dropout_transition=2`.
- Exportación directa a `./assets/exports/gfcut_export.mp4` con registro en el Activity Log.

### 3. Arrastre de Clips entre Pistas (Cross-Track Drag & Drop)
- Arrastra clips interactivamente entre pistas compatibles (`V2 ↔ V1`, `A1 ↔ A2 ↔ A3`).
- Detección visual de la pista de destino y movimiento en tiempo real (`preview_move_clip_cross_track`).
- Registro atómico de instantánea en el historial de Deshacer/Rehacer (Ctrl+Z / Ctrl+Y) al soltar el ratón (`commit_move_clip_cross_track`).

### 4. Recorte de Bordes (Interactive Trim) y Snapping Magnético
- Zonas de agarre interactivas en los extremos de cada clip con cursor bidireccional.
- Ajuste magnético inteligente con tolerancia a cortes cercanos y al cabezal de reproducción.
- Guía vertical continua punteada durante el recorte.

### 5. Integración con Google AI Studio y B-Roll Procedural
- **Detección Dinámica de Modelos**: Consulta `GET /v1beta/models` en vivo para seleccionar automáticamente el modelo Flash más reciente disponible.
- **Resiliencia ante saturación (503 / 429)**: Reintentos automáticos con backoff exponencial.
- **Generación Procedural Local**: Si la nube no dispone de modelos de video (Veo / Imagen), FFmpeg genera un clip MP4 procedural animado directamente en `./assets/videos/` y lo registra en los activos del proyecto (`media_assets`).

### 6. Reloj de Reproducción Anclado a `Instant` y Pre-Buffering
- Reproducción fluida y precisa a 1.0x sin desincronización de audio/video.
- Precalentamiento de cortes entre tomas consecutivas para eliminar tirones en arranques en frío.

---

## 🚀 Compilación y Ejecución

### Prerrequisitos
- **Rust** (versión estable 1.80+)
- **FFmpeg** instalado y disponible en el `PATH` (o configurado vía `GFCUT_FFMPEG_DIR`)

### Comandos de Verificación y Compilación

```powershell
# Comprobar todos los crates del workspace
cargo check --workspace

# Ejecutar la suite completa de pruebas unitarias y de integración
cargo test --workspace

# Compilar el binario de escritorio en modo Release
cargo build --release -p gfcut-desktop

# Ejecutar GFCut Studio con el proyecto de demostración
cargo run -p gfcut-desktop -- demo.gfcut.json
```

---

## ⌨️ Atajos de Teclado Principales

| Atajo | Acción |
| :--- | :--- |
| `Espacio` | Reproducir / Pausar reproducción |
| `S` | Cortar clip seleccionado en la posición del cabezal |
| `V` | Activar herramienta Selección / Cursor |
| `I` | Establecer punto de Entrada (Mark In) |
| `O` | Establecer punto de Salida (Mark Out) |
| `Ctrl + Z` | Deshacer última acción (50 niveles de historial) |
| `Ctrl + Y` / `Ctrl + Shift + Z` | Rehacer última acción deshecha |
| `Supr` / `Retroceso` | Eliminar clip seleccionado |
| `Ctrl + E` | Exportar proyecto a MP4 |
| `Ctrl + L` | Abrir / Cerrar panel de registros de actividad (Logs) |

---

## 📄 Licencia

Este proyecto está distribuido bajo la licencia [MIT](LICENSE).
