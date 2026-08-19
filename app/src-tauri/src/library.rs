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
    /// Recorded length in seconds, summed across every take. 0 when no readable
    /// audio survives.
    pub duration_secs: u64,
    /// Whether `transcript.md` was written (i.e. the meeting finished and was
    /// transcribed, rather than being abandoned mid-recording).
    pub transcribed: bool,
    /// Segments that hold audio `transcript.md` doesn't cover — the app died
    /// mid-recording, or a continuation was never stopped cleanly. Non-empty
    /// means the meeting can be finished with one press.
    pub pending_segments: Vec<u32>,
    /// Whether the language model has already written notes for this meeting.
    pub has_notes: bool,
    /// Whether those notes predate audio that has since been added, so they
    /// describe only part of the meeting.
    pub notes_stale: bool,
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

/// The model's write-up of the transcript, under the `General` template — and
/// also the legacy filename from before each template got its own cache file,
/// which is why `cached_notes` and `migrate_legacy_cache` below treat it
/// specially. Separate from `TYPED_NOTES` because both used to land in the
/// same file: whatever you typed made the app believe notes had been
/// generated, the Enhanced tab showed your own typing back to you, and
/// generating for real would have overwritten it.
const SUMMARY: &str = "summary.md";

/// Filename holding one template's cached notes. `General` keeps the original
/// `summary.md` name; the rest get their own file so switching templates can
/// never overwrite another template's notes. Fixed strings, not built from
/// anything a caller controls, so a filename can't collide with another
/// meeting file or escape the meeting directory. The match is exhaustive on
/// purpose: a new `Template` variant fails to compile here until it's given a
/// file of its own.
fn summary_filename(template: crate::chat::Template) -> &'static str {
    match template {
        crate::chat::Template::General => SUMMARY,
        crate::chat::Template::Standup => "summary-standup.md",
        crate::chat::Template::OneOnOne => "summary-one-on-one.md",
        crate::chat::Template::Interview => "summary-interview.md",
    }
}

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

/// The first time we see a meeting from before per-template caching, copy its
/// `summary.md` into the dedicated file for the template `meta.json` says it
/// was generated under. Without this, generating notes under a *different*
/// template later would overwrite `summary.md` (that other template's
/// dedicated file, per `summary_filename`) and destroy the only copy of the
/// original notes.
///
/// `summary.md` itself is left exactly where it is — this only ever creates a
/// second copy, never removes anything. A no-op once that dedicated file
/// exists, or for a meeting whose recorded (or defaulted) template is
/// `General`, since `summary.md` already *is* General's file.
fn migrate_legacy_cache(dir: &Path) {
    let template = meta_template(dir).unwrap_or_default();
    if template == crate::chat::Template::General {
        return;
    }
    let dedicated = dir.join(summary_filename(template));
    if dedicated.is_file() {
        return;
    }
    if let Ok(text) = std::fs::read_to_string(dir.join(SUMMARY)) {
        if !text.trim().is_empty() {
            let _ = crate::store::write(&dedicated, &text);
        }
    }
}

/// The cached notes for `template`, if this meeting has any: its dedicated
/// file first, then `summary.md` as a fallback for a meeting that predates
/// per-template caching and whose `meta.json` says `summary.md` is that
/// template's notes. `General`'s dedicated file *is* `summary.md`, so it only
/// ever goes through that same gated fallback check — otherwise a legacy
/// meeting generated under, say, Interview would have its notes handed back
/// for a `General` request too, just because they happen to share a filename.
fn cached_notes(dir: &Path, template: crate::chat::Template) -> Option<String> {
    if template != crate::chat::Template::General {
        if let Ok(text) = std::fs::read_to_string(dir.join(summary_filename(template))) {
            if !text.trim().is_empty() {
                return Some(text);
            }
        }
    }
    if meta_template(dir).unwrap_or_default() == template {
        if let Ok(text) = std::fs::read_to_string(dir.join(SUMMARY)) {
            if !text.trim().is_empty() {
                return Some(text);
            }
        }
    }
    None
}

