// Model management — fetch the Whisper ggml model on first run.
//
// The transcription lane (M4) needs a ggml `.bin` on disk. Rather than pull in an
// async HTTP stack, we shell out to `curl` (always present on macOS) to download
// the model from Hugging Face into the app-support models directory. The download
// is resumable (`-C -`) and atomic-ish: we fetch to a `.part` file and rename on
// success so a half-download never looks like a valid model.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::transcribe::default_model_path;

/// Hugging Face URL for the default model (base.en — a good speed/accuracy
/// balance for meeting speech, ~148 MB).
const MODEL_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin";

/// Ensure the default model exists locally, downloading it if absent. Returns the
/// path to the ready model. Blocking; run off the UI thread.
pub fn ensure_model() -> Result<PathBuf, String> {
    let dest = default_model_path();
    if dest.exists() && file_len(&dest) > 1_000_000 {
        return Ok(dest);
    }
    download_to(&dest, MODEL_URL)?;
    Ok(dest)
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
