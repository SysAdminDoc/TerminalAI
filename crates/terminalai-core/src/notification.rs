//! Deduplicated attention notifications for the fleet.
//!
//! Notifications are derived from session transitions rather than emitted for
//! every hook payload. A stable key makes repeated hooks idempotent, the
//! repository group gives a future UI a natural batching boundary, and a
//! notification is retracted as soon as the agent resumes progress.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::session::{Session, SessionId, SessionStatus};

/// Ignore attention signals during the short startup period. Hooks can arrive
/// while the CLI is still wiring its native session and terminal state.
pub const STARTUP_GRACE_PERIOD: Duration = Duration::from_secs(5);
/// Ignore attention signals immediately after a tool begins. Long-running
/// tools often emit intermediate prompts before their real completion state.
///
/// Deliberately shorter than [`AGENT_AUTO_RESOLVE_DEADLINE`] but not by much,
/// because the states this applies to — a permission request — do not expire on
/// their own: nobody answers them but the operator, so the only cost of waiting
/// is lateness, and the benefit is not raising a notification for a prompt the
/// tool itself was about to withdraw.
pub const LONG_TOOL_GRACE_PERIOD: Duration = Duration::from_secs(30);

/// How long an agent waits for an answer before proceeding without one.
///
/// Claude Code's `AskUserQuestion` continues after sixty seconds
/// (anthropics/claude-code#73125). This is the budget the operator actually
/// has, and every grace period below is measured against it rather than chosen
/// on its own.
pub const AGENT_AUTO_RESOLVE_DEADLINE: Duration = Duration::from_secs(60);

