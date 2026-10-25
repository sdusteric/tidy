//! Tidy's GPUI user interface. It talks to the core only through `tidy_core::api` commands and
//! events: the app sends [`tidy_core::Command`]s and shows the [`tidy_core::Event`]s that come
//! back. It never moves a file itself.

pub mod app;
pub mod assets;
pub mod demo;
pub mod format;
pub mod macos;
pub mod motion;
pub mod theme;
pub mod updates;
pub mod views;

#[cfg(test)]
mod tests;

pub use app::{
    CloseWindow, Core, MainWindow, Model, OpenKind, Opener, Quit, Screen, TidyModel, Tokio,
    graceful_shutdown, install, install_demo, quit_on_signals, reopen, spawn_pump, start_heartbeat,
};
pub use assets::AppAssets;
pub use views::main_window::{TidyView, open_main_window};

/// Registers Tidy's key bindings, actions, menus and quit hook. Call once, after
/// `gpui_kit::init(cx)`.
pub fn init(cx: &mut gpui_kit::App) {
    app::init(cx);
}
