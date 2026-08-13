// Live transcription — text while the meeting is still happening.
//
// The recording lanes (mic, system audio) already write WAVs. Reading those back
// mid-write is unreliable: the header's length field isn't correct until the file
// is finalized. So instead each lane pushes a copy of its samples into its own
// lane of a `Tap`, downmixed to mono and resampled to Whisper's 16 kHz on the way
// in. A worker thread drains the tap — *summing* the lanes, the way the offline
// pass mixes the two WAVs — decodes finished windows, and emits them as Tauri
// events. The final, accurate transcript is still produced from the WAVs at stop;
// this lane is for the live panel only.
//
// Chunking is the interesting part, and it is where both of this lane's failure
// modes live.
//
// Cutting every N seconds slices words in half, so the worker waits for a pause
// instead — a stretch of at least PAUSE_MIN_SECS quiet enough, relative to the
// speech around it, to be the end of a phrase. That relative threshold is the
// whole trick: a soft-spoken person's pauses are quieter than a loud person's
// speech, so any fixed level either hears a pause in every syllable or never
// hears one at all. Only when a window runs past MAX_WINDOW without a pause does
// it get cut at its quietest moment regardless.
//
// The window length is also the latency: nothing is on screen until a window
// closes. Decoding costs a fraction of a second, so cutting on phrase boundaries
// rather than on a fixed six-second timer is what makes text land about half a
// second after each sentence instead of several seconds after every other one.
// Each window is levelled up before decoding and handed the tail of the previous
// line as context, which is what keeps short windows as accurate as long ones.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use serde::Serialize;

use crate::transcribe::{Quality, Transcriber, WHISPER_RATE};

/// Don't decode until at least this much audio has accumulated.
///
/// This is what the live panel's latency actually is: nobody sees a word until a
/// window closes. Decoding a window costs well under a second, so the window
/// length — not the model — is the whole delay. Short enough to feel live, long
/// enough that Whisper still has a phrase to work with rather than a syllable.
const MIN_WINDOW_SECS: f32 = 1.5;
/// Force a cut once the window reaches this length, pause or not. Somebody who
/// talks without drawing breath still gets text, just in longer pieces.
const MAX_WINDOW_SECS: f32 = 8.0;
/// A quiet stretch this long is treated as the end of a phrase.
const PAUSE_MIN_SECS: f32 = 0.28;
/// How much louder a window's speech must be than its quiet end before we
/// believe it contains a pause at all. This only has to catch the degenerate
/// case — a window of one unvarying level, where the threshold would land on the
/// audio itself and every frame would read as a pause. Telling a real pause from
/// the dip between two syllables is the run-length test's job, not this one, and
/// setting this high enough to do that work would rule out anyone soft-spoken:
/// a quiet voice in a normal room clears its own noise floor by less than 3x.
const PAUSE_DYNAMIC_RANGE: f32 = 1.5;
/// How much of the previous line to hand the decoder as context. A sentence or
/// two is enough to keep names consistent; more just crowds the prompt.
const CONTEXT_CHARS: usize = 180;

/// Hard ceiling on one lane's backlog, in samples at 16 kHz (five minutes). The
/// audio callback can outrun the decoder on a slow machine; without a cap that
/// backlog is unbounded growth for as long as the meeting runs. Dropping the
/// oldest audio loses transcript, which is recoverable — the offline pass over
/// the full recording still sees everything.
const MAX_TAP_SAMPLES: usize = 16_000 * 300;

/// How far one lane may fall behind the other before the tap stops waiting for
/// it and treats the gap as silence (two seconds at 16 kHz). Normally both lanes
/// deliver continuously and this never fires; it exists so a device that drops
/// out mid-meeting stalls the live panel for two seconds rather than forever.
const MAX_LANE_SKEW: usize = 16_000 * 2;

/// Ceiling on retained live lines. The UI only ever shows the tail, and the
/// authoritative transcript is written from the offline pass.
const MAX_LIVE_LINES: usize = 4_000;
/// Frame size used when hunting for the quietest moment to cut on.
const PAUSE_FRAME_SECS: f32 = 0.02;
/// How often the worker checks whether a window is ready. Has to be small
/// against `MIN_WINDOW_SECS` or it becomes latency of its own.
const POLL_MS: u64 = 100;

/// One line of live transcript, as emitted to the UI.
#[derive(Debug, Clone, Serialize)]
pub struct LiveLine {
    /// Milliseconds from the start of the meeting.
    pub at_ms: u64,
    pub text: String,
}