/// The grace applied to a question the agent will answer for itself.
///
/// The long-tool grace spent thirty of the operator's sixty seconds — they were
/// told at the halfway point, and if they missed it the agent proceeded on its
/// own. Five seconds still filters the intermediate prompts a tool emits as it
/// starts, and leaves fifty-five of the sixty to answer in. The asymmetry is the
/// point: a late notification about a question costs the answer entirely, while
/// an early one about a permission prompt costs only noise.
pub const QUESTION_GRACE_PERIOD: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AttentionNotification {
    /// Stable across repeated hook deliveries for this session/status pair.
    pub dedup_key: String,
    /// Normalized working-directory key used to batch notifications by repo.
    pub group_key: String,
    pub session_id: SessionId,
    pub status: SessionStatus,
    pub created_at: SystemTime,
    /// Raised because the session stopped making progress, not because the
    /// agent asked for anything. The status is a working one, so without this
    /// the notification would be indistinguishable from a busy session.
    #[serde(default)]
    pub stalled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum NotificationEvent {
    Raised {
        notification: AttentionNotification,
    },
    Retracted {
        dedup_key: String,
        session_id: SessionId,
        group_key: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuppressionReason {
    Startup,
    LongTool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationChange {
    Raised(AttentionNotification),
    Retracted(AttentionNotification),
    Suppressed {
        notification: AttentionNotification,
        reason: SuppressionReason,
    },
}

#[derive(Debug, Clone)]
struct PendingNotification {
    notification: AttentionNotification,
    due: SystemTime,
}

impl NotificationChange {
    /// Convert only visible lifecycle changes into wire events. Suppression is
    /// intentionally silent: the row still reflects the attention status,
    /// while no toast or badge event is created during the grace window.
    pub fn into_event(self) -> Option<NotificationEvent> {
        match self {
            Self::Raised(notification) => Some(NotificationEvent::Raised { notification }),
            Self::Retracted(notification) => Some(NotificationEvent::Retracted {
                dedup_key: notification.dedup_key,
                session_id: notification.session_id,
                group_key: notification.group_key,
            }),
            Self::Suppressed { .. } => None,
        }
    }
}

#[derive(Debug, Default)]
pub struct NotificationCenter {
    active: BTreeMap<String, AttentionNotification>,
    pending: BTreeMap<SessionId, PendingNotification>,
}

impl NotificationCenter {
    /// Observe the result of a session status transition.
    ///
    /// `previous_status` and `previous_state_since` are supplied separately
    /// because an attention hook changes the session's status before the
    /// center decides whether the prior state was inside a grace window.
    pub fn observe(
        &mut self,
        session: &Session,
        previous_status: SessionStatus,
        previous_state_since: SystemTime,
        now: SystemTime,
    ) -> Vec<NotificationChange> {
        let Some(status) = attention_status(session.status) else {
            return self.retract_session(&session.id);
        };

        let notification = AttentionNotification::new(session, status, now);
        if self.active.contains_key(&notification.dedup_key) {
            return Vec::new();
        }

        let mut changes = self.retract_session(&session.id);
        if let Some(reason) = suppression_reason(status, previous_status, previous_state_since, now)
        {
            self.pending.insert(
                session.id.clone(),
                PendingNotification {
                    notification: notification.clone(),
                    due: suppression_deadline(reason, status, now),
                },
            );
            changes.push(NotificationChange::Suppressed {
                notification,
                reason,
            });
            return changes;
        }
        self.active
            .insert(notification.dedup_key.clone(), notification.clone());
        changes.push(NotificationChange::Raised(notification));
        changes
    }

    pub fn retract_session(&mut self, id: &SessionId) -> Vec<NotificationChange> {
        self.pending.remove(id);
        let keys: Vec<_> = self
            .active
            .iter()
            .filter(|(_, notification)| notification.session_id == *id)
            .map(|(key, _)| key.clone())
            .collect();
        keys.into_iter()
            .filter_map(|key| self.active.remove(&key))
            .map(NotificationChange::Retracted)
            .collect()
    }

    /// Re-evaluate attention states whose initial notification fell inside a
    /// grace window. The registry calls this from its existing scheduler, so a
    /// persistent prompt is raised even if no later status transition arrives.
    pub fn recheck(
        &mut self,
        sessions: &[Session],
        now: SystemTime,
    ) -> Vec<NotificationChange> {
        let due: Vec<_> = self
            .pending
            .iter()
            .filter(|(_, pending)| pending.due <= now)
            .map(|(id, _)| id.clone())
            .collect();
        let mut changes = Vec::new();
        for id in due {
            let Some(pending) = self.pending.remove(&id) else {
                continue;
            };
            let Some(session) = sessions
                .iter()
                .find(|session| session.id == pending.notification.session_id)
            else {
                continue;
            };
            if session.status != pending.notification.status {
                continue;
            }
            let notification = AttentionNotification::new(session, session.status, now);
            if self.active.contains_key(&notification.dedup_key) {
                continue;
            }
            changes.extend(self.retract_session(&id));
            self.active
                .insert(notification.dedup_key.clone(), notification.clone());
            changes.push(NotificationChange::Raised(notification));
        }

        // A stall is the absence of a transition, so it can only be noticed
        // here. One notification per stalled session, retracted by the ordinary
        // path as soon as the session moves to any other status.
        for session in sessions {
            if !session.stalled {
                continue;
            }
            let notification = AttentionNotification::stalled(session, now);
            if self.active.contains_key(&notification.dedup_key) {
                continue;
            }
            self.active
                .insert(notification.dedup_key.clone(), notification.clone());
            changes.push(NotificationChange::Raised(notification));
        }
        changes
    }

    pub fn active(&self) -> Vec<AttentionNotification> {
        self.active.values().cloned().collect()
    }

    /// Return active notifications grouped by normalized repository key.
    pub fn active_by_group(&self) -> BTreeMap<String, Vec<AttentionNotification>> {
        let mut grouped = BTreeMap::new();
        for notification in self.active.values() {
            grouped
                .entry(notification.group_key.clone())
                .or_insert_with(Vec::new)
                .push(notification.clone());
        }
        grouped
    }
}

impl AttentionNotification {
    fn new(session: &Session, status: SessionStatus, now: SystemTime) -> Self {
        let group_key = repo_group_key(&session.cwd);
        let dedup_key = format!(
            "repo={group_key};session={};status={}",
            session.id,
            status_key(status)
        );
        Self {
            dedup_key,
            group_key,
            session_id: session.id.clone(),
            status,
            created_at: now,
            stalled: false,
        }
    }

    /// A stall notification for a session that is still nominally working.
    ///
    /// Its own dedup key, so it is raised once and survives alongside nothing
    /// else: the session leaving the working status retracts it through the
    /// ordinary path, because the key is scoped to the session.
    fn stalled(session: &Session, now: SystemTime) -> Self {
        let group_key = repo_group_key(&session.cwd);
        Self {
            dedup_key: format!("repo={group_key};session={};status=stalled", session.id),
            group_key,
            session_id: session.id.clone(),
            status: session.status,
            created_at: now,
            stalled: true,
        }
    }
}

pub fn repo_group_key(path: &Path) -> String {
    let normalized = path
        .to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_ascii_lowercase();
    if normalized.is_empty() {
        "/".into()
    } else {
        normalized
    }
}

fn attention_status(status: SessionStatus) -> Option<SessionStatus> {
    matches!(
        status,
        SessionStatus::NeedsApproval | SessionStatus::AwaitingInput | SessionStatus::NeedsYou
    )
    .then_some(status)
}

fn status_key(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::NeedsApproval => "needs-approval",
        SessionStatus::AwaitingInput => "awaiting-input",
        SessionStatus::NeedsYou => "needs-you",
        _ => unreachable!("status_key only receives attention states"),
    }
}

/// Whether the agent will proceed on its own if nobody answers.
///
/// A question does; a permission request does not. That difference is what
/// decides how much of the operator's budget the fleet is allowed to spend
/// before telling them.
pub fn expires_without_an_answer(status: SessionStatus) -> bool {
    matches!(
        status,
        SessionStatus::AwaitingInput | SessionStatus::NeedsYou
    )
}

/// How long a transition into `status` may be held back.
fn tool_grace_for(status: SessionStatus) -> Duration {
    if expires_without_an_answer(status) {
        QUESTION_GRACE_PERIOD
    } else {
        LONG_TOOL_GRACE_PERIOD
    }
}

fn suppression_reason(
    status: SessionStatus,
    previous_status: SessionStatus,
    previous_state_since: SystemTime,
    now: SystemTime,
) -> Option<SuppressionReason> {
    let elapsed = now.duration_since(previous_state_since).unwrap_or_default();
    if previous_status == SessionStatus::Starting && elapsed < STARTUP_GRACE_PERIOD {
        Some(SuppressionReason::Startup)
    } else if previous_status == SessionStatus::Working && elapsed < tool_grace_for(status) {
        Some(SuppressionReason::LongTool)
    } else {
        None
    }
}

fn suppression_deadline(
    reason: SuppressionReason,
    status: SessionStatus,
    now: SystemTime,
) -> SystemTime {
    let grace = match reason {
        SuppressionReason::Startup => STARTUP_GRACE_PERIOD,
        SuppressionReason::LongTool => tool_grace_for(status),
    };
    now.checked_add(grace).unwrap_or(now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::Agent;
    use crate::launch::spec_for;
    use std::path::Path;

    fn session(path: &str, status: SessionStatus) -> Session {
        let spec = spec_for(Agent::Claude, Path::new(path));
        let mut session = Session::new(SessionId::new(1), &spec);
        session.status = status;
        session
    }

    #[test]
    fn attention_has_stable_key_and_repository_group() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let mut session = session(r"C:\Repos\TerminalAI\", SessionStatus::NeedsYou);
        session.state_since = SystemTime::UNIX_EPOCH;
        let mut center = NotificationCenter::default();

        let first = center
            .observe(&session, SessionStatus::Idle, session.state_since, now)
            .into_iter()
            .next()
            .expect("first attention event");
        let NotificationChange::Raised(first) = first else {
            panic!("expected raised event")
        };
        assert_eq!(first.group_key, "c:/repos/terminalai");
        assert!(first.dedup_key.contains("session=s0001"));
        assert!(center
            .observe(&session, SessionStatus::NeedsYou, session.state_since, now)
            .is_empty());
        assert_eq!(center.active_by_group()["c:/repos/terminalai"].len(), 1);
    }

    #[test]
    fn proceeding_status_retracts_the_active_notification() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let mut session = session("repo", SessionStatus::NeedsApproval);
        session.state_since = SystemTime::UNIX_EPOCH;
        let mut center = NotificationCenter::default();
        assert!(matches!(
            center
                .observe(&session, SessionStatus::Idle, session.state_since, now)
                .first(),
            Some(NotificationChange::Raised(_))
        ));

        session.status = SessionStatus::Working;
        let changes = center.observe(
            &session,
            SessionStatus::NeedsApproval,
            session.state_since,
            now,
        );
        assert!(matches!(
            changes.first(),
            Some(NotificationChange::Retracted(_))
        ));
        assert!(center.active().is_empty());
    }

    #[test]
    fn attention_status_changes_retract_prior_status_and_idle_clears_all() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let mut session = session("repo", SessionStatus::NeedsApproval);
        session.state_since = SystemTime::UNIX_EPOCH;
        let mut center = NotificationCenter::default();

        assert!(matches!(
            center
                .observe(&session, SessionStatus::Idle, session.state_since, now)
                .first(),
            Some(NotificationChange::Raised(_))
        ));

        session.status = SessionStatus::AwaitingInput;
        let changes = center.observe(
            &session,
            SessionStatus::NeedsApproval,
            session.state_since,
            now,
        );
        assert_eq!(
            changes
                .iter()
                .filter(|change| matches!(change, NotificationChange::Retracted(_)))
                .count(),
            1
        );
        assert!(changes
            .iter()
            .any(|change| matches!(change, NotificationChange::Raised(_))));
        assert_eq!(center.active().len(), 1);

        session.status = SessionStatus::Idle;
        let changes = center.observe(
            &session,
            SessionStatus::AwaitingInput,
            session.state_since,
            now,
        );
        assert_eq!(changes.len(), 1);
        assert!(matches!(
            changes.first(),
            Some(NotificationChange::Retracted(_))
        ));
        assert!(center.active().is_empty());
    }

    #[test]
    fn startup_and_long_tool_grace_suppress_attention_without_active_state() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let mut center = NotificationCenter::default();
        let mut startup = session("repo", SessionStatus::NeedsYou);
        let startup_since = now - Duration::from_secs(1);
        startup.state_since = now;
        assert!(matches!(
            center
                .observe(&startup, SessionStatus::Starting, startup_since, now)
                .first(),
            Some(NotificationChange::Suppressed {
                reason: SuppressionReason::Startup,
                ..
            })
        ));

        let mut tool = session("repo", SessionStatus::NeedsYou);
        let tool_since = now - Duration::from_secs(1);
        tool.state_since = now;
        assert!(matches!(
            center
                .observe(&tool, SessionStatus::Working, tool_since, now)
                .first(),
            Some(NotificationChange::Suppressed {
                reason: SuppressionReason::LongTool,
                ..
            })
        ));
        assert!(center.active().is_empty());

        assert!(matches!(
            center
                .observe(
                    &tool,
                    SessionStatus::Working,
                    now - Duration::from_secs(31),
                    now
                )
                .first(),
            Some(NotificationChange::Raised(_))
        ));
    }

    #[test]
    fn suppressed_attention_is_raised_after_the_grace_recheck() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let mut tool = session("repo", SessionStatus::NeedsYou);
        tool.state_since = now;
        let mut center = NotificationCenter::default();
        assert!(matches!(
            center
                .observe(
                    &tool,
                    SessionStatus::Working,
                    now - Duration::from_secs(1),
                    now
                )
                .first(),
            Some(NotificationChange::Suppressed {
                reason: SuppressionReason::LongTool,
                ..
            })
        ));

        let changes = center.recheck(
            std::slice::from_ref(&tool),
            now + LONG_TOOL_GRACE_PERIOD + Duration::from_secs(1),
        );
        assert!(matches!(
            changes.first(),
            Some(NotificationChange::Raised(_))
        ));
        assert_eq!(center.active().len(), 1);
    }

    #[test]
    fn lifecycle_changes_are_wire_serializable() {
        let notification = NotificationEvent::Retracted {
            dedup_key: "repo=x".into(),
            session_id: SessionId::new(2),
            group_key: "x".into(),
        };
        let json = serde_json::to_string(&notification).expect("encode");
        assert!(json.contains("\"kind\":\"retracted\""));
        assert!(matches!(
            serde_json::from_str::<NotificationEvent>(&json),
            Ok(NotificationEvent::Retracted { .. })
        ));
    }

    /// The operator gets sixty seconds to answer a question before the agent
    /// answers it for them. Spending thirty of those on a grace period told them
    /// at the halfway point.
    #[test]
    fn a_question_is_reported_well_inside_the_agents_own_deadline() {
        assert!(
            QUESTION_GRACE_PERIOD * 4 < AGENT_AUTO_RESOLVE_DEADLINE,
            "the grace period spends most of the answering budget"
        );

        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        // Ten seconds into a tool: past a question's grace, inside a permission
        // request's. The two must not be suppressed alike.
        let began = now - Duration::from_secs(10);

        for (status, suppressed) in [
            (SessionStatus::AwaitingInput, false),
            (SessionStatus::NeedsYou, false),
            (SessionStatus::NeedsApproval, true),
        ] {
            let mut center = NotificationCenter::default();
            let mut row = session("repo", status);
            row.state_since = now;
            let changes = center.observe(&row, SessionStatus::Working, began, now);
            let held = matches!(
                changes.first(),
                Some(NotificationChange::Suppressed { .. })
            );
            assert_eq!(
                held, suppressed,
                "{status:?} was {} ten seconds into a tool",
                if held { "held back" } else { "raised" }
            );
        }
    }

    #[test]
    fn only_the_states_the_agent_can_answer_for_itself_are_shortened() {
        assert!(expires_without_an_answer(SessionStatus::AwaitingInput));
        assert!(expires_without_an_answer(SessionStatus::NeedsYou));
        // Nobody but the operator answers a permission request, so there is no
        // deadline to race and no reason to trade away the de-noising.
        assert!(!expires_without_an_answer(SessionStatus::NeedsApproval));
        assert!(!expires_without_an_answer(SessionStatus::Working));
    }
}
