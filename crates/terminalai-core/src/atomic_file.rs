//! Crash-safe replacement for small user-facing files.
//!
//! Writers in TerminalAI coordinate through a sidecar advisory lock, write a
//! fully synced temporary file, and replace the destination in one rename
//! operation. A caller can also retain the previous contents as a `.bak`
//! sibling before the new file becomes visible.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(windows)]
use std::thread;
#[cfg(windows)]
use std::time::Duration;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Replace `path` with `contents` without exposing a partially written file.
///
/// When `keep_backup` is true and the destination exists, the previous bytes
/// are first retained at `<path>.bak`. The backup is itself replaced
/// atomically, so a failed write leaves the original destination untouched.
pub fn write_atomic(path: &Path, contents: &[u8], keep_backup: bool) -> io::Result<()> {
    write_atomic_with(path, contents, keep_backup, write_contents)
}

fn write_atomic_with(
    path: &Path,
    contents: &[u8],
    keep_backup: bool,
    writer: impl Fn(&mut File, &[u8]) -> io::Result<()>,
) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let _lock = AdvisoryLock::acquire(path)?;

    if keep_backup && path.is_file() {
        let previous = fs::read(path)?;
        replace_bytes(&backup_path(path), &previous, &writer)?;
    }
    replace_bytes(path, contents, &writer)
}

fn write_contents(file: &mut File, contents: &[u8]) -> io::Result<()> {
    file.write_all(contents)?;
    file.sync_all()
}

fn replace_bytes(
    path: &Path,
    contents: &[u8],
    writer: &impl Fn(&mut File, &[u8]) -> io::Result<()>,
) -> io::Result<()> {
    let temp = temporary_path(path);
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        writer(&mut file, contents)?;
        drop(file);
        replace_file(&temp, path)?;
        sync_parent(path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn temporary_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    path.with_file_name(format!(".{name}.tmp-{}-{sequence}", std::process::id()))
}

fn backup_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    path.with_file_name(format!("{name}.bak"))
}

fn lock_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    path.with_file_name(format!(".{name}.terminalai.lock"))
}

fn replace_file(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{
            MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        };

        let from: Vec<u16> = from.as_os_str().encode_wide().chain([0]).collect();
        let to: Vec<u16> = to.as_os_str().encode_wide().chain([0]).collect();
        let flags = MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH;
        let replaced = unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), flags) };
        if replaced == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        fs::rename(from, to)
    }
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> io::Result<()> {
    Ok(())
}

struct AdvisoryLock {
    file: File,
}

impl AdvisoryLock {
    fn acquire(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path(path))?;
        lock_file(&file)?;
        Ok(Self { file })
    }
}

impl Drop for AdvisoryLock {
    fn drop(&mut self) {
        unlock_file(&self.file);
    }
}

#[cfg(unix)]
fn lock_file(file: &File) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(unix)]
fn unlock_file(file: &File) {
    use std::os::unix::io::AsRawFd;
    let _ = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
}

#[cfg(windows)]
fn lock_file(file: &File) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::ERROR_LOCK_VIOLATION;
    use windows_sys::Win32::Storage::FileSystem::LockFile;

    loop {
        let locked = unsafe { LockFile(file.as_raw_handle() as _, 0, 0, u32::MAX, u32::MAX) };
        if locked != 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_LOCK_VIOLATION as i32) {
            return Err(error);
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(windows)]
fn unlock_file(file: &File) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::UnlockFile;
    let _ = unsafe { UnlockFile(file.as_raw_handle() as _, 0, 0, u32::MAX, u32::MAX) };
}

#[cfg(not(any(unix, windows)))]
fn lock_file(_file: &File) -> io::Result<()> {
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn unlock_file(_file: &File) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(1);

    fn test_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "terminalai-atomic-file-{}-{}",
            std::process::id(),
            NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).expect("test dir");
        dir
    }

    #[test]
    fn replacement_keeps_backup_and_syncs_new_contents() {
        let dir = test_dir();
        let path = dir.join("settings.json");
        fs::write(&path, b"old").expect("seed");
        write_atomic(&path, b"new", true).expect("replace");
        assert_eq!(fs::read(&path).expect("read new"), b"new");
        assert_eq!(
            fs::read(path.with_file_name("settings.json.bak")).expect("read backup"),
            b"old"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn injected_write_failure_leaves_original_intact() {
        let dir = test_dir();
        let path = dir.join("settings.json");
        fs::write(&path, b"original").expect("seed");
        let error = write_atomic_with(&path, b"replacement", true, |_file, _contents| {
            Err(io::Error::other("injected write failure"))
        });
        assert!(error.is_err());
        assert_eq!(fs::read(&path).expect("read original"), b"original");
        let _ = fs::remove_dir_all(dir);
    }
}
