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
    /// Which note template was used the last time notes were generated, so the
    /// picker reopens on the same choice. Defaults when nothing is recorded yet.
    pub template: crate::chat::Template,
    pub dir: String,
    /// The folder this meeting is filed under, or `None` when it sits directly
    /// under the recordings root (unsorted).
    pub folder: Option<String>,
}

/// All meetings on disk, newest first. Folder names sort chronologically, so
/// this is a reverse name sort. Unparseable folders are skipped rather than
/// failing the whole listing — a stray directory must not break the home screen.
pub fn list_meetings() -> Vec<Meeting> {
    let root = recordings_root();
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };

    // (id, dir, folder) for every meeting found, whether unsorted or filed.
    let mut found: Vec<(String, std::path::PathBuf, Option<String>)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if parse_stamp(&name).is_some() {
            found.push((name, path, None));
            continue;
        }
        // Not meeting-shaped: a folder. Walk one level in for its meetings.
        let Ok(inner) = std::fs::read_dir(&path) else { continue };
        for inner_entry in inner.flatten() {
            let inner_path = inner_entry.path();
            if !inner_path.is_dir() {
                continue;
            }
            let inner_name = inner_entry.file_name().to_string_lossy().into_owned();
            if parse_stamp(&inner_name).is_some() {
                found.push((inner_name, inner_path, Some(name.clone())));
            }
        }
    }

    found.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    found.reverse();

    found
        .into_iter()
        .map(|(name, dir, folder)| read_meeting(&dir, &name, folder))
        .collect()
}

/// One folder of meetings, as reconstructed from its directory under the
/// recordings root.
#[derive(Debug, Clone, Serialize)]
pub struct Folder {
    pub name: String,
    pub count: usize,
}

/// Every folder under the recordings root, alphabetically by name.
pub fn list_folders() -> Vec<Folder> {
    let root = recordings_root();
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };

    let mut folders: Vec<Folder> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if parse_stamp(&name).is_some() {
                return None;
            }
            let count = std::fs::read_dir(e.path())
                .map(|inner| {
                    inner
                        .flatten()
                        .filter(|i| {
                            i.path().is_dir()
                                && parse_stamp(&i.file_name().to_string_lossy()).is_some()
                        })
                        .count()
                })
                .unwrap_or(0);
            Some(Folder { name, count })
        })
        .collect();
    folders.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    folders
}

/// Reject anything that can't safely be a folder name: empty, path
/// separators, `..`, more than one path component, or a name that would
/// itself parse as a meeting timestamp — which would make a folder
/// indistinguishable from a meeting while walking the tree.
fn validate_folder_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || Path::new(name).components().count() != 1
    {
        return Err(format!("invalid folder name: {name}"));
    }
    if parse_stamp(name).is_some() {
        return Err(format!("\"{name}\" looks like a meeting, not a folder name"));
    }
    Ok(())
}

/// Create a new, empty folder.
pub fn create_folder(name: &str) -> Result<(), String> {
    let name = name.trim();
    validate_folder_name(name)?;
    let dir = recordings_root().join(name);
    if dir.exists() {
        return Err(format!("\"{name}\" already exists"));
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))
}

/// Rename a folder in place. Meetings inside move with it — it's the same
/// directory, just renamed.
pub fn rename_folder(old: &str, new: &str) -> Result<(), String> {
    let new = new.trim();
    validate_folder_name(new)?;
    let old_dir = recordings_root().join(old);
    if !old_dir.is_dir() {
        return Err(format!("no such folder: {old}"));
    }
    let new_dir = recordings_root().join(new);
    if new_dir.exists() {
        return Err(format!("\"{new}\" already exists"));
    }
    std::fs::rename(&old_dir, &new_dir).map_err(|e| format!("rename folder: {e}"))
}

/// Delete a folder. Refuses while it still holds anything, so filing a note
/// away is never silently undone by a folder cleanup.
pub fn delete_folder(name: &str) -> Result<(), String> {
    let dir = recordings_root().join(name);
    if !dir.is_dir() {
        return Err(format!("no such folder: {name}"));
    }
    let has_entries = std::fs::read_dir(&dir)
        .map_err(|e| format!("read {}: {e}", dir.display()))?
        .next()
        .is_some();
    if has_entries {
        return Err(format!("\"{name}\" still has notes in it — move them out first"));
    }
    std::fs::remove_dir(&dir).map_err(|e| format!("remove {}: {e}", dir.display()))
}

