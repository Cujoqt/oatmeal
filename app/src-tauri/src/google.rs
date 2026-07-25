//! Google account: OAuth (installed-app loopback flow with PKCE) + Calendar.
//!
//! The user creates a "Desktop app" OAuth client in their own Google Cloud
//! project and pastes the client ID/secret into Settings. Connecting spins up a
//! throwaway `127.0.0.1` listener, opens the consent screen in the default
//! browser, catches the redirect, and trades the code for tokens. The refresh
//! token is stored owner-only under the app-support root; access tokens are
//! refreshed on demand.
//!
//! Like `model.rs`, HTTP goes through `curl` rather than pulling an async HTTP
//! stack into the build for a handful of requests.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::settings;

const AUTH_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
const USERINFO_ENDPOINT: &str = "https://www.googleapis.com/oauth2/v2/userinfo";
const EVENTS_ENDPOINT: &str = "https://www.googleapis.com/calendar/v3/calendars/primary/events";
const SCOPE: &str = "https://www.googleapis.com/auth/calendar.readonly https://www.googleapis.com/auth/userinfo.email";

/// How long the loopback listener waits for the browser round-trip.
const CONSENT_TIMEOUT: Duration = Duration::from_secs(180);

// ── token storage ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Tokens {
    refresh_token: String,
    #[serde(default)]
    access_token: String,
    /// Unix seconds; 0 means "unknown, refresh before use".
    #[serde(default)]
    expires_at: u64,
    #[serde(default)]
    email: String,
}

/// Tokens are credentials: overwrite them on the way out rather than leaving
/// copies in freed heap pages.
impl Drop for Tokens {
    fn drop(&mut self) {
        for field in [&mut self.refresh_token, &mut self.access_token] {
            field.zeroize();
        }
    }
}

fn tokens_path() -> PathBuf {
    settings::support_root().join("google-tokens.json")
}

fn load_tokens() -> Option<Tokens> {
    let raw = std::fs::read_to_string(tokens_path()).ok()?;
    let tokens: Tokens = serde_json::from_str(&raw).ok()?;
    (!tokens.refresh_token.is_empty()).then_some(tokens)
}

fn save_tokens(tokens: &Tokens) -> Result<(), String> {
    let path = tokens_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
        settings::restrict_dir(parent);
    }
    let text = serde_json::to_string_pretty(tokens).map_err(|e| format!("serialize tokens: {e}"))?;
    std::fs::write(&path, text).map_err(|e| format!("write {}: {e}", path.display()))?;
    settings::restrict(&path);
    Ok(())
}

/// Connection state for the Settings tab.
#[derive(Debug, Clone, Serialize)]
pub struct GoogleStatus {
    /// A client ID *and* secret are on disk, so a connect can be attempted.
    pub client_configured: bool,
    /// A refresh token is stored.
    pub connected: bool,
    pub email: String,
}

pub fn status() -> GoogleStatus {
    let tokens = load_tokens();
    GoogleStatus {
        client_configured: settings::client_id().is_some() && settings::client_secret().is_some(),
        connected: tokens.is_some(),
        email: tokens.map(|t| t.email.clone()).unwrap_or_default(),
    }
}

pub fn disconnect() -> Result<(), String> {
    match std::fs::remove_file(tokens_path()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("remove {}: {e}", tokens_path().display())),
    }
}

pub fn is_connected() -> bool {
    load_tokens().is_some()
}

// ── small crypto/encoding helpers ────────────────────────────────────────────

fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[(n >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[n as usize & 63] as char);
        }
    }
    out // unpadded, which is what PKCE wants
}

