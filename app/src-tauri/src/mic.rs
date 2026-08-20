// M2 — the microphone lane.
//
// Captures the default input device with cpal and streams it to a 16-bit PCM WAV
// on disk via hound. cpal's `Stream` is `!Send`, so it must live on the thread
// that created it; we spawn a dedicated capture thread, build + play the stream
// there, and park it until a stop flag flips. The WAV writer is shared with the
// audio callback through an `Arc<Mutex<..>>` (the only thing that crosses the
// thread boundary), and finalized when the thread unwinds.
//
// Later milestones mix this lane with the system-audio lane (M3) before handing
// PCM to Whisper (M4); for now it stands alone so the mic path can be verified.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;
use hound::{WavSpec, WavWriter};

use crate::live::Lane;

type SharedWriter = Arc<Mutex<Option<WavWriter<std::io::BufWriter<std::fs::File>>>>>;

/// How long each measurement window is. Long enough that callback jitter and
/// stream start-up latency wash out, short enough that the live lane is handed
/// the right rate within a few seconds of the device moving.
///
/// The window repeats for the whole recording rather than settling once: the
/// device that caused this moved its rate twenty minutes into a take, so a meter
/// that made up its mind in the first ten seconds would have missed it entirely.
const WINDOW_MS: u64 = 5_000;

/// Rates a real device actually runs at. A measurement is only believed when it
/// lands on one of these, so a burst of late callbacks can't invent a rate.
const STANDARD_RATES: [u32; 12] = [
    8_000, 11_025, 16_000, 22_050, 24_000, 32_000, 44_100, 48_000, 88_200, 96_000, 176_400,
    192_000,
];

/// What the device is *really* delivering, as opposed to what it said it would.
///
/// cpal reads the device's nominal rate once, when the stream is built, and
/// never revisits it. CoreAudio lets another app move that rate underneath a
/// running stream — joining a Zoom call does exactly this — and when it happens
/// the samples keep arriving at the new rate while the label stays stale. Every
/// clock in Oatmeal is derived from that label, so a device that doubles its
/// rate makes a 33-minute meeting report 67 minutes and hands the live worker
/// twice the audio it can decode in real time.
///
/// So count frames against the wall clock and, once there is enough to be sure,
/// believe the count instead.
struct RateMeter {
    declared: u32,
    started: Instant,
    /// Frames seen since the stream began, and where the open window started.
    frames: AtomicU64,
    mark_ms: AtomicU64,
    mark_frames: AtomicU64,
    /// The rate the most recent closed window measured. What the tap is told.
    rate: AtomicU32,
    /// Frames attributed to each entry of `STANDARD_RATES`, so a file recorded
    /// across a rate change can still be labelled with whichever rate covered
    /// most of it.
    tally: [AtomicU64; STANDARD_RATES.len()],
}

