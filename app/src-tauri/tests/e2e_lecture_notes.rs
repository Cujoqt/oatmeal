// End-to-end check for the Lecture template — proves the *real* local model
// actually emits the markers the frontend parses (LaTeX delimiters, the
// review-questions heading, '> Solution:' lines), which no unit test can
// cover since those all stub the model.
//
// Ignored by default: it needs the chat model resident (~1.9 GB, downloaded on
// first use) and runs real inference. Run explicitly with:
//
//     cargo test --test e2e_lecture_notes -- --ignored --nocapture

use oatmeal_app_lib::{chat, model};

#[test]
#[ignore = "loads the ~1.9 GB chat model and runs real inference"]
fn a_math_lecture_is_written_up_with_latex_and_review_questions() {
    let path = model::ensure_chat_model().expect("chat model download/lookup failed");

    let transcript = "\
        Today we are doing the power rule. The derivative of x to the n is n times x to the \
        n minus one. So the derivative of x squared is 2 x. The derivative of x cubed is 3 x \
        squared. Now let us integrate. The integral from zero to two of 3 x squared d x is x \
        cubed evaluated from zero to two, which is eight. Next the limit as h goes to zero of \
        f of x plus h minus f of x over h. That is the definition of the derivative.";

    let notes = chat::write_notes(&path, transcript, chat::Template::Lecture)
        .expect("write_notes failed");

    println!("{notes}");

    assert!(notes.contains("## Review questions"), "no review section");
    assert!(
        notes.contains(r"\(") || notes.contains(r"\["),
        "no LaTeX math delimiters"
    );
    assert!(notes.contains("> Solution:"), "no solution lines");
    assert!(!notes.contains('$'), "model used dollar delimiters");
}
