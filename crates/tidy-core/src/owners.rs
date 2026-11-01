//! Who owns a folder: the static table that maps folder names to apps and tools, the lookup
//! of installed apps by their `Info.plist`, and a small `Info.plist` reader (design 6.2,
//! rule 8).
//!
//! An owner is a display name plus the bundle ids and process names that show the owner is
//! running. A rule checks them against the [`crate::model::RunningSet`] from the process probe.
//! Installed apps are read from `/Applications` and `<root>/Applications` in production, and
//! only from `<root>/Applications` under `TIDY_HOME`, so a test never reads the real machine.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::env::Env;

// ---------- Owners ----------

/// A fixed owner: the name shown in "Quit <name> first" and the ids that mean it is running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OwnerInfo {
    pub name: &'static str,
    /// Bundle ids and process names, compared without regard to case.
    pub ids: &'static [&'static str],
}

impl OwnerInfo {
    pub fn to_owner(self) -> Owner {
        Owner {
            name: self.name.to_string(),
            ids: self.ids.iter().map(|s| s.to_string()).collect(),
        }
    }
}

/// An owner resolved at scan time (from the static table or an installed app).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Owner {
    pub name: String,
    pub ids: Vec<String>,
}

impl Owner {
    /// The warning on a blocked item when this owner is running, or None when it is not. When
    /// the process list could not be read the item is blocked too, never treated as idle.
    pub fn blocked_by(&self, running: &crate::model::RunningSet) -> Option<String> {
        if running.is_unknown() {
            return Some(format!("Tidy couldn't check whether {} is open", self.name));
        }
        running
            .any(&self.ids)
            .then(|| format!("Quit {} first", self.name))
    }
}

