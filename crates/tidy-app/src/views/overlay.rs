//! Overlay motion (design 11 and C36 to C39): one enter and exit for the full-window sheets
//! (the confirm dialogs, History and System Data), the why-safe popover's settle, and the two
//! content fades (System Data's first measure, a History entry opening).
//!
//! Tidy draws these overlays itself, so macOS adds no motion of its own. Each one comes in over
//! [`m::CARD_IN`] with the `enter` curve: the scrim, which holds the card, goes from clear to
//! its full color while the card settles up from [`SETTLE`] pt below its place. Closing plays the
//! reverse over [`m::NOTE_OUT`] with the `exit` curve on a leaving copy of the overlay, the same
//! way the notice band leaves. Buttons, Return and Escape work from the first frame: the motion
//! only changes how the overlay looks, never whether it takes input.
//!
//! Every motion is keyed by a fresh generation, plays only within [`m::CHANGE_GATE`] of the
//! change (so a window that opens on a settled state, or a demo shot of one, shows it still),
//! and is instant under Reduce Motion: GPUI then draws the end frame.

use std::time::{Duration, Instant};

use gpui_kit::*;

use crate::theme::motion as m;
use crate::views::checkup::just_changed;
use crate::views::dialogs::DialogSpec;
use crate::views::main_window::TidyView;
use crate::views::trust::DetailsAt;

/// How far below its place a sheet's card starts, in points.
pub const SETTLE: f32 = 6.;
/// How far nearer the click the why-safe popover starts, in points.
pub const POP_SETTLE: f32 = 4.;

/// The three full-window sheets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Sheet {
    Dialog,
    History,
    SystemData,
}

impl Sheet {
    const ALL: [Sheet; 3] = [Sheet::Dialog, Sheet::History, Sheet::SystemData];

    fn index(self) -> usize {
        match self {
            Sheet::Dialog => 0,
            Sheet::History => 1,
            Sheet::SystemData => 2,
        }
    }

    /// The element ids of the scrim and card animations, in and out.
    fn ids(self) -> [&'static str; 4] {
        match self {
            Sheet::Dialog => [
                "dialog-in",
                "dialog-card-in",
                "dialog-out",
                "dialog-card-out",
            ],
            Sheet::History => [
                "history-in",
                "history-card-in",
                "history-out",
                "history-card-out",
            ],
            Sheet::SystemData => [
                "sysdata-in",
                "sysdata-card-in",
                "sysdata-out",
                "sysdata-card-out",
            ],
        }
    }
}

/// How an overlay is drawn this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// As it is: no motion.
    Still,
    /// Coming in, keyed by this generation.
    Enter(usize),
    /// The leaving copy, keyed by this generation.
    Leave(usize),
}

/// The phase of something that changed at `changed` (with its generation): entering while the
/// change is recent and motion is allowed, still otherwise.
pub fn phase_for(changed: Option<(Instant, usize)>, reduce_motion: bool) -> Phase {
    match changed {
        Some((at, generation)) if !reduce_motion && just_changed(Some(at)) => {
            Phase::Enter(generation)
        }
        _ => Phase::Still,
    }
}

/// What the view keeps to animate the overlays.
#[derive(Default)]
pub struct OverlayState {
    /// Whether each sheet was open at the last render.
    was_open: [bool; 3],
    /// When each sheet opened, with the generation keying its enter.
    pub entered: [Option<(Instant, usize)>; 3],
    /// A sheet that closed and is fading out: since when, and its generation.
    pub leaving: [Option<(Instant, usize)>; 3],
    /// The dialog drawn at the last render, and the one fading out.
    last_dialog: Option<DialogSpec>,
    pub dialog_leaving: Option<DialogSpec>,
    /// The popover drawn at the last render, when it opened, and the one fading out.
    last_details: Option<DetailsAt>,
    pub details_entered: Option<(Instant, usize)>,
    pub details_leaving: Option<(DetailsAt, Instant, usize)>,
    /// Whether the System Data sheet had a report at the last render, and when its first
    /// report arrived while it was open.
    had_report: bool,
    pub report_arrived: Option<(Instant, usize)>,
    /// The History entry opened last: its id, when, and the generation keying its fade.
    pub history_opened: Option<(String, Instant, usize)>,
}

