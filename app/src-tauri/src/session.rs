// M6 — session orchestration.
//
// Ties the lanes together into one meeting recording: start the mic (M2) and
// system-audio (M3) lanes into a per-meeting folder, then on stop mix the two
// mono tracks, transcribe the mix (M4), and write a transcript markdown file next
// to the audio. The heavy lifting (capture, whisper) lives in the other modules;
// this file is the glue that decides file layout and how the two lanes combine.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::transcribe::{self, Transcript};

/// Everything about one recording on disk. Serialized to the UI so it can show
/// the folder / drive the stop+transcribe step.
#[derive(Debug, Clone, Serialize)]
pub struct SessionPaths {
    pub dir: String,
    pub mic_wav: String,
    pub sys_wav: String,
    pub title: String,
    pub slug: String,
    /// Which take within the meeting this is: 1 for the original recording, 2
    /// and up for continuations. Decides which lane WAVs are written and where
    /// the transcript's clock picks up from.
    pub segment: u32,
}

/// Result of finishing a meeting: the transcript plus where it was written.
#[derive(Debug, Clone, Serialize)]
pub struct MeetingResult {
    pub transcript_path: String,
    pub dir: String,
    pub text: String,
    pub segments: Vec<transcribe::Segment>,
}

/// Base directory holding all recordings.
pub fn recordings_root() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    Path::new(&home).join("Library/Application Support/dev.oatmeal.app/recordings")
}

/// Compute the on-disk layout for a new meeting titled `title`. Creates the
/// directory. The folder name is `<YYYYMMDD-HHMMSS>-<slug>` for natural sorting.
pub fn new_session(title: &str) -> Result<SessionPaths, String> {
    let slug = slugify(title);
    let stamp = timestamp();
    let folder = if slug.is_empty() {
        stamp.clone()
    } else {
        format!("{stamp}-{slug}")
    };
    let dir = recordings_root().join(&folder);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("create session dir {}: {e}", dir.display()))?;

    let (mic_wav, sys_wav) = segment_paths(&dir, 1);
    Ok(SessionPaths {
        mic_wav: mic_wav.display().to_string(),
        sys_wav: sys_wav.display().to_string(),
        dir: dir.display().to_string(),
        title: title.to_string(),
        slug,
        segment: 1,
    })
}

/// Layout for another take into a meeting that already exists — the user
/// stopped, deliberately or by accident, and wants to keep going.
///
/// The continuation gets its own pair of lane WAVs rather than being appended
/// into the originals: appending to a WAV means rewriting its RIFF header, and
/// a crash part-way through that corrupts the audio that had already recorded
/// fine. Two files can't lose the first one.
pub fn continue_session(dir: &Path, title: &str) -> Result<SessionPaths, String> {
    if !dir.is_dir() {
        return Err(format!("no such meeting folder: {}", dir.display()));
    }
    let segment = next_segment(dir);
    let (mic_wav, sys_wav) = segment_paths(dir, segment);
    Ok(SessionPaths {
        mic_wav: mic_wav.display().to_string(),
        sys_wav: sys_wav.display().to_string(),
        dir: dir.display().to_string(),
        title: title.to_string(),
        slug: slugify(title),
        segment,
    })
}

/// The two lane WAVs for segment `n` of a meeting. Segment 1 keeps the original
/// `mic.wav` / `system.wav` names, so every meeting recorded before
/// continuations existed is still laid out exactly as it was.
pub fn segment_paths(dir: &Path, n: u32) -> (PathBuf, PathBuf) {
    if n <= 1 {
        (dir.join("mic.wav"), dir.join("system.wav"))
    } else {
        (
            dir.join(format!("mic.{n:03}.wav")),
            dir.join(format!("system.{n:03}.wav")),
        )
    }
}

