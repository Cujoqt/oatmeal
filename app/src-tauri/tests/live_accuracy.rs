// Measures the *live* transcription lane: how accurate it is and how far behind
// the speaker it runs. The offline pass has `e2e_transcribe`; this is its
// counterpart for the panel people actually watch during a meeting.
//
// Ignored by default — it needs the Whisper model on disk, `say` to synthesise
// speech, and it plays the audio through in real time, so it takes minutes.
//
//     cargo test --test live_accuracy -- --ignored --nocapture
//
// What it does: synthesises known sentences with `say`, feeds them into a real
// `Tap`/`LiveSession` pair at wall-clock speed (100 ms at a time, exactly as a
// capture callback would), and scores the emitted lines against the text it fed
// in. Three scenarios, because the complaints are different:
//
//   loud       — a speaker close to the mic, quiet room. The control.
//   quiet      — someone across the table: speech 25 dB down, over a room-tone
//                noise floor, so the signal-to-noise ratio is what actually
//                degrades. Simply scaling clean synthetic speech does not
//                reproduce this — it stays perfectly clean, just smaller.
//   two lanes  — mic *and* system audio both feeding the tap, which is how every
//                real meeting runs.
//
// Accuracy is 1 - WER against the sentences that were fed in. Latency is measured
// against the wall clock only, never against the worker's own timestamps, so the
// metric can't be fooled by the timeline bug it is meant to catch.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use oatmeal_app_lib::live::{LiveSession, Tap};
use oatmeal_app_lib::transcribe::WHISPER_RATE;

/// Accuracy the live lane has to reach before this is considered fixed.
const TARGET_ACCURACY: f64 = 0.90;
/// How long after a phrase is finished its text may take to appear. This is the
/// number that decides whether the panel feels live: text lands this long after
/// the speaker stops, every phrase, for the whole meeting.
const TARGET_PHRASE_LAG_MS: i64 = 1_500;
/// How long a speaker may talk before the *first* words appear on screen.
///
/// Necessarily at least as long as their opening sentence: a window is only cut
/// once the phrase in it has ended, so a four-second sentence cannot show up in
/// three. Sub-second first text is what streaming partial hypotheses buy —
/// re-decoding the in-progress window and rewriting the last line as it firms
/// up — which is a different feature from this one, and not built here.
const TARGET_FIRST_TEXT_MS: u64 = 5_000;
/// How long after the last word is spoken its text may take to arrive.
const TARGET_TAIL_LAG_MS: i64 = 3_000;
/// How far ahead of real elapsed audio the worker's own clock may run. Running
/// *behind* is normal — the last line starts before the audio ends — but running
/// ahead means it is being handed more samples than were recorded, which is what
/// appending two capture lanes into one buffer does.
const TARGET_TIMELINE_AHEAD: f64 = 1.15;

/// Sentences to speak. Plain meeting English with a few proper nouns and
/// numbers, since those are what a live panel gets wrong first.
const SCRIPT: &[&str] = &[
    "Good morning everyone, thanks for joining the weekly sync.",
    "The migration to the new database finished on Tuesday afternoon.",
    "We are still seeing about four hundred failed requests per hour.",
    "Priya is going to look at the retry logic before Thursday.",
    "Can we push the launch date back by two weeks?",
    "I think that is reasonable given where the testing stands.",
    "The budget for next quarter has not been approved yet.",
    "Let us follow up on that in the design review tomorrow.",
];

/// Synthesise one sentence to 16 kHz mono f32 via `say`, then read it back.
fn speak(text: &str, idx: usize) -> Vec<f32> {
    let path = std::env::temp_dir().join(format!("oatmeal-live-{}-{idx}.wav", std::process::id()));
    let status = std::process::Command::new("say")
        .args(["--data-format=LEF32@16000", "--file-format=WAVE", "-r", "175", "-o"])
        .arg(&path)
        .arg(text)
        .status()
        .expect("spawn say");
    assert!(status.success(), "say failed for {text:?}");
    let samples = oatmeal_app_lib::transcribe::load_wav_mono_16k(&path).expect("read say output");
    std::fs::remove_file(&path).ok();
    samples
}

/// The whole script as one stream, with a short pause between sentences so the
/// chunker has somewhere natural to cut. Normalised to a realistic close-mic
/// peak so the scenarios below differ only by what we do to it.
fn script_audio() -> Vec<f32> {
    let gap = vec![0.0f32; (WHISPER_RATE as f32 * 0.45) as usize];
    let mut all = Vec::new();
    for (i, line) in SCRIPT.iter().enumerate() {
        all.extend_from_slice(&speak(line, i));
        all.extend_from_slice(&gap);
    }
    let peak = all.iter().fold(0.0f32, |m, &s| m.max(s.abs())).max(1e-6);
    for s in &mut all {
        *s = *s / peak * 0.6;
    }
    all
}

