//! The theme screens: the grid with twelve tiles, an instance page on its Mods tab, the new instance sheet and the
//! appearance page with the theme picker, drawn in any JSON theme, optionally saved as a PNG and closed.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use freya::elements::image::ImageHandle;
use freya::engine::prelude::{AlphaType, ColorType, IPoint, ISize, ImageInfo};
use freya::prelude::*;
use notify::Watcher;

use super::art::{self, Sky};
use super::i18n::{tr, trf};
use super::icons::Icon;
use super::theme::{self, Theme, shell, space};
use super::themes::{self, Library, Origin, Token};

const BODY: [&str; 3] = ["Segoe UI Variable Text", "Segoe UI", "Inter"];
const DISPLAY: [&str; 3] = ["Segoe UI Variable Display", "Segoe UI", "Inter"];
const MONO: [&str; 3] = ["Cascadia Mono", "Consolas", "Menlo"];
const SHEET_H: f32 = 664.;
const SETTLE: Duration = Duration::from_millis(2500);
const ORIGINALS: [&str; 3] = ["lantern", "biome", "high-contrast"];
const KNOWN: [&str; 8] = ["catppuccin", "nord", "dracula", "gruvbox", "solarized", "tokyo-night", "rose-pine", "github"];
const FUN: [&str; 7] = ["nether", "creeper", "enderman", "redstone", "hacker", "vaporwave", "windows-98"];
const EXAMPLE: &str = r##"{
  "name": "Mein Theme",
  "author": "Ich",
  "appearance": "dark",
  "colors": {
    "surface": "#1A1714",
    "raised": "#211D19",
    "sunken": "#13110F",
    "text-1": "#EFE8DD",
    "text-2": "#BDB2A3",
    "text-3": "#9A8F80",
    "accent": "#5EC9C1",
    "on-accent": "#0D1A19",
    "attention": "#F2B04A",
    "danger": "#EE7059",
    "success": "#93CB7C"
  }
}"##;

static OS_SCALE: Mutex<f64> = Mutex::new(1.0);

thread_local! {
    static PICTURES: RefCell<HashMap<(u32, u8), Option<ImageHandle>>> = RefCell::new(HashMap::new());
    static FACES: RefCell<HashMap<u32, Option<ImageHandle>>> = RefCell::new(HashMap::new());
}

/// The screen to draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Grid,
    Instance,
    Sheet,
    Themes,
    Schema,
}

/// Command line of the theme screens.
#[derive(Clone, Debug)]
pub struct Options {
    pub theme: Option<String>,
    pub dark: Option<bool>,
    pub screen: Screen,
    pub lang: String,
    pub size: (f64, f64),
    pub device_scale: Option<f64>,
    pub shot: Option<PathBuf>,
}

impl Options {
    /// Reads `--theme <id>`, `--appearance light|dark`, `--screen grid|instance|sheet|themes|schema`, `--lang`,
    /// `--size`, `--device-scale` and `--shot`; `None` without `--theme` or `--screen`. Without `--appearance` the
    /// system's light or dark is followed.
    pub fn parse(args: &[String]) -> Option<Self> {
        let mut o = Options { theme: None, dark: None, screen: Screen::Grid, lang: String::new(), size: (1440., 900.), device_scale: None, shot: None };
        let mut known = false;
        let mut it = args.iter();
        while let Some(name) = it.next() {
            let Some(value) = it.next() else { break };
            match name.as_str() {
                "--theme" => {
                    o.theme = (value != "default").then(|| value.clone());
                    known = true;
                }
                "--appearance" => o.dark = Some(value != "light"),
                "--screen" => {
                    o.screen = match value.as_str() {
                        "instance" => Screen::Instance,
                        "sheet" => Screen::Sheet,
                        "themes" => Screen::Themes,
                        "schema" => Screen::Schema,
                        _ => Screen::Grid,
                    };
                    known = true;
                }
                "--lang" => o.lang.clone_from(value),
                "--size" => {
                    if let Some((w, h)) = value.split_once('x') {
                        o.size = (w.trim().parse().unwrap_or(o.size.0), h.trim().parse().unwrap_or(o.size.1));
                    }
                }
                "--device-scale" => o.device_scale = value.parse().ok(),
                "--shot" => o.shot = Some(PathBuf::from(value)),
                _ => {}
            }
        }
        known.then_some(o)
    }
}

/// Opens the window with the chosen screen; with `--shot` it saves the screen once settled and exits.
pub fn run(options: Options) {
    super::i18n::init(&options.lang);
    let library = Library::load(&themes::dir());
    let surface = library.chosen(options.theme.as_deref()).map_or(Color::BLACK, |t| Theme::new(t, options.dark.unwrap_or(true)).surface);
    let (size, device) = (options.size, options.device_scale);
    let window = WindowConfig::new_app(Gallery { options })
        .with_size(size.0, size.1)
        .with_title("Manycraft theme")
        .with_background((surface.r(), surface.g(), surface.b()))
        .with_window_handle(move |window| {
            *OS_SCALE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = window.scale_factor();
            if let Some(device) = device {
                let _ = window.request_inner_size(freya::winit::dpi::PhysicalSize::new((size.0 * device).round() as u32, (size.1 * device).round() as u32));
            }
        });
    launch(LaunchConfig::new().with_window(window));
}

fn shoot(path: PathBuf) {
    let window = Platform::get().current_window();
    let receiver = window.post_render_callback(move |surface| {
        let (w, h) = (surface.width(), surface.height());
        let info = ImageInfo::new(ISize::new(w, h), ColorType::RGBA8888, AlphaType::Unpremul, None);
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        let ok = surface.read_pixels(&info, &mut pixels, (w * 4) as usize, IPoint::new(0, 0));
        (ok, w as u32, h as u32, pixels)
    });
    window.request_redraw();
    spawn(async move {
        if let Ok((true, w, h, pixels)) = receiver.await
            && let Some(img) = image::RgbaImage::from_raw(w, h, pixels)
        {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = img.save_with_format(&path, image::ImageFormat::Png);
        }
        Platform::get().exit();
    });
}

fn watch(dir: PathBuf) -> async_channel::Receiver<()> {
    let (tx, rx) = async_channel::unbounded();
    std::thread::spawn(move || {
        let (events_tx, events) = std::sync::mpsc::channel::<notify::Result<notify::Event>>();
        let Ok(mut watcher) = notify::recommended_watcher(events_tx) else { return };
        let parent = dir.parent().map_or_else(|| dir.clone(), Path::to_path_buf);
        let _ = std::fs::create_dir_all(&parent);
        if watcher.watch(&parent, notify::RecursiveMode::NonRecursive).is_err() {
            return;
        }
        let mut inside = dir.is_dir() && watcher.watch(&dir, notify::RecursiveMode::NonRecursive).is_ok();
        while let Ok(event) = events.recv() {
            if !event.is_ok_and(|e| e.paths.iter().any(|p| p.starts_with(&dir))) {
                continue;
            }
            if !dir.is_dir() {
                inside = false;
            } else if !inside {
                inside = watcher.watch(&dir, notify::RecursiveMode::NonRecursive).is_ok();
            }
            std::thread::sleep(Duration::from_millis(150));
            while events.try_recv().is_ok() {}
            if tx.send_blocking(()).is_err() {
                break;
            }
        }
    });
    rx
}

fn open_folder() -> std::io::Result<()> {
    let dir = themes::dir();
    std::fs::create_dir_all(&dir)?;
    let opener = if cfg!(windows) { "explorer" } else if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    std::process::Command::new(opener).arg(&dir).spawn().map(drop)
}

/// What the appearance page changes.
#[derive(Clone, Copy, PartialEq)]
struct Actions {
    library: State<Library>,
    chosen: State<Option<String>>,
    sheet: State<bool>,
    notice: State<Option<Result<String, String>>>,
}

impl Actions {
    fn open(self) {
        let mut notice = self.notice;
        if open_folder().is_err() {
            notice.set(Some(Err(tr("Der Ordner lässt sich nicht öffnen."))));
        }
    }