/// Which segment a lane file belongs to, or `None` if it isn't a lane file.
fn segment_index(name: &str) -> Option<u32> {
    if name == "mic.wav" || name == "system.wav" {
        return Some(1);
    }
    let rest = name
        .strip_prefix("mic.")
        .or_else(|| name.strip_prefix("system."))?;
    rest.strip_suffix(".wav")?.parse().ok()
}

/// The segment a continuation should record into: one past the highest whose
/// files already exist, so a WAV that already holds audio is never reopened for
/// writing.
pub fn next_segment(dir: &Path) -> u32 {
    let mut highest = 0;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if let Some(n) = segment_index(&entry.file_name().to_string_lossy()) {
                highest = highest.max(n);
            }
        }
    }
    highest + 1
}

/// Every segment of this meeting that actually captured audio, ascending.
///
/// A lane that never opened leaves no file at all; one that opened and captured
/// nothing leaves a bare zero-frame WAV. Neither is worth handing to Whisper,
/// and neither should make an abandoned folder look like a recording worth
/// finishing.
pub fn recorded_segments(dir: &Path) -> Vec<u32> {
    let mut found = std::collections::BTreeSet::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    for entry in entries.flatten() {
        let Some(n) = segment_index(&entry.file_name().to_string_lossy()) else {
            continue;
        };
        if lane_has_audio(&entry.path()) {
            found.insert(n);
        }
    }
    found.into_iter().collect()
}

/// Whether a lane WAV holds any frames at all.
fn lane_has_audio(path: &Path) -> bool {
    hound::WavReader::open(path)
        .map(|r| r.duration() > 0)
        .unwrap_or(false)
}

/// Length of one lane in centiseconds, or 0 when it's missing or unreadable.
fn lane_len_cs(path: &Path) -> i64 {
    let Ok(reader) = hound::WavReader::open(path) else {
        return 0;
    };
    let rate = reader.spec().sample_rate;
    if rate == 0 {
        return 0;
    }
    i64::from(reader.duration()) * 100 / i64::from(rate)
}

/// Length of one segment — the longer of its two lanes — in centiseconds.
pub fn segment_len_cs(dir: &Path, n: u32) -> i64 {
    let (mic, sys) = segment_paths(dir, n);
    lane_len_cs(&mic).max(lane_len_cs(&sys))
}

/// The whole meeting's recorded length in seconds, summed across every segment.
/// A continuation makes the meeting longer, so the duration the library reports
/// has to count all the takes, not just the first.
pub fn total_len_secs(dir: &Path) -> u64 {
    let cs: i64 = recorded_segments(dir)
        .into_iter()
        .map(|n| segment_len_cs(dir, n))
        .sum();
    cs.max(0) as u64 / 100
}

/// Where this segment's transcript clock starts: everything recorded before it.
fn segment_offset_cs(dir: &Path, segment: u32) -> i64 {
    (1..segment).map(|n| segment_len_cs(dir, n)).sum()
}

/// Mix segment `segment`'s two lane WAVs (either may be missing/empty),
/// transcribe the result, and fold it into `transcript.md` in `dir`.
/// `model_path`/`language` may be empty.
///
/// The first segment writes the file; a continuation appends to it, with its
/// timestamps shifted past whatever was already recorded. Re-transcribing the
/// whole meeting to add five minutes to the end would be minutes of Whisper for
/// no new information.
pub fn finish(
    dir: &Path,
    title: &str,
    segment: u32,
    model_path: &str,
    language: Option<&str>,
) -> Result<MeetingResult, String> {
    let (mic_wav, sys_wav) = segment_paths(dir, segment);
    let mic = load_optional(&mic_wav);
    let sys = load_optional(&sys_wav);

    let mixed = mix(&mic, &sys);
    if mixed.is_empty() {
        return Err("both audio lanes were empty — nothing to transcribe".into());
    }

    let transcript = transcribe::transcribe_samples(model_path, &mixed, language)?;
    let path = write_transcript_md(dir, title, &transcript, segment_offset_cs(dir, segment))?;

    Ok(MeetingResult {
        transcript_path: path.display().to_string(),
        dir: dir.display().to_string(),
        text: transcript.text,
        segments: transcript.segments,
    })
}