pub const CHROME: OwnerInfo = OwnerInfo {
    name: "Google Chrome",
    ids: &[
        "com.google.Chrome",
        "Google Chrome",
        "chrome-headless-shell",
    ],
};
pub const XCODE: OwnerInfo = OwnerInfo {
    name: "Xcode",
    ids: &["com.apple.dt.Xcode", "Xcode"],
};
pub const CORE_SIMULATOR: OwnerInfo = OwnerInfo {
    name: "CoreSimulatorService",
    ids: &[
        "com.apple.CoreSimulator.CoreSimulatorService",
        "CoreSimulatorService",
    ],
};
pub const NPM: OwnerInfo = OwnerInfo {
    name: "npm",
    ids: &["npm"],
};
pub const NPM_OR_NPX: OwnerInfo = OwnerInfo {
    name: "npm",
    ids: &["npm", "npx"],
};
pub const NODE_GYP: OwnerInfo = OwnerInfo {
    name: "node-gyp",
    ids: &["node-gyp"],
};
pub const BREW: OwnerInfo = OwnerInfo {
    name: "Homebrew",
    ids: &["brew"],
};
pub const PIP: OwnerInfo = OwnerInfo {
    name: "pip",
    ids: &["pip", "pip3"],
};
pub const PNPM: OwnerInfo = OwnerInfo {
    name: "pnpm",
    ids: &["pnpm"],
};
pub const UV: OwnerInfo = OwnerInfo {
    name: "uv",
    ids: &["uv"],
};
pub const BUN: OwnerInfo = OwnerInfo {
    name: "Bun",
    ids: &["bun"],
};
pub const GRADLE: OwnerInfo = OwnerInfo {
    name: "Gradle",
    ids: &["gradle", "java"],
};
pub const CARGO: OwnerInfo = OwnerInfo {
    name: "Cargo",
    ids: &["cargo", "rustc"],
};
pub const ZED: OwnerInfo = OwnerInfo {
    name: "Zed",
    ids: &["dev.zed.Zed", "zed"],
};
pub const CLAUDE_CODE: OwnerInfo = OwnerInfo {
    name: "Claude Code",
    ids: &["claude"],
};
pub const FIREBASE: OwnerInfo = OwnerInfo {
    name: "Firebase CLI",
    ids: &["firebase"],
};
pub const GH: OwnerInfo = OwnerInfo {
    name: "GitHub CLI",
    ids: &["gh"],
};
pub const RIPGREP: OwnerInfo = OwnerInfo {
    name: "ripgrep",
    ids: &["rg"],
};
pub const YT_DLP: OwnerInfo = OwnerInfo {
    name: "yt-dlp",
    ids: &["yt-dlp"],
};
pub const LM_STUDIO: OwnerInfo = OwnerInfo {
    name: "LM Studio",
    ids: &["ai.elementlabs.lmstudio", "LM Studio"],
};
pub const OLLAMA: OwnerInfo = OwnerInfo {
    name: "Ollama",
    ids: &["com.electron.ollama", "Ollama", "ollama"],
};
pub const WHISPER: OwnerInfo = OwnerInfo {
    name: "Whisper",
    ids: &["whisper", "whisper-cpp"],
};
pub const FLUID_AUDIO: OwnerInfo = OwnerInfo {
    name: "FluidAudio",
    ids: &["FluidAudio"],
};
pub const HYPERFRAMES: OwnerInfo = OwnerInfo {
    name: "HyperFrames",
    ids: &["hyperframes"],
};
pub const LTX_DESKTOP: OwnerInfo = OwnerInfo {
    name: "LTX Desktop",
    ids: &["LTX Desktop", "LTXDesktop"],
};
/// The JetBrains IDEs, whose system folders sit in `Library/Caches/JetBrains/<product>`. Bundle
/// ids from each app's Info.plist, and the launcher names in `Contents/MacOS`.
pub const JETBRAINS: OwnerInfo = OwnerInfo {
    name: "your JetBrains IDE",
    ids: &[
        "com.jetbrains.intellij",
        "com.jetbrains.intellij.ce",
        "com.jetbrains.pycharm",
        "com.jetbrains.pycharm.ce",
        "com.jetbrains.WebStorm",
        "com.jetbrains.goland",
        "com.jetbrains.CLion",
        "com.jetbrains.rider",
        "com.jetbrains.PhpStorm",
        "com.jetbrains.rubymine",
        "com.jetbrains.datagrip",
        "com.jetbrains.rustrover",
        "com.jetbrains.dataspell",
        "com.jetbrains.toolbox",
        "idea",
        "pycharm",
        "webstorm",
        "goland",
        "clion",
        "rider",
        "phpstorm",
        "rubymine",
        "datagrip",
        "rustrover",
        "dataspell",
    ],
};
pub const POETRY: OwnerInfo = OwnerInfo {
    name: "Poetry",
    ids: &["poetry"],
};
/// RubyGems and Bundler. `gem` and `bundle` are Ruby scripts; the process probe sees them by
/// the script name.
pub const RUBYGEMS: OwnerInfo = OwnerInfo {
    name: "RubyGems",
    ids: &["gem", "bundle", "bundler"],
};
/// pyenv and its python-build plugin (shell scripts).
pub const PYENV: OwnerInfo = OwnerInfo {
    name: "pyenv",
    ids: &["pyenv", "python-build"],
};
/// rbenv and its ruby-build plugin (shell scripts).
pub const RBENV: OwnerInfo = OwnerInfo {
    name: "rbenv",
    ids: &["rbenv", "ruby-build"],
};
pub const GO: OwnerInfo = OwnerInfo {
    name: "Go",
    ids: &["go", "gopls"],
};
pub const ANYTHING_LLM: OwnerInfo = OwnerInfo {
    name: "AnythingLLM",
    ids: &["AnythingLLM", "anythingllm-desktop"],
};

struct FolderOwner {
    /// The folder name, compared without regard to case.
    folder: &'static str,
    info: OwnerInfo,
}

const fn fo(folder: &'static str, name: &'static str, ids: &'static [&'static str]) -> FolderOwner {
    FolderOwner {
        folder,
        info: OwnerInfo { name, ids },
    }
}

