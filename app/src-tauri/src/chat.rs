// The local language model — note summaries and meeting recaps.
//
// Whisper turns speech into text; this turns that text into something worth
// reading. It runs llama.cpp on the Metal GPU with a small instruct model, so
// like every other part of Oatmeal it works with the network off and nothing
// leaves the machine.
//
// The model is loaded lazily on first use and then kept resident: loading costs
// seconds and a couple of gigabytes, and a recap you ask for mid-meeting has to
// come back promptly. A fresh context is created per generation so one answer
// can't leak into the next.

use std::num::NonZeroU32;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaChatMessage, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;

/// Context window. Long meetings are summarized in chunks rather than by
/// growing this — 8k keeps the KV cache small enough to stay quick.
const N_CTX: u32 = 8192;
/// Cap on generated tokens, so a degenerate loop can't run forever.
const MAX_TOKENS: usize = 1024;
/// Roughly four characters per token; used to decide when a transcript needs
/// chunking rather than tokenizing it twice to find out.
const CHARS_PER_TOKEN: usize = 4;

/// llama.cpp's backend may only be initialized once per process.
fn backend() -> Result<&'static LlamaBackend, String> {
    static BACKEND: OnceLock<Result<LlamaBackend, String>> = OnceLock::new();
    BACKEND
        .get_or_init(|| {
            let mut b = LlamaBackend::init().map_err(|e| format!("init llama backend: {e}"))?;
            // llama.cpp logs prolifically to stderr; quiet it.
            b.void_logs();
            Ok(b)
        })
        .as_ref()
        .map_err(|e| e.clone())
}

/// The loaded chat model. Cloning is not possible; use [`with_model`].
struct Loaded {
    model: LlamaModel,
}

fn loaded() -> &'static Mutex<Option<Loaded>> {
    static LOADED: OnceLock<Mutex<Option<Loaded>>> = OnceLock::new();
    LOADED.get_or_init(|| Mutex::new(None))
}

/// Whether the model is already resident, i.e. whether the next call is instant
/// or has to pay the load cost first.
pub fn is_loaded() -> bool {
    loaded().lock().map(|l| l.is_some()).unwrap_or(false)
}

/// Load the model at `path` if it isn't already, then run `f` against it.
fn with_model<T>(path: &Path, f: impl FnOnce(&LlamaModel) -> Result<T, String>) -> Result<T, String> {
    let backend = backend()?;
    let mut slot = loaded().lock().map_err(|_| "chat model state poisoned")?;

    if slot.is_none() {
        if !path.exists() {
            return Err(format!("no chat model at {}", path.display()));
        }
        // Offload everything to Metal; these models are small enough to fit and
        // CPU-only generation is too slow to feel interactive.
        let params = LlamaModelParams::default().with_n_gpu_layers(1_000_000);
        let model = LlamaModel::load_from_file(backend, path, &params)
            .map_err(|e| format!("load chat model: {e}"))?;
        *slot = Some(Loaded { model });
    }

    let loaded = slot.as_ref().expect("just loaded");
    f(&loaded.model)
}

/// Release the model, freeing its memory. Called when a long idle period makes
/// holding a couple of gigabytes rude.
pub fn unload() {
    if let Ok(mut slot) = loaded().lock() {
        *slot = None;
    }
}

/// Generate a reply to `user` under the guidance of `system`.
pub fn complete(model_path: &Path, system: &str, user: &str) -> Result<String, String> {
    complete_streaming(model_path, system, user, &mut |_| {})
}

