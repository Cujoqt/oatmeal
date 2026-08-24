use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Difficulty {
    Easy,
    Medium,
    Hard,
}

/// What the user asked for on the Study tab. These travel with every generate
/// call rather than living in `config.json`: they describe one generation, not
/// a preference, and the only thing worth remembering is what was used last.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StudySettings {
    pub count: u32,
    pub difficulty: Difficulty,
    /// Empty means the whole recording.
    pub topic_focus: String,
}

/// The count the UI offers, and the range anything arriving from it is held to.
/// The bound is not cosmetic: the JSON for a much larger set runs past the
/// generation cap and comes back truncated, which parses as nothing.
pub const MIN_COUNT: u32 = 3;
pub const MAX_COUNT: u32 = 30;

impl StudySettings {
    /// The count arrives from a number input the user can type anything into.
    fn sanitized(&self) -> StudySettings {
        StudySettings {
            count: self.count.clamp(MIN_COUNT, MAX_COUNT),
            difficulty: self.difficulty,
            topic_focus: self.topic_focus.trim().to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Flashcard {
    pub front: String,
    pub back: String,
}

/// Field names here are the ones the quiz prompt asks the model for, so what
/// the model emits deserializes directly and the cache file reads the same. The
/// webview reads `correct_index` under that name too.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuizQuestion {
    pub question: String,
    /// Exactly four, validated on the way out of the model.
    pub options: Vec<String>,
    /// Indexes `options`. Cached alongside the question so grading a finished
    /// quiz is a comparison in the webview, not a second run of the model.
    pub correct_index: u32,
}

const PLAN_FILE: &str = "study-plan.md";
const FLASHCARDS_FILE: &str = "flashcards.json";
const QUIZ_FILE: &str = "quiz.json";

/// Where each artifact's settings are remembered inside the meeting's shared
/// `meta.json`, so reopening the tab shows the count and difficulty that
/// produced what is on screen.
const PLAN_SETTINGS_KEY: &str = "study_plan_settings";
const FLASHCARDS_SETTINGS_KEY: &str = "study_flashcards_settings";
const QUIZ_SETTINGS_KEY: &str = "study_quiz_settings";

/// Write a generated artifact and record the settings it was generated under,
/// in that order: the settings describe the file, so a crash between the two
/// leaves stale settings rather than settings for a file that isn't there.
fn store_generated(
    dir: &Path,
    file: &str,
    key: &str,
    contents: &str,
    settings: &StudySettings,
) -> Result<(), String> {
    crate::store::write(&dir.join(file), contents)?;
    crate::library::update_meta(dir, |meta| {
        meta.insert(key.into(), serde_json::to_value(settings).unwrap());
    })
}

/// The settings the cached copy of `key` was generated under, if any.
fn cached_settings(dir: &Path, key: &str) -> Option<StudySettings> {
    serde_json::from_value(crate::library::read_meta(dir).get(key)?.clone()).ok()
}

/// A cached artifact is only a hit when it was generated under the same
/// settings. Asking for hard questions and being handed the easy ones still on
/// disk would read as the difficulty control doing nothing.
fn cached_under(dir: &Path, file: &str, key: &str, settings: &StudySettings) -> Option<String> {
    if cached_settings(dir, key)? != *settings {
        return None;
    }
    let text = std::fs::read_to_string(dir.join(file)).ok()?;
    (!text.trim().is_empty()).then_some(text)
}

pub fn generate_study_plan(
    id: &str,
    settings: &StudySettings,
    force: bool,
) -> Result<String, String> {
    let settings = settings.sanitized();
    let dir = crate::library::meeting_dir(id)?;

    if !force {
        if let Some(cached) = cached_under(&dir, PLAN_FILE, PLAN_SETTINGS_KEY, &settings) {
            return Ok(cached);
        }
    }

    let source = crate::library::source_text(id)?;
    let model = crate::model::ensure_chat_model()?;
    let plan = crate::chat::generate_study_plan(&model, &source, &settings)?;

    store_generated(&dir, PLAN_FILE, PLAN_SETTINGS_KEY, &plan, &settings)?;
    Ok(plan)
}

pub fn generate_flashcards(
    id: &str,
    settings: &StudySettings,
    force: bool,
) -> Result<Vec<Flashcard>, String> {
    let settings = settings.sanitized();
    let dir = crate::library::meeting_dir(id)?;

    if !force {
        if let Some(cached) = cached_under(&dir, FLASHCARDS_FILE, FLASHCARDS_SETTINGS_KEY, &settings)
        {
            if let Ok(cards) = serde_json::from_str::<Vec<Flashcard>>(&cached) {
                return Ok(cards);
            }
        }
    }

    let source = crate::library::source_text(id)?;
    let model = crate::model::ensure_chat_model()?;
    let cards = crate::chat::generate_flashcards(&model, &source, &settings)?;

    let json = serde_json::to_string_pretty(&cards)
        .map_err(|e| format!("could not save the flashcards: {e}"))?;
    store_generated(
        &dir,
        FLASHCARDS_FILE,
        FLASHCARDS_SETTINGS_KEY,
        &json,
        &settings,
    )?;
    Ok(cards)
}

pub fn generate_quiz(
    id: &str,
    settings: &StudySettings,
    force: bool,
) -> Result<Vec<QuizQuestion>, String> {
    let settings = settings.sanitized();
    let dir = crate::library::meeting_dir(id)?;

    if !force {
        if let Some(cached) = cached_under(&dir, QUIZ_FILE, QUIZ_SETTINGS_KEY, &settings) {
            if let Ok(questions) = serde_json::from_str::<Vec<QuizQuestion>>(&cached) {
                return Ok(questions);
            }
        }
    }

    let source = crate::library::source_text(id)?;
    let model = crate::model::ensure_chat_model()?;
    let questions = crate::chat::generate_quiz(&model, &source, &settings)?;

    let json = serde_json::to_string_pretty(&questions)
        .map_err(|e| format!("could not save the quiz: {e}"))?;
    store_generated(&dir, QUIZ_FILE, QUIZ_SETTINGS_KEY, &json, &settings)?;
    Ok(questions)
}

/// What the Study tab shows on open. Nothing generated yet is `None`, not an
/// error — the tab is opened far more often than it is generated into.
pub fn cached_study_plan(id: &str) -> Result<Option<String>, String> {
    let dir = crate::library::meeting_dir(id)?;
    Ok(std::fs::read_to_string(dir.join(PLAN_FILE))
        .ok()
        .filter(|t| !t.trim().is_empty()))
}

pub fn cached_flashcards(id: &str) -> Result<Option<Vec<Flashcard>>, String> {
    let dir = crate::library::meeting_dir(id)?;
    Ok(std::fs::read_to_string(dir.join(FLASHCARDS_FILE))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok()))
}

pub fn cached_quiz(id: &str) -> Result<Option<Vec<QuizQuestion>>, String> {
    let dir = crate::library::meeting_dir(id)?;
    Ok(std::fs::read_to_string(dir.join(QUIZ_FILE))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok()))
}

