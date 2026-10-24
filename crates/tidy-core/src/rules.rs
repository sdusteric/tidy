//! The Cleanup rule table as data (design sections 6.1 to 6.3).
//!
//! Every row of the design's tables is a [`Rule`] (or a [`ProjectRule`] for project build
//! output). The scan in `cleanup` reads this table; nothing else decides what is offered.
//! Paths are relative to the scan root (the home folder in production).
//!
//! # Rule ids
//!
//! An item id is `<rule id>:<path relative to the scan root>`. A row that folds small matches
//! ("38 small items, 12 MB") uses `<rule id>.small:<folder that holds the members>`.
//!
//! | Rule id | Row |
//! |---|---|
//! | `cache.app` | `Library/Caches/<name>` and `Library/Application Support/Caches/<name>` of a known owner (Safe) |
//! | `cache.unknown` | the same with no known owner, and Electron cache folders with no known owner (Review) |
//! | `cache.chrome` | `Library/Caches/Google/Chrome/<profile>` and the headless folders (Safe) |
//! | `cache.chrome.crx` | Chrome's crx and shader caches in Application Support (Safe) |
//! | `cache.electron` | Cache, Code Cache, GPUCache and similar folders of Electron and Chromium apps (Safe) |
//! | `cache.electron.sw` | `Service Worker/CacheStorage` (Review) |
//! | `logs.app` | files in `Library/Logs/<app>` older than 14 days (Safe) |
//! | `logs.diag` | files in `Library/Logs/DiagnosticReports` older than 30 days (Safe) |
//! | `dev.deriveddata`, `dev.previews`, `dev.coresim-caches` | Xcode build data (Safe) |
//! | `dev.devicesupport`, `dev.archives` | Xcode device symbols and archives (Review) |
//! | `dev.xcode-codingassistant`, `dev.simulators` | Report only |
//! | `dev.npm-cacache`, `dev.npx`, `dev.node-gyp`, `dev.brew-downloads` | package caches (Safe) |
//! | `dev.pip`, `dev.pnpm`, `dev.uv`, `dev.bun`, `dev.gradle-caches`, `dev.cache` | package caches (Safe) |
//! | `dev.gradle-wrapper`, `dev.m2`, `dev.cargo-registry`, `dev.cocoapods-repos` | package caches (Review) |
//! | `dev.playwright`, `dev.agent-versions`, `dev.zed`, `dev.androidstudio` | tool data (Review) |
//! | `dev.pnpm-store` | Report only |
//! | `proj.<kind>` | project build output, see [`PROJECT_RULES`] |
//! | `ai.huggingface`, `ai.lmstudio`, `ai.ollama`, `ai.whisper`, `ai.fluidaudio`, `ai.hyperframes`, `ai.ltx`, `ai.anythingllm`, `ai.chrome-models` | models (Review) |
//! | `ai.anythingllm-data`, `ai.vm` | Report only |

use crate::model::{Class, Group, MB};
use crate::owners::{self, OwnerInfo};

/// The smallest row (1 MB). Smaller matches of one rule fold into a single row.
pub const MIN_ROW_BYTES: u64 = MB;
/// Anything modified within this many seconds of now is not offered (rule 6.2.2).
pub const RECENT_SECS: u64 = 24 * 3600;
/// How deep the project walk goes below a project root.
pub const PROJECT_MAX_DEPTH: usize = 6;
/// The suffix of the rule id on a folded row.
pub const SMALL_SUFFIX: &str = ".small";

/// How old a target must be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Age {
    /// No age rule beyond the 24-hour skip.
    None,
    /// The newest file mtime inside the target is older than this many days.
    Target(u32),
    /// The newest file mtime in the project folder (the parent of the build output,
    /// excluding build output and `.git`) is older than this many days.
    Project(u32),
}

impl Age {
    pub fn days(self) -> Option<u32> {
        match self {
            Age::None => None,
            Age::Target(d) | Age::Project(d) => Some(d),
        }
    }
}

/// How a rule finds the process that blocks it.
#[derive(Clone, Copy, Debug)]
pub enum Owner {
    /// No process blocks this target.
    None,
    /// A fixed app or tool.
    Fixed(&'static OwnerInfo),
    /// The owner is resolved from the name of the folder (static table, then installed apps).
    Folder,
}

/// What "keep the newest" groups by. Entries are grouped within one parent folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeepBy {
    /// All entries of the parent are one group.
    Folder,
    /// The name before the last `-`: `chromium-1148` and `chromium-1150` are one browser.
    BeforeLastDash,
    /// The device model, the first word of an entry such as `iPhone15,2 17.5 (21F90)`. An
    /// entry whose first word has no comma has no model; those entries are one group.
    DeviceModel,
}

