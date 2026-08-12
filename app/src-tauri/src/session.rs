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

    Ok(SessionPaths {
        mic_wav: dir.join("mic.wav").display().to_string(),
        sys_wav: dir.join("system.wav").display().to_string(),
        dir: dir.display().to_string(),
        title: title.to_string(),
        slug,
    })
}

/// Mix the two lane WAVs (either may be missing/empty), transcribe the result,
/// and write `transcript.md` into `dir`. `model_path`/`language` may be empty.
pub fn finish(
    dir: &Path,
    title: &str,
    mic_wav: &Path,
    sys_wav: &Path,
    model_path: &str,
    language: Option<&str>,
) -> Result<MeetingResult, String> {
    let mic = load_optional(mic_wav);
    let sys = load_optional(sys_wav);

    let mixed = mix(&mic, &sys);
    if mixed.is_empty() {
        return Err("both audio lanes were empty — nothing to transcribe".into());
    }

    let transcript = transcribe::transcribe_samples(model_path, &mixed, language)?;
    let path = write_transcript_md(dir, title, &transcript)?;

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
fn write_transcript_md(dir: &Path, title: &str, t: &Transcript) -> Result<PathBuf, String> {
    let mut md = String::new();
    md.push_str(&format!("# {}\n\n", if title.is_empty() { "Untitled meeting" } else { title }));
    md.push_str(&format!("_Recorded {}_\n\n", human_now()));
    md.push_str("## Transcript\n\n");
    for seg in &t.segments {
        let text = seg.text.trim();
        if text.is_empty() {
            continue;
        }
        md.push_str(&format!("**[{}]** {}\n\n", fmt_cs(seg.start_cs), text));
    }
    if t.segments.is_empty() {
        md.push_str("_(no speech detected)_\n");
    }

    let path = dir.join("transcript.md");
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
}
