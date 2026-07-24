// Live transcription — text while the meeting is still happening.
//
// The recording lanes (mic, system audio) already write WAVs. Reading those back
// mid-write is unreliable: the header's length field isn't correct until the file
// is finalized. So instead each lane pushes a copy of its samples into a `Tap`,
// downmixed to mono and resampled to Whisper's 16 kHz on the way in. A worker
// thread drains the tap, decodes finished windows, and emits them as Tauri events.
//
// Chunking is the interesting part. Cutting every N seconds slices words in half.
// Instead the worker waits until it has at least MIN_WINDOW of audio, then looks
// for the quietest short frame in the back half of the window and cuts there — a
// crude but effective "cut on a pause". The final, accurate transcript is still
// produced from the WAVs at stop; this lane is for the live panel only.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use serde::Serialize;

use crate::transcribe::{Quality, Transcriber, WHISPER_RATE};

/// Don't decode until at least this much audio has accumulated.
const MIN_WINDOW_SECS: f32 = 6.0;
/// Force a cut once the window reaches this length, pause or not.
const MAX_WINDOW_SECS: f32 = 12.0;
/// Frame size used when hunting for the quietest moment to cut on.
const PAUSE_FRAME_SECS: f32 = 0.02;
/// How often the worker checks whether a window is ready.
const POLL_MS: u64 = 400;

/// One line of live transcript, as emitted to the UI.
#[derive(Debug, Clone, Serialize)]
pub struct LiveLine {
    /// Milliseconds from the start of the meeting.
    pub at_ms: u64,
    pub text: String,
}

/// Shared sink the capture lanes push into. Cheap to clone; the audio callback
/// only takes the lock long enough to append.
#[derive(Default)]
pub struct Tap {
    samples: Mutex<Vec<f32>>,
}

impl Tap {
    /// Append interleaved samples from a capture callback, downmixing to mono and
    /// resampling to 16 kHz. Runs on the realtime audio thread, so it stays
    /// allocation-light and never blocks on anything but this lock.
    pub fn push(&self, data: &[f32], channels: u16, rate: u32) {
        if data.is_empty() || channels == 0 || rate == 0 {
            return;
        }
        let ch = channels as usize;
        let frames = data.len() / ch;
        if frames == 0 {
            return;
        }

        // Downmix, then take every (rate / 16000)th frame with linear
        // interpolation — the same approach as the offline path.
        let ratio = rate as f32 / WHISPER_RATE as f32;
        let out_len = (frames as f32 / ratio) as usize;
        let mut out = Vec::with_capacity(out_len);
        for i in 0..out_len {
            let src = i as f32 * ratio;
            let a = src.floor() as usize;
            let b = (a + 1).min(frames - 1);
            let t = src - a as f32;
            let mono = |f: usize| {
                let base = f * ch;
                data[base..base + ch].iter().sum::<f32>() / ch as f32
            };
            out.push(mono(a) * (1.0 - t) + mono(b) * t);
        }

        if let Ok(mut buf) = self.samples.lock() {
            buf.extend_from_slice(&out);
        }
    }

    /// Take everything buffered so far.
    fn drain(&self) -> Vec<f32> {
        match self.samples.lock() {
            Ok(mut buf) => std::mem::take(&mut *buf),
            Err(_) => Vec::new(),
        }
    }
}

/// A running live-transcription worker. Dropping it stops the worker.
pub struct LiveSession {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    /// Every line emitted so far, so a recap can be asked for mid-meeting.
    lines: Arc<Mutex<Vec<LiveLine>>>,
}

impl LiveSession {
    /// Start decoding whatever `tap` receives. `emit` is called for each finished
    /// line; it runs on the worker thread.
    pub fn start(
        tap: Arc<Tap>,
        model_path: String,
        language: Option<String>,
        emit: impl Fn(LiveLine) + Send + 'static,
    ) -> Result<Self, String> {
        // Load the model up front so a missing/corrupt model fails the start call
        // rather than silently producing no live text.
        let transcriber = Transcriber::load(&model_path)?;

        let stop = Arc::new(AtomicBool::new(false));
        let lines: Arc<Mutex<Vec<LiveLine>>> = Arc::new(Mutex::new(Vec::new()));

        let worker_stop = stop.clone();
        let worker_lines = lines.clone();
        let handle = std::thread::Builder::new()
            .name("oatmeal-live".into())
            .spawn(move || {
                run(tap, transcriber, language, worker_stop, worker_lines, emit);
            })
            .map_err(|e| format!("spawn live thread: {e}"))?;

        Ok(Self {
            stop,
            handle: Some(handle),
            lines,
        })
    }

    /// Every line emitted so far.
    pub fn lines(&self) -> Vec<LiveLine> {
        self.lines.lock().map(|l| l.clone()).unwrap_or_default()
    }

    /// Stop the worker and join it.
    pub fn stop(mut self) {
        self.signal_and_join();
    }

