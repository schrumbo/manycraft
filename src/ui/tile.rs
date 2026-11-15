//! One instance in the mosaic: all preview, with the account and its status in a strip on top and the action that fits its phase on hover.

use std::collections::HashMap;
use std::sync::Arc;

use gpui_kit::component::menu::ContextMenuExt;
use gpui_kit::prelude::*;
use gpui_kit::*;
use uuid::Uuid;

use super::actions::{EditBatch, OpenGameFolder, Play, Reconnect, ReconnectMany, Respawn, RestartMany, ShowInstance, SignInAgain, Start, StartMany, Stop, StopMany};
use super::drive::Mark;
use super::main_window::MainWindow;
use super::model::{self, Phase};
use super::motion::Soft;
use super::identity;
use super::theme::{Theme, control, font, iconsize, radius, space, tile, tint, with_alpha};
use super::widgets::{icon, tip};
use crate::core::i18n::{tr, trf};

/// What a tile shows, copied out of the model for one frame.
pub struct TileData {
    pub key: String,
    pub name: String,
    pub account_id: Uuid,
    /// The account's colour index.
    pub color: u8,
    pub batch_id: String,
    pub phase: Phase,
    /// The status in a word.
    pub status: String,
    /// The full status, with the server, for the tooltip.
    pub detail: String,
    pub stopping: bool,
    pub whispers: u32,
    pub preview: Option<Arc<RenderImage>>,
    /// Whether `preview` is the running game rather than the still of an earlier run.
    pub live: bool,
    /// The account's skin face, on its chip and when there is no preview.
    pub face: Option<Arc<RenderImage>>,
    pub selected: bool,
    /// Whether the tile is part of a selection of several.
    pub picked: bool,
    pub playing: bool,
}

impl TileData {
    fn same(&self, other: &Self) -> bool {
        self.key == other.key
            && self.name == other.name
            && self.color == other.color
            && self.phase == other.phase
            && self.status == other.status
            && self.detail == other.detail
            && self.stopping == other.stopping
            && self.whispers == other.whispers
            && self.selected == other.selected
            && self.picked == other.picked
            && self.playing == other.playing
            && self.live == other.live
            && same_image(&self.preview, &other.preview)
            && same_image(&self.face, &other.face)
    }
}

fn same_image(a: &Option<Arc<RenderImage>>, b: &Option<Arc<RenderImage>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}

/// One tile as its own view, so a change to one instance redraws only its tile.
pub struct TileView {
    data: TileData,
    geometry: Geometry,
    offset: (f32, f32),
    hovered: bool,
    main: WeakEntity<MainWindow>,
}

impl Render for TileView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::get(cx);
        let (me, key) = (cx.entity(), self.data.key.clone());
        let hover = move |hovered: &bool, _: &mut Window, cx: &mut App| {
            if *hovered {
                unhover_except(Some(&key), cx);
                cx.set_global(Hovered(Some((key.clone(), me.downgrade()))));
            } else if cx.try_global::<Hovered>().and_then(|h| h.0.as_ref()).is_some_and(|(k, _)| *k == key) {
                cx.set_global(Hovered(None));
            }
            me.update(cx, |v, cx| {
                if v.hovered != *hovered {
                    v.hovered = *hovered;
                    cx.notify();
                }
            });
        };
        render(&self.data, &self.geometry, self.offset, self.hovered, hover, self.main.clone(), &t)
    }
}

/// The tile under the pointer, if any.
#[derive(Default)]
struct Hovered(Option<(String, WeakEntity<TileView>)>);

impl Global for Hovered {}

/// The key of the tile under the pointer.
pub fn hovered(cx: &App) -> Option<String> {
    cx.try_global::<Hovered>().and_then(|h| h.0.as_ref()).map(|(k, _)| k.clone())
}

/// Drops the hover of the tile under the pointer unless it is `key`, for tiles that left the grid without the pointer leaving them.
pub fn unhover_except(key: Option<&str>, cx: &mut App) {
    let Some((hovered, view)) = cx.try_global::<Hovered>().and_then(|h| h.0.clone()) else { return };
    if key == Some(hovered.as_str()) {
        return;
    }
    cx.set_global(Hovered(None));
    view.update(cx, |v, cx| {
        if v.hovered {
            v.hovered = false;
            cx.notify();
        }
    })
    .ok();
}