/// File a meeting into `folder`, or back to Unsorted when `folder` is
/// `None`. The meeting keeps its id — this only moves which directory it
/// lives in.
pub fn move_meeting_to_folder(id: &str, folder: Option<&str>) -> Result<(), String> {
    let dir = meeting_dir(id)?;

    let dest_parent = match folder {
        Some(name) => {
            validate_folder_name(name)?;
            let folder_dir = recordings_root().join(name);
            if !folder_dir.is_dir() {
                return Err(format!("no such folder: {name}"));
            }
            folder_dir
        }
        None => recordings_root(),
    };

    let dest = dest_parent.join(id);
    if dest == dir {
        return Ok(());
    }
    if dest.exists() {
        return Err(format!("a meeting named {id} already exists there"));
    }
    std::fs::rename(&dir, &dest).map_err(|e| format!("move {}: {e}", dir.display()))
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

fn read_meeting(dir: &Path, name: &str, folder: Option<String>) -> Meeting {
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
        template: meta_template(dir).unwrap_or_default(),
        dir: dir.display().to_string(),
        folder,
    }
}

/// Meetings whose title, transcript, or notes contain `query` (case-insensitive
/// substring). There's no index — one person's worth of meetings is cheap
/// enough to grep on every keystroke.
pub fn search_meetings(query: &str) -> Vec<Meeting> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return list_meetings();
    }
    list_meetings()
        .into_iter()
        .filter(|m| meeting_matches(m, &q))
        .collect()
}