fn random_bytes(n: usize) -> Result<Vec<u8>, String> {
    let mut buf = vec![0u8; n];
    let mut file = std::fs::File::open("/dev/urandom").map_err(|e| format!("open urandom: {e}"))?;
    file.read_exact(&mut buf)
        .map_err(|e| format!("read urandom: {e}"))?;
    Ok(buf)
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => match u8::from_str_radix(&s[i + 1..i + 3], 16) {
                Ok(b) => {
                    out.push(b);
                    i += 3;
                }
                Err(_) => {
                    out.push(b'%');
                    i += 1;
                }
            },
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ── HTTP through curl ────────────────────────────────────────────────────────

/// POST a form to Google. The fields go in through a `curl` config on stdin
/// rather than argv — a client secret or an auth code in a process listing is
/// readable by every other process running as this user.
fn curl_form(url: &str, fields: &[(&str, &str)]) -> Result<serde_json::Value, String> {
    let mut config = String::from("silent\nshow-error\nmax-time = 30\n");
    for (key, value) in fields {
        config.push_str(&format!("data-urlencode = \"{key}={}\"\n", escape_config(value)));
    }
    config.push_str(&format!("url = \"{}\"\n", escape_config(url)));

    let mut child = Command::new("curl")
        .arg("--config")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn curl: {e}"))?;
    child
        .stdin
        .take()
        .ok_or("curl stdin closed")?
        .write_all(config.as_bytes())
        .map_err(|e| format!("write curl config: {e}"))?;
    let out = child
        .wait_with_output()
        .map_err(|e| format!("curl: {e}"))?;
    parse_json(&out.stdout, &out.stderr)
}

/// Quote-escape a value for a curl config line.
fn escape_config(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// GET with a bearer token, again via a stdin config so the token never appears
/// in a process listing.
fn curl_get(url: &str, access_token: &str) -> Result<serde_json::Value, String> {
    let config = format!(
        "silent\nshow-error\nmax-time = 30\nheader = \"Authorization: Bearer {}\"\nurl = \"{}\"\n",
        escape_config(access_token),
        escape_config(url)
    );
    let mut child = Command::new("curl")
        .arg("--config")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn curl: {e}"))?;
    child
        .stdin
        .take()
        .ok_or("curl stdin closed")?
        .write_all(config.as_bytes())
        .map_err(|e| format!("write curl config: {e}"))?;
    let out = child
        .wait_with_output()
        .map_err(|e| format!("curl: {e}"))?;
    parse_json(&out.stdout, &out.stderr)
}

fn parse_json(stdout: &[u8], stderr: &[u8]) -> Result<serde_json::Value, String> {
    let body = String::from_utf8_lossy(stdout);
    if body.trim().is_empty() {
        return Err(format!(
            "no response from Google ({})",
            String::from_utf8_lossy(stderr).trim()
        ));
    }
    let json: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("unexpected response from Google: {e}"))?;
    if let Some(err) = json.get("error") {
        let description = json
            .get("error_description")
            .and_then(|v| v.as_str())
            .or_else(|| err.get("message").and_then(|v| v.as_str()))
            .unwrap_or("");
        let code = err.as_str().unwrap_or("error");
        return Err(if description.is_empty() {
            format!("Google returned {code}")
        } else {
            format!("Google: {description}")
        });
    }
    Ok(json)
}

// ── credential import ────────────────────────────────────────────────────────
//
// Google hands out a `client_secret_*.json` download when you create a desktop
// OAuth client. Reading that file is the whole setup — nobody should have to
// copy two long strings by hand.

/// Where the guided flow sends people to create the client.
pub const CONSOLE_URL: &str = "https://console.cloud.google.com/auth/clients/create";

/// Pull the client pair out of Google's credentials JSON. The interesting keys
/// live under `installed` for a desktop client and `web` for the other kind;
/// a bare `{client_id, client_secret}` object is accepted too.
fn parse_credentials(text: &str) -> Option<(String, String)> {
    let json: serde_json::Value = serde_json::from_str(text).ok()?;
    for node in [json.get("installed"), json.get("web"), Some(&json)] {
        let Some(node) = node else { continue };
        let id = node.get("client_id").and_then(|v| v.as_str()).unwrap_or("");
        let secret = node
            .get("client_secret")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !id.is_empty() && !secret.is_empty() {
            return Some((id.to_string(), secret.to_string()));
        }
    }
    None
}

/// Save a client from pasted text: either Google's JSON, or the ID and secret on
/// their own (one per line, or separated by whitespace).
pub fn import_credentials(text: &str) -> Result<GoogleStatus, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("nothing pasted yet".into());
    }
    if let Some((id, secret)) = parse_credentials(text) {
        settings::save_client(&id, &secret)?;
        return Ok(status());
    }

    // Loose paste: find the two recognisable shapes anywhere in the text.
    let words: Vec<&str> = text.split_whitespace().collect();
    let id = words
        .iter()
        .find(|w| w.ends_with(".apps.googleusercontent.com"))
        .copied();
    let secret = words.iter().find(|w| w.starts_with("GOCSPX-")).copied();
    match (id, secret) {
        (Some(id), Some(secret)) => {
            settings::save_client(id, secret)?;
            Ok(status())
        }
        _ => Err("couldn't find a client ID and secret in that — paste the JSON Google downloaded, or the ID and secret together.".into()),
    }
}

