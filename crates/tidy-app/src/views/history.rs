//! The History sheet (design sections 5.1 and 11): 560 x 480 pt, one 44 pt row per entry (date,
//! action and counts, size), expandable to a list of 32 pt item rows. Put Back is Finder's job,
//! so there is no such button in M1 (C9). Items still at their original path offer Reveal.

use gpui_kit::assets::IconName;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use tidy_core::model::{Action, HistoryEntry, HistoryItem, ItemResult};

use crate::app::{OpenKind, open_path};
use crate::format;
use crate::theme::{MONO_FONT, Theme, metrics, tnum};
use crate::views::main_window::TidyView;
use crate::views::{hairline, icon_button};

/// "Cleanup", "Large files" or "Emptied the Trash".
pub fn action_label(action: Action) -> &'static str {
    match action {
        Action::Clean => "Cleanup",
        Action::Trash => "Large files",
        Action::EmptyTrash => "Emptied the Trash",
    }
}

/// The second line of an entry: "Cleanup, 12 items, 3 skipped".
pub fn entry_line(e: &HistoryEntry) -> String {
    let mut line = format!(
        "{}, {}",
        action_label(e.action),
        format::items(e.items.len())
    );
    let skipped = e.skipped();
    if skipped > 0 {
        line.push_str(&format!(", {skipped} skipped"));
    }
    line
}

/// What happened to one item, as a short phrase.
pub fn result_label(item: &HistoryItem) -> String {
    let with_reason = |head: &str| match &item.reason {
        Some(r) if !r.is_empty() => format!("{head}: {r}"),
        _ => head.to_string(),
    };
    match item.result {
        ItemResult::Trashed => "In the Trash".to_string(),
        ItemResult::Skipped => with_reason("Skipped"),
        ItemResult::Failed => with_reason("Failed"),
        ItemResult::Deleted => "Deleted".to_string(),
        ItemResult::Missing => "Already gone".to_string(),
    }
}

impl TidyView {
    pub fn toggle_history_entry(&mut self, id: &str, cx: &mut Context<Self>) {
        if !self.history_expanded.remove(id) {
            self.history_expanded.insert(id.to_string());
        }
        cx.notify();
    }

    pub(crate) fn render_history(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity().downgrade();
        let close_view = view.clone();
        let scrim_view = view.clone();
        let (entries, present) = {
            let h = &self.model.read(cx).history;
            (h.entries.clone(), h.present.clone())
        };
        let root = self
            .model
            .read(cx)
            .cleanup
            .plan
            .as_ref()
            .map(|p| p.root.clone())
            .unwrap_or_default();
        let hover = t.row_hover;

        let body: AnyElement = match entries {
            None => message(t, "Loading...").into_any_element(),
            Some(list) if list.is_empty() => {
                message(t, "Nothing has been cleaned yet.").into_any_element()
            }
            Some(list) => {
                let mut rows: Vec<AnyElement> = Vec::new();
                for (n, e) in list.iter().rev().enumerate() {
                    let open = self.history_expanded.contains(&e.id);
                    let toggle = view.clone();
                    let id = e.id.clone();
                    rows.push(
                        div()
                            .id(("history-entry", n))
                            .debug_selector(move || format!("history-entry-{n}"))
                            .flex_none()
                            .h(px(44.))
                            .px(px(16.))
                            .gap(px(12.))
                            .flex()
                            .items_center()
                            .cursor_pointer()
                            .hover(move |s| s.bg(hover))
                            .on_click(move |_, _, cx| {
                                let id = id.clone();
                                let _ = toggle.update(cx, |v, cx| v.toggle_history_entry(&id, cx));
                            })
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .text_size(px(13.))
                                            .line_height(px(17.))
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(t.text)
                                            .whitespace_nowrap()
                                            .child(format::date_time(e.at)),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(12.))
                                            .line_height(px(16.))
                                            .text_color(t.text_2)
                                            .whitespace_nowrap()
                                            .child(entry_line(e)),
                                    ),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(px(13.))
                                    .font_features(tnum())
                                    .text_color(t.text_2)
                                    .child(format::size(e.trashed_bytes)),
                            )
                            .into_any_element(),
                    );
                    if open {
                        for (k, item) in e.items.iter().enumerate() {
                            rows.push(history_item(t, n, k, item, &root, &present));
                        }
                    }
                }
                div()
                    .id("history-list")
                    .debug_selector(|| "history-list".to_string())
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .py(px(4.))
                    .children(rows)
                    .into_any_element()
            }
        };

        let card = div()
            .id("history-card")
            .debug_selector(|| "history".to_string())
            .w(px(metrics::HISTORY_W))
            .h(px(metrics::HISTORY_H))
            .max_w_full()
            .max_h_full()
            .rounded(px(12.))
            .bg(t.pop_bg)
            .shadow(t.pop_shadow())
            .overflow_hidden()
            .flex()
            .flex_col()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex_none()
                    .h(px(44.))
                    .pl(px(16.))
                    .pr(px(10.))
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(13.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(t.text)
                            .child("History"),
                    )
                    .child(icon_button(
                        "history-close",
                        "history-close".to_string(),
                        IconName::X,
                        t,
                        move |_, cx| {
                            let _ = close_view.update(cx, |v, cx| {
                                v.history_open = false;
                                cx.notify();
                            });
                        },
                    )),
            )
            .child(hairline(t))
            .child(body);

        div()
            .id("history-scrim")
            .absolute()
            .inset_0()
            .bg(t.scrim())
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                let _ = scrim_view.update(cx, |v, cx| {
                    v.history_open = false;
                    cx.notify();
                });
            })
            .child(card)
            .into_any_element()
    }
}

