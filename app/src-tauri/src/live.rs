// M7 — live transcription.
//
// The batch path (M4/M6) transcribes the finished mix when the meeting stops.
// That is still the authoritative transcript. This module adds the *preview*
// that makes the app feel alive: while the lanes record, a worker thread keeps a
// Whisper context warm and transcribes the audio in chunks as it arrives,
// emitting each finished chunk to the UI as a Tauri event.
//
// Shape of the pipeline:
//
//   mic lane    ─┐
//                ├─►  LiveTap (16 kHz mono, per-lane queues)
//   sysaudio    ─┘         │
//                          │  drained + mixed every 250 ms
//                          ▼
//                    LiveTranscriber worker
//                      • buffers until it has >= MIN_CHUNK_SECS
//                      • cuts at the quietest point near the tail so we
//                        almost never split a word
//                      • runs whisper.cpp on that chunk
//                      • emits `oatmeal://live-segment` per line
//
// Chunks are transcribed independently and never rewritten, so the UI can append
// and nothing ever flickers. The cost is slightly less context than a full-file
// pass — which is exactly why the final transcript is still produced from the
// whole mix at stop.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Runtime};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::transcribe::{resample_linear, resolve_model_path, WHISPER_RATE};

/// Event carrying one finished live transcript line.
pub const EVENT_SEGMENT: &str = "oatmeal://live-segment";
/// Event carrying worker status changes (`loading`, `listening`, `error`).
pub const EVENT_STATE: &str = "oatmeal://live-state";

/// How often the worker drains the tap.
const POLL_MS: u64 = 250;
/// Don't transcribe until we have at least this much audio — shorter chunks give
/// Whisper too little context and produce noise.
const MIN_CHUNK_SECS: f32 = 5.0;
/// Cut unconditionally once a chunk gets this long, even mid-word.
const MAX_CHUNK_SECS: f32 = 14.0;
/// Window used when hunting for a quiet split point.
const SPLIT_WINDOW_MS: usize = 120;
/// Chunks quieter than this RMS are treated as silence and skipped — no point
/// burning a Whisper pass on room tone (and it stops "you" / "thanks" hallucinations).
const SILENCE_RMS: f32 = 0.0025;

fn secs_to_samples(secs: f32) -> usize {
    (secs * WHISPER_RATE as f32) as usize
}

/// One live transcript line. Times are centiseconds from the start of the
/// meeting, matching the batch `transcribe::Segment` units.
#[derive(Debug, Clone, Serialize)]
pub struct LiveSegment {
    pub start_cs: i64,
    pub end_cs: i64,
    pub text: String,
}

/// Worker status pushed to the UI so it can show "starting", "listening", or an
/// error without guessing.
#[derive(Debug, Clone, Serialize)]
pub struct LiveState {
    pub state: &'static str,
    pub message: String,
}

// ── the tap ─────────────────────────────────────────────────────────────────

/// Shared sink the capture lanes push 16 kHz mono audio into.
///
/// Each lane gets its own queue: they run on independent realtime threads and a
/// lane may be missing entirely (denied permission, no mic). The worker drains
/// both and mixes them the same way `session::mix` does for the final file —
/// sum, zero-pad the shorter, clamp.
#[derive(Default)]
pub struct LiveTap {
    mic: Mutex<Vec<f32>>,
    sys: Mutex<Vec<f32>>,
}

impl LiveTap {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Append 16 kHz mono samples from the microphone lane.
    pub fn push_mic(&self, samples: &[f32]) {
        if let Ok(mut q) = self.mic.lock() {
            q.extend_from_slice(samples);
        }
    }

    /// Append 16 kHz mono samples from the system-audio lane.
    pub fn push_sys(&self, samples: &[f32]) {
        if let Ok(mut q) = self.sys.lock() {
            q.extend_from_slice(samples);
        }
    }

