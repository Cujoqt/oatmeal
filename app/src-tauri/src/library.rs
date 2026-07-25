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
    /// Whether the language model has already written notes for this meeting.
    pub has_notes: bool,
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

/// What the person typed during the meeting. Written continuously by
/// `session::write_notes`.
const TYPED_NOTES: &str = "notes.md";

/// The model's write-up of the transcript. Separate from `TYPED_NOTES` because
/// both used to land in the same file: whatever you typed made the app believe
/// notes had been generated, the Enhanced tab showed your own typing back to you,
/// and generating for real would have overwritten it.
const SUMMARY: &str = "summary.md";

/// Whether `notes.md` holds typed notes rather than an older build's model output.
/// `session::write_notes` stamps every file it writes with a `_Written …_` line.
fn is_typed_notes(text: &str) -> bool {
    text.lines().take(4).any(|line| line.starts_with("_Written "))
}

/// Older builds cached the model's write-up in `notes.md`. Move one aside the
/// first time we see it, so a meeting summarized by an earlier version keeps its
/// notes instead of silently regenerating them.
fn migrate_legacy_summary(dir: &Path) -> Option<String> {
    let legacy = dir.join(TYPED_NOTES);
    let text = std::fs::read_to_string(&legacy).ok()?;
    if text.trim().is_empty() || is_typed_notes(&text) {
        return None;
    }
    let summary = dir.join(SUMMARY);
    std::fs::rename(&legacy, &summary).ok()?;
    Some(text)
}

/// Whether a meeting already has a model-written summary.
fn has_summary(dir: &Path) -> bool {
    if dir.join(SUMMARY).is_file() {
        return true;
    }
    // A legacy notes.md that isn't typed notes still counts — it is a summary,
    // it just hasn't been renamed yet.
    std::fs::read_to_string(dir.join(TYPED_NOTES))
        .map(|text| !text.trim().is_empty() && !is_typed_notes(&text))
        .unwrap_or(false)
}

