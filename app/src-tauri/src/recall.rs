// Ask a question across the whole library.
//
// The per-meeting ask can only help someone who already knows which meeting
// holds the answer, and usually they don't — "what did we decide about
// pricing?" is a question about the library, not about a meeting. This module
// picks the meetings most likely to contain the answer, assembles as much of
// them as the context window will hold, and hands that to the model.
//
// Retrieval is deliberately plain: term overlap against what is already on
// disk, no embeddings and no index. `library::search_meetings` cannot be
// reused for this — it looks for the literal phrase, and no transcript
// contains the sentence "what did we decide about pricing".
//
// The one thing this must never do is answer with no sources. A confident
// paragraph about a meeting that never happened is worse than "I couldn't find
// it", so when nothing scores, the model is not called at all.

use std::path::Path;

use serde::Serialize;

use crate::chat::{CHARS_PER_TOKEN, MAX_TOKENS, N_CTX};
use crate::library::{self, Meeting};

/// Room set aside for the system prompt, the question, and the `[1] Title …`
/// header above each excerpt. Deliberately generous: overrunning the context
/// window fails the whole answer, while losing a few hundred characters of
/// transcript costs almost nothing.
const PROMPT_OVERHEAD_TOKENS: usize = 512;

/// Characters of meeting text the prompt may carry. The context window has to
/// hold the prompt *and* the answer the model writes into it, so the budget is
/// the window minus everything else that has to fit:
/// (8192 − 1024 − 512) tokens × 4 chars = 26624 characters.
const CONTEXT_BUDGET_CHARS: usize =
    (N_CTX as usize - MAX_TOKENS - PROMPT_OVERHEAD_TOKENS) * CHARS_PER_TOKEN;

/// How many meetings may be quoted. Past a handful the excerpts get too short
/// to say anything, and the citation list stops being readable.
const MAX_MEETINGS: usize = 5;

/// No meeting may take more than an even share of the budget, so one two-hour
/// transcript cannot crowd the other four out of the prompt entirely.
const PER_MEETING_CHARS: usize = CONTEXT_BUDGET_CHARS / MAX_MEETINGS;

/// Shortest run of characters worth treating as a search term.
const MIN_TERM_LEN: usize = 2;

/// A term repeated forty times shouldn't beat a meeting that matches every
/// term once, so each term's contribution to the score saturates.
const MAX_HITS_PER_TERM: usize = 5;

/// A term in the title says more than the same term buried in an hour of ASR.
const TITLE_WEIGHT: usize = 3;

/// What the user is told when their question matches nothing on disk. The
/// model never sees the question in that case.
const NO_MATCH: &str =
    "I couldn't find anything about that in your meetings. Try different words, \
     or check that the meeting you have in mind was transcribed.";

/// Words carried by the shape of a question rather than its subject. Dropping
/// them is the whole difference between matching "decide"/"pricing" and
/// matching nothing at all.
const STOPWORDS: &[&str] = &[
    "a", "about", "after", "again", "all", "also", "am", "an", "and", "any", "anyone", "are",
    "around", "as", "at", "back", "be", "because", "been", "before", "being", "both", "but", "by",
    "can", "come", "could", "did", "do", "does", "doing", "done", "down", "each", "even", "ever",
    "every", "for", "from", "get", "give", "go", "going", "gone", "got", "had", "has", "have",
    "he", "her", "here", "hers", "him", "his", "how", "i", "if", "in", "into", "is", "it", "its",
    "just", "know", "like", "make", "many", "may", "me", "might", "mine", "more", "most", "much",
    "must", "my", "need", "no", "not", "now", "of", "off", "on", "one", "only", "or", "other",
    "our", "ours", "out", "over", "own", "put", "said", "same", "say", "says", "see", "she",
    "should", "so", "some", "still", "such", "take", "tell", "than", "that", "the", "their",
    "them", "then", "there", "these", "they", "thing", "things", "think", "this", "those",
    "through", "to", "too", "up", "us", "use", "very", "want", "was", "we", "well", "were",
    "what", "when", "where", "which", "while", "who", "whom", "why", "will", "with", "would",
    "yes", "you", "your", "yours",
];

