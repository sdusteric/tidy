//! Tidy core: the scan engine, the Trash and history, and the actor the UI and tidyctl talk
//! to. No UI dependencies.
//!
//! Everything that touches the disk takes an [`Env`], which carries the scan root, Tidy's
//! folders, the settings and the four seams (Trasher, ProcessProbe, Clock, LastUsed). Tests
//! and agents always run with `TIDY_HOME` (or [`Env::for_tests`]), so they never see the real
//! home folder or the real Trash.

pub mod api;
pub mod clean;
pub mod cleanup;
pub mod core;
pub mod env;
pub mod history;
pub mod large;
pub mod lastused;
pub mod lock;
pub mod model;
pub mod owners;
pub mod packages;
pub mod paths;
pub mod procs;
pub mod rules;
pub mod settings;
pub mod store;
pub mod trash;
pub mod walk;

pub use api::{Command, CoreHandle, Event, ScanKind};
pub use core::start;
pub use env::{Clock, Env, LastUsed, ProcessProbe, SystemSeams, Trasher};
pub use lock::{InstanceLock, LockError};
pub use model::{
    Class, FolderNode, Group, HistoryEntry, HistoryItem, Item, LargeItem, LargeScan, Plan,
    RunningSet,
};
pub use paths::AppPaths;
pub use settings::Settings;
