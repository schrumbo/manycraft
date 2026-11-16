//! Skin ändern: front and back preview, a skin file with its arm model, the skins an account wore lately and the capes it
//! owns. A driven instance gets a canned profile and never reaches Minecraft services.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use freya::elements::image::ImageHandle;
use freya::engine::prelude::AlphaType;
use freya::prelude::*;
use image::{Rgba, RgbaImage};
use uuid::Uuid;

use crate::core::account::{Account, AccountKind};
use crate::core::auth::{self, AuthError};
use crate::core::i18n::{tr, trf};
use crate::core::skin_change::{self as change, Applied, Cape, Model, Services, SkinError, Wardrobe};
use crate::core::{drive, skin, skins};
use crate::ui::forms::{self, Problem};
use crate::view::drive::use_spot;
use crate::view::icons::{Icon, icon};
use crate::view::kit::button::Button;
use crate::view::kit::group::{note, status};
use crate::view::kit::segmented::{Segment, Segmented};
use crate::view::kit::sheet::{self, Card, Primary};
use crate::view::kit::text::{caption, line, mono, para, secondary, strong};
use crate::view::kit::tooltip::WithTip;
use crate::view::kit::{off_thread, ring, toast, use_theme};
use crate::view::sheets::account::{self, Start};
use crate::view::store::{Store, use_store};
use crate::view::theme::{Theme, font, frame, iconsize, space};

const CANNED_DELAY: Duration = Duration::from_millis(450);
const PREVIEW: f32 = 5.;
const RECENT: f32 = 2.;
const CAPE: f32 = 3.;
const CHOICE: f32 = 88.;
const PICK: f32 = 2.;
const COLUMN: f32 = 200.;
const BODY: f32 = 260.;
const DIM: f32 = 0.45;
const DETAIL_LINES: usize = 8;
const CAPES_PER_ROW: usize = 3;
const RECENT_PER_ROW: usize = 6;
const FACE_SCALE: u32 = 4;

/// Opens the skin sheet for a Microsoft account; does nothing for an unknown or offline one.
pub fn open(store: Store, id: Uuid) {
    let account = store.model.peek().account(id).cloned();
    if let Some(account) = account.filter(|a| a.kind == AccountKind::Microsoft) {
        sheet::open(store.ui, SkinSheet { account });
    }
}

#[derive(Clone)]
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

fn put_on(account: &Account, services: Option<Arc<Services>>, new: Option<Worn>, old: Option<(RgbaImage, Model)>, lost: Option<(String, Model)>, cape: Option<Option<String>>) -> Outcome {
    let old = old.or_else(|| {
        let (url, model) = lost?;
        Some((services.as_ref()?.texture(&url).ok().and_then(usable)?, model))
    });
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

fn auth_text(e: &AuthError) -> String {
    match e {
        AuthError::SignInAgain => tr("Die gespeicherte Anmeldung gilt nicht mehr. Melde dich erneut an."),
        AuthError::NoXboxProfile => tr("Dieses Konto hat noch kein Xbox-Profil. Melde dich einmal auf xbox.com an, dann klappt es."),
        AuthError::RegionBlocked => tr("Xbox Live ist im Land dieses Kontos nicht verfügbar."),
        AuthError::AdultVerification => tr("Dieses Konto muss erst bei Microsoft das Alter bestätigen."),
        AuthError::ChildAccount => tr("Das ist ein Kinderkonto. Ein Erwachsener muss es erst zu einer Microsoft-Familie hinzufügen."),
        AuthError::XboxRejected(code) => trf("Xbox Live hat die Anmeldung abgelehnt (Code %lld).", &[code]),
        other => other.to_string(),
    }
}

fn problem(e: &anyhow::Error) -> Problem {
    if let Some(s) = e.chain().find_map(|c| c.downcast_ref::<SkinError>()) {
        let detail = matches!(s, SkinError::Unavailable(_)).then(|| format!("{e:#}"));
        return Problem { text: skin_text(s), detail };
    }
    if let Some(a) = e.chain().find_map(|c| c.downcast_ref::<AuthError>()) {
        return Problem::plain(auth_text(a));
    }
    forms::explain(e)
}

fn signs_in_again(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.downcast_ref::<AuthError>() == Some(&AuthError::SignInAgain))
}

