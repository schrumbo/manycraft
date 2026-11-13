//! One instance in the grid: its preview, name and status, with the action that fits its phase.

use std::collections::HashMap;
use std::sync::Arc;

use gpui_kit::component::menu::ContextMenuExt;
use gpui_kit::prelude::*;
use gpui_kit::*;
use uuid::Uuid;

use super::actions::{EditBatch, Play, Reconnect, SignInAgain, Start, Stop};
use super::drive::Mark;
use super::main_window::MainWindow;
use super::model::{self, Phase};
use super::motion::Soft;
use super::theme::{Theme, control, font, iconsize, radius, shell, space, tile, tint, with_alpha};
use super::widgets::{KeyNav, icon, tip};
use crate::core::i18n::{tr, trf};

/// What a tile shows, copied out of the model for one frame.
pub struct TileData {
    pub key: String,
    pub name: String,
    pub account_id: Uuid,
    pub batch_id: String,
    pub phase: Phase,
    pub status: String,
    pub stopping: bool,
    pub whispers: u32,
    pub preview: Option<Arc<RenderImage>>,
    pub selected: bool,
    pub playing: bool,
}

impl TileData {
    fn same(&self, other: &Self) -> bool {
        self.key == other.key
            && self.name == other.name
            && self.phase == other.phase
            && self.status == other.status
            && self.stopping == other.stopping
            && self.whispers == other.whispers
            && self.selected == other.selected
            && self.playing == other.playing
            && match (&self.preview, &other.preview) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            }
    }
}

/// One tile as its own view, so a change to one instance redraws only its tile.
pub struct TileView {
    data: TileData,
    geometry: Geometry,
    offset: (f32, f32),
    focused: bool,
    main: WeakEntity<MainWindow>,
}

impl Render for TileView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::get(cx);
        render(&self.data, &self.geometry, self.offset, self.focused, self.main.clone(), &t)
    }
}

/// The tile views by instance key.
#[derive(Default)]
pub struct TileViews(HashMap<String, Entity<TileView>>);

impl Global for TileViews {}

/// The cached view of the tile for `data`; it redraws only when something it shows changed.
pub fn view(data: TileData, geometry: Geometry, offset: (f32, f32), focused: bool, main: WeakEntity<MainWindow>, cx: &mut App) -> AnyElement {
    let style = StyleRefinement::default().w(px(geometry.width)).h(px(geometry.height())).flex_none();
    let existing = cx.default_global::<TileViews>().0.get(&data.key).cloned();
    let entity = match existing {
        Some(entity) => {
            entity.update(cx, |v, cx| {
                if !v.data.same(&data) || v.geometry != geometry || v.offset != offset || v.focused != focused {
                    *v = TileView { data, geometry, offset, focused, main };
                    cx.notify();
                }
            });
            entity
        }
        None => {
            let key = data.key.clone();
            let entity = cx.new(|_| TileView { data, geometry, offset, focused, main });
            cx.global_mut::<TileViews>().0.insert(key, entity.clone());
            entity
        }
    };
    entity.cached(style).into_any_element()
}

/// Shows new previews in the tiles that are drawn, without redrawing anything else.
pub fn show_previews(previews: Vec<(String, Arc<RenderImage>)>, cx: &mut App) {
    for (key, image) in previews {
        let Some(entity) = cx.try_global::<TileViews>().and_then(|v| v.0.get(&key).cloned()) else { continue };
        entity.update(cx, |v, cx| {
            v.data.preview = Some(image);
            cx.notify();
        });
    }
}

/// Forgets the views of instances that are gone.
pub fn forget_except(keep: &dyn Fn(&str) -> bool, cx: &mut App) {
    if cx.has_global::<TileViews>() {
        cx.global_mut::<TileViews>().0.retain(|k, _| keep(k));
    }
}

/// Sizes of a tile of one width.
#[derive(Clone, Copy, PartialEq)]
pub struct Geometry {
    pub width: f32,
    pub preview: f32,
    pub strip: f32,
    pub small: bool,
}

impl Geometry {
    pub fn new(width: f32) -> Self {
        let small = width < tile::MEDIUM;
        Self { width, preview: (width * 9. / 16.).round(), strip: if small { tile::STRIP_SMALL } else { tile::STRIP }, small }
    }

    pub fn height(&self) -> f32 {
        self.preview + self.strip
    }
}

