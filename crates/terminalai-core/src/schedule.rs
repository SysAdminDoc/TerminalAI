//! Running the same work across the same projects again, on a cadence.
//!
//! The work queue already runs one stored prompt across many projects on
//! demand, refusing dirty trees, asking the fleet for one slot at a time and
//! recording every outcome. This is the same run on a timer, and it is
//! deliberately thin: a schedule decides *when* to start a run and nothing
//! about how one behaves. Every refusal the on-demand path enforces applies to
//! a scheduled firing because a scheduled firing goes through that path.
//!
//! Three rules are the whole design:
//!
//! - **A machine that was asleep does not wake up owing eight runs.** Missed
//!   occurrences are skipped, not queued: firing them in a burst would put the
//!   same prompt into the same repositories several times over, and the second
//!   one lands on the first one's uncommitted work. The count of what was
//!   missed is reported instead.
//! - **A firing never interrupts the run before it.** Starting a run replaces
//!   the previous one, so a schedule that fired while forty projects were still
//!   working would destroy the report the operator was going to read. It is
//!   recorded as skipped, with the reason.
//! - **Every firing is written down, including the ones that did nothing.** A
//!   schedule the operator was not present for is only trustworthy if it can
//!   say what it did while they were away.
//!
//! Cadence is an interval, not a wall-clock time of day. `SystemTime` has no
//! local-time facility in `std`, and "every 24 hours" is one dependency cheaper
//! than "at 02:00 local" — which drifts by an hour twice a year anyway.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

/// The shortest cadence a run may have. The unit of work is an agent session
/// per project, so anything faster is not a schedule — it is a loop, and the
/// fleet's admission gate would spend its time refusing it.
pub const MIN_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// The longest, so a mistyped value cannot park a schedule past the point
/// anyone remembers making it.
pub const MAX_INTERVAL: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// How many firings are kept. Enough to see a pattern, bounded because this
/// file is rewritten on every firing and read on every window open.
pub const MAX_HISTORY: usize = 20;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ScheduleError {
    #[error("a scheduled run needs at least one project")]
    NoProjects,
    #[error("the interval must be between 15 minutes and 30 days")]
    Interval,
}

/// What one firing did. Recorded whether or not it started anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FiringResult {
    /// A run was created for this many projects. What became of them is the
    /// run's own report; this only says the schedule got that far.
    Started { projects: usize },
    /// Nothing was started, and why. A schedule that silently does nothing is
    /// indistinguishable from one that is broken.
    Skipped { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleFiring {
    pub at: SystemTime,
    pub result: FiringResult,
    /// Occurrences that came due while nothing was running to fire them —
    /// the machine asleep, the window closed. Skipped rather than queued.
    #[serde(default)]
    pub missed: u32,
}

/// One stored prompt, one set of projects, one cadence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkSchedule {
    /// The stored prompt's name, resolved at firing time rather than copied
    /// here: a prompt the operator edits must take effect on the next run, and
    /// one they delete has to fail loudly rather than run a stale copy.
    pub prompt: String,
    pub projects: Vec<PathBuf>,
    pub interval_seconds: u64,
    pub next_due: SystemTime,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub history: Vec<ScheduleFiring>,
}

impl WorkSchedule {
    /// A schedule whose first firing is one interval from now.
    ///
    /// Not immediately: the operator setting a schedule has almost always just
    /// run the thing by hand, and a schedule that fires the moment it is
    /// created would run it twice.
    pub fn new(
        prompt: &str,
        projects: Vec<PathBuf>,
        interval: Duration,
        now: SystemTime,
    ) -> Result<Self, ScheduleError> {
        if projects.is_empty() {
            return Err(ScheduleError::NoProjects);
        }
        if interval < MIN_INTERVAL || interval > MAX_INTERVAL {
            return Err(ScheduleError::Interval);
        }
        Ok(Self {
            prompt: prompt.to_owned(),
            projects,
            interval_seconds: interval.as_secs(),
            next_due: now + interval,
            paused: false,
            history: Vec::new(),
        })
    }

    pub fn interval(&self) -> Duration {
        Duration::from_secs(self.interval_seconds)
    }

    /// Whether a firing is owed now. A paused schedule is never due — it keeps
    /// its next-due time so resuming does not silently fire.
    pub fn is_due(&self, now: SystemTime) -> bool {
        !self.paused && now >= self.next_due
    }

    /// Move past every occurrence that has already come and gone, and report
    /// how many were skipped beyond the one being fired now.
    ///
    /// The alternative — advancing by one interval per firing — turns a laptop
    /// that was closed for a weekend into a burst of runs the operator never
    /// asked for, each one landing on the previous one's uncommitted work.
    pub fn advance_past(&mut self, now: SystemTime) -> u32 {
        let interval = self.interval();
        let mut missed = 0u32;
        // A zero interval cannot reach here (`new` bounds it), but division by
        // one is still the safe shape if a hand-edited file says otherwise.
        let step = interval.max(MIN_INTERVAL);
        loop {
            self.next_due += step;
            if self.next_due > now {
                return missed;
            }
            missed = missed.saturating_add(1);
            if missed == u32::MAX {
                return missed;
            }
        }
    }