/// The keep-newest policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keep {
    None,
    /// Never offer the newest entry of each group (newest file inside, ties by name).
    Newest(KeepBy),
}

/// How the row is named.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Name {
    /// The rule's label alone, or the owner's name when it is known.
    Label,
    /// The entry's file name.
    Entry,
    /// `<label>: <entry>`.
    LabelEntry,
    /// `<label> (<entry>)`.
    LabelParen,
    /// A Hugging Face model folder `models--org--name` shown as `org/name`.
    HuggingFace,
}

/// Where a rule looks. Patterns are relative to the scan root and split on `/`. A segment is a
/// literal, `*` (any child, except names that start with a dot), `prefix*`, or several of
/// these joined with `|`.
#[derive(Clone, Copy, Debug)]
pub enum Target {
    /// Every match is one item (a folder or a file).
    Glob(&'static str),
    /// The files directly inside every folder the pattern matches are items.
    Files(&'static str),
    /// The folders directly inside this one are items; the owner comes from each name.
    CacheFolders(&'static str),
    /// The listed subfolders of the user-data folders of Electron and Chromium apps under
    /// `Library/Application Support`, including each Chrome profile.
    Chromium(&'static [&'static str]),
}

/// One row of table 6.1.
#[derive(Clone, Copy, Debug)]
pub struct Rule {
    pub id: &'static str,
    pub group: Group,
    pub class: Class,
    pub targets: &'static [Target],
    pub age: Age,
    pub owner: Owner,
    pub keep: Keep,
    /// Also keep an entry that a running process uses (its executable lies inside).
    pub in_use: bool,
    pub name: Name,
    /// The tool or app name, used by [`Name`].
    pub label: &'static str,
    /// The meta line. `{app}` is the owner or folder name and `{days}` the age in days.
    pub reason: &'static str,
    /// Names the wildcard segments never match (case-insensitive). An entry that ends with
    /// `*` is a prefix.
    pub skip: &'static [&'static str],
}

const D: Rule = Rule {
    id: "",
    group: Group::Developer,
    class: Class::Safe,
    targets: &[],
    age: Age::None,
    owner: Owner::None,
    keep: Keep::None,
    in_use: false,
    name: Name::Label,
    label: "",
    reason: "",
    skip: &[],
};

/// The subfolders that are safe to remove in Electron and Chromium apps (table 6.1).
pub const CHROMIUM_SAFE: &[&str] = &[
    "Cache",
    "Code Cache",
    "GPUCache",
    "DawnGraphiteCache",
    "DawnWebGPUCache",
    "GrShaderCache",
    "ShaderCache",
    "Crashpad/completed",
    "Service Worker/ScriptCache",
];
/// The subfolder that goes to Review.
pub const CHROMIUM_REVIEW: &[&str] = &["Service Worker/CacheStorage"];

/// `Library/Caches` names that other rules claim, or that are excluded. The generic cache rule
/// never offers them.
pub const CACHE_SKIP: &[&str] = &[
    "com.apple.*",
    "Tidy",
    "Google",
    "node-gyp",
    "Homebrew",
    "pip",
    "pnpm",
    "ms-playwright",
];

/// Targets that are never offered (table 6.1, Excluded rows): installed software, user history
/// and Tidy's own folders. Relative to the scan root, without regard to case.
pub const EXCLUDED: &[&str] = &[
    ".cargo/bin",
    ".rustup",
    ".cursor/extensions",
    ".docker",
    ".claude/projects",
    "Library/Caches/Tidy",
    "Library/Logs/Tidy",
    "Library/Application Support/Tidy",
    "/Library/Logs",
];

/// The rule for a cache folder whose owner is unknown (Review).
pub const UNKNOWN_CACHE: Rule = Rule {
    id: "cache.unknown",
    group: Group::Caches,
    class: Class::Review,
    reason: "Tidy couldn't tell which app owns this.",
    name: Name::Entry,
    ..D
};

/// Table 6.1, in the order the scan applies it. A path claimed by an earlier rule is not
/// offered again by a later one.
pub static RULES: &[Rule] = &[
    // ---------- System and app caches ----------
    Rule {
        id: "cache.chrome",
        group: Group::Caches,
        targets: &[
            Target::Glob("Library/Caches/Google/Chrome/*"),
            Target::Glob("Library/Caches/Google/Chrome-headless|Chrome for Testing-headless"),
        ],
        owner: Owner::Fixed(&owners::CHROME),
        name: Name::LabelParen,
        label: "Google Chrome",
        reason: "Browser cache, rebuilt by Chrome",
        ..D
    },
    Rule {
        id: "cache.chrome.crx",
        group: Group::Caches,
        targets: &[Target::Glob(
            "Library/Application Support/Google/Chrome/component_crx_cache|extensions_crx_cache|GraphiteDawnCache|GrShaderCache|ShaderCache",
        )],
        owner: Owner::Fixed(&owners::CHROME),
        name: Name::LabelEntry,
        label: "Google Chrome",
        reason: "Cache, Chrome downloads it again",
        ..D
    },
    Rule {
        id: "cache.electron",
        group: Group::Caches,
        targets: &[Target::Chromium(CHROMIUM_SAFE)],
        owner: Owner::Folder,
        name: Name::LabelEntry,
        reason: "Cache, rebuilt by the app",
        ..D
    },
    Rule {
        id: "cache.electron.sw",
        group: Group::Caches,
        class: Class::Review,
        targets: &[Target::Chromium(CHROMIUM_REVIEW)],
        owner: Owner::Folder,
        name: Name::LabelEntry,
        reason: "Some websites may load slower once or lose offline data.",
        ..D
    },
    Rule {
        id: "cache.app",
        group: Group::Caches,
        targets: &[
            Target::CacheFolders("Library/Caches"),
            Target::CacheFolders("Library/Application Support/Caches"),
        ],
        owner: Owner::Folder,
        name: Name::Label,
        reason: "Cache, rebuilt by the app",
        skip: CACHE_SKIP,
        ..D
    },
    // ---------- Logs ----------
    Rule {
        id: "logs.app",
        group: Group::Logs,
        targets: &[Target::Files("Library/Logs/*")],
        age: Age::Target(14),
        owner: Owner::Folder,
        name: Name::Entry,
        reason: "Log from {app}, older than 14 days",
        skip: &["DiagnosticReports", "Tidy"],
        ..D
    },
    Rule {
        id: "logs.diag",
        group: Group::Logs,
        targets: &[Target::Files("Library/Logs/DiagnosticReports")],
        age: Age::Target(30),
        name: Name::Entry,
        reason: "Crash report older than 30 days",
        ..D
    },
    // ---------- Developer ----------
    Rule {
        id: "dev.deriveddata",
        targets: &[Target::Glob("Library/Developer/Xcode/DerivedData/*")],
        age: Age::Target(7),
        owner: Owner::Fixed(&owners::XCODE),
        name: Name::LabelEntry,
        label: "Xcode build data",
        reason: "Build data, Xcode makes it again",
        ..D
    },
    Rule {
        id: "dev.previews",
        targets: &[Target::Glob("Library/Developer/Xcode/UserData/Previews")],
        owner: Owner::Fixed(&owners::XCODE),
        label: "Xcode previews",
        reason: "Preview data, Xcode makes it again",
        ..D
    },
    Rule {
        id: "dev.coresim-caches",
        targets: &[Target::Glob("Library/Developer/CoreSimulator/Caches")],
        owner: Owner::Fixed(&owners::CORE_SIMULATOR),
        label: "Simulator caches",
        reason: "Cache, rebuilt by the simulator",
        ..D
    },
    Rule {
        id: "dev.devicesupport",
        class: Class::Review,
        targets: &[Target::Glob(
            "Library/Developer/Xcode/iOS DeviceSupport|watchOS DeviceSupport|tvOS DeviceSupport|visionOS DeviceSupport|xrOS DeviceSupport/*",
        )],
        keep: Keep::Newest(KeepBy::DeviceModel),
        name: Name::LabelEntry,
        label: "Device support",
        reason: "Xcode copies it again when you connect the device, which takes a few minutes.",
        ..D
    },
    Rule {
        id: "dev.archives",
        class: Class::Review,
        targets: &[Target::Glob("Library/Developer/Xcode/Archives/*/*")],
        age: Age::Target(90),
        name: Name::LabelEntry,
        label: "Xcode archive",
        reason: "Holds the symbols for a build you shipped.",
        ..D
    },
    Rule {
        id: "dev.xcode-codingassistant",
        class: Class::ReportOnly,
        targets: &[Target::Glob("Library/Developer/Xcode/CodingAssistant")],
        label: "Xcode coding assistant",
        reason: "Unknown format.",
        ..D
    },
    Rule {
        id: "dev.simulators",
        class: Class::ReportOnly,
        targets: &[Target::Glob("Library/Developer/CoreSimulator/Devices")],
        label: "Simulator devices",
        reason: "Remove unused simulators with: xcrun simctl delete unavailable",
        ..D
    },
    Rule {
        id: "dev.npm-cacache",
        targets: &[Target::Glob(".npm/_cacache")],
        owner: Owner::Fixed(&owners::NPM),
        label: "npm cache",
        reason: "Package cache, npm downloads it again",
        ..D
    },
    Rule {
        id: "dev.npx",
        targets: &[Target::Glob(".npm/_npx")],
        owner: Owner::Fixed(&owners::NPM_OR_NPX),
        label: "npx cache",
        reason: "Package cache, npx downloads it again",
        ..D
    },
    Rule {
        id: "dev.node-gyp",
        targets: &[Target::Glob("Library/Caches/node-gyp")],
        owner: Owner::Fixed(&owners::NODE_GYP),
        label: "node-gyp cache",
        reason: "Node headers, node-gyp downloads them again",
        ..D
    },
    Rule {
        id: "dev.brew-downloads",
        targets: &[Target::Files("Library/Caches/Homebrew/downloads")],
        age: Age::Target(30),
        owner: Owner::Fixed(&owners::BREW),
        name: Name::Entry,
        label: "Homebrew download",
        reason: "Download older than 30 days",
        ..D
    },
    Rule {
        id: "dev.pip",
        targets: &[
            Target::Glob("Library/Caches/pip"),
            Target::Glob(".cache/pip"),
        ],
        owner: Owner::Fixed(&owners::PIP),
        label: "pip cache",
        reason: "Package cache, pip downloads it again",
        ..D
    },
    Rule {
        id: "dev.pnpm",
        targets: &[Target::Glob("Library/Caches/pnpm")],
        owner: Owner::Fixed(&owners::PNPM),
        label: "pnpm cache",
        reason: "Package cache, pnpm downloads it again",
        ..D
    },
    Rule {
        id: "dev.uv",
        targets: &[Target::Glob(".cache/uv")],
        owner: Owner::Fixed(&owners::UV),
        label: "uv cache",
        reason: "Package cache, uv downloads it again",
        ..D
    },
    Rule {
        id: "dev.bun",
        targets: &[Target::Glob(".bun/install/cache")],
        owner: Owner::Fixed(&owners::BUN),
        label: "Bun cache",
        reason: "Package cache, Bun downloads it again",
        ..D
    },
    Rule {
        id: "dev.gradle-caches",
        targets: &[Target::Glob(".gradle/caches")],
        owner: Owner::Fixed(&owners::GRADLE),
        label: "Gradle cache",
        reason: "Package cache, Gradle downloads it again",
        ..D
    },
    Rule {
        id: "dev.gradle-wrapper",
        class: Class::Review,
        targets: &[Target::Glob(".gradle/wrapper")],
        label: "Gradle wrapper",
        reason: "Gradle downloads it again on the next build.",
        ..D
    },
    Rule {
        id: "dev.m2",
        class: Class::Review,
        targets: &[Target::Glob(".m2/repository")],
        label: "Maven repository",
        reason: "May hold libraries you built locally with mvn install.",
        ..D
    },
    Rule {
        id: "dev.cargo-registry",
        class: Class::Review,
        targets: &[Target::Glob(".cargo/registry/cache|src")],
        owner: Owner::Fixed(&owners::CARGO),
        name: Name::LabelEntry,
        label: "Cargo registry",
        reason: "Cargo downloads crates again; offline builds fail until then.",
        ..D
    },
    Rule {
        id: "dev.cocoapods-repos",
        class: Class::Review,
        targets: &[Target::Glob(".cocoapods/repos")],
        label: "CocoaPods repos",
        reason: "CocoaPods downloads them again.",
        ..D
    },
    Rule {
        id: "dev.pnpm-store",
        class: Class::ReportOnly,
        targets: &[Target::Glob("Library/pnpm/store")],
        label: "pnpm store",
        reason: "Reclaim unused packages with: pnpm store prune",
        ..D
    },
    Rule {
        id: "dev.playwright",
        class: Class::Review,
        targets: &[Target::Glob("Library/Caches/ms-playwright/*")],
        keep: Keep::Newest(KeepBy::BeforeLastDash),
        name: Name::LabelEntry,
        label: "Playwright",
        reason: "An older browser build, Playwright downloads it again if a project needs it.",
        ..D
    },
    Rule {
        id: "dev.agent-versions",
        class: Class::Review,
        targets: &[
            Target::Glob(".local/share/claude/versions/*"),
            Target::Glob(".local/share/cursor-agent/versions/*"),
        ],
        keep: Keep::Newest(KeepBy::Folder),
        in_use: true,
        name: Name::LabelEntry,
        label: "Old version",
        reason: "An older version of the tool, kept only for rollback.",
        ..D
    },
    Rule {
        id: "dev.zed",
        class: Class::Review,
        targets: &[Target::Glob(
            "Library/Application Support/Zed/languages|node",
        )],
        owner: Owner::Fixed(&owners::ZED),
        name: Name::LabelEntry,
        label: "Zed",
        reason: "Zed downloads it again.",
        ..D
    },
    Rule {
        id: "dev.androidstudio",
        class: Class::Review,
        targets: &[Target::Glob(
            "Library/Application Support/Google/AndroidStudio*",
        )],
        keep: Keep::Newest(KeepBy::Folder),
        name: Name::Entry,
        label: "Android Studio",
        reason: "Settings of an older Android Studio version.",
        ..D
    },
    Rule {
        id: "dev.cache",
        targets: &[
            Target::Glob(".cache/firebase"),
            Target::Glob(".cache/gh"),
            Target::Glob(".cache/vscode-ripgrep"),
            Target::Glob(".cache/yt-dlp"),
        ],
        owner: Owner::Folder,
        name: Name::Label,
        reason: "Cache, rebuilt by the tool",
        ..D
    },
    // ---------- AI models and large app data ----------
    Rule {
        id: "ai.huggingface",
        group: Group::AiModels,
        class: Class::Review,
        targets: &[Target::Glob(".cache/huggingface/hub/models--*")],
        name: Name::HuggingFace,
        label: "Hugging Face",
        reason: "Model, downloaded again when a tool asks for it.",
        ..D
    },
    Rule {
        id: "ai.lmstudio",
        group: Group::AiModels,
        class: Class::Review,
        targets: &[
            Target::Glob(".cache/lm-studio/models/*/*"),
            Target::Glob(".lmstudio/models/*/*"),
        ],
        owner: Owner::Fixed(&owners::LM_STUDIO),
        name: Name::LabelEntry,
        label: "LM Studio",
        reason: "Model, downloaded again from LM Studio.",
        ..D
    },
    Rule {
        id: "ai.ollama",
        group: Group::AiModels,
        class: Class::Review,
        targets: &[Target::Glob(".ollama/models")],
        owner: Owner::Fixed(&owners::OLLAMA),
        label: "Ollama models",
        reason: "Models, downloaded again with ollama pull.",
        ..D
    },
    Rule {
        id: "ai.whisper",
        group: Group::AiModels,
        class: Class::Review,
        targets: &[Target::Glob(".cache/whisper")],
        owner: Owner::Fixed(&owners::WHISPER),
        label: "Whisper models",
        reason: "Models, downloaded again when needed.",
        ..D
    },
    Rule {
        id: "ai.fluidaudio",
        group: Group::AiModels,
        class: Class::Review,
        targets: &[
            Target::Glob(".cache/fluidaudio"),
            Target::Glob("Library/Application Support/FluidAudio"),
        ],
        owner: Owner::Fixed(&owners::FLUID_AUDIO),
        label: "FluidAudio models",
        reason: "Models, downloaded again when needed.",
        ..D
    },
    Rule {
        id: "ai.hyperframes",
        group: Group::AiModels,
        class: Class::Review,
        targets: &[Target::Glob(".cache/hyperframes/*")],
        owner: Owner::Fixed(&owners::HYPERFRAMES),
        name: Name::LabelEntry,
        label: "HyperFrames",
        reason: "Downloaded again when needed.",
        ..D
    },
    Rule {
        id: "ai.ltx",
        group: Group::AiModels,
        class: Class::Review,
        targets: &[Target::Glob(
            "Library/Application Support/LTXDesktop/models/*",
        )],
        owner: Owner::Fixed(&owners::LTX_DESKTOP),
        name: Name::LabelEntry,
        label: "LTX Desktop",
        reason: "Model, downloaded again from LTX Desktop.",
        ..D
    },
    Rule {
        id: "ai.anythingllm",
        group: Group::AiModels,
        class: Class::Review,
        targets: &[Target::Glob(
            "Library/Application Support/anythingllm-desktop/storage/models",
        )],
        owner: Owner::Fixed(&owners::ANYTHING_LLM),
        label: "AnythingLLM models",
        reason: "Models, downloaded again from AnythingLLM.",
        ..D
    },
    Rule {
        id: "ai.anythingllm-data",
        group: Group::AiModels,
        class: Class::ReportOnly,
        targets: &[Target::Glob(
            "Library/Application Support/anythingllm-desktop/storage/*",
        )],
        name: Name::LabelEntry,
        label: "AnythingLLM",
        reason: "Holds your workspaces and chats.",
        skip: &["models"],
        ..D
    },
    Rule {
        id: "ai.chrome-models",
        group: Group::AiModels,
        class: Class::Review,
        targets: &[Target::Glob(
            "Library/Application Support/Google/Chrome/OptGuideOnDeviceModel|SODALanguagePacks|Snapshots",
        )],
        owner: Owner::Fixed(&owners::CHROME),
        name: Name::LabelEntry,
        label: "Google Chrome",
        reason: "Chrome downloads it again when needed.",
        ..D
    },
    Rule {
        id: "ai.vm",
        group: Group::AiModels,
        class: Class::ReportOnly,
        targets: &[
            Target::Glob(".colima"),
            Target::Glob(".lima"),
            Target::Glob("Library/Application Support/Claude/vm_bundles"),
        ],
        label: "Virtual machine disk",
        name: Name::Entry,
        reason: "Virtual machine disk. Remove it from the app that made it.",
        ..D
    },
];

// ---------- Project build output (table 6.3) ----------

/// What must be true next to (or inside) a build output folder for it to be offered.
#[derive(Clone, Copy, Debug)]
pub enum Markers {
    /// The parent holds at least one of these names. A name that ends with `*` is a prefix
    /// (`settings.gradle*`).
    ParentHasAny(&'static [&'static str]),
    /// `Cargo.toml` in the parent, or `CACHEDIR.TAG` inside with Cargo's signature line.
    CargoTarget,
    /// `Podfile` in the parent. Safe with a `Podfile.lock`, Review without.
    Pods,
    /// `pyvenv.cfg` inside, and `pyproject.toml`, `requirements.txt` or `uv.lock` beside it.
    Venv,
    /// A `.py` file in the parent.
    PythonSource,
    /// An `.xcodeproj` or `.xcworkspace` in the parent.
    XcodeProject,
    /// One of the names in the parent, and the folder is named in the parent's `.gitignore`.
    GitIgnored(&'static [&'static str]),
    /// One of the first names in the parent, plus one of the lock files.
    WithLockfile(&'static [&'static str], &'static [&'static str]),
}

