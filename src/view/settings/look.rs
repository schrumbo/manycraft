//! Erscheinungsbild: light, dark or as the system, and the theme, picked from the JSON themes and applied at once.

use std::rc::Rc;

use freya::prelude::*;

use super::{Prefs, use_prefs};
use crate::core::i18n::{tr, trf};
use crate::core::settings::Appearance;
use crate::core::themes::{self, Library, Origin, Token};
use crate::view::Look;
use crate::view::drive::use_spot;
use crate::view::icons::{Icon, icon};
use crate::view::kit::button::Button;
use crate::view::kit::group::{Line, group, note, status};
use crate::view::kit::segmented::{Segment, Segmented};
use crate::view::kit::text::line;
use crate::view::kit::{open_external, ring, toast, use_theme};
use crate::view::store::use_store;
use crate::view::theme::{color, font, iconsize, space};

const COLUMNS: usize = 4;
const SWATCH_H: f32 = 64.;

fn wants_dark(appearance: Appearance) -> bool {
    match appearance {
        Appearance::Light => false,
        Appearance::Dark => true,
        Appearance::System => *Platform::get().current_window().preferred_theme.peek() == PreferredTheme::Dark,
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

/// The appearance, the theme picker and the user's own theme files.
#[derive(PartialEq)]
pub struct LookGroup;

impl Component for LookGroup {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let p = use_prefs();
        let look = use_consume::<Look>();
        let s = p.read();
        let library = look.library.read().clone();
        let chosen = library.chosen(s.theme.as_deref()).map(|c| c.id.clone()).unwrap_or_default();
        let appearance = s.appearance;
        let modes = [Appearance::System, Appearance::Light, Appearance::Dark];
        let segments = vec![
            Segment { id: "settings.look.system".into(), label: tr("Automatisch"), tip: Some(tr("Folgt dem System")) },
            Segment { id: "settings.look.light".into(), label: tr("Hell"), tip: None },
            Segment { id: "settings.look.dark".into(), label: tr("Dunkel"), tip: None },
        ];
        let at = modes.iter().position(|m| *m == appearance).unwrap_or(0);
        let theme_id = s.theme.clone();
        let mode = Line::new("settings.look.row", tr("Erscheinungsbild"))
            .control(Segmented::new(tr("Erscheinungsbild"), segments, at).on_choose(move |i| choose(p, look, modes[i.min(2)], theme_id.clone())))
            .into_element();
        let dark = wants_dark(appearance);
        let mut pick = Line::new("settings.theme.row", tr("Theme")).below(Picker { library: library.clone(), chosen: chosen.clone(), dark, appearance });
        if let Some(c) = library.get(&chosen)
            && !c.has(dark)
        {
            pick = pick.hint(if dark { trf("%@ gibt es nur hell.", &[&c.name]) } else { trf("%@ gibt es nur dunkel.", &[&c.name]) });
        }
        let template = library.get(&chosen).cloned();
        let own = Line::new("settings.theme.own", tr("Eigene Themes"))
            .hint(tr("Jede JSON-Datei im Themes-Ordner erscheint hier. Eine Kopie eines Themes ist der schnellste Anfang."))
            .control(
                rect()
                    .horizontal()
                    .spacing(space::S)
                    .child(Button::new("settings.theme.copy", tr("Kopieren")).tip(tr("Legt das gewählte Theme als eigene Datei in den Themes-Ordner")).disabled(template.is_none().then(|| tr("Kein Theme gewählt"))).on_press(move |_| {
                        if let Some(source) = &template {
                            copy(store, look, p, appearance, source.clone());
                        }
                    }))
                    .child(Button::new("settings.theme.folder", tr("Ordner öffnen")).on_press(move |_| open_folder()))
                    .child(Button::icon("settings.theme.reload", Icon::Refresh, tr("Themes neu laden")).on_press(move |_| look.reload())),
            )
            .into_element();
        let mut rows = vec![mode, pick.into_element(), own];
        for broken in &library.broken {
            let first = broken.errors.first().cloned().unwrap_or_default();
            rows.push(Line::new(format!("settings.theme.broken.{}", broken.file), broken.file.clone()).below(status(&t, first, t.danger_text)).into_element());
        }
        group(&t, Some(tr("Erscheinungsbild")), rows, None)
    }
}

fn open_folder() {
    let dir = themes::dir();
    if std::fs::create_dir_all(&dir).is_ok() {
        open_external("open folder", dir.display().to_string());
    }
}

fn copy(store: crate::view::store::Store, look: Look, p: Prefs, appearance: Appearance, source: themes::Theme) {
    let (tx, rx) = async_channel::bounded(1);
    let spawned = std::thread::Builder::new().name("theme-copy".into()).spawn(move || {
        let _ = tx.send_blocking(themes::copy_as_template(&source, &themes::dir()).map_err(|e| e.to_string()));
    });
    if spawned.is_err() {
        return;
    }
    spawn_in_window(async move {
        match rx.recv().await {
            Ok(Ok(path)) => {
                look.reload();
                let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                choose(p, look, appearance, Some(format!("user/{stem}")));
                toast::show(store.ui, tr("Kopie liegt im Themes-Ordner"), None);
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
            grid = grid.child(note(&t, tr("Keine Themes gefunden.")));
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
        let th = &self.theme;
        let spot = use_spot(&format!("settings.theme.{}", th.id.replace('/', ".")), "radio", &th.name, true);
        let pal = th.palette(self.dark);
        let c = |k: Token| color(pal.get(k));
        let r = (th.radius * 4.).clamp(0., 8.);
        let tile = |attention: bool| {
            let mut b = rect().width(Size::flex(1.)).height(Size::fill()).corner_radius(r * 0.6).background(c(Token::Raised));
            if attention {
                b = b.border(Border::new().fill(c(Token::Attention)).width(1.5).alignment(BorderAlignment::Inner));
            }
            b
        };
        let swatch = rect()
            .width(Size::fill())
            .height(Size::px(SWATCH_H))
            .corner_radius(t.shape.small + 2.)
            .background(c(Token::Surface))
            .overflow(Overflow::Clip)
            .horizontal()
            .child(
                rect()
                    .width(Size::percent(28.))
                    .height(Size::fill())
                    .background(c(Token::Sidebar))
                    .padding(space::XS + 2.)
                    .spacing(space::XS)
                    .child(rect().width(Size::percent(80.)).height(Size::px(4.)).corner_radius(2.).background(c(Token::Accent)))
                    .child(rect().width(Size::percent(60.)).height(Size::px(3.)).corner_radius(2.).background(c(Token::Text3)))
                    .child(rect().width(Size::percent(70.)).height(Size::px(3.)).corner_radius(2.).background(c(Token::Text3))),
            )
            .child(
                rect()
                    .width(Size::flex(1.))
                    .height(Size::fill())
                    .padding(space::XS + 2.)
                    .spacing(space::XS)
                    .child(rect().width(Size::percent(55.)).height(Size::px(4.)).corner_radius(2.).background(c(Token::Text1)))
                    .child(rect().width(Size::fill()).height(Size::flex(1.)).horizontal().spacing(space::XS).child(tile(false)).child(tile(true)).content(Content::flex()))
                    .child(rect().width(Size::fill()).horizontal().main_align(Alignment::End).child(rect().width(Size::px(22.)).height(Size::px(6.)).corner_radius(3.).background(c(Token::Accent))))
                    .content(Content::flex()),
            )
            .content(Content::flex());
        let edge = if self.on { t.accent } else if *hovered.read() { t.text3 } else { t.separator };
        let mut frame = rect().width(Size::fill()).padding(2.).corner_radius(t.shape.small + 4.).border(Border::new().fill(edge).width(if self.on { 2. } else { 1. }).alignment(BorderAlignment::Inner)).child(swatch);
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
        let mut name = rect().width(Size::fill()).horizontal().cross_align(Alignment::Center).spacing(space::XS).child(line(&t, th.name.clone(), font::CALLOUT, t.text1).font_weight(if self.on { FontWeight::SEMI_BOLD } else { FontWeight::MEDIUM }).width(Size::flex(1.))).content(Content::flex());
        if self.on {
            name = name.child(icon(&t, Icon::Check, iconsize::S, t.accent_text));
        }
        let press = self.on_press.clone();
        rect()
            .a11y_role(AccessibilityRole::RadioButton)
            .a11y_alt(th.name.clone())
            .width(Size::flex(1.))
            .spacing(space::XS)
            .cursor(CursorIcon::Pointer)
            .on_sized(spot)
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| hovered.set_if_modified(false))
            .on_pointer_press(move |e: Event<PointerEventData>| {
                if let PointerEventData::Mouse(m) = &*e
                    && m.button == Some(MouseButton::Left)
                {
                    press.call(());
                }
            })
            .child(frame)
            .child(rect().padding((0., space::XXS)).spacing(1.).child(name).child(line(&t, kind, font::CAPTION, t.text3)))
    }
}