/// A meeting the answer drew on, in the order it was quoted. The UI turns each
/// of these into a chip that opens the meeting.
#[derive(Debug, Clone, Serialize)]
pub struct Source {
    pub id: String,
    pub title: String,
    pub started_at: String,
}

/// An answer written from the library, plus the meetings behind it.
#[derive(Debug, Clone, Serialize)]
pub struct LibraryAnswer {
    pub answer: String,
    pub sources: Vec<Source>,
}

/// The meaningful words of `question`, lowercased and deduplicated.
fn terms(question: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in question.split(|c: char| !c.is_alphanumeric()) {
        let term = raw.to_lowercase();
        if term.len() < MIN_TERM_LEN || STOPWORDS.contains(&term.as_str()) {
            continue;
        }
        if !out.contains(&term) {
            out.push(term);
        }
    }
    out
}

/// How many times `term` starts a word in `text`. Both must already be
/// lowercase.
///
/// Plain substring counting would score "art" for every "start". Requiring a
/// word boundary in front keeps that out while still letting "price" match
/// "prices" and "pricing" — the only stemming this needs.
fn word_start_hits(text: &str, term: &str) -> usize {
    text.match_indices(term)
        .filter(|(i, _)| {
            !text[..*i]
                .chars()
                .next_back()
                .map(|c| c.is_alphanumeric())
                .unwrap_or(false)
        })
        .count()
}

/// How well a meeting answers to `terms`. Zero means it is not a candidate.
fn score(title: &str, notes: &str, transcript: &str, terms: &[String]) -> usize {
    let title = title.to_lowercase();
    let body = format!("{}\n{}", notes.to_lowercase(), transcript.to_lowercase());
    terms
        .iter()
        .map(|term| {
            let in_body = word_start_hits(&body, term).min(MAX_HITS_PER_TERM);
            let in_title = word_start_hits(&title, term).min(1) * TITLE_WEIGHT;
            in_body + in_title
        })
        .sum()
}

/// The first of `names` in `dir` that holds something, or an empty string.
fn read_first_nonempty(dir: &Path, names: &[&str]) -> String {
    for name in names {
        if let Ok(text) = std::fs::read_to_string(dir.join(name)) {
            if !text.trim().is_empty() {
                return text;
            }
        }
    }
    String::new()
}