/// Folder names (in `Library/Caches`, `Library/Application Support`, `Library/Logs`, `.cache`)
/// that map to an app or tool without looking at the installed apps.
static FOLDER_OWNERS: &[FolderOwner] = &[
    fo("Google", CHROME.name, CHROME.ids),
    fo("com.google.Chrome", CHROME.name, CHROME.ids),
    fo(
        "company.thebrowser.dia",
        "Dia",
        &["company.thebrowser.dia", "Dia"],
    ),
    fo("Dia", "Dia", &["company.thebrowser.dia", "Dia"]),
    fo(
        "company.thebrowser.Browser",
        "Arc",
        &["company.thebrowser.Browser", "Arc"],
    ),
    fo("Arc", "Arc", &["company.thebrowser.Browser", "Arc"]),
    fo("claude-cli-nodejs", CLAUDE_CODE.name, CLAUDE_CODE.ids),
    fo(
        "Claude",
        "Claude",
        &["com.anthropic.claudefordesktop", "Claude"],
    ),
    fo(
        "com.anthropic.claudefordesktop",
        "Claude",
        &["com.anthropic.claudefordesktop", "Claude"],
    ),
    fo(
        "com.tinyspeck.slackmacgap",
        "Slack",
        &["com.tinyspeck.slackmacgap", "Slack"],
    ),
    fo("Slack", "Slack", &["com.tinyspeck.slackmacgap", "Slack"]),
    fo("discord", "Discord", &["com.hnc.Discord", "Discord"]),
    fo(
        "com.hnc.Discord",
        "Discord",
        &["com.hnc.Discord", "Discord"],
    ),
    fo(
        "Code",
        "Visual Studio Code",
        &["com.microsoft.VSCode", "Code", "Code Helper"],
    ),
    fo(
        "com.microsoft.VSCode",
        "Visual Studio Code",
        &["com.microsoft.VSCode", "Code", "Code Helper"],
    ),
    fo(
        "Cursor",
        "Cursor",
        &["com.todesktop.230313mzl4w4u92", "Cursor"],
    ),
    fo("dev.zed.Zed", ZED.name, ZED.ids),
    fo("Zed", ZED.name, ZED.ids),
    fo("Notion", "Notion", &["notion.id", "Notion"]),
    fo("notion.id", "Notion", &["notion.id", "Notion"]),
    fo("Figma", "Figma", &["com.figma.Desktop", "Figma"]),
    fo(
        "com.spotify.client",
        "Spotify",
        &["com.spotify.client", "Spotify"],
    ),
    fo("Spotify", "Spotify", &["com.spotify.client", "Spotify"]),
    fo(
        "org.mozilla.firefox",
        "Firefox",
        &["org.mozilla.firefox", "firefox"],
    ),
    fo("Firefox", "Firefox", &["org.mozilla.firefox", "firefox"]),
    fo(
        "Microsoft Edge",
        "Microsoft Edge",
        &["com.microsoft.edgemac", "Microsoft Edge"],
    ),
    fo(
        "com.microsoft.edgemac",
        "Microsoft Edge",
        &["com.microsoft.edgemac", "Microsoft Edge"],
    ),
    fo(
        "BraveSoftware",
        "Brave",
        &["com.brave.Browser", "Brave Browser"],
    ),
    fo(
        "com.brave.Browser",
        "Brave",
        &["com.brave.Browser", "Brave Browser"],
    ),
    fo("ollama", OLLAMA.name, OLLAMA.ids),
    fo("lm-studio", LM_STUDIO.name, LM_STUDIO.ids),
    fo("LM Studio", LM_STUDIO.name, LM_STUDIO.ids),
    fo(
        "org.swift.swiftpm",
        "Swift Package Manager",
        &["swift-package", "swift-build"],
    ),
    fo("go-build", GO.name, GO.ids),
    fo("gopls", "gopls", &["gopls"]),
    fo("golangci-lint", "golangci-lint", &["golangci-lint"]),
    fo("composer", "Composer", &["composer"]),
    fo(
        "typescript",
        "TypeScript",
        &["tsserver", "typescript-language-server"],
    ),
    fo("deno", "Deno", &["deno"]),
    fo("black", "Black", &["black"]),
    fo("puppeteer", "Puppeteer", &["puppeteer"]),
    fo("selenium", "Selenium", &["selenium-manager"]),
    fo("electron", "Electron", &["electron", "Electron"]),
    fo("yarn", "Yarn", &["yarn"]),
    fo("Yarn", "Yarn", &["yarn"]),
    fo("Cypress", "Cypress", &["Cypress", "cypress"]),
    fo("pypoetry", POETRY.name, POETRY.ids),
    fo("pre-commit", "pre-commit", &["pre-commit"]),
    fo("firebase", FIREBASE.name, FIREBASE.ids),
    fo("gh", GH.name, GH.ids),
    fo("vscode-ripgrep", RIPGREP.name, RIPGREP.ids),
    fo("yt-dlp", YT_DLP.name, YT_DLP.ids),
    fo("uv", UV.name, UV.ids),
    fo("pip", PIP.name, PIP.ids),
    fo("pnpm", PNPM.name, PNPM.ids),
    fo("node-gyp", NODE_GYP.name, NODE_GYP.ids),
    fo("Homebrew", BREW.name, BREW.ids),
];

