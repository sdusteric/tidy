# Tidy

Tidy is a personal macOS utility that does the useful part of CleanMyMac: it finds space you can get back (caches, old logs, old build output, package caches), shows what takes up your disk, shows what is using your memory, runs a weekly security check, and moves things to the Trash only when you ask. Nothing runs in the background. The only thing Tidy deletes is what it moved to the Trash earlier, and only when you empty it. It never asks for Full Disk Access.

## Screens

The sidebar has five screens, in this order, reachable with Cmd-1 to Cmd-5:

| Screen | Key | What it does |
|---|---|---|
| Check up | Cmd-1 | One button runs the cleanup scan, the Security checks and a memory snapshot, and shows three cards that link to their screens. Holds the opt-in weekly reminder, the "What is System Data on this Mac?" sheet and the "What Tidy never does" list. |
| Cleanup | Cmd-2 | Space you can get back, in six groups (the last one, Old installers, lists `.dmg` and `.pkg` files in Downloads older than 30 days, always unticked, once a Large files scan has let Tidy into Downloads). Safe items are ticked, Review items are not. Every row's Details button (or Show Details in its "..." menu) says why it is offered, what rebuilds it, which checks guard the move, and the rule and path pattern that matched. |
| Large files | Cmd-3 | The biggest files and folders, or the ones not opened lately, with Reveal and Move to Trash. |
| Memory | Cmd-4 | A verdict from memory pressure, three figures (Pressure, Swap used, Compressed), a pressure graph, a "What would help" line while pressure is not Normal, and the top apps by memory with Relaunch and a polite Quit. Chrome, Edge, Brave and Arc rows above 4 GB name the browser's own memory setting. The only screen that polls: every 2 seconds, and only while it is visible. |
| Security | Cmd-5 | FileVault, Firewall, Gatekeeper, System Integrity Protection, XProtect, macOS updates (on demand), login items and Chrome extensions, with New and Changed marks against your last check. It never asks for a permission. |

Cmd-R runs the current screen again.