/// One row of table 6.3.
#[derive(Clone, Copy, Debug)]
pub struct ProjectRule {
    pub id: &'static str,
    /// Folder names this row applies to.
    pub names: &'static [&'static str],
    pub markers: Markers,
    pub class: Class,
    pub age: Age,
    pub owner: Owner,
    pub reason: &'static str,
}

const P: ProjectRule = ProjectRule {
    id: "",
    names: &[],
    markers: Markers::ParentHasAny(&[]),
    class: Class::Safe,
    age: Age::Project(30),
    owner: Owner::None,
    reason: "Build output, project not touched for {days} days",
};

/// Table 6.3, in match order: the first row whose name and markers fit wins, so the Android
/// `build` row comes before the generic `build` row.
pub static PROJECT_RULES: &[ProjectRule] = &[
    ProjectRule {
        id: "proj.target",
        names: &["target"],
        markers: Markers::CargoTarget,
        owner: Owner::Fixed(&owners::CARGO),
        ..P
    },
    ProjectRule {
        id: "proj.node_modules",
        names: &["node_modules"],
        markers: Markers::ParentHasAny(&["package.json"]),
        ..P
    },
    ProjectRule {
        id: "proj.web-cache",
        names: &[
            ".next",
            ".nuxt",
            ".svelte-kit",
            ".turbo",
            ".parcel-cache",
            ".angular",
        ],
        markers: Markers::ParentHasAny(&["package.json"]),
        ..P
    },
    ProjectRule {
        id: "proj.pods",
        names: &["Pods"],
        markers: Markers::Pods,
        ..P
    },
    ProjectRule {
        id: "proj.swiftpm",
        names: &[".build"],
        markers: Markers::ParentHasAny(&["Package.swift"]),
        ..P
    },
    ProjectRule {
        id: "proj.gradle",
        names: &[".gradle"],
        markers: Markers::ParentHasAny(&["build.gradle", "build.gradle.kts", "settings.gradle*"]),
        owner: Owner::Fixed(&owners::GRADLE),
        ..P
    },
    ProjectRule {
        id: "proj.gradle-build",
        names: &["build"],
        markers: Markers::ParentHasAny(&["build.gradle", "build.gradle.kts", "settings.gradle*"]),
        owner: Owner::Fixed(&owners::GRADLE),
        ..P
    },
    ProjectRule {
        id: "proj.venv",
        names: &[".venv", "venv"],
        markers: Markers::Venv,
        ..P
    },
    ProjectRule {
        id: "proj.pycache",
        names: &["__pycache__"],
        markers: Markers::PythonSource,
        age: Age::None,
        reason: "Python cache, rebuilt when the code runs",
        ..P
    },
    ProjectRule {
        id: "proj.deriveddata",
        names: &["DerivedData"],
        markers: Markers::XcodeProject,
        age: Age::Project(7),
        owner: Owner::Fixed(&owners::XCODE),
        ..P
    },
    ProjectRule {
        id: "proj.dist",
        names: &["dist"],
        markers: Markers::GitIgnored(&[
            "package.json",
            "Cargo.toml",
            "build.gradle",
            "build.gradle.kts",
            "CMakeLists.txt",
        ]),
        class: Class::Review,
        ..P
    },
    ProjectRule {
        id: "proj.build",
        names: &["build"],
        markers: Markers::GitIgnored(&[
            "package.json",
            "Cargo.toml",
            "build.gradle",
            "build.gradle.kts",
            "CMakeLists.txt",
        ]),
        class: Class::Review,
        ..P
    },
    ProjectRule {
        id: "proj.vendor",
        names: &["vendor"],
        markers: Markers::WithLockfile(
            &["composer.json", "Gemfile"],
            &["composer.lock", "Gemfile.lock"],
        ),
        class: Class::Review,
        ..P
    },
];

