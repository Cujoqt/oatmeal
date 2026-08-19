pub mod apple_calendar;
mod autoanswer;
pub mod chat;
pub mod diarize;
pub mod homework;
pub mod library;
pub mod live;
mod mic;
pub mod model;
pub mod recall;
pub mod session;
pub mod settings;
pub mod store;
mod sleep;
mod sysaudio;
pub mod transcribe;
pub mod update;
pub mod video;
mod window;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tauri::Manager;

use live::{LiveSession, Tap};
use mic::MicRecorder;
use sysaudio::SysAudioRecorder;

/// Event carrying each piece of a streamed chat answer: `{ seq, text }`.
const CHAT_TOKEN_EVENT: &str = "oatmeal://chat-token";

/// Pieces of a streamed live auto-answer: `{ seq, text }`. Separate from
/// `CHAT_TOKEN_EVENT` so the panel's answer stream never interleaves with an
/// answer the user typed into the note window's ask box.
const LIVE_ANSWER_EVENT: &str = "oatmeal://live-answer";

/// Event telling the UI the machine slept mid-recording: `{ asleep_ms }`. The
/// audio for that stretch does not exist, so the take is stopped rather than
/// left with a hole in it.
const SLEPT_EVENT: &str = "oatmeal://slept";

/// Label of the floating live-transcript window declared in `tauri.conf.json`.
const TRANSCRIPT_WINDOW: &str = "transcript";

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
    /// When the take now recording started, for the on-screen clock.
    ///
    /// Deliberately not the meeting folder's creation date: continuing a meeting
    /// records into the folder it already had, so that date is when the meeting
    /// was *first* started — a day ago, if it was yesterday's — and the clock
    /// would open at that. This is only ever read while a session is live, and a
    /// session cannot outlive the process holding it, so there is nothing to
    /// recover after a restart.
    pub take_started: Mutex<Option<std::time::Instant>>,
    /// Live transcription worker for the in-progress meeting.
    pub live: Mutex<Option<LiveSession>>,
    /// Notes typed before any meeting existed, flushed to disk once one does.
    pub pending_notes: Mutex<Option<(String, String)>>,
    /// Folder of the most recent meeting, so notes typed *after* it stopped keep
    /// landing next to that meeting's audio instead of vanishing.
    pub last_dir: Mutex<Option<String>>,
    /// Rate limiter for the live panel's auto-answers, so answering questions
    /// can't starve the live transcription that shares the same GPU.
    pub auto_answer: Mutex<autoanswer::Gate>,
}