impl TidyView {
    fn next_generation(&mut self) -> usize {
        self.card_generation += 1;
        self.card_generation
    }

    /// Compares what is open now with the last render and starts the motions that follow:
    /// an enter for a sheet or popover that just opened, a leaving copy for one that just
    /// closed, and a fade for System Data's first report. Called once per render, before the
    /// overlays are drawn.
    pub(crate) fn track_overlays(&mut self, cx: &mut Context<Self>) {
        let reduce_motion = cx.reduce_motion();
        let (sysdata_open, has_report) = {
            let s = &self.model.read(cx).system_data;
            (s.open, s.report.is_some())
        };
        let open_now = [self.dialog.is_some(), self.history_open, sysdata_open];
        let sysdata_was_open = self.overlays.was_open[Sheet::SystemData.index()];
        let mut started: Vec<usize> = Vec::new();
        for sheet in Sheet::ALL {
            let i = sheet.index();
            let was = self.overlays.was_open[i];
            let replaced = sheet == Sheet::Dialog
                && was
                && open_now[i]
                && self.overlays.last_dialog != self.dialog;
            if (open_now[i] && !was) || replaced {
                // Opening again while the old one leaves drops the leaving copy.
                self.overlays.leaving[i] = None;
                if sheet == Sheet::Dialog {
                    self.overlays.dialog_leaving = None;
                }
                let generation = self.next_generation();
                self.overlays.entered[i] = Some((Instant::now(), generation));
            } else if !open_now[i] && was {
                self.overlays.entered[i] = None;
                if !reduce_motion {
                    let generation = self.next_generation();
                    self.overlays.leaving[i] = Some((Instant::now(), generation));
                    if sheet == Sheet::Dialog {
                        self.overlays.dialog_leaving = self.overlays.last_dialog.clone();
                    }
                    started.push(generation);
                }
            }
            self.overlays.was_open[i] = open_now[i];
        }
        self.overlays.last_dialog = self.dialog.clone();

        // The why-safe popover: a new one comes in; one opened over another shows at once and
        // the old one goes without an exit.
        let last = self.overlays.last_details.clone();
        match (&last, &self.details.clone()) {
            (None, Some(_)) => {
                self.overlays.details_leaving = None;
                let generation = self.next_generation();
                self.overlays.details_entered = Some((Instant::now(), generation));
            }
            (Some(old), Some(new)) if old.id != new.id => {
                self.overlays.details_leaving = None;
                self.overlays.details_entered = None;
            }
            (Some(old), None) => {
                self.overlays.details_entered = None;
                if !reduce_motion {
                    let generation = self.next_generation();
                    self.overlays.details_leaving = Some((old.clone(), Instant::now(), generation));
                    started.push(generation);
                }
            }
            _ => {}
        }
        self.overlays.last_details = self.details.clone();

        // System Data: the first report of this opening fades in; a re-measure does not.
        if sysdata_open && sysdata_was_open && has_report && !self.overlays.had_report {
            let generation = self.next_generation();
            self.overlays.report_arrived = Some((Instant::now(), generation));
        }
        if !sysdata_open {
            self.overlays.report_arrived = None;
        }
        self.overlays.had_report = sysdata_open && has_report;

        if !started.is_empty() {
            // The leaving copies go once they have faded, so nothing invisible stays.
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(m::NOTE_OUT + Duration::from_millis(20))
                    .await;
                let _ = this.update(cx, |v, cx| {
                    v.drop_leaving(&started);
                    cx.notify();
                });
            })
            .detach();
        }
    }

    /// Drops the leaving copies made with these generations (a newer one stays).
    fn drop_leaving(&mut self, generations: &[usize]) {
        let o = &mut self.overlays;
        for i in 0..3 {
            if o.leaving[i].is_some_and(|(_, g)| generations.contains(&g)) {
                o.leaving[i] = None;
                if i == Sheet::Dialog.index() {
                    o.dialog_leaving = None;
                }
            }
        }
        if o.details_leaving
            .as_ref()
            .is_some_and(|(_, _, g)| generations.contains(g))
        {
            o.details_leaving = None;
        }
    }

    /// The phase a sheet is drawn in while open.
    pub(crate) fn sheet_phase(&self, sheet: Sheet, reduce_motion: bool) -> Phase {
        phase_for(self.overlays.entered[sheet.index()], reduce_motion)
    }

    /// The leaving copy of a sheet to draw, if one is fading out.
    pub(crate) fn sheet_leaving(&self, sheet: Sheet) -> Option<Phase> {
        self.overlays.leaving[sheet.index()].map(|(_, generation)| Phase::Leave(generation))
    }
}

