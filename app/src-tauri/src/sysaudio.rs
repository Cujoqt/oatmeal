// M3 — the system-audio lane.
//
// Captures everything the machine is playing (the far side of a Zoom/Discord
// call, a shared video, etc.) via ScreenCaptureKit, and streams it to a WAV on
// disk. ScreenCaptureKit is the only sanctioned way to tap system audio on
// modern macOS — no kernel extension, no virtual driver like BlackHole.
//
// SCK hands audio to a callback on its own dispatch queue as Float32,
// non-interleaved (one AudioBuffer per channel). We down-mix to mono i16 — that
// is exactly what Whisper wants in M4, and it halves the file — and append it to
// the WAV. `excludes_current_process_audio` keeps Oatmeal's own UI sounds out of
// the capture so we never feed the meeting back into itself.
//
// Like the mic lane, the SCStream is created and torn down on one dedicated
// thread; the WAV writer is the only state shared with the audio callback,
// behind an `Arc<Mutex<..>>`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use hound::{WavSpec, WavWriter};

use crate::live::Lane;
use screencapturekit::prelude::{
    CMSampleBuffer, CMSampleBufferExt, SCContentFilter, SCShareableContent, SCStream,
    SCStreamConfiguration, SCStreamOutputType,
};

/// Sample rate we ask ScreenCaptureKit to deliver. 48 kHz is SCK's native rate;
/// M4 will downsample to 16 kHz for Whisper.
const SAMPLE_RATE: u32 = 48_000;

type SharedWriter = Arc<Mutex<Option<WavWriter<std::io::BufWriter<std::fs::File>>>>>;

/// A running system-audio capture. Dropping it (or calling `stop`) ends the
/// stream and finalizes the WAV header.
pub struct SysAudioRecorder {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<Result<(), String>>>,
    path: PathBuf,
}

impl SysAudioRecorder {
    /// Begin capturing system audio into `path` (a `.wav`). Returns once the
    /// stream is live, or an error (no display, capture permission denied, …).
    pub fn start(path: PathBuf) -> Result<Self, String> {
        Self::start_with_tap(path, None)
    }

    /// As `start`, but also mirror the captured audio into `tap` so the live
    /// transcription worker can decode the far side of the call as it arrives.
    pub fn start_with_tap(path: PathBuf, tap: Option<Lane>) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();

        let thread_stop = stop.clone();
        let thread_path = path.clone();
        let handle = std::thread::Builder::new()
            .name("oatmeal-sysaudio".into())
            .spawn(move || run_capture(thread_path, thread_stop, ready_tx, tap))
            .map_err(|e| format!("spawn sysaudio thread: {e}"))?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                stop,
                handle: Some(handle),
                path,
            }),
            Ok(Err(e)) => {
                let _ = handle.join();
                Err(e)
            }
            Err(_) => {
                let _ = handle.join();
                Err("sysaudio thread exited before signaling readiness".into())
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
                Err(_) => Err("sysaudio thread panicked".into()),
            }
        } else {
            Ok(())
        }
    }
}

impl Drop for SysAudioRecorder {
    fn drop(&mut self) {
        let _ = self.signal_and_join();
    }
}