    fn signal_and_join(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for LiveSession {
    fn drop(&mut self) {
        self.signal_and_join();
    }
}

fn run(
    tap: Arc<Tap>,
    transcriber: Transcriber,
    language: Option<String>,
    stop: Arc<AtomicBool>,
    lines: Arc<Mutex<Vec<LiveLine>>>,
    emit: impl Fn(LiveLine),
) {
    let rate = WHISPER_RATE as f32;
    let min_window = (MIN_WINDOW_SECS * rate) as usize;
    let max_window = (MAX_WINDOW_SECS * rate) as usize;

    let mut pending: Vec<f32> = Vec::new();
    let mut elapsed_samples: usize = 0;

    loop {
        let stopping = stop.load(Ordering::SeqCst);
        pending.extend_from_slice(&tap.drain());

        let ready = pending.len() >= min_window;
        if !ready && !stopping {
            std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
            continue;
        }
        if pending.is_empty() {
            // Nothing left and we're shutting down.
            if stopping {
                break;
            }
            continue;
        }

        // On the final pass, decode whatever is left rather than waiting for a
        // full window — otherwise the last few seconds never appear.
        let cut = if stopping && pending.len() < min_window {
            pending.len()
        } else {
            find_cut(&pending, min_window, max_window)
        };

        let window: Vec<f32> = pending.drain(..cut).collect();
        let at_ms = (elapsed_samples as f64 / rate as f64 * 1000.0) as u64;
        elapsed_samples += window.len();

        if !is_silent(&window) {
            match transcriber.run(&window, language.as_deref(), Quality::Fast) {
                Ok(t) => {
                    let text = t.text.trim().to_string();
                    if !text.is_empty() && !is_noise(&text) {
                        let line = LiveLine { at_ms, text };
                        if let Ok(mut l) = lines.lock() {
                            l.push(line.clone());
                        }
                        emit(line);
                    }
                }
                Err(e) => eprintln!("[oatmeal] live decode: {e}"),
            }
        }

        if stopping && pending.is_empty() {
            break;
        }
    }
}

/// Choose where to end the next window: the quietest frame in the back half of
/// the candidate range, so cuts land on pauses rather than mid-word. Falls back
/// to the hard maximum when the audio never dips.
fn find_cut(samples: &[f32], min_window: usize, max_window: usize) -> usize {
    let end = samples.len().min(max_window);
    if end <= min_window {
        return end;
    }
    let frame = (PAUSE_FRAME_SECS * WHISPER_RATE as f32) as usize;
    if frame == 0 {
        return end;
    }

    let mut best = end;
    let mut best_energy = f32::MAX;
    let mut i = min_window;
    while i + frame <= end {
        let energy: f32 = samples[i..i + frame].iter().map(|s| s.abs()).sum();
        if energy < best_energy {
            best_energy = energy;
            best = i + frame;
        }
        i += frame;
    }
    best
}

/// Whether a window is quiet enough that decoding it would only produce
/// hallucinated filler.
fn is_silent(samples: &[f32]) -> bool {
    if samples.is_empty() {
        return true;
    }
    let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    peak < 0.006
}

/// Whisper's stock output over near-silence. Worth dropping before it reaches
/// the live panel.
fn is_noise(text: &str) -> bool {
    let t = text.trim().trim_matches(|c: char| !c.is_alphanumeric());
    let lower = t.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "" | "you" | "thank you" | "thanks for watching" | "blank_audio" | "silence" | "music"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tap_downmixes_and_resamples_to_16k() {
        let tap = Tap::default();
        // 1 second of 48 kHz stereo: left 1.0, right -1.0 → mono 0.0.
        let frames = 48_000;
        let data: Vec<f32> = (0..frames).flat_map(|_| [1.0f32, -1.0f32]).collect();
        tap.push(&data, 2, 48_000);

        let out = tap.drain();
        // A second of 16 kHz mono, give or take the interpolation tail.
        assert!(
            (out.len() as i64 - 16_000).abs() <= 2,
            "got {} samples",
            out.len()
        );
        assert!(out.iter().all(|s| s.abs() < 1e-6), "channels should cancel");
    }

    #[test]
    fn cut_lands_on_the_quiet_frame() {
        let rate = WHISPER_RATE as usize;
        // 10s of tone with a silent gap at 8s.
        let mut samples = vec![0.5f32; 10 * rate];
        let gap = 8 * rate;
        for s in &mut samples[gap..gap + rate / 10] {
            *s = 0.0;
        }

        let cut = find_cut(&samples, 6 * rate, 12 * rate);
        // Within a frame or two of the gap, not at the hard maximum.
        assert!(
            cut > gap && cut < gap + rate / 5,
            "cut at {cut}, gap at {gap}"
        );
    }

    #[test]
    fn silence_and_filler_are_dropped() {
        assert!(is_silent(&[0.0; 1000]));
        assert!(!is_silent(&[0.5; 1000]));
        assert!(is_noise("[BLANK_AUDIO]"));
        assert!(is_noise("Thank you."));
        assert!(!is_noise("So the midterm covers chapters four through six."));
    }
}
