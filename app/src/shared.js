// Constants shared by the note window and the floating transcript window.
//
// The two windows are separate webviews but the same origin, so localStorage is
// shared and Tauri events broadcast to both. Everything that has to line up
// between them lives here.

/// Event emitted by Rust (`live.rs`) for each finished live transcript line:
/// `{ at_ms, text }`.
export const BACKEND = {
  line: 'oatmeal://live-line',
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

/// Transcription language code, or '' for auto-detect.
export function getLang() {
  const v = localStorage.getItem(LANG_KEY)
  return v === null ? 'en' : v
}

export function setLang(code) {
  localStorage.setItem(LANG_KEY, code)
}

/// Format milliseconds from the start of the meeting as `M:SS`.
export function fmtMs(ms) {
  const total = Math.floor(ms / 1000)
  return `${Math.floor(total / 60)}:${String(total % 60).padStart(2, '0')}`
}

export function escapeHtml(s) {
  return String(s).replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]))
}