/// The owner for a folder name from the static table, if it has one.
pub fn folder_owner(name: &str) -> Option<Owner> {
    FOLDER_OWNERS
        .iter()
        .find(|f| f.folder.eq_ignore_ascii_case(name))
        .map(|f| f.info.to_owner())
}

// ---------- Installed apps ----------

/// One installed app, from its `Info.plist`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledApp {
    /// The display name: `CFBundleDisplayName`, else `CFBundleName`, else the file name.
    pub name: String,
    pub bundle_id: String,
    /// `CFBundleExecutable`, the process name when the app runs.
    pub executable: Option<String>,
    /// `CFBundleShortVersionString`.
    pub version: Option<String>,
    pub path: PathBuf,
}

impl InstalledApp {
    pub fn to_owner(&self) -> Owner {
        let mut ids = vec![self.bundle_id.clone()];
        if let Some(e) = &self.executable {
            ids.push(e.clone());
        }
        Owner {
            name: self.name.clone(),
            ids,
        }
    }
}

/// The apps found in the application folders.
#[derive(Clone, Debug, Default)]
pub struct InstalledApps {
    by_id: HashMap<String, InstalledApp>,
    by_name: HashMap<String, String>,
}

impl InstalledApps {
    /// The apps for this environment: `/Applications` and `<root>/Applications` in
    /// production, only `<root>/Applications` when the environment is overridden.
    pub fn load(env: &Env) -> InstalledApps {
        Self::scan(&Self::folders(env))
    }