/// Whether a meeting's title or on-disk content (transcript, typed notes, or
/// the model's summary) contains `q`. `q` must already be lowercased.
fn meeting_matches(m: &Meeting, q: &str) -> bool {
    if m.title.to_lowercase().contains(q) {
        return true;
    }
    let dir = Path::new(&m.dir);
    ["transcript.md", TYPED_NOTES, SUMMARY]
        .iter()
        .filter_map(|file| std::fs::read_to_string(dir.join(file)).ok())
        .any(|text| text.to_lowercase().contains(q))
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

/// The template notes were last generated with, if any were ever generated.
fn meta_template(dir: &Path) -> Option<crate::chat::Template> {
    serde_json::from_value(read_meta(dir).get("template")?.clone()).ok()
}

/// Read `meta.json` as a JSON object, or an empty one if it doesn't exist or
/// isn't valid JSON. Every writer merges into this so fields it doesn't touch
/// — a rename's title, a generation's chosen template — survive.
fn read_meta(dir: &Path) -> serde_json::Map<String, serde_json::Value> {
    std::fs::read_to_string(dir.join("meta.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

fn write_meta(dir: &Path, meta: &serde_json::Map<String, serde_json::Value>) -> Result<(), String> {
    let path = dir.join("meta.json");
    crate::store::write(&path, &serde_json::Value::Object(meta.clone()).to_string())
}

/// Serializes read-modify-write of `meta.json`.
///
/// `store::write` makes each individual write atomic, but not a read followed by
/// a write. That gap matters now that the heavy commands are
/// `#[tauri::command(async)]` and genuinely run on different threads: `write_notes`
/// reads meta, then writes it back after a long generation, so a `rename_meeting`
/// landing in between would have its new title overwritten by the stale copy.
/// One process-wide lock is enough — these edits are two-field JSON writes.
static META_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Read `meta.json`, let `edit` change it, and write it back — with no window for
/// another thread to read the same document and clobber the result.
fn update_meta(
    dir: &Path,
    edit: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>),
) -> Result<(), String> {
    let _guard = META_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut meta = read_meta(dir);
    edit(&mut meta);
    write_meta(dir, &meta)
}

/// Resolve a meeting id (a folder name) to its directory, refusing anything
/// that isn't a plain name directly under the recordings root. The id crosses
/// the boundary from the UI, so `..` or an absolute path must not be able to
/// point the rename/delete commands at arbitrary files.
/// Resolve a meeting id to its directory, wherever it's currently filed:
/// directly under the root (unsorted), or one level down inside a folder.
/// Folders are never nested further, so one extra level of search is enough.
fn find_meeting_dir(id: &str) -> Option<std::path::PathBuf> {
    let root = recordings_root();
    let flat = root.join(id);
    if flat.is_dir() {
        return Some(flat);
    }
    let entries = std::fs::read_dir(&root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() && parse_stamp(&name).is_none() {
            let nested = path.join(id);
            if nested.is_dir() {
                return Some(nested);
            }
        }
    }
    None
}

fn meeting_dir(id: &str) -> Result<std::path::PathBuf, String> {
    if id.is_empty()
        || id.contains('/')
        || id.contains('\\')
        || id.contains("..")
        || Path::new(id).components().count() != 1
    {
        return Err(format!("invalid meeting id: {id}"));
    }
    find_meeting_dir(id).ok_or_else(|| format!("no such meeting: {id}"))
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

    update_meta(&dir, |meta| {
        meta.insert("title".into(), serde_json::Value::String(title.to_string()));
    })?;

    let transcript = dir.join("transcript.md");
    if let Ok(text) = std::fs::read_to_string(&transcript) {
        let rest = text.split_once('\n').map(|(_, r)| r).unwrap_or("");
        let updated = format!("# {title}\n{rest}");
        crate::store::write(&transcript, &updated)?;
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

/// Replace characters that can't survive in a filename with `-`, trim the
/// result, and fall back to a fixed name if nothing usable is left.
fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' { c } else { '-' })
        .collect();
    let trimmed = cleaned.trim().trim_matches('-').trim();
    if trimmed.is_empty() { "meeting".to_string() } else { trimmed.to_string() }
}

/// Bundle a meeting's write-up and transcript into one shareable Markdown file
/// under `~/Downloads/Oatmeal Exports/<title>-<date>/`, and return that folder's
/// path. A meeting with nothing written up or transcribed yet has nothing worth
/// sharing, so that's an error rather than an empty file.
pub fn export_meeting(id: &str) -> Result<String, String> {
    let dir = meeting_dir(id)?;
    let meeting = read_meeting(&dir, id, None);

    let sections = [
        ("Notes", std::fs::read_to_string(dir.join(SUMMARY)).unwrap_or_default()),
        ("Your notes", typed_notes(id).unwrap_or_default()),
        ("Transcript", transcript_text(id).unwrap_or_default()),
    ];

    let mut body = format!("# {}\n\n", meeting.title);
    let mut has_content = false;
    for (heading, text) in &sections {
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        body.push_str(&format!("## {heading}\n\n{text}\n\n"));
        has_content = true;
    }
    if !has_content {
        return Err("this meeting has nothing to export yet".into());
    }

    let name = sanitize_filename(&meeting.title);
    let date = meeting.started_at.get(..10).unwrap_or("");
    let folder_name = if date.is_empty() { name.clone() } else { format!("{name}-{date}") };

    let home = std::env::var("HOME").map_err(|_| "no HOME set".to_string())?;
    let dest_dir = Path::new(&home)
        .join("Downloads")
        .join("Oatmeal Exports")
        .join(folder_name);
    std::fs::create_dir_all(&dest_dir)
        .map_err(|e| format!("create {}: {e}", dest_dir.display()))?;

    let dest_file = dest_dir.join(format!("{name}.md"));
    std::fs::write(&dest_file, body.trim_end().to_string() + "\n")
        .map_err(|e| format!("write {}: {e}", dest_file.display()))?;

    Ok(dest_dir.display().to_string())
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
pub(crate) fn strip_transcript_markup(md: &str) -> String {
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
pub fn write_notes(id: &str, template: crate::chat::Template, force: bool) -> Result<String, String> {
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
    let notes = crate::chat::write_notes(&model, &transcript, template)?;

    crate::store::write(&summary_path, &notes)?;

    update_meta(&dir, |meta| {
        meta.insert("template".into(), serde_json::to_value(template).unwrap());
    })?;

    Ok(notes)
}

/// Text to draft a follow-up message from: the AI-written summary (generating
/// it if this is the first time it's been asked for), falling back to what the
/// user typed by hand if there's no transcript to summarize. Never the raw
/// transcript — a follow-up should read like the notes, not the ASR output.
///
/// The template only decides how notes are *written*, and a meeting that
/// already has notes gets the cached copy back untouched, so the default is all
/// this path can meaningfully ask for.
pub fn followup_source(id: &str) -> Result<String, String> {
    match write_notes(id, crate::chat::Template::default(), false) {
        Ok(notes) => Ok(notes),
        Err(_) => {
            let typed = typed_notes(id)?;
            if typed.trim().is_empty() {
                Err("this meeting has no notes yet".into())
            } else {
                Ok(typed)
            }
        }
    }
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
    /// `$HOME` is process-global, so these tests can't overlap — not even with
    /// `homework.rs`'s tests, hence the shared lock in `settings.rs`.
    use crate::settings::HOME_ENV_LOCK as HOME_LOCK;

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
            assert_eq!(
                write_notes(id, crate::chat::Template::General, false).unwrap(),
                "## Summary\n\ncached"
            );
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
    fn search_matches_title_transcript_and_notes() {
        with_temp_home(|_| {
            let standup = seed_meeting("20260724-100000-standup");
            std::fs::write(standup.join("notes.md"), "# Standup\n\n_Written 2026-07-24 22:37_\n\nship the roadmap Friday\n").unwrap();

            let kickoff = seed_meeting("20260724-110000-kickoff");
            std::fs::write(kickoff.join("transcript.md"), "# Kickoff\n\n## Transcript\n\nwelcome aboard\n").unwrap();
            std::fs::write(kickoff.join("summary.md"), "## Summary\n\nAgreed on the budget\n").unwrap();

            let lecture = seed_meeting("20260724-120000-lecture");
            std::fs::write(lecture.join("transcript.md"), "# Lecture\n\n## Transcript\n\nquietly listening\n").unwrap();

            // Title match, case-insensitive.
            assert_eq!(search_meetings("kickoff").len(), 1);
            // Typed-notes content match.
            let by_notes = search_meetings("roadmap");
            assert_eq!(by_notes.len(), 1);
            assert_eq!(by_notes[0].id, "20260724-100000-standup");
            // Model summary content match.
            let by_summary = search_meetings("budget");
            assert_eq!(by_summary.len(), 1);
            assert_eq!(by_summary[0].id, "20260724-110000-kickoff");
            // Transcript body content match.
            let by_transcript = search_meetings("Quietly Listening");
            assert_eq!(by_transcript.len(), 1);
            assert_eq!(by_transcript[0].id, "20260724-120000-lecture");
            // No match anywhere.
            assert!(search_meetings("nonexistent query").is_empty());
            // Blank query behaves like list_meetings.
            assert_eq!(search_meetings("   ").len(), 3);
        });
    }

    #[test]
    fn recovers_title_from_slug() {
        assert_eq!(
            title_from_slug("20260724-103300-acme-discovery").as_deref(),
            Some("Acme discovery")
        );
        assert_eq!(title_from_slug("20260724-103300"), None);
    }

    #[test]
    fn sanitizes_titles_into_filenames() {
        assert_eq!(sanitize_filename("Acme: Q3 / Discovery"), "Acme- Q3 - Discovery");
        assert_eq!(sanitize_filename("  ../../etc  "), "etc");
        assert_eq!(sanitize_filename("::://"), "meeting");
    }

    #[test]
    fn export_bundles_notes_and_transcript_into_downloads() {
        with_temp_home(|home| {
            let id = "20260724-103300-standup";
            let dir = seed_meeting(id);
            std::fs::write(dir.join("summary.md"), "## Summary\n\nShipped the export feature.").unwrap();

            let dest = export_meeting(id).unwrap();
            let expected_dir = home
                .join("Downloads")
                .join("Oatmeal Exports")
                .join("Standup-2026-07-24");
            assert_eq!(dest, expected_dir.display().to_string());

            let md = std::fs::read_to_string(expected_dir.join("Standup.md")).unwrap();
            assert!(md.starts_with("# Standup\n"), "got: {md:?}");
            assert!(md.contains("Shipped the export feature."));
            assert!(md.contains("hi"), "transcript body should be bundled too");
        });
    }

    #[test]
    fn export_fails_when_there_is_nothing_to_share() {
        with_temp_home(|_| {
            let id = "20260724-103300-empty";
            let dir = recordings_root().join(id);
            std::fs::create_dir_all(&dir).unwrap();
            assert!(export_meeting(id).is_err());
        });
    }

    #[test]
    fn list_meetings_tags_folder_for_nested_and_root() {
        with_temp_home(|_| {
            seed_meeting("20260724-090000-root-standup");

            let folder_dir = recordings_root().join("Client Work");
            std::fs::create_dir_all(&folder_dir).unwrap();
            let nested = folder_dir.join("20260724-100000-kickoff");
            std::fs::create_dir_all(&nested).unwrap();
            std::fs::write(nested.join("transcript.md"), "# Kickoff\n\n## Transcript\n\nhi\n").unwrap();

            let listed = list_meetings();
            assert_eq!(listed.len(), 2);

            let root = listed.iter().find(|m| m.id == "20260724-090000-root-standup").unwrap();
            assert_eq!(root.folder, None);

            let nested_m = listed.iter().find(|m| m.id == "20260724-100000-kickoff").unwrap();
            assert_eq!(nested_m.folder.as_deref(), Some("Client Work"));
        });
    }

    #[test]
    fn meeting_dir_resolves_meetings_nested_in_a_folder() {
        with_temp_home(|_| {
            let folder_dir = recordings_root().join("Client Work");
            std::fs::create_dir_all(&folder_dir).unwrap();
            let id = "20260724-100000-kickoff";
            let nested = folder_dir.join(id);
            std::fs::create_dir_all(&nested).unwrap();
            std::fs::write(nested.join("transcript.md"), "# Kickoff\n\n## Transcript\n\nhi\n").unwrap();

            // rename_meeting resolves its directory through meeting_dir, so this
            // exercises the folder-aware lookup end to end.
            rename_meeting(id, "Renamed kickoff").unwrap();
            let listed = list_meetings();
            assert_eq!(listed[0].title, "Renamed kickoff");
        });
    }

    #[test]
    fn folders_can_be_created_renamed_and_listed() {
        with_temp_home(|_| {
            create_folder("Client Work").unwrap();
            create_folder("  Interviews  ").unwrap(); // trimmed before use
            assert!(create_folder("Client Work").is_err(), "duplicate name");

            let mut names: Vec<_> = list_folders().into_iter().map(|f| f.name).collect();
            names.sort();
            assert_eq!(names, vec!["Client Work", "Interviews"]);

            rename_folder("Interviews", "Candidate interviews").unwrap();
            let names: Vec<_> = list_folders().into_iter().map(|f| f.name).collect();
            assert!(names.contains(&"Candidate interviews".to_string()));
            assert!(!names.contains(&"Interviews".to_string()));

            assert!(
                rename_folder("Candidate interviews", "Client Work").is_err(),
                "collides with an existing folder"
            );
        });
    }

    #[test]
    fn folder_names_are_validated() {
        with_temp_home(|_| {
            assert!(create_folder("20260724-100000-standup").is_err(), "looks like a meeting");
            assert!(create_folder("").is_err());
            assert!(create_folder("a/b").is_err());
            assert!(create_folder("../escape").is_err());
        });
    }

    #[test]
    fn delete_folder_is_blocked_while_it_has_notes() {
        with_temp_home(|_| {
            create_folder("Client Work").unwrap();
            let nested = recordings_root().join("Client Work").join("20260724-100000-kickoff");
            std::fs::create_dir_all(&nested).unwrap();

            assert!(delete_folder("Client Work").is_err(), "must refuse while non-empty");

            std::fs::remove_dir_all(&nested).unwrap();
            delete_folder("Client Work").unwrap();
            assert!(list_folders().is_empty());
        });
    }

    #[test]
    fn move_meeting_files_and_unfiles() {
        with_temp_home(|_| {
            let id = "20260724-100000-kickoff";
            seed_meeting(id);
            create_folder("Client Work").unwrap();

            move_meeting_to_folder(id, Some("Client Work")).unwrap();
            let listed = list_meetings();
            assert_eq!(listed[0].folder.as_deref(), Some("Client Work"));
            assert!(!recordings_root().join(id).exists());
            assert!(recordings_root().join("Client Work").join(id).exists());

            move_meeting_to_folder(id, None).unwrap();
            let listed = list_meetings();
            assert_eq!(listed[0].folder, None);
            assert!(recordings_root().join(id).exists());
        });
    }

    #[test]
    fn move_meeting_rejects_an_unknown_folder() {
        with_temp_home(|_| {
            let id = "20260724-100000-kickoff";
            seed_meeting(id);
            assert!(move_meeting_to_folder(id, Some("Nonexistent")).is_err());
        });
    }
}
