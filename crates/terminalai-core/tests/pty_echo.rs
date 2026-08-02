use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use terminalai_core::launch::ResolvedCommand;
use terminalai_core::pty::{default_size, PtySession};

#[cfg(windows)]
fn echo_command() -> ResolvedCommand {
    ResolvedCommand {
        program: PathBuf::from("cmd.exe"),
        args: vec!["/c".into(), "echo terminalai-pty-ok".into()],
        cwd: std::env::current_dir().expect("test cwd"),
    }
}

#[cfg(unix)]
fn echo_command() -> ResolvedCommand {
    ResolvedCommand {
        program: PathBuf::from("/bin/echo"),
        args: vec!["terminalai-pty-ok".into()],
        cwd: std::env::current_dir().expect("test cwd"),
    }
}

#[cfg(any(windows, unix))]
#[test]
fn real_pty_echo_delivers_output_and_exit_status() {
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let session = PtySession::spawn(&echo_command(), default_size(), move |chunk| {
        tx.send(chunk.to_vec()).expect("collect pty output");
    })
    .expect("spawn echo on a real pty");

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut output = Vec::new();
    let exit = loop {
        while let Ok(chunk) = rx.try_recv() {
            output.extend_from_slice(&chunk);
        }
        if let Some(code) = session.try_wait().expect("query pty exit status") {
            break code;
        }
        assert!(
            Instant::now() < deadline,
            "echo did not exit; output so far: {:?}",
            String::from_utf8_lossy(&output)
        );
        std::thread::sleep(Duration::from_millis(20));
    };

    // ConPTY/conhost can flush the final bytes just after try_wait reports the
    // child status. Keep the reader alive briefly so this test covers both
    // output delivery and exit detection rather than relying on EOF.
    let settle_deadline = Instant::now() + Duration::from_millis(300);
    while Instant::now() < settle_deadline {
        while let Ok(chunk) = rx.try_recv() {
            output.extend_from_slice(&chunk);
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    assert_eq!(
        exit,
        0,
        "echo failed: {:?}",
        String::from_utf8_lossy(&output)
    );
    assert!(
        String::from_utf8_lossy(&output).contains("terminalai-pty-ok"),
        "pty output did not contain the marker: {:?}",
        String::from_utf8_lossy(&output)
    );
}