/// Load a lane WAV as 16 kHz mono, or an empty vec if it's missing/unreadable.
/// A lane failing (denied permission, no device) must not sink the whole meeting.
fn load_optional(path: &Path) -> Vec<f32> {
    if !path.exists() {
        return Vec::new();
    }
    match transcribe::load_wav_mono_16k(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[oatmeal] skipping lane {}: {e}", path.display());
            Vec::new()
        }
    }
}

/// Sum two mono tracks sample-for-sample, padding the shorter with silence, and
/// clamp to avoid clipping. Both are already 16 kHz mono so indices line up.
fn mix(a: &[f32], b: &[f32]) -> Vec<f32> {
    let n = a.len().max(b.len());
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let s = a.get(i).copied().unwrap_or(0.0) + b.get(i).copied().unwrap_or(0.0);
        out.push(s.clamp(-1.0, 1.0));
    }
    out
}

/// Write the notes the user typed during the meeting to `notes.md`, next to the
/// audio and the transcript. Called repeatedly while they type (the UI debounces),
/// so it is a plain overwrite — cheap, and the last write wins.
pub fn write_notes(dir: &Path, title: &str, body: &str) -> Result<PathBuf, String> {
    let heading = if title.trim().is_empty() {
        "Untitled note"
    } else {
        title.trim()
    };
    let md = format!("# {heading}\n\n_Written {}_\n\n{}\n", human_now(), body.trim_end());
    let path = dir.join("notes.md");
    crate::store::write(&path, &md)?;
    Ok(path)
}

/// Write a human-readable transcript markdown with timestamped lines.
///
/// When `transcript.md` already has content this appends to it instead of
/// replacing it — that is how a continuation lands in the same document as the
/// take before it. `offset_cs` shifts this segment's timestamps so the clock
/// keeps running across takes rather than restarting at 0:00.
///
/// The whole file is rewritten through `store::write` rather than opened in
/// append mode: an interrupted append leaves a half-written line in the one
/// document the recording can't be re-made from.
fn write_transcript_md(
    dir: &Path,
    title: &str,
    t: &Transcript,
    offset_cs: i64,
) -> Result<PathBuf, String> {
    let path = dir.join("transcript.md");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let appending = !existing.trim().is_empty();

    let mut md = String::new();
    if appending {
        md.push_str(existing.trim_end());
        md.push_str("\n\n");
    } else {
        md.push_str(&format!("# {}\n\n", if title.is_empty() { "Untitled meeting" } else { title }));
        md.push_str(&format!("_Recorded {}_\n\n", human_now()));
        md.push_str("## Transcript\n\n");
    }

    let mut wrote_any = false;
    for seg in &t.segments {
        let text = seg.text.trim();
        if text.is_empty() {
            continue;
        }
        md.push_str(&format!("**[{}]** {}\n\n", fmt_cs(seg.start_cs + offset_cs), text));
        wrote_any = true;
    }
    if !wrote_any && !appending {
        md.push_str("_(no speech detected)_\n");
    }

    crate::store::write(&path, &md)?;
    Ok(path)
}

/// Format centiseconds as `M:SS`.
fn fmt_cs(cs: i64) -> String {
    let total_secs = cs / 100;
    let m = total_secs / 60;
    let s = total_secs % 60;
    format!("{m}:{s:02}")
}

/// A filesystem-safe lowercase slug of the title.
fn slugify(title: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for c in title.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out.chars().take(48).collect()
}

/// Compact sortable timestamp `YYYYMMDD-HHMMSS` in local time, derived from the
/// system clock without pulling in `chrono`.
fn timestamp() -> String {
    let (y, mo, d, h, mi, s) = local_ymdhms();
    format!("{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}")
}