/// Same generation, but each decoded piece is handed to `on_token` as it arrives.
///
/// A local model on a laptop produces a paragraph over several seconds. Waiting for
/// the whole thing before showing anything reads as a hang, so the caller that a
/// person is watching streams instead.
pub fn complete_streaming(
    model_path: &Path,
    system: &str,
    user: &str,
    on_token: &mut dyn FnMut(&str),
) -> Result<String, String> {
    with_model(model_path, |model| {
        let backend = backend()?;

        let template = model
            .chat_template(None)
            .map_err(|e| format!("model has no chat template: {e}"))?;
        let messages = vec![
            LlamaChatMessage::new("system".into(), system.into())
                .map_err(|e| format!("system message: {e}"))?,
            LlamaChatMessage::new("user".into(), user.into())
                .map_err(|e| format!("user message: {e}"))?,
        ];
        let prompt = model
            .apply_chat_template(&template, &messages, true)
            .map_err(|e| format!("apply chat template: {e}"))?;

        let tokens = model
            .str_to_token(&prompt, AddBos::Always)
            .map_err(|e| format!("tokenize prompt: {e}"))?;

        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(N_CTX))
            .with_n_batch(N_CTX);
        let mut ctx = model
            .new_context(backend, ctx_params)
            .map_err(|e| format!("create llama context: {e}"))?;

        let n_ctx = ctx.n_ctx() as usize;
        if tokens.len() + 64 >= n_ctx {
            return Err(format!(
                "prompt is {} tokens, which doesn't leave room in a {n_ctx}-token context",
                tokens.len()
            ));
        }

        let mut batch = LlamaBatch::new(n_ctx, 1);
        let last = tokens.len() - 1;
        for (i, token) in tokens.iter().enumerate() {
            // Only the final token needs its logits; the rest just prime the KV cache.
            batch
                .add(*token, i as i32, &[0], i == last)
                .map_err(|e| format!("build prompt batch: {e}"))?;
        }
        ctx.decode(&mut batch)
            .map_err(|e| format!("decode prompt: {e}"))?;

        // Low temperature: these are summarization tasks, where faithfulness
        // matters far more than variety.
        let mut sampler = LlamaSampler::chain_simple([
            LlamaSampler::top_k(40),
            LlamaSampler::top_p(0.9, 1),
            LlamaSampler::temp(0.3),
            LlamaSampler::dist(1234),
        ]);

        let mut out = String::new();
        let mut n_cur = batch.n_tokens();
        let mut decoder = encoding_rs::UTF_8.new_decoder();

        for _ in 0..MAX_TOKENS {
            let token = sampler.sample(&ctx, batch.n_tokens() - 1);
            sampler.accept(token);
            if model.is_eog_token(token) {
                break;
            }

            let bytes = model
                .token_to_piece_bytes(token, 64, false, None)
                .map_err(|e| format!("detokenize: {e}"))?;
            let mut piece = String::with_capacity(bytes.len());
            let _ = decoder.decode_to_string(&bytes, &mut piece, false);
            if !piece.is_empty() {
                on_token(&piece);
            }
            out.push_str(&piece);

            batch.clear();
            batch
                .add(token, n_cur, &[0], true)
                .map_err(|e| format!("build sampling batch: {e}"))?;
            n_cur += 1;

            if n_cur as usize >= n_ctx {
                break;
            }
            ctx.decode(&mut batch)
                .map_err(|e| format!("decode token: {e}"))?;
        }

        Ok(out.trim().to_string())
    })
}

// ── prompts ──────────────────────────────────────────────────────────────────

const NOTES_SYSTEM: &str = "\
You write meeting and lecture notes from raw transcripts. The transcript comes from \
automatic speech recognition, so it contains errors, false starts and no speaker labels — \
read through them and write what was actually meant.

Write in Markdown. Open with a one-paragraph summary of what the session was about and \
what came of it. Then, only where the material supports them, add sections: key points, \
decisions, action items, open questions. Use '## ' headings and '- ' bullets.

Be faithful to the transcript. Never invent names, numbers, dates or commitments that are \
not there. If the transcript is too short or too garbled to summarize, say so plainly in \
one sentence and stop.";

const RECAP_SYSTEM: &str = "\
You answer questions about a meeting or lecture that is being recorded right now, using \
only its transcript. The transcript comes from automatic speech recognition, so expect \
errors and no speaker labels.

Answer in two or three sentences unless the question demands more. Be concrete and quote \
specifics from the transcript where they help. If the transcript does not contain the \
answer, say so — do not guess.";

const CHUNK_SYSTEM: &str = "\
You are condensing one part of a long transcript so it can be summarized as a whole. \
Capture every substantive point, decision, name, number and commitment in compact prose. \
Do not editorialize and do not add a preamble.";

const FOLLOWUP_SYSTEM: &str = "\
You write a brief, friendly follow-up message summarizing what was discussed and any next \
steps, suitable to paste into an email or chat message. Do not invent facts not present in \
the notes you're given.