/// Runs on the dedicated capture thread: builds the SCStream, starts it, parks
/// until `stop` flips, then finalizes the WAV.
fn run_capture(
    path: PathBuf,
    stop: Arc<AtomicBool>,
    ready: std::sync::mpsc::Sender<Result<(), String>>,
    tap: Option<Lane>,
) -> Result<(), String> {
    macro_rules! bail {
        ($e:expr) => {{
            let e = $e;
            let _ = ready.send(Err(e.clone()));
            return Err(e);
        }};
    }

    // Pick the main display. SCK requires a content filter even for audio-only
    // capture; the display just anchors the stream — we never read its frames.
    let content = match SCShareableContent::get() {
        Ok(c) => c,
        Err(e) => bail!(format!(
            "screen-capture content unavailable (permission?): {e}"
        )),
    };
    let display = match content.displays().into_iter().next() {
        Some(d) => d,
        None => bail!("no display available for system-audio capture".to_string()),
    };

    let filter = SCContentFilter::create()
        .with_display(&display)
        .with_excluding_windows(&[])
        .build();

    // Audio on, our own process excluded, video kept tiny (SCK still produces
    // frames, but we register no Screen handler so they cost almost nothing).
    let config = SCStreamConfiguration::new()
        .with_width(128)
        .with_height(128)
        .with_captures_audio(true)
        .with_sample_rate(SAMPLE_RATE as i32)
        .with_channel_count(2)
        .with_excludes_current_process_audio(true);

    let spec = WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let writer = match WavWriter::create(&path, spec) {
        Ok(w) => w,
        Err(e) => bail!(format!("create wav {}: {e}", path.display())),
    };
    let writer: SharedWriter = Arc::new(Mutex::new(Some(writer)));

    let cb_writer = writer.clone();
    let cb_tap = tap.clone();
    let mut stream = SCStream::new(&filter, &config);
    stream.add_output_handler(
        move |sample: CMSampleBuffer, of_type: SCStreamOutputType| {
            if of_type == SCStreamOutputType::Audio {
                write_audio_sample(&cb_writer, cb_tap.as_ref(), &sample);
            }
        },
        SCStreamOutputType::Audio,
    );

    if let Err(e) = stream.start_capture() {
        bail!(format!("start system-audio capture: {e}"));
    }

    // Stream is live — unblock `start`.
    let _ = ready.send(Ok(()));

    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    // Stop the stream first so no more callbacks fire, then finalize the WAV.
    if let Err(e) = stream.stop_capture() {
        eprintln!("[oatmeal] stop system-audio capture: {e}");
    }
    if let Some(w) = writer.lock().unwrap().take() {
        w.finalize().map_err(|e| format!("finalize wav: {e}"))?;
    }
    Ok(())
}

/// Down-mix one SCK audio sample buffer to mono i16 and append it. Runs on SCK's
/// dispatch queue. SCK delivers Float32 non-interleaved: one `AudioBuffer` per
/// channel, each holding the same number of frames. Averaging the channels gives
/// a mono track; if only one buffer is present it passes through unchanged.
fn write_audio_sample(writer: &SharedWriter, tap: Option<&Lane>, sample: &CMSampleBuffer) {
    let list = match sample.audio_buffer_list() {
        Some(l) => l,
        None => return,
    };
    let num_buffers = list.num_buffers();
    if num_buffers == 0 {
        return;
    }

    // Reinterpret each channel buffer's bytes as f32 samples.
    let channels: Vec<&[f32]> = (0..num_buffers)
        .filter_map(|i| list.get(i))
        .map(|buf| {
            let bytes = buf.data();
            // SAFETY: SCK guarantees Float32 PCM; the byte slice is 4-byte aligned
            // and sized to whole f32s. We only read.
            unsafe {
                std::slice::from_raw_parts(bytes.as_ptr() as *const f32, bytes.len() / 4)
            }
        })
        .collect();

    if channels.is_empty() {
        return;
    }
    let frames = channels.iter().map(|c| c.len()).min().unwrap_or(0);
    if frames == 0 {
        return;
    }

    let mut guard = match writer.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    let w = match guard.as_mut() {
        Some(w) => w,
        None => return,
    };

    let inv = 1.0 / num_buffers as f32;
    let mut mono_frames: Vec<f32> = if tap.is_some() {
        Vec::with_capacity(frames)
    } else {
        Vec::new()
    };
    for frame in 0..frames {
        let mut acc = 0.0f32;
        for ch in &channels {
            acc += ch[frame];
        }
        let mono = (acc * inv).clamp(-1.0, 1.0);
        let _ = w.write_sample((mono * i16::MAX as f32) as i16);
        if tap.is_some() {
            mono_frames.push(mono);
        }
    }

    // Release the writer lock before touching the tap so the two never contend.
    drop(guard);
    if let Some(tap) = tap {
        // Already mono at SCK's configured rate.
        tap.push(&mono_frames, 1, SAMPLE_RATE);
    }
}
