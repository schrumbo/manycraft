//! Erscheinungsbild: light, dark or as the system, and the theme, picked from the JSON themes and applied at once.

use std::rc::Rc;

use freya::prelude::*;

use super::{Prefs, use_prefs};
use crate::core::i18n::{self, tr, trf};
use crate::core::settings::Appearance;
use crate::core::themes::{self, Library, Origin, Token};
use crate::view::Look;
use crate::view::drive::use_spot;
use crate::view::icons::{Icon, icon};
use crate::view::kit::button::Button;
use crate::view::kit::group::{Line, group, note, status};
use crate::view::kit::select::Select;
use crate::view::kit::segmented::{Segment, Segmented};
use crate::view::kit::text::line;
use crate::view::kit::{Bounds, off_thread, open_external, ring, toast, use_focus_return, use_theme};
use crate::view::store::{Store, use_store};
use crate::view::theme::{Theme, color, font, iconsize, level, space};
use crate::view::ui::Ui;

const COLUMNS: usize = 4;
const GALLERY_W: f32 = 640.;
const GALLERY_MIN_H: f32 = 420.;
const SWATCH_H: f32 = 64.;
const SWATCH_PAD: f32 = space::XS + 2.;
const SWATCH_INSET: f32 = 2.;
const FRAME_GROW: f32 = 4.;
const SIDEBAR_SHARE: f32 = 28.;
const SIDE_LINES: [f32; 3] = [80., 60., 70.];
const TITLE_LINE: f32 = 55.;
const ACCENT_H: f32 = 4.;
const LINE_H: f32 = 3.;
const LINE_ROUND: f32 = 2.;
const PILL: (f32, f32) = (22., 6.);
const TILE_ROUND: f32 = 0.6;
const ROUND: (f32, f32) = (4., 8.);
const ATTENTION_EDGE: f32 = 1.5;
const EDGE: (f32, f32) = (1., 2.);
const LABEL_GAP: f32 = 1.;

fn label(th: &themes::Theme) -> String {
    match th.origin {
        Origin::BuiltIn => tr(&th.name),
        Origin::User(_) => th.name.clone(),
    }
}

fn wants_dark(appearance: Appearance, shown: bool) -> bool {
    match appearance {
        Appearance::Light => false,
        Appearance::Dark => true,
        Appearance::System => shown,
    }
}

fn choose(p: Prefs, mut look: Look, appearance: Appearance, theme: Option<String>) {
    let kept = theme.clone();
    p.change(move |s| {
        s.appearance = appearance;
        s.theme = kept;
    });
    look.choice.set_if_modified((appearance, theme));
}

const LANGS: [Option<&str>; 3] = [None, Some("de"), Some("en")];

fn set_language(p: Prefs, mut theme: State<Rc<Theme>>, code: Option<&'static str>) {
    p.change(move |s| s.language = code.map(str::to_string));
    i18n::init(code.unwrap_or_default());
    let same = (**theme.peek()).clone();
    theme.set(Rc::new(same));
}

/// Language, light or dark, and the theme, whose gallery opens over the page.
#[derive(PartialEq)]
pub struct LookGroup;

