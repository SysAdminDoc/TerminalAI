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

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Width of one ledger bucket. Spend within a minute is summed together.
pub const BUCKET: Duration = Duration::from_secs(60);

/// How far back the ceiling looks by default.
pub const DEFAULT_SPEND_WINDOW: Duration = Duration::from_secs(24 * 60 * 60);

/// One minute of fleet spend.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SpendBucket {
    /// Whole minutes since the Unix epoch.
    pub minute: u64,
    pub usd: f64,
    /// Which sessions the minute's spend belongs to.
    ///
    /// Carried per bucket rather than as a running per-session total because
    /// the question this answers is always about a *window* -- "who consumed
    /// the quota that is refusing us right now" -- and a running total cannot
    /// be restricted to one afterwards.
    ///
    /// `#[serde(default)]` so a store written before this field loads as a
    /// window with no attribution rather than refusing: losing the breakdown
    /// must never cost the operator their session list, which is the same rule
    /// `from_buckets` already follows.
    #[serde(default)]
    pub by_session: BTreeMap<String, f64>,
}

impl SpendBucket {
    /// A bucket with no spend in it yet.
    fn empty(minute: u64) -> Self {
        Self {
            minute,
            usd: 0.0,
            by_session: BTreeMap::new(),
        }
    }
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
        self.record_session_at(at, None, delta_usd);
    }

    /// The same, attributed to the session that spent it.
    ///
    /// `None` records the money without attribution rather than dropping it:
    /// the fleet total must stay correct even for a delta whose owner is not
    /// known, and a breakdown that silently omits spend is worse than one that
    /// admits an unattributed remainder.
    pub fn record_session_at(
        &mut self,
        at: SystemTime,
        session: Option<&str>,
        delta_usd: f64,
    ) {
        if !delta_usd.is_finite() || delta_usd <= 0.0 {
            return;
        }
        let minute = minute_of(at);
        // Resolve the bucket first, then add to it once. The earlier version of
        // this attributed the session only on the "last bucket matches" path,
        // so an out-of-order arrival or a new minute recorded the money in the
        // fleet total and dropped it from the breakdown -- which is the exact
        // failure the breakdown exists to avoid, and it is silent.
        let index = match self.buckets.back() {
            Some(last) if last.minute == minute => self.buckets.len() - 1,
            // Out-of-order arrivals are rare but real -- a transcript poll can
            // report an older stamp than the one before it. Fold them into the
            // matching bucket instead of appending out of order.
            Some(last) if last.minute > minute => {
                match self.buckets.iter().position(|bucket| bucket.minute == minute) {
                    Some(index) => index,
                    None => {
                        let index = self
                            .buckets
                            .iter()
                            .position(|bucket| bucket.minute > minute)
                            .unwrap_or(self.buckets.len());
                        self.buckets.insert(index, SpendBucket::empty(minute));
                        index
                    }
                }
            }
            _ => {
                self.buckets.push_back(SpendBucket::empty(minute));
                self.buckets.len() - 1
            }
        };
        let bucket = &mut self.buckets[index];
        bucket.usd += delta_usd;
        if let Some(session) = session {
            *bucket.by_session.entry(session.to_owned()).or_insert(0.0) += delta_usd;
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

    /// Who spent the money inside the window ending at `now`, largest first.
    ///
    /// This is the question a rate-limited fleet actually has: not "what has
    /// this session cost since it started" but "which sessions consumed the
    /// window that is refusing us now". A session's running total cannot answer
    /// it, because it includes everything before the window opened.
    ///
    /// The returned figures are this tool's own transcript arithmetic. They are
    /// never the provider's accounting and must not be presented as it.
    pub fn window_by_session_at(
        &self,
        now: SystemTime,
        window: Duration,
    ) -> Vec<(String, f64)> {
        let now_minute = minute_of(now);
        let span = (window.as_secs() / BUCKET.as_secs()).max(1);
        let oldest = now_minute.saturating_sub(span.saturating_sub(1));
        let mut totals: BTreeMap<&str, f64> = BTreeMap::new();
        for bucket in self.buckets.iter().filter(|b| b.minute >= oldest) {
            for (session, usd) in &bucket.by_session {
                *totals.entry(session.as_str()).or_insert(0.0) += usd;
            }
        }
        let mut ranked: Vec<(String, f64)> = totals
            .into_iter()
            .map(|(session, usd)| (session.to_owned(), usd))
            .collect();
        // Largest first, then by id so equal figures order reproducibly rather
        // than by whatever the map yielded.
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        ranked
    }

    /// Window spend this ledger cannot attribute to any session.
    ///
    /// Reported rather than hidden. A breakdown that quietly omits spend reads
    /// as a complete account of the window and is not one -- and money recorded
    /// before this ledger had a session dimension lands here by construction.
    pub fn window_unattributed_at(&self, now: SystemTime, window: Duration) -> f64 {
        let attributed: f64 = self
            .window_by_session_at(now, window)
            .iter()
            .map(|(_, usd)| usd)
            .sum();
        (self.window_total_at(now, window) - attributed).max(0.0)
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
            SpendBucket { minute: 5, usd: 1.0, by_session: BTreeMap::new() },
            SpendBucket { minute: 1, usd: 2.0, by_session: BTreeMap::new() },
            SpendBucket { minute: 9, usd: f64::NAN, by_session: BTreeMap::new() },
            SpendBucket { minute: 9, usd: -1.0, by_session: BTreeMap::new() },
        ]);
        let minutes: Vec<_> = ledger.buckets().map(|bucket| bucket.minute).collect();
        assert_eq!(minutes, vec![1, 5]);
        assert_eq!(ledger.window_total_at(at(300), Duration::from_secs(3600)), 3.0);
    }
}

