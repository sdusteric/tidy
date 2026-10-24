//! Fake system answers for running a check up on the fixture home: the outputs of the
//! Security status commands and a process table, in the formats of `TIDY_TEST_COMMANDS`
//! ([`tidy_core::env::ReplayRunner::from_file`]) and `TIDY_TEST_MEMORY`
//! ([`tidy_core::env::FixedMemory::from_file`]).
//!
//! The command outputs are the ones captured on the development Mac (FileVault on, the
//! firewall off, Gatekeeper and SIP on), so a run with them has exactly one amber system
//! check. `softwareupdate` has no answer: a run that tried it would get "not found", and the
//! runner records the call so a test can prove it never happened.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tidy_core::env::{CommandOutput, MemoryTable};
use tidy_core::model::{PageStats, ProcInfo, SwapUsage, VmStats};

/// The file names [`write_fakes`] uses.
pub const COMMANDS_FILE: &str = "commands.json";
pub const MEMORY_FILE: &str = "memory.json";

const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;
/// The page size of Apple silicon.
pub const FAKE_PAGE_SIZE: u64 = 16_384;

/// The swap in use in [`fake_memory`].
pub const FAKE_SWAP_USED: u64 = 512 * MIB;
/// The compressor pages in [`fake_memory`]; times [`FAKE_PAGE_SIZE`] this is 1 GiB.
pub const FAKE_COMPRESSOR_PAGES: u64 = GIB / FAKE_PAGE_SIZE;

/// The Security status commands and their captured outputs.
pub fn fake_commands() -> BTreeMap<String, CommandOutput> {
    let mut m = BTreeMap::new();
    for (line, out) in [
        ("/usr/bin/fdesetup status", "FileVault is On.\n"),
        (
            "/usr/libexec/ApplicationFirewall/socketfilterfw --getglobalstate",
            "Firewall is disabled. (State = 0)\n",
        ),
        ("/usr/sbin/spctl --status", "assessments enabled\n"),
        (
            "/usr/bin/csrutil status",
            "System Integrity Protection status: enabled.\n",
        ),
    ] {
        m.insert(line.to_string(), CommandOutput::ok(out));
    }
    m
}

fn proc(pid: i32, ppid: i32, name: &str, path: Option<&str>, footprint: Option<u64>) -> ProcInfo {
    ProcInfo {
        pid,
        ppid,
        responsible: None,
        uid: if footprint.is_some() { 501 } else { 0 },
        name: name.to_string(),
        path: path.map(PathBuf::from),
        footprint,
        // As when `top` cannot run: the System row has no size.
        top_footprint: None,
    }
}

/// A small process table at normal pressure: an app with two helpers, a second app, a
/// command-line process, and two root processes Tidy cannot read.
pub fn fake_memory() -> MemoryTable {
    let editor = "/Applications/Example Editor.app/Contents/MacOS/Example Editor";
    let helper = "/Applications/Example Editor.app/Contents/Frameworks/Example Editor Helper.app/Contents/MacOS/Example Editor Helper";
    let notes = "/Applications/Example Notes.app/Contents/MacOS/Example Notes";
    MemoryTable {
        own_pid: 0,
        processes: vec![
            proc(1, 0, "launchd", Some("/sbin/launchd"), None),
            proc(90, 1, "WindowServer", None, None),
            proc(400, 1, "Example Editor", Some(editor), Some(2 * GIB)),
            proc(
                401,
                400,
                "Example Editor Helper",
                Some(helper),
                Some(600 * MIB),
            ),
            proc(
                402,
                400,
                "Example Editor Helper",
                Some(helper),
                Some(300 * MIB),
            ),
            proc(500, 1, "Example Notes", Some(notes), Some(400 * MIB)),
            proc(600, 1, "node", Some("/usr/local/bin/node"), Some(250 * MIB)),
        ],
        vm: VmStats {
            pressure_level: 1,
            total_bytes: 16 * GIB,
            swap: SwapUsage {
                total: 2 * GIB,
                used: FAKE_SWAP_USED,
                free: 2 * GIB - FAKE_SWAP_USED,
            },
            pages: PageStats {
                page_size: FAKE_PAGE_SIZE,
                free: 40_000,
                active: 300_000,
                inactive: 280_000,
                speculative: 5_000,
                wired: 120_000,
                purgeable: 2_000,
                external: 250_000,
                internal: 330_000,
                compressor: FAKE_COMPRESSOR_PAGES,
                compressed: 3 * FAKE_COMPRESSOR_PAGES,
                swapins: 0,
                swapouts: 0,
            },
        },
    }
}

/// The files [`write_fakes`] wrote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FakeFiles {
    /// For `TIDY_TEST_COMMANDS`.
    pub commands: PathBuf,
    /// For `TIDY_TEST_MEMORY`.
    pub memory: PathBuf,
}

/// Writes [`COMMANDS_FILE`] and [`MEMORY_FILE`] into `dir`.
pub fn write_fakes(dir: &Path) -> Result<FakeFiles> {
    let files = FakeFiles {
        commands: dir.join(COMMANDS_FILE),
        memory: dir.join(MEMORY_FILE),
    };
    for (path, json) in [
        (
            &files.commands,
            serde_json::to_string_pretty(&fake_commands())?,
        ),
        (&files.memory, serde_json::to_string_pretty(&fake_memory())?),
    ] {
        fs::write(path, format!("{json}\n"))
            .with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidy_core::env::{CommandRunner, FixedMemory, MemoryProbe, ReplayRunner};

    #[test]
    fn the_files_read_back_as_the_fakes() {
        let tmp = tempfile::tempdir().unwrap();
        let files = write_fakes(tmp.path()).unwrap();
        let runner = ReplayRunner::from_file(&files.commands).unwrap();
        let out = runner
            .run(
                "/usr/bin/fdesetup",
                &["status"],
                std::time::Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(out.stdout, "FileVault is On.\n");
        assert!(
            runner
                .run(
                    "/usr/sbin/softwareupdate",
                    &["--list"],
                    std::time::Duration::from_secs(1)
                )
                .is_err()
        );
        let memory = FixedMemory::from_file(&files.memory).unwrap();
        assert_eq!(memory.processes(), fake_memory().processes);
        assert_eq!(memory.vm().unwrap(), fake_memory().vm);
        assert_eq!(
            memory.vm().unwrap().pages.compressed_bytes(),
            GIB,
            "1 GiB compressed"
        );
    }
}