    /// Take everything queued so far, mixed to a single mono track.
    pub fn drain_mixed(&self) -> Vec<f32> {
        let mic = self
            .mic
            .lock()
            .map(|mut q| std::mem::take(&mut *q))
            .unwrap_or_default();
        let sys = self
            .sys
            .lock()
            .map(|mut q| std::mem::take(&mut *q))
            .unwrap_or_default();

        if sys.is_empty() {
            return mic;
        }
        if mic.is_empty() {
            return sys;
        }
        let n = mic.len().max(sys.len());
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let s = mic.get(i).copied().unwrap_or(0.0) + sys.get(i).copied().unwrap_or(0.0);
            out.push(s.clamp(-1.0, 1.0));
        }
        out
    }
}

/// Convert one interleaved capture callback into 16 kHz mono, ready for the tap.
///
/// Called from realtime audio threads, so it does exactly one pass to mono and
/// one linear resample — no DSP crate, no filtering. Good enough for a preview;
/// the final transcript resamples the same way from the on-disk WAV.
pub fn interleaved_to_mono_16k<I: Iterator<Item = f32>>(
    samples: I,
    channels: u16,
    sample_rate: u32,
) -> Vec<f32> {
    let ch = channels.max(1) as usize;
    let mono: Vec<f32> = if ch == 1 {
        samples.collect()
    } else {
        let mut out = Vec::new();
        let mut acc = 0.0f32;
        let mut n = 0usize;
        for s in samples {
            acc += s;
            n += 1;
            if n == ch {
                out.push(acc / ch as f32);
                acc = 0.0;
                n = 0;
            }
        }
        out
    };
    resample_linear(&mono, sample_rate, WHISPER_RATE)
}

// ── the worker ──────────────────────────────────────────────────────────────

/// A running live-transcription worker. Dropping it (or calling `stop`) ends the
/// thread after the in-flight chunk finishes.
pub struct LiveTranscriber {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl LiveTranscriber {
    /// Spawn the worker. Never fails loudly: a missing/unloadable model is
    /// reported to the UI as an `error` state and the meeting keeps recording,
    /// because losing the live preview must not lose the recording.
    pub fn start<R: Runtime>(
        app: AppHandle<R>,
        tap: Arc<LiveTap>,
        model_path: String,
        language: Option<String>,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let handle = std::thread::Builder::new()
            .name("oatmeal-live".into())
            .spawn(move || run_worker(app, tap, thread_stop, model_path, language))
            .ok();
        Self { stop, handle }
    }

