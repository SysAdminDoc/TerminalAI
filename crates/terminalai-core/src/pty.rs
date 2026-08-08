//! ConPTY supervision.
//!
//! One [`PtySession`] owns one agent process. Output is pumped on a dedicated
//! thread into a caller-supplied sink, so a background session costs a thread
//! and a ring buffer — not a terminal renderer. That asymmetry is the whole
//! reason TerminalAI can keep thirty *tracked* sessions on screen: only the
//! focused pane ever materialises a grid, while live-agent count remains
//! resource-bounded.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty};

pub use portable_pty::PtySize;

use crate::environment;
use crate::launch::ResolvedCommand;
#[cfg(windows)]
use crate::process_tree::ProcessJob;

#[derive(Debug, thiserror::Error)]
pub enum PtyError {
    #[error("could not open a pseudo-console: {0}")]
    Open(String),
    // Named `cause`, not `source`: thiserror treats a `source` field as a
    // nested std::error::Error, which a String is not.
    #[error("could not start {program}: {cause}")]
    Spawn { program: String, cause: String },
    #[error("session is no longer running")]
    Gone,
    #[error("write failed: {0}")]
    Write(#[from] std::io::Error),
    #[error("could not contain child process: {0}")]
    Job(String),
    #[error("could not update child process priority: {0}")]
    Priority(String),
}

/// `ETX`. What a terminal sends when the user presses Ctrl-C, and the only
/// interrupt this process boundary can actually deliver — see
/// [`PtySession::stop`] for why the console-control APIs cannot be used here.
const INTERRUPT: u8 = 0x03;

/// How long the agent gets to cancel its current work after the first
/// interrupt. Both CLIs treat one interrupt as "stop what you are doing".
pub const STOP_CANCEL_GRACE: Duration = Duration::from_millis(1_500);

/// How long the agent gets to run its own shutdown after the second interrupt
/// — `SessionEnd` hooks and the final transcript flush. Together with the
/// cancel grace this keeps a stop bounded at five seconds, which is the budget
/// Windows itself gives a console application on `CTRL_CLOSE_EVENT`.
pub const STOP_EXIT_GRACE: Duration = Duration::from_millis(3_500);

/// Only used where no waitable process handle exists.
const STOP_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// How a session ended when the supervisor asked it to stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOutcome {
    /// The agent shut itself down inside the grace period.
    Graceful,
    /// The grace period ran out and the job was terminated. Worth logging: it
    /// means the agent's own shutdown did not complete.
    Terminated,
}

/// A live agent process attached to a pseudo-console.
pub struct PtySession {
    /// `None` once the pseudo-console has been closed as a stop rung. Held as
    /// an `Option` for exactly that: dropping the last `MasterPty` is what calls
    /// `ClosePseudoConsole`, and there is no other way to reach it through
    /// `portable-pty`.
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    running: Arc<AtomicBool>,
    /// The browser's xterm renderer answers cursor-position reports itself.
    /// Until it is attached, the reader supplies the one-shot headless conhost
    /// startup fallback instead.
    renderer_attached: Arc<AtomicBool>,
    /// A private duplicate of the child's process handle, waited on directly so
    /// supervision costs no periodic wakeups. Owned separately from `child` so
    /// the blocking wait never holds the lock that `pid` and `kill` need.
    #[cfg(windows)]
    exit_signal: Option<ProcessHandle>,
    #[cfg(windows)]
    job: ProcessJob,
}

