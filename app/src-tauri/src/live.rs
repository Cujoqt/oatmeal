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

use crate::transcribe::{normalize_for_repeat_check, Quality, Transcriber, WHISPER_RATE};

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

/// How many consecutive live lines may say the same thing before the rest are
/// dropped.
///
/// `Transcriber::run` already collapses repeated segments, but its guard starts
/// over on every call, and the live lane calls it once per window — so a
/// repetition loop that emits the phrase once per window walks straight past it.
/// The loop is also self-feeding here: `tail` hands the repeated line to the
/// next window as its prompt, and `Quality::Fast` turns off the temperature
/// fallback that is whisper.cpp's own way out of one. Observed live as
/// "I don't know." and "Our orange juice is going to decrease." filling the
/// panel for minutes while the offline pass over the same audio came out clean.
const MAX_REPEATED_LINES: usize = 2;

/// Hard ceiling on one lane's backlog, in samples at 16 kHz (five minutes). The
/// audio callback can outrun the decoder on a slow machine; without a cap that
/// backlog is unbounded growth for as long as the meeting runs. Dropping the
/// oldest audio loses transcript, which is recoverable — the offline pass over
/// the full recording still sees everything.
const MAX_TAP_SAMPLES: usize = 16_000 * 300;

/// Most audio the live worker will hold waiting to be decoded, in seconds.
///
/// This is the panel's worst-case lag. The tap's own ceiling never binds — the
/// worker drains it on every pass — so without a cap here a decode that runs
/// slower than real time backs up without limit, and the panel ends a long
/// meeting minutes behind the room. Thirty seconds is late enough to ride out a
/// slow stretch and short enough that nobody reads it as live.
const MAX_PENDING_SECS: f32 = 30.0;

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
    /// Set if a lane's backlog ever overflowed and audio was thrown away.
    ///
    /// Losing audio is survivable for the live panel, which is a convenience.
    /// It is not survivable for a transcript built from this same audio, so
    /// anything doing that has to be able to ask, and fall back to the WAVs —
    /// which are complete no matter how far behind the decoder fell.
    dropped: AtomicBool,
}

#[derive(Default)]
struct LaneBuf {
    samples: Mutex<Vec<f32>>,
    /// Cleared when a lane's recorder never started, so `drain` stops waiting on
    /// a buffer that will never receive anything.
    live: AtomicBool,
    /// Resampler position, carried between callbacks. Only ever touched by this
    /// lane's capture thread.
    resampler: Mutex<Resampler>,
}

