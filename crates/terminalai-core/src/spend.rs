//! Fleet-wide spend accounting over a rolling window.
//!
//! A per-session budget bounds one agent. Nothing bounded the fleet, so twenty
//! sessions each obeying a $5 cap could spend $100 while every individual limit
//! reported itself satisfied. This ledger is the aggregate the admission gate
//! consults before it starts anything new.
//!
//! Spend is recorded as **deltas**, because a session reports its cost as a
//! running total: the ledger sees the increase between two reports, never the
//! total, so a long-lived session contributes to the window it actually spent
//! in rather than to the window it happened to be observed in.
//!
//! Storage is bucketed by minute rather than kept per event. A rolling window
//! then costs at most `window / 1 minute` entries no matter how chatty the
//! fleet is — bounded by construction, in the same spirit as the scrollback
//! ring, instead of by a cap someone has to remember to enforce.

use std::collections::VecDeque;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Width of one ledger bucket. Spend within a minute is summed together.
pub const BUCKET: Duration = Duration::from_secs(60);

/// How far back the ceiling looks by default.
pub const DEFAULT_SPEND_WINDOW: Duration = Duration::from_secs(24 * 60 * 60);

/// One minute of fleet spend.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SpendBucket {
    /// Whole minutes since the Unix epoch.
    pub minute: u64,
    pub usd: f64,
}

/// Rolling-window fleet spend, oldest bucket first.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SpendLedger {
    buckets: VecDeque<SpendBucket>,
}

fn minute_of(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / BUCKET.as_secs()
}