    /// Signal the worker to finish and wait for it.
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

impl Drop for LiveTranscriber {
    fn drop(&mut self) {
        self.signal_and_join();
    }
}

fn emit_state<R: Runtime>(app: &AppHandle<R>, state: &'static str, message: impl Into<String>) {
    let _ = app.emit(
        EVENT_STATE,
        LiveState {
            state,
            message: message.into(),
        },
    );
}

fn run_worker<R: Runtime>(
    app: AppHandle<R>,
    tap: Arc<LiveTap>,
    stop: Arc<AtomicBool>,
    model_path: String,
    language: Option<String>,
) {
    emit_state(&app, "loading", "Warming up the transcription model…");

    let model = resolve_model_path(&model_path);
    let ctx = match WhisperContext::new_with_params(&model, WhisperContextParameters::default()) {
        Ok(c) => c,
        Err(e) => {
            emit_state(&app, "error", format!("live transcription unavailable: {e}"));
            return;
        }
    };
    let mut state = match ctx.create_state() {
        Ok(s) => s,
        Err(e) => {
            emit_state(&app, "error", format!("live transcription unavailable: {e}"));
            return;
        }
    };

    let threads = std::thread::available_parallelism()
        .map(|n| n.get() as i32)
        .unwrap_or(4);

    emit_state(&app, "listening", "Listening…");

    let min_chunk = secs_to_samples(MIN_CHUNK_SECS);
    let max_chunk = secs_to_samples(MAX_CHUNK_SECS);

    let mut pending: Vec<f32> = Vec::new();
    // Samples already emitted — the time offset for the next chunk.
    let mut consumed: usize = 0;

    loop {
        let stopping = stop.load(Ordering::SeqCst);
        pending.extend_from_slice(&tap.drain_mixed());

        // While recording, wait for a full chunk. On stop, flush whatever is left
        // so the last few seconds still show up before the batch pass replaces it.
        let cut = if pending.len() >= min_chunk {
            Some(choose_cut(&pending, min_chunk, max_chunk))
        } else if stopping && !pending.is_empty() {
            Some(pending.len())
        } else {
            None
        };

        if let Some(cut) = cut {
            let chunk: Vec<f32> = pending.drain(..cut).collect();
            let offset_cs = (consumed as i64 * 100) / WHISPER_RATE as i64;
            consumed += chunk.len();

            if rms(&chunk) >= SILENCE_RMS {
                match transcribe_chunk(&mut state, &chunk, language.as_deref(), threads) {
                    Ok(segments) => {
                        for mut seg in segments {
                            seg.start_cs += offset_cs;
                            seg.end_cs += offset_cs;
                            let _ = app.emit(EVENT_SEGMENT, seg);
                        }
                    }
                    Err(e) => eprintln!("[oatmeal] live chunk failed: {e}"),
                }
            }
            // Loop again immediately rather than sleeping: transcription may have
            // taken longer than a poll interval, so more audio is likely already
            // waiting — and on stop this drains the backlog chunk by chunk until
            // `pending` is empty and the `cut == None` path breaks us out.
            continue;
        }

        if stopping {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
    }
}

/// Run one chunk through the warm Whisper state.
fn transcribe_chunk(
    state: &mut whisper_rs::WhisperState,
    samples: &[f32],
    language: Option<&str>,
    threads: i32,
) -> Result<Vec<LiveSegment>, String> {
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    match language {
        Some(lang) => params.set_language(Some(lang)),
        None => params.set_detect_language(true),
    }
    params.set_n_threads(threads);
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    // A live chunk is a fragment, not a document — don't let Whisper carry a bad
    // guess forward, and don't let it invent text for near-silence.
    params.set_no_context(true);
    params.set_single_segment(false);
    params.set_suppress_blank(true);

    state
        .full(params, samples)
        .map_err(|e| format!("whisper inference failed: {e}"))?;

    let mut out = Vec::new();
    for seg in state.as_iter() {
        let text = seg
            .to_str_lossy()
            .map(|c| c.into_owned())
            .unwrap_or_default();
        if text.trim().is_empty() {
            continue;
        }
        out.push(LiveSegment {
            start_cs: seg.start_timestamp(),
            end_cs: seg.end_timestamp(),
            text,
        });
    }
    Ok(out)
}

/// Pick where to end the next chunk.
///
/// Anywhere in `[min, min(len, max)]` is legal; we take the quietest short window
/// in that range so the cut lands in a pause rather than mid-syllable. If the
/// buffer already exceeds `max` we cut at `max` regardless.
fn choose_cut(buf: &[f32], min: usize, max: usize) -> usize {
    let hard = buf.len().min(max);
    if hard <= min {
        return hard;
    }
    let win = (WHISPER_RATE as usize * SPLIT_WINDOW_MS) / 1000;
    if hard - min <= win {
        return hard;
    }

    let mut best = hard;
    let mut best_energy = f32::MAX;
    let mut start = min;
    while start + win <= hard {
        let energy: f32 = buf[start..start + win].iter().map(|s| s * s).sum();
        if energy < best_energy {
            best_energy = energy;
            best = start + win / 2;
        }
        start += win / 2;
    }
    best
}

/// Root-mean-square level of a chunk, used as the silence gate.
fn rms(buf: &[f32]) -> f32 {
    if buf.is_empty() {
        return 0.0;
    }
    let sum: f32 = buf.iter().map(|s| s * s).sum();
    (sum / buf.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tap_mixes_both_lanes_and_pads_the_shorter() {
        let tap = LiveTap::new();
        tap.push_mic(&[0.5, 0.5, 0.5]);
        tap.push_sys(&[0.2]);
        let mixed = tap.drain_mixed();
        assert_eq!(mixed.len(), 3);
        assert!((mixed[0] - 0.7).abs() < 1e-6);
        assert!((mixed[1] - 0.5).abs() < 1e-6);
        // Draining is destructive.
        assert!(tap.drain_mixed().is_empty());
    }

    #[test]
    fn tap_passes_a_lone_lane_through_untouched() {
        let tap = LiveTap::new();
        tap.push_sys(&[0.1, -0.1]);
        assert_eq!(tap.drain_mixed(), vec![0.1, -0.1]);
    }

    #[test]
    fn tap_clamps_when_both_lanes_are_loud() {
        let tap = LiveTap::new();
        tap.push_mic(&[0.9, -0.9]);
        tap.push_sys(&[0.9, -0.9]);
        assert_eq!(tap.drain_mixed(), vec![1.0, -1.0]);
    }

    #[test]
    fn interleaved_downmixes_channels_and_resamples() {
        // 0.1s of 48 kHz stereo where the channels cancel.
        let frames = 4_800;
        let data: Vec<f32> = (0..frames).flat_map(|_| [0.5f32, -0.5f32]).collect();
        let mono = interleaved_to_mono_16k(data.into_iter(), 2, 48_000);
        assert!((mono.len() as i64 - 1_600).abs() <= 2, "len {}", mono.len());
        let peak = mono.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        assert!(peak < 0.05, "expected cancellation, peak {peak}");
    }

    #[test]
    fn interleaved_mono_at_target_rate_is_a_passthrough() {
        let x = vec![0.1, -0.2, 0.3];
        assert_eq!(
            interleaved_to_mono_16k(x.clone().into_iter(), 1, WHISPER_RATE),
            x
        );
    }

    #[test]
    fn cut_lands_in_the_quiet_gap() {
        // Realistic sizes: 5 s minimum, 14 s maximum, 12 s of audio that is loud
        // everywhere except a one-second pause centred on 9 s.
        let min = secs_to_samples(5.0);
        let max = secs_to_samples(14.0);
        let mut buf = vec![0.8f32; secs_to_samples(12.0)];
        let gap = secs_to_samples(8.5)..secs_to_samples(9.5);
        for s in &mut buf[gap] {
            *s = 0.0;
        }

        let cut = choose_cut(&buf, min, max);
        // The cut must land inside the pause, not in the surrounding speech.
        assert!(
            cut >= secs_to_samples(8.5) && cut <= secs_to_samples(9.5),
            "cut at {:.2}s, expected inside the 8.5–9.5s pause",
            cut as f32 / WHISPER_RATE as f32
        );
    }

    #[test]
    fn cut_falls_back_to_the_hard_limit_when_audio_is_uniformly_loud() {
        // No pause anywhere: we still have to cut, and never past `max`.
        let min = secs_to_samples(5.0);
        let max = secs_to_samples(14.0);
        let buf = vec![0.8f32; secs_to_samples(30.0)];
        let cut = choose_cut(&buf, min, max);
        assert!(cut >= min && cut <= max, "cut {cut} out of range");
    }

    #[test]
    fn cut_never_exceeds_the_hard_maximum() {
        let buf = vec![0.5f32; 50_000];
        let cut = choose_cut(&buf, 1_000, 10_000);
        assert!(cut <= 10_000, "cut {cut}");
    }

    #[test]
    fn cut_returns_buffer_length_when_short() {
        let buf = vec![0.5f32; 500];
        assert_eq!(choose_cut(&buf, 1_000, 10_000), 500);
    }

    #[test]
    fn rms_gate_separates_silence_from_speech() {
        assert!(rms(&vec![0.0; 1_000]) < SILENCE_RMS);
        assert!(rms(&vec![0.2; 1_000]) > SILENCE_RMS);
        assert_eq!(rms(&[]), 0.0);
    }
}