/// Folder names the project walk never enters, matched or not: build output is huge, and a
/// match is never walked into.
pub fn is_output_name(name: &str) -> bool {
    name == ".git" || PROJECT_RULES.iter().any(|r| r.names.contains(&name))
}

// ---------- Lookups ----------

/// The rule behind a rule id. A folded row's `.small` suffix is ignored.
pub fn rule(id: &str) -> Option<&'static Rule> {
    let id = id.strip_suffix(SMALL_SUFFIX).unwrap_or(id);
    if id == UNKNOWN_CACHE.id {
        return Some(&UNKNOWN_CACHE);
    }
    RULES.iter().find(|r| r.id == id)
}

/// The project rule behind a rule id. A folded row's `.small` suffix is ignored.
pub fn project_rule(id: &str) -> Option<&'static ProjectRule> {
    let id = id.strip_suffix(SMALL_SUFFIX).unwrap_or(id);
    PROJECT_RULES.iter().find(|r| r.id == id)
}

/// Whether `rel` (relative to the scan root) is inside an excluded target.
pub fn is_excluded_rel(rel: &std::path::Path) -> bool {
    let parts: Vec<String> = rel
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().to_ascii_lowercase()),
            _ => None,
        })
        .collect();
    if parts.len() >= 3
        && parts[0] == "library"
        && parts[1] == "caches"
        && parts[2].starts_with("com.apple.")
    {
        return true;
    }
    EXCLUDED.iter().filter(|e| !e.starts_with('/')).any(|e| {
        let e: Vec<String> = e.split('/').map(|s| s.to_ascii_lowercase()).collect();
        parts.len() >= e.len() && parts[..e.len()] == e[..]
    })
}