impl SpendLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Restore from a persisted snapshot, dropping anything malformed rather
    /// than refusing the whole store: a spend ledger is an accounting aid, and
    /// losing it must never cost the operator their session list.
    pub fn from_buckets(buckets: impl IntoIterator<Item = SpendBucket>) -> Self {
        let mut buckets: Vec<_> = buckets
            .into_iter()
            .filter(|bucket| bucket.usd.is_finite() && bucket.usd >= 0.0)
            .collect();
        buckets.sort_by_key(|bucket| bucket.minute);
        Self {
            buckets: buckets.into(),
        }
    }

    pub fn buckets(&self) -> impl Iterator<Item = &SpendBucket> {
        self.buckets.iter()
    }

    /// Add one increase in fleet spend. Non-finite and non-positive deltas are
    /// ignored: a cost that went backwards means the transcript was re-read or
    /// the price table changed, neither of which is money spent again.
    pub fn record_at(&mut self, at: SystemTime, delta_usd: f64) {
        if !delta_usd.is_finite() || delta_usd <= 0.0 {
            return;
        }
        let minute = minute_of(at);
        match self.buckets.back_mut() {
            Some(last) if last.minute == minute => last.usd += delta_usd,
            // Out-of-order arrivals are rare but real — a transcript poll can
            // report an older stamp than the one before it. Fold them into the
            // matching bucket instead of appending out of order.
            Some(last) if last.minute > minute => {
                match self
                    .buckets
                    .iter_mut()
                    .find(|bucket| bucket.minute == minute)
                {
                    Some(bucket) => bucket.usd += delta_usd,
                    None => {
                        let index = self
                            .buckets
                            .iter()
                            .position(|bucket| bucket.minute > minute)
                            .unwrap_or(self.buckets.len());
                        self.buckets.insert(index, SpendBucket { minute, usd: delta_usd });
                    }
                }
            }
            _ => self.buckets.push_back(SpendBucket { minute, usd: delta_usd }),
        }
    }

    /// Drop buckets that have aged out of the window.
    pub fn prune_at(&mut self, now: SystemTime, window: Duration) {
        let now_minute = minute_of(now);
        let span = (window.as_secs() / BUCKET.as_secs()).max(1);
        let oldest = now_minute.saturating_sub(span.saturating_sub(1));
        while self
            .buckets
            .front()
            .is_some_and(|bucket| bucket.minute < oldest)
        {
            self.buckets.pop_front();
        }
    }

    /// Fleet spend inside the window ending at `now`.
    pub fn window_total_at(&self, now: SystemTime, window: Duration) -> f64 {
        let now_minute = minute_of(now);
        let span = (window.as_secs() / BUCKET.as_secs()).max(1);
        let oldest = now_minute.saturating_sub(span.saturating_sub(1));
        self.buckets
            .iter()
            .filter(|bucket| bucket.minute >= oldest)
            .map(|bucket| bucket.usd)
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.buckets.is_empty()
    }

    pub fn len(&self) -> usize {
        self.buckets.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn spend_inside_the_window_is_summed() {
        let mut ledger = SpendLedger::new();
        ledger.record_at(at(0), 1.0);
        ledger.record_at(at(30), 2.0);
        ledger.record_at(at(120), 4.0);
        assert_eq!(ledger.window_total_at(at(120), Duration::from_secs(600)), 7.0);
    }

    #[test]
    fn one_minute_of_spend_occupies_one_bucket() {
        let mut ledger = SpendLedger::new();
        for _ in 0..100 {
            ledger.record_at(at(10), 0.01);
        }
        assert_eq!(ledger.len(), 1);
        assert!((ledger.window_total_at(at(10), Duration::from_secs(600)) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn spend_ages_out_of_the_window() {
        let mut ledger = SpendLedger::new();
        ledger.record_at(at(0), 5.0);
        ledger.record_at(at(3600), 1.0);
        let window = Duration::from_secs(600);
        assert_eq!(ledger.window_total_at(at(3600), window), 1.0);
        ledger.prune_at(at(3600), window);
        assert_eq!(ledger.len(), 1);
    }

    #[test]
    fn a_cost_that_went_backwards_is_not_money_spent_again() {
        let mut ledger = SpendLedger::new();
        ledger.record_at(at(0), -3.0);
        ledger.record_at(at(0), 0.0);
        ledger.record_at(at(0), f64::NAN);
        ledger.record_at(at(0), f64::INFINITY);
        assert!(ledger.is_empty());
        assert_eq!(ledger.window_total_at(at(0), DEFAULT_SPEND_WINDOW), 0.0);
    }

    #[test]
    fn an_out_of_order_report_lands_in_its_own_minute() {
        let mut ledger = SpendLedger::new();
        ledger.record_at(at(600), 1.0);
        ledger.record_at(at(60), 2.0);
        ledger.record_at(at(300), 4.0);
        let minutes: Vec<_> = ledger.buckets().map(|bucket| bucket.minute).collect();
        assert_eq!(minutes, vec![1, 5, 10], "buckets stay ordered by minute");
        assert_eq!(ledger.window_total_at(at(600), Duration::from_secs(3600)), 7.0);
    }

    #[test]
    fn an_out_of_order_report_in_an_existing_minute_is_folded_in() {
        let mut ledger = SpendLedger::new();
        ledger.record_at(at(60), 1.0);
        ledger.record_at(at(600), 1.0);
        ledger.record_at(at(90), 2.0);
        assert_eq!(ledger.len(), 2);
        assert_eq!(ledger.window_total_at(at(600), Duration::from_secs(3600)), 4.0);
    }

    #[test]
    fn a_restored_ledger_keeps_only_usable_buckets() {
        let ledger = SpendLedger::from_buckets([
            SpendBucket { minute: 5, usd: 1.0 },
            SpendBucket { minute: 1, usd: 2.0 },
            SpendBucket { minute: 9, usd: f64::NAN },
            SpendBucket { minute: 9, usd: -1.0 },
        ]);
        let minutes: Vec<_> = ledger.buckets().map(|bucket| bucket.minute).collect();
        assert_eq!(minutes, vec![1, 5]);
        assert_eq!(ledger.window_total_at(at(300), Duration::from_secs(3600)), 3.0);
    }
}