/// Whether the game of an instance in `phase` has a window to show.
pub fn playable(phase: &Phase) -> bool {
    matches!(phase, Phase::Menu | Phase::Connecting { .. } | Phase::Queue { .. } | Phase::Playing { .. } | Phase::Disconnected { .. } | Phase::Dead { .. })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Primary {
    Play,
    Reconnect,
    Start,
    SignIn,
}

fn primary(phase: &Phase, stopping: bool) -> Option<Primary> {
    if stopping {
        return None;
    }
    match phase {
        Phase::Disconnected { .. } | Phase::Dead { .. } => Some(Primary::Reconnect),
        Phase::Stopped | Phase::Crashed { .. } | Phase::Failed { .. } => Some(Primary::Start),
        Phase::LoginNeeded => Some(Primary::SignIn),
        Phase::Menu | Phase::Connecting { .. } | Phase::Queue { .. } | Phase::Playing { .. } => Some(Primary::Play),
        Phase::Starting { .. } => None,
    }
}

impl Primary {
    fn label(self) -> String {
        match self {
            Self::Play => tr("Spielen"),
            Self::Reconnect => tr("Neu verbinden"),
            Self::Start => tr("Starten"),
            Self::SignIn => tr("Anmelden"),
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Play => "gamepad-2",
            Self::Reconnect => "refresh-cw",
            Self::Start => "play",
            Self::SignIn => "log-in",
        }
    }

    fn action(self, key: &str, account: Uuid) -> Box<dyn Action> {
        match self {
            Self::Play => Box::new(Play(key.to_owned())),
            Self::Reconnect => Box::new(Reconnect(key.to_owned())),
            Self::Start => Box::new(Start(key.to_owned())),
            Self::SignIn => Box::new(SignInAgain(account.to_string())),
        }
    }
}

/// Runs what the tile's main button would, for Enter on a tile whose game cannot be shown.
pub fn primary_action(key: &str, window: &mut Window, cx: &mut App) {
    let Some((phase, stopping, account)) = model::get(cx).read(cx).instances.get(key).map(|i| (i.phase.clone(), i.stopping, i.account_id)) else { return };
    if let Some(p) = primary(&phase, stopping) {
        window.dispatch_action(p.action(key, account), cx);
    }
}

fn tone(d: &TileData, t: &Theme) -> Option<(Hsla, Hsla, &'static str)> {
    match d.phase {
        Phase::LoginNeeded => Some((t.danger, t.danger_text, "log-in")),
        Phase::Disconnected { .. } => Some((t.attention, t.attention_text, "wifi-off")),
        Phase::Dead { .. } | Phase::Crashed { .. } | Phase::Failed { .. } => Some((t.attention, t.attention_text, "triangle-alert")),
        _ => None,
    }
}

fn dispatch(action: Box<dyn Action>) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    move |_, window, cx| {
        cx.stop_propagation();
        window.dispatch_action(action.boxed_clone(), cx);
    }
}

fn pill(id: String, p: Primary, d: &TileData, t: &Theme) -> Stateful<Div> {
    let label = p.label();
    let (fill, text) = if p == Primary::SignIn { (t.danger, t.on_danger) } else { (t.accent, t.on_accent) };
    div()
        .id(SharedString::from(id))
        .mark("button", label.clone())
        .h(px(control::S))
        .px(px(space::M))
        .rounded(px(radius::FULL))
        .bg(fill)
        .text_color(text)
        .text_size(px(font::CALLOUT))
        .font_weight(FontWeight::SEMIBOLD)
        .flex()
        .flex_none()
        .items_center()
        .gap(px(space::XS))
        .cursor_pointer()
        .soft(t.shade, radius::FULL)
        .child(icon(p.icon(), iconsize::XS, text))
        .child(label)
        .on_click(dispatch(p.action(&d.key, d.account_id)))
}

fn round_button(id: String, icon_name: &'static str, hint: String, action: Box<dyn Action>, t: &Theme) -> Stateful<Div> {
    div()
        .id(SharedString::from(id))
        .mark("button", hint.clone())
        .size(px(control::S))
        .rounded(px(radius::FULL))
        .bg(with_alpha(t.material, tint::FLOATING))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .soft(t.ink.opacity(tint::HOVER * 2.), radius::FULL)
        .child(icon(icon_name, iconsize::XS, t.primary))
        .tooltip(tip(hint))
        .on_click(dispatch(action))
}

