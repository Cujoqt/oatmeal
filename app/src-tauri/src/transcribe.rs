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
const WHISPER_RATE: u32 = 16_000;

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

/// Default on-disk location for the bundled/downloaded model.
pub fn default_model_path() -> PathBuf {
    // Kept alongside the user's data; the download step (later milestone) writes
    // here. `dirs`-free to avoid a dependency: use HOME.
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    Path::new(&home)
        .join("Library/Application Support/dev.oatmeal.app/models")
        .join("ggml-base.en.bin")
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
    let model = resolve_model_path(model_path);
    if !model.exists() {
        return Err(format!(
            "whisper model not found at {} — download a ggml model there first",
            model.display()
        ));
    }
    if samples.is_empty() {
        return Err("no audio samples to transcribe".into());
    }

    let ctx = WhisperContext::new_with_params(&model, WhisperContextParameters::default())
        .map_err(|e| format!("load whisper model: {e}"))?;
    let mut state = ctx
        .create_state()
        .map_err(|e| format!("create whisper state: {e}"))?;

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    if let Some(lang) = language {
        params.set_language(Some(lang));
    } else {
        params.set_detect_language(true);
    }
    let threads = std::thread::available_parallelism()
        .map(|n| n.get() as i32)
        .unwrap_or(4);
    params.set_n_threads(threads);
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
fn resample_linear(input: &[f32], src_rate: u32, dst_rate: u32) -> Vec<f32> {
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
