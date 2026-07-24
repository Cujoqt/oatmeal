mod mic;
pub mod model;
pub mod session;
mod sysaudio;
pub mod transcribe;
mod window;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tauri::Manager;

use mic::MicRecorder;
use sysaudio::SysAudioRecorder;

/// Global app state. M1 added the hide flag; M2 the microphone lane; M3 the
/// system-audio lane. Later milestones add the transcription worker handles.
pub struct AppState {
    /// Whether the main window is currently hidden from screen capture.
    pub hidden_from_capture: AtomicBool,
    /// The active microphone capture, if recording.
    pub mic: Mutex<Option<MicRecorder>>,
    /// The active system-audio capture, if recording.
    pub sysaudio: Mutex<Option<SysAudioRecorder>>,
    /// Paths for the in-progress meeting, if a session is running.
    pub session: Mutex<Option<session::SessionPaths>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            // Default ON — the whole point of the app is to be invisible by default.
            hidden_from_capture: AtomicBool::new(true),
            mic: Mutex::new(None),
            sysaudio: Mutex::new(None),
            session: Mutex::new(None),
        }
    }
}

/// Toggle/set the screen-share hide flag for the main window.
#[tauri::command]
fn set_hidden_from_capture(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    hidden: bool,
) -> Result<(), String> {
    window::apply_on_main(&app, "main", hidden)?;
    state.hidden_from_capture.store(hidden, Ordering::SeqCst);
    Ok(())
}

/// Read the current hide flag (UI reads this on load to sync its toggle).
#[tauri::command]
fn is_hidden_from_capture(state: tauri::State<'_, AppState>) -> bool {
    state.hidden_from_capture.load(Ordering::SeqCst)
}

/// Start capturing the default microphone into `path` (a `.wav`). Errors if a
/// capture is already running or the device can't be opened.
#[tauri::command]
fn start_mic_recording(state: tauri::State<'_, AppState>, path: String) -> Result<(), String> {
    let mut slot = state.mic.lock().map_err(|_| "mic state poisoned")?;
    if slot.is_some() {
        return Err("microphone is already recording".into());
    }
    let recorder = MicRecorder::start(PathBuf::from(path))?;
    *slot = Some(recorder);
    Ok(())
}

/// Stop the active microphone capture and finalize the WAV. Returns the path that
/// was written. Errors if nothing was recording.
#[tauri::command]
fn stop_mic_recording(state: tauri::State<'_, AppState>) -> Result<String, String> {
    let recorder = {
        let mut slot = state.mic.lock().map_err(|_| "mic state poisoned")?;
        slot.take().ok_or("microphone is not recording")?
    };
    let path = recorder.path().display().to_string();
    recorder.stop()?;
    Ok(path)
}

/// Whether the microphone lane is currently capturing.
#[tauri::command]
fn is_mic_recording(state: tauri::State<'_, AppState>) -> bool {
    state
        .mic
        .lock()
        .map(|slot| slot.is_some())
        .unwrap_or(false)
}

/// Start capturing system audio (via ScreenCaptureKit) into `path` (a `.wav`).
#[tauri::command]
fn start_sysaudio_recording(
    state: tauri::State<'_, AppState>,
    path: String,
) -> Result<(), String> {
    let mut slot = state.sysaudio.lock().map_err(|_| "sysaudio state poisoned")?;
    if slot.is_some() {
        return Err("system audio is already recording".into());
    }
    let recorder = SysAudioRecorder::start(PathBuf::from(path))?;
    *slot = Some(recorder);
    Ok(())
}

/// Stop the active system-audio capture and finalize the WAV. Returns its path.
#[tauri::command]
fn stop_sysaudio_recording(state: tauri::State<'_, AppState>) -> Result<String, String> {
    let recorder = {
        let mut slot = state.sysaudio.lock().map_err(|_| "sysaudio state poisoned")?;
        slot.take().ok_or("system audio is not recording")?
    };
    let path = recorder.path().display().to_string();
    recorder.stop()?;
    Ok(path)
}

/// Whether the system-audio lane is currently capturing.
#[tauri::command]
fn is_sysaudio_recording(state: tauri::State<'_, AppState>) -> bool {
    state
        .sysaudio
        .lock()
        .map(|slot| slot.is_some())
        .unwrap_or(false)
}

/// Transcribe a WAV file on disk with on-device Whisper (M4). `model_path` may be
/// empty to use `OATMEAL_MODEL` / the default model location; `language` empty
/// means auto-detect. Blocking — Tauri runs commands off the UI thread.
#[tauri::command]
fn transcribe_wav(
    model_path: String,
    wav_path: String,
    language: String,
) -> Result<transcribe::Transcript, String> {
    let lang = if language.trim().is_empty() {
        None
    } else {
        Some(language.trim())
    };
    transcribe::transcribe_wav(&model_path, Path::new(&wav_path), lang)
}

