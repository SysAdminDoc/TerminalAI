fn main() {
    harden_dll_search_path();
    install_agent_manifests();
    // Keep this guard in the process entry point. The nonblocking tracing
    // writer must outlive every worker, including the panic hook's final event.
    let logging = terminalai_daemon::init_logging();
    let log_hub = logging.as_ref().map(|logging| logging.hub());
    if let Err(error) = terminalai_daemon::run_with_log_hub(log_hub) {
        eprintln!("terminalai-daemon: {error}");
        std::process::exit(1);
    }
}

/// Layer the operator's agent manifests over the built-ins, before anything can
/// resolve a binary or build an argument vector.
///
/// A manifest decides what argv a session runs, so it is trusted local
/// configuration only — read from the operator's own data directory, never from
/// a repository. A file that exists and is wrong refuses the launch mapping
/// rather than being ignored: falling back to the built-ins would silently
/// launch every session with a mapping the operator did not choose. Logging is
/// not up yet, so this reports on stderr, which the daemon's supervisor keeps.
fn install_agent_manifests() {
    let Some(path) = terminalai_core::manifest::override_path() else {
        return;
    };
    match terminalai_core::manifest::load_overrides(&path) {
        Ok(None) => {}
        Ok(Some(manifests)) => {
            terminalai_core::manifest::install_overrides(manifests);
        }
        Err(error) => {
            eprintln!("terminalai-daemon: {} is unusable: {error}", path.display());
            std::process::exit(1);
        }
    }
}

/// Restrict library loading to System32 before any pseudo-console is created.
///
/// `portable-pty` 0.9 asks Windows for `conpty.dll` by bare name before falling
/// back to the system copy. No such file exists in System32 on Windows 11 26100,
/// so that search walks the default order — which includes the application
/// directory and the current working directory, both of which a non-administrator
/// can write. A DLL planted there would load into the one process that owns every
/// supervised agent.
///
/// `LOAD_LIBRARY_SEARCH_SYSTEM32` removes the current directory and `PATH` from
/// the search entirely. It is process-wide and takes effect for every subsequent
/// load, so it has to run before the first `PtySession`.
fn harden_dll_search_path() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::LibraryLoader::{
            SetDefaultDllDirectories, LOAD_LIBRARY_SEARCH_SYSTEM32,
        };

        // Not fatal — the daemon still works — but running with the default,
        // wider search order is worth saying out loud rather than swallowing.
        if unsafe { SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32) } == 0 {
            eprintln!(
                "terminalai-daemon: could not restrict the DLL search path to System32; \
                 library loading is using the default search order"
            );
        }
    }
}