fn handle(image: &RgbaImage) -> Option<ImageHandle> {
    ImageHandle::from_rgba(image.width(), image.height(), Bytes::from(image.as_raw().clone()), AlphaType::Unpremul)
}

fn show_face(store: Store, id: Uuid, face: Vec<u8>) {
    let Some(image) = RgbaImage::from_raw(skin::SIDE, skin::SIDE, face) else { return };
    if let Some(shown) = handle(&change::scaled(&image, FACE_SCALE)) {
        let mut faces = store.faces;
        faces.write().insert(id, shown);
    }
}

async fn ask() -> Option<PathBuf> {
    rfd::AsyncFileDialog::new().set_title(tr("Skin wählen")).add_filter(tr("PNG-Bild"), &["png"]).pick_file().await.map(|f| f.path().to_path_buf())
}

struct Picked {
    name: Option<String>,
    worn: Worn,
    recent: Option<usize>,
}

struct Shown {
    worn: Worn,
    thumb: ImageHandle,
}

struct Ready {
    services: Option<Arc<Services>>,
    wardrobe: Wardrobe,
    skin: Option<RgbaImage>,
    capes: Vec<(Cape, Option<ImageHandle>)>,
    cloaks: Vec<(String, RgbaImage)>,
}

enum Load {
    Loading,
    Ready(Box<Ready>),
    Failed(Problem, bool),
}

type Views = Option<(ImageHandle, ImageHandle)>;

#[derive(Clone, Copy)]
struct Ctl {
    store: Store,
    id: Uuid,
    account: State<Rc<Account>>,
    load: State<Load>,
    pick: State<Option<Picked>>,
    pick_problem: State<Option<String>>,
    picking: State<bool>,
    model: State<Model>,
    cape: State<Option<String>>,
    recent: State<Vec<Shown>>,
    views: State<Views>,
    applying: State<bool>,
    error: State<Option<Problem>>,
    details: State<bool>,
}

impl Ctl {
    fn alive(self) -> bool {
        self.load.try_peek().is_some()
    }

    fn ready<R>(self, f: impl FnOnce(&Ready) -> R) -> Option<R> {
        match &*self.load.peek() {
            Load::Ready(r) => Some(f(r)),
            _ => None,
        }
    }

    fn shown_skin(self) -> Option<RgbaImage> {
        if let Some(p) = &*self.pick.peek() {
            return Some(p.worn.image.clone());
        }
        self.ready(|r| r.skin.clone()).flatten()
    }

    fn skin_changes(self) -> bool {
        self.pick.peek().is_some() || self.ready(|r| r.skin.is_some() && *self.model.peek() != r.wardrobe.model).unwrap_or(false)
    }

    fn cape_changes(self) -> bool {
        self.ready(|r| self.cape.peek().as_deref() != r.wardrobe.cape()).unwrap_or(false)
    }

    fn can_apply(self) -> bool {
        !*self.applying.peek() && self.ready(|_| ()).is_some() && (self.skin_changes() || self.cape_changes())
    }

    fn redraw(mut self) {
        let model = *self.model.peek();
        let cloak = self.ready(|r| self.cape.peek().as_deref().and_then(|id| r.cloaks.iter().find(|(c, _)| c == id)).map(|(_, front)| front.clone())).flatten();
        let views = self.shown_skin().and_then(|skin| {
            let (front, mut back) = change::front_and_back(&skin, model);
            if let Some(cloak) = &cloak {
                change::hang_cape(&mut back, cloak);
            }
            Some((handle(&front)?, handle(&back)?))
        });
        self.views.set(views);
    }

    fn set_recent(mut self, recent: Vec<Worn>) {
        let kept = recent
            .into_iter()
            .filter_map(|worn| {
                let (front, _) = change::front_and_back(&worn.image, worn.model);
                Some(Shown { thumb: handle(&front)?, worn })
            })
            .collect();
        self.recent.set(kept);
    }

