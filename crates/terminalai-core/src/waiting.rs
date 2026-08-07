//! Waiting for a session to reach a state — pure, no lock, no thread, no clock.
//!
//! The one primitive that turns a fleet API into something agents can
//! coordinate over: block until another session genuinely needs input, instead
//! of polling and guessing. Sits beside [`crate::admission`], [`crate::restart`]
//! and [`crate::context`] for the same reason — the decision is separable from
//! the machinery, and a policy interleaved with I/O cannot be stated.
//!
//! # The server never blocks
//!
//! The MCP server is a single-threaded loop over stdio lines. A tool that slept
//! until its condition came true would hold that loop, so one agent waiting on
//! another would stall *every* other read on the same server — the exact
//! opposite of a fleet primitive. So a wait that cannot be satisfied yet returns
//! immediately, saying how long is left and how soon to ask again, and the
//! client comes back. That is what the 2026-07-28 Multi Round-Trip Requests
//! pattern exists for, and it is why this is built on MRTR rather than on the
//! server-initiated requests that revision removed.
//!
//! # Waiting is a read
//!
//! It needs no write token. A wait never types into a session, never wakes one
//! and never answers a prompt — it observes. The read-only server is therefore
//! allowed to serve it, which matters because the coordinating agent is usually
//! not the one holding the operator's write token.

use std::time::Duration;

/// How soon a caller should ask again while a wait is unsatisfied.
///
/// Long enough that a stalled condition is not a busy loop over a pipe, short
/// enough that "another agent is now blocked" is acted on while it is still
/// true. A caller may ignore it; it is a hint, not a contract.
pub const RETRY_AFTER: Duration = Duration::from_millis(750);
/// Longest total wait a caller may ask for. A wait is a bounded question, and
/// an unbounded one is a hang that looks like a working call.
pub const MAX_WAIT: Duration = Duration::from_secs(30 * 60);
/// Used when the caller names no timeout.
pub const DEFAULT_WAIT: Duration = Duration::from_secs(5 * 60);

/// The states that mean a session is waiting on a person.
///
/// The default target, because "block until another agent genuinely needs
/// input" is the case this exists for. Kept as the status strings the wire
/// already uses, so a caller can name any of them explicitly too.
pub const ATTENTION_STATES: &[&str] = &["needs-approval", "needs-you", "awaiting-input"];

/// What a caller is waiting for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitRequest {
    pub session: String,
    /// Statuses that satisfy the wait. Empty means [`ATTENTION_STATES`].
    pub states: Vec<String>,
    /// How much of the caller's total wait is left.
    pub remaining: Duration,
}

/// How a wait resolved this time round.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitOutcome {
    /// The session is in one of the wanted states now.
    Reached { status: String },
    /// Not yet, and there is time left. The caller retries.
    Pending { remaining: Duration },
    /// The caller's own deadline passed. A complete answer, not an error: a
    /// bounded wait expiring is the bound working.
    TimedOut,
    /// No such session.
    ///
    /// Deliberately distinct from a timeout. Waiting the full five minutes on a
    /// mistyped id and then reporting "timed out" is the failure that makes a
    /// wait primitive untrustworthy — the caller cannot tell a slow condition
    /// from one that can never be true.
    Unknown,
}

/// Clamp a caller-supplied timeout into something answerable.
pub fn bounded_wait(requested: Option<Duration>) -> Duration {
    requested.unwrap_or(DEFAULT_WAIT).min(MAX_WAIT)
}

/// Decide a wait against the fleet as it is right now.
///
/// `current` is the session's status, or `None` when the fleet has no such
/// session. Nothing here reads a clock: `remaining` is measured by the caller,
/// which is what lets the whole rule be tested without sleeping.
pub fn evaluate(current: Option<&str>, request: &WaitRequest) -> WaitOutcome {
    let Some(current) = current else {
        return WaitOutcome::Unknown;
    };
    if satisfies(current, &request.states) {
        return WaitOutcome::Reached {
            status: current.to_owned(),
        };
    }
    // The state check comes first on purpose. A wait whose condition is already
    // true must report that even if its deadline has just passed — the caller
    // asked whether the session reached the state, and it did.
    if request.remaining.is_zero() {
        return WaitOutcome::TimedOut;
    }
    WaitOutcome::Pending {
        remaining: request.remaining,
    }
}