**Keyboard.** Every control is reachable with Tab and Shift-Tab, in drawing order: the sidebar, the toolbar, the rows (a row's checkbox, then its Details, Reveal and more buttons), then the status bar. Space or Return presses the focused control. The focused control gets a 2 pt ring in the text color, 2 pt off its edge, only while the last input was a key press. A row shows its buttons while Tab is on it, as it does under the pointer. On the Cleanup, Large files and Memory lists, Tab scrolls the list when it runs past the last row in view. A dialog opens with Cancel focused, so Return alone never confirms: Tab to the action button first. Tab stays inside a dialog, a sheet or a menu while it is open, Escape closes it, and the focus goes back to the control that opened it. Cmd-1 to Cmd-5 and Cmd-R work wherever the focus is.

**Relaunch.** Relaunch sends the same polite terminate, waits up to 10 seconds for the app to exit, then opens the same app bundle again without bringing it to the front. It never force quits: an app still open after 10 seconds stays open and its row says so. Only real apps have it (never the System row, plain processes, Finder or Tidy).

**Before each move.** Right before anything goes to the Trash (Cleanup and Large files alike), Tidy reads the ignore list from settings.json again (a read error stops the run), checks with libproc that no process of yours holds a file open inside the path, and for build output checks that it holds no `*-keypair.json`, no `.git` and no file Git tracks. A process list it cannot read skips every move.

**Quit.** Quit sends the app a polite terminate. The row says Quit requested; after 10 seconds without an exit it says Still open, and Force Quit appears in the more menu behind a confirm. Tidy, Finder, WindowServer and loginwindow have no Quit button.

**Weekly reminder.** Off by default. Switching it on asks for notification permission at that moment, never at launch. Tidy then schedules one notification for 10:00 local time, 7 days after the last check up, and moves it after each check up and on each launch, so an ignored reminder never repeats. If permission is denied, the toggle shows a button to open Notifications settings.

**Updates of Tidy itself.** Tidy updates itself with Sparkle (see Updates below). Tidy > Check for Updates… asks the update feed now. The Check up screen has an "Automatically install updates" switch under the weekly reminder (on by default): Tidy downloads updates by itself and installs them when it quits. With it off, Tidy still checks daily and asks first.

**System Data.** "What is System Data on this Mac?" on Check up (and "What's in System Data?" under the disk figure) opens a sheet that measures, read-only and only when opened, the parts of System Data Tidy can see without Full Disk Access: caches, logs, Xcode build data, simulators, virtual machine disks and iPhone backups when readable. Docker and local Time Machine snapshots are Report only rows with commands to copy; Tidy only ever runs `tmutil listlocalsnapshots /` and never runs `docker`. The sheet never says Apple's System Data figure will shrink.

**Check up and macOS updates.** `softwareupdate --list` is slow and asks Apple, so it runs only when you press Check now on the Security screen or tick Include macOS updates on Check up.

It is a Rust + GPUI app built on the same stack and look as Haul. The design document is the contract: [docs/superpowers/specs/2026-10-23-tidy-design.md](docs/superpowers/specs/2026-10-23-tidy-design.md).

## Crates

| Crate | Kind | Contents |
|---|---|---|
| `crates/tidy-core` | library, no UI | The model, the command and event API, the environment and its test seams (including `MemoryProbe`, `CommandRunner` and `OpenFilesProbe`), the Git index reader of the project guard, paths, settings, history storage, the instance lock, the Trash guard, the walker, the memory engine, the Security checks and their diff against the last check, the check up log, the reminder date math and the core actor |
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
| `explain ID` | Cleanup scan, then the details of one item: reason, what rebuilds it, checks, rule and pattern |
| `system-data` | What Tidy can measure of System Data; lists local snapshots with `tmutil listlocalsnapshots /` (refused under `TIDY_HOME` unless `TIDY_TEST_COMMANDS` replays it) |

The full offline acceptance run is `scripts/accept.sh offline`. It prints PASS, FAIL or SKIP for AC1 to AC9 (M1 and M2) and U1 (the update contract) and a summary; a full run has FAIL 0 and SKIP 0. AC6 and AC7 read this Mac, read-only (`sysctl`, `vm_stat`, `top`, `fdesetup`, `socketfilterfw`, `spctl`, `csrutil`, the launch agent folders and signatures); nothing is quit, signalled or asked for.

Screenshots of the five screens, light and dark, 1800 x 1240 px, without a window or a change of the frontmost app:

```sh
scripts/shots-bundle.sh && SHOTS_RENDER=1 scripts/shots.sh checkup memory security cleanup large
```

They are written to `docs/app-shots/<scenario>-<light|dark>.png`. With no arguments `scripts/shots.sh` shoots the same five. Two more show the History sheet (`history`: four entries, the newest one open, with items in the Trash that Finder's Put Back restores and one skipped item that is still in place) and the focus ring (`cleanup-focus`: the keyboard on a row checkbox).

## Updates

Tidy updates itself with Sparkle 2.10.0, following the shared contract in `../app-updates/CONTRACT.md` (the app-updates repo next to this one).

- The framework is `../app-updates/vendor/Sparkle-2.10.0/Sparkle.framework`, or the path in `SPARKLE_FRAMEWORK`. `crates/tidy-app/build.rs` links it into the `tidy` binary only (rpath `@executable_path/../Frameworks`; debug builds also get an rpath to the framework's own folder so tests and `cargo run` start). The build stops with a message when the framework is missing. tidy-core and `tidyctl` never link it.
- `scripts/bundle.sh` copies it into `Tidy.app/Contents/Frameworks` with `ditto` and signs inside out with the same certificate: `Installer.xpc`, `Downloader.xpc`, `Updater.app`, `Autoupdate`, the framework, then the app. The app's own signature is unchanged (no hardened runtime, no entitlements). `codesign --verify --deep --strict` must pass.
- Info.plist has `SUFeedURL` (`https://sdusteric.github.io/app-updates/tidy/appcast.xml`), `SUPublicEDKey` (the contract key), `SUEnableAutomaticChecks`, `SUAutomaticallyUpdate` and `SUScheduledCheckInterval` (86400).
- `CFBundleShortVersionString` is the `VERSION` file at the repo root (semver; keep it equal to the version in `Cargo.toml`). `CFBundleVersion` is the UTC build time `YYYYMMDDHHMM`.
- The app starts one `SPUStandardUpdaterController` on the main thread after launch and keeps it while it runs (`crates/tidy-app/src/updates.rs`). Sparkle keeps the user's choices in its own defaults; Tidy saves nothing for it.
- The updater never starts with `TIDY_NO_UPDATES=1`, `TIDY_BACKGROUND`, `TIDY_DEMO`, `TIDY_DEMO_SHOT`, `SHOTS_RENDER`, `TIDY_HOME`, in unit tests, or outside a bundle that has `SUFeedURL`. Check for Updates… is then greyed out and the Check up row shows "Off". `updates::decide` is that rule and has its own tests.
- `scripts/check-updates.sh <Tidy.app> <tidyctl>` checks a bundle against the contract: the framework is there, `SUPublicEDKey` is the contract key, `otool -L` shows `@rpath/Sparkle.framework/Versions/B/Sparkle` for the app and no Sparkle for `tidyctl`, and `codesign --verify --deep --strict` passes. `scripts/accept.sh offline` runs it as U1, right after the AC5 bundle.
- `scripts/release.sh [--notes notes.md] [--dry-run <dir>]` runs `scripts/accept.sh offline` (with `ACCEPT_SKIP_BUNDLE=1`, never `ACCEPT_SKIP_GATES`), builds the release bundle and a release `tidyctl`, runs `scripts/check-updates.sh`, then calls `../app-updates/tools/publish.sh --app tidy --bundle <Tidy.app>` with the notes and the dry-run folder. `APP_UPDATES_PUBLISH` points it at another publish.sh. Without `--dry-run` it publishes for real.

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
| `TIDY_BACKGROUND=1` | The app never activates and never shows a window. Forced on by `TIDY_DEMO`. Sparkle does not start. |
| `TIDY_NO_UPDATES=1` | Sparkle does not start: no update checks in this run. |
| `SPARKLE_FRAMEWORK=<path>` | The Sparkle.framework that the build links and `scripts/bundle.sh` embeds (default `../app-updates/vendor/Sparkle-2.10.0/Sparkle.framework`). |
| `APP_UPDATES_PUBLISH=<path>` | The publish.sh that `scripts/release.sh` calls (default `../app-updates/tools/publish.sh`). |
| `TIDY_DEMO=<scenario>` | Fixed UI for screenshots: `checkup`, `cleanup`, `cleanup-done`, `cleanup-empty`, `large`, `large-not-opened`, `large-folders`, `large-empty`, `memory`, `memory-tight`, `memory-relaunch`, `memory-critical`, `security`, `security-first`, `security-items`, `cleanup-why`, `system-data`, `system-data-report`, `checkup-trust`, `cleanup-confirm`, `history`, `cleanup-focus`. No core, no lock, no disk access. |
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
- Never let Sparkle start or check for updates in a test or an agent run. Tests use `FakeUpdates`; set `TIDY_NO_UPDATES=1` for anything else that runs the app.
- Never run `softwareupdate`. `SystemRunner` refuses it under `TIDY_HOME` and in tests.
- Never run `tmutil` or `docker` against this Mac. `SystemRunner` refuses `tmutil` under `TIDY_HOME` and in tests (and every verb but `listlocalsnapshots /` always), and refuses `docker` always; tests replay `tmutil` output through `TIDY_TEST_COMMANDS` or `ReplayRunner`.
- Security reads of the real Mac are read-only and limited to the status commands, the XProtect plist, the launch agent and daemon folders, `launchctl print`, signature checks of the programs those name, and a copy of Chrome's Secure Preferences.
