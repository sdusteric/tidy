# Tidy

Tidy is a personal macOS utility that does the useful part of CleanMyMac: it finds space you can get back (caches, old logs, old build output, package caches), shows what takes up your disk, shows what is using your memory, runs a weekly security check, and moves things to the Trash only when you ask. Nothing runs in the background, nothing is deleted outside the Trash, and it never asks for Full Disk Access.

## Screens

The sidebar has five screens, in this order, reachable with Cmd-1 to Cmd-5:

| Screen | Key | What it does |
|---|---|---|
| Check up | Cmd-1 | One button runs the cleanup scan, the Security checks and a memory snapshot, and shows three cards that link to their screens. Holds the opt-in weekly reminder. |
| Cleanup | Cmd-2 | Space you can get back, in five groups. Safe items are ticked, Review items are not. |
| Large files | Cmd-3 | The biggest files and folders, or the ones not opened lately, with Reveal and Move to Trash. |
| Memory | Cmd-4 | A verdict from memory pressure, three figures (Pressure, Swap used, Compressed), a pressure graph and the top apps by memory with a polite Quit. The only screen that polls: every 2 seconds, and only while it is visible. |
| Security | Cmd-5 | FileVault, Firewall, Gatekeeper, System Integrity Protection, XProtect, macOS updates (on demand), login items and Chrome extensions, with New and Changed marks against your last check. It never asks for a permission. |

Cmd-R runs the current screen again.

**Quit.** Quit sends the app a polite terminate. The row says Quit requested; after 10 seconds without an exit it says Still open, and Force Quit appears in the more menu behind a confirm. Tidy, Finder, WindowServer and loginwindow have no Quit button.

**Weekly reminder.** Off by default. Switching it on asks for notification permission at that moment, never at launch. Tidy then schedules one notification for 10:00 local time, 7 days after the last check up, and moves it after each check up and on each launch, so an ignored reminder never repeats. If permission is denied, the toggle shows a button to open Notifications settings.

**Check up and macOS updates.** `softwareupdate --list` is slow and asks Apple, so it runs only when you press Check now on the Security screen or tick Include macOS updates on Check up.

It is a Rust + GPUI app built on the same stack and look as Haul. The design document is the contract: [docs/superpowers/specs/2026-10-23-tidy-design.md](docs/superpowers/specs/2026-10-23-tidy-design.md).

## Crates

| Crate | Kind | Contents |
|---|---|---|
| `crates/tidy-core` | library, no UI | The model, the command and event API, the environment and its test seams (including `MemoryProbe` and `CommandRunner`), paths, settings, history storage, the instance lock, the Trash guard, the walker, the memory engine, the Security checks and their diff against the last check, the check up log, the reminder date math and the core actor |
| `crates/tidy-app` | library + `tidy` binary | The GPUI app: sidebar, the five screens, History sheet, dialogs, the Quit and notification wrappers (`Terminator`, `Notifier`), demo scenarios |
| `crates/tidy-cli` | `tidyctl` binary | A headless driver of tidy-core that prints one JSON object per line |
| `crates/tidy-fixtures` | library + `tidy-fixture-home` binary | Fake home folders with exact sizes and dates, the results the scans must produce, and fake Security command outputs and a fake process table |

## Verify

The gates (acceptance criterion 1):

```sh
cargo build --workspace --all-targets
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --exclude tidy-app
cargo test -p tidy-app -- --test-threads=1
```

Run one crate's tests with `cargo test -p tidy-core`, or a subset with a filter such as `cargo test -p tidy-core trash::`.

A scan against a fake home, never the real one:

```sh
D=$(mktemp -d)
cargo run -p tidy-fixtures --bin tidy-fixture-home -- "$D"   # prints the TIDY_TEST_NOW value
TIDY_HOME="$D/tidy" TIDY_SCAN_ROOT="$D/home" TIDY_TEST_NOW=<printed> \
  TIDY_TEST_RUNNING=com.google.Chrome cargo run -p tidy-cli -- scan
```

Memory, Security and Check up from the command line. `memory` and `security` only read; `checkup` writes `checkups.jsonl` and the run's Security file into Tidy's data folder, so always set `TIDY_HOME`:

```sh
T=$(mktemp -d)
TIDY_HOME="$T" cargo run -p tidy-cli -- memory                     # one snapshot: pressure, swap, compressed, apps
TIDY_HOME="$T" cargo run -p tidy-cli -- security                   # the checks; softwareupdate is not run
TIDY_HOME="$T" cargo run -p tidy-cli -- security --include-updates # adds softwareupdate --list (refused under TIDY_HOME)
# checkup on the fixture home from the scan example above, with the same TIDY_HOME, TIDY_SCAN_ROOT and TIDY_TEST_NOW:
TIDY_HOME="$D/tidy" TIDY_SCAN_ROOT="$D/home" TIDY_TEST_NOW=<printed> cargo run -p tidy-cli -- checkup
```

`tidyctl` commands:

| Command | Does |
|---|---|
| `scan` | Cleanup scan, prints the plan |
| `large [--min-mb N] [--not-opened] [--folders [REL_PATH]]` | Large files scan |
| `clean --safe` or `clean --ids ID,...` | Scan, then move the items to the Trash (needs `TIDY_HOME` or `--real`) |
| `trash PATH...`, `empty-trash` | Move paths to the Trash, delete what Tidy moved there (same rule) |
| `history [--last N]` | Print history entries |
| `memory` | One memory snapshot with the apps grouped under their responsible app |
| `security [--include-updates]` | The Security checks, saved as `data/security/<id>.json`, one file per run |
| `checkup [--include-updates]` | Cleanup scan, Security checks and a memory snapshot; appends one entry to `data/checkups.jsonl` |

The full offline acceptance run is `scripts/accept.sh offline`. It prints PASS, FAIL or SKIP for AC1 to AC9 (M1 and M2) and a summary; a full run has FAIL 0 and SKIP 0. AC6 and AC7 read this Mac, read-only (`sysctl`, `vm_stat`, `top`, `fdesetup`, `socketfilterfw`, `spctl`, `csrutil`, the launch agent folders and signatures); nothing is quit, signalled or asked for.

Screenshots of the five screens, light and dark, 1800 x 1240 px, without a window or a change of the frontmost app:

```sh
scripts/shots-bundle.sh && SHOTS_RENDER=1 scripts/shots.sh checkup memory security cleanup large
```

They are written to `docs/app-shots/<scenario>-<light|dark>.png`. With no arguments `scripts/shots.sh` shoots the same five.

## Environment variables

| Variable | Effect |
|---|---|
| `TIDY_HOME=<dir>` | Moves every Tidy folder under `<dir>` (`data/`, `cache/`, `logs/`), swaps the macOS Trash for a folder Trash in `<dir>/data/trash`, and enables the test hooks below. An empty value is ignored. |
| `TIDY_SCAN_ROOT=<dir>` | The fake home that the rules and the large-file scan resolve against. Only under `TIDY_HOME`; the default is `$TIDY_HOME/home`. |
| `TIDY_TEST_RUNNING=<ids>` | Comma-separated bundle ids and process names treated as running, replacing the real process list. Only under `TIDY_HOME`. |
| `TIDY_TEST_NOW=<unix secs>` | A fixed clock for age rules and "not opened" dates. Only under `TIDY_HOME`. |
| `TIDY_TEST_COMMANDS=<file.json>` | Captured outputs of the Security status commands (and `softwareupdate`) that replace the real command runner. `tidy-fixture-home` writes `commands.json`. Only under `TIDY_HOME`. |
| `TIDY_TEST_MEMORY=<file.json>` | A fake process table and vm figures that replace the real memory probe. `tidy-fixture-home` writes `memory.json`. Only under `TIDY_HOME`. |
| `TIDY_TEST_TZ=<POSIX TZ rule>` | A fixed time zone for the reminder date, for example `PST8PDT,M3.2.0,M11.1.0`. Only under `TIDY_HOME`. |
| `TIDY_TEST_NO_SPOTLIGHT=1` | Last-use dates from file times only. The default under `TIDY_HOME`; set it to `0` to allow Spotlight. |
| `TIDY_BACKGROUND=1` | The app never activates and never shows a window. Forced on by `TIDY_DEMO`. |
| `TIDY_DEMO=<scenario>` | Fixed UI for screenshots: `checkup`, `cleanup`, `cleanup-done`, `cleanup-empty`, `large`, `large-not-opened`, `large-folders`, `large-empty`, `memory`, `memory-tight`, `security`, `security-first`, `security-items`. No core, no lock, no disk access. |
| `TIDY_DEMO_SHOT=<file.png>` | With the `shot-export` feature, renders the first frame offscreen at 2x and writes the PNG. |
| `TIDY_DEMO_EXIT_SECS` | Demo self-quit, default 30. |
| `TIDY_APPEARANCE=<light or dark>` | Forces the appearance. |
| `TIDY_LOG` | tracing filter for tidyctl (default `warn`). |

Without `TIDY_HOME`, the test hooks are ignored and logged, and destructive tidyctl commands refuse to run (exit 5) unless you pass `--real`.

## Safety rules for tests and agents

- Always set `TIDY_HOME` to a temp dir and `TIDY_SCAN_ROOT` to a fake home inside it. Never scan, clean or measure the real home folder.
- Never use the macOS Trash. Under `TIDY_HOME` the Trash is a folder, and `SystemTrash` refuses to run while `TIDY_HOME` is set. Its one real-Trash test runs only when `TIDY_TEST_REAL_TRASH=1` is set by hand.
- Never show a window or take focus. Screenshots render offscreen with `SHOTS_RENDER=1 scripts/shots.sh`.
- Never quit, terminate or signal an app or process you did not start. Tests use the fake `Terminator`; a test that needs a real process spawns its own `/bin/sleep` and acts on that PID only.
- Never request notification permission or post a notification from a test or an agent run. Tests use the fake `Notifier`.
- Never run `softwareupdate`. `SystemRunner` refuses it under `TIDY_HOME` and in tests.
- Security reads of the real Mac are read-only and limited to the status commands, the XProtect plist, the launch agent and daemon folders, `launchctl print`, signature checks of the programs those name, and a copy of Chrome's Secure Preferences.
