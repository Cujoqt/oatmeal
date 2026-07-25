// M4 — transcription.
//
// Takes a WAV written by the mic (M2) or system-audio (M3) lane and runs it
// through whisper.cpp (via whisper-rs) fully on-device. Whisper wants 16 kHz
// mono f32; our lanes record at the device / SCK native rate (44.1 or 48 kHz),
// so we down-mix to mono and linearly resample to 16 kHz first.
//
// The model is a ggml `.bin` file on disk. Resolution order for its path:
//   1. explicit argument, if non-empty
//   2. the `OATMEAL_MODEL` environment variable
//   3. `<models_dir>/ggml-base.en.bin` (see `default_model_path`)
// Downloading the model when absent is handled elsewhere; here a missing model
// is a plain error with the resolved path so the caller can fetch it.

use std::path::{Path, PathBuf};

use serde::Serialize;
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters,
};

/// Target sample rate for Whisper.
pub const WHISPER_RATE: u32 = 16_000;

/// One transcript line with its time span (centiseconds, as Whisper reports).
#[derive(Debug, Clone, Serialize)]
pub struct Segment {
    /// Start time in centiseconds (10 ms units).
    pub start_cs: i64,
    /// End time in centiseconds.
    pub end_cs: i64,
    pub text: String,
}

/// A full transcription result: the segments plus the joined plain text.
#[derive(Debug, Clone, Serialize)]
pub struct Transcript {
    pub segments: Vec<Segment>,
    pub text: String,
}

/// Resolve which model file to use, given a possibly-empty explicit path.
pub fn resolve_model_path(explicit: &str) -> PathBuf {
    if !explicit.trim().is_empty() {
        return PathBuf::from(explicit);
    }
    if let Ok(env) = std::env::var("OATMEAL_MODEL") {
        if !env.trim().is_empty() {
            return PathBuf::from(env);
        }
    }
    default_model_path()
}

/// Directory holding every downloaded model (Whisper and the chat model).
pub fn model_dir() -> PathBuf {
    // `dirs`-free to avoid a dependency: use HOME.
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    Path::new(&home).join("Library/Application Support/dev.oatmeal.app/models")
}

/// Default Whisper model file. `small.en` transcribes technical speech markedly
/// better than `base.en` while still running several times faster than realtime
/// on Apple silicon, which live transcription depends on.
pub const DEFAULT_WHISPER_FILE: &str = "ggml-small.en.bin";

/// Default on-disk location for the downloaded Whisper model.
pub fn default_model_path() -> PathBuf {
    model_dir().join(DEFAULT_WHISPER_FILE)
}

/// Transcribe a WAV file. `language` is a Whisper language code ("en"), or `None`
/// to auto-detect. `model_path` may be empty to use the resolution fallback.
pub fn transcribe_wav(
    model_path: &str,
    wav_path: &Path,
    language: Option<&str>,
) -> Result<Transcript, String> {
    let model = resolve_model_path(model_path);
    if !model.exists() {
        return Err(format!(
            "whisper model not found at {} — download a ggml model there first",
            model.display()
        ));
    }

    let samples = load_wav_mono_16k(wav_path)?;
    if samples.is_empty() {
        return Err("audio file contained no samples".into());
    }
    transcribe_samples(model_path, &samples, language)
}

/// Transcribe already-prepared 16 kHz mono f32 samples (e.g. a mix of the mic and
/// system-audio lanes). `model_path` may be empty to use the resolution fallback.
pub fn transcribe_samples(
    model_path: &str,
    samples: &[f32],
    language: Option<&str>,
) -> Result<Transcript, String> {
    Transcriber::load(model_path)?.run(samples, language, Quality::Accurate)
}

/// How hard Whisper should work. Loading the model dominates either way; this
/// only changes decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    /// Beam search — the final pass over a finished recording, where a few extra
    /// seconds buy noticeably fewer errors on names and technical vocabulary.
    Accurate,
    /// Greedy — the live pass, which has to keep ahead of the speaker.
    Fast,
}