/// The cached notes for whichever template this meeting is currently showing
/// (`meta.json`'s `template`, defaulting like the picker does). Read-only —
/// never generates and never migrates — so callers that just want to display
/// or search a meeting's notes (export, search) can call it on every meeting
/// without risking a model run or a write.
fn selected_notes(dir: &Path) -> String {
    cached_notes(dir, meta_template(dir).unwrap_or_default()).unwrap_or_default()
}

/// Whether a meeting already has a model-written summary, under any template.
fn has_summary(dir: &Path) -> bool {
    let templates = [
        crate::chat::Template::General,
        crate::chat::Template::Standup,
        crate::chat::Template::OneOnOne,
        crate::chat::Template::Interview,
    ];
    if templates.iter().any(|t| dir.join(summary_filename(*t)).is_file()) {
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

    Meeting {
        id: name.to_string(),
        title,
        started_at: parse_stamp(name).unwrap_or_default(),
        duration_secs: crate::session::total_len_secs(dir),
        transcribed,
        pending_segments: pending_segments(dir),
        has_notes: has_summary(dir),
        notes_stale: read_meta(dir)
            .get("notes_stale")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        template: meta_template(dir).unwrap_or_default(),
        dir: dir.display().to_string(),
        folder,
    }
}

/// One meeting by id, wherever it is filed. `folder` is left unset — the folder
/// is learned from the directory walk, which a single lookup doesn't do.
pub fn meeting(id: &str) -> Result<Meeting, String> {
    let dir = meeting_dir(id)?;
    Ok(read_meeting(&dir, id, None))
}

/// How many segments `transcript.md` already covers.
///
/// Meetings recorded before continuations existed carry no counter, so a
/// transcript is taken to mean their one and only segment is done — otherwise
/// every meeting in the library would suddenly offer to be re-transcribed.
fn transcribed_segments(dir: &Path) -> u32 {
    if let Some(n) = read_meta(dir)
        .get("transcribed_segments")
        .and_then(|v| v.as_u64())
    {
        return n as u32;
    }
    u32::from(dir.join("transcript.md").is_file())
}

/// Segments with audio that never made it into `transcript.md`.
fn pending_segments(dir: &Path) -> Vec<u32> {
    let done = transcribed_segments(dir);
    crate::session::recorded_segments(dir)
        .into_iter()
        .filter(|n| *n > done)
        .collect()
}

/// Record that `transcript.md` now covers every segment up to `n`.
pub fn record_transcribed_segment(dir: &Path, n: u32) -> Result<(), String> {
    update_meta(dir, |meta| {
        let done = meta
            .get("transcribed_segments")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        if u64::from(n) > done {
            meta.insert("transcribed_segments".into(), serde_json::json!(n));
        }
    })
}

/// Flag a meeting's model-written notes as describing only part of it, because
/// audio recorded after they were written has now been transcribed in.
/// Regenerating is a full model run, so the user is told rather than charged
/// for it silently. A meeting with no notes has nothing to go stale.
pub fn mark_notes_stale(dir: &Path) -> Result<(), String> {
    if !has_summary(dir) {
        return Ok(());
    }
    update_meta(dir, |meta| {
        meta.insert("notes_stale".into(), serde_json::Value::Bool(true));
    })
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
    if selected_notes(dir).to_lowercase().contains(q) {
        return true;
    }
    ["transcript.md", TYPED_NOTES, "video-1.md", "video-2.md", "video-3.md"]
        .iter()
        .filter_map(|file| std::fs::read_to_string(dir.join(file)).ok())
        .any(|text| text.to_lowercase().contains(q))
}

// ── search snippets ─────────────────────────────────────────────────────────
//
// Knowing that a meeting matched isn't much help when the transcript is
// thousands of words long: the useful answer is the sentence the words were
// said in. These excerpts are bounded on both axes — a fixed context window and
// a fixed count per meeting — so a one-letter query can't hand the UI a whole
// transcript to render.

/// Characters of context kept either side of a hit.
const SNIPPET_CONTEXT: usize = 70;

/// Most snippets returned for one meeting, counting every source together.
const MAX_SNIPPETS: usize = 3;

/// Which part of a meeting an excerpt came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SnippetSource {
    Title,
    /// Either file of notes: what the model wrote, or what the user typed.
    Notes,
    Transcript,
}