/// Whether a name matches one skip entry (a trailing `*` makes it a prefix).
pub fn skipped(skip: &[&str], name: &str) -> bool {
    skip.iter().any(|s| match s.strip_suffix('*') {
        Some(prefix) => name
            .to_ascii_lowercase()
            .starts_with(&prefix.to_ascii_lowercase()),
        None => s.eq_ignore_ascii_case(name),
    })
}

/// Fills `{app}` and `{days}` in a reason.
pub fn fill_reason(reason: &str, app: &str, days: Option<u64>) -> String {
    let mut s = reason.replace("{app}", app);
    if let Some(d) = days {
        s = s.replace("{days}", &d.to_string());
    }
    s
}

/// The groups' share of the table, for tests and docs: how many rules each group has.
pub fn rule_count(group: Group) -> usize {
    RULES.iter().filter(|r| r.group == group).count()
        + if group == Group::Developer {
            PROJECT_RULES.len()
        } else {
            0
        }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::path::Path;

    #[test]
    fn ids_are_unique_and_well_formed() {
        let mut seen = HashSet::new();
        for id in RULES
            .iter()
            .map(|r| r.id)
            .chain(PROJECT_RULES.iter().map(|r| r.id))
            .chain([UNKNOWN_CACHE.id])
        {
            assert!(seen.insert(id), "duplicate rule id {id}");
            assert!(!id.contains(':') && !id.is_empty(), "{id}");
        }
        for r in RULES {
            assert!(!r.reason.is_empty(), "{} has no reason", r.id);
            assert!(!r.targets.is_empty(), "{} has no target", r.id);
        }
        for r in PROJECT_RULES {
            assert!(!r.names.is_empty(), "{} has no names", r.id);
        }
    }

    #[test]
    fn ai_models_rules_are_never_safe_and_have_the_right_group() {
        for r in RULES.iter().filter(|r| r.group == Group::AiModels) {
            assert_ne!(r.class, Class::Safe, "{}", r.id);
        }
        assert!(RULES.iter().all(|r| r.group != Group::Trash));
        assert!(rule_count(Group::Developer) > 20);
    }

    #[test]
    fn report_only_and_review_rows_match_the_design() {
        let class = |id: &str| rule(id).unwrap().class;
        assert_eq!(class("dev.simulators"), Class::ReportOnly);
        assert_eq!(class("dev.pnpm-store"), Class::ReportOnly);
        assert_eq!(class("dev.xcode-codingassistant"), Class::ReportOnly);
        assert_eq!(class("ai.vm"), Class::ReportOnly);
        assert_eq!(class("dev.m2"), Class::Review);
        assert_eq!(class("dev.cargo-registry"), Class::Review);
        assert_eq!(class("dev.gradle-wrapper"), Class::Review);
        assert_eq!(class("cache.electron.sw"), Class::Review);
        assert_eq!(class("cache.app"), Class::Safe);
        assert_eq!(class("dev.deriveddata"), Class::Safe);
        assert_eq!(rule("dev.deriveddata").unwrap().age, Age::Target(7));
        assert_eq!(rule("logs.app").unwrap().age, Age::Target(14));
        assert_eq!(rule("logs.diag").unwrap().age, Age::Target(30));
        assert_eq!(rule("dev.archives").unwrap().age, Age::Target(90));
        assert_eq!(rule("dev.brew-downloads").unwrap().age, Age::Target(30));
    }

    #[test]
    fn a_folded_row_finds_its_rule() {
        assert_eq!(rule("logs.app.small").unwrap().id, "logs.app");
        assert_eq!(
            project_rule("proj.pycache.small").unwrap().id,
            "proj.pycache"
        );
        assert_eq!(rule("cache.unknown").unwrap().class, Class::Review);
        assert!(rule("nope").is_none());
    }

    #[test]
    fn project_rules_come_in_match_order() {
        let pos = |id: &str| PROJECT_RULES.iter().position(|r| r.id == id).unwrap();
        assert!(pos("proj.gradle-build") < pos("proj.build"));
        assert!(is_output_name("node_modules"));
        assert!(is_output_name(".git"));
        assert!(!is_output_name("src"));
        assert_eq!(project_rule("proj.pycache").unwrap().age, Age::None);
        assert_eq!(
            project_rule("proj.deriveddata").unwrap().age,
            Age::Project(7)
        );
        assert_eq!(project_rule("proj.dist").unwrap().class, Class::Review);
    }

    #[test]
    fn excluded_targets_are_recognized() {
        for rel in [
            ".cargo/bin/cargo",
            ".rustup",
            ".cursor/extensions/x",
            ".docker/config.json",
            ".claude/projects/a",
            "Library/Caches/com.apple.Safari",
            "Library/Caches/Tidy/last-large.json",
        ] {
            assert!(is_excluded_rel(Path::new(rel)), "{rel}");
        }
        for rel in [
            ".cargo/registry",
            "Library/Caches/com.example.Idle",
            "Library/Application Support/com.apple.foo",
            ".claude/settings.json",
        ] {
            assert!(!is_excluded_rel(Path::new(rel)), "{rel}");
        }
    }

    #[test]
    fn skip_lists_match_names_and_prefixes() {
        assert!(skipped(CACHE_SKIP, "com.apple.Safari"));
        assert!(skipped(CACHE_SKIP, "COM.APPLE.x"));
        assert!(skipped(CACHE_SKIP, "google"));
        assert!(!skipped(CACHE_SKIP, "com.example.Idle"));
        assert!(!skipped(CACHE_SKIP, "pipx"));
    }

    #[test]
    fn reasons_fill_their_placeholders() {
        assert_eq!(
            fill_reason("Log from {app}, older than 14 days", "Slack", None),
            "Log from Slack, older than 14 days"
        );
        assert_eq!(
            fill_reason(
                "Build output, project not touched for {days} days",
                "",
                Some(41)
            ),
            "Build output, project not touched for 41 days"
        );
    }
}
