//! On-disk settings shared by the UI and the Google module.
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

/// The settings the UI edits. The client secret never reaches the webview — the
/// UI only learns whether one is stored.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub google_client_id: String,
    pub google_client_secret_set: bool,
    /// Whisper language code; empty means auto-detect.
    pub language: String,
}

pub fn load() -> Settings {
    let doc = read_raw();
    Settings {
        google_client_id: str_field(&doc, "googleClientId"),
        google_client_secret_set: !str_field(&doc, "googleClientSecret").is_empty(),
        language: str_field(&doc, "language"),
    }
}

pub fn client_id() -> Option<String> {
    let id = str_field(&read_raw(), "googleClientId");
    (!id.is_empty()).then_some(id)
}

pub fn client_secret() -> Option<String> {
    let secret = str_field(&read_raw(), "googleClientSecret");
    (!secret.is_empty()).then_some(secret)
}

/// Save the editable fields. A blank `google_client_secret` leaves whatever is
/// already stored alone; the UI sends a literal `"-"` to clear it.
pub fn save(
    google_client_id: &str,
    google_client_secret: &str,
    language: &str,
) -> Result<Settings, String> {
    let mut doc = read_raw();
    let obj = doc
        .as_object_mut()
        .ok_or("config file is not a JSON object")?;

    obj.insert("googleClientId".into(), google_client_id.trim().into());
    obj.insert("language".into(), language.trim().into());
    match google_client_secret.trim() {
        "" => {}
        "-" => {
            obj.remove("googleClientSecret");
        }
        secret => {
            obj.insert("googleClientSecret".into(), secret.into());
        }
    }

    write(&doc)?;
    Ok(load())
}

/// Store just the OAuth client pair, leaving every other setting alone. The
/// guided sign-in path uses this: the user never sees the two fields.
pub fn save_client(client_id: &str, client_secret: &str) -> Result<Settings, String> {
    let client_id = client_id.trim();
    let client_secret = client_secret.trim();
    if client_id.is_empty() || client_secret.is_empty() {
        return Err("that file didn't contain a client ID and secret".into());
    }
    let mut doc = read_raw();
    let obj = doc
        .as_object_mut()
        .ok_or("config file is not a JSON object")?;
    obj.insert("googleClientId".into(), client_id.into());
    obj.insert("googleClientSecret".into(), client_secret.into());
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

/// A secret that is overwritten when it goes out of scope.
pub struct Secret(pub String);

impl Drop for Secret {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.0.zeroize();
    }
}
