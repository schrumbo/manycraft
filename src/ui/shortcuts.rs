//! Every keyboard shortcut on one panel, opened with Strg+/ or from the palette.

use gpui_kit::prelude::*;
use gpui_kit::*;

use super::actions::{
    ClearSelection, EditShownBatch, ImportBatch, MessageSelected, NewInstance, NextAttention, NextInstance, OpenChat, OpenPalette, OpenSettings, PreviousInstance, Quit, ReconnectSelected, RestartSelected, SelectAll,
    ShowAccounts, ShowAll, ShowShortcuts, StartSelected, StopSelected, ToggleFullscreen, ToggleSidebar, Undo,
};
use super::drive::Mark;
use super::main_window::{FocusSearch, MainWindow};
use super::palette;
use super::settings_window::{chord_keys, keycap};
use super::theme::{Theme, font, radius, shortcuts, space};
use crate::core::i18n::tr;
use crate::core::settings::HOTKEYS;

/// Opens the overview, or closes it when it is open.
pub fn toggle(this: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    if this.shortcuts {
        return close(this, window, cx);
    }
    this.palette = None;
    this.overlay = false;
    this.shortcuts = true;
    window.focus(&this.focus, cx);
    cx.notify();
}

/// Closes the overview and gives the focus back to the page.
pub fn close(this: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    this.shortcuts = false;
    this.focus_page(window, cx);
    cx.notify();
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

fn sections(cx: &App) -> Vec<(String, Vec<(String, Vec<String>)>)> {
    let bound = |items: Vec<(String, Box<dyn Action>)>| -> Vec<(String, Vec<String>)> { items.into_iter().filter_map(|(label, action)| palette::shortcut(action.as_ref(), cx).map(|keys| (label, caps(&keys)))).collect() };
    let window = bound(vec![
        (tr("Befehle"), Box::new(OpenPalette)),
        (tr("Suchen"), Box::new(FocusSearch)),
        (tr("Seitenleiste ein- oder ausblenden"), Box::new(ToggleSidebar)),
        (tr("Vollbild ein- oder ausschalten"), Box::new(ToggleFullscreen)),
        (tr("Einstellungen"), Box::new(OpenSettings)),
        (tr("Tastenkürzel"), Box::new(ShowShortcuts)),
        (tr("Rückgängig"), Box::new(Undo)),
        (tr("Manycraft beenden"), Box::new(Quit)),
    ]);
    let pages = bound(vec![
        (tr("Alle"), Box::new(ShowAll)),
        (tr("Accounts"), Box::new(ShowAccounts)),
        (tr("Chat"), Box::new(OpenChat)),
        (tr("Neue Instanz"), Box::new(NewInstance)),
        (tr("Batch bearbeiten"), Box::new(EditShownBatch)),
        (tr("Instanz importieren…"), Box::new(ImportBatch)),
    ]);
    let selection = bound(vec![
        (tr("Alles auswählen"), Box::new(SelectAll)),
        (tr("Auswahl aufheben"), Box::new(ClearSelection)),
        (tr("Ausgewählte starten"), Box::new(StartSelected)),
        (tr("Ausgewählte stoppen"), Box::new(StopSelected)),
        (tr("Ausgewählte neu starten"), Box::new(RestartSelected)),
        (tr("Ausgewählte neu verbinden"), Box::new(ReconnectSelected)),
        (tr("Nachricht senden…"), Box::new(MessageSelected)),
    ]);
    let games = bound(vec![
        (tr("Nächsten Account spielen"), Box::new(NextInstance)),
        (tr("Vorherigen Account spielen"), Box::new(PreviousInstance)),
        (tr("Nächsten, der dich braucht"), Box::new(NextAttention)),
    ]);
    let in_game = HOTKEYS.iter().map(|(label, chord)| (tr(label), chord_keys(chord))).collect();
    vec![(tr("Fenster"), window), (tr("Seiten"), pages), (tr("Auswahl im Raster"), selection), (tr("Spiele wechseln"), games), (tr("Im Spiel"), in_game)]
}

fn section(title: String, items: Vec<(String, Vec<String>)>, t: &Theme) -> Div {
    div()
        .flex()
        .flex_col()
        .child(div().pb(px(space::XS)).text_size(px(font::CAPTION)).font_weight(FontWeight::SEMIBOLD).text_color(t.tertiary).child(title))
        .children(items.into_iter().map(|(label, keys)| {
            div()
                .id(SharedString::from(format!("shortcut-{label}")))
                .mark("row", label.clone())
                .h(px(shortcuts::ROW))
                .flex()
                .items_center()
                .gap(px(space::M))
                .child(div().flex_1().min_w_0().truncate().text_size(px(font::BODY)).text_color(t.primary).child(label))
                .child(div().flex_none().flex().items_center().gap(px(space::XS)).children(keys.into_iter().map(|k| keycap(k, t))))
        }))
}

/// Draws the open overview over the window.
pub fn render(this: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) -> Option<AnyElement> {
    if !this.shortcuts {
        return None;
    }
    let t = Theme::get(cx);
    let viewport = window.viewport_size();
    let width = shortcuts::WIDTH.min(f32::from(viewport.width) - space::L * 2.);
    let left = ((f32::from(viewport.width) - width) / 2.).max(space::L);
    let height = (f32::from(viewport.height) - shortcuts::TOP * 2.).max(shortcuts::ROW * 4.);
    let two = width >= shortcuts::TWO_COLUMNS;
    let mut all = sections(cx).into_iter().filter(|(_, items)| !items.is_empty()).map(|(title, items)| section(title, items, &t)).collect::<Vec<_>>();
    let right = if two { all.split_off(2) } else { Vec::new() };
    let column = |parts: Vec<Div>| div().flex_1().min_w_0().flex().flex_col().gap(px(space::L)).children(parts);
    Some(
        div()
            .id("shortcuts-backdrop")
            .absolute()
            .inset_0()
            .occlude()
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| close(this, window, cx)))
            .child(
                div()
                    .id("shortcuts")
                    .mark("dialog", tr("Tastenkürzel"))
                    .absolute()
                    .top(px(shortcuts::TOP))
                    .left(px(left))
                    .w(px(width))
                    .max_h(px(height))
                    .bg(t.content)
                    .rounded(px(radius::CARD))
                    .border_1()
                    .border_color(t.separator)
                    .shadow_lg()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .flex_none()
                            .px(px(space::XL))
                            .pt(px(space::L))
                            .pb(px(space::M))
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(div().text_size(px(font::TITLE3)).font_weight(FontWeight::SEMIBOLD).text_color(t.primary).child(tr("Tastenkürzel")))
                            .child(
                                super::sheet::icon_button("shortcuts-close", "x", format!("{} · Esc", tr("Schließen")), t.secondary, &t).on_click(cx.listener(|this, _, window, cx| close(this, window, cx))),
                            ),
                    )
                    .child(div().h(px(1.)).flex_none().bg(t.separator))
                    .child(
                        div()
                            .id("shortcuts-list")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .px(px(space::XL))
                            .py(px(space::L))
                            .flex()
                            .gap(px(space::XL))
                            .child(column(all))
                            .when(two, |d| d.child(column(right))),
                    ),
            )
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::caps;

    #[test]
    fn every_key_of_a_shortcut_gets_its_own_cap() {
        if cfg!(target_os = "macos") {
            assert_eq!(caps("⇧⌘C"), ["⇧", "⌘", "C"]);
            assert_eq!(caps("F11"), ["F11"]);
        } else {
            assert_eq!(caps("Strg+Umschalt+C"), ["Strg", "Umschalt", "C"]);
            assert_eq!(caps("F11"), ["F11"]);
            assert_eq!(caps("Strg+Plus"), ["Strg", "Plus"]);
        }
    }
}
