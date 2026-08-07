//! Whether the fleet may start anything new.
//!
//! Every decision here is a pure function of the configuration and a summary of
//! what the fleet already holds. This module takes no lock, spawns no thread and
//! reads no clock: the registry owns that state and passes the answer in. The
//! split is deliberate — the gate is the one piece of registry behaviour that
//! decides whether an operator's launch happens at all, and it should be
//! testable without a process, a pty or a wall clock.

use std::time::{Duration, SystemTime};

pub const DEFAULT_MAX_LIVE_SESSIONS: usize = 3;
pub const DEFAULT_SESSION_BUDGET_USD: f64 = 5.0;

/// Admission limits are owned by the daemon but kept in the registry so every
/// process launch, including automatic restarts, observes the same cap.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AdmissionConfig {
    pub max_live_sessions: usize,
    /// Applied to Claude launches that did not supply an explicit cap. Codex
    /// has no equivalent launcher flag and therefore leaves this unused.
    pub default_budget_usd: Option<f64>,
    /// Fleet spend allowed inside `spend_window` before nothing new starts.
    /// `None` disables the ceiling; running sessions are never stopped by it.
    pub spend_ceiling_usd: Option<f64>,
    /// How far back the ceiling looks.
    pub spend_window: Duration,
    /// Private commit the whole fleet may hold before nothing new starts.
    /// Admission projects an unsampled session at its agent's measured typical
    /// size rather than at zero, because admitting on "we have not looked yet"
    /// is how a machine gets oversubscribed.
    pub memory_budget_bytes: Option<u64>,
    /// Per-session job memory cap. Exceeding it fails allocations inside the
    /// agent; it does not terminate the session.
    pub session_memory_cap_bytes: Option<u64>,
    /// How many processes one session's job may hold at once.
    pub max_processes_per_session: Option<u32>,
}

/// What one unsampled session is assumed to need.
///
/// Measured on the development machine and recorded in `CLAUDE.md`: Claude Code
/// around 509 MB, Codex around 322 MB. Used only until the session reports its
/// own figure, so a wrong guess corrects itself within a sampling interval.
pub const ASSUMED_SESSION_BYTES_CLAUDE: u64 = 509 * 1024 * 1024;
pub const ASSUMED_SESSION_BYTES_CODEX: u64 = 322 * 1024 * 1024;

pub fn assumed_session_bytes(agent: crate::agent::Agent) -> u64 {
    match agent {
        crate::agent::Agent::Claude => ASSUMED_SESSION_BYTES_CLAUDE,
        crate::agent::Agent::Codex => ASSUMED_SESSION_BYTES_CODEX,
    }
}

impl AdmissionConfig {
    pub fn new(max_live_sessions: usize, default_budget_usd: Option<f64>) -> Self {
        Self {
            max_live_sessions: max_live_sessions.max(1),
            default_budget_usd: default_budget_usd
                .filter(|value| value.is_finite() && *value >= 0.0),
            spend_ceiling_usd: None,
            spend_window: crate::spend::DEFAULT_SPEND_WINDOW,
            memory_budget_bytes: None,
            session_memory_cap_bytes: None,
            max_processes_per_session: None,
        }
    }

    /// Set the memory limits. Zero and non-finite figures disable rather than
    /// admitting nothing, for the same reason the spend ceiling does: a
    /// misconfigured limit must not halt the fleet.
    pub fn with_memory_limits(
        mut self,
        budget_bytes: Option<u64>,
        session_cap_bytes: Option<u64>,
        max_processes: Option<u32>,
    ) -> Self {
        self.memory_budget_bytes = budget_bytes.filter(|bytes| *bytes > 0);
        self.session_memory_cap_bytes = session_cap_bytes.filter(|bytes| *bytes > 0);
        self.max_processes_per_session = max_processes.filter(|count| *count > 0);
        self
    }

    /// The job limits one session's process tree is created with.
    pub fn job_limits(&self) -> crate::process_tree::JobLimits {
        crate::process_tree::JobLimits {
            memory_bytes: self.session_memory_cap_bytes,
            active_processes: self.max_processes_per_session,
        }
    }

