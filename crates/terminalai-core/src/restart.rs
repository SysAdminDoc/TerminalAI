//! What one process exit means for the session that owned it.
//!
//! The supervisor's restart policy: whether an exit is worth recovering from,
//! how much of the budget it spends, when the budget resets, and how long to
//! wait before trying again. Like [`crate::admission`], it takes no lock, spawns
//! no thread and reads no clock — the caller measures how long the process ran
//! and this module decides what follows. The one source of nondeterminism is the
//! jitter draw, which is the point of the jitter.

use std::time::Duration;

/// `STATUS_CONTROL_C_EXIT` — what a Windows console process reports after the
/// operator pressed Ctrl-C in its pane. It is a deliberate stop by a person,
/// not a fault, so the supervisor treats it as one.
pub const STATUS_CONTROL_C_EXIT: u32 = 0xC000_013A;

/// Maximum number of automatic restart attempts for one session. A session
/// that keeps failing after this limit stays failed until the operator revives
/// it explicitly.
pub const MAX_RESTARTS: u32 = 5;
pub const RESTART_BACKOFF_BASE: Duration = Duration::from_millis(250);
pub const RESTART_BACKOFF_MAX: Duration = Duration::from_secs(30);

/// How long a process must run before its predecessors' restarts stop counting
/// against it.
///
/// Every mature supervisor scopes the budget to a window rather than to the
/// lifetime of the thing it supervises: OTP pairs `intensity` with `period`,
/// systemd pairs `StartLimitBurst` with `StartLimitIntervalSec`, and Kubernetes
/// resets the CrashLoopBackOff counter after ten minutes of successful running.
/// Ten minutes is Kubernetes' number, chosen for the same reason: it is long
/// enough that a genuine crash loop cannot hide inside it, and short enough that
/// a session which crashes once a day recovers every day. Without it, five
/// restarts spread over a week permanently kill a session that ran healthily in
/// between.
pub const RESTART_WINDOW: Duration = Duration::from_secs(10 * 60);

/// Whether an exit is something to recover from.
///
/// Every mature supervisor draws this line: OTP calls it the `transient`
/// restart type and systemd calls it `Restart=on-abnormal`, and both restart a
/// child only when it ended abnormally. Restarting an agent that finished its
/// work re-runs work nobody asked for and bills quota for it, up to
/// [`MAX_RESTARTS`] times.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitClass {
    /// The agent ended on purpose. There is nothing to recover.
    Finished,
    /// The agent died, or died in a way we could not read. Bring it back.
    Abnormal,
}

/// Classify one process exit.
///
/// An unreadable exit code is abnormal: the supervisor cannot prove the agent
/// meant to stop, and the cost of a spurious restart is lower than the cost of
/// silently abandoning a crashed session.
pub fn classify_exit(exit_code: Option<u32>) -> ExitClass {
    match exit_code {
        Some(0) | Some(STATUS_CONTROL_C_EXIT) => ExitClass::Finished,
        _ => ExitClass::Abnormal,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartDecision {
    Backoff(Duration),
    Failed,
    /// The agent exited cleanly. No restart is scheduled and none ever will be
    /// without an explicit operator action.
    Finished,
}

/// Everything the policy needs to know about the exit, measured by the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exit {
    pub exit_code: Option<u32>,
    /// Restarts this session has already spent.
    pub restarts_spent: u32,
    /// How long the process that just exited ran, when its start was recorded.
    /// `None` means the supervisor never saw it start, which cannot earn the
    /// clean slate a full window earns.
    pub ran_for: Option<Duration>,
}

/// What the session should do next, and what its budget looks like afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    pub decision: RestartDecision,
    /// The restart count the session carries forward. Already includes both the
    /// window reset and the attempt this exit spends.
    pub restarts_spent: u32,
}

/// The supervisor's whole restart policy, as one function of one exit.
///
/// Order matters and is the reason this is worth naming: classify before
/// counting. An agent that finished its work is not spending a restart from the
/// budget, and it is not coming back on its own — it is done.
pub fn decide(exit: &Exit) -> Outcome {
    // A process that ran for a full window earned a clean slate. Scoping the
    // budget this way is what stops five restarts spread over a week from
    // permanently killing a session that ran healthily in between.
    let restarts_spent = if exit.ran_for.is_some_and(|ran_for| ran_for >= RESTART_WINDOW) {
        0
    } else {
        exit.restarts_spent
    };
    if classify_exit(exit.exit_code) == ExitClass::Finished {
        return Outcome {
            decision: RestartDecision::Finished,
            restarts_spent,
        };
    }
    if restarts_spent >= MAX_RESTARTS {
        return Outcome {
            decision: RestartDecision::Failed,
            restarts_spent,
        };
    }
    let attempt = restarts_spent + 1;
    Outcome {
        decision: RestartDecision::Backoff(backoff(attempt)),
        restarts_spent: attempt,
    }
}