/// Largest index `i <= at` that `s` may be sliced at.
fn floor_boundary(s: &str, at: usize) -> usize {
    let mut i = at.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Up to `cap` characters of `text`, taken around the earliest place a query
/// term appears rather than from the top.
///
/// A two-hour transcript opens with people joining the call; the part that
/// mentions the question is what the model needs, and it is rarely first.
fn excerpt_around(text: &str, terms: &[String], cap: usize) -> String {
    if text.len() <= cap {
        return text.to_string();
    }
    let lower = text.to_lowercase();
    // Lowercasing can change a string's length (a handful of characters expand),
    // which would make offsets found in `lower` wrong for `text`. Rare enough to
    // just fall back to the opening when it happens.
    if lower.len() != text.len() {
        return text[..floor_boundary(text, cap)].to_string();
    }
    let hit = terms
        .iter()
        .filter_map(|term| lower.find(term.as_str()))
        .min()
        .unwrap_or(0);

    // Keep a little of what led up to the match, so the excerpt has context.
    let start = hit.saturating_sub(cap / 3).min(text.len() - cap);
    let start = floor_boundary(text, start);
    let end = floor_boundary(text, start + cap);
    text[start..end].to_string()
}

/// Pick the meetings most likely to answer `question` and lay them out as the
/// excerpt block the model reads. Returns the block and the meetings quoted in
/// it, best match first; both are empty when nothing matches.
fn gather(question: &str) -> (String, Vec<Source>) {
    let terms = terms(question);
    if terms.is_empty() {
        return (String::new(), Vec::new());
    }

    // (score, meeting, the text to quote from it)
    let mut ranked: Vec<(usize, Meeting, String)> = Vec::new();
    for meeting in library::list_meetings() {
        let dir = Path::new(&meeting.dir);
        // The model's write-up first, then whatever was typed during the
        // meeting — both are "notes" as far as this is concerned.
        let notes = read_first_nonempty(dir, &["summary.md", "notes.md"]);
        let transcript = std::fs::read_to_string(dir.join("transcript.md"))
            .map(|md| library::strip_transcript_markup(&md))
            .unwrap_or_default();

        // Scored against everything on disk: a meeting whose only mention of
        // pricing is in the transcript still has to be findable.
        let score = score(&meeting.title, &notes, &transcript, &terms);
        if score == 0 {
            continue;
        }
        // Quoted from the notes when there are any. They are already condensed,
        // so the same number of characters carries far more of the meeting than
        // raw ASR output does.
        let text = if notes.trim().is_empty() { transcript } else { notes };
        if text.trim().is_empty() {
            continue;
        }
        ranked.push((score, meeting, text));
    }

    // `list_meetings` is newest first and `sort_by` is stable, so meetings that
    // score the same stay in recency order.
    ranked.sort_by(|a, b| b.0.cmp(&a.0));

    let mut context = String::new();
    let mut sources: Vec<Source> = Vec::new();
    let mut remaining = CONTEXT_BUDGET_CHARS;

    for (_, meeting, text) in ranked.into_iter().take(MAX_MEETINGS) {
        let header = format!(
            "[{}] {} ({})\n",
            sources.len() + 1,
            meeting.title,
            meeting.started_at
        );
        // The header and the blank line after the excerpt come out of the same
        // budget, so the assembled block can never exceed it.
        let room = remaining.saturating_sub(header.len() + 2);
        let cap = PER_MEETING_CHARS.min(room);
        if cap == 0 {
            break;
        }
        let excerpt = excerpt_around(text.trim(), &terms, cap);
        if excerpt.trim().is_empty() {
            continue;
        }
        let entry = format!("{header}{excerpt}\n\n");
        remaining -= entry.len();
        context.push_str(&entry);
        sources.push(Source {
            id: meeting.id,
            title: meeting.title,
            started_at: meeting.started_at,
        });
    }

    (context, sources)
}

/// Answer `question` from the library, streaming the reply as it is written.
///
/// When nothing on disk matches, this returns a plain "couldn't find it"
/// without loading the model: an answer with no sources behind it is the one
/// failure this feature must not have.
pub fn answer(question: &str, on_token: &mut dyn FnMut(&str)) -> Result<LibraryAnswer, String> {
    let question = question.trim();
    if question.is_empty() {
        return Err("ask a question first".into());
    }

    let (context, sources) = gather(question);
    if sources.is_empty() {
        return Ok(LibraryAnswer {
            answer: NO_MATCH.into(),
            sources,
        });
    }

    let model = crate::model::ensure_chat_model()?;
    let answer = crate::chat::answer_from_library(&model, &context, question, on_token)?;
    Ok(LibraryAnswer { answer, sources })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::recordings_root;

    /// `$HOME` is process-global, so these tests can't overlap — not with each
    /// other and not with `library.rs`'s, hence the shared lock.
    use crate::settings::HOME_ENV_LOCK as HOME_LOCK;

    fn with_temp_home<T>(f: impl FnOnce() -> T) -> T {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = std::env::temp_dir().join(format!(
            "oatmeal-recall-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let prev = std::env::var("HOME").ok();
        std::env::set_var("HOME", &home);

        let out = f();

        match prev {
            Some(p) => std::env::set_var("HOME", p),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(&home);
        out
    }

    /// Lay a meeting out on disk the way `session.rs` does. `files` is
    /// `(name, contents)` — `transcript.md`, `summary.md`, `notes.md`.
    fn seed(id: &str, files: &[(&str, &str)]) {
        let dir = recordings_root().join(id);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, body) in files {
            std::fs::write(dir.join(name), body).unwrap();
        }
    }

    fn transcript(title: &str, body: &str) -> String {
        format!("# {title}\n\n_Recorded 2026-07-24 10:33_\n\n## Transcript\n\n**[0:00]** {body}\n")
    }

    fn ids(sources: &[Source]) -> Vec<&str> {
        sources.iter().map(|s| s.id.as_str()).collect()
    }

    #[test]
    fn a_question_keeps_its_subject_and_drops_its_scaffolding() {
        assert_eq!(terms("What did we decide about pricing?"), ["decide", "pricing"]);
        // Repeats collapse, case is folded, punctuation splits.
        assert_eq!(terms("Pricing, pricing — PRICING!"), ["pricing"]);
        // A question made entirely of stopwords has nothing to search for.
        assert!(terms("what did we do about it?").is_empty());
        assert!(terms("   ").is_empty());
    }

    #[test]
    fn ranks_by_term_overlap_where_phrase_search_finds_nothing() {
        with_temp_home(|| {
            seed(
                "20260701-090000-pricing-review",
                &[
                    ("transcript.md", &transcript("Pricing review", "we went round on the pricing tiers and decided to hold at twenty nine dollars")),
                ],
            );
            seed(
                "20260702-090000-standup",
                &[(
                    "transcript.md",
                    // Thick with the words a question is made of, and with none
                    // of the words the question is *about*.
                    &transcript("Standup", "so what did we do about the deploys, we did them, and what about the flaky test"),
                )],
            );
            seed(
                "20260703-090000-roadmap",
                &[(
                    "transcript.md",
                    &transcript("Roadmap", "we decided to push the mobile client to Q4"),
                )],
            );

            let question = "What did we decide about pricing?";

            // The gap this feature exists to close: the literal phrase is in no
            // transcript, so the existing search returns nothing.
            assert!(
                library::search_meetings(question).is_empty(),
                "phrase search should find nothing — that is the whole problem"
            );

            let (context, sources) = gather(question);
            assert_eq!(
                ids(&sources),
                ["20260701-090000-pricing-review", "20260703-090000-roadmap"],
                "pricing first (both terms), roadmap second (only 'decided')"
            );
            // The standup matches neither term and must not be quoted at all.
            assert!(!context.contains("flaky test"), "got: {context}");
            assert!(context.contains("twenty nine dollars"), "got: {context}");
        });
    }

    #[test]
    fn notes_are_quoted_in_preference_to_the_transcript() {
        with_temp_home(|| {
            seed(
                "20260704-090000-pricing",
                &[
                    ("transcript.md", &transcript("Pricing", "um so uh the pricing thing yeah")),
                    ("summary.md", "## Decisions\n\n- Pricing holds at $29 for the year.\n"),
                ],
            );

            let (context, sources) = gather("what did we decide about pricing");
            assert_eq!(sources.len(), 1);
            assert!(context.contains("Pricing holds at $29"), "got: {context}");
            assert!(!context.contains("um so uh"), "the transcript should not be quoted: {context}");
        });
    }

    #[test]
    fn a_meeting_with_no_notes_falls_back_to_its_transcript() {
        with_temp_home(|| {
            seed(
                "20260705-090000-pricing",
                &[("transcript.md", &transcript("Pricing", "the pricing page ships Tuesday"))],
            );

            let (context, sources) = gather("pricing");
            assert_eq!(sources.len(), 1);
            assert!(context.contains("the pricing page ships Tuesday"), "got: {context}");
        });
    }

    #[test]
    fn one_enormous_meeting_cannot_crowd_out_the_others() {
        with_temp_home(|| {
            // Far more pricing talk than the whole budget, in one meeting.
            let flood = "pricing pricing pricing ".repeat(CONTEXT_BUDGET_CHARS / 10);
            seed(
                "20260710-090000-marathon",
                &[("transcript.md", &transcript("Marathon", &flood))],
            );
            for i in 1..=4 {
                seed(
                    &format!("2026070{i}-090000-small"),
                    &[(
                        "transcript.md",
                        &transcript("Small", &format!("a quick word about pricing number {i}")),
                    )],
                );
            }

            let (context, sources) = gather("pricing");
            assert_eq!(sources.len(), 5, "every matching meeting must still be quoted");
            assert_eq!(sources[0].id, "20260710-090000-marathon", "the flood still ranks first");
            for i in 1..=4 {
                assert!(
                    context.contains(&format!("pricing number {i}")),
                    "meeting {i} was crowded out of: {} chars",
                    context.len()
                );
            }
            // Nothing may take more than its share, and the whole block has to
            // leave the model room to answer.
            // Each entry is a `[n] Title (date)` header line then the excerpt.
            let biggest = context
                .split("\n\n")
                .filter_map(|entry| entry.split_once('\n'))
                .map(|(_header, excerpt)| excerpt.len())
                .max()
                .unwrap_or(0);
            assert!(biggest <= PER_MEETING_CHARS, "one excerpt took {biggest} chars");
            assert!(
                context.len() <= CONTEXT_BUDGET_CHARS,
                "context is {} chars, budget is {CONTEXT_BUDGET_CHARS}",
                context.len()
            );
        });
    }

    #[test]
    fn only_a_bounded_number_of_meetings_are_quoted() {
        with_temp_home(|| {
            for i in 1..=8 {
                seed(
                    &format!("2026080{i}-090000-pricing"),
                    &[("transcript.md", &transcript("Pricing", "we talked pricing again"))],
                );
            }
            let (_, sources) = gather("pricing");
            assert_eq!(sources.len(), MAX_MEETINGS);
            // Equal scores keep the library's newest-first order.
            assert_eq!(sources[0].id, "20260808-090000-pricing");
        });
    }

    #[test]
    fn the_budget_leaves_the_model_room_to_answer() {
        // The prompt and the answer share one window; this is the arithmetic
        // the assembled context is sized against.
        assert!(
            CONTEXT_BUDGET_CHARS + (MAX_TOKENS + PROMPT_OVERHEAD_TOKENS) * CHARS_PER_TOKEN
                <= N_CTX as usize * CHARS_PER_TOKEN,
            "the context budget does not leave room for {MAX_TOKENS} tokens of answer"
        );
    }

    #[test]
    fn the_excerpt_is_taken_around_the_match_not_the_opening() {
        with_temp_home(|| {
            let filler = "hello can everyone hear me alright ".repeat(400);
            seed(
                "20260712-090000-long",
                &[(
                    "transcript.md",
                    &transcript("Long", &format!("{filler} we settled the pricing at twenty nine {filler}")),
                )],
            );
            let (context, sources) = gather("pricing");
            assert_eq!(sources.len(), 1);
            assert!(
                context.contains("settled the pricing at twenty nine"),
                "the excerpt missed the match; it is {} chars",
                context.len()
            );
        });
    }

    #[test]
    fn nothing_relevant_means_no_sources_and_the_model_is_never_called() {
        with_temp_home(|| {
            seed(
                "20260713-090000-standup",
                &[("transcript.md", &transcript("Standup", "deploys are green"))],
            );

            // There is no chat model under this temp HOME, so reaching the model
            // at all would fail rather than return Ok.
            let mut tokens = String::new();
            let got = answer("What did we decide about pricing?", &mut |t| tokens.push_str(t))
                .expect("a question with no match is not an error");

            assert!(got.sources.is_empty());
            assert_eq!(got.answer, NO_MATCH);
            assert!(tokens.is_empty(), "nothing should have been generated");
        });
    }

    #[test]
    fn an_empty_question_is_refused() {
        with_temp_home(|| {
            assert!(answer("   ", &mut |_| {}).is_err());
        });
    }
}
