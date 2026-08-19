//! Who said it.
//!
//! Whisper writes down *what* was said and has no idea how many people are in
//! the room, so every transcript reads as one voice. Speaker labels come from a
//! second pass over the same audio: pyannote's segmentation model marks where
//! speech is and where the voice changes, a speaker-embedding model turns each
//! of those stretches into a vector, and the vectors are clustered. "Speaker 2"
//! therefore means "the same voice as the rest of cluster 2" — not a name, and
//! not an identity that survives into another meeting.
//!
//! Both models are ONNX and run on-device through sherpa-onnx, downloaded once
//! like Whisper's, so this adds nothing to what leaves the machine.
//!
//! It is deliberately not part of stopping a meeting. The pass runs at roughly a
//! fifth of real time on one thread — minutes for a long meeting — and the
//! transcript is worth reading before that finishes, so labelling is a separate
//! step that rewrites `transcript.md` in place when it is done.

use std::path::{Path, PathBuf};

use crate::transcribe::model_dir;

/// Where speech is, and whose voice it was. Centiseconds, to match the
/// transcript's own timeline.
#[derive(Debug, Clone, Copy)]
pub struct Speech {
    pub start_cs: i64,
    pub end_cs: i64,
    /// Cluster id from the diarizer, 0-based.
    pub speaker: i32,
}

/// pyannote segmentation 3.0, exported to ONNX by the sherpa-onnx project.
const SEG_FILE: &str = "pyannote-segmentation-3-0.onnx";
const SEG_URL: &str =
    "https://huggingface.co/csukuangfj/sherpa-onnx-pyannote-segmentation-3-0/resolve/main/model.onnx";

/// NeMo TitaNet-small: English speaker embeddings, ~40 MB. The larger models
/// are better at telling similar voices apart and several times slower, and this
/// pass is already the slowest thing in the app per second of audio.
const EMB_FILE: &str = "nemo_en_titanet_small.onnx";
const EMB_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/nemo_en_titanet_small.onnx";

/// Rough download size for the UI's copy: both files together.
pub const APPROX_MB: u32 = 46;

/// Cosine distance below which two stretches are called the same voice. 0.5 is
/// sherpa's default and errs towards splitting one person into two rather than
/// merging two people into one — the failure that is at least visible.
const CLUSTER_THRESHOLD: f32 = 0.5;

/// Speech shorter than this is not labelled: a third of a second is a cough or
/// an "mm", and embedding it produces a vector that clusters at random.
const MIN_SPEECH_SECS: f32 = 0.5;

/// A gap shorter than this inside one person's speech does not end their turn.
const MIN_SILENCE_SECS: f32 = 0.5;

pub fn seg_model_path() -> PathBuf {
    model_dir().join(SEG_FILE)
}

pub fn emb_model_path() -> PathBuf {
    model_dir().join(EMB_FILE)
}

/// Whether both models are already on disk.
pub fn models_present() -> bool {
    crate::model::is_present(&seg_model_path()) && crate::model::is_present(&emb_model_path())
}

/// Fetch both models if they aren't here yet. Blocking; ~46 MB on first run.
pub fn ensure_models() -> Result<(PathBuf, PathBuf), String> {
    let seg = seg_model_path();
    if !crate::model::is_present(&seg) {
        crate::model::download_to(&seg, SEG_URL)?;
    }
    let emb = emb_model_path();
    if !crate::model::is_present(&emb) {
        crate::model::download_to(&emb, EMB_URL)?;
    }
    Ok((seg, emb))
}

/// Run the diarizer over 16 kHz mono samples.
pub fn diarize_samples(samples: &[f32]) -> Result<Vec<Speech>, String> {
    use sherpa_rs::diarize::{Diarize, DiarizeConfig};

    if samples.is_empty() {
        return Ok(Vec::new());
    }
    let (seg, emb) = ensure_models()?;

    let mut diarizer = Diarize::new(
        seg,
        emb,
        DiarizeConfig {
            // No fixed number of speakers: a meeting is however many people
            // turned up, so cluster on distance instead.
            num_clusters: None,
            threshold: Some(CLUSTER_THRESHOLD),
            min_duration_on: Some(MIN_SPEECH_SECS),
            min_duration_off: Some(MIN_SILENCE_SECS),
            ..Default::default()
        },
    )
    .map_err(|e| format!("speaker models would not load: {e}"))?;

    let segments = diarizer
        .compute(samples.to_vec(), None)
        .map_err(|e| format!("speaker pass failed: {e}"))?;

    Ok(segments
        .into_iter()
        .map(|s| Speech {
            start_cs: (s.start * 100.0) as i64,
            end_cs: (s.end * 100.0) as i64,
            speaker: s.speaker,
        })
        .collect())
}

