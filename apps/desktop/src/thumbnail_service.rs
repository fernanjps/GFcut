use std::{
    collections::{HashMap, HashSet},
    ffi::OsStr,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    thread,
};

use gpui::RenderImage;
use image::{Frame, ImageBuffer, ImageFormat, Rgba};

use crate::frame_decoder::{
     configure_headless_command, resolve_media_binary, rgba_to_render_image,
};

pub(crate) const THUMBNAIL_WIDTH: u32 = 240;
pub(crate) const THUMBNAIL_HEIGHT: u32 = 135;

#[derive(Clone, Debug)]
pub(crate) struct ThumbnailRequest {
    pub(crate) source_path: PathBuf,
    pub(crate) timestamp_secs: f64,
}

#[derive(Clone)]
pub(crate) struct ThumbnailResponse {
    pub(crate) source_path: PathBuf,
    pub(crate) image: Arc<RenderImage>,
}

/// Runs FFmpeg to generate a lightweight `240x135` JPEG thumbnail at `timestamp_secs`:
/// `ffmpeg -ss <timestamp> -i <input> -vframes 1 -f image2 -s 240x135 pipe:1`
pub(crate) fn extract_jpeg_thumbnail_bytes(
    source_path: &Path,
    timestamp_secs: f64,
) -> Option<Vec<u8>> {
    if !source_path.exists() {
        return None;
    }

    let ffmpeg = resolve_media_binary("ffmpeg");
    let ts_arg = format!("{:.3}", timestamp_secs.max(0.0));
    let size_arg = format!("{THUMBNAIL_WIDTH}x{THUMBNAIL_HEIGHT}");

    let mut cmd = Command::new(ffmpeg);
    configure_headless_command(&mut cmd);

    let output = cmd
        .args(["-hide_banner", "-loglevel", "error", "-ss", &ts_arg, "-i"])
        .arg(source_path)
        .args([
            "-vframes",
            "1",
            "-f",
            "image2",
            "-c:v",
            "mjpeg",
            "-s",
            &size_arg,
            "pipe:1",
        ])
        .output()
        .ok()?;

    if !output.status.success() || output.stdout.is_empty() {
        return None;
    }

    Some(output.stdout)
}

/// Decodes in-memory JPEG bytes into a GPUI [`RenderImage`] (converting RGBA to BGRA).
pub(crate) fn jpeg_bytes_to_render_image(jpeg_bytes: &[u8]) -> Option<Arc<RenderImage>> {
    let decoded = image::load_from_memory_with_format(jpeg_bytes, ImageFormat::Jpeg)
        .or_else(|_| image::load_from_memory(jpeg_bytes))
        .ok()?;
    let rgba = decoded.into_rgba8();
    let (width, height) = rgba.dimensions();
    rgba_to_render_image(width, height, rgba.into_raw())
}

/// Builds a sleek `240x135` studio cover image when an asset is audio-only or FFmpeg frame extraction
/// is unavailable, ensuring every card and timeline clip has an immediate visual thumbnail.
pub(crate) fn procedural_thumbnail_render_image(
    seed_label: &str,
    is_audio: bool,
) -> Arc<RenderImage> {
    let width = THUMBNAIL_WIDTH;
    let height = THUMBNAIL_HEIGHT;
    let mut raw_bgra = vec![0u8; (width * height * 4) as usize];

    let hash = seed_label.bytes().fold(2166136261u32, |acc, b| {
        (acc ^ u32::from(b)).wrapping_mul(16777619)
    });

    let accent_r = if is_audio {
        16u8
    } else {
        (40 + (hash & 0x3F)) as u8
    };
    let accent_g = if is_audio {
        185u8
    } else {
        (95 + ((hash >> 6) & 0x3F)) as u8
    };
    let accent_b = if is_audio {
        145u8
    } else {
        (195 + ((hash >> 12) & 0x37)) as u8
    };

    let mid_y = (height / 2) as i32;

    for y in 0..height {
        for x in 0..width {
            let idx = ((y * width + x) * 4) as usize;
            let fx = x as f32 / width as f32;
            let fy = y as f32 / height as f32;

            let mut r = (14.0 + fx * f32::from(accent_r) * 0.35 + (1.0 - fy) * 18.0) as u8;
            let mut g = (18.0 + fx * f32::from(accent_g) * 0.35 + fy * 14.0) as u8;
            let mut b = (28.0 + (1.0 - fx) * f32::from(accent_b) * 0.42) as u8;

            if is_audio {
                // Draw a stylized emerald/cyan waveform across the center of the card
                let col = x / 4;
                let wave_seed = hash.wrapping_add(col.wrapping_mul(1103515245));
                let amp = 6 + ((wave_seed >> 16) % 36) as i32;
                let dist = (y as i32 - mid_y).abs();
                if x % 4 < 2 && dist <= amp {
                    r = 35;
                    g = 220;
                    b = 175;
                }
            } else if x % 24 == 0 || y % 24 == 0 {
                r = r.saturating_add(10);
                g = g.saturating_add(12);
                b = b.saturating_add(16);
            }

            // BGRA byte order for GPUI
            raw_bgra[idx] = b;
            raw_bgra[idx + 1] = g;
            raw_bgra[idx + 2] = r;
            raw_bgra[idx + 3] = 255;
        }
    }

    let buffer = ImageBuffer::<Rgba<u8>, Vec<u8>>::from_raw(width, height, raw_bgra)
        .expect("valid 240x135 BGRA thumbnail buffer");
    Arc::new(RenderImage::new([Frame::new(buffer)]))
}