    /// The application folders for this environment.
    pub fn folders(env: &Env) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        if !env.overridden {
            dirs.push(PathBuf::from("/Applications"));
        }
        dirs.push(env.root.join("Applications"));
        dirs
    }

    /// Reads every `*.app` directly in `dirs`, and one level down in folders such as
    /// `Utilities`.
    pub fn scan(dirs: &[PathBuf]) -> InstalledApps {
        let mut apps = InstalledApps::default();
        for dir in dirs {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            for e in entries.flatten() {
                let path = e.path();
                let Ok(meta) = e.metadata() else { continue };
                if !meta.is_dir() {
                    continue;
                }
                if is_app(&path) {
                    apps.add(&path);
                } else if let Ok(inner) = std::fs::read_dir(&path) {
                    for e in inner.flatten() {
                        let p = e.path();
                        if is_app(&p) && e.metadata().is_ok_and(|m| m.is_dir()) {
                            apps.add(&p);
                        }
                    }
                }
            }
        }
        apps
    }

    fn add(&mut self, app: &Path) {
        if let Some(a) = read_app(app) {
            self.by_name
                .entry(a.name.to_ascii_lowercase())
                .or_insert_with(|| a.bundle_id.to_ascii_lowercase());
            if let Some(stem) = app.file_stem() {
                self.by_name
                    .entry(stem.to_string_lossy().to_ascii_lowercase())
                    .or_insert_with(|| a.bundle_id.to_ascii_lowercase());
            }
            self.by_id
                .entry(a.bundle_id.to_ascii_lowercase())
                .or_insert(a);
        }
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// The app with this bundle id, ignoring case.
    pub fn by_bundle_id(&self, id: &str) -> Option<&InstalledApp> {
        self.by_id.get(&id.to_ascii_lowercase())
    }

    /// The app with this display name or file name, ignoring case.
    pub fn by_name(&self, name: &str) -> Option<&InstalledApp> {
        let id = self.by_name.get(&name.to_ascii_lowercase())?;
        self.by_id.get(id)
    }

    /// Resolves a folder name to its owner: the static table first, then an installed app
    /// whose bundle id is the name, then (when `match_names`) one whose name is the name.
    pub fn resolve(&self, folder: &str, match_names: bool) -> Option<Owner> {
        if let Some(o) = folder_owner(folder) {
            return Some(o);
        }
        if let Some(a) = self.by_bundle_id(folder) {
            return Some(a.to_owner());
        }
        if match_names {
            return self.by_name(folder).map(InstalledApp::to_owner);
        }
        None
    }
}

fn is_app(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("app"))
}

/// Reads `<app>/Contents/Info.plist`. None when it is missing, unreadable or has no bundle id.
pub fn read_app(app: &Path) -> Option<InstalledApp> {
    let bytes = std::fs::read(app.join("Contents/Info.plist")).ok()?;
    let keys = read_plist_strings(
        &bytes,
        &[
            "CFBundleIdentifier",
            "CFBundleDisplayName",
            "CFBundleName",
            "CFBundleExecutable",
            "CFBundleShortVersionString",
        ],
    );
    let bundle_id = keys.get("CFBundleIdentifier")?.clone();
    let name = keys
        .get("CFBundleDisplayName")
        .or_else(|| keys.get("CFBundleName"))
        .cloned()
        .filter(|n| !n.is_empty())
        .or_else(|| app.file_stem().map(|s| s.to_string_lossy().into_owned()))?;
    Some(InstalledApp {
        name,
        bundle_id,
        executable: keys.get("CFBundleExecutable").cloned(),
        version: keys.get("CFBundleShortVersionString").cloned(),
        path: app.to_path_buf(),
    })
}

/// The bundle id of an app folder, from its `Info.plist`.
pub fn bundle_id_of(app: &Path) -> Option<String> {
    let bytes = std::fs::read(app.join("Contents/Info.plist")).ok()?;
    read_plist_strings(&bytes, &["CFBundleIdentifier"]).remove("CFBundleIdentifier")
}

// ---------- Info.plist reader ----------

/// The string values of `keys` in the top-level dictionary of a plist, XML or binary. Keys
/// that are absent or hold other types are left out. Malformed input gives an empty map.
pub fn read_plist_strings(bytes: &[u8], keys: &[&str]) -> HashMap<String, String> {
    if bytes.starts_with(b"bplist00") {
        read_binary(bytes, keys).unwrap_or_default()
    } else {
        read_xml(bytes, keys)
    }
}

fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn read_xml(bytes: &[u8], keys: &[&str]) -> HashMap<String, String> {
    let text = String::from_utf8_lossy(bytes);
    let mut out = HashMap::new();
    let mut depth = 0usize;
    let mut pending: Option<String> = None;
    let mut rest: &str = &text;
    while let Some(lt) = rest.find('<') {
        let after = &rest[lt + 1..];
        // Comments, the doctype and the xml declaration.
        if after.starts_with("!--") {
            match after.find("-->") {
                Some(end) => {
                    rest = &after[end + 3..];
                    continue;
                }
                None => break,
            }
        }
        let Some(gt) = after.find('>') else { break };
        let tag = &after[..gt];
        rest = &after[gt + 1..];
        if tag.starts_with('?') || tag.starts_with('!') {
            continue;
        }
        let closing = tag.starts_with('/');
        let self_closing = tag.ends_with('/');
        let name = tag
            .trim_start_matches('/')
            .trim_end_matches('/')
            .split_whitespace()
            .next()
            .unwrap_or("");
        match (name, closing) {
            ("dict" | "array", false) if !self_closing => {
                depth += 1;
                if depth > 1 {
                    pending = None;
                }
            }
            ("dict" | "array", true) => {
                depth = depth.saturating_sub(1);
                if depth == 1 {
                    pending = None;
                }
            }
            ("key", false) if depth == 1 && !self_closing => {
                let end = rest.find('<').unwrap_or(rest.len());
                pending = Some(unescape(&rest[..end]));
            }
            ("string", false) if depth == 1 && !self_closing => {
                let end = rest.find('<').unwrap_or(rest.len());
                if let Some(k) = pending.take()
                    && keys.contains(&k.as_str())
                {
                    out.insert(k, unescape(rest[..end].trim()));
                }
            }
            (_, false) if depth == 1 && !matches!(name, "key" | "string") => {
                // Another scalar (integer, true, false, date, data): its key is used up.
                pending = None;
            }
            _ => {}
        }
    }
    out
}

fn read_binary(b: &[u8], keys: &[&str]) -> Option<HashMap<String, String>> {
    if b.len() < 8 + 32 {
        return None;
    }
    let t = &b[b.len() - 32..];
    let off_size = t[6] as usize;
    let ref_size = t[7] as usize;
    let num_objects = be(&t[8..16])? as usize;
    let top = be(&t[16..24])? as usize;
    let table = be(&t[24..32])? as usize;
    if !(1..=8).contains(&off_size) || !(1..=8).contains(&ref_size) || top >= num_objects {
        return None;
    }
    let offset_of = |idx: usize| -> Option<usize> {
        let at = table.checked_add(idx.checked_mul(off_size)?)?;
        be(b.get(at..at.checked_add(off_size)?)?).map(|v| v as usize)
    };
    // The marker byte and the length of the object at `off`; returns (marker, len, data_start).
    let header = |off: usize| -> Option<(u8, usize, usize)> {
        let m = *b.get(off)?;
        let mut len = (m & 0x0f) as usize;
        let mut at = off + 1;
        if len == 0x0f && (m >> 4) != 0 {
            let im = *b.get(at)?;
            if im >> 4 != 1 {
                return None;
            }
            let n = 1usize << (im & 0x0f);
            len = be(b.get(at + 1..at + 1 + n)?)? as usize;
            at += 1 + n;
        }
        Some((m, len, at))
    };
    let string_at = |idx: usize| -> Option<String> {
        let (m, len, at) = header(offset_of(idx)?)?;
        match m >> 4 {
            0x5 => Some(String::from_utf8_lossy(b.get(at..at.checked_add(len)?)?).into_owned()),
            0x6 => {
                let raw = b.get(at..at.checked_add(len.checked_mul(2)?)?)?;
                let units: Vec<u16> = raw
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_be_bytes(*c))
                    .collect();
                Some(String::from_utf16_lossy(&units))
            }
            _ => None,
        }
    };
    let (m, count, at) = header(offset_of(top)?)?;
    if m >> 4 != 0xD {
        return None;
    }
    let mut out = HashMap::new();
    for i in 0..count {
        let kat = at.checked_add(i.checked_mul(ref_size)?)?;
        let vat = at.checked_add(count.checked_add(i)?.checked_mul(ref_size)?)?;
        let k = be(b.get(kat..kat.checked_add(ref_size)?)?)? as usize;
        let v = be(b.get(vat..vat.checked_add(ref_size)?)?)? as usize;
        let Some(key) = string_at(k) else { continue };
        if keys.contains(&key.as_str())
            && let Some(val) = string_at(v)
        {
            out.insert(key, val);
        }
    }
    Some(out)
}

