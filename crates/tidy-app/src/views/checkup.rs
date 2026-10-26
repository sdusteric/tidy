//! The Check up screen (design sections 5.5 and 11): one button that runs the Cleanup scan, the
//! Security checks and a memory snapshot, three cards that link to those screens, the last
//! check up date, "Include macOS updates", and the opt-in weekly reminder.
//!
//! The reminder asks for notification permission only when the user switches it on, never at
//! launch. At launch, with the reminder on, the app only looks the permission up (which never
//! prompts) and reschedules its single notification. A denied permission shows
//! "Notifications are off for Tidy" with a button to open the Notifications settings.

use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use tidy_core::api::Command;
use tidy_core::model::{Checkup, SettingsLink};
use tidy_core::reminder::ReminderDate;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::app::Tri;
use crate::app::{Notice, Screen, TidyModel, notifier, now_unix, open_settings, send};
use crate::format;
use crate::macos::Permission;
use crate::theme::{DisplayType, Theme, figure, metrics, motion as m, tnum};
use crate::views::main_window::TidyView;
use crate::views::{KeyStop, checkbox, large_button, text_button};

pub const RUN_LABEL: &str = "Run Check up";
pub const RUNNING_LABEL: &str = "Running Check up";
pub const UPDATES_LABEL: &str = "Include macOS updates";
pub const UPDATES_NOTE: &str = "Also asks Apple for updates. This can take up to a minute.";
pub const REMINDER_TITLE: &str = "Weekly reminder";
pub const REMINDER_OFF_NOTE: &str = "One notification, a week after your last check up. Tidy asks for notification permission when you turn this on.";
pub const REMINDER_DENIED: &str = "Notifications are off for Tidy";
pub const REMINDER_DENIED_NOTE: &str =
    "Turn them on in System Settings and Tidy will schedule the reminder.";
pub const REMINDER_UNAVAILABLE: &str = "Reminders need the installed Tidy app.";
/// The automatic update row under the reminder. Sparkle keeps the choice in its own defaults,
/// not in Tidy's settings.
pub const AUTO_UPDATE_TITLE: &str = "Automatically install updates";
pub const AUTO_UPDATE_ON_NOTE: &str = "Tidy checks daily and installs updates when it quits.";
pub const AUTO_UPDATE_OFF_NOTE: &str = "Tidy checks daily and asks before installing.";
/// No updater in this run: TIDY_NO_UPDATES, a test or screenshot run, or not the app bundle.
pub const AUTO_UPDATE_OFF_LABEL: &str = "Off";
pub const AUTO_UPDATE_UNAVAILABLE: &str = "This copy of Tidy does not update itself.";
pub const HERO_NOTE: &str =
    "Cleanup, security and memory in one pass. Nothing is cleaned until you choose.";

/// Which parts of a running check up have reported.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Parts {
    pub cleanup: bool,
    pub security: bool,
    pub memory: bool,
}

#[derive(Default)]
pub struct CheckupState {
    /// The latest check up, from `checkups.jsonl` or from this session.
    pub last: Option<Checkup>,
    /// The last check up was asked for.
    pub loaded: bool,
    pub running: bool,
    pub parts: Parts,
    /// What the notification center said last. None until it was looked up.
    pub permission: Option<Permission>,
    /// When the pending reminder fires, once it has been scheduled.
    pub next_reminder: Option<ReminderDate>,
    pub notice: Option<Notice>,
    pub notice_leaving: Option<Notice>,
}

impl TidyModel {
    /// Entering the screen reads the last check up from disk once and looks up the
    /// notification permission (never a prompt), so a denial shows without a click.
    pub(crate) fn enter_checkup(&mut self, cx: &mut Context<Self>) {
        self.load_last_checkup(cx);
        if self.settings.reminder_enabled {
            self.refresh_permission(cx);
        }
    }

