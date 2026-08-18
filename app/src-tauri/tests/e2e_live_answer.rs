// End-to-end check for the live panel's auto-answer path — proves the *real*
// local model answers a spoken question from its own knowledge, which the unit
// tests (stubbed model) can't cover.
//
// Ignored by default: it needs the chat model resident (~1.9 GB, downloaded on
// first use) and runs real inference. Run explicitly with:
//
//     cargo test --test e2e_live_answer -- --ignored --nocapture
//
// No microphone, screen capture or audio is needed — it calls the answer path
// directly with a question, the way the panel does once it has detected one.

use std::time::Instant;

use oatmeal_app_lib::{chat, model};

#[test]
#[ignore = "loads the ~1.9 GB chat model and runs real inference"]
fn answers_a_general_knowledge_question_from_its_own_knowledge() {
    let path = model::ensure_chat_model().expect("chat model download/lookup failed");
    assert!(path.exists(), "chat model missing after ensure_chat_model");

    // The whole point of this path: the answer is NOT in the transcript. A
    // general-knowledge question with no supporting context still gets answered.
    let question = "What is the capital of France?";
    let started = Instant::now();
    let mut streamed = String::new();
    let answer = chat::answer_live(&path, "", question, &mut |piece| streamed.push_str(piece))
        .expect("answer_live returned an error");
    let elapsed = started.elapsed();

    eprintln!("Q: {question}\nA: {answer}\n({} ms)", elapsed.as_millis());

    assert!(!answer.trim().is_empty(), "answer was empty");
    assert!(
        answer.to_lowercase().contains("paris"),
        "expected the answer to name Paris, got: {answer:?}"
    );
    // The streamed pieces must reconstruct the returned answer, so the card the
    // user watches fill in matches the final text.
    assert_eq!(streamed.trim(), answer, "streamed tokens != returned answer");
    // Short by design — it appears live over a call. Generous ceiling; the point
    // is to catch a runaway, not to police length exactly.
    assert!(
        answer.chars().count() < 600,
        "answer is far longer than a live reply should be: {} chars",
        answer.chars().count()
    );
}

#[test]
#[ignore = "loads the ~1.9 GB chat model and runs real inference"]
fn transcript_context_does_not_block_a_knowledge_answer() {
    let path = model::ensure_chat_model().expect("chat model download/lookup failed");

    // A slice of unrelated meeting chatter is present as context. The grounded
    // recap path would refuse ("that didn't come up"); the live path must not —
    // it answers from what the model knows regardless of the transcript.
    let transcript = "So for the pizza order I think we said three larges, \
        two with pepperoni and one plain, and we should ask about gluten-free crust.";
    let question = "Who wrote the play Hamlet?";
    let answer = chat::answer_live(&path, transcript, question, &mut |_| {})
        .expect("answer_live returned an error");

    eprintln!("Q: {question}\nA: {answer}");
    assert!(
        answer.to_lowercase().contains("shakespeare"),
        "expected Shakespeare despite the pizza context, got: {answer:?}"
    );
}