/// The newest `client_secret*.json` sitting in ~/Downloads, imported. This is the
/// happy path: create the client, hit download, come back, click the button.
pub fn import_downloaded() -> Result<GoogleStatus, String> {
    let home = std::env::var("HOME").map_err(|_| "no home directory")?;
    let downloads = PathBuf::from(&home).join("Downloads");
    let entries = std::fs::read_dir(&downloads)
        .map_err(|e| format!("read {}: {e}", downloads.display()))?;

    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if !(name.starts_with("client_secret") && name.ends_with(".json")) {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        if best.as_ref().map_or(true, |(seen, _)| modified > *seen) {
            best = Some((modified, path));
        }
    }

    let (_, path) = best.ok_or(
        "no client_secret….json in your Downloads folder yet — download it from the Google Cloud page first.",
    )?;
    let text = std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let (id, secret) = parse_credentials(&text)
        .ok_or_else(|| format!("{} doesn't look like Google credentials", path.display()))?;
    settings::save_client(&id, &secret)?;
    Ok(status())
}

/// Open a URL in the default browser — the console link in the guided flow.
pub fn open_url(url: &str) -> Result<(), String> {
    if !url.starts_with("https://") {
        return Err("refusing to open a non-https URL".into());
    }
    Command::new("open")
        .arg(url)
        .status()
        .map(|_| ())
        .map_err(|e| format!("could not open the browser: {e}"))
}

// ── the consent round-trip ───────────────────────────────────────────────────

/// Run the full connect flow. Blocking: opens the browser and waits for the
/// redirect (up to `CONSENT_TIMEOUT`) before exchanging the code.
pub fn connect() -> Result<GoogleStatus, String> {
    let client_id =
        settings::client_id().ok_or("No Google client ID saved yet — add one in Settings first.")?;
    let client_secret = settings::client_secret()
        .ok_or("No Google client secret saved yet — add one in Settings first.")?;

    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|e| format!("could not open the loopback listener: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("loopback address: {e}"))?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}");

    let verifier = base64url(&random_bytes(32)?);
    let challenge = base64url(&Sha256::digest(verifier.as_bytes()));
    let state = base64url(&random_bytes(16)?);

    let url = format!(
        "{AUTH_ENDPOINT}?client_id={}&redirect_uri={}&response_type=code&scope={}\
         &code_challenge={}&code_challenge_method=S256&access_type=offline&prompt=consent&state={}",
        percent_encode(&client_id),
        percent_encode(&redirect_uri),
        percent_encode(SCOPE),
        percent_encode(&challenge),
        percent_encode(&state),
    );

    Command::new("open")
        .arg(&url)
        .status()
        .map_err(|e| format!("could not open the browser: {e}"))?;

    let mut code = wait_for_code(&listener, &state)?;

    let token = curl_form(
        TOKEN_ENDPOINT,
        &[
            ("code", &code),
            ("client_id", &client_id),
            ("client_secret", &client_secret),
            ("redirect_uri", &redirect_uri),
            ("grant_type", "authorization_code"),
            ("code_verifier", &verifier),
        ],
    )?;

    let refresh_token = token
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if refresh_token.is_empty() {
        return Err("Google did not return a refresh token — remove Oatmeal at myaccount.google.com/permissions and connect again.".into());
    }
    let access_token = token
        .get("access_token")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let expires_in = token.get("expires_in").and_then(|v| v.as_u64()).unwrap_or(0);

    let email = curl_get(USERINFO_ENDPOINT, &access_token)
        .ok()
        .and_then(|json| json.get("email").and_then(|v| v.as_str()).map(String::from))
        .unwrap_or_default();

    save_tokens(&Tokens {
        refresh_token,
        access_token,
        expires_at: now_secs() + expires_in.saturating_sub(60),
        email,
    })?;

    let mut verifier = verifier;
    code.zeroize();
    verifier.zeroize();
    Ok(status())
}

