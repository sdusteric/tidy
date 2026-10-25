//! The main window (design section 11): a sidebar on the left, the selected screen on the right.
//! This file holds the view, the chrome the screens share (toolbar, notice band, status bar,
//! empty state) and the overlays (menus and dialogs). The screens live in `checkup.rs`,
//! `cleanup.rs`, `large.rs`, `memory.rs` and `security.rs`.

use std::collections::HashSet;

use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use tidy_core::api::Command;

use crate::app::{
    DialogCancel, DialogConfirm, GoCheckup, GoCleanup, GoLarge, GoMemory, GoSecurity,
    MEMORY_PERIOD, MainWindow, Notice, ParentFolder, ROOT_CONTEXT, RUNNING_PERIOD, Rescan, Screen,
    TidyModel, TrashSelection, background_mode, model, send,
};
use crate::format;
use crate::theme::{Theme, metrics, motion as m, tnum};
use crate::views::cleanup::RowMenu;
use crate::views::dialogs::{DialogKind, DialogSpec, render_dialog};
use crate::views::memory::MemoryMenu;
use crate::views::overlay::{self, Phase, Sheet};
use crate::views::trust::DetailsAt;
use crate::views::{hairline, icon_button, large_button, text_button};

pub struct TidyView {
    pub model: Entity<TidyModel>,
    pub(crate) focus: FocusHandle,
    pub(crate) cleanup_scroll: UniformListScrollHandle,
    pub(crate) large_scroll: UniformListScrollHandle,
    pub(crate) memory_scroll: UniformListScrollHandle,
    pub(crate) security_scroll: ScrollHandle,
    pub(crate) checkup_scroll: ScrollHandle,
    pub(crate) sysdata_scroll: ScrollHandle,
    /// The row under the pointer (its key), which shows its hover actions.
    pub hovered: Option<String>,
    /// The "..." menu of a Cleanup row, when open.
    pub menu: Option<RowMenu>,
    /// The threshold menu, when open: where it was clicked.
    pub threshold_menu: Option<Point<Pixels>>,
    /// The "..." menu of a Memory row (Force Quit), when open.
    pub memory_menu: Option<MemoryMenu>,
    /// The details popover of a Cleanup item, when open (design 17.1).
    pub details: Option<DetailsAt>,
    /// The command whose Copy button was clicked last ("Copied").
    pub copied: Option<String>,
    /// The confirm dialog, when open.
    pub dialog: Option<DialogSpec>,
    pub history_open: bool,
    /// History entries (by id) whose item list is open.
    pub history_expanded: HashSet<String>,
    /// The figure each Check up card showed last (Cleanup, Security, Memory), to notice when
    /// one goes from "Checking" to a result.
    pub(crate) card_figures: [String; 3],
    /// When each card's result arrived, with the generation that keys its fade-in.
    pub(crate) card_changed: [Option<(std::time::Instant, usize)>; 3],
    /// Counts the animations started from this view, so each gets a fresh element id.
    pub(crate) card_generation: usize,
    /// When the reminder switch was last clicked, with the generation keying the knob slide.
    pub(crate) switch_flipped: Option<(std::time::Instant, usize)>,
    /// The same for the automatic update switch.
    pub(crate) updates_flipped: Option<(std::time::Instant, usize)>,
    /// The overlays' enter and exit motion (dialogs, History, System Data, the popover).
    pub(crate) overlays: crate::views::overlay::OverlayState,
    /// Set on a mouse-down on empty toolbar space; the first move then drags the window.
    should_move: bool,
    /// Whether macOS is showing any part of the window. False while it is minimized, hidden,
    /// fully covered or on another Space, and then the Memory screen does not poll.
    window_visible: bool,
    _subs: Vec<Subscription>,
}