/// The settings the tab should reopen with: whatever produced the material
/// already on disk. All three are generated together, so the plan's copy
/// speaks for the set.
pub fn last_settings(id: &str) -> Result<Option<StudySettings>, String> {
    let dir = crate::library::meeting_dir(id)?;
    Ok(cached_settings(&dir, PLAN_SETTINGS_KEY)
        .or_else(|| cached_settings(&dir, FLASHCARDS_SETTINGS_KEY))
        .or_else(|| cached_settings(&dir, QUIZ_SETTINGS_KEY)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of this test's own. These tests pass the path straight to
    /// the functions under test, so unlike `library.rs`'s they never touch
    /// `$HOME` and never need its lock.
    /// `store::write` refuses process-wide once a `store` test trips the
    /// data-version lock, so any test that writes has to hold the same lock
    /// `store.rs`, `library.rs` and `homework.rs` serialize on.
    fn with_store<T>(f: impl FnOnce() -> T) -> T {
        let _guard = crate::settings::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        f()
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "oatmeal-study-{}-{}-{:?}",
            tag,
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn settings(count: u32, difficulty: Difficulty, focus: &str) -> StudySettings {
        StudySettings {
            count,
            difficulty,
            topic_focus: focus.into(),
        }
    }

    #[test]
    fn a_count_outside_the_offered_range_is_clamped() {
        assert_eq!(settings(0, Difficulty::Easy, "").sanitized().count, MIN_COUNT);
        assert_eq!(
            settings(9_000, Difficulty::Easy, "").sanitized().count,
            MAX_COUNT
        );
        assert_eq!(settings(10, Difficulty::Easy, "").sanitized().count, 10);
    }

    /// A focus of spaces is not a focus, and has to compare equal to a blank
    /// one or the cache would miss every time the field was cleared by typing.
    #[test]
    fn a_blank_topic_focus_is_normalized_away() {
        assert_eq!(
            settings(10, Difficulty::Medium, "   ").sanitized().topic_focus,
            ""
        );
        assert_eq!(
            settings(10, Difficulty::Medium, "  pricing ")
                .sanitized()
                .topic_focus,
            "pricing"
        );
    }

    #[test]
    fn a_cache_written_under_other_settings_is_not_a_hit() {
        with_store(|| {
            let dir = temp_dir("settings-mismatch");
            let easy = settings(10, Difficulty::Easy, "");
            let hard = settings(10, Difficulty::Hard, "");

            store_generated(dir.as_path(), PLAN_FILE, PLAN_SETTINGS_KEY, "# plan", &easy)
                .expect("store");

            assert_eq!(
                cached_under(dir.as_path(), PLAN_FILE, PLAN_SETTINGS_KEY, &easy).as_deref(),
                Some("# plan"),
            );
            assert!(
                cached_under(dir.as_path(), PLAN_FILE, PLAN_SETTINGS_KEY, &hard).is_none(),
                "harder questions must not be served from the easy ones on disk",
            );
        });
    }

    /// Written before the settings key existed, or by a build that didn't write
    /// one: the file is shown when the tab opens, but it is not a cache hit, so
    /// Generate still runs the model rather than handing back unknown material.
    #[test]
    fn a_file_with_no_settings_recorded_is_shown_but_not_reused() {
        let dir = temp_dir("orphan-file");
        std::fs::write(dir.as_path().join(PLAN_FILE), "# orphan").expect("write");

        assert!(cached_under(dir.as_path(), PLAN_FILE, PLAN_SETTINGS_KEY, &settings(10, Difficulty::Easy, "")).is_none());
        assert_eq!(
            std::fs::read_to_string(dir.as_path().join(PLAN_FILE)).unwrap(),
            "# orphan"
        );
    }

    #[test]
    fn settings_survive_a_round_trip_through_meta_json() {
        with_store(|| {
            let dir = temp_dir("meta-round-trip");
            let asked = settings(17, Difficulty::Hard, "pricing");

            store_generated(dir.as_path(), QUIZ_FILE, QUIZ_SETTINGS_KEY, "[]", &asked)
                .expect("store");

            assert_eq!(cached_settings(dir.as_path(), QUIZ_SETTINGS_KEY), Some(asked));
        });
    }

    /// `meta.json` is shared with the title and the notes template, so writing
    /// study settings must merge rather than replace.
    #[test]
    fn storing_study_settings_leaves_the_rest_of_meta_alone() {
        with_store(|| {
            let dir = temp_dir("meta-merge");
            crate::library::update_meta(dir.as_path(), |meta| {
                meta.insert("title".into(), "Kickoff".into());
            })
            .expect("seed");

            store_generated(
                dir.as_path(),
                PLAN_FILE,
                PLAN_SETTINGS_KEY,
                "# plan",
                &settings(10, Difficulty::Easy, ""),
            )
            .expect("store");

            let meta = crate::library::read_meta(dir.as_path());
            assert_eq!(meta.get("title").and_then(|v| v.as_str()), Some("Kickoff"));
            assert!(meta.contains_key(PLAN_SETTINGS_KEY));
        });
    }
}
