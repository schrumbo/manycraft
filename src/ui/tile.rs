//! One instance in the mosaic: all preview, with the account and its status in a strip on top and the action that fits its phase on hover.

use std::collections::HashMap;
use std::sync::Arc;

use gpui_kit::component::menu::ContextMenuExt;
use gpui_kit::prelude::*;
use gpui_kit::*;
use uuid::Uuid;

use super::actions::{EditBatch, OpenGameFolder, Play, Reconnect, ReconnectMany, ReconnectSelected, Respawn, RestartMany, RestartSelected, ShowHistory, ShowInstance, SignInAgain, Start, StartMany, StartSelected, Stop, StopMany, StopSelected, TakeOver, OpenSettingsAt};
use super::instance_files::ShowCrash;
use super::drive::Mark;
use super::main_window::MainWindow;
use super::route::Section;
use super::model::{self, Game, Model, Phase};
use crate::app::model::{Primary, Stopping, main_action, primary};
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
    /// Why the account dropped, for the line under the strip.
    pub reason: Option<String>,
    /// The newest events, newest first, while the history popover of this tile is open.
    pub history: Option<Vec<Moment>>,
    pub stopping: bool,
    /// Whether the stop has run long enough that the tile offers to end the game at once.
    pub overdue: bool,
    pub preview: Option<Arc<RenderImage>>,
    /// Whether `preview` is the running game rather than the still of an earlier run.
    pub live: bool,
    /// The account's skin face, on its chip and when there is no preview.
    pub face: Option<Arc<RenderImage>>,
    pub selected: bool,
    /// Whether the tile is part of a selection of several.
    pub picked: bool,
    pub playing: bool,
    /// Whether the account plays this instance in another game, so a start here must end that one first.
    pub busy: bool,
    pub instance_id: String,
}

impl TileData {
    fn same(&self, other: &Self) -> bool {
        self.key == other.key
            && self.name == other.name
            && self.color == other.color
            && self.phase == other.phase
            && self.status == other.status
            && self.detail == other.detail
            && self.reason == other.reason
            && self.history == other.history
            && self.stopping == other.stopping
            && self.overdue == other.overdue
            && self.selected == other.selected
            && self.picked == other.picked
            && self.playing == other.playing
            && self.busy == other.busy
            && self.live == other.live
            && same_image(&self.preview, &other.preview)
            && same_image(&self.face, &other.face)
    }
}

/// One event in a tile's history popover: when, what, and the reason the server or game gave.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Moment {
    pub when: String,
    pub what: String,
    pub reason: String,
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
                super::state::watch(|w| w.hovered = Some(key.clone()), cx);
            } else if cx.try_global::<Hovered>().and_then(|h| h.0.as_ref()).is_some_and(|(k, _)| *k == key) {
                cx.set_global(Hovered(None));
                super::state::watch(|w| w.hovered = None, cx);
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
    super::state::watch(|w| w.hovered = None, cx);
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
    entity.cached(style).movable().into_any_element()
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
    matches!(phase, Phase::Menu | Phase::Connecting { .. } | Phase::Queue { .. } | Phase::Playing { .. } | Phase::NetworkLost { .. } | Phase::ServerAway { .. } | Phase::Disconnected { .. } | Phase::Dead { .. })
}

trait Look {
    fn icon(self) -> &'static str;
    fn action(self, key: &str, account: Uuid) -> Box<dyn Action>;
}

impl Look for Primary {
    fn icon(self) -> &'static str {
        match self {
            Self::Play => "gamepad-2",
            Self::Reconnect => "refresh-cw",
            Self::Respawn => "heart-pulse",
            Self::Start => "play",
            Self::Retry => "refresh-cw",
            Self::SignIn => "log-in",
            Self::TakeOver => "arrow-right-left",
            Self::ForceQuit => "power",
        }
    }

    fn action(self, key: &str, account: Uuid) -> Box<dyn Action> {
        match self {
            Self::Play => Box::new(Play(key.to_owned())),
            Self::ForceQuit => Box::new(Stop(key.to_owned())),
            Self::Reconnect => Box::new(Reconnect(key.to_owned())),
            Self::Respawn => Box::new(Respawn(key.to_owned())),
            Self::Start | Self::Retry => Box::new(Start(key.to_owned())),
            Self::SignIn => Box::new(SignInAgain(account.to_string())),
            Self::TakeOver => Box::new(TakeOver(key.to_owned())),
        }
    }
}

