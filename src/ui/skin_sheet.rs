//! The sheet that changes a Microsoft account's skin and cape: front and back preview, a skin file with its arm model,
//! the skins it wore lately and the capes it owns. A driven instance gets a canned profile and never reaches Minecraft services.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use gpui_kit::base::actions::{Cancel, Confirm};
use gpui_kit::prelude::*;
use gpui_kit::*;
use image::{Rgba, RgbaImage};
use uuid::Uuid;

use super::actions::{ChangeSkin, SignInAgain};
use super::drive::Mark;
use super::forms::{self, Problem};
use super::motion::Soft;
use super::sheet::{self, Dismissed, Look};
use super::state::render_image;
use super::theme::{Theme, font, iconsize, radius, space, tint, wardrobe};
use super::widgets::{KeyNav, icon, segment, segmented, tip};
use crate::core::account::{Account, AccountKind};
use crate::core::auth::{self, AuthError};
use crate::core::i18n::{tr, trf};
use crate::core::skin_change::{self as change, Applied, Cape, Model, Services, SkinError, Wardrobe};
use crate::core::{drive, skin, skins};

const CANNED_DELAY: Duration = Duration::from_millis(450);

/// Registers the skin action.
pub fn init(cx: &mut App) {
    cx.on_action(|action: &ChangeSkin, cx| {
        let Ok(id) = Uuid::parse_str(&action.0) else { return };
        let account = super::model::get(cx).read(cx).account(id).cloned();
        if let Some(account) = account.filter(|a| a.kind == AccountKind::Microsoft) {
            sheet::in_window(cx, move |window, cx| open(account, window, cx));
        }
    });
}

fn open(account: Account, window: &mut Window, cx: &mut App) {
    if sheet::busy(window, cx) {
        return;
    }
    let view = cx.new(|cx| SkinSheet::new(account, cx));
    cx.observe_release(&view, |s, cx| {
        for image in s.images.drain(..) {
            cx.drop_image(image, None);
        }
    })
    .detach();
    sheet::show(view, sheet::WIDE, window, cx);
}

struct Worn {
    png: Vec<u8>,
    image: RgbaImage,
    model: Model,
}

struct Closet {
    services: Option<Arc<Services>>,
    wardrobe: Wardrobe,
    skin: Option<RgbaImage>,
    capes: Vec<(String, RgbaImage)>,
    recent: Vec<Worn>,
}

struct Outcome {
    wardrobe: Option<Wardrobe>,
    skin: Option<RgbaImage>,
    face: Option<Vec<u8>>,
    recent: Option<Vec<Worn>>,
    error: Option<anyhow::Error>,
}

fn recent(id: Uuid) -> Vec<Worn> {
    change::recent(id)
        .into_iter()
        .filter_map(|r| {
            let png = std::fs::read(&r.path).ok()?;
            let image = change::check_png(&png).ok()?;
            Some(Worn { png, image, model: r.model })
        })
        .collect()
}

fn usable(image: RgbaImage) -> Option<RgbaImage> {
    matches!(image.dimensions(), (64, 64 | 32)).then_some(image)
}

fn load(account: &Account) -> Result<Closet> {
    let recent = recent(account.id);
    if drive::fenced("skin profile", &account.name) {
        return canned_closet(recent);
    }
    let session = auth::session_for(account)?;
    let services = Services::new(account.proxy.as_deref(), session.access_token)?;
    let wardrobe = services.wardrobe()?;
    let skin = wardrobe.skin_url.as_deref().and_then(|url| services.texture(url).ok()).and_then(usable);
    let capes = wardrobe.capes.iter().filter_map(|c| Some((c.id.clone(), services.texture(&c.url).ok()?))).collect();
    Ok(Closet { services: Some(Arc::new(services)), wardrobe, skin, capes, recent })
}

