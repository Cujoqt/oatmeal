// Model management — fetch ggml model files on first run.
//
// Rather than pull in an async HTTP stack, we shell out to `curl` (always present
// on macOS). Downloads are resumable (`-C -`) and atomic-ish: fetch to a `.part`
// file and rename on success, so a half-download never looks like a valid model.
//
// Two families live here: Whisper (speech-to-text) and the local chat model used
// for note summaries and recaps. They differ only in size and URL.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::transcribe::{model_dir, resolve_model_path};

/// A downloadable Whisper model. `small.en` is the default: clearly better than
/// `base.en` on lectures and technical vocabulary, still comfortably faster than
/// realtime on Apple silicon so live transcription keeps up.
pub struct WhisperModel {
    pub name: &'static str,
    pub file: &'static str,
    pub url: &'static str,
    /// Rough download size, for the UI's progress copy.
    pub approx_mb: u32,
}

pub const WHISPER_MODELS: &[WhisperModel] = &[
    WhisperModel {
        name: "base.en",
        file: "ggml-base.en.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin",
        approx_mb: 148,
    },
    WhisperModel {
        name: "small.en",
        file: "ggml-small.en.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin",
        approx_mb: 466,
    },
    WhisperModel {
        name: "medium.en",
        file: "ggml-medium.en.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-medium.en.bin",
        approx_mb: 1533,
    },
];

/// Look a Whisper model up by short name.
pub fn whisper_model(name: &str) -> Option<&'static WhisperModel> {
    WHISPER_MODELS.iter().find(|m| m.name == name)
}

/// Ensure the Whisper model exists locally, downloading it if absent. Returns the
/// path to the ready model. Blocking; run off the UI thread.
pub fn ensure_model() -> Result<PathBuf, String> {
    let dest = resolve_model_path("");
    if is_present(&dest) {
        return Ok(dest);
    }
    // Match the resolved filename back to a known model so we know where to
    // fetch it from. An unrecognised OATMEAL_MODEL path can't be downloaded.
    let file = dest
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or_default();
    let model = WHISPER_MODELS
        .iter()
        .find(|m| m.file == file)
        .ok_or_else(|| format!("don't know where to download {file} from"))?;

    download_to(&dest, model.url)?;
    Ok(dest)
}

/// The local chat model behind note summaries and recaps.
///
/// Qwen2.5 3B Instruct at Q4_K_M: ~2 GB on disk and a few hundred MB of working
/// set, which keeps Oatmeal installable on a machine that also has to hold the
/// Whisper model and the recordings themselves. Override with `OATMEAL_LLM`.
pub const CHAT_MODEL_FILE: &str = "qwen2.5-3b-instruct-q4_k_m.gguf";
const CHAT_MODEL_URL: &str = "https://huggingface.co/bartowski/Qwen2.5-3B-Instruct-GGUF/resolve/main/Qwen2.5-3B-Instruct-Q4_K_M.gguf";
pub const CHAT_MODEL_APPROX_MB: u32 = 1930;

/// Where the chat model lives on disk, honouring `OATMEAL_LLM`.
pub fn chat_model_path() -> PathBuf {
    if let Ok(env) = std::env::var("OATMEAL_LLM") {
        if !env.trim().is_empty() {
            return PathBuf::from(env);
        }
    }
    model_dir().join(CHAT_MODEL_FILE)
}

/// Ensure the chat model exists locally, downloading it if absent. Blocking.
pub fn ensure_chat_model() -> Result<PathBuf, String> {
    let dest = chat_model_path();
    if is_present(&dest) {
        return Ok(dest);
    }
    if dest.file_name().and_then(|f| f.to_str()) != Some(CHAT_MODEL_FILE) {
        return Err(format!(
            "no model at {} — OATMEAL_LLM points somewhere Oatmeal can't download",
            dest.display()
        ));
    }
    download_to(&dest, CHAT_MODEL_URL)?;
    Ok(dest)
}

/// Whether a model file is present and not obviously a stub.
pub fn is_present(p: &Path) -> bool {
    file_len(p) > 1_000_000
}

fn download_to(dest: &Path, url: &str) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("create models dir {}: {e}", parent.display()))?;
    }
    let part = dest.with_extension("part");

    let status = Command::new("curl")
        .arg("-L") // follow redirects (HF serves a CDN redirect)
        .arg("-f") // fail on HTTP errors instead of writing an error body
        .arg("-C")
        .arg("-") // resume a partial download if one exists
        .arg("-o")
        .arg(&part)
        .arg(url)
        .status()
        .map_err(|e| format!("spawn curl: {e}"))?;

    if !status.success() {
        return Err(format!(
            "curl exited with {} while downloading the model",
            status.code().unwrap_or(-1)
        ));
    }
    if file_len(&part) < 1_000_000 {
        let _ = std::fs::remove_file(&part);
        return Err("downloaded model looks truncated".into());
    }
    std::fs::rename(&part, dest)
        .map_err(|e| format!("finalize model file {}: {e}", dest.display()))?;
    Ok(())
}

fn file_len(p: &Path) -> u64 {
    std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
}