#[cfg(test)]
mod attribution_tests {
    use super::*;

    fn at(minutes: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(minutes * 60)
    }

    #[test]
    fn the_window_says_who_spent_it_largest_first() {
        let mut ledger = SpendLedger::new();
        ledger.record_session_at(at(0), Some("s0001"), 1.0);
        ledger.record_session_at(at(0), Some("s0002"), 4.0);
        ledger.record_session_at(at(1), Some("s0001"), 2.0);
        let ranked = ledger.window_by_session_at(at(1), Duration::from_secs(3600));
        assert_eq!(
            ranked,
            vec![("s0002".to_owned(), 4.0), ("s0001".to_owned(), 3.0)]
        );
    }

    #[test]
    fn spend_outside_the_window_is_not_attributed_to_it() {
        // The whole point: a session's running total includes everything before
        // the window opened, and that is exactly what must not be reported as
        // having consumed the current quota.
        let mut ledger = SpendLedger::new();
        ledger.record_session_at(at(0), Some("old"), 100.0);
        ledger.record_session_at(at(600), Some("recent"), 1.0);
        let ranked = ledger.window_by_session_at(at(600), Duration::from_secs(3600));
        assert_eq!(ranked, vec![("recent".to_owned(), 1.0)]);
    }

    #[test]
    fn an_out_of_order_arrival_is_still_attributed() {
        // The first version of this attributed only on the "last bucket
        // matches" path, so an older stamp reached the fleet total and vanished
        // from the breakdown. Silent, and in the direction that understates.
        let mut ledger = SpendLedger::new();
        ledger.record_session_at(at(5), Some("s0001"), 1.0);
        ledger.record_session_at(at(3), Some("s0002"), 2.0);
        let window = Duration::from_secs(3600);
        assert_eq!(
            ledger.window_by_session_at(at(5), window),
            vec![("s0002".to_owned(), 2.0), ("s0001".to_owned(), 1.0)]
        );
        assert_eq!(ledger.window_unattributed_at(at(5), window), 0.0);
    }

    #[test]
    fn a_new_minute_is_still_attributed() {
        // The other path the first version dropped.
        let mut ledger = SpendLedger::new();
        ledger.record_session_at(at(1), Some("s0001"), 1.0);
        ledger.record_session_at(at(2), Some("s0001"), 1.0);
        assert_eq!(
            ledger.window_by_session_at(at(2), Duration::from_secs(3600)),
            vec![("s0001".to_owned(), 2.0)]
        );
    }

    #[test]
    fn unattributed_spend_is_reported_rather_than_hidden() {
        // A ledger restored from a store written before buckets had a session
        // dimension has money and no owners. Reporting zero sessions and a
        // full total would read as a complete account of the window.
        let mut ledger = SpendLedger::new();
        ledger.record_at(at(0), 7.0);
        ledger.record_session_at(at(0), Some("s0001"), 3.0);
        let window = Duration::from_secs(3600);
        assert_eq!(ledger.window_total_at(at(0), window), 10.0);
        assert_eq!(
            ledger.window_by_session_at(at(0), window),
            vec![("s0001".to_owned(), 3.0)]
        );
        assert_eq!(ledger.window_unattributed_at(at(0), window), 7.0);
    }

    #[test]
    fn equal_figures_rank_reproducibly() {
        let mut ledger = SpendLedger::new();
        ledger.record_session_at(at(0), Some("s0002"), 1.0);
        ledger.record_session_at(at(0), Some("s0001"), 1.0);
        let ranked = ledger.window_by_session_at(at(0), Duration::from_secs(3600));
        assert_eq!(ranked[0].0, "s0001");
    }

    #[test]
    fn a_store_written_before_attribution_still_loads() {
        // `by_session` is `#[serde(default)]`; a bucket without it must restore
        // as unattributed spend rather than refusing the whole ledger.
        let bucket: SpendBucket =
            serde_json::from_str(r#"{"minute":10,"usd":2.5}"#).expect("an older bucket loads");
        assert_eq!(bucket.usd, 2.5);
        assert!(bucket.by_session.is_empty());
    }
}