fn put_on(account: &Account, services: Option<Arc<Services>>, new: Option<Worn>, old: Option<(RgbaImage, Model)>, cape: Option<Option<String>>) -> Outcome {
    let skin = new.as_ref().map(|w| (w.png.as_slice(), w.model));
    let cape_ref = cape.as_ref().map(|c| c.as_deref());
    let applied = match services {
        Some(services) => change::apply(&services, skin, cape_ref),
        None => canned_apply(skin.map(|s| s.1), cape_ref),
    };
    let mut out = Outcome { wardrobe: applied.wardrobe, skin: None, face: None, recent: None, error: applied.error };
    if applied.skin_done
        && let Some(new) = new
    {
        if let Some((image, model)) = old
            && let Ok(png) = change::png(&image)
        {
            let _ = change::remember(account.id, &png, model);
        }
        let _ = change::remember(account.id, &new.png, new.model);
        if let Some(hash) = out.wardrobe.as_ref().and_then(|w| w.skin_url.as_deref()).and_then(change::texture_hash) {
            let _ = skins::adopt(account, &hash, &new.png);
        }
        out.face = skin::adopt(account.id, &new.image);
        out.skin = Some(new.image);
        out.recent = Some(recent(account.id));
    }
    out
}

fn canned_outcome() -> String {
    drive::dir().and_then(|d| std::fs::read_to_string(d.join("skin")).ok()).unwrap_or_default().trim().to_string()
}

fn canned_skin() -> RgbaImage {
    let mut s = RgbaImage::new(64, 64);
    let mut fill = |x0: u32, y0: u32, w: u32, h: u32, c: [u8; 3]| {
        for y in y0..y0 + h {
            for x in x0..x0 + w {
                let shade = if (x + y) % 3 == 0 { 12 } else { 0 };
                s.put_pixel(x, y, Rgba([c[0].saturating_sub(shade), c[1].saturating_sub(shade), c[2].saturating_sub(shade), 255]));
            }
        }
    };
    fill(0, 0, 32, 16, [0xc6, 0x8d, 0x6b]);
    fill(8, 0, 16, 8, [0x3b, 0x26, 0x15]);
    fill(0, 8, 32, 2, [0x3b, 0x26, 0x15]);
    fill(16, 16, 24, 16, [0x2b, 0x9c, 0xa8]);
    fill(40, 16, 16, 16, [0x2b, 0x9c, 0xa8]);
    fill(44, 26, 12, 6, [0xc6, 0x8d, 0x6b]);
    fill(0, 16, 16, 16, [0x35, 0x3f, 0x8c]);
    fill(16, 48, 16, 16, [0x35, 0x3f, 0x8c]);
    fill(32, 48, 16, 16, [0x2b, 0x9c, 0xa8]);
    fill(36, 58, 12, 6, [0xc6, 0x8d, 0x6b]);
    fill(9, 12, 2, 1, [0xff, 0xff, 0xff]);
    fill(13, 12, 2, 1, [0xff, 0xff, 0xff]);
    fill(10, 12, 1, 1, [0x4a, 0x3a, 0x8f]);
    fill(13, 12, 1, 1, [0x4a, 0x3a, 0x8f]);
    s
}

fn canned_cape(c: [u8; 3], stripe: [u8; 3]) -> RgbaImage {
    RgbaImage::from_fn(64, 32, |x, y| {
        let band = (6..9).contains(&y) || (x + y) % 7 == 0;
        let p = if band { stripe } else { c };
        Rgba([p[0], p[1], p[2], 255])
    })
}

const CANNED_CAPES: [(&str, &str, [u8; 3], [u8; 3]); 3] = [("canned-migrator", "Migrator", [0x6b, 0x3f, 0x1f], [0xd9, 0xa4, 0x41]), ("canned-pan", "Pan", [0x2f, 0x7d, 0x3a], [0xf2, 0xe2, 0x9a]), ("canned-common", "Common", [0x3a, 0x4c, 0x8c], [0xe0, 0xe0, 0xe0])];

fn canned_wardrobe(worn: Option<&str>) -> Wardrobe {
    let capes = if canned_outcome() == "nocapes" { &[][..] } else { &CANNED_CAPES[..] };
    Wardrobe {
        skin_url: Some("http://textures.minecraft.net/texture/ca11ed".into()),
        model: Model::Classic,
        capes: capes.iter().map(|(id, alias, _, _)| Cape { id: (*id).into(), alias: (*alias).into(), url: String::new(), active: worn == Some(*id) }).collect(),
    }
}

