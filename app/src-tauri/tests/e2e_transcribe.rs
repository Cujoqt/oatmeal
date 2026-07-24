// End-to-end transcription test — proves the M4 whisper path works on real audio.
//
// Ignored by default: it downloads the ggml model (~148 MB, once) and a short
// speech clip, then runs on-device inference. Run explicitly with:
//
//     cargo test --test e2e_transcribe -- --ignored --nocapture
//
// No microphone or screen-capture permission is needed — it reads a WAV file, so
// it exercises resampling + whisper + segment extraction without any live device.

use std::path::PathBuf;
use std::process::Command;

use oatmeal_app_lib::{model, transcribe};

const SAMPLE_URL: &str =
    "https://github.com/ggerganov/whisper.cpp/raw/master/samples/jfk.wav";

#[test]
#[ignore = "downloads model + sample and runs whisper inference"]
fn transcribes_known_speech_clip() {
    // 1. Model present (downloads base.en on first run).
    let model_path = model::ensure_model().expect("model download/lookup failed");
    assert!(model_path.exists(), "model missing after ensure_model");

    // 2. Fetch the canonical JFK sample (16 kHz mono — hits the resampler
    //    passthrough path).
    let wav = std::env::temp_dir().join("oatmeal-jfk.wav");
    if !wav.exists() {
        let status = Command::new("curl")
            .args(["-Lf", "-o"])
            .arg(&wav)
            .arg(SAMPLE_URL)
            .status()
            .expect("spawn curl");
        assert!(status.success(), "failed to download sample clip");
    }

    // 3. Transcribe.
    let out: PathBuf = wav.clone();
    let transcript =
        transcribe::transcribe_wav("", &out, Some("en")).expect("transcription failed");

    let text = transcript.text.to_lowercase();
    println!("--- transcript ---\n{}\n------------------", transcript.text);

    // 4. The clip is JFK's "ask not what your country can do for you". Assert a
    //    couple of distinctive words survived the whole pipeline.
    assert!(!transcript.segments.is_empty(), "no segments produced");
    assert!(
        text.contains("country") && text.contains("americans"),
        "expected JFK phrasing, got: {text}"
    );
}
