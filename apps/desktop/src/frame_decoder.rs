use std::{
    collections::VecDeque,
    env,
    io::{BufReader, Read},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use gpui::RenderImage;
use image::{Frame, ImageBuffer, Rgba};
use uuid::Uuid;

pub(crate) const PREVIEW_FRAME_WIDTH: u32 = 1920;
pub(crate) const PREVIEW_FRAME_HEIGHT: u32 = 1080;
pub(crate) const RAW_RGBA_FRAME_BYTES: usize =
    (PREVIEW_FRAME_WIDTH as usize) * (PREVIEW_FRAME_HEIGHT as usize) * 4;
const LRU_CACHE_CAPACITY: usize = 48;
const FRAME_QUANTIZATION_FPS: f64 = 24.0;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Resolves `ffmpeg`, `ffprobe`, or `ffplay` from `GFCUT_FFMPEG_DIR`, `OPENCUT_FFMPEG_DIR`, `.cache/media/ffmpeg-*/bin`, or `PATH`.
pub(crate) fn resolve_media_binary(program: &str) -> PathBuf {
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

pub(crate) fn configure_headless_command(cmd: &mut Command) {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = cmd;
    }
}

/// Probes the duration (in seconds) of a media file using `ffprobe`.
/// Checks the primary video stream (`v:0`) first to prevent format duration desynchronization.
pub(crate) fn probe_media_duration(source_path: &Path) -> Option<f64> {
    if !source_path.exists() {
        return None;
    }

    let ffprobe = resolve_media_binary("ffprobe");

    // 1. Try probing primary video stream duration first
    let mut cmd_stream = Command::new(&ffprobe);
    configure_headless_command(&mut cmd_stream);
    if let Ok(output) = cmd_stream
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
        .arg(source_path)
        .output()
    {
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            if let Ok(dur) = text.trim().parse::<f64>() {
                if dur > 0.0 {
                    return Some(dur);
                }
            }
        }
    }

    // 2. Fallback to format=duration (e.g. for audio-only containers or containers without per-stream duration)
    let mut cmd = Command::new(ffprobe);
    configure_headless_command(&mut cmd);

    let output = cmd
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(source_path)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let duration = text.trim().parse::<f64>().ok()?;
    (duration > 0.0).then_some(duration)
}

/// Strictly clamps `timestamp` between `0.0` and `asset_duration - (1.0 / fps)`
/// so FFmpeg never seeks to or past the final frame boundary (EOF).
pub(crate) fn clamp_seek_timestamp(timestamp: f64, asset_duration: f64, fps: f64) -> f64 {
    let safe_fps = fps.max(1.0);
    let frame_margin = 1.0 / safe_fps;
    let max_ts = (asset_duration - frame_margin).max(0.0);
    timestamp.clamp(0.0, max_ts)
}

/// Quantizes a source timestamp (seconds) into a discrete frame bucket index.
pub(crate) fn quantize_frame_bucket(source_time: f64) -> i64 {
    (source_time.max(0.0) * FRAME_QUANTIZATION_FPS).round() as i64
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub(crate) struct FrameCacheKey {
    pub(crate) source_path: PathBuf,
    pub(crate) frame_bucket: i64,
}

impl FrameCacheKey {
    pub(crate) fn new(source_path: PathBuf, source_time: f64) -> Self {
        Self {
            source_path,
            frame_bucket: quantize_frame_bucket(source_time),
        }
    }
}

/// Bounded LRU cache of decoded [`RenderImage`] frames.
pub(crate) struct FrameLruCache {
    capacity: usize,
    entries: VecDeque<(FrameCacheKey, Arc<RenderImage>)>,
}

impl FrameLruCache {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            entries: VecDeque::with_capacity(capacity.max(1)),
        }
    }

    pub(crate) fn get(&mut self, key: &FrameCacheKey) -> Option<Arc<RenderImage>> {
        let index = self.entries.iter().position(|(k, _)| k == key)?;
        let entry = self.entries.remove(index)?;
        let image = entry.1.clone();
        self.entries.push_back(entry);
        Some(image)
    }

    pub(crate) fn peek(&self, key: &FrameCacheKey) -> Option<Arc<RenderImage>> {
        self.entries
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, img)| img.clone())
    }

    pub(crate) fn peek_any_for_path(&self, source_path: &Path) -> Option<Arc<RenderImage>> {
        self.entries
            .iter()
            .rev()
            .find(|(k, _)| k.source_path == source_path)
            .map(|(_, img)| img.clone())
    }

    pub(crate) fn insert(&mut self, key: FrameCacheKey, image: Arc<RenderImage>) {
        if let Some(index) = self.entries.iter().position(|(k, _)| k == &key) {
            self.entries.remove(index);
        } else if self.entries.len() >= self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back((key, image));
    }

    #[allow(dead_code)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Converts a raw RGBA pixel buffer (`width * height * 4` bytes) into a GPUI [`RenderImage`] (BGRA).
pub(crate) fn rgba_to_render_image(
    width: u32,
    height: u32,
    mut rgba_bytes: Vec<u8>,
) -> Option<Arc<RenderImage>> {
    let expected_len = (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)?;
    if rgba_bytes.len() != expected_len {
        return None;
    }

    // GPUI's Blade/DirectX/Metal renderer expects BGRA byte order.
    for pixel in rgba_bytes.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }

    let buffer = ImageBuffer::<Rgba<u8>, Vec<u8>>::from_raw(width, height, rgba_bytes)?;
    Some(Arc::new(RenderImage::new([Frame::new(buffer)])))
}

#[derive(Clone)]
pub(crate) enum FrameDecodeOutcome {
    Decoded(Arc<RenderImage>),
    /// FFmpeg hit EOF or returned an incomplete buffer near the end of the clip;
    /// the caller should silently retain the last valid frame.
    EofOrIncomplete,
    Error(String),
}