fn preview(d: &TileData, g: &Geometry, group: &SharedString, t: &Theme) -> AnyElement {
    let attention = tone(d, t).is_some();
    let content = match &d.preview {
        Some(image) => img(ImageSource::Render(Arc::clone(image))).size_full().object_fit(ObjectFit::Cover).when(attention, |i| i.opacity(tile::DIMMED)).into_any_element(),
        None => {
            let initial: String = d.name.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default();
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(t.raised)
                .child(div().text_size(px(if g.small { tile::INITIAL_SMALL } else { tile::INITIAL })).font_weight(FontWeight::SEMIBOLD).text_color(t.tertiary).child(initial))
                .into_any_element()
        }
    };
    let running = d.phase.is_running() && !d.stopping;
    let main = primary(&d.phase, d.stopping).filter(|p| *p != Primary::SignIn || g.small);
    let stop_or_start: Option<(&'static str, String, Box<dyn Action>)> = if running && main != Some(Primary::Start) {
        Some(("square", tr("Stoppen"), Box::new(Stop(d.key.clone()))))
    } else {
        None
    };
    let actions = div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .gap(px(space::S))
        .bg(black().opacity(tint::SCRIM))
        .opacity(if d.selected { 1. } else { 0. })
        .group_hover(group.clone(), |s| s.opacity(1.))
        .when_some(main, |row, p| row.child(pill(format!("primary-{}", d.key), p, d, t)))
        .when_some(stop_or_start, |row, (name, hint, action)| row.child(round_button(format!("stop-{}", d.key), name, hint, action, t)));
    let whispers = (d.whispers > 0).then(|| {
        let n = d.whispers;
        div()
            .id(SharedString::from(format!("whispers-{}", d.key)))
            .mark("badge", n.to_string())
            .absolute()
            .top(px(space::S))
            .right(px(space::S))
            .min_w(px(tile::BADGE))
            .h(px(tile::BADGE))
            .px(px(space::XS))
            .rounded(px(radius::FULL))
            .bg(t.accent)
            .text_color(t.on_accent)
            .text_size(px(font::CAPTION2))
            .font_weight(FontWeight::BOLD)
            .flex()
            .items_center()
            .justify_center()
            .gap(px(space::XXS))
            .child(icon("message-square", iconsize::XS, t.on_accent))
            .child(n.to_string())
            .tooltip(tip(if n == 1 { tr("1 ungelesene Flüsternachricht") } else { trf("%lld ungelesene Flüsternachrichten", &[&n]) }))
    });
    div().relative().w_full().h(px(g.preview)).overflow_hidden().bg(t.raised).child(content).child(actions).children(whispers).into_any_element()
}

fn strip(d: &TileData, g: &Geometry, t: &Theme) -> AnyElement {
    let tone = tone(d, t);
    let status_color = match tone {
        Some((_, text, _)) => text,
        None if d.phase == Phase::Stopped && !d.stopping => t.tertiary,
        None => t.secondary,
    };
    let running_dot = matches!(d.phase, Phase::Playing { .. }) && !d.stopping;
    let sign_in = (d.phase == Phase::LoginNeeded && !g.small).then(|| {
        let label = tr("Anmelden");
        div()
            .id(SharedString::from(format!("sign-in-{}", d.key)))
            .mark("button", label.clone())
            .h(px(control::S))
            .px(px(space::S))
            .rounded(px(radius::INNER))
            .border_1()
            .border_color(with_alpha(t.danger, tint::OUTLINE))
            .text_color(t.danger_text)
            .text_size(px(font::CALLOUT))
            .font_weight(FontWeight::MEDIUM)
            .flex()
            .flex_none()
            .items_center()
            .cursor_pointer()
            .key_nav()
            .soft(with_alpha(t.danger, tint::HOVER * 2.), radius::INNER)
            .child(label)
            .on_click(dispatch(Box::new(SignInAgain(d.account_id.to_string()))))
    });
    div()
        .h(px(g.strip))
        .when_some(tone, |s, (fill, _, _)| s.bg(with_alpha(fill, tint::ATTENTION)))
        .px(px(space::S + space::XXS))
        .flex()
        .items_center()
        .gap(px(space::S))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().text_size(px(if g.small { font::CALLOUT } else { font::BODY })).font_weight(FontWeight::SEMIBOLD).text_color(t.primary).truncate().child(d.name.clone()))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(space::XS))
                        .min_w_0()
                        .when_some(tone, |s, (_, text, name)| s.child(icon(name, iconsize::XS, text)))
                        .when(running_dot, |s| s.child(div().size(px(shell::DOT)).flex_none().rounded(px(radius::FULL)).bg(t.success)))
                        .child(
                            div()
                                .id(SharedString::from(format!("status-{}", d.key)))
                                .mark("status", d.status.clone())
                                .min_w_0()
                                .truncate()
                                .text_size(px(font::CAPTION))
                                .text_color(status_color)
                                .child(d.status.clone())
                                .when(g.small, |s| s.tooltip(tip(d.status.clone()))),
                        ),
                ),
        )
        .children(sign_in)
        .into_any_element()
}

