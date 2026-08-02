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

use crate::launch::ResolvedCommand;

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
}

/// A live agent process attached to a pseudo-console.
pub struct PtySession {
    master: Mutex<Box<dyn MasterPty + Send>>,
    child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    running: Arc<AtomicBool>,
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
    pub fn spawn<F>(
        cmd: &ResolvedCommand,
        size: PtySize,
        mut on_output: F,
    ) -> Result<Self, PtyError>
    where
        F: FnMut(&[u8]) + Send + 'static,
    {
        let pair = native_pty_system()
            .openpty(size)
            .map_err(|e| PtyError::Open(e.to_string()))?;

        let mut builder = CommandBuilder::new(&cmd.program);
        configure_environment(&mut builder);
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

    /// Exit status if the process has finished, `None` while it is still alive.
    pub fn try_wait(&self) -> Result<Option<u32>, PtyError> {
        let mut c = self.child.lock().map_err(|_| PtyError::Gone)?;
        match c.try_wait() {
            Ok(Some(status)) => Ok(Some(status.exit_code())),
            Ok(None) => Ok(None),
            Err(_) => Err(PtyError::Gone),
        }
    }

    /// Terminate the agent. Used when the user closes a session.
    pub fn kill(&self) -> Result<(), PtyError> {
        let mut c = self.child.lock().map_err(|_| PtyError::Gone)?;
        c.kill().map_err(|_| PtyError::Gone)?;
        self.running.store(false, Ordering::SeqCst);
        Ok(())
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

/// `portable-pty` starts with the entire parent environment, including values
/// it discovers in the Windows registry. Agents routinely read environment
/// variables, so passing that through would turn TerminalAI into a credential
/// fan-out mechanism. Keep the process environment intentionally small.
fn configure_environment(builder: &mut CommandBuilder) {
    builder.env_clear();
    for key in safe_environment_keys() {
        if let Some(value) = std::env::var_os(key) {
            builder.env(key, value);
        }
    }
}

#[cfg(windows)]
fn safe_environment_keys() -> &'static [&'static str] {
    &[
        "PATH",
        "SYSTEMROOT",
        "TEMP",
        "USERPROFILE",
        "COMSPEC",
        "PATHEXT",
    ]
}

#[cfg(not(windows))]
fn safe_environment_keys() -> &'static [&'static str] {
    &["PATH", "HOME", "TMPDIR", "TERM", "LANG", "SHELL"]
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
        assert!(!safe_environment_keys()
            .iter()
            .any(|key| key.contains("KEY")));
        assert!(!safe_environment_keys()
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
