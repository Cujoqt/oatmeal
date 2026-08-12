//! Locally stored homework/to-do items with a due date. Independent of
//! meetings — no recordings, no transcripts, just a small JSON list beside
//! the rest of the app's local config.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::settings::{restrict_dir, support_root};

fn store_path() -> PathBuf {
    support_root().join("homework.json")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomeworkItem {
    pub id: String,
    pub title: String,
    pub note: String,
    /// `YYYY-MM-DD`.
    pub due_date: String,
    pub done: bool,
}

fn read_all() -> Vec<HomeworkItem> {
    std::fs::read_to_string(store_path())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn write_all(items: &[HomeworkItem]) -> Result<(), String> {
    let path = store_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("create {}: {e}", parent.display()))?;
        restrict_dir(parent);
    }
    let text =
        serde_json::to_string_pretty(items).map_err(|e| format!("serialize homework: {e}"))?;
    crate::store::write(&path, &text)
}

/// `YYYY-MM-DD`, nothing fancier — good enough to sort lexicographically and
/// to catch obviously malformed input from the date picker.
fn validate_date(date: &str) -> Result<(), String> {
    let bytes = date.as_bytes();
    let valid_shape = date.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && date[0..4].bytes().all(|b| b.is_ascii_digit())
        && date[5..7].bytes().all(|b| b.is_ascii_digit())
        && date[8..10].bytes().all(|b| b.is_ascii_digit());
    if !valid_shape {
        return Err(format!("invalid date: {date}"));
    }
    Ok(())
}

fn new_id(existing: &[HomeworkItem]) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let mut id = format!("hw-{millis}");
    while existing.iter().any(|i| i.id == id) {
        id.push('-');
        id.push('1');
    }
    id
}

/// Every homework item, soonest due date first.
pub fn list_homework() -> Vec<HomeworkItem> {
    let mut items = read_all();
    items.sort_by(|a, b| a.due_date.cmp(&b.due_date).then_with(|| a.id.cmp(&b.id)));
    items
}

pub fn add_homework(title: &str, note: &str, due_date: &str) -> Result<HomeworkItem, String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("homework needs a title".into());
    }
    validate_date(due_date)?;

    let mut items = read_all();
    let item = HomeworkItem {
        id: new_id(&items),
        title: title.to_string(),
        note: note.trim().to_string(),
        due_date: due_date.to_string(),
        done: false,
    };
    items.push(item.clone());
    write_all(&items)?;
    Ok(item)
}

pub fn set_homework_done(id: &str, done: bool) -> Result<(), String> {
    let mut items = read_all();
    let item = items
        .iter_mut()
        .find(|i| i.id == id)
        .ok_or_else(|| format!("no such item: {id}"))?;
    item.done = done;
    write_all(&items)
}

pub fn delete_homework(id: &str) -> Result<(), String> {
    let mut items = read_all();
    let before = items.len();
    items.retain(|i| i.id != id);
    if items.len() == before {
        return Err(format!("no such item: {id}"));
    }
    write_all(&items)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `$HOME` is process-global, so this shares `library.rs`'s lock (defined
    /// in `settings.rs`) rather than declaring an independent one — otherwise
    /// the two test suites can race when `cargo test` runs them concurrently.
    use crate::settings::HOME_ENV_LOCK as HOME_LOCK;

    /// Point `$HOME` at a scratch dir for the duration of the closure — same
    /// pattern as `library.rs`'s tests, so homework tests can't collide with a
    /// developer's real config or with `library.rs`'s own tests.
    fn with_temp_home<T>(f: impl FnOnce() -> T) -> T {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = std::env::temp_dir().join(format!(
            "oatmeal-hw-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&home).unwrap();
        let prev = std::env::var("HOME").ok();
        std::env::set_var("HOME", &home);

        let out = f();

        match prev {
            Some(p) => std::env::set_var("HOME", p),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(&home);
        out
    }

    #[test]
    fn added_items_are_listed_by_due_date_ascending() {
        with_temp_home(|| {
            add_homework("Read chapter 4", "", "2026-08-20").unwrap();
            add_homework("Turn in essay", "final draft", "2026-08-15").unwrap();
            let items = list_homework();
            assert_eq!(items.len(), 2);
            assert_eq!(items[0].title, "Turn in essay");
            assert_eq!(items[0].note, "final draft");
            assert_eq!(items[1].title, "Read chapter 4");
        });
    }

    #[test]
    fn add_rejects_blank_title_or_bad_date() {
        with_temp_home(|| {
            assert!(add_homework("   ", "", "2026-08-15").is_err());
            assert!(add_homework("Read", "", "not-a-date").is_err());
            assert!(add_homework("Read", "", "2026-8-15").is_err(), "must be zero-padded");
        });
    }

    #[test]
    fn done_can_be_toggled_and_items_deleted() {
        with_temp_home(|| {
            let item = add_homework("Read chapter 4", "", "2026-08-15").unwrap();
            set_homework_done(&item.id, true).unwrap();
            assert!(list_homework()[0].done);

            delete_homework(&item.id).unwrap();
            assert!(list_homework().is_empty());
            assert!(delete_homework(&item.id).is_err(), "already gone");
        });
    }

    #[test]
    fn unknown_id_is_an_error() {
        with_temp_home(|| {
            assert!(set_homework_done("hw-nonexistent", true).is_err());
        });
    }
}
