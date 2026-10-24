//! Single-instance guard: an exclusive `flock` on `data/lock`.
//!
//! The app takes it for its whole life, and destructive tidyctl commands (`clean`, `trash`,
//! `empty-trash`) take it for theirs, so two writers never move or delete at once. Read-only
//! commands (`scan`, `large`, `history`) never take it and work while the app runs.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("Tidy is already running")]
    AlreadyRunning,
    #[error("couldn't open the lock file: {0}")]
    Io(#[from] io::Error),
}

/// Holds the lock until dropped (or until the process exits, even by a crash).
#[derive(Debug)]
pub struct InstanceLock {
    _file: File,
}

impl InstanceLock {
    /// Takes `flock(LOCK_EX | LOCK_NB)` on `path`, creating its folder. Returns
    /// `AlreadyRunning` when another holder (in this or another process) has it.
    pub fn acquire(path: &Path) -> Result<InstanceLock, LockError> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        // SAFETY: the fd is valid for the lifetime of `file`.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            let err = io::Error::last_os_error();
            return if err.kind() == io::ErrorKind::WouldBlock {
                Err(LockError::AlreadyRunning)
            } else {
                Err(LockError::Io(err))
            };
        }
        // The pid is informational only.
        let _ = file.set_len(0);
        let _ = write!(file, "{}", std::process::id());
        Ok(InstanceLock { _file: file })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_acquire_fails_while_held() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("data/lock");
        let first = InstanceLock::acquire(&path).unwrap();
        let err = InstanceLock::acquire(&path).unwrap_err();
        assert!(matches!(err, LockError::AlreadyRunning));
        assert_eq!(err.to_string(), "Tidy is already running");
        drop(first);
    }

    #[test]
    fn released_on_drop() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("lock");
        drop(InstanceLock::acquire(&path).unwrap());
        // flock belongs to the open file description, not the fd. When another test in this
        // binary spawns a child process, the child gets a copy of every open fd until its exec
        // closes the O_CLOEXEC ones. If that spawn lands while this test holds the lock, the
        // lock outlives our drop by that short window. So allow a brief, bounded wait: a lock
        // that is not released on drop stays held and still fails here.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let again = loop {
            match InstanceLock::acquire(&path) {
                Ok(lock) => break lock,
                Err(LockError::AlreadyRunning) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(e) => panic!("lock not released on drop: {e}"),
            }
        };
        drop(again);
    }

    #[test]
    fn contention_from_threads_has_one_winner() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("lock");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(6));
        let handles: Vec<_> = (0..6)
            .map(|_| {
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    let got = InstanceLock::acquire(&path);
                    // Keep winners alive until every thread has tried.
                    barrier.wait();
                    got.is_ok()
                })
            })
            .collect();
        let winners = handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .filter(|w| *w)
            .count();
        assert_eq!(winners, 1);
    }

    /// Runs only as the child of `other_process_contends`: takes the lock and holds it.
    #[test]
    fn lock_holder_child() {
        let Some(path) = std::env::var_os("TIDY_LOCK_TEST_CHILD") else {
            return;
        };
        let _guard = InstanceLock::acquire(Path::new(&path)).unwrap();
        println!("LOCKED");
        std::thread::sleep(std::time::Duration::from_secs(60));
    }

    /// Starts this test binary again as a child that holds the lock, checks the contention,
    /// then stops that child (by its handle, so only the process this test started).
    #[test]
    fn other_process_contends() {
        use std::io::{BufRead, BufReader};
        use std::process::{Command, Stdio};

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("lock");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "lock::tests::lock_holder_child",
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("TIDY_LOCK_TEST_CHILD", &path)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let mut locked = false;
        for line in lines.by_ref() {
            if line.unwrap().contains("LOCKED") {
                locked = true;
                break;
            }
        }
        let result = std::panic::catch_unwind(|| {
            assert!(locked, "child never took the lock");
            assert!(matches!(
                InstanceLock::acquire(&path),
                Err(LockError::AlreadyRunning)
            ));
        });
        child.kill().unwrap();
        child.wait().unwrap();
        result.unwrap();
        // The OS releases the lock when the holder dies.
        InstanceLock::acquire(&path).unwrap();
    }

    #[test]
    fn writes_pid() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("lock");
        let _g = InstanceLock::acquire(&path).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            std::process::id().to_string()
        );
    }
}
