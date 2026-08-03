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

/// A live agent process attached to a pseudo-console.
pub struct PtySession {
    master: Mutex<Box<dyn MasterPty + Send>>,
    child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    running: Arc<AtomicBool>,
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

        #[cfg(windows)]
        let exit_signal = child.as_raw_handle().and_then(duplicate_process_handle);

        #[cfg(windows)]
        let job = {
            let process = child.as_raw_handle().ok_or_else(|| {
                PtyError::Job("portable-pty did not expose a process handle".into())
            });
            match process.and_then(|process| ProcessJob::assign(process).map_err(PtyError::Job)) {
                Ok(job) => job,
                Err(error) => {
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
        std::thread::Builder::new()
            .name("terminalai-pty-reader".into())
            .spawn(move || {
                let mut buf = [0u8; 8192];
                // Carry the last few bytes so a query split across two reads is
                // still recognised.
                let mut tail: Vec<u8> = Vec::with_capacity(8);
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let chunk = &buf[..n];
                            tail.extend_from_slice(chunk);
                            if contains(&tail, DSR_CURSOR_QUERY) {
                                if let Ok(mut w) = answerer.lock() {
                                    let _ = w.write_all(DSR_CURSOR_REPLY);
                                    let _ = w.flush();
                                }
                            }
                            let keep = tail.len().saturating_sub(DSR_CURSOR_QUERY.len() - 1);
                            tail.drain(..keep);
                            on_output(chunk);
                        }
                    }
                }
                flag.store(false, Ordering::SeqCst);
            })
            .map_err(PtyError::Write)?;

        Ok(Self {
            master: Mutex::new(pair.master),
            child: Arc::new(Mutex::new(child)),
            writer,
            running,
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

    /// Resize the console. Both CLIs redraw on SIGWINCH-equivalent, so this is
    /// what makes a pane usable after the user drags a splitter.
    pub fn resize(&self, size: PtySize) -> Result<(), PtyError> {
        self.master
            .lock()
            .map_err(|_| PtyError::Gone)?
            .resize(size)
            .map_err(|e| PtyError::Open(e.to_string()))
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// Mark the root process as background work or restore its normal policy.
    ///
    /// The registry calls this whenever focus or pin state changes. The job
    /// object still owns descendant cleanup; this policy controls scheduling
    /// and memory pressure for the supervised process boundary.
    pub fn set_background(&self, background: bool) -> Result<(), PtyError> {
        #[cfg(windows)]
        {
            let pid = self.pid().ok_or(PtyError::Gone)?;
            crate::process_tree::set_background_priority(pid, background)
                .map_err(PtyError::Priority)?;
        }
        #[cfg(not(windows))]
        let _ = background;
        Ok(())
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

    /// Terminate the agent. Used when the user closes a session.
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

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
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
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

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
}