/// Deterministic broadband noise at a given amplitude — a stand-in for room tone,
/// fan hum and mic self-noise. A fixed LCG keeps runs comparable.
fn room_tone(len: usize, amplitude: f32, seed: u64) -> Vec<f32> {
    let mut x = seed | 1;
    (0..len)
        .map(|_| {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let u = ((x >> 33) as f32 / (1u64 << 31) as f32) - 1.0;
            u * amplitude
        })
        .collect()
}

/// Speech at `gain`, mixed over a fixed noise floor. This is what "not yelling"
/// does to the audio: the level drops but the room does not get any quieter, so
/// the signal-to-noise ratio collapses.
fn quiet_speaker(speech: &[f32], gain: f32, noise: f32) -> Vec<f32> {
    let tone = room_tone(speech.len(), noise, 0x0A7EA1);
    speech
        .iter()
        .zip(tone.iter())
        .map(|(s, n)| (s * gain + n).clamp(-1.0, 1.0))
        .collect()
}

/// One emitted line with the wall-clock moment it arrived.
struct Observed {
    at_ms: u64,
    wall_ms: u64,
    text: String,
}

/// Result of running one scenario.
struct Run {
    accuracy: f64,
    /// Wall-clock ms from the first sample being pushed to the first text.
    first_text_ms: u64,
    /// Wall-clock ms from the last sample being pushed to the last text.
    tail_lag_ms: i64,
    /// Worst gap between a phrase's audio ending and its text appearing.
    worst_phrase_lag_ms: i64,
    /// The worker's final timestamp over the real audio duration; 1.0 is correct.
    timeline_ratio: f64,
    lines: usize,
    heard: String,
    /// (audio position, wall clock, text) per emitted line.
    timings: Vec<(u64, u64, String)>,
}

/// Feed `lanes` into one live session at wall-clock speed and collect what comes
/// back out. Every lane pushes into the same tap, exactly as `begin_session` wires
/// the mic and system-audio recorders.
fn run_scenario(lanes: &[Vec<f32>]) -> Run {
    let (tap, sinks) = Tap::with_lanes(lanes.len());
    let observed: Arc<Mutex<Vec<Observed>>> = Arc::new(Mutex::new(Vec::new()));
    let started = Instant::now();
    let first_text = Arc::new(AtomicU64::new(u64::MAX));

    let sink = observed.clone();
    let first = first_text.clone();
    let session = LiveSession::start(tap.clone(), String::new(), Some("en".into()), move |line| {
        let wall_ms = started.elapsed().as_millis() as u64;
        first.fetch_min(wall_ms, Ordering::SeqCst);
        sink.lock().unwrap().push(Observed {
            at_ms: line.at_ms,
            wall_ms,
            text: line.text,
        });
    })
    .expect("live session should start");

    // Push 100 ms at a time from every lane, sleeping to keep wall clock in step
    // with audio time — the whole point is to measure how far behind we run.
    let step = (WHISPER_RATE / 10) as usize;
    let longest = lanes.iter().map(|l| l.len()).max().unwrap_or(0);
    let mut pos = 0;
    while pos < longest {
        let end = (pos + step).min(longest);
        for (lane, sink) in lanes.iter().zip(sinks.iter()) {
            if pos < lane.len() {
                let stop = end.min(lane.len());
                sink.push(&lane[pos..stop], 1, WHISPER_RATE);
            }
        }
        let target = Duration::from_millis((end as u64 * 1000) / WHISPER_RATE as u64);
        let elapsed = started.elapsed();
        if target > elapsed {
            std::thread::sleep(target - elapsed);
        }
        pos = end;
    }
    let audio_done_ms = started.elapsed().as_millis() as u64;
    let audio_secs = longest as f64 / WHISPER_RATE as f64;

    session.stop();

    let lines = observed.lock().unwrap();
    let heard = lines
        .iter()
        .map(|l| l.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let last_wall = lines.last().map(|l| l.wall_ms).unwrap_or(audio_done_ms);
    let last_at = lines.last().map(|l| l.at_ms).unwrap_or(0);

    // Audio is fed in real time from t=0, so the audio of a line covering
    // [at_ms, next.at_ms) is all in hand at wall clock next.at_ms. How long after
    // that its text appeared is the lag the person watching actually feels.
    let worst_phrase_lag_ms = lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let audio_complete = lines.get(i + 1).map(|n| n.at_ms).unwrap_or(audio_done_ms);
            l.wall_ms as i64 - audio_complete as i64
        })
        .max()
        .unwrap_or(i64::MAX);

    let spoken = SCRIPT.join(" ");
    Run {
        accuracy: accuracy(&spoken, &heard),
        first_text_ms: first_text.load(Ordering::SeqCst),
        tail_lag_ms: last_wall as i64 - audio_done_ms as i64,
        worst_phrase_lag_ms,
        timeline_ratio: (last_at as f64 / 1000.0) / audio_secs,
        lines: lines.len(),
        heard,
        timings: lines
            .iter()
            .map(|l| (l.at_ms, l.wall_ms, l.text.clone()))
            .collect(),
    }
}