/// Accept connections until one carries the OAuth redirect with our `state`.
/// Anything else (a favicon probe, a stray request) is answered and ignored.
fn wait_for_code(listener: &TcpListener, state: &str) -> Result<String, String> {
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("listener: {e}"))?;
    let deadline = Instant::now() + CONSENT_TIMEOUT;

    while Instant::now() < deadline {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).ok();
                match handle_redirect(stream, state) {
                    Ok(Some(code)) => return Ok(code),
                    Ok(None) => continue,
                    Err(e) => return Err(e),
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(120));
            }
            Err(e) => return Err(format!("loopback accept: {e}")),
        }
    }
    Err("timed out waiting for the Google consent screen".into())
}

/// Read one request, reply, and pull the code out if it's the redirect.
/// `Ok(None)` means "not our request, keep listening".
fn handle_redirect(mut stream: TcpStream, state: &str) -> Result<Option<String>, String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| format!("loopback read timeout: {e}"))?;

    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf).unwrap_or(0);
    let request = String::from_utf8_lossy(&buf[..n]);
    let target = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("")
        .to_string();

    let query = target.split_once('?').map(|(_, q)| q).unwrap_or("");
    let mut code = None;
    let mut got_state = None;
    let mut error = None;
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        match key {
            "code" => code = Some(percent_decode(value)),
            "state" => got_state = Some(percent_decode(value)),
            "error" => error = Some(percent_decode(value)),
            _ => {}
        }
    }

    if let Some(err) = error {
        reply(
            &mut stream,
            "Oatmeal couldn’t connect",
            &format!("Google said: {err}"),
        );
        return Err(format!("Google denied the request: {err}"));
    }

    match (code, got_state) {
        (Some(code), Some(got)) if got == state => {
            reply(
                &mut stream,
                "Google connected",
                "You can close this tab and go back to Oatmeal.",
            );
            Ok(Some(code))
        }
        (Some(_), _) => {
            reply(
                &mut stream,
                "Oatmeal couldn’t connect",
                "The login state didn’t match — start the connection again.",
            );
            Err("the OAuth state did not match — the redirect was not ours".into())
        }
        _ => {
            reply(&mut stream, "Oatmeal", "Waiting for Google…");
            Ok(None)
        }
    }
}

fn reply(stream: &mut TcpStream, title: &str, body: &str) {
    let html = format!(
        "<!doctype html><meta charset=utf-8><title>{title}</title>\
         <body style=\"font-family:-apple-system,system-ui,sans-serif;background:#1c1c1c;color:#ececea;\
         display:flex;flex-direction:column;align-items:center;justify-content:center;height:100vh;margin:0\">\
         <h1 style=\"font-weight:500\">{title}</h1><p style=\"color:#b6b6b1\">{body}</p>"
    );
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        html.len(),
        html
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

// ── access tokens ────────────────────────────────────────────────────────────

/// A valid access token, refreshing when the stored one is expired/missing.
fn access_token() -> Result<String, String> {
    let mut tokens = load_tokens().ok_or("Google isn’t connected yet.")?;
    if !tokens.access_token.is_empty() && tokens.expires_at > now_secs() {
        return Ok(tokens.access_token.clone());
    }

    let client_id = settings::client_id().ok_or("No Google client ID saved.")?;
    let client_secret = settings::client_secret().ok_or("No Google client secret saved.")?;
    let refreshed = curl_form(
        TOKEN_ENDPOINT,
        &[
            ("client_id", &client_id),
            ("client_secret", &client_secret),
            ("refresh_token", &tokens.refresh_token),
            ("grant_type", "refresh_token"),
        ],
    )?;

    let access = refreshed
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or("Google did not return an access token — reconnect in Settings.")?;
    let expires_in = refreshed
        .get("expires_in")
        .and_then(|v| v.as_u64())
        .unwrap_or(3600);

    tokens.access_token = access.to_string();
    tokens.expires_at = now_secs() + expires_in.saturating_sub(60);
    save_tokens(&tokens)?;
    Ok(tokens.access_token.clone())
}

// ── calendar ─────────────────────────────────────────────────────────────────

/// One occurrence, ready for the "Coming up" panel. Times are ISO-8601 strings
/// the webview parses in local time: timed events keep their offset, all-day
/// events are bare dates.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Event {
    pub id: String,
    pub summary: String,
    pub start: String,
    pub end: Option<String>,
    pub all_day: bool,
    pub location: Option<String>,
    /// Join link pulled off the event, when there is one.
    pub link: Option<String>,
}