impl Component for LookGroup {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let p = use_prefs();
        let look = use_consume::<Look>();
        let drawn = use_consume::<State<Rc<Theme>>>();
        let mut anchor = use_state(Bounds::default);
        let mut open = use_state(|| false);
        let s = p.read();
        let library = look.library.read().clone();
        let chosen = library.chosen(s.theme.as_deref()).map(|c| c.id.clone()).unwrap_or_default();
        let appearance = s.appearance;
        let languages = vec![tr("Automatisch"), "Deutsch".to_string(), "English".to_string()];
        let at_lang = LANGS.iter().position(|l| *l == s.language.as_deref()).unwrap_or(0);
        let language = Line::new("settings.language.row", tr("Sprache"))
            .control(Select::new("settings.language", tr("Sprache"), languages, at_lang).on_choose(move |i| set_language(p, drawn, LANGS[i.min(2)])))
            .into_element();
        let modes = [Appearance::System, Appearance::Light, Appearance::Dark];
        let segments = vec![
            Segment { id: "settings.look.system".into(), label: tr("Automatisch"), tip: Some(tr("Folgt dem System")) },
            Segment { id: "settings.look.light".into(), label: tr("Hell"), tip: None },
            Segment { id: "settings.look.dark".into(), label: tr("Dunkel"), tip: None },
        ];
        let at = modes.iter().position(|m| *m == appearance).unwrap_or(0);
        let theme_id = s.theme.clone();
        let mode = Line::new("settings.look.row", tr("Hell oder dunkel"))
            .control(Segmented::new(tr("Hell oder dunkel"), segments, at).on_choose(move |i| choose(p, look, modes[i.min(2)], theme_id.clone())))
            .into_element();
        let dark = wants_dark(appearance, t.dark);
        let current = library.get(&chosen);
        let name = current.map(label).unwrap_or_default();
        let mut pick = Line::new("settings.theme.row", trf("Theme: %@", &[&name])).control(
            rect()
                .on_sized(move |e: Event<SizedEventData>| anchor.set_if_modified(Bounds::of(&e)))
                .child(Button::new("settings.theme.all", tr("Alle Themes…")).on_press(move |_| open.set(true))),
        );
        if let Some(c) = current
            && !c.has(dark)
        {
            pick = pick.hint(if dark { trf("%@ gibt es nur hell.", &[&label(c)]) } else { trf("%@ gibt es nur dunkel.", &[&label(c)]) });
        }
        let mut rows = vec![language, mode, pick.into_element()];
        if cfg!(target_os = "linux") {
            rows.push(super::toggle(p, "settings.system_frame", tr("Fensterrahmen vom System"), tr("Zeigt die Titelleiste und die Fensterknöpfe deines Desktops."), s.system_frame, |s, on| {
                s.system_frame = on;
                crate::view::native::frame::set_system_frame(on, true);
            }));
        }
        for broken in &library.broken {
            let first = broken.errors.first().cloned().unwrap_or_default();
            rows.push(
                Line::new(format!("settings.theme.broken.{}", broken.file), broken.file.clone())
                    .hint(tr("Korrigiere die Datei im Themes-Ordner oder entferne sie."))
                    .control(Button::new(format!("settings.theme.broken.{}.folder", broken.file), tr("Ordner öffnen")).on_press(move |_| open_folder()))
                    .below(status(&t, first, t.danger_text))
                    .into_element(),
            );
        }
        let shown = *open.read();
        use_focus_return(shown);
        rect()
            .width(Size::fill())
            .child(group(&t, Some(tr("Allgemein")), rows, None))
            .maybe_child(shown.then(|| Gallery { library, chosen, dark, appearance, anchor: *anchor.read(), on_close: EventHandler::new(move |_| open.set(false)) }))
            .into_element()
    }
}

/// Every theme as a card, over the page under the button that opened it; picking applies at once and keeps it open.
#[derive(PartialEq)]
struct Gallery {
    library: Rc<Library>,
    chosen: String,
    dark: bool,
    appearance: Appearance,
    anchor: Bounds,
    on_close: EventHandler<()>,
}

