<div align="center">

# 🥣 Oatmeal

**A native Mac meeting notetaker that's invisible to screen sharing.**

[![License: MIT](https://img.shields.io/badge/License-MIT-b48455.svg)](LICENSE)
[![Local First](https://img.shields.io/badge/audio-never%20leaves%20your%20machine-2ea44f)](#privacy)
[![No Cloud](https://img.shields.io/badge/cloud-none-critical)](#how-it-works)
[![Platform](https://img.shields.io/badge/platform-macOS%20(Apple%20Silicon)-000000)](#install)
[![PRs Welcome](https://img.shields.io/badge/PRs-welcome-brightgreen.svg)](#contributing)

*Mic + system audio, transcribed on-device with Whisper, written up by a local
language model — all inside a small floating window that never appears in Zoom,
Meet, or any screen share. No bots joining calls. No accounts. No API keys.
No cloud.*

</div>

---

## How it works

```
you, in a meeting ──►  Oatmeal.app  (invisible to screen share)
                        mic + system audio
                             │
                             ▼
                whisper.cpp transcribes live, on-device
                             │
                             ▼
                llama.cpp writes notes, on-device
                             │
                             ▼
        ~/Library/Application Support/dev.oatmeal.app/recordings/
```

The recorder is a native window — `NSWindow.sharingType = NSWindowSharingNone` —
so it's excluded from every screen/window capture (Zoom, Discord, Loom,
QuickTime…) while staying fully visible on your own display. A title-bar pill
lets you flip it visible again when you actually want to share it. Nothing
about the meeting — audio, transcript, or notes — ever leaves the machine
except the one-time model downloads on first launch.

## Install

Grab the latest DMG from **[Releases](https://github.com/Cujoqt/oatmeal/releases)**:

1. Download `Oatmeal-<version>-apple-silicon.dmg` and drag **Oatmeal** to
   Applications.
2. First launch: right-click the app and choose **Open** — the build is
   ad-hoc signed, so Gatekeeper asks once.
3. macOS will ask for **Microphone** and **Screen Recording** permission.
   Screen Recording is what ScreenCaptureKit uses to capture the other side
   of the call — no video is ever recorded, only audio.
4. Hit **Record**. On first use Oatmeal downloads the Whisper model
   (~148 MB) and the local notes model, then everything works offline.

Requires Apple Silicon (M-series) macOS. There is no Intel or
Windows/Linux build of the native app today.

<details>
<summary>Building from source</summary>

Requires the Rust toolchain, Node, and `cmake` (for whisper.cpp):

```sh
brew install cmake
npm install                 # repo root — installs the Tauri CLI
npx tauri dev --config app/src-tauri/tauri.conf.json
```

See [`app/README.md`](app/README.md) for the full layout, the command
surface, and how to produce a signed local build (so macOS doesn't forget
the calendar permission on every rebuild).

</details>

## What it does

| Lane | How |
|---|---|
| **Invisible window** | `NSWindow.sharingType = NSWindowSharingNone`, flippable back to visible from the title bar. |
| **Microphone + system audio** | `cpal` for the mic, `ScreenCaptureKit` for a system-audio tap — no BlackHole or virtual driver. |
| **Live transcript** | `whisper-rs` (whisper.cpp, Metal GPU) streams lines into a floating window while you record. |
| **Notes** | A local instruct model (`llama.cpp`, Metal GPU) writes a summary once the meeting ends; you can also type your own notes as you go. |
| **Calendar** | Reads Apple Calendar (EventKit — iCloud, Google, Exchange, whatever your Mac already has) so upcoming meetings show on the home screen. |

Meetings are saved to
`~/Library/Application Support/dev.oatmeal.app/recordings/<date>-<title>/` as
plain `transcript.md` and `notes.md` files next to the audio — nothing is
locked in a database.

## Updating

Your meetings are **not** inside the app. They live under
`~/Library/Application Support/dev.oatmeal.app/`, so replacing Oatmeal in
`/Applications` never touches a recording, a transcript, or a note. Every write
to those files is atomic — a crash, or quitting the app to install a new
version, cannot leave a half-written note behind.

Oatmeal checks this repository's releases when it starts. A newer version shows
a strip you can dismiss; occasionally a release is marked required, and then the
app asks you to update before continuing. If it can't reach GitHub it says
nothing and carries on — you can always record offline.

One wrinkle worth knowing: builds are ad-hoc signed, so macOS treats each
release as a new app identity and will ask for **Microphone** and **Screen
Recording** again after an update.

## Privacy

- Transcription and note-writing both run **on-device** — Whisper and the
  local language model, via whisper.cpp/llama.cpp on the Metal GPU. Audio is
  never stored or uploaded anywhere.
- The only network calls Oatmeal makes are the one-time model downloads on
  first launch, an update check that asks GitHub for this repository's release
  list when the app starts, and — if you're using the Calendar features —
  EventKit itself running in-process. The update check sends nothing but the
  request.
- Attaching a YouTube video to a note reaches the network twice more, and both
  are worth knowing about. **Pasting the link already contacts YouTube** —
  Oatmeal looks the video up to read its title and length before you press
  Transcribe, so the request goes out as soon as you paste, not when you
  confirm. And the first time you do this, Oatmeal downloads a **third-party
  helper program, `yt-dlp`, from github.com and runs it on your machine**;
  nothing else in Oatmeal does that. The version is pinned in the source, so
  the app never fetches whatever build happens to be newest. These requests
  tell Google which video you asked for and the IP address you're asking from,
  but send nothing about your meetings, notes, or transcripts, and none of it
  happens unless you paste a link.
- Recording people has consent rules that vary by place. Tell attendees
  you're taking notes.

## Alternative: browser recorder + your coding agent

This repo also ships a second, older way to run Oatmeal — a zero-dependency
local web server instead of a native app, meant to be driven by a coding
agent (Claude Code, Cursor, Codex…) rather than the app's own local model.
Transcripts land as plain Markdown in a `meetings/` folder that your agent
reads, writes notes into, and can commit to git as a shared team knowledge
base. Cross-platform, but the window is **not** hidden from screen sharing.

```bash
git clone https://github.com/Cujoqt/oatmeal.git
cd oatmeal
npm install
npm start          # recorder at http://localhost:4123
```

Then paste one line into your coding agent:

> **Read SKILL.md and set up Oatmeal for me.**

The agent starts the recorder, opens http://localhost:4123, and tells you how
to record. (Claude Code auto-discovers the skill via `.claude/skills/` —
mentioning meetings is enough.) When a meeting starts: hit **Record**, share
**Entire screen** with **"share system audio" checked** (window shares carry
no audio). When it ends, tell your agent:

> **write up my meeting**

You get summary, key points, decisions, and action items — committed to git
if the repo has a remote.

<details>
<summary>Claude Code superpowers for this path</summary>

| You type | What happens |
|---|---|
| `/meeting` | Recorder starts (if needed) and opens in your browser |
| `/writeup` | Latest transcript becomes polished notes: summary, decisions, action items — committed & pushed |
| `/recall what did we tell Acme about pricing?` | Grounded answer from every meeting in the knowledge base, with file citations |
| `/prep akrit` | Pre-meeting brief: what you discussed last time, open action items, promises made, suggested agenda |

- **It notices unwritten meetings.** A `SessionStart` hook checks for
  transcripts with no notes — open Claude Code after a call and it offers the
  write-up before you ask.
- **MCP auto-registers.** [`.mcp.json`](.mcp.json) exposes `list_meetings` /
  `search_meetings` / `get_meeting` to any MCP client the moment you open the
  repo.

Other agents: the same flows work by asking in plain English — the commands
are just markdown files in [`.claude/commands/`](.claude/commands/), readable
by anything.

</details>

<details>
<summary>Running as a background service (no agent, no terminal open)</summary>

```bash
node scripts/install-autostart.mjs
```

Registers the recorder as a real background service — a Startup-folder entry
on Windows, a LaunchAgent on macOS, `systemd --user` on Linux — with no admin
rights and no permission prompts. Starts at every login, keeps running.
Uninstall with `node scripts/install-autostart.mjs --uninstall`.

If your coding agent has (or can add) a Google/Outlook Calendar connector,
just ask it to wire your calendar into Oatmeal. No connector? Copy
`oatmeal.config.example.json` to `oatmeal.config.json`, paste your calendar's
ICS feed URL, and re-run the install script — it also installs a calendar
watcher that opens the recorder ~7 minutes before each meeting.

</details>

## What's in the repo

| Path | What it is |
|---|---|
| [`app/`](app/README.md) | The native macOS app (Tauri + Rust) — invisible recorder window, on-device Whisper + local LLM |
| [`SKILL.md`](SKILL.md) | The product spec your coding agent follows for the browser-recorder path — setup, notes flow, knowledge base rules, calendar automation |
| [`capture/`](capture/) | Zero-dependency local server + recorder page for the browser-recorder path |
| [`meetings/`](meetings/) | Transcripts + notes written by the browser-recorder path. Plain Markdown. |
| [`scripts/mcp-server.mjs`](scripts/mcp-server.mjs) | Optional MCP server — expose `meetings/` to any MCP client |
| [`scripts/install-autostart.mjs`](scripts/install-autostart.mjs) | Registers the browser recorder (+ calendar watcher) as a background OS service |
| [`scripts/calendar-watch.mjs`](scripts/calendar-watch.mjs) | Standalone ICS calendar poller for the browser-recorder path |
| [`.claude/skills/`](.claude/skills/) | Auto-discovery so Claude Code picks up the skill on clone |

## Contributing

PRs welcome.

**Good first issues:**
- Smarter meeting-title detection (from calendar or first few words)
- Calendar connector recipes for the browser-recorder path (Google, Outlook, Slack integration guides)
- **Full multi-person diarization** (mic vs system audio are already tagged "You"/"Room" — telling apart two+ people on the *other* end of the call would need Pyannote or similar)
- Better error messages (when Whisper fails, when git push fails, etc.)

**Not in scope:** cloud sync, accounts, compliance features (fine-grained audit logs), multi-language models beyond Whisper's baseline. Those belong in derivatives, not core.

## License

MIT © Vedant Soni — free forever.