/// What the UI gets back. `connected: false` means "no Google account yet",
/// which the panel shows as a setup hint rather than an error.
#[derive(Debug, Clone, Serialize)]
pub struct CalendarFeed {
    pub connected: bool,
    pub events: Vec<Event>,
}

/// Upcoming events from the primary calendar, already expanded to single
/// occurrences by Google (`singleEvents=true`). The window runs from the start
/// of yesterday to `days + 1` days out, in UTC — the extra day on each side
/// covers the gap between UTC here and this machine's local zone, and the
/// webview does the exact filtering.
pub fn list_events(days: u32) -> Result<CalendarFeed, String> {
    if !is_connected() {
        return Ok(CalendarFeed {
            connected: false,
            events: Vec::new(),
        });
    }
    let token = access_token()?;
    let url = format!(
        "{EVENTS_ENDPOINT}?singleEvents=true&orderBy=startTime&maxResults=250&timeMin={}&timeMax={}",
        percent_encode(&utc_rfc3339(-1)),
        percent_encode(&utc_rfc3339(days as i64 + 1)),
    );
    let json = curl_get(&url, &token)?;

    let items = json
        .get("items")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut events = Vec::new();
    for item in items {
        if item.get("status").and_then(|v| v.as_str()) == Some("cancelled") {
            continue;
        }
        let Some((start, all_day)) = stamp(item.get("start")) else {
            continue;
        };
        events.push(Event {
            id: item
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            summary: item
                .get("summary")
                .and_then(|v| v.as_str())
                .unwrap_or("(no title)")
                .to_string(),
            start,
            end: stamp(item.get("end")).map(|(s, _)| s),
            all_day,
            location: item
                .get("location")
                .and_then(|v| v.as_str())
                .map(String::from),
            link: meeting_link(&item),
        });
    }
    Ok(CalendarFeed {
        connected: true,
        events,
    })
}

/// Google sends either `dateTime` (RFC-3339 with an offset) or `date` for
/// all-day events. Both parse in the webview; the flag tells the UI which.
fn stamp(node: Option<&serde_json::Value>) -> Option<(String, bool)> {
    let node = node?;
    if let Some(datetime) = node.get("dateTime").and_then(|v| v.as_str()) {
        return Some((datetime.to_string(), false));
    }
    node.get("date")
        .and_then(|v| v.as_str())
        .map(|d| (d.to_string(), true))
}

/// The join link: Meet's `hangoutLink`, else the first video entry point
/// (Zoom/Teams land there when the invite was made through Google).
fn meeting_link(item: &serde_json::Value) -> Option<String> {
    if let Some(link) = item.get("hangoutLink").and_then(|v| v.as_str()) {
        return Some(link.to_string());
    }
    item.get("conferenceData")?
        .get("entryPoints")?
        .as_array()?
        .iter()
        .find(|entry| entry.get("entryPointType").and_then(|v| v.as_str()) == Some("video"))
        .and_then(|entry| entry.get("uri").and_then(|v| v.as_str()))
        .map(String::from)
}