fn read_meeting(dir: &Path, name: &str) -> Meeting {
    let transcript = dir.join("transcript.md");
    let transcribed = transcript.exists();

    // A rename writes meta.json, so it outranks the recorded-at-the-time
    // heading and the folder slug.
    let title = meta_title(&dir.join("meta.json"))
        .or_else(|| transcript_title(&transcript))
        .or_else(|| title_from_slug(name))
        .unwrap_or_else(|| "Untitled meeting".into());

    let duration_secs = wav_secs(&dir.join("mic.wav")).max(wav_secs(&dir.join("system.wav")));

    Meeting {
        id: name.to_string(),
        title,
        started_at: parse_stamp(name).unwrap_or_default(),
        duration_secs,
        transcribed,
        has_notes: has_summary(dir),
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

/// Title from a rename, if the meeting has been renamed.
fn meta_title(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let meta: serde_json::Value = serde_json::from_str(&text).ok()?;
    let title = meta.get("title")?.as_str()?.trim();
    (!title.is_empty()).then(|| title.to_string())
}

/// Resolve a meeting id (a folder name) to its directory, refusing anything
/// that isn't a plain name directly under the recordings root. The id crosses
/// the boundary from the UI, so `..` or an absolute path must not be able to
/// point the rename/delete commands at arbitrary files.
fn meeting_dir(id: &str) -> Result<std::path::PathBuf, String> {
    if id.is_empty()
        || id.contains('/')
        || id.contains('\\')
        || id.contains("..")
        || Path::new(id).components().count() != 1
    {
        return Err(format!("invalid meeting id: {id}"));
    }
    let dir = recordings_root().join(id);
    if !dir.is_dir() {
        return Err(format!("no such meeting: {id}"));
    }
    Ok(dir)
}

/// Rename a meeting. The new title is recorded in `meta.json`; the transcript's
/// own heading is rewritten too so the exported markdown doesn't disagree with
/// the app. The folder name is deliberately left alone — it encodes the
/// recording time and is what every other path is derived from.
pub fn rename_meeting(id: &str, title: &str) -> Result<(), String> {
    let dir = meeting_dir(id)?;
    let title = title.trim();
    if title.is_empty() {
        return Err("a meeting needs a title".into());
    }

    let meta = serde_json::json!({ "title": title });
    let meta_path = dir.join("meta.json");
    std::fs::write(&meta_path, meta.to_string())
        .map_err(|e| format!("write {}: {e}", meta_path.display()))?;

    let transcript = dir.join("transcript.md");
    if let Ok(text) = std::fs::read_to_string(&transcript) {
        let rest = text.split_once('\n').map(|(_, r)| r).unwrap_or("");
        let updated = format!("# {title}\n{rest}");
        std::fs::write(&transcript, updated)
            .map_err(|e| format!("write {}: {e}", transcript.display()))?;
    }
    Ok(())
}

/// Move a meeting to the Trash. Deliberately not a hard delete: a recording
/// can't be re-made, so a misclick has to stay recoverable from Finder.
pub fn delete_meeting(id: &str) -> Result<String, String> {
    let dir = meeting_dir(id)?;
    let home = std::env::var("HOME").map_err(|_| "no HOME set".to_string())?;
    let trash = Path::new(&home).join(".Trash");
    std::fs::create_dir_all(&trash).map_err(|e| format!("open Trash: {e}"))?;

    // Finder's own collision behaviour: keep the name, add a counter.
    let mut dest = trash.join(id);
    let mut n = 2;
    while dest.exists() {
        dest = trash.join(format!("{id} {n}"));
        n += 1;
    }

    std::fs::rename(&dir, &dest).map_err(|e| {
        format!("move {} to Trash: {e}", dir.display())
    })?;
    Ok(dest.display().to_string())
}

/// The transcript's spoken text, with the markdown scaffolding and timestamps
/// stripped — what the language model should actually read.
pub fn transcript_text(id: &str) -> Result<String, String> {
    let dir = meeting_dir(id)?;
    let path = dir.join("transcript.md");
    let md = std::fs::read_to_string(&path)
        .map_err(|_| "this meeting has no transcript yet".to_string())?;
    Ok(strip_transcript_markup(&md))
}

/// Pull the spoken words out of a `transcript.md`, dropping the title, the
/// recorded-at line, the `## Transcript` heading and the `**[m:ss]**` stamps.
fn strip_transcript_markup(md: &str) -> String {
    let mut out = String::new();
    for line in md.lines() {
        let line = line.trim();
        if line.is_empty()
            || line.starts_with('#')
            || (line.starts_with('_') && line.ends_with('_'))
        {
            continue;
        }
        // `**[1:23]** words words` → `words words`
        let text = match line.strip_prefix("**[") {
            Some(rest) => rest.split_once("]**").map(|(_, t)| t).unwrap_or(rest),
            None => line,
        };
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(text);
    }
    out
}

/// One timestamped line of a saved transcript, for the note view's raw tab.
#[derive(Debug, Clone, Serialize)]
pub struct TranscriptLine {
    /// Display timestamp as written in the markdown, e.g. `1:23`.
    pub at: String,
    pub text: String,
}

/// Parse a saved `transcript.md` back into its timestamped lines.
pub fn transcript_lines(id: &str) -> Result<Vec<TranscriptLine>, String> {
    let dir = meeting_dir(id)?;
    let md = std::fs::read_to_string(dir.join("transcript.md"))
        .map_err(|_| "this meeting has no transcript yet".to_string())?;

    let mut out = Vec::new();
    for line in md.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("**[") else { continue };
        let Some((at, text)) = rest.split_once("]**") else { continue };
        let text = text.trim();
        if !text.is_empty() {
            out.push(TranscriptLine {
                at: at.to_string(),
                text: text.to_string(),
            });
        }
    }
    Ok(out)
}

/// Write structured notes for a meeting, caching them as `summary.md` beside the
/// transcript. Returns the cached copy unless `force` asks for a rewrite —
/// generation takes real time and the result doesn't change on its own.
///
/// This never touches `notes.md`: that file belongs to whoever was typing during
/// the meeting.
pub fn write_notes(id: &str, force: bool) -> Result<String, String> {
    let dir = meeting_dir(id)?;
    let summary_path = dir.join(SUMMARY);
    if !force {
        if let Ok(existing) = std::fs::read_to_string(&summary_path) {
            if !existing.trim().is_empty() {
                return Ok(existing);
            }
        }
        if let Some(migrated) = migrate_legacy_summary(&dir) {
            return Ok(migrated);
        }
    }

    let transcript = transcript_text(id)?;
    let model = crate::model::ensure_chat_model()?;
    let notes = crate::chat::write_notes(&model, &transcript)?;

    std::fs::write(&summary_path, &notes)
        .map_err(|e| format!("write {}: {e}", summary_path.display()))?;
    Ok(notes)
}

/// What the person typed during the meeting, if anything. The heading and the
/// `_Written …_` stamp are dropped: the note view has its own title and date.
pub fn typed_notes(id: &str) -> Result<String, String> {
    let dir = meeting_dir(id)?;
    let text = match std::fs::read_to_string(dir.join(TYPED_NOTES)) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(e) => return Err(format!("read {}: {e}", dir.join(TYPED_NOTES).display())),
    };
    if !is_typed_notes(&text) {
        return Ok(String::new());
    }
    let body: String = text
        .lines()
        .skip_while(|line| {
            line.starts_with("# ") || line.starts_with("_Written ") || line.trim().is_empty()
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(body.trim().to_string())
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

    /// Point `$HOME` at a scratch dir for the duration of the closure, so the
    /// tests exercise the real path logic (recordings root *and* Trash) without
    /// touching the developer's own recordings.
    /// `$HOME` is process-global, so these tests can't overlap.
    static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_temp_home<T>(f: impl FnOnce(&Path) -> T) -> T {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = std::env::temp_dir().join(format!(
            "oatmeal-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&home).unwrap();
        let prev = std::env::var("HOME").ok();
        std::env::set_var("HOME", &home);

        let out = f(&home);

        match prev {
            Some(p) => std::env::set_var("HOME", p),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(&home);
        out
    }

    /// Create a meeting folder with a transcript, as `session.rs` would.
    fn seed_meeting(id: &str) -> std::path::PathBuf {
        let dir = recordings_root().join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("transcript.md"), "# Standup\n\n## Transcript\n\nhi\n").unwrap();
        dir
    }


    #[test]
    fn typed_notes_are_not_mistaken_for_a_summary() {
        let typed = "# Standup\n\n_Written 2026-07-24 22:37_\n\nmy own bullet points\n";
        let model = "## Summary\n\nThe team agreed to ship on Friday.\n";
        assert!(is_typed_notes(typed));
        assert!(!is_typed_notes(model));
    }

    #[test]
    fn a_typed_note_does_not_count_as_notes_written() {
        with_temp_home(|_| {
            let dir = seed_meeting("20260724-100000-standup");
            std::fs::write(
                dir.join("notes.md"),
                "# Standup\n\n_Written 2026-07-24 22:37_\n\nmine\n",
            )
            .unwrap();

            // Typing during a meeting used to light up the "notes written" dot and
            // make the Enhanced tab show your own typing back to you.
            assert!(!has_summary(&dir));
            assert!(migrate_legacy_summary(&dir).is_none());
            assert!(dir.join("notes.md").is_file(), "typed notes must survive");
        });
    }

    #[test]
    fn a_legacy_summary_moves_to_its_own_file() {
        with_temp_home(|_| {
            let dir = seed_meeting("20260724-110000-kickoff");
            std::fs::write(
                dir.join("notes.md"),
                "## Summary\n\nwritten by an older build\n",
            )
            .unwrap();
            assert!(has_summary(&dir), "a legacy notes.md is still a summary");

            let migrated = migrate_legacy_summary(&dir).expect("migrated");
            assert!(migrated.contains("older build"));
            assert!(dir.join("summary.md").is_file());
            assert!(!dir.join("notes.md").exists());
        });
    }

    #[test]
    fn rename_updates_both_the_listing_and_the_transcript() {
        with_temp_home(|_| {
            let id = "20260724-103300-standup";
            let dir = seed_meeting(id);

            rename_meeting(id, "  Weekly sync  ").unwrap();

            // The listing reflects the new title...
            let listed = list_meetings();
            assert_eq!(listed.len(), 1);
            assert_eq!(listed[0].title, "Weekly sync");
            // ...and so does the exported markdown, body intact.
            let md = std::fs::read_to_string(dir.join("transcript.md")).unwrap();
            assert!(md.starts_with("# Weekly sync\n"), "got: {md:?}");
            assert!(md.contains("hi"));
            // The folder name is untouched — it encodes the recording time.
            assert!(dir.exists());

            assert!(rename_meeting(id, "   ").is_err(), "blank title must fail");
        });
    }

    #[test]
    fn delete_moves_the_meeting_to_the_trash() {
        with_temp_home(|home| {
            let id = "20260724-090000-acme";
            let dir = seed_meeting(id);

            let dest = delete_meeting(id).unwrap();

            assert!(!dir.exists(), "meeting should have left the recordings root");
            assert!(list_meetings().is_empty());
            // Recoverable: the audio and transcript still exist under ~/.Trash.
            let trashed = home.join(".Trash").join(id);
            assert_eq!(dest, trashed.display().to_string());
            assert!(trashed.join("transcript.md").exists());

            // A second meeting with the same name lands beside it, not on top.
            seed_meeting(id);
            let second = delete_meeting(id).unwrap();
            assert!(second.ends_with(&format!("{id} 2")), "got: {second}");
            assert!(trashed.join("transcript.md").exists());

            assert!(delete_meeting(id).is_err(), "already gone");
        });
    }

    #[test]
    fn transcript_markup_is_stripped_to_spoken_words() {
        let md = "# Standup\n\n_Recorded 2026-07-24 10:33_\n\n## Transcript\n\n**[0:00]** Morning everyone.\n\n**[0:04]** Let's start with the roadmap.\n";
        assert_eq!(
            strip_transcript_markup(md),
            "Morning everyone. Let's start with the roadmap."
        );
    }

    #[test]
    fn notes_are_cached_and_reused() {
        with_temp_home(|_| {
            let id = "20260724-110000-lecture";
            let dir = seed_meeting(id);
            std::fs::write(dir.join("summary.md"), "## Summary\n\ncached").unwrap();

            // Returns the cache without touching the model, which isn't present.
            assert_eq!(write_notes(id, false).unwrap(), "## Summary\n\ncached");
            assert!(list_meetings()[0].has_notes);
        });
    }

    #[test]
    fn meeting_dir_rejects_ids_that_escape_the_root() {
        // These must fail on the id check, before any filesystem lookup.
        for bad in ["", "..", "../../etc", "a/b", "/etc/passwd", "x\\y"] {
            assert!(
                meeting_dir(bad).is_err(),
                "expected {bad:?} to be rejected"
            );
        }
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
