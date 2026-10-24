//! The contract between the UI (or tidyctl) and the core: commands in, events out (design
//! section 9). The UI never touches the disk or the Trash itself; it sends a [`Command`] and
//! renders the [`Event`]s that come back.
//!
//! Both channels are unbounded, so sending never blocks and is safe from the UI thread. The
//! app drains events with Haul's batched pump (every 100 ms), so progress events never flood
//! repaints.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

use crate::model::{HistoryEntry, LargeScan, Plan, RunningSet};
use crate::settings::Settings;

/// Which scan a progress or stop event is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanKind {
    Cleanup,
    Large,
}

/// Sent to the core.
#[derive(Debug)]
pub enum Command {
    /// Run the Cleanup rules against the scan root. Answered with [`Event::CleanupPlan`].
    /// Only one Cleanup scan runs at a time; a second request while one runs is ignored.
    ScanCleanup,
    /// Scan the home folder for large files and folder totals. Answered with
    /// [`Event::LargeResult`]. `record_min` is the smallest file the scan records; None means
    /// [`crate::model::LARGE_RECORD_MIN`] (50 MB, C13). tidyctl passes a smaller value for
    /// `--min-mb` below that.
    ScanLarge { record_min: Option<u64> },
    /// Move these items of the last Cleanup plan to the Trash, re-checking each one right
    /// before its move. Answered with progress, one [`Event::ItemTrashed`] or
    /// [`Event::ItemSkipped`] per id, then [`Event::CleanDone`].
    Clean { item_ids: Vec<String> },
    /// Move these paths (from Large files) to the Trash through the same guard. Item ids in
    /// the events are the paths as given. Ends with [`Event::CleanDone`] whose action is
    /// `trash`.
    TrashPaths { paths: Vec<PathBuf> },
    /// Delete for good the items Tidy moved to the Trash, by their recorded Trash paths.
    /// Answered with [`Event::EmptyTrashDone`].
    EmptyTidyTrash,
    /// Stop the running scans at their next folder. Answered with [`Event::ScanStopped`] for
    /// each scan that was running.
    Cancel,
    /// "Never show this again": add the path to the ignore list in settings. Answered with
    /// [`Event::SettingsChanged`].
    Ignore { path: PathBuf },
    /// Replace the settings (clamped, then saved). Answered with [`Event::SettingsChanged`].
    UpdateSettings(Settings),
    /// Read the process list now and send [`Event::RunningChanged`] when it differs from the
    /// last one sent. The Cleanup screen sends this every 3 seconds while it is visible (C6).
    CheckRunning,
    /// Every history entry, oldest first. Unreadable lines are skipped.
    LoadHistory {
        reply: oneshot::Sender<Vec<HistoryEntry>>,
    },
    /// Stop everything cleanly, then reply on the channel.
    Shutdown(oneshot::Sender<()>),
}

/// Sent from the core to the UI.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// A scan is running. `current` is the folder it is in, for logs and the CLI.
    ScanProgress {
        kind: ScanKind,
        files: u64,
        bytes: u64,
        current: Option<PathBuf>,
    },
    /// A Cleanup scan finished. The list appears all at once (C4).
    CleanupPlan(Plan),
    /// A Large files scan finished.
    LargeResult(LargeScan),
    /// A scan was cancelled before it finished. The previous result stays valid.
    ScanStopped { kind: ScanKind },
    /// `done` of `total` items of a clean or trash have been handled.
    CleanProgress { done: usize, total: usize },
    /// One item reached the Trash. The row fades out.
    ItemTrashed {
        id: String,
        trash_path: Option<PathBuf>,
    },
    /// One item failed a check or the move. The row stays with the reason.
    ItemSkipped { id: String, reason: String },
    /// A clean or trash ended. The entry is already in history.jsonl.
    CleanDone(HistoryEntry),
    /// Empty Trash ended. The entry is already in history.jsonl.
    EmptyTrashDone(HistoryEntry),
    /// The running processes changed (after a [`Command::CheckRunning`] or a scan).
    RunningChanged(RunningSet),
    /// The settings changed and were saved.
    SettingsChanged(Settings),
    /// Something the user asked for could not be done. `what` names the action (`scan`,
    /// `large`, `clean`, `trash`, `empty_trash`, `settings`), `message` is a plain sentence.
    Failed { what: String, message: String },
}

/// A clonable handle for sending commands to the core.
#[derive(Clone, Debug)]
pub struct CoreHandle {
    tx: mpsc::UnboundedSender<Command>,
}

impl CoreHandle {
    pub fn new(tx: mpsc::UnboundedSender<Command>) -> CoreHandle {
        CoreHandle { tx }
    }

    /// Returns false when the core has stopped and the command was dropped.
    pub fn send(&self, cmd: Command) -> bool {
        self.tx.send(cmd).is_ok()
    }

    /// Asks the core for its history and waits for the answer. Empty when the core is gone.
    pub async fn history(&self) -> Vec<HistoryEntry> {
        let (reply, rx) = oneshot::channel();
        if self.send(Command::LoadHistory { reply }) {
            rx.await.unwrap_or_default()
        } else {
            Vec::new()
        }
    }

    /// Asks the core to shut down and waits until it has finished. Returns at once if the
    /// core is already gone.
    pub async fn shutdown(&self) {
        let (done_tx, done_rx) = oneshot::channel();
        if self.send(Command::Shutdown(done_tx)) {
            let _ = done_rx.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_reports_closed_channel() {
        let (tx, rx) = mpsc::unbounded_channel();
        let h = CoreHandle::new(tx);
        assert!(h.send(Command::ScanCleanup));
        drop(rx);
        assert!(!h.send(Command::ScanCleanup));
    }

    #[test]
    fn send_from_plain_thread_arrives() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let h = CoreHandle::new(tx);
        let h2 = h.clone();
        std::thread::spawn(move || {
            h2.send(Command::Clean {
                item_ids: vec!["a:b".into()],
            })
        })
        .join()
        .unwrap();
        match rx.try_recv().unwrap() {
            Command::Clean { item_ids } => assert_eq!(item_ids, ["a:b"]),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn shutdown_waits_for_reply() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let h = CoreHandle::new(tx);
        tokio::spawn(async move {
            if let Some(Command::Shutdown(done)) = rx.recv().await {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                let _ = done.send(());
            }
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), h.shutdown())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn shutdown_and_history_return_when_core_is_gone() {
        let (tx, rx) = mpsc::unbounded_channel();
        drop(rx);
        let h = CoreHandle::new(tx);
        h.shutdown().await;
        assert!(h.history().await.is_empty());
    }

    #[test]
    fn scan_kind_names() {
        assert_eq!(
            serde_json::to_string(&ScanKind::Large).unwrap(),
            "\"large\""
        );
    }
}