Plain prose, not Markdown — no headings or bullet asterisks. A short paragraph or two is \
enough; add a plain list of next steps only if the notes name any. No subject line, and no \
greeting or sign-off naming a specific person unless the notes do.";

const STANDUP_SYSTEM: &str = "\
You write standup notes from a raw meeting transcript. The transcript comes from automatic \
speech recognition, so it contains errors, false starts and no speaker labels — read through \
them and work out who is speaking from context.

Write in Markdown. For each person who gave an update, add a '## ' heading with their name \
(or 'Speaker' plus a number if a name never comes up) and '- ' bullets for what they did \
yesterday, what they're doing today, and any blockers — omit a bullet if the transcript didn't \
cover it. Close with a 'Blockers' section listing every blocker again in one place, or say \
there were none.

Be faithful to the transcript. Never invent names, numbers, dates or commitments that are \
not there. If the transcript is too short or too garbled to summarize, say so plainly in \
one sentence and stop.";

const ONE_ON_ONE_SYSTEM: &str = "\
You write notes from a 1:1 meeting transcript. The transcript comes from automatic speech \
recognition, so it contains errors, false starts and no speaker labels — read through them \
and write what was actually meant.

Write in Markdown. Open with a one-paragraph summary of how the conversation went. Then, only \
where the material supports them, add sections: talking points, feedback given, and \
follow-ups (commitments either person made for next time). Use '## ' headings and '- ' \
bullets.

Be faithful to the transcript. Never invent names, numbers, dates or commitments that are \
not there. If the transcript is too short or too garbled to summarize, say so plainly in \
one sentence and stop.";

const INTERVIEW_SYSTEM: &str = "\
You write interview debrief notes from a raw transcript. The transcript comes from automatic \
speech recognition, so it contains errors, false starts and no speaker labels — read through \
them and distinguish interviewer from candidate by context.

Write in Markdown. Open with a one-paragraph summary of the candidate's overall signal. Then, \
only where the material supports them, add sections: strengths, concerns, and a \
recommendation (hire, no hire, or more signal needed, with the reasoning in one or two \
sentences). Use '## ' headings and '- ' bullets.

Be faithful to the transcript. Never invent names, numbers, dates or claims that are not \
there. If the transcript is too short or too garbled to assess, say so plainly in one \
sentence and stop.";

/// Which shape of notes to write. `General` reproduces the original fixed
/// format; the others match the system prompt to the kind of meeting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Template {
    General,
    Standup,
    OneOnOne,
    Interview,
}

impl Default for Template {
    fn default() -> Self {
        Template::General
    }
}

impl Template {
    fn system_prompt(self) -> &'static str {
        match self {
            Template::General => NOTES_SYSTEM,
            Template::Standup => STANDUP_SYSTEM,
            Template::OneOnOne => ONE_ON_ONE_SYSTEM,
            Template::Interview => INTERVIEW_SYSTEM,
        }
    }
}

/// Write structured notes for a finished transcript, shaped by `template`.
///
/// Long transcripts exceed the context window, so they're condensed in passes:
/// each chunk is summarized on its own, then the summaries are written up
/// together. This is lossier than a single pass but degrades gracefully, which
/// matters for a two-hour lecture.
pub fn write_notes(model_path: &Path, transcript: &str, template: Template) -> Result<String, String> {
    let transcript = transcript.trim();
    if transcript.split_whitespace().count() < 20 {
        return Err("transcript is too short to summarize".into());
    }

    let budget = (N_CTX as usize - 1500) * CHARS_PER_TOKEN;
    if transcript.len() <= budget {
        return complete(model_path, template.system_prompt(), transcript);
    }

    let chunks = split_into_chunks(transcript, budget);
    let mut condensed = String::new();
    for (i, chunk) in chunks.iter().enumerate() {
        let part = complete(model_path, CHUNK_SYSTEM, chunk)?;
        condensed.push_str(&format!("\n\n--- part {} ---\n{}", i + 1, part));
    }
    complete(model_path, template.system_prompt(), condensed.trim())
}