fn canned_closet(recent: Vec<Worn>) -> Result<Closet> {
    std::thread::sleep(CANNED_DELAY);
    match canned_outcome().as_str() {
        "loadfail" => return Err(SkinError::Unavailable(503).into()),
        "signin" => return Err(AuthError::SignInAgain.into()),
        _ => {}
    }
    let wardrobe = canned_wardrobe(Some("canned-pan"));
    let capes = wardrobe.capes.iter().filter_map(|c| CANNED_CAPES.iter().find(|k| k.0 == c.id).map(|k| (c.id.clone(), canned_cape(k.2, k.3)))).collect();
    Ok(Closet { services: None, wardrobe, skin: Some(canned_skin()), capes, recent })
}

fn canned_apply(skin: Option<Model>, cape: Option<Option<&str>>) -> Applied {
    std::thread::sleep(CANNED_DELAY);
    let outcome = canned_outcome();
    drive::note(format!("skin canned {}", if outcome.is_empty() { "ok" } else { &outcome }));
    if outcome == "ratelimit" {
        return Applied { wardrobe: None, skin_done: false, error: Some(SkinError::RateLimited.into()) };
    }
    let mut wardrobe = canned_wardrobe(cape.flatten());
    if let Some(model) = skin {
        wardrobe.model = model;
        wardrobe.skin_url = Some(format!("http://textures.minecraft.net/texture/{}", Uuid::new_v4().simple()));
    }
    let error = (outcome == "capefail" && cape.is_some()).then(|| SkinError::CapeRejected.into());
    Applied { wardrobe: Some(wardrobe), skin_done: skin.is_some(), error }
}

fn skin_text(e: &SkinError) -> String {
    match e {
        SkinError::NotPng => tr("Das ist keine PNG-Datei. Wähl ein Skin-Bild im PNG-Format."),
        SkinError::WrongSize(w, h) => trf("Ein Skin ist 64 × 64 oder 64 × 32 Pixel groß, dieses Bild %lld × %lld.", &[w, h]),
        SkinError::TooLarge => tr("Die Datei ist zu groß für einen Skin."),
        SkinError::RateLimited => tr("Microsoft lässt gerade keine Änderung zu, versuch es in einer Minute nochmal."),
        SkinError::SignInAgain => tr("Die Anmeldung ist abgelaufen. Schließ das Fenster und öffne „Skin ändern“ noch einmal."),
        SkinError::SkinRejected => tr("Minecraft hat diesen Skin abgelehnt. Versuch es mit einer anderen Datei."),
        SkinError::CapeRejected => tr("Diesen Umhang kann das Konto gerade nicht tragen."),
        SkinError::Unavailable(_) => tr("Die Minecraft-Dienste antworten gerade nicht. Versuch es gleich noch einmal."),
    }
}

fn problem(e: &anyhow::Error) -> Problem {
    if let Some(s) = e.chain().find_map(|c| c.downcast_ref::<SkinError>()) {
        let detail = matches!(s, SkinError::Unavailable(_)).then(|| format!("{e:#}"));
        return Problem { text: skin_text(s), detail };
    }
    if let Some(a) = e.chain().find_map(|c| c.downcast_ref::<AuthError>()) {
        return Problem::plain(super::remote::auth_text(a));
    }
    forms::explain(e)
}

fn signs_in_again(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.downcast_ref::<AuthError>() == Some(&AuthError::SignInAgain))
}

struct Picked {
    name: Option<String>,
    worn: Worn,
    recent: Option<usize>,
}

struct Ready {
    services: Option<Arc<Services>>,
    wardrobe: Wardrobe,
    skin: Option<RgbaImage>,
    capes: Vec<(Cape, Option<Arc<RenderImage>>)>,
}

enum Load {
    Loading,
    Ready(Ready),
    Failed(Problem, bool),
}

struct SkinSheet {
    account: Account,
    load: Load,
    pick: Option<Picked>,
    pick_problem: Option<String>,
    picking: bool,
    model: Model,
    cape: Option<String>,
    recent: Vec<(Worn, Arc<RenderImage>)>,
    views: Option<(Arc<RenderImage>, Arc<RenderImage>)>,
    applying: bool,
    error: Option<Problem>,
    details: bool,
    images: Vec<Arc<RenderImage>>,
    root: FocusHandle,
}