/// A big-endian unsigned integer of 1 to 8 bytes.
fn be(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() || bytes.len() > 8 {
        return None;
    }
    Some(bytes.iter().fold(0u64, |acc, b| (acc << 8) | u64::from(*b)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleName</key>
	<string>Idle &amp; Co</string>
	<key>CFBundleIdentifier</key>
	<string>com.example.Idle</string>
	<key>LSEnvironment</key>
	<dict>
		<key>CFBundleIdentifier</key>
		<string>com.example.Nested</string>
	</dict>
	<key>CFBundleURLTypes</key>
	<array><dict><key>CFBundleName</key><string>Inner</string></dict></array>
	<key>LSUIElement</key>
	<true/>
	<key>CFBundleExecutable</key>
	<string>Idle</string>
</dict>
</plist>"#;

    #[test]
    fn xml_plist_reads_top_level_strings_only() {
        let m = read_plist_strings(
            XML.as_bytes(),
            &[
                "CFBundleIdentifier",
                "CFBundleName",
                "CFBundleExecutable",
                "Nope",
            ],
        );
        assert_eq!(m["CFBundleIdentifier"], "com.example.Idle");
        assert_eq!(m["CFBundleName"], "Idle & Co");
        assert_eq!(m["CFBundleExecutable"], "Idle");
        assert!(!m.contains_key("Nope"));
        assert!(read_plist_strings(b"garbage", &["a"]).is_empty());
        assert!(read_plist_strings(b"", &["a"]).is_empty());
    }

    #[test]
    fn binary_plist_reads_ascii_and_utf16_strings() {
        let mut b = b"bplist00".to_vec();
        let mut offsets = vec![b.len()];
        b.extend_from_slice(&[0xD3, 1, 2, 3, 4, 5, 6]);
        for s in [
            "CFBundleIdentifier",
            "CFBundleName",
            "CFBundleExecutable",
            "com.example.Bin",
        ] {
            offsets.push(b.len());
            if s.len() < 15 {
                b.push(0x50 | s.len() as u8);
            } else {
                // A length of 15 or more is an extra one-byte integer object.
                b.extend_from_slice(&[0x5F, 0x10, s.len() as u8]);
            }
            b.extend_from_slice(s.as_bytes());
        }
        offsets.push(b.len());
        b.push(0x64);
        for u in "Büro".encode_utf16() {
            b.extend_from_slice(&u.to_be_bytes());
        }
        offsets.push(b.len());
        b.push(0x53);
        b.extend_from_slice(b"Bin");
        let table = b.len();
        for o in &offsets {
            b.push(*o as u8);
        }
        let mut trailer = [0u8; 32];
        trailer[6] = 1;
        trailer[7] = 1;
        trailer[15] = offsets.len() as u8;
        trailer[31] = table as u8;
        b.extend_from_slice(&trailer);
        let m = read_plist_strings(
            &b,
            &["CFBundleIdentifier", "CFBundleName", "CFBundleExecutable"],
        );
        assert_eq!(m["CFBundleIdentifier"], "com.example.Bin");
        assert_eq!(m["CFBundleName"], "Büro");
        assert_eq!(m["CFBundleExecutable"], "Bin");
        // A truncated file does not panic.
        for n in 0..b.len() {
            let _ = read_plist_strings(&b[..n], &["CFBundleIdentifier"]);
        }
    }

    /// A plist written by macOS itself (`plutil`), so the binary reader is checked against the
    /// real format, not only against the test's own writer. Skipped when plutil is missing.
    #[test]
    fn binary_plist_from_plutil_reads() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("Info.plist");
        std::fs::write(&file, XML).unwrap();
        let Ok(status) = std::process::Command::new("/usr/bin/plutil")
            .args(["-convert", "binary1"])
            .arg(&file)
            .status()
        else {
            return;
        };
        assert!(status.success());
        let bytes = std::fs::read(&file).unwrap();
        assert!(bytes.starts_with(b"bplist00"));
        let m = read_plist_strings(
            &bytes,
            &["CFBundleIdentifier", "CFBundleName", "CFBundleExecutable"],
        );
        assert_eq!(m["CFBundleIdentifier"], "com.example.Idle");
        assert_eq!(m["CFBundleName"], "Idle & Co");
        assert_eq!(m["CFBundleExecutable"], "Idle");
    }

    fn make_app(dir: &Path, file: &str, plist: &str) -> PathBuf {
        let app = dir.join(file);
        std::fs::create_dir_all(app.join("Contents")).unwrap();
        std::fs::write(app.join("Contents/Info.plist"), plist).unwrap();
        app
    }

    #[test]
    fn installed_apps_are_found_by_id_and_name() {
        let tmp = tempfile::tempdir().unwrap();
        let apps = tmp.path().join("Applications");
        make_app(&apps, "Idle.app", XML);
        make_app(
            &apps.join("Utilities"),
            "Tool.app",
            "<plist><dict><key>CFBundleIdentifier</key><string>com.example.Tool</string></dict></plist>",
        );
        make_app(&apps, "Broken.app", "not a plist");
        std::fs::create_dir_all(apps.join("Plain Folder")).unwrap();
        let found = InstalledApps::scan(&[apps.clone(), tmp.path().join("missing")]);
        assert_eq!(found.len(), 2);
        let idle = found.by_bundle_id("COM.EXAMPLE.IDLE").unwrap();
        assert_eq!(idle.name, "Idle & Co");
        assert_eq!(idle.executable.as_deref(), Some("Idle"));
        assert_eq!(found.by_name("idle").unwrap().bundle_id, "com.example.Idle");
        assert_eq!(found.by_name("Tool").unwrap().name, "Tool");
        let o = found.resolve("com.example.Idle", false).unwrap();
        assert_eq!(o.name, "Idle & Co");
        assert_eq!(o.ids, ["com.example.Idle", "Idle"]);
        assert!(found.resolve("Idle", false).is_none());
        assert!(found.resolve("Idle", true).is_some());
        assert!(found.resolve("com.unknown.thing", true).is_none());
        // The static table wins and needs no installed app.
        assert_eq!(
            found.resolve("Google", false).unwrap().name,
            "Google Chrome"
        );
    }

    #[test]
    fn the_static_table_maps_folder_names() {
        let chrome = folder_owner("google").unwrap();
        assert!(chrome.ids.iter().any(|i| i == "com.google.Chrome"));
        assert_eq!(folder_owner("Company.TheBrowser.Dia").unwrap().name, "Dia");
        assert_eq!(folder_owner("claude-cli-nodejs").unwrap().ids, ["claude"]);
        assert!(folder_owner("mystery").is_none());
        assert!(folder_owner("com.apple.Safari").is_none());
    }

    #[test]
    fn an_owner_is_blocked_when_any_id_runs() {
        let o = CHROME.to_owner();
        assert_eq!(o.blocked_by(&crate::model::RunningSet::parse("npm")), None);
        assert_eq!(
            o.blocked_by(&crate::model::RunningSet::parse("com.google.chrome")),
            Some("Quit Google Chrome first".to_string())
        );
        assert_eq!(
            o.blocked_by(&crate::model::RunningSet::unknown())
                .as_deref(),
            Some("Tidy couldn't check whether Google Chrome is open")
        );
    }
}