    fn load_last_checkup(&mut self, cx: &mut Context<Self>) {
        if self.checkup.loaded {
            return;
        }
        let (reply, rx) = tokio::sync::oneshot::channel();
        if !send(cx, Command::LastCheckup { reply }) {
            return;
        }
        self.checkup.loaded = true;
        cx.spawn(async move |this, cx| {
            let last = rx.await.ok().flatten();
            let _ = this.update(cx, |m, cx| {
                if m.checkup.last.is_none() {
                    m.checkup.last = last;
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Run Check up: the Cleanup scan, the Security checks and a memory snapshot. Each part
    /// reports with its usual event, so the other screens update too.
    pub fn run_checkup(&mut self, cx: &mut Context<Self>) {
        if self.checkup.running {
            return;
        }
        let include_updates = self.settings.include_updates;
        if send(cx, Command::RunCheckup { include_updates }) {
            self.checkup.running = true;
            self.checkup.parts = Parts::default();
            self.checkup.notice = None;
            self.checkup.notice_leaving = None;
            self.begin_activity();
            // The scan follows a click, so privacy prompts may follow (C3).
            self.remember(|s| s.cleanup_scanned_once = true, cx);
            cx.notify();
        }
    }

    pub fn set_include_updates(&mut self, on: bool, cx: &mut Context<Self>) {
        self.remember(|s| s.include_updates = on, cx);
        cx.notify();
    }

    pub(crate) fn on_checkup_done(
        &mut self,
        checkup: Checkup,
        reminder: ReminderDate,
        cx: &mut Context<Self>,
    ) {
        self.checkup.running = false;
        self.checkup.parts = Parts {
            cleanup: true,
            security: true,
            memory: true,
        };
        self.checkup.last = Some(checkup);
        self.checkup.loaded = true;
        // A new check up moves the reminder: replace the single pending notification.
        if self.settings.reminder_enabled && self.checkup.permission == Some(Permission::Granted) {
            if let Some(n) = notifier(cx) {
                n.schedule(&reminder);
            }
            self.checkup.next_reminder = Some(reminder);
        }
    }

    pub fn dismiss_checkup_notice(&mut self, cx: &mut Context<Self>) {
        self.checkup.notice = None;
        self.checkup.notice_leaving = None;
        cx.notify();
    }

    // ----- the weekly reminder -----

    /// At launch: with the reminder on, look up the permission and reschedule. With it off
    /// nothing is asked of the notification center at all. Never prompts.
    pub fn start_reminder(&mut self, cx: &mut Context<Self>) {
        if self.settings.reminder_enabled {
            self.refresh_permission(cx);
        }
    }

    /// Looks up the notification permission (no prompt). When it is granted and the reminder
    /// is on, the single reminder is rescheduled.
    pub fn refresh_permission(&mut self, cx: &mut Context<Self>) {
        let Some(n) = notifier(cx) else {
            return;
        };
        let rx = n.status();
        self.await_permission(rx, cx);
    }

    fn await_permission(
        &mut self,
        rx: tokio::sync::oneshot::Receiver<Permission>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let Ok(permission) = rx.await else { return };
            let _ = this.update(cx, |m, cx| {
                m.checkup.permission = Some(permission);
                if permission == Permission::Granted && m.settings.reminder_enabled {
                    m.reschedule_reminder(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The reminder toggle. Switching it on is the only thing that asks for notification
    /// permission; switching it off removes the pending notification.
    pub fn set_reminder(&mut self, on: bool, cx: &mut Context<Self>) {
        if on == self.settings.reminder_enabled {
            return;
        }
        self.remember(|s| s.reminder_enabled = on, cx);
        if on {
            if let Some(n) = notifier(cx) {
                let rx = n.request();
                self.await_permission(rx, cx);
            }
        } else {
            if let Some(n) = notifier(cx) {
                n.cancel();
            }
            self.checkup.next_reminder = None;
        }
        cx.notify();
    }

    /// Asks the core when the reminder is due and replaces the pending notification with it.
    fn reschedule_reminder(&mut self, cx: &mut Context<Self>) {
        let (reply, rx) = tokio::sync::oneshot::channel();
        if !send(cx, Command::NextReminder { reply }) {
            return;
        }
        cx.spawn(async move |this, cx| {
            let Ok(date) = rx.await else { return };
            let _ = this.update(cx, |m, cx| {
                if m.settings.reminder_enabled && m.checkup.permission == Some(Permission::Granted)
                {
                    if let Some(n) = notifier(cx) {
                        n.schedule(&date);
                    }
                    m.checkup.next_reminder = Some(date);
                    cx.notify();
                }
            });
        })
        .detach();
    }
}

/// Opens Tidy on Check up each time the user clicks the reminder.
pub fn watch_reminder_clicks(
    mut rx: UnboundedReceiver<()>,
    model: WeakEntity<TidyModel>,
    cx: &mut App,
) {
    cx.spawn(async move |cx| {
        while rx.recv().await.is_some() {
            if model
                .update(cx, |m, cx| m.select_screen(Screen::Checkup, cx))
                .is_err()
            {
                break;
            }
            cx.update(crate::reopen);
        }
    })
    .detach();
}

// ---------- cards ----------

/// One of the three cards.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Card {
    /// The Figure: "6.3 GB", "All good", "Normal".
    pub figure: String,
    /// Text after the Figure on the same baseline: "ready to clean".
    pub suffix: Option<String>,
    /// The line under it.
    pub line: String,
}

impl Card {
    fn checking() -> Card {
        Card {
            figure: "Checking".to_string(),
            suffix: None,
            line: String::new(),
        }
    }

    fn missing(line: &str) -> Card {
        Card {
            figure: "Not checked".to_string(),
            suffix: None,
            line: line.to_string(),
        }
    }
}

/// The three cards for the current state.
pub struct Cards {
    pub cleanup: Card,
    pub security: Card,
    pub memory: Card,
}

pub fn cards(m: &TidyModel) -> Cards {
    let c = &m.checkup;
    let part = |done: bool| c.running && !done;
    let last = c.last.as_ref();

    let cleanup = if part(c.parts.cleanup) {
        Card::checking()
    } else if m.cleanup.plan.is_some() {
        let safe = m.cleanup.safe_bytes().unwrap_or(0);
        let review = m.cleanup.review_bytes();
        let line = if let Some(Notice::Moved { bytes }) = &m.cleanup.notice {
            format!("Moved {} to the Trash", format::size(*bytes))
        } else if review > 0 {
            format!("{} more to review", format::size(review))
        } else {
            String::new()
        };
        if safe == 0 {
            Card {
                figure: "Nothing to clean".to_string(),
                suffix: None,
                line,
            }
        } else {
            Card {
                figure: format::size(safe),
                suffix: Some("ready to clean".to_string()),
                line,
            }
        }
    } else if let Some(cl) = last.and_then(|l| l.cleanup) {
        Card {
            figure: format::size(cl.safe_bytes),
            suffix: Some("ready to clean".to_string()),
            line: "Run Check up again to clean".to_string(),
        }
    } else if last.is_some() {
        Card::missing("This part did not finish.")
    } else {
        Card::missing("Run Check up to see.")
    };

    let security = if part(c.parts.security) {
        Card::checking()
    } else if let Some(s) = last.and_then(|l| l.security.as_ref()) {
        Card {
            figure: s.headline(),
            suffix: None,
            line: if s.first_run {
                "This is your first check".to_string()
            } else if s.gray > 0 {
                format!("{} not checked", s.gray)
            } else {
                String::new()
            },
        }
    } else if let Some(r) = &m.security.report {
        Card {
            figure: r.headline(),
            suffix: None,
            line: String::new(),
        }
    } else if last.is_some() {
        Card::missing("This part did not finish.")
    } else {
        Card::missing("Run Check up to see.")
    };

    let memory = if part(c.parts.memory) {
        Card::checking()
    } else if let Some(mem) = last.and_then(|l| l.memory) {
        Card {
            figure: mem.pressure.label().to_string(),
            suffix: None,
            line: mem.verdict().to_string(),
        }
    } else if let Some(s) = &m.memory.snapshot {
        Card {
            figure: s.pressure.label().to_string(),
            suffix: None,
            line: s.verdict().to_string(),
        }
    } else if last.is_some() {
        Card::missing("This part did not finish.")
    } else {
        Card::missing("Run Check up to see.")
    };

    Cards {
        cleanup,
        security,
        memory,
    }
}

/// "Last check up 2 days ago" or "No check up yet".
pub fn last_line(last: Option<&Checkup>, now: u64) -> String {
    match last {
        Some(c) => format!("Last check up {}", format::ago(now, c.at)),
        None => "No check up yet".to_string(),
    }
}

/// The reminder's description under the toggle.
pub fn reminder_note(
    enabled: bool,
    permission: Option<Permission>,
    next: Option<&ReminderDate>,
) -> String {
    match (enabled, permission) {
        (false, _) => REMINDER_OFF_NOTE.to_string(),
        (true, Some(Permission::Denied)) => REMINDER_DENIED.to_string(),
        (true, Some(Permission::Unavailable)) => REMINDER_UNAVAILABLE.to_string(),
        (true, _) => match next {
            Some(d) => format!(
                "Tidy will remind you on {}. One reminder at most, even if you don't open Tidy.",
                format::reminder_when(d)
            ),
            None => "Tidy will remind you a week after your next check up.".to_string(),
        },
    }
}

// ---------- drawing ----------

/// A switch: 34 x 20 pt, a white knob with Haul's knob shadow that sits right when on. When
/// `slide` is set (the switch was just clicked) the knob slides to its side over 160 ms
/// instead of jumping there.
pub fn switch(
    id: &'static str,
    on: bool,
    slide: Option<usize>,
    t: &Theme,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    const OFF_X: f32 = 2.;
    const ON_X: f32 = 16.;
    let knob = div()
        .absolute()
        .top(px(2.))
        .size(px(16.))
        .rounded_full()
        .bg(t.on_accent)
        .shadow(t.knob_shadow());
    let (from, to) = if on { (OFF_X, ON_X) } else { (ON_X, OFF_X) };
    let knob = match slide {
        Some(generation) => knob
            .left(px(from))
            .with_animation(
                ("switch-knob", generation),
                Animation::new(m::SWITCH).with_easing(m::curve(m::enter())),
                move |el, d| el.left(px(from + (to - from) * d)),
            )
            .into_any_element(),
        None => knob.left(px(to)).into_any_element(),
    };
    div()
        .id(id)
        .role(gpui_kit::accesskit::Role::Switch)
        .aria_toggled(if on {
            gpui_kit::accesskit::Toggled::True
        } else {
            gpui_kit::accesskit::Toggled::False
        })
        .debug_selector(|| id.to_string())
        .relative()
        .flex_none()
        .w(px(34.))
        .h(px(20.))
        .rounded_full()
        .when(on, |d| d.bg(t.accent))
        .when(!on, |d| d.bg(t.separator_strong))
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            on_click(window, cx)
        })
        .key_stop(t)
        .child(knob)
}

/// Whether something that changed at `at` is still recent enough to animate.
pub(crate) fn just_changed(at: Option<std::time::Instant>) -> bool {
    at.is_some_and(|at| at.elapsed() < m::CHANGE_GATE)
}

impl TidyView {
    #[allow(clippy::too_many_arguments)]
    fn card(
        &self,
        t: &Theme,
        key: &'static str,
        icon: IconName,
        title: &'static str,
        card: &Card,
        screen: Screen,
        footer: Option<AnyElement>,
    ) -> Stateful<Div> {
        let model = self.model.clone();
        let hover = t.card_hover();
        div()
            .id(key)
            .debug_selector(move || key.to_string())
            .flex_1()
            .min_w_0()
            .p(px(14.))
            .rounded(px(10.))
            .bg(t.card_bg())
            .flex()
            .flex_col()
            .gap(px(6.))
            .cursor_pointer()
            .hover(move |s| s.bg(hover))
            .on_click(move |_, _, cx| {
                model.update(cx, |m, cx| m.select_screen(screen, cx));
            })
            .key_stop(t)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(Icon::new(icon).size(px(16.)).text_color(t.text_2))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(13.))
                            .display(FontWeight::BOLD)
                            .text_color(t.text)
                            .child(title),
                    )
                    .child(
                        Icon::new(IconName::ChevronRight)
                            .size(px(12.))
                            .text_color(t.text_3),
                    ),
            )
            .child(
                div()
                    .mt(px(2.))
                    .flex()
                    .flex_wrap()
                    .items_baseline()
                    .gap(px(6.))
                    .child(
                        div()
                            .debug_selector(move || format!("{key}-figure"))
                            .text_size(px(figure::SIZE))
                            .line_height(px(figure::LINE))
                            .display(FontWeight::BOLD)
                            .font_features(tnum())
                            .text_color(t.text)
                            .child(card.figure.clone()),
                    )
                    .children(card.suffix.clone().map(|s| {
                        div()
                            .text_size(px(15.))
                            .text_color(t.text_2)
                            .whitespace_nowrap()
                            .child(s)
                    })),
            )
            .child(
                div()
                    .debug_selector(move || format!("{key}-line"))
                    .flex_1()
                    .min_h(px(48.))
                    .text_size(px(12.))
                    .line_height(px(16.))
                    .text_color(t.text_2)
                    .child(card.line.clone()),
            )
            .children(footer)
    }

    /// The whole Check up screen.
    pub(crate) fn render_checkup(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let now = now_unix(cx);
        let model = self.model.clone();
        let view = cx.entity().downgrade();
        let (
            cards,
            last,
            running,
            include,
            reminder_on,
            permission,
            next,
            notice,
            notice_leaving,
            cleaning,
            ticked,
            any_review,
        ) = {
            let m = self.model.read(cx);
            (
                cards(m),
                m.checkup.last.clone(),
                m.checkup.running,
                m.settings.include_updates,
                m.settings.reminder_enabled,
                m.checkup.permission,
                m.checkup.next_reminder,
                m.checkup.notice.clone(),
                m.checkup.notice_leaving.clone(),
                m.cleanup.cleaning,
                (m.cleanup.ticked_items().len(), m.cleanup.ticked_bytes()),
                m.cleanup.any_review_ticked(),
            )
        };
        let scrolled = self.checkup_scroll.offset().y < px(0.);

        let controls = if running {
            self.busy_controls(t, "Running Check up".to_string(), cx)
                .into_any_element()
        } else {
            div().into_any_element()
        };
        let toolbar = self.toolbar(t, "Check up", scrolled, controls, cx);

        let shown = notice
            .as_ref()
            .map(|n| (n, false))
            .or(notice_leaving.as_ref().map(|n| (n, true)));
        let notice_band = shown.map(|(n, leaving)| {
            let model = model.clone();
            self.notice_band(
                t,
                n,
                leaving,
                false,
                move |_, cx| {
                    model.update(cx, |m, cx| m.dismiss_checkup_notice(cx));
                },
                cx,
            )
        });

        // The summary, as on the other screens: when the last check up was as the Figure, what
        // a check up does under it. Then the action row: the button and its one option.
        let run_model = model.clone();
        let updates_model = model.clone();
        let summary = div()
            .debug_selector(|| "checkup-hero".to_string())
            .flex_none()
            .h(px(metrics::SUMMARY_H))
            .px(px(20.))
            .flex()
            .flex_col()
            .justify_center()
            .child(
                div()
                    .debug_selector(|| "last-checkup".to_string())
                    .text_size(px(figure::SIZE))
                    .line_height(px(figure::LINE))
                    .display(FontWeight::BOLD)
                    .font_features(tnum())
                    .text_color(t.text)
                    .whitespace_nowrap()
                    .child(last_line(last.as_ref(), now)),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .line_height(px(16.))
                    .text_color(t.text_2)
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(HERO_NOTE),
            );
        let actions = div()
            .flex_none()
            .px(px(20.))
            .pt(px(4.))
            .pb(px(20.))
            .flex()
            .items_center()
            .gap(px(20.))
            .child(large_button(
                "run-checkup",
                "run-checkup".to_string(),
                if running { RUNNING_LABEL } else { RUN_LABEL },
                !running,
                t,
                move |_, cx| {
                    run_model.update(cx, |m, cx| m.run_checkup(cx));
                },
            ))
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .items_start()
                    .gap(px(8.))
                    .child(checkbox(
                        "include-updates",
                        "include-updates".to_string(),
                        if include { Tri::On } else { Tri::Off },
                        true,
                        t,
                        move |_, cx| {
                            updates_model.update(cx, |m, cx| m.set_include_updates(!include, cx));
                        },
                    ))
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .line_height(px(16.))
                                    .text_color(t.text)
                                    .child(UPDATES_LABEL),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .line_height(px(16.))
                                    .text_color(t.text_2)
                                    .child(UPDATES_NOTE),
                            ),
                    ),
            );

        // The cleanup card's Clean button does what the one on Cleanup does.
        let clean_label = format::clean_label(ticked.1, any_review, cleaning);
        let can_clean = ticked.0 > 0 && cleaning.is_none();
        let clean_view = view.clone();
        let clean_button = text_button(
            "checkup-clean",
            "checkup-clean".to_string(),
            clean_label,
            true,
            can_clean,
            t,
            move |_, cx| {
                cx.stop_propagation();
                let _ = clean_view.update(cx, |v, cx| v.request_clean(cx));
            },
        );
        let clean_footer = (ticked.0 > 0 || cleaning.is_some())
            .then(|| div().flex().child(clean_button).into_any_element());

        // A card whose part just reported fades its result in over "Checking".
        let reduce_motion = cx.reduce_motion();
        let figures = [
            cards.cleanup.figure.clone(),
            cards.security.figure.clone(),
            cards.memory.figure.clone(),
        ];
        for (i, figure) in figures.into_iter().enumerate() {
            let was_checking = self.card_figures[i] == Card::checking().figure;
            if was_checking && figure != self.card_figures[i] {
                self.card_changed[i] = Some((std::time::Instant::now(), self.card_generation));
                self.card_generation += 1;
            }
            self.card_figures[i] = figure;
        }
        let fade = |i: usize, card: Stateful<Div>| -> AnyElement {
            match self.card_changed[i] {
                Some((at, generation)) if !reduce_motion && just_changed(Some(at)) => card
                    .with_animation(
                        ("card-in", generation),
                        Animation::new(m::CARD_IN).with_easing(m::curve(m::enter())),
                        |el, d| el.opacity(d),
                    )
                    .into_any_element(),
                _ => card.into_any_element(),
            }
        };
        let card_row = div()
            .flex_none()
            .px(px(20.))
            .flex()
            .items_stretch()
            .gap(px(12.))
            .child(fade(
                0,
                self.card(
                    t,
                    "card-cleanup",
                    IconName::BrushCleaning,
                    "Cleanup",
                    &cards.cleanup,
                    Screen::Cleanup,
                    clean_footer,
                ),
            ))
            .child(fade(
                1,
                self.card(
                    t,
                    "card-security",
                    IconName::ShieldCheck,
                    "Security",
                    &cards.security,
                    Screen::Security,
                    None,
                ),
            ))
            .child(fade(
                2,
                self.card(
                    t,
                    "card-memory",
                    IconName::MemoryStick,
                    "Memory",
                    &cards.memory,
                    Screen::Memory,
                    None,
                ),
            ));

        // The weekly reminder, as a Haul settings row: the title and note at the left, the
        // switch at the right.
        let denied = reminder_on && permission == Some(Permission::Denied);
        let toggle_model = model.clone();
        let toggle_view = view.clone();
        let slide = (!reduce_motion && just_changed(self.switch_flipped.map(|(at, _)| at)))
            .then(|| self.switch_flipped.map_or(0, |(_, g)| g));
        let reminder = div()
            .debug_selector(|| "reminder".to_string())
            .mx(px(20.))
            .mt(px(12.))
            .px(px(14.))
            .py(px(12.))
            .rounded(px(10.))
            .bg(t.card_bg())
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(16.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .line_height(px(17.))
                                    .display(FontWeight::SEMIBOLD)
                                    .text_color(t.text)
                                    .child(REMINDER_TITLE),
                            )
                            .child(
                                div()
                                    .debug_selector(|| "reminder-note".to_string())
                                    .text_size(px(12.))
                                    .line_height(px(16.))
                                    .font_weight(if denied {
                                        FontWeight::MEDIUM
                                    } else {
                                        FontWeight::NORMAL
                                    })
                                    .text_color(if denied { t.warning } else { t.text_2 })
                                    .child(reminder_note(reminder_on, permission, next.as_ref())),
                            ),
                    )
                    .child(switch(
                        "reminder-toggle",
                        reminder_on,
                        slide,
                        t,
                        move |_, cx| {
                            let _ = toggle_view.update(cx, |v, cx| {
                                v.switch_flipped =
                                    Some((std::time::Instant::now(), v.card_generation));
                                v.card_generation += 1;
                                cx.notify();
                            });
                            toggle_model.update(cx, |m, cx| m.set_reminder(!reminder_on, cx));
                        },
                    )),
            )
            .children(denied.then(|| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(16.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(12.))
                            .line_height(px(16.))
                            .text_color(t.text_2)
                            .child(REMINDER_DENIED_NOTE),
                    )
                    .child(text_button(
                        "notifications-settings",
                        "notifications-settings".to_string(),
                        SettingsLink::Notifications.label(),
                        false,
                        true,
                        &t.on(t.card_bg()),
                        move |_, cx| open_settings(SettingsLink::Notifications, cx),
                    ))
            }));

        // Automatic updates, the same kind of row under the reminder. Sparkle saves the choice.
        let auto_update = crate::updates::control(cx).map(|c| crate::updates::auto_install(&*c));
        let update_slide = (!reduce_motion && just_changed(self.updates_flipped.map(|(at, _)| at)))
            .then(|| self.updates_flipped.map_or(0, |(_, g)| g));
        let update_note = match auto_update {
            Some(true) => AUTO_UPDATE_ON_NOTE,
            Some(false) => AUTO_UPDATE_OFF_NOTE,
            None => AUTO_UPDATE_UNAVAILABLE,
        };
        let update_control = match auto_update {
            Some(on) => {
                let flip_view = view.clone();
                switch("auto-update-toggle", on, update_slide, t, move |_, cx| {
                    let _ = flip_view.update(cx, |v, cx| {
                        v.updates_flipped = Some((std::time::Instant::now(), v.card_generation));
                        v.card_generation += 1;
                        cx.notify();
                    });
                    if let Some(c) = crate::updates::control(cx) {
                        crate::updates::set_auto_install(&*c, !on);
                    }
                })
                .into_any_element()
            }
            None => div()
                .debug_selector(|| "auto-update-off".to_string())
                .flex_none()
                .text_size(px(12.))
                .line_height(px(16.))
                .text_color(t.text_2)
                .child(AUTO_UPDATE_OFF_LABEL)
                .into_any_element(),
        };
        let updates = div()
            .debug_selector(|| "auto-update".to_string())
            .mx(px(20.))
            .mt(px(12.))
            .px(px(14.))
            .py(px(12.))
            .rounded(px(10.))
            .bg(t.card_bg())
            .flex()
            .items_center()
            .gap(px(16.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .text_size(px(13.))
                            .line_height(px(17.))
                            .display(FontWeight::SEMIBOLD)
                            .text_color(t.text)
                            .child(AUTO_UPDATE_TITLE),
                    )
                    .child(
                        div()
                            .debug_selector(|| "auto-update-note".to_string())
                            .text_size(px(12.))
                            .line_height(px(16.))
                            .text_color(t.text_2)
                            .child(update_note),
                    ),
            )
            .child(update_control);

        let body = div()
            .id("checkup-body")
            .debug_selector(|| "checkup-body".to_string())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.checkup_scroll)
            .child(summary)
            .child(actions)
            .child(card_row)
            .child(reminder)
            .child(updates)
            .child(self.system_data_row(t))
            .child(self.never_card(t));

        // The reminder row already says when it fires, so the status bar has nothing to add.
        let status = self.status_bar(t, String::new(), None);

        div()
            .debug_selector(|| "checkup".to_string())
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .child(toolbar)
            .children(notice_band)
            .child(body)
            .child(status)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn last_line_is_relative() {
        assert_eq!(last_line(None, 1_000), "No check up yet");
        let c = Checkup {
            at: 1_000,
            ..Checkup::default()
        };
        assert_eq!(
            last_line(Some(&c), 1_000 + 2 * 86_400 + 5),
            "Last check up 2 days ago"
        );
    }

    #[test]
    fn the_reminder_note_follows_the_state() {
        let date = ReminderDate {
            at: 1,
            year: 2026,
            month: 10,
            day: 30,
            hour: 10,
            minute: 0,
        };
        assert_eq!(reminder_note(false, None, None), REMINDER_OFF_NOTE);
        assert_eq!(
            reminder_note(true, Some(Permission::Denied), Some(&date)),
            REMINDER_DENIED
        );
        assert_eq!(
            reminder_note(true, Some(Permission::Unavailable), None),
            REMINDER_UNAVAILABLE
        );
        assert_eq!(
            reminder_note(true, Some(Permission::Granted), Some(&date)),
            "Tidy will remind you on Oct 30 at 10:00 AM. One reminder at most, even if you don't open Tidy."
        );
        assert!(reminder_note(true, Some(Permission::Granted), None).contains("next check up"));
    }

    #[test]
    fn motion_plays_only_right_after_a_change() {
        use std::time::{Duration, Instant};
        assert!(!just_changed(None));
        assert!(just_changed(Some(Instant::now())));
        let old = Instant::now().checked_sub(Duration::from_secs(2));
        assert!(old.is_none_or(|at| !just_changed(Some(at))));
    }

    #[test]
    fn copy_has_no_em_dashes() {
        for s in [
            RUN_LABEL,
            RUNNING_LABEL,
            UPDATES_LABEL,
            UPDATES_NOTE,
            REMINDER_TITLE,
            REMINDER_OFF_NOTE,
            REMINDER_DENIED,
            REMINDER_DENIED_NOTE,
            REMINDER_UNAVAILABLE,
            AUTO_UPDATE_TITLE,
            AUTO_UPDATE_ON_NOTE,
            AUTO_UPDATE_OFF_NOTE,
            AUTO_UPDATE_UNAVAILABLE,
            HERO_NOTE,
        ] {
            assert!(!s.contains('\u{2014}'), "{s}");
        }
    }
}
