//! The block of the three items: where each stands, who it came from, and the earlier versions to go back to.

use freya::prelude::*;

use super::words::{blurb, label, when};
use super::{Ctx, Loaded, Who};
use crate::core::i18n::{tr, trf};
use crate::core::share::{self, ITEMS, Item, Note, Standing, Version};
use crate::view::kit::button::Button;
use crate::view::kit::group::{Line, group};
use crate::view::kit::select::Select;
use crate::view::kit::{off_ui, use_theme};
use crate::view::native;
use crate::view::theme::{control, space};

/// What each item stands on, with its history.
#[derive(PartialEq)]
pub(super) struct KindsBlock {
    pub ctx: Ctx,
    pub loaded: Loaded,
    pub who: Vec<Who>,
    pub change: EventHandler<Item>,
}

impl Component for KindsBlock {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let ctx = self.ctx;
        let mut picked = use_state(|| [0usize; 3]);
        let busy = use_state(|| false);
        let mut notes = use_state(|| self.loaded.notes.clone());
        use_side_effect_with_deps(&self.loaded.notes, move |found| {
            if notes.try_peek().is_some() {
                notes.set(found.clone());
            }
        });
        let mut rows: Vec<Element> = Vec::new();
        for (n, item) in ITEMS.into_iter().enumerate() {
            let standing = self.loaded.standing.iter().find(|s| s.item == item).cloned().unwrap_or(Standing { item, base: None, changed: None, present: false });
            let sharers = self.who.iter().filter(|w| w.sync.get(item) && (item != Item::Options || share::options_travel(&w.version))).count();
            let versions = self.loaded.versions.iter().find(|(i, _)| *i == item).and_then(|(_, v)| v.clone());
            let key = key(item);
            let mut line = Line::new(format!("sync.kind.{key}"), label(item)).hint(format!("{} {}", blurb(item), state(&standing, sharers, self.who.len())));
            let change = self.change.clone();
            let folder = share::folder();
            let path = if item == Item::Servers { folder.join("servers.dat") } else if item == Item::Options { folder.join("options.txt") } else { folder.join("config") };
            let present = standing.present;
            let open = Button::new(format!("sync.kind.{key}.open"), if item == Item::Config { tr("Ordner öffnen") } else { tr("Datei öffnen") }).plain().disabled((!present).then(|| tr("Noch nicht vorhanden."))).on_press(move |_| native::open(&path));
            let switch_base = Button::new(format!("sync.kind.{key}.base"), tr("Basis ändern …")).plain().on_press(move |_| change.call(item));
            line = line.control(rect().horizontal().cross_align(Alignment::Center).spacing(space::S).child(open).child(switch_base));
            if let Some(found) = versions.filter(|v| !v.is_empty()) {
                let at = picked.read()[n].min(found.len() - 1);
                let labels: Vec<String> = found.iter().map(describe).collect();
                let restore_one = found[at].clone();
                let select = Select::new(format!("sync.history.{key}"), tr("Früherer Stand"), labels, at).width(control::VERSION_MENU).on_choose(move |i| {
                    let mut next = *picked.peek();
                    next[n] = i;
                    picked.set(next);
                });
                let back = Button::new(format!("sync.history.{key}.restore"), tr("Zurückholen")).disabled(busy.read().then(|| tr("Wird zurückgeholt …"))).on_press(move |_| {
                    if !*busy.peek() {
                        restore(ctx, restore_one.clone(), busy);
                    }
                });
                line = line.below(rect().horizontal().cross_align(Alignment::Center).spacing(space::S).child(select).child(back));
            }
            rows.push(line.into_element());
        }
        if let Some(note) = notes.read().first() {
            rows.push(note_line(&t, "sync.note", note, notes));
        }
        group(&t, Some(tr("Was synchronisiert wird")), rows, Some(tr("Es gibt jede Datei nur einmal. Synchronisierende Instanzen lesen und ändern dieselbe. Vor jedem Start hebt Manycraft den Stand auf und holt ihn zurück, wenn der Start ihn zurücksetzt. Frühere Stände bleiben in der Liste.")))
    }
}

fn key(item: Item) -> &'static str {
    match item {
        Item::Options => "options",
        Item::Config => "config",
        Item::Servers => "servers",
    }
}

/// Where `item` stands, in a sentence.
fn state(s: &Standing, sharers: usize, total: usize) -> String {
    let count = trf("%lld von %lld Instanzen synchronisieren.", &[&sharers, &total]);
    if !s.present {
        return format!("{} {count}", tr("Noch leer: der Stand entsteht bei der Einrichtung oder wenn eine Instanz speichert."));
    }
    let mut parts = Vec::new();
    if let Some((name, at)) = &s.base {
        parts.push(trf("Übernommen aus „%@“, %@.", &[name, &when(*at)]));
    }
    if let Some((name, at)) = &s.changed
        && s.base.as_ref() != Some(&(name.clone(), *at))
    {
        parts.push(trf("Zuletzt geändert von „%@“, %@.", &[name, &when(*at)]));
    }
    parts.push(count);
    parts.join(" ")
}

fn describe(v: &Version) -> String {
    let files = if v.item == Item::Config && v.files > 1 { trf(" · %@ Dateien", &[&v.files]) } else { String::new() };
    let at = when(v.written.unwrap_or(v.at));
    match (&v.source, v.own) {
        (Some(name), true) => format!("{at} · {}{files}", trf("eigener Stand von %@", &[name])),
        (Some(name), false) => format!("{at} · {}{files}", trf("von %@", &[name])),
        (None, _) => format!("{at}{files}"),
    }
}

fn restore(ctx: Ctx, version: Version, mut busy: State<bool>) {
    busy.set(true);
    spawn_in_window(async move {
        match off_ui(move || share::restore(&version)).await {
            Some(Ok(())) => {
                let _ = crate::view::kit::toast::show(ctx.store.ui, tr("Zurückgeholt. Mod-Einstellungen gelten sofort, Spiel-Optionen und Serverliste ab dem nächsten Start."), None);
            }
            Some(Err(e)) => ctx.failed(tr("Nicht zurückgeholt."), &e),
            None => ctx.unexplained(),
        }
        if busy.try_peek().is_some() {
            busy.set(false);
        }
        ctx.refresh();
    });
}

/// A row saying what sync restored on its own, with a way to say it was seen.
fn note_line(t: &crate::view::theme::Theme, id: &str, note: &Note, mut notes: State<Vec<Note>>) -> Element {
    use crate::view::kit::group::Line as Row;
    let what = if note.files.len() == 1 { trf("„%@“", &[&note.files[0]]) } else { trf("%@ Mod-Einstellungen", &[&note.files.len()]) };
    let hint = trf("Der Start von „%@“ hat %@ zurückgesetzt oder beschädigt (%@). Der Stand davor ist wieder da.", &[&note.instance, &what, &when(note.at)]);
    let seen = Button::new(format!("{id}.seen"), tr("Verstanden")).plain().on_press(move |_| {
        notes.set(Vec::new());
        spawn_in_window(async move {
            let _ = off_ui(share::clear_notes).await;
        });
    });
    Row::new(id.to_owned(), tr("Automatisch wiederhergestellt")).hint(hint).hint_tone(t.attention_text).control(seen).into_element()
}
