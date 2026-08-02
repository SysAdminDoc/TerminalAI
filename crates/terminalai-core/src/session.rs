//! The fleet row model.
//!
//! A [`Session`] is what one line of the fleet list renders. It is deliberately
//! small and cheap to clone: the GUI re-renders the whole list on every status
//! change, and there may be thirty tracked sessions even while only a bounded
//! number of agent processes remain live.
//!
//! Status is *pushed* here by agent hooks and transcript tailing, never polled.
//! Polling a pty every two seconds is what every other multiplexer does, and it
//! is both wrong (you miss transitions between polls) and expensive.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use crate::agent::Agent;
use crate::launch::{Effort, LaunchSpec};

#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct SessionId(pub String);

impl SessionId {
    pub fn new(seq: u64) -> Self {
        SessionId(format!("s{seq:04}"))
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What the status dot shows. Ordering matters: the fleet list sorts by this
/// descending, so anything wanting the user floats to the top.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum SessionStatus {
    /// Process gone.
    Exited,
    /// Started, nothing has happened yet.
    Starting,
    /// Waiting for the user to type.
    Idle,
    /// Model is generating.
    Thinking,
    /// Running a tool — the long tail where sessions get stuck.
    Working,
    /// Blocked on a permission prompt or a question. Demands attention.
    NeedsYou,
    /// Waiting for the user to answer an idle prompt.
    AwaitingInput,
    /// Waiting for an explicit permission decision.
    NeedsApproval,
}

impl SessionStatus {
    /// Colour token for the UI theme (Catppuccin Mocha names).
    pub fn colour(self) -> &'static str {
        match self {
            SessionStatus::Exited => "overlay0",
            SessionStatus::Starting => "sapphire",
            SessionStatus::Idle => "surface2",
            SessionStatus::Thinking => "mauve",
            SessionStatus::Working => "yellow",
            SessionStatus::NeedsApproval => "peach",
            SessionStatus::AwaitingInput => "yellow",
            SessionStatus::NeedsYou => "peach",
        }
    }

    pub fn is_live(self) -> bool {
        !matches!(self, SessionStatus::Exited)
    }
}

/// One row of the fleet list.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub agent: Agent,
    /// User-supplied, else derived from the folder name.
    pub name: String,
    pub cwd: PathBuf,
    pub model: Option<String>,
    pub effort: Option<Effort>,
    pub status: SessionStatus,
    /// Last line of agent output, trimmed for the row.
    pub last_line: String,
    /// Native session id, once the agent reports one. Enables resume and fork.
    pub native_id: Option<String>,
    pub started_at: SystemTime,
    pub status_since: SystemTime,
    /// Accumulated spend, when the agent reports it.
    pub cost_usd: Option<f64>,
    /// Set when the session enters an attention state and cleared when the user looks.
    pub unread: bool,
    /// Pinned sessions keep a live terminal grid even when not focused.
    pub pinned: bool,
}

impl Session {
    pub fn new(id: SessionId, spec: &LaunchSpec) -> Self {
        let now = SystemTime::now();
        let name = spec.name.clone().unwrap_or_else(|| folder_label(&spec.cwd));
        Self {
            id,
            agent: spec.agent,
            name,
            cwd: spec.cwd.clone(),
            model: spec.model.clone(),
            effort: spec.effort,
            status: SessionStatus::Starting,
            last_line: String::new(),
            native_id: None,
            started_at: now,
            status_since: now,
            cost_usd: None,
            unread: false,
            pinned: false,
        }
    }

    /// Apply a status transition, stamping the clock and raising the unread flag
    /// when the session starts wanting something.
    pub fn set_status(&mut self, status: SessionStatus) {
        if self.status == status {
            return;
        }
        if matches!(
            status,
            SessionStatus::NeedsApproval | SessionStatus::AwaitingInput | SessionStatus::NeedsYou
        ) {
            self.unread = true;
        }
        self.status = status;
        self.status_since = SystemTime::now();
    }

    /// How long the session has held its current status — the number that tells
    /// you at a glance which agent is wedged.
    pub fn in_status_for(&self) -> Duration {
        SystemTime::now()
            .duration_since(self.status_since)
            .unwrap_or_default()
    }

    pub fn set_last_line(&mut self, line: &str) {
        self.last_line = trim_for_row(line, 160);
    }
}

/// Sort key for the fleet list: attention first, then longest-waiting.
pub fn fleet_order(a: &Session, b: &Session) -> std::cmp::Ordering {
    b.status
        .cmp(&a.status)
        .then_with(|| b.in_status_for().cmp(&a.in_status_for()))
        .then_with(|| a.id.cmp(&b.id))
}

fn folder_label(cwd: &std::path::Path) -> String {
    cwd.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| cwd.to_string_lossy().into_owned())
}

/// Collapse whitespace and clip — agent output is full of carriage returns and
/// spinner frames that would otherwise wreck the row layout.
fn trim_for_row(s: &str, max: usize) -> String {
    let mut out = String::with_capacity(max.min(s.len()));
    let mut space = false;
    for ch in s.chars() {
        let ch = if ch.is_whitespace() || ch.is_control() {
            ' '
        } else {
            ch
        };
        if ch == ' ' {
            if space || out.is_empty() {
                continue;
            }
            space = true;
        } else {
            space = false;
        }
        out.push(ch);
        if out.chars().count() >= max {
            break;
        }
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::spec_for;
    use std::path::Path;

    fn session(status: SessionStatus) -> Session {
        let spec = spec_for(Agent::Claude, Path::new("."));
        let mut s = Session::new(SessionId::new(1), &spec);
        s.status = status;
        s
    }

    #[test]
    fn attention_sorts_to_the_top() {
        let mut v = [
            session(SessionStatus::Idle),
            session(SessionStatus::NeedsYou),
            session(SessionStatus::Working),
        ];
        v.sort_by(fleet_order);
        assert_eq!(v[0].status, SessionStatus::NeedsYou);
        assert_eq!(v[2].status, SessionStatus::Idle);
    }

    #[test]
    fn needs_you_raises_unread_but_repeat_transitions_do_not_restamp() {
        let mut s = session(SessionStatus::Working);
        s.set_status(SessionStatus::NeedsYou);
        assert!(s.unread);
        let stamp = s.status_since;
        s.set_status(SessionStatus::NeedsYou);
        assert_eq!(
            s.status_since, stamp,
            "a repeated status must not reset the timer"
        );
    }

    #[test]
    fn spinner_noise_does_not_wreck_the_row() {
        let mut s = session(SessionStatus::Working);
        s.set_last_line("  Thinking\r\n\t  \u{1b}   about   it  ");
        assert_eq!(s.last_line, "Thinking about it");
    }

    #[test]
    fn row_text_is_clipped() {
        let mut s = session(SessionStatus::Idle);
        s.set_last_line(&"x".repeat(500));
        assert_eq!(s.last_line.chars().count(), 160);
    }

    #[test]
    fn unnamed_sessions_borrow_the_folder_name() {
        let spec = spec_for(Agent::Codex, Path::new(r"C:\Users\me\repos\TerminalAI"));
        assert_eq!(Session::new(SessionId::new(2), &spec).name, "TerminalAI");
    }
}