/// One excerpt of a meeting, around one occurrence of the query.
#[derive(Debug, Clone, Serialize)]
pub struct Snippet {
    pub source: SnippetSource,
    /// A single line of context around the hit, elided with `…` where it was
    /// clipped. Whitespace is collapsed so it reads as one line in a list.
    pub text: String,
}

/// A meeting that matched a search, and where it matched.
#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub meeting: Meeting,
    pub snippets: Vec<Snippet>,
}

/// Lowercase one char at a time so the result indexes 1:1 with the original.
/// `str::to_lowercase` can lengthen the string (`İ` → two chars), which would
/// slide every index after it and slice the window in the wrong place; keeping
/// only the first char of such an expansion trades an exotic mismatch for
/// indices that always line up.
fn lower_chars(chars: &[char]) -> Vec<char> {
    chars
        .iter()
        .map(|c| c.to_lowercase().next().unwrap_or(*c))
        .collect()
}

/// Char index of the first occurrence of `needle` in `hay`, both lowercased.
fn find_chars(hay: &[char], needle: &[char]) -> Option<usize> {
    if needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| &hay[i..i + needle.len()] == needle)
}

/// Squash runs of whitespace (newlines included) into single spaces, so an
/// excerpt spanning a line break still renders as one line.
fn collapse_ws(chars: &[char]) -> String {
    chars
        .iter()
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Excerpts around each occurrence of `needle` (already lowercased) in `text`,
/// stopping at `limit`. Hits inside a window already emitted are skipped, so a
/// dense match doesn't return the same sentence three times.
fn snippets_from(
    source: SnippetSource,
    text: &str,
    needle: &[char],
    limit: usize,
) -> Vec<Snippet> {
    if needle.is_empty() || limit == 0 {
        return Vec::new();
    }
    let chars: Vec<char> = text.chars().collect();
    let lower = lower_chars(&chars);

    let mut out = Vec::new();
    let mut from = 0;
    while out.len() < limit && from < lower.len() {
        let Some(rel) = find_chars(&lower[from..], needle) else {
            break;
        };
        let at = from + rel;
        let start = at.saturating_sub(SNIPPET_CONTEXT);
        let end = (at + needle.len() + SNIPPET_CONTEXT).min(chars.len());

        let mut excerpt = String::new();
        if start > 0 {
            excerpt.push('…');
        }
        excerpt.push_str(&collapse_ws(&chars[start..end]));
        if end < chars.len() {
            excerpt.push('…');
        }
        out.push(Snippet { source, text: excerpt });

        from = end.max(at + needle.len());
    }
    out
}

/// Every excerpt worth showing for one meeting: title first, then notes, then
/// the transcript, capped at `MAX_SNIPPETS` across all of them.
fn meeting_snippets(m: &Meeting, needle: &[char]) -> Vec<Snippet> {
    let dir = Path::new(&m.dir);
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).unwrap_or_default();
    // The transcript is excerpted from its spoken words, not the raw markdown:
    // nobody wants `**[0:04]**` in the middle of a quote.
    let spoken = strip_transcript_markup(&read("transcript.md"));

    let sources = [
        (SnippetSource::Title, m.title.clone()),
        (SnippetSource::Notes, selected_notes(dir)),
        (SnippetSource::Notes, read(TYPED_NOTES)),
        (SnippetSource::Transcript, spoken),
    ];

    let mut out: Vec<Snippet> = Vec::new();
    for (source, text) in sources {
        if out.len() >= MAX_SNIPPETS {
            break;
        }
        out.extend(snippets_from(source, &text, needle, MAX_SNIPPETS - out.len()));
    }
    out
}