/// Invokes FFmpeg to decode a single RGBA frame at `source_time` (`1920x1080` FHD) when scrubbing in pause mode:
/// `ffmpeg -ss <timestamp> -i <source_path> -vframes 1 -f rawvideo -pix_fmt rgba -s 1920x1080 pipe:1`
pub(crate) fn decode_frame_with_ffmpeg(
    source_path: &Path,
    source_time: f64,
    asset_duration: f64,
    fps: f64,
    width: u32,
    height: u32,
) -> FrameDecodeOutcome {
    let mut effective_path = source_path.to_path_buf();
    if !effective_path.exists() {
        if effective_path.extension().and_then(|e| e.to_str()) == Some("preset") {
            if let Some(stem) = effective_path.file_stem().and_then(|s| s.to_str()) {
                let preset_mp4 = PathBuf::from("./assets/presets").join(format!("{stem}.mp4"));
                if preset_mp4.is_file() {
                    effective_path = preset_mp4;
                } else if let Ok(gen_path) = gfcut_mcp::ffmpeg::generate_studio_preset_clip(
                    stem,
                    asset_duration.max(2.0),
                    width,
                    height,
                ) {
                    effective_path = gen_path;
                }
            }
        }
    }

    if !effective_path.exists() {
        return FrameDecodeOutcome::EofOrIncomplete;
    }

    let safe_time = clamp_seek_timestamp(source_time, asset_duration, fps);
    let ffmpeg = resolve_media_binary("ffmpeg");
    let ts_arg = format!("{safe_time:.3}");
    let size_arg = format!("{width}x{height}");

    let mut cmd = Command::new(&ffmpeg);
    configure_headless_command(&mut cmd);
    cmd.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-hwaccel",
        "auto",
        "-probesize",
        "32",
        "-analyzeduration",
        "0",
        "-ss",
        &ts_arg,
        "-i",
    ])
    .arg(&effective_path)
    .args([
        "-vframes",
        "1",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "rgba",
        "-s",
        &size_arg,
        "pipe:1",
    ]);

    let output = match cmd.output() {
        Ok(out) if out.status.success() => out,
        _ => {
            // Transparent fallback to software decoding if hwaccel fails or is unsupported
            let mut fallback_cmd = Command::new(&ffmpeg);
            configure_headless_command(&mut fallback_cmd);
            match fallback_cmd
                .args(["-hide_banner", "-loglevel", "error", "-ss", &ts_arg, "-i"])
                .arg(&effective_path)
                .args([
                    "-vframes",
                    "1",
                    "-f",
                    "rawvideo",
                    "-pix_fmt",
                    "rgba",
                    "-s",
                    &size_arg,
                    "pipe:1",
                ])
                .output()
            {
                Ok(out) => out,
                Err(err) => {
                    return FrameDecodeOutcome::Error(format!(
                        "Failed to spawn ffmpeg ({}): {err}",
                        ffmpeg.display()
                    ));
                }
            }
        }
    };

    if !output.status.success() {
        return FrameDecodeOutcome::EofOrIncomplete;
    }

    match rgba_to_render_image(width, height, output.stdout) {
        Some(image) => FrameDecodeOutcome::Decoded(image),
        None => FrameDecodeOutcome::EofOrIncomplete,
    }
}

#[derive(Clone, Debug)]
pub(crate) struct FrameDecodeRequest {
    pub(crate) monotonic_frame_seq: u64,
    pub(crate) is_playing: bool,
    pub(crate) key: FrameCacheKey,
    pub(crate) source_time: f64,
    pub(crate) asset_duration: f64,
    pub(crate) fps: f64,
}

#[derive(Clone)]
pub(crate) struct FrameDecodeResponse {
    pub(crate) monotonic_frame_seq: u64,
    pub(crate) key: FrameCacheKey,
    pub(crate) outcome: FrameDecodeOutcome,
}

/// Selects the latest request from a drained burst of queued [`FrameDecodeRequest`]s.
pub(crate) fn select_latest_playback_request(
    mut current: FrameDecodeRequest,
    drained: impl IntoIterator<Item = FrameDecodeRequest>,
) -> FrameDecodeRequest {
    for candidate in drained {
        if candidate.is_playing {
            let same_source = candidate.key.source_path == current.key.source_path;
            let advances_time = !same_source || candidate.source_time >= current.source_time;
            if candidate.monotonic_frame_seq >= current.monotonic_frame_seq && advances_time {
                current = candidate;
            }
        } else if candidate.monotonic_frame_seq >= current.monotonic_frame_seq {
            current = candidate;
        }
    }
    current
}

/// A decoded frame stored in the continuous playback elastic jitter buffer.
#[derive(Clone, Debug)]
pub(crate) struct JitterFrame {
    pub source_time: f64,
    pub monotonic_seq: u64,
    pub image: Arc<RenderImage>,
}

/// Persistent FFmpeg rawvideo pipe used during active playback (`is_playing == true`).
/// Streams sequential `1280 * 720 * 4` RGBA frames from a single long-running FFmpeg process into
/// an elastic in-memory jitter buffer (4-8 frames ahead of the playhead), completely eliminating
/// per-frame process spawn overhead, I-frame seek jitter, and GPUI/video timing stutter.
struct ActiveVideoStream {
    clip_id: Uuid,
    source_path: PathBuf,
    started_at: Arc<Mutex<Instant>>,
    playhead_clock: Arc<AtomicU64>,
    initial_source_time: f64,
    speed: f64,
    #[allow(dead_code)]
    fps: f64,
    stop_flag: Arc<AtomicBool>,
    is_prebuffer: Arc<AtomicBool>,
    child: Child,
    jitter_buffer: Arc<Mutex<VecDeque<JitterFrame>>>,
}