/// Whether the account of `game` plays its instance in another game while `game` itself does not run.
pub fn busy(game: &Game, m: &Model) -> bool {
    !game.phase.is_active() && m.playing_elsewhere(&game.instance_id, game.account_id, &game.key).is_some()
}

/// Runs what the tile's main button would, for Enter on a tile whose game cannot be shown.
pub fn primary_action(key: &str, window: &mut Window, cx: &mut App) {
    let m = model::get(cx).read(cx);
    let Some((phase, stopping, account)) = m.games.get(key).map(|i| (i.phase.clone(), Stopping::of(i.stopping, i.overdue), i.account_id)) else { return };
    let busy = m.games.get(key).is_some_and(|i| busy(i, m));
    if let Some(p) = main_action(&phase, stopping, busy).filter(|p| *p != Primary::ForceQuit) {
        window.dispatch_action(p.action(key, account), cx);
    }
}

fn tone(d: &TileData, t: &Theme) -> Option<(Hsla, Hsla, &'static str)> {
    d.phase.need().map(|n| super::needs::look(n, t))
}

fn dispatch(action: Box<dyn Action>) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    move |_, window, cx| {
        cx.stop_propagation();
        window.dispatch_action(action.boxed_clone(), cx);
    }
}

fn pill(id: String, p: Primary, d: &TileData, compact: bool, t: &Theme) -> Stateful<Div> {
    let label = p.label();
    let (fill, text) = match p {
        Primary::SignIn => (t.danger, t.on_danger),
        Primary::ForceQuit => (with_alpha(t.material, tint::FLOATING), t.primary),
        _ => (t.accent, t.on_accent),
    };
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
        .when(p == Primary::ForceQuit && t.dark, |d| d.border(px(tile::HAIRLINE)).border_color(t.primary.opacity(tint::OUTLINE)))
        .soft(if p == Primary::ForceQuit { t.ink.opacity(tint::HOVER * 2.) } else { t.shade }, radius::FULL)
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
        _ if cut_off(d) => Some(("wifi-off", t.tertiary)),
        Phase::Starting { .. } if !d.stopping => Some(("hourglass", t.tertiary)),
        Phase::Plain if !d.stopping => Some(("app-window", t.tertiary)),
        _ => Some(("loader-circle", t.tertiary)),
    }
}