impl TidyView {
    pub fn new(model: Entity<TidyModel>, window: &mut Window, cx: &mut Context<Self>) -> TidyView {
        let observed = cx.observe(&model, |_, _, cx| cx.notify());
        let appearance = cx.observe_window_appearance(window, |_, window, cx| {
            gpui_kit::component::theme::Theme::sync_system_appearance(Some(window), cx);
            crate::theme::sync_component_tokens_for(window, cx);
            cx.notify();
        });
        // macOS posts no Reduce Motion change Base can follow, so re-read it whenever the
        // window becomes active.
        let activation = cx.observe_window_activation(window, |_, window, cx| {
            if window.is_window_active() {
                gpui_kit::base::apply_system_reduce_motion(cx);
            }
        });
        // The root element must hold focus, or the window's key bindings do nothing.
        let focus = cx.focus_handle();
        window.focus(&focus, cx);

        let demo = crate::demo::state(cx).is_some();
        let visibility = cx.observe_window_visibility(window, move |v, visibility, _, cx| {
            v.window_visible = visibility.is_visible();
            // Back in view on Memory: read now rather than at the next tick.
            if !demo && v.window_visible && v.model.read(cx).screen == Screen::Memory {
                v.model.update(cx, |m, cx| m.snapshot_memory(cx));
            }
        });
        if !demo {
            // While the Cleanup screen is visible, re-check which apps are running (C6). It
            // costs nothing when no item is blocked.
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(RUNNING_PERIOD).await;
                    let alive = this.update(cx, |v, cx| {
                        let m = v.model.read(cx);
                        if m.screen == Screen::Cleanup && m.watches_running() {
                            send(cx, Command::CheckRunning);
                        }
                    });
                    if alive.is_err() {
                        break;
                    }
                }
            })
            .detach();
            // The Memory screen is the only one that polls: a snapshot every 2 seconds while it
            // is the visible screen in a window macOS is showing, nothing otherwise. The loop
            // ends with the window.
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(MEMORY_PERIOD).await;
                    let alive = this.update(cx, |v, cx| {
                        if v.window_visible && v.model.read(cx).screen == Screen::Memory {
                            v.model.update(cx, |m, cx| m.snapshot_memory(cx));
                        }
                    });
                    if alive.is_err() {
                        break;
                    }
                }
            })
            .detach();
            // The window may open on a screen that reads something when it is shown.
            model.update(cx, |m, cx| m.on_view_opened(cx));
        }
        // A demo can't scroll its hidden window, so it starts the Security list scrolled.
        let security_scroll = ScrollHandle::new();
        if let Some(d) = crate::demo::state(cx).filter(|d| d.scroll_y > 0.) {
            security_scroll.set_offset(point(px(0.), px(-d.scroll_y)));
        }
        let sysdata_scroll = ScrollHandle::new();
        if let Some(d) = crate::demo::state(cx).filter(|d| d.sysdata_scroll_y > 0.) {
            sysdata_scroll.set_offset(point(px(0.), px(-d.sysdata_scroll_y)));
        }
        let checkup_scroll = ScrollHandle::new();
        if let Some(d) = crate::demo::state(cx).filter(|d| d.checkup_scroll_y > 0.) {
            checkup_scroll.set_offset(point(px(0.), px(-d.checkup_scroll_y)));
        }
        TidyView {
            model,
            focus,
            cleanup_scroll: UniformListScrollHandle::new(),
            large_scroll: UniformListScrollHandle::new(),
            memory_scroll: UniformListScrollHandle::new(),
            security_scroll,
            checkup_scroll,
            sysdata_scroll,
            hovered: crate::demo::state(cx).and_then(|d| d.hovered.clone()),
            menu: None,
            threshold_menu: None,
            memory_menu: None,
            details: crate::demo::state(cx).and_then(|d| {
                d.details.as_ref().map(|(id, x, y)| DetailsAt {
                    id: id.clone(),
                    at: point(px(*x), px(*y)),
                })
            }),
            copied: None,
            dialog: None,
            history_open: false,
            history_expanded: HashSet::new(),
            card_figures: Default::default(),
            card_changed: [None; 3],
            card_generation: 0,
            switch_flipped: None,
            updates_flipped: None,
            overlays: Default::default(),
            should_move: false,
            window_visible: window.is_visible(),
            _subs: vec![observed, appearance, activation, visibility],
        }
    }

    pub fn set_hovered(&mut self, key: &str, hovered: bool, cx: &mut Context<Self>) {
        let next = if hovered {
            Some(key.to_string())
        } else if self.hovered.as_deref() == Some(key) {
            None
        } else {
            self.hovered.clone()
        };
        if next != self.hovered {
            self.hovered = next;
            cx.notify();
        }
    }

    // ----- overlays -----

    pub fn open_dialog(&mut self, spec: DialogSpec, cx: &mut Context<Self>) {
        if self.dialog.is_none() {
            self.menu = None;
            self.threshold_menu = None;
            self.memory_menu = None;
            self.details = None;
            self.dialog = Some(spec);
            cx.notify();
        }
    }

    /// Escape: closes the topmost overlay.
    pub fn cancel_overlay(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_some()
            || self.threshold_menu.take().is_some()
            || self.memory_menu.take().is_some()
            || self.details.take().is_some()
            || self.dialog.take().is_some()
        {
            cx.notify();
        } else if self.history_open {
            self.history_open = false;
            cx.notify();
        } else if self.model.read(cx).system_data.open {
            self.model.update(cx, |m, cx| m.close_system_data(cx));
        }
    }

    /// Return: confirms the open dialog.
    pub fn confirm_dialog(&mut self, cx: &mut Context<Self>) {
        let Some(spec) = self.dialog.take() else {
            return;
        };
        match spec.kind {
            DialogKind::CleanReview => {
                self.model.update(cx, |m, cx| m.clean_ticked(cx));
            }
            DialogKind::TrashLarge { rels } => {
                self.model.update(cx, |m, cx| m.trash_rels(&rels, cx));
            }
            DialogKind::EmptyTrash => {
                self.model.update(cx, |m, cx| m.empty_trash(cx));
            }
            DialogKind::ForceQuit { id } => {
                self.model.update(cx, |m, cx| m.force_quit(&id, cx));
            }
            DialogKind::TrashLoginItem { key } => {
                self.model.update(cx, |m, cx| m.trash_login_item(&key, cx));
            }
        }
        cx.notify();
    }

    pub fn request_empty_trash(&mut self, cx: &mut Context<Self>) {
        let trash = self.model.read(cx).cleanup.trash;
        if trash.items > 0 {
            self.open_dialog(DialogSpec::empty_trash(trash), cx);
        }
    }

    pub fn open_history(&mut self, cx: &mut Context<Self>) {
        self.menu = None;
        self.threshold_menu = None;
        self.history_open = true;
        self.model.update(cx, |m, cx| m.load_history(cx));
        cx.notify();
    }

    // ----- chrome shared by both screens -----

    /// Makes empty space in `el` move the window on drag and zoom it on a double click
    /// (gpui-component's TitleBar pattern): the move starts on the first mouse move after a
    /// press, never in the press itself.
    pub(crate) fn drag_area(&self, el: Stateful<Div>, cx: &mut Context<Self>) -> Stateful<Div> {
        el.on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _, _, _| this.should_move = true),
        )
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|this, _, _, _| this.should_move = false),
        )
        .on_mouse_down_out(cx.listener(|this, _, _, _| this.should_move = false))
        .on_mouse_move(cx.listener(|this, _, window, _| {
            if this.should_move {
                this.should_move = false;
                window.start_window_move();
            }
        }))
        .on_click(|ev, window, _| {
            if ev.click_count() == 2 {
                window.titlebar_double_click();
            }
        })
    }

    /// The 52 pt toolbar: the screen title at the left, `controls` at the right. The hairline
    /// under it shows only when the list is scrolled.
    pub(crate) fn toolbar(
        &self,
        t: &Theme,
        title: &'static str,
        scrolled: bool,
        controls: impl IntoElement,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let bar = div()
            .id("toolbar")
            .flex_none()
            .h(px(metrics::TOOLBAR_H))
            .pl(px(20.))
            .pr(px(20.))
            .flex()
            .items_center()
            .gap(px(12.))
            .bg(t.window_bg)
            // The hairline takes 1 pt of the bar's height, so 1 pt on top keeps the controls
            // centered on the row.
            .pt(px(1.))
            .border_b_1()
            .border_color(if scrolled {
                t.separator
            } else {
                gpui_kit::transparent_black()
            });
        self.drag_area(bar, cx)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_size(px(15.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.text)
                    .whitespace_nowrap()
                    .child(title),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    // A press on a control never starts a window drag.
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(controls),
            )
    }

    /// The scanning controls: a spinner, the count, and a Stop button.
    pub(crate) fn scanning_controls(
        &self,
        t: &Theme,
        files: u64,
        on_stop: impl Fn(&mut Window, &mut App) + 'static,
        cx: &App,
    ) -> Div {
        let spinner = Icon::new(IconName::LoaderCircle)
            .size(px(14.))
            .text_color(t.text_2);
        let spinner = if cx.reduce_motion() {
            spinner.into_any_element()
        } else {
            spinner
                .with_animation(
                    "scan-spinner",
                    Animation::new(m::SPIN).repeat(),
                    |icon, delta| icon.transform(Transformation::rotate(percentage(delta))),
                )
                .into_any_element()
        };
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(
                div()
                    .debug_selector(|| "scanning".to_string())
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .text_size(px(12.))
                    .font_features(tnum())
                    .text_color(t.text_2)
                    .whitespace_nowrap()
                    .child(spinner)
                    .child(format::scanning(files)),
            )
            .child(text_button(
                "scan-stop",
                "scan-stop".to_string(),
                "Stop",
                false,
                true,
                t,
                on_stop,
            ))
    }

    /// A spinner and a label (no Stop button) for work the user cannot cancel: a Security run
    /// or a check up.
    pub(crate) fn busy_controls(&self, t: &Theme, label: String, cx: &App) -> Div {
        let spinner = Icon::new(IconName::LoaderCircle)
            .size(px(14.))
            .text_color(t.text_2);
        let spinner = if cx.reduce_motion() {
            spinner.into_any_element()
        } else {
            spinner
                .with_animation(
                    "busy-spinner",
                    Animation::new(m::SPIN).repeat(),
                    |icon, delta| icon.transform(Transformation::rotate(percentage(delta))),
                )
                .into_any_element()
        };
        div()
            .debug_selector(|| "busy".to_string())
            .flex()
            .items_center()
            .gap(px(6.))
            .text_size(px(12.))
            .font_features(tnum())
            .text_color(t.text_2)
            .whitespace_nowrap()
            .child(spinner)
            .child(label)
    }

    /// The band under the toolbar after a clean or a failure.
    pub(crate) fn notice_band(
        &self,
        t: &Theme,
        notice: &Notice,
        leaving: bool,
        with_empty_trash: bool,
        on_dismiss: impl Fn(&mut Window, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let on_band = t.on(t.window_bg.blend(t.fill));
        let (icon, color, text, key) = match notice {
            Notice::Moved { bytes } => (
                IconName::CircleCheck,
                t.success,
                format!(
                    "Moved {} to the Trash. Empty the Trash to free the space.",
                    format::size(*bytes)
                ),
                *bytes,
            ),
            Notice::Error(message) => (IconName::TriangleAlert, t.warning, message.clone(), 0),
            Notice::Info(message) => (IconName::CircleCheck, t.success, message.clone(), 1),
        };
        let moved = matches!(notice, Notice::Moved { .. });
        let view = cx.entity().downgrade();
        let history_view = view.clone();
        let band = div()
            .debug_selector(|| "notice".to_string())
            .flex_none()
            .mt(px(2.))
            .mx(px(12.))
            .mb(px(6.))
            .pl(px(12.))
            .pr(px(8.))
            .py(px(8.))
            .rounded(px(8.))
            .bg(t.fill)
            .flex()
            .items_center()
            .gap(px(10.))
            .text_size(px(12.))
            .line_height(px(16.))
            .text_color(t.text)
            .child(Icon::new(icon).size(px(16.)).text_color(color))
            .child(div().flex_1().min_w_0().child(text))
            .when(moved && with_empty_trash, |d| {
                d.child(text_button(
                    "notice-empty-trash",
                    "notice-empty-trash".to_string(),
                    "Empty Trash\u{2026}",
                    false,
                    true,
                    &on_band,
                    move |_, cx| {
                        let _ = view.update(cx, |v, cx| v.request_empty_trash(cx));
                    },
                ))
            })
            .when(moved, |d| {
                d.child(text_button(
                    "notice-history",
                    "notice-history".to_string(),
                    "Show History",
                    false,
                    true,
                    &on_band,
                    move |_, cx| {
                        let _ = history_view.update(cx, |v, cx| v.open_history(cx));
                    },
                ))
            })
            .child(icon_button(
                "notice-dismiss",
                "notice-dismiss".to_string(),
                IconName::X,
                t,
                on_dismiss,
            ));
        if cx.reduce_motion() {
            if leaving {
                div().into_any_element()
            } else {
                band.into_any_element()
            }
        } else if leaving {
            band.with_animation(
                ("notice-out", key as usize),
                Animation::new(m::NOTE_OUT).with_easing(m::curve(m::exit())),
                |el, t| el.opacity(1. - t),
            )
            .into_any_element()
        } else {
            band.with_animation(
                ("notice-in", key as usize),
                Animation::new(m::NOTE_IN).with_easing(m::curve(m::enter())),
                |el, t| el.opacity(t),
            )
            .into_any_element()
        }
    }

    /// The 30 pt status bar: text at the left, `right` at the right. With nothing to say (an
    /// empty state before the first scan) it is left out rather than drawn as a blank strip.
    pub(crate) fn status_bar(&self, t: &Theme, left: String, right: Option<AnyElement>) -> Div {
        if left.is_empty() && right.is_none() {
            return div();
        }
        div()
            .debug_selector(|| "status-bar".to_string())
            .relative()
            .flex_none()
            .h(px(metrics::STATUS_H))
            .pl(px(20.))
            .pr(px(10.))
            .bg(t.bar_bg)
            .flex()
            .items_center()
            .justify_between()
            .gap(px(12.))
            .text_size(px(12.))
            .text_color(t.text_2)
            .font_features(tnum())
            .whitespace_nowrap()
            .child(
                div()
                    .absolute()
                    .top(px(-1.))
                    .left_0()
                    .right_0()
                    .child(hairline(t)),
            )
            .child(
                div()
                    .debug_selector(|| "status-left".to_string())
                    .min_w_0()
                    .overflow_hidden()
                    .child(left),
            )
            .children(right)
    }

    /// A centered empty state: icon, title, one line, one primary button.
    pub(crate) fn empty_state(
        &self,
        t: &Theme,
        icon: IconName,
        title: &'static str,
        body: String,
        button: &'static str,
        on_click: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Div {
        div()
            .debug_selector(|| "empty-state".to_string())
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .child(
                div()
                    .max_w(px(360.))
                    .px(px(16.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(8.))
                    .child(Icon::new(icon).size(px(32.)).text_color(t.text_3))
                    .child(
                        div()
                            .mt(px(4.))
                            .text_size(px(15.))
                            .line_height(px(20.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(t.text)
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .line_height(px(18.))
                            .text_color(t.text_2)
                            .text_center()
                            .child(body),
                    )
                    .child(div().mt(px(8.)).child(large_button(
                        "empty-action",
                        "empty-action".to_string(),
                        button,
                        true,
                        t,
                        on_click,
                    ))),
            )
    }

    /// The menu and dialog layers, drawn over everything. A sheet or popover that just closed
    /// is drawn once more as a leaving copy while it fades (see `overlay`).
    fn render_overlays(&mut self, t: &Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        self.track_overlays(cx);
        let reduce_motion = cx.reduce_motion();
        let mut layers = Vec::new();
        if let Some(menu) = self.menu.clone() {
            layers.push(self.render_row_menu(t, &menu, cx));
        }
        if let Some(at) = self.threshold_menu {
            layers.push(self.render_threshold_menu(t, at, cx));
        }
        if let Some(menu) = self.memory_menu.clone() {
            layers.push(self.render_memory_menu(t, &menu, cx));
        }
        if let Some((at, _, generation)) = self.overlays.details_leaving.clone()
            && self.details.is_none()
        {
            layers.push(self.render_details(t, &at, Phase::Leave(generation), cx));
        }
        if let Some(at) = self.details.clone() {
            let phase = overlay::phase_for(self.overlays.details_entered, reduce_motion);
            layers.push(self.render_details(t, &at, phase, cx));
        }
        if let (Some(phase), Some(spec)) = (
            self.sheet_leaving(Sheet::Dialog),
            self.overlays.dialog_leaving.clone(),
        ) {
            // The leaving copy takes no action: its buttons and scrim do nothing.
            layers.push(render_dialog(&spec, t, phase, |_, _| {}, |_, _| {}));
        }
        if let Some(spec) = self.dialog.clone() {
            let view = cx.entity().downgrade();
            let cancel_view = view.clone();
            let phase = self.sheet_phase(Sheet::Dialog, reduce_motion);
            layers.push(render_dialog(
                &spec,
                t,
                phase,
                move |_, cx| {
                    let _ = cancel_view.update(cx, |v, cx| {
                        v.dialog = None;
                        cx.notify();
                    });
                },
                move |_, cx| {
                    let _ = view.update(cx, |v, cx| v.confirm_dialog(cx));
                },
            ));
        }
        if self.history_open {
            let phase = self.sheet_phase(Sheet::History, reduce_motion);
            layers.push(self.render_history(t, phase, cx));
        } else if let Some(phase) = self.sheet_leaving(Sheet::History) {
            layers.push(self.render_history(t, phase, cx));
        }
        if self.model.read(cx).system_data.open {
            let phase = self.sheet_phase(Sheet::SystemData, reduce_motion);
            layers.push(self.render_system_data(t, phase, cx));
        } else if let Some(phase) = self.sheet_leaving(Sheet::SystemData) {
            layers.push(self.render_system_data(t, phase, cx));
        }
        layers
    }
}

impl Render for TidyView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::of(window);
        let screen = self.model.read(cx).screen;
        let sidebar = self.render_sidebar(&t, cx);
        let content = match screen {
            Screen::Checkup => self.render_checkup(&t, cx),
            Screen::Cleanup => self.render_cleanup(&t, cx),
            Screen::Large => self.render_large(&t, cx),
            Screen::Memory => self.render_memory(&t, cx),
            Screen::Security => self.render_security(&t, cx),
        };
        let overlays = self.render_overlays(&t, cx);
        let model = self.model.clone();

        div()
            .id("tidy-root")
            .key_context(ROOT_CONTEXT)
            .track_focus(&self.focus)
            .on_action({
                let model = model.clone();
                move |_: &GoCheckup, _, cx| {
                    model.update(cx, |m, cx| m.select_screen(Screen::Checkup, cx));
                }
            })
            .on_action({
                let model = model.clone();
                move |_: &GoCleanup, _, cx| {
                    model.update(cx, |m, cx| m.select_screen(Screen::Cleanup, cx));
                }
            })
            .on_action({
                let model = model.clone();
                move |_: &GoLarge, _, cx| {
                    model.update(cx, |m, cx| m.select_screen(Screen::Large, cx));
                }
            })
            .on_action({
                let model = model.clone();
                move |_: &GoMemory, _, cx| {
                    model.update(cx, |m, cx| m.select_screen(Screen::Memory, cx));
                }
            })
            .on_action({
                let model = model.clone();
                move |_: &GoSecurity, _, cx| {
                    model.update(cx, |m, cx| m.select_screen(Screen::Security, cx));
                }
            })
            .on_action({
                let model = model.clone();
                move |_: &Rescan, _, cx| {
                    model.update(cx, |m, cx| m.rescan(cx));
                }
            })
            .on_action({
                let model = model.clone();
                move |_: &ParentFolder, _, cx| {
                    model.update(cx, |m, cx| m.parent_folder(cx));
                }
            })
            .on_action(cx.listener(|this, _: &TrashSelection, _, cx| this.trash_selection(cx)))
            .on_action(cx.listener(|this, _: &DialogConfirm, _, cx| this.confirm_dialog(cx)))
            .on_action(cx.listener(|this, _: &DialogCancel, _, cx| this.cancel_overlay(cx)))
            .relative()
            .size_full()
            .flex()
            .flex_row()
            .bg(t.window_bg)
            .text_color(t.text)
            .text_size(px(13.))
            .child(sidebar)
            .child(content)
            .children(overlays)
    }
}

/// Opens the main window, or brings the existing one forward. With TIDY_BACKGROUND set the
/// window is created but never put on screen, and the app is never activated, so a test launch
/// shows nothing and takes no focus.
pub fn open_main_window(cx: &mut App) -> Option<AnyWindowHandle> {
    let background = background_mode();
    if let Some(handle) = cx.try_global::<MainWindow>().and_then(|g| g.0) {
        let alive = handle
            .update(cx, |_, window, _| {
                if !background {
                    window.activate_window();
                }
            })
            .is_ok();
        if alive {
            return Some(handle);
        }
    }
    let model = model(cx);
    // Demo shots use their own size, below the normal minimum.
    let (win_w, win_h, min_size) = match crate::demo::active().and_then(crate::demo::main_size) {
        Some((w, h)) => (w, h, None),
        None => (
            metrics::MAIN_W,
            metrics::MAIN_H,
            Some(size(px(metrics::MAIN_MIN_W), px(metrics::MAIN_MIN_H))),
        ),
    };
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(crate::demo::window_bounds(
            size(px(win_w), px(win_h)),
            cx,
        ))),
        // Content runs under the title bar. The traffic lights sit centered on the 52 pt
        // toolbar row, at x 20 inside the sidebar.
        titlebar: Some(TitlebarOptions {
            title: Some("Tidy".into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(20.), px(19.))),
        }),
        window_min_size: min_size,
        focus: !background,
        show: !background,
        app_id: Some("dev.sdusteric.tidy".into()),
        ..Default::default()
    };
    match gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| TidyView::new(model, window, cx))
    }) {
        Ok((handle, _)) => {
            cx.set_global(MainWindow(Some(handle)));
            Some(handle)
        }
        Err(err) => {
            tracing::error!("cannot open the main window: {err:#}");
            None
        }
    }
}
