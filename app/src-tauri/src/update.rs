//! Update checks against GitHub Releases.
//!
//! There is no update server. The repository's own releases API is the manifest,
//! and the DMG already attached to each release is the download — so publishing
//! an update is exactly what it is today: push a tag.
//!
//! A release becomes *mandatory* by putting a line in its notes:
//!
//! ```text
//! Oatmeal-Minimum-Version: 1.3.0
//! ```
//!
//! Anyone below that is asked to update before they can keep using the app. With
//! no such line, a newer release is only ever a suggestion.
//!
//! The check runs here rather than in the webview on purpose: the CSP allows the
//! frontend to talk to nothing but the IPC bridge, and widening it to reach
//! github.com would be a real loosening of the app's security posture for a
//! request Rust can make just as easily. Follows `model.rs` in shelling out to
//! `curl` rather than pulling in an HTTP stack.

use std::process::Command;

use serde::Serialize;

/// Releases API for the repository the app ships from.
const RELEASES_LATEST: &str = "https://api.github.com/repos/Cujoqt/oatmeal/releases/latest";

/// Only URLs under this prefix are ever handed to `open`, so a surprising API
/// response can't turn into "Oatmeal opened something else".
const REPO_PREFIX: &str = "https://github.com/Cujoqt/oatmeal/";

/// Opt-in marker in a release's notes that makes the release mandatory.
const MIN_VERSION_MARKER: &str = "oatmeal-minimum-version:";

/// The version this build was compiled as — the same string as `tauri.conf.json`'s.
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// What the UI needs to decide between saying nothing, nagging, and blocking.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    pub current: String,
    pub latest: Option<String>,
    /// A release newer than this build exists.
    pub update_available: bool,
    /// This build is older than a published minimum — the UI blocks on this.
    pub mandatory: bool,
    pub minimum: Option<String>,
    pub release_url: Option<String>,
    pub download_url: Option<String>,
    /// False when GitHub could not be reached. `mandatory` is never true then:
    /// failing to check must not lock someone out of recording a meeting.
    pub checked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Numeric release ordering. Accepts `v1.2.0`, `1.2`, `1.2.0-rc.1`; pre-release
/// and build suffixes are ignored, which is enough for comparing our own tags.
fn parse_version(raw: &str) -> Option<(u32, u32, u32)> {
    let s = raw.trim();
    let s = s.strip_prefix('v').or_else(|| s.strip_prefix('V')).unwrap_or(s);
    // Cut anything that isn't part of the dotted numbers.
    let core = s
        .split(|c: char| c == '-' || c == '+' || c == ' ')
        .next()
        .unwrap_or("");
    if core.is_empty() {
        return None;
    }
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    // A missing minor/patch is zero, so `1.2` compares against `1.2.0`.
    let minor = match parts.next() {
        Some(p) => p.parse().ok()?,
        None => 0,
    };
    let patch = match parts.next() {
        Some(p) => p.parse().ok()?,
        None => 0,
    };
    Some((major, minor, patch))
}

/// True when `candidate` is a strictly higher version than `current`. Unparseable
/// input is never "newer" — a malformed tag must not trigger a blocking prompt.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(c), Some(now)) => c > now,
        _ => false,
    }
}

