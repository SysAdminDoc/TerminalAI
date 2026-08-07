//! How full a session's context window is — pure, no lock, no thread, no clock.
//!
//! Sits beside [`crate::admission`] and [`crate::restart`] for the same reason:
//! the question ("how much room is left") is separable from the machinery that
//! measures it, and a policy interleaved with mutation cannot be stated.
//!
//! # Occupancy is the latest request, not the sum
//!
//! The one thing to get right. [`crate::transcript::UsageTotals`] accumulates
//! every request a session ever made, and a session two hundred turns deep has
//! summed millions of tokens while its window holds sixty thousand. Reporting
//! the total as occupancy would put every long session at several hundred
//! percent of a window it is nowhere near filling — confidently, and in the one
//! field an operator would act on.
//!
//! What actually occupies the window is what the provider counted as the prompt
//! on the **most recent** request: fresh input, cache reads and cache writes
//! together. That is a measured figure, not a projection.
//!
//! # The window is reported, never inferred
//!
//! Codex states `model_context_window` on every usage event, so its rows carry a
//! real denominator. Claude Code does not put one in its transcript, and the
//! vendored price table was trimmed to costs, so nothing here knows a Claude
//! model's window. A guessed denominator is worse than none — it would put a
//! percentage next to a number nobody can check — so a session with no reported
//! window reports its occupancy alone and the row shows an em dash for the rest.

/// Fraction of the window past which the fleet says a session is filling up.
pub const PRESSURE_WARN: f64 = 0.75;
/// Fraction past which the session is close enough that its next turn is likely
/// to be compacted or refused.
pub const PRESSURE_CRITICAL: f64 = 0.90;

/// Where an occupancy reading came from. Kept on the reading because the two
/// sources answer with different authority: one is the agent stating its own
/// window, the other is arithmetic over what the provider billed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ContextSource {
    /// The agent reported its own usage against its own window.
    Agent,
    /// Derived from the last priced request in the session's transcript.
    Transcript,
}

/// How much room is left, as a band rather than a number, so the row and its
/// styling agree on the boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ContextPressure {
    Comfortable,
    Filling,
    Critical,
}

/// One session's context reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ContextUsage {
    /// Tokens occupying the window as of the last measurement.
    pub used_tokens: u64,
    /// The model's window, when the agent reported one. Never inferred.
    pub window_tokens: Option<u64>,
    pub source: ContextSource,
}

impl ContextUsage {
    /// A reading against a window the agent stated.
    pub fn reported(used_tokens: u64, window_tokens: Option<u64>) -> Self {
        Self {
            used_tokens,
            window_tokens,
            source: ContextSource::Agent,
        }
    }

    /// A reading derived from the transcript, with no window to measure against.
    pub fn derived(used_tokens: u64) -> Self {
        Self {
            used_tokens,
            window_tokens: None,
            source: ContextSource::Transcript,
        }
    }

    /// Fraction of the window in use, or `None` when no window is known.
    ///
    /// A zero window is treated as no window rather than as a division: a
    /// provider reporting `0` has told us nothing, and infinity in a percentage
    /// field is the worst possible reading of that.
    pub fn used_fraction(&self) -> Option<f64> {
        let window = self.window_tokens.filter(|window| *window > 0)?;
        Some(self.used_tokens as f64 / window as f64)
    }

    /// The pressure band, or `None` when there is no window to be under
    /// pressure against. Absence is not comfort — a row with no denominator
    /// says so rather than showing green.
    pub fn pressure(&self) -> Option<ContextPressure> {
        let fraction = self.used_fraction()?;
        Some(if fraction >= PRESSURE_CRITICAL {
            ContextPressure::Critical
        } else if fraction >= PRESSURE_WARN {
            ContextPressure::Filling
        } else {
            ContextPressure::Comfortable
        })
    }

    /// Tokens still available, when that is knowable. Saturating: a provider
    /// that reports usage above its own stated window leaves zero headroom, not
    /// a wrapped enormous one.
    pub fn headroom_tokens(&self) -> Option<u64> {
        self.window_tokens
            .map(|window| window.saturating_sub(self.used_tokens))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reading_with_no_window_has_no_percentage_and_no_pressure() {
        // The whole point of the em dash: nothing here may invent a denominator.
        let usage = ContextUsage::derived(60_000);
        assert_eq!(usage.used_fraction(), None);
        assert_eq!(usage.pressure(), None);
        assert_eq!(usage.headroom_tokens(), None);
        assert_eq!(usage.source, ContextSource::Transcript);
    }

    #[test]
    fn a_zero_window_is_no_window_rather_than_a_division() {
        let usage = ContextUsage::reported(1_000, Some(0));
        assert_eq!(usage.used_fraction(), None);
        assert_eq!(usage.pressure(), None);
    }

    #[test]
    fn the_pressure_bands_are_inclusive_at_their_thresholds() {
        let at = |used: u64| ContextUsage::reported(used, Some(100_000)).pressure();
        assert_eq!(at(74_999), Some(ContextPressure::Comfortable));
        assert_eq!(at(75_000), Some(ContextPressure::Filling));
        assert_eq!(at(89_999), Some(ContextPressure::Filling));
        assert_eq!(at(90_000), Some(ContextPressure::Critical));
    }

    #[test]
    fn usage_beyond_the_reported_window_leaves_no_headroom() {
        // Providers have reported usage above their own stated window. Wrapping
        // would turn "full" into "18 exabytes free".
        let usage = ContextUsage::reported(120_000, Some(100_000));
        assert_eq!(usage.headroom_tokens(), Some(0));
        assert_eq!(usage.pressure(), Some(ContextPressure::Critical));
    }

    #[test]
    fn headroom_is_what_is_left() {
        let usage = ContextUsage::reported(40_000, Some(200_000));
        assert_eq!(usage.headroom_tokens(), Some(160_000));
        assert_eq!(usage.pressure(), Some(ContextPressure::Comfortable));
    }
}