fn message(t: &Theme, text: &'static str) -> Div {
    div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(13.))
        .text_color(t.text_2)
        .child(text)
}

/// A 32 pt item row: the original path in mono, the size, the result, and Reveal when the item
/// is still where it was.
fn history_item(
    t: &Theme,
    entry: usize,
    k: usize,
    item: &HistoryItem,
    root: &std::path::Path,
    present: &std::collections::HashSet<std::path::PathBuf>,
) -> AnyElement {
    let path = item.path.clone();
    let reveal = present.contains(&item.path);
    let shown = format::middle_truncate(&format::display_path(&item.path, root), 44);
    div()
        .id(("history-item", entry * 10_000 + k))
        .debug_selector(move || format!("history-item-{entry}-{k}"))
        .flex_none()
        .h(px(32.))
        .pl(px(32.))
        .pr(px(16.))
        .gap(px(12.))
        .flex()
        .items_center()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .font_family(MONO_FONT)
                .text_size(px(12.))
                .text_color(t.text_2)
                .whitespace_nowrap()
                .child(shown),
        )
        .child(
            div()
                .flex_none()
                .text_size(px(12.))
                .font_features(tnum())
                .text_color(t.text_2)
                .whitespace_nowrap()
                .child(format::size(item.bytes)),
        )
        .child(
            div()
                .flex_none()
                .max_w(px(160.))
                .overflow_hidden()
                .text_ellipsis()
                .text_size(px(12.))
                .text_color(match item.result {
                    ItemResult::Skipped | ItemResult::Failed => t.warning,
                    _ => t.text_2,
                })
                .whitespace_nowrap()
                .child(result_label(item)),
        )
        .when(reveal, |d| {
            d.child(icon_button(
                ("history-reveal", entry * 10_000 + k),
                format!("history-reveal-{entry}-{k}"),
                IconName::FolderSearch,
                t,
                move |_, cx| open_path(OpenKind::Reveal, &path, cx),
            ))
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    use std::path::PathBuf;
    use tidy_core::model::{Source, TrasherKind};

    fn entry(skipped: bool) -> HistoryEntry {
        let item = |result, reason: Option<&str>| HistoryItem {
            path: PathBuf::from("/fake/x"),
            trash_path: None,
            bytes: 10,
            rule: "r".into(),
            result,
            reason: reason.map(str::to_string),
            trash_id: None,
        };
        let mut items = vec![item(ItemResult::Trashed, None)];
        if skipped {
            items.push(item(ItemResult::Skipped, Some("Google Chrome opened")));
        }
        HistoryEntry {
            id: "2026-10-23T14:02:11Z-1".into(),
            at: 1_792_764_131,
            action: Action::Clean,
            source: Source::Cleanup,
            trasher: TrasherKind::Dir,
            items,
            trashed_bytes: 10,
            message: None,
        }
    }

    #[test]
    fn lines_count_items_and_skips() {
        assert_eq!(entry_line(&entry(false)), "Cleanup, 1 item");
        assert_eq!(entry_line(&entry(true)), "Cleanup, 2 items, 1 skipped");
        let mut e = entry(false);
        e.action = Action::Trash;
        assert!(entry_line(&e).starts_with("Large files"));
        e.action = Action::EmptyTrash;
        assert!(entry_line(&e).starts_with("Emptied the Trash"));
    }

    #[test]
    fn results_read_as_phrases() {
        let e = entry(true);
        assert_eq!(result_label(&e.items[0]), "In the Trash");
        assert_eq!(result_label(&e.items[1]), "Skipped: Google Chrome opened");
    }
}
