//! Durability for the files Oatmeal keeps on disk.
//!
//! Two jobs, both about never losing a meeting to an app update:
//!
//! 1. **Atomic replace.** A bare `fs::write` truncates the file and *then*
//!    writes. If the process dies in between — a crash, a panic, or the app
//!    being quit to install a new version — what's left on disk is a truncated
//!    `notes.md`. Writing a sibling temp file, flushing it to the platter, and
//!    renaming it over the target means a reader only ever sees the whole old
//!    file or the whole new one.
//!
//! 2. **A version stamp.** Recordings live under the app-support root, entirely
//!    outside the `.app` bundle, so replacing the bundle can't touch them. The
//!    danger is the *other* direction: running an older build against data a
//!    newer one wrote, which would quietly rewrite it in the older shape. When
//!    that's detected every write is refused instead.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::settings::{restrict, restrict_dir, support_root};

/// Schema version of everything under the support root. Bump only when a build
/// can no longer read what its predecessor wrote — and add the migration that
/// makes the old shape readable before you do.
pub const DATA_VERSION: u32 = 1;

/// Flipped when `prepare` finds data from a newer build than this one. Every
/// write funnels through `write`, so one flag is enough to protect all of them.
static WRITES_LOCKED: AtomicBool = AtomicBool::new(false);

/// Makes temp names unique between threads — the record path is `(async)` now,
/// so two writes really can be in flight at once.
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Whether writing user data is currently refused (data is from a newer build).
pub fn writes_locked() -> bool {
    WRITES_LOCKED.load(Ordering::SeqCst)
}

pub fn version_path() -> PathBuf {
    support_root().join("data-version.json")
}

/// What `prepare` found on disk at startup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum Compatibility {
    /// Nothing on disk yet — first run.
    Fresh,
    /// Stamped with exactly this build's version.
    Current,
    /// Data from an older build, now stamped forward. `backup` is where the old
    /// documents were copied, when there were any to copy.
    Migrated { from: u32, backup: Option<String> },
    /// Data from a *newer* build. Writes are refused; the app needs updating.
    TooNew { found: u32 },
}

#[derive(Serialize, Deserialize)]
struct Stamp {
    version: u32,
    /// Informational: which app version wrote the stamp.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    written_by: String,
}

/// The version recorded on disk, or `None` if there is no stamp.
pub fn stored_version() -> Option<u32> {
    let text = std::fs::read_to_string(version_path()).ok()?;
    serde_json::from_str::<Stamp>(&text).ok().map(|s| s.version)
}

/// True when the support root already holds real user data. Distinguishes a
/// genuinely fresh install from one that predates the version stamp.
fn has_existing_data() -> bool {
    let root = support_root();
    ["recordings", "config.json", "homework.json"]
        .iter()
        .any(|name| root.join(name).exists())
}

/// Reconcile this build against whatever is on disk. Call once at startup,
/// before anything reads or writes user data.
pub fn prepare() -> Compatibility {
    let outcome = match stored_version() {
        Some(found) if found > DATA_VERSION => {
            // Do not stamp, do not migrate, do not write. Downgrading is the one
            // case where being helpful destroys data.
            WRITES_LOCKED.store(true, Ordering::SeqCst);
            eprintln!(
                "[oatmeal] data on disk is version {found}, this build understands \
                 {DATA_VERSION} — refusing to write until Oatmeal is updated"
            );
            return Compatibility::TooNew { found };
        }
        Some(found) if found == DATA_VERSION => Compatibility::Current,
        Some(found) => {
            let backup = match back_up_documents(found) {
                Ok(dir) => dir,
                Err(e) => {
                    // A failed backup must not become a failed launch, but it is
                    // worth saying out loud.
                    eprintln!("[oatmeal] could not back up v{found} data: {e}");
                    None
                }
            };
            Compatibility::Migrated { from: found, backup }
        }
        None if has_existing_data() => {
            // Pre-stamp install. Its layout *is* v1, so adopt it rather than
            // treating it as a migration.
            Compatibility::Current
        }
        None => Compatibility::Fresh,
    };

    if let Err(e) = stamp() {
        eprintln!("[oatmeal] could not write the data version stamp: {e}");
    }
    outcome
}