fn is_audio_extension(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "wav" | "mp3" | "flac" | "ogg" | "aac" | "m4a"
            )
        })
}

/// Generates a `240x135` [`RenderImage`] thumbnail for `source_path` at `timestamp_secs`.
pub(crate) fn generate_thumbnail_for_path(
    source_path: &Path,
    timestamp_secs: f64,
) -> Arc<RenderImage> {
    if !is_audio_extension(source_path) {
        if let Some(jpeg_bytes) = extract_jpeg_thumbnail_bytes(source_path, timestamp_secs) {
            if let Some(render_img) = jpeg_bytes_to_render_image(&jpeg_bytes) {
                return render_img;
            }
        }
        // Retry at 0.0s if timestamp_secs was near EOF or failed
        if timestamp_secs > 0.01 {
            if let Some(jpeg_bytes) = extract_jpeg_thumbnail_bytes(source_path, 0.0) {
                if let Some(render_img) = jpeg_bytes_to_render_image(&jpeg_bytes) {
                    return render_img;
                }
            }
        }
    }

    let label = source_path
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or("media");
    procedural_thumbnail_render_image(label, is_audio_extension(source_path))
}

/// Non-blocking background service that generates and caches `240x135` [`RenderImage`] thumbnails.
pub(crate) struct ThumbnailService {
    cache: HashMap<PathBuf, Arc<RenderImage>>,
    in_flight: HashSet<PathBuf>,
    req_tx: flume::Sender<ThumbnailRequest>,
}

impl ThumbnailService {
    pub(crate) fn spawn() -> (Self, flume::Receiver<ThumbnailResponse>) {
        let (req_tx, req_rx) = flume::unbounded::<ThumbnailRequest>();
        let (resp_tx, resp_rx) = flume::unbounded::<ThumbnailResponse>();

        thread::Builder::new()
            .name("gfcut-thumbnail-service".to_owned())
            .spawn(move || {
                while let Ok(req) = req_rx.recv() {
                    let image = generate_thumbnail_for_path(&req.source_path, req.timestamp_secs);
                    if resp_tx
                        .send(ThumbnailResponse {
                            source_path: req.source_path,
                            image,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .expect("failed to spawn thumbnail background service");

        (
            Self {
                cache: HashMap::new(),
                in_flight: HashSet::new(),
                req_tx,
            },
            resp_rx,
        )
    }

    /// Returns the cached thumbnail if available, otherwise queues an asynchronous background build.
    pub(crate) fn request_thumbnail(&mut self, source_path: &Path, seek_time: f64) {
        if self.cache.contains_key(source_path) || self.in_flight.contains(source_path) {
            return;
        }
        let path_buf = source_path.to_path_buf();
        self.in_flight.insert(path_buf.clone());
        let _ = self.req_tx.send(ThumbnailRequest {
            source_path: path_buf,
            timestamp_secs: seek_time.max(0.0),
        });
    }

    /// Looks up a cached `240x135` [`RenderImage`] thumbnail for `source_path`.
    pub(crate) fn get_cached(&self, source_path: &Path) -> Option<Arc<RenderImage>> {
        self.cache.get(source_path).cloned()
    }

    /// Stores a completed thumbnail into the in-memory cache.
    pub(crate) fn handle_worker_response(&mut self, response: ThumbnailResponse) -> bool {
        self.in_flight.remove(&response.source_path);
        self.cache.insert(response.source_path, response.image);
        true
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        THUMBNAIL_HEIGHT, THUMBNAIL_WIDTH, ThumbnailResponse, ThumbnailService,
        procedural_thumbnail_render_image,
    };

    #[test]
    fn generates_procedural_thumbnail_with_expected_dimensions() {
        let video_thumb = procedural_thumbnail_render_image("clip_01.mp4", false);
        let audio_thumb = procedural_thumbnail_render_image("voiceover.wav", true);

        let expected_bytes = (THUMBNAIL_WIDTH * THUMBNAIL_HEIGHT * 4) as usize;
        assert_eq!(video_thumb.as_bytes(0).unwrap().len(), expected_bytes);
        assert_eq!(audio_thumb.as_bytes(0).unwrap().len(), expected_bytes);
    }

    #[test]
    fn caches_thumbnail_responses_in_memory() {
        let (mut service, _rx) = ThumbnailService::spawn();
        let path = PathBuf::from("test_asset.mp4");
        assert!(service.get_cached(&path).is_none());

        let img = procedural_thumbnail_render_image("test_asset.mp4", false);
        service.handle_worker_response(ThumbnailResponse {
            source_path: path.clone(),
            image: img,
        });

        assert!(service.get_cached(&path).is_some());
    }
}