/// A loaded Whisper model, reusable across many calls.
///
/// Loading the model costs hundreds of milliseconds and half a gigabyte, so live
/// transcription holds one of these for the length of a meeting rather than
/// paying that per chunk.
pub struct Transcriber {
    ctx: WhisperContext,
}

/// How many CPU threads a decode may use.
///
/// The live lane runs *while* the meeting is being recorded: audio callbacks have
/// to hit their deadlines and the window has to stay responsive. Handing whisper
/// every core starves both, which shows up as a stuttering UI and dropped audio.
/// The offline pass runs after the meeting, with nothing else competing, but still
/// leaves one core so the app can redraw.
fn thread_budget(quality: Quality) -> i32 {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4) as i32;
    match quality {
        Quality::Fast => (cores - 2).max(2),
        Quality::Accurate => (cores - 1).max(2),
    }
}

impl Transcriber {
    /// Load the model at `model_path` (empty for the resolution fallback).
    pub fn load(model_path: &str) -> Result<Self, String> {
        let model = resolve_model_path(model_path);
        if !model.exists() {
            return Err(format!(
                "whisper model not found at {} — download a ggml model there first",
                model.display()
            ));
        }
        let ctx = WhisperContext::new_with_params(&model, WhisperContextParameters::default())
            .map_err(|e| format!("load whisper model: {e}"))?;
        Ok(Self { ctx })
    }

    /// Transcribe 16 kHz mono samples.
    pub fn run(
        &self,
        samples: &[f32],
        language: Option<&str>,
        quality: Quality,
    ) -> Result<Transcript, String> {
        if samples.is_empty() {
            return Err("no audio samples to transcribe".into());
        }

        let mut state = self
            .ctx
            .create_state()
            .map_err(|e| format!("create whisper state: {e}"))?;

        let strategy = match quality {
            Quality::Accurate => SamplingStrategy::BeamSearch {
                beam_size: 5,
                patience: 0.0,
            },
            Quality::Fast => SamplingStrategy::Greedy { best_of: 1 },
        };
        let mut params = FullParams::new(strategy);
        if let Some(lang) = language {
            params.set_language(Some(lang));
        } else {
            params.set_detect_language(true);
        }
        params.set_n_threads(thread_budget(quality));
        // Whisper loves to emit "[BLANK_AUDIO]" and hallucinate stock phrases over
        // silence. Suppressing non-speech tokens cuts most of that.
        params.set_suppress_blank(true);
        params.set_suppress_nst(true);
        // whisper.cpp is noisy on stdout; silence what we can.
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        state
            .full(params, samples)
            .map_err(|e| format!("whisper inference failed: {e}"))?;

        let mut segments = Vec::new();
        let mut text = String::new();
        for seg in state.as_iter() {
            let s = seg.to_str_lossy().map(|c| c.into_owned()).unwrap_or_default();
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                if !text.is_empty() {
                    text.push(' ');
                }
                text.push_str(trimmed);
            }
            segments.push(Segment {
                start_cs: seg.start_timestamp(),
                end_cs: seg.end_timestamp(),
                text: s,
            });
        }

        Ok(Transcript { segments, text })
    }
}

/// Read a WAV (i16 or f32, any channel count / sample rate) and return 16 kHz
/// mono f32 samples ready for Whisper.
pub fn load_wav_mono_16k(path: &Path) -> Result<Vec<f32>, String> {
    let mut reader =
        hound::WavReader::open(path).map_err(|e| format!("open wav {}: {e}", path.display()))?;
    let spec = reader.spec();
    let channels = spec.channels.max(1) as usize;

    // Read to interleaved f32 in -1.0..=1.0 regardless of on-disk format.
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 / max).map_err(|e| e.to_string()))
                .collect::<Result<Vec<_>, _>>()?
        }
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .map(|s| s.map_err(|e| e.to_string()))
            .collect::<Result<Vec<_>, _>>()?,
    };

    // Down-mix to mono by averaging channels.
    let mono: Vec<f32> = if channels <= 1 {
        interleaved
    } else {
        interleaved
            .chunks(channels)
            .map(|frame| frame.iter().sum::<f32>() / channels as f32)
            .collect()
    };

    Ok(resample_linear(&mono, spec.sample_rate, WHISPER_RATE))
}