impl EventEmitter<Dismissed> for SkinSheet {}

fn picture(image: &RgbaImage, points: f32) -> Option<Arc<RenderImage>> {
    let scaled = change::scaled(image, (points * wardrobe::DENSITY).round() as u32);
    render_image(scaled.width(), scaled.height(), scaled.into_raw())
}

impl SkinSheet {
    fn new(account: Account, cx: &mut Context<Self>) -> Self {
        let mut sheet = Self {
            account,
            load: Load::Loading,
            pick: None,
            pick_problem: None,
            picking: false,
            model: Model::Classic,
            cape: None,
            recent: Vec::new(),
            views: None,
            applying: false,
            error: None,
            details: false,
            images: Vec::new(),
            root: cx.focus_handle(),
        };
        sheet.reload(cx);
        sheet
    }

    fn keep(&mut self, image: Option<Arc<RenderImage>>) -> Option<Arc<RenderImage>> {
        if let Some(image) = &image {
            self.images.push(Arc::clone(image));
        }
        image
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        self.load = Load::Loading;
        self.error = None;
        let account = self.account.clone();
        let task = cx.background_executor().spawn(async move { load(&account) });
        cx.spawn(async move |this, cx| {
            let loaded = task.await;
            let _ = this.update(cx, |s, cx| s.loaded(loaded, cx));
        })
        .detach();
        cx.notify();
    }

    fn loaded(&mut self, loaded: Result<Closet>, cx: &mut Context<Self>) {
        match loaded {
            Ok(closet) => {
                let capes = closet
                    .wardrobe
                    .capes
                    .iter()
                    .map(|c| {
                        let shown = closet.capes.iter().find(|(id, _)| *id == c.id).and_then(|(_, image)| change::cape_front(image)).and_then(|front| picture(&front, wardrobe::CAPE));
                        (c.clone(), self.keep(shown))
                    })
                    .collect();
                self.model = closet.wardrobe.model;
                self.cape = closet.wardrobe.cape().map(str::to_owned);
                self.set_recent(closet.recent);
                self.load = Load::Ready(Ready { services: closet.services, wardrobe: closet.wardrobe, skin: closet.skin, capes });
                self.redraw(cx);
            }
            Err(e) => self.load = Load::Failed(problem(&e), signs_in_again(&e)),
        }
        cx.notify();
    }

    fn set_recent(&mut self, recent: Vec<Worn>) {
        let mut kept = Vec::with_capacity(recent.len());
        for worn in recent {
            let (front, _) = change::front_and_back(&worn.image, worn.model);
            if let Some(thumb) = self.keep(picture(&front, wardrobe::RECENT)) {
                kept.push((worn, thumb));
            }
        }
        self.recent = kept;
    }

    fn shown_skin(&self) -> Option<&RgbaImage> {
        match (&self.pick, &self.load) {
            (Some(p), _) => Some(&p.worn.image),
            (None, Load::Ready(r)) => r.skin.as_ref(),
            _ => None,
        }
    }

    fn redraw(&mut self, cx: &mut Context<Self>) {
        let views = self.shown_skin().map(|s| change::front_and_back(s, self.model));
        let new = views.and_then(|(front, back)| Some((picture(&front, wardrobe::PREVIEW)?, picture(&back, wardrobe::PREVIEW)?)));
        if let Some((front, back)) = std::mem::replace(&mut self.views, new) {
            self.images.retain(|i| !Arc::ptr_eq(i, &front) && !Arc::ptr_eq(i, &back));
            cx.drop_image(front, None);
            cx.drop_image(back, None);
        }
        if let Some((front, back)) = &self.views {
            self.images.push(Arc::clone(front));
            self.images.push(Arc::clone(back));
        }
        cx.notify();
    }

    fn ready(&self) -> Option<&Ready> {
        match &self.load {
            Load::Ready(r) => Some(r),
            _ => None,
        }
    }

    fn skin_changes(&self) -> bool {
        self.pick.is_some() || self.ready().is_some_and(|r| r.skin.is_some() && self.model != r.wardrobe.model)
    }

    fn cape_changes(&self) -> bool {
        self.ready().is_some_and(|r| self.cape.as_deref() != r.wardrobe.cape())
    }

