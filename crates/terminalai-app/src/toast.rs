//! Windows toasts for sessions that want the operator.
//!
//! The fleet's whole premise is that you stop watching it, which only works if
//! it can reach you when a session blocks. Two constraints shape everything
//! here:
//!
//! - **An unpackaged Win32 app cannot raise a toast without a Start Menu
//!   shortcut carrying `System.AppUserModelID`.** That shortcut is created by
//!   preflight; without it Windows silently drops the notification, so a missing
//!   shortcut is reported rather than producing a toast that never appears.
//! - **Click-to-activate normally needs a COM activator** registered under
//!   `HKCU\Software\Classes\CLSID`. This app never needs one, because it is
//!   already running when it raises the toast: the in-process `on_activated`
//!   handler fires while the notification's owner is alive, which is exactly
//!   our case. Activating a *closed* app would need the activator, and that is
//!   deliberately not supported — TerminalAI supervises live sessions.
//!
//! Toasts are short-lived on purpose. A toast that outlives the state it
//! describes tells the operator to go look at a session that has since moved
//! on, and Windows offers no reliable way to withdraw one from the action
//! centre from an unpackaged process, so the fix is to not leave one there.

use std::sync::mpsc::Sender;

use tauri_winrt_notification::{Duration as ToastDuration, Sound, Toast};
use terminalai_core::{Session, SessionStatus};

/// What a click on a toast asks the app to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToastActivation {
    /// Focus this session and raise the window.
    Focus(String),
}

/// Whether a status is one the operator needs to act on.
///
/// Deliberately narrower than "not idle": a working session is the normal case
/// and toasting it would train the operator to dismiss without reading.
pub fn wants_attention(status: SessionStatus) -> bool {
    matches!(
        status,
        SessionStatus::NeedsYou | SessionStatus::NeedsApproval | SessionStatus::AwaitingInput
    )
}

/// One line describing why the session wants attention.
pub fn attention_summary(session: &Session) -> String {
    match session.status {
        SessionStatus::NeedsApproval => "Waiting for a permission decision".to_owned(),
        SessionStatus::AwaitingInput => "Waiting for your answer".to_owned(),
        _ => "Needs you".to_owned(),
    }
}

/// The toast's body: what the session is and where it lives.
pub fn attention_body(session: &Session) -> String {
    let folder = session
        .cwd
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| session.cwd.to_string_lossy().into_owned());
    let agent = match session.agent {
        terminalai_core::Agent::Claude => "Claude Code",
        terminalai_core::Agent::Codex => "Codex",
    };
    format!("{agent} · {folder}")
}

/// Raise a toast for a session that wants attention.
///
/// `activations` receives the session id when the operator clicks the toast.
/// Failure is returned rather than logged here so the caller can decide whether
/// a missing Start Menu shortcut is worth surfacing.
pub fn raise_attention_toast(
    app_user_model_id: &str,
    session: &Session,
    activations: Sender<ToastActivation>,
) -> Result<(), String> {
    let id = session.id.0.clone();
    Toast::new(app_user_model_id)
        .title(&session.name)
        .text1(&attention_summary(session))
        .text2(&attention_body(session))
        // Short, not Long: see the module note on withdrawal.
        .duration(ToastDuration::Short)
        .sound(Some(Sound::Reminder))
        .on_activated(move |_action| {
            // The channel is the only thing this handler touches. It runs on a
            // WinRT thread, and doing Tauri work there would cross an apartment
            // boundary the runtime does not expect.
            let _ = activations.send(ToastActivation::Focus(id.clone()));
            Ok(())
        })
        .show()
        .map_err(|error| format!("could not raise a toast: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use terminalai_core::launch::LaunchSpec;
    use terminalai_core::{Agent, SessionId};

    fn session(status: SessionStatus, name: &str, cwd: &str) -> Session {
        let spec = LaunchSpec {
            agent: Agent::Claude,
            name: Some(name.to_owned()),
            cwd: PathBuf::from(cwd),
            ..LaunchSpec::default()
        };
        let mut session = Session::new(SessionId::new(1), &spec);
        session.status = status;
        session
    }

    #[test]
    fn only_states_that_block_on_the_operator_raise_a_toast() {
        // Toasting a working session would train the operator to dismiss
        // without reading, which costs the ones that matter.
        for wanted in [
            SessionStatus::NeedsYou,
            SessionStatus::NeedsApproval,
            SessionStatus::AwaitingInput,
        ] {
            assert!(wants_attention(wanted), "{wanted:?}");
        }
        for ignored in [
            SessionStatus::Working,
            SessionStatus::Thinking,
            SessionStatus::Idle,
            SessionStatus::Starting,
            SessionStatus::Queued,
            SessionStatus::Exited,
            SessionStatus::Unknown,
            // A rate-limited session is blocked, but the operator can do
            // nothing about it — a toast would be an interruption with no
            // available action.
            SessionStatus::RateLimited,
        ] {
            assert!(!wants_attention(ignored), "{ignored:?}");
        }
    }

    #[test]
    fn the_toast_says_which_session_and_why() {
        // A toast reading only "Needs you" is useless with a fleet of thirty.
        let session = session(SessionStatus::NeedsApproval, "shop-api", r"C:\repos\shop");
        assert_eq!(attention_summary(&session), "Waiting for a permission decision");
        let body = attention_body(&session);
        assert!(body.contains("Claude Code"), "{body}");
        assert!(body.contains("shop"), "{body}");
    }

    #[test]
    fn each_attention_state_reads_differently() {
        let approval = session(SessionStatus::NeedsApproval, "a", r"C:\r");
        let input = session(SessionStatus::AwaitingInput, "a", r"C:\r");
        let needs = session(SessionStatus::NeedsYou, "a", r"C:\r");
        let summaries = [
            attention_summary(&approval),
            attention_summary(&input),
            attention_summary(&needs),
        ];
        let unique: std::collections::BTreeSet<_> = summaries.iter().collect();
        assert_eq!(unique.len(), 3, "{summaries:?}");
    }

    #[test]
    fn a_folder_without_a_final_component_still_produces_a_body() {
        // A drive root has no file_name; falling through to an empty string
        // would render the toast as a bare separator.
        let session = session(SessionStatus::NeedsYou, "root", r"C:\");
        assert!(!attention_body(&session).ends_with("· "));
    }
}