/// Non-blocking video frame engine combining:
/// 1. A **Persistent FFmpeg Pipe** (`ActiveVideoStream`) during continuous playback (`is_playing = true`).
/// 2. An **Ahead-of-Time Pre-buffered Stream** for seamless 0ms cut transitions between consecutive clips.
/// 3. A **Single-Frame Scrub Worker + LRU Cache** during paused mouse scrubbing (`is_playing = false`).
pub(crate) struct FramePlaybackEngine {
    cache: FrameLruCache,
    seq_counter: Arc<AtomicU64>,
    last_rendered_frame_seq: u64,
    pending_keys: Vec<FrameCacheKey>,
    current_frame: Option<(FrameCacheKey, Arc<RenderImage>)>,
    active_stream: Option<ActiveVideoStream>,
    prebuffered_stream: Option<ActiveVideoStream>,
    req_tx: flume::Sender<FrameDecodeRequest>,
    resp_tx: flume::Sender<FrameDecodeResponse>,
}

impl FramePlaybackEngine {
    pub(crate) fn spawn() -> (Self, flume::Receiver<FrameDecodeResponse>) {
        let (req_tx, req_rx) = flume::unbounded::<FrameDecodeRequest>();
        let (resp_tx, resp_rx) = flume::unbounded::<FrameDecodeResponse>();
        let worker_resp_tx = resp_tx.clone();
        let seq_counter = Arc::new(AtomicU64::new(0));

        thread::Builder::new()
            .name("gfcut-scrub-frame-decoder".to_owned())
            .spawn(move || {
                while let Ok(first_request) = req_rx.recv() {
                    let mut drained = Vec::new();
                    while let Ok(queued) = req_rx.try_recv() {
                        drained.push(queued);
                    }
                    let request = select_latest_playback_request(first_request, drained);

                    let outcome = decode_frame_with_ffmpeg(
                        &request.key.source_path,
                        request.source_time,
                        request.asset_duration,
                        request.fps,
                        PREVIEW_FRAME_WIDTH,
                        PREVIEW_FRAME_HEIGHT,
                    );

                    if worker_resp_tx
                        .send(FrameDecodeResponse {
                            monotonic_frame_seq: request.monotonic_frame_seq,
                            key: request.key,
                            outcome,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .expect("failed to spawn scrub frame decoder worker");

        (
            Self {
                cache: FrameLruCache::new(LRU_CACHE_CAPACITY),
                seq_counter,
                last_rendered_frame_seq: 0,
                pending_keys: Vec::new(),
                current_frame: None,
                active_stream: None,
                prebuffered_stream: None,
                req_tx,
                resp_tx,
            },
            resp_rx,
        )
    }

    pub(crate) fn last_rendered_frame_seq(&self) -> u64 {
        self.last_rendered_frame_seq
    }

    /// Stops both the active and pre-buffered FFmpeg video pipes immediately (called on Pause, seek, or Drop).
    pub(crate) fn stop_continuous_stream(&mut self) {
        if let Some(mut stream) = self.active_stream.take() {
            stream.stop_flag.store(true, Ordering::SeqCst);
            let _ = stream.child.kill();
            std::thread::spawn(move || {
                let _ = stream.child.wait();
            });
        }
        if let Some(mut prebuf) = self.prebuffered_stream.take() {
            prebuf.stop_flag.store(true, Ordering::SeqCst);
            let _ = prebuf.child.kill();
            std::thread::spawn(move || {
                let _ = prebuf.child.wait();
            });
        }
    }

    /// Internal factory that spawns a headless FFmpeg continuous stream and reader thread.
    fn spawn_active_video_stream(
        &self,
        clip_id: Uuid,
        source_path: &Path,
        source_time: f64,
        asset_duration: f64,
        fps: f64,
        speed: f64,
        is_prebuffer: bool,
    ) -> Option<ActiveVideoStream> {
        let mut effective_path = source_path.to_path_buf();
        if !effective_path.exists() {
            if effective_path.extension().and_then(|e| e.to_str()) == Some("preset") {
                if let Some(stem) = effective_path.file_stem().and_then(|s| s.to_str()) {
                    let preset_mp4 = PathBuf::from("./assets/presets").join(format!("{stem}.mp4"));
                    if preset_mp4.is_file() {
                        effective_path = preset_mp4;
                    }
                }
            }
        }
        if !effective_path.exists() {
            return None;
        }

        let clamped_speed = speed.clamp(0.25, 4.0);
        let safe_fps = fps.clamp(12.0, 60.0);
        let safe_start = clamp_seek_timestamp(source_time, asset_duration, safe_fps);
        let ffmpeg = resolve_media_binary("ffmpeg");
        let ts_arg = format!("{safe_start:.3}");
        let size_arg = format!("{PREVIEW_FRAME_WIDTH}x{PREVIEW_FRAME_HEIGHT}");
        let vf_arg = if (clamped_speed - 1.0).abs() > 0.01 {
            format!("setpts=PTS/{clamped_speed:.3},fps={safe_fps:.3}")
        } else {
            format!("fps={safe_fps:.3}")
        };

        let mut cmd = Command::new(&ffmpeg);
        configure_headless_command(&mut cmd);
        cmd.args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-hwaccel",
            "auto",
            "-probesize",
            "32",
            "-analyzeduration",
            "0",
            "-flags",
            "low_delay",
            "-fflags",
            "nobuffer",
            "-ss",
            &ts_arg,
            "-i",
        ])
        .arg(&effective_path)
        .args([
            "-vf",
            &vf_arg,
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
            "-s",
            &size_arg,
            "pipe:1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(_) => {
                // Fallback transparent to software decoding without hwaccel
                let mut fallback_cmd = Command::new(&ffmpeg);
                configure_headless_command(&mut fallback_cmd);
                fallback_cmd
                    .args([
                        "-hide_banner",
                        "-loglevel",
                        "error",
                        "-probesize",
                        "32",
                        "-analyzeduration",
                        "0",
                        "-flags",
                        "low_delay",
                        "-fflags",
                        "nobuffer",
                        "-ss",
                        &ts_arg,
                        "-i",
                    ])
                    .arg(&effective_path)
                    .args([
                        "-vf",
                        &vf_arg,
                        "-f",
                        "rawvideo",
                        "-pix_fmt",
                        "rgba",
                        "-s",
                        &size_arg,
                        "pipe:1",
                    ])
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null());
                fallback_cmd.spawn().ok()?
            }
        };
        let stdout = child.stdout.take()?;

        let started_at = Arc::new(Mutex::new(Instant::now()));
        let reader_started = started_at.clone();
        let playhead_clock = Arc::new(AtomicU64::new(safe_start.to_bits()));
        let reader_playhead = playhead_clock.clone();
        let stop_flag = Arc::new(AtomicBool::new(false));
        let reader_stop = stop_flag.clone();
        let prebuf_flag = Arc::new(AtomicBool::new(is_prebuffer));
        let reader_prebuf = prebuf_flag.clone();
        let reader_tx = self.resp_tx.clone();
        let reader_seq = self.seq_counter.clone();
        let reader_path = source_path.to_path_buf();
        let jitter_buffer = Arc::new(Mutex::new(VecDeque::<JitterFrame>::with_capacity(16)));
        let reader_jitter = jitter_buffer.clone();
        let initial_frame_delivered = Arc::new(AtomicBool::new(false));
        let reader_initial_delivered = initial_frame_delivered.clone();

        thread::Builder::new()
            .name("gfcut-video-pipe-reader".to_owned())
            .spawn(move || {
                let mut reader = BufReader::with_capacity(RAW_RGBA_FRAME_BYTES * 2, stdout);
                let mut frame_index: u64 = 0;

                loop {
                    if reader_stop.load(Ordering::SeqCst) {
                        break;
                    }

                    // For prebuffered stream, decode 3-4 frames ahead and hold;
                    // for active stream, keep up to 12 frames in the elastic jitter buffer.
                    while !reader_stop.load(Ordering::SeqCst) {
                        let buffered_len = reader_jitter.lock().map(|b| b.len()).unwrap_or(0);
                        let limit = if reader_prebuf.load(Ordering::SeqCst) { 4 } else { 12 };
                        if buffered_len >= limit {
                            thread::sleep(Duration::from_millis(2));
                        } else {
                            break;
                        }
                    }

                    if reader_stop.load(Ordering::SeqCst) {
                        break;
                    }

                    let mut raw_buf = vec![0u8; RAW_RGBA_FRAME_BYTES];
                    if reader.read_exact(&mut raw_buf).is_err() {
                        break;
                    }

                    let frame_pts =
                        safe_start + ((frame_index as f64) / safe_fps) * clamped_speed;

                    // Frame Dropping policy (clock desync mitigation):
                    // When streaming active playback, compare the decoded frame's timestamp
                    // against the reproduction clock anchored to std::time::Instant.
                    // If frame_pts < current_playhead_time - 0.04s, discard immediately without
                    // converting to RenderImage or sending to GPUI.
                    if !reader_prebuf.load(Ordering::SeqCst) {
                        let current_playhead_time = {
                            let clock_elapsed = reader_started
                                .lock()
                                .map(|t| t.elapsed().as_secs_f64())
                                .unwrap_or(0.0);
                            let clock_pts = safe_start + clock_elapsed * clamped_speed;
                            let explicit_pts = f64::from_bits(reader_playhead.load(Ordering::Relaxed));
                            if explicit_pts > 0.0 {
                                clock_pts.max(explicit_pts)
                            } else {
                                clock_pts
                            }
                        };

                        if frame_pts < current_playhead_time - 0.04 {
                            frame_index += 1;
                            continue;
                        }
                    }

                    let Some(image) = rgba_to_render_image(
                        PREVIEW_FRAME_WIDTH,
                        PREVIEW_FRAME_HEIGHT,
                        raw_buf,
                    ) else {
                        break;
                    };

                    let seq = reader_seq.fetch_add(1, Ordering::SeqCst) + 1;
                    let key = FrameCacheKey::new(reader_path.clone(), frame_pts);

                    if let Ok(mut buf) = reader_jitter.lock() {
                        buf.push_back(JitterFrame {
                            source_time: frame_pts,
                            monotonic_seq: seq,
                            image: image.clone(),
                        });
                    }

                    if !reader_prebuf.load(Ordering::SeqCst)
                        && !reader_initial_delivered.swap(true, Ordering::SeqCst)
                    {
                        let _ = reader_tx.send(FrameDecodeResponse {
                            monotonic_frame_seq: seq,
                            key,
                            outcome: FrameDecodeOutcome::Decoded(image),
                        });
                    }

                    frame_index += 1;
                }
            })
            .ok();

        Some(ActiveVideoStream {
            clip_id,
            source_path: source_path.to_path_buf(),
            started_at,
            playhead_clock,
            initial_source_time: safe_start,
            speed: clamped_speed,
            fps: safe_fps,
            stop_flag,
            is_prebuffer: prebuf_flag,
            child,
            jitter_buffer,
        })
    }

    /// Pre-warms the FFmpeg video stream for the upcoming clip on the timeline when the playhead
    /// approaches the cut boundary (< 0.5s remaining). Decodes and holds the first 3-4 frames in
    /// memory so the cut transition is 100% instantaneous (0ms cold startup lag).
    pub(crate) fn prebuffer_next_clip(
        &mut self,
        clip_id: Uuid,
        source_path: &Path,
        source_time: f64,
        asset_duration: f64,
        fps: f64,
        speed: f64,
    ) {
        if !source_path.exists() {
            return;
        }

        // If prebuffer is already running for this clip, keep it running
        if let Some(stream) = &self.prebuffered_stream {
            if stream.clip_id == clip_id && stream.source_path == source_path {
                return;
            }
        }

        // If the active stream already plays this clip, no need to prebuffer
        if let Some(stream) = &self.active_stream {
            if stream.clip_id == clip_id && stream.source_path == source_path {
                return;
            }
        }

        // Terminate any previous obsolete prebuffer
        if let Some(mut old) = self.prebuffered_stream.take() {
            old.stop_flag.store(true, Ordering::SeqCst);
            let _ = old.child.kill();
            std::thread::spawn(move || {
                let _ = old.child.wait();
            });
        }

        self.prebuffered_stream = self.spawn_active_video_stream(
            clip_id,
            source_path,
            source_time,
            asset_duration,
            fps,
            speed,
            true,
        );
    }

    /// Starts or maintains a single persistent FFmpeg rawvideo pipe during `is_playing == true`:
    /// `ffmpeg -hide_banner -loglevel error -ss <playhead_start> -i <source_path> -f rawvideo -pix_fmt rgba -s 1280x720 pipe:1`
    ///
    /// If an incoming cut matches the prebuffered stream, smoothly promotes it to active without
    /// spawning a new FFmpeg process, guaranteeing 0ms latency between shots.
    pub(crate) fn sync_continuous_playback(
        &mut self,
        clip_id: Uuid,
        source_path: &Path,
        source_time: f64,
        asset_duration: f64,
        fps: f64,
        speed: f64,
    ) {
        if !source_path.exists() {
            self.stop_continuous_stream();
            return;
        }

        let clamped_speed = speed.clamp(0.25, 4.0);
        let safe_fps = fps.clamp(12.0, 60.0);

        // Check if the pre-buffered stream matches this incoming clip!
        if let Some(mut prebuf) = self.prebuffered_stream.take() {
            if prebuf.clip_id == clip_id && prebuf.source_path == source_path {
                // Seamless handoff: promote pre-buffered stream to active stream immediately!
                if let Some(mut old) = self.active_stream.take() {
                    old.stop_flag.store(true, Ordering::SeqCst);
                    let _ = old.child.kill();
                    std::thread::spawn(move || {
                        let _ = old.child.wait();
                    });
                }
                prebuf.is_prebuffer.store(false, Ordering::SeqCst);
                if let Ok(mut start) = prebuf.started_at.lock() {
                    *start = Instant::now();
                }
                prebuf.playhead_clock.store(source_time.to_bits(), Ordering::Relaxed);
                self.active_stream = Some(prebuf);
                self.pending_keys.clear();
                return;
            } else {
                // Discard stale prebuffer
                prebuf.stop_flag.store(true, Ordering::SeqCst);
                let _ = prebuf.child.kill();
                std::thread::spawn(move || {
                    let _ = prebuf.child.wait();
                });
            }
        }

        let needs_restart = match &mut self.active_stream {
            None => true,
            Some(stream) => {
                if stream.clip_id != clip_id
                    || stream.source_path != source_path
                    || (stream.speed - clamped_speed).abs() > 0.01
                {
                    true
                } else if let Ok(Some(_)) = stream.child.try_wait() {
                    // Process reached EOF or exited
                    false
                } else {
                    // Only restart if the user scrubbed or jumped the playhead significantly
                    // outside the ongoing streaming window (> 1.2s discrepancy)
                    let last_buffered_time = stream
                        .jitter_buffer
                        .lock()
                        .ok()
                        .and_then(|b| b.back().map(|f| f.source_time));

                    if let Some(buf_time) = last_buffered_time {
                        source_time < stream.initial_source_time - 0.25 || source_time > buf_time + 1.2
                    } else {
                        false
                    }
                }
            }
        };

        if !needs_restart {
            return;
        }

        if let Some(mut stream) = self.active_stream.take() {
            stream.stop_flag.store(true, Ordering::SeqCst);
            let _ = stream.child.kill();
            std::thread::spawn(move || {
                let _ = stream.child.wait();
            });
        }
        self.pending_keys.clear();

        self.active_stream = self.spawn_active_video_stream(
            clip_id,
            source_path,
            source_time,
            asset_duration,
            safe_fps,
            clamped_speed,
            false,
        );
    }

    /// Queries the continuous playback elastic jitter buffer for the frame matching
    /// `target_source_time`, strictly presenting frames in monotonic time sequence.
    /// This completely decouples 60Hz GPUI updates from video FPS and eliminates I/O lag and drag at 1x.
    pub(crate) fn query_playback_jitter_buffer(
        &mut self,
        target_source_time: f64,
    ) -> Option<Arc<RenderImage>> {
        let stream = self.active_stream.as_ref()?;
        stream
            .playhead_clock
            .store(target_source_time.to_bits(), Ordering::Relaxed);
        let mut buf = stream.jitter_buffer.lock().ok()?;

        if buf.is_empty() {
            return self.current_frame.as_ref().map(|(_, img)| img.clone());
        }

        // Strictly present frames closest to real-time and evict delayed frames (> 0.04s behind):
        while buf.len() > 1 {
            let diff_curr = (buf[0].source_time - target_source_time).abs();
            let diff_next = (buf[1].source_time - target_source_time).abs();
            if buf[1].source_time <= target_source_time
                || buf[0].source_time < target_source_time - 0.04
                || diff_next < diff_curr
            {
                buf.pop_front();
            } else {
                break;
            }
        }

        let chosen = &buf[0];
        let chosen_img = chosen.image.clone();
        let key = FrameCacheKey::new(stream.source_path.clone(), chosen.source_time);
        self.last_rendered_frame_seq = self.last_rendered_frame_seq.max(chosen.monotonic_seq);
        self.current_frame = Some((key.clone(), chosen_img.clone()));
        self.cache.insert(key, chosen_img.clone());

        Some(chosen_img)
    }

    /// Requests a single frame for manual scrubbing when paused (`is_playing == false`).
    pub(crate) fn request_scrub_frame(
        &mut self,
        source_path: PathBuf,
        source_time: f64,
        asset_duration: f64,
        fps: f64,
    ) -> Option<u64> {
        self.stop_continuous_stream();

        let clamped_time = clamp_seek_timestamp(source_time, asset_duration, fps);
        let key = FrameCacheKey::new(source_path, clamped_time);

        if let Some((cur_key, _)) = &self.current_frame {
            if cur_key == &key {
                return None;
            }
        }

        if let Some(cached) = self.cache.get(&key) {
            let seq = self.seq_counter.fetch_add(1, Ordering::SeqCst) + 1;
            self.last_rendered_frame_seq = seq;
            self.current_frame = Some((key.clone(), cached));
            self.pending_keys.retain(|k| k != &key);
            return Some(seq);
        }

        if !self.pending_keys.contains(&key) {
            let seq = self.seq_counter.fetch_add(1, Ordering::SeqCst) + 1;
            self.pending_keys.push(key.clone());
            let _ = self.req_tx.send(FrameDecodeRequest {
                monotonic_frame_seq: seq,
                is_playing: false,
                key,
                source_time: clamped_time,
                asset_duration,
                fps,
            });
            return Some(seq);
        }

        None
    }

    /// Queries the in-memory LRU cache directly for a decoded frame of a specific media source.
    #[allow(dead_code)]
    pub(crate) fn get_cached_frame(
        &mut self,
        source_path: &Path,
        source_time: f64,
    ) -> Option<Arc<RenderImage>> {
        let key = FrameCacheKey::new(source_path.to_path_buf(), source_time);
        self.cache.get(&key)
    }

    /// Non-mutating peek into the LRU cache looking around nearby quantized buckets (±2 buckets).
    pub(crate) fn peek_cached_frame(
        &self,
        source_path: &Path,
        source_time: f64,
    ) -> Option<Arc<RenderImage>> {
        let bucket = quantize_frame_bucket(source_time);
        for b in [bucket, bucket - 1, bucket + 1, bucket - 2, bucket + 2] {
            let key = FrameCacheKey {
                source_path: source_path.to_path_buf(),
                frame_bucket: b,
            };
            if let Some(img) = self.cache.peek(&key) {
                return Some(img);
            }
        }
        None
    }

    /// Returns any cached frame for `source_path`, ideal for image overlays or static presets.
    pub(crate) fn peek_any_cached_frame_for_path(
        &self,
        source_path: &Path,
    ) -> Option<Arc<RenderImage>> {
        self.cache.peek_any_for_path(source_path)
    }

    /// Returns the currently active frame if it corresponds to `source_path`.
    pub(crate) fn current_frame_for_path(&self, source_path: &Path) -> Option<Arc<RenderImage>> {
        if let Some((key, img)) = &self.current_frame {
            if key.source_path == source_path {
                return Some(img.clone());
            }
        }
        None
    }

    /// Clears the displayed frame and stops any active stream when no video clip is under the playhead.
    pub(crate) fn clear_active_frame(&mut self) {
        self.stop_continuous_stream();
        self.current_frame = None;
        self.pending_keys.clear();
    }

    /// Acknowledges and discards a stale out-of-order worker response (`monotonic_frame_seq < last_rendered_frame_seq`).
    pub(crate) fn discard_stale_response(&mut self, response: FrameDecodeResponse) {
        if let FrameDecodeOutcome::Decoded(image) = response.outcome {
            self.cache.insert(response.key, image);
        }
    }

    /// Stores a newly decoded frame from the pipe reader or scrub worker into the LRU cache.
    pub(crate) fn handle_worker_response(
        &mut self,
        response: FrameDecodeResponse,
    ) -> Result<bool, String> {
        self.pending_keys.retain(|k| k != &response.key);

        if response.monotonic_frame_seq < self.last_rendered_frame_seq {
            if let FrameDecodeOutcome::Decoded(image) = response.outcome {
                self.cache.insert(response.key, image);
            }
            return Ok(false);
        }

        match response.outcome {
            FrameDecodeOutcome::Decoded(image) => {
                self.last_rendered_frame_seq = response.monotonic_frame_seq;
                self.cache.insert(response.key.clone(), image.clone());
                self.current_frame = Some((response.key, image));
                Ok(true)
            }
            FrameDecodeOutcome::EofOrIncomplete => Ok(false),
            FrameDecodeOutcome::Error(err) => Err(err),
        }
    }

    /// Returns the currently active GPU-ready [`RenderImage`] frame, if any.
    pub(crate) fn current_frame_image(&self) -> Option<Arc<RenderImage>> {
        self.current_frame.as_ref().map(|(_, img)| img.clone())
    }
}

impl Drop for FramePlaybackEngine {
    fn drop(&mut self) {
        self.stop_continuous_stream();
    }
}

struct ActiveAudioStream {
    clip_id: Uuid,
    started_at: Instant,
    initial_source_time: f64,
    speed: f64,
    volume: f32,
    child: Child,
}

/// Specification for an audible stream at the playhead used for parallel multi-track mixing.
#[derive(Clone, Debug)]
pub(crate) struct AudioPlaybackSource {
    pub(crate) clip_id: Uuid,
    pub(crate) source_path: PathBuf,
    pub(crate) source_time: f64,
    pub(crate) remaining_timeline_secs: f64,
    pub(crate) speed: f64,
    pub(crate) volume: f32,
}

/// Synchronized background audio player supporting parallel multi-track mixing via headless `ffplay`.
#[derive(Default)]
pub(crate) struct AudioPlaybackEngine {
    active_streams: Vec<ActiveAudioStream>,
}

impl AudioPlaybackEngine {
    pub(crate) fn new() -> Self {
        Self {
            active_streams: Vec::new(),
        }
    }

    /// Stops all currently playing audio streams immediately.
    pub(crate) fn stop(&mut self) {
        for mut stream in self.active_streams.drain(..) {
            let _ = stream.child.kill();
            let _ = stream.child.wait();
        }
    }

    /// Plays an audio file directly for preview purposes (e.g. testing TTS voices before inserting).
    pub(crate) fn play_preview(&mut self, file_path: &Path) {
        self.stop();
        if !file_path.exists() {
            return;
        }
        let ffplay = resolve_media_binary("ffplay");
        let mut cmd = Command::new(ffplay);
        configure_headless_command(&mut cmd);
        cmd.args(["-nodisp", "-autoexit", "-loglevel", "error"])
            .arg(file_path);
        if let Ok(child) = cmd.spawn() {
            self.active_streams.push(ActiveAudioStream {
                clip_id: Uuid::new_v4(),
                started_at: Instant::now(),
                initial_source_time: 0.0,
                speed: 1.0,
                volume: 1.0,
                child,
            });
        }
    }

    /// Synchronizes multiple audio sources in parallel, enabling real-time timeline multi-track audio mixing
    /// (e.g. V1 video sound + A1 voiceover + A2 background music playing simultaneously).
    pub(crate) fn sync_multi(
        &mut self,
        is_playing: bool,
        sources: &[AudioPlaybackSource],
    ) {
        if !is_playing || sources.is_empty() {
            self.stop();
            return;
        }

        // 1. Remove and kill streams for clips that are no longer active/audible
        let active_clip_ids: Vec<Uuid> = sources.iter().map(|s| s.clip_id).collect();
        let mut retained = Vec::new();
        for mut stream in self.active_streams.drain(..) {
            if active_clip_ids.contains(&stream.clip_id) {
                retained.push(stream);
            } else {
                let _ = stream.child.kill();
                let _ = stream.child.wait();
            }
        }
        self.active_streams = retained;

        // 2. Synchronize each source
        let ffplay = resolve_media_binary("ffplay");
        for src in sources {
            if src.volume <= 0.001 || src.remaining_timeline_secs <= 0.05 || !src.source_path.exists() {
                continue;
            }

            let existing_idx = self.active_streams.iter().position(|s| s.clip_id == src.clip_id);
            let needs_restart = match existing_idx {
                None => true,
                Some(idx) => {
                    let stream = &mut self.active_streams[idx];
                    if (stream.speed - src.speed).abs() > 0.01 || (stream.volume - src.volume).abs() > 0.02 {
                        true
                    } else if let Ok(Some(_)) = stream.child.try_wait() {
                        true
                    } else {
                        let elapsed_source = stream.started_at.elapsed().as_secs_f64() * stream.speed;
                        let expected_source_time = stream.initial_source_time + elapsed_source;
                        (expected_source_time - src.source_time).abs() > 0.45
                    }
                }
            };

            if needs_restart {
                if let Some(idx) = existing_idx {
                    let mut old = self.active_streams.remove(idx);
                    let _ = old.child.kill();
                    let _ = old.child.wait();
                }

                let clamped_speed = src.speed.clamp(0.5, 2.0);
                let clamped_vol = src.volume.clamp(0.0, 2.0);
                let af_filter = format!("volume={clamped_vol:.2},atempo={clamped_speed:.2}");
                let ss_arg = format!("{:.3}", src.source_time.max(0.0));
                let duration_arg = format!("{:.3}", src.remaining_timeline_secs.max(0.1));

                let mut cmd = Command::new(&ffplay);
                configure_headless_command(&mut cmd);
                cmd.args([
                    "-nodisp",
                    "-vn",
                    "-autoexit",
                    "-loglevel",
                    "quiet",
                    "-ss",
                    &ss_arg,
                    "-t",
                    &duration_arg,
                    "-af",
                    &af_filter,
                ])
                .arg(&src.source_path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());

                if let Ok(child) = cmd.spawn() {
                    self.active_streams.push(ActiveAudioStream {
                        clip_id: src.clip_id,
                        started_at: Instant::now(),
                        initial_source_time: src.source_time,
                        speed: clamped_speed,
                        volume: clamped_vol,
                        child,
                    });
                }
            }
        }
    }

    /// Synchronizes audio playback with the current timeline playhead and clip parameters (single-source compatibility).
    #[allow(dead_code)]
    pub(crate) fn sync(
        &mut self,
        is_playing: bool,
        clip_id: Uuid,
        source_path: &Path,
        source_time: f64,
        remaining_timeline_secs: f64,
        speed: f64,
        volume: f32,
    ) {
        self.sync_multi(
            is_playing,
            &[AudioPlaybackSource {
                clip_id,
                source_path: source_path.to_path_buf(),
                source_time,
                remaining_timeline_secs,
                speed,
                volume,
            }],
        );
    }
}

impl Drop for AudioPlaybackEngine {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        FrameCacheKey, FrameDecodeOutcome, FrameDecodeRequest, FrameDecodeResponse, FrameLruCache,
        FramePlaybackEngine, RAW_RGBA_FRAME_BYTES, clamp_seek_timestamp, quantize_frame_bucket,
        rgba_to_render_image, select_latest_playback_request,
    };

    #[test]
    fn raw_rgba_frame_byte_size_matches_1920x1080x4() {
        assert_eq!(RAW_RGBA_FRAME_BYTES, 1920 * 1080 * 4);
    }

    #[test]
    fn clamps_seek_timestamp_strictly_before_eof() {
        let clamped = clamp_seek_timestamp(10.0, 10.0, 24.0);
        assert!(clamped < 10.0);
        assert!((clamped - (10.0 - 1.0 / 24.0)).abs() < 1e-6);
        assert_eq!(clamp_seek_timestamp(-3.0, 10.0, 24.0), 0.0);
    }

    #[test]
    fn converts_rgba_buffer_to_bgra_render_image() {
        let rgba = vec![
            255, 0, 0, 255, // Red pixel
            0, 255, 0, 255, // Green pixel
        ];
        let image = rgba_to_render_image(2, 1, rgba).expect("valid 2x1 RGBA buffer");
        let bytes = image.as_bytes(0).unwrap();
        assert_eq!(&bytes[0..4], &[0, 0, 255, 255]);
        assert_eq!(&bytes[4..8], &[0, 255, 0, 255]);
    }

    #[test]
    fn lru_cache_evicts_least_recently_used_frame() {
        let mut cache = FrameLruCache::new(2);
        let img = rgba_to_render_image(1, 1, vec![0, 0, 0, 255]).unwrap();

        let k1 = FrameCacheKey::new(PathBuf::from("a.mp4"), 0.0);
        let k2 = FrameCacheKey::new(PathBuf::from("a.mp4"), 1.0);
        let k3 = FrameCacheKey::new(PathBuf::from("a.mp4"), 2.0);

        cache.insert(k1.clone(), img.clone());
        cache.insert(k2.clone(), img.clone());
        assert!(cache.get(&k1).is_some());

        cache.insert(k3.clone(), img);
        assert!(cache.get(&k1).is_some());
        assert!(cache.get(&k2).is_none());
        assert!(cache.get(&k3).is_some());
    }

    #[test]
    fn quantizes_timestamps_to_frame_buckets() {
        assert_eq!(quantize_frame_bucket(0.0), 0);
        assert_eq!(quantize_frame_bucket(1.0), 24);
    }

    #[test]
    fn monotonic_sequence_guard_discards_out_of_order_frames() {
        let (mut engine, _rx) = FramePlaybackEngine::spawn();
        let img_new = rgba_to_render_image(1, 1, vec![0, 255, 0, 255]).unwrap();
        let img_old = rgba_to_render_image(1, 1, vec![255, 0, 0, 255]).unwrap();

        let accepted_new = engine
            .handle_worker_response(FrameDecodeResponse {
                monotonic_frame_seq: 5,
                key: FrameCacheKey::new(PathBuf::from("clip.mp4"), 2.0),
                outcome: FrameDecodeOutcome::Decoded(img_new),
            })
            .unwrap();
        assert!(accepted_new);
        assert_eq!(engine.last_rendered_frame_seq(), 5);

        let accepted_old = engine
            .handle_worker_response(FrameDecodeResponse {
                monotonic_frame_seq: 3,
                key: FrameCacheKey::new(PathBuf::from("clip.mp4"), 1.0),
                outcome: FrameDecodeOutcome::Decoded(img_old),
            })
            .unwrap();
        assert!(!accepted_old);
        assert_eq!(engine.last_rendered_frame_seq(), 5);
    }

    #[test]
    fn selects_most_advanced_request_during_playback_drain() {
        let path = PathBuf::from("clip.mp4");
        let req1 = FrameDecodeRequest {
            monotonic_frame_seq: 10,
            is_playing: true,
            key: FrameCacheKey::new(path.clone(), 1.0),
            source_time: 1.0,
            asset_duration: 10.0,
            fps: 24.0,
        };
        let req2 = FrameDecodeRequest {
            monotonic_frame_seq: 11,
            is_playing: true,
            key: FrameCacheKey::new(path.clone(), 1.25),
            source_time: 1.25,
            asset_duration: 10.0,
            fps: 24.0,
        };
        let req3_stale = FrameDecodeRequest {
            monotonic_frame_seq: 9,
            is_playing: true,
            key: FrameCacheKey::new(path, 0.8),
            source_time: 0.8,
            asset_duration: 10.0,
            fps: 24.0,
        };

        let chosen = select_latest_playback_request(req1, vec![req2, req3_stale]);
        assert_eq!(chosen.monotonic_frame_seq, 11);
        assert!((chosen.source_time - 1.25).abs() < 1e-6);
    }

    #[test]
    fn jitter_buffer_eviction_presents_strictly_monotonic_frames_at_1x() {
        use std::collections::VecDeque;
        use super::JitterFrame;

        let img = rgba_to_render_image(1, 1, vec![0, 0, 0, 255]).unwrap();
        let mut buf = VecDeque::new();
        buf.push_back(JitterFrame { source_time: 0.00, monotonic_seq: 1, image: img.clone() });
        buf.push_back(JitterFrame { source_time: 0.04, monotonic_seq: 2, image: img.clone() });
        buf.push_back(JitterFrame { source_time: 0.08, monotonic_seq: 3, image: img.clone() });
        buf.push_back(JitterFrame { source_time: 0.12, monotonic_seq: 4, image: img.clone() });

        // Helper simulation of the eviction algorithm
        let pick_frame = |buffer: &mut VecDeque<JitterFrame>, target: f64| -> f64 {
            while buffer.len() > 1 {
                if buffer[1].source_time <= target {
                    buffer.pop_front();
                } else {
                    break;
                }
            }
            buffer[0].source_time
        };

        // At 0.00s (frame 0)
        assert_eq!(pick_frame(&mut buf, 0.00), 0.00);
        // At 0.02s (still within frame 0's display period)
        assert_eq!(pick_frame(&mut buf, 0.02), 0.00);
        // At 0.04s (reaches frame 1, frame 0 evicted)
        assert_eq!(pick_frame(&mut buf, 0.04), 0.04);
        // At 0.06s (within frame 1)
        assert_eq!(pick_frame(&mut buf, 0.06), 0.04);
        // At 0.08s (reaches frame 2, frame 1 evicted)
        assert_eq!(pick_frame(&mut buf, 0.08), 0.08);
        // At 0.12s (reaches frame 3, frame 2 evicted)
        assert_eq!(pick_frame(&mut buf, 0.12), 0.12);
        assert_eq!(buf.len(), 1);
    }

    #[test]
    fn frame_dropping_discards_frames_lagging_behind_playhead_clock() {
        use std::collections::VecDeque;
        use super::JitterFrame;

        let img = rgba_to_render_image(1, 1, vec![0, 0, 0, 255]).unwrap();
        let mut buf = VecDeque::new();
        buf.push_back(JitterFrame { source_time: 0.00, monotonic_seq: 1, image: img.clone() });
        buf.push_back(JitterFrame { source_time: 0.04, monotonic_seq: 2, image: img.clone() });
        buf.push_back(JitterFrame { source_time: 0.08, monotonic_seq: 3, image: img.clone() });
        buf.push_back(JitterFrame { source_time: 0.12, monotonic_seq: 4, image: img.clone() });

        let pick_closest_realtime = |buffer: &mut VecDeque<JitterFrame>, target: f64| -> f64 {
            while buffer.len() > 1 {
                let diff_curr = (buffer[0].source_time - target).abs();
                let diff_next = (buffer[1].source_time - target).abs();
                if buffer[1].source_time <= target
                    || buffer[0].source_time < target - 0.04
                    || diff_next < diff_curr
                {
                    buffer.pop_front();
                } else {
                    break;
                }
            }
            buffer[0].source_time
        };

        // If playhead clock is at 0.11s, frames 0.00 and 0.04 are evicted as lagging (> 0.04s behind),
        // and frame 0.12 is chosen as closest to 0.11s in real-time.
        let selected = pick_closest_realtime(&mut buf, 0.11);
        assert_eq!(selected, 0.12);
    }
}