/// Midnight UTC `offset_days` from today, as RFC-3339 — what `timeMin`/`timeMax`
/// want. Derived from the epoch directly so no date crate is needed.
fn utc_rfc3339(offset_days: i64) -> String {
    let days = (now_secs() as i64).div_euclid(86_400) + offset_days;
    let (y, mo, d) = civil_from_days(days);
    format!("{y:04}-{mo:02}-{d:02}T00:00:00Z")
}

/// Days since the epoch → civil date (Howard Hinnant's `civil_from_days`).
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 }.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    ((y + i64::from(m <= 2)) as i32, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_json_parses_from_either_wrapper() {
        let desktop = r#"{"installed":{"client_id":"abc.apps.googleusercontent.com","client_secret":"GOCSPX-xyz","redirect_uris":["http://localhost"]}}"#;
        assert_eq!(
            parse_credentials(desktop),
            Some(("abc.apps.googleusercontent.com".into(), "GOCSPX-xyz".into()))
        );
        let bare = r#"{"client_id":"a","client_secret":"b"}"#;
        assert_eq!(parse_credentials(bare), Some(("a".into(), "b".into())));
        assert_eq!(parse_credentials("not json"), None);
        assert_eq!(parse_credentials(r#"{"installed":{"client_id":"a"}}"#), None);
    }

    #[test]
    fn loose_paste_finds_the_two_shapes() {
        let text = "Client ID\n123-abc.apps.googleusercontent.com\nClient secret\nGOCSPX-secret";
        let (id, secret) = parse_credentials(text)
            .or_else(|| {
                let words: Vec<&str> = text.split_whitespace().collect();
                let id = words.iter().find(|w| w.ends_with(".apps.googleusercontent.com"))?;
                let secret = words.iter().find(|w| w.starts_with("GOCSPX-"))?;
                Some((id.to_string(), secret.to_string()))
            })
            .expect("both parts found");
        assert_eq!(id, "123-abc.apps.googleusercontent.com");
        assert_eq!(secret, "GOCSPX-secret");
    }

    #[test]
    fn base64url_is_unpadded_and_url_safe() {
        assert_eq!(base64url(b"hello world!"), "aGVsbG8gd29ybGQh");
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
        assert_eq!(base64url(b""), "");
    }

    #[test]
    fn pkce_challenge_matches_the_rfc_example() {
        // RFC 7636 appendix B.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            base64url(&Sha256::digest(verifier.as_bytes())),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn percent_roundtrip_keeps_query_values_intact() {
        let raw = "4/0Ab_5q l+x?&=%";
        assert_eq!(percent_decode(&percent_encode(raw)), raw);
    }

    #[test]
    fn api_errors_surface_their_description() {
        let body = br#"{"error":"invalid_grant","error_description":"Bad Request"}"#;
        let err = parse_json(body, b"").unwrap_err();
        assert!(err.contains("Bad Request"), "{err}");
    }

    #[test]
    fn stamps_flag_all_day_events() {
        let datetime = serde_json::json!({ "dateTime": "2026-07-24T09:00:00-07:00" });
        assert_eq!(
            stamp(Some(&datetime)),
            Some(("2026-07-24T09:00:00-07:00".into(), false))
        );
        let date = serde_json::json!({ "date": "2026-07-24" });
        assert_eq!(stamp(Some(&date)), Some(("2026-07-24".into(), true)));
        assert_eq!(stamp(None), None);
    }

    #[test]
    fn meeting_links_prefer_meet_then_video_entry_points() {
        let meet = serde_json::json!({ "hangoutLink": "https://meet.google.com/abc" });
        assert_eq!(
            meeting_link(&meet).as_deref(),
            Some("https://meet.google.com/abc")
        );

        let zoom = serde_json::json!({
            "conferenceData": { "entryPoints": [
                { "entryPointType": "phone", "uri": "tel:+1" },
                { "entryPointType": "video", "uri": "https://zoom.us/j/1" }
            ]}
        });
        assert_eq!(meeting_link(&zoom).as_deref(), Some("https://zoom.us/j/1"));
        assert_eq!(meeting_link(&serde_json::json!({})), None);
    }

    #[test]
    fn epoch_days_convert_to_civil_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }
}
