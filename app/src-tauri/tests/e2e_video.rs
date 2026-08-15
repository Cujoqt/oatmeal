// End-to-end video test — proves the yt-dlp path works against a real video on
// real YouTube, and that the whole import (download → symphonia decode →
// resample → range cut → whisper → `video-N.md`) produces readable words.
//
// Ignored by default: it downloads yt-dlp (~30 MB, once, pinned version), talks
// to YouTube, pulls a real audio track and runs on-device inference. Run it
// explicitly with:
//
//     cargo test --test e2e_video -- --ignored --nocapture

use std::path::PathBuf;

/// Blender Foundation's "Big Buck Bunny" — Creative Commons, stable, short.
const PROBE_URL: &str = "https://www.youtube.com/watch?v=YE7VzlLtp-4";

/// "Copyright and Fair Use for Images, Videos, and Audio", DesignLab
/// UW–Madison — Creative Commons Attribution, four minutes, and *narrated*.
/// Big Buck Bunny is music and sound effects, so a transcript of it says
/// nothing about whether the audio path works.
const SPEECH_URL: &str = "https://www.youtube.com/watch?v=mGWJAAFhtRk";

#[test]
#[ignore = "downloads yt-dlp and contacts YouTube"]
fn probes_a_real_video() {
    let info = oatmeal_app_lib::video::probe(PROBE_URL).expect("probe");

    println!("probed: {} ({}s)", info.title, info.duration_secs);

    assert!(
        info.duration_secs > 60.0,
        "unexpected duration {}",
        info.duration_secs
    );
    assert!(!info.title.is_empty(), "expected a non-empty title");
}

#[test]
#[ignore = "downloads a real video and runs whisper inference"]
fn imports_a_real_video_into_a_meeting() {
    // The accurate model is what `transcribe_samples` loads. Skipping rather
    // than failing keeps a machine without an 850 MB download from reporting a
    // broken import path when nothing is broken.
    let model = oatmeal_app_lib::transcribe::resolve_accurate_path("");
    if !model.exists() {
        println!(
            "SKIPPED: no whisper model at {} — run the app once, or fetch it with \
             model::ensure_model, then re-run this test",
            model.display()
        );
        return;
    }

    // A real meeting folder under the real recordings root, because `import`
    // resolves its directory through `library::meeting_dir`. Named so a leftover
    // from a crashed run is obvious, and removed at the end either way.
    let id = "20260101-000000-e2e-video-test";
    let dir: PathBuf = oatmeal_app_lib::session::recordings_root().join(id);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create test meeting dir");
    std::fs::write(
        dir.join("transcript.md"),
        "# E2E video test\n\n## Transcript\n\nplaceholder meeting transcript\n",
    )
    .expect("seed transcript");

    // Half a minute of the narration proper — past the title card, and short
    // enough that the test stays in the low minutes.
    let result = oatmeal_app_lib::video::import(id, SPEECH_URL, "0:30", "1:00");

    let written = match result {
        Ok(p) => p,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            panic!("import failed: {e}");
        }
    };
    let md = std::fs::read_to_string(&written).expect("read the written video markdown");
    let _ = std::fs::remove_dir_all(&dir);

    println!("--- {written} ---\n{md}\n------------------");

    assert!(
        written.ends_with("video-1.md"),
        "expected video-1.md, got {written}"
    );
    assert!(
        md.contains("_From https://www.youtube.com/watch?v=mGWJAAFhtRk"),
        "provenance line missing:\n{md}"
    );
    assert!(md.contains("0:30 to 1:00"), "range missing:\n{md}");

    // The spoken words, with the heading and provenance lines dropped — the same
    // view of the file the note-writer gets. Asserting on the body rather than
    // the whole file is what makes this a test of the decode and not of
    // `render_markdown`, which already has a unit test.
    let body = md
        .split("## Transcript")
        .nth(1)
        .expect("transcript section missing")
        .trim();
    assert!(
        body.split_whitespace().count() >= 20,
        "expected a real transcript of 30 seconds of speech, got: {body:?}"
    );
    // This half minute walks through the Creative Commons licences by name.
    // Two words, lowercased, so a missed comma or a mis-heard name doesn't fail
    // the run — but a decode that returns noise or silence does.
    assert!(
        body.to_lowercase().contains("creative commons"),
        "expected the narration's subject in the transcript, got: {body:?}"
    );
}
