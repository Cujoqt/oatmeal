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

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

/// Releases API for the repository the app ships from.
const RELEASES_LATEST: &str = "https://api.github.com/repos/Cujoqt/oatmeal/releases/latest";

/// Releases API for one tag — how the release-notes page finds the notes that
/// belong to the build actually running, rather than the newest published one.
const RELEASES_BY_TAG: &str = "https://api.github.com/repos/Cujoqt/oatmeal/releases/tags/";

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

// ── Release notes for the running build ──────────────────────────────────────

/// The notes GitHub holds for one published release. `body` is the Markdown the
/// release was published with, rendered by the frontend's own renderer.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseNotes {
    pub tag: String,
    /// The release title. Falls back to the tag when a release was published
    /// without one, so the page never shows an empty heading.
    pub name: String,
    pub body: String,
    /// The release page, only when GitHub gave one inside this repository.
    pub release_url: Option<String>,
}

/// Pull the notes out of a release document fetched by tag. Split from the
/// network call so the shape handling is testable without GitHub.
fn notes_from_release(doc: &serde_json::Value, tag: &str) -> ReleaseNotes {
    let name = doc
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(tag)
        .to_string();
    ReleaseNotes {
        tag: doc
            .get("tag_name")
            .and_then(|v| v.as_str())
            .unwrap_or(tag)
            .to_string(),
        name,
        body: doc
            .get("body")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string(),
        release_url: doc
            .get("html_url")
            .and_then(|v| v.as_str())
            .filter(|u| u.starts_with(REPO_PREFIX))
            .map(|s| s.to_string()),
    }
}

/// The notes for the release this build was compiled as. Unlike `check`, this
/// one reports its failures: the page exists because someone asked to read the
/// notes, so "GitHub is unreachable" is the answer, not silence.
pub fn notes() -> Result<ReleaseNotes, String> {
    notes_with(current_version(), fetch_release_by_tag)
}

fn notes_with(
    version: &str,
    fetch: impl FnOnce(&str) -> Result<Vec<u8>, String>,
) -> Result<ReleaseNotes, String> {
    let tag = format!("v{version}");
    let body = fetch(&tag)?;
    let doc: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| format!("could not read the release: {e}"))?;
    // A tag with no release comes back as a `message` document, not a release.
    if doc.get("tag_name").is_none() {
        return Err(format!("GitHub has no published release for {tag}"));
    }
    Ok(notes_from_release(&doc, &tag))
}