/// Record this build's schema version.
pub fn stamp() -> Result<(), String> {
    let doc = Stamp {
        version: DATA_VERSION,
        written_by: env!("CARGO_PKG_VERSION").to_string(),
    };
    let text = serde_json::to_string_pretty(&doc)
        .map_err(|e| format!("serialize data version: {e}"))?;
    write(&version_path(), &text)
}

/// Copy the small JSON documents aside before a migration rewrites them.
///
/// Recordings are deliberately not copied: each meeting is its own folder,
/// written once and never bulk-rewritten, and duplicating the audio would cost
/// gigabytes to guard against a risk that doesn't apply to it.
fn back_up_documents(from: u32) -> Result<Option<String>, String> {
    let root = support_root();
    let docs: Vec<PathBuf> = ["config.json", "homework.json"]
        .iter()
        .map(|n| root.join(n))
        .filter(|p| p.exists())
        .collect();
    if docs.is_empty() {
        return Ok(None);
    }

    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dir = root.join("backups").join(format!("v{from}-{secs}"));
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    restrict_dir(&dir);
    for doc in &docs {
        let name = doc.file_name().ok_or("backup source has no file name")?;
        std::fs::copy(doc, dir.join(name))
            .map_err(|e| format!("copy {}: {e}", doc.display()))?;
    }
    Ok(Some(dir.display().to_string()))
}