/// The tile views by instance key.
#[derive(Default)]
pub struct TileViews(HashMap<String, Entity<TileView>>);

impl Global for TileViews {}

/// The cached view of the tile for `data`; it redraws only when something it shows changed.
pub fn view(data: TileData, geometry: Geometry, offset: (f32, f32), main: WeakEntity<MainWindow>, cx: &mut App) -> AnyElement {
    let style = StyleRefinement::default().w(px(geometry.width)).h(px(geometry.height)).flex_none();
    let existing = cx.default_global::<TileViews>().0.get(&data.key).cloned();
    let entity = match existing {
        Some(entity) => {
            entity.update(cx, |v, cx| {
                if !v.data.same(&data) || v.geometry != geometry || v.offset != offset {
                    *v = TileView { data, geometry, offset, hovered: v.hovered, main };
                    cx.notify();
                }
            });
            entity
        }
        None => {
            let key = data.key.clone();
            let entity = cx.new(|cx: &mut Context<TileView>| {
                cx.observe_global::<Theme>(|_, cx| cx.notify()).detach();
                TileView { data, geometry, offset, hovered: false, main }
            });
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
            v.data.live = true;
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

/// The size of one tile.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Geometry {
    pub width: f32,
    pub height: f32,
}

impl Geometry {
    pub fn new(width: f32, height: f32) -> Self {
        Self { width, height }
    }

    /// Whether the overlay strip is too narrow for the status word.
    pub fn wordless(&self) -> bool {
        self.width < tile::WORDLESS
    }

    /// Where a picture of `aspect` (width over height) lies so that it covers the tile: left, top, width and height. A tile narrower
    /// than the picture loses both sides evenly; a wider one loses the top, keeping the bottom with hotbar, hearts and chat.
    pub fn cover(&self, aspect: f32) -> (f32, f32, f32, f32) {
        let aspect = aspect.max(f32::EPSILON);
        if self.width / self.height.max(1.) < aspect {
            let width = self.height * aspect;
            ((self.width - width) / 2., 0., width, self.height)
        } else {
            let height = self.width / aspect;
            (0., self.height - height, self.width, height)
        }
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
    Respawn,
    Start,
    SignIn,
}

fn primary(phase: &Phase, stopping: bool) -> Option<Primary> {
    if stopping {
        return None;
    }
    match phase {
        Phase::Disconnected { .. } => Some(Primary::Reconnect),
        Phase::Dead { .. } => Some(Primary::Respawn),
        Phase::Stopped | Phase::Crashed { .. } | Phase::Failed { .. } => Some(Primary::Start),
        Phase::LoginNeeded => Some(Primary::SignIn),
        Phase::Menu | Phase::Connecting { .. } | Phase::Queue { .. } | Phase::Playing { .. } => Some(Primary::Play),
        Phase::Starting { .. } | Phase::Plain => None,
        Phase::Banned { .. } => Some(Primary::Reconnect),
        Phase::Restarting { .. } => Some(Primary::Start),
    }
}

impl Primary {
    fn label(self) -> String {
        match self {
            Self::Play => tr("Spielen"),
            Self::Reconnect => tr("Neu verbinden"),
            Self::Respawn => tr("Wiederbeleben"),
            Self::Start => tr("Starten"),
            Self::SignIn => tr("Anmelden"),
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Play => "gamepad-2",
            Self::Reconnect => "refresh-cw",
            Self::Respawn => "heart-pulse",
            Self::Start => "play",
            Self::SignIn => "log-in",
        }
    }

    fn action(self, key: &str, account: Uuid) -> Box<dyn Action> {
        match self {
            Self::Play => Box::new(Play(key.to_owned())),
            Self::Reconnect => Box::new(Reconnect(key.to_owned())),
            Self::Respawn => Box::new(Respawn(key.to_owned())),
            Self::Start => Box::new(Start(key.to_owned())),
            Self::SignIn => Box::new(SignInAgain(account.to_string())),
        }
    }
}

/// Runs what the tile's main button would, for Enter on a tile whose game cannot be shown.
pub fn primary_action(key: &str, window: &mut Window, cx: &mut App) {
    let Some((phase, stopping, account)) = model::get(cx).read(cx).games.get(key).map(|i| (i.phase.clone(), i.stopping, i.account_id)) else { return };
    if let Some(p) = primary(&phase, stopping) {
        window.dispatch_action(p.action(key, account), cx);
    }
}

fn tone(d: &TileData, t: &Theme) -> Option<(Hsla, Hsla, &'static str)> {
    match d.phase {
        Phase::LoginNeeded => Some((t.danger, t.danger_text, "log-in")),
        Phase::Banned { .. } => Some((t.danger, t.danger_text, "ban")),
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

fn pill(id: String, p: Primary, d: &TileData, compact: bool, t: &Theme) -> Stateful<Div> {
    let label = p.label();
    let (fill, text) = if p == Primary::SignIn { (t.danger, t.on_danger) } else { (t.accent, t.on_accent) };
    div()
        .id(SharedString::from(id))
        .mark("button", label.clone())
        .h(px(control::S))
        .when(compact, |d| d.w(px(control::S)).justify_center())
        .when(!compact, |d| d.pl(px(space::S)).pr(px(space::M)))
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
        .when(!compact, |d| d.child(label.clone()))
        .when(compact, |d| d.tooltip(tip(label)))
        .on_click(dispatch(p.action(&d.key, d.account_id)))
}

fn round_button(id: String, icon_name: &'static str, hint: String, action: Box<dyn Action>, t: &Theme) -> Stateful<Div> {
    div()
        .id(SharedString::from(id))
        .mark("button", hint.clone())
        .size(px(control::M))
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

fn placeholder(d: &TileData, t: &Theme) -> Option<(&'static str, Hsla)> {
    if let Some((_, text, name)) = tone(d, t) {
        return Some((name, text));
    }
    match d.phase {
        Phase::Stopped if !d.stopping => None,
        Phase::Starting { .. } if !d.stopping => Some(("hourglass", t.tertiary)),
        Phase::Plain if !d.stopping => Some(("app-window", t.tertiary)),
        _ => Some(("loader-circle", t.tertiary)),
    }
}

fn glyph(d: &TileData, neutral: Hsla, t: &Theme) -> AnyElement {
    if let Some((fill, _, name)) = tone(d, t) {
        return icon(name, iconsize::XS, fill).into_any_element();
    }
    let dot = div().size(px(tile::GLYPH)).mx(px((iconsize::XS - tile::GLYPH) / 2.)).flex_none().rounded(px(radius::FULL));
    let dot = match d.phase {
        _ if d.stopping => dot.border_1().border_color(neutral),
        Phase::Playing { .. } => dot.bg(t.success),
        Phase::Menu | Phase::Queue { .. } | Phase::Plain => dot.bg(neutral),
        _ => dot.border_1().border_color(neutral),
    };
    dot.into_any_element()
}

fn aspect(image: &RenderImage) -> f32 {
    let size = image.size(0);
    let (w, h) = (size.width.0 as f32, size.height.0 as f32);
    if w > 0. && h > 0. { w / h } else { tile::ASPECT }
}

fn picture(d: &TileData, g: &Geometry, t: &Theme) -> Option<AnyElement> {
    let image = d.preview.as_ref()?;
    let (left, top, width, height) = g.cover(aspect(image));
    let shown = img(ImageSource::Render(Arc::clone(image))).absolute().left(px(left)).top(px(top)).w(px(width)).h(px(height)).object_fit(ObjectFit::Fill);
    Some(if d.live { shown.when(tone(d, t).is_some(), |i| i.opacity(tile::DIMMED)).into_any_element() } else { shown.grayscale(true).opacity(tint::STILL).into_any_element() })
}

fn actions(d: &TileData, g: &Geometry, t: &Theme) -> Option<Div> {
    let active = d.phase.is_active() && !d.stopping;
    let main = primary(&d.phase, d.stopping);
    let stop = (active && (main != Some(Primary::Start) || matches!(d.phase, Phase::Restarting { .. }))).then(|| round_button(format!("stop-{}", d.key), "square", tr("Stoppen"), Box::new(Stop(d.key.clone())), t));
    if main.is_none() && stop.is_none() {
        return None;
    }
    let shade = linear_gradient(180., linear_color_stop(black().opacity(0.), 0.), linear_color_stop(black().opacity(tint::PREVIEW_SHADE), 1.));
    Some(
        div()
            .absolute()
            .inset_0()
            .child(div().absolute().left_0().right_0().bottom_0().h(px(tile::SHADE.min(g.height))).bg(shade))
            .child(
                div()
                    .absolute()
                    .left(px(space::S))
                    .right(px(space::S))
                    .bottom(px(space::S))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(space::S))
                    .child(div().min_w_0().flex().children(main.map(|p| pill(format!("primary-{}", d.key), p, d, g.wordless(), t))))
                    .children(stop),
            ),
    )
}

fn whispers(d: &TileData, t: &Theme) -> Option<Stateful<Div>> {
    let n = d.whispers;
    (n > 0).then(|| {
        div()
            .id(SharedString::from(format!("whispers-{}", d.key)))
            .mark("badge", n.to_string())
            .flex_none()
            .min_w(px(tile::BADGE))
            .h(px(tile::BADGE))
            .px(px(space::XS))
            .rounded(px(radius::FULL))
            .bg(t.accent)
            .shadow(super::widgets::badge_edge(t))
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
    })
}

fn strip(d: &TileData, g: &Geometry, t: &Theme) -> Div {
    let pictured = d.preview.is_some();
    let (ink, quiet) = if pictured { (white(), white().opacity(tint::OVERLAY_TEXT)) } else { (t.primary, t.secondary) };
    let shade = pictured.then(|| {
        let fade = linear_gradient(180., linear_color_stop(black().opacity(tint::PREVIEW_SHADE), 0.), linear_color_stop(black().opacity(0.), 1.));
        div().absolute().top_0().left_0().right_0().h(px(tile::BAR_SHADE.min(g.height))).bg(fade)
    });
    let face = d.face.as_ref().map(|f| ImageSource::Render(Arc::clone(f)));
    let status = div()
        .id(SharedString::from(format!("status-{}", d.key)))
        .mark("status", d.status.clone())
        .flex()
        .flex_none()
        .items_center()
        .gap(px(space::XS))
        .child(glyph(d, quiet, t))
        .when(!g.wordless(), |s| s.child(div().text_size(px(font::CAPTION)).font_weight(FontWeight::MEDIUM).text_color(quiet).child(d.status.clone())))
        .tooltip(tip(d.detail.clone()));
    let name = div()
        .id(SharedString::from(format!("name-{}", d.key)))
        .flex_1()
        .min_w_0()
        .truncate()
        .text_size(px(font::CALLOUT))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(ink)
        .child(d.name.clone())
        .tooltip(tip(d.name.clone()));
    div().absolute().top_0().left_0().right_0().children(shade).child(
        div()
            .id(SharedString::from(format!("strip-{}", d.key)))
            .relative()
            .h(px(tile::BAR))
            .px(px(space::S))
            .flex()
            .items_center()
            .gap(px(space::XS + space::XXS))
            .child(identity::chip(&d.name, d.color, face, tile::CHIP, font::CAPTION2))
            .child(name)
            .children(whispers(d, t))
            .child(status),
    )
}

/// Draws one tile at `offset` from its slot, which animates a re-sort.
pub fn render(d: &TileData, g: &Geometry, offset: (f32, f32), hovered: bool, hover: impl Fn(&bool, &mut Window, &mut App) + 'static, view: WeakEntity<MainWindow>, t: &Theme) -> AnyElement {
    let tone = tone(d, t);
    let ring = if d.selected || d.picked {
        Some(t.accent)
    } else if hovered {
        Some(t.ink.opacity(tint::OUTLINE))
    } else if let Some((fill, _, _)) = tone {
        Some(fill)
    } else {
        d.playing.then(|| with_alpha(t.accent, tint::OUTLINE))
    };
    let mark = placeholder(d, t).filter(|_| !d.live).map(|(name, color)| icon(name, iconsize::L, color));
    let face = (d.preview.is_none() && mark.is_none()).then(|| match &d.face {
        Some(face) => img(ImageSource::Render(Arc::clone(face))).size(px(tile::FACE)).flex_none().opacity(tint::FACE).into_any_element(),
        None => icon("user", iconsize::L, with_alpha(t.secondary, tint::FACE)).into_any_element(),
    });
    let (key, name) = (d.key.clone(), d.name.clone());
    let select = view.clone();
    let (pick, pick_key) = (view.clone(), d.key.clone());
    let menu_view = view.clone();
    let menu_key = d.key.clone();
    let menu_batch = d.batch_id.clone();
    let (can_play, running, stopping) = (playable(&d.phase), d.phase.is_running(), d.stopping);
    let dead = matches!(d.phase, Phase::Dead { .. }) && !stopping;
    let reconnectable = running && !matches!(d.phase, Phase::Starting { .. } | Phase::Plain) && !stopping;
    let tile = super::drive::marked(div().id(SharedString::from(format!("tile-{}", d.key))), "clickable")
        .mark("tile", name)
        .relative()
        .left(px(offset.0))
        .top(px(offset.1))
        .size_full()
        .overflow_hidden()
        .bg(t.well)
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .active(|s| s.opacity(tint::PRESSED))
        .children(picture(d, g, t))
        .children(mark)
        .children(face)
        .child(strip(d, g, t))
        .children(actions(d, g, t).filter(|_| hovered || d.selected || d.phase == Phase::LoginNeeded))
        .children(ring.map(|c| div().absolute().inset_0().border(px(tile::RING)).border_color(c)))
        .on_hover(hover)
        .on_mouse_down(MouseButton::Right, move |_, window, cx| {
            let _ = pick.update(cx, |this, cx| {
                if this.grid.picks.set.contains(&pick_key) {
                    this.grid.selected = Some(pick_key.clone());
                } else {
                    this.grid.click(&pick_key, false, false);
                }
                window.focus(&this.grid.focus, cx);
                cx.notify();
            });
        })
        .on_click(move |e, window, cx| {
            let m = e.modifiers();
            let toggle = if cfg!(target_os = "macos") { m.platform } else { m.control };
            let _ = select.update(cx, |this, cx| {
                this.grid.click(&key, m.shift, toggle);
                window.focus(&this.grid.focus, cx);
                cx.notify();
            });
            if e.click_count() == 2 && can_play && !m.shift && !toggle {
                window.dispatch_action(Box::new(Play(key.clone())), cx);
            }
        });
    let (account, login) = (d.account_id, d.phase == Phase::LoginNeeded);
    tile.context_menu(move |menu, window, cx| {
        let chosen = menu_view.upgrade().map(|v| v.read(cx).grid.chosen()).unwrap_or_default();
        let several = chosen.len() > 1 && chosen.contains(&menu_key);
        let keys = if several { chosen.clone() } else { vec![menu_key.clone()] };
        let (ids, current) = accounts_of(&keys, cx);
        if several {
            let offers = super::grid::offers(&chosen, model::get(cx).read(cx));
            let mut menu = menu.label(trf("%lld ausgewählt", &[&chosen.len()]));
            for (n, (label, _, shown, count)) in offers.into_iter().enumerate() {
                let keys = super::palette::shortcut(shown.as_ref(), cx);
                let action: Box<dyn Action> = match n {
                    0 => Box::new(StartMany(chosen.clone())),
                    1 => Box::new(StopMany(chosen.clone())),
                    2 => Box::new(RestartMany(chosen.clone())),
                    3 => Box::new(ReconnectMany(chosen.clone())),
                    _ => shown,
                };
                menu = menu.menu_element_with_disabled(action, count == 0, super::palette::menu_row(label, keys, count == 0));
            }
            return menu.separator().submenu(tr("Farbe"), window, cx, move |m, _, _| identity::colors(m, ids.clone(), current));
        }
        let menu = if can_play { menu.menu(tr("Spielen"), Box::new(Play(menu_key.clone()))) } else { menu };
        let menu = if dead { menu.menu(tr("Wiederbeleben"), Box::new(Respawn(menu_key.clone()))) } else { menu };
        let menu = menu.menu_with_disabled(tr("Neu verbinden"), Box::new(Reconnect(menu_key.clone())), !reconnectable);
        let menu = if running || stopping {
            menu.menu_with_disabled(tr("Stoppen"), Box::new(Stop(menu_key.clone())), stopping)
        } else if login {
            menu.menu(tr("Anmelden"), Box::new(SignInAgain(account.to_string())))
        } else {
            menu.menu(tr("Starten"), Box::new(Start(menu_key.clone())))
        };
        let menu = menu.menu_with_disabled(tr("Neu starten"), Box::new(RestartMany(vec![menu_key.clone()])), stopping || login);
        menu.separator().submenu(tr("Farbe"), window, cx, move |m, _, _| identity::colors(m, ids.clone(), current)).map(|menu| match crate::core::fleet::solo_instance(&menu_batch) {
            Some(instance) => menu.menu(tr("Instanz öffnen"), Box::new(ShowInstance(instance.to_owned()))),
            None => menu.menu(tr("Batch bearbeiten"), Box::new(EditBatch(menu_batch.clone()))),
        }).menu(tr("Spielordner öffnen"), Box::new(OpenGameFolder(menu_key.clone())))
    })
    .into_any_element()
}

/// The account ids behind the instances `keys`, each once, and their colour when they all share one.
pub fn accounts_of(keys: &[String], cx: &App) -> (Vec<String>, Option<u8>) {
    let m = model::get(cx).read(cx);
    let mut ids: Vec<Uuid> = Vec::new();
    for id in keys.iter().filter_map(|k| m.games.get(k)).map(|i| i.account_id) {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    let colors: Vec<u8> = ids.iter().filter_map(|id| m.account(*id)).map(|a| a.color()).collect();
    let same = colors.first().copied().filter(|c| colors.iter().all(|o| o == c));
    (ids.iter().map(Uuid::to_string).collect(), same)
}

#[cfg(test)]
mod tests {
    use super::{Geometry, Phase, Primary, primary, tile};

    #[test]
    fn the_main_button_follows_the_phase() {
        let lost = Phase::Disconnected { server: None, reason: String::new() };
        assert!(primary(&lost, false) == Some(Primary::Reconnect));
        let dead = Phase::Dead { server: None, reason: String::new() };
        assert!(primary(&dead, false) == Some(Primary::Respawn));
        assert!(primary(&Phase::Stopped, false) == Some(Primary::Start));
        assert!(primary(&Phase::Crashed { code: Some(1) }, false) == Some(Primary::Start));
        assert!(primary(&Phase::Menu, false) == Some(Primary::Play));
        assert!(primary(&Phase::LoginNeeded, false) == Some(Primary::SignIn));
        assert!(primary(&Phase::Starting { done: 1, total: 2 }, false).is_none());
        assert!(primary(&Phase::Menu, true).is_none());
    }

    #[test]
    fn previews_cover_the_tile_cropping_the_sides_evenly_or_the_top() {
        let picture = 16. / 9.;
        assert_eq!(Geometry::new(320., 180.).cover(picture), (0., 0., 320., 180.), "a 16:9 tile shows the whole picture");
        assert_eq!(Geometry::new(160., 180.).cover(picture), (-80., 0., 320., 180.), "a narrower tile loses both sides evenly");
        assert_eq!(Geometry::new(640., 180.).cover(picture), (0., -180., 640., 360.), "a wider tile loses the top, keeps the bottom");
        assert_eq!(Geometry::new(100., 300.).cover(picture).1, 0., "a tall tile keeps its full height");
        assert!(Geometry::new(tile::MIN_W, tile::MIN_H).wordless() && !Geometry::new(tile::WORDLESS, tile::MIN_H).wordless());
    }

}