impl RateMeter {
    fn new(declared: u32) -> Self {
        Self {
            declared,
            started: Instant::now(),
            frames: AtomicU64::new(0),
            mark_ms: AtomicU64::new(0),
            mark_frames: AtomicU64::new(0),
            rate: AtomicU32::new(declared),
            tally: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }

    /// Record `frames` more frames and return the rate to use for them. Runs on
    /// the realtime audio thread, so it only touches atomics.
    fn observe(&self, frames: usize) -> u32 {
        self.observe_at(frames, self.started.elapsed().as_millis() as u64)
    }

    /// `observe` with the clock supplied, so the windowing can be tested without
    /// a test that has to sit and wait for real seconds to pass.
    fn observe_at(&self, frames: usize, now_ms: u64) -> u32 {
        let total = self.frames.fetch_add(frames as u64, Ordering::Relaxed) + frames as u64;
        let mark_ms = self.mark_ms.load(Ordering::Relaxed);

        if now_ms.saturating_sub(mark_ms) >= WINDOW_MS {
            // Claim the window before measuring it: two callbacks can arrive at
            // once, and only one of them should close it.
            if self
                .mark_ms
                .compare_exchange(mark_ms, now_ms, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                let since = total - self.mark_frames.swap(total, Ordering::Relaxed);
                let secs = (now_ms - mark_ms) as f64 / 1000.0;
                let rate = snap_rate(since as f64 / secs, self.declared);
                self.rate.store(rate, Ordering::Relaxed);
                if let Some(i) = STANDARD_RATES.iter().position(|&r| r == rate) {
                    self.tally[i].fetch_add(since, Ordering::Relaxed);
                }
            }
        }
        self.rate.load(Ordering::Relaxed)
    }

    /// The rate the finished file should claim: whichever one covered the most
    /// of it. A file whose rate genuinely moved mid-take cannot be labelled
    /// correctly by a single number — this is the least wrong one available.
    fn dominant_rate(&self) -> u32 {
        let mut best = (0u64, self.declared);
        for (i, seen) in self.tally.iter().enumerate() {
            let n = seen.load(Ordering::Relaxed);
            if n > best.0 {
                best = (n, STANDARD_RATES[i]);
            }
        }
        best.1
    }

    /// Whether the device was seen running at more than one rate during the take.
    fn rate_moved(&self) -> bool {
        self.tally
            .iter()
            .filter(|seen| seen.load(Ordering::Relaxed) > 0)
            .count()
            > 1
    }
}

/// The standard rate a measurement is really showing, or `declared` when the
/// measurement agrees with it or is too far from any real rate to trust.
fn snap_rate(measured: f64, declared: u32) -> u32 {
    if !measured.is_finite() || measured <= 0.0 {
        return declared;
    }
    // Within a few percent of the label is just callback jitter, not a device
    // that moved.
    if (measured - declared as f64).abs() / declared as f64 <= 0.05 {
        return declared;
    }
    let nearest = STANDARD_RATES
        .iter()
        .copied()
        .min_by(|a, b| {
            let da = (*a as f64 - measured).abs();
            let db = (*b as f64 - measured).abs();
            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap_or(declared);
    if (nearest as f64 - measured).abs() / nearest as f64 <= 0.05 {
        nearest
    } else {
        declared
    }
}

/// Rewrite the sample rate in a finished WAV's `fmt ` chunk.
///
/// The samples are right — only the label was wrong — so correcting the header
/// is what makes the file play at the speed it was recorded at, and what makes
/// every duration and timestamp derived from it correct. hound writes the header
/// when the file is created, long before the true rate is known, and only
/// patches the sizes in `finalize`.
fn patch_wav_rate(path: &Path, rate: u32, channels: u16) -> Result<(), String> {
    use std::io::{Read, Seek, SeekFrom, Write};

    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| format!("open wav to correct its rate: {e}"))?;

    // Find `fmt ` rather than assuming it sits at byte 12: hound is free to put
    // other chunks first, and a wrong offset would corrupt the file.
    let mut head = [0u8; 128];
    let read = file
        .read(&mut head)
        .map_err(|e| format!("read wav header: {e}"))?;
    let fmt = head[..read]
        .windows(4)
        .position(|w| w == b"fmt ")
        .ok_or_else(|| "no fmt chunk in wav".to_string())?;

    // Inside the chunk body: rate at +4, byte rate at +8.
    let body = (fmt + 8) as u64;
    file.seek(SeekFrom::Start(body + 4))
        .map_err(|e| format!("seek wav fmt chunk: {e}"))?;
    file.write_all(&rate.to_le_bytes())
        .map_err(|e| format!("write wav sample rate: {e}"))?;
    let byte_rate = rate * channels as u32 * 2;
    file.write_all(&byte_rate.to_le_bytes())
        .map_err(|e| format!("write wav byte rate: {e}"))?;
    Ok(())
}

/// A running microphone capture. Dropping it (or calling `stop`) ends the stream
/// and finalizes the WAV header.
pub struct MicRecorder {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<Result<(), String>>>,
    path: PathBuf,
}

impl MicRecorder {
    /// Begin capturing the default input device into `path` (a `.wav`). Returns
    /// once the stream is live, or an error describing why it couldn't start.
    pub fn start(path: PathBuf) -> Result<Self, String> {
        Self::start_with_tap(path, None)
    }

    /// As `start`, but also mirror the captured audio into `tap` so the live
    /// transcription worker can decode it while the meeting is still running.
    pub fn start_with_tap(path: PathBuf, tap: Option<Lane>) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        // Channel to surface a start-time error (bad device, unbuildable stream)
        // back to the caller synchronously.
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();

        let thread_stop = stop.clone();
        let thread_path = path.clone();
        let handle = std::thread::Builder::new()
            .name("oatmeal-mic".into())
            .spawn(move || run_capture(thread_path, thread_stop, ready_tx, tap))
            .map_err(|e| format!("spawn mic thread: {e}"))?;

        // Wait for the capture thread to report the stream is up (or failed).
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                stop,
                handle: Some(handle),
                path,
            }),
            Ok(Err(e)) => {
                // Thread already bailed; join to reap it.
                let _ = handle.join();
                Err(e)
            }
            Err(_) => {
                let _ = handle.join();
                Err("mic thread exited before signaling readiness".into())
            }
        }
    }

    /// Path of the WAV being written.
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// Stop capture, finalize the WAV, and return once the thread is joined.
    pub fn stop(mut self) -> Result<(), String> {
        self.signal_and_join()
    }

    fn signal_and_join(&mut self) -> Result<(), String> {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            match handle.join() {
                Ok(res) => res,
                Err(_) => Err("mic thread panicked".into()),
            }
        } else {
            Ok(())
        }
    }
}

