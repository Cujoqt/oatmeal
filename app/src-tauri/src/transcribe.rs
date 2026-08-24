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

/// Model for the *live* lane, where latency is the product.
///
/// `small.en` is not the most accurate model available, and it is here anyway:
/// the live panel is judged on how soon a phrase appears, and measurement says
/// the bigger models cost far more in lag than they return in words. Swapping
/// this for `large-v3-turbo` left accuracy flat — 96.3% either way on the
/// two-lane scenario — while worst-phrase latency went from about 550 ms to
/// about 2400 ms. The accurate model earns its keep on the saved transcript
/// instead; see `ACCURATE_WHISPER_FILE`.
pub const DEFAULT_WHISPER_FILE: &str = "ggml-small.en.bin";

/// Model for the passes nobody is waiting on: the background block pass during
/// a meeting, and the final transcript written at stop.
///
/// `large-v3-turbo` carries large-v3's full encoder with only four decoder
/// layers, so it reads names and technical vocabulary far better than `small.en`
/// at a fraction of large-v3's decode cost. Quantized to q8_0 — near-lossless,
/// and half the memory of the f16 build, which matters on a machine holding the
/// live model and the chat model at the same time. These passes have minutes of
/// slack, so the extra time costs nobody anything.
///
/// Unlike the `.en` models this one is multilingual, so short inputs need a
/// language passed rather than detected.
pub const ACCURATE_WHISPER_FILE: &str = "ggml-large-v3-turbo-q8_0.bin";

/// Default on-disk location for the live Whisper model.
pub fn default_model_path() -> PathBuf {
    model_dir().join(DEFAULT_WHISPER_FILE)
}

/// On-disk location for the model the accurate passes use.
pub fn accurate_model_path() -> PathBuf {
    model_dir().join(ACCURATE_WHISPER_FILE)
}

/// Same resolution order as `resolve_model_path`, but falling back to the
/// accurate model. `OATMEAL_MODEL` still overrides both, so a run can be pinned
/// to one model end to end when comparing them.
pub fn resolve_accurate_path(explicit: &str) -> PathBuf {
    if !explicit.trim().is_empty() {
        return PathBuf::from(explicit);
    }
    if let Ok(env) = std::env::var("OATMEAL_MODEL") {
        if !env.trim().is_empty() {
            return PathBuf::from(env);
        }
    }
    accurate_model_path()
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
    Transcriber::load_accurate(model_path)?.run(samples, language, Quality::Accurate, None)
}

/// How hard Whisper should work. Loading the model dominates either way; this
/// only changes decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    /// Beam search — the final pass over a finished recording, where a few extra
    /// seconds buy noticeably fewer errors on names and technical vocabulary.
    Accurate,
    /// Beam search, but sharing the machine. The same decoding as `Accurate`, run
    /// on a block of a meeting that is *still recording*, so it has to leave the
    /// live lane enough cores to keep up with the speaker. It has ten minutes of
    /// wall clock to chew through ten minutes of audio, so it can afford to be
    /// slow; live transcription falling behind is what cannot be afforded.
    Background,
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
        // Runs *alongside* the live lane, so it takes what is left over rather
        // than what it would like.
        Quality::Background => (cores / 3).max(1),
        Quality::Accurate => (cores - 1).max(2),
    }
}

impl Transcriber {
    /// Load the live model at `model_path` (empty for the resolution fallback).
    pub fn load(model_path: &str) -> Result<Self, String> {
        Self::load_at(resolve_model_path(model_path))
    }

    /// Load the model the accurate passes use (empty for the fallback).
    pub fn load_accurate(model_path: &str) -> Result<Self, String> {
        Self::load_at(resolve_accurate_path(model_path))
    }