/// The speaker talking at `cs`, if any stretch covers it.
///
/// Whisper's line and the diarizer's stretch rarely start on the same
/// centisecond, so a line is matched to whichever stretch overlaps the window
/// between it and the line after it — the same reason a line that lands in a
/// gap (a laugh, an overlap the segmenter dropped) keeps no label at all rather
/// than borrowing the previous one.
pub fn speaker_between(spans: &[Speech], start_cs: i64, end_cs: i64) -> Option<i32> {
    let mut best: Option<(i64, i32)> = None;
    for s in spans {
        let overlap = s.end_cs.min(end_cs) - s.start_cs.max(start_cs);
        if overlap <= 0 {
            continue;
        }
        if best.map(|(o, _)| overlap > o).unwrap_or(true) {
            best = Some((overlap, s.speaker));
        }
    }
    best.map(|(_, sp)| sp)
}

/// Rewrite `transcript.md` so each line carries the voice that said it, and
/// return how many lines got one.
///
/// The label goes *inside* the existing `**[0:12]**` marker rather than in front
/// of the text, so `strip_transcript_markup` keeps dropping it — the language
/// model must not read "Speaker 2" as though somebody said it — and so a
/// transcript written before this feature existed still parses.
pub fn label_transcript(dir: &Path, spans: &[Speech]) -> Result<usize, String> {
    let path = dir.join("transcript.md");
    let md = std::fs::read_to_string(&path)
        .map_err(|_| "this meeting has no transcript to label".to_string())?;

    // Line starts, so each line's window ends where the next one begins.
    let starts: Vec<Option<i64>> = md.lines().map(|l| line_start_cs(l.trim())).collect();
    let mut out = String::with_capacity(md.len() + spans.len() * 12);
    let mut labelled = 0;

    for (i, line) in md.lines().enumerate() {
        let Some(start_cs) = starts[i] else {
            out.push_str(line);
            out.push('\n');
            continue;
        };
        let next = starts[i + 1..]
            .iter()
            .flatten()
            .copied()
            .next()
            .unwrap_or(start_cs + 1500);
        let trimmed = line.trim();
        let (stamp, text) = trimmed
            .strip_prefix("**[")
            .and_then(|r| r.split_once("]**"))
            .expect("line_start_cs only accepts this shape");
        // Re-labelling an already-labelled transcript replaces the old label.
        let stamp = stamp.split(" · ").next().unwrap_or(stamp);

        match speaker_between(spans, start_cs, next) {
            Some(sp) => {
                labelled += 1;
                out.push_str(&format!("**[{stamp} · Speaker {}]**{text}\n", sp + 1));
            }
            None => out.push_str(&format!("**[{stamp}]**{text}\n")),
        }
    }

    crate::store::write(&path, &out)?;
    Ok(labelled)
}

/// Centiseconds for a `**[1:23]**` / `**[1:23 · Speaker 2]**` line, or `None`
/// for anything that isn't a transcript line.
fn line_start_cs(line: &str) -> Option<i64> {
    let stamp = line.strip_prefix("**[")?.split_once("]**")?.0;
    let stamp = stamp.split(" · ").next().unwrap_or(stamp);
    let (m, s) = stamp.split_once(':')?;
    Some((m.trim().parse::<i64>().ok()? * 60 + s.trim().parse::<i64>().ok()?) * 100)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(start: i64, end: i64, speaker: i32) -> Speech {
        Speech {
            start_cs: start,
            end_cs: end,
            speaker,
        }
    }

    #[test]
    fn a_line_takes_the_voice_it_overlaps_most() {
        let spans = [span(0, 500, 0), span(500, 2000, 1)];
        // Mostly inside the second stretch.
        assert_eq!(speaker_between(&spans, 400, 1500), Some(1));
        assert_eq!(speaker_between(&spans, 0, 400), Some(0));
    }

    #[test]
    fn a_line_in_a_gap_is_left_unlabelled() {
        let spans = [span(0, 500, 0), span(3000, 4000, 1)];
        assert_eq!(speaker_between(&spans, 1000, 2000), None);
    }

    #[test]
    fn timestamps_parse_with_and_without_a_label() {
        assert_eq!(line_start_cs("**[1:23]** hi"), Some(8300));
        assert_eq!(line_start_cs("**[1:23 · Speaker 2]** hi"), Some(8300));
        assert_eq!(line_start_cs("## Transcript"), None);
        assert_eq!(line_start_cs(""), None);
    }

    #[test]
    fn labelling_is_idempotent_and_keeps_the_text() {
        let dir = std::env::temp_dir().join(format!("oatmeal-dz-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("transcript.md"),
            "# M\n\n## Transcript\n\n**[0:00]** one\n\n**[0:20]** two\n",
        )
        .unwrap();

        let spans = [span(0, 1000, 0), span(1900, 3000, 1)];
        assert_eq!(label_transcript(&dir, &spans).unwrap(), 2);
        let once = std::fs::read_to_string(dir.join("transcript.md")).unwrap();
        assert!(once.contains("**[0:00 · Speaker 1]** one"), "{once}");
        assert!(once.contains("**[0:20 · Speaker 2]** two"), "{once}");

        // Running it again must replace the labels, not stack them up.
        assert_eq!(label_transcript(&dir, &spans).unwrap(), 2);
        assert_eq!(std::fs::read_to_string(dir.join("transcript.md")).unwrap(), once);

        // And the label must not survive into what the model reads.
        assert_eq!(crate::library::strip_transcript_markup(&once), "one two");
        std::fs::remove_dir_all(&dir).ok();
    }
}