    fn can_apply(&self) -> bool {
        !self.applying && self.ready().is_some() && (self.skin_changes() || self.cape_changes())
    }

    fn choose_file(&mut self, cx: &mut Context<Self>) {
        if self.applying || self.picking {
            return;
        }
        self.picking = true;
        cx.notify();
        let prompt = (!drive::on()).then(|| cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some(tr("Skin wählen").into()) }));
        cx.spawn(async move |this, cx| {
            let paths: Option<Vec<PathBuf>> = match prompt {
                Some(prompt) => prompt.await.ok().and_then(Result::ok).flatten(),
                None => drive::answer("skin file"),
            };
            let Some(path) = paths.and_then(|p| p.into_iter().next()) else {
                let _ = this.update(cx, |s, cx| {
                    s.picking = false;
                    cx.notify();
                });
                return;
            };
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
            let read = cx.background_executor().spawn(async move { change::read_png(&path) }).await;
            let _ = this.update(cx, |s, cx| {
                s.picking = false;
                match read {
                    Ok((png, image)) => {
                        s.model = change::guess_model(&image);
                        s.pick = Some(Picked { name, worn: Worn { png, model: s.model, image }, recent: None });
                        s.pick_problem = None;
                        s.error = None;
                        s.redraw(cx);
                    }
                    Err(e) => {
                        s.pick_problem = Some(problem(&e).text);
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn use_recent(&mut self, at: usize, cx: &mut Context<Self>) {
        if self.applying {
            return;
        }
        let Some((worn, _)) = self.recent.get(at) else { return };
        self.model = worn.model;
        self.pick = Some(Picked { name: None, worn: Worn { png: worn.png.clone(), image: worn.image.clone(), model: worn.model }, recent: Some(at) });
        self.pick_problem = None;
        self.error = None;
        self.redraw(cx);
    }

    fn set_model(&mut self, model: Model, cx: &mut Context<Self>) {
        if self.applying || self.model == model {
            return;
        }
        self.model = model;
        self.error = None;
        self.redraw(cx);
    }

    fn set_cape(&mut self, cape: Option<String>, cx: &mut Context<Self>) {
        if self.applying {
            return;
        }
        self.cape = cape;
        self.error = None;
        cx.notify();
    }

    fn apply(&mut self, cx: &mut Context<Self>) {
        if !self.can_apply() {
            return;
        }
        let Some(ready) = self.ready() else { return };
        let services = ready.services.clone();
        let old = ready.skin.clone().map(|s| (s, ready.wardrobe.model));
        let new = if self.skin_changes() {
            match &self.pick {
                Some(p) => Some(Worn { png: p.worn.png.clone(), image: p.worn.image.clone(), model: self.model }),
                None => old.as_ref().and_then(|(image, _)| Some(Worn { png: change::png(image).ok()?, image: image.clone(), model: self.model })),
            }
        } else {
            None
        };
        let old = if self.pick.is_some() { old } else { None };
        let cape = self.cape_changes().then(|| self.cape.clone());
        self.applying = true;
        self.error = None;
        cx.notify();
        let account = self.account.clone();
        let task = cx.background_executor().spawn(async move { put_on(&account, services, new, old, cape) });
        cx.spawn(async move |this, cx| {
            let outcome = task.await;
            let _ = this.update(cx, |s, cx| s.applied(outcome, cx));
        })
        .detach();
    }

    fn applied(&mut self, outcome: Outcome, cx: &mut Context<Self>) {
        self.applying = false;
        if let Some(face) = outcome.face {
            super::stills::show_face(self.account.id, &face, cx);
        }
        if outcome.skin.is_some() {
            super::accounts::redraw(cx);
        }
        let Some(error) = outcome.error else {
            cx.emit(Dismissed);
            return;
        };
        if let Some(recent) = outcome.recent {
            self.set_recent(recent);
        }
        if let Load::Ready(ready) = &mut self.load {
            if let Some(skin) = outcome.skin {
                ready.skin = Some(skin);
                self.pick = None;
            }
            if let Some(w) = outcome.wardrobe {
                for (cape, _) in &mut ready.capes {
                    cape.active = w.cape() == Some(cape.id.as_str());
                }
                ready.wardrobe.model = w.model;
                ready.wardrobe.skin_url = w.skin_url;
                ready.wardrobe.capes = w.capes;
            }
        }
        self.error = Some(problem(&error));
        self.redraw(cx);
    }

    fn sign_in_again(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.account.id.to_string();
        cx.emit(Dismissed);
        window.dispatch_action(Box::new(SignInAgain(id)), cx);
    }

    fn render_preview(&self, t: &Theme) -> Div {
        let side = |image: Option<Arc<RenderImage>>, label: String| {
            let (w, h) = (change::VIEW_W as f32 * wardrobe::PREVIEW, change::VIEW_H as f32 * wardrobe::PREVIEW);
            let shown = match image {
                Some(image) => img(ImageSource::Render(image)).w(px(w)).h(px(h)).object_fit(ObjectFit::Fill).into_any_element(),
                None => div().w(px(w)).h(px(h)).flex().items_center().justify_center().child(icon("user", iconsize::HERO, t.tertiary)).into_any_element(),
            };
            div().flex().flex_col().items_center().gap(px(space::S)).child(shown).child(div().text_size(px(font::CAPTION)).text_color(t.secondary).child(label))
        };
        let (front, back) = match &self.views {
            Some((f, b)) => (Some(Arc::clone(f)), Some(Arc::clone(b))),
            None => (None, None),
        };
        div()
            .w(px(wardrobe::COLUMN))
            .flex_none()
            .flex()
            .justify_center()
            .gap(px(space::L))
            .py(px(space::L))
            .rounded(px(radius::CARD))
            .bg(t.well)
            .child(side(front, tr("Vorne")))
            .child(side(back, tr("Hinten")))
    }

    fn render_skin(&self, cx: &mut Context<Self>, t: &Theme) -> Div {
        let busy = self.applying;
        let file = sheet::button("skin-file", tr("Datei wählen…"), Some("file-image"), Look::Secondary, busy, self.picking, t).on_click(cx.listener(|s, _, _, cx| s.choose_file(cx)));
        let chosen = match &self.pick {
            Some(Picked { name: Some(name), .. }) => Some(name.clone()),
            Some(Picked { recent: Some(_), .. }) => Some(tr("Zuletzt verwendet")),
            _ => None,
        };
        let hint = Some(tr("PNG mit 64 × 64 oder 64 × 32 Pixeln"));
        let enabled = !busy && self.shown_skin().is_some();
        let pick = cx.listener(|s, n: &usize, _, cx| s.set_model(if *n == 1 { Model::Slim } else { Model::Classic }, cx));
        let segments = [segment("model-classic", tr("Klassisch"), self.model == Model::Classic, enabled, t), segment("model-slim", tr("Schlank"), self.model == Model::Slim, enabled, t)];
        let model = segmented("skin-model", segments, move |n, window, cx| pick(&n, window, cx), t).when(!enabled, |d| d.opacity(tint::DISABLED));
        div()
            .flex()
            .flex_col()
            .gap(px(space::XS))
            .child(sheet::label(tr("Skin"), t))
            .child(div().flex().items_center().gap(px(space::S)).child(file).children(chosen.map(|c| div().min_w_0().truncate().text_size(px(font::CALLOUT)).text_color(t.secondary).child(c))))
            .child(sheet::note(hint, self.pick_problem.clone(), t))
            .child(div().pt(px(space::XS)).child(sheet::label(tr("Modell"), t)))
            .child(div().flex().child(model))
    }

    fn render_recent(&self, cx: &mut Context<Self>, t: &Theme) -> Option<Div> {
        if self.recent.is_empty() {
            return None;
        }
        let chosen = self.pick.as_ref().and_then(|p| p.recent);
        let (w, h) = (change::VIEW_W as f32 * wardrobe::RECENT, change::VIEW_H as f32 * wardrobe::RECENT);
        let items = self.recent.iter().enumerate().map(|(n, (_, thumb))| {
            let selected = chosen == Some(n);
            div()
                .id(("recent-skin", n))
                .mark("option", trf("Zuletzt verwendet %lld", &[&(n + 1)]))
                .p(px(space::XS))
                .rounded(px(radius::CONTROL))
                .border_2()
                .border_color(if selected { t.accent } else { transparent_black() })
                .cursor_pointer()
                .key_nav()
                .soft(t.ink.opacity(tint::HOVER), radius::CONTROL)
                .tooltip(tip(tr("Diesen Skin wieder nehmen")))
                .on_click(cx.listener(move |s, _, _, cx| s.use_recent(n, cx)))
                .child(img(ImageSource::Render(Arc::clone(thumb))).w(px(w)).h(px(h)).object_fit(ObjectFit::Fill))
        });
        Some(div().flex().flex_col().gap(px(space::XS)).child(sheet::label(tr("Zuletzt verwendet"), t)).child(div().flex().flex_wrap().gap(px(space::XS)).children(items)))
    }

    fn render_capes(&self, ready: &Ready, cx: &mut Context<Self>, t: &Theme) -> Div {
        let section = div().flex().flex_col().gap(px(space::XS)).child(sheet::label(tr("Umhang"), t));
        if ready.capes.is_empty() {
            return section.child(div().text_size(px(font::CALLOUT)).text_color(t.secondary).child(tr("Dieses Konto hat keine Umhänge.")));
        }
        let (w, h) = (change::CAPE_W as f32 * wardrobe::CAPE, change::CAPE_H as f32 * wardrobe::CAPE);
        let choice = |id: SharedString, label: String, selected: bool, look: AnyElement, pick: Option<String>| {
            div()
                .id(id)
                .mark("option", label.clone())
                .w(px(wardrobe::CHOICE))
                .p(px(space::S))
                .rounded(px(radius::CONTROL))
                .border_2()
                .border_color(if selected { t.accent } else { transparent_black() })
                .bg(t.raised)
                .cursor_pointer()
                .key_nav()
                .soft(t.ink.opacity(tint::HOVER), radius::CONTROL)
                .tooltip(tip(label.clone()))
                .on_click(cx.listener(move |s, _, _, cx| s.set_cape(pick.clone(), cx)))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(space::XS))
                .child(div().w(px(w)).h(px(h)).flex().items_center().justify_center().child(look))
                .child(div().w_full().text_center().truncate().text_size(px(font::CAPTION)).text_color(if selected { t.primary } else { t.secondary }).child(label))
        };
        let none = choice("cape-none".into(), tr("Kein Umhang"), self.cape.is_none(), icon("circle-off", iconsize::L, t.tertiary).into_any_element(), None);
        let capes = ready.capes.iter().map(|(cape, image)| {
            let look = match image {
                Some(image) => img(ImageSource::Render(Arc::clone(image))).w(px(w)).h(px(h)).object_fit(ObjectFit::Fill).into_any_element(),
                None => icon("image-off", iconsize::L, t.tertiary).into_any_element(),
            };
            let label = if cape.alias.is_empty() { tr("Umhang") } else { cape.alias.clone() };
            choice(SharedString::from(format!("cape-{}", cape.id)), label, self.cape.as_deref() == Some(cape.id.as_str()), look, Some(cape.id.clone()))
        });
        section.child(div().flex().flex_wrap().gap(px(space::S)).child(none).children(capes))
    }

    fn render_body(&mut self, cx: &mut Context<Self>, t: &Theme) -> Div {
        match &self.load {
            Load::Loading => div().h(px(wardrobe::BODY)).flex().items_center().justify_center().child(sheet::waiting(tr("Skin wird geladen …"), t)),
            Load::Failed(why, _) => div().h(px(wardrobe::BODY)).flex().items_center().justify_center().child(div().max_w(px(wardrobe::BODY + wardrobe::COLUMN)).child(sheet::problem(
                "skin-load",
                "circle-alert",
                t.danger_text,
                why,
                self.details,
                cx.listener(|s, _, _, cx| {
                    s.details = !s.details;
                    cx.notify();
                }),
                t,
            ))),
            Load::Ready(ready) => {
                let capes = self.render_capes(ready, cx, t);
                let column = div().flex_1().min_w_0().flex().flex_col().gap(px(space::L)).child(self.render_skin(cx, t)).children(self.render_recent(cx, t)).child(capes);
                div().flex().items_start().gap(px(space::XL)).child(self.render_preview(t)).child(column)
            }
        }
    }
}

impl Render for SkinSheet {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if window.focused(cx).is_none() {
            window.focus(&self.root, cx);
        }
        let t = Theme::get(cx);
        let body = self.render_body(cx, &t);
        let error = self.error.clone().map(|e| {
            div().px(px(space::XL)).pb(px(space::M)).child(sheet::problem(
                "skin-apply",
                "circle-alert",
                t.danger_text,
                &e,
                self.details,
                cx.listener(|s, _, _, cx| {
                    s.details = !s.details;
                    cx.notify();
                }),
                &t,
            ))
        });
        let cancel = sheet::button("skin-cancel", tr("Abbrechen"), None, Look::Secondary, false, false, &t).on_click(cx.listener(|_, _, _, cx| cx.emit(Dismissed)));
        let main = match &self.load {
            Load::Failed(_, true) => sheet::button("skin-sign-in", tr("Erneut anmelden"), Some("log-in"), Look::Primary, false, false, &t).on_click(cx.listener(|s, _, window, cx| s.sign_in_again(window, cx))),
            Load::Failed(_, false) => sheet::button("skin-retry", tr("Erneut versuchen"), None, Look::Primary, false, false, &t).on_click(cx.listener(|s, _, _, cx| s.reload(cx))),
            _ => sheet::button("skin-apply", tr("Übernehmen"), None, Look::Primary, !self.can_apply(), self.applying, &t).on_click(cx.listener(|s, _, _, cx| s.apply(cx))),
        };
        div()
            .id("skin-sheet")
            .track_focus(&self.root)
            .w_full()
            .flex()
            .flex_col()
            .text_color(t.primary)
            .on_action(cx.listener(|s, _: &Confirm, window, cx| {
                if s.root.is_focused(window) && s.can_apply() {
                    s.apply(cx);
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|s, _: &Cancel, _, cx| {
                if s.applying {
                    return;
                }
                cx.propagate();
            }))
            .child(sheet::header(tr("Skin ändern"), Some(self.account.name.clone()), &t))
            .child(div().px(px(space::XL)).pb(px(space::L)).child(body))
            .children(error)
            .child(sheet::footer(Vec::new(), vec![cancel.into_any_element(), main.into_any_element()], &t))
    }
}

#[cfg(test)]
mod tests {
    use super::{AuthError, CANNED_CAPES, SkinError, canned_cape, canned_skin, change, problem, signs_in_again, skin_text};

    #[test]
    fn every_skin_error_reads_as_plain_german_with_a_next_step() {
        let all = [
            SkinError::NotPng,
            SkinError::WrongSize(128, 128),
            SkinError::TooLarge,
            SkinError::RateLimited,
            SkinError::SignInAgain,
            SkinError::SkinRejected,
            SkinError::CapeRejected,
            SkinError::Unavailable(503),
        ];
        let texts: Vec<String> = all.iter().map(skin_text).collect();
        for (i, a) in texts.iter().enumerate() {
            assert!(!a.is_empty() && !a.contains("Minecraft services"));
            assert!(texts.iter().skip(i + 1).all(|b| a != b));
        }
        assert_eq!(texts[3], "Microsoft lässt gerade keine Änderung zu, versuch es in einer Minute nochmal.");
        assert!(texts[1].contains("128"));
    }

    #[test]
    fn a_failure_keeps_its_cause_only_where_it_helps() {
        let rate = problem(&anyhow::Error::from(SkinError::RateLimited).context("upload"));
        assert_eq!(rate.detail, None);
        assert!(rate.text.starts_with("Microsoft"));
        assert!(problem(&SkinError::Unavailable(502).into()).detail.unwrap().contains("502"));
        let again: anyhow::Error = AuthError::SignInAgain.into();
        assert!(signs_in_again(&again));
        assert!(!signs_in_again(&SkinError::SignInAgain.into()));
    }

    #[test]
    fn the_canned_skin_and_capes_are_drawable() {
        assert!(change::check_png(&change::png(&canned_skin()).unwrap()).is_ok());
        for (_, _, c, s) in CANNED_CAPES {
            assert!(change::cape_front(&canned_cape(c, s)).is_some());
        }
    }
}
