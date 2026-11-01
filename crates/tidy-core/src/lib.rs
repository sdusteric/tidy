//! Tidy core: the scan engine, the Trash and history, and the actor the UI and tidyctl talk
//! to. No UI dependencies.
//!
//! Everything that touches the disk takes an [`Env`], which carries the scan root, Tidy's
//! folders, the settings, the local time zone and the six seams (Trasher, ProcessProbe,
//! Clock, LastUsed, and from M2 MemoryProbe and CommandRunner). Tests and agents always run
//! with `TIDY_HOME` (or [`Env::for_tests`]), so they never see the real home folder or the
//! real Trash.

pub mod api;
pub mod checkup;
pub mod clean;
pub mod cleanup;
pub mod core;
pub mod env;
pub mod explain;
pub mod gitindex;
pub mod history;
pub mod large;
pub mod lastused;
pub mod lock;
pub mod memory;
pub mod model;
pub mod owners;
pub mod packages;
pub mod paths;
pub mod procs;
pub mod reminder;
pub mod rules;
pub mod security;
pub mod settings;
pub mod shelf;
pub mod store;
pub mod sysdata;
pub mod trash;
pub mod uninstall;
pub mod walk;

pub use api::{Command, CoreHandle, Event, ScanKind};
pub use core::{start, start_with_sources};
pub use env::{
    Clock, CommandOutput, CommandRunner, Env, LastUsed, MemoryProbe, OpenFilesProbe, ProcessProbe,
    SystemSeams, Trasher,
};
pub use lock::{InstanceLock, LockError};
pub use model::{
    AppMemory, CheckId, CheckResult, Checkup, ChromeExtension, Class, FolderNode, Group,
    HistoryEntry, HistoryItem, Item, LargeItem, LargeScan, LoginItem, Mark, MemoryKind,
    MemorySnapshot, Plan, Pressure, RunningSet, SecurityReport, SettingsLink, Signer, Status,
};
pub use paths::AppPaths;
pub use reminder::ReminderDate;
pub use settings::Settings;
