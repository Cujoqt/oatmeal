// Meeting library — reads past recordings back off disk.
//
// The home screen shows a "recent meetings" list and a couple of headline stats.
// There is no database: `session.rs` already lays every meeting out as
// `<recordings_root>/<YYYYMMDD-HHMMSS>-<slug>/{mic,system}.wav + transcript.md`,
// so the folder tree *is* the index. This module walks it and reconstructs the
// facts the UI needs — nothing here is stored twice.

use std::path::Path;

use serde::Serialize;

use crate::session::recordings_root;

/// One past meeting, as reconstructed from its folder.
#[derive(Debug, Clone, Serialize)]
pub struct Meeting {
    /// Folder name — stable id for the meeting.
    pub id: String,
    /// Title, from the transcript's `# heading` when present, else the slug.
    pub title: String,
    /// Local naive ISO (`YYYY-MM-DDTHH:MM:SS`) so JS `new Date(...)` reads it
    /// as local time, matching the clock the recording was named with.
    pub started_at: String,
    /// Longest lane's duration in seconds. 0 when no readable audio survives.
    pub duration_secs: u64,
    /// Whether `transcript.md` was written (i.e. the meeting finished and was
    /// transcribed, rather than being abandoned mid-recording).
    pub transcribed: bool,
    pub dir: String,
}

/// All meetings on disk, newest first. Folder names sort chronologically, so
/// this is a reverse name sort. Unparseable folders are skipped rather than
/// failing the whole listing — a stray directory must not break the home screen.
pub fn list_meetings() -> Vec<Meeting> {
    let root = recordings_root();
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };

    let mut dirs: Vec<_> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| parse_stamp(name).is_some())
        .collect();
    dirs.sort_unstable();
    dirs.reverse();

    dirs.iter()
        .map(|name| read_meeting(&root.join(name), name))
        .collect()
}

fn read_meeting(dir: &Path, name: &str) -> Meeting {
    let transcript = dir.join("transcript.md");
    let transcribed = transcript.exists();

    let title = transcript_title(&transcript)
        .or_else(|| title_from_slug(name))
        .unwrap_or_else(|| "Untitled meeting".into());

    let duration_secs = wav_secs(&dir.join("mic.wav")).max(wav_secs(&dir.join("system.wav")));

    Meeting {
        id: name.to_string(),
        title,
        started_at: parse_stamp(name).unwrap_or_default(),
        duration_secs,
        transcribed,
        dir: dir.display().to_string(),
    }
}

/// `YYYYMMDD-HHMMSS[-slug]` → `YYYY-MM-DDTHH:MM:SS`, or None if the prefix
/// isn't a timestamp.
fn parse_stamp(name: &str) -> Option<String> {
    let (date, rest) = name.split_once('-')?;
    let time: String = rest.chars().take(6).collect();
    if date.len() != 8 || time.len() != 6 {
        return None;
    }
    if !date.bytes().chain(time.bytes()).all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(format!(
        "{}-{}-{}T{}:{}:{}",
        &date[0..4],
        &date[4..6],
        &date[6..8],
        &time[0..2],
        &time[2..4],
        &time[4..6],
    ))
}

/// Recover a display title from the folder slug (`acme-discovery` → `Acme
/// discovery`). None when the meeting was recorded without a title.
fn title_from_slug(name: &str) -> Option<String> {
    // Strip the `YYYYMMDD-HHMMSS-` prefix; what's left is the slug.
    let slug = name.splitn(3, '-').nth(2)?;
    if slug.is_empty() {
        return None;
    }
    let words = slug.replace('-', " ");
    let mut chars = words.chars();
    let first = chars.next()?;
    Some(first.to_uppercase().collect::<String>() + chars.as_str())
}

/// First `# heading` of the transcript, which `session::write_transcript_md`
/// writes as the meeting title.
fn transcript_title(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let heading = text.lines().next()?.strip_prefix("# ")?.trim();
    (!heading.is_empty() && heading != "Untitled meeting").then(|| heading.to_string())
}

/// Duration of a WAV in whole seconds, or 0 if it's missing or unreadable.
fn wav_secs(path: &Path) -> u64 {
    let Ok(reader) = hound::WavReader::open(path) else {
        return 0;
    };
    let spec = reader.spec();
    if spec.sample_rate == 0 || spec.channels == 0 {
        return 0;
    }
    u64::from(reader.duration()) / u64::from(spec.sample_rate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_timestamped_folder_names() {
        assert_eq!(
            parse_stamp("20260724-103300-standup").as_deref(),
            Some("2026-07-24T10:33:00")
        );
        // No slug is still a valid meeting folder.
        assert_eq!(
            parse_stamp("20260724-103300").as_deref(),
            Some("2026-07-24T10:33:00")
        );
    }

    #[test]
    fn rejects_non_meeting_folders() {
        assert_eq!(parse_stamp("models"), None);
        assert_eq!(parse_stamp("not-a-stamp-here"), None);
        assert_eq!(parse_stamp("2026072-103300"), None);
    }

    #[test]
    fn recovers_title_from_slug() {
        assert_eq!(
            title_from_slug("20260724-103300-acme-discovery").as_deref(),
            Some("Acme discovery")
        );
        assert_eq!(title_from_slug("20260724-103300"), None);
    }
}