impl Component for Gallery {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let p = use_prefs();
        let look = use_consume::<Look>();
        let ui = use_consume::<Ui>();
        let (w, h) = *ui.window.read();
        let width = GALLERY_W.min(w - space::L * 2.);
        let x = (self.anchor.x + self.anchor.w - width).clamp(space::L, (w - width - space::L).max(space::L));
        let below = self.anchor.y + self.anchor.h + space::XS;
        let (y, max_h) = if h - below - space::L >= GALLERY_MIN_H { (below, h - below - space::L) } else { (space::L, h - space::L * 2.) };
        let template = self.library.get(&self.chosen).cloned();
        let appearance = self.appearance;
        let close = self.on_close.clone();
        let keys = self.on_close.clone();
        let own = rect()
            .width(Size::fill())
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::S)
            .padding((space::S, 0., 0., 0.))
            .child(rect().width(Size::flex(1.)).spacing(space::XXS).child(line(&t, tr("Eigene Themes"), font::CALLOUT, t.text1)).child(note(&t, tr("Jede Theme-Datei im Themes-Ordner erscheint hier."))))
            .child(Button::new("settings.theme.copy", tr("Kopieren")).tip(tr("Legt das gewählte Theme als eigene Datei in den Themes-Ordner")).disabled(template.is_none().then(|| tr("Kein Theme gewählt"))).on_press(move |_| {
                if let Some(source) = &template {
                    copy(store, look, p, appearance, source.clone());
                }
            }))
            .child(Button::new("settings.theme.folder", tr("Ordner öffnen")).on_press(move |_| open_folder()))
            .child(Button::icon("settings.theme.reload", Icon::Refresh, tr("Themes neu laden")).on_press(move |_| look.reload()))
            .content(Content::flex());
        let panel = rect()
            .position(Position::new_absolute().left(x).top(y))
            .a11y_role(AccessibilityRole::Dialog)
            .a11y_alt(tr("Alle Themes"))
            .a11y_builder(|n| n.set_modal())
            .width(Size::px(width))
            .padding(space::M)
            .background(t.raised)
            .corner_radius(t.shape.control + 2.)
            .border(Border::new().fill(t.separator).width(1.).alignment(BorderAlignment::Outer))
            .shadow(t.elevation.float.clone())
            .on_mouse_down(|e: Event<MouseEventData>| e.stop_propagation())
            .child(
                ScrollView::new()
                    .width(Size::fill())
                    .height(Size::auto())
                    .max_height(Size::px(max_h - space::M * 2.))
                    .child(rect().width(Size::fill()).spacing(space::S).child(Picker { library: self.library.clone(), chosen: self.chosen.clone(), dark: self.dark, appearance }).child(own)),
            );
        rect()
            .position(Position::new_global().left(0.).top(0.))
            .layer(Layer::OverlayLevel(level::MENU))
            .width(Size::px(w))
            .height(Size::px(h))
            .on_mouse_down(move |_| close.call(()))
            .on_wheel(|e: Event<WheelEventData>| e.stop_propagation())
            .on_global_key_down(move |e: Event<KeyboardEventData>| {
                if e.key == Key::Named(NamedKey::Escape) {
                    e.stop_propagation();
                    e.prevent_default();
                    keys.call(());
                }
            })
            .child(panel)
    }
}

fn open_folder() {
    let dir = themes::dir();
    if std::fs::create_dir_all(&dir).is_ok() {
        open_external("open folder", dir.display().to_string());
    }
}

fn copy(store: Store, look: Look, p: Prefs, appearance: Appearance, source: themes::Theme) {
    let Some(rx) = off_thread("theme-copy", move || themes::copy_as_template(&source, &themes::dir()).map_err(|e| e.to_string())) else { return };
    spawn_in_window(async move {
        match rx.recv().await {
            Ok(Ok(path)) => {
                let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                toast::show(store.ui, tr("Kopie liegt im Themes-Ordner"), None);
                look.reload_then(move || {
                    if p.alive() {
                        choose(p, look, appearance, Some(format!("user/{stem}")));
                    }
                });
            }
            Ok(Err(e)) => store.dispatch(crate::app::event::Event::Error(trf("Das Theme ließ sich nicht kopieren: %@", &[&e]))),
            Err(_) => {}
        }
    });
}

#[derive(PartialEq)]
struct Picker {
    library: Rc<Library>,
    chosen: String,
    dark: bool,
    appearance: Appearance,
}