/// Replace `path` with `contents`, atomically.
///
/// The temp file is a sibling, not in `/tmp`: `rename` is only atomic within one
/// filesystem, and the support root can sit on a different volume than `$TMPDIR`.
pub fn write(path: &Path, contents: &str) -> Result<(), String> {
    if writes_locked() {
        return Err(format!(
            "this data was written by a newer version of Oatmeal — update to keep \
             editing it. Nothing was changed ({}).",
            path.display()
        ));
    }

    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;

    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("{} has no file name", path.display()))?;
    let tmp = parent.join(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        TMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));

    // Scoped so the handle is closed before the rename.
    let flush = |tmp: &Path| -> std::io::Result<()> {
        let mut f = std::fs::File::create(tmp)?;
        f.write_all(contents.as_bytes())?;
        // Durable *before* the rename, otherwise a crash can leave the new name
        // pointing at unwritten blocks.
        f.sync_all()
    };
    if let Err(e) = flush(&tmp) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("write {}: {e}", tmp.display()));
    }

    // Take the owner-only mode while it is still the temp name, so the file is
    // never briefly world-readable under its real one.
    restrict(&tmp);

    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("replace {}: {e}", path.display()));
    }

    // Best effort: makes the rename itself survive a power loss. Not all
    // filesystems allow opening a directory, so a failure here is not fatal.
    if let Ok(dir) = std::fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `$HOME` is process-global — shares the lock in `settings.rs` for the same
    /// reason `homework.rs` and `library.rs` do.
    use crate::settings::HOME_ENV_LOCK as HOME_LOCK;

    fn with_temp_home<T>(f: impl FnOnce() -> T) -> T {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        WRITES_LOCKED.store(false, Ordering::SeqCst);
        let home = std::env::temp_dir().join(format!(
            "oatmeal-store-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let prev = std::env::var("HOME").ok();
        std::env::set_var("HOME", &home);

        let out = f();

        match prev {
            Some(p) => std::env::set_var("HOME", p),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(&home);
        WRITES_LOCKED.store(false, Ordering::SeqCst);
        out
    }

    #[test]
    fn write_replaces_the_file_and_leaves_no_temp_behind() {
        with_temp_home(|| {
            let path = support_root().join("recordings/20260812-1200-standup/notes.md");
            write(&path, "# Standup\n\nfirst\n").unwrap();
            write(&path, "# Standup\n\nsecond\n").unwrap();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "# Standup\n\nsecond\n");

            let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
                .unwrap()
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| n.ends_with(".tmp"))
                .collect();
            assert!(leftovers.is_empty(), "temp files left behind: {leftovers:?}");
        })
    }

    /// The point of the exercise: a failed write must not destroy what was there.
    /// A read-only parent makes creating the temp file fail at exactly the moment
    /// a bare `fs::write` would already have truncated the original. (Testing the
    /// directory's mode rather than a predicted temp name keeps this from racing
    /// other suites that now also write through `store::write`.)
    #[test]
    fn a_failed_write_leaves_the_previous_contents_intact() {
        use std::os::unix::fs::PermissionsExt;

        with_temp_home(|| {
            let path = support_root().join("config.json");
            let original = "{\"displayName\":\"Dylan\"}";
            write(&path, original).unwrap();

            let parent = path.parent().unwrap().to_path_buf();
            std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o500)).unwrap();

            let err = write(&path, "clobbered").unwrap_err();

            // Restore before asserting, so a failure still leaves a removable dir.
            std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();

            assert!(err.contains("config.json"), "unhelpful error: {err}");
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                original,
                "a failed write must leave the previous notes untouched"
            );
        })
    }

    #[test]
    fn a_fresh_install_is_stamped_with_this_version() {
        with_temp_home(|| {
            assert_eq!(prepare(), Compatibility::Fresh);
            assert_eq!(stored_version(), Some(DATA_VERSION));
            // Second launch sees its own stamp.
            assert_eq!(prepare(), Compatibility::Current);
        })
    }

    /// Data written before the stamp existed is v1 by definition — adopt it,
    /// don't treat it as a migration and don't refuse it.
    #[test]
    fn data_predating_the_stamp_is_adopted_as_current() {
        with_temp_home(|| {
            let root = support_root();
            std::fs::create_dir_all(root.join("recordings")).unwrap();
            assert_eq!(prepare(), Compatibility::Current);
            assert_eq!(stored_version(), Some(DATA_VERSION));
        })
    }

    #[test]
    fn older_data_is_backed_up_then_stamped_forward() {
        with_temp_home(|| {
            let root = support_root();
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(root.join("config.json"), "{\"displayName\":\"Dylan\"}").unwrap();
            std::fs::write(
                version_path(),
                serde_json::json!({ "version": 0 }).to_string(),
            )
            .unwrap();

            let outcome = prepare();
            let Compatibility::Migrated { from, backup } = outcome else {
                panic!("expected a migration, got {outcome:?}");
            };
            assert_eq!(from, 0);
            let backup = backup.expect("config.json should have been copied aside");
            assert_eq!(
                std::fs::read_to_string(PathBuf::from(backup).join("config.json")).unwrap(),
                "{\"displayName\":\"Dylan\"}"
            );
            assert_eq!(stored_version(), Some(DATA_VERSION));
        })
    }

    /// The case that protects a meeting: an older build must not rewrite newer
    /// data in its own shape.
    #[test]
    fn newer_data_locks_writes_and_keeps_the_stamp() {
        with_temp_home(|| {
            let root = support_root();
            std::fs::create_dir_all(&root).unwrap();
            let future = DATA_VERSION + 7;
            std::fs::write(
                version_path(),
                serde_json::json!({ "version": future }).to_string(),
            )
            .unwrap();
            std::fs::write(root.join("homework.json"), "[]").unwrap();

            assert_eq!(prepare(), Compatibility::TooNew { found: future });
            assert!(writes_locked());

            // Every write refuses, and says why.
            let err = write(&root.join("homework.json"), "[]").unwrap_err();
            assert!(err.contains("newer version"), "unhelpful error: {err}");
            assert_eq!(std::fs::read_to_string(root.join("homework.json")).unwrap(), "[]");
            // The newer stamp is left exactly as it was.
            assert_eq!(stored_version(), Some(future));
        })
    }
}
