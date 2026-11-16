use freya::prelude::*;

use super::list::{self, Data};
use super::{Ctx, Live};
use crate::core::content::{self, Answer, Bisect, Outcome};
use crate::core::i18n::{tr, trf};
use crate::ui::forms::{self, Problem};
use crate::view::icons::{Icon, icon};
use crate::view::kit::button::Button;
use crate::view::kit::sheet;
use crate::view::kit::text::{secondary, strong};
use crate::view::kit::off_ui;
use crate::view::theme::{Theme, font, iconsize, space};

/// One move of the troubleshooter.
#[derive(Clone, Copy)]
pub(super) enum Step {
    Start,
    Answer(Answer),
    Stop,
    Dismiss,
}

impl Ctx {
    fn running_games(&self) -> bool {
        self.store.model.peek().games.values().any(|g| g.instance_id == *self.id && g.phase.is_active())
    }

    pub(super) fn ask_bisect(&self) {
        if self.bisect.peek().is_some() {
            return;
        }
        if self.running_games() {
            self.note(tr("Stoppe zuerst die laufenden Spiele."));
            return;
        }
        let on = self.data.peek().entries().iter().filter(|e| e.enabled && e.on_disk).count();
        if on < 2 {
            self.note(tr("Zum Eingrenzen müssen mindestens zwei Mods an sein."));
            return;
        }
        let message = trf("Manycraft schaltet in jeder Runde einen Teil der Mods aus. Du startest das Spiel und sagst, ob der Fehler noch da ist. Nach etwa %lld Runden steht fest, welcher Mod es ist. Danach ist alles wieder wie vorher.", &[&list::rounds(on)]);
        let ctx = self.clone();
        sheet::confirm(self.store.ui, "instance.mods.bisect.ask", tr("Fehlerhaften Mod finden?"), message, tr("Starten"), false, move |_| ctx.bisect_step(Step::Start));
    }

    pub(super) fn bisect_step(&self, step: Step) {
        if *self.bisecting.peek() {
            return;
        }
        if !matches!(step, Step::Dismiss) && self.running_games() {
            self.note(tr("Beende zuerst das Spiel."));
            return;
        }
        self.bisecting.soft(|b| *b = true);
        self.settle();
        let ctx = self.clone();
        spawn_in_window(async move {
            let id = ctx.id();
            let result = off_ui(move || match step {
                Step::Start => content::start(&id).map(|(made, b)| (Some(made), Some(b))),
                Step::Answer(answer) => content::answer(&id, answer).map(|(made, b)| (Some(made), Some(b))),
                Step::Stop => content::stop(&id).map(|made| (Some(made), None)),
                Step::Dismiss => content::dismiss(&id).map(|()| (None, None)),
            })
            .await;
            ctx.bisecting.soft(|b| *b = false);
            match result {
                Some(Ok((made, bisect))) => {
                    ctx.bisect.soft(|b| *b = bisect);
                    if let Some(made) = made {
                        ctx.put(made);
                    }
                }
                Some(Err(e)) => ctx.fail(forms::explain_as(tr("Die Fehlersuche hat nicht geklappt."), &e)),
                None => ctx.fail(Problem::plain(forms::unexplained())),
            }
            ctx.rescan();
        });
    }
}

fn heading(t: &Theme, glyph: Icon, color: Color, text: String) -> Element {
    rect().width(Size::fill()).horizontal().cross_align(Alignment::Center).spacing(space::S).child(icon(t, glyph, iconsize::M, color)).child(strong(t, text, font::BODY, t.text1).width(Size::flex(1.))).content(Content::flex()).into_element()
}

/// The card above the list while the troubleshooter runs or has an answer.
pub(super) fn card(ctx: &Ctx, t: &Theme, b: &Bisect, data: &Data, running: bool) -> Element {
    let mut card = rect()
        .a11y_role(AccessibilityRole::Group)
        .a11y_alt(tr("Fehlerhaften Mod finden…"))
        .width(Size::fill())
        .spacing(space::S)
        .padding(space::M)
        .corner_radius(t.shape.tile)
        .background(t.raised)
        .border(Border::new().fill(t.separator).width(1.).alignment(BorderAlignment::Inner));
    let wait = running.then(|| tr("Beende zuerst das Spiel."));
    match &b.outcome {
        None => {
            let off = b.off.len();
            let total = data.entries().len();
            let text = trf("Fehlersuche · Runde %lld von etwa %lld", &[&(b.round + 1), &b.rounds.max(b.round + 1)]);
            let stop = ctx.clone();
            let (still, gone) = (ctx.clone(), ctx.clone());
            let top = rect()
                .width(Size::fill())
                .horizontal()
                .cross_align(Alignment::Center)
                .spacing(space::S)
                .content(Content::flex())
                .child(heading(t, Icon::Alert, t.accent_text, text))
                .child(Button::new("instance.mods.bisect.stop", tr("Abbrechen")).plain().on_press(move |_| stop.bisect_step(Step::Stop)));
            card = card
                .child(top)
                .child(secondary(t, trf("%lld von %lld Mods sind gerade aus. Starte das Spiel und sieh nach, ob der Fehler noch auftritt.", &[&off, &total])).width(Size::fill()).max_lines(3))
                .child(
                    rect()
                        .horizontal()
                        .spacing(space::S)
                        .child(Button::new("instance.mods.bisect.still", tr("Fehler ist noch da")).disabled(wait.clone()).on_press(move |_| still.bisect_step(Step::Answer(Answer::Still))))
                        .child(Button::new("instance.mods.bisect.gone", tr("Fehler ist weg")).disabled(wait).on_press(move |_| gone.bisect_step(Step::Answer(Answer::Gone)))),
                );
        }
        Some(outcome) => {
            let (text, glyph, color) = match outcome {
                Outcome::Found(file) => (trf("%@ verursacht den Fehler.", &[&data.label_of(std::slice::from_ref(file))]), Icon::Check, t.success_text),
                Outcome::Several(files) => (trf("Einer dieser Mods verursacht den Fehler: %@.", &[&list::listing(&data.names(files))]), Icon::Help, t.attention_text),
                Outcome::Nothing => (tr("Kein Mod verursacht den Fehler."), Icon::Help, t.text2),
            };
            let rest = match outcome {
                Outcome::Nothing => tr("Der Fehler liegt woanders. Alle Mods sind wieder wie vorher."),
                _ => tr("Alle Mods sind wieder wie vorher."),
            };
            let mut buttons = rect().horizontal().spacing(space::S);
            if let Outcome::Found(file) = outcome
                && data.entry(file).is_some_and(|e| e.enabled)
            {
                let (file, off) = (file.clone(), ctx.clone());
                buttons = buttons.child(Button::new("instance.mods.bisect.off", trf("%@ ausschalten", &[&data.label_of(std::slice::from_ref(&file))])).primary().on_press(move |_| off.switch(vec![file.clone()], false)));
            }
            let done = ctx.clone();
            buttons = buttons.child(Button::new("instance.mods.bisect.done", tr("Fertig")).on_press(move |_| done.bisect_step(Step::Dismiss)));
            card = card.child(heading(t, glyph, color, text)).child(secondary(t, rest).width(Size::fill()).max_lines(3)).child(buttons);
        }
    }
    card.into_element()
}