/// Draws one tile at `offset` from its slot, which animates a re-sort.
pub fn render(d: &TileData, g: &Geometry, offset: (f32, f32), grid_focused: bool, view: WeakEntity<MainWindow>, t: &Theme) -> AnyElement {
    let group: SharedString = format!("tile-{}", d.key).into();
    let tone = tone(d, t);
    let edge = match (tone, d.playing) {
        (Some((ring, _, _)), _) => Some(ring),
        (None, true) => Some(with_alpha(t.accent, tint::OUTLINE)),
        _ => None,
    };
    let focus = (d.selected && grid_focused).then_some(t.accent);
    let (key, name) = (d.key.clone(), d.name.clone());
    let select = view.clone();
    let (pick, pick_key) = (view.clone(), d.key.clone());
    let menu_key = d.key.clone();
    let menu_batch = d.batch_id.clone();
    let (can_play, running, stopping) = (playable(&d.phase), d.phase.is_running(), d.stopping);
    let reconnectable = running && !matches!(d.phase, Phase::Starting { .. }) && !stopping;
    let tile = div()
        .id(SharedString::from(format!("tile-{}", d.key)))
        .mark("tile", name)
        .group(group.clone())
        .relative()
        .left(px(offset.0))
        .top(px(offset.1))
        .w(px(g.width))
        .h(px(g.height()))
        .flex_none()
        .flex()
        .flex_col()
        .rounded(px(radius::CARD))
        .overflow_hidden()
        .bg(t.content)
        .border_1()
        .border_color(t.separator)
        .when_some(edge, |s, color| s.border_color(color))
        .when_some(edge.filter(|_| focus.is_none()), |s, color| s.shadow(vec![BoxShadow { color, offset: point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(tile::RING - 1.), inset: false }]))
        .when_some(focus, |s, color| s.shadow(vec![BoxShadow { color, offset: point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(tile::RING + 1.), inset: false }]))
        .cursor_pointer()
        .soft(t.ink.opacity(tint::HOVER), radius::CARD)
        .child(preview(d, g, &group, t))
        .child(strip(d, g, t))
        .on_mouse_down(MouseButton::Right, move |_, window, cx| {
            let _ = pick.update(cx, |this, cx| {
                this.grid.selected = Some(pick_key.clone());
                window.focus(&this.grid.focus, cx);
                cx.notify();
            });
        })
        .on_click(move |e, window, cx| {
            let _ = select.update(cx, |this, cx| {
                this.grid.selected = Some(key.clone());
                window.focus(&this.grid.focus, cx);
                cx.notify();
            });
            if e.click_count() == 2 && can_play {
                window.dispatch_action(Box::new(Play(key.clone())), cx);
            }
        });
    let (account, login) = (d.account_id, d.phase == Phase::LoginNeeded);
    tile.context_menu(move |menu, _, _| {
        let menu = if can_play { menu.menu(tr("Spielen"), Box::new(Play(menu_key.clone()))) } else { menu };
        let menu = menu.menu_with_disabled(tr("Neu verbinden"), Box::new(Reconnect(menu_key.clone())), !reconnectable);
        let menu = if running || stopping {
            menu.menu_with_disabled(tr("Stoppen"), Box::new(Stop(menu_key.clone())), stopping)
        } else if login {
            menu.menu(tr("Anmelden"), Box::new(SignInAgain(account.to_string())))
        } else {
            menu.menu(tr("Starten"), Box::new(Start(menu_key.clone())))
        };
        menu.separator().menu(tr("Batch bearbeiten"), Box::new(EditBatch(menu_batch.clone())))
    })
    .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{Geometry, Phase, Primary, primary, tile};

    #[test]
    fn the_main_button_follows_the_phase() {
        let lost = Phase::Disconnected { server: None, reason: String::new() };
        assert!(primary(&lost, false) == Some(Primary::Reconnect));
        assert!(primary(&Phase::Stopped, false) == Some(Primary::Start));
        assert!(primary(&Phase::Crashed { code: Some(1) }, false) == Some(Primary::Start));
        assert!(primary(&Phase::Menu, false) == Some(Primary::Play));
        assert!(primary(&Phase::LoginNeeded, false) == Some(Primary::SignIn));
        assert!(primary(&Phase::Starting { done: 1, total: 2 }, false).is_none());
        assert!(primary(&Phase::Menu, true).is_none());
    }

    #[test]
    fn tiles_keep_sixteen_by_nine_previews() {
        let g = Geometry::new(320.);
        assert_eq!(g.preview, 180.);
        assert_eq!(g.height(), 180. + tile::STRIP);
        assert!(Geometry::new(160.).small);
    }
}