    fn load_at(model: PathBuf) -> Result<Self, String> {
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
    /// Transcribe 16 kHz mono samples, with `context` — usually the text just
    /// before this audio — as the decoder's starting point.
    ///
    /// The live lane decodes a few seconds at a time, so each window arrives with
    /// no idea what was being said a moment ago. Handing Whisper the previous
    /// line keeps names and terms spelled consistently across a cut, which is
    /// where short windows otherwise lose to long ones.
    pub fn run(
        &self,
        samples: &[f32],
        language: Option<&str>,
        quality: Quality,
        context: Option<&str>,
    ) -> Result<Transcript, String> {
        if samples.is_empty() {
            return Err("no audio samples to transcribe".into());
        }

        let mut state = self
            .ctx
            .create_state()
            .map_err(|e| format!("create whisper state: {e}"))?;

        let strategy = match quality {
            Quality::Accurate | Quality::Background => SamplingStrategy::BeamSearch {
                beam_size: 5,
                patience: 0.0,
            },
            Quality::Fast => SamplingStrategy::Greedy { best_of: 1 },
        };
        let mut params = FullParams::new(strategy);
        // whisper.cpp re-decodes a window at rising temperatures whenever the
        // result looks unconfident — up to six passes over the same audio. On
        // quiet far-field speech that trips on nearly every window, and the
        // retries return hallucinations anyway, so the live lane pays six times
        // over for output it then has to filter. It is the difference between a
        // window costing 400 ms and 2.3 s, which is the difference between the
        // panel keeping up and falling minutes behind. The background pass keeps
        // the fallback: it has the time, and it writes the record that lasts.
        if matches!(quality, Quality::Fast) {
            params.set_temperature_inc(0.0);
        }
        // `"auto"` rather than `set_detect_language(true)`: whisper.cpp treats the
        // latter as *detect and stop*, returning zero segments once it has the
        // language — so auto-detect handed back an empty transcript for real
        // speech. `"auto"` runs the same detection and then decodes.
        params.set_language(Some(language.unwrap_or("auto")));
        params.set_n_threads(thread_budget(quality));
        // A null byte here would panic inside whisper-rs, which on the live worker
        // thread means no more transcript for the rest of the meeting.
        if let Some(ctx) = context.filter(|c| !c.trim().is_empty() && !c.contains('\0')) {
            params.set_initial_prompt(ctx);
        }
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

        let mut raw_segments = Vec::new();
        for seg in state.as_iter() {
            let s = seg.to_str_lossy().map(|c| c.into_owned()).unwrap_or_default();
            raw_segments.push(Segment {
                start_cs: seg.start_timestamp(),
                end_cs: seg.end_timestamp(),
                text: collapse_repeated_sentences(&s),
            });
        }
        let segments = collapse_repeated_segments(raw_segments);

        let mut text = String::new();
        for seg in &segments {
            let trimmed = seg.text.trim();
            if !trimmed.is_empty() {
                if !text.is_empty() {
                    text.push(' ');
                }
                text.push_str(trimmed);
            }
        }

        Ok(Transcript { segments, text })
    }
}

/// How many consecutive occurrences of the same text survive before the rest
/// are dropped — both of whole segments, and of sentences inside one segment.
///
/// whisper.cpp feeds each decoded window's text back in as the prompt for the
/// next window, with no escape hatch: once a window locks onto a short
/// phrase, that phrase primes every window after it to repeat itself, and the
/// repeat stays confident enough that the temperature-fallback retry never
/// fires to break the loop. Two real transcripts hit this — a quiet stretch
/// spiraled into "Okay." for the rest of a recording, and mid-sentence real
/// speech spiraled into "I make more money." for thirty-odd lines before
/// recovering on its own. Collapsing runs of identical consecutive segments
/// is the only lever available outside whisper.cpp itself.
const MAX_CONSECUTIVE_REPEATS: usize = 2;

/// Normalize a segment's text for repeat comparison: trims whitespace and a
/// trailing sentence terminator, folds case. Whisper's own repeats vary in
/// exactly this — "Okay." vs "okay" vs "Okay" — so an exact-string compare
/// would miss most real loops.
pub(crate) fn normalize_for_repeat_check(s: &str) -> String {
    s.trim()
        .trim_end_matches(|c: char| matches!(c, '.' | '!' | '?'))
        .to_lowercase()
}

/// Split `text` into sentences, keeping each terminator and the spacing before
/// the next sentence attached to the sentence it follows, so rejoining the
/// pieces reproduces the input exactly.
fn sentences(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        if matches!(bytes[i], b'.' | b'!' | b'?') {
            // Run past a whole "?!" or "..." rather than cutting inside it.
            while i + 1 < bytes.len() && matches!(bytes[i + 1], b'.' | b'!' | b'?') {
                i += 1;
            }
            let mut end = i + 1;
            while end < bytes.len() && bytes[end] == b' ' {
                end += 1;
            }
            out.push(&text[start..end]);
            start = end;
            i = end;
            continue;
        }
        i += 1;
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

/// Drop sentences beyond `MAX_CONSECUTIVE_REPEATS` identical consecutive
/// repeats *inside one segment*.
///
/// This is the shape the loop actually arrives in. Replaying the audio that
/// looped, with the phrase as the decoder's prompt, reproduces it every time
/// and returns all forty-odd repeats as the text of a **single** segment — so
/// `collapse_repeated_segments`, which compares one segment against the next,
/// has nothing to compare and lets the whole run through.
fn collapse_repeated_sentences(text: &str) -> String {
    let parts = sentences(text);
    let mut out = String::with_capacity(text.len());
    let mut last_norm = String::new();
    let mut run = 0usize;
    for part in parts {
        let norm = normalize_for_repeat_check(part);
        if norm.is_empty() {
            last_norm.clear();
            run = 0;
            out.push_str(part);
            continue;
        }
        if norm == last_norm {
            run += 1;
        } else {
            last_norm = norm;
            run = 1;
        }
        if run <= MAX_CONSECUTIVE_REPEATS {
            out.push_str(part);
        }
    }
    out
}

/// Drop segments beyond `MAX_CONSECUTIVE_REPEATS` identical consecutive
/// repeats, keeping segment order and timestamps for everything that survives.
fn collapse_repeated_segments(raw: Vec<Segment>) -> Vec<Segment> {
    let mut out = Vec::with_capacity(raw.len());
    let mut last_norm = String::new();
    let mut run = 0usize;
    for seg in raw {
        let norm = normalize_for_repeat_check(&seg.text);
        if norm.is_empty() {
            last_norm.clear();
            run = 0;
            out.push(seg);
            continue;
        }
        if norm == last_norm {
            run += 1;
        } else {
            last_norm = norm;
            run = 1;
        }
        if run <= MAX_CONSECUTIVE_REPEATS {
            out.push(seg);
        }
    }
    out
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
        let background = thread_budget(Quality::Background);

        // Never all of them, never fewer than two, and the live lane is the more
        // frugal of the pair.
        assert!(fast >= 2 && fast < cores.max(3), "fast budget {fast} of {cores}");
        assert!(accurate >= 2 && accurate < cores.max(3), "accurate budget {accurate}");
        assert!(fast <= accurate);

        // The background pass shares the machine with the live lane, so between
        // them they must not ask for every core — that is the whole reason it is
        // a separate setting and not just `Accurate`.
        assert!(background >= 1, "background budget {background}");
        assert!(background < accurate, "background must yield to the final pass");
        assert!(
            background + fast <= cores.max(3),
            "live ({fast}) + background ({background}) would oversubscribe {cores} cores"
        );
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

    fn seg(text: &str) -> Segment {
        Segment { start_cs: 0, end_cs: 0, text: text.to_string() }
    }

    #[test]
    fn collapses_silence_hallucination_loop() {
        // Reproduces the observed bug: whisper.cpp's beam-search pass locked
        // onto "Okay." over a quiet stretch and repeated it for the rest of
        // the recording.
        let raw: Vec<Segment> = std::iter::repeat_with(|| seg(" Okay.")).take(50).collect();
        let out = collapse_repeated_segments(raw);
        assert_eq!(out.len(), MAX_CONSECUTIVE_REPEATS);
    }

    #[test]
    fn collapses_mid_sentence_hallucination_loop() {
        // Reproduces the second observed case: real speech ("I make more
        // money. I buy more of those things.") spiraled into repeating
        // "I make more money." for ~30 lines before recovering on its own.
        let mut raw = vec![seg("Yeah."), seg("Confident is movies burgers pizzas.")];
        raw.extend(std::iter::repeat_with(|| seg("I make more money.")).take(30));
        raw.push(seg("Okay. Some goods I think we use."));

        let out = collapse_repeated_segments(raw);

        let repeats = out
            .iter()
            .filter(|s| normalize_for_repeat_check(&s.text) == "i make more money")
            .count();
        assert_eq!(repeats, MAX_CONSECUTIVE_REPEATS);
        // Everything before and after the loop survives untouched.
        assert_eq!(out.first().unwrap().text, "Yeah.");
        assert_eq!(out.last().unwrap().text, "Okay. Some goods I think we use.");
    }

    #[test]
    fn leaves_legitimate_short_runs_alone() {
        let raw = vec![seg("No."), seg("No."), seg("I mean it.")];
        let out = collapse_repeated_segments(raw);
        assert_eq!(out.len(), 3);
    }

    /// The shape the loop actually arrives in: one segment, forty-four repeats
    /// inside it. Replaying the audio that looped, primed with the phrase,
    /// reproduces this exactly.
    #[test]
    fn collapses_a_loop_that_arrives_inside_one_segment() {
        let looped = " I don't know.".repeat(44);
        let out = collapse_repeated_sentences(&looped);
        // The spacing whisper emitted is kept as-is; `run` trims it when it
        // joins segments into the transcript text.
        assert_eq!(out.trim(), "I don't know. I don't know.");
    }

    /// The loop usually ends mid-phrase, and the words after it are real speech
    /// that has to survive.
    #[test]
    fn speech_after_an_intra_segment_loop_survives() {
        let text = "Yeah. Okay. Okay. Okay. Okay. Okay. What the demand curve was.";
        assert_eq!(
            collapse_repeated_sentences(text),
            "Yeah. Okay. Okay. What the demand curve was."
        );
    }

    #[test]
    fn a_segment_with_no_repeats_is_returned_unchanged() {
        for text in [
            " Supply goes down, production costs have increased.",
            "Which curve, which way, and why?",
            "Wait... what? No!",
            "no terminator at all",
            "",
        ] {
            assert_eq!(collapse_repeated_sentences(text), text, "mangled: {text:?}");
        }
    }

    #[test]
    fn splitting_into_sentences_loses_nothing() {
        for text in [
            " One. Two.  Three",
            "Wait... what?! Fine.",
            "trailing space. ",
            "",
        ] {
            assert_eq!(sentences(text).concat(), text, "lossy: {text:?}");
        }
    }
}
