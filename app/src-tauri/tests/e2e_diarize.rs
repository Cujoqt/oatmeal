// End-to-end speaker labelling — proves the diarization path really separates
// voices, on audio where the answer is known.
//
// Ignored by default: it downloads the two speaker models (~46 MB, once) and a
// four-speaker clip, then runs both ONNX models on-device. Run explicitly with:
//
//     cargo test --test e2e_diarize -- --ignored --nocapture
//
// Unit tests cover the mapping from stretches onto transcript lines; only this
// one can tell you the models are wired up and actually hearing more than one
// person. Point OATMEAL_DIARIZE_WAV at a recording of your own to hear what it
// makes of that instead.

use std::path::PathBuf;
use std::process::Command;

use oatmeal_app_lib::{diarize, transcribe};

/// Four people, in turn. Published by sherpa-onnx alongside the segmentation
/// model, so it is the same audio the upstream project checks against.
const SAMPLE_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-segmentation-models/0-four-speakers-zh.wav";

#[test]
#[ignore = "downloads ~46 MB of models and runs two ONNX models"]
fn hears_four_different_people() {
    let wav = match std::env::var("OATMEAL_DIARIZE_WAV") {
        Ok(p) if !p.trim().is_empty() => PathBuf::from(p),
        _ => fetch_sample(),
    };

    let samples = transcribe::load_wav_mono_16k(&wav).expect("read the clip");
    assert!(!samples.is_empty(), "the clip decoded to nothing");
    println!("{} s of audio", samples.len() / 16_000);

    let t = std::time::Instant::now();
    let spans = diarize::diarize_samples(&samples).expect("speaker pass");
    println!("{} stretches in {:?}", spans.len(), t.elapsed());

    let mut ids: Vec<i32> = spans.iter().map(|s| s.speaker).collect();
    ids.sort_unstable();
    ids.dedup();
    println!("voices: {ids:?}");

    for s in spans.iter().take(12) {
        println!(
            "  {:>6.1}–{:<6.1} speaker {}",
            s.start_cs as f32 / 100.0,
            s.end_cs as f32 / 100.0,
            s.speaker + 1
        );
    }

    // Every stretch has to be a real stretch, or the mapping onto transcript
    // lines is meaningless.
    for s in &spans {
        assert!(s.end_cs > s.start_cs, "backwards stretch: {s:?}");
    }

    if std::env::var("OATMEAL_DIARIZE_WAV").is_ok() {
        // Somebody else's audio — nothing to assert about how many people are
        // in it, so the printout above is the result.
        return;
    }
    assert!(
        ids.len() >= 3,
        "four people speak in this clip; the pass only separated {}",
        ids.len()
    );
}

fn fetch_sample() -> PathBuf {
    let wav = std::env::temp_dir().join("oatmeal-four-speakers.wav");
    if !wav.exists() {
        let ok = Command::new("curl")
            .args(["-fsSL", "-o"])
            .arg(&wav)
            .arg(SAMPLE_URL)
            .status()
            .expect("run curl")
            .success();
        assert!(ok, "could not download the sample clip");
    }
    wav
}
