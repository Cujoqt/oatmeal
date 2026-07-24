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
| **Notes** | Writes `transcript.md` with timestamped lines next to the audio. |

## Layout

```
app/
├── src/                    frontend (vanilla, editorial UI)
│   ├── index.html
│   └── app.js              drives the Tauri commands
└── src-tauri/
    ├── src/
    │   ├── lib.rs          command surface + AppState
    │   ├── window.rs       M1 — screen-share hide (NSWindow sharingType)
    │   ├── mic.rs          M2 — cpal microphone lane
    │   ├── sysaudio.rs     M3 — ScreenCaptureKit system-audio lane
    │   ├── transcribe.rs   M4 — whisper-rs + resampling
    │   ├── model.rs        first-run ggml model download (curl)
    │   └── session.rs      M6 — record → mix → transcribe → transcript.md
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
- `start_session(title)` / `stop_session(modelPath, language)` / `is_session_active()`
- `ensure_model()` / `default_model_path()`
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

## Permissions

macOS will prompt for **Microphone** and **Screen Recording** on first use
(the latter is how ScreenCaptureKit gates system-audio capture). Both are
declared in `Info.plist`; nothing is captured until you grant them.
