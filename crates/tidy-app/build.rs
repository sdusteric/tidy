//! Links Sparkle.framework (auto-update) into the app. Only this crate links it: tidy-core and
//! tidyctl never do.
//!
//! The framework comes from the app-updates repo next to this one,
//! `../app-updates/vendor/Sparkle-2.10.0/Sparkle.framework`, or from the path in
//! `SPARKLE_FRAMEWORK`. scripts/bundle.sh copies the same framework into
//! `Tidy.app/Contents/Frameworks`, which the binary finds through the rpath
//! `@executable_path/../Frameworks`. Debug builds (tests, `cargo run`) also get an rpath to the
//! framework's own folder, so they start outside a bundle; release builds never carry that path.

use std::path::PathBuf;

const ENV: &str = "SPARKLE_FRAMEWORK";
const DEFAULT: &str = "../../../app-updates/vendor/Sparkle-2.10.0/Sparkle.framework";

fn main() {
    println!("cargo:rerun-if-env-changed={ENV}");
    println!("cargo:rerun-if-changed=build.rs");

    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default());
    let framework = match std::env::var_os(ENV).filter(|v| !v.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => manifest.join(DEFAULT),
    };
    let binary = framework.join("Versions/B/Sparkle");
    if !binary.is_file() {
        eprintln!(
            "error: Sparkle.framework not found at {}\n\
             tidy-app links Sparkle 2.10.0 for auto-update. Check out the app-updates repo next \
             to this one (../app-updates/vendor/Sparkle-2.10.0/Sparkle.framework) or set {ENV} \
             to the path of a Sparkle.framework.",
            framework.display()
        );
        std::process::exit(1);
    }
    let framework = framework.canonicalize().unwrap_or(framework);
    let dir = framework
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    println!(
        "cargo:rerun-if-changed={}",
        framework.join("Versions/B/Sparkle").display()
    );
    println!("cargo:rustc-link-search=framework={}", dir.display());
    println!("cargo:rustc-link-lib=framework=Sparkle");
    println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path/../Frameworks");
    if std::env::var("PROFILE").as_deref() == Ok("debug") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", dir.display());
    }
}
