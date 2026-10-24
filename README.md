# Tidy

Tidy is a personal macOS utility that does the useful part of CleanMyMac: it finds space you can get back (caches, old logs, old build output, package caches), shows what takes up your disk, and moves things to the Trash only when you ask. Nothing runs in the background, nothing is deleted outside the Trash, and it never asks for Full Disk Access.

It is a Rust + GPUI app built on the same stack and look as Haul. The design document is the contract: [docs/superpowers/specs/2026-10-23-tidy-design.md](docs/superpowers/specs/2026-10-23-tidy-design.md).

## Crates

| Crate | Kind | Contents |
|---|---|---|
| `crates/tidy-core` | library, no UI | The model, the command and event API, the environment and its test seams, paths, settings, history storage, the instance lock, the Trash guard, the walker and the core actor |
| `crates/tidy-app` | library + `tidy` binary | The GPUI app: sidebar, Cleanup and Large files screens, History sheet, dialogs, demo scenarios |
| `crates/tidy-cli` | `tidyctl` binary | A headless driver of tidy-core that prints one JSON object per line |
| `crates/tidy-fixtures` | library + `tidy-fixture-home` binary | Fake home folders with exact sizes and dates, and the results the scans must produce |

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

The full offline acceptance run is `scripts/accept.sh offline`.

## Environment variables

| Variable | Effect |
|---|---|
| `TIDY_HOME=<dir>` | Moves every Tidy folder under `<dir>` (`data/`, `cache/`, `logs/`), swaps the macOS Trash for a folder Trash in `<dir>/data/trash`, and enables the test hooks below. An empty value is ignored. |
| `TIDY_SCAN_ROOT=<dir>` | The fake home that the rules and the large-file scan resolve against. Only under `TIDY_HOME`; the default is `$TIDY_HOME/home`. |
| `TIDY_TEST_RUNNING=<ids>` | Comma-separated bundle ids and process names treated as running, replacing the real process list. Only under `TIDY_HOME`. |
| `TIDY_TEST_NOW=<unix secs>` | A fixed clock for age rules and "not opened" dates. Only under `TIDY_HOME`. |
| `TIDY_TEST_NO_SPOTLIGHT=1` | Last-use dates from file times only. The default under `TIDY_HOME`; set it to `0` to allow Spotlight. |
| `TIDY_BACKGROUND=1` | The app never activates and never shows a window. Forced on by `TIDY_DEMO`. |
| `TIDY_DEMO=<scenario>` | Fixed UI for screenshots: `cleanup`, `cleanup-done`, `cleanup-empty`, `large`, `large-not-opened`, `large-folders`, `large-empty`. No core, no lock, no disk access. |
| `TIDY_DEMO_SHOT=<file.png>` | With the `shot-export` feature, renders the first frame offscreen at 2x and writes the PNG. |
| `TIDY_DEMO_EXIT_SECS` | Demo self-quit, default 30. |
| `TIDY_APPEARANCE=<light or dark>` | Forces the appearance. |
| `TIDY_LOG` | tracing filter for tidyctl (default `warn`). |

Without `TIDY_HOME`, the test hooks are ignored and logged, and destructive tidyctl commands refuse to run (exit 5) unless you pass `--real`.

## Safety rules for tests and agents

- Always set `TIDY_HOME` to a temp dir and `TIDY_SCAN_ROOT` to a fake home inside it. Never scan, clean or measure the real home folder.
- Never use the macOS Trash. Under `TIDY_HOME` the Trash is a folder, and `SystemTrash` refuses to run while `TIDY_HOME` is set. Its one real-Trash test runs only when `TIDY_TEST_REAL_TRASH=1` is set by hand.
- Never show a window or take focus. Screenshots render offscreen with `SHOTS_RENDER=1 scripts/shots.sh`.
