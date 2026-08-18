// Rate limiting for the live panel's auto-answers.
//
// When the panel spots a spoken question it asks the local model to answer it.
// Whisper and that model share the one GPU, so an answer per sentence would
// starve live transcription — and the transcript is what the user actually came
// for. Two limits keep the feature from taking the recording down with it: only
// one answer generates at a time, and a minimum interval sits between the *starts*
// of consecutive answers.

use std::time::{Duration, Instant};

/// Least time between the starts of two auto-answers.
pub const MIN_INTERVAL: Duration = Duration::from_secs(6);

/// Longest question worth answering. Anything longer is almost always a run-on of
/// misrecognized speech rather than a real question, and spending the GPU on it
/// would delay the answer to the next real one.
pub const MAX_QUESTION_CHARS: usize = 500;

/// Single-flight, rate-limited gate. One instance lives in `AppState`; it is not
/// `Clone`, so there is exactly one owner of the "an answer is running" fact.
#[derive(Default)]
pub struct Gate {
    in_flight: bool,
    last_started: Option<Instant>,
}

impl Gate {
    /// Claim the gate for an answer starting at `now`, returning whether the
    /// caller may proceed. Refuses — changing nothing — if an answer is already
    /// running or the last one started less than `MIN_INTERVAL` ago. On `true`
    /// the caller must pair this with `finish()` when the answer ends, success or
    /// failure, or the gate stays claimed forever.
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
    /// answer waits out the rest of the window before the next one can start.
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