    /// Set the fleet ceiling. A non-finite or negative figure disables it
    /// rather than admitting nothing, and a zero-length window is refused for
    /// the same reason: a misconfigured ceiling must not halt the fleet.
    pub fn with_spend_ceiling(
        mut self,
        ceiling_usd: Option<f64>,
        window: Option<Duration>,
    ) -> Self {
        self.spend_ceiling_usd = ceiling_usd.filter(|value| value.is_finite() && *value >= 0.0);
        self.spend_window = window
            .filter(|value| !value.is_zero())
            .unwrap_or(crate::spend::DEFAULT_SPEND_WINDOW);
        self
    }

    /// Read daemon-wide limits without introducing a second config file.
    /// TERMINALAI_DEFAULT_BUDGET_USD=none disables the Claude default cap.
    pub fn from_environment() -> Result<Self, String> {
        let max_live_sessions = std::env::var("TERMINALAI_MAX_LIVE_SESSIONS")
            .ok()
            .map(|value| {
                value.parse::<usize>().map_err(|_| {
                    "TERMINALAI_MAX_LIVE_SESSIONS must be a positive integer".to_string()
                })
            })
            .transpose()?
            .unwrap_or(DEFAULT_MAX_LIVE_SESSIONS);
        let default_budget_usd = match std::env::var("TERMINALAI_DEFAULT_BUDGET_USD") {
            Ok(value)
                if value.eq_ignore_ascii_case("none") || value.eq_ignore_ascii_case("off") =>
            {
                None
            }
            Ok(value) => Some(value.parse::<f64>().map_err(|_| {
                "TERMINALAI_DEFAULT_BUDGET_USD must be a non-negative decimal or 'none'".to_string()
            })?),
            Err(_) => Some(DEFAULT_SESSION_BUDGET_USD),
        };
        let spend_ceiling_usd = match std::env::var("TERMINALAI_SPEND_CEILING_USD") {
            Ok(value)
                if value.eq_ignore_ascii_case("none") || value.eq_ignore_ascii_case("off") =>
            {
                None
            }
            Ok(value) => Some(value.parse::<f64>().map_err(|_| {
                "TERMINALAI_SPEND_CEILING_USD must be a non-negative decimal or 'none'".to_string()
            })?),
            Err(_) => None,
        };
        let spend_window = std::env::var("TERMINALAI_SPEND_WINDOW_HOURS")
            .ok()
            .map(|value| {
                value
                    .parse::<f64>()
                    .ok()
                    .filter(|hours| hours.is_finite() && *hours > 0.0)
                    .map(|hours| Duration::from_secs_f64(hours * 3600.0))
                    .ok_or_else(|| {
                        "TERMINALAI_SPEND_WINDOW_HOURS must be a positive decimal".to_string()
                    })
            })
            .transpose()?;
        let config = Self::new(max_live_sessions, default_budget_usd);
        if config.max_live_sessions != max_live_sessions {
            return Err("TERMINALAI_MAX_LIVE_SESSIONS must be at least 1".into());
        }
        if config.default_budget_usd != default_budget_usd {
            return Err("TERMINALAI_DEFAULT_BUDGET_USD must be finite and non-negative".into());
        }
        let config = config.with_spend_ceiling(spend_ceiling_usd, spend_window);
        if config.spend_ceiling_usd != spend_ceiling_usd {
            return Err("TERMINALAI_SPEND_CEILING_USD must be finite and non-negative".into());
        }
        let memory_budget_bytes = megabytes_from_env("TERMINALAI_MEMORY_BUDGET_MB")?;
        let session_memory_cap_bytes = megabytes_from_env("TERMINALAI_SESSION_MEMORY_CAP_MB")?;
        let max_processes_per_session = std::env::var("TERMINALAI_MAX_PROCESSES_PER_SESSION")
            .ok()
            .map(|value| {
                value.parse::<u32>().map_err(|_| {
                    "TERMINALAI_MAX_PROCESSES_PER_SESSION must be a positive integer".to_string()
                })
            })
            .transpose()?;
        let config = config.with_memory_limits(
            memory_budget_bytes,
            session_memory_cap_bytes,
            max_processes_per_session,
        );
        Ok(config)
    }
}

/// Megabytes from an environment variable, as bytes. `none`/`off` disables.
fn megabytes_from_env(name: &str) -> Result<Option<u64>, String> {
    match std::env::var(name) {
        Ok(value) if value.eq_ignore_ascii_case("none") || value.eq_ignore_ascii_case("off") => {
            Ok(None)
        }
        Ok(value) => value
            .parse::<u64>()
            .map(|megabytes| Some(megabytes.saturating_mul(1024 * 1024)))
            .map_err(|_| format!("{name} must be a non-negative integer of megabytes or 'none'")),
        Err(_) => Ok(None),
    }
}

