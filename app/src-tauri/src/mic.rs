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

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;
use hound::{WavSpec, WavWriter};

type SharedWriter = Arc<Mutex<Option<WavWriter<std::io::BufWriter<std::fs::File>>>>>;

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
        let stop = Arc::new(AtomicBool::new(false));
        // Channel to surface a start-time error (bad device, unbuildable stream)
        // back to the caller synchronously.
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();

        let thread_stop = stop.clone();
        let thread_path = path.clone();
        let handle = std::thread::Builder::new()
            .name("oatmeal-mic".into())
            .spawn(move || run_capture(thread_path, thread_stop, ready_tx))
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

    let err_fn = |e| eprintln!("[oatmeal] mic stream error: {e}");

    // Build a stream whose callback converts whatever native format the device
    // hands us into i16 PCM and appends it to the WAV.
    let stream_res = match sample_format {
        SampleFormat::F32 => {
            let w = writer.clone();
            device.build_input_stream(
                &config,
                move |data: &[f32], _| write_samples(&w, data.iter().map(|&s| f32_to_i16(s))),
                err_fn,
                None,
            )
        }
        SampleFormat::I16 => {
            let w = writer.clone();
            device.build_input_stream(
                &config,
                move |data: &[i16], _| write_samples(&w, data.iter().copied()),
                err_fn,
                None,
            )
        }
        SampleFormat::U16 => {
            let w = writer.clone();
            device.build_input_stream(
                &config,
                move |data: &[u16], _| {
                    write_samples(&w, data.iter().map(|&s| (s as i32 - 32768) as i16))
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
