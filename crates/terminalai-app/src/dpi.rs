//! What this process tells Windows about its DPI awareness.
//!
//! This machine runs at 125% scaling, and awareness is a *process* property
//! decided by whoever declares it first. Nothing here declared it, so the value
//! was whatever Tauri and wry happened to set — inherited rather than chosen,
//! and therefore not something the code could rely on or a reader could check.
//!
//! The failure it prevents is silent by construction. An under-aware process is
//! not told the truth about monitors: `GetSystemMetrics` and window rectangles
//! come back virtualized, so every number is plausible, self-consistent and
//! wrong by the scaling factor. The repository's own screenshot and
//! visual-isolation tooling already has to declare awareness independently for
//! exactly this reason.
//!
//! So this declares per-monitor-v2 explicitly, before any window exists, and
//! then **reads back what the process actually has**. Declaring without reading
//! back would be the same class of mistake in a different place: a call whose
//! failure is an error code nobody looks at.

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::UI::HiDpi::{
        AreDpiAwarenessContextsEqual, GetAwarenessFromDpiAwarenessContext,
        GetThreadDpiAwarenessContext, SetProcessDpiAwarenessContext,
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, DPI_AWARENESS_PER_MONITOR_AWARE,
        DPI_AWARENESS_SYSTEM_AWARE, DPI_AWARENESS_UNAWARE,
    };

    /// What the process ended up with.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Awareness {
        Unaware,
        SystemAware,
        /// Per-monitor v1: aware of changes, but not of the non-client area or
        /// of dialogs, which v2 added.
        PerMonitor,
        PerMonitorV2,
        /// The process reported something this build does not model. Named
        /// rather than folded into `Unaware`, because "we could not read it" and
        /// "it is the worst value" are different facts.
        Unknown,
    }

    impl std::fmt::Display for Awareness {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str(match self {
                Awareness::Unaware => "unaware",
                Awareness::SystemAware => "system-aware",
                Awareness::PerMonitor => "per-monitor-v1",
                Awareness::PerMonitorV2 => "per-monitor-v2",
                Awareness::Unknown => "unknown",
            })
        }
    }

    /// What the process reports right now.
    pub fn current() -> Awareness {
        // SAFETY: both calls take a context handle and return a value; neither
        // writes through a pointer and neither can fail in a way that leaves
        // state behind.
        unsafe {
            let context = GetThreadDpiAwarenessContext();
            if AreDpiAwarenessContextsEqual(context, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) != 0
            {
                // Checked first and separately: `GetAwarenessFromDpiAwarenessContext`
                // reports v1 and v2 as the same value, so the distinction this
                // whole module exists for is invisible to it.
                return Awareness::PerMonitorV2;
            }
            match GetAwarenessFromDpiAwarenessContext(context) {
                DPI_AWARENESS_UNAWARE => Awareness::Unaware,
                DPI_AWARENESS_SYSTEM_AWARE => Awareness::SystemAware,
                DPI_AWARENESS_PER_MONITOR_AWARE => Awareness::PerMonitor,
                _ => Awareness::Unknown,
            }
        }
    }

    /// Declare per-monitor-v2, then report what the process actually has.
    ///
    /// The return value is the *effective* awareness, not whether the call
    /// succeeded. Failure is expected and usually benign: awareness can only be
    /// set once per process, so a second call — or a manifest that got there
    /// first — fails with `ERROR_ACCESS_DENIED` while leaving the process
    /// correctly configured. What matters is the value, which is why it is read
    /// back rather than assumed from the return code.
    pub fn declare() -> Awareness {
        // SAFETY: takes a context constant and sets a process-wide property.
        // Called before any window exists, which is the documented requirement.
        unsafe {
            SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }
        current()
    }
}

#[cfg(not(windows))]
mod imp {
    /// DPI awareness is a Windows concept. Modelled on other platforms only so
    /// the cross-target check compiles this file rather than skipping it.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Awareness {
        NotApplicable,
    }

    impl std::fmt::Display for Awareness {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("not applicable")
        }
    }

    pub fn current() -> Awareness {
        Awareness::NotApplicable
    }

    pub fn declare() -> Awareness {
        Awareness::NotApplicable
    }
}

// `current` is used by the tests and by the non-Windows stub; re-exported
// separately so a build without tests does not warn about it.
#[cfg(test)]
pub use imp::current;
pub use imp::{declare, Awareness};

/// Declare awareness and record what the process ended up with.
///
/// Logged at `warn` when it is not what was asked for, because an inherited
/// value is exactly the case this exists to make visible: everything downstream
/// keeps working and every measurement is wrong by the scaling factor.
pub fn declare_and_report() {
    let effective = declare();
    #[cfg(windows)]
    if effective != Awareness::PerMonitorV2 {
        tracing::warn!(
            dpi_awareness = %effective,
            "per-monitor-v2 DPI awareness was requested but the process has something else; \
             monitor and window measurements will be virtualized"
        );
        return;
    }
    tracing::info!(dpi_awareness = %effective, "DPI awareness declared");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(windows)]
    fn declaring_leaves_the_process_per_monitor_v2() {
        // A real call against the real API, in this process. The declaration is
        // idempotent from the caller's point of view: the second call fails with
        // ERROR_ACCESS_DENIED and the process is still correctly configured,
        // which is exactly why `declare` reports the value it read back instead
        // of the value it asked for.
        assert_eq!(declare(), Awareness::PerMonitorV2);
        assert_eq!(declare(), Awareness::PerMonitorV2, "a second call still reports the truth");
        assert_eq!(current(), Awareness::PerMonitorV2);
    }

    #[test]
    fn every_awareness_has_a_name_a_log_line_can_carry() {
        // A diagnostic that renders as a debug enum is a diagnostic nobody
        // greps for.
        assert!(!current().to_string().is_empty());
        #[cfg(windows)]
        for awareness in [
            Awareness::Unaware,
            Awareness::SystemAware,
            Awareness::PerMonitor,
            Awareness::PerMonitorV2,
            Awareness::Unknown,
        ] {
            let rendered = awareness.to_string();
            assert!(!rendered.is_empty());
            assert!(!rendered.contains("Awareness"), "{rendered} reads as a type name");
        }
    }
}
