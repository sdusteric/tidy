# Uninstaller Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A sixth Tidy screen that moves an app and its Library leftovers to the Trash after showing every item with its size.

**Architecture:** A new pure-ish core module `tidy-core::uninstall` (list, find, guard, move) behind an `AppSigner` seam, three core commands, three tidyctl commands, and an app screen `views/uninstaller.rs` with its state, reusing the receipt dialog, the notice band and the REMOVE fade.

**Tech Stack:** Rust 2024, GPUI (gpui-kit 0.8.0), libc, Security.framework through the existing `security::codesign`.

**Spec:** `docs/superpowers/specs/2026-10-23-tidy-design.md`, section 19 (C46 to C57).

## Global Constraints

- Every removal goes through `env.trasher`; never `SystemTrash` in tests; tests use `Env::for_tests` and temp folders, never the real `~/Library` or `/Applications`.
- Left out always: `com.apple.*` ids, Apple-anchored signatures, `dev.sdusteric.tidy*`, the running Tidy bundle.
- Copy: American spelling, sentence case, no em or en dashes, buttons that open a confirm end in "…".
- Gate: `cargo build --workspace --all-targets && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace --exclude tidy-app && cargo test -p tidy-app -- --test-threads=1`, then `scripts/accept.sh offline` (FAIL 0, SKIP 0), once.

## Review Focus

- A Library entry that belongs to a different app with a longer id (`com.google.Chrome.canary` when removing `com.google.Chrome`) must stay. Test: `longer_id_of_another_app_wins`.
- A linked `Library/Caches` (pointing outside the home folder) must not let a move leave the home folder. Test: `linked_library_folder_is_refused`.
- The app starts running between the plan and the move: nothing moves. Test: `running_at_move_time_skips_everything`.
- The UI sends an id that is not a leftover (or the app path of another app): skipped. Test: `unknown_ids_are_skipped`.
- A team container shared with another installed app is never ticked. Test: `shared_team_container_is_review`.

---

### Task 1: Core listing and matching (`uninstall.rs`)

**Files:** Create `crates/tidy-core/src/uninstall.rs`; modify `lib.rs`, `model.rs` (`Action::Uninstall`, `Source::Uninstaller`), `owners.rs` (read version), `security/codesign.rs` (team id).

**Interfaces (produces):**
- `trait AppSigner: Send + Sync { fn sign(&self, app: &Path) -> AppSign }`, `struct AppSign { apple: bool, team_id: Option<String>, team_name: Option<String> }`, `struct CodeSignApps` (production), `struct FakeSigner` (by path, test helper, pub).
- `struct AppEntry { path, name, bundle_id, version: Option<String>, executable: Option<String>, team_id, team_name, bytes: u64, last_used: Option<u64>, helpers: Vec<Helper>, movable: bool }`, `struct AppList { apps: Vec<AppEntry>, left_out: usize, at: u64 }`.
- `enum LeftoverKind { App, AppSupport, Caches, Preferences, Container, GroupContainer, AppScripts, SavedState, Logs, WebData, WebKit, LaunchAgent, System }` with `label()`.
- `struct Leftover { id: String /* absolute path */, path, kind, bytes: Option<u64>, ticked: bool, selectable: bool, by_name: bool, note: Option<String> }`, `struct UninstallPlan { app: AppEntry, items: Vec<Leftover>, blocked: Option<String> }`.
- `fn list_apps(env: &Env, signer: &dyn AppSigner) -> AppList`, `fn find_leftovers(env: &Env, signer: &dyn AppSigner, app: &Path) -> Option<UninstallPlan>`, `fn system_library(env: &Env) -> PathBuf`.

- [ ] Tests (in-module, temp fixture home): `lists_apps_and_leaves_out_apple_and_tidy`, `finds_each_kind_by_bundle_id`, `helper_ids_match`, `dot_boundary` (`com.example.app2` stays), `longer_id_of_another_app_wins`, `names_match_only_when_unique_and_not_vendor`, `shared_team_container_is_review`, `own_team_container_is_ticked`, `launch_agent_by_program_path`, `system_library_is_report_only`, `root_owned_app_is_not_movable` (chmod a fixture bundle read-only), `running_app_is_blocked`.
- [ ] Implement until they pass.

### Task 2: Guarded move (`uninstall::uninstall`)

**Interfaces:** `fn uninstall(env: &Env, signer: &dyn AppSigner, app: &Path, ids: &[String], events: &mut dyn FnMut(Event)) -> HistoryEntry`; `clean::MoveGate`, `clean::move_checked`, `clean::record`, `clean::finish` become `pub(crate)`.

- [ ] Tests: `moves_app_and_ticked_leftovers_to_dir_trash` (DirTrash moves, history entry `uninstall`/`uninstaller`, before/after listing differs only by them), `unknown_ids_are_skipped`, `running_at_move_time_skips_everything`, `linked_library_folder_is_refused`, `report_only_is_never_moved`, `ignore_list_and_open_files_skip`, `empty_trash_counts_uninstall_items`.
- [ ] Implement.

### Task 3: Core commands and tidyctl

**Files:** `api.rs` (`ListApps { reply }`, `FindLeftovers { app, reply }`, `Uninstall { app, ids }`), `core.rs` (job queue entry; signer `Arc<dyn AppSigner>` on the actor, `CodeSignApps` by default), `tidy-cli/src/main.rs` (`apps`, `leftovers APP`, `uninstall APP [--ids]`), `tidy-cli/tests/cli.rs`.

- [ ] CLI tests: `apps_lists_fixture_apps`, `uninstall_moves_ticked_items`, `uninstall_refuses_real_home` (exit 5).
- [ ] Implement.

### Task 4: App screen

**Files:** create `crates/tidy-app/src/views/uninstaller.rs` (state, render, actions); modify `app.rs` (`Screen::Uninstaller`, Cmd-6, event routing for `CleanDone` with source `uninstaller`), `views/sidebar.rs`, `views/main_window.rs`, `views/history.rs` (label), `views/dialogs.rs` (receipt spec), `demo.rs` (`uninstaller`, `uninstaller-app`), `tests.rs`.

- [ ] App tests: `uninstaller_lists_and_sorts`, `opening_an_app_asks_for_leftovers`, `uninstall_opens_receipt_then_sends_command`, `open_app_disables_uninstall`, `done_returns_to_list_with_notice`, `cmd_6_selects_uninstaller`.
- [ ] Implement; shots for both scenarios in light and dark.

### Task 5: UI review, docs, gate

- [ ] impeccable critique and audit on the shots, design-taste-frontend app-screen rules, unslop on copy, motion gate check; fix findings.
- [ ] README (six screens, Cmd-6, uninstaller paragraph), spec 19 as-built notes.
- [ ] Full gate once; commit; push; install if Tidy is not running.
