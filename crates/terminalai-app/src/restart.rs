//! Telling Windows how to bring this app back.
//!
//! The daemon is designed to outlive its window: it owns every live agent
//! process, and reattaching to it is a path this project already builds and
//! tests. But nothing told Windows that, so a forced restart — a crash, a hang,
//! or the reboot at the end of a Windows update — dropped the operator into an
//! empty desktop with a fleet still running behind it. The sessions survived
//! and the only thing that did not was the way to see them.
//!
//! `RegisterApplicationRestart` closes that. It is registration only: it
//! changes nothing about how the app runs, and it is why this is worth doing at
//! startup rather than being handled at shutdown, which is exactly the moment a
//! crashed process does not get.
//!
//! # The command line matters more than it looks
//!
//! Windows relaunches with the command line registered here, and this binary is
//! also its own hook adapter — `terminalai hook claude` reads a payload from
//! stdin and exits. Registering the wrong arguments would have Windows
//! resurrect the app as a hook adapter with no stdin: a process that starts,
//! reads nothing and exits, leaving the desktop just as empty while looking
//! like the restart worked.

/// Longest command line `RegisterApplicationRestart` accepts, including the
/// terminator. Registration fails outright above it rather than truncating.
pub const RESTART_MAX_CMD_LINE: usize = 1024;

/// Argument the app is relaunched with, so a restarted run can say so.
///
/// Deliberately not a bare marker file or an environment variable: the command
/// line is what Windows actually replays, so it is the only thing that is true
/// by construction rather than by something having been written down first.
pub const RESTARTED_FLAG: &str = "--restarted";

/// The command line to register.
///
/// Never includes the executable path — Windows supplies that — and never
/// includes `hook`, which would relaunch this binary as its own hook adapter
/// with nothing on stdin.
pub fn command_line() -> &'static str {
    RESTARTED_FLAG
}

/// Was this process started by Windows restarting it?
pub fn was_restarted(args: &[String]) -> bool {
    args.iter().any(|arg| arg == RESTARTED_FLAG)
}

#[cfg(windows)]
fn register_with(command: &str) -> Result<(), String> {
    use windows_sys::Win32::System::Recovery::RegisterApplicationRestart;

    if command.len() >= RESTART_MAX_CMD_LINE {
        return Err(format!(
            "restart command line is {} characters, over the {RESTART_MAX_CMD_LINE} limit",
            command.len()
        ));
    }
    let wide: Vec<u16> = command.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` is a NUL-terminated UTF-16 buffer that outlives the call.
    // Flags 0 registers for all four reasons — crash, hang, patch and reboot —
    // which is the whole point: the reboot at the end of a Windows update is
    // the most likely one to happen unattended.
    let status = unsafe { RegisterApplicationRestart(wide.as_ptr(), 0) };
    if status == 0 {
        Ok(())
    } else {
        Err(format!("RegisterApplicationRestart returned 0x{status:08x}"))
    }
}

#[cfg(not(windows))]
fn register_with(_command: &str) -> Result<(), String> {
    Err("application restart is a Windows facility".to_owned())
}

/// Register for restart, and say what happened.
///
/// A failure is logged and never fatal. Not being restartable is a worse
/// experience than being restartable; it is not a reason to refuse to start,
/// and the fleet behind the window is unaffected either way.
pub fn register() {
    match register_with(command_line()) {
        Ok(()) => tracing::info!(
            command_line = command_line(),
            "registered for restart after a crash, hang or Windows update"
        ),
        Err(error) => tracing::warn!(
            %error,
            "could not register for restart; a forced restart will leave the fleet running \
             with no window attached to it"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registered_command_line_never_relaunches_the_hook_adapter() {
        // This binary is its own hook adapter. Registering `hook` would have
        // Windows resurrect it as one, with nothing on stdin: a process that
        // starts, reads nothing and exits, leaving the desktop as empty as
        // before while looking like the restart worked.
        assert!(!command_line().split_whitespace().any(|arg| arg == "hook"));
        // And never the executable path, which Windows supplies itself.
        assert!(!command_line().contains(".exe"));
    }

    #[test]
    fn the_command_line_fits_what_the_api_accepts() {
        // Registration fails outright above the limit rather than truncating,
        // so a command line that grew past it would silently stop registering.
        assert!(command_line().len() < RESTART_MAX_CMD_LINE);
    }

    #[test]
    fn an_oversized_command_line_is_refused_rather_than_registered() {
        let long = "x".repeat(RESTART_MAX_CMD_LINE);
        let error = register_with(&long).expect_err("an oversized command line must be refused");
        assert!(error.contains("over the"), "{error}");
    }

    #[test]
    fn a_restarted_run_can_tell() {
        assert!(was_restarted(&[RESTARTED_FLAG.to_owned()]));
        assert!(was_restarted(&["--other".into(), RESTARTED_FLAG.to_owned()]));
        assert!(!was_restarted(&[]));
        assert!(!was_restarted(&["hook".into(), "claude".into()]));
    }

    #[test]
    #[cfg(windows)]
    fn registering_succeeds_against_the_real_api() {
        // The call itself, not a stand-in. Registration is idempotent — a
        // second call replaces the first — so this is safe to run in a test
        // process and proves the flags and encoding are accepted rather than
        // merely compiling.
        register_with(command_line()).expect("registration should succeed");
    }
}