/// Draws a sheet: the scrim with the card on it, in its phase. The scrim fades and the card
/// settles together, so the window darkens and clears with the sheet instead of in one frame.
pub fn present(
    sheet: Sheet,
    scrim: Stateful<Div>,
    card: impl Styled + IntoElement + 'static,
    phase: Phase,
) -> AnyElement {
    let [scrim_in, card_in, scrim_out, card_out] = sheet.ids();
    match phase {
        Phase::Still => scrim.child(card).into_any_element(),
        Phase::Enter(generation) => {
            let card = card.relative().with_animation(
                (card_in, generation),
                Animation::new(m::CARD_IN).with_easing(m::curve(m::enter())),
                |el, d| el.top(px(SETTLE * (1. - d))),
            );
            scrim
                .child(card)
                .with_animation(
                    (scrim_in, generation),
                    Animation::new(m::CARD_IN).with_easing(m::curve(m::enter())),
                    |el, d| el.opacity(d),
                )
                .into_any_element()
        }
        Phase::Leave(generation) => {
            let card = card.relative().with_animation(
                (card_out, generation),
                Animation::new(m::NOTE_OUT).with_easing(m::curve(m::exit())),
                |el, d| el.top(px(SETTLE * d)),
            );
            scrim
                .child(card)
                .with_animation(
                    (scrim_out, generation),
                    Animation::new(m::NOTE_OUT).with_easing(m::curve(m::exit())),
                    |el, d| el.opacity(1. - d),
                )
                .into_any_element()
        }
    }
}

/// The why-safe popover's card in its phase: in, it fades up while it settles from
/// [`POP_SETTLE`] pt nearer the click; out, it only fades.
pub fn present_popover(card: impl Styled + IntoElement + 'static, phase: Phase) -> AnyElement {
    match phase {
        Phase::Still => card.into_any_element(),
        Phase::Enter(generation) => card
            .relative()
            .with_animation(
                ("details-in", generation),
                Animation::new(m::NOTE_IN).with_easing(m::curve(m::enter())),
                |el, d| el.opacity(d).top(px(-POP_SETTLE * (1. - d))),
            )
            .into_any_element(),
        Phase::Leave(generation) => card
            .with_animation(
                ("details-out", generation),
                Animation::new(m::REMOVE).with_easing(m::curve(m::exit())),
                |el, d| el.opacity(1. - d),
            )
            .into_any_element(),
    }
}

/// Content that fades in once (System Data's first report, a History entry's items).
pub fn fade_in(
    id: &'static str,
    el: impl IntoElement + 'static,
    phase: Phase,
    duration: Duration,
) -> AnyElement {
    match phase {
        Phase::Enter(generation) => div()
            .child(el)
            .with_animation(
                (id, generation),
                Animation::new(duration).with_easing(m::curve(m::enter())),
                |el, d| el.opacity(d),
            )
            .into_any_element(),
        _ => el.into_any_element(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn motion_plays_only_right_after_the_change_and_never_under_reduce_motion() {
        let now = Some((Instant::now(), 7));
        assert_eq!(phase_for(now, false), Phase::Enter(7));
        assert_eq!(phase_for(now, true), Phase::Still);
        assert_eq!(phase_for(None, false), Phase::Still);
        let old = Instant::now().checked_sub(m::CHANGE_GATE + Duration::from_millis(10));
        if let Some(old) = old {
            assert_eq!(phase_for(Some((old, 1)), false), Phase::Still);
        }
    }

    #[test]
    fn the_sheets_use_the_card_and_note_timings() {
        assert_eq!(m::CARD_IN, Duration::from_millis(200));
        assert_eq!(m::NOTE_OUT, Duration::from_millis(200));
        assert_eq!(m::NOTE_IN, Duration::from_millis(160));
        assert_eq!(m::REMOVE, Duration::from_millis(140));
        // Every sheet has its own ids, so two never share an animation.
        let mut ids: Vec<&str> = Sheet::ALL.iter().flat_map(|s| s.ids()).collect();
        let n = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), n);
    }
}