    fn reload(mut self) {
        self.load.set(Load::Loading);
        self.error.set(None);
        let account = Account::clone(&self.account.peek());
        let Some(rx) = off_thread("skin-load", move || load(&account)) else {
            self.load.set(Load::Failed(Problem::plain(tr("Das Skin-Profil lässt sich gerade nicht laden. Versuch es gleich noch einmal.")), false));
            return;
        };
        spawn_in_window(async move {
            let loaded = rx.recv().await;
            if !self.alive() {
                return;
            }
            match loaded {
                Ok(loaded) => self.loaded(loaded),
                Err(_) => {
                    let mut load = self.load;
                    load.set(Load::Failed(Problem::plain(tr("Das Skin-Profil lässt sich gerade nicht laden. Versuch es gleich noch einmal.")), false));
                }
            }
        });
    }

    fn loaded(mut self, loaded: Result<Closet>) {
        match loaded {
            Ok(closet) => {
                let cloaks: Vec<(String, RgbaImage)> = closet.capes.iter().filter_map(|(id, image)| Some((id.clone(), change::cape_front(image)?))).collect();
                let capes = closet.wardrobe.capes.iter().map(|c| (c.clone(), cloaks.iter().find(|(id, _)| *id == c.id).and_then(|(_, front)| handle(front)))).collect();
                self.model.set(closet.wardrobe.model);
                self.cape.set(closet.wardrobe.cape().map(str::to_owned));
                self.pick.set(None);
                self.set_recent(closet.recent);
                self.load.set(Load::Ready(Box::new(Ready { services: closet.services, wardrobe: closet.wardrobe, skin: closet.skin, capes, cloaks })));
                self.redraw();
            }
            Err(e) => self.load.set(Load::Failed(problem(&e), signs_in_again(&e))),
        }
    }

    fn choose_file(mut self) {
        if *self.applying.peek() || *self.picking.peek() {
            return;
        }
        self.picking.set(true);
        spawn_in_window(async move {
            let path = if drive::on() { drive::answer("skin file").and_then(|p| p.into_iter().next()) } else { ask().await };
            if !self.alive() {
                return;
            }
            let Some(path) = path else {
                self.picking.set(false);
                return;
            };
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
            let read = match off_thread("skin-read", move || change::read_png(&path)) {
                Some(rx) => rx.recv().await.ok(),
                None => None,
            };
            if !self.alive() {
                return;
            }
            self.picking.set(false);
            match read {
                Some(Ok((png, image))) => {
                    let model = change::guess_model(&image);
                    self.model.set(model);
                    self.pick.set(Some(Picked { name, worn: Worn { png, image, model }, recent: None }));
                    self.pick_problem.set(None);
                    self.error.set(None);
                    self.redraw();
                }
                Some(Err(e)) => self.pick_problem.set(Some(problem(&e).text)),
                None => self.pick_problem.set(Some(tr("Die Datei lässt sich gerade nicht lesen. Versuch es noch einmal."))),
            }
        });
    }

    fn use_recent(mut self, at: usize) {
        if *self.applying.peek() {
            return;
        }
        let Some((worn, model)) = self.recent.peek().get(at).map(|s| (s.worn.clone(), s.worn.model)) else { return };
        self.model.set(model);
        self.pick.set(Some(Picked { name: None, worn, recent: Some(at) }));
        self.pick_problem.set(None);
        self.error.set(None);
        self.redraw();
    }

    fn set_model(mut self, model: Model) {
        if *self.applying.peek() || *self.model.peek() == model {
            return;
        }
        self.model.set(model);
        self.error.set(None);
        self.redraw();
    }

    fn set_cape(mut self, cape: Option<String>) {
        if *self.applying.peek() {
            return;
        }
        self.cape.set(cape);
        self.error.set(None);
        self.redraw();
    }

    fn clear_pick(mut self) {
        if *self.applying.peek() {
            return;
        }
        let Some(model) = self.ready(|r| r.wardrobe.model) else { return };
        self.pick.set(None);
        self.pick_problem.set(None);
        self.model.set(model);
        self.error.set(None);
        self.redraw();
    }

