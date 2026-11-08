# Tidy design

Date: 2026-10-23. Status: approved for milestone 1 (the user asked for the recommended option on every open choice).

Tidy is a personal macOS utility that replaces CleanMyMac for one person on one Mac. It is a Rust + GPUI app built on the same stack, crate split and look as Haul (`/Users/ericlee/Documents/projects/haul`). This document is the contract for building it. Where it says "C<n>", that is a call made by the agent on the user's behalf, listed in section 15 so the user can check them in one place.

## 1. Intent

The user asked for four things, in their words:

1. "clean up files that can be purged"
2. "release memory somehow"
3. "what files are huge"
4. "quick security check"

Follow-up answers: memory matters when the Mac "was either slow or a weekly routine run". Cleanup should show both a list and a one-click clean. Project folders should be scanned. Huge files should show the biggest ones and the ones not opened lately on the same screen, with a toggle or filter. The security check is for weekly peace of mind.

Why build it: CleanMyMac costs a subscription, runs background agents, asks for Full Disk Access, and uses alarming copy to sell the next action. Tidy should do the useful 20% of that with nothing running in the background, nothing deleted outside the Trash, and no scare copy.

## 2. Goals

- Find space that can be reclaimed with no visible loss (caches, old logs, old build output, package caches) and clean it with one click.
- Show the larger, riskier items (AI models, old device symbols, big app data) for the user to decide on, never cleaned by the one click.
- Answer "what is taking up my disk" with a list of large files and a folder drill-down, including "big and not opened in months".
- Explain why the Mac is slow (memory pressure, swap, which apps use the most) and let the user quit an app politely (M2).
- Give a weekly security summary in plain words, with each amber item linking to the right System Settings page (M2).
- One "Check up" button that runs all of it, with an optional weekly reminder (M2).
- Uninstall an app together with what it leaves in the home folder's Library (section 19, added 2026-10-31): the last CleanMyMac feature the user relied on.
- Be safe by construction: every removal goes to the Trash and is written to a history file; tests and agents can never touch the real home folder or the real Trash.

## 3. Non-goals (for now)

- Finding leftovers of apps that are already gone (orphans). The uninstaller of section 19 starts from an installed app.
- Malware database or signature scanning. XProtect already does this; Tidy only reports whether XProtect is fresh.
- App updater.
- Menu bar monitor or any always-running agent.
- Duplicate file finder, photo cleanup, mail attachment cleanup.
- Cleaning anything outside the user's home folder, or anything that needs root.
- Distribution outside this Mac (no notarization, no Developer ID, no App Store).

## 4. Anti-goals

Tidy must never do these, and a change that does one is a bug:

- Delete anything without going through the Trash. The only permanent deletion is Empty Trash, a separate action with its own confirm.
- Prompt for, require, or nag about Full Disk Access. Tidy works with what macOS grants a normal app, and says plainly when something is out of reach.
- Show false or inflated alarms ("Your Mac is infected", "3,412 junk files are slowing your Mac", red badges for normal states). Compressed memory and file cache are normal and are described as normal.
- Nag: no Dock badge for cleanable space, no notifications other than the opt-in weekly reminder, no "upgrade" or "act now" copy, no reminder more than once a week.
- Use `sudo`, a privileged helper, a launch daemon, or a launch agent.
- Kill a process. Quitting is a polite request (`NSRunningApplication.terminate`), and Force Quit needs an explicit confirm (M2).
- Run cleanup commands implicitly (`brew cleanup`, `pnpm store prune`, `simctl delete`, `purge`, `tmutil`). If Tidy suggests one, it shows the exact command for the user to copy.

## 5. Screens

The window has a sidebar on the left and the selected screen on the right. M1 ships Cleanup and Large files. M2 adds Check up (at the top of the sidebar), Memory and Security (C1: the sidebar shows only screens that work; no "coming soon" rows).

Sidebar order when complete: Check up, Cleanup, Large files, Memory, Security. Shortcuts: Cmd-1 to Cmd-5 in that order (in M1, Cmd-2 and Cmd-3 for the two screens). Cmd-R rescans the current screen. Cmd-W closes the window and the app keeps running; a Dock click reopens it (same as Haul, C2).

### 5.1 Cleanup (M1)

**First visit.** No automatic scan, because a scan of project folders under `~/Documents` triggers a macOS privacy prompt and prompts must only follow a click (C3). The screen shows an empty state with a Scan button. After the first explicit scan, Tidy records `cleanup_scanned_once = true` in settings and later visits rescan automatically when the screen opens and the last scan is older than 10 minutes.

**Scan.** Runs the rule table in section 6 against the scan root. Progress shows in the toolbar as a spinner and "Scanning, 41,200 files". The list appears when the scan finishes (no streaming rows, C4). A typical scan on this Mac measures about 30 GB across a few hundred thousand files and should finish in under 10 seconds with the bulk walker.

**Result.** Six groups in a fixed order: System and app caches, Logs, Developer, AI models and large app data, Old installers (C39), Trash. Each group header shows a tri-state checkbox, the group name, the item count and the group total. Groups with zero items are hidden. Groups start expanded except "AI models and large app data", which starts collapsed (C5).

Each item has one of four classes:

| Class | Checkbox | Included in "Clean safe items" | Shown |
|---|---|---|---|
| Safe | ticked | yes | yes |
| Review | unticked | no, only if the user ticks it | yes, with a "Review" tag and a one-line reason |
| Report only | none | never | yes, with "Tidy won't remove this" and the reason |
| Excluded | n/a | never | never |

An item whose owner app is running is shown unticked and disabled, with the meta line "Quit Google Chrome first" in the warning color. While the Cleanup screen is visible, Tidy re-checks running processes every 3 seconds (cheap, no disk access) and enables the item when the app quits, ticking it again if its class is Safe (C6). Tidy never quits the app from this screen.