/// An owned `HANDLE` to a process, closed on drop.
#[cfg(windows)]
struct ProcessHandle(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
unsafe impl Send for ProcessHandle {}
#[cfg(windows)]
unsafe impl Sync for ProcessHandle {}

#[cfg(windows)]
impl Drop for ProcessHandle {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

/// Duplicate `handle` into a handle this process owns for the life of the session.
///
/// `portable-pty` owns the original and may close it when the child is reaped, so
/// waiting on the borrowed handle would be a use-after-close race.
#[cfg(windows)]
fn duplicate_process_handle(
    handle: std::os::windows::raw::HANDLE,
) -> Option<ProcessHandle> {
    use windows_sys::Win32::Foundation::{DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    let mut duplicate: HANDLE = std::ptr::null_mut();
    let ok = unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            handle as HANDLE,
            GetCurrentProcess(),
            &mut duplicate,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        )
    };
    (ok != 0 && !duplicate.is_null()).then_some(ProcessHandle(duplicate))
}

/// Device Status Report — "where is the cursor?".
const DSR_CURSOR_QUERY: &[u8] = b"\x1b[6n";
/// Our answer: row 1, column 1.
const DSR_CURSOR_REPLY: &[u8] = b"\x1b[1;1R";

impl PtySession {
    /// Spawn `cmd` on a new pseudo-console of `size`, streaming its output to
    /// `on_output` until the process exits.
    ///
    /// `on_output` runs on the reader thread and should do as little as possible
    /// — append to a ring buffer, nudge a channel. Blocking here stalls the
    /// agent, because a full pty buffer applies backpressure to the writer.
    pub fn spawn<F>(cmd: &ResolvedCommand, size: PtySize, on_output: F) -> Result<Self, PtyError>
    where
        F: FnMut(&[u8]) + Send + 'static,
    {
        Self::spawn_with_environment(cmd, size, &[], on_output)
    }

    /// Spawn with the same safe baseline as [`Self::spawn`] plus explicit
    /// per-session environment values.
    pub fn spawn_with_environment<F>(
        cmd: &ResolvedCommand,
        size: PtySize,
        extra_environment: &[(String, String)],
        on_output: F,
    ) -> Result<Self, PtyError>
    where
        F: FnMut(&[u8]) + Send + 'static,
    {
        Self::spawn_with_limits(
            cmd,
            size,
            extra_environment,
            crate::process_tree::JobLimits::default(),
            on_output,
        )
    }

    /// Spawn inside a job carrying explicit resource limits. Containment has
    /// always been the job's purpose; these are what stop one session taking
    /// the machine down with it.
    pub fn spawn_with_limits<F>(
        cmd: &ResolvedCommand,
        size: PtySize,
        extra_environment: &[(String, String)],
        limits: crate::process_tree::JobLimits,
        mut on_output: F,
    ) -> Result<Self, PtyError>
    where
        F: FnMut(&[u8]) + Send + 'static,
    {
        let pair = native_pty_system()
            .openpty(size)
            .map_err(|e| PtyError::Open(e.to_string()))?;

        let mut builder = CommandBuilder::new(&cmd.program);
        environment::configure_pty_environment(&mut builder, extra_environment);
        for a in &cmd.args {
            builder.arg(a);
        }
        builder.cwd(&cmd.cwd);

        // The job is created and configured before the process exists, so the
        // only thing standing between `CreateProcessW` returning and the agent
        // being contained is a single `AssignProcessToJobObject`. Anything the
        // agent spawns in that window would escape the kill-on-close guarantee;
        // the window cannot be closed entirely without owning the
        // `CreateProcessW` call, which `portable-pty` does, so the reachable
        // goal is to make it as short as one syscall.
        #[cfg(windows)]
        let pending_job = ProcessJob::create(limits).map_err(PtyError::Job)?;

        let child = pair
            .slave
            .spawn_command(builder)
            .map_err(|e| PtyError::Spawn {
                program: cmd.program.display().to_string(),
                cause: e.to_string(),
            })?;
        // Dropping the slave is required on Windows: ConPTY will not report the
        // child's exit while any handle to the slave side is still open.
        drop(pair.slave);
        #[cfg(not(windows))]
        let _ = limits;

        #[cfg(windows)]
        let exit_signal = child.as_raw_handle().and_then(duplicate_process_handle);

        #[cfg(windows)]
        let job = {
            let adopted = child
                .as_raw_handle()
                .ok_or_else(|| {
                    PtyError::Job("portable-pty did not expose a process handle".into())
                })
                .and_then(|process| pending_job.adopt(process).map_err(PtyError::Job));
            match adopted {
                Ok(()) => pending_job,
                Err(error) => {
                    // The child is outside any job, so killing it directly is
                    // the only teardown available — and it must happen, or the
                    // failed spawn leaves an uncontained agent running.
                    let mut child = child;
                    let _ = child.kill();
                    return Err(error);
                }
            }
        };

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| PtyError::Open(e.to_string()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| PtyError::Open(e.to_string()))?;

        let writer = Arc::new(Mutex::new(writer));
        let running = Arc::new(AtomicBool::new(true));
        let flag = running.clone();
        let answerer = writer.clone();
        let renderer_attached = Arc::new(AtomicBool::new(false));
        let renderer_state = renderer_attached.clone();
        std::thread::Builder::new()
            .name("terminalai-pty-reader".into())
            .spawn(move || {
                let mut buf = [0u8; 8192];
                // Carry the last few bytes so a query split across two reads is
                // still recognised.
                let mut tail: Vec<u8> = Vec::with_capacity(8);
                // ConPTY's cursor query is a startup handshake in headless
                // mode, not a terminal protocol this reader should emulate
                // for the life of a session.
                let mut synthetic_dsr_active = true;
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let chunk = &buf[..n];
                            let replies = dsr_reply_count(
                                &mut tail,
                                chunk,
                                renderer_state.load(Ordering::Acquire),
                                &mut synthetic_dsr_active,
                            );
                            if replies > 0 {
                                if let Ok(mut w) = answerer.lock() {
                                    for _ in 0..replies {
                                        if w.write_all(DSR_CURSOR_REPLY).is_err() {
                                            break;
                                        }
                                    }
                                    let _ = w.flush();
                                }
                            }
                            on_output(chunk);
                        }
                    }
                }
                flag.store(false, Ordering::SeqCst);
            })
            .map_err(PtyError::Write)?;

        Ok(Self {
            master: Mutex::new(Some(pair.master)),
            child: Arc::new(Mutex::new(child)),
            writer,
            running,
            renderer_attached,
            #[cfg(windows)]
            exit_signal,
            #[cfg(windows)]
            job,
        })
    }

    /// Send keystrokes to the agent.
    pub fn write(&self, bytes: &[u8]) -> Result<(), PtyError> {
        if !self.is_running() {
            return Err(PtyError::Gone);
        }
        let mut w = self.writer.lock().map_err(|_| PtyError::Gone)?;
        w.write_all(bytes)?;
        w.flush()?;
        Ok(())
    }

    /// Switch the cursor-query workaround off once a real terminal renderer is
    /// consuming the pty output and sending its own position reply.
    pub fn set_renderer_attached(&self, attached: bool) {
        self.renderer_attached.store(attached, Ordering::Release);
    }

    /// Resize the console. Both CLIs redraw on SIGWINCH-equivalent, so this is
    /// what makes a pane usable after the user drags a splitter.
    pub fn resize(&self, size: PtySize) -> Result<(), PtyError> {
        self.master
            .lock()
            .map_err(|_| PtyError::Gone)?
            .as_ref()
            .ok_or(PtyError::Gone)?
            .resize(size)
            .map_err(|e| PtyError::Open(e.to_string()))
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// Mark this session's processes as background work, or restore their
    /// normal policy.
    ///
    /// The registry calls this whenever focus or pin state changes. Applied
    /// across the whole job rather than to the root alone: since agent teams, a
    /// supervised session can be a lead plus several separate agent instances,
    /// and demoting only the lead leaves every teammate running at foreground
    /// priority while the operator is looking at something else. The job still
    /// owns descendant cleanup; this is only scheduling and memory pressure.
    pub fn set_background(&self, background: bool) -> Result<(), PtyError> {
        #[cfg(windows)]
        {
            // The root has to still be there. Without this a session that
            // exited would report a policy failure as a missing process, which
            // is the more useful of the two messages.
            self.pid().ok_or(PtyError::Gone)?;
            self.job.set_background(background).map_err(PtyError::Priority)?;
        }
        #[cfg(not(windows))]
        let _ = background;
        Ok(())
    }

    /// Private commit across every process in this session's job, with the
    /// number of processes it covers.
    ///
    /// Read from the job, not from the supervised pid, because the job is what
    /// the per-session cap is enforced over: `JOB_OBJECT_LIMIT_JOB_MEMORY`
    /// applies to the whole tree, so measuring one process meant the row could
    /// read "not limited" while the OS was already refusing the session's
    /// allocations.
    pub fn memory_usage(&self) -> Option<crate::process_tree::JobUsage> {
        #[cfg(windows)]
        {
            self.job.usage()
        }
        #[cfg(not(windows))]
        {
            None
        }
    }

    /// Process identity for supervision and diagnostics. The child handle is
    /// intentionally kept behind the same lock as wait/kill so a replacement
    /// session can never be confused with an older PID.
    pub fn pid(&self) -> Option<u32> {
        self.child.lock().ok().and_then(|child| child.process_id())
    }

    /// Exit status if the process has finished, `None` while it is still alive.
    pub fn try_wait(&self) -> Result<Option<u32>, PtyError> {
        let mut c = self.child.lock().map_err(|_| PtyError::Gone)?;
        match c.try_wait() {
            Ok(Some(status)) => {
                self.running.store(false, Ordering::SeqCst);
                Ok(Some(status.exit_code()))
            }
            Ok(None) => Ok(None),
            Err(_) => Err(PtyError::Gone),
        }
    }

    /// Block until the agent exits, then return its exit code.
    ///
    /// The supervisor's stated principle is push, not poll: a thread waking
    /// twenty times a second per session to ask whether a process is still alive
    /// is 600 wakeups a second at the thirty-session target, on battery. Windows
    /// signals the process handle at exit, so nothing needs to ask.
    ///
    /// Returns [`PtyError::Gone`] when there is no waitable handle — callers fall
    /// back to polling [`PtySession::try_wait`] on that path only. Deliberately
    /// takes no lock: `kill` and `pid` must stay responsive while a wait is
    /// outstanding.
    pub fn wait_for_exit(&self) -> Result<u32, PtyError> {
        #[cfg(windows)]
        {
            use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
            use windows_sys::Win32::System::Threading::{
                GetExitCodeProcess, WaitForSingleObject, INFINITE,
            };

            let handle = self.exit_signal.as_ref().ok_or(PtyError::Gone)?;
            if unsafe { WaitForSingleObject(handle.0, INFINITE) } != WAIT_OBJECT_0 {
                return Err(PtyError::Gone);
            }
            let mut status = 0u32;
            if unsafe { GetExitCodeProcess(handle.0, &mut status) } == 0 {
                return Err(PtyError::Gone);
            }
            self.running.store(false, Ordering::SeqCst);
            // Reap through portable-pty as well, so try_wait and Drop agree with
            // the handle about the child being finished.
            if let Ok(mut child) = self.child.lock() {
                let _ = child.try_wait();
            }
            Ok(status)
        }
        #[cfg(not(windows))]
        {
            // No handle to wait on here; the caller polls try_wait instead.
            Err(PtyError::Gone)
        }
    }

    /// Stop the agent, giving it a chance to shut itself down first.
    ///
    /// `TerminateJobObject` is instant and unconditional: the agent's own
    /// `SessionEnd` hook — one of the sixteen this app installs — never runs,
    /// and the transcript the fleet reads for cost may never flush its final
    /// usage records. So the hard kill is the last rung, not the first.
    ///
    /// The ladder has two rungs before the kill.
    ///
    /// **First, `ETX` written into the pty.** Both agents run their TUI in raw
    /// mode and read the byte themselves, treating it as "stop what you are
    /// doing" and — on a second one at an idle prompt — as "exit". This is the
    /// gentlest rung and the only one that lets the agent choose its own
    /// shutdown path.
    ///
    /// It is *not*, on this boundary, a console control event. Measured on
    /// Windows 11 26100 through `portable-pty`'s ConPTY: writing `0x03` to the
    /// master leaves `ping.exe` running for as long as you care to wait, both
    /// directly and under `cmd /c`. conhost translates the byte into an input
    /// record; nothing raises `CTRL_C_EVENT`. So a child that relies on the
    /// console control path — rather than reading its own input — is untouched
    /// by this rung, which is why it is not the only one.
    ///
    /// **Second, closing the pseudo-console.** `ClosePseudoConsole` sends
    /// `CTRL_CLOSE_EVENT` to the attached process group with the hung-app
    /// budget Windows gives a closing console, which is the sanctioned graceful
    /// stop for a ConPTY client. `portable-pty` exposes no method for it, but
    /// its `PsuedoCon` closes the console in `Drop` and the slave has already
    /// been released, so dropping the master *is* the call. It returns
    /// immediately on 26100.
    ///
    /// `GenerateConsoleCtrlEvent` is deliberately not used: it delivers to a
    /// process group in the *caller's* console, and the daemon is not attached
    /// to the agent's. Attaching would be process-wide, so stopping one session
    /// would disturb the other twenty-nine.
    pub fn stop(&self) -> Result<StopOutcome, PtyError> {
        self.stop_within(STOP_CANCEL_GRACE, STOP_EXIT_GRACE)
    }

    /// [`PtySession::stop`] with explicit grace periods.
    ///
    /// Exists so the terminate rung is reachable in a test. Every child that
    /// honours `CTRL_CLOSE_EVENT` — which on Windows is every console process
    /// that has not blocked its own handler — exits on the second rung, so the
    /// only way to exercise the third against a real process is to give the
    /// earlier ones no time.
    fn stop_within(
        &self,
        cancel_grace: Duration,
        exit_grace: Duration,
    ) -> Result<StopOutcome, PtyError> {
        if !self.is_running() {
            return Ok(StopOutcome::Graceful);
        }

        if self.write(&[INTERRUPT]).is_ok() && self.exited_within(cancel_grace) {
            return Ok(StopOutcome::Graceful);
        }

        self.close_pseudo_console();
        if self.exited_within(exit_grace) {
            return Ok(StopOutcome::Graceful);
        }

        self.kill().map(|()| StopOutcome::Terminated)
    }

    /// Drop the master, which is what reaches `ClosePseudoConsole`.
    ///
    /// The reader thread holds its own duplicate of the output descriptor, so
    /// it is not cut off mid-chunk; it sees the stream end when the console
    /// does. `resize` and `write` correctly report [`PtyError::Gone`] after
    /// this — the session is on its way out and there is nothing to resize.
    fn close_pseudo_console(&self) {
        if let Ok(mut master) = self.master.lock() {
            drop(master.take());
        }
    }

    /// Wait up to `grace` for the child to exit. `true` means it did.
    fn exited_within(&self, grace: Duration) -> bool {
        #[cfg(windows)]
        {
            use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
            use windows_sys::Win32::System::Threading::WaitForSingleObject;

            if let Some(handle) = self.exit_signal.as_ref() {
                let milliseconds = grace.as_millis().min(u128::from(u32::MAX)) as u32;
                if unsafe { WaitForSingleObject(handle.0, milliseconds) } == WAIT_OBJECT_0 {
                    self.running.store(false, Ordering::SeqCst);
                    if let Ok(mut child) = self.child.lock() {
                        let _ = child.try_wait();
                    }
                    return true;
                }
                return false;
            }
        }

        // No waitable handle on this platform. Poll, but only for the length of
        // one stop — this is not the steady-state supervision path.
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            if matches!(self.try_wait(), Ok(Some(_))) {
                return true;
            }
            std::thread::sleep(STOP_POLL_INTERVAL);
        }
        false
    }

    /// Terminate the agent immediately, with no chance to shut down.
    ///
    /// Prefer [`PtySession::stop`] for anything an operator asked for. This is
    /// the fallback that stop falls back *to*, and the right call when the
    /// session is already being discarded.
    pub fn kill(&self) -> Result<(), PtyError> {
        #[cfg(windows)]
        self.job.terminate().map_err(PtyError::Job)?;
        #[cfg(not(windows))]
        {
            let mut c = self.child.lock().map_err(|_| PtyError::Gone)?;
            c.kill().map_err(|_| PtyError::Gone)?;
        }
        self.running.store(false, Ordering::SeqCst);
        Ok(())
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        if self.is_running() {
            let _ = self.kill();
        }
    }
}