/// Shared sink the capture lanes push into, one buffer per lane.
///
/// The lanes are separate on purpose. Both the mic and the system-audio recorder
/// feed this tap, and what the decoder needs is the two of them *summed* — the
/// same thing `session::mix` does to the two WAVs for the offline transcript.
/// Appending both into one buffer instead splices two independent timelines into
/// one stream, which stretches the audio and chops words across the seams.
pub struct Tap {
    lanes: Vec<LaneBuf>,
}

#[derive(Default)]
struct LaneBuf {
    samples: Mutex<Vec<f32>>,
    /// Cleared when a lane's recorder never started, so `drain` stops waiting on
    /// a buffer that will never receive anything.
    live: AtomicBool,
}

/// A handle to one lane of a `Tap`, held by a capture recorder.
#[derive(Clone)]
pub struct Lane {
    tap: Arc<Tap>,
    index: usize,
}

impl Tap {
    /// A tap with `lanes` independent inputs. Returns the shared tap and a handle
    /// per lane.
    pub fn with_lanes(lanes: usize) -> (Arc<Self>, Vec<Lane>) {
        let tap = Arc::new(Self {
            lanes: (0..lanes)
                .map(|_| LaneBuf {
                    samples: Mutex::new(Vec::new()),
                    live: AtomicBool::new(true),
                })
                .collect(),
        });
        let handles = (0..lanes)
            .map(|index| Lane {
                tap: tap.clone(),
                index,
            })
            .collect();
        (tap, handles)
    }

    /// Mark a lane as never going to deliver, because its recorder failed to
    /// start. Its buffer is skipped from then on.
    pub fn retire(&self, lane: usize) {
        if let Some(l) = self.lanes.get(lane) {
            l.live.store(false, Ordering::SeqCst);
        }
    }

    /// Append interleaved samples to one lane, downmixing to mono and resampling
    /// to 16 kHz. Runs on the realtime audio thread, so it stays allocation-light
    /// and never blocks on anything but this lane's lock.
    fn push(&self, lane: usize, data: &[f32], channels: u16, rate: u32) {
        if data.is_empty() || channels == 0 || rate == 0 {
            return;
        }
        let buf = match self.lanes.get(lane) {
            Some(l) => l,
            None => return,
        };
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

        if let Ok(mut buf) = buf.samples.lock() {
            buf.extend_from_slice(&out);
            if buf.len() > MAX_TAP_SAMPLES {
                let overflow = buf.len() - MAX_TAP_SAMPLES;
                buf.drain(..overflow);
            }
        }
    }

    /// Take the audio every live lane has in common, summed sample-for-sample.
    ///
    /// The lanes are captured from independent clocks, so they are aligned by
    /// position from the start of the meeting — the same assumption the offline
    /// mix makes about the two WAVs.
    fn drain(&self) -> Vec<f32> {
        let mut guards = Vec::with_capacity(self.lanes.len());
        for lane in &self.lanes {
            if !lane.live.load(Ordering::SeqCst) {
                continue;
            }
            match lane.samples.lock() {
                Ok(g) => guards.push(g),
                // A poisoned lane lock means a capture callback panicked; the
                // meeting is still recording, so carry on without it.
                Err(_) => continue,
            }
        }
        if guards.is_empty() {
            return Vec::new();
        }

        let shortest = guards.iter().map(|g| g.len()).min().unwrap_or(0);
        let longest = guards.iter().map(|g| g.len()).max().unwrap_or(0);
        let n = shortest.max(longest.saturating_sub(MAX_LANE_SKEW));
        if n == 0 {
            return Vec::new();
        }

        let mut out = vec![0.0f32; n];
        for g in guards.iter_mut() {
            let take = n.min(g.len());
            for (o, s) in out.iter_mut().zip(g[..take].iter()) {
                *o += *s;
            }
            g.drain(..take);
        }
        for s in &mut out {
            *s = s.clamp(-1.0, 1.0);
        }
        out
    }
}