impl Component for Picker {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let p = use_prefs();
        let look = use_consume::<Look>();
        let a11y = use_a11y();
        let focus = use_focus(a11y);
        let ids: Vec<String> = self.library.themes.iter().map(|th| th.id.clone()).collect();
        let at = ids.iter().position(|id| *id == self.chosen).unwrap_or(0);
        let appearance = self.appearance;
        let keyed = ids.clone();
        let mut grid = rect()
            .a11y_id(a11y)
            .a11y_focusable(true)
            .a11y_auto_focus(true)
            .a11y_role(AccessibilityRole::RadioGroup)
            .a11y_alt(tr("Theme"))
            .width(Size::fill())
            .spacing(space::S)
            .on_key_down(move |e: Event<KeyboardEventData>| {
                if !a11y.is_focused() || keyed.is_empty() {
                    return;
                }
                let last = keyed.len() - 1;
                let next = match e.key {
                    Key::Named(NamedKey::ArrowRight) => (at + 1).min(last),
                    Key::Named(NamedKey::ArrowLeft) => at.saturating_sub(1),
                    Key::Named(NamedKey::ArrowDown) => (at + COLUMNS).min(last),
                    Key::Named(NamedKey::ArrowUp) => at.saturating_sub(COLUMNS),
                    Key::Named(NamedKey::Home) => 0,
                    Key::Named(NamedKey::End) => last,
                    _ => return,
                };
                e.stop_propagation();
                e.prevent_default();
                if next != at {
                    choose(p, look, appearance, Some(keyed[next].clone()));
                }
            });
        let focused = focus() == Focus::Keyboard;
        for (r, chunk) in self.library.themes.chunks(COLUMNS).enumerate() {
            let mut row = rect().width(Size::fill()).horizontal().spacing(space::S);
            for (c, theme) in chunk.iter().enumerate() {
                let i = r * COLUMNS + c;
                let id = theme.id.clone();
                row = row.child(Card {
                    theme: theme.clone(),
                    dark: self.dark,
                    on: i == at,
                    ring: focused && i == at,
                    on_press: EventHandler::new(move |_| {
                        a11y.request_focus();
                        choose(p, look, appearance, Some(id.clone()));
                    }),
                });
            }
            for _ in chunk.len()..COLUMNS {
                row = row.child(rect().width(Size::flex(1.)));
            }
            grid = grid.child(row.content(Content::flex()));
        }
        if self.library.themes.is_empty() {
            grid = grid.child(
                rect()
                    .width(Size::fill())
                    .horizontal()
                    .cross_align(Alignment::Center)
                    .spacing(space::S)
                    .child(note(&t, tr("Keine Themes gefunden.")))
                    .child(Button::new("settings.theme.empty.folder", tr("Ordner öffnen")).on_press(move |_| open_folder())),
            );
        }
        grid
    }
}

#[derive(PartialEq)]
struct Card {
    theme: themes::Theme,
    dark: bool,
    on: bool,
    ring: bool,
    on_press: EventHandler<()>,
}