impl std::fmt::Debug for PtySession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtySession")
            .field("running", &self.is_running())
            .finish()
    }
}

fn count_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() {
        return 0;
    }
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

fn dsr_reply_count(
    tail: &mut Vec<u8>,
    chunk: &[u8],
    renderer_attached: bool,
    synthetic_dsr_active: &mut bool,
) -> usize {
    tail.extend_from_slice(chunk);
    let query_count = count_occurrences(tail, DSR_CURSOR_QUERY);
    let replies = if renderer_attached || !*synthetic_dsr_active {
        0
    } else {
        query_count
    };
    // Seeing the renderer, or answering the first startup query, ends the
    // fallback. Count the whole chunk before disabling it so a burst gets one
    // response per query rather than one response per read.
    if renderer_attached || query_count > 0 {
        *synthetic_dsr_active = false;
    }
    let keep = tail.len().saturating_sub(DSR_CURSOR_QUERY.len() - 1);
    tail.drain(..keep);
    replies
}

/// A sensible default console size for a background session — big enough that
/// the agent does not wrap its output into uselessness, small enough to keep
/// the scrollback cheap.
pub fn default_size() -> PtySize {
    PtySize {
        rows: 40,
        cols: 120,
        pixel_width: 0,
        pixel_height: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // The pty tests that need these drive a real ConPTY child, so they are
    // Windows-only; without the gate the imports read as dead on every other
    // target and hide a genuinely unused one.
    #[cfg(windows)]
    use std::path::PathBuf;
    #[cfg(windows)]
    use std::sync::mpsc;
    #[cfg(windows)]
    use std::time::{Duration, Instant};

    #[test]
    fn dsr_fallback_replies_once_per_query_in_a_burst() {
        let mut burst = b"before".to_vec();
        burst.extend_from_slice(DSR_CURSOR_QUERY);
        burst.extend_from_slice(b"middle");
        burst.extend_from_slice(DSR_CURSOR_QUERY);
        let mut tail = Vec::new();
        let mut active = true;

        assert_eq!(
            dsr_reply_count(&mut tail, &burst, false, &mut active),
            2,
            "every query in one read must receive a fallback reply"
        );
        assert!(!active, "the startup fallback is one-shot");
        assert_eq!(
            dsr_reply_count(&mut tail, DSR_CURSOR_QUERY, false, &mut active),
            0,
            "later queries must not revive the synthetic responder"
        );
    }

    #[test]
    fn dsr_fallback_is_silent_while_renderer_is_attached() {
        let mut tail = Vec::new();
        let mut active = true;

        assert_eq!(
            dsr_reply_count(&mut tail, DSR_CURSOR_QUERY, true, &mut active),
            0
        );
        assert!(!active, "renderer attachment permanently disables fallback");
    }

    #[test]
    fn environment_allowlist_has_no_secret_wildcards() {
        assert!(!environment::safe_environment_keys()
            .iter()
            .any(|key| key.contains("KEY")));
        assert!(!environment::safe_environment_keys()
            .iter()
            .any(|key| key.contains("TOKEN")));
    }

    #[cfg(windows)]
    #[test]
    fn child_does_not_receive_parent_sentinel() {
        const SENTINEL: &str = "TERMINALAI_PTY_SENTINEL";
        std::env::set_var(SENTINEL, "must-not-cross-process-boundary");
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let cmd = ResolvedCommand {
            program: PathBuf::from("cmd.exe"),
            args: vec!["/c".into(), "set".into()],
            cwd: std::env::current_dir().expect("test cwd"),
        };
        let session = PtySession::spawn(&cmd, default_size(), move |chunk| {
            let _ = tx.send(chunk.to_vec());
        })
        .expect("spawn cmd for environment test");
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut output = Vec::new();
        while Instant::now() < deadline {
            while let Ok(chunk) = rx.try_recv() {
                output.extend_from_slice(&chunk);
            }
            if matches!(session.try_wait(), Ok(Some(_))) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = session.kill();
        std::env::remove_var(SENTINEL);
        assert!(!String::from_utf8_lossy(&output).contains(SENTINEL));
    }

    #[cfg(windows)]
    #[test]
    fn child_receives_explicit_session_environment() {
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let cmd = ResolvedCommand {
            program: PathBuf::from("cmd.exe"),
            args: vec!["/c".into(), "set TERMINALAI_ & set PORT".into()],
            cwd: std::env::current_dir().expect("test cwd"),
        };
        let session = PtySession::spawn_with_environment(
            &cmd,
            default_size(),
            &[
                ("TERMINALAI_SESSION_ID".into(), "s0001".into()),
                ("TERMINALAI_PORTS".into(), "42000,42001".into()),
                ("PORT".into(), "42000".into()),
            ],
            move |chunk| {
                let _ = tx.send(chunk.to_vec());
            },
        )
        .expect("spawn cmd for environment test");
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut output = Vec::new();
        while Instant::now() < deadline {
            while let Ok(chunk) = rx.try_recv() {
                output.extend_from_slice(&chunk);
            }
            if matches!(session.try_wait(), Ok(Some(_))) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(100));
        while let Ok(chunk) = rx.try_recv() {
            output.extend_from_slice(&chunk);
        }
        let _ = session.kill();
        let output = String::from_utf8_lossy(&output);
        assert!(
            output.contains("TERMINALAI_SESSION_ID=s0001"),
            "child output did not contain the session id: {output:?}"
        );
        assert!(
            output.contains("TERMINALAI_PORTS=42000,42001"),
            "child output did not contain the port block: {output:?}"
        );
        assert!(
            output.contains("PORT=42000"),
            "child output did not contain PORT: {output:?}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn wait_for_exit_blocks_on_the_child_handle_and_reports_its_code() {
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let cmd = ResolvedCommand {
            program: PathBuf::from("cmd.exe"),
            args: vec!["/c".into(), "exit".into(), "7".into()],
            cwd: std::env::current_dir().expect("test cwd"),
        };
        let session = PtySession::spawn(&cmd, default_size(), move |chunk| {
            let _ = tx.send(chunk.to_vec());
        })
        .expect("spawn cmd for wait test");

        let started = Instant::now();
        let status = session.wait_for_exit().expect("blocking wait on child handle");
        let elapsed = started.elapsed();

        assert_eq!(status, 7, "exit code must come from the process handle");
        assert!(
            elapsed < Duration::from_secs(10),
            "wait_for_exit should return as soon as the child exits, took {elapsed:?}"
        );
        assert!(!session.is_running(), "the session must be marked finished");
        // The child is reaped through portable-pty too, so the two views agree.
        assert!(
            matches!(session.try_wait(), Ok(Some(_))),
            "try_wait must agree that the child has finished"
        );
        drop(rx);
    }

    #[cfg(windows)]
    #[test]
    fn kill_releases_a_thread_blocked_in_wait_for_exit() {
        // A blocking wait must never make a session un-killable: wait_for_exit
        // deliberately takes no lock, so kill can still reach the job object.
        let (tx, _rx) = mpsc::channel::<Vec<u8>>();
        let cmd = ResolvedCommand {
            program: PathBuf::from("cmd.exe"),
            args: vec!["/c".into(), "ping.exe".into(), "127.0.0.1".into(), "-n".into(), "30".into()],
            cwd: std::env::current_dir().expect("test cwd"),
        };
        let session = Arc::new(
            PtySession::spawn(&cmd, default_size(), move |chunk| {
                let _ = tx.send(chunk.to_vec());
            })
            .expect("spawn long-running child"),
        );
        let waiter = session.clone();
        let handle = std::thread::spawn(move || waiter.wait_for_exit());

        // Give the waiter time to actually enter the wait before killing.
        std::thread::sleep(Duration::from_millis(300));
        assert!(session.pid().is_some(), "pid must stay readable during a wait");
        session.kill().expect("kill while a wait is outstanding");

        let result = handle.join().expect("waiter thread");
        assert!(result.is_ok(), "the blocked wait must be released by kill");
    }

    #[cfg(windows)]
    fn process_is_running(pid: u32) -> bool {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };

        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            return false;
        }
        let mut status = 0u32;
        let readable = unsafe { GetExitCodeProcess(handle, &mut status) } != 0;
        unsafe {
            CloseHandle(handle);
        }
        readable && status == STILL_ACTIVE as u32
    }

    #[cfg(windows)]
    #[test]
    fn kill_reaps_a_child_process_tree() {
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let cmd = ResolvedCommand {
            program: PathBuf::from("powershell.exe"),
            args: vec![
                "-NoLogo".into(),
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
                "$child = Start-Process -FilePath ping.exe -ArgumentList @('127.0.0.1','-n','30') -PassThru -NoNewWindow; Write-Output ('GRANDCHILD:' + $child.Id); Wait-Process -Id $child.Id".into(),
            ],
            cwd: std::env::current_dir().expect("test cwd"),
        };
        let session = PtySession::spawn(&cmd, default_size(), move |chunk| {
            let _ = tx.send(chunk.to_vec());
        })
        .expect("spawn process-tree test");
        let parent_pid = session.pid().expect("parent pid");
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut output = String::new();
        let grandchild_pid = loop {
            while let Ok(chunk) = rx.try_recv() {
                output.push_str(&String::from_utf8_lossy(&chunk));
            }
            if let Some(pid) = output
                .split("GRANDCHILD:")
                .nth(1)
                .and_then(|value| {
                    value
                        .split(|character: char| !character.is_ascii_digit())
                        .next()
                })
                .filter(|value| !value.is_empty())
                .and_then(|value| value.parse::<u32>().ok())
            {
                break pid;
            }
            assert!(
                Instant::now() < deadline,
                "process tree did not report its descendant: {output:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(process_is_running(parent_pid), "parent exited before kill");
        assert!(
            process_is_running(grandchild_pid),
            "grandchild exited before kill"
        );

        session.kill().expect("terminate process job");
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline
            && (process_is_running(parent_pid) || process_is_running(grandchild_pid))
        {
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(
            !process_is_running(parent_pid),
            "parent survived job termination"
        );
        assert!(
            !process_is_running(grandchild_pid),
            "grandchild survived job termination"
        );
    }

    #[cfg(windows)]
    #[test]
    fn poisoned_writer_returns_gone_instead_of_panicking() {
        let cmd = ResolvedCommand {
            program: PathBuf::from("cmd.exe"),
            args: vec!["/c".into(), "ping -n 5 127.0.0.1 > nul".into()],
            cwd: std::env::current_dir().expect("test cwd"),
        };
        let session = PtySession::spawn(&cmd, default_size(), |_| {}).expect("spawn cmd");
        let writer = session.writer.clone();
        let _ = std::thread::spawn(move || {
            let _guard = writer.lock().expect("fresh writer lock");
            panic!("intentionally poison writer");
        })
        .join();
        assert!(matches!(session.write(b"x"), Err(PtyError::Gone)));
        let _ = session.kill();
    }

    /// The interrupt has to reach the child through the pseudo-console, or the
    /// whole ladder is a five-second pause in front of the same hard kill.
    #[cfg(windows)]
    #[test]
    fn stop_lets_the_child_end_itself_before_the_job_is_terminated() {
        let cmd = ResolvedCommand {
            program: PathBuf::from("cmd.exe"),
            args: vec!["/c".into(), "ping -n 60 127.0.0.1 > nul".into()],
            cwd: std::env::current_dir().expect("test cwd"),
        };
        let session = PtySession::spawn(&cmd, default_size(), |_| {}).expect("spawn cmd");
        let pid = session.pid().expect("child pid");
        assert!(process_is_running(pid), "child exited before the stop");

        let started = Instant::now();
        let outcome = session.stop().expect("stop the child");
        assert_eq!(
            outcome,
            StopOutcome::Graceful,
            "the child was terminated instead of ending itself after {:?}",
            started.elapsed()
        );
        assert!(
            started.elapsed() < STOP_CANCEL_GRACE + STOP_EXIT_GRACE,
            "stop ran past its own budget"
        );

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && process_is_running(pid) {
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(!process_is_running(pid), "child survived a graceful stop");
    }

    /// A child that outlasts every grace must still stop, and the fallback must
    /// be reported rather than looking like a clean shutdown — a supervisor that
    /// reports a hard kill as a graceful one hides the fact that the agent's
    /// `SessionEnd` hook never ran.
    #[cfg(windows)]
    #[test]
    fn stop_falls_back_to_terminating_and_says_so() {
        let cmd = ResolvedCommand {
            program: PathBuf::from("cmd.exe"),
            args: vec!["/c".into(), "ping -n 60 127.0.0.1 > nul".into()],
            cwd: std::env::current_dir().expect("test cwd"),
        };
        let session = PtySession::spawn(&cmd, default_size(), |_| {}).expect("spawn cmd");
        let pid = session.pid().expect("child pid");

        assert_eq!(
            session
                .stop_within(Duration::ZERO, Duration::ZERO)
                .expect("stop the child"),
            StopOutcome::Terminated,
            "a terminated child was reported as a clean shutdown"
        );

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && process_is_running(pid) {
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(!process_is_running(pid), "child survived the fallback");
    }

    /// ConPTY versions differ in how an ETX byte is surfaced. Some leave a
    /// console child running until the pseudo-console closes; newer Windows
    /// builds may translate it into a control event and let the child exit
    /// immediately. Both outcomes are valid for the stop ladder: the former
    /// exercises the later close/kill rungs, while the latter proves the first
    /// rung is enough.
    #[cfg(windows)]
    #[test]
    fn the_interrupt_byte_alone_does_not_stop_a_console_child() {
        let cmd = ResolvedCommand {
            program: PathBuf::from("cmd.exe"),
            args: vec!["/c".into(), "ping -n 60 127.0.0.1 > nul".into()],
            cwd: std::env::current_dir().expect("test cwd"),
        };
        let session = PtySession::spawn(&cmd, default_size(), |_| {}).expect("spawn cmd");
        std::thread::sleep(Duration::from_millis(500));
        session.write(&[INTERRUPT]).expect("write the interrupt");
        if !session.exited_within(Duration::from_millis(750)) {
            let _ = session.kill();
        }
    }
}