/// Answer a question about a transcript, streaming the reply as it is generated.
pub fn recap(
    model_path: &Path,
    transcript: &str,
    question: &str,
    on_token: &mut dyn FnMut(&str),
) -> Result<String, String> {
    let transcript = transcript.trim();
    if transcript.is_empty() {
        return Err("nothing has been transcribed yet".into());
    }
    let budget = (N_CTX as usize - 1000) * CHARS_PER_TOKEN;
    // Questions like "what did I miss" are about the recent past, so when the
    // transcript overflows, keep the end rather than the beginning.
    let context = tail(transcript, budget);

    let user = format!("Transcript:\n{context}\n\nQuestion: {question}");
    complete_streaming(model_path, RECAP_SYSTEM, &user, on_token)
}

/// Draft a follow-up message from a meeting's notes, streaming the reply as it
/// is generated. Takes notes rather than the transcript — the source is already
/// a human-readable write-up, not raw ASR output.
pub fn draft_followup(
    model_path: &Path,
    notes: &str,
    on_token: &mut dyn FnMut(&str),
) -> Result<String, String> {
    let notes = notes.trim();
    if notes.is_empty() {
        return Err("this meeting has no notes yet".into());
    }
    let budget = (N_CTX as usize - 500) * CHARS_PER_TOKEN;
    let context = tail(notes, budget);

    let user = format!("Meeting notes:\n{context}");
    complete_streaming(model_path, FOLLOWUP_SYSTEM, &user, on_token)
}

/// The last `budget` bytes of `text`, snapped forward to a character boundary.
///
/// Slicing a transcript by byte offset panics the moment the cut lands inside a
/// multi-byte character — an em dash or an accent in someone's name is enough.
fn tail(text: &str, budget: usize) -> &str {
    if text.len() <= budget {
        return text;
    }
    let mut cut = text.len() - budget;
    while cut < text.len() && !text.is_char_boundary(cut) {
        cut += 1;
    }
    &text[cut..]
}

/// Split on paragraph boundaries where possible, falling back to a hard cut so a
/// transcript with no blank lines still makes progress.
fn split_into_chunks(text: &str, budget: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();

    for para in text.split("\n\n") {
        if current.len() + para.len() + 2 > budget && !current.is_empty() {
            chunks.push(std::mem::take(&mut current));
        }
        if para.len() > budget {
            // One enormous paragraph: cut it on char boundaries.
            let mut rest = para;
            while rest.len() > budget {
                let mut cut = budget;
                while cut > 0 && !rest.is_char_boundary(cut) {
                    cut -= 1;
                }
                chunks.push(rest[..cut].to_string());
                rest = &rest[cut..];
            }
            current.push_str(rest);
        } else {
            if !current.is_empty() {
                current.push_str("\n\n");
            }
            current.push_str(para);
        }
    }
    if !current.trim().is_empty() {
        chunks.push(current);
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_never_splits_a_character() {
        // The cut lands mid-character on purpose: "…" is three bytes.
        let text = "one … two … three";
        for budget in 1..text.len() {
            let got = tail(text, budget);
            assert!(text.ends_with(got), "{got:?} is not a suffix of {text:?}");
        }
        assert_eq!(tail("short", 99), "short");
    }

    #[test]
    fn chunks_respect_the_budget_and_lose_nothing() {
        let text = (0..50)
            .map(|i| format!("Paragraph number {i} with a little bit of text in it."))
            .collect::<Vec<_>>()
            .join("\n\n");

        let chunks = split_into_chunks(&text, 200);
        assert!(chunks.len() > 1, "expected the text to be split");
        for c in &chunks {
            assert!(c.len() <= 200, "chunk of {} exceeds budget", c.len());
        }
        // Every paragraph survives somewhere.
        let joined = chunks.join("\n\n");
        for i in 0..50 {
            assert!(joined.contains(&format!("Paragraph number {i} ")), "lost {i}");
        }
    }

    #[test]
    fn a_paragraph_larger_than_the_budget_is_cut_up() {
        let text = "x".repeat(1000);
        let chunks = split_into_chunks(&text, 300);
        assert!(chunks.len() >= 4);
        assert_eq!(chunks.concat().len(), 1000);
    }

    #[test]
    fn refuses_transcripts_with_nothing_in_them() {
        let err = write_notes(Path::new("/nonexistent"), "um, so, yeah", Template::General).unwrap_err();
        assert!(err.contains("too short"), "got: {err}");
    }
}