impl Lane {
    /// Append interleaved capture samples to this lane.
    pub fn push(&self, data: &[f32], channels: u16, rate: u32) {
        self.tap.push(self.index, data, channels, rate);
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
    // The tail of what has been said, carried into the next decode so a short
    // window still knows what conversation it is in.
    let mut context = String::new();

    loop {
        let stopping = stop.load(Ordering::SeqCst);
        pending.extend_from_slice(&tap.drain());

        if pending.is_empty() {
            // Nothing left and we're shutting down.
            if stopping {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
            continue;
        }

        // Where to end this window. In order of preference: everything that's
        // left if we're shutting down, the end of a phrase, or — once the window
        // has grown too long to keep holding text back — the quietest moment in
        // it.
        let cut = if stopping {
            pending.len().min(max_window)
        } else if let Some(end) = find_endpoint(&pending, min_window, max_window) {
            end
        } else if pending.len() >= max_window {
            find_cut(&pending, min_window, max_window)
        } else {
            // Mid-phrase. Waiting for the speaker to finish the thought costs a
            // moment but is the difference between a clean line and a sliced
            // word — decoding half a word gets it wrong *and* poisons the
            // context handed to the next window.
            std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
            continue;
        };

        let mut window: Vec<f32> = pending.drain(..cut).collect();
        let at_ms = (elapsed_samples as f64 / rate as f64 * 1000.0) as u64;
        elapsed_samples += window.len();

        if !is_silent(&window) {
            normalize(&mut window);
            match transcriber.run(
                &window,
                language.as_deref(),
                Quality::Fast,
                Some(context.as_str()),
            ) {
                Ok(t) => {
                    let text = t.text.trim().to_string();
                    if !text.is_empty() && !is_noise(&text) {
                        context = tail(&context, &text);
                        let line = LiveLine { at_ms, text };
                        if let Ok(mut l) = lines.lock() {
                            l.push(line.clone());
                            if l.len() > MAX_LIVE_LINES {
                                let overflow = l.len() - MAX_LIVE_LINES;
                                l.drain(..overflow);
                            }
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

/// Where the current phrase ends, or `None` if the speaker is still mid-thought.
///
/// This is what keeps short windows from mangling words. The threshold is
/// computed from *this* window rather than fixed, because a fixed one can only
/// ever suit one kind of speaker: a quiet talker's pauses are quieter than a
/// loud talker's speech, so an absolute level either finds a pause in every
/// syllable or never finds one at all. Taking it relative to the window's own
/// loud and quiet ends adapts to both, and to the room they're sitting in.
fn find_endpoint(samples: &[f32], min_window: usize, max_window: usize) -> Option<usize> {
    let end = samples.len().min(max_window);
    if end <= min_window {
        return None;
    }
    let frame = (PAUSE_FRAME_SECS * WHISPER_RATE as f32) as usize;
    if frame == 0 {
        return None;
    }

    let levels: Vec<f32> = samples[..end]
        .chunks(frame)
        .filter(|c| c.len() == frame)
        .map(|c| (c.iter().map(|s| s * s).sum::<f32>() / c.len() as f32).sqrt())
        .collect();
    if levels.len() < 4 {
        return None;
    }

    let run = (((PAUSE_MIN_SECS * WHISPER_RATE as f32) as usize) / frame).max(1);

    let mut sorted = levels.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let loud = sorted[sorted.len() * 9 / 10];
    // The floor is the quietest *sustained* stretch, not a percentile. A pause is
    // a small share of a window that is mostly speech — early on it can be a
    // tenth of it — so any percentile low enough to land inside the pause when it
    // is brief is also low enough to be pure chance. The quietest run of the
    // length we're hunting for is exactly the thing being looked for.
    let quiet = levels
        .windows(run)
        .map(|w| w.iter().sum::<f32>() / run as f32)
        .fold(f32::MAX, f32::min);
    if quiet >= f32::MAX || loud < quiet * PAUSE_DYNAMIC_RANGE {
        // Every frame is at much the same level: either nobody has stopped
        // talking, or nobody has started. Either way there is no phrase boundary
        // in here to cut on.
        return None;
    }
    // Halfway between the window's noise floor and its speech, on a log scale —
    // as far from both as it can get, which is what makes it hold up at the 10 dB
    // signal-to-noise ratio a soft-spoken person across the table actually has.
    let threshold = (loud * quiet).sqrt();

    let mut quiet_from: Option<usize> = None;
    for (i, level) in levels.iter().enumerate() {
        if *level <= threshold {
            let start = *quiet_from.get_or_insert(i);
            let cut = (i + 1) * frame;
            // Cut *after* the pause, so the next window opens on speech and this
            // one ends with the silence that tells Whisper the phrase finished.
            if i + 1 - start >= run && cut >= min_window {
                return Some(cut.min(end));
            }
        } else {
            quiet_from = None;
        }
    }
    None
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

/// The last `CONTEXT_CHARS` of `previous` followed by `latest`, cut on a
/// character boundary so the prompt is always valid UTF-8.
fn tail(previous: &str, latest: &str) -> String {
    let joined = if previous.is_empty() {
        latest.to_string()
    } else {
        format!("{previous} {latest}")
    };
    if joined.len() <= CONTEXT_CHARS {
        return joined;
    }
    let mut start = joined.len() - CONTEXT_CHARS;
    while start < joined.len() && !joined.is_char_boundary(start) {
        start += 1;
    }
    joined[start..].to_string()
}

/// Level a boosted window is brought up to. Short of 1.0 so the loudest syllable
/// has somewhere to go and nothing clips.
const TARGET_PEAK: f32 = 0.7;
/// Most a window may be boosted. A cap keeps a window of nothing but room tone
/// from being amplified into something Whisper will try to read words out of.
const MAX_GAIN: f32 = 24.0;

/// Bring a quiet window up to a level Whisper was trained on.
///
/// Whisper's front end has no automatic gain control: someone across the table
/// lands in the bottom few bits of the range the model expects, and the log-mel
/// features it derives get squashed toward the floor. Scaling the window up
/// doesn't improve its signal-to-noise ratio — nothing can — but it does put the
/// speech where the model can read it, which is most of the difference between a
/// speaker who has to shout and one who doesn't.
///
/// Only ever boosts. Audio that is already loud enough is left exactly as it is.
fn normalize(samples: &mut [f32]) {
    let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    if peak <= f32::EPSILON {
        return;
    }
    let gain = (TARGET_PEAK / peak).min(MAX_GAIN);
    if gain <= 1.0 {
        return;
    }
    for s in samples.iter_mut() {
        *s = (*s * gain).clamp(-1.0, 1.0);
    }
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
        let (tap, lanes) = Tap::with_lanes(1);
        // 1 second of 48 kHz stereo: left 1.0, right -1.0 → mono 0.0.
        let frames = 48_000;
        let data: Vec<f32> = (0..frames).flat_map(|_| [1.0f32, -1.0f32]).collect();
        lanes[0].push(&data, 2, 48_000);

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
    fn two_lanes_are_summed_not_concatenated() {
        let (tap, lanes) = Tap::with_lanes(2);
        lanes[0].push(&[0.25; 16_000], 1, WHISPER_RATE);
        lanes[1].push(&[0.5; 16_000], 1, WHISPER_RATE);

        let out = tap.drain();
        // One second in, one second out — not two.
        assert_eq!(out.len(), 16_000, "lanes must overlay, not append");
        assert!(
            out.iter().all(|s| (s - 0.75).abs() < 1e-6),
            "lanes must sum sample-for-sample, got {:?}",
            &out[..4]
        );
    }

    #[test]
    fn drain_waits_for_the_slower_lane_then_gives_up() {
        let (tap, lanes) = Tap::with_lanes(2);
        // Only one lane has delivered: hold the audio back so the two stay aligned.
        lanes[0].push(&[0.5; 16_000], 1, WHISPER_RATE);
        assert!(tap.drain().is_empty(), "should wait for the other lane");

        // Still nothing from lane 1 well past the skew limit — stop waiting and
        // treat the missing lane as silence rather than stalling the panel.
        lanes[0].push(&[0.5; MAX_LANE_SKEW], 1, WHISPER_RATE);
        let out = tap.drain();
        assert_eq!(out.len(), 16_000);
        assert!(out.iter().all(|s| (s - 0.5).abs() < 1e-6));
    }

    #[test]
    fn a_retired_lane_never_holds_the_others_back() {
        let (tap, lanes) = Tap::with_lanes(2);
        tap.retire(1);
        lanes[0].push(&[0.5; 800], 1, WHISPER_RATE);

        let out = tap.drain();
        assert_eq!(out.len(), 800, "a lane that never started must not block");
    }

    /// Speech-shaped audio: `speech` alternating with `gap`-long pauses, over a
    /// noise floor, all scaled by `level`.
    fn phrases(spec: &[(f32, f32)], level: f32, noise: f32) -> Vec<f32> {
        let rate = WHISPER_RATE as f32;
        let mut out = Vec::new();
        let mut phase = 0.0f32;
        for (speech, gap) in spec {
            for _ in 0..((speech * rate) as usize) {
                phase += 0.05;
                out.push(phase.sin() * level);
            }
            out.extend(std::iter::repeat(0.0).take((gap * rate) as usize));
        }
        // Dither the whole thing so nothing is mathematically perfect silence.
        for (i, s) in out.iter_mut().enumerate() {
            *s += ((i % 71) as f32 / 71.0 - 0.5) * noise;
        }
        out
    }

    #[test]
    fn endpoint_lands_after_the_pause_not_mid_word() {
        let rate = WHISPER_RATE as usize;
        // Two seconds of speech, a third of a second of quiet, then more speech.
        let samples = phrases(&[(2.0, 0.35), (2.0, 0.35)], 0.5, 0.001);

        let cut = find_endpoint(&samples, (1.5 * rate as f32) as usize, 8 * rate)
            .expect("a clear pause should be found");
        // Inside the gap or just past it — never back inside the first phrase.
        assert!(
            cut >= 2 * rate && cut <= (2.5 * rate as f32) as usize,
            "cut at {cut}, gap runs {}..{}",
            2 * rate,
            (2.35 * rate as f32) as usize
        );
    }

    #[test]
    fn a_quiet_speakers_pauses_are_found_too() {
        let rate = WHISPER_RATE as usize;
        // Same shape, 25 dB down over a room-tone floor — the case an absolute
        // threshold gets wrong, because this speech is quieter than a loud
        // speaker's silence.
        let samples = phrases(&[(2.0, 0.35), (2.0, 0.35)], 0.03, 0.004);

        let cut = find_endpoint(&samples, (1.5 * rate as f32) as usize, 8 * rate)
            .expect("a quiet speaker's pause should be found too");
        assert!(
            cut >= 2 * rate && cut <= (2.6 * rate as f32) as usize,
            "cut at {cut}"
        );
    }

    #[test]
    fn unbroken_speech_keeps_listening_rather_than_slicing_a_word() {
        let rate = WHISPER_RATE as usize;
        // Six seconds without a real breath.
        let samples = phrases(&[(6.0, 0.0)], 0.5, 0.001);
        assert!(
            find_endpoint(&samples, (1.5 * rate as f32) as usize, 8 * rate).is_none(),
            "no pause here, so nothing should be cut"
        );
    }

    #[test]
    fn the_dip_between_syllables_is_not_a_phrase_boundary() {
        let rate = WHISPER_RATE as usize;
        // Speech has gaps in it constantly — stops, breaths between words — but
        // they are short. Six seconds of 180 ms syllables separated by 120 ms
        // dips is continuous talking, and cutting in one would slice a word.
        let syllables: Vec<(f32, f32)> = (0..20).map(|_| (0.18, 0.12)).collect();
        let samples = phrases(&syllables, 0.5, 0.001);
        assert!(
            find_endpoint(&samples, (1.5 * rate as f32) as usize, 8 * rate).is_none(),
            "a 120 ms dip is shorter than a pause and must not end a window"
        );
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
    fn context_keeps_the_recent_tail_on_a_character_boundary() {
        assert_eq!(tail("", "hello"), "hello");
        assert_eq!(tail("hello", "there"), "hello there");

        // Long history is trimmed from the front, and the newest text survives.
        let long = "word ".repeat(200);
        let out = tail(&long, "the very last thing said");
        assert!(out.len() <= CONTEXT_CHARS + 4, "context grew to {}", out.len());
        assert!(out.ends_with("the very last thing said"));

        // Multi-byte characters must not be sliced in half.
        let accented = "café ".repeat(100);
        let out = tail(&accented, "naïve résumé");
        assert!(out.ends_with("naïve résumé"));
    }

    #[test]
    fn quiet_audio_is_boosted_and_loud_audio_is_left_alone() {
        // A speaker across the table: 25 dB down. Should come up near the target.
        let mut quiet = vec![0.03f32, -0.02, 0.01, -0.03];
        normalize(&mut quiet);
        let peak = quiet.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!((peak - TARGET_PEAK).abs() < 1e-3, "quiet peak came out {peak}");

        // Close-mic audio is already in range; leave it untouched rather than
        // pushing it toward clipping.
        let loud = vec![0.8f32, -0.75, 0.9];
        let mut same = loud.clone();
        normalize(&mut same);
        assert_eq!(same, loud, "loud audio must not be rescaled");
    }

    #[test]
    fn boosting_is_capped_so_room_tone_is_not_amplified_into_speech() {
        let mut tone = vec![0.0004f32, -0.0003, 0.0004];
        normalize(&mut tone);
        let peak = tone.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak < 0.02, "room tone was boosted to {peak}");
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
