//! Every keyboard shortcut of the window on one sheet, opened with F1 or from the palette.

use freya::prelude::*;

use crate::core::i18n::tr;
use crate::core::settings::HOTKEYS;
use crate::view::commands::{Command, Shortcut};
use crate::view::kit::sheet::{self, Card, Primary};
use crate::view::kit::text::{caption, line};
use crate::view::kit::use_theme;
use crate::view::settings::{chord_keys, keycap};
use crate::view::store::use_store;
use crate::view::theme::{Theme, control, font, frame, space};
use crate::view::ui::Ui;

const WIDTH: f32 = 720.;
const TWO_COLUMNS: f32 = 600.;
const ROW: f32 = control::S + space::XS;
const CHROME: f32 = 200.;
const LEFT_SECTIONS: usize = 3;

type Section = (String, Vec<(String, Vec<String>)>);

/// Opens the overview of every shortcut as the window's sheet.
pub fn open(ui: Ui) {
    sheet::open(ui, Shortcuts);
}

fn caps(label: &str) -> Vec<String> {
    if cfg!(target_os = "macos") {
        let glyphs = label.chars().take_while(|c| "⌃⌥⇧⌘".contains(*c)).count();
        let mut out: Vec<String> = label.chars().take(glyphs).map(String::from).collect();
        out.push(label.chars().skip(glyphs).collect());
        out
    } else {
        label.split('+').map(str::to_owned).collect()
    }
}

fn sections() -> Vec<Section> {
    use Command::*;
    let bound = |list: &[Command]| list.iter().filter_map(|c| c.shortcut().map(|s| (c.label(), caps(&s.label())))).collect::<Vec<_>>();
    let mut tabs = caps(&Shortcut::alt(Code::Digit1).label());
    if let Some(last) = tabs.last_mut() {
        *last = "1–5".to_owned();
    }
    let mut instance = vec![
        (tr("Spielen"), caps(&Shortcut::primary(Code::KeyR).label())),
        (tr("Stoppen"), caps(&Shortcut::primary(Code::Period).label())),
        (tr("Umbenennen"), caps(&Shortcut::bare(Code::F2).label())),
        (tr("Tab wechseln"), tabs),
    ];
    instance.extend(bound(&[AddMods]));
    let mut switch = bound(&[NextInstance, PreviousInstance, NextAttention]);
    let more: Vec<_> = HOTKEYS.iter().map(|(label, chord)| (tr(label), chord_keys(chord))).filter(|(_, keys)| !switch.iter().any(|(_, k)| k == keys)).collect();
    switch.extend(more);
    vec![
        (tr("Fenster"), bound(&[OpenPalette, FocusSearch, ToggleSidebar, OpenSettings, ShowShortcuts, Undo, Quit])),
        (tr("Seiten"), bound(&[ShowAll, ShowAccounts, OpenChat, SwitchAccount, AddAccount, NewInstance, NewBatch, Import, FindModpacks, OpenTrash])),
        (tr("Instanz"), instance),
        (tr("Batches"), bound(&[SelectAll, ClearSelection, StartSelected, StopSelected, RestartSelected, ReconnectSelected, StartAll, StopAll, ShowTiles, ShowTable, Larger, Smaller, Fit])),
        (tr("Spiele wechseln, auch im Spiel"), switch),
    ]
}

fn section(t: &Theme, (title, rows): Section) -> Element {
    rect()
        .width(Size::fill())
        .child(rect().padding((0., 0., space::XS, 0.)).child(caption(t, title).font_weight(FontWeight::SEMI_BOLD)))
        .children(rows.into_iter().map(|(label, keys)| {
            rect()
                .a11y_role(AccessibilityRole::ListItem)
                .a11y_alt(format!("{label}: {}", keys.join(" ")))
                .width(Size::fill())
                .height(Size::px(ROW))
                .horizontal()
                .cross_align(Alignment::Center)
                .spacing(space::M)
                .content(Content::flex())
                .child(line(t, label, font::BODY, t.text1).max_lines(1).text_overflow(TextOverflow::Ellipsis).width(Size::flex(1.)))
                .child(rect().horizontal().spacing(space::XS).children(keys.into_iter().map(|k| keycap(t, k))))
                .into_element()
        }))
        .into_element()
}

#[derive(PartialEq)]
struct Shortcuts;

impl Component for Shortcuts {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let ui = store.ui;
        let (w, h) = *ui.window.read();
        let width = WIDTH.min(w - space::L * 2.);
        let two = width >= TWO_COLUMNS;
        let mut all: Vec<Element> = sections().into_iter().filter(|(_, rows)| !rows.is_empty()).map(|s| section(&t, s)).collect();
        let right = if two { all.split_off(LEFT_SECTIONS.min(all.len())) } else { Vec::new() };
        let column = |parts: Vec<Element>| rect().width(Size::flex(1.)).spacing(space::L).children(parts);
        let mut columns = rect().width(Size::fill()).horizontal().spacing(space::XL).content(Content::flex()).child(column(all));
        if two {
            columns = columns.child(column(right));
        }
        let room = (h - frame::SHEET_TOP - CHROME).max(ROW * 4.);
        let body = ScrollView::new().width(Size::fill()).height(Size::auto()).max_height(Size::px(room)).child(columns);
        Card::new("shortcuts", tr("Tastenkürzel"))
            .width(width)
            .inform()
            .primary(Primary { label: tr("Fertig"), disabled: None, danger: false, busy: false, run: EventHandler::new(move |_| sheet::close(ui)) })
            .body(body)
    }
}

#[cfg(test)]
mod tests {
    use super::caps;

    #[test]
    fn every_key_of_a_shortcut_gets_its_own_cap() {
        if cfg!(target_os = "macos") {
            assert_eq!(caps("⇧⌘C"), ["⇧", "⌘", "C"]);
            assert_eq!(caps("F2"), ["F2"]);
        } else {
            assert_eq!(caps("Strg+Umschalt+C"), ["Strg", "Umschalt", "C"]);
            assert_eq!(caps("F2"), ["F2"]);
            assert_eq!(caps("Strg+Plus"), ["Strg", "Plus"]);
        }
    }
}
