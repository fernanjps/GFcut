use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    thread,
};

use crate::frame_decoder::{configure_headless_command, resolve_media_binary};

/// Number of peak samples extracted per audio second (100 Hz, 1 sample every 10ms).
pub(crate) const SAMPLES_PER_SECOND: usize = 100;

#[derive(Clone, Debug)]
pub(crate) struct WaveformResponse {
    pub(crate) source_path: PathBuf,
    pub(crate) peaks: Arc<Vec<u8>>,
}

/// Runs FFmpeg to decode raw 8000 Hz mono 16-bit PCM audio and extracts 100 RMS/peak samples per second (values `0..=255`).
///
/// Execution runs strictly on background worker threads (`WaveformService`) and never blocks the UI thread.
pub(crate) fn extract_waveform_peaks(source_path: &Path) -> Option<Vec<u8>> {
    if !source_path.exists() {
        return None;
    }

    let ffmpeg = resolve_media_binary("ffmpeg");
    let mut cmd = Command::new(ffmpeg);
    configure_headless_command(&mut cmd);

    let output = cmd
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(source_path)
        .args([
            "-vn",
            "-ac",
            "1",
            "-ar",
            "8000",
            "-f",
            "s16le",
            "pipe:1",
        ])
        .output()
        .ok()?;

    if !output.status.success() || output.stdout.is_empty() {
        return None;
    }

    let raw_bytes = output.stdout;
    // 8000 Hz / 100 samples per sec = 80 audio samples per peak bucket (160 bytes of s16le).
    let chunk_size = 160;
    let mut peaks = Vec::with_capacity(raw_bytes.len() / chunk_size + 1);

    for chunk in raw_bytes.chunks(chunk_size) {
        let mut max_abs: u32 = 0;
        let mut sum_sq: u64 = 0;
        let samples = chunk.len() / 2;
        if samples == 0 {
            continue;
        }

        for i in 0..samples {
            let s = i16::from_le_bytes([chunk[i * 2], chunk[i * 2 + 1]]);
            let abs = s.unsigned_abs() as u32;
            if abs > max_abs {
                max_abs = abs;
            }
            sum_sq += (s as i64 * s as i64) as u64;
        }

        let rms = (sum_sq as f64 / samples as f64).sqrt();
        let peak_norm = (max_abs as f64 / 32767.0).min(1.0);
        let rms_norm = ((rms / 32767.0) * 1.6).min(1.0);
        let hybrid = (0.55 * peak_norm + 0.45 * rms_norm).clamp(0.0, 1.0);
        let byte_val = (hybrid * 255.0).round() as u8;
        peaks.push(byte_val);
    }

    Some(peaks)
}

/// Resamples a clip's peak slice (`in_point..out_point` in seconds) into `count` normalized values (`0.08..=1.0`).
pub(crate) fn resample_peaks_for_range(
    peaks: &[u8],
    in_point: f64,
    out_point: f64,
    count: usize,
) -> Vec<f32> {
    if count == 0 {
        return Vec::new();
    }
    if peaks.is_empty() || out_point <= in_point {
        return vec![0.12; count];
    }

    let start_idx = (in_point.max(0.0) * SAMPLES_PER_SECOND as f64) as usize;
    let end_idx = (out_point.max(0.0) * SAMPLES_PER_SECOND as f64) as usize;
    let slice = if start_idx < peaks.len() {
        let end = end_idx.min(peaks.len()).max(start_idx + 1);
        &peaks[start_idx..end]
    } else {
        &peaks[..]
    };

    if slice.is_empty() {
        return vec![0.12; count];
    }

    let mut result = Vec::with_capacity(count);
    let step = slice.len() as f64 / count as f64;

    for i in 0..count {
        let sub_start = (i as f64 * step) as usize;
        let sub_end = (((i + 1) as f64 * step) as usize).min(slice.len()).max(sub_start + 1);
        let mut max_val = 0u8;
        for &val in &slice[sub_start..sub_end] {
            if val > max_val {
                max_val = val;
            }
        }
        let frac = (max_val as f32 / 255.0).clamp(0.08, 1.0);
        result.push(frac);
    }

    result
}

/// Non-blocking background worker that extracts and caches audio RMS/peak envelopes.
pub(crate) struct WaveformService {
    cache: HashMap<PathBuf, Arc<Vec<u8>>>,
    in_flight: HashSet<PathBuf>,
    req_tx: flume::Sender<PathBuf>,
}

impl WaveformService {
    pub(crate) fn spawn() -> (Self, flume::Receiver<WaveformResponse>) {
        let (req_tx, req_rx) = flume::unbounded::<PathBuf>();
        let (resp_tx, resp_rx) = flume::unbounded::<WaveformResponse>();

        thread::Builder::new()
            .name("gfcut-waveform-service".to_owned())
            .spawn(move || {
                while let Ok(path) = req_rx.recv() {
                    let peaks = extract_waveform_peaks(&path).unwrap_or_default();
                    if resp_tx
                        .send(WaveformResponse {
                            source_path: path,
                            peaks: Arc::new(peaks),
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .expect("spawn gfcut-waveform-service thread");

        (
            Self {
                cache: HashMap::new(),
                in_flight: HashSet::new(),
                req_tx,
            },
            resp_rx,
        )
    }

    /// Queues a media file for background audio peak extraction if not already cached or pending.
    pub(crate) fn request(&mut self, path: PathBuf) {
        if self.cache.contains_key(&path) || self.in_flight.contains(&path) {
            return;
        }

        self.in_flight.insert(path.clone());
        let _ = self.req_tx.send(path);
    }

    /// Retrieves the cached peak buffer for `path`, if available.
    pub(crate) fn get(&self, path: &Path) -> Option<Arc<Vec<u8>>> {
        self.cache.get(path).cloned()
    }

    /// Handles a completed background extraction, storing it in the in-memory cache.
    pub(crate) fn handle_worker_response(&mut self, resp: WaveformResponse) -> bool {
        self.in_flight.remove(&resp.source_path);
        let changed = !self.cache.contains_key(&resp.source_path);
        self.cache.insert(resp.source_path, resp.peaks);
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resamples_peak_slice_properly() {
        let peaks: Vec<u8> = vec![0, 50, 100, 200, 255, 128, 64, 32];
        let res = resample_peaks_for_range(&peaks, 0.0, 0.08, 4);
        assert_eq!(res.len(), 4);
        for &val in &res {
            assert!(val >= 0.08 && val <= 1.0);
        }
    }

    #[test]
    fn empty_peaks_produces_baseline_height() {
        let res = resample_peaks_for_range(&[], 0.0, 5.0, 10);
        assert_eq!(res.len(), 10);
        assert_eq!(res[0], 0.12);
    }
}