fn glyph(d: &TileData, neutral: Hsla, t: &Theme) -> AnyElement {
    if let Some((fill, _, name)) = tone(d, t) {
        return icon(name, iconsize::XS, fill).into_any_element();
    }
    if cut_off(d) {
        return icon("wifi-off", iconsize::XS, neutral).into_any_element();
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

fn cut_off(d: &TileData) -> bool {
    !d.stopping && matches!(d.phase, Phase::NetworkLost { .. } | Phase::NoNetwork)
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
    let active = d.phase.stoppable() && !d.stopping;
    let main = main_action(&d.phase, Stopping::of(d.stopping, d.overdue), d.busy);
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

fn strip(d: &TileData, g: &Geometry, t: &Theme) -> Div {
    let pictured = d.preview.is_some();
    let (ink, quiet) = if pictured { (white(), white().opacity(tint::OVERLAY_TEXT)) } else { (t.primary, t.secondary) };
    let reason = d.reason.clone().filter(|_| !g.wordless());
    let shade = pictured.then(|| {
        let fade = linear_gradient(180., linear_color_stop(black().opacity(tint::PREVIEW_SHADE), 0.), linear_color_stop(black().opacity(0.), 1.));
        let tall = tile::BAR_SHADE + if reason.is_some() { tile::REASON } else { 0. };
        div().absolute().top_0().left_0().right_0().h(px(tall.min(g.height))).bg(fade)
    });
    let face = d.face.as_ref().map(|f| ImageSource::Render(Arc::clone(f)));
    let worded = !g.wordless() || d.selected;
    let word = match tone(d, t) {
        Some((_, text, _)) if d.selected && !pictured => text,
        Some((fill, _, _)) if d.selected => fill,
        _ => quiet,
    };
    let status = div()
        .id(SharedString::from(format!("status-{}", d.key)))
        .mark("status", d.status.clone())
        .flex()
        .flex_none()
        .items_center()
        .gap(px(space::XS))
        .child(glyph(d, quiet, t))
        .when(worded, |s| s.child(div().text_size(px(font::CAPTION)).font_weight(FontWeight::MEDIUM).text_color(word).child(d.status.clone())))
        .when(!g.wordless(), |s| s.tooltip(tip(d.detail.clone())));
    let name = div()
        .id(SharedString::from(format!("name-{}", d.key)))
        .flex_1()
        .min_w_0()
        .truncate()
        .text_size(px(font::CALLOUT))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(ink)
        .child(d.name.clone())
        .when(!g.wordless(), |n| n.tooltip(tip(d.name.clone())));
    let indent = space::S + tile::CHIP + space::XS + space::XXS;
    let because = reason.map(|r| {
        div()
            .id(SharedString::from(format!("reason-{}", d.key)))
            .mark("reason", r.clone())
            .relative()
            .h(px(tile::REASON))
            .pl(px(indent))
            .pr(px(space::S))
            .truncate()
            .text_size(px(font::CAPTION))
            .text_color(quiet)
            .tooltip(tip(r.clone()))
            .child(r)
    });
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
            .child(status),
    )
    .children(because)
}

fn history(d: &TileData, t: &Theme) -> Option<AnyElement> {
    let moments = d.history.as_ref()?;
    let row = |m: &Moment| {
        div()
            .flex()
            .items_baseline()
            .gap(px(space::S))
            .text_size(px(font::CALLOUT))
            .child(div().flex_none().w(px(tile::HISTORY_WHEN)).truncate().text_color(t.tertiary).font_features(super::toolbar::tabular()).child(m.when.clone()))
            .child(div().flex_none().text_color(t.primary).font_weight(FontWeight::MEDIUM).child(m.what.clone()))
            .when(!m.reason.is_empty(), |r| r.child(div().min_w_0().truncate().text_color(t.secondary).child(m.reason.clone())))
    };
    let list = if moments.is_empty() {
        div().text_size(px(font::CALLOUT)).text_color(t.secondary).child(tr("Noch nichts passiert.")).into_any_element()
    } else {
        div().flex().flex_col().gap(px(space::XS)).children(moments.iter().map(row)).into_any_element()
    };
    let card = div()
        .id(SharedString::from(format!("history-{}", d.key)))
        .mark("popover", tr("Verlauf"))
        .occlude()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(|_, _, cx| cx.stop_propagation())
        .w(px(tile::HISTORY))
        .p(px(space::M))
        .flex()
        .flex_col()
        .gap(px(space::S))
        .rounded(px(radius::CARD))
        .bg(t.material)
        .border_1()
        .border_color(t.separator)
        .shadow_lg()
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(space::XXS))
                .child(div().truncate().text_size(px(font::BODY)).font_weight(FontWeight::SEMIBOLD).text_color(t.primary).child(d.name.clone()))
                .child(div().text_size(px(font::CALLOUT)).text_color(t.secondary).child(d.detail.clone())),
        )
        .child(div().h(px(1.)).bg(t.separator))
        .child(list);
    Some(div().absolute().left(px(space::S)).bottom(px(space::S)).child(deferred(anchored().anchor(Anchor::TopLeft).snap_to_window_with_margin(px(space::S)).child(card)).with_priority(1)).into_any_element())
}

