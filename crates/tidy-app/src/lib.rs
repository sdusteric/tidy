//! Tidy's GPUI user interface. It talks to the core only through `tidy_core::api` commands and
//! events: the app sends [`tidy_core::Command`]s and shows the [`tidy_core::Event`]s that come
//! back. It never moves a file itself.

pub mod app;
pub mod assets;
pub mod demo;
pub mod format;
pub mod macos;
pub mod motion;
pub mod open_urls;
pub mod theme;
pub mod treemap;
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

/// Runs the quick actions in URLs macOS handed the app. `tidy://checkup` brings the main window
/// back (like a Dock click), selects Check up and starts it; the check up only reads and never
/// cleans. `tidy://check-for-updates` takes the menu item's path. Returns how many ran.
pub fn handle_open_urls(urls: &[String], cx: &mut gpui_kit::App) -> usize {
    let actions = open_urls::actions_for_open_urls(urls);
    for action in &actions {
        match action {
            open_urls::Action::Checkup => {
                open_main_window(cx);
                let model = app::model(cx);
                model.update(cx, |m, cx| {
                    m.select_screen(Screen::Checkup, cx);
                    m.run_checkup(cx);
                });
            }
            open_urls::Action::CheckForUpdates => updates::check_for_updates(cx),
        }
    }
    actions.len()
}