/// Does `status` satisfy the wanted set?
pub fn satisfies(status: &str, wanted: &[String]) -> bool {
    if wanted.is_empty() {
        return ATTENTION_STATES.contains(&status);
    }
    wanted.iter().any(|state| state == status)
}

/// How long to suggest the caller waits before asking again, never longer than
/// the wait has left. Suggesting 750 ms when 100 ms remain would make every
/// wait overshoot its own deadline by most of a second.
pub fn retry_after(remaining: Duration) -> Duration {
    RETRY_AFTER.min(remaining)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(states: &[&str], remaining_ms: u64) -> WaitRequest {
        WaitRequest {
            session: "s0001".into(),
            states: states.iter().map(|state| (*state).to_owned()).collect(),
            remaining: Duration::from_millis(remaining_ms),
        }
    }

    #[test]
    fn an_unknown_session_is_not_a_timeout() {
        // The failure that makes a wait primitive untrustworthy: waiting the
        // full five minutes on a mistyped id and then reporting a timeout, so
        // the caller cannot tell a slow condition from an impossible one.
        assert_eq!(evaluate(None, &request(&[], 60_000)), WaitOutcome::Unknown);
        // Even with no time left, "there is no such session" is the true answer.
        assert_eq!(evaluate(None, &request(&[], 0)), WaitOutcome::Unknown);
    }

    #[test]
    fn the_default_target_is_a_session_waiting_on_a_person() {
        // "Block until another agent genuinely needs input" is the case this
        // exists for, so naming no state means exactly that.
        for status in ATTENTION_STATES {
            assert_eq!(
                evaluate(Some(status), &request(&[], 60_000)),
                WaitOutcome::Reached {
                    status: (*status).to_string()
                },
                "{status} should satisfy the default wait"
            );
        }
        // A busy session is not waiting on anyone.
        assert!(matches!(
            evaluate(Some("working"), &request(&[], 60_000)),
            WaitOutcome::Pending { .. }
        ));
        // Neither is a rate-limited one: it is waiting on a provider.
        assert!(matches!(
            evaluate(Some("rate-limited"), &request(&[], 60_000)),
            WaitOutcome::Pending { .. }
        ));
    }

    #[test]
    fn a_named_state_replaces_the_default_rather_than_adding_to_it() {
        let idle_only = request(&["idle"], 60_000);
        assert_eq!(
            evaluate(Some("idle"), &idle_only),
            WaitOutcome::Reached {
                status: "idle".into()
            }
        );
        assert!(
            matches!(
                evaluate(Some("needs-approval"), &idle_only),
                WaitOutcome::Pending { .. }
            ),
            "an attention state must not satisfy a wait that named something else"
        );
    }

    #[test]
    fn a_condition_already_true_beats_an_expired_deadline() {
        // The caller asked whether the session reached the state. It did.
        // Reporting a timeout because the clock ran out in the same instant
        // would lose an answer we actually have.
        assert_eq!(
            evaluate(Some("needs-approval"), &request(&[], 0)),
            WaitOutcome::Reached {
                status: "needs-approval".into()
            }
        );
    }

    #[test]
    fn an_expired_wait_on_an_unmet_condition_times_out() {
        assert_eq!(evaluate(Some("working"), &request(&[], 0)), WaitOutcome::TimedOut);
    }

    #[test]
    fn a_timeout_is_clamped_rather_than_refused() {
        // An unbounded wait is a hang that looks like a working call.
        assert_eq!(bounded_wait(None), DEFAULT_WAIT);
        assert_eq!(bounded_wait(Some(Duration::from_secs(1))), Duration::from_secs(1));
        assert_eq!(bounded_wait(Some(Duration::from_secs(86_400))), MAX_WAIT);
    }

    #[test]
    fn the_retry_hint_never_overshoots_the_deadline() {
        // Suggesting 750ms when 100ms remain makes every wait overshoot its own
        // deadline by most of a second.
        assert_eq!(retry_after(Duration::from_millis(100)), Duration::from_millis(100));
        assert_eq!(retry_after(Duration::from_secs(60)), RETRY_AFTER);
    }

    #[test]
    fn several_states_can_be_waited_on_at_once() {
        let either = request(&["idle", "exited"], 60_000);
        assert!(matches!(evaluate(Some("idle"), &either), WaitOutcome::Reached { .. }));
        assert!(matches!(evaluate(Some("exited"), &either), WaitOutcome::Reached { .. }));
        assert!(matches!(evaluate(Some("working"), &either), WaitOutcome::Pending { .. }));
    }
}