impl Drop for MicRecorder {
    fn drop(&mut self) {
        // Best-effort stop if the caller dropped us without calling `stop`.
        let _ = self.signal_and_join();
    }
}

/// Runs on the dedicated capture thread: builds the stream, plays it, and parks
/// until `stop` flips, then finalizes the WAV.
fn run_capture(
    path: PathBuf,
    stop: Arc<AtomicBool>,
    ready: std::sync::mpsc::Sender<Result<(), String>>,
    tap: Option<Lane>,
) -> Result<(), String> {
    // Anything that fails during setup is reported via `ready` so `start` can
    // return it synchronously; a helper keeps that plumbing tidy.
    macro_rules! bail {
        ($e:expr) => {{
            let e = $e;
            let _ = ready.send(Err(e.clone()));
            return Err(e);
        }};
    }

    let host = cpal::default_host();
    let device = match host.default_input_device() {
        Some(d) => d,
        None => bail!("no default input device (microphone) found".to_string()),
    };
    let supported = match device.default_input_config() {
        Ok(c) => c,
        Err(e) => bail!(format!("default input config: {e}")),
    };

    let sample_format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();
    let channels = config.channels;
    let sample_rate = config.sample_rate.0;

    let spec = WavSpec {
        channels,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let writer = match WavWriter::create(&path, spec) {
        Ok(w) => w,
        Err(e) => bail!(format!("create wav {}: {e}", path.display())),
    };
    let writer: SharedWriter = Arc::new(Mutex::new(Some(writer)));

    // `sample_rate` above is only what the device *claimed* when the stream was
    // built. Watch what it actually delivers; see `RateMeter`.
    let meter = Arc::new(RateMeter::new(sample_rate));
    let frames_in = move |samples: usize| samples / channels.max(1) as usize;

    let err_fn = |e| eprintln!("[oatmeal] mic stream error: {e}");

    // Build a stream whose callback converts whatever native format the device
    // hands us into i16 PCM and appends it to the WAV.
    // Feeding the live tap needs f32; convert once per callback and reuse it for
    // both the WAV and the tap so non-f32 devices don't pay twice.
    let stream_res = match sample_format {
        SampleFormat::F32 => {
            let w = writer.clone();
            let t = tap.clone();
            let m = meter.clone();
            device.build_input_stream(
                &config,
                move |data: &[f32], _| {
                    let rate = m.observe(frames_in(data.len()));
                    write_samples(&w, data.iter().map(|&s| f32_to_i16(s)));
                    if let Some(t) = &t {
                        t.push(data, channels, rate);
                    }
                },
                err_fn,
                None,
            )
        }
        SampleFormat::I16 => {
            let w = writer.clone();
            let t = tap.clone();
            let m = meter.clone();
            device.build_input_stream(
                &config,
                move |data: &[i16], _| {
                    let rate = m.observe(frames_in(data.len()));
                    write_samples(&w, data.iter().copied());
                    if let Some(t) = &t {
                        let f: Vec<f32> =
                            data.iter().map(|&s| s as f32 / i16::MAX as f32).collect();
                        t.push(&f, channels, rate);
                    }
                },
                err_fn,
                None,
            )
        }
        SampleFormat::U16 => {
            let w = writer.clone();
            let t = tap.clone();
            let m = meter.clone();
            device.build_input_stream(
                &config,
                move |data: &[u16], _| {
                    let rate = m.observe(frames_in(data.len()));
                    write_samples(&w, data.iter().map(|&s| (s as i32 - 32768) as i16));
                    if let Some(t) = &t {
                        let f: Vec<f32> = data
                            .iter()
                            .map(|&s| (s as i32 - 32768) as f32 / i16::MAX as f32)
                            .collect();
                        t.push(&f, channels, rate);
                    }
                },
                err_fn,
                None,
            )
        }
        other => bail!(format!("unsupported sample format: {other:?}")),
    };

    let stream = match stream_res {
        Ok(s) => s,
        Err(e) => bail!(format!("build input stream: {e}")),
    };
    if let Err(e) = stream.play() {
        bail!(format!("start input stream: {e}"));
    }

    // Stream is live — unblock `start`.
    let _ = ready.send(Ok(()));

    // Park until asked to stop. The stream stays alive because it (and its
    // callback) are owned here on this thread.
    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    // Drop the stream first so no more callbacks fire, then finalize the WAV.
    drop(stream);
    if let Some(w) = writer.lock().unwrap().take() {
        w.finalize().map_err(|e| format!("finalize wav: {e}"))?;
    }

    // The header was written before anyone knew what the device would really
    // do. If it moved, correct the label now that the file is closed — a failure
    // here is worth reporting but must not lose the recording.
    let measured = meter.dominant_rate();
    if meter.rate_moved() {
        eprintln!(
            "[oatmeal] mic device changed rate mid-take; {} is labelled {measured} Hz, the rate it ran at longest",
            path.display()
        );
    }
    if measured != sample_rate {
        eprintln!(
            "[oatmeal] mic device declared {sample_rate} Hz but delivered {measured} Hz; correcting {}",
            path.display()
        );
        if let Err(e) = patch_wav_rate(&path, measured, channels) {
            eprintln!("[oatmeal] correct mic wav rate: {e}");
        }
    }
    Ok(())
}

/// Append i16 samples to the shared writer. Runs on cpal's realtime audio thread,
/// so it stays allocation-light and never blocks on anything but the writer lock.
fn write_samples<I: Iterator<Item = i16>>(writer: &SharedWriter, samples: I) {
    let mut guard = match writer.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    if let Some(w) = guard.as_mut() {
        for s in samples {
            // Ignore per-sample write errors (e.g. disk full) rather than panic
            // on the audio thread; the finalize step will surface a truncated file.
            let _ = w.write_sample(s);
        }
    }
}

/// Convert a normalized f32 sample (`-1.0..=1.0`) to i16 with clamping.
#[inline]
fn f32_to_i16(s: f32) -> i16 {
    let clamped = s.clamp(-1.0, 1.0);
    (clamped * i16::MAX as f32) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact failure seen in the field: the device declared 24 kHz and
    /// delivered 48 kHz, which made a 33-minute meeting report 67 minutes.
    #[test]
    fn a_device_that_doubles_its_rate_is_measured_not_believed() {
        assert_eq!(snap_rate(48_332.0, 24_000), 48_000);
    }

    #[test]
    fn jitter_around_the_declared_rate_leaves_it_alone() {
        assert_eq!(snap_rate(23_940.0, 24_000), 24_000);
        assert_eq!(snap_rate(48_210.0, 48_000), 48_000);
    }

    /// A measurement that lands nowhere near a real rate — a stalled stream, a
    /// burst of late callbacks — must not be allowed to invent one.
    #[test]
    fn a_measurement_off_every_standard_rate_is_ignored() {
        assert_eq!(snap_rate(3.0, 44_100), 44_100);
        assert_eq!(snap_rate(60_000.0, 44_100), 44_100);
        assert_eq!(snap_rate(f64::NAN, 44_100), 44_100);
        assert_eq!(snap_rate(0.0, 44_100), 44_100);
    }

    #[test]
    fn the_meter_reports_the_declared_rate_until_a_window_closes() {
        let meter = RateMeter::new(24_000);
        assert_eq!(meter.observe_at(4_800, 100), 24_000);
        assert_eq!(meter.observe_at(4_800, 4_999), 24_000);
        assert_eq!(meter.dominant_rate(), 24_000);
    }

    /// Feed one second of frames per simulated second at `rate`.
    fn feed(meter: &RateMeter, rate: u32, secs: u64, from_ms: u64) -> u32 {
        let mut last = 0;
        for s in 0..secs {
            last = meter.observe_at(rate as usize, from_ms + (s + 1) * 1_000);
        }
        last
    }

    /// The failure exactly as it happened: the device is honest for the opening
    /// stretch, then doubles its rate twenty minutes in when a call joins. A
    /// meter that made its mind up early would never notice.
    #[test]
    fn a_rate_change_partway_through_a_take_is_caught() {
        let meter = RateMeter::new(24_000);
        assert_eq!(feed(&meter, 24_000, 1_200, 0), 24_000);
        assert_eq!(feed(&meter, 48_000, 60, 1_200_000), 48_000);
        assert!(meter.rate_moved());
    }

    /// A file recorded across a change gets the rate it spent longest at, since
    /// one number cannot describe it correctly.
    #[test]
    fn the_finished_file_is_labelled_with_the_rate_it_ran_at_longest() {
        let meter = RateMeter::new(24_000);
        feed(&meter, 24_000, 60, 0);
        feed(&meter, 48_000, 600, 60_000);
        assert_eq!(meter.dominant_rate(), 48_000);
        assert!(meter.rate_moved());
    }

    #[test]
    fn a_device_that_never_moves_is_not_reported_as_moving() {
        let meter = RateMeter::new(48_000);
        feed(&meter, 48_000, 120, 0);
        assert!(!meter.rate_moved());
        assert_eq!(meter.dominant_rate(), 48_000);
    }

    /// The header is written before the true rate is known, so correcting it
    /// afterwards is what makes every duration derived from the file right.
    #[test]
    fn patching_a_finished_wav_corrects_its_rate_without_touching_the_audio() {
        let dir = std::env::temp_dir().join(format!("oatmeal-rate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mic.wav");

        let spec = WavSpec {
            channels: 1,
            sample_rate: 24_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = WavWriter::create(&path, spec).unwrap();
        for i in 0..1_000i32 {
            w.write_sample((i % 100) as i16).unwrap();
        }
        w.finalize().unwrap();

        patch_wav_rate(&path, 48_000, 1).unwrap();

        let reader = hound::WavReader::open(&path).unwrap();
        assert_eq!(reader.spec().sample_rate, 48_000);
        assert_eq!(reader.spec().channels, 1);
        assert_eq!(reader.len(), 1_000, "audio must be untouched");
        let samples: Vec<i16> = reader.into_samples::<i16>().map(|s| s.unwrap()).collect();
        assert_eq!(samples[0], 0);
        assert_eq!(samples[101], 1);

        std::fs::remove_dir_all(&dir).ok();
    }
}