/// Full jitter: `random(0, min(cap, base·2^n))`.
///
/// Failures here are correlated by construction — one provider rate limit or one
/// network drop takes every session in the fleet at the same instant — so a
/// deterministic delay guarantees all of them retry together against the service
/// that just refused them. AWS measured full jitter beating un-jittered backoff
/// by over 50% on contending calls, and it is the variant that spreads a
/// synchronised fleet fastest.
pub fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(jitter(backoff_ceiling(attempt).as_millis() as u64))
}

/// The un-jittered ceiling the delay is drawn from. Separate so a test can pin
/// the exponential growth without depending on the random draw.
pub fn backoff_ceiling(attempt: u32) -> Duration {
    let exponent = attempt.saturating_sub(1).min(63);
    let multiplier = 1u128 << exponent;
    let millis = RESTART_BACKOFF_BASE.as_millis().saturating_mul(multiplier);
    let capped = millis.min(RESTART_BACKOFF_MAX.as_millis());
    Duration::from_millis(capped as u64)
}

/// Uniform draw from `0..=ceiling`.
///
/// `getrandom` is already a workspace dependency, so this costs no new crate. A
/// random source that fails returns the ceiling rather than zero: an immediate
/// unjittered retry into a provider that just refused the whole fleet is the one
/// outcome worth avoiding.
fn jitter(ceiling_millis: u64) -> u64 {
    if ceiling_millis == 0 {
        return 0;
    }
    let mut bytes = [0u8; 8];
    if getrandom::fill(&mut bytes).is_err() {
        return ceiling_millis;
    }
    u64::from_le_bytes(bytes) % (ceiling_millis + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crashed(restarts_spent: u32, ran_for: Option<Duration>) -> Exit {
        Exit {
            exit_code: Some(1),
            restarts_spent,
            ran_for,
        }
    }

    #[test]
    fn the_ceiling_doubles_until_it_is_capped() {
        assert_eq!(backoff_ceiling(1), RESTART_BACKOFF_BASE);
        assert_eq!(backoff_ceiling(2), RESTART_BACKOFF_BASE * 2);
        assert_eq!(backoff_ceiling(3), RESTART_BACKOFF_BASE * 4);
        assert_eq!(backoff_ceiling(20), RESTART_BACKOFF_MAX);
        // Saturating rather than overflowing at an absurd attempt count.
        assert_eq!(backoff_ceiling(u32::MAX), RESTART_BACKOFF_MAX);
    }

    #[test]
    fn every_delay_is_drawn_from_below_its_ceiling() {
        for attempt in 1..=MAX_RESTARTS {
            for _ in 0..64 {
                assert!(backoff(attempt) <= backoff_ceiling(attempt));
            }
        }
    }

    #[test]
    fn a_clean_exit_spends_no_restart_and_is_not_retried() {
        let outcome = decide(&Exit {
            exit_code: Some(0),
            restarts_spent: 2,
            ran_for: Some(Duration::from_secs(1)),
        });
        assert_eq!(outcome.decision, RestartDecision::Finished);
        assert_eq!(outcome.restarts_spent, 2);
    }

    #[test]
    fn a_ctrl_c_exit_counts_as_finished_rather_than_as_a_crash() {
        let outcome = decide(&Exit {
            exit_code: Some(STATUS_CONTROL_C_EXIT),
            restarts_spent: 0,
            ran_for: None,
        });
        assert_eq!(outcome.decision, RestartDecision::Finished);
    }

    #[test]
    fn an_unreadable_exit_is_retried_rather_than_abandoned() {
        let outcome = decide(&Exit {
            exit_code: None,
            restarts_spent: 0,
            ran_for: None,
        });
        assert!(matches!(outcome.decision, RestartDecision::Backoff(_)));
        assert_eq!(outcome.restarts_spent, 1);
    }

    #[test]
    fn the_budget_is_terminal_once_spent() {
        let outcome = decide(&crashed(MAX_RESTARTS, Some(Duration::from_secs(1))));
        assert_eq!(outcome.decision, RestartDecision::Failed);
        assert_eq!(outcome.restarts_spent, MAX_RESTARTS);
    }

    #[test]
    fn a_full_window_of_running_earns_a_clean_slate() {
        let exhausted = decide(&crashed(MAX_RESTARTS, Some(RESTART_WINDOW)));
        assert!(matches!(exhausted.decision, RestartDecision::Backoff(_)));
        assert_eq!(exhausted.restarts_spent, 1);
        // One tick short of the window is not a clean slate.
        let short = decide(&crashed(
            MAX_RESTARTS,
            Some(RESTART_WINDOW - Duration::from_millis(1)),
        ));
        assert_eq!(short.decision, RestartDecision::Failed);
    }

    #[test]
    fn a_process_never_seen_to_start_does_not_earn_the_reset() {
        // `None` is "the supervisor never recorded a start", which is not
        // evidence of a long healthy run and must not be read as one.
        assert_eq!(
            decide(&crashed(MAX_RESTARTS, None)).decision,
            RestartDecision::Failed
        );
    }
}
