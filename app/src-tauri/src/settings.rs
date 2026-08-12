//! On-disk settings.
//!
//! One JSON file under the app-support root. Unknown keys are preserved on save,
//! so a hand-edited config (or a key an older build wrote) survives a round-trip
//! through the Settings tab.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Directory holding the config and the Google tokens.
pub fn support_root() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join("Library/Application Support/dev.oatmeal.app")
}

/// Serializes any test that points `$HOME` at a scratch directory (`library.rs`
/// and `homework.rs` both do this). `$HOME` is process-global, so without a
/// shared lock two such tests running on different threads — as `cargo test`
/// does by default — can race and read/write each other's scratch dir.
#[cfg(test)]
pub(crate) static HOME_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn config_path() -> PathBuf {
    support_root().join("config.json")
}

/// The raw config document, as stored.
pub fn read_raw() -> serde_json::Value {
    if let Ok(raw) = std::fs::read_to_string(config_path()) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&raw) {
            if json.is_object() {
                return json;
            }
        }
    }
    serde_json::json!({})
}

fn str_field(doc: &serde_json::Value, key: &str) -> String {
    doc.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string()
}

/// The settings the UI edits.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    /// What the app calls you — used in the greeting and on your own notes.
    pub display_name: String,
    /// Whisper language code; empty means auto-detect.
    pub language: String,
}

pub fn load() -> Settings {
    let doc = read_raw();
    Settings {
        display_name: str_field(&doc, "displayName"),
        language: str_field(&doc, "language"),
    }
}

/// Save the editable fields.
pub fn save(display_name: &str, language: &str) -> Result<Settings, String> {
    let mut doc = read_raw();
    let obj = doc
        .as_object_mut()
        .ok_or("config file is not a JSON object")?;
    obj.insert("displayName".into(), display_name.trim().into());
    obj.insert("language".into(), language.trim().into());
    write(&doc)?;
    Ok(load())
}

/// Write the config back, owner-only.
fn write(doc: &serde_json::Value) -> Result<(), String> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
        restrict_dir(parent);
    }
    let text = serde_json::to_string_pretty(doc).map_err(|e| format!("serialize config: {e}"))?;
    std::fs::write(&path, text).map_err(|e| format!("write {}: {e}", path.display()))?;
    restrict(&path);
    Ok(())
}

/// The config and the token file hold credentials — keep them owner-only, and
/// the directory that holds them closed to other accounts on the machine.
pub fn restrict(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
}

/// `0700` on the support directory, so another account on this Mac can't list
/// recordings or read the token file's name, let alone its contents.
pub fn restrict_dir(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
    }
}