fn fetch_release_by_tag(tag: &str) -> Result<Vec<u8>, String> {
    // The tag is built from the compiled-in version, so there is nothing
    // user-supplied in this URL.
    let url = format!("{RELEASES_BY_TAG}{tag}");
    let out = Command::new("curl")
        .arg("-fsSL")
        .arg("--max-time")
        .arg("10")
        .arg("-H")
        .arg("User-Agent: Oatmeal")
        .arg("-H")
        .arg("Accept: application/vnd.github+json")
        .arg(&url)
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

// ── Installing the update in place ───────────────────────────────────────────
//
// Handing someone a DMG to mount and drag is the step where an update stops
// happening. Fetching it here instead also settles the Gatekeeper prompt that
// greeted every update: `com.apple.quarantine` is stamped on by whatever
// downloads a file, and `curl` — unlike a browser — does not stamp it.
//
// What it cannot fix is the permission dialogs. macOS keys those grants to the
// code signature, and Oatmeal is ad-hoc signed (`signingIdentity: "-"`), so
// every build is a stranger to TCC no matter how it arrives. That needs a
// Developer ID certificate and notarization, which is a release-pipeline
// change, not something the app can do to itself.

/// Everything is staged while the app is still up, so a failure is a message on
/// screen rather than a half-replaced bundle. Only the swap itself has to wait
/// for the process to be gone, and that is all this script does.
const SWAP_SCRIPT: &str = r#"#!/bin/sh
# $1 pid to wait for  $2 installed bundle  $3 staged bundle  $4 scratch path for
# the old bundle  $5 the downloaded disk image
while kill -0 "$1" 2>/dev/null; do sleep 0.2; done
sleep 0.5
rm -rf "$4"
mv "$2" "$4" || exit 1
# Put the old one back if the new one won't move into place: better a stale
# Oatmeal than no Oatmeal.
if ! mv "$3" "$2"; then mv "$4" "$2"; exit 1; fi
rm -rf "$4"
open "$2"
rm -f "$5"
"#;

/// The `.app` an executable is running out of. `current_exe` lands on
/// `Oatmeal.app/Contents/MacOS/oatmeal-app`, so the bundle is three levels up.
/// A binary running outside a bundle — `cargo run`, or a test — has nothing to
/// replace, and says so rather than guessing at a path to delete.
fn bundle_of(exe: &Path) -> Option<&Path> {
    exe.ancestors()
        .nth(3)
        .filter(|p| p.extension().map_or(false, |e| e == "app"))
}

/// Can we replace the bundle without asking for an admin password? Probing is
/// the only honest answer — directory permission bits don't account for ACLs or
/// a read-only volume. Not user data, so a plain write is right here.
fn writable(dir: &Path) -> bool {
    let probe = dir.join(".oatmeal-write-probe");
    match fs::write(&probe, b"") {
        Ok(()) => {
            let _ = fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

fn run(cmd: &mut Command, what: &str) -> Result<(), String> {
    match cmd.output() {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => {
            let why = String::from_utf8_lossy(&o.stderr);
            let why = why.trim();
            Err(if why.is_empty() {
                format!("{what} failed")
            } else {
                format!("{what} failed: {why}")
            })
        }
        Err(e) => Err(format!("could not run {what}: {e}")),
    }
}

/// The `.app` inside a mounted release image. Found by looking rather than by
/// name so renaming the product doesn't silently break updating.
fn app_in(mount: &Path) -> Result<PathBuf, String> {
    fs::read_dir(mount)
        .map_err(|e| format!("could not read the disk image: {e}"))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.extension().map_or(false, |e| e == "app"))
        .ok_or_else(|| "the disk image has no application in it".into())
}

/// Download the release image and stage the new bundle beside the installed
/// one, then hand the swap to a detached script that waits for this process to
/// exit. Returns once the swap is armed — the caller quits the app, and the
/// script reopens it.
pub fn install(url: &str) -> Result<(), String> {
    if !url.starts_with(REPO_PREFIX) || !url.ends_with(".dmg") {
        return Err("refusing to install anything but a disk image from the Oatmeal repository".into());
    }

    let exe = std::env::current_exe().map_err(|e| format!("could not find the running app: {e}"))?;
    let bundle = bundle_of(&exe)
        .ok_or("Oatmeal isn't running from an .app bundle, so it can't replace itself")?
        .to_path_buf();
    let parent = bundle
        .parent()
        .ok_or("the installed app has no containing folder")?
        .to_path_buf();
    if !writable(&parent) {
        return Err(format!(
            "Oatmeal can't write to {} — install the update from the disk image instead",
            parent.display()
        ));
    }

    // Staged next to the installed app so the swap is a rename on one volume.
    let staged = parent.join(".Oatmeal.app.new");
    let old = parent.join(".Oatmeal.app.old");

    // A fixed name rather than a fresh temp dir each time, so a run that dies
    // before its cleanup leaves at most one image behind, not one per attempt.
    let work = std::env::temp_dir().join("oatmeal-update");
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).map_err(|e| format!("could not make a scratch folder: {e}"))?;
    let dmg = work.join("Oatmeal.dmg");
    let mount = work.join("mnt");

    run(
        Command::new("curl")
            .arg("-fsSL")
            .arg("--max-time")
            .arg("300")
            .arg("-H")
            .arg("User-Agent: Oatmeal")
            .arg("-o")
            .arg(&dmg)
            .arg(url),
        "downloading the update",
    )?;

    run(
        Command::new("hdiutil")
            .arg("attach")
            .arg(&dmg)
            .arg("-nobrowse")
            .arg("-quiet")
            .arg("-readonly")
            .arg("-mountpoint")
            .arg(&mount),
        "opening the update",
    )?;

    // Everything from here has a mounted image to put back, so failures detach
    // before they return.
    let detach = || {
        let _ = Command::new("hdiutil")
            .arg("detach")
            .arg(&mount)
            .arg("-quiet")
            .output();
    };
    let copied = app_in(&mount).and_then(|app| {
        let _ = fs::remove_dir_all(&staged);
        // `ditto` rather than a recursive copy: it is the tool that preserves
        // the symlinks, permissions and extended attributes an .app is made of.
        run(
            Command::new("ditto").arg(&app).arg(&staged),
            "unpacking the update",
        )
    });
    detach();
    if let Err(e) = copied {
        let _ = fs::remove_dir_all(&staged);
        return Err(e);
    }

    let script = work.join("swap.sh");
    fs::write(&script, SWAP_SCRIPT).map_err(|e| format!("could not stage the update: {e}"))?;
    Command::new("/bin/sh")
        .arg(&script)
        .arg(std::process::id().to_string())
        .arg(&bundle)
        .arg(&staged)
        .arg(&old)
        .arg(&dmg)
        .spawn()
        .map_err(|e| format!("could not start the installer: {e}"))?;
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

    #[test]
    fn the_bundle_is_found_three_levels_above_the_executable() {
        assert_eq!(
            bundle_of(Path::new("/Applications/Oatmeal.app/Contents/MacOS/oatmeal-app")),
            Some(Path::new("/Applications/Oatmeal.app")),
        );
        // `cargo run` and the test binary have no bundle to replace. Guessing
        // here would mean `rm -rf`ing a directory that is not an app.
        assert_eq!(bundle_of(Path::new("/Users/x/oatmeal/target/debug/oatmeal-app")), None);
        assert_eq!(bundle_of(Path::new("/oatmeal-app")), None);
    }

    /// `install` deletes and replaces a directory, so the URL it trusts has to
    /// be one this project published — checked before anything is downloaded.
    #[test]
    fn install_refuses_anything_but_a_repository_disk_image() {
        for url in [
            "https://evil.example/Oatmeal.dmg",
            "https://github.com/someone-else/oatmeal/releases/download/v9/Oatmeal.dmg",
            // Right repository, but not an image — a release can attach anything.
            "https://github.com/Cujoqt/oatmeal/releases/download/v1.5.1/notes.txt",
        ] {
            assert!(install(url).is_err(), "should have refused {url}");
        }
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

    #[test]
    fn notes_are_asked_for_by_the_running_version_s_tag() {
        let mut asked = String::new();
        let got = notes_with("1.10.6", |tag| {
            asked = tag.to_string();
            Ok(br#"{"tag_name":"v1.10.6","name":"Quieter startup","body":"- Fixed a thing\n","html_url":"https://github.com/Cujoqt/oatmeal/releases/tag/v1.10.6"}"#.to_vec())
        })
        .expect("a well-formed release should parse");
        assert_eq!(asked, "v1.10.6");
        assert_eq!(got.name, "Quieter startup");
        assert_eq!(got.body, "- Fixed a thing");
        assert!(got.release_url.is_some());
    }

    /// A release published without a title must still show a heading.
    #[test]
    fn a_nameless_release_falls_back_to_its_tag() {
        let doc = serde_json::json!({ "tag_name": "v1.4.0", "name": "  ", "body": "x" });
        assert_eq!(notes_from_release(&doc, "v1.4.0").name, "v1.4.0");
    }

    /// GitHub answers an unreleased tag with a `message` document. That is a
    /// missing release, not notes with no body.
    #[test]
    fn a_tag_with_no_release_is_an_error_not_empty_notes() {
        let err = notes_with("9.9.9", |_| Ok(br#"{"message":"Not Found"}"#.to_vec()))
            .expect_err("a message document is not a release");
        assert!(err.contains("v9.9.9"), "should name the tag it looked for: {err}");
    }

    /// Unlike the update check, this one reports the failure — the user asked
    /// for the page, so an empty screen would be a lie.
    #[test]
    fn an_unreachable_github_is_reported() {
        let err = notes_with("1.0.0", |_| Err("could not reach GitHub".into()))
            .expect_err("a failed fetch must surface");
        assert_eq!(err, "could not reach GitHub");
    }

    /// An `html_url` outside the project's repository is dropped, exactly as the
    /// update check drops it — nothing off-repo ever reaches `open`.
    #[test]
    fn a_release_url_outside_the_repo_is_dropped() {
        let doc = serde_json::json!({
            "tag_name": "v1.4.0",
            "body": "x",
            "html_url": "https://example.com/not-us"
        });
        assert_eq!(notes_from_release(&doc, "v1.4.0").release_url, None);
    }
}