/// Why the gate is not starting anything new right now.
///
/// Kept as one value so every admission site gives the same answer, and so the
/// operator is told which limit they hit rather than watching a row sit in
/// `Queued` with no explanation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AdmissionBlock {
    /// Every live slot is taken.
    SlotsFull,
    /// Fleet spend inside the window has reached the ceiling.
    SpendCeiling,
    /// Admitting another session would put projected private commit over the
    /// memory budget.
    MemoryBudget,
}

impl Default for AdmissionConfig {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_LIVE_SESSIONS, None)
    }
}

/// What the fleet already holds, as the gate sees it.
///
/// The registry walks its own state to build this — which sessions hold a slot,
/// what they are expected to cost in memory, and what has been spent inside the
/// window — and then asks [`block`]. Passing a summary rather than the live map
/// is what keeps the decision free of the registry's lock and clock.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct FleetDemand {
    /// Sessions holding an admission slot.
    ///
    /// Rate-limited sessions are excluded by the caller: they are running, but
    /// the provider is refusing them work, so counting them would keep a queued
    /// session waiting behind a process that provably cannot progress.
    pub admitted: usize,
    /// Sessions whose process is still resident, whether or not it holds a slot.
    ///
    /// Always at least [`Self::admitted`]. The two differ by exactly the
    /// rate-limited rows, which is the whole reason this field exists: a slot is
    /// a claim on *progress* and can be released the moment a provider starts
    /// refusing, but the agent process does not exit and does not give its
    /// memory back.
    pub resident: usize,
    /// Private commit the resident sessions are expected to hold, counting a
    /// session that has not been sampled yet at its agent's measured typical
    /// size.
    pub projected_memory_bytes: u64,
    /// Fleet spend inside the ceiling's window, as of the caller's `now`.
    pub spend_window_usd: f64,
}

impl FleetDemand {
    /// Count one session that holds a slot. It is resident too — nothing holds a
    /// slot without a process behind it.
    pub fn admit(&mut self, agent: crate::agent::Agent, memory_bytes: Option<u64>) {
        self.admitted = self.admitted.saturating_add(1);
        self.reside(agent, memory_bytes);
    }

    /// Count one session that has a process but no slot: a rate-limited row.
    ///
    /// It cannot make progress, so it is right that it is not blocking the
    /// queue — but its private commit is as real as any other session's, and
    /// leaving it out of the projection lets the gate admit work the machine
    /// cannot physically hold.
    pub fn reside(&mut self, agent: crate::agent::Agent, memory_bytes: Option<u64>) {
        self.resident = self.resident.saturating_add(1);
        self.projected_memory_bytes = self
            .projected_memory_bytes
            .saturating_add(memory_bytes.unwrap_or_else(|| assumed_session_bytes(agent)));
    }
}

