// Rate limiting for the live panel's calls into the local model.
//
// Whisper and that model share the one GPU, so calling it too often would starve
// live transcription — and the transcript is what the user actually came for. Two
// features share this gate: auto-answering a spotted question, and converting
// spoken mathematics to LaTeX for math-lecture mode. Giving each its own gate
// would mean two independent six-second windows contending against the same
// device — twelve seconds of contention per minute against one transcript.
// Two limits keep either feature from taking the recording down with it: only
// one call runs at a time, and a minimum interval sits between the *starts* of
// consecutive calls, no matter which feature made them.

use std::time::{Duration, Instant};

/// Least time between the starts of two calls, regardless of which feature made
/// them.
pub const MIN_INTERVAL: Duration = Duration::from_secs(6);

/// Longest line worth sending to the model, whichever feature sends it.
/// Anything longer is almost always a run-on of misrecognized speech rather
/// than a real question or expression, and spending the GPU on it would delay
/// the next real one.
pub const MAX_QUESTION_CHARS: usize = 500;

/// Single-flight, rate-limited gate shared by every live-panel feature that calls
/// the local model. One instance lives in `AppState`; it is not `Clone`, so there
/// is exactly one owner of the "a call is running" fact, no matter how many
/// features claim it.
#[derive(Default)]
pub struct Gate {
    in_flight: bool,
    last_started: Option<Instant>,
}

impl Gate {
    /// Claim the gate for a call starting at `now`, returning whether the caller
    /// may proceed. Refuses — changing nothing — if a call is already running or
    /// the last one started less than `MIN_INTERVAL` ago, whichever feature
    /// started it. On `true` the caller must pair this with `finish()` when the
    /// call ends, success or failure, or the gate stays claimed forever.
    pub fn try_begin(&mut self, now: Instant) -> bool {
        if self.in_flight {
            return false;
        }
        if let Some(last) = self.last_started {
            if now.duration_since(last) < MIN_INTERVAL {
                return false;
            }
        }
        self.in_flight = true;
        self.last_started = Some(now);
        true
    }

    /// Release the gate. The interval still counts from `try_begin`, so a quick
    /// call waits out the rest of the window before the next one — from either
    /// feature — can start.
    pub fn finish(&mut self) {
        self.in_flight = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_one_answer_runs_at_a_time() {
        let mut g = Gate::default();
        let t = Instant::now();
        assert!(g.try_begin(t), "the first claim wins");
        assert!(!g.try_begin(t), "a second is refused while one is in flight");
        g.finish();
    }

    #[test]
    fn a_finished_answer_still_waits_out_the_interval() {
        let mut g = Gate::default();
        let t0 = Instant::now();
        assert!(g.try_begin(t0));
        g.finish();
        assert!(
            !g.try_begin(t0 + Duration::from_secs(1)),
            "finished, but too soon after the last start"
        );
        assert!(
            g.try_begin(t0 + MIN_INTERVAL),
            "allowed once the interval has passed"
        );
    }

    #[test]
    fn math_and_auto_answer_contend_for_the_same_gate() {
        let mut g = Gate::default();
        let t = Instant::now();
        assert!(g.try_begin(t), "an auto-answer claims it");
        assert!(!g.try_begin(t), "a math conversion cannot start alongside it");
        g.finish();
        assert!(!g.try_begin(t + Duration::from_secs(1)), "still inside the interval");
        assert!(g.try_begin(t + MIN_INTERVAL), "allowed once the interval passes");
    }

    #[test]
    fn the_interval_counts_from_the_start_not_the_finish() {
        let mut g = Gate::default();
        let t0 = Instant::now();
        assert!(g.try_begin(t0));
        g.finish();
        // The window is measured from t0, so exactly MIN_INTERVAL later is fine
        // even though `finish` happened at some unknown point in between.
        assert!(g.try_begin(t0 + MIN_INTERVAL));
    }
}
