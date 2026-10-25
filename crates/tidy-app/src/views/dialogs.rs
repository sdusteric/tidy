//! Confirm dialogs (design section 11): 420 pt wide, a title, one sentence, the items in a
//! scrollable box up to 200 pt high, and Cancel plus the action at the right. Return confirms
//! and Escape cancels.
//!
//! The dialog is drawn inside the main window's view over a scrim, so it is part of the view
//! state and the tests can see what it says. What a dialog asks for is plain data
//! ([`DialogSpec`]); the view turns a confirm into a `Command`.

use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use tidy_core::model::{Class, Item as CleanItem, TrashSummary};

use crate::app::LargeEntry;
use crate::format;
use crate::theme::{Theme, metrics, tnum};
use crate::views::overlay::{Phase, Sheet, present};

/// Shown when any selected item lives under `~/Library` (C10).
pub const APP_DATA_NOTE: &str =
    "Some of these belong to apps. The app may stop working or download them again.";
/// Shown when the selection is over 1 GB (C15).
pub const CLONE_NOTE: &str = "Copies made by Finder can share space, so this may free less";
/// "Over 1 GB" in bytes, as Finder counts.
pub const CLONE_NOTE_MIN: u64 = 1_000_000_000;

/// What a confirmed dialog does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DialogKind {
    /// Clean with Review items ticked: sends `Clean` for the ticked items.
    CleanReview,
    /// Move these Large files rows (by `rel`) to the Trash.
    TrashLarge { rels: Vec<String> },
    /// Delete the items Tidy moved to the Trash.
    EmptyTrash,
    /// Force Quit the Memory row with this id (design section 5.3).
    ForceQuit { id: String },
    /// Move the launch agent with this key to the Trash (design section 5.4).
    TrashLoginItem { key: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DialogSpec {
    pub kind: DialogKind,
    pub title: String,
    pub body: Option<String>,
    /// (name, size) per item.
    pub rows: Vec<(String, String)>,
    pub notes: Vec<String>,
    pub confirm: String,
    pub destructive: bool,
}

impl DialogSpec {
    /// The dialog for "Clean selected" with Review items ticked. `ticked` is every ticked item;
    /// the Review ones are listed by name and size.
    pub fn review(ticked: &[&CleanItem]) -> DialogSpec {
        let review: Vec<&&CleanItem> = ticked.iter().filter(|i| i.class == Class::Review).collect();
        let safe: Vec<&&CleanItem> = ticked.iter().filter(|i| i.class == Class::Safe).collect();
        let mut notes = Vec::new();
        if !safe.is_empty() {
            let bytes: u64 = safe.iter().map(|i| i.bytes_alloc).sum();
            notes.push(format!(
                "{} that are safe to remove ({}) go with them.",
                format::items(safe.len()),
                format::size(bytes)
            ));
        }
        DialogSpec {
            kind: DialogKind::CleanReview,
            title: "Move these to the Trash?".to_string(),
            body: Some("You can put them back from the Trash until you empty it.".to_string()),
            rows: review
                .iter()
                .map(|i| (i.name.clone(), format::size(i.bytes_alloc)))
                .collect(),
            notes,
            confirm: "Move to Trash".to_string(),
            destructive: false,
        }
    }

    /// The dialog for Move to Trash on Large files. Large files are user data, so every move
    /// asks first, even for one item (C14).
    pub fn trash(entries: &[LargeEntry]) -> DialogSpec {
        let total: u64 = entries.iter().map(|e| e.bytes).sum();
        let mut notes = Vec::new();
        if entries.iter().any(|e| e.app_data) {
            notes.push(APP_DATA_NOTE.to_string());
        }
        if total > CLONE_NOTE_MIN {
            notes.push(CLONE_NOTE.to_string());
        }
        DialogSpec {
            kind: DialogKind::TrashLarge {
                rels: entries.iter().map(|e| e.rel.clone()).collect(),
            },
            title: if entries.len() == 1 {
                "Move this to the Trash?".to_string()
            } else {
                format!("Move these {} items to the Trash?", entries.len())
            },
            body: Some("You can put them back from the Trash until you empty it.".to_string()),
            rows: entries
                .iter()
                .map(|e| (e.name.clone(), format::size(e.bytes)))
                .collect(),
            notes,
            confirm: "Move to Trash".to_string(),
            destructive: false,
        }
    }

    /// The dialog for Force Quit: unsaved changes are lost, so it uses the destructive style.
    pub fn force_quit(id: &str, name: &str) -> DialogSpec {
        DialogSpec {
            kind: DialogKind::ForceQuit { id: id.to_string() },
            title: format!("Force Quit {name}?"),
            body: Some(format!("Unsaved changes in {name} will be lost.")),
            rows: Vec::new(),
            notes: Vec::new(),
            confirm: "Force Quit".to_string(),
            destructive: true,
        }
    }

    /// The dialog for Move to Trash on a user launch agent. Only the plist moves; the program
    /// it starts stays where it is.
    pub fn trash_login_item(key: &str, label: &str) -> DialogSpec {
        DialogSpec {
            kind: DialogKind::TrashLoginItem {
                key: key.to_string(),
            },
            title: "Move this login item to the Trash?".to_string(),
            body: Some(format!(
                "Tidy moves the launch agent file for {label} to the Trash. The program it starts stays where it is. You can put the file back from the Trash until you empty it."
            )),
            rows: Vec::new(),
            notes: Vec::new(),
            confirm: "Move to Trash".to_string(),
            destructive: false,
        }
    }

    /// The dialog for Empty Trash: permanent, so it uses the danger button.
    pub fn empty_trash(summary: TrashSummary) -> DialogSpec {
        DialogSpec {
            kind: DialogKind::EmptyTrash,
            title: "Empty the Trash?".to_string(),
            body: Some(format!(
                "Permanently delete the {} Tidy moved to the Trash ({})? This can't be undone.",
                format::items(summary.items as usize),
                format::size(summary.bytes)
            )),
            rows: Vec::new(),
            notes: Vec::new(),
            confirm: "Empty Trash".to_string(),
            destructive: true,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ButtonKind {
    Secondary,
    Primary,
    Danger,
}

fn dialog_button(
    id: &'static str,
    label: String,
    kind: ButtonKind,
    t: &Theme,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let base = div()
        .id(id)
        .debug_selector(|| id.to_string())
        .flex_none()
        .h(px(24.))
        .px(px(12.))
        .rounded(px(6.))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(12.))
        .font_weight(FontWeight::MEDIUM)
        .whitespace_nowrap()
        .cursor_pointer()
        .child(label)
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, window, cx| on_click(window, cx));
    let pressed = t.row_pressed;
    match kind {
        ButtonKind::Secondary => base
            .bg(t.button_fill())
            .text_color(t.text)
            .shadow(t.button_shadow())
            .active(move |s| s.bg(pressed)),
        ButtonKind::Primary => base
            .bg(t.accent)
            .text_color(gpui_kit::white())
            .active(|s| s.opacity(0.9)),
        ButtonKind::Danger => base
            .bg(t.danger_button)
            .text_color(gpui_kit::white())
            .active(|s| s.opacity(0.9)),
    }
}

/// A scrim over the whole window with the dialog card centered on it. A click on the scrim
/// cancels.
pub fn render_dialog(
    spec: &DialogSpec,
    t: &Theme,
    phase: Phase,
    on_cancel: impl Fn(&mut Window, &mut App) + Clone + 'static,
    on_confirm: impl Fn(&mut Window, &mut App) + 'static,
) -> AnyElement {
    let cancel_scrim = on_cancel.clone();
    // A leaving copy only fades: it takes no clicks.
    let leaving = matches!(phase, Phase::Leave(_));
    let list = (!spec.rows.is_empty()).then(|| {
        div()
            .id("dialog-list")
            .debug_selector(|| "dialog-list".to_string())
            .max_h(px(200.))
            .overflow_y_scroll()
            .rounded(px(8.))
            .bg(t.fill)
            .px(px(10.))
            .py(px(6.))
            .flex()
            .flex_col()
            .gap(px(3.))
            .text_size(px(12.))
            .font_features(tnum())
            .children(spec.rows.iter().map(|(name, size)| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_color(t.text)
                            .child(name.clone()),
                    )
                    .child(div().flex_none().text_color(t.text_2).child(size.clone()))
            }))
    });
    let card = div()
        .id("dialog-card")
        .debug_selector(|| "dialog".to_string())
        .w(px(metrics::DIALOG_W))
        .max_w_full()
        .p(px(16.))
        .rounded(px(12.))
        .bg(t.pop_bg)
        .shadow(t.pop_shadow())
        .flex()
        .flex_col()
        .gap(px(12.))
        // A click inside the card is not a click on the scrim.
        .when(!leaving, |d| {
            d.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        })
        .child(
            div()
                .debug_selector(|| "dialog-title".to_string())
                .text_size(px(13.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.text)
                .child(spec.title.clone()),
        )
        .children(spec.body.clone().map(|body| {
            div()
                .debug_selector(|| "dialog-body".to_string())
                .text_size(px(13.))
                .line_height(px(18.))
                .text_color(t.text)
                .child(body)
        }))
        .children(list)
        .children(spec.notes.iter().map(|note| {
            div()
                .text_size(px(12.))
                .line_height(px(16.))
                .text_color(t.text_2)
                .child(note.clone())
        }))
        .child(
            div()
                .flex()
                .justify_end()
                .gap(px(8.))
                .child(dialog_button(
                    "dialog-cancel",
                    "Cancel".to_string(),
                    ButtonKind::Secondary,
                    t,
                    on_cancel,
                ))
                .child(dialog_button(
                    "dialog-confirm",
                    spec.confirm.clone(),
                    if spec.destructive {
                        ButtonKind::Danger
                    } else {
                        ButtonKind::Primary
                    },
                    t,
                    on_confirm,
                )),
        );
    let scrim = div()
        .id("dialog-scrim")
        .absolute()
        .inset_0()
        .bg(t.scrim())
        .flex()
        .items_center()
        .justify_center()
        .when(!leaving, |d| {
            d.on_mouse_down(MouseButton::Left, move |_, window, cx| {
                cancel_scrim(window, cx)
            })
        });
    present(Sheet::Dialog, scrim, card, phase)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    use std::path::PathBuf;
    use tidy_core::model::{Group, LargeKind, UseSource};

    fn item(name: &str, class: Class, bytes: u64) -> CleanItem {
        CleanItem {
            id: format!("r:{name}"),
            group: Group::Caches,
            class,
            ticked: class == Class::Safe,
            blocked: None,
            name: name.into(),
            reason: "x".into(),
            path: PathBuf::from("/x").join(name),
            bytes_alloc: bytes,
            bytes_logical: bytes,
            newest_mtime: None,
            members: vec![],
        }
    }

    fn entry(name: &str, bytes: u64, app_data: bool) -> LargeEntry {
        LargeEntry {
            rel: name.into(),
            name: name.into(),
            path: PathBuf::from("/x").join(name),
            kind: LargeKind::File,
            bytes,
            last_used: None,
            source: UseSource::FileTimes,
            app_data,
            files: 0,
            share: None,
            drill: false,
        }
    }

    #[test]
    fn review_dialog_lists_review_items_by_name_and_size() {
        let a = item("Hugging Face", Class::Review, 2_000_000_000);
        let b = item("Chrome", Class::Safe, 640_000_000);
        let spec = DialogSpec::review(&[&a, &b]);
        assert_eq!(spec.title, "Move these to the Trash?");
        assert_eq!(
            spec.body.as_deref(),
            Some("You can put them back from the Trash until you empty it.")
        );
        assert_eq!(
            spec.rows,
            vec![("Hugging Face".to_string(), "2 GB".to_string())]
        );
        assert_eq!(spec.confirm, "Move to Trash");
        assert!(!spec.destructive);
        assert_eq!(spec.notes.len(), 1);
        assert!(spec.notes[0].contains("1 item"));
    }

    #[test]
    fn trash_dialog_adds_the_notes_it_needs() {
        let small = DialogSpec::trash(&[entry("a.mov", 700_000_000, false)]);
        assert!(small.notes.is_empty());
        let app = DialogSpec::trash(&[entry("Library/x", 700_000_000, true)]);
        assert_eq!(app.notes, vec![APP_DATA_NOTE.to_string()]);
        let big = DialogSpec::trash(&[entry("a.mov", 1_000_000_001, false)]);
        assert_eq!(big.notes, vec![CLONE_NOTE.to_string()]);
        let both = DialogSpec::trash(&[
            entry("a", 800_000_000, true),
            entry("b", 800_000_000, false),
        ]);
        assert_eq!(both.notes.len(), 2);
        assert_eq!(both.title, "Move these 2 items to the Trash?");
        // Exactly 1 GB is not over 1 GB.
        assert!(
            DialogSpec::trash(&[entry("a", 1_000_000_000, false)])
                .notes
                .is_empty()
        );
    }

    #[test]
    fn empty_trash_dialog_is_destructive_and_says_what_it_deletes() {
        let spec = DialogSpec::empty_trash(TrashSummary {
            items: 214,
            bytes: 2_100_000_000,
        });
        assert!(spec.destructive);
        assert_eq!(spec.confirm, "Empty Trash");
        assert_eq!(
            spec.body.as_deref(),
            Some(
                "Permanently delete the 214 items Tidy moved to the Trash (2.1 GB)? This can't be undone."
            )
        );
    }

    #[test]
    fn dialog_copy_has_no_em_dashes() {
        let specs = [
            DialogSpec::review(&[&item("a", Class::Review, 1)]),
            DialogSpec::trash(&[entry("a", 2_000_000_000, true)]),
            DialogSpec::empty_trash(TrashSummary { items: 1, bytes: 1 }),
        ];
        for s in specs {
            let all = format!("{} {:?} {:?} {}", s.title, s.body, s.notes, s.confirm);
            assert!(!all.contains('\u{2014}'));
        }
    }
}