impl Default for AppState {
    fn default() -> Self {
        // Default ON — the whole point of the app is to be invisible by default.
        // `OATMEAL_VISIBLE=1` starts it visible instead, which is the only way to
        // screenshot or screen-record the UI (for docs, bug reports, or dev): a
        // window with `sharingType = none` captures as blank, including to our own
        // tooling. The in-app toggle can flip it back at any time.
        let hidden = std::env::var("OATMEAL_VISIBLE")
            .map(|v| v.trim().is_empty() || v == "0")
            .unwrap_or(true);
        Self {
            hidden_from_capture: AtomicBool::new(hidden),
            mic: Mutex::new(None),
            sysaudio: Mutex::new(None),
            session: Mutex::new(None),
            take_started: Mutex::new(None),
            live: Mutex::new(None),
            pending_notes: Mutex::new(None),
            last_dir: Mutex::new(None),
            auto_answer: Mutex::new(autoanswer::Gate::default()),
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
    // The floating transcript window shows the same meeting content, so it has to
    // follow the same rule — hiding one and not the other would leak everything.
    let _ = window::apply_on_main(&app, TRANSCRIPT_WINDOW, hidden);
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
/// means auto-detect. Blocking, and *not* off the UI thread: a plain
/// `#[tauri::command]` runs on the main thread. Nothing in the UI calls this yet;
/// whatever does needs `(async)` first, like the rest of the Whisper path.
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
#[tauri::command(async)]
fn ensure_model() -> Result<String, String> {
    model::ensure_model().map(|p| p.display().to_string())
}

// ── Meeting session (M6) — the product-level record→transcribe flow ──────────

/// Start a meeting: create its folder and fire up both audio lanes. A lane that
/// can't start (no mic, denied screen-capture) is logged and skipped rather than
/// failing the whole meeting — but if *neither* starts, that's an error.
#[tauri::command(async)]
fn start_session(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    title: String,
    language: String,
) -> Result<session::SessionPaths, String> {
    let paths = begin_session(&app, state.inner(), &language, || session::new_session(&title))?;

    // Any notes typed before the meeting existed now have a home. Only a *new*
    // meeting gets them: flushing a stray draft into a continuation would write
    // it over the notes that meeting already has.
    if let Ok(mut pending) = state.pending_notes.lock() {
        if let Some((t, body)) = pending.take() {
            if let Err(e) = session::write_notes(Path::new(&paths.dir), &t, &body) {
                eprintln!("[oatmeal] flush pending notes: {e}");
            }
        }
    }

    Ok(paths)
}

/// Keep recording into a meeting that was already stopped — the user hit stop
/// by accident, or the conversation carried on after they thought it was over.
///
/// The extra audio goes into new segment WAVs in the same folder, and is
/// appended to the existing transcript when this take stops; the meeting keeps
/// its id, its notes and its place in the library.
#[tauri::command(async)]
fn continue_session(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    id: String,
    language: String,
) -> Result<session::SessionPaths, String> {
    let meeting = library::meeting(&id)?;
    let dir = PathBuf::from(&meeting.dir);
    begin_session(&app, state.inner(), &language, || {
        session::continue_session(&dir, &meeting.title)
    })
}

/// Bring a meeting's recording up: both audio lanes into the WAVs `paths`
/// names, the live transcription worker, and the session state.
///
/// Shared by `start_session` and `continue_session` — the only thing that
/// differs between them is which files they record into, so `paths` is a
/// closure run under the session lock rather than an argument. Taking the lock
/// first is what makes "a meeting is already being recorded" reliable: the
/// record path is `(async)`, so two of these genuinely can overlap.
fn begin_session(
    app: &tauri::AppHandle,
    state: &AppState,
    language: &str,
    paths: impl FnOnce() -> Result<session::SessionPaths, String>,
) -> Result<session::SessionPaths, String> {
    let mut sess = state.session.lock().map_err(|_| "session state poisoned")?;
    if sess.is_some() {
        return Err("a meeting is already being recorded".into());
    }
    let paths = paths()?;

    // Each recorder mirrors into its own lane of one tap, which sums them — the
    // same mix the final transcript is made from. A lane whose recorder never
    // starts is retired so the tap doesn't sit waiting on a buffer that will
    // never fill.
    let (tap, mut lanes) = Tap::with_lanes(2);
    let sys_lane = lanes.pop();
    let mic_lane = lanes.pop();

    let mut any = false;
    {
        let mut m = state.mic.lock().map_err(|_| "mic state poisoned")?;
        if m.is_none() {
            match MicRecorder::start_with_tap(PathBuf::from(&paths.mic_wav), mic_lane) {
                Ok(r) => {
                    *m = Some(r);
                    any = true;
                }
                Err(e) => {
                    eprintln!("[oatmeal] mic lane did not start: {e}");
                    tap.retire(0);
                }
            }
        } else {
            tap.retire(0);
        }
    }
    {
        let mut s = state.sysaudio.lock().map_err(|_| "sysaudio state poisoned")?;
        if s.is_none() {
            match SysAudioRecorder::start_with_tap(PathBuf::from(&paths.sys_wav), sys_lane) {
                Ok(r) => {
                    *s = Some(r);
                    any = true;
                }
                Err(e) => {
                    eprintln!("[oatmeal] system-audio lane did not start: {e}");
                    tap.retire(1);
                }
            }
        } else {
            tap.retire(1);
        }
    }

    if !any {
        return Err("neither the microphone nor system-audio lane could start — check permissions".into());
    }

    // Live transcription is a convenience, not the product: if the worker can't
    // start (missing model, say), the meeting still records and still gets its
    // accurate transcript at stop.
    {
        let mut slot = state.live.lock().map_err(|_| "live state poisoned")?;
        let handle = app.clone();
        // The offline pass can auto-detect: it sees the whole meeting. The live
        // lane sees a second or two at a time, which is not enough to tell one
        // language from another — a misdetected window decodes to nonsense, and
        // the nonsense then becomes the next window's prompt. So an unset picker
        // means English here rather than detection. Anyone who picks a language
        // still gets it.
        let lang = {
            let l = language.trim();
            Some(if l.is_empty() { "en".to_string() } else { l.to_string() })
        };
        match LiveSession::start(tap, String::new(), lang, move |line| {
            use tauri::Emitter;
            if let Err(e) = handle.emit("oatmeal://live-line", line) {
                eprintln!("[oatmeal] emit live line: {e}");
            }
        }) {
            Ok(live) => *slot = Some(live),
            Err(e) => eprintln!("[oatmeal] live transcription did not start: {e}"),
        }
    }

    // Notes typed from here on belong next to this meeting's audio.
    *state.last_dir.lock().map_err(|_| "notes state poisoned")? = Some(paths.dir.clone());

    *sess = Some(paths.clone());
    // The clock runs from here, not from whenever this meeting's folder was made.
    if let Ok(mut started) = state.take_started.lock() {
        *started = Some(std::time::Instant::now());
    }
    Ok(paths)
}

/// Lines the live worker has produced so far this meeting. The recap feature
/// reads this to answer questions before the meeting has even ended.
#[tauri::command]
fn live_lines(state: tauri::State<'_, AppState>) -> Vec<live::LiveLine> {
    state
        .live
        .lock()
        .ok()
        .and_then(|l| l.as_ref().map(|s| s.lines()))
        .unwrap_or_default()
}

/// Save the freeform notes the user types on the new-note page.
///
/// Distinct from `write_notes`, which asks the local model to write notes *for*
/// you: this is what you typed yourself, kept verbatim in `notes.md`.
///
/// Notes go to the active meeting's folder; once it stops they keep going to that
/// same folder, so writing up straight after a call is not lost. Before any
/// meeting has happened there is nowhere to put them, so they are held in memory
/// and flushed when `start_session` creates the first folder.
#[tauri::command]
fn save_notes(
    state: tauri::State<'_, AppState>,
    title: String,
    body: String,
) -> Result<Option<String>, String> {
    let dir = state
        .session
        .lock()
        .map_err(|_| "session state poisoned")?
        .as_ref()
        .map(|p| p.dir.clone())
        .or_else(|| state.last_dir.lock().ok().and_then(|d| d.clone()));

    match dir {
        Some(dir) => {
            let path = session::write_notes(Path::new(&dir), &title, &body)?;
            Ok(Some(path.display().to_string()))
        }
        None => {
            *state
                .pending_notes
                .lock()
                .map_err(|_| "notes state poisoned")? = Some((title, body));
            Ok(None)
        }
    }
}

/// Show or hide the floating live-transcript window — the panel you park over a
/// call. It renders the same `oatmeal://live-line` events the in-app dock does.
#[tauri::command]
fn set_transcript_window_visible(app: tauri::AppHandle, visible: bool) -> Result<(), String> {
    let win = app
        .get_webview_window(TRANSCRIPT_WINDOW)
        .ok_or("transcript window not found")?;
    if visible {
        win.show().map_err(|e| e.to_string())?;
        win.set_focus().map_err(|e| e.to_string())?;
    } else {
        win.hide().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// What one speaker pass found.
#[derive(serde::Serialize)]
struct SpeakerResult {
    /// Transcript lines that came away with a voice on them.
    labelled: usize,
    /// Distinct voices heard.
    speakers: usize,
}

/// Whether the speaker models are already downloaded.
#[tauri::command]
fn speaker_models_ready() -> bool {
    diarize::models_present()
}

/// Roughly what the speaker models weigh, for the UI's copy.
#[tauri::command]
fn speaker_models_mb() -> u32 {
    diarize::APPROX_MB
}

/// Fetch the speaker models. ~46 MB the first time, so off the UI thread.
#[tauri::command(async)]
fn ensure_speaker_models() -> Result<(), String> {
    diarize::ensure_models().map(|_| ())
}

/// Work out who said each line of a meeting's transcript and write the voices
/// into it.
///
/// A second pass over the whole recording — minutes for a long meeting — so it
/// is `(async)` and never part of stopping: the transcript is already readable
/// while this runs, and `transcript.md` is rewritten in place when it lands.
#[tauri::command(async)]
fn identify_speakers(id: String) -> Result<SpeakerResult, String> {
    let meeting = library::meeting(&id)?;
    let dir = PathBuf::from(&meeting.dir);

    let samples = session::meeting_samples(&dir);
    if samples.is_empty() {
        return Err("this meeting has no audio left to listen to".into());
    }
    let spans = diarize::diarize_samples(&samples)?;
    let labelled = diarize::label_transcript(&dir, &spans)?;

    let mut ids: Vec<i32> = spans.iter().map(|s| s.speaker).collect();
    ids.sort_unstable();
    ids.dedup();
    Ok(SpeakerResult {
        labelled,
        speakers: ids.len(),
    })
}

/// Pin the floating transcript window to whatever Space you switch to.
///
/// Always-on-top only keeps the panel above other windows on the Space it was
/// opened in, so swiping to the call left the live lines and the auto-answer
/// behind. Pinning is a plain `#[tauri::command]` on purpose: it talks to
/// AppKit, which is main-thread-only.
#[tauri::command]
fn set_transcript_pinned(app: tauri::AppHandle, pinned: bool) -> Result<(), String> {
    let win = app
        .get_webview_window(TRANSCRIPT_WINDOW)
        .ok_or("transcript window not found")?;
    window::set_pinned(&win, pinned)
}

/// Whether the floating transcript window is currently on screen.
#[tauri::command]
fn is_transcript_window_visible(app: tauri::AppHandle) -> bool {
    app.get_webview_window(TRANSCRIPT_WINDOW)
        .and_then(|w| w.is_visible().ok())
        .unwrap_or(false)
}

/// Milliseconds since the in-progress meeting started, or `None` when idle.
///
/// After a relaunch mid-meeting the UI has no idea how long it has been running,
/// so its timer used to restart from zero and under-report an hour-old meeting.
/// The session folder's creation time is the start time.
#[tauri::command]
fn session_elapsed_ms(state: tauri::State<'_, AppState>) -> Option<u64> {
    elapsed_ms(&state)
}

/// How long the current take has been running, or `None` if nothing is.
fn elapsed_ms(state: &AppState) -> Option<u64> {
    // Only meaningful while something is actually recording; the clock is hidden
    // otherwise, and a stale start time would seed it with a wrong number.
    state.session.lock().ok()?.as_ref()?;
    let started = (*state.take_started.lock().ok()?)?;
    Some(started.elapsed().as_millis() as u64)
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
#[tauri::command(async)]
fn stop_session(
    state: tauri::State<'_, AppState>,
    model_path: String,
    language: String,
) -> Result<session::MeetingResult, String> {
    let paths = {
        let mut sess = state.session.lock().map_err(|_| "session state poisoned")?;
        sess.take().ok_or("no meeting is being recorded")?
    };
    if let Ok(mut started) = state.take_started.lock() {
        *started = None;
    }

    // Stop the live worker first and collect whatever its background pass
    // already transcribed properly. Releasing it before the final pass also
    // keeps peak memory down — it holds the Whisper model both share.
    let progress = state
        .live
        .lock()
        .map_err(|_| "live state poisoned")?
        .take()
        .map(|live| live.finish());

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
    let dir = PathBuf::from(&paths.dir);
    let result = session::finish_from_blocks(
        &dir,
        &paths.title,
        paths.segment,
        &model_path,
        lang,
        progress,
    )?;
    library::record_transcribed_segment(&dir, paths.segment)?;
    if paths.segment > 1 {
        // This take was appended to a transcript the notes were written from,
        // so those notes no longer describe the whole meeting.
        library::mark_notes_stale(&dir)?;
    }
    Ok(result)
}

/// Transcribe audio that was recorded but never written up, and fold it into
/// the meeting's `transcript.md`.
///
/// This is how a meeting survives the app dying mid-recording: the lane WAVs are
/// on disk, but nothing ever ran Whisper over them. Deliberately user-triggered
/// — transcription is the heaviest thing the app does, and doing it unasked at
/// launch would spend somebody's battery on a meeting they may not want.
#[tauri::command(async)]
fn finish_meeting(
    state: tauri::State<'_, AppState>,
    id: String,
    model_path: String,
    language: String,
) -> Result<library::Meeting, String> {
    let meeting = library::meeting(&id)?;
    let dir = PathBuf::from(&meeting.dir);

    // The WAVs of a running recording are still being written to; handing them
    // to Whisper would transcribe a prefix of a meeting that hasn't ended.
    if is_recording_into(state.inner(), &dir) {
        return Err("that meeting is still recording — stop it first".into());
    }
    if meeting.pending_segments.is_empty() {
        return Err("this meeting has already been transcribed".into());
    }

    let lang = if language.trim().is_empty() {
        None
    } else {
        Some(language.trim())
    };

    for n in &meeting.pending_segments {
        session::finish(&dir, &meeting.title, *n, &model_path, lang)?;
        library::record_transcribed_segment(&dir, *n)?;
        library::mark_notes_stale(&dir)?;
    }

    library::meeting(&id)
}

/// Whether this run is currently recording into `dir`.
fn is_recording_into(state: &AppState, dir: &Path) -> bool {
    let sess = state.session.lock().unwrap_or_else(|e| e.into_inner());
    sess.as_ref()
        .map(|paths| Path::new(&paths.dir) == dir)
        .unwrap_or(false)
}

/// Every past meeting on disk, newest first — drives the home screen's recent
/// list and headline stats. Cheap enough to call on every home-screen render.
#[tauri::command]
fn list_meetings() -> Vec<library::Meeting> {
    library::list_meetings()
}

/// Meetings matching `query` against title, transcript, or notes — the
/// sidebar's full-text search. Same cost as `list_meetings`, just filtered.
#[tauri::command]
fn search_meetings(query: String) -> Vec<library::Meeting> {
    library::search_meetings(&query)
}

/// The same matches, each carrying a few excerpts of the text that matched, so
/// the dashboard can show *where* a meeting matched rather than only that it
/// did. This reads and scans every meeting's transcript and notes on each
/// (debounced) keystroke, so it runs off the main thread.
#[tauri::command(async)]
fn search_snippets(query: String) -> Vec<library::SearchHit> {
    library::search_hits(&query)
}

// ── Local language model — notes and recaps ─────────────────────────────────

/// Ensure the chat model is downloaded. Separate from `ensure_model` because
/// it's a much larger download and only needed for summaries, not recording.
#[tauri::command(async)]
fn ensure_chat_model() -> Result<String, String> {
    model::ensure_chat_model().map(|p| p.display().to_string())
}

/// Whether the chat model is on disk, and whether it's already resident in
/// memory — the UI uses this to warn before a multi-gigabyte download.
#[tauri::command(async)]
fn chat_model_status() -> serde_json::Value {
    let path = model::chat_model_path();
    serde_json::json!({
        "path": path.display().to_string(),
        "present": model::is_present(&path),
        "loaded": chat::is_loaded(),
        "approx_mb": model::CHAT_MODEL_APPROX_MB,
    })
}

/// A saved transcript's timestamped lines, for the note view's raw tab.
#[tauri::command]
fn meeting_segments(id: String) -> Result<Vec<library::TranscriptLine>, String> {
    library::transcript_lines(&id)
}

/// Write structured notes for a past meeting and cache them next to the
/// transcript, so reopening a note doesn't re-run the model.
#[tauri::command(async)]
fn write_notes(id: String, template: chat::Template, force: bool) -> Result<String, String> {
    library::write_notes(&id, template, force)
}

/// Answer a question about a meeting. `id` empty means the meeting currently
/// being recorded, answered from the live transcript so far.
#[tauri::command(async)]
fn ask_meeting(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    id: String,
    question: String,
) -> Result<String, String> {
    // The live transcript is the fast model's, and lossier; `recap` softens its
    // "that never came up" refusal accordingly.
    let live = id.trim().is_empty();
    let transcript = if live {
        let lines = state
            .live
            .lock()
            .ok()
            .and_then(|l| l.as_ref().map(|s| s.lines()))
            .unwrap_or_default();
        lines
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        library::transcript_text(&id)?
    };

    let path = model::ensure_chat_model()?;
    chat::recap(&path, &transcript, &question, live, &mut chat_token_sink(app))
}

/// Answer a question the live panel overheard in the meeting, streaming a short
/// reply drawn from the model's own knowledge. This is the auto-answer path: the
/// panel calls it on its own when it detects a spoken question, so the limits
/// that protect the recording live here, not in the UI.
///
/// - Only one answer runs at a time and a minimum interval sits between them
///   (`autoanswer::Gate`), because Whisper and this model share the GPU.
/// - Over-long "questions" are dropped as misrecognized run-ons.
/// - The reply stays on the machine (local model) and is never written into the
///   meeting; the panel shows it as unverified.
///
/// A refused claim returns an error the panel treats as "skip this one"; it is
/// not shown to the user.
#[tauri::command(async)]
fn answer_live_question(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    question: String,
) -> Result<String, String> {
    let question = question.trim();
    if question.is_empty() {
        return Err("no question to answer".into());
    }
    if question.chars().count() > autoanswer::MAX_QUESTION_CHARS {
        return Err("question is too long to auto-answer".into());
    }

    // Claim the gate up front and release it no matter how the answer ends. A
    // poisoned lock means some other answer panicked mid-flight; treat that as
    // "busy" rather than trying to recover the count.
    {
        let mut gate = state.auto_answer.lock().map_err(|_| "auto-answer busy")?;
        if !gate.try_begin(std::time::Instant::now()) {
            return Err("auto-answer is rate limited".into());
        }
    }
    let result = generate_live_answer(&app, &state, question);
    if let Ok(mut gate) = state.auto_answer.lock() {
        gate.finish();
    }
    result
}

/// The generation half of `answer_live_question`, split out so the gate is
/// always released even when this returns early.
fn generate_live_answer(
    app: &tauri::AppHandle,
    state: &tauri::State<'_, AppState>,
    question: &str,
) -> Result<String, String> {
    let recent = state
        .live
        .lock()
        .ok()
        .and_then(|l| l.as_ref().map(|s| s.lines()))
        .unwrap_or_default()
        .iter()
        .map(|l| l.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");

    let path = model::ensure_chat_model()?;
    chat::answer_live(&path, &recent, question, &mut live_answer_sink(app.clone()))
}

/// Sink that streams a live auto-answer's tokens to the panel on its own event.
/// `seq` starts at 1 so the panel can tell the first token — which clears the
/// previous answer — from the rest.
fn live_answer_sink(app: tauri::AppHandle) -> impl FnMut(&str) {
    use tauri::Emitter;
    let mut seq = 0u32;
    move |piece: &str| {
        seq += 1;
        let _ = app.emit(
            LIVE_ANSWER_EVENT,
            serde_json::json!({ "seq": seq, "text": piece }),
        );
    }
}

/// Load the chat model into memory ahead of time, so the first auto-answer isn't
/// paying the multi-second load while the user waits. The panel calls this when
/// auto-answer is switched on. Idempotent: the model is kept resident, so later
/// calls are cheap.
#[tauri::command(async)]
fn warm_chat_model() -> Result<(), String> {
    let path = model::ensure_chat_model()?;
    chat::warm(&path)
}

/// Answer a question from every meeting in the library, rather than from one
/// the user has already picked. Streams the same `CHAT_TOKEN_EVENT` tokens as
/// `ask_meeting`, and names the meetings it drew on so the UI can link to them.
#[tauri::command(async)]
fn ask_library(app: tauri::AppHandle, question: String) -> Result<recall::LibraryAnswer, String> {
    recall::answer(&question, &mut chat_token_sink(app))
}

/// Sink that forwards generated tokens to the window as they arrive: a local
/// model takes seconds to write a paragraph, and a silent wait reads as a hang.
/// `seq` lets the UI tell the first token from the rest.
fn chat_token_sink(app: tauri::AppHandle) -> impl FnMut(&str) {
    use tauri::Emitter;
    let mut seq = 0u32;
    move |piece: &str| {
        seq += 1;
        let _ = app.emit(
            CHAT_TOKEN_EVENT,
            serde_json::json!({ "seq": seq, "text": piece }),
        );
    }
}

/// Draft a follow-up message from a meeting's notes — text for the user to copy
/// and send themselves; Oatmeal never sends it anywhere.
#[tauri::command(async)]
fn draft_followup(app: tauri::AppHandle, id: String) -> Result<String, String> {
    let notes = library::followup_source(&id)?;
    let path = model::ensure_chat_model()?;
    let cfg = settings::load();
    let style = chat::FollowupStyle::from_settings(&cfg.followup_style, &cfg.followup_custom);
    chat::draft_followup(&path, &notes, &style, &mut chat_token_sink(app))
}

/// Free the chat model's memory.
#[tauri::command(async)]
fn unload_chat_model() {
    chat::unload();
}

/// Retitle a past meeting. Writes `meta.json` and rewrites the transcript
/// heading; the folder name (and so the recording's timestamp) is untouched.
#[tauri::command]
fn rename_meeting(id: String, title: String) -> Result<(), String> {
    library::rename_meeting(&id, &title)
}

/// Move a past meeting to the Trash, returning where it landed. Recoverable
/// from Finder — audio can't be re-recorded, so this is never a hard delete.
#[tauri::command]
fn delete_meeting(id: String) -> Result<String, String> {
    library::delete_meeting(&id)
}

/// Every folder under the recordings root, for the sidebar's Folders section.
#[tauri::command]
fn list_folders() -> Vec<library::Folder> {
    library::list_folders()
}

/// Create a new, empty folder to file notes into.
#[tauri::command]
fn create_folder(name: String) -> Result<(), String> {
    library::create_folder(&name)
}

/// Rename a folder in place.
#[tauri::command]
fn rename_folder(old: String, new: String) -> Result<(), String> {
    library::rename_folder(&old, &new)
}

/// Delete an empty folder. Refuses if it still has notes in it.
#[tauri::command]
fn delete_folder(name: String) -> Result<(), String> {
    library::delete_folder(&name)
}

/// File a meeting into a folder, or back to Unsorted when `folder` is `None`.
#[tauri::command]
fn move_meeting_to_folder(id: String, folder: Option<String>) -> Result<(), String> {
    library::move_meeting_to_folder(&id, folder.as_deref())
}

/// Bundle a meeting's notes and transcript into a Markdown file under
/// `~/Downloads/Oatmeal Exports/` and reveal it in Finder, so it can be shared
/// without touching the app's own recordings folder.
#[tauri::command]
fn export_meeting(id: String) -> Result<String, String> {
    let dest = library::export_meeting(&id)?;
    std::process::Command::new("open")
        .arg(&dest)
        .status()
        .map_err(|e| format!("reveal export in Finder: {e}"))?;
    Ok(dest)
}

// ── settings + calendar ──────────────────────────────────────────────────────

/// The editable settings: what the app should call you, and how transcription
/// runs.
#[tauri::command]
fn get_settings() -> settings::Settings {
    settings::load()
}

#[tauri::command]
fn save_settings(
    display_name: String,
    language: String,
    followup_style: String,
    followup_custom: String,
    chunk_seconds: u32,
) -> Result<settings::Settings, String> {
    settings::save(
        &display_name,
        &language,
        &followup_style,
        &followup_custom,
        chunk_seconds,
    )
}

/// Whether macOS has granted calendar access, without prompting.
#[tauri::command]
fn calendar_authorized() -> bool {
    apple_calendar::is_authorized()
}

/// Ask macOS for calendar access. Blocks on the permission prompt.
#[tauri::command]
fn calendar_request_access() -> Result<bool, String> {
    apple_calendar::request_access()
}

/// Upcoming events straight from Apple Calendar — no account, no keys, no
/// network. `authorized: false` means the permission hasn't been granted yet.
#[tauri::command]
fn list_events(days: u32) -> Result<apple_calendar::CalendarFeed, String> {
    apple_calendar::list_events(days)
}

/// Open System Settings on the Calendars privacy pane — where a denied
/// permission has to be undone, since macOS won't prompt twice.
#[tauri::command]
fn open_calendar_settings() -> Result<(), String> {
    std::process::Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Calendars")
        .status()
        .map(|_| ())
        .map_err(|e| format!("open System Settings: {e}"))
}

/// The notes the user typed during a meeting, for the note view. Empty when they
/// typed nothing — `notes.md` and the model's `summary.md` are separate files.
#[tauri::command]
fn meeting_typed_notes(id: String) -> Result<String, String> {
    library::typed_notes(&id)
}

/// Every homework item, soonest due date first, for the Homework view.
#[tauri::command]
fn list_homework() -> Vec<homework::HomeworkItem> {
    homework::list_homework()
}

/// Add a homework item with a due date.
#[tauri::command]
fn add_homework(title: String, note: String, due_date: String) -> Result<homework::HomeworkItem, String> {
    homework::add_homework(&title, &note, &due_date)
}

/// Toggle a homework item's done state.
#[tauri::command]
fn set_homework_done(id: String, done: bool) -> Result<(), String> {
    homework::set_homework_done(&id, done)
}

/// Delete a homework item.
#[tauri::command]
fn delete_homework(id: String) -> Result<(), String> {
    homework::delete_homework(&id)
}

// ── Updates ──────────────────────────────────────────────────────────────────

/// Ask GitHub whether a newer release exists. Network-bound, hence `(async)`.
/// Never fails: an unreachable check reports `checked: false` so the UI stays
/// quiet instead of locking someone out of a meeting.
#[tauri::command(async)]
fn check_for_update() -> update::UpdateStatus {
    update::check()
}

/// The version this build was compiled as. Separate from `check_for_update` so
/// the title bar can show it immediately instead of waiting on a network call
/// that may take ten seconds to time out.
#[tauri::command]
fn app_version() -> &'static str {
    update::current_version()
}

/// Open a release page or its DMG in the browser. Refuses links outside the
/// project's own repository. Spawning `open` and waiting on it blocks, so it
/// stays off the main thread like everything else here that blocks.
#[tauri::command(async)]
fn open_update_download(url: String) -> Result<(), String> {
    update::open_download(&url)
}

/// Download and install the update in place, then quit — the installer script
/// waits for this process to be gone, swaps the bundle and reopens Oatmeal.
///
/// Refuses while a meeting is being recorded: quitting mid-session would take
/// the recording with it, and no update is worth a lost meeting. The frontend
/// falls back to opening the DMG in a browser if this fails for any reason.
#[tauri::command(async)]
fn install_update(app: tauri::AppHandle, state: tauri::State<'_, AppState>, url: String) -> Result<(), String> {
    if state.session.lock().map(|s| s.is_some()).unwrap_or(false) {
        return Err("Stop the recording before updating.".into());
    }
    update::install(&url)?;
    app.exit(0);
    Ok(())
}

/// What a YouTube URL points at, so the UI can show the title and reject a
/// range typed past the end of it. Network and subprocess work — async.
#[tauri::command(async)]
fn video_probe(url: String) -> Result<video::VideoInfo, String> {
    video::probe(&url)
}

/// Attach a video's watched stretch to a meeting. Minutes of Whisper — async.
///
/// Refused while a meeting is recording: this competes for the same cores as
/// the live lane, and the live lane falling behind the speaker is the one thing
/// that cannot be recovered afterwards.
#[tauri::command(async)]
fn video_import(
    state: tauri::State<'_, AppState>,
    meeting_id: String,
    url: String,
    start: String,
    end: String,
) -> Result<String, String> {
    if is_session_active(state) {
        return Err("finish the recording first — importing a video would slow it down".into());
    }
    video::import(&meeting_id, &url, &start, &end)
}

// ── Data compatibility ───────────────────────────────────────────────────────

/// How this build's understanding of the on-disk format compares to what is
/// actually there. The UI reads this to explain itself when writes are refused
/// because the data came from a newer Oatmeal.
#[tauri::command]
fn data_status() -> serde_json::Value {
    serde_json::json!({
        "dataVersion": store::DATA_VERSION,
        "storedVersion": store::stored_version(),
        "writesLocked": store::writes_locked(),
        "lockReason": store::lock_reason(),
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AppState::default())
        // Closing the main window destroys it, but macOS keeps the process
        // alive — so the app sat in the Dock with nothing to show, and
        // relaunching only re-activated that windowless process. From the
        // outside it looked like Oatmeal refused to open a second time.
        // Hide the window instead and bring it back on reopen. Hiding also
        // means closing the window can't silently end a recording that is
        // still running; ⌘Q still quits for real.
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
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
            continue_session,
            stop_session,
            finish_meeting,
            is_session_active,
            session_elapsed_ms,
            list_meetings,
            search_meetings,
            search_snippets,
            live_lines,
            save_notes,
            set_transcript_window_visible,
            set_transcript_pinned,
            speaker_models_ready,
            speaker_models_mb,
            ensure_speaker_models,
            identify_speakers,
            is_transcript_window_visible,
            ensure_chat_model,
            chat_model_status,
            write_notes,
            meeting_segments,
            meeting_typed_notes,
            ask_meeting,
            answer_live_question,
            warm_chat_model,
            ask_library,
            draft_followup,
            unload_chat_model,
            rename_meeting,
            delete_meeting,
            export_meeting,
            get_settings,
            save_settings,
            calendar_authorized,
            calendar_request_access,
            open_calendar_settings,
            list_events,
            list_folders,
            create_folder,
            rename_folder,
            delete_folder,
            move_meeting_to_folder,
            list_homework,
            add_homework,
            set_homework_done,
            delete_homework,
            data_status,
            app_version,
            check_for_update,
            open_update_download,
            install_update,
            video_probe,
            video_import
        ])
        .setup(|app| {
            // Before anything reads or writes a meeting: reconcile this build
            // against what is already on disk. Refuses to write at all if the
            // data came from a newer Oatmeal, rather than rewriting it older.
            match store::prepare() {
                store::Compatibility::Migrated { from, backup } => {
                    eprintln!(
                        "[oatmeal] data migrated from v{from}{}",
                        backup
                            .map(|b| format!(", previous documents copied to {b}"))
                            .unwrap_or_default()
                    );
                }
                state => eprintln!("[oatmeal] data check: {state:?}"),
            }
            // Apply the hide flag as soon as the window exists.
            let hidden = app
                .state::<AppState>()
                .hidden_from_capture
                .load(Ordering::SeqCst);
            for label in ["main", TRANSCRIPT_WINDOW] {
                if let Some(win) = app.get_webview_window(label) {
                    if let Err(e) = window::set_hidden_from_capture(&win, hidden) {
                        eprintln!("[oatmeal] initial hide of {label} failed: {e}");
                    }
                }
            }
            // A machine that sleeps mid-meeting records nothing until it wakes.
            // The UI stops the take and says so; Rust only reports the gap.
            let handle = app.handle().clone();
            sleep::on_wake(move |asleep_ms| {
                use tauri::Emitter;
                let recording = handle
                    .state::<AppState>()
                    .session
                    .lock()
                    .map(|s| s.is_some())
                    .unwrap_or(false);
                if !recording {
                    return;
                }
                let _ = handle.emit(SLEPT_EVENT, serde_json::json!({ "asleep_ms": asleep_ms }));
            });

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while running Oatmeal")
        .run(|_app, _event| {
            // Clicking the Dock icon, or launching Oatmeal again while it is
            // already running, raises this rather than starting a new process.
            // Without it the hidden window above would never come back.
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = _event {
                if let Some(win) = _app.get_webview_window("main") {
                    let _ = win.show();
                    let _ = win.unminimize();
                    let _ = win.set_focus();
                }
            }
        });
}

#[cfg(test)]
mod tests {
    /// A plain `#[tauri::command]` runs its body inline on the main thread, so a
    /// command that downloads a model, opens an audio device or runs Whisper
    /// freezes the window until it returns — the spinning-wait-cursor on the
    /// record button. `(async)` hands the same sync body to the async runtime
    /// instead. Anything on the record → transcribe → notes path belongs here.
    #[test]
    fn heavy_commands_do_not_run_on_the_main_thread() {
        let src = include_str!("lib.rs");
        for name in [
            "ensure_model",
            "start_session",
            "continue_session",
            "stop_session",
            "finish_meeting",
            "ensure_chat_model",
            "write_notes",
            "ask_meeting",
            "answer_live_question",
            "warm_chat_model",
            "ask_library",
            "draft_followup",
            "check_for_update",
            "open_update_download",
            "install_update",
            "video_probe",
            "video_import",
            "search_snippets",
            "chat_model_status",
            "unload_chat_model",
            "ensure_speaker_models",
            "identify_speakers",
        ] {
            let decl = format!("fn {name}(");
            let at = src
                .find(&decl)
                .unwrap_or_else(|| panic!("{name} is no longer declared in lib.rs"));
            let attr = src[..at].rsplit_once('\n').expect("attribute above fn").0;
            assert!(
                attr.ends_with("#[tauri::command(async)]"),
                "{name} must be #[tauri::command(async)] — a blocking body on the \
                 main thread freezes the UI"
            );
        }
    }

    /// Tauri's `dragDropEnabled` defaults to true, which hands every drag that
    /// enters the webview to the native file-drop handler. That handler always
    /// reports the drag as handled, so wry never forwards it to the page and
    /// HTML5 `dragover`/`drop` never fire — which is why dragging a meeting
    /// onto a folder in the sidebar did nothing. Oatmeal accepts no dropped
    /// files, so the native handler has nothing to do here.
    #[test]
    fn main_window_leaves_html_drag_and_drop_to_the_page() {
        let conf = include_str!("../tauri.conf.json");
        let main = conf
            .split("\"label\": \"main\"")
            .nth(1)
            .expect("main window in tauri.conf.json");
        let main = &main[..main.find('}').expect("end of the main window block")];
        assert!(
            main.contains("\"dragDropEnabled\": false"),
            "the main window must set dragDropEnabled: false, or the sidebar's \
             drag-to-folder stops working"
        );
    }

    /// The clock on screen counts this take, not the meeting. Continuing a
    /// meeting records into the folder it already had, so anything derived from
    /// that folder — its creation date, which is what this used to read — starts
    /// the clock at whenever the meeting was *first* recorded. Continue
    /// yesterday's meeting and it opened at 24 hours.
    #[test]
    fn the_clock_counts_the_current_take_not_the_whole_meeting() {
        use std::time::{Duration, Instant};

        let state = crate::AppState::default();
        let dir = "/tmp/oatmeal-elapsed/20260812-110000-standup";

        // Nothing recording: no clock, whatever start time is lying around.
        *state.take_started.lock().unwrap() = Some(Instant::now());
        assert_eq!(crate::elapsed_ms(&state), None, "no session means no clock");

        // Continuing a meeting first recorded a day ago: the folder is old, the
        // take is seconds old, and the clock must show the take.
        *state.session.lock().unwrap() = Some(crate::session::SessionPaths {
            dir: dir.into(),
            mic_wav: format!("{dir}/mic.002.wav"),
            sys_wav: format!("{dir}/system.002.wav"),
            title: "Standup".into(),
            slug: "standup".into(),
            segment: 2,
        });
        *state.take_started.lock().unwrap() =
            Some(Instant::now() - Duration::from_secs(90));

        let elapsed = crate::elapsed_ms(&state).expect("a running take has a clock");
        assert!(
            (90_000..91_000).contains(&elapsed),
            "expected ~90s for this take, got {elapsed} ms"
        );
    }

    /// Finishing an abandoned recording runs Whisper over the lane WAVs as they
    /// sit on disk. Doing that to a meeting *this run is still recording into*
    /// would transcribe a prefix of a file that is still growing, and hand back
    /// a "final" transcript of half a meeting.
    #[test]
    fn a_meeting_being_recorded_right_now_is_not_offered_for_transcription() {
        use std::path::Path;

        let state = crate::AppState::default();
        let live = "/tmp/oatmeal-recording-guard/20260812-120000-live";
        let paths = crate::session::SessionPaths {
            dir: live.into(),
            mic_wav: format!("{live}/mic.wav"),
            sys_wav: format!("{live}/system.wav"),
            title: "Live".into(),
            slug: "live".into(),
            segment: 1,
        };

        assert!(
            !crate::is_recording_into(&state, Path::new(live)),
            "nothing is recording yet"
        );

        *state.session.lock().unwrap() = Some(paths);
        assert!(crate::is_recording_into(&state, Path::new(live)));
        assert!(
            !crate::is_recording_into(
                &state,
                Path::new("/tmp/oatmeal-recording-guard/20260812-110000-other")
            ),
            "a different meeting is still fair game"
        );
    }
}