/// Draws one tile at `offset` from its slot, which animates a re-sort.
pub fn render(d: &TileData, g: &Geometry, offset: (f32, f32), hovered: bool, hover: impl Fn(&bool, &mut Window, &mut App) + 'static, view: WeakEntity<MainWindow>, t: &Theme) -> AnyElement {
    let tone = tone(d, t);
    let ring = if d.selected || d.picked {
        Some(t.selection_ring)
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
    let (can_play, running, stopping, overdue) = (playable(&d.phase) && !d.stopping, d.phase.is_running(), d.stopping, d.stopping && d.overdue);
    let dead = matches!(d.phase, Phase::Dead { .. }) && !stopping;
    let reconnectable = running && !matches!(d.phase, Phase::Starting { .. } | Phase::Plain) && !stopping;
    let dropped = matches!(d.phase, Phase::Disconnected { .. });
    let banned = matches!(d.phase, Phase::Banned { .. });
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
        .children(actions(d, g, t).filter(|_| hovered || d.selected || d.phase == Phase::LoginNeeded || (d.stopping && d.overdue)))
        .children(ring.map(|c| div().absolute().inset_0().border(px(tile::RING)).border_color(c)))
        .children(history(d, t))
        .when(g.wordless(), |tile| tile.tooltip(tip(format!("{} · {}", d.name, d.detail))))
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
    let (account, login, waits, busy) = (d.account_id, d.phase == Phase::LoginNeeded, d.phase == Phase::NoNetwork, d.busy);
    let (crashed, menu_instance) = (matches!(d.phase, Phase::Crashed { .. }), d.instance_id.clone());
    tile.context_menu(move |menu, window, cx| {
        let chosen = menu_view.upgrade().map(|v| v.read(cx).grid.chosen()).unwrap_or_default();
        let several = chosen.len() > 1 && chosen.contains(&menu_key);
        let keys = if several { chosen.clone() } else { vec![menu_key.clone()] };
        let (ids, current) = accounts_of(&keys, cx);
        if several {
            let offers = super::grid::offers(&chosen, model::get(cx).read(cx));
            let mut menu = menu.label(trf("%lld ausgewählt", &[&chosen.len()]));
            for (n, super::grid::Offer { label, action: shown, able: count, .. }) in offers.into_iter().enumerate() {
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
        let row = |text: String, shown: &dyn Action, disabled: bool| super::palette::menu_row(text, super::palette::shortcut(shown, cx), disabled);
        let menu = if can_play {
            let enter = Keystroke::parse("enter").ok().map(|k| super::palette::label(&k));
            menu.menu_element_with_disabled(Box::new(Play(menu_key.clone())), false, super::palette::menu_row(tr("Spielen"), enter, false))
        } else {
            menu
        };
        let menu = if dead { menu.menu(tr("Wiederbeleben"), Box::new(Respawn(menu_key.clone()))) } else { menu };
        let space = Keystroke::parse("space").ok().map(|k| super::palette::label(&k));
        let menu = menu.menu_element_with_disabled(Box::new(ShowHistory(menu_key.clone())), false, super::palette::menu_row(tr("Verlauf"), space, false));
        let menu = if crashed { menu.menu(tr("Absturzbericht…"), Box::new(ShowCrash { instance: menu_instance.clone(), account: account.to_string() })) } else { menu };
        let menu = if overdue {
            menu.menu_element_with_disabled(Box::new(Stop(menu_key.clone())), false, row(tr("Sofort beenden"), &StopSelected, false))
        } else if running || stopping || waits {
            menu.menu_element_with_disabled(Box::new(Stop(menu_key.clone())), stopping, row(tr("Stoppen"), &StopSelected, stopping))
        } else if login {
            menu.menu(tr("Anmelden"), Box::new(SignInAgain(account.to_string())))
        } else if busy {
            menu.menu(tr("Dort beenden und hier starten"), Box::new(TakeOver(menu_key.clone())))
        } else {
            menu.menu_element_with_disabled(Box::new(Start(menu_key.clone())), false, row(tr("Starten"), &StartSelected, false))
        };
        let restart = stopping || login;
        let menu = menu.menu_element_with_disabled(Box::new(RestartMany(vec![menu_key.clone()])), restart, row(tr("Neu starten"), &RestartSelected, restart));
        let menu = if banned { menu } else { menu.menu_element_with_disabled(Box::new(Reconnect(menu_key.clone())), !reconnectable, row(tr("Neu verbinden"), &ReconnectSelected, !reconnectable)) };
        let menu = if dropped { menu.menu(tr("Automatisch neu verbinden…"), Box::new(OpenSettingsAt(Section::Reconnect))) } else { menu };
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
    use super::{Geometry, Phase, Primary, Stopping, main_action, primary, tile};

    #[test]
    fn the_main_button_follows_the_phase() {
        let lost = Phase::Disconnected { server: None, reason: String::new() };
        assert!(primary(&lost, Stopping::No) == Some(Primary::Reconnect));
        let dead = Phase::Dead { server: None, reason: String::new() };
        assert!(primary(&dead, Stopping::No) == Some(Primary::Respawn));
        assert!(primary(&Phase::Stopped, Stopping::No) == Some(Primary::Start));
        assert!(primary(&Phase::Crashed { code: Some(1) }, Stopping::No) == Some(Primary::Start));
        assert!(primary(&Phase::Menu, Stopping::No) == Some(Primary::Play));
        assert!(primary(&Phase::LoginNeeded, Stopping::No) == Some(Primary::SignIn));
        assert!(primary(&Phase::Starting { place: 1, total: 2 }, Stopping::No).is_none());
        assert!(primary(&Phase::Menu, Stopping::Yes).is_none());
        assert!(primary(&Phase::Menu, Stopping::Overdue) == Some(Primary::ForceQuit));
        assert!(primary(&Phase::Starting { place: 0, total: 0 }, Stopping::Overdue) == Some(Primary::ForceQuit));
        assert!(main_action(&Phase::Failed { reason: String::new() }, Stopping::No, true) == Some(Primary::TakeOver));
        assert!(main_action(&Phase::Stopped, Stopping::No, false) == Some(Primary::Start));
        assert!(primary(&Phase::Banned { server: None, reason: String::new() }, Stopping::No).is_none());
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