    fn apply(mut self) {
        if !self.can_apply() {
            return;
        }
        let Some((services, old, lost)) = self.ready(|r| (r.services.clone(), r.skin.clone().map(|s| (s, r.wardrobe.model)), r.wardrobe.skin_url.clone().map(|url| (url, r.wardrobe.model)))) else { return };
        let skin_changes = self.skin_changes();
        let cape_changes = self.cape_changes();
        let model = *self.model.peek();
        let pick = self.pick.peek().as_ref().map(|p| Worn { model, ..p.worn.clone() });
        let lost = if pick.is_some() && old.is_none() { lost } else { None };
        let cape = cape_changes.then(|| self.cape.peek().clone());
        let account = Account::clone(&self.account.peek());
        self.applying.set(true);
        self.error.set(None);
        let work = move || {
            let new = if skin_changes {
                match &pick {
                    Some(w) => Some(w.clone()),
                    None => old.as_ref().and_then(|(image, _)| Some(Worn { png: change::png(image).ok()?, image: image.clone(), model })),
                }
            } else {
                None
            };
            let old = if pick.is_some() { old } else { None };
            put_on(&account, services, new, old, lost, cape)
        };
        let Some(rx) = off_thread("skin-apply", work) else {
            self.failed_to_apply();
            return;
        };
        spawn_in_window(async move {
            match rx.recv().await {
                Ok(outcome) => self.applied(outcome, skin_changes, cape_changes),
                Err(_) if self.alive() => self.failed_to_apply(),
                Err(_) => {}
            }
        });
    }

    fn failed_to_apply(mut self) {
        self.applying.set(false);
        self.error.set(Some(Problem::plain(tr("Die Änderung lässt sich gerade nicht senden. Versuch es noch einmal."))));
    }

    fn applied(mut self, outcome: Outcome, skin_asked: bool, cape_asked: bool) {
        if let Some(face) = outcome.face {
            show_face(self.store, self.id, face);
        }
        if !self.alive() {
            let text = match &outcome.error {
                Some(e) => problem(e).text,
                None => done_text(skin_asked, cape_asked),
            };
            toast::show(self.store.ui, text, None);
            return;
        }
        self.applying.set(false);
        let Some(error) = outcome.error else {
            toast::show(self.store.ui, done_text(skin_asked, cape_asked), None);
            sheet::close(self.store.ui);
            return;
        };
        if let Some(recent) = outcome.recent {
            self.set_recent(recent);
        }
        let model = *self.model.peek();
        let skin_taken = outcome.skin.is_some();
        let mut load = self.load;
        if let Load::Ready(ready) = &mut *load.write() {
            if let Some(skin) = outcome.skin {
                ready.skin = Some(skin);
                ready.wardrobe.model = model;
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
        if skin_taken {
            self.pick.set(None);
        }
        self.error.set(Some(problem(&error)));
        self.redraw();
    }
}

fn done_text(skin: bool, cape: bool) -> String {
    match (skin, cape) {
        (true, true) => tr("Skin und Umhang geändert."),
        (true, false) => tr("Skin geändert."),
        _ => tr("Umhang geändert."),
    }
}

fn pixels(handle: ImageHandle, w: f32, h: f32) -> Element {
    image(handle).sampling_mode(SamplingMode::Nearest).aspect_ratio(AspectRatio::None).width(Size::px(w)).height(Size::px(h)).into_element()
}

fn keyed(key: String, choice: Choice) -> Element {
    rect().key(key).child(choice).into_element()
}

fn heading(t: &Theme, text: String) -> Label {
    strong(t, text, font::CALLOUT, t.text2)
}

fn notice(t: &Theme, color: Color, text: String) -> Element {
    rect()
        .width(Size::fill())
        .horizontal()
        .spacing(space::XS)
        .content(Content::flex())
        .child(rect().padding((space::XXS, 0., 0., 0.)).child(icon(t, Icon::CircleAlert, iconsize::S, color)))
        .child(para(t, text, font::CALLOUT, color).width(Size::flex(1.)))
        .into_element()
}

fn problem_view(t: &Theme, id: &str, problem: &Problem, mut open: State<bool>) -> Element {
    let mut column = rect().width(Size::fill()).child(notice(t, t.danger_text, problem.text.clone()));
    if let Some(detail) = &problem.detail {
        let shown = *open.read();
        let toggle = Button::new(format!("{id}.details"), if shown { tr("Details ausblenden") } else { tr("Details") }).plain().glyph(if shown { Icon::ChevronDown } else { Icon::ChevronRight }).on_press(move |_| open.toggle());
        column = column.child(rect().horizontal().child(toggle));
        if shown {
            column = column.child(rect().width(Size::fill()).padding((space::XS, space::S)).corner_radius(t.shape.small).background(t.well).child(mono(t, detail.clone(), font::CAPTION, t.text2).width(Size::fill()).max_lines(DETAIL_LINES)));
        }
    }
    column.into_element()
}

/// A skin or cape to pick: a picture with an optional caption, ringed in the accent while chosen.
#[derive(Clone, PartialEq)]
struct Choice {
    id: String,
    label: String,
    caption: Option<String>,
    picture: Element,
    selected: bool,
    enabled: bool,
    width: Option<f32>,
    pad: f32,
    on_press: EventHandler<()>,
}

impl Component for Choice {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let a11y = use_a11y();
        let focus = use_focus(a11y);
        let mut hovered = use_state(|| false);
        let spot = use_spot(&self.id, "option", &self.label, self.enabled);
        let press = self.on_press.clone();
        let enabled = self.enabled;
        let picked = Border::new().fill(if self.selected { t.accent } else { Color::TRANSPARENT }).width(PICK).alignment(BorderAlignment::Inner);
        let mut inner = rect().padding(self.pad).corner_radius(t.shape.control).border(picked).cross_align(Alignment::Center).spacing(space::XS).background(if enabled && *hovered.read() { t.hover } else { Color::TRANSPARENT }).child(self.picture.clone());
        if let Some(c) = &self.caption {
            inner = inner.child(line(&t, c.clone(), font::CAPTION, if self.selected { t.text1 } else { t.text2 }).width(Size::fill()).text_align(TextAlign::Center));
        }
        let mut outer = rect()
            .a11y_id(a11y)
            .a11y_focusable(enabled)
            .a11y_role(AccessibilityRole::RadioButton)
            .a11y_alt(self.label.clone())
            .background(t.raised)
            .corner_radius(t.shape.control)
            .opacity(if enabled { 1. } else { DIM })
            .cursor(if enabled { CursorIcon::Pointer } else { CursorIcon::NotAllowed })
            .on_sized(spot)
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| hovered.set_if_modified(false))
            .child(inner);
        if let Some(w) = self.width {
            outer = outer.width(Size::px(w));
        }
        if enabled {
            outer = outer.on_all_press(move |_| {
                a11y.request_focus();
                press.call(());
            });
        }
        if let Some(border) = ring(&t, focus()) {
            outer = outer.border(border);
        }
        WithTip::new(Some(self.label.clone()), outer)
    }
}