    /// Write down what a firing did, keeping the record bounded and newest
    /// first so the window shows the last thing that happened without scrolling.
    pub fn record(&mut self, firing: ScheduleFiring) {
        self.history.insert(0, firing);
        self.history.truncate(MAX_HISTORY);
    }

    /// The most recent firing, which is what the window shows.
    pub fn last_firing(&self) -> Option<&ScheduleFiring> {
        self.history.first()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: Duration = Duration::from_secs(3600);

    fn schedule(now: SystemTime) -> WorkSchedule {
        WorkSchedule::new("drain", vec![PathBuf::from("/repo")], 4 * HOUR, now).expect("schedule")
    }

    #[test]
    fn a_new_schedule_does_not_fire_the_moment_it_is_made() {
        // The operator setting one has almost always just run it by hand.
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let schedule = schedule(now);
        assert!(!schedule.is_due(now));
        assert!(!schedule.is_due(now + 4 * HOUR - Duration::from_secs(1)));
        assert!(schedule.is_due(now + 4 * HOUR));
    }

    #[test]
    fn a_schedule_needs_projects_and_a_sane_interval() {
        let now = SystemTime::UNIX_EPOCH;
        assert_eq!(
            WorkSchedule::new("drain", Vec::new(), 4 * HOUR, now),
            Err(ScheduleError::NoProjects)
        );
        assert_eq!(
            WorkSchedule::new("drain", vec![PathBuf::from("/repo")], Duration::from_secs(60), now),
            Err(ScheduleError::Interval),
            "a one-minute cadence is a loop, not a schedule"
        );
        assert_eq!(
            WorkSchedule::new(
                "drain",
                vec![PathBuf::from("/repo")],
                MAX_INTERVAL + Duration::from_secs(1),
                now
            ),
            Err(ScheduleError::Interval)
        );
        assert!(WorkSchedule::new(
            "drain",
            vec![PathBuf::from("/repo")],
            MIN_INTERVAL,
            now
        )
        .is_ok());
    }

    #[test]
    fn a_paused_schedule_is_never_due_and_keeps_its_place() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let mut schedule = schedule(now);
        schedule.paused = true;
        let due = schedule.next_due;
        assert!(!schedule.is_due(now + 40 * HOUR));
        assert_eq!(
            schedule.next_due, due,
            "pausing must not silently move the next firing"
        );
        // And resuming does not fire for everything that was owed while paused.
        schedule.paused = false;
        assert!(schedule.is_due(now + 40 * HOUR));
    }

    #[test]
    fn a_weekend_asleep_owes_one_run_not_forty_two() {
        // The failure this prevents: forty-two firings in a row, each landing on
        // the uncommitted work of the one before it.
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let mut schedule = schedule(now);
        let woke = now + 48 * HOUR + Duration::from_secs(30);
        assert!(schedule.is_due(woke));
        let missed = schedule.advance_past(woke);
        assert_eq!(missed, 11, "48 hours at four-hourly is twelve occurrences");
        assert!(
            schedule.next_due > woke,
            "the next firing is still in the past: {:?}",
            schedule.next_due
        );
        assert!(
            schedule.next_due <= woke + 4 * HOUR,
            "the schedule skipped past the next real occurrence"
        );
        assert!(!schedule.is_due(woke));
    }

    #[test]
    fn an_ordinary_firing_misses_nothing() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let mut schedule = schedule(now);
        let fired = now + 4 * HOUR + Duration::from_secs(5);
        assert_eq!(schedule.advance_past(fired), 0);
        assert_eq!(schedule.next_due, now + 8 * HOUR);
    }

    #[test]
    fn the_record_is_bounded_and_newest_first() {
        // It is rewritten on every firing and read on every window open, and a
        // schedule the operator was not present for is only trustworthy if it
        // can say what it did.
        let now = SystemTime::UNIX_EPOCH;
        let mut schedule = schedule(now);
        for index in 0..(MAX_HISTORY + 5) {
            schedule.record(ScheduleFiring {
                at: now + Duration::from_secs(index as u64),
                result: FiringResult::Started { projects: index },
                missed: 0,
            });
        }
        assert_eq!(schedule.history.len(), MAX_HISTORY);
        assert_eq!(
            schedule.last_firing().map(|firing| firing.result.clone()),
            Some(FiringResult::Started {
                projects: MAX_HISTORY + 4
            })
        );
    }
}