/// Human-friendly `YYYY-MM-DD HH:MM` for the transcript header.
fn human_now() -> String {
    let (y, mo, d, h, mi, _) = local_ymdhms();
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}")
}

/// Break the current time into calendar fields. Uses `date` for the local-time
/// conversion (timezone-correct) and falls back to a rough UTC calc if that
/// fails — avoids a chrono/time dependency for a purely cosmetic value.
fn local_ymdhms() -> (i32, u32, u32, u32, u32, u32) {
    if let Ok(out) = std::process::Command::new("date")
        .arg("+%Y %m %d %H %M %S")
        .output()
    {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout);
            let nums: Vec<i64> = s
                .split_whitespace()
                .filter_map(|p| p.parse().ok())
                .collect();
            if nums.len() == 6 {
                return (
                    nums[0] as i32,
                    nums[1] as u32,
                    nums[2] as u32,
                    nums[3] as u32,
                    nums[4] as u32,
                    nums[5] as u32,
                );
            }
        }
    }
    // Fallback: epoch-based UTC (date is always present on macOS, so this is
    // effectively unreachable, but keeps the function total).
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86_400;
    let tod = secs % 86_400;
    (1970 + (days / 365) as i32, 1, 1, (tod / 3600) as u32, (tod % 3600 / 60) as u32, (tod % 60) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mix_pads_shorter_lane_and_sums() {
        let a = vec![0.5, 0.5, 0.5];
        let b = vec![0.2];
        let m = mix(&a, &b);
        assert_eq!(m.len(), 3);
        assert!((m[0] - 0.7).abs() < 1e-6);
        assert!((m[1] - 0.5).abs() < 1e-6); // b padded with silence
    }

    #[test]
    fn mix_clamps_to_unit_range() {
        let m = mix(&[0.9, -0.9], &[0.9, -0.9]);
        assert_eq!(m, vec![1.0, -1.0]);
    }

    #[test]
    fn slugify_is_filesystem_safe() {
        assert_eq!(slugify("Acme Discovery Call!"), "acme-discovery-call");
        assert_eq!(slugify("  weird__name  "), "weird-name");
        assert_eq!(slugify(""), "");
        assert!(slugify("a".repeat(100).as_str()).len() <= 48);
    }

    #[test]
    fn write_notes_creates_titled_markdown() {
        let dir = std::env::temp_dir().join(format!("oatmeal-notes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = write_notes(&dir, "Acme call", "- ship the thing\n").unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.starts_with("# Acme call"), "{body}");
        assert!(body.contains("- ship the thing"), "{body}");

        // Untitled notes still get a heading, and a second write overwrites.
        write_notes(&dir, "   ", "second").unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.starts_with("# Untitled note"), "{body}");
        assert!(!body.contains("ship the thing"), "{body}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn fmt_cs_renders_minutes_seconds() {
        assert_eq!(fmt_cs(0), "0:00");
        assert_eq!(fmt_cs(500), "0:05"); // 5.00s
        assert_eq!(fmt_cs(6_500), "1:05"); // 65.00s
    }

    // ── continuations ────────────────────────────────────────────────────────

    /// `store::write` consults a process-global writes-locked flag that the
    /// store suite flips, so anything writing through it takes the same lock the
    /// rest of the tests use.
    use crate::settings::HOME_ENV_LOCK;

    fn with_scratch<T>(f: impl FnOnce(&Path) -> T) -> T {
        let _guard = HOME_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!(
            "oatmeal-session-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let out = f(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        out
    }

    /// A silent 16 kHz lane WAV of `frames` samples — the layout code only ever
    /// reads the header, so the samples themselves don't have to be anything.
    fn lane_wav(path: &Path, frames: u32) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for _ in 0..frames {
            w.write_sample(0i16).unwrap();
        }
        w.finalize().unwrap();
    }

    /// A transcript of `(start_cs, text)` lines, as whisper would hand one back.
    fn transcript(lines: &[(i64, &str)]) -> Transcript {
        Transcript {
            segments: lines
                .iter()
                .map(|(cs, text)| transcribe::Segment {
                    start_cs: *cs,
                    end_cs: *cs + 100,
                    text: (*text).to_string(),
                })
                .collect(),
            text: lines.iter().map(|(_, t)| *t).collect::<Vec<_>>().join(" "),
        }
    }

    #[test]
    fn a_continuation_records_into_new_files_beside_the_originals() {
        with_scratch(|dir| {
            lane_wav(&dir.join("mic.wav"), 16_000);
            lane_wav(&dir.join("system.wav"), 16_000);

            let paths = continue_session(dir, "Acme call").unwrap();
            assert_eq!(paths.segment, 2);
            assert!(paths.mic_wav.ends_with("/mic.002.wav"), "{}", paths.mic_wav);
            assert!(paths.sys_wav.ends_with("/system.002.wav"), "{}", paths.sys_wav);
            // The first take's audio is left exactly where it was: not reopened,
            // not rewritten, not appended into.
            assert!(dir.join("mic.wav").is_file());
            assert!(!dir.join("mic.002.wav").exists(), "nothing is written yet");

            // A third take goes past the second rather than over it.
            lane_wav(&dir.join("mic.002.wav"), 16_000);
            assert_eq!(continue_session(dir, "Acme call").unwrap().segment, 3);
        })
    }

    #[test]
    fn a_lane_that_captured_nothing_is_not_a_recording_worth_finishing() {
        with_scratch(|dir| {
            // The lane opened but never got a sample — an empty WAV, not audio.
            lane_wav(&dir.join("mic.wav"), 0);
            assert!(recorded_segments(dir).is_empty());

            lane_wav(&dir.join("mic.wav"), 8_000);
            assert_eq!(recorded_segments(dir), vec![1]);

            // Gaps are fine: only the system lane came up for the third take.
            lane_wav(&dir.join("system.003.wav"), 1_600);
            assert_eq!(recorded_segments(dir), vec![1, 3]);
        })
    }

    #[test]
    fn a_continuation_appends_to_the_transcript_with_the_clock_carried_over() {
        with_scratch(|dir| {
            // Take one: 90 seconds, already written up.
            lane_wav(&dir.join("mic.wav"), 16_000 * 90);
            write_transcript_md(dir, "Acme call", &transcript(&[(0, "Morning.")]), 0).unwrap();

            // Take two picks up where take one stopped.
            lane_wav(&dir.join("mic.002.wav"), 16_000 * 10);
            let offset = segment_offset_cs(dir, 2);
            assert_eq!(offset, 9_000, "take two starts 90s into the meeting");
            write_transcript_md(dir, "Acme call", &transcript(&[(500, "One more thing.")]), offset)
                .unwrap();

            let md = std::fs::read_to_string(dir.join("transcript.md")).unwrap();
            assert_eq!(md.matches("# Acme call").count(), 1, "one document, not two: {md}");
            assert!(md.contains("**[0:00]** Morning."), "{md}");
            assert!(md.contains("**[1:35]** One more thing."), "{md}");
            assert!(md.find("Morning.") < md.find("One more thing."), "out of order: {md}");

            // The meeting is as long as everything recorded into it.
            assert_eq!(total_len_secs(dir), 100);
        })
    }

    #[test]
    fn a_silent_continuation_does_not_stamp_over_what_was_already_transcribed() {
        with_scratch(|dir| {
            write_transcript_md(dir, "Acme call", &transcript(&[(0, "Morning.")]), 0).unwrap();
            write_transcript_md(dir, "Acme call", &transcript(&[]), 500).unwrap();

            let md = std::fs::read_to_string(dir.join("transcript.md")).unwrap();
            assert!(md.contains("Morning."), "{md}");
            assert!(!md.contains("no speech detected"), "{md}");
        })
    }
}