fn rows(items: Vec<Element>, per_row: usize, gap: f32) -> Element {
    let mut column = rect().spacing(gap);
    for chunk in items.chunks(per_row) {
        column = column.child(rect().horizontal().spacing(gap).children(chunk.iter().cloned()));
    }
    column.into_element()
}

#[derive(PartialEq)]
struct SkinSheet {
    account: Account,
}

impl Component for SkinSheet {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let ctl = Ctl {
            store,
            id: self.account.id,
            account: use_state(|| Rc::new(self.account.clone())),
            load: use_state(|| Load::Loading),
            pick: use_state(|| None),
            pick_problem: use_state(|| None),
            picking: use_state(|| false),
            model: use_state(Model::default),
            cape: use_state(|| None),
            recent: use_state(Vec::new),
            views: use_state(|| None),
            applying: use_state(|| false),
            error: use_state(|| None),
            details: use_state(|| false),
        };
        use_hook(move || ctl.reload());
        let applying = *ctl.applying.read();
        let load = ctl.load.read();
        let can_apply = {
            let _ = (ctl.pick.read().is_some(), *ctl.model.read(), ctl.cape.read().is_some());
            ctl.can_apply()
        };
        let body = match &*load {
            Load::Loading => rect().width(Size::fill()).height(Size::px(BODY)).center().child(secondary(&t, tr("Skin wird geladen …"))).into_element(),
            Load::Failed(why, _) => rect().width(Size::fill()).height(Size::px(BODY)).center().child(rect().width(Size::fill()).child(problem_view(&t, "skin.load", why, ctl.details))).into_element(),
            Load::Ready(ready) => {
                let side = |handle: Option<ImageHandle>, text: String| {
                    let (w, h) = (change::VIEW_W as f32 * PREVIEW, change::VIEW_H as f32 * PREVIEW);
                    let shown = match handle {
                        Some(handle) => pixels(handle, w, h),
                        None => rect().width(Size::px(w)).height(Size::px(h)).center().child(icon(&t, Icon::User, iconsize::HERO, t.text3)).into_element(),
                    };
                    rect().cross_align(Alignment::Center).spacing(space::S).child(shown).child(caption(&t, text))
                };
                let (front, back) = match &*ctl.views.read() {
                    Some((f, b)) => (Some(f.clone()), Some(b.clone())),
                    None => (None, None),
                };
                let preview = rect().width(Size::px(COLUMN)).padding((space::L, 0.)).horizontal().main_align(Alignment::Center).spacing(space::L).background(t.well).corner_radius(t.shape.tile).child(side(front, tr("Vorne"))).child(side(back, tr("Hinten")));

                let picked = ctl.pick.read();
                let chosen = match &*picked {
                    Some(Picked { name: Some(name), .. }) => Some(name.clone()),
                    Some(Picked { recent: Some(_), .. }) => Some(tr("Zuletzt verwendet")),
                    _ => None,
                };
                let reason = if applying { Some(tr("Wird übernommen …")) } else if *ctl.picking.read() { Some(tr("Der Dateidialog ist offen.")) } else { None };
                let file = Button::new("skin.file", tr("Datei wählen…")).glyph(Icon::Image).disabled(reason).on_press(move |_| ctl.choose_file());
                let mut file_row = rect().width(Size::fill()).horizontal().cross_align(Alignment::Center).spacing(space::S).content(Content::flex()).child(file);
                if let Some(c) = chosen {
                    file_row = file_row.child(rect().width(Size::flex(1.)).child(line(&t, c, font::CALLOUT, t.text2)));
                }
                if picked.is_some() && !applying {
                    file_row = file_row.child(Button::icon("skin.clear", Icon::Close, tr("Bisherigen Skin behalten")).on_press(move |_| ctl.clear_pick()));
                }
                let hint = match &*ctl.pick_problem.read() {
                    Some(text) => status(&t, text.clone(), t.danger_text),
                    None => note(&t, tr("PNG mit 64 × 64 oder 64 × 32 Pixeln")),
                };
                let model_on = !applying && ctl.shown_skin().is_some();
                let current = *ctl.model.read();
                let segments = vec![Segment { id: "skin.model.classic".into(), label: tr("Klassisch"), tip: None }, Segment { id: "skin.model.slim".into(), label: tr("Schlank"), tip: None }];
                let model = rect().horizontal().opacity(if model_on { 1. } else { DIM }).child(Segmented::new(tr("Modell"), segments, usize::from(current == Model::Slim)).on_choose(move |n| {
                    if model_on {
                        ctl.set_model(if n == 1 { Model::Slim } else { Model::Classic });
                    }
                }));
                let skin_section = rect().width(Size::fill()).spacing(space::XS).child(heading(&t, tr("Skin"))).child(file_row).child(hint).child(rect().padding((space::XS, 0., 0., 0.)).child(heading(&t, tr("Modell")))).child(model);

                let mut column = rect().width(Size::flex(1.)).spacing(space::L).child(skin_section);
                let chosen_recent = picked.as_ref().and_then(|p| p.recent);
                let shown = ctl.recent.read();
                if !shown.is_empty() {
                    let (w, h) = (change::VIEW_W as f32 * RECENT, change::VIEW_H as f32 * RECENT);
                    let items = shown
                        .iter()
                        .enumerate()
                        .map(|(n, s)| {
                            let label = trf("Zuletzt verwendet %lld", &[&(n + 1)]);
                            keyed(format!("recent-{n}"), Choice { id: format!("skin.recent.{n}"), label, caption: None, picture: pixels(s.thumb.clone(), w, h), selected: chosen_recent == Some(n), enabled: !applying, width: None, pad: space::XS, on_press: EventHandler::new(move |_| ctl.use_recent(n)) })
                        })
                        .collect();
                    column = column.child(rect().width(Size::fill()).spacing(space::XS).child(heading(&t, tr("Zuletzt verwendet"))).child(rows(items, RECENT_PER_ROW, space::XS)));
                }
                let worn = ctl.cape.read().clone();
                let mut capes = rect().width(Size::fill()).spacing(space::XS).child(heading(&t, tr("Umhang")));
                if ready.capes.is_empty() {
                    capes = capes.child(para(&t, tr("Dieses Konto hat keine Umhänge."), font::CALLOUT, t.text2));
                } else {
                    let (w, h) = (change::CAPE_W as f32 * CAPE, change::CAPE_H as f32 * CAPE);
                    let slot = |content: Element| rect().width(Size::px(w)).height(Size::px(h)).center().child(content).into_element();
                    let none_label = tr("Kein Umhang");
                    let mut items = vec![
                        keyed("cape-none".into(), Choice { id: "skin.cape.none".into(), label: none_label.clone(), caption: Some(none_label), picture: slot(icon(&t, Icon::CircleOff, iconsize::L, t.text3)), selected: worn.is_none(), enabled: !applying, width: Some(CHOICE), pad: space::S, on_press: EventHandler::new(move |_| ctl.set_cape(None)) }),
                    ];
                    for (n, (cape, picture)) in ready.capes.iter().enumerate() {
                        let label = if cape.alias.is_empty() { tr("Umhang") } else { cape.alias.clone() };
                        let look = match picture {
                            Some(picture) => pixels(picture.clone(), w, h),
                            None => slot(icon(&t, Icon::Image, iconsize::L, t.text3)),
                        };
                        let id = cape.id.clone();
                        items.push(keyed(format!("cape-{}", cape.id), Choice { id: format!("skin.cape.{n}"), label: label.clone(), caption: Some(label), picture: look, selected: worn.as_deref() == Some(cape.id.as_str()), enabled: !applying, width: Some(CHOICE), pad: space::S, on_press: EventHandler::new(move |_| ctl.set_cape(Some(id.clone()))) }));
                    }
                    capes = capes.child(rows(items, CAPES_PER_ROW, space::S));
                }
                column = column.child(capes);
                rect().width(Size::fill()).horizontal().spacing(space::XL).content(Content::flex()).child(preview).child(column).into_element()
            }
        };
        let mut content = rect().width(Size::fill()).spacing(space::M).child(body);
        if let Some(why) = &*ctl.error.read() {
            content = content.child(problem_view(&t, "skin.apply", why, ctl.details));
        }
        let (label, reason, id, run): (String, Option<String>, &str, Box<dyn Fn()>) = match &*load {
            Load::Failed(_, true) => (tr("Erneut anmelden"), None, "skin.sign-in", Box::new(move || account::open(store, Start::Again(vec![ctl.id])))),
            Load::Failed(_, false) => (tr("Erneut versuchen"), None, "skin.retry", Box::new(move || ctl.reload())),
            _ if applying => (tr("Wird übernommen …"), Some(tr("Wird übernommen …")), "skin.apply", Box::new(|| {})),
            Load::Loading => (tr("Übernehmen"), Some(tr("Das Profil wird noch geladen.")), "skin.apply", Box::new(|| {})),
            Load::Ready(_) => (tr("Übernehmen"), (!can_apply).then(|| tr("Ändere zuerst Skin, Modell oder Umhang.")), "skin.apply", Box::new(move || ctl.apply())),
        };
        let primary = Primary { label, disabled: reason, danger: false, busy: applying, run: EventHandler::new(move |_| run()) };
        Card::new("skin", tr("Skin ändern")).message(self.account.name.clone()).body(content).primary(primary).primary_id(id).width(frame::SHEET)
    }
}

#[cfg(test)]
mod tests {
    use super::{AuthError, CANNED_CAPES, SkinError, canned_cape, canned_skin, change, problem, signs_in_again, skin_text, usable};

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

    #[test]
    fn only_skin_sized_images_are_usable() {
        assert!(usable(image::RgbaImage::new(64, 64)).is_some());
        assert!(usable(image::RgbaImage::new(64, 32)).is_some());
        assert!(usable(image::RgbaImage::new(128, 128)).is_none());
    }
}