/// Where Oatmeal expects the Whisper model on disk (UI shows this / drives the
/// download step).
#[tauri::command]
fn default_model_path() -> String {
    transcribe::default_model_path().display().to_string()
}

/// Ensure the Whisper model is present, downloading it on first run. Returns the
/// model path. Blocking (can take a while on a cold download).
#[tauri::command]
fn ensure_model() -> Result<String, String> {
    model::ensure_model().map(|p| p.display().to_string())
}

// ── Meeting session (M6) — the product-level record→transcribe flow ──────────

/// Start a meeting: create its folder and fire up both audio lanes. A lane that
/// can't start (no mic, denied screen-capture) is logged and skipped rather than
/// failing the whole meeting — but if *neither* starts, that's an error.
#[tauri::command]
fn start_session(
    state: tauri::State<'_, AppState>,
    title: String,
) -> Result<session::SessionPaths, String> {
    let mut sess = state.session.lock().map_err(|_| "session state poisoned")?;
    if sess.is_some() {
        return Err("a meeting is already being recorded".into());
    }
    let paths = session::new_session(&title)?;

    let mut any = false;
    {
        let mut m = state.mic.lock().map_err(|_| "mic state poisoned")?;
        if m.is_none() {
            match MicRecorder::start(PathBuf::from(&paths.mic_wav)) {
                Ok(r) => {
                    *m = Some(r);
                    any = true;
                }
                Err(e) => eprintln!("[oatmeal] mic lane did not start: {e}"),
            }
        }
    }
    {
        let mut s = state.sysaudio.lock().map_err(|_| "sysaudio state poisoned")?;
        if s.is_none() {
            match SysAudioRecorder::start(PathBuf::from(&paths.sys_wav)) {
                Ok(r) => {
                    *s = Some(r);
                    any = true;
                }
                Err(e) => eprintln!("[oatmeal] system-audio lane did not start: {e}"),
            }
        }
    }

    if !any {
        return Err("neither the microphone nor system-audio lane could start — check permissions".into());
    }
    *sess = Some(paths.clone());
    Ok(paths)
}

/// Whether a meeting is currently being recorded.
#[tauri::command]
fn is_session_active(state: tauri::State<'_, AppState>) -> bool {
    state
        .session
        .lock()
        .map(|s| s.is_some())
        .unwrap_or(false)
}

/// Stop the meeting: end both lanes, mix + transcribe, and write `transcript.md`.
/// `model_path`/`language` may be empty to use the fallbacks.
#[tauri::command]
fn stop_session(
    state: tauri::State<'_, AppState>,
    model_path: String,
    language: String,
) -> Result<session::MeetingResult, String> {
    let paths = {
        let mut sess = state.session.lock().map_err(|_| "session state poisoned")?;
        sess.take().ok_or("no meeting is being recorded")?
    };

    if let Some(r) = state
        .mic
        .lock()
        .map_err(|_| "mic state poisoned")?
        .take()
    {
        if let Err(e) = r.stop() {
            eprintln!("[oatmeal] mic stop: {e}");
        }
    }
    if let Some(r) = state
        .sysaudio
        .lock()
        .map_err(|_| "sysaudio state poisoned")?
        .take()
    {
        if let Err(e) = r.stop() {
            eprintln!("[oatmeal] sysaudio stop: {e}");
        }
    }

    let lang = if language.trim().is_empty() {
        None
    } else {
        Some(language.trim())
    };
    session::finish(
        Path::new(&paths.dir),
        &paths.title,
        Path::new(&paths.mic_wav),
        Path::new(&paths.sys_wav),
        &model_path,
        lang,
    )
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            set_hidden_from_capture,
            is_hidden_from_capture,
            start_mic_recording,
            stop_mic_recording,
            is_mic_recording,
            start_sysaudio_recording,
            stop_sysaudio_recording,
            is_sysaudio_recording,
            transcribe_wav,
            default_model_path,
            ensure_model,
            start_session,
            stop_session,
            is_session_active
        ])
        .setup(|app| {
            // Apply the hide flag as soon as the window exists.
            let hidden = app
                .state::<AppState>()
                .hidden_from_capture
                .load(Ordering::SeqCst);
            if let Some(win) = app.get_webview_window("main") {
                if let Err(e) = window::set_hidden_from_capture(&win, hidden) {
                    eprintln!("[oatmeal] initial hide failed: {e}");
                }
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Oatmeal");
}