fn words(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_string())
        .filter(|w| !w.is_empty())
        .collect()
}

/// 1 - word error rate, floored at zero.
fn accuracy(reference: &str, hypothesis: &str) -> f64 {
    let r = words(reference);
    let h = words(hypothesis);
    if r.is_empty() {
        return 0.0;
    }
    // Levenshtein over words.
    let mut prev: Vec<usize> = (0..=h.len()).collect();
    let mut cur = vec![0usize; h.len() + 1];
    for i in 1..=r.len() {
        cur[0] = i;
        for j in 1..=h.len() {
            let sub = prev[j - 1] + usize::from(r[i - 1] != h[j - 1]);
            cur[j] = sub.min(prev[j] + 1).min(cur[j - 1] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    let dist = prev[h.len()] as f64;
    (1.0 - dist / r.len() as f64).max(0.0)
}

fn report(name: &str, run: &Run) {
    println!(
        "\n=== {name} ===\n  accuracy       {:.1}%\n  worst phrase   {} ms behind\n  first text     {} ms\n  tail lag       {} ms\n  timeline ratio {:.2}x\n  lines          {}\n  heard: {}\n",
        run.accuracy * 100.0,
        run.worst_phrase_lag_ms,
        run.first_text_ms,
        run.tail_lag_ms,
        run.timeline_ratio,
        run.lines,
        run.heard
    );
    println!("  {:>9}  {:>9}  {:>7}  text", "audio at", "shown at", "behind");
    for (at_ms, wall_ms, text) in &run.timings {
        println!(
            "  {at_ms:>7} ms  {wall_ms:>7} ms  {:>5} ms  {text}",
            *wall_ms as i64 - *at_ms as i64
        );
    }
}

fn check(name: &str, run: &Run, failures: &mut Vec<String>) {
    if run.accuracy < TARGET_ACCURACY {
        failures.push(format!(
            "{name}: accuracy {:.1}% < {:.0}%",
            run.accuracy * 100.0,
            TARGET_ACCURACY * 100.0
        ));
    }
    if run.first_text_ms > TARGET_FIRST_TEXT_MS {
        failures.push(format!(
            "{name}: first text after {} ms > {} ms",
            run.first_text_ms, TARGET_FIRST_TEXT_MS
        ));
    }
    if run.tail_lag_ms > TARGET_TAIL_LAG_MS {
        failures.push(format!(
            "{name}: tail lag {} ms > {} ms",
            run.tail_lag_ms, TARGET_TAIL_LAG_MS
        ));
    }
    if run.worst_phrase_lag_ms > TARGET_PHRASE_LAG_MS {
        failures.push(format!(
            "{name}: slowest phrase took {} ms to appear > {} ms",
            run.worst_phrase_lag_ms, TARGET_PHRASE_LAG_MS
        ));
    }
    if run.timeline_ratio > TARGET_TIMELINE_AHEAD {
        failures.push(format!(
            "{name}: timestamps run at {:.2}x real time",
            run.timeline_ratio
        ));
    }
}

#[test]
#[ignore = "needs the whisper model + `say`, and runs audio at wall-clock speed"]
fn live_lane_is_accurate_and_prompt() {
    whisper_rs::install_logging_hooks();
    assert!(
        oatmeal_app_lib::transcribe::default_model_path().exists(),
        "whisper model not downloaded; run the app once or the e2e test first"
    );

    let audio = script_audio();
    println!(
        "script is {:.1}s of speech",
        audio.len() as f32 / WHISPER_RATE as f32
    );

    // Control: one lane, close-mic level, quiet room.
    let loud = run_scenario(&[quiet_speaker(&audio, 1.0, 0.0004)]);
    report("loud, single lane", &loud);

    // The complaint: someone who isn't yelling, in a room with a noise floor.
    let quiet = run_scenario(&[quiet_speaker(&audio, 0.055, 0.004)]);
    report("quiet, single lane", &quiet);

    // How every real meeting runs: mic and system audio both feeding the tap.
    // The speech is on the system lane; the mic lane is an empty room.
    let both = run_scenario(&[
        room_tone(audio.len(), 0.001, 0xB0B),
        quiet_speaker(&audio, 1.0, 0.0004),
    ]);
    report("two lanes (mic + system)", &both);

    let mut failures = Vec::new();
    check("loud", &loud, &mut failures);
    check("quiet", &quiet, &mut failures);
    check("two lanes", &both, &mut failures);
    assert!(
        failures.is_empty(),
        "live lane below target:\n  {}",
        failures.join("\n  ")
    );
}
