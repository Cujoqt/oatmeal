# Working on Oatmeal

Oatmeal records meetings locally: mic + system audio, on-device Whisper, notes from
a local language model, and a window that stays invisible to screen sharing.
Nothing is uploaded. The product makes exactly two kinds of network call: the
one-time model download, and an update check that asks GitHub for the release
list on launch (`update.rs`). Neither sends anything about a meeting.

## How to work here

These four rules outrank habit, tidiness, and any impulse to be helpful beyond
what was asked.

### 1. Think before coding

State assumptions out loud before writing code. Surface confusion instead of
smoothing over it, and lay out tradeoffs where a real choice exists. Ask a
clarifying question rather than guessing silently at intent or architecture — a
wrong guess costs more than a question. If you must proceed without an answer, say
which assumption you picked and why.

### 2. Simplicity first

Solve the problem in front of you and nothing else. No speculative features, no
abstractions with one caller, no "flexibility" nobody asked for. The bar is the
simplest thing a senior engineer would sign off on — if a second implementation
would be needed before an abstraction pays for itself, don't build the abstraction
yet.

### 3. Surgical changes

Touch only what the request requires. Do not refactor, reformat, rename, or
"improve" adjacent code, comments, or unrelated files — not even when they are
messy, inconsistent, or plainly broken. Notice the mess, mention it, leave it
alone. A diff that is larger than the request is a defect, because every extra
line is another thing the reviewer has to trust.

### 4. Goal-driven execution

Turn the task into a goal with a check that can fail. Not "fix the bug" but "write
a test that reproduces it, then make it pass" — then loop until the check passes.
For UI, the check is a rendered screenshot or a queried DOM state, not a reading of
the source. Report what was verified and how; if something could not be verified,
say so plainly instead of implying it works.

## Project specifics

- **All CSS is inline in the HTML.** The CSP forbids remote stylesheets and fonts,
  and the app must work offline. Never add a `<link>` to a font or style host.
- **Both dark-mode blocks change together:** `:root[data-theme="dark"]` and the
  `prefers-color-scheme` query duplicate their values because a media query can't
  share a rule with a plain selector.
- **One router.** `showView()` in `app.js` owns which surface is visible; nav
  highlighting is derived from it, never set by hand.
- **The recorder's window is hidden from screen capture by default**
  (`NSWindowSharingNone`), so screenshots of it come out blank until the title-bar
  pill is switched to "visible to shares". This is the product's headline feature,
  not a bug.
- **Calendar is Apple Calendar via EventKit**, in-process so the permission is
  attributed to Oatmeal itself. Reading `~/Library/Calendars` as files is not
  possible (TCC). A denied permission is terminal — macOS will not prompt twice, so
  the UI must offer System Settings instead of asking again.
- **Anything that blocks must not block the UI thread.** Whisper and the chat model
  are the two heavy paths; leave the UI a couple of cores and stream results where
  the user is waiting on them. Concretely: a plain `#[tauri::command]` runs its body
  *on the main thread*, so anything doing network, audio-device, Whisper or LLM work
  must be `#[tauri::command(async)]`. `lib.rs`'s test
  `heavy_commands_do_not_run_on_the_main_thread` fails if one regresses — add new
  heavy commands to its list.
- **Never `fs::write` user data — use `store::write`.** It writes a sibling temp
  file, fsyncs, and renames, so a crash or the app being quit mid-write can't leave
  a truncated `notes.md`. The one deliberate exception is the export to
  `~/Downloads`, which creates a new file outside the support root and must keep
  working even when writes are locked.
- **The support root carries a `data-version.json` stamp** (`store::DATA_VERSION`).
  Older data is copied into `backups/` and stamped forward; data from a *newer*
  build locks every write and puts the update gate up, rather than letting an old
  build rewrite notes in an older shape. Bump `DATA_VERSION` only alongside the
  migration that makes the old shape readable.
- **Updates come from GitHub Releases — there is no update server.** `update.rs`
  asks the releases API for the newest tag and compares it to the built-in version.
  A release is only *mandatory* if its notes contain a line like
  `Oatmeal-Minimum-Version: 1.3.0`; anyone below that gets a blocking gate. Leave
  the line out and a new release is just a dismissible suggestion. A check that
  fails (offline, rate-limited, mangled marker) must always fail *open* — never
  block someone who is about to record a meeting. The check runs in Rust on
  purpose: the CSP lets the webview reach nothing but the IPC bridge, and widening
  it to hit github.com would be a real loosening for a request Rust can make.
- **Sign local builds with the Apple Development cert**, otherwise macOS forgets
  the calendar permission on every rebuild: an ad-hoc signature is a new code
  identity each time, and TCC keys its grants to that identity. This bites
  *shipped* builds too: `tauri.conf.json` keeps `signingIdentity: "-"`, so every
  release is a new identity and users re-grant Microphone and Screen Recording
  after each update. Now that the app prompts people to update, that is a real
  cost — fixing it properly means a Developer ID signature plus notarization in
  CI, not another workaround.

  ```sh
  cd app && APPLE_SIGNING_IDENTITY="Apple Development: dylanmo8025@icloud.com (W45G6BS8JA)" cargo tauri build
  ```

  `tauri.conf.json` keeps `signingIdentity: "-"` so CI, which has no certificate,
  still builds. The env var overrides it locally.
- **Tests:** `cd app/src-tauri && cargo test --lib`. The real-audio end-to-end test
  is `#[ignore]`d: `cargo test --test e2e_transcribe -- --ignored --nocapture`.
- **To check UI without a full build:** copy `app/src` somewhere, inject a stub
  `window.__TAURI__ = { core: { invoke }, event: { listen, emit } }` ahead of the
  module script, serve it, and drive it with a headless browser.
