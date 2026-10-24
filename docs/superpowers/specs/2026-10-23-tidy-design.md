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
- Be safe by construction: every removal goes to the Trash and is written to a history file; tests and agents can never touch the real home folder or the real Trash.

## 3. Non-goals (for now)

- App uninstaller (finding an app's leftovers).
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

**Result.** Five groups in a fixed order: System and app caches, Logs, Developer, AI models and large app data, Trash. Each group header shows a tri-state checkbox, the group name, the item count and the group total. Groups with zero items are hidden. Groups start expanded except "AI models and large app data", which starts collapsed (C5).

Each item has one of four classes:

| Class | Checkbox | Included in "Clean safe items" | Shown |
|---|---|---|---|
| Safe | ticked | yes | yes |
| Review | unticked | no, only if the user ticks it | yes, with a "Review" tag and a one-line reason |
| Report only | none | never | yes, with "Tidy won't remove this" and the reason |
| Excluded | n/a | never | never |

An item whose owner app is running is shown unticked and disabled, with the meta line "Quit Google Chrome first" in the warning color. While the Cleanup screen is visible, Tidy re-checks running processes every 3 seconds (cheap, no disk access) and enables the item when the app quits, ticking it again if its class is Safe (C6). Tidy never quits the app from this screen.

**Item row content.** Checkbox, a glyph tile for the group, the name (the app or tool name when known, otherwise the folder name), the meta line (the rule's reason, for example "Cache, rebuilt by the app" or "Build output, project not touched for 41 days"), and the size on the right. Hover shows Reveal in Finder and a "..." menu with "Never show this again" (adds the path to the ignore list in settings) and "Copy Path".

**Clean safe items.** The primary toolbar button reads "Clean 6.3 GB" (the sum of ticked items) and is disabled at zero.

- When every ticked item is Safe, clicking it cleans immediately with no dialog, because everything goes to the Trash and can be put back (C7).
- When any Review item is ticked, the button reads "Clean selected" and opens a confirm dialog that lists the ticked Review items by name and size: "Move these to the Trash? You can put them back from the Trash until you empty it." Buttons: Cancel, Move to Trash.

**During a clean.** The button shows "Cleaning 4 of 12". Each row fades out over 140 ms as its item reaches the Trash (Haul's REMOVE motion; instant under Reduce Motion). Right before moving each item, Tidy re-runs that item's checks (section 6.2). An item that fails a check stays in the list with the reason ("Google Chrome opened, skipped"). One clean writes one history entry.

**After a clean.** A notice band under the toolbar: "Moved 6.3 GB to the Trash. The space is freed when you empty the Trash." with buttons Empty Trash and Show History. It stays until dismissed or the next scan.

**Status bar.** Left: "12 selected, 6.3 GB". Right: "In the Trash from Tidy: 2.1 GB" and a text button "Empty Trash...".

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
- **Three figures**: Pressure (Normal, Warning, Critical from `kern.memorystatus_vm_pressure_level`), Swap used (from `vm.swapusage`), Compressed (compressor pages times page size). Each has a one-line explanation under it. A small pressure history graph covers the time the screen has been open (no background sampling).
- **Top apps by memory**: processes grouped under their responsible app (`responsibility_get_pid_responsible_for_pid` via `dlsym`, falling back to the parent chain and then to the `.app` prefix of `proc_pidpath`). Each row: app icon, name, footprint ("4.2 GB"), number of helper processes, and a Quit button. Rows for processes Tidy cannot attribute show the process name. Root-owned and other users' processes cannot be read by an app; they are summed into one row "System processes" (sized with `top`, see the M2 notes), without a Quit button.
- **Quit** sends `terminate`. The row shows "Quit requested" and then disappears when the app exits. After 10 seconds without exit, the row shows "Still open. The app may be asking to save." and the "..." menu offers Force Quit with a confirm ("Unsaved changes in X will be lost."). Tidy itself, Finder, WindowServer and loginwindow have no Quit button.
- Refresh every 2 seconds while the screen is visible; nothing runs when it is not.
- No "Free up RAM" button, because there is no honest user-level way to do it (`purge` needs root and only drops file cache).

### 5.4 Security (M2)

A list of checks, each with a status dot (green, amber, gray for "could not check"), a title, one plain sentence, and for amber items a button that opens the right System Settings page. Results of each run are saved, and items that changed since the last run show "New" or "Changed".

| Check | Source | Amber when | Button |
|---|---|---|---|
| FileVault | `fdesetup status` | Off | Open Privacy & Security |
| Firewall | `socketfilterfw --getglobalstate` | Disabled | Open Network settings (firewall anchor unverified) |
| Gatekeeper | `spctl --status` | Assessments disabled | Open Privacy & Security |
| System Integrity Protection | `csrutil status` | Disabled | none, explains how to turn it back on in Recovery |
| XProtect | mtime and version of `XProtect.bundle/Contents/Info.plist` | Not updated in 60 days | Open Software Update |
| macOS updates | `softwareupdate --list`, run on demand, 60 s timeout | An update is listed | Open Software Update |
| Login items and background helpers | plists in `~/Library/LaunchAgents`, `/Library/LaunchAgents`, `/Library/LaunchDaemons`, plus `launchctl print gui/<uid>` | An item is new since the last check, or its program is unsigned or not notarized | Reveal in Finder, Move to Trash (user agents only), Open Login Items |
| Chrome extensions | `Secure Preferences` of every Chrome profile (copied before parsing) | An extension was added since the last check | Open `chrome://extensions` is not possible from outside Chrome, so the row says where to look |

Login items list "new since last check" first. Each item shows label, program path, signer (team name or "Apple" or "Not signed"), notarized yes or no (from `SecStaticCodeCheckValidity` and `SecAssessment`), and whether it runs at load. Signature checks run in the background, one item at a time.

The first run has nothing to compare against and says "This is your first check. Next time Tidy will show what changed." Gray items say why ("Tidy couldn't read this without Full Disk Access") with no button that asks for access.

Copy rules: no "threat", "infected", "danger" or "risk score". An amber item says what is off and what it means in one sentence, for example "Firewall is off. Your Mac accepts incoming connections from apps that ask for them. Most home networks are fine either way."

### 5.5 Check up (M2)

One button, "Run Check up", that runs the cleanup scan, the security check (without the slow `softwareupdate --list` unless the user ticked "Include macOS updates") and a memory snapshot. The result is three cards: Cleanup ("6.3 GB ready to clean" with a Clean button that does the same as on the Cleanup screen), Security ("All good" or "2 things to look at"), Memory (the verdict line). Each card links to its screen. The date of the last check up shows under the button.

**Weekly reminder** (opt-in toggle on this screen, off by default). Turning it on requests notification permission at that moment, never at launch. Tidy schedules one non-repeating `UNCalendarNotificationTrigger` for 7 days after the last check up, at 10:00 on that day, and reschedules it on each launch and after each check up (C16). If the app is never opened again, at most one reminder arrives. The notification says "Time for your weekly check up." and opens Tidy on Check up. If permission is denied, the toggle shows "Notifications are off for Tidy" with a button to open Notifications settings.

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
| `Library/Application Support/<App>/{Cache, Code Cache, GPUCache, DawnGraphiteCache, DawnWebGPUCache, GrShaderCache, ShaderCache, Crashpad/completed, Service Worker/ScriptCache}` for Electron and Chromium apps, including each Chrome profile | Safe | Owner not running. Only these subfolder names, never their siblings. |
| `.../Service Worker/CacheStorage` (same apps) | Review | "Some websites may load slower once or lose offline data." |
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
| `.cache/<tool>` for firebase, gh, vscode-ripgrep, yt-dlp | Safe | Owner tool not running. Unknown `.cache/<name>` folders are not offered. |
| `.cargo/bin`, `.rustup`, `.cursor/extensions`, `.docker`, `.claude/projects` | Excluded | Installed software or user history. |

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
5. Right before each move, re-check: the path still exists, `realpath` is strictly inside the scan root, it is not on the denylist or the user's ignore list, its markers are still present (table 6.3), and the owner is not running. A failed check skips the item with a reason.
6. On EPERM or "Operation not permitted", skip the item with "macOS didn't allow access" and never retry in a loop.
7. Only targets in the rule table are offered. There is no "scan everything" mode and no free-form delete in Cleanup.
8. Owner resolution: a static table maps folder names to bundle ids and process names (Google to Chrome, `company.thebrowser.dia` to Dia, and so on). A folder name that looks like a bundle id is checked against installed apps in `/Applications` and `~/Applications` by reading each `Info.plist`. Running processes come from `proc_listpids` plus `proc_pidpath`; the bundle id of a process is read from the `Info.plist` of the `.app` that contains its executable (C20: plain libc, works the same in the app and in tidyctl).

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

**New type role**: Figure, 22 / 28, weight 600, tabular, `text`. Used for the summary number.

**Window**: 900 x 620 pt default, minimum 760 x 480, transparent title bar, traffic lights at x 20 inside the sidebar, centered on the 52 pt toolbar row. Title "Tidy".

**Sidebar** (200 pt wide, `sidebar_bg`, a hairline `separator` on its right edge):
- Top 52 pt holds the traffic lights only.
- Rows start at y 52, inset 10 left and right: height 28, radius 6, padding 0 8, gap 8; Lucide icon 16 (Check up `Stethoscope`, Cleanup `Sparkles`, Large files `HardDrive`, Memory `MemoryStick`, Security `ShieldCheck`; add any not bundled to `icon_assets!`) in `text_2`, label 13/400 `text`, trailing figure 12 tnum `text_2` (Cleanup: the safe total after a scan, "6.3 GB"; Large files: the item count). Selected row: `sidebar_selected` fill, icon in `accent`. 4 pt between rows.
- Bottom, 16 pt from the edge: "Macintosh HD" 12/500 `text`, a 4 pt bar (track `track`, fill `accent`, radius 2) showing used space, and "212 GB available of 1 TB" 12 tnum `text_2`. Available means `NSURLVolumeAvailableCapacityForImportantUsageKey`, which matches Finder (C25).

**Content area** (`window_bg`):
- **Toolbar** 52 pt, aligned with the title bar, padding 0 12 0 20. Left: screen title 15/600. Right, gap 8: screen controls, then the primary button. Drag and double-click on empty toolbar space move and zoom the window (Haul's `start_window_move`). A hairline under it only when the list is scrolled.
- **Summary strip** 64 pt, padding 0 20: the Figure ("6.3 GB" with " ready to clean" in 15/400 `text_2` on the same baseline), and under it one 12/400 `text_2` line ("34 GB more to review · Scanned 2 min ago").
- **Notice band** (when present) as in Haul: margin 2 12 6, padding 8 8 8 12, radius 8, `fill` background, icon, sentence, buttons on the right.
- **List** in a `uniform_list`, inset 8 left and right.
- **Status bar** 30 pt, `bar_bg`, hairline on top, padding 0 10 0 20: left text 12 tnum `text_2`, right text buttons.

**Cleanup list** (every row 44 pt so `uniform_list` holds; group headers are rows too):
- Group header row: padding 0 10, gap 8: chevron 12 `text_3` (rotates 90 degrees when expanded, snap, no animation), checkbox 16 (tri-state), name 13/600 `text`, count 12 `text_2` ("14 items"), flexible space, size 13/600 tnum `text`. A hairline `separator` above every group header except the first.
- Item row: left padding 34 (aligns under the group name), gap 10: checkbox 16, glyph tile 28 (radius 6, `fill_2`, icon 16 `text_2`), a text column (name 13/500 `text` on top, meta 12/400 `text_2` below; meta uses `warning` for "Quit X first" and `text_2` otherwise), an optional tag (11/600 `text_2`, 1 px `tag_border`, radius 4, height 16, padding 0 5, text "Review" or "Report only"), flexible space, size 13/400 tnum `text_2`, and a 60 pt trailing area holding the hover actions (Reveal `FolderSearch`, more `Ellipsis`, 26 pt icon buttons). Hover fill `row_hover`, radius 8. Disabled (blocked) rows show the checkbox at 40% opacity.
- Report only rows have no checkbox; a `Lock` icon 12 `text_3` takes its place.
- Toolbar controls: History (clock icon button), Rescan (`RefreshCw` icon button), primary "Clean 6.3 GB" (Haul's primary text button, 24 pt high).
- Empty state (before the first scan, or after a scan finds nothing): centered column, max width 360: icon 32 `text_3`, title 15/600, one line 13 `text_2`, primary button. Copy before the first scan: "Find space you can get back" / "Tidy looks at caches, old logs, old build output and package caches. Nothing moves until you choose, and everything goes to the Trash." / "Scan". After an empty result: "Nothing to clean right now" / "Caches of apps that are open aren't counted. Quit them and scan again." / "Scan Again".

**Large files list** (every row 52 pt):
- Toolbar controls, left to right: segmented "Files | Folders" (gpui-component tab or a two-button group, 24 pt), segmented "Biggest | Not opened lately" (hidden in Folders mode), threshold select "Over 500 MB", Rescan icon button. No primary button; the action lives in the status bar.
- Breadcrumb row (Folders mode only, 28 pt, padding 0 20): segments 12/500 `text_2` separated by `ChevronRight` 12 `text_3`; the last segment in `text`; earlier segments are buttons.
- Row: padding 0 10, gap 10: checkbox 16, glyph tile 32 (radius 7, `fill_2`, icon 18 `text_2`), text column (name 13/500 `text`; path 12 mono `text_2`, middle-truncated; in Folders mode the path line is replaced by the 4 pt share bar, 160 pt wide, `track` and `accent`), tags, flexible space, the date phrase 12 `text_2` (fixed 150 pt column, right-aligned), size 13/500 tnum `text` (fixed 80 pt column, right-aligned), 60 pt hover actions (Reveal, Move to Trash `Trash2`). In Folders mode folders show a trailing `ChevronRight` instead of Move to Trash at rest.
- The "smaller items" row: no checkbox, name in `text_2`, no actions.
- Status bar: left "3 selected, 12.4 GB" or "48 items, up to 214 GB"; right "Move to Trash..." text button, disabled with no selection.
- Scanning: the toolbar shows a spinner and "Scanning, 214,000 files" in 12 tnum `text_2`; a Stop text button cancels. The previous result stays visible, dimmed to 50%, until the new one arrives.

**Dialogs**: gpui-component dialog, 420 pt wide, title 13/600, body 13/400 with the item list in a scrollable box up to 200 pt high (12 tnum rows: name left, size right), buttons right-aligned: Cancel (secondary) and the action (primary; Empty Trash uses `danger_button`). Return confirms, Escape cancels.

**History sheet**: 560 x 480 pt, rows 44 pt: date 13/500, action and counts 12 `text_2`, size right; expanded items are 32 pt rows with mono paths.

**Motion**: only Haul's existing motions apply. Row fade-out 140 ms when an item is trashed (send first, fade on `ItemTrashed`; restore the row with its skip reason on `ItemSkipped`). Notice band in 160 ms, out 200 ms. Indeterminate spinner while scanning. Nothing counts up, nothing celebrates.

**Copy rules**: sentence case, plain words, sizes with one decimal above 1 GB ("6.3 GB", "640 MB"), dates as relative phrases ("8 months ago"), no exclamation marks.

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