**Item row content.** Checkbox, a glyph tile for the group, the name (the app or tool name when known, otherwise the folder name), the meta line (the rule's reason, for example "Cache, rebuilt by the app" or "Build output, project not touched for 41 days"), and the size on the right. Hover shows Reveal in Finder and a "..." menu with "Never show this again" (adds the path to the ignore list in settings) and "Copy Path".

**Clean safe items.** The primary toolbar button reads "Clean 6.3 GB" (the sum of ticked items). With nothing ticked it reads "Clean" and is disabled, the summary Figure reads "Nothing selected" (no "ready to clean" after it), and the status bar says what is left ("6 items, none selected").

- When every ticked item is Safe, clicking it cleans immediately with no dialog, because everything goes to the Trash and can be put back (C7).
- When any Review item is ticked, the button reads "Clean selected" and opens a confirm dialog that lists the ticked Review items by name and size: "Move these to the Trash? You can put them back from the Trash until you empty it." Buttons: Cancel, Move to Trash.

**During a clean.** The button shows "Cleaning 4 of 12". Each row fades out over 140 ms as its item reaches the Trash (Haul's REMOVE motion; instant under Reduce Motion). Right before moving each item, Tidy re-runs that item's checks (section 6.2). An item that fails a check stays in the list with the reason ("Google Chrome opened, skipped"). One clean writes one history entry.

**After a clean.** A notice band under the toolbar: "Moved 6.3 GB to the Trash. Empty the Trash to free the space." with buttons "Empty Trash…" and Show History. It stays until dismissed or the next scan.

**Rows that are gone.** When the Cleanup or Large files screen is shown again, and when the window becomes active, rows whose path no longer exists (one `symlink_metadata` each, off the UI thread above 200 paths) are dropped and the totals follow, with no rescan. After a clean, skipped rows stay with their reason and trashed rows do not return until a rescan finds them again.

**Status bar.** Left: "12 selected, 6.3 GB". Right: "In the Trash from Tidy: 2.1 GB" and a text button "Empty Trash…".

**Empty Trash.** A separate action, never part of Clean (decided before this document). It opens a confirm dialog: "Permanently delete the 214 items Tidy moved to the Trash (2.1 GB)? This can't be undone." Buttons: Cancel, Empty Trash (destructive style). It deletes only the items Tidy put there, using the Trash paths recorded in history, because listing `~/.Trash` needs Full Disk Access (C8). If macOS refuses (EPERM), Tidy shows "macOS didn't let Tidy empty the Trash. Empty it from Finder." and writes the failure to history. Other items in the Trash are never touched and never counted.

**History.** A clock icon button in the toolbar opens a History sheet: one row per entry (date, action, item count, size, "3 skipped" when relevant), expandable to the item list with original path, size and result. Rows offer Reveal in Finder for items still at their original path. Put Back is Finder's job (it works because Tidy uses the same Trash call as Finder), so Tidy has no Put Back button in M1 (C9).

### 5.2 Large files (M1)

**Scope.** The home folder minus the hard denylist (section 7). This includes `~/Library` so that, for example, 66.9 GB of LTX models in Application Support shows up. Items under `~/Library` carry an "App data" tag (C10).

**First visit.** Empty state: "See what takes up space. Tidy lists files and folders over 500 MB in your home folder. macOS may ask once each for Documents, Desktop and Downloads." Button: "Scan Home Folder". The first scan is the only time those prompts can appear. Later visits reuse the last result (saved in the cache folder) and show "Scanned 2 hours ago" with Rescan; they do not rescan on their own, because a home scan is the slow one (C11).

**Two modes**, picked with a segmented control in the toolbar: Files and Folders.

**Files mode.** One list of items at or above the size threshold. A file is an item. A package (`.app`, `.bundle`, `.framework`, `.xcodeproj`, `.xcworkspace`, `.photoslibrary` is excluded, and any folder that conforms to `com.apple.package`) counts as one item with the total size of its contents; Tidy never lists files inside a package. A second segmented control picks the filter:

- **Biggest**: all items at or above the threshold, sorted by size, largest first.
- **Not opened lately**: the same items, keeping only those whose last-use date is older than 90 days, sorted by size (C12).

Last-use date: Spotlight's `kMDItemLastUsedDate` when it exists (apps and documents opened through LaunchServices). Otherwise the later of mtime and atime. The row says which one it is: "Opened 8 months ago" or "Modified 2 years ago". Items with neither (impossible in practice) count as old.

Threshold: a menu with 100 MB, 500 MB (default), 1 GB, 5 GB. It applies to both filters and to Folders mode. Changing it refilters the saved scan without rescanning (the scan records every file of 50 MB or more and every folder total, C13).

Row content: checkbox, a glyph tile by kind (film, archive, disk image, app, folder, document), the name, the path in mono under it with the middle truncated (`~/Movies/2025/.../trip.mov`), the date phrase, and the size. Tags: "App data" for items under `~/Library`, "Package" for bundles. Hover shows Reveal in Finder and Move to Trash.

**Folders mode.** A drill-down starting at the home folder. A breadcrumb row under the toolbar ("Home > Library > Application Support"). Rows are the children of the current folder, sorted by size: folders, packages and files above the threshold, each with a 4 px bar under the name showing its share of the current folder. Children below the threshold are folded into one last row "1,204 smaller items, 3.1 GB" with no actions. Double-click or the trailing chevron opens a folder. Packages are leaves. Cmd-Up goes to the parent.

**Move to Trash.** Large files are user data, so every move asks first, even for one item (C14). Ticking rows shows "3 selected, 12.4 GB" in the status bar with a Move to Trash button. The hover trash button selects that single row and opens the same dialog. Cmd-Backspace does the same for the selection. The dialog lists names and sizes. When any item is "App data", the dialog adds: "Some of these belong to apps. The app may stop working or download them again." The move writes one history entry.

**Size honesty.** Sizes are allocated bytes (space that would be freed). APFS clones share space and Tidy cannot see that, so totals in the summary read "up to 214 GB". Because Tidy cannot detect clones, the Move to Trash dialog adds "Copies made by Finder can share space, so this may free less" whenever the selection is over 1 GB (C15).

### 5.3 Memory (M2)

Purpose: answer "why is my Mac slow right now" and let the user quit what is heavy.

- **Verdict line** at the top, chosen from memory pressure: Normal: "Memory is fine. macOS is using spare memory for caching, which is normal." Warning: "Memory is tight. Quitting a large app below will help." Critical: "Memory is very tight. Your Mac is moving memory to disk, which slows it down."
- **Three figures**: Pressure (Normal, Warning, Critical from `kern.memorystatus_vm_pressure_level`), Swap used (from `vm.swapusage`), Compressed (compressor pages times page size). Each has a one-line explanation under it that says what the figure is, never whether it is fine, so it cannot contradict a Warning figure: "How hard macOS works to find free memory.", "Memory macOS moved to disk to make room.", "Memory macOS compressed to fit more in." The verdict line is in `text` like the other screens' summaries; only the Pressure figure and the graph bars carry the warning and error colors. A small pressure history graph covers the time the screen has been open (no background sampling); its caption says how long that is ("Pressure since you opened this screen", "2 min"), and once it is full, "Pressure in the last 5 min".
- **Top apps by memory**: processes grouped under their responsible app (`responsibility_get_pid_responsible_for_pid` via `dlsym`, falling back to the parent chain and then to the `.app` prefix of `proc_pidpath`). Each row: app icon, name, footprint ("4.2 GB"), number of helper processes, and a Quit button. Rows for processes Tidy cannot attribute show the process name. Root-owned and other users' processes cannot be read by an app; they are summed into one row "System processes" (sized with `top`, see the M2 notes), without a Quit button.
- **Quit** sends `terminate`. The row shows "Quit requested" and then disappears when the app exits. After 10 seconds without exit, the row shows "Still open. The app may be asking to save." and the "..." menu offers Force Quit with a confirm ("Unsaved changes in X will be lost."). Tidy itself, Finder, WindowServer and loginwindow have no Quit button.
- Refresh every 2 seconds while the screen is visible; nothing runs when it is not.
- No "Free up RAM" button, because there is no honest user-level way to do it (`purge` needs root and only drops file cache).
- Relaunch next to Quit, a tip about the browser's own memory setting on Chromium browser rows above 4 GB, and a "What would help" line while pressure is not Normal: section 18.2 (C41 to C43).

### 5.4 Security (M2)

A list of checks, each with a status dot (green, amber, gray for "could not check"), a title, one plain sentence, and for amber items a button that opens the right System Settings page. Results of each run are saved, and items that changed since the last run show "New" or "Changed".

| Check | Source | Amber when | Button |
|---|---|---|---|
| FileVault | `fdesetup status` | Off | Open Privacy & Security |
| Firewall | `socketfilterfw --getglobalstate` | Disabled | Open Network Settings (firewall anchor unverified) |
| Gatekeeper | `spctl --status` | Assessments disabled | Open Privacy & Security |
| System Integrity Protection | `csrutil status` | Disabled | none, explains how to turn it back on in Recovery |
| XProtect | mtime and version of `XProtect.bundle/Contents/Info.plist` | Not updated in 60 days | Open Software Update |
| macOS updates | `softwareupdate --list`, run on demand, 60 s timeout | An update is listed | Open Software Update |
| Login items and background helpers | plists in `~/Library/LaunchAgents`, `/Library/LaunchAgents`, `/Library/LaunchDaemons`, plus `launchctl print gui/<uid>` | An item is new since the last check, or its program is unsigned or not notarized | Reveal in Finder, Move to Trash (user agents only), Open Login Items |
| Chrome extensions | `Secure Preferences` of every Chrome profile (copied before parsing) | An extension was added since the last check | Open `chrome://extensions` is not possible from outside Chrome, so the row says where to look |

Login items list "new since last check" first. Each item shows label, program path, signer (team name or "Apple" or "Not signed"), notarized yes or no (from `SecStaticCodeCheckValidity` and `SecAssessment`, shown as "Checked by Apple" or "Not checked by Apple"), and whether it runs at load ("Starts automatically" or "Runs when needed"; a launch daemon adds "System service"). Signature checks run in the background, one item at a time.

The summary line joins its parts with " · " and drops a sentence's own full stop there: "This is your first check. Next time Tidy will show what changed · 1 not checked". A gray check counts as "not checked" (it could not run, or it is macOS updates, which wait for a click). On screen the macOS updates row says "Not checked. Tidy asks Apple for updates only when you click Check Now." next to a Check Now button; tidyctl keeps the core's longer sentence.

The first run has nothing to compare against and says "This is your first check. Next time Tidy will show what changed." Gray items say why ("Tidy couldn't read this without Full Disk Access") with no button that asks for access.

Copy rules: no "threat", "infected", "danger" or "risk score". An amber item says what is off and what it means in one sentence, for example "Firewall is off. Your Mac accepts incoming connections from apps that ask for them. Most home networks are fine either way."

### 5.5 Check up (M2)

One button, "Run Check up", that runs the cleanup scan, the security check (without the slow `softwareupdate --list` unless the user ticked "Include macOS updates") and a memory snapshot. The result is three cards: Cleanup ("6.3 GB ready to clean" with a Clean button that does the same as on the Cleanup screen), Security ("All good" or "2 things to look at"), Memory (the verdict line). Each card links to its screen. The date of the last check up is the screen's summary Figure ("Last check up 2 days ago", with "Cleanup, security and memory in one pass. Nothing is cleaned until you choose." under it), the same 64 pt strip the other screens open with; the Run Check up button (the large primary, 28 pt) and "Include macOS updates" share the row under it. A card fades its result in over 200 ms when its part reports during a run.

**Weekly reminder** (opt-in toggle on this screen, off by default). Turning it on requests notification permission at that moment, never at launch. Tidy schedules one non-repeating `UNCalendarNotificationTrigger` for 7 days after the last check up, at 10:00 on that day, and reschedules it on each launch and after each check up (C16). If the app is never opened again, at most one reminder arrives. The notification says "Time for your weekly check up." and opens Tidy on Check up. If permission is denied, the toggle shows "Notifications are off for Tidy" with a button "Open Notifications Settings". The reminder is a Haul settings row: title and note at the left, the switch at the right. The note already says when the reminder fires, so the screen has no status bar.

## 6. Cleanup rules

### 6.1 Rule table

Paths are relative to the scan root (the home folder in production). "Owner not running" means no process of the owning app or tool is running, checked at scan time and again right before the move. "Age" uses the newest mtime of any file inside the target, never the folder's own mtime.

**System and app caches**

| Target | Class | Rule |
|---|---|---|
| `Library/Caches/com.apple.*` | Excluded | macOS-managed. |
| `Library/Caches/<name>` where `<name>` maps to a known app or is a bundle id of an installed app | Safe | Owner not running. The folder itself goes to the Trash; the app recreates it. No age filter (C17). |
| `Library/Caches/<name>` with no known owner | Review | "Tidy couldn't tell which app owns this." |
| `Library/Caches/Google/Chrome/<profile>`, `Library/Caches/Google/Chrome-headless`, `Library/Caches/Google/Chrome for Testing-headless` | Safe | Chrome (or `chrome-headless-shell`) not running. |
| `Library/Application Support/<App>/{Cache, Code Cache, GPUCache, DawnGraphiteCache, DawnWebGPUCache, GrShaderCache, ShaderCache, Crashpad/completed}` for Electron and Chromium apps, including each Chrome profile | Safe | Owner not running. Only these subfolder names, never their siblings. |
| `.../Service Worker/CacheStorage` (same apps) | Review | "Some websites may load slower once or lose offline data." |
| `.../Service Worker/ScriptCache` (same apps) | Review | `cache.electron.scripts`, "Some extensions may stop working until you turn them off and on again." Moved out of Safe on 2026-10-25 (C32). |
| `Library/Application Support/Google/Chrome/{component_crx_cache, extensions_crx_cache, GraphiteDawnCache, GrShaderCache, ShaderCache}` | Safe | Chrome not running. |
| `Library/Application Support/Caches/<name>` | same as `Library/Caches/<name>` | |
| `Library/Caches/Tidy` and every Tidy folder | Excluded | |

**Logs**

| Target | Class | Rule |
|---|---|---|
| Files in `Library/Logs/<app>/` | Safe | File older than 14 days and owner not running. Files only; the folder stays. |
| Files in `Library/Logs/DiagnosticReports/` | Safe | Older than 30 days. |
| `Library/Logs/Tidy` | Excluded | |
| `/Library/Logs` | Excluded | Outside the home folder. |

**Developer**

| Target | Class | Rule |
|---|---|---|
| `Library/Developer/Xcode/DerivedData/<entry>` | Safe | Xcode not running, entry older than 7 days. |
| `Library/Developer/Xcode/UserData/Previews` | Safe | Xcode not running. |
| `Library/Developer/CoreSimulator/Caches` | Safe | CoreSimulatorService not running. |
| `Library/Developer/Xcode/iOS DeviceSupport/<entry>` (and watchOS, tvOS, visionOS) | Review | Never the newest entry per device model. "Xcode copies it again when you connect the device, which takes a few minutes." |
| `Library/Developer/Xcode/Archives/<date>/<archive>` | Review | Older than 90 days. "Holds the symbols for a build you shipped." |
| `Library/Developer/Xcode/CodingAssistant` | Report only | Unknown format. |
| `Library/Developer/CoreSimulator/Devices` | Report only | Needs `simctl` to remove safely; Tidy shows "Remove unused simulators with: xcrun simctl delete unavailable" (C18). |
| Project build output (table 6.3) | Safe or Review per table | Marker present, age over 30 days, no blocking process. |
| `.npm/_cacache` | Safe | No `npm` running. |
| `.npm/_npx` | Safe | No `npm`, `npx` or `npm exec` running. |
| `Library/Caches/node-gyp` | Safe | No `node-gyp` running. |
| `Library/Caches/Homebrew/downloads/*` | Safe | File older than 30 days, no `brew` running. Never runs `brew cleanup`. |
| `Library/Caches/pip`, `Library/Caches/pnpm`, `.cache/uv`, `.cache/pip`, `.bun/install/cache` | Safe | Owner tool not running. |
| `.gradle/caches` | Safe | No `gradle` or `java` running. |
| `.gradle/wrapper` | Review | "Gradle downloads it again on the next build." |
| `.m2/repository` | Review | "May hold libraries you built locally with mvn install." (C19) |
| `.cargo/registry/{cache, src}` | Review | No `cargo` or `rustc` running. "Cargo downloads crates again; offline builds fail until then." |
| `.cocoapods/repos` | Review | |
| `Library/pnpm/store` | Report only | "Reclaim unused packages with: pnpm store prune" |
| `Library/Caches/ms-playwright/<browser>-<rev>` | Review | Never the newest revision per browser. |
| `.local/share/claude/versions/*`, `.local/share/cursor-agent/versions/*` | Review | Never the newest version or the one a running process uses. |
| `Library/Application Support/Zed/{languages, node}` | Review | Zed not running. |
| `Library/Application Support/Google/AndroidStudio*` | Review | Never the newest version folder. |
| `.cache/<tool>` for firebase, gh, vscode-ripgrep, yt-dlp, pre-commit | Safe | Owner tool not running. Unknown `.cache/<name>` folders are not offered. |
| `Library/Caches/{go-build, gopls, golangci-lint, composer, typescript, deno, black}` | Safe | Through `cache.app`: the static owner table names Go, gopls, golangci-lint, Composer, TypeScript, Deno and Black (C38). |
| `Library/Caches/pypoetry/{cache, artifacts}` | Safe | `dev.poetry`, Poetry not running. |
| `Library/Caches/pypoetry/virtualenvs/<env>` | Review | `dev.poetry-venvs`, older than 30 days, Poetry not running. "A project's Python environment. poetry install makes it again." |
| `Library/Caches/JetBrains/<product>/{caches, index}` | Safe | `dev.jetbrains`, no JetBrains IDE running. Local History and the rest of the product folder are never offered. |
| `.gem/ruby/<v>/cache`, `.rbenv/versions/<v>/lib/ruby/gems/<abi>/cache` | Safe | `dev.gem`, no `gem` or `bundle` running. Installed gems stay. |
| `.bundle/cache` | Safe | `dev.bundler`, no `gem` or `bundle` running. |
| `.pyenv/cache`, `.rbenv/cache` | Safe | `dev.pyenv-cache`, `dev.rbenv-cache`, pyenv (or python-build) and rbenv (or ruby-build) not running. Source downloads only; installed versions stay. |
| `.cache/puppeteer`, `.cache/selenium`, `Library/Caches/electron` | Review | `dev.tool-downloads`, never a build a running process uses. Large to download again. |
| `go/pkg/mod` | Report only | `dev.go-modcache`, "Remove downloaded Go modules with: go clean -modcache" (the files are read-only). |
| `.cargo/bin`, `.rustup`, `.docker` | Excluded | Installed software. |
| `.claude`, `.claude.json`, `.codex`, `.cursor`, `Library/Application Support/Cursor/User` | Excluded | AI coding tools' conversations, sessions, transcripts and memory (Claude Code `projects` and `sessions`, Codex `sessions` and `history.jsonl`, Cursor `chats`, `projects` and workspace history). Nothing rebuilds them (C37). |

**AI models and large app data** (always Review, unticked, never included in the one-click clean, one row per model where the layout allows)

| Target | Class | Rule |
|---|---|---|
| `.cache/huggingface/hub/models--*` | Review | Per model. |
| `.cache/lm-studio/models/<publisher>/<model>`, `.lmstudio/models/<publisher>/<model>` | Review | Per model, LM Studio not running. |
| `.ollama/models` | Review | Whole folder, Ollama not running. |
| `.cache/whisper`, `.cache/fluidaudio`, `Library/Application Support/FluidAudio` | Review | Owner not running. |
| `.cache/hyperframes/<sub>` | Review | Per subfolder, no hyperframes process running. |
| `Library/Application Support/LTXDesktop/models/<model>` | Review | Per model, LTX Desktop not running. |
| `Library/Application Support/anythingllm-desktop/storage/models` | Review | AnythingLLM not running. The rest of `storage` is Report only (it holds workspaces and chats). |
| `Library/Application Support/Google/Chrome/{OptGuideOnDeviceModel, SODALanguagePacks, Snapshots}` | Review | Chrome not running. "Chrome downloads it again when needed." |
| `.colima`, `Library/Application Support/Claude/vm_bundles`, other VM disks | Report only | "Virtual machine disk. Remove it from the app that made it." |

**Old installers** (C39; always Review, unticked, never part of the one-click clean)

| Target | Class | Rule |
|---|---|---|
| `Downloads/*.dmg`, `Downloads/*.pkg` (files directly in Downloads) | Review | `downloads.installers`, older than 30 days. "In Downloads, not changed for 45 days". Downloads is read only after the first Large files scan, whose click is when macOS asks for it. |

**Trash**

| Target | Class | Rule |
|---|---|---|
| Items Tidy moved to the Trash (from history) that still exist | Empty Trash action | Separate confirmed action, never ticked, never part of Clean. |
| Anything else in the Trash | Excluded | Tidy cannot list it without Full Disk Access and does not try. |

### 6.2 Rules that apply to every target

1. Dry run is the default: a scan only produces a plan. Nothing moves until the user clicks.
2. Skip anything modified in the last 24 hours, in every group (age rules above are stricter where they exist).
3. Minimum size per row is 1 MB. Smaller matches of the same rule fold into one row, "38 small items, 12 MB", that cleans as a unit.
4. Never follow symlinks and never cross a volume boundary (compare the device id with the scan root's).
5. Right before each move, re-check: the path still exists, `realpath` is strictly inside the scan root, it is not on the denylist or the user's ignore list, its markers are still present (table 6.3), the project guard still passes (6.3), the owner is not running, and no process of the user holds a file open inside it (C34). A failed check skips the item with a reason. The ignore list is read from settings.json again for every move, on every path to the Trash (Clean, Large files, project output), and a read error stops the run (C36). A process list that cannot be read skips every move (C35).
6. On EPERM or "Operation not permitted", skip the item with "macOS didn't allow access" and never retry in a loop.
7. Only targets in the rule table are offered. There is no "scan everything" mode and no free-form delete in Cleanup.
8. Owner resolution: a static table maps folder names to bundle ids and process names (Google to Chrome, `company.thebrowser.dia` to Dia, and so on). A folder name that looks like a bundle id is checked against installed apps in `/Applications` and `~/Applications` by reading each `Info.plist`. Running processes come from `proc_listpids` plus `proc_pidpath`; the bundle id of a process is read from the `Info.plist` of the `.app` that contains its executable (C20: plain libc, works the same in the app and in tidyctl).
9. Each rule has a time budget of 30 seconds per scan (gathering, dates and sizes together). A rule that runs out is stopped, offers nothing, and the scan says "Skipped <rule> because it took too long. Scan again to try it."; the other rules go on (C40).

### 6.3 Project build output

Project roots: `~/Documents/projects`, `~/Developer`, `~/Projects`, `~/src`, `~/code`, whichever exist, editable in settings (C21). Walk each root, never into `.git`, never into a match (do not walk inside `node_modules`), never through symlinks.

| Folder | Required marker | Class |
|---|---|---|
| `target` | `Cargo.toml` in the parent, or `CACHEDIR.TAG` inside with the Cargo signature line | Safe after 30 days, no `cargo` or `rustc` running |
| `node_modules` | `package.json` in the parent | Safe after 30 days |
| `.next`, `.nuxt`, `.svelte-kit`, `.turbo`, `.parcel-cache`, `.angular` | `package.json` in the parent | Safe after 30 days |
| `Pods` | `Podfile` and `Podfile.lock` in the parent | Safe after 30 days (Review when `Podfile.lock` is missing) |
| `.build` | `Package.swift` in the parent | Safe after 30 days |
| `.gradle`, Android `build` | `build.gradle`, `build.gradle.kts` or `settings.gradle*` in the parent | Safe after 30 days, no `gradle` or `java` running |
| `.venv`, `venv` | `pyvenv.cfg` inside, and `pyproject.toml`, `requirements.txt` or `uv.lock` beside it | Safe after 30 days |
| `__pycache__` | a `.py` file beside it | Safe |
| `DerivedData` | an `.xcodeproj` or `.xcworkspace` beside it | Safe after 7 days, Xcode not running |
| `dist`, `build` | `package.json`, `Cargo.toml`, `build.gradle*` or `CMakeLists.txt` in the parent, and the folder named in the project's `.gitignore` | Review |
| `vendor` | `composer.json` or `Gemfile` in the parent, plus its lock file | Review |

**Project guard** (C33). Whatever its markers say, build output is never offered, and is skipped at the move, when it holds a `*-keypair.json` file, a `.git` folder or file anywhere inside, or a file Git tracks (read from the repository's `.git/index`, versions 2 to 4, without running `git`). An index that cannot be read counts as tracking something.

**Offered means movable.** The scan never offers a row whose move the checks would refuse. It leaves such a row out (it does not show it as Review): the rule is the same "never offered" as the project guard, so a row that cannot move never comes back ticked on every scan. Besides the project guard, the scan leaves out any folder that holds a name the trash guard refuses (`Cookies`, `Login Data`, `*.sqlite` and the rest of the denied names), which the move reports as "Holds files Tidy never touches". Project roots are walked under the spelling the disk has: a settings entry `Documents/projects` finds `Documents/Projects` on a case-insensitive volume, and entries that resolve to one folder are walked once, so item paths match what the move check resolves and a real link anywhere on the path is still refused.

"After 30 days" means the newest file mtime anywhere in the project folder (the parent, excluding the build output itself and `.git`) is older than 30 days, so an active project is never offered. On this Mac that means Haul's 35 GB `target` is not offered while Haul is being worked on, which is correct.

## 7. Scanning scope and privacy (TCC)

- **Scope**: the home folder only. Nothing outside it is scanned except reads needed for checks (`/Applications` Info.plists for owner lookup; M2 security sources).
- **Hard denylist** (never scanned, never shown, in Cleanup and Large files): `Library/Containers`, `Library/Group Containers`, `Library/Mail`, `Library/Messages`, `Library/Safari`, `Library/Calendars`, `Library/Reminders`, `Library/Application Support/AddressBook`, `Library/Application Support/com.apple.TCC`, `Library/Metadata`, `Library/Biome`, `Library/Keychains`, `Library/Mobile Documents` (iCloud Drive), `Pictures/Photos Library.photoslibrary`, `.Trash`, `.ssh`, `.gnupg`, `.aws`, `.config`, and inside browser profiles `Login Data`, `Cookies`, `IndexedDB`, `Local Storage`, `Session Storage`, `File System`, and any `*.sqlite`.
- **Protected folders the app does scan**: Documents, Desktop and Downloads. macOS asks once per folder. The first access happens only from a click (Scan Home Folder, or the first Cleanup scan when a project root is under Documents). Info.plist carries `NSDocumentsFolderUsageDescription`, `NSDesktopFolderUsageDescription` and `NSDownloadsFolderUsageDescription` with a plain sentence each. If the user declines, Tidy skips that folder and shows one line in the summary: "Skipped Documents (access not allowed). You can allow it in System Settings > Privacy & Security > Files and Folders." with a button to that pane.
- **No Full Disk Access.** Tidy never asks for it and never shows an onboarding step for it. Paths that need it are excluded up front, so the user never sees a wall of EPERM rows.
- **Tests and agents** never scan Documents, Desktop or Downloads: under `TIDY_HOME` the scan root is a fake home (section 8), and project roots resolve inside it.
- **Signing and grants**: the bundle is signed with the same Apple Development certificate as Haul and the stable bundle id `dev.sdusteric.tidy`, so the folder grants survive rebuilds.

## 8. Data, history and test redirection

### 8.1 Files

| File | Production location | Under `TIDY_HOME` |
|---|---|---|
| `settings.json` | `~/Library/Application Support/Tidy/` | `$TIDY_HOME/data/` |
| `history.jsonl` | `~/Library/Application Support/Tidy/` | `$TIDY_HOME/data/` |
| `lock` | `~/Library/Application Support/Tidy/` | `$TIDY_HOME/data/` |
| `security/<yyyy-mm-dd>.json` (M2) | `~/Library/Application Support/Tidy/` | `$TIDY_HOME/data/` |
| `checkups.jsonl` (M2) | `~/Library/Application Support/Tidy/` | `$TIDY_HOME/data/` |
| `last-cleanup.json`, `last-large.json` | `~/Library/Caches/Tidy/` | `$TIDY_HOME/cache/` |
| `tidy.log` | `~/Library/Logs/Tidy/` | `$TIDY_HOME/logs/` |
| Trash | the macOS Trash (`SystemTrash`) | `$TIDY_HOME/data/trash/` (`DirTrash`) |
| Scan root | the home folder | `$TIDY_SCAN_ROOT`, default `$TIDY_HOME/home` |

### 8.2 Settings

`settings.json` follows Haul's pattern: `#[serde(default)]` struct, a `version` field with migrations, unknown enum values fall back to defaults, clamped on save, written atomically (temp file, fsync, rename, fsync the folder). Fields in M1: `version`, `large_min_bytes` (500 MB), `not_opened_days` (90), `project_roots`, `ignored_paths`, `cleanup_scanned_once`, `large_scanned_once`. M2 adds `weekly_reminder`, `include_updates_in_checkup`.

### 8.3 History

`history.jsonl`: one JSON object per line, appended with `O_APPEND` and fsync. A line that fails to parse is skipped on read and logged, never fatal. One entry per user action:

```json
{"id":"2026-10-23T14:02:11Z-1","at":1792764131,"action":"clean","source":"cleanup","trasher":"system",
 "items":[{"path":"/Users/ericlee/Library/Caches/node-gyp","trash_path":"/Users/ericlee/.Trash/node-gyp","bytes":135266304,"rule":"dev.node-gyp","result":"trashed"},
          {"path":"/Users/ericlee/Library/Caches/Google/Chrome/Default","trash_path":null,"bytes":1857028096,"rule":"cache.chrome","result":"skipped","reason":"Google Chrome is running"}],
 "trashed_bytes":135266304}
```

`action` is `clean`, `trash` (from Large files) or `empty_trash`. `trasher` is `system` or `dir` (so a test or a reader can prove which Trash was used). `trash_path` comes from the `resultingItemURL` out-parameter of `trashItemAtURL`, which Haul discards and Tidy keeps.

### 8.4 Environment variables

| Variable | Effect |
|---|---|
| `TIDY_HOME=<dir>` | Moves every Tidy folder under `<dir>` (table 8.1), swaps the trasher for `DirTrash`, and enables the test hooks below. An empty value is ignored. |
| `TIDY_SCAN_ROOT=<dir>` | The fake home that rules and the large-file scan resolve against. Honored only when `TIDY_HOME` is set; without `TIDY_HOME` it is ignored and logged. |
| `TIDY_TEST_RUNNING=<ids>` | Comma-separated bundle ids and process names treated as running, replacing the real process list. Only under `TIDY_HOME`. |
| `TIDY_TEST_NOW=<unix secs>` | Fixed clock for age rules and "not opened" dates. Only under `TIDY_HOME`. |
| `TIDY_TEST_NO_SPOTLIGHT=1` | Use file times only for last-use dates. Only under `TIDY_HOME` (and it is the default under `TIDY_HOME`, so fixture dates decide, C22). |
| `TIDY_BACKGROUND=1` | The app never activates and never shows a window (windows exist, hidden). Forced on by `TIDY_DEMO`. |
| `TIDY_DEMO=<scenario>` | Fixed UI for screenshots: `cleanup`, `cleanup-done`, `cleanup-empty`, `large`, `large-not-opened`, `large-folders`, `large-empty`. Unknown values fall back to `cleanup`. No core, no lock, no disk access. |
| `TIDY_DEMO_SHOT=<file.png>` | With the `shot-export` feature, renders the first frame offscreen at 2x and writes the PNG. |
| `TIDY_DEMO_EXIT_SECS` | Demo self-quit, default 30. |
| `TIDY_APPEARANCE=<light or dark>` | Forces the appearance, applied before `gpui_kit::init`. |
| `TIDY_LOG` | tracing filter for tidyctl (default `warn`). |

Safety by construction: the only code path that constructs `SystemTrash` is `Env::from_env()` when `TIDY_HOME` is unset, and `SystemTrash::trash` itself refuses (returns an error) when `TIDY_HOME` is set in the process environment, as a second guard.

## 9. Architecture and crate layout

Workspace at the repo root, resolver 3, edition 2024, `[workspace.package] version = "0.1.0"` (read by `bundle.sh`). Pins copied exactly from Haul: `gpui-kit =0.8.0`, `gpui-pre =0.3.9` and `gpui-pre-macos =0.3.9` (shot-export and dev-dependency only), `objc2 =0.6.5`, `objc2-foundation =0.3.2` (add `NSFileManager`, `NSURL`, `NSError`, `NSString`, `NSBundle`, `NSProcessInfo`), `objc2-app-kit =0.3.2` (`NSApplication`, `NSWorkspace`, `NSRunningApplication`, `NSResponder`, `NSImage` if needed), `tokio =1.54.0`, `serde_json =1.0.154`, `dirs =7.0.0`, `libc 0.2`, `anyhow`, `thiserror`, `tracing`, `tracing-subscriber`, `tempfile` (dev). M2 adds `security-framework` (or FFI), `objc2-user-notifications`, `mach2`. No network crates.

| Crate | Kind | Contents |
|---|---|---|
| `crates/tidy-core` | lib, no UI | `model` (Item, Class, Group, Plan, LargeScan, FolderNode, HistoryEntry), `api` (Command, Event, CoreHandle), `env` (Env, the four seams below, test hooks), `paths` (AppPaths, `TIDY_HOME`), `settings`, `store` (write_atomic), `lock`, `walk` (bulk walker), `rules` (the rule table as data), `cleanup` (scan to Plan), `large` (files and folder tree), `procs` (process probe), `lastused` (Spotlight FFI), `trash` (Trasher, SystemTrash, DirTrash, guard), `clean` (executor), `history`, `core` (the actor behind `start`) |
| `crates/tidy-app` | lib + bin `tidy` | Haul's shell (main, app, theme, motion, macos, demo, assets, format) plus `views/{sidebar, cleanup, large, history, dialogs, mod}` |
| `crates/tidy-cli` | bin `tidyctl` | Headless driver of tidy-core, JSON lines out |
| `crates/tidy-fixtures` | lib + bin `tidy-fixture-home` | Builds fake home trees with exact sizes and dates, and the expected results |

**Seams (traits in `tidy-core::env`)**, each with a production and a fake implementation:

- `Trasher`: `fn trash(&self, path: &Path) -> io::Result<Option<PathBuf>>` returning the Trash path. `SystemTrash` (NSFileManager) and `DirTrash` (moves into a folder, " 2" suffix on collisions, records moves). Haul's version returns `()`; Tidy's returns the resulting path.
- `ProcessProbe`: `fn running(&self) -> RunningSet` (bundle ids and process names). `LibcProbe` and `FixedProbe`.
- `Clock`: `fn now(&self) -> SystemTime`. `SystemClock` and `FixedClock`.
- `LastUsed`: `fn last_used(&self, path: &Path) -> Option<SystemTime>`. `Spotlight` (MDItem FFI) and `FileTimesOnly`.

`Env { root, paths, trasher, procs, clock, last_used, settings, overridden }` is passed to every scan and clean function. `Env::from_env()` builds the production or `TIDY_HOME` variant; `Env::for_tests(fake_home, tidy_home)` builds the all-fake variant and never touches the real home.

**Core boundary** (same shape as Haul): `tidy_core::start(env, rt) -> (CoreHandle, UnboundedReceiver<Event>)`. Commands: `ScanCleanup`, `ScanLarge`, `Clean { item_ids }`, `TrashPaths { paths }`, `EmptyTidyTrash`, `Cancel`, `Ignore { path }`, `UpdateSettings`, `LoadHistory { reply }`, `Shutdown(oneshot)`. Events: `ScanProgress { kind, files, bytes, current }`, `CleanupPlan(Plan)`, `LargeResult(LargeScan)`, `CleanProgress { done, total }`, `ItemTrashed { id, trash_path }`, `ItemSkipped { id, reason }`, `CleanDone(HistoryEntry)`, `EmptyTrashDone(HistoryEntry)`, `RunningChanged(RunningSet)`, `Failed { what, message }`. Scans run on `spawn_blocking`; only one scan of each kind at a time; `Cancel` stops at the next directory. The app uses Haul's batched event pump (100 ms) so progress events never flood repaints.

Item ids are stable strings `<rule id>:<path relative to root>`, so the CLI and tests can name items.

**Walker** (`walk.rs`): `getattrlistbulk` through `libc` with `ATTR_CMN_RETURNED_ATTRS | NAME | DEVID | OBJTYPE | FILEID` and `ATTR_FILE_LINKCOUNT | TOTALSIZE | ALLOCSIZE`, descending with `openat`, skipping `VLNK`, stopping at a different device id, deduplicating hard links on (device, file id) when the link count is above 1, and reporting both allocated and logical bytes. A plain `std::fs` walker with the same semantics exists as the reference; a test asserts both give identical totals on every fixture. Work is split across a few threads by top-level folder.

**Lock**: `flock` on `data/lock`, taken by the app and by destructive tidyctl commands. Read-only tidyctl commands (`scan`, `large`, `history`) never take it, so they work while the app runs.

**tidyctl** commands (one JSON object per line on stdout, each with an `"event"` key; the last line is the result):

| Command | Does | Exit codes |
|---|---|---|
| `tidyctl scan` | Cleanup scan, prints progress then `{"event":"plan",...}` | 0, 1 error |
| `tidyctl large [--min-mb N] [--not-opened] [--folders [REL_PATH]]` | Large files scan | 0, 1 |
| `tidyctl clean --safe` or `--ids ID,...` | Scan, then clean the safe items (or the named ids) | 0, 3 some items failed or skipped, 4 lock held, 5 refused |
| `tidyctl trash PATH...` | Move paths to the Trash through the same guard as Large files | same |
| `tidyctl empty-trash` | Delete the items Tidy moved to the Trash | same |
| `tidyctl history [--last N]` | Print history entries | 0 |

Destructive commands refuse with exit 5 and "Refusing to use the real home folder. Set TIDY_HOME, or pass --real." unless `TIDY_HOME` is set or `--real` is given (C23). Exit 2 is bad usage.

**App `--self-test`**: prints a JSON report (version, bundle id, resolved paths, free space on the home volume, walker check on a temp tree it creates and removes inside its own temp dir) and exits before any GPUI object exists. It never scans the real home.

## 10. Test strategy

Rules for every test and every agent run: never the real Trash, never the real home folder, never a visible window, never focus.

- **Fixtures** (`tidy-fixtures`): `FakeHome::build(dir, clock) -> Expected` writes a declarative tree under a temp dir (always under `$TMPDIR`, never under Documents). File sizes are multiples of 4096 bytes filled with a deterministic non-zero pattern, so allocated bytes equal logical bytes on APFS and expected sizes are exact. Dates are set with `utimensat` relative to the fixed clock. `Expected` lists every item the scans must find with id, class, ticked, blocked reason and bytes, and every path that must not be found. The bin `tidy-fixture-home <dir>` writes the tree plus `expected-plan.json` and `expected-large.json` and prints the `TIDY_TEST_NOW` value to use.
- **What the standard fixture contains**: a cache of an idle known app (Safe), a Chrome cache with Chrome in `TIDY_TEST_RUNNING` (blocked), a `com.apple.*` cache (Excluded, absent from output), a cache with no known owner (Review), logs older and newer than 14 days, a crash report older than 30 days, DerivedData older and newer than 7 days, an idle Rust project with `target` (Safe), an active Rust project touched yesterday (not offered), an old `node_modules` (Safe), a `dist` not in `.gitignore` (not offered) and one in `.gitignore` (Review), `.npm/_cacache` (Safe), a Hugging Face model (Review), a VM disk folder (Report only), a symlink pointing outside the root (not followed), a hard-linked pair (counted once), a sparse file with a large logical size and tiny allocation (not a large file), a `Big.app` bundle of many small files whose total passes the threshold (one item), a large file last modified 200 days ago and one modified 3 days ago, and a denylisted `Library/Containers` folder with a big file (never seen).
- **Core unit tests** (`#[test]` in each module): rule matching, marker checks, age math with `FixedClock`, owner resolution, the `trash::check` guard (relative paths, `..`, symlinks out of the root, the root itself), walker equivalence, settings migration, history append and corrupt-line tolerance.
- **Core integration tests** (`crates/tidy-core/tests/`): scan the fixture and compare to `Expected` exactly; clean safe items with `DirTrash` and assert the moved set equals the Safe set and every other fixture path is unchanged (a before and after tree listing); re-check at clean time (flip a `FixedProbe` between scan and clean, assert the item is skipped); large files modes and filters.
- **CLI tests** (`crates/tidy-cli/tests/`): run the built `tidyctl` with `TIDY_HOME`, `TIDY_SCAN_ROOT`, `TIDY_TEST_NOW` and `TIDY_TEST_RUNNING` against a fixture, parse JSON lines, check exit codes, and check that destructive commands without `TIDY_HOME` exit 5 (run with `HOME` pointed at a temp dir as an extra guard).
- **App tests** (`crates/tidy-app/src/tests.rs`): Haul's harness. `#[gpui::test]` on `TestAppContext`, fake `Core` channel, recording `Opener` (Reveal in Finder) and no real trasher (the app only sends commands). Tests feed `CleanupPlan` and `LargeResult` events and assert rows, ticks, totals, button labels, the confirm dialog for Review items, the fade then `Clean` command, the threshold refilter, the folder drill-down, and the `LastMenu` contents. Run with `--test-threads=1`. `macos.rs` calls are `#[cfg(not(test))]`.
- **Screenshots**: `shot-export` feature, `TIDY_DEMO` scenarios, `SHOTS_RENDER=1` offscreen render at 2x. The script launches with `open -g -n` and `TIDY_BACKGROUND=1`, `TIDY_HOME=<mktemp>`, `TIDY_SCAN_ROOT=<mktemp>/home`, checks the frontmost app before and after (exit 3 if it changed), and never needs Screen Recording.
- **Guard test**: a test greps the repo's test sources and fails if `SystemTrash` appears outside `trash.rs` and `env.rs`.
- **Never in tests**: `SystemTrash`, the real `HOME`, `open` without `-g`, Spotlight queries on real paths, `softwareupdate`, notifications.

## 11. Visual design

Reuse Haul v2 (`haul/docs/mockup/v2/DESIGN.md`) as the base: the same `Theme` struct and tokens (`window_bg`, `bar_bg`, `raised`, `fill`, `fill_2`, `row_hover`, `row_pressed`, `text`, `text_2`, `text_3`, `separator`, `separator_strong`, `accent`, `accent_ring`, `track`, `success*`, `warning*`, `error*`, `gray_badge`, `button_bg`, `pop_bg`, `group_bg`), `Theme::edge` for hairlines, `button_fill`, `sync_component_tokens` after `sync_system_appearance`, system font, `tnum` for every size and count, mono for paths, Lucide icons at 16 px stroke 1.75, and the same motion constants and Reduce Motion behavior. Light and dark follow the system and `TIDY_APPEARANCE`.

**New tokens** (C24):

| Token | Light | Dark | Use |
|---|---|---|---|
| `sidebar_bg` | `#F4F4F6` | `#232326` | Sidebar |
| `sidebar_selected` | black 7% | white 9% | Selected sidebar row |
| `tag_border` | = `separator_strong` | = `separator_strong` | Review, App data, Package tags |
| `danger_button` | = `error_badge` | = `error_badge` | Empty Trash confirm button fill, white text |
| `checkbox_border` | black 28% | white 30% | Outline of an unticked checkbox (about 3:1 on the window). A disabled unticked box is filled with `fill` and outlined with `separator_strong`, so it does not read as clickable |

Cards and groups (Check up cards, the reminder row, Memory figures and graph) share one treatment: `fill` flattened on `window_bg` (`Theme::card_bg`), radius 10, no border; a clickable card hovers to `row_hover` over that. The switch knob takes Haul's knob shadow (`0 1px 2px black 30%` + a 0.5 pt black 6% edge).

**New type role**: Figure, 22 / 28, weight 600, tabular, `text`. Used for the summary number.

**Window**: 900 x 620 pt default, minimum 760 x 480, transparent title bar, traffic lights at x 20 inside the sidebar, centered on the 52 pt toolbar row. Title "Tidy".

**Sidebar** (200 pt wide, `sidebar_bg`, a hairline `separator` on its right edge):
- Top 52 pt holds the traffic lights only.
- Rows start at y 52, inset 10 left and right: height 28, radius 6, padding 0 8, gap 8; Lucide icon 16 (Check up `Stethoscope`, Cleanup `BrushCleaning`, Large files `HardDrive`, Memory `MemoryStick`, Security `ShieldCheck`; add any not bundled to `icon_assets!`) in `text_2`, label 13/400 `text`, trailing figure 12 tnum `text_2` (Cleanup: the safe total after a scan, "6.3 GB", hidden when it is zero; Large files: the item count). Selected row: `sidebar_selected` fill, icon in `accent`. 4 pt between rows.
- Bottom, 16 pt from the edge: "Macintosh HD" 12/500 `text`, a 4 pt bar (track `track`, fill `accent`, radius 2) showing used space, and "212 GB available of 1 TB" 12 tnum `text_2`. Available means `NSURLVolumeAvailableCapacityForImportantUsageKey`, which matches Finder (C25).

**Content area** (`window_bg`):
- **Toolbar** 52 pt, aligned with the title bar, padding 0 20, so the right-most control lines up with the content edge. Left: screen title 15/600. Right, gap 8: screen controls, then the primary button. Drag and double-click on empty toolbar space move and zoom the window (Haul's `start_window_move`). A hairline shows only when the list is scrolled, at the edge where the list goes under what stays put: under the summary strip on Cleanup, under the "Top apps by memory" header on Memory, under the breadcrumb in Folders mode, and under the toolbar elsewhere. On Security the summary scrolls with the checks.
- **Summary strip** 64 pt, padding 0 20: the Figure ("6.3 GB" with " ready to clean" in 15/400 `text_2` on the same baseline), and under it one 12/400 `text_2` line ("34 GB more to review · Scanned 2 min ago").
- **Notice band** (when present) as in Haul: margin 2 12 6, padding 8 8 8 12, radius 8, `fill` background, icon, sentence, buttons on the right.
- **List** in a `uniform_list`, inset 8 left and right.
- **Status bar** 30 pt, `bar_bg`, hairline on top, padding 0 10 0 20: left text 12 tnum `text_2`, right text buttons. Left out when it has nothing to say (the empty states before a first scan, Check up).

**Cleanup list** (every row 44 pt so `uniform_list` holds; group headers are rows too):
- Group header row: padding 0 10, gap 8: chevron 12 `text_3` (rotates 90 degrees when expanded, snap, no animation), checkbox 16 (tri-state), name 13/600 `text`, count 12 `text_2` ("14 items"), flexible space, size 13/600 tnum `text`, then an empty 60 pt area so the group total lines up with the item sizes. A hairline `separator` above every group header except the first.
- Item row: left padding 30 (its checkbox sits under the group's checkbox, one column of boxes), gap 10: checkbox 16, glyph tile 28 (radius 6, `fill_2`, icon 16 `text_2`), a text column (name 13/500 `text` on top with an optional tag 8 pt after it, as Haul's quality tag; meta 12/400 `text_2` below; meta uses `warning` for "Quit X first" and `text_2` otherwise; the tag is 11/600 `text_2`, 1 px `tag_border`, radius 4, height 16, padding 0 5, text "Review" or "Report only"), flexible space, size 13/400 tnum `text_2`, and a 60 pt trailing area holding the hover actions (Reveal `FolderSearch`, more `Ellipsis`, 26 pt icon buttons). Hover fill `row_hover`, radius 8. Disabled (blocked) rows show the checkbox at 40% opacity.
- Report only rows have no checkbox; a `Lock` icon 12 `text_3` takes its place.
- Toolbar controls: History (clock icon button), Rescan (`RefreshCw` icon button), primary "Clean 6.3 GB" (Haul's primary text button, 24 pt high).
- Empty state (before the first scan, or after a scan finds nothing): centered column, max width 360: icon 32 `text_3`, title 15/600, one line 13 `text_2`, the large primary button (28 pt, radius 7, 13/500, also used for Run Check up). Copy before the first scan: "Find space you can get back" / "Tidy looks at caches, old logs, old build output and package caches. Nothing moves until you choose, and everything goes to the Trash." / "Scan". After an empty result: "Nothing to clean right now" / "Caches of apps that are open aren't counted. Quit them and scan again." / "Scan Again".

**Large files list** (every row 52 pt):
- Toolbar controls, left to right: segmented "Biggest | Not opened lately" (hidden in Folders mode), segmented "Files | Folders" (a two-button group, 24 pt), threshold select "Over 500 MB", Rescan icon button. Files | Folders sits right of the filter so hiding the filter does not move the control just clicked. The selected segment is the secondary button flattened on the track, so it stands out in dark mode too. No primary button; the action lives in the status bar.
- Breadcrumb row (Folders mode only, 28 pt, padding 0 20 0 16, so the first segment's text lines up with the title at 20): segments 12/500 `text_2` separated by `ChevronRight` 12 `text_3`; the last segment in `text`; earlier segments are buttons.
- Row: padding 0 10, gap 10: checkbox 16, glyph tile 32 (radius 7, `fill_2`, icon 18 `text_2`), text column (name 13/500 `text` with the tags 8 pt after it on the same line; path 12 mono `text_2`, middle-truncated to the whole column width; in Folders mode the path line is replaced by the 4 pt share bar, 160 pt wide, `track` and `accent`), flexible space, the date phrase 12 `text_2` (fixed 150 pt column, right-aligned), size 13/500 tnum `text` (fixed 80 pt column, right-aligned), 60 pt hover actions (Reveal, Move to Trash `Trash2`). In Folders mode folders show a trailing `ChevronRight` instead of Move to Trash at rest.
- The "smaller items" row: no checkbox, name in `text_2`, no actions.
- Status bar: left "3 selected, 12.4 GB" or "48 items, up to 214 GB"; right "Move to Trash…" text button, disabled with no selection.
- Scanning: the toolbar shows a spinner and "Scanning, 214,000 files" in 12 tnum `text_2`; a Stop text button cancels. The previous result stays visible, dimmed to 50%, until the new one arrives.

**Dialogs**: gpui-component dialog, 420 pt wide, title 13/600, body 13/400 with the item list in a scrollable box up to 200 pt high (12 tnum rows: name left, size right), buttons right-aligned: Cancel (secondary) and the action (primary; Empty Trash uses `danger_button`). Cancel has the keyboard focus when the dialog opens, Space or Return presses the focused button (so Return alone cancels), Escape cancels, and Tab stays inside the dialog (section 11.y).

**History sheet**: 560 x 480 pt, rows 44 pt: date 13/500, action and counts 12 `text_2`, size right; expanded items are 32 pt rows with mono paths.

**Motion**: Haul's existing motions, plus two state changes. Row fade-out 140 ms when an item is trashed (send first, fade on `ItemTrashed`; restore the row with its skip reason on `ItemSkipped`). Notice band in 160 ms, out 200 ms. Indeterminate spinner while scanning. The reminder switch knob slides to its side over 160 ms (`enter` curve) after a click, and a Check up card fades its result in over 200 ms when its part reports; both play only within 400 ms of the change (`CHANGE_GATE`), so a window that opens on a settled state shows it still, and both are instant under Reduce Motion. Nothing counts up, nothing celebrates; the one exception is the disk bar moving after a clean (section 20, C61). Overlays (dialogs, History, System Data, the details popover) come in and leave as section 18.3 describes (C44).

**Copy rules**: sentence case for text and labels, title case for buttons and menu items ("Open Network Settings", "Check Now", "Scan Home Folder"), plain words, sizes with one decimal above 1 GB ("6.3 GB", "640 MB"), dates as relative phrases ("8 months ago"), no exclamation marks. A button or menu item that opens a confirm ends in an ellipsis character ("Empty Trash…", "Move to Trash…", "Force Quit…"), and middle-truncated paths use it too (`~/Movies/2025/…/trip.mov`).

### 11.x Preflight look (2026-10-25)

The look replaces Haul v2's grays and blue accent. Tokens (light / dark): ground `#FAFAF8` / `#0B0C0B`, sidebar and status bar `#F0F0EC` / `#131412`, ink `#0D0E0C` / `#F4F4F0`, `text_2` `#555852` / `#A2A49E`, line `#DADAD4` / `#2A2C28`, band fill `#EFEFEB` / `#1A1B19`, ok `#0B7550` / `#46D39A`, amber `#A64B00` / `#F2A03D`, quiet dot `#CFCFC8` / `#3A3C37`. `accent` is the ink, so primary buttons, checked boxes, the segmented selection and the selected sidebar row are ink fills with `on_accent` text. Display type is Avenir Next (800 for the Cleanup Figure, 700 for headings, sizes and buttons, 600 for the sidebar and row names); body text stays in the system font. The sidebar shows labels only.

Cleanup shows the Figure at 56 pt in the header with the Clean button at its right, group headers as gray bands, and a row of check dots before each size. Each dot is one guard from `tidy_core::explain` for that row's rule (`Explanation::guards`), so N differs by rule (5 for a plain cache, more for build output) and is never fixed. A guard fails from real state: a running owner fails "Skipped while ... is running" (`Item::blocked`), a skipped move fails "Checked again right before the move". Report only rows have no guards and no dots. When every guard passes, all dots are the quiet gray and "N of N" is `text_2`; a row with a failed guard draws passed dots green, failed dots as rings, and "k of N" in amber. The count is always text, and the details popover repeats it as "k of N checks passed" next to the guard list (check marks, amber crosses for failures), then If removed, Rule and Path.

The Move to Trash confirmations (Cleanup with a Review item ticked, Large files) are receipts: a line per category (per item on Large files) with a dot leader, a double rule, the total at 34 pt, and "Nothing is deleted. Every item goes to the Trash and Tidy writes it in History, so you can put it back." The zigzag bottom edge of the mockup is left out because gpui has no clean way to clip a div to a path.

Contrast: every text and background pair is at least 4.5:1 and display text at least 3:1 in both sets; `theme::tests::documented_contrast_ratios_hold` computes them and fails below the bar. gpui exposes no accessibility label for a div, so the row's meaning is carried by the "k of N" text, never by the dots. The focus ring is specified in 11.y.

### 11.y Keyboard access (2026-10-25)

gpui-pre 0.3.9 has what this needs, so Tidy adds no focus manager of its own: `FocusHandle` with `tab_stop` and `tab_index`, `track_focus` and `tab_index` on a div (an element with an id and `tab_index` gets a handle kept in its element state), `Window::focus_next` and `focus_prev`, Space and Return mapped to a click on release, `on_action` with `key_context`, `focus` and `focus_visible` styles, `on_focus_lost`, and, from gpui-base, a Tab/Shift-Tab binding plus `focus_trap`. What gpui lacks, Tidy adds:

- **Order.** Every control is a tab stop at index 0 (`views::KeyStop`), so the order is the drawing order: sidebar rows, toolbar (segments, threshold, buttons), list rows, status bar, then any overlay. A Cleanup row is checkbox, Details, Reveal, more; a Large files row is checkbox, Reveal, Move to Trash or Open. Disabled controls are not stops. The sidebar has no "current section" jump; each of the five rows is a stop.
- **Row buttons.** They exist for every row but are drawn only while the pointer or the keyboard is on the row (`RowFocus`, one non-stop handle per row). They stay in the tab order while hidden, so Shift-Tab reaches them too.
- **Long lists.** `uniform_list` draws only the rows in view. When Tab or Shift-Tab leaves such a list and rows remain in that direction, `TidyView::move_focus` keeps the focus, scrolls one row and retries after the next frame (at most 40 rows per press).
- **Ring.** `Theme::focus_shadow`: a 2 pt ring in `text` with a 2 pt gap painted in the surface color the control sits on (`Theme::on`), so it also shows on ink buttons and on the selected sidebar row. Ink on every surface Tidy uses is above 3:1 (`documented_pairs` holds it in both sets). The ring shows after a key press and hides after a mouse press (`theme::ring_visible`), not on mouse moves as gpui's `focus_visible` does. No motion: the ring appears at once (motion gate: a Tab press is a frequent action, so it fails the frequency question).
- **Overlays.** A dialog, History, System Data, the details popover and the three small menus each trap Tab (`focus_trap`) and take the focus when they open: Cancel for a dialog, the overlay itself for the rest, so the first Tab lands on its first control. When one closes, the focus returns to what had it. If the focused control leaves the screen, `on_focus_lost` hands the focus to the nearest ancestor still drawn (or the window), so Cmd-1 to Cmd-5 and Cmd-R keep working.
- **Return.** No longer a global confirm. It presses the focused control, and a dialog starts on Cancel.
- **Screen readers.** Icon buttons carry a role and a label, checkboxes and switches a toggled state, sidebar rows and segments a selected state. Text buttons and rows rely on their text.

## 12. Packaging

Copy Haul's scripts and rename:

- `resources/Info.plist.in`: `CFBundleExecutable` `tidy`, `CFBundleName` and `CFBundleDisplayName` Tidy, `@BUNDLE_ID@`, `@VERSION@`, `@BUILD@`, `LSMinimumSystemVersion` 14.0, `NSHighResolutionCapable`, `NSSupportsAutomaticTermination` and `NSSupportsSuddenTermination` false, the three folder usage descriptions. No URL schemes, no document types, no local network string.
- `scripts/bundle.sh [--release] [--install]`: builds `-p tidy-app`, outputs `target/bundle/Tidy.app`, refuses any output path that does not end in `bundle/Tidy.app`, signs with `SIGN_ID` (default: the same Apple Development certificate hash Haul uses), `--timestamp=none`, no hardened runtime, no entitlements, no `--deep`. Env: `TIDY_BUNDLE_BIN`, `TIDY_BUNDLE_ID` (default `dev.sdusteric.tidy`), `INSTALL_DIR`, `CARGO_TARGET_DIR`. Never launches the app. Icon from `resources/AppIcon.png` or `scripts/make-icon.swift`.
- `scripts/shots-bundle.sh`: `--features shot-export` into `target/shots-build`, bundle id `dev.sdusteric.tidy.shots`.
- `scripts/shots.sh [scenario ...]`: default scenarios `cleanup large`, each in light and dark, written to `docs/app-shots/<scenario>-<appearance>.png` (`OUT` overrides). Uses `scripts/winid.swift` only in non-render mode. Checks the binary contains `TIDY_DEMO` and `TIDY_DEMO_SHOT`.
- `scripts/accept.sh offline` and `scripts/accept-lib.sh`: M1 acceptance (section 13), bash 3.2, run directory under `target/accept/<timestamp>`, `remove_run_path` limited to `target/accept/`, `clear_tidy_env` unsets every `TIDY_*` variable before each step. No `net`, `headful` or `gui` group in M1 (C26).

## 13. Milestones

### M1: workspace, engine, Cleanup and Large files

Scope: the workspace and shared types; the scan engine (walker, rules, cleanup plan, large files, process probe, last-use dates); Trash, clean executor and history; tidyctl; the app shell with the sidebar, the Cleanup and Large files screens, History sheet and dialogs; demo scenarios and offscreen screenshots; bundle and shots scripts; the offline acceptance script.

M1 acceptance criteria:

1. **Gates green**: `cargo build --workspace --all-targets && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace --exclude tidy-app && cargo test -p tidy-app -- --test-threads=1` exits 0.
2. **Cleanup scan is exact**: after `tidy-fixture-home $D`, running `TIDY_HOME=$D/tidy TIDY_SCAN_ROOT=$D/home TIDY_TEST_NOW=<printed> TIDY_TEST_RUNNING=com.google.Chrome tidyctl scan` prints JSON lines whose final `plan` matches `$D/expected-plan.json` exactly (ids, allocated bytes, class, ticked, blocked reason), with no excluded or denylisted path anywhere in the output.
3. **Clean moves exactly the safe set**: `tidyctl clean --safe` with the same environment exits 0; the set of paths moved into `$D/tidy/data/trash` equals the ticked Safe items in the expected plan; a before and after listing of `$D/home` differs only by those paths; `$D/tidy/data/history.jsonl` has one `clean` entry with `"trasher":"dir"` listing each item with its bytes; the same command without `TIDY_HOME` (and `HOME` set to a temp dir) exits 5 and moves nothing.
4. **Large files is exact**: `tidyctl large --min-mb 4` on the fixture lists exactly the items in `expected-large.json` (`Big.app` as one item, the sparse file absent, the hard-linked pair counted once, nothing reached through the symlink, nothing from `Library/Containers`); `--not-opened` lists only the item dated 200 days back; `--folders` reports the expected folder totals.
5. **Bundle and background screenshots**: `scripts/bundle.sh --release && codesign --verify --strict --verbose=2 target/bundle/Tidy.app && plutil -lint target/bundle/Tidy.app/Contents/Info.plist && target/bundle/Tidy.app/Contents/MacOS/tidy --self-test` exits 0, and `scripts/shots-bundle.sh && SHOTS_RENDER=1 scripts/shots.sh cleanup large` writes `cleanup-light.png`, `cleanup-dark.png`, `large-light.png`, `large-dark.png` at 1800 x 1240 px without changing the frontmost app. `scripts/accept.sh offline` runs criteria 1 to 5 and prints PASS for each.

### M2: Memory, Security, Check up and reminder

Outline (a separate plan will detail it):

- `tidy-core::memory`: pressure, swap, compressed, page stats via `sysctlbyname` and `host_statistics64` (`mach2`); per-process footprint via `proc_listpids` and `proc_pid_rusage`; grouping via `dlsym` of the responsibility function with fallbacks; `MemoryProbe` seam with a fake for tests.
- App side `NSRunningApplication` for names, icons, `terminate`, `isTerminated` polling, Force Quit with confirm. Memory screen per 5.3, refreshed every 2 s while visible.
- `tidy-core::security`: each check behind a `CommandRunner` seam (fake outputs captured from this Mac as fixtures), launch agent and daemon plist parsing, `SecStaticCode` and `SecAssessment` signature checks, Chrome `Secure Preferences` parsing, weekly result files and the diff that marks "New" and "Changed". Security screen per 5.4 with deep links opened from clicks only.
- Check up screen per 5.5, `checkups.jsonl`, the opt-in weekly reminder through `UNUserNotificationCenter` with a single rescheduled trigger.
- tidyctl: `memory`, `security [--include-updates]`, `checkup`.
- Demo scenarios and screenshots for the three screens; acceptance criteria for parsing (fixture outputs), the diff, grouping on fake process tables, and the reminder schedule math with `FixedClock`.

## 14. Risks

- **Emptying the Trash without Full Disk Access** may fail with EPERM even for paths Tidy recorded. This can only be tested by the user on the real Trash, so it is a manual check after M1 ships; the fallback message is already designed.
- **getattrlistbulk parsing** is unsafe code with packed buffers. The std walker reference and equality tests on every fixture are the guard.
- **Exact sizes in fixtures** assume APFS and no transparent compression on plain writes. If a fixture run sees allocated bytes differ, the fixture writer must switch to incompressible data and the test must compare against `st_blocks * 512` read back after writing.
- **Owner resolution** for cache folders whose names are neither bundle ids nor in the static table will push items to Review. That is safe but may make the safe total look small at first; the table grows as needed.
- **TCC prompts**: the first Large files scan can show three prompts in a row. Acceptable, since it follows a click and happens once.
- **Trash clutter**: cleaning old log files moves many small files into the Trash one by one. It is recoverable and Empty Trash handles it, but Finder's Trash window may look busy.
- **gpui pins**: `render_to_image` and the lockfile depend on `gpui-pre =0.3.9`. Do not bump gpui in M1.
- **`uniform_list` fixed heights** force group headers and items to share 44 pt in Cleanup.
- **Spotlight last-use dates** are often missing for plain files and sometimes stale; the UI labels the fallback honestly.
- **Notification identity** (M2) depends on the stable signature; ad-hoc builds would lose the permission.

## 15. Calls made for the user

Decided before this document (by the planning agent, on the user's instruction to use the recommended option): the name Tidy, bundle id `dev.sdusteric.tidy`, the four crates and `tidyctl`, Haul v2's visual language in light and dark, Trash for every removal with a history entry per clean, Empty Trash as a separate confirmed action, the five cleanup groups, Safe ticked and Review unticked, running-app caches shown unticked with "Quit <app> first", one Large files list with the Biggest and Not opened lately filter, a 500 MB default threshold, a Folders mode, bundles as one item, Reveal and Move to Trash, home folder scope that skips other apps' protected data, the app scanning Documents, Desktop and Downloads while tests and agents never do, no Full Disk Access, no root, no helper, and the M2 contents of Memory, Security and Check up.

Made in this document:

- C1. The sidebar shows only working screens; M2 screens appear in M2.
- C2. Closing the window keeps the app running, like Haul.
- C3. No automatic Cleanup scan until the user has clicked Scan once, so privacy prompts only follow a click.
- C4. Scan results appear when the scan finishes, not row by row.
- C5. "AI models and large app data" starts collapsed.
- C6. Blocked items re-enable themselves when the owner app quits (process re-check every 3 s while visible).
- C7. Cleaning only Safe items needs no confirm dialog; any ticked Review item adds one.
- C8. Empty Trash deletes only items Tidy moved there, using recorded Trash paths.
- C9. No Put Back button in M1; Finder's Put Back works.
- C10. Large files includes `~/Library` (minus the denylist) with an "App data" tag, so app model folders are visible.
- C11. Large files does not rescan on its own after the first scan.
- C12. "Not opened lately" means 90 days, with the same size threshold as Biggest.
- C13. The large scan records files of 50 MB or more so threshold changes need no rescan.
- C14. Every Large files move asks first, even for one item.
- C15. Totals read "up to" and a clone note shows for selections over 1 GB, since clones cannot be detected.
- C16. The weekly reminder is a single trigger rescheduled 7 days after the last check up, so it never repeats while ignored.
- C17. Caches of known apps that are not running are Safe with no age filter, and the whole cache folder goes to the Trash.
- C18. Simulator devices are Report only in M1, with the `simctl` command shown.
- C19. `~/.m2/repository` is Review, not Safe, because it can hold locally installed artifacts.
- C20. Running processes come from libc (`proc_listpids`, `proc_pidpath`), not NSWorkspace, so the CLI and the app see the same list.
- C21. Default project roots: `~/Documents/projects`, `~/Developer`, `~/Projects`, `~/src`, `~/code`.
- C22. Under `TIDY_HOME`, last-use dates come from file times only.
- C23. Destructive tidyctl commands refuse the real home unless `--real` is passed.
- C24. Four new tokens (`sidebar_bg`, `sidebar_selected`, `tag_border`, `danger_button`) and one new type role (Figure).
- C25. Free space uses "available for important usage", matching Finder.
- C26. M1 acceptance has only the offline group; no GUI launch group.
- C27. Every Cleanup row, Safe ones included, opens a details popover (section 17.1). Its text comes from the rule table only: the existing reason, plus a `rebuild` sentence and, for Report only rows, a `command` per rule.
- C28. "System Data on this Mac" is a sheet with History's chrome, opened from a Check up row and from a link under the disk figure in the sidebar. It measures only when opened (section 17.2).
- C29. The sheet never says Tidy will shrink Apple's System Data figure. It names what Tidy can measure, says "Not measured" or "Needs Full Disk Access" where it cannot, and gives the rest one row with no number.
- C30. Docker and local Time Machine snapshots are Report only rows of that sheet, each with a plain explanation and a Copy button for its command. Tidy runs only the read-only `tmutil listlocalsnapshots /`; `SystemRunner` refuses `docker` always, every other `tmutil` verb always, and `tmutil` entirely under `TIDY_HOME` and in tests.
- C31. Check up ends with a "What Tidy never does" card of six items (section 17.3).
- C32. `Service Worker/ScriptCache` of Chromium and Electron apps is Review (`cache.electron.scripts`), not Safe: removing it can leave extensions broken until they are turned off and on again.
- C33. A project guard keeps build output that holds a `*-keypair.json`, a nested `.git` or a Git-tracked file out of the plan and out of every move; Tidy reads `.git/index` itself and treats an unreadable index as "tracked".
- C34. Right before each move (Clean and Large files alike) Tidy reads the open files of the user's own processes with libproc, no sudo, leaving its own process out. A path a process holds open, or holds something open inside, is skipped with "Open in <name>, skipped". When the table cannot be read, every move is skipped.
- C35. A process list that cannot be read means "unknown": scan rows with an owner are blocked with "Tidy couldn't check whether <app> is open", and every move is skipped.
- C36. The ignore list is read from settings.json before each move, together with the copy in memory, on every path to the Trash. A path that is ignored, inside an ignored path, or holds one is skipped. A file that cannot be read or parsed stops the run, and the history entry says so.
- C37. The data folders of AI coding tools are Excluded whole: `.claude`, `.claude.json`, `.codex`, `.cursor` and Cursor's `User` folder. Their caches are not worth the risk to conversations and memory.
- C38. More tool caches, each with its reason and "If removed" line (C27): Go, gopls, golangci-lint, Composer, TypeScript, Deno and Black by owner name; Poetry (downloads Safe, environments Review); JetBrains `caches` and `index` only; gem, Bundler, pyenv and rbenv download caches; puppeteer, selenium and Electron downloads as Review; the Go module cache as Report only. The generic cache rule leaves `pypoetry`, `JetBrains` and `electron` to these rules. Not added: name-based orphan detection, container caches, anything inside a tool's cache, emptying the Trash.
- C39. "Old installers" is its own group: `.dmg` and `.pkg` files directly in Downloads, older than 30 days, always Review. The rule reads Downloads only after the first Large files scan, so an automatic Cleanup rescan never brings up the Downloads privacy prompt.
- C40. Each Cleanup rule has a 30 second budget per scan; one that runs out is reported in the plan's `skipped` lines, which the Cleanup screen now shows in its notice band. A walk stopped part way is never used for a date or a size.
- C41. Relaunch on the Memory screen (18.2).
- C42. The browser memory tip (18.2).
- C43. The "What would help" line (18.2).
- C44. One enter and exit for the overlays, the popover settle and two content fades (18.3).
- C45. Demo scenarios `memory-relaunch` and `memory-critical`; the Cleanup demo has two old installers.

## 16. As-built: M2 (Memory, Security, Check up and reminder)

This section records what was built for M2 and where it differs from sections 5.3 to 5.5. Sections 5.3 to 5.5 and 11 stay the contract.

**Acceptance.** `scripts/accept.sh offline` prints PASS or FAIL for AC1 to AC9. AC1 to AC5 are the M1 criteria, unchanged. The M2 criteria are:

| Id | Checks |
|---|---|
| AC6 Memory | Core tests on a fake process table (grouping under the responsible app, the parent-chain and `.app` fallbacks, one System processes row, no Quit for Tidy, Finder, WindowServer, loginwindow and kernel_task); app tests with a fake terminator (polite terminate, "Quit requested", "Still open" after 10 s, Force Quit behind a confirm, polling every 2 s only while visible); no test names a real terminator, notifier or signal. On this Mac, `tidyctl memory` exits 0 in under 3 s and its pressure level (exact), swap used and compressed bytes (within 10%) agree with `sysctl` and `vm_stat` taken right after, and the pids that `top -l 1 -o mem -n 5` lists are all in some row. |
| AC7 Security | Core tests that parse captured outputs of `fdesetup`, `socketfilterfw`, `spctl` and `csrutil` (on and off), XProtect amber after 60 days with a `FixedClock`, launch agent and daemon fixture folders, Apple-signed `/bin/ls` against an unsigned binary the test compiles, Chrome Secure Preferences, the New and Changed diff and the first-run sentence, and `softwareupdate` not running without `--include-updates`; no scare word in any string literal of `crates/*/src`. On this Mac, `TIDY_HOME=<temp> tidyctl security` exits 0 in under 20 s, lists the eight checks of 5.4 with macOS updates gray and not checked, never starts `softwareupdate` (polled with `pgrep`) and leaves the frontmost app unchanged. |
| AC8 Check up | `tidyctl checkup` on the fixture home exits 0, prints the plan, security and memory lines and a final `checkup` line with the cleanup total, the security summary and the memory verdict, and appends one entry to `data/checkups.jsonl` (a second run appends a second). The reminder is 10:00 on the day 7 days later. Reminder math tests (`FixedClock`, both DST changes, rescheduling after a check up and on launch) and app tests with a fake notifier (permission only when the toggle goes on, never at launch, the Notifications settings button when denied). |
| AC9 App | The sidebar lists Check up, Cleanup, Large files, Memory, Security with Cmd-1 to Cmd-5; long paths in Large files are fitted to the row with an ellipsis; `scripts/shots-bundle.sh && SHOTS_RENDER=1 scripts/shots.sh checkup memory security cleanup large` writes ten distinct PNGs at 1800 x 1240 without changing the frontmost app. |

A full run has FAIL 0 and SKIP 0. `ACCEPT_SKIP_GATES=1` and `ACCEPT_SKIP_BUNDLE=1` still skip AC1, and AC5 with the shots of AC9; those runs are for development only.

**tidy-core.**
- `memory`: `MemoryProbe` seam (`LibcMemory` in production, `FixedMemory` for tests and `TIDY_TEST_MEMORY`). `group` is pure. Pressure is `kern.memorystatus_vm_pressure_level` (1 normal, 2 warning, 4 critical), swap is `vm.swapusage`, and Compressed is "Pages occupied by compressor" times the page size, the same figure `vm_stat` prints.
- `security`: `system` (the status commands and `softwareupdate --list`), `launchd` (plist parsing, XML and binary), `codesign` (signer, notarized), `chrome` (Secure Preferences), `diff` (the marks against the last check), `cf` (Core Foundation helpers). Seam: `CommandRunner` (`SystemRunner`, `ReplayRunner` for `TIDY_TEST_COMMANDS`). `SystemRunner` refuses `softwareupdate` under `TIDY_HOME` and in tests.
- `checkup` and `reminder`: the check up log and the reminder date math (`TIDY_TEST_TZ` fixes the zone).
- Commands: `SnapshotMemory`, `RunSecurity { include_updates }`, `LastSecurity`, `RunCheckup { include_updates }`, `LastCheckup`, `NextReminder`. Events: `MemorySnapshot`, `SecurityProgress`, `SecurityResult`, `CheckupDone`.
- Settings version 2 adds `reminder_enabled` and `include_updates`, both off.
- Files in the data folder: `security/<id>.json`, one per run, named for its UTC time (for example `20261023-140211.json`), and `checkups.jsonl`. Each run marks New and Changed against the run just before it, whenever that was, and the summary line says when ("New and changed since your check 2 days ago"). The newest 100 runs are kept. Security runs only when asked: the Run Check and Rescan buttons, a check up, or `tidyctl`. (Changed 2026-10-24 at the user's request; M2 first saved one file per ISO week.)

**tidy-app.** `views/{checkup, memory, security}`; `macos.rs` holds the `Terminator` (`NSRunningApplication` terminate and forceTerminate) and `Notifier` (`UNUserNotificationCenter`) traits with `SystemTerminator`, `SystemNotifier` and the fakes the tests use. The app tests never construct the system ones.

**tidyctl.** `memory`, `security [--include-updates]` and `checkup [--include-updates]`, with the events above (`memory`, `security_progress`, `security`, `plan`, then `checkup` as the last line). Exit 3 on `checkup` means a part could not be done.

**Demo scenarios.** `checkup`, `memory`, `memory-tight`, `security`, `security-first`, `security-items` next to the M1 ones. `scripts/shots.sh` takes any of them and by default shoots the five screens in sidebar order.

**Differences from the text above.**
- The System processes row gets its size from `top`. macOS does not let an app read the footprint of another user's process: `proc_pid_rusage`, `PROC_PIDTASKINFO`, `PROC_PIDTASKALLINFO` and `memorystatus_control` all fail with EPERM. An estimate from the `vm_stat` page counts does not work either, because the readable footprints alone already add up to more than the anonymous and compressed pages. `/usr/bin/top` ships with macOS setuid root, so the memory probe runs `top -l 1 -s 0 -F -stats pid,mem` (read-only) and takes its MEM column, the footprint `top` and Activity Monitor show, for each process it cannot read itself. A sample costs about a second of CPU, so it is taken at most every 20 seconds: the first snapshot waits for it (`tidyctl memory` stays well under 3 s), later ones use the last sample and start a new one in the background. The System row sums those sizes and takes its place among the top rows by size, above "Other". It names its processes (`members`: WindowServer and kernel_task first, then the largest), and the note reads "WindowServer, kernel_task and more." When `top` cannot run or times out (5 s), the row has no size, sorts last, and the note adds "Tidy can't read their memory." The owner and parent of those processes come from `PROC_PIDT_SHORTBSDINFO`, which macOS answers for every user.
- `spctl --status` prints "assessments disabled" and exits 1. The Gatekeeper check takes exit 1 as an answer; every other status command answers only with exit 0.
- A check up sends its events through the core actor, so the actor holds the Cleanup plan before the app shows it, and a Cleanup scan asked for while the check up is scanning waits for the check up's plan instead of starting a second scan.
- The Check up screen has its own primary button in the content area ("Run Check up"), not in the toolbar.

## 17. Trust surfaces (design note, 2026-10-25)

From the user research (`reddit/tidy.md`, gap rows 3, 5, 6, 11 and 18): people trust a cleaner that says why each item is safe, explains System Data without promising to shrink it, and states what it never does. These three surfaces add no deletion path, need no Full Disk Access, sudo or background agent, and reuse section 11's tokens (`card_bg`, `pop_bg`, `pop_shadow`, `fill`, `text`, `text_2`, `text_3`, `tag_border`, `accent`), type roles and motion. Nothing new animated when they shipped; section 18.3 later added their enter and exit.

### 17.1 Item details (C27)

- **Open.** A fourth hover action on every Cleanup item row, `Info` 16 in the 26 pt icon button, left of Reveal; the trailing area grows from 60 to 86 pt (group totals keep lining up with item sizes). The "..." menu gets "Show Details" as its first entry. Both open the same popover at the click, drawn like the row menu (`pop_bg`, `pop_shadow`, radius 10), 420 pt wide, its top right corner 14 pt under the click, snapped 8 pt inside the window. A click outside or Escape closes it. It is a popover, not a modal: the list stays live and nothing is asked.
- **Content**, top to bottom, padding 14:
  - Name 13/600 `text` with the row's tag after it (Review, Report only), the size 13 tnum `text_2` at the right.
  - The verdict, 13/400 `text`, one or two lines: Safe "Safe to clean. <reason>", Review "Worth a look before you clean it. <reason>", Report only "Tidy won't remove this. <reason>".
  - A hairline, then a two-column list like Finder's Get Info: labels 12 `text_2` in an 80 pt column, values 12 `text` (the rule id in mono `text`, patterns and the path in mono `text_2`; 420 pt keeps a 40 character pattern on one line). Rows: **If removed** (the rule's `rebuild` sentence), **Checks** (one line each behind a `Check` 11 in `text_3`, built from the rule: the owner not running, the age rule, the newest entry kept, the 24 hour skip, and always "Checked again right before the move" and "Goes to the Trash, so you can put it back"; Report only rows say "Tidy never moves it"), **Rule** (rule id in mono, then the matched pattern such as `Library/Caches/<app>`), **Path** (the item's path from `~`, middle-truncated).
  - Report only rows with a command add a command box: `fill` background, radius 6, the command in 12 mono `text`, and a secondary "Copy" text button at the right. Copying writes the clipboard only.
- **Model.** `tidy_core::explain::explain(&Item) -> Explanation` is pure and tested; the popover only draws it. An item whose rule id is unknown (a demo row) still gets its reason and path.

### 17.2 System Data on this Mac (C28, C29, C30)

- **Open.** On Check up, a row in the settings-row style under Automatic updates: `HardDrive` 16 `text_2`, title "What is System Data on this Mac?" 13/500, note "See what Tidy can measure without Full Disk Access." 12 `text_2`, a `ChevronRight` 12 `text_3`; the whole row is clickable and hovers to `card_hover`. In the sidebar, a plain text button "What's in System Data?" under the disk line.
- **Sheet.** History's chrome at 600 x 520 pt: 44 pt header with the title "System Data on this Mac" and a close button, a hairline, then a scrolling body with 16 pt side padding.
  - Lead paragraph, 13 `text`, across the sheet (two lines): "System Settings counts caches, logs, developer files, virtual machines and snapshots as System Data. Here is the part Tidy can measure without Full Disk Access."
  - The figure line: Figure 22/600 tnum with " measured" in 15 `text_2`, and one 12 `text_2` line ("Measured just now"). While measuring: the spinner and "Measuring" in the toolbar style.
  - Rows, 52 pt where single-line and taller when the note wraps, separated by hairlines: glyph tile 28 (`fill_2`, icon 16), title 13/500 with the tag "Report only" on the report rows, a note 12 `text_2` that says what it is and what to do, and the size 13 tnum `text_2` at the right, or "Not measured" / "Needs Full Disk Access" / "None right now" in 12 `text_2`. Report rows carry one command box each (as 17.1).
  - Rows, in order: Caches (`Library/Caches`), Logs (`Library/Logs`), Xcode build data (DerivedData, Archives, device support, Previews), Simulators (`Library/Developer/CoreSimulator`, command `xcrun simctl delete unavailable`), Virtual machines (`.colima`, `.lima`, `.android/avd`, Claude's `vm_bundles`), iPhone and iPad backups (`Library/Application Support/MobileSync/Backup`, "Needs Full Disk Access" when it cannot be read), Docker (Report only, shown when `.docker` exists; its disk image lives in Docker's app container, which Tidy never opens, so it is not measured; commands `docker system df` and `docker system prune`), Local Time Machine snapshots (Report only; the count and the newest date from `tmutil listlocalsnapshots /`; macOS does not report their size; command `tmutil listlocalsnapshots /`). Rows with nothing found are left out, except the snapshots row.
  - Last row, no size, `Lock` glyph: "Everything else" / "macOS itself, other apps' private data, and Mail, Messages and Photos libraries. Tidy can't measure these without Full Disk Access, and it doesn't ask for it."
  - Footer, 12 `text_2`: "Moving things to the Trash frees disk space, but macOS computes System Data its own way, so that figure may not drop by the same amount. Caches also grow back as apps use them."
- **Measuring** is read-only (`walk::measure` with the denylist on) and runs once per opening, on a click, never at launch or in a check up. `tidyctl system-data` prints the same report.

### 17.3 What Tidy never does (C31)

- The last block on Check up, after the System Data row: the heading "What Tidy never does" 13/600 `text` 20 pt from the row above, then one `card_bg` card (radius 10, padding 14) holding a two-column grid of six items, 12 pt column gap, 10 pt row gap. Each item: a `Ban` icon 14 `text_3`, a title 13/500 `text`, and a note 12 `text_2`.
- Items: "Ask for Full Disk Access" / "It works with what macOS gives every app."; "Run in the background" / "Nothing starts at login or runs after you quit."; "Use sudo or ask for your password" / "No helper tools and no admin rights."; "Delete outside the Trash" / "Items go to the Trash. You can put them back until you empty it."; "Empty the Trash on its own" / "Emptying it is a separate step you confirm."; "Use scare tactics" / "No red badges, countdowns or alarm words."
- Demo scenarios `cleanup-why`, `system-data`, `system-data-report` (the sheet scrolled to Docker and the snapshots) and `checkup-trust` (Check up scrolled to the end) cover the three surfaces in the screenshots.

## 18. Mole lessons, more coverage, Relaunch and overlay motion (design note, 2026-10-25)

From the review of Mole, a GPL-3.0 cleaner (`reddit/mole.md`): its worst incidents came from caches deleted while something still used them, a whitelist that one command never read, keys and repositories inside build output, and AI tool histories. Tidy takes the ideas and the well-known paths only; every rule here is written from the tool's own documentation and checked against a fixture home. No code, rule list or wording comes from Mole.

### 18.1 Safety (C32 to C37, C40)

- The last gate before a move (`clean.rs`, `MoveGate`) is shared by Clean and Move to Trash: the ignore list read fresh (fail closed), then the open files of the user's processes (libproc `PROC_PIDLISTFDS` and `PROC_PIDFDVNODEPATHINFO`, fail closed). It runs after the path guard and the rule's re-check.
- The project guard (`cleanup.rs`, `Guard`) runs in the scan's date walk and again, from scratch, at the move. The scan also drops folders that hold a denied name (`trash::holds_denied_name`, the check the move makes), so nothing is offered that the move would refuse for that reason.
- A new seam, `OpenFilesProbe` (`LibcOpenFiles` in production, `FixedOpenFiles` in tests), joins the others in `env.rs`. The conservative default seams make every move skip.
- Skip reasons: "On your ignore list", "Tidy couldn't read your ignore list, skipped", "Open in <name>, skipped", "Tidy couldn't check which files are open, skipped", "Tidy couldn't read which apps are open, skipped", "Holds a keypair file, skipped", "Holds a Git repository, skipped", "Git tracks files inside it, skipped", "Tidy couldn't read the project's Git index, skipped".
- The details popover lists two more checks: "Skipped while a program has a file open inside it" on every item Tidy may move, and "Never when it holds a keypair, a Git repository or files Git tracks" on build output.

### 18.2 Memory screen (C41 to C43)

- **Relaunch.** A secondary text button left of Quit, on rows of real apps that may be quit (an `.app` bundle; never the System row, plain processes, Finder, WindowServer, loginwindow or Tidy). The action column grows from 112 to 168 pt and the "Memory in use" label moves with it. A press reads the app's `bundleURL` from `NSRunningApplication`, sends the polite `terminate`, and looks every 250 ms for up to 10 seconds. Once the app has exited, Tidy opens the same bundle with `NSWorkspace openApplicationAtURL` without bringing it to the front, so the user keeps their place in Tidy. It never force quits. Row lines: "Quitting, then opening again" while it waits; "Opened again" for 4 seconds; "Didn't quit in 10 seconds, so Tidy left it open." (warning color, both buttons back, no Force Quit); "Quit, but macOS didn't open it again. Open it from the Dock." (warning color). Without a bundle URL nothing is sent and the row says "macOS did not pass the request on." Tests use the fake `Terminator` (`bundle_path`, `open_app`); no test quits, relaunches or signals a real app.
- **Browser tip.** On Chrome, Edge, Brave and Arc rows (by bundle id) above 4 GB, the line under the name names the browser's own setting, after a 12 pt `Lightbulb` in `text_2`, in place of the helper count: Chrome "Memory Saver, in Settings > Performance, frees idle tabs."; Brave "Memory Saver, in Settings > System, frees idle tabs."; Edge "Sleeping tabs, in Settings > System and performance, free memory."; Arc "Archiving idle tabs sooner, in Settings > Profiles, frees memory." (Arc has no Memory Saver; it archives idle tabs on a per-profile timer). A Quit or Relaunch state takes the line's place while it lasts.
- **What would help.** While pressure is Warning or Critical, one line between the graph and "Top apps by memory": a 13 pt `Lightbulb`, "What would help" in 12/500 `text`, then the two largest rows that may be quit, in `text_2`: "Quit or relaunch Google Chrome (5.6 GB) or Xcode (4.3 GB)." It is plain text, not a card: the verdict and the Pressure figure already carry the color. With no app to name it says "Quit apps you aren't using." Nothing is purged and nothing asks for admin rights.

### 18.3 Overlay motion (C44)

From the animation gate (`motion/tidy.md`), all four rows:

- **Sheets** (confirm dialogs, History, System Data) share `overlay::present`. In: the scrim, which holds the card, fades from clear over `CARD_IN` 200 ms with the `enter` curve while the card settles up from 6 pt below its place. Out: the reverse over `NOTE_OUT` 200 ms with the `exit` curve, drawn from a leaving copy the view keeps (the dialog's spec, or the sheet drawn once more). The leaving copy takes no clicks and goes when its timer ends. Buttons, Return and Escape work from the first frame; reopening while a copy leaves drops the copy.
- **Why-safe popover.** In: opacity and a 4 pt settle toward the click over `NOTE_IN` 160 ms, `enter`. Out: opacity only over `REMOVE` 140 ms, `exit`, from an inert copy. Opening another row's details shows the new one at once and drops the old one with no exit.
- **System Data first measure.** When the first report of an opening arrives, the figure, the rows and the footer fade in over `CARD_IN`. A sheet reopened on a cached report, and a re-measure, show still.
- **History.** An entry that opens fades its item rows in over `NOTE_IN`; rows below move at once; closing is instant.
- Every motion is keyed by a fresh generation and plays only within `CHANGE_GATE` of the change, so a window that opens on a settled state, or a demo shot, shows it still. Under Reduce Motion nothing plays and nothing leaves through a copy.


## 19. Uninstaller (design note, 2026-10-31)

The user cancelled Setapp on 2026-10-26. The one CleanMyMac feature they still relied on was removing an app together with the files it leaves behind, and they asked for it on 2026-10-31. The brief: remove an app with its Application Support, Caches, Preferences, Containers and Group Containers, Saved Application State, Logs, HTTPStorages, WebKit data and launch agents, matched by bundle id and team; Trash only, never a permanent delete; never system apps, Apple apps or a running app; show what will move, with sizes, before anything moves. The calls below (C46 to C57) were made by the building agent without a question round, because the brief settled the behavior.

### 19.1 Screen (C46)

- **Sidebar.** A sixth row, "Uninstaller", after Security, Cmd-6, and the same item in the Go menu. Cmd-1 to Cmd-5 keep their screens. No trailing figure.
- **List.** Toolbar: the title, a segmented "Name | Size | Last opened" sort (Name by default), Rescan. Last opened puts what was not used first: apps with no open on record (Spotlight has no last-use date, so the row says "Modified"), oldest file date first, then the apps opened longest ago (added 2026-11-08 at the user's request). No summary strip, as Large files. Rows are 52 pt: the app icon 32 (read from the bundle a few per frame, an `AppWindow` glyph tile until it loads), the name 13/600 `text` with a tag, the meta line 12 `text_2` ("Version 4.41 · Zoom Video Communications, Inc. · Opened 7 months ago": version, signing team, last-use phrase; a Mac App Store app, signed by Apple's "Apple Mac OS Application Signing" certificate, says "App Store" in the team's place), the size 13/600 tnum `text` in the 80 pt column, and a trailing `ChevronRight` 12 `text_3`. The whole row is a key stop and a button (Space or Return opens it). Tag "Needs admin" on apps Tidy cannot move (19.4). Status bar: "12 apps, 9 GB · Tidy doesn't list Apple's apps or itself" at the left, "Listed 2 min ago" at the right.
- **Listing** runs when the screen opens with no list, or a list older than 10 minutes (`STALE_SCAN_SECS`), and on Rescan or Cmd-R. Reading `/Applications` asks macOS for nothing, so no click is needed first (C3 is about privacy prompts). The toolbar shows the spinner and "Listing apps" while it runs; the old list stays, dimmed.
- **Detail.** Opening a row shows that app on the same screen: the toolbar keeps the title and holds the primary "Uninstall…" (it opens the confirm, and is off while nothing is ticked, while the app is open, while Tidy is looking, and while an uninstall runs, when it reads "Moving 3 of 10"). Under it a breadcrumb row "All apps > Slack" (as Folders mode; Cmd-Up and "All apps" go back). Then a 64 pt header: the app icon 40, the name 20 display bold, the meta line of the list row under it, and at the right the ticked total in the Figure role ("706 MB") over "selected". Rows are 44 pt, as Cleanup items: checkbox (a `Lock` for Report only rows), glyph tile 28 by kind (the app icon for the app row), the kind label 13/600 (`Slack.app` for the app row) with an optional tag ("Name match", "Review", "Report only", "Needs admin"), the meta line 12 (the path from `~` in mono, middle-truncated to fit, or the row's note in `text_2`, or a skip reason in `warning`), the size 13 tnum `text_2` in a 90 pt column ("Size unknown" when unreadable), and Reveal on hover. Clicking a row ticks it. Status bar: "9 of 10 selected, 706 MB".
- **Looking.** Leftovers are found only when a row is opened (C48). While Tidy looks, the body says "Looking for its files" / "Tidy is looking for the files Slack left behind."; when they arrive the rows fade in over `CARD_IN` 200 ms (`enter` curve), keyed by the look and only within `CHANGE_GATE`, still under Reduce Motion. A late answer for an app that was closed is dropped.
- **Open app** (C54). When the app or one of its helpers runs, the header's meta line turns `warning` and says "Quit Slack to uninstall it. Tidy checks again every few seconds." and Uninstall is off. The process list is read every 3 seconds while the detail is visible (the Cleanup watch, `CheckRunning`).
- **Confirm** (C55). Always asked, like Large files (C14): the receipt dialog, titled "Uninstall Slack?", a line per item with its kind and size, the total, and "Nothing is deleted. Every item goes to the Trash and Tidy writes it in History, so you can put it back." When a launch agent is ticked it adds "Launch agents stop running after you log out or restart." Buttons: Cancel (focused), Move to Trash.
- **During and after.** Rows fade out over `REMOVE` 140 ms as each reaches the Trash (the existing motion); a skipped row stays with its reason in the meta line, in `warning`. When the app itself went to the Trash and nothing was skipped, the screen goes back to the list, the app's row is gone, and the band says "Moved 706 MB to the Trash. Empty the Trash to free the space." with Empty Trash… and Show History (the same band as Cleanup). Otherwise the detail stays, with the same band, or "Tidy moved nothing. Each row says why." when nothing moved.
- **Motion** (motion gate, `app-updates/docs/motion-gate.md`). One new motion: the rows' fade-in after a look (occasional, preventing a jarring change, 200 ms, an existing token). Rejected: a transition on the drill-down (list navigation, and Folders mode drills instantly) and moving rows when the sort changes (rows the user reads do not move for style). The trashed-row fade is `REMOVE`; the band and the receipt use their existing motion.

### 19.2 Which apps are listed (C47)

- Every `*.app` folder directly in `/Applications` and `~/Applications`, and one level down (`Utilities`, `Setapp`, `Chrome Apps.localized`). Under `TIDY_HOME` only `<scan root>/Applications` (the same folders `InstalledApps` reads). Links are not followed.
- Left out, and never offered anywhere in the uninstaller: a bundle id that starts with `com.apple.`, a bundle signed by Apple itself (`anchor apple`), Tidy (`dev.sdusteric.tidy` and the shots build), the app Tidy runs from, and a bundle with no `Info.plist` or no bundle id.
- Per app: name (`CFBundleDisplayName`, `CFBundleName`, file name), bundle id, `CFBundleShortVersionString`, the executable, the team id and team name from the code signature (none when unsigned), the allocated size of the bundle, the last-use date (Spotlight, else the bundle's file times, as Large files), the helper bundle ids (19.3), and whether Tidy can move it (19.4).

### 19.3 What counts as a leftover (C49)

All under `~/Library` (the scan root's `Library`), read only after a click. "Matches the app" means the entry's name, case-insensitively, equals one of the app's ids or starts with one of them followed by a dot. The ids are the app's bundle id and the bundle ids of its helpers: `Contents/PlugIns/*.appex`, `Contents/Library/LoginItems/*.app`, `Contents/Helpers/*.app`. An entry that also matches a longer id of another installed app belongs to that app instead, so uninstalling `com.google.Chrome` never takes `com.google.Chrome.canary`.

| Kind | Where | Matched by |
|---|---|---|
| Application Support | `Application Support/<entry>` | ids; or the app's name |
| Caches | `Caches/<entry>` | ids; or the app's name |
| Preferences | `Preferences/<entry>.plist`, `Preferences/ByHost/<entry>.plist` | ids (the host UUID is the part after the id) |
| Container | `Containers/<entry>` | ids |
| Shared container | `Group Containers/<entry>` | `<team id>.<rest>` or `group.<id>` |
| Application scripts | `Application Scripts/<entry>` | ids |
| Saved window state | `Saved Application State/<id>.savedState` | ids |
| Logs | `Logs/<entry>` | ids; or the app's name |
| Web data | `HTTPStorages/<entry>` (folder and `.binarycookies`) | ids |
| WebKit data | `WebKit/<entry>` | ids |
| Launch agent | `LaunchAgents/*.plist` | the file name or `Label` matches the ids, or `Program` / `ProgramArguments[0]` lies inside the app bundle (a path with `..` never does) |

- **Names.** Only Application Support, Caches and Logs folders match by name. A name match uses the display (or bundle) name, the file name and the executable, each at least 3 characters, never a vendor or framework word (`Google`, `Microsoft`, `Adobe`, `Apple`, `Mozilla`, `JetBrains`, `Electron`, `Python`, `Java`) and never a name another installed app (listed or not) or an app in `/System/Applications` also has. Name matches are ticked and carry the tag "Name match".
- **Ids.** An app or helper whose bundle id is empty or has no dot is neither listed nor matched (an empty id would match `.GlobalPreferences.plist`). Helper ids under `com.apple.` are dropped, and no entry whose name starts with `com.apple.` is ever matched.
- **A second copy.** When another installed bundle has one of the app's ids (`Slack 2.app`, or a direct download beside the App Store copy), every leftover is listed unticked with the tag "Review" and the note "Also used by Slack 2".
- **Shared containers.** A `<team id>.<rest>` container is ticked when no other installed app has that team id, or when `<rest>` matches the app's ids. Otherwise it is listed unticked with the tag "Review" and the note "Shared with Word and Excel", because removing it would affect those apps.
- **Outside the home folder** (C52). `/Library/LaunchAgents`, `/Library/LaunchDaemons`, `/Library/PrivilegedHelperTools` and `/Library/Application Support` entries that match the ids are listed as Report only (lock, no checkbox, tag "Report only", note "Needs an administrator, so Tidy leaves it. Remove it in Finder."), with Reveal. Under `TIDY_HOME` that folder is `$TIDY_HOME/system/Library`.
- Sizes are allocated bytes (`walk::measure`, staying on the item's volume). An entry Tidy cannot read is listed with "Size unknown" and stays ticked; the move decides.
- Not leftovers: anything else in the Library, Tidy's own folders, the Trash, Documents, and the app's files in other apps' folders.

### 19.4 Removing (C50, C51, C53)

- One `Uninstall { app, ids }` command. The core finds the leftovers again from scratch at that moment and moves only ids that are in the fresh result and selectable; any other id is skipped with "Tidy didn't find this again. Open the app again first." The app goes first, then the leftovers in list order.
- Its own guard, not `trash::check`: the app sits outside the scan root, and `Containers` and `Group Containers` are on the hard denylist that `trash::check` enforces for Cleanup and Large files. The guard is an allowlist instead. The app bundle must still be a real folder (not a link) whose parent resolves to one of the app folders of 19.2, with the same bundle id, not left out by 19.2, and not running. A leftover must still match the app by 19.3, sit directly in its expected folder, and that folder must resolve to the expected place under the resolved scan root (a linked `Library/Caches` is refused). A leftover that is a link is moved as the link. Then the shared last gate (`MoveGate`): the ignore list read fresh, and the open files of the user's processes. Report only rows are never moved.
- Running: the app's bundle id, its executable, and its helpers' ids and executables. When any runs, or the process list cannot be read, every item is skipped with "Quit Slack first" (or "Tidy couldn't check whether Slack is open").
- **Needs admin** (C51). An app bundle Tidy's user cannot rename (`access(W_OK)` fails on the bundle or its folder, as with Mac App Store apps and installer packages) is a Report only row: "Tidy can't move this app without an administrator. Drag it to the Trash in Finder, which asks for your password." Its leftovers can still be moved. Tidy never asks for a password. When macOS refuses the move itself (App Management), the row says "macOS didn't let Tidy move the app. Drag it to the Trash in Finder."
- Quitting Tidy stops an uninstall before its next item; the items left are skipped with "Tidy stopped before this item".
- Launch agents are moved, not unloaded: Tidy runs no `launchctl` command (C53), and the confirm says when they stop.
- History: one entry, action `uninstall`, source `uninstaller`, each item with its rule (`uninstall.app`, `uninstall.caches` and so on). History lists it as "Uninstaller"; Empty Trash counts its items like any other move.

### 19.5 Core, CLI and tests (C56, C57)

- `tidy-core::uninstall`: `list_apps(env, signer) -> AppList`, `find_leftovers(env, signer, app) -> UninstallPlan`, `uninstall(env, signer, app, ids, events) -> HistoryEntry`. The signer is a seam (`AppSigner`: Apple or not, team id, team name) with `CodeSign` in production and a fake in tests. Commands `ListApps { reply }` and `FindLeftovers { app, reply }` run beside the actor like `MeasureSystemData`; `Uninstall` is a queued action like `Clean`, ending in `CleanDone` with source `uninstaller`.
- Acceptance: `scripts/accept.sh offline` adds AC10 (the core, tidyctl and app tests of the uninstaller), and AC9 now checks Cmd-1 to Cmd-6 and shoots six screens (twelve PNGs).
- tidyctl: `apps` (the list), `leftovers APP` (the plan for an app path or bundle id), `uninstall APP [--ids ID,...]` (all ticked items by default). `uninstall` is destructive: it takes the lock and refuses the real home without `TIDY_HOME` or `--real` (exit 5), like `clean`.
- Tests build fixture homes in temp folders: `Applications/*.app` with XML `Info.plist`s, a Library with each kind of leftover, look-alikes that must not match (`com.example.app2`, a longer id of another app, a shared team container, a vendor-named folder, a linked `Library/Caches`), and a `system/Library`. They never read the real `~/Library` or `/Applications`, never use the macOS Trash, and never run `codesign` on real apps (the fake signer answers).
- Demo scenarios `uninstaller` (the list), `uninstaller-app` (zoom.us with name matches, a shared container under Review, a launch agent and two Report only rows) and `uninstaller-confirm` (its receipt) cover the screenshots. The demo shows glyph tiles where the real app shows app icons.

### 19.6 Calls

- C46. The uninstaller is a sixth screen, "Uninstaller", Cmd-6, after Security, with the app's details on the same screen behind a breadcrumb.
- C47. It lists `/Applications` and `~/Applications` one level deep, and leaves out Apple's apps (id or signature), Tidy and the running copy of Tidy. Listing needs no click.
- C48. Leftovers are looked for only when the user opens an app, since reading a sandboxed app's container may bring up macOS's "access data from other apps" prompt.
- C49. Matching is by bundle id and helper ids with a dot boundary, the longest installed id wins, names only when unique and not a vendor word, and team containers only when no other installed app shares the team.
- C50. The uninstaller has its own allowlist guard instead of the Cleanup denylist, because the app lives outside the home folder and containers are on that denylist.
- C51. Apps Tidy cannot move without an administrator are Report only; their leftovers can still go.
- C52. Matches in `/Library` are Report only.
- C53. Launch agents are moved but not unloaded; Tidy runs no `launchctl`.
- C54. An open app, or one of its helpers, blocks the whole uninstall until it quits.
- C55. Uninstall always asks first, with the receipt.
- C56. One new motion, the rows' fade-in after a look, on the existing `CARD_IN` token.
- C57. `tidyctl apps`, `leftovers` and `uninstall`.

## 20. Size map (design note, 2026-11-04)

The owner asked for more visuals and motion, and to see what can be deleted by size. Mockup: `docs/mockup/size-map.html` (and `size-map.png`).

### 20.1 What it shows (C58, C59, C60)

- A size map is a squarified treemap (`treemap::squarify`, `treemap::nested`): each tile's area is its size. Tiles sit in fractions of the map, so a resize keeps them in place. The map repeats the list and never replaces it: every tile is a row below it, and the list keeps the checkboxes, details and keyboard access.
- **Cleanup** (C58): a 120 pt map between the summary line and the list, 20 pt margins, 12 pt below. Tiles are the items that can be removed, grouped as the list groups them (groups inset 2 pt, tiles 2 pt apart); a collapsed group keeps its tiles. Report only items stay off the map, since it shows space you can get back. A click on a tile ticks or unticks the item, as its checkbox does; a blocked tile takes no click.
- **Large files, Folders mode** (C59): a 220 pt map of the open folder between the breadcrumb and the rows, the "smaller items" last. A click on a folder that has contents opens it, as the row's chevron does; on a file, package or empty folder it selects it. The smaller items tile takes no click. The scrolled hairline moves under the map.
- **Look** (C60), in the Preflight tokens only: ink (`accent`, `on_accent` text) means ticked or selected, so it will move, as an ink checkbox does. Unticked tiles are `fill` with `text`. A blocked tile (quit the app first) is `fill` with a 1.5 pt `warning` outline and its size in `warning`. The smaller items tile is an outline in `separator` with `text_2`. Tiles have radius 6 (3 when under 24 pt), the name at 12/600 and the size at 12/600 tnum, the name only at 52 x 24 pt or more and the size only from 44 pt high. "Review" shows under the name from 60 pt high, and a folder that opens shows a `ChevronRight` from 72 pt wide. Hover draws a 2 pt ring (`text`, or `on_accent` at 70% on an ink tile) and lights the row; hovering a row rings its tile. A scan in progress dims the map to 50% with the list.

### 20.2 Motion (C61)

Checked against `app-updates/docs/motion-gate.md`. Each motion plays only on a change after the first frame, so a window that opens on a settled state, and a demo shot, show still; under Reduce Motion none of them plays.

| Moment | Frequency | Purpose | Motion |
|---|---|---|---|
| Items reach the Trash | Occasional | Preventing a jarring change | The tile fades with its row (`REMOVE` 140 ms, `exit`), then the tiles left slide to their new places over `CARD_IN` 200 ms, `enter`. Only when the same result lost tiles and the map kept its size. |
| Opening a folder tile, going back up | Occasional | Spatial consistency | The tile grows to fill the map over `ZOOM` 220 ms, `enter`; going up, the parent map starts with the child we left filling it and settles over the same time. A jump across, a new mode or a new threshold shows the new map at once. |
| A scan result arrives | Occasional | Preventing a jarring change | Groups fade in `STAGGER` 40 ms apart, each over `CARD_IN`, `enter` (Large files: the first six tiles, then the rest with the sixth). Never blocks clicks. |
| After a clean or a trash | Rare | State indication | The sidebar disk bar's used part moves to its new share over `DISK` 320 ms, `enter`; a move that is still playing starts again from where the bar is. The numbers change at once. |

Rejected: counting the Figure up (a number the user reads), animating a tile on tick (tens a session), animating the Memory graph (live data), and any motion on Cmd-1 to Cmd-6.

### 20.3 Calls

- C58. Cleanup gets a size map of the removable items above the list; Report only items stay off it.
- C59. Large files gets a size map in Folders mode only. Files mode stays a list, since its rows come from many folders and a map of them says less than the sorted list.
- C60. Ink means "will move" on the map as on the checkboxes. No other color is added.
- C61. Four motions on the existing curves, with three new durations (`ZOOM`, `STAGGER`, `DISK`). After a clean, only the disk bar moves, and it moves by the space freed.