/// Pull the mandatory-minimum out of a release body, if the author put one there.
///
/// The directive has to be the *whole* line — a line that merely mentions the
/// marker in prose is not one. That matters more than it looks: the release notes
/// announcing this feature will naturally contain a sentence like "mark a release
/// required with `Oatmeal-Minimum-Version: 1.3.0`", and matching that would gate
/// every user on a release nobody meant to make mandatory.
fn minimum_from_notes(body: &str) -> Option<String> {
    for line in body.lines() {
        // Strip list bullets, quote markers and emphasis from both ends so a
        // decorated standalone line still counts as standalone.
        let bare = line.trim_matches(|c: char| {
            c.is_whitespace() || matches!(c, '#' | '*' | '-' | '_' | '>' | '`')
        });
        let lowered = bare.to_ascii_lowercase();
        // Must *start* with the marker, not merely contain it.
        if !lowered.starts_with(MIN_VERSION_MARKER) {
            continue;
        }
        let value = bare[MIN_VERSION_MARKER.len()..]
            .trim_matches(|c: char| c.is_whitespace() || c == '`' || c == '*' || c == '"');
        // The remainder must be nothing but the version. Anything else is prose,
        // and a mangled value fails open (no gate) rather than locking everyone
        // out of the app.
        if !value.is_empty() && value.split(|c: char| c.is_whitespace()).count() == 1 {
            if parse_version(value).is_some() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// Build a status from an already-fetched release document. Split out from the
/// network call so the decision logic is testable without GitHub.
fn status_from_release(doc: &serde_json::Value, current: &str) -> UpdateStatus {
    let latest = doc
        .get("tag_name")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string());
    let body = doc.get("body").and_then(|v| v.as_str()).unwrap_or("");
    let minimum = minimum_from_notes(body);

    let release_url = doc
        .get("html_url")
        .and_then(|v| v.as_str())
        .filter(|u| u.starts_with(REPO_PREFIX))
        .map(|s| s.to_string());

    // The DMG attached to the release — what "download the update" opens.
    let download_url = doc
        .get("assets")
        .and_then(|v| v.as_array())
        .and_then(|assets| {
            assets
                .iter()
                .filter_map(|a| a.get("browser_download_url").and_then(|v| v.as_str()))
                .find(|u| u.ends_with(".dmg") && u.starts_with(REPO_PREFIX))
        })
        .map(|s| s.to_string());

    UpdateStatus {
        current: current.to_string(),
        update_available: latest.as_deref().map(|l| is_newer(l, current)).unwrap_or(false),
        mandatory: minimum.as_deref().map(|m| is_newer(m, current)).unwrap_or(false),
        latest,
        minimum,
        release_url,
        download_url,
        checked: true,
        error: None,
    }
}

/// Ask GitHub for the newest release. Never returns an error: a failed check is
/// reported as "not checked" so the UI can stay quiet rather than block.
pub fn check() -> UpdateStatus {
    check_with(fetch_latest_release)
}

/// `check` with the network call injected, so the fail-open paths that matter
/// most — unreachable, and unparseable — are testable without GitHub.
fn check_with(fetch: impl FnOnce() -> Result<Vec<u8>, String>) -> UpdateStatus {
    let unchecked = |error: String| UpdateStatus {
        current: current_version().to_string(),
        checked: false,
        error: Some(error),
        ..Default::default()
    };

    let body = match fetch() {
        Ok(b) => b,
        Err(e) => return unchecked(e),
    };

    match serde_json::from_slice::<serde_json::Value>(&body) {
        Ok(doc) => status_from_release(&doc, current_version()),
        Err(e) => unchecked(format!("could not read the release list: {e}")),
    }
}

fn fetch_latest_release() -> Result<Vec<u8>, String> {
    let out = Command::new("curl")
        .arg("-fsSL")
        .arg("--max-time")
        .arg("10")
        // The API rejects requests without a User-Agent.
        .arg("-H")
        .arg("User-Agent: Oatmeal")
        .arg("-H")
        .arg("Accept: application/vnd.github+json")
        .arg(RELEASES_LATEST)
        .output();

    match out {
        Ok(o) if o.status.success() => Ok(o.stdout),
        Ok(o) => Err(format!(
            "could not reach GitHub (curl exited {})",
            o.status.code().unwrap_or(-1)
        )),
        Err(e) => Err(format!("could not run curl: {e}")),
    }
}

/// Open a release page or DMG in the browser. Refuses anything outside the
/// project's own repository.
pub fn open_download(url: &str) -> Result<(), String> {
    if !url.starts_with(REPO_PREFIX) {
        return Err("refusing to open a link outside the Oatmeal repository".into());
    }
    Command::new("open")
        .arg(url)
        .status()
        .map_err(|e| format!("open {url}: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically_not_lexically() {
        // The case a string compare gets wrong.
        assert!(is_newer("1.10.0", "1.9.0"));
        assert!(!is_newer("1.9.0", "1.10.0"));
        assert!(is_newer("v1.3.0", "1.2.0"));
        assert!(is_newer("2.0.0", "1.99.99"));
        assert!(!is_newer("1.2.0", "1.2.0"));
        // A shorter tag is padded with zeros, not treated as smaller.
        assert!(!is_newer("1.2", "1.2.0"));
        assert!(is_newer("1.2.1", "1.2"));
    }

    #[test]
    fn unparseable_versions_are_never_newer() {
        // Otherwise a junk tag could put every user behind a blocking modal.
        assert!(!is_newer("not-a-version", "1.2.0"));
        assert!(!is_newer("", "1.2.0"));
        assert!(!is_newer("1.3.0", "who knows"));
    }

    #[test]
    fn a_release_is_only_mandatory_when_its_notes_say_so() {
        let plain = serde_json::json!({
            "tag_name": "v1.3.0",
            "body": "## Install\n\nDrag it to Applications.",
            "html_url": "https://github.com/Cujoqt/oatmeal/releases/tag/v1.3.0",
        });
        let s = status_from_release(&plain, "1.2.0");
        assert!(s.update_available, "1.3.0 is newer than 1.2.0");
        assert!(!s.mandatory, "no marker means no gate");
        assert_eq!(s.minimum, None);
    }

    #[test]
    fn the_marker_makes_older_builds_mandatory() {
        let doc = serde_json::json!({
            "tag_name": "v1.3.0",
            "body": "## Notes\n\n- **Oatmeal-Minimum-Version:** `1.3.0`\n\nFixes things.",
        });
        let blocked = status_from_release(&doc, "1.2.0");
        assert_eq!(blocked.minimum.as_deref(), Some("1.3.0"));
        assert!(blocked.mandatory, "1.2.0 is below the published minimum");

        // Someone already on the minimum is not blocked by it.
        let fine = status_from_release(&doc, "1.3.0");
        assert!(!fine.mandatory);
        assert!(!fine.update_available);
    }

    #[test]
    fn a_mangled_marker_fails_open() {
        let doc = serde_json::json!({
            "tag_name": "v1.3.0",
            "body": "Oatmeal-Minimum-Version: soon\n",
        });
        let s = status_from_release(&doc, "1.2.0");
        assert_eq!(s.minimum, None, "unparseable minimum must be ignored");
        assert!(!s.mandatory, "a typo must not lock anyone out");
    }

    #[test]
    fn only_repository_urls_are_offered_or_opened() {
        let doc = serde_json::json!({
            "tag_name": "v1.3.0",
            "html_url": "https://evil.example/releases/tag/v1.3.0",
            "assets": [
                { "browser_download_url": "https://evil.example/Oatmeal.dmg" },
                { "browser_download_url": "https://github.com/Cujoqt/oatmeal/releases/download/v1.3.0/Oatmeal-1.3.0-apple-silicon.dmg" }
            ],
        });
        let s = status_from_release(&doc, "1.2.0");
        assert_eq!(s.release_url, None, "off-repo release page dropped");
        assert!(s.download_url.unwrap().starts_with(REPO_PREFIX));
        assert!(open_download("https://evil.example/x.dmg").is_err());
    }

    /// Hits the real releases API. Ignored by default like the audio end-to-end
    /// test, since it needs the network:
    /// `cargo test --lib update::tests::live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_check_reaches_github_and_parses_the_release() {
        let s = check();
        println!("{}", serde_json::to_string_pretty(&s).unwrap());
        assert!(s.checked, "check failed: {:?}", s.error);
        let latest = s.latest.expect("a published release should have a tag");
        assert!(
            parse_version(&latest).is_some(),
            "tag {latest} did not parse as a version"
        );
        // This build is the newest one published, so there is nothing to install
        // and certainly nothing mandatory.
        assert!(!s.update_available, "unexpected newer release {latest}");
        assert!(!s.mandatory);
        assert!(
            s.download_url.unwrap_or_default().ends_with(".dmg"),
            "the release should still attach a DMG to download"
        );
    }

    /// The guarantee the whole design rests on: if we cannot reach GitHub, the app
    /// reports "unknown" — never "you are current", never "you must update". Runs
    /// `check_with` for real rather than hand-building a status, so it would catch
    /// the failure path being rewritten to block.
    #[test]
    fn an_unreachable_check_never_blocks() {
        let s = check_with(|| Err("curl exited 6".into()));
        assert!(!s.checked, "a failed fetch must not look checked");
        assert!(!s.mandatory, "being offline must never gate the app");
        assert!(!s.update_available);
        assert_eq!(s.latest, None);
        assert_eq!(s.error.as_deref(), Some("curl exited 6"));
        assert_eq!(s.current, current_version());
    }

    /// Same guarantee for a reply that arrives but isn't the JSON we expect — a
    /// captive-portal login page, or a rate-limit body.
    #[test]
    fn a_reply_that_is_not_a_release_never_blocks() {
        for body in [
            &b"<html>sign in to continue</html>"[..],
            &b""[..],
            &b"{\"message\":\"API rate limit exceeded\""[..],
        ] {
            let s = check_with(|| Ok(body.to_vec()));
            assert!(!s.checked, "unparseable body should not look checked: {body:?}");
            assert!(!s.mandatory);
            assert!(!s.update_available);
        }
    }

    /// A well-formed reply with no releases at all must also stay quiet.
    #[test]
    fn an_empty_release_document_never_blocks() {
        let s = check_with(|| Ok(b"{}".to_vec()));
        assert!(s.checked, "valid JSON did parse");
        assert_eq!(s.latest, None);
        assert!(!s.update_available);
        assert!(!s.mandatory);
    }

    /// The release notes announcing this very feature will mention the marker in a
    /// sentence. That must not gate anybody.
    #[test]
    fn the_marker_mentioned_in_prose_does_not_gate() {
        for body in [
            "Mark a release required with `Oatmeal-Minimum-Version: 1.3.0` in its notes.",
            "- You can now set Oatmeal-Minimum-Version: 9.9.9 to force an update",
            "See the docs for Oatmeal-Minimum-Version: 1.3.0 and friends",
        ] {
            let doc = serde_json::json!({ "tag_name": "v1.3.0", "body": body });
            let s = status_from_release(&doc, "1.2.0");
            assert_eq!(s.minimum, None, "prose should not be read as a directive: {body}");
            assert!(!s.mandatory, "prose must not gate the app: {body}");
        }
    }

    /// ...while the directive on a line of its own still does, decorated or not.
    #[test]
    fn the_marker_on_its_own_line_still_gates() {
        for body in [
            "Oatmeal-Minimum-Version: 1.3.0",
            "## Notes\n\nOatmeal-Minimum-Version: 1.3.0\n\nFixes things.",
            "- **Oatmeal-Minimum-Version:** `1.3.0`",
            "> Oatmeal-Minimum-Version: v1.3.0",
        ] {
            let doc = serde_json::json!({ "tag_name": "v1.3.0", "body": body });
            let s = status_from_release(&doc, "1.2.0");
            assert!(s.minimum.is_some(), "should have found a minimum in: {body}");
            assert!(s.mandatory, "should gate 1.2.0: {body}");
        }
    }
}
