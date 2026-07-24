# 🥣 Oatmeal — native recorder (`app/`)

A native macOS build of the Oatmeal recorder, as a Tauri app. Where the
browser recorder (repo root) leans on `getDisplayMedia` and a "share system
audio" checkbox, this app captures both audio lanes natively and — the headline
feature — **stays invisible to screen sharing**.

Everything runs on-device. No network calls except the one-time Whisper model
download.

## What it does

| Lane | How |
|------|-----|
| **Invisible window** | `NSWindow.sharingType = NSWindowSharingNone` — excluded from every ScreenCaptureKit/CGDisplayStream capture (Zoom, Discord, Loom, QuickTime…) while still visible on your own display. |
| **Microphone** | `cpal` default input device → 16-bit WAV. |
| **System audio** | `ScreenCaptureKit` audio tap → mono WAV. No BlackHole / virtual driver. Excludes Oatmeal's own output. |
| **Transcription** | `whisper-rs` (whisper.cpp, Metal GPU) — mixes both lanes, resamples to 16 kHz mono, runs `ggml-base.en` locally. |
| **Live transcript** | A warm Whisper context transcribes 5–14 s chunks *while* you record, cut at the quietest point so words survive the boundary. Lines stream into a floating, always-on-top window. |
| **Notes** | You type into the note window; it autosaves to `notes.md`. On stop, `transcript.md` is written with timestamped lines next to the audio. |

## Layout

```
app/
├── src/                    frontend (vanilla, two windows)
│   ├── base.css            shared design tokens
│   ├── shared.js           event names + formatting shared by both windows
│   ├── index.html/app.js   note window: title, notes, recording dock
│   └── transcript.html/.js floating live-transcript window
└── src-tauri/
    ├── src/
    │   ├── lib.rs          command surface + AppState
    │   ├── window.rs       M1 — screen-share hide (NSWindow sharingType)
    │   ├── mic.rs          M2 — cpal microphone lane
    │   ├── sysaudio.rs     M3 — ScreenCaptureKit system-audio lane
    │   ├── transcribe.rs   M4 — whisper-rs + resampling
    │   ├── model.rs        first-run ggml model download (curl)
    │   ├── session.rs      M6 — record → mix → transcribe → transcript.md
    │   └── live.rs         M7 — streaming chunk transcription + Tauri events
    ├── Info.plist          mic + screen-capture usage strings
    └── entitlements.plist  hardened-runtime audio entitlements
```

## Build & run

Requires the Rust toolchain, Node, and `cmake` (for whisper.cpp):

```sh
brew install cmake          # once
npm install                 # repo root — installs the Tauri CLI
npx tauri dev --config app/src-tauri/tauri.conf.json
```

On first record the app downloads `ggml-base.en.bin` (~148 MB) into
`~/Library/Application Support/dev.oatmeal.app/models/`. Set `OATMEAL_MODEL` to
point at a different ggml model.

## Command surface (Tauri `invoke`)

- `set_hidden_from_capture(hidden)` / `is_hidden_from_capture()`
- `start_session(title, language)` / `stop_session(modelPath, language)` / `is_session_active()`
- `save_notes(title, body)` — writes `notes.md`; buffered in memory until a session exists
- `set_transcript_window_visible(visible)` / `is_transcript_window_visible()`
- `ensure_model()` / `default_model_path()`

Events emitted by the backend while recording:

- `oatmeal://live-segment` — one finished transcript line `{ start_cs, end_cs, text }`
- `oatmeal://live-state` — worker status `{ state: loading|listening|error, message }`
- lane-level: `start_mic_recording` / `start_sysaudio_recording` / `transcribe_wav` (dev/testing)

## Tests

```sh
cd app/src-tauri
cargo test --lib                                        # DSP + util unit tests
cargo test --test e2e_transcribe -- --ignored --nocapture   # real whisper run
```

The e2e test downloads `ggml-base.en` and a short speech clip, then transcribes
it on-device and asserts the text — no mic needed. It's `#[ignore]` so the
default `cargo test` stays fast and offline.

## Bundle

```sh
cargo install tauri-cli --version "^2.0" --locked       # once
cargo tauri build --config app/src-tauri/tauri.conf.json
```

Produces `Oatmeal.app` / `Oatmeal.dmg` under
`app/src-tauri/target/release/bundle/`. The bundle is ad-hoc signed
(`signingIdentity: "-"`); a real Developer ID + notarization are needed for
distribution outside your own machine.

## Two windows

The note window (`main`) owns the session: it starts and stops recording, saves
notes, and broadcasts state. The transcript window (`transcript`) is a
transparent, undecorated, always-on-top panel that only renders — its stop button
asks `main` to act, so there is exactly one owner of the session state. Both
windows carry the same `sharingType` flag, so hiding one hides both.

## Permissions

macOS will prompt for **Microphone** and **Screen Recording** on first use
(the latter is how ScreenCaptureKit gates system-audio capture). Both are
declared in `Info.plist`; nothing is captured until you grant them.