/// The one place that decides whether anything new may start.
///
/// Every admission site calls this so the slot cap and the spend ceiling cannot
/// drift apart: a second copy of "is there room" is how one path ends up
/// enforcing a limit the other ignores.
pub fn block(config: &AdmissionConfig, demand: &FleetDemand) -> Option<AdmissionBlock> {
    if demand.admitted >= config.max_live_sessions {
        return Some(AdmissionBlock::SlotsFull);
    }
    if let Some(ceiling) = config.spend_ceiling_usd {
        if demand.spend_window_usd >= ceiling {
            return Some(AdmissionBlock::SpendCeiling);
        }
    }
    if let Some(budget) = config.memory_budget_bytes {
        // Projection, not measurement: the session being admitted has no
        // process yet, so the question is whether the fleet would still fit
        // once it does. Headroom for one more is what makes this a gate rather
        // than a post-mortem — blocking only once the total is already over
        // admits exactly the session that puts it there.
        let headroom = ASSUMED_SESSION_BYTES_CLAUDE.min(ASSUMED_SESSION_BYTES_CODEX);
        // An empty fleet always gets one session: a budget too small for any
        // agent is a misconfiguration, and halting entirely would hide it.
        //
        // Residency, not slots: a fleet whose only session is rate-limited still
        // has an agent process holding half a gigabyte, so it is not empty and
        // does not get the exemption.
        let occupied = demand.resident > 0;
        if occupied && demand.projected_memory_bytes.saturating_add(headroom) > budget {
            return Some(AdmissionBlock::MemoryBudget);
        }
    }
    None
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AdmissionSnapshot {
    pub max_live_sessions: usize,
    pub live_sessions: usize,
    pub queued_sessions: usize,
    pub aggregate_cost_usd: f64,
    /// Nonblocking event delivery drops since daemon start. Output and row
    /// updates are deliberately lossy when a subscriber is stalled; clients
    /// can recover authoritative state with Snapshot/Reattach.
    #[serde(default)]
    pub dropped_events: u64,
    /// Which price table any reported cost was computed against. Shown beside
    /// the figure so a stale table is visible rather than assumed current.
    #[serde(default)]
    pub pricing_version: String,
    /// The upstream commit date the embedded price table came from,
    /// `YYYY-MM-DD`. `None` when the vendored snapshot could not be parsed and
    /// the hardcoded fallback is in use — which is itself worth showing, since
    /// a fallback table has no date to age at all.
    #[serde(default)]
    pub pricing_committed: Option<String>,
    /// How many sessions actually reported a cost. Zero means the fleet spend is
    /// unknown, not zero.
    #[serde(default)]
    pub sessions_reporting_cost: usize,
    /// Fleet spend inside the ceiling's window. Always reported, so the figure
    /// is visible before a ceiling is ever configured.
    #[serde(default)]
    pub spend_window_usd: f64,
    /// The configured ceiling, if any. `None` means nothing is being enforced.
    #[serde(default)]
    pub spend_ceiling_usd: Option<f64>,
    /// Width of the rolling window, in hours.
    #[serde(default)]
    pub spend_window_hours: f64,
    /// Why nothing new is starting, when something is stopping it.
    #[serde(default)]
    pub admission_block: Option<AdmissionBlock>,
    /// Private commit the fleet may hold, if a budget is configured.
    #[serde(default)]
    pub memory_budget_bytes: Option<u64>,
    /// What the fleet is expected to hold, counting unsampled sessions at their
    /// agent's measured typical size.
    #[serde(default)]
    pub projected_memory_bytes: u64,
    /// The per-session job cap, if one is configured.
    #[serde(default)]
    pub session_memory_cap_bytes: Option<u64>,
    /// Sessions whose allocations the job is currently refusing.
    #[serde(default)]
    pub memory_limited_sessions: usize,
    /// Agents that reported expired credentials. Only an explicit "not logged
    /// in" lands here; an unreachable probe stays silent rather than raising a
    /// banner the operator cannot act on.
    #[serde(default)]
    pub expired_auth: Vec<crate::auth::AgentAuth>,
    /// Agents whose own launcher can enforce a per-session budget. Codex has no
    /// documented equivalent, so its sessions are admission-refused only and
    /// the header has to say so rather than implying a hard stop.
    #[serde(default)]
    pub budget_enforced_agents: Vec<String>,
    /// Sessions a provider is currently refusing work. Surfaced in the header
    /// because a limited fleet otherwise reads as a busy one.
    #[serde(default)]
    pub rate_limited_sessions: usize,
    /// The earliest reported reset among them, if any of them said. `None` with
    /// a nonzero count means no session reported a reset time — the header says
    /// so rather than showing a guess.
    #[serde(default)]
    pub earliest_rate_limit_reset: Option<SystemTime>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::Agent;

    /// A fleet where every resident session also holds a slot — the ordinary
    /// case. Rate limiting is what separates the two, and it has its own tests.
    fn demand(admitted: usize, projected: u64) -> FleetDemand {
        FleetDemand {
            admitted,
            resident: admitted,
            projected_memory_bytes: projected,
            spend_window_usd: 0.0,
        }
    }

    #[test]
    fn slots_fill_before_any_other_limit_is_consulted() {
        let config = AdmissionConfig::new(2, None);
        assert_eq!(block(&config, &demand(1, 0)), None);
        assert_eq!(block(&config, &demand(2, 0)), Some(AdmissionBlock::SlotsFull));
        assert_eq!(block(&config, &demand(3, 0)), Some(AdmissionBlock::SlotsFull));
    }

    #[test]
    fn the_spend_ceiling_blocks_at_the_ceiling_not_past_it() {
        let config = AdmissionConfig::new(8, None).with_spend_ceiling(Some(2.0), None);
        let mut under = demand(1, 0);
        under.spend_window_usd = 1.99;
        assert_eq!(block(&config, &under), None);
        let mut at = demand(1, 0);
        at.spend_window_usd = 2.0;
        assert_eq!(block(&config, &at), Some(AdmissionBlock::SpendCeiling));
    }

    #[test]
    fn an_empty_fleet_is_admitted_even_under_an_unmeetable_memory_budget() {
        // A budget smaller than any agent is a misconfiguration. Refusing every
        // launch would hide it behind a fleet that simply never starts.
        let config = AdmissionConfig::new(4, None).with_memory_limits(Some(1024), None, None);
        assert_eq!(block(&config, &demand(0, 0)), None);
        assert_eq!(
            block(&config, &demand(1, 0)),
            Some(AdmissionBlock::MemoryBudget)
        );
    }

    #[test]
    fn the_memory_gate_leaves_headroom_for_the_session_being_admitted() {
        let budget = ASSUMED_SESSION_BYTES_CODEX * 4;
        let config = AdmissionConfig::new(8, None).with_memory_limits(Some(budget), None, None);
        // Room for the smallest agent on top of what is already held.
        let fits = budget - ASSUMED_SESSION_BYTES_CODEX;
        assert_eq!(block(&config, &demand(1, fits)), None);
        assert_eq!(
            block(&config, &demand(1, fits + 1)),
            Some(AdmissionBlock::MemoryBudget)
        );
    }

    #[test]
    fn a_rate_limited_session_releases_its_slot_but_not_its_memory() {
        // The two limits are released at different moments. A provider refusing
        // a session frees it from blocking the queue; it does not make the agent
        // process exit, and the budget exists to stop the machine being
        // oversubscribed by processes that are actually there.
        let budget = ASSUMED_SESSION_BYTES_CLAUDE * 2;
        let config = AdmissionConfig::new(4, None).with_memory_limits(Some(budget), None, None);
        let mut demand = FleetDemand::default();
        demand.reside(Agent::Claude, None);
        demand.reside(Agent::Claude, None);

        assert_eq!(demand.admitted, 0, "neither row holds a slot");
        assert_eq!(demand.resident, 2);
        assert_eq!(
            block(&config, &demand),
            Some(AdmissionBlock::MemoryBudget),
            "two resident agents fill the budget even with every slot free"
        );
    }

    #[test]
    fn a_fleet_of_only_rate_limited_rows_is_not_an_empty_fleet() {
        // The empty-fleet exemption exists so a misconfigured budget cannot halt
        // everything. A fleet holding a resident agent is not that case.
        let config = AdmissionConfig::new(4, None).with_memory_limits(Some(1024), None, None);
        let mut demand = FleetDemand::default();
        assert_eq!(block(&config, &demand), None, "nothing is running yet");
        demand.reside(Agent::Claude, None);
        assert_eq!(
            block(&config, &demand),
            Some(AdmissionBlock::MemoryBudget),
            "a rate-limited agent still holds its memory"
        );
    }

    #[test]
    fn an_unsampled_session_is_projected_at_its_agents_measured_size() {
        let mut demand = FleetDemand::default();
        demand.admit(Agent::Claude, None);
        demand.admit(Agent::Codex, None);
        demand.admit(Agent::Claude, Some(64 * 1024 * 1024));
        assert_eq!(demand.admitted, 3);
        assert_eq!(
            demand.projected_memory_bytes,
            ASSUMED_SESSION_BYTES_CLAUDE + ASSUMED_SESSION_BYTES_CODEX + 64 * 1024 * 1024
        );
    }

    #[test]
    fn misconfigured_limits_disable_rather_than_admitting_nothing() {
        let config = AdmissionConfig::new(0, Some(f64::NAN))
            .with_spend_ceiling(Some(f64::INFINITY), Some(Duration::ZERO))
            .with_memory_limits(Some(0), Some(0), Some(0));
        assert_eq!(config.max_live_sessions, 1);
        assert_eq!(config.default_budget_usd, None);
        assert_eq!(config.spend_ceiling_usd, None);
        assert_eq!(config.spend_window, crate::spend::DEFAULT_SPEND_WINDOW);
        assert_eq!(config.memory_budget_bytes, None);
        assert_eq!(config.session_memory_cap_bytes, None);
        assert_eq!(config.max_processes_per_session, None);
    }
}
