// Constants shared by the note window and the floating transcript window.
//
// The two windows are separate webviews but the same origin, so localStorage is
// shared and Tauri events broadcast to both. Everything that has to line up
// between them lives here.

/// Event emitted by Rust (`live.rs`) for each finished live transcript line:
/// `{ at_ms, text }`.
export const BACKEND = {
  line: 'oatmeal://live-line',
  /// Each piece of a streamed chat answer: `{ seq, text }`.
  chatToken: 'oatmeal://chat-token',
  /// Each piece of a streamed live auto-answer: `{ seq, text }`. Separate from
  /// `chatToken` so the panel's answer never interleaves with a typed one.
  liveAnswer: 'oatmeal://live-answer',
  /// Each piece of a streamed live math conversion: `{ seq, text }`. Separate
  /// from `liveAnswer` for the same reason that one is separate from
  /// `chatToken` — the live-panel streams must never interleave.
  liveMath: 'oatmeal://live-math',
  /// The machine slept mid-recording: `{ asleep_ms }`. Nothing was captured for
  /// that stretch, so the take is stopped rather than left with a hole in it.
  slept: 'oatmeal://slept',
  /// Quitting was refused because notes are still being written — killing the
  /// process now would lose the summary outright, since it only saves once
  /// the whole thing is generated.
  quitBlockedNotes: 'oatmeal://quit-blocked-notes',
}

/// Events the two windows use to talk to each other.
export const EVENTS = {
  ...BACKEND,
  /// Transcript window asks the note window to start/stop the session. The note
  /// window owns the flow so there is exactly one source of truth.
  toggleRecord: 'oatmeal://ui-toggle-record',
  /// Note window broadcasts session state `{ active, title }`.
  session: 'oatmeal://ui-session',
  /// Note window broadcasts worker/session status for the panel to show.
  state: 'oatmeal://ui-state',
  /// Transcript window asks to be dismissed.
  hideTranscript: 'oatmeal://ui-hide-transcript',
}

export const LANG_KEY = 'oatmeal.lang'

/// Seconds of transcript read as a single paragraph. Mirrored into localStorage
/// because both windows render transcripts long before Settings is opened, and
/// the config lives a Rust call away.
export const CHUNK_KEY = 'oatmeal.chunkSeconds'
export const DEFAULT_CHUNK_SECS = 30

export function chunkSeconds() {
  const v = Number(localStorage.getItem(CHUNK_KEY))
  return v >= 5 && v <= 300 ? v : DEFAULT_CHUNK_SECS
}

/// Transcription language code, or '' for auto-detect.
export function getLang() {
  const v = localStorage.getItem(LANG_KEY)
  return v === null ? 'en' : v
}

export function setLang(code) {
  localStorage.setItem(LANG_KEY, code)
}

/// Format milliseconds from the start of the meeting as `M:SS`, or `H:MM:SS`
/// once the meeting has run past an hour.
export function fmtMs(ms) {
  const total = Math.max(0, Math.floor(ms / 1000))
  const h = Math.floor(total / 3600)
  const m = Math.floor((total % 3600) / 60)
  const pad = (n) => String(n).padStart(2, '0')
  return h > 0 ? `${h}:${pad(m)}:${pad(total % 60)}` : `${m}:${pad(total % 60)}`
}

export function escapeHtml(s) {
  return String(s).replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]))
}