/// Linear-interpolation resampler. Good enough for speech going into Whisper;
/// avoids pulling in a DSP crate for an integer-ish downsample.
pub fn resample_linear(input: &[f32], src_rate: u32, dst_rate: u32) -> Vec<f32> {
    if src_rate == dst_rate || input.is_empty() {
        return input.to_vec();
    }
    let ratio = dst_rate as f64 / src_rate as f64;
    let out_len = ((input.len() as f64) * ratio).round() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src_pos = i as f64 / ratio;
        let idx = src_pos.floor() as usize;
        let frac = (src_pos - idx as f64) as f32;
        let a = input.get(idx).copied().unwrap_or(0.0);
        let b = input.get(idx + 1).copied().unwrap_or(a);
        out.push(a + (b - a) * frac);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_decoding_leaves_cores_for_audio_and_ui() {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4) as i32;
        let fast = thread_budget(Quality::Fast);
        let accurate = thread_budget(Quality::Accurate);

        // Never all of them, never fewer than two, and the live lane is the more
        // frugal of the pair.
        assert!(fast >= 2 && fast < cores.max(3), "fast budget {fast} of {cores}");
        assert!(accurate >= 2 && accurate < cores.max(3), "accurate budget {accurate}");
        assert!(fast <= accurate);
    }

    #[test]
    fn resample_passthrough_when_rates_match() {
        let x = vec![0.1, -0.2, 0.3];
        assert_eq!(resample_linear(&x, 16_000, 16_000), x);
    }

    #[test]
    fn resample_48k_to_16k_thirds_the_length() {
        let input = vec![0.0f32; 4_800]; // 0.1s @ 48k
        let out = resample_linear(&input, 48_000, 16_000);
        // 4800 * (16000/48000) = 1600, ±1 for rounding.
        assert!((out.len() as i64 - 1_600).abs() <= 1, "got {}", out.len());
    }

    #[test]
    fn resample_interpolates_midpoint() {
        // Upsample 2 -> 4 samples; the value between 0.0 and 1.0 should be ~0.5.
        let out = resample_linear(&[0.0, 1.0], 2, 4);
        assert!(out.len() >= 3);
        assert!((out[1] - 0.5).abs() < 0.2, "midpoint was {}", out[1]);
    }

    #[test]
    fn wav_roundtrip_downmixes_and_resamples() {
        // Write a 2-channel 48k i16 WAV, read it back as 16k mono.
        let dir = std::env::temp_dir();
        let path = dir.join(format!("oatmeal-test-{}.wav", std::process::id()));
        {
            let spec = hound::WavSpec {
                channels: 2,
                sample_rate: 48_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let mut w = hound::WavWriter::create(&path, spec).unwrap();
            // 4800 stereo frames (0.1s): left = +half, right = -half -> mono ~0.
            for _ in 0..4_800 {
                w.write_sample(16_000i16).unwrap();
                w.write_sample(-16_000i16).unwrap();
            }
            w.finalize().unwrap();
        }
        let mono = load_wav_mono_16k(&path).unwrap();
        std::fs::remove_file(&path).ok();

        // ~0.1s at 16k mono.
        assert!((mono.len() as i64 - 1_600).abs() <= 2, "len {}", mono.len());
        // L+R average cancels to near silence.
        let peak = mono.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        assert!(peak < 0.05, "expected near-silent downmix, peak {peak}");
    }
}