/// Meetings matching `query`, each with the excerpts that made it match.
///
/// Matching is the same case-insensitive substring test `search_meetings` uses
/// — the whole query, spaces and all, so a two-word query is a phrase — because
/// the caller shows both result sets and they must not disagree. A blank query
/// isn't a search and has nothing to excerpt, so it returns nothing rather than
/// every meeting.
pub fn search_hits(query: &str) -> Vec<SearchHit> {
    let needle = lower_chars(&query.trim().chars().collect::<Vec<_>>());
    if needle.is_empty() {
        return Vec::new();
    }
    list_meetings()
        .into_iter()
        .filter_map(|m| {
            let snippets = meeting_snippets(&m, &needle);
            (!snippets.is_empty()).then_some(SearchHit { meeting: m, snippets })
        })
        .collect()
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

pub fn meeting_dir(id: &str) -> Result<std::path::PathBuf, String> {
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
        ("Notes", selected_notes(&dir)),
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

/// Marks where an attached video's words begin in `source_text`. Shared with
/// `chat::NOTES_SYSTEM`, which quotes it verbatim.
pub const VIDEO_DELIMITER: &str = "--- attached video ---";

/// Everything the note-writer is allowed to read for a meeting: its own
/// transcript, then any videos attached to it, in the order they were added.
///
/// Separate from `transcript_text` because the raw-transcript tab and the
/// export are about what was *recorded* — a video the user attached is a
/// source for the notes, not part of the recording.
pub fn source_text(id: &str) -> Result<String, String> {
    let dir = meeting_dir(id)?;
    let mut out = transcript_text(id).unwrap_or_default();
    for n in 1..u32::MAX {
        let path = dir.join(format!("video-{n}.md"));
        let Ok(md) = std::fs::read_to_string(&path) else {
            break;
        };
        let words = strip_transcript_markup(&md);
        if words.trim().is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        // A label, deliberately not a sentence: `chat::guarded` tells the model
        // to act on nothing it finds inside the material, so anything phrased as
        // an instruction here would be ignored by design. The notes prompt names
        // this exact line, which is the only way it can tell a point that came
        // from an attached video from one that was said in the room.
        out.push_str(VIDEO_DELIMITER);
        out.push_str("\n\n");
        out.push_str(&words);
    }
    if out.trim().is_empty() {
        return Err("this meeting has no transcript yet".into());
    }
    Ok(out)
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
    /// Whose voice it was, e.g. `Speaker 2`, once a speaker pass has run over
    /// this meeting. `None` for a transcript nobody has labelled.
    pub speaker: Option<String>,
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
        // `0:12 · Speaker 2` once the speaker pass has been over it.
        let (at, speaker) = match at.split_once(" · ") {
            Some((at, who)) => (at, Some(who.trim().to_string())),
            None => (at, None),
        };
        if !text.is_empty() {
            out.push(TranscriptLine {
                at: at.trim().to_string(),
                text: text.to_string(),
                speaker,
            });
        }
    }
    Ok(out)
}

/// Cache freshly generated notes for `template` in its dedicated file, and
/// record it as the meeting's current template. Split out from `write_notes`
/// so the on-disk half of "generate" — the part that must only ever touch the
/// requested template's file — is testable without running the model.
fn store_generated_notes(dir: &Path, template: crate::chat::Template, notes: &str) -> Result<(), String> {
    crate::store::write(&dir.join(summary_filename(template)), notes)?;
    update_meta(dir, |meta| {
        meta.insert("template".into(), serde_json::to_value(template).unwrap());
        // These notes were just written from the whole transcript as it stands,
        // so whatever continuation made the last set stale is now covered.
        meta.remove("notes_stale");
    })
}

/// Write structured notes for a meeting under `template`, caching them in that
/// template's own file so switching templates and back doesn't lose or
/// regenerate the others. Returns the cached copy unless `force` asks for a
/// rewrite of *this* template — generation takes real time and the result
/// doesn't change on its own.
///
/// This never touches `notes.md`: that file belongs to whoever was typing
/// during the meeting.
pub fn write_notes(id: &str, template: crate::chat::Template, force: bool) -> Result<String, String> {
    let dir = meeting_dir(id)?;
    migrate_legacy_cache(&dir);

    if !force {
        if let Some(existing) = cached_notes(&dir, template) {
            return Ok(existing);
        }
        if template == crate::chat::Template::General {
            if let Some(migrated) = migrate_legacy_summary(&dir) {
                return Ok(migrated);
            }
        }
    }

    let transcript = source_text(id)?;
    let model = crate::model::ensure_chat_model()?;
    let notes = crate::chat::write_notes(&model, &transcript, template)?;

    store_generated_notes(&dir, template, &notes)?;

    Ok(notes)
}

/// Text to draft a follow-up message from: the AI-written summary (generating
/// it if this is the first time it's been asked for), falling back to what the
/// user typed by hand if there's no transcript to summarize. Never the raw
/// transcript — a follow-up should read like the notes, not the ASR output.
///
/// Uses whichever template the meeting is currently showing (`meta.json`'s
/// `template`, same default the picker uses), so a meeting that's been
/// written up as an Interview gets a follow-up drafted from its Interview
/// notes rather than triggering a fresh General generation.
pub fn followup_source(id: &str) -> Result<String, String> {
    let dir = meeting_dir(id)?;
    let template = meta_template(&dir).unwrap_or_default();
    match write_notes(id, template, false) {
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

#[cfg(test)]
mod tests {

    /// A labelled transcript has to come back with the voice split out, and an
    /// unlabelled one — every transcript written before speakers existed —
    /// has to keep working.
    #[test]
    fn transcript_lines_split_the_speaker_out() {
        with_temp_home(|_| {
            let id = "20260101-000000-m";
            let dir = recordings_root().join(id);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("transcript.md"),
                "# M\n\n## Transcript\n\n**[0:00 · Speaker 2]** hello\n\n**[0:09]** plain\n",
            )
            .unwrap();

            let lines = transcript_lines(id).unwrap();
            assert_eq!(lines.len(), 2);
            assert_eq!(lines[0].at, "0:00");
            assert_eq!(lines[0].speaker.as_deref(), Some("Speaker 2"));
            assert_eq!(lines[0].text, "hello");
            assert_eq!(lines[1].at, "0:09");
            assert_eq!(lines[1].speaker, None);
        })
    }
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
    fn notes_for_one_template_survive_generating_another() {
        with_temp_home(|_| {
            let id = "20260724-110000-interview";
            let dir = seed_meeting(id);
            // Pre-seed both caches directly, so neither `write_notes` call below
            // is a cache miss and the (absent) model is never reached.
            std::fs::write(dir.join("summary.md"), "## General\n\ngeneral notes").unwrap();
            std::fs::write(dir.join("summary-interview.md"), "## Interview\n\ninterview notes").unwrap();

            assert_eq!(
                write_notes(id, crate::chat::Template::General, false).unwrap(),
                "## General\n\ngeneral notes"
            );
            assert_eq!(
                write_notes(id, crate::chat::Template::Interview, false).unwrap(),
                "## Interview\n\ninterview notes"
            );
            // Switching back to General doesn't disturb Interview's file, or vice
            // versa — this is the bug report: picking a different template must
            // not "forget" what the other one had.
            assert_eq!(
                write_notes(id, crate::chat::Template::General, false).unwrap(),
                "## General\n\ngeneral notes"
            );
            assert_eq!(
                std::fs::read_to_string(dir.join("summary-interview.md")).unwrap(),
                "## Interview\n\ninterview notes"
            );
        });
    }

    #[test]
    fn legacy_summary_is_returned_for_the_template_it_was_generated_under() {
        with_temp_home(|_| {
            let id = "20260724-120000-legacy";
            let dir = seed_meeting(id);
            // As a pre-fix meeting would look: one summary.md, and meta.json
            // recording which template it was written under.
            std::fs::write(dir.join("summary.md"), "## Summary\n\nwritten before templates existed").unwrap();
            update_meta(&dir, |meta| {
                meta.insert(
                    "template".into(),
                    serde_json::to_value(crate::chat::Template::Interview).unwrap(),
                );
            })
            .unwrap();

            // Asking for the template it was actually generated under returns it
            // as-is — no model, no regeneration.
            let notes = write_notes(id, crate::chat::Template::Interview, false).unwrap();
            assert_eq!(notes, "## Summary\n\nwritten before templates existed");

            // It's been given its own dedicated file, so a later switch away and
            // back no longer depends on meta.json's `template` field to find it.
            assert_eq!(
                std::fs::read_to_string(dir.join("summary-interview.md")).unwrap(),
                "## Summary\n\nwritten before templates existed"
            );
            // And summary.md itself is untouched — nothing here deletes it.
            assert!(dir.join("summary.md").is_file());
        });
    }

    #[test]
    fn regenerating_one_template_leaves_the_others_untouched() {
        with_temp_home(|_| {
            let id = "20260724-130000-panel";
            let dir = seed_meeting(id);
            std::fs::write(dir.join("summary.md"), "## General\n\noriginal").unwrap();
            std::fs::write(dir.join("summary-standup.md"), "## Standup\n\noriginal").unwrap();

            // This is exactly what `write_notes` does after the model returns —
            // exercised directly so `force`'s isolation is verified without
            // running the (multi-gigabyte, slow) local model.
            store_generated_notes(&dir, crate::chat::Template::Interview, "## Interview\n\nfresh").unwrap();

            assert_eq!(
                std::fs::read_to_string(dir.join("summary.md")).unwrap(),
                "## General\n\noriginal",
                "regenerating Interview must not touch General's file"
            );
            assert_eq!(
                std::fs::read_to_string(dir.join("summary-standup.md")).unwrap(),
                "## Standup\n\noriginal",
                "regenerating Interview must not touch Standup's file"
            );
            assert_eq!(
                std::fs::read_to_string(dir.join("summary-interview.md")).unwrap(),
                "## Interview\n\nfresh"
            );
            assert_eq!(meta_template(&dir), Some(crate::chat::Template::Interview));
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

    /// A meeting folder with a chosen title and one line of spoken words, so a
    /// snippet test can put the hit exactly where it wants it.
    fn seed_transcript(id: &str, title: &str, body: &str) -> std::path::PathBuf {
        let dir = recordings_root().join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("transcript.md"),
            format!("# {title}\n\n_Recorded 2026-07-24 10:33_\n\n## Transcript\n\n**[0:00]** {body}\n"),
        )
        .unwrap();
        dir
    }

    #[test]
    fn snippets_reach_the_very_start_and_the_very_end_of_the_text() {
        with_temp_home(|_| {
            let filler = "filler ".repeat(80);
            seed_transcript("20260724-100000-opening", "Opening", &format!("roadmap {filler}"));
            seed_transcript("20260724-110000-closing", "Closing", &format!("{filler} roadmap"));

            let hits = search_hits("roadmap");
            assert_eq!(hits.len(), 2);
            let snips = |id: &str| {
                hits.iter()
                    .find(|h| h.meeting.id == id)
                    .unwrap_or_else(|| panic!("no hit for {id}"))
                    .snippets
                    .clone()
            };

            // A hit at char 0 has nothing to its left, so no leading ellipsis.
            let opening = snips("20260724-100000-opening");
            assert_eq!(opening.len(), 1);
            assert_eq!(opening[0].source, SnippetSource::Transcript);
            assert!(opening[0].text.starts_with("roadmap"), "got: {:?}", opening[0].text);
            assert!(opening[0].text.ends_with('…'), "got: {:?}", opening[0].text);

            // ...and a hit on the last word has nothing to its right.
            let closing = snips("20260724-110000-closing");
            assert_eq!(closing.len(), 1);
            assert!(closing[0].text.starts_with('…'), "got: {:?}", closing[0].text);
            assert!(closing[0].text.ends_with("roadmap"), "got: {:?}", closing[0].text);
        });
    }

    #[test]
    fn snippets_are_capped_per_meeting_and_bounded_in_length() {
        with_temp_home(|_| {
            let gap = "filler ".repeat(60);
            let body = vec!["roadmap"; 6].join(&gap);
            seed_transcript("20260724-100000-long", "Long", &body);

            let hits = search_hits("roadmap");
            assert_eq!(hits.len(), 1);
            let snips = &hits[0].snippets;
            assert_eq!(snips.len(), MAX_SNIPPETS, "six hits must not all come back");
            for s in snips {
                assert!(s.text.to_lowercase().contains("roadmap"), "got: {:?}", s.text);
                let max = 2 * SNIPPET_CONTEXT + "roadmap".len() + 2;
                assert!(s.text.chars().count() <= max, "{} chars: {:?}", s.text.chars().count(), s.text);
            }
            // Separate windows, not the same sentence three times over.
            assert_ne!(snips[0].text, snips[1].text);

            // A one-letter query is the worst case for dumping a transcript.
            let broad = search_hits("a");
            assert_eq!(broad.len(), 1);
            assert!(broad[0].snippets.len() <= MAX_SNIPPETS);
            for s in &broad[0].snippets {
                assert!(s.text.chars().count() <= 2 * SNIPPET_CONTEXT + 3);
            }
        });
    }

    #[test]
    fn a_title_only_match_reports_the_title_as_its_source() {
        with_temp_home(|_| {
            // No notes.md and no summary.md — a meeting nobody has written up yet.
            seed_transcript("20260724-100000-acme", "Acme discovery", "we talked about pricing");

            let hits = search_hits("ACME");
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].snippets.len(), 1);
            assert_eq!(hits[0].snippets[0].source, SnippetSource::Title);
            assert_eq!(hits[0].snippets[0].text, "Acme discovery");

            assert!(search_hits("nothing like this").is_empty());
            assert!(search_hits("   ").is_empty(), "a blank query is not a search");
        });
    }

    #[test]
    fn content_shorter_than_the_window_is_excerpted_whole() {
        with_temp_home(|_| {
            let dir = seed_transcript("20260724-100000-standup", "Standup", "nothing relevant here");
            std::fs::write(dir.join("notes.md"), "ship it").unwrap();

            let hits = search_hits("ship");
            assert_eq!(hits.len(), 1);
            let s = &hits[0].snippets[0];
            assert_eq!(s.source, SnippetSource::Notes);
            assert_eq!(s.text, "ship it", "text shorter than the window needs no ellipsis");
        });
    }

    #[test]
    fn a_multi_term_query_is_a_phrase_and_sources_serialize_lowercase() {
        with_temp_home(|_| {
            seed_transcript(
                "20260724-100000-standup",
                "Standup",
                "we agreed to Ship The Roadmap on Friday",
            );

            let hits = search_hits("ship the roadmap");
            assert_eq!(hits.len(), 1);
            assert!(hits[0].snippets[0].text.contains("Ship The Roadmap"), "got: {:?}", hits[0].snippets[0].text);
            // Words that never appear together don't match — same rule as
            // `search_meetings`, so the two result sets can't disagree.
            assert!(search_hits("roadmap tuesday").is_empty());
            assert_eq!(search_meetings("ship the roadmap").len(), hits.len());

            // The frontend keys the source badge off these strings.
            let wire = serde_json::to_value(&hits).unwrap();
            assert_eq!(wire[0]["snippets"][0]["source"], "transcript");
            assert_eq!(wire[0]["meeting"]["id"], "20260724-100000-standup");
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

    // ── unfinished recordings ────────────────────────────────────────────────

    /// A lane WAV of `frames` silent 16 kHz samples, as a recording leaves behind.
    fn lane(dir: &Path, name: &str, frames: u32) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(dir.join(name), spec).unwrap();
        for _ in 0..frames {
            w.write_sample(0i16).unwrap();
        }
        w.finalize().unwrap();
    }

    #[test]
    fn an_interrupted_recording_is_offered_for_finishing() {
        with_temp_home(|_| {
            // Oatmeal died mid-meeting: the audio is on disk, nothing ever ran
            // Whisper over it, and today the meeting is simply stranded.
            let crashed = recordings_root().join("20260812-090000-crashed");
            std::fs::create_dir_all(&crashed).unwrap();
            lane(&crashed, "mic.wav", 16_000 * 42);

            // A lane that opened and captured nothing is not a meeting to
            // finish — there is no audio in it.
            let silent = recordings_root().join("20260812-080000-silent");
            std::fs::create_dir_all(&silent).unwrap();
            lane(&silent, "mic.wav", 0);

            // And a meeting that finished normally — no `transcribed_segments`
            // counter, because it predates continuations — must stay finished
            // rather than offering to be transcribed all over again.
            let done = seed_meeting("20260812-070000-standup");
            lane(&done, "mic.wav", 16_000 * 60);

            let listed = list_meetings();
            let by = |id: &str| listed.iter().find(|m| m.id == id).unwrap().clone();

            let crashed = by("20260812-090000-crashed");
            assert!(!crashed.transcribed);
            assert_eq!(crashed.pending_segments, vec![1]);
            assert_eq!(crashed.duration_secs, 42);

            assert!(by("20260812-080000-silent").pending_segments.is_empty());
            assert!(by("20260812-070000-standup").pending_segments.is_empty());
        });
    }

    #[test]
    fn a_continuation_that_was_never_transcribed_is_pending_on_its_own() {
        with_temp_home(|_| {
            let id = "20260812-100000-acme";
            let dir = seed_meeting(id);
            lane(&dir, "mic.wav", 16_000 * 60);
            record_transcribed_segment(&dir, 1).unwrap();
            assert!(meeting(id).unwrap().pending_segments.is_empty());

            // The user carried on recording and the app died before that take
            // was written up. The first take's transcript is still good; only
            // the new one is outstanding.
            lane(&dir, "mic.002.wav", 16_000 * 30);
            let m = meeting(id).unwrap();
            assert!(m.transcribed);
            assert_eq!(m.pending_segments, vec![2]);
            assert_eq!(m.duration_secs, 90, "both takes count towards the length");

            record_transcribed_segment(&dir, 2).unwrap();
            assert!(meeting(id).unwrap().pending_segments.is_empty());
        });
    }

    #[test]
    fn adding_audio_marks_existing_notes_out_of_date() {
        with_temp_home(|_| {
            let id = "20260812-110000-kickoff";
            let dir = seed_meeting(id);

            // Nothing written up yet — there is nothing to go stale.
            mark_notes_stale(&dir).unwrap();
            assert!(!meeting(id).unwrap().notes_stale);

            std::fs::write(dir.join("summary.md"), "## Summary\n\nagreed the budget\n").unwrap();
            mark_notes_stale(&dir).unwrap();
            let m = meeting(id).unwrap();
            assert!(m.has_notes);
            assert!(m.notes_stale);

            // Stale means "incomplete", not "thrown away": the cached write-up
            // is still what the note view shows until it is regenerated.
            assert!(write_notes(id, crate::chat::Template::General, false)
                .unwrap()
                .contains("budget"));
        });
    }

    #[test]
    fn note_sources_include_attached_videos() {
        let _guard = crate::settings::HOME_ENV_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join("oatmeal-source-text-home");
        let _ = std::fs::remove_dir_all(&home);
        let dir = home
            .join("Library/Application Support/dev.oatmeal.app/recordings/20260814-090000-ecology");
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("HOME", &home);

        std::fs::write(
            dir.join("transcript.md"),
            "# Ecology\n\n_Recorded 14 Aug_\n\n## Transcript\n\n**[0:05]** we talked about carbon\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("video-1.md"),
            "# Lecture 4\n\n_From https://www.youtube.com/watch?v=abc — 0:00 to 5:00_\n\n## Transcript\n\nthe nitrogen cycle matters\n",
        )
        .unwrap();

        let text = source_text("20260814-090000-ecology").unwrap();
        assert!(text.contains("we talked about carbon"), "meeting words missing: {text}");
        assert!(text.contains("the nitrogen cycle matters"), "video words missing: {text}");
        assert!(!text.contains("youtube.com"), "provenance leaked into the prompt: {text}");
        // The notes prompt asks for "(from video)" on points that appear only in
        // attached material, which it can only honour if the material is marked.
        let (before, after) = text.split_once(VIDEO_DELIMITER).expect("no video delimiter");
        assert!(before.contains("we talked about carbon"), "meeting words after the mark: {text}");
        assert!(after.contains("the nitrogen cycle matters"), "video words before the mark: {text}");

        let _ = std::fs::remove_dir_all(&home);
    }
}