/// Where a lane's resampler had got to when the last capture callback ended.
///
/// Capture arrives in chunks, but the resampled result has to be one continuous
/// stream. Restarting at position zero every callback does two things, both bad:
/// it rounds the output length down each time, throwing away a fraction of a
/// sample hundreds of times a second, and it resets the interpolation phase, so
/// every buffer boundary is a small discontinuity.
///
/// The dropped fractions are the serious half. They accumulate — at 48 kHz with
/// 512-frame buffers it is about fourteen seconds of audio per hour — and the
/// two lanes lose at *different* rates, because the mic runs at whatever its
/// device offers while system audio is pinned to 48 kHz. `drain` aligns the
/// lanes by position, so an hour in, one lane is summing over audio the other
/// recorded seconds earlier. Summing a conversation onto a delayed copy of
/// itself is the worst input Whisper can be handed.
#[derive(Default)]
struct Resampler {
    /// Next sample to read, as a fractional frame index into the *current*
    /// chunk. Negative down to -1 means the read lands between the previous
    /// chunk's last frame and this one's first.
    pos: f64,
    /// The previous chunk's final frame, so interpolation can cross the seam.
    last: f32,
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
                    resampler: Mutex::new(Resampler::default()),
                })
                .collect(),
            dropped: AtomicBool::new(false),
        });
        let handles = (0..lanes)
            .map(|index| Lane {
                tap: tap.clone(),
                index,
            })
            .collect();
        (tap, handles)
    }

    /// Whether any audio was thrown away because the decoder fell too far
    /// behind. If this is true, nothing derived from this tap is a complete
    /// record of the meeting.
    pub fn dropped_audio(&self) -> bool {
        self.dropped.load(Ordering::SeqCst)
    }

    /// Record that audio was dropped downstream of the tap — the live worker
    /// abandoning a backlog it can no longer catch up on. Same meaning as a tap
    /// overflow: the WAVs are now the only complete record of the meeting.
    pub fn mark_dropped(&self) {
        self.dropped.store(true, Ordering::SeqCst);
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

        let mut state = match buf.resampler.lock() {
            Ok(s) => s,
            Err(_) => return,
        };

        // Decimating to 16 kHz without band-limiting first does fold everything
        // above 8 kHz back into the speech band, and a fourth-order Butterworth
        // ahead of the drop was measured here to make transcription *worse*, not
        // better: -1.2 points on the loud scenario and -6.1 on two lanes, against
        // +1.3 on the quiet one. Whisper's mel filterbank tops out around 8 kHz
        // anyway, and the filter's skirt takes real fricative energy with it well
        // before the aliases it removes were costing anything. Left out on
        // purpose — the obvious fix here is a regression.
        let mono = |f: usize| {
            let base = f * ch;
            data[base..base + ch].iter().sum::<f32>() / ch as f32
        };

        // Resample with linear interpolation, resuming exactly where
        // the previous callback stopped. `pos` is a fractional frame index into
        // this chunk and may start slightly negative, meaning the next output
        // sample falls between the last frame of the previous chunk and the
        // first of this one — which is why the seam sample is kept.
        let ratio = rate as f64 / WHISPER_RATE as f64;
        let mut out = Vec::with_capacity((frames as f64 / ratio) as usize + 1);
        let last_frame = frames as i64 - 1;
        loop {
            let i = state.pos.floor();
            let idx = i as i64;
            let frac = (state.pos - i) as f32;
            if idx > last_frame {
                break;
            }
            let a = if idx < 0 { state.last } else { mono(idx as usize) };
            if frac == 0.0 {
                // Landed exactly on a frame, so no neighbour is needed. Integer
                // ratios — 48 kHz to 16 kHz, or no resampling at all — take this
                // path every time and stay sample-exact.
                out.push(a);
            } else {
                // Interpolating needs the frame after `idx` too. When that is
                // past the end of this chunk, stop and let the next callback
                // finish the sample. Rounding it away instead is precisely the
                // leak this carried state exists to close.
                if idx + 1 > last_frame {
                    break;
                }
                let b = mono((idx + 1) as usize);
                out.push(a + (b - a) * frac);
            }
            state.pos += ratio;
        }
        // Rebase onto the next chunk, whose frame 0 is this chunk's `frames`.
        state.pos -= frames as f64;
        state.last = mono(frames - 1);
        drop(state);

        if let Ok(mut buf) = buf.samples.lock() {
            buf.extend_from_slice(&out);
            if buf.len() > MAX_TAP_SAMPLES {
                let overflow = buf.len() - MAX_TAP_SAMPLES;
                buf.drain(..overflow);
                self.dropped.store(true, Ordering::SeqCst);
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

/// How much audio the background accurate pass takes at a time.
///
/// Ten minutes is long enough that the per-call overhead is noise and Whisper
/// has plenty of context, and short enough that finishing a meeting only ever
/// leaves the last few minutes to do. Blocks are cut where the live lane already
/// found a phrase boundary, so no block ever starts mid-word.
const BLOCK_SECS: f32 = 600.0;

/// Audio banked for the background accurate pass, and what it has produced.
///
/// The live lane has already mixed the capture lanes and resampled them to what
/// Whisper wants, so the accurate pass reads the same stream rather than going
/// back to the WAVs. That is the whole saving: by the time someone finishes a
/// meeting, everything but the last block has already been transcribed properly.
#[derive(Default)]
struct Bank {
    /// Mixed audio the accurate pass has not taken yet.
    pending: Mutex<Vec<f32>>,
    /// Finished blocks in order, each with where it starts in the meeting.
    done: Mutex<Vec<(usize, crate::transcribe::Transcript)>>,
    /// Set once the live worker has exited and no more audio can arrive.
    ///
    /// The stop flag alone is not enough to tell the block worker it is done:
    /// the live worker decodes its last window *after* that flag goes up, so a
    /// block worker that quit on an empty backlog the moment it saw the flag
    /// would drop whatever was said last.
    sealed: AtomicBool,
    /// Whether anything is going to read `pending`.
    ///
    /// False when the accurate model failed to load and no block worker was
    /// started. Banking regardless would pile up a meeting's audio — around
    /// 230 MB an hour — for a reader that does not exist.
    wanted: bool,
}

/// What the background pass managed to finish, handed over when a meeting stops.
pub struct Progress {
    /// Transcribed blocks, in order, each with its start offset in samples.
    pub blocks: Vec<(usize, crate::transcribe::Transcript)>,
    /// Whether these blocks cover the meeting with nothing missing. False if the
    /// tap ever dropped audio, in which case the WAVs are the only complete
    /// record and this must not be used.
    pub complete: bool,
}

/// A running live-transcription worker. Dropping it stops the worker.
pub struct LiveSession {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    /// Every line emitted so far, so a recap can be asked for mid-meeting.
    lines: Arc<Mutex<Vec<LiveLine>>>,
    /// The background accurate pass, and what it has banked so far.
    bank: Arc<Bank>,
    block_handle: Option<JoinHandle<()>>,
    tap: Arc<Tap>,
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
        let transcriber = Arc::new(Transcriber::load(&model_path)?);

        // The background pass reads a bigger, slower model than the live lane —
        // it has ten minutes of wall clock per block and nobody watching it, so
        // it can afford what the live panel cannot.
        //
        // Failing to load it is not fatal. Blocks are an optimisation: without
        // them `finish` hands back nothing and the meeting is transcribed from
        // the WAVs at stop, exactly as it was before the background pass existed.
        // Losing the live panel over it would be the worse trade.
        let accurate = match Transcriber::load_accurate("") {
            Ok(t) => Some(Arc::new(t)),
            Err(e) => {
                eprintln!("[oatmeal] background pass disabled: {e}");
                None
            }
        };

        let stop = Arc::new(AtomicBool::new(false));
        let lines: Arc<Mutex<Vec<LiveLine>>> = Arc::new(Mutex::new(Vec::new()));
        let bank = Arc::new(Bank {
            wanted: accurate.is_some(),
            ..Default::default()
        });

        let worker_stop = stop.clone();
        let worker_lines = lines.clone();
        let worker_tap = tap.clone();
        let worker_bank = bank.clone();
        let worker_model = transcriber;
        let worker_lang = language.clone();
        let handle = std::thread::Builder::new()
            .name("oatmeal-live".into())
            .spawn(move || {
                run(
                    worker_tap,
                    worker_model,
                    worker_lang,
                    worker_stop,
                    worker_lines,
                    worker_bank,
                    emit,
                );
            })
            .map_err(|e| format!("spawn live thread: {e}"))?;

        let block_handle = match accurate {
            Some(model) => {
                let block_bank = bank.clone();
                Some(
                    std::thread::Builder::new()
                        .name("oatmeal-blocks".into())
                        .spawn(move || {
                            run_blocks(model, language, block_bank);
                        })
                        .map_err(|e| format!("spawn block thread: {e}"))?,
                )
            }
            None => None,
        };

        Ok(Self {
            stop,
            handle: Some(handle),
            lines,
            bank,
            block_handle,
            tap,
        })
    }

    /// Stop everything and hand back what the background pass finished.
    ///
    /// The live worker is joined first so the last of the audio reaches the bank
    /// before the block worker is asked to flush it.
    pub fn finish(mut self) -> Progress {
        self.signal_and_join();
        let blocks = self
            .bank
            .done
            .lock()
            .map(|mut d| std::mem::take(&mut *d))
            .unwrap_or_default();
        Progress {
            complete: !self.tap.dropped_audio(),
            blocks,
        }
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
        // Only now can no more audio arrive — the live worker has exited, however
        // it exited. Sealing here rather than at the end of the worker itself
        // means a panicking live thread still releases the block worker instead
        // of leaving it spinning for the life of the process.
        self.bank.sealed.store(true, Ordering::SeqCst);
        if let Some(h) = self.block_handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for LiveSession {
    fn drop(&mut self) {
        self.signal_and_join();
    }
}

/// Transcribe banked audio properly, a block at a time, while the meeting runs.
///
/// Same decoding the final pass uses, just on fewer cores and started early. The
/// work is not extra — it is the work finishing a meeting would have done
/// anyway, moved to while there is nothing else to wait for.
fn run_blocks(transcriber: Arc<Transcriber>, language: Option<String>, bank: Arc<Bank>) {
    let block = (BLOCK_SECS * WHISPER_RATE as f32) as usize;
    // Where the next block starts, in samples from the beginning of the meeting.
    let mut offset = 0usize;

    loop {
        // Read `sealed` before draining, never after: if the live worker seals
        // between the drain and the check, the next pass still sees the flag and
        // an empty backlog, which is the correct place to stop.
        let sealed = bank.sealed.load(Ordering::SeqCst);

        let taken = {
            let mut pending = match bank.pending.lock() {
                Ok(p) => p,
                Err(_) => return,
            };
            if pending.len() >= block {
                Some(pending.drain(..block).collect::<Vec<f32>>())
            } else if sealed && !pending.is_empty() {
                // Last call: take whatever is left, however short.
                Some(std::mem::take(&mut *pending))
            } else {
                None
            }
        };

        let Some(audio) = taken else {
            // Nothing to take. Only actually finished if no more can arrive.
            if sealed {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
            continue;
        };

        let len = audio.len();
        match transcriber.run(&audio, language.as_deref(), Quality::Background, None) {
            Ok(t) => {
                if let Ok(mut done) = bank.done.lock() {
                    done.push((offset, t));
                }
            }
            // A block that fails is not fatal on its own, but it does leave a
            // hole, and a transcript with a silent hole in it is worse than a
            // slow one. Drop everything banked so the caller falls back to the
            // WAVs, which are complete.
            Err(e) => {
                eprintln!("[oatmeal] background block: {e}");
                if let Ok(mut done) = bank.done.lock() {
                    done.clear();
                }
                return;
            }
        }
        offset += len;
    }
}

fn run(
    tap: Arc<Tap>,
    transcriber: Arc<Transcriber>,
    language: Option<String>,
    stop: Arc<AtomicBool>,
    lines: Arc<Mutex<Vec<LiveLine>>>,
    bank: Arc<Bank>,
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
    let mut repeats = RepeatGuard::default();

    let max_pending = (MAX_PENDING_SECS * rate) as usize;
    let mut last_report = std::time::Instant::now();

    loop {
        let stopping = stop.load(Ordering::SeqCst);
        pending.extend_from_slice(&tap.drain());

        // Whatever is waiting here *is* the panel's lag: the worker drains the
        // tap every pass, so a decode that runs slower than real time backs up in
        // this buffer and nowhere else. Left alone it has no ceiling, and the
        // panel drifts further behind for the rest of the meeting. Past the cap
        // the oldest audio is worth less than catching up, so drop it — but bank
        // it first, because the background pass needs an unbroken timeline, and
        // advance the clock over it so every later timestamp still lands where it
        // belongs.
        if !stopping && pending.len() > max_pending {
            let overflow = pending.len() - max_pending;
            let dropped: Vec<f32> = pending.drain(..overflow).collect();
            if bank.wanted {
                if let Ok(mut b) = bank.pending.lock() {
                    b.extend_from_slice(&dropped);
                }
            }
            elapsed_samples += overflow;
            // The next window follows a hole, so the previous line is no longer
            // the sentence before it.
            context.clear();
            tap.mark_dropped();
            eprintln!(
                "[oatmeal] live lane fell {:.0}s behind; skipped {:.0}s to catch up",
                (max_pending + overflow) as f32 / rate,
                overflow as f32 / rate
            );
        }

        if last_report.elapsed() >= std::time::Duration::from_secs(60) {
            last_report = std::time::Instant::now();
            eprintln!(
                "[oatmeal] live lane {:.1}s behind",
                pending.len() as f32 / rate
            );
        }

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

        // Bank the audio before it is levelled, and whether or not it turns out
        // to be silence: the background pass needs an unbroken timeline, and a
        // per-window gain would leave it with audio that jumps in level at every
        // phrase boundary.
        if bank.wanted {
            if let Ok(mut b) = bank.pending.lock() {
                b.extend_from_slice(&window);
            }
        }

        if !is_silent(&window) {
            normalize(&mut window);
            match transcriber.run(
                &window,
                language.as_deref(),
                Quality::Fast,
                Some(context.as_str()),
            ) {
                Ok(t) => {
                    let text = strip_sound_tags(t.text.trim());
                    if !text.is_empty() && !is_noise(&text) {
                        if repeats.is_looping(&text) {
                            // Whisper is looping. Drop the line, and clear the
                            // context so the next window isn't prompted with the
                            // phrase that is keeping the loop going.
                            context.clear();
                            continue;
                        }
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
/// Counts how many windows in a row have decoded to the same line, so a
/// repetition loop can be cut off across windows the way
/// `collapse_repeated_segments` cuts it off inside one.
#[derive(Default)]
struct RepeatGuard {
    last: String,
    run: usize,
}

impl RepeatGuard {
    /// Record `text` and report whether it is one repeat too many.
    fn is_looping(&mut self, text: &str) -> bool {
        let norm = normalize_for_repeat_check(text);
        if norm == self.last {
            self.run += 1;
        } else {
            self.last = norm;
            self.run = 1;
        }
        self.run > MAX_REPEATED_LINES
    }
}

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

/// Strip the sound tags Whisper writes when it decides a stretch is music
/// rather than speech: `♪ Want to email so ♪`, `[MUSIC]`, `(applause)`.
///
/// These have to be removed here rather than suppressed inside whisper.cpp.
/// `set_suppress_nst` works by looking the literal strings `"♪"`, `"♪♪"` … up in
/// the model's vocabulary, and `small.en`'s 50,257 tokens contain none of them —
/// the note is assembled from byte-level pieces, so every lookup misses and the
/// suppression is a no-op for this model.
fn strip_sound_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if matches!(c, '♪' | '♫' | '♬' | '♩') {
            i += 1;
            continue;
        }
        let closer = match c {
            '(' => Some(')'),
            '[' => Some(']'),
            _ => None,
        };
        if let Some(closer) = closer {
            if let Some(offset) = chars[i + 1..].iter().position(|&x| x == closer) {
                let end = i + 1 + offset;
                let inner: String = chars[i + 1..end].iter().collect();
                if is_sound_tag(&inner) {
                    i = end + 1;
                    continue;
                }
            }
        }
        out.push(c);
        i += 1;
    }
    // Collapse the runs of spaces the removals leave behind.
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Sound tags Whisper writes in brackets. Matched against a bracket group's
/// entire contents, never against a fragment.
const SOUND_TAGS: [&str; 9] = [
    "music",
    "applause",
    "laughter",
    "silence",
    "blank_audio",
    "inaudible",
    "noise",
    "cough",
    "sound",
];

/// Whether a bracket group is one of Whisper's sound tags rather than something
/// that was said.
///
/// Only a whole-contents match counts, because brackets are ordinary speech:
/// "(x + 1)", "f(x)", an aside like "(last week)". Dropping every bracket group
/// would delete that content and leave a line that still reads as a sentence, so
/// nothing downstream could tell it had been cut. Anything holding a digit or an
/// operator is kept whatever it says — that is arithmetic, not a tag.
fn is_sound_tag(inner: &str) -> bool {
    let t = inner.trim().to_ascii_lowercase();
    if t.is_empty() || t.chars().any(|c| c.is_ascii_digit() || "+-*/^=<>".contains(c)) {
        return false;
    }
    SOUND_TAGS.contains(&t.replace(' ', "_").as_str())
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

    /// Audio arrives in callbacks, not in one lump, and the resampled result has
    /// to be one continuous stream regardless of where the chunk boundaries fell.
    ///
    /// Restarting the resampler each callback rounded the output length down
    /// every time. The loss is invisible per chunk and ruinous per hour: at these
    /// rates it ran to seconds of audio thrown away, and because the two lanes
    /// run at different rates they lost at different rates, sliding out of
    /// alignment with each other while `drain` still summed them by position.
    #[test]
    fn chunked_pushes_do_not_leak_samples() {
        // A minute, delivered in awkward buffers that divide evenly into neither
        // the rate nor the 16 kHz target.
        for (rate, buf) in [(48_000u32, 512usize), (44_100, 512), (44_100, 480)] {
            let (tap, lanes) = Tap::with_lanes(1);
            let seconds = 60;
            let total = rate as usize * seconds;
            let mut pushed = 0;
            while pushed < total {
                let n = buf.min(total - pushed);
                lanes[0].push(&vec![0.5f32; n], 1, rate);
                pushed += n;
            }

            let got = tap.drain().len() as i64;
            let want = (WHISPER_RATE as usize * seconds) as i64;
            // One sample of slack per chunk boundary would be a leak; the whole
            // point is that there is at most a sample or two in total.
            assert!(
                (got - want).abs() <= 2,
                "rate {rate} buf {buf}: got {got} samples, want {want} \
                 ({} lost)",
                want - got
            );
        }
    }

    /// The mic runs at whatever its device offers while system audio is pinned to
    /// 48 kHz, so the two lanes resample by different ratios. They still have to
    /// stay aligned with each other, because `drain` sums them by position.
    #[test]
    fn lanes_at_different_rates_stay_aligned() {
        let (tap, lanes) = Tap::with_lanes(2);
        let seconds = 60;
        for (lane, rate, buf) in [(0usize, 44_100u32, 512usize), (1, 48_000, 1024)] {
            let total = rate as usize * seconds;
            let mut pushed = 0;
            while pushed < total {
                let n = buf.min(total - pushed);
                lanes[lane].push(&vec![0.25f32; n], 1, rate);
                pushed += n;
            }
        }

        // `drain` returns what the lanes have in common, so a drift between them
        // shows up as a short result even though each lane was fed a full minute.
        let got = tap.drain().len() as i64;
        let want = (WHISPER_RATE as usize * seconds) as i64;
        assert!(
            (got - want).abs() <= 2,
            "lanes drifted apart: got {got}, want {want}"
        );
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

    /// The exact shape seen in the panel: words wrapped in notes. whisper.cpp's
    /// own suppression cannot reach these — the tokens are not in the vocabulary.
    #[test]
    fn music_notes_are_stripped_from_real_speech() {
        assert_eq!(strip_sound_tags("\u{266a} Just so we let you know ASAP \u{266a}"), "Just so we let you know ASAP");
        assert_eq!(
            strip_sound_tags("\u{266a} And then the team fees is huge \u{266a} \u{266a} We'll get the house of honor \u{266a}"),
            "And then the team fees is huge We'll get the house of honor"
        );
    }

    /// A line that was nothing but notes has to come out empty so `is_noise`
    /// drops it rather than the panel showing a blank row.
    #[test]
    fn a_line_of_only_sound_tags_becomes_empty() {
        assert!(strip_sound_tags("\u{266a}\u{266a}\u{266a}").is_empty());
        assert!(strip_sound_tags("[MUSIC]").is_empty());
        assert!(strip_sound_tags("(applause)").is_empty());
        assert!(strip_sound_tags("  \u{266b}  ").is_empty());
    }

    /// Brackets are ordinary speech far more often than they are tags. Cutting
    /// them all leaves a line that still reads as a sentence, so nothing
    /// downstream can tell the content was deleted.
    #[test]
    fn spoken_parentheses_survive() {
        for said in [
            "factor (x + 1)(x - 3)",
            "f(x) is continuous",
            "the derivative of (2x+1)^5",
            "sin(theta) over cos(theta)",
            "the meeting (last week) went fine",
            "call the function (it takes one argument)",
        ] {
            assert_eq!(strip_sound_tags(said), said, "mutilated: {said}");
        }
    }

    #[test]
    fn only_whole_contents_matching_a_tag_are_stripped() {
        assert!(is_sound_tag("MUSIC"));
        assert!(is_sound_tag("Blank_Audio"));
        assert!(is_sound_tag(" applause "));
        assert!(is_sound_tag("blank audio"));
        assert!(!is_sound_tag("x + 1"));
        assert!(!is_sound_tag("music of the spheres"));
        assert!(!is_sound_tag("theta"));
        assert!(!is_sound_tag(""));
    }

    #[test]
    fn a_tag_beside_real_speech_takes_only_itself() {
        assert_eq!(
            strip_sound_tags("[MUSIC] so the answer is f(x) [applause]"),
            "so the answer is f(x)"
        );
    }

    #[test]
    fn ordinary_speech_is_left_alone() {
        let plain = "Do we spectate or help? And then the other question.";
        assert_eq!(strip_sound_tags(plain), plain);
        assert_eq!(strip_sound_tags("I hate this chair."), "I hate this chair.");
    }

    #[test]
    fn silence_and_filler_are_dropped() {
        assert!(is_silent(&[0.0; 1000]));
        assert!(!is_silent(&[0.5; 1000]));
        assert!(is_noise("[BLANK_AUDIO]"));
        assert!(is_noise("Thank you."));
        assert!(!is_noise("So the midterm covers chapters four through six."));
    }

    /// The observed bug: one window per repeat, so the per-window collapse in
    /// `Transcriber::run` never sees two of them together and the panel fills
    /// with the same sentence for minutes.
    #[test]
    fn a_phrase_repeating_across_windows_is_cut_off() {
        let mut guard = RepeatGuard::default();
        let kept = (0..40)
            .filter(|_| !guard.is_looping("Our orange juice is going to decrease."))
            .count();
        assert_eq!(kept, MAX_REPEATED_LINES);
    }

    /// Whisper's own repeats vary in case and final punctuation, so the compare
    /// has to be normalized or most real loops slip through.
    #[test]
    fn repeats_are_matched_past_case_and_final_punctuation() {
        let mut guard = RepeatGuard::default();
        assert!(!guard.is_looping("I don't know."));
        assert!(!guard.is_looping("I don't know"));
        assert!(guard.is_looping("i don't know!"));
    }

    /// Real speech after the loop has to come through, and the run has to reset
    /// so the same phrase can be said again later.
    #[test]
    fn real_speech_survives_and_resets_the_run() {
        let mut guard = RepeatGuard::default();
        for _ in 0..10 {
            guard.is_looping("I don't know.");
        }
        assert!(!guard.is_looping("What the demand curve was."));
        assert!(!guard.is_looping("I don't know."));
    }
}