impl Component for Card {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let mut hovered = use_state(|| false);
        let mut pressed = use_state(|| false);
        let th = &self.theme;
        let spot = use_spot(&format!("settings.theme.{}", th.id.replace('/', ".")), "radio", &label(th), true);
        let pal = th.palette(self.dark);
        let c = |k: Token| color(pal.get(k));
        let r = (th.radius * ROUND.0).clamp(0., ROUND.1);
        let tile = |attention: bool| {
            let mut b = rect().width(Size::flex(1.)).height(Size::fill()).corner_radius(r * TILE_ROUND).background(c(Token::Raised));
            if attention {
                b = b.border(Border::new().fill(c(Token::Attention)).width(ATTENTION_EDGE).alignment(BorderAlignment::Inner));
            }
            b
        };
        let bar = |share: f32, h: f32, token: Token| rect().width(Size::percent(share)).height(Size::px(h)).corner_radius(LINE_ROUND).background(c(token));
        let swatch_radius = t.shape.small + SWATCH_INSET;
        let mut swatch = rect()
            .width(Size::fill())
            .height(Size::px(SWATCH_H))
            .corner_radius(swatch_radius)
            .background(c(Token::Surface))
            .overflow(Overflow::Clip)
            .horizontal()
            .child(
                rect()
                    .width(Size::percent(SIDEBAR_SHARE))
                    .height(Size::fill())
                    .background(c(Token::Sidebar))
                    .padding(SWATCH_PAD)
                    .spacing(space::XS)
                    .child(bar(SIDE_LINES[0], ACCENT_H, Token::Accent))
                    .child(bar(SIDE_LINES[1], LINE_H, Token::Text3))
                    .child(bar(SIDE_LINES[2], LINE_H, Token::Text3)),
            )
            .child(
                rect()
                    .width(Size::flex(1.))
                    .height(Size::fill())
                    .padding(SWATCH_PAD)
                    .spacing(space::XS)
                    .child(bar(TITLE_LINE, ACCENT_H, Token::Text1))
                    .child(rect().width(Size::fill()).height(Size::flex(1.)).horizontal().spacing(space::XS).child(tile(false)).child(tile(true)).content(Content::flex()))
                    .child(rect().width(Size::fill()).horizontal().main_align(Alignment::End).child(rect().width(Size::px(PILL.0)).height(Size::px(PILL.1)).corner_radius(PILL.1 / 2.).background(c(Token::Accent))))
                    .content(Content::flex()),
            )
            .content(Content::flex());
        if *pressed.read() {
            swatch = swatch.child(rect().position(Position::new_absolute().top(0.).left(0.)).width(Size::fill()).height(Size::fill()).background(t.press));
        }
        let edge = if self.on { t.accent } else if *hovered.read() { t.text3 } else { t.separator };
        let mut frame = rect()
            .width(Size::fill())
            .padding(SWATCH_INSET)
            .corner_radius(t.shape.small + FRAME_GROW)
            .border(Border::new().fill(edge).width(if self.on { EDGE.1 } else { EDGE.0 }).alignment(BorderAlignment::Inner))
            .child(swatch);
        if self.ring
            && let Some(b) = ring(&t, Focus::Keyboard)
        {
            frame = frame.border(b);
        }
        let kind = match (th.light.is_some(), th.dark.is_some()) {
            (true, true) => tr("Hell und dunkel"),
            (false, true) => tr("Nur dunkel"),
            _ => tr("Nur hell"),
        };
        let kind = if matches!(th.origin, Origin::User(_)) { format!("{kind} · {}", tr("Eigenes")) } else { kind };
        let mut name = rect().width(Size::fill()).horizontal().cross_align(Alignment::Center).spacing(space::XS).child(line(&t, label(th), font::CALLOUT, t.text1).font_weight(if self.on { FontWeight::SEMI_BOLD } else { FontWeight::MEDIUM }).width(Size::flex(1.))).content(Content::flex());
        if self.on {
            name = name.child(icon(&t, Icon::Check, iconsize::S, t.accent_text));
        }
        let press = self.on_press.clone();
        rect()
            .a11y_role(AccessibilityRole::RadioButton)
            .a11y_alt(label(th))
            .width(Size::flex(1.))
            .spacing(space::XS)
            .cursor(CursorIcon::Pointer)
            .on_sized(spot)
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| {
                hovered.set_if_modified(false);
                pressed.set_if_modified(false);
            })
            .on_mouse_down(move |_| pressed.set(true))
            .on_mouse_up(move |_| pressed.set_if_modified(false))
            .on_pointer_press(move |e: Event<PointerEventData>| {
                if let PointerEventData::Mouse(m) = &*e
                    && m.button == Some(MouseButton::Left)
                {
                    press.call(());
                }
            })
            .child(frame)
            .child(rect().padding((0., space::XXS)).spacing(LABEL_GAP).child(name).child(line(&t, kind, font::CAPTION, t.text3)))
    }
}