    fn copy(self) {
        let Actions { mut library, mut chosen, mut notice, .. } = self;
        let dir = themes::dir();
        let copied = {
            let lib = library.read();
            let id = chosen.read().clone();
            lib.chosen(id.as_deref()).map(|source| (themes::copy_as_template(source, &dir), source.name.clone()))
        };
        match copied {
            Some((Ok(path), name)) => {
                let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                library.set(Library::load(&dir));
                chosen.set(Some(format!("user/{stem}")));
                notice.set(Some(Ok(trf("Kopie von „%@“ angelegt. Öffne den Ordner und ändere die Farben; Manycraft zeigt sie sofort.", &[&name]))));
            }
            Some((Err(_), _)) => notice.set(Some(Err(tr("Kopieren hat nicht geklappt. Ist der Ordner schreibgeschützt?")))),
            None => {}
        }
    }
}

struct Gallery {
    options: Options,
}

impl App for Gallery {
    fn render(&self) -> impl IntoElement {
        let device = self.options.device_scale;
        let shot = self.options.shot.clone();
        let mut library = use_state(|| Library::load(&themes::dir()));
        let chosen = use_state(|| self.options.theme.clone());
        let sheet_open = use_state(|| self.options.screen == Screen::Schema);
        let notice = use_state(|| None::<Result<String, String>>);
        use_hook(move || {
            if let Some(device) = device {
                let os = *OS_SCALE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                Platform::get().current_window().set_custom_scale_factor(device / os);
            }
            if let Some(path) = shot {
                let (tx, rx) = async_channel::bounded::<()>(1);
                std::thread::spawn(move || {
                    std::thread::sleep(SETTLE);
                    let _ = tx.send_blocking(());
                });
                spawn(async move {
                    if rx.recv().await.is_ok() {
                        shoot(path);
                    }
                });
            } else {
                let changes = watch(themes::dir());
                spawn(async move {
                    while changes.recv().await.is_ok() {
                        library.set(Library::load(&themes::dir()));
                    }
                });
            }
        });
        let system_dark = *Platform::get().current_window().preferred_theme.read() == PreferredTheme::Dark;
        let dark = self.options.dark.unwrap_or(system_dark);
        let lib = library.read();
        let id = chosen.read().clone();
        let Some(source) = lib.chosen(id.as_deref()) else {
            return rect().into_element();
        };
        theme::set_font(source.font.as_deref());
        let t = &Theme::new(source, dark);
        let width = self.options.size.0 as f32;
        let height = self.options.size.1 as f32;
        let actions = Actions { library, chosen, sheet: sheet_open, notice };
        match self.options.screen {
            Screen::Grid => grid_screen(t, width, height, None),
            Screen::Instance => instance_screen(t, width),
            Screen::Sheet => grid_screen(t, width, height, Some(sheet(t))),
            Screen::Themes | Screen::Schema => {
                let page = Page { lib: &lib, source, dark, actions, notice: notice.read().clone() };
                let overlay = (*sheet_open.read()).then(|| schema_sheet(t, actions));
                themes_screen(t, width, height, &page, overlay)
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Playing,
    Connecting,
    Reconnecting,
    Attention,
    Banned,
    Crashed,
    Hidden,
}

struct Account {
    name: &'static str,
    state: Phase,
    sky: Sky,
    batch: usize,
}

const ACCOUNTS: [Account; 12] = [
    Account { name: "Kiesel", state: Phase::Playing, sky: Sky::Day, batch: 0 },
    Account { name: "Birke", state: Phase::Playing, sky: Sky::Day, batch: 0 },
    Account { name: "Lehm", state: Phase::Attention, sky: Sky::Dusk, batch: 0 },
    Account { name: "Tuff", state: Phase::Playing, sky: Sky::Dusk, batch: 0 },
    Account { name: "Moos", state: Phase::Connecting, sky: Sky::Day, batch: 0 },
    Account { name: "Basalt", state: Phase::Hidden, sky: Sky::Night, batch: 0 },
    Account { name: "Kalzit", state: Phase::Playing, sky: Sky::Day, batch: 1 },
    Account { name: "Quarz", state: Phase::Crashed, sky: Sky::Day, batch: 1 },
    Account { name: "Fichte", state: Phase::Playing, sky: Sky::Night, batch: 1 },
    Account { name: "Akazie", state: Phase::Hidden, sky: Sky::Day, batch: 2 },
    Account { name: "Kelp", state: Phase::Reconnecting, sky: Sky::Dusk, batch: 2 },
    Account { name: "Glut", state: Phase::Banned, sky: Sky::Night, batch: 2 },
];

fn batch_name(index: usize) -> String {
    match index {
        0 => tr("Farmen"),
        1 => tr("Handel"),
        _ => tr("Bauen"),
    }
}

fn status(state: Phase) -> String {
    match state {
        Phase::Playing => tr("Spielt"),
        Phase::Connecting => tr("Verbindet…"),
        Phase::Reconnecting => trf("Verbindet neu · %lld/%lld", &[&2, &5]),
        Phase::Attention => trf("Getrennt · %@", &[&tr("Anmeldung abgelaufen")]),
        Phase::Banned => tr("Gesperrt"),
        Phase::Crashed => trf("Abgestürzt · Code %lld", &[&1]),
        Phase::Hidden => tr("Im Hintergrund"),
    }
}

fn needs_you(state: Phase) -> bool {
    matches!(state, Phase::Attention | Phase::Banned)
}

fn faces(mut l: Label, chain: [&'static str; 3]) -> Label {
    if let Some(font) = theme::font() {
        l = l.font_family(font);
    }
    for family in chain {
        l = l.font_family(family);
    }
    l
}

fn text(s: impl Into<String>, size: f32, color: Color) -> Label {
    faces(label().text(s.into()).font_size(size).color(color).max_lines(1).text_overflow(TextOverflow::Ellipsis), BODY)
}

fn para(s: impl Into<String>, size: f32, color: Color) -> Label {
    faces(label().text(s.into()).font_size(size).color(color).width(Size::fill()).line_height(1.35), BODY)
}

fn mono(s: impl Into<String>, size: f32, color: Color) -> Label {
    let mut l = label().text(s.into()).font_size(size).color(color);
    for family in MONO {
        l = l.font_family(family);
    }
    l
}

fn strong(s: impl Into<String>, size: f32, color: Color) -> Label {
    text(s, size, color).font_weight(FontWeight::SEMI_BOLD)
}

fn title(t: &Theme, s: impl Into<String>, color: Color) -> Label {
    faces(label().text(s.into()).font_size(t.kind.title).color(color).font_weight(FontWeight::SEMI_BOLD).max_lines(1), DISPLAY)
}

fn figure(s: impl Into<String>, size: f32, color: Color) -> Label {
    text(s, size, color).font_tabular()
}

fn icon(t: &Theme, which: Icon, size: f32, color: Color) -> Element {
    SvgViewer::new(which.svg(t.icon_stroke, size)).width(Size::px(size)).height(Size::px(size)).color(color).show_loader(false).into_element()
}

fn spacer() -> Element {
    rect().width(Size::flex(1.)).into_element()
}

fn pressable(inner: Element, on_press: impl FnMut(Event<PressEventData>) + 'static) -> Element {
    rect().cursor(CursorIcon::Pointer).on_press(on_press).child(inner).into_element()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Look {
    Primary,
    Secondary,
    Plain,
    Material,
}

fn button(t: &Theme, label: String, glyph: Option<Icon>, look: Look, tall: bool) -> Element {
    let (fill, ink) = match look {
        Look::Primary => (t.accent, t.on_accent),
        Look::Secondary => (t.raised, t.text1),
        Look::Plain => (Color::TRANSPARENT, t.accent_text),
        Look::Material => (t.content, t.text1),
    };
    let h = if tall { shell::CONTROL_L } else { shell::CONTROL };
    let pad = if look == Look::Plain { space::S } else { space::M };
    let mut b = rect()
        .height(Size::px(h))
        .padding((0., pad))
        .horizontal()
        .cross_align(Alignment::Center)
        .spacing(space::XS + space::XXS)
        .background(fill)
        .corner_radius(t.shape.control);
    if look == Look::Material {
        b = b.shadow(Shadow::from((0., 2., 8., 0., Color::BLACK.with_af32(0.25))));
    }
    if let Some(g) = glyph {
        b = b.child(icon(t, g, shell::ICON_S + 2., ink));
    }
    let weight = if look == Look::Primary { FontWeight::SEMI_BOLD } else { FontWeight::MEDIUM };
    b.child(text(label, t.kind.body, ink).font_weight(weight)).into_element()
}

fn icon_button(t: &Theme, glyph: Icon) -> Element {
    rect().width(Size::px(shell::CONTROL_L)).height(Size::px(shell::CONTROL_L)).center().background(t.raised).corner_radius(t.shape.control).child(icon(t, glyph, shell::ICON, t.text1)).into_element()
}

fn field(t: &Theme, value: Option<String>, placeholder: String, glyph: Option<Icon>, focused: bool, width: Size) -> Element {
    let mut f = rect()
        .width(width.clone())
        .height(Size::px(shell::CONTROL_L))
        .padding((0., space::M - 2.))
        .horizontal()
        .cross_align(Alignment::Center)
        .spacing(space::S)
        .background(if t.dark { t.well } else { t.content })
        .corner_radius(t.shape.field)
        .border(Border::new().fill(t.separator).width(1.).alignment(BorderAlignment::Inner));
    if let Some(g) = glyph {
        f = f.child(icon(t, g, shell::ICON_S + 2., t.text3));
    }
    f = match value {
        Some(v) => f.child(text(v, t.kind.body, t.text1)).child(rect().width(Size::px(1.5)).height(Size::px(t.kind.body + 3.)).background(t.focus)),
        None => f.child(text(placeholder, t.kind.body, t.text3)),
    };
    if !focused {
        return f.into_element();
    }
    rect()
        .width(width)
        .padding(shell::RING + 1.)
        .height(Size::px(shell::CONTROL_L + (shell::RING + 1.) * 2.))
        .margin(-(shell::RING + 1.))
        .corner_radius(t.shape.field + shell::RING + 1.)
        .border(Border::new().fill(t.focus.with_af32(0.85)).width(shell::RING).alignment(BorderAlignment::Inner))
        .child(f)
        .into_element()
}

fn popup(t: &Theme, value: String, width: f32) -> Element {
    rect()
        .width(Size::px(width))
        .height(Size::px(shell::CONTROL_L))
        .padding((0., space::M - 2.))
        .horizontal()
        .cross_align(Alignment::Center)
        .background(t.raised)
        .corner_radius(t.shape.field)
        .child(figure(value, t.kind.body, t.text1))
        .child(spacer())
        .child(icon(t, Icon::ChevronDown, shell::ICON_S + 2., t.text2))
        .content(Content::flex())
        .into_element()
}

fn segmented(t: &Theme, items: &[String], selected: usize) -> Element {
    let inset = space::XXS + 1.;
    let segments = items.iter().enumerate().map(|(i, item)| {
        let on = i == selected;
        let mut s = rect().height(Size::px(shell::CONTROL - 2.)).padding((0., space::M + 2.)).center().corner_radius((t.shape.control - inset).max(0.));
        s = s.child(text(item.clone(), t.kind.callout + 0.5, if on { t.text1 } else { t.text2 }).font_weight(if on { FontWeight::SEMI_BOLD } else { FontWeight::NORMAL }));
        if on {
            s = s.background(if t.dark { t.separator } else { t.content }).shadow(Shadow::from((0., 1., 2., 0., Color::BLACK.with_af32(if t.dark { 0.35 } else { 0.12 }))));
        }
        s.into_element()
    });
    rect().horizontal().padding(inset).spacing(space::XXS).background(t.raised).corner_radius(t.shape.control).children(segments).into_element()
}

fn switch(t: &Theme, on: bool) -> Element {
    let knob = shell::SWITCH_H - 4.;
    rect()
        .width(Size::px(shell::SWITCH_W))
        .height(Size::px(shell::SWITCH_H))
        .padding(2.)
        .horizontal()
        .main_align(if on { Alignment::End } else { Alignment::Start })
        .background(if on { t.accent } else { t.separator })
        .corner_radius(999.)
        .child(rect().width(Size::px(knob)).height(Size::px(knob)).background(Color::WHITE).corner_radius(999.).shadow(Shadow::from((0., 1., 2., 0., Color::BLACK.with_af32(0.25)))))
        .into_element()
}

fn badge(t: &Theme, value: String, fill: Color, ink: Color) -> Element {
    rect()
        .min_width(Size::px(shell::BADGE))
        .height(Size::px(shell::BADGE))
        .padding((0., space::XS + 1.))
        .center()
        .background(fill)
        .corner_radius(shell::BADGE / 2.)
        .child(figure(value, t.kind.caption, ink).font_weight(FontWeight::SEMI_BOLD))
        .into_element()
}

fn picture(seed: u32, sky: Sky) -> Option<ImageHandle> {
    PICTURES.with(|cache| cache.borrow_mut().entry((seed, sky as u8)).or_insert_with(|| ImageHandle::from_rgba(art::WIDTH, art::HEIGHT, Bytes::from(art::scene(seed, sky)), AlphaType::Unpremul)).clone())
}

fn face_image(t: &Theme, seed: u32, size: f32) -> Element {
    let skins = [[0xE0, 0xAC, 0x8A], [0xC6, 0x8B, 0x63], [0x8D, 0x5A, 0x3B], [0xF1, 0xC7, 0xA5]];
    let skin = skins[seed as usize % skins.len()];
    let handle = FACES.with(|cache| cache.borrow_mut().entry(seed).or_insert_with(|| ImageHandle::from_rgba(8, 8, Bytes::from(art::face(seed, skin)), AlphaType::Unpremul)).clone());
    match handle {
        Some(handle) => image(handle).width(Size::px(size)).height(Size::px(size)).sampling_mode(SamplingMode::Nearest).corner_radius((size * 0.22).min(t.shape.chip)).into_element(),
        None => rect().width(Size::px(size)).height(Size::px(size)).into_element(),
    }
}

fn chip(t: &Theme, seed: u32, color: Color, size: f32) -> Element {
    let rim = 2.;
    rect().padding(rim).background(color).corner_radius(if t.shape.chip > 100. { 999. } else { t.shape.chip + rim }).child(face_image(t, seed, size - rim * 2.)).into_element()
}

fn sidebar(t: &Theme, height: f32, selected: &str) -> Element {
    let row = |glyph: Icon, label: String, count: Option<String>, on: bool, attention: bool| -> Element {
        let mut r = rect()
            .width(Size::fill())
            .height(Size::px(shell::ROW))
            .padding((0., space::S + 2.))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::S + 2.)
            .background(if on { t.selection } else { Color::TRANSPARENT })
            .corner_radius(t.shape.control - 1.)
            .content(Content::flex())
            .child(icon(t, glyph, shell::ICON, if on { t.accent_text } else { t.text2 }))
            .child(text(label, t.kind.body, t.text1).font_weight(if on { FontWeight::SEMI_BOLD } else { FontWeight::NORMAL }).width(Size::flex(1.)));
        if let Some(count) = count {
            r = if attention { r.child(badge(t, count, t.attention, t.on_attention)) } else { r.child(figure(count, t.kind.callout, t.text3)) };
        }
        r.into_element()
    };
    let section = |label: String| rect().padding((space::L, space::S + 2., space::XS + 2., space::S + 2.)).child(strong(label, t.kind.caption, t.text3)).into_element();
    let attention = ACCOUNTS.iter().filter(|a| needs_you(a.state)).count();
    let mut col = rect()
        .width(Size::px(shell::SIDEBAR))
        .height(Size::px(height))
        .padding((space::L + space::S, space::S + 2.))
        .spacing(space::XXS)
        .background(t.sidebar)
        .child(row(Icon::Grid, tr("Alle"), Some(ACCOUNTS.len().to_string()), selected == "all", false))
        .child(row(Icon::Alert, tr("Brauchen dich"), Some(attention.to_string()), false, true))
        .child(row(Icon::Users, tr("Accounts"), None, false, false))
        .child(section(tr("Batches")));
    for b in 0..3 {
        let n = ACCOUNTS.iter().filter(|a| a.batch == b).count();
        col = col.child(row(Icon::Layers, batch_name(b), Some(n.to_string()), false, false));
    }
    col = col.child(section(tr("Instanzen")));
    col = col.child(row(Icon::Play, tr("Technik"), None, selected == "tech", false));
    col = col.child(row(Icon::Play, "Vanilla 26.3".to_string(), None, false, false));
    col = col.child(section(tr("Einstellungen")));
    col = col.child(row(Icon::Palette, tr("Erscheinungsbild"), None, selected == "appearance", false));
    let edge = rect().width(Size::px(1.)).height(Size::fill()).background(t.separator);
    rect().horizontal().child(col).child(edge).into_element()
}

fn toolbar(t: &Theme, title_text: String, subtitle: String, trailing: Vec<Element>) -> Element {
    rect()
        .width(Size::fill())
        .height(Size::px(shell::TOOLBAR))
        .padding((0., t.grid_pad))
        .horizontal()
        .cross_align(Alignment::Center)
        .spacing(space::M)
        .content(Content::flex())
        .child(rect().width(Size::flex(1.)).spacing(1.).child(strong(title_text, t.kind.headline, t.text1)).child(text(subtitle, t.kind.callout, t.text2)))
        .children(trailing)
        .into_element()
}

fn ring_color(t: &Theme, selected: bool, account: &Account) -> Option<Color> {
    if selected {
        Some(t.tile_ring)
    } else if needs_you(account.state) {
        Some(t.attention)
    } else {
        None
    }
}

fn status_glyph(t: &Theme, state: Phase) -> Element {
    match state {
        Phase::Playing => rect().width(Size::px(7.)).height(Size::px(7.)).background(t.success).corner_radius(99.).into_element(),
        Phase::Connecting | Phase::Reconnecting => icon(t, Icon::Refresh, shell::ICON_S, t.text2),
        Phase::Attention => icon(t, Icon::LogIn, shell::ICON_S, t.attention_text),
        Phase::Banned => icon(t, Icon::Alert, shell::ICON_S, t.attention_text),
        Phase::Crashed => icon(t, Icon::Crash, shell::ICON_S, t.danger_text),
        Phase::Hidden => icon(t, Icon::Moon, shell::ICON_S, t.text3),
    }
}

fn progress(t: &Theme, width: f32) -> Element {
    rect()
        .width(Size::px(width))
        .height(Size::px(shell::PROGRESS))
        .background(t.separator)
        .corner_radius(99.)
        .child(rect().width(Size::percent(42.)).height(Size::fill()).background(t.accent).corner_radius(99.))
        .into_element()
}

fn tile(t: &Theme, index: usize, account: &Account, width: f32, selected: bool) -> Element {
    let seed = index as u32 * 7 + 3;
    let ph = (width * 9. / 16.).round();
    let th = ph + t.strip;
    let tone = t.identity[index % t.identity.len()];
    let shown = |opacity: f32| match picture(seed, account.sky) {
        Some(handle) => image(handle).width(Size::fill()).height(Size::px(ph)).aspect_ratio(AspectRatio::None).opacity(opacity).into_element(),
        None => rect().width(Size::fill()).height(Size::px(ph)).into_element(),
    };
    let well = || rect().width(Size::fill()).height(Size::px(ph)).background(t.well).center().spacing(space::M);
    let body = match account.state {
        Phase::Playing => shown(1.),
        Phase::Hidden => rect().width(Size::fill()).height(Size::px(ph)).background(t.well).child(shown(t.hidden_dim)).into_element(),
        Phase::Connecting | Phase::Reconnecting => well().child(face_image(t, seed, 32.)).child(progress(t, 96.)).into_element(),
        Phase::Attention | Phase::Banned => {
            let mut dimmed = rect().width(Size::fill()).height(Size::px(ph)).background(t.well).child(shown(0.55));
            if account.state == Phase::Attention {
                dimmed = dimmed.child(rect().position(Position::new_absolute().top(0.).left(0.)).width(Size::fill()).height(Size::px(ph)).center().child(button(t, tr("Anmelden"), Some(Icon::LogIn), Look::Material, false)));
            }
            dimmed.into_element()
        }
        Phase::Crashed => well().child(icon(t, Icon::Crash, 24., t.danger)).child(button(t, tr("Erneut versuchen"), Some(Icon::Refresh), Look::Material, false)).into_element(),
    };
    let status_color = match account.state {
        Phase::Playing | Phase::Connecting | Phase::Reconnecting => t.text2,
        Phase::Attention | Phase::Banned => t.attention_text,
        Phase::Crashed => t.danger_text,
        Phase::Hidden => t.text3,
    };
    let strip = rect().width(Size::fill()).height(Size::px(t.strip - t.turf)).padding((space::S - t.turf / 2., space::M - 2.)).spacing(space::XS + 1.);
    let name_row = rect()
        .width(Size::fill())
        .horizontal()
        .cross_align(Alignment::Center)
        .spacing(space::S - 1.)
        .content(Content::flex())
        .child(chip(t, seed, if t.turf_band { t.raised } else { tone }, 18.))
        .child(strong(account.name, t.kind.body, t.text1).width(Size::flex(1.)))
        .child(figure(batch_name(account.batch), t.kind.caption, t.text3));
    let status_row = rect()
        .width(Size::fill())
        .horizontal()
        .cross_align(Alignment::Center)
        .spacing(space::XS + 2.)
        .padding((0., 0., 0., space::XS - 1.))
        .child(rect().width(Size::px(shell::ICON_S)).center().child(status_glyph(t, account.state)))
        .child(text(status(account.state), t.kind.callout, status_color));
    let strip = strip.child(name_row).child(status_row);
    let mut card = rect()
        .width(Size::px(width))
        .height(Size::px(th))
        .background(t.content)
        .corner_radius(t.shape.tile)
        .overflow(Overflow::Clip)
        .child(body)
        .child(rect().width(Size::fill()).height(Size::px(t.turf)).background(tone))
        .child(strip);
    if let Some(shadow) = &t.elevation.card {
        card = card.shadow(shadow.clone());
    }
    if needs_you(account.state)
        && let Some(glow) = &t.elevation.glow
    {
        card = card.shadow(glow.clone());
    }
    if let Some(rim) = t.elevation.rim {
        card = card.border(Border::new().fill(rim).width(1.).alignment(BorderAlignment::Inner));
    }
    if account.state == Phase::Hidden {
        card = card.child(rect().position(Position::new_absolute().top(space::S).right(space::S)).child(icon(t, Icon::Moon, shell::ICON, t.text2)));
    }
    let gap = shell::RING + shell::RING_GAP;
    let mut wrap = rect().width(Size::px(width + gap * 2.)).height(Size::px(th + gap * 2.)).padding(gap).corner_radius(t.shape.tile + gap);
    if let Some(color) = ring_color(t, selected, account) {
        let width = if selected || t.elevation.glow.is_none() { shell::RING } else { 1.5 };
        wrap = wrap.border(Border::new().fill(color).width(width).alignment(BorderAlignment::Inner));
    }
    wrap.child(card).into_element()
}

fn grid(t: &Theme, main: f32) -> Element {
    let gap = shell::RING + shell::RING_GAP;
    let cols = shell::COLUMNS;
    let avail = main - t.grid_pad * 2.;
    let width = ((avail - (cols - 1) as f32 * t.grid_gap) / cols as f32).floor();
    let spacing = t.grid_gap - gap * 2.;
    let rows = ACCOUNTS.chunks(cols).enumerate().map(|(r, chunk)| {
        rect()
            .horizontal()
            .spacing(spacing)
            .children(chunk.iter().enumerate().map(|(c, account)| {
                let index = r * cols + c;
                tile(t, index, account, width, index == 0)
            }))
            .into_element()
    });
    rect().padding((t.grid_pad - gap - space::XS, t.grid_pad - gap)).spacing(spacing).children(rows).into_element()
}

fn with_overlay(t: &Theme, root: Rect, width: f32, height: f32, overlay: Option<Element>) -> Element {
    let Some(sheet) = overlay else {
        return root.into_element();
    };
    root.child(rect().position(Position::new_absolute().top(0.).left(0.)).width(Size::px(width)).height(Size::px(height)).background(t.scrim).layer(20))
        .child(rect().position(Position::new_absolute().top(shell::SHEET_TOP).left(((width - shell::SHEET) / 2.).round())).layer(30).child(sheet))
        .into_element()
}

fn grid_screen(t: &Theme, width: f32, height: f32, overlay: Option<Element>) -> Element {
    let main = width - shell::SIDEBAR - 1.;
    let playing = ACCOUNTS.iter().filter(|a| a.state == Phase::Playing).count();
    let attention = ACCOUNTS.iter().filter(|a| needs_you(a.state)).count();
    let subtitle = format!("{} · {}", trf("%lld spielen", &[&playing]), trf("%lld brauchen dich", &[&attention]));
    let tools = vec![field(t, None, tr("Suchen"), Some(Icon::Search), false, Size::px(shell::SEARCH)), button(t, tr("Neue Instanz"), Some(Icon::Plus), Look::Secondary, true)];
    let content = rect().width(Size::px(main)).height(Size::fill()).background(t.surface).child(toolbar(t, tr("Alle"), subtitle, tools)).child(grid(t, main));
    let root = rect().width(Size::fill()).height(Size::fill()).horizontal().background(t.surface).child(sidebar(t, height, "all")).child(content);
    with_overlay(t, root, width, height, overlay)
}

struct Mod {
    name: &'static str,
    author: &'static str,
    version: &'static str,
    on: bool,
    update: bool,
    color: u32,
}

const MODS: [Mod; 9] = [
    Mod { name: "Sodium", author: "CaffeineMC", version: "0.7.2", on: true, update: false, color: 0x5FA8D3 },
    Mod { name: "Lithium", author: "CaffeineMC", version: "0.16.0", on: true, update: true, color: 0xC4C4C4 },
    Mod { name: "Fabric API", author: "FabricMC", version: "0.130.0", on: true, update: false, color: 0xDBD0B4 },
    Mod { name: "Iris Shaders", author: "IrisShaders", version: "1.9.1", on: true, update: false, color: 0x9B7BFF },
    Mod { name: "Mod Menu", author: "Prospector", version: "15.0.0", on: true, update: false, color: 0x8DB360 },
    Mod { name: "Xaero's Minimap", author: "xaero96", version: "25.2.10", on: true, update: true, color: 0xD9895B },
    Mod { name: "Litematica", author: "masady", version: "0.23.1", on: false, update: false, color: 0x4FB0A0 },
    Mod { name: "MiniHUD", author: "masady", version: "0.35.0", on: false, update: false, color: 0x7F8FC2 },
    Mod { name: "Carpet", author: "gnembon", version: "1.4.169", on: true, update: false, color: 0xC0574A },
];

fn mod_icon(t: &Theme, m: &Mod, size: f32) -> Element {
    let fill = Color::from(0xFF00_0000 | m.color);
    let letter = m.name.chars().next().map(String::from).unwrap_or_default();
    rect().width(Size::px(size)).height(Size::px(size)).center().background(fill).corner_radius((size * 0.24).min(t.shape.control + 2.)).child(strong(letter, t.kind.headline + 1., t.ink_on(fill))).into_element()
}

fn mod_row(t: &Theme, m: &Mod, selected: bool, last: bool) -> Element {
    let fill = if selected { t.selection } else { Color::TRANSPARENT };
    let dim = if m.on { 1. } else { 0.55 };
    let middle = rect().width(Size::flex(1.)).spacing(2.).opacity(dim).child(strong(m.name, t.kind.body, t.text1)).child(text(trf("von %@", &[&m.author]), t.kind.callout, t.text2));
    let mut r = rect()
        .width(Size::fill())
        .height(Size::px(shell::MOD_ROW))
        .padding((0., space::L))
        .horizontal()
        .cross_align(Alignment::Center)
        .spacing(space::M)
        .content(Content::flex())
        .background(fill)
        .child(rect().opacity(dim).child(mod_icon(t, m, shell::MOD_ICON)))
        .child(middle);
    if m.update {
        r = r.child(button(t, tr("Aktualisieren"), Some(Icon::Download), Look::Plain, false));
    }
    r = r.child(rect().width(Size::px(84.)).main_align(Alignment::End).horizontal().child(figure(m.version, t.kind.callout, t.text2))).child(switch(t, m.on));
    let mut col = rect().width(Size::fill()).child(r);
    if !last {
        col = col.child(rect().width(Size::fill()).padding((0., 0., 0., space::L + shell::MOD_ICON + space::M)).background(fill).child(hairline(t)));
    }
    col.into_element()
}

fn instance_screen(t: &Theme, width: f32) -> Element {
    let main = width - shell::SIDEBAR - 1.;
    let pad = t.grid_pad + space::XS;
    let hero = match picture(91, Sky::Day) {
        Some(handle) => image(handle).width(Size::px(shell::HERO_ICON)).height(Size::px(shell::HERO_ICON)).aspect_ratio(AspectRatio::Max).image_cover(ImageCover::Center).corner_radius(t.shape.tile).into_element(),
        None => rect().into_element(),
    };
    let meta = format!("Fabric 0.17.2 · Minecraft 26.3 · {}", trf("%lld Mods", &[&MODS.len()]));
    let header = rect()
        .width(Size::fill())
        .horizontal()
        .cross_align(Alignment::Center)
        .spacing(space::L)
        .content(Content::flex())
        .child(rect().width(Size::px(shell::HERO_ICON)).height(Size::px(shell::HERO_ICON)).child(hero))
        .child(rect().width(Size::flex(1.)).spacing(space::XS).child(title(t, tr("Technik"), t.text1)).child(figure(meta, t.kind.body, t.text2)))
        .child(icon_button(t, Icon::More))
        .child(button(t, tr("Spielen"), Some(Icon::Play), Look::Primary, true));
    let tabs = [tr("Mods"), tr("Welten"), tr("Screenshots"), tr("Protokolle"), tr("Einstellungen")];
    let updates = MODS.iter().filter(|m| m.update).count();
    let tools = rect()
        .width(Size::fill())
        .horizontal()
        .cross_align(Alignment::Center)
        .spacing(space::M)
        .content(Content::flex())
        .child(field(t, None, tr("Mods filtern"), Some(Icon::Search), false, Size::px(260.)))
        .child(figure(trf("%lld Mods", &[&MODS.len()]), t.kind.callout, t.text3))
        .child(spacer())
        .child(button(t, trf("%lld Updates", &[&updates]), Some(Icon::Refresh), Look::Secondary, true))
        .child(button(t, tr("Mod hinzufügen"), Some(Icon::Plus), Look::Secondary, true));
    let mut list = rect().width(Size::fill()).height(Size::px(MODS.len() as f32 * (shell::MOD_ROW + 1.) - 1.)).background(t.content).corner_radius(t.shape.tile).overflow(Overflow::Clip);
    if let Some(shadow) = &t.elevation.card {
        list = list.shadow(shadow.clone());
    }
    if let Some(rim) = t.elevation.rim {
        list = list.border(Border::new().fill(rim).width(1.).alignment(BorderAlignment::Inner));
    }
    list = list.children(MODS.iter().enumerate().map(|(i, m)| mod_row(t, m, i == 1, i + 1 == MODS.len())));
    let content = rect()
        .width(Size::px(main))
        .height(Size::fill())
        .padding((pad, pad))
        .spacing(space::XL - 2.)
        .background(t.surface)
        .child(header)
        .child(segmented(t, &tabs, 0))
        .child(rect().width(Size::fill()).spacing(space::M).child(tools).child(list));
    rect().width(Size::fill()).height(Size::fill()).horizontal().background(t.surface).child(sidebar(t, 900., "tech")).child(content).into_element()
}

fn hairline(t: &Theme) -> Element {
    rect().width(Size::fill()).height(Size::px(1.)).background(t.separator).into_element()
}

fn caption_label(t: &Theme, s: String) -> Element {
    strong(s, t.kind.callout, t.text2).into_element()
}

fn import_card(t: &Theme, glyph: Icon, heading: String, detail: String) -> Element {
    rect()
        .width(Size::fill())
        .height(Size::px(shell::MOD_ICON + (space::M - 2.) * 2.))
        .padding((0., space::M + 2.))
        .horizontal()
        .cross_align(Alignment::Center)
        .spacing(space::M)
        .content(Content::flex())
        .background(t.raised)
        .corner_radius(t.shape.control + 2.)
        .child(rect().width(Size::px(shell::MOD_ICON)).height(Size::px(shell::MOD_ICON)).center().background(t.content).corner_radius(t.shape.control).child(icon(t, glyph, shell::ICON, t.accent_text)))
        .child(rect().width(Size::flex(1.)).spacing(2.).child(strong(heading, t.kind.body, t.text1)).child(text(detail, t.kind.caption, t.text2)))
        .into_element()
}

fn sheet_frame(t: &Theme, inner: Element) -> Element {
    let mut frame = rect().background(t.content).corner_radius(t.shape.sheet).overflow(Overflow::Clip).shadow(t.elevation.float.clone()).child(inner);
    if let Some(rim) = t.elevation.rim {
        frame = frame.border(Border::new().fill(rim).width(1.).alignment(BorderAlignment::Inner));
    }
    frame.into_element()
}

fn footer(buttons: Vec<Element>) -> Element {
    rect().width(Size::fill()).padding((space::M + 2., space::XL)).horizontal().cross_align(Alignment::Center).spacing(space::S).content(Content::flex()).child(spacer()).children(buttons).into_element()
}

fn sheet(t: &Theme) -> Element {
    let form = rect()
        .width(Size::fill())
        .horizontal()
        .spacing(space::M)
        .content(Content::flex())
        .child(rect().width(Size::flex(1.)).spacing(space::XS + 2.).child(caption_label(t, tr("Name"))).child(field(t, Some(tr("Technik")), tr("z. B. Technik"), None, true, Size::fill())))
        .child(rect().width(Size::px(shell::FIELD_VERSION)).spacing(space::XS + 2.).child(caption_label(t, tr("Minecraft-Version"))).child(popup(t, "26.3".to_string(), shell::FIELD_VERSION)));
    let loaders = ["Fabric".to_string(), "Quilt".to_string(), "Vanilla".to_string()];
    let loader = rect().spacing(space::XS + 2.).child(caption_label(t, tr("Loader"))).child(segmented(t, &loaders, 0));
    let advanced = rect().horizontal().cross_align(Alignment::Center).spacing(space::XS + 2.).child(icon(t, Icon::ChevronRight, shell::ICON_S + 2., t.text2)).child(text(tr("Erweitert"), t.kind.body, t.text2));
    let defaults = [&MODS[0], &MODS[1], &MODS[2], &MODS[4]];
    let rows = defaults.iter().enumerate().flat_map(|(i, m)| {
        let r = rect()
            .width(Size::fill())
            .height(Size::px(40.))
            .padding((0., space::M))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::M - 2.)
            .content(Content::flex())
            .child(mod_icon(t, m, 24.))
            .child(strong(m.name, t.kind.body, t.text1).width(Size::flex(1.)))
            .child(figure(m.version, t.kind.callout, t.text3))
            .child(icon(t, Icon::Close, shell::ICON_S + 2., t.text3));
        let line = (i > 0).then(|| hairline(t));
        line.into_iter().chain([r.into_element()])
    });
    let mods_head = rect()
        .width(Size::fill())
        .horizontal()
        .cross_align(Alignment::Center)
        .spacing(space::S)
        .content(Content::flex())
        .child(caption_label(t, tr("Mods")))
        .child(figure(defaults.len().to_string(), t.kind.callout, t.text3))
        .child(spacer())
        .child(button(t, tr("Mod hinzufügen"), Some(Icon::Plus), Look::Plain, false));
    let mod_list = rect().width(Size::fill()).height(Size::px(defaults.len() as f32 * 41. - 1.)).background(if t.dark { t.well } else { t.surface }).corner_radius(t.shape.control + 2.).overflow(Overflow::Clip).children(rows);
    let imports = rect()
        .width(Size::fill())
        .spacing(space::S)
        .child(import_card(t, Icon::Download, tr("Aus anderem Launcher importieren…"), tr("Prism, Modrinth, CurseForge, ATLauncher und mehr")))
        .child(import_card(t, Icon::FileDown, tr("Datei importieren…"), tr("Modpack (.mrpack) oder Prism-Export")));
    let body = rect()
        .width(Size::fill())
        .height(Size::flex(1.))
        .padding((space::XL - 2., space::XL, space::L, space::XL))
        .spacing(space::L + 2.)
        .child(strong(tr("Instanz erstellen"), t.kind.headline + 2., t.text1))
        .child(form)
        .child(loader)
        .child(rect().width(Size::fill()).spacing(space::S).child(mods_head).child(mod_list))
        .child(imports)
        .child(advanced);
    let buttons = vec![button(t, tr("Abbrechen"), None, Look::Secondary, true), button(t, tr("Instanz erstellen"), None, Look::Primary, true)];
    sheet_frame(t, rect().width(Size::px(shell::SHEET)).height(Size::px(SHEET_H)).content(Content::flex()).child(body).child(hairline(t)).child(footer(buttons)).into_element())
}

/// What the appearance page shows.
struct Page<'a> {
    lib: &'a Library,
    source: &'a themes::Theme,
    dark: bool,
    actions: Actions,
    notice: Option<Result<String, String>>,
}

fn mini(p: &Theme, width: f32, height: f32) -> Element {
    let pad = 8.;
    let side = (width * 0.24).round();
    let bar = |w: Size, h: f32, c: Color| rect().width(w).height(Size::px(h)).background(c).corner_radius(h / 2.).into_element();
    let nav = rect()
        .width(Size::px(side))
        .height(Size::fill())
        .padding(pad)
        .spacing(6.)
        .background(p.sidebar)
        .child(rect().width(Size::fill()).height(Size::px(12.)).padding((0., 4.)).center().background(p.selection).corner_radius(3.).child(bar(Size::fill(), 4., p.text1)))
        .child(rect().padding((0., 4.)).width(Size::fill()).child(bar(Size::percent(70.), 4., p.text2)))
        .child(rect().padding((0., 4.)).width(Size::fill()).child(bar(Size::percent(50.), 4., p.text2)))
        .child(rect().padding((0., 4.)).width(Size::fill()).child(bar(Size::percent(60.), 4., p.text3)));
    let main_w = width - side - pad * 2.;
    let gap = 6.;
    let tw = ((main_w - gap * 2.) / 3.).floor();
    let th = ((height - pad * 2. - 12. - gap * 2.) / 2.).floor();
    let dots = [p.success, p.success, p.attention, p.danger, p.text3, p.success];
    let tones: Vec<Color> = if p.own_identity { (0..6).map(|i| p.identity[i % p.identity.len()]).collect() } else { vec![p.accent, p.success, p.attention, p.danger, p.text2, p.focus] };
    let tile = |i: usize| {
        let ring = match i {
            0 => Some(p.tile_ring),
            2 => Some(p.attention),
            _ => None,
        };
        let card = rect()
            .width(Size::px(tw))
            .height(Size::px(th))
            .background(p.content)
            .corner_radius((p.shape.tile * 0.5).min(6.))
            .overflow(Overflow::Clip)
            .child(rect().width(Size::fill()).height(Size::percent(58.)).background(tones[i].with_af32(if i == 4 { 0.4 } else { 0.85 })))
            .child(rect().width(Size::fill()).height(Size::fill()).padding((0., 5.)).horizontal().cross_align(Alignment::Center).spacing(4.).child(rect().width(Size::px(5.)).height(Size::px(5.)).background(dots[i]).corner_radius(3.)).child(bar(Size::percent(60.), 3., p.text2)));
        match ring {
            Some(c) => card.border(Border::new().fill(c).width(1.5).alignment(BorderAlignment::Outer)).into_element(),
            None => card.into_element(),
        }
    };
    let row = |from: usize| rect().horizontal().spacing(gap).children((from..from + 3).map(&tile)).into_element();
    let top = rect().width(Size::fill()).height(Size::px(12.)).horizontal().cross_align(Alignment::Center).content(Content::flex()).child(bar(Size::percent(36.), 5., p.text1)).child(spacer()).child(rect().width(Size::px(30.)).height(Size::px(11.)).background(p.accent).corner_radius((p.shape.control * 0.6).min(6.)));
    let main = rect().width(Size::flex(1.)).height(Size::fill()).padding(pad).spacing(gap).child(top).child(row(0)).child(row(3));
    rect().width(Size::px(width)).height(Size::px(height)).horizontal().content(Content::flex()).background(p.surface).child(nav).child(main).into_element()
}

#[derive(PartialEq)]
struct ThemeCard {
    id: String,
    name: String,
    caption: String,
    badge: Option<String>,
    warn: Option<String>,
    preview: Theme,
    chrome: Theme,
    selected: bool,
    width: f32,
    chosen: State<Option<String>>,
    key: DiffKey,
}

impl KeyExt for ThemeCard {
    fn key_mut(&mut self) -> &mut DiffKey {
        &mut self.key
    }
}

impl Component for ThemeCard {
    fn render(&self) -> impl IntoElement {
        let mut hovering = use_state(|| false);
        let a11y = use_a11y();
        let focus = use_focus(a11y);
        let t = &self.chrome;
        let ph = (self.width * 0.62).round();
        let gap = shell::RING + shell::RING_GAP;
        let radius = (self.preview.shape.tile).clamp(2., 12.);
        let ring = if focus() == Focus::Keyboard {
            Some((t.focus, shell::RING))
        } else if self.selected {
            Some((t.tile_ring, shell::RING))
        } else if *hovering.read() {
            Some((t.separator, shell::RING))
        } else {
            None
        };
        let mut frame = rect().width(Size::px(self.width + gap * 2.)).height(Size::px(ph + gap * 2.)).padding(gap).corner_radius(radius + gap);
        if let Some((fill, width)) = ring {
            frame = frame.border(Border::new().fill(fill).width(width).alignment(BorderAlignment::Inner));
        }
        let preview = rect().corner_radius(radius).overflow(Overflow::Clip).border(Border::new().fill(t.separator).width(1.).alignment(BorderAlignment::Outer)).child(mini(&self.preview, self.width, ph));
        let mut name_row = rect().width(Size::fill()).horizontal().cross_align(Alignment::Center).spacing(space::S).content(Content::flex()).child(strong(self.name.clone(), t.kind.body, t.text1).width(Size::flex(1.)));
        if self.selected {
            name_row = name_row.child(icon(t, Icon::Check, shell::ICON, t.accent_text));
        }
        let mut caption_row = rect().width(Size::fill()).height(Size::px(shell::BADGE)).horizontal().cross_align(Alignment::Center).spacing(space::S).content(Content::flex()).child(text(self.caption.clone(), t.kind.caption, t.text3).width(Size::flex(1.)));
        if let Some(b) = &self.badge {
            caption_row = caption_row.child(badge(t, b.clone(), t.raised, t.text2));
        }
        let mut info = rect().width(Size::fill()).padding((0., gap)).spacing(3.).child(name_row).child(caption_row);
        if let Some(warn) = &self.warn {
            info = info.child(text(warn.clone(), t.kind.caption, t.attention_text));
        }
        let id = self.id.clone();
        let mut chosen = self.chosen;
        rect()
            .width(Size::px(self.width + gap * 2.))
            .margin(-gap)
            .spacing(space::S - gap)
            .a11y_id(a11y)
            .a11y_focusable(true)
            .a11y_role(AccessibilityRole::Button)
            .cursor(CursorIcon::Pointer)
            .on_all_press(move |e: Event<PressEventData>| {
                let left = match e.data() {
                    PressEventData::Mouse(data) => data.button == Some(MouseButton::Left),
                    _ => true,
                };
                if left {
                    a11y.request_focus();
                    chosen.set(Some(id.clone()));
                }
            })
            .on_pointer_over(move |_| hovering.set_if_modified(true))
            .on_pointer_out(move |_| hovering.set_if_modified(false))
            .child(frame.child(preview))
            .child(info)
    }

    fn render_key(&self) -> DiffKey {
        self.key.clone()
    }
}

fn card_for(t: &Theme, page: &Page, theme: &themes::Theme, width: f32) -> Element {
    let selected = theme.id == page.source.id;
    let caption = match &theme.origin {
        Origin::User(path) => path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        Origin::BuiltIn => match (theme.author.as_deref(), theme.license.as_deref()) {
            (Some(a), Some(l)) if a != l => format!("{a} · {l}"),
            (Some(a), _) => a.to_string(),
            (None, l) => l.unwrap_or_default().to_string(),
        },
    };
    let badge = if theme.id == themes::DEFAULT {
        Some(tr("Standard"))
    } else if !theme.has(true) {
        Some(tr("Nur hell"))
    } else if !theme.has(false) {
        Some(tr("Nur dunkel"))
    } else {
        None
    };
    let warn = match theme.warnings.len() {
        0 => None,
        1 => Some(tr("1 Hinweis")),
        n => Some(trf("%lld Hinweise", &[&n])),
    };
    let name = if theme.origin == Origin::BuiltIn { tr(&theme.name) } else { theme.name.clone() };
    ThemeCard { id: theme.id.clone(), name, caption, badge, warn, preview: Theme::new(theme, page.dark), chrome: t.clone(), selected, width, chosen: page.actions.chosen, key: DiffKey::None }.key(&theme.id).into_element()
}

fn card_grid(t: &Theme, page: &Page, list: &[&themes::Theme], width: f32, columns: usize) -> Element {
    let gap = space::L;
    let rows = list.chunks(columns).map(|chunk| rect().horizontal().spacing(gap).children(chunk.iter().map(|theme| card_for(t, page, theme, width))).into_element());
    rect().spacing(space::XL).children(rows).into_element()
}

fn group(t: &Theme, heading: String, detail: String, body: Element) -> Element {
    let head = rect().width(Size::fill()).spacing(2.).child(strong(heading, t.kind.headline, t.text1)).child(text(detail, t.kind.callout, t.text2));
    rect().width(Size::fill()).spacing(space::M + 2.).child(head).child(body).into_element()
}

fn shown_errors(errors: &[String]) -> Vec<String> {
    const SHOWN: usize = 4;
    let mut lines: Vec<String> = errors.iter().take(SHOWN).cloned().collect();
    if errors.len() > SHOWN {
        lines.push(trf("und %lld weitere", &[&(errors.len() - SHOWN)]));
    }
    lines
}

fn notice_box(t: &Theme, glyph: Icon, tint: Color, ink: Color, heading: String, lines: Vec<String>) -> Element {
    let mut col = rect().width(Size::flex(1.)).spacing(3.).child(strong(heading, t.kind.body, t.text1));
    for line in lines {
        col = col.child(para(line, t.kind.callout, t.text2));
    }
    rect()
        .width(Size::fill())
        .padding((space::M, space::M + 2.))
        .horizontal()
        .spacing(space::M - 2.)
        .content(Content::flex())
        .background(tint)
        .corner_radius(t.shape.control + 2.)
        .child(rect().padding((1., 0., 0., 0.)).child(icon(t, glyph, shell::ICON, ink)))
        .child(col)
        .into_element()
}

fn themes_screen(t: &Theme, width: f32, height: f32, page: &Page, overlay: Option<Element>) -> Element {
    let main = width - shell::SIDEBAR - 1.;
    let columns = 4;
    let avail = main - t.grid_pad * 2.;
    let card_w = ((avail - (columns - 1) as f32 * space::L) / columns as f32).floor();
    let pick = |ids: &[&str]| ids.iter().filter_map(|id| page.lib.get(id)).collect::<Vec<_>>();
    let own: Vec<&themes::Theme> = page.lib.themes.iter().filter(|x| matches!(x.origin, Origin::User(_))).collect();
    let actions = page.actions;
    let subtitle = if !page.source.has(page.dark) {
        if page.dark { trf("„%@“ gibt es nur hell.", &[&page.source.name]) } else { trf("„%@“ gibt es nur dunkel.", &[&page.source.name]) }
    } else {
        tr("Hell oder dunkel folgt dem System.")
    };
    let mut sheet = actions.sheet;
    let trailing = vec![
        pressable(button(t, tr("Eigenes Theme bauen"), Some(Icon::Help), Look::Plain, true), move |_| sheet.set(true)),
        pressable(button(t, tr("Ordner öffnen"), Some(Icon::Folder), Look::Secondary, true), move |_| actions.open()),
        pressable(button(t, tr("Als Vorlage kopieren"), Some(Icon::Copy), Look::Secondary, true), move |_| actions.copy()),
    ];
    let mut own_body = rect().width(Size::fill()).spacing(space::M);
    if matches!(page.source.origin, Origin::User(_)) && !page.source.warnings.is_empty() {
        own_body = own_body.child(notice_box(t, Icon::Alert, t.attention.with_af32(0.12), t.attention_text, trf("„%@“: so liest Manycraft die Datei", &[&page.source.name]), page.source.warnings.clone()));
    }
    for broken in &page.lib.broken {
        own_body = own_body.child(notice_box(t, Icon::Alert, t.attention.with_af32(0.12), t.attention_text, trf("%@ wird nicht geladen", &[&broken.file]), shown_errors(&broken.errors)));
    }
    own_body = if own.is_empty() {
        own_body.child(para(tr("Noch keine eigenen Themes. „Als Vorlage kopieren“ legt eine Kopie des gewählten Themes in den Ordner; dort änderst du die Farben mit einem Texteditor."), t.kind.body, t.text2))
    } else {
        own_body.child(card_grid(t, page, &own, card_w, columns))
    };
    let mut body = rect().width(Size::fill()).padding((space::S, t.grid_pad, t.grid_pad * 2., t.grid_pad)).spacing(space::XL + space::L);
    match &page.notice {
        Some(Ok(text)) => body = body.child(notice_box(t, Icon::Check, t.success.with_af32(0.12), t.success_text, tr("Vorlage angelegt"), vec![text.clone()])),
        Some(Err(text)) => body = body.child(notice_box(t, Icon::Alert, t.danger.with_af32(0.12), t.danger_text, tr("Das hat nicht geklappt"), vec![text.clone()])),
        None => {}
    }
    body = body
        .child(group(t, tr("Manycraft"), tr("Von Manycraft gestaltet"), card_grid(t, page, &pick(&ORIGINALS), card_w, columns)))
        .child(group(t, tr("Bekannte Paletten"), tr("Nachgebaut nach ihren freien Vorlagen, Lizenz an jeder Karte"), card_grid(t, page, &pick(&KNOWN), card_w, columns)))
        .child(group(t, tr("Zum Spaß"), tr("Laut, aber gut lesbar"), card_grid(t, page, &pick(&FUN), card_w, columns)))
        .child(group(t, tr("Eigene"), trf("Aus dem Ordner %@", &[&"themes"]), own_body.into_element()));
    let content = rect()
        .width(Size::px(main))
        .height(Size::fill())
        .background(t.surface)
        .child(toolbar(t, tr("Erscheinungsbild"), subtitle, trailing))
        .child(ScrollView::new().width(Size::fill()).height(Size::flex(1.)).child(body));
    let root = rect().width(Size::fill()).height(Size::fill()).horizontal().background(t.surface).child(sidebar(t, height, "appearance")).child(content.content(Content::flex()));
    with_overlay(t, root, width, height, overlay)
}

fn schema_sheet(t: &Theme, actions: Actions) -> Element {
    let row = |key: String, purpose: String, note: Option<String>| -> Element {
        let mut r = rect().width(Size::fill()).padding((space::S - 1., 0.)).horizontal().cross_align(Alignment::Center).spacing(space::M).content(Content::flex()).child(rect().width(Size::px(132.)).child(mono(key, t.kind.callout, t.text1))).child(text(purpose, t.kind.callout, t.text2).width(Size::flex(1.)));
        if let Some(note) = note {
            r = r.child(text(note, t.kind.caption, t.text3));
        }
        r.into_element()
    };
    let list = |rows: Vec<Element>| {
        let mut col = rect().width(Size::fill());
        for (i, r) in rows.into_iter().enumerate() {
            if i > 0 {
                col = col.child(hairline(t));
            }
            col = col.child(r);
        }
        col.into_element()
    };
    let colors = list(themes::Token::ALL.iter().map(|&token: &Token| row(token.key().to_string(), token.purpose(), token.required().then(|| tr("Pflicht")))).collect());
    let extras = list(vec![
        row("name".into(), tr("Name in der Auswahl"), Some(tr("Pflicht"))),
        row("author, license, source".into(), tr("Wer es gemacht hat, Lizenz, Link"), None),
        row("radius".into(), tr("Ecken: 0 eckig, 1 normal, 2 sehr rund"), None),
        row("density".into(), tr("Abstände: compact, comfortable oder spacious"), None),
        row("font".into(), tr("Schriftart, z. B. \"Inter\""), None),
        row("signature".into(), tr("glow: Kacheln, die dich brauchen, leuchten; turf: Farbband in Account-Farbe"), None),
        row("identity".into(), tr("Bis zu 16 Farben für die Account-Chips"), None),
    ]);
    let code = rect().width(Size::fill()).padding((space::M, space::M + 2.)).background(t.well).corner_radius(t.shape.field).child(mono(EXAMPLE, t.kind.callout, t.text1).line_height(1.3));
    let body = rect()
        .width(Size::fill())
        .padding((space::XL - 2., space::XL, space::XL, space::XL))
        .spacing(space::L)
        .child(strong(tr("Eigenes Theme bauen"), t.kind.headline + 2., t.text1))
        .child(para(tr("Ein Theme ist eine JSON-Datei im Ordner „themes“. Manycraft zeigt Änderungen sofort, sobald du speicherst. Am leichtesten startest du mit „Als Vorlage kopieren“."), t.kind.body, t.text2))
        .child(code)
        .child(para(tr("Für hell und dunkel in einer Datei: statt „appearance“ und „colors“ die Blöcke „light“ und „dark“, jeder mit denselben Farben."), t.kind.body, t.text2))
        .child(rect().width(Size::fill()).spacing(space::XS).child(caption_label(t, tr("Farben"))).child(colors))
        .child(rect().width(Size::fill()).spacing(space::XS).child(caption_label(t, tr("Weitere Einträge"))).child(extras))
        .child(para(tr("Ist eine Farbe zu blass, macht Manycraft sie lesbar (Text 4,5:1, Flächen und Ringe 3:1) und sagt dir, welche. Fehlt eine Pflichtfarbe oder ist die Datei kaputt, steht der Grund unter „Eigene“; die anderen Themes laufen weiter."), t.kind.body, t.text2));
    let mut sheet = actions.sheet;
    let done = pressable(button(t, tr("Fertig"), None, Look::Primary, true), move |_| sheet.set(false));
    let inner = rect().width(Size::px(shell::SHEET)).height(Size::px(SHEET_H)).content(Content::flex()).child(ScrollView::new().width(Size::fill()).height(Size::flex(1.)).child(body)).child(hairline(t)).child(footer(vec![done]));
    sheet_frame(t, inner.into_element())
}
