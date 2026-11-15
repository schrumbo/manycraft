//! Colour themes as JSON files: the built-ins compiled in and the user's own in `<data>/themes`. Every file is
//! checked with German messages; a colour with too little contrast is moved toward black or white until it reads,
//! with a warning, so a bad file never breaks the window.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::i18n::{Lang, lang, tr, trf};
use super::paths;

/// The theme a fresh install uses, in the system's light or dark appearance.
pub const DEFAULT: &str = "lantern";

const MAX_BYTES: u64 = 256 * 1024;

const BUILT_IN: [(&str, &str); 19] = [
    ("lantern", include_str!("../../assets/themes/lantern.json")),
    ("biome", include_str!("../../assets/themes/biome.json")),
    ("tinte", include_str!("../../assets/themes/tinte.json")),
    ("catppuccin", include_str!("../../assets/themes/catppuccin.json")),
    ("nord", include_str!("../../assets/themes/nord.json")),
    ("dracula", include_str!("../../assets/themes/dracula.json")),
    ("gruvbox", include_str!("../../assets/themes/gruvbox.json")),
    ("solarized", include_str!("../../assets/themes/solarized.json")),
    ("tokyo-night", include_str!("../../assets/themes/tokyo-night.json")),
    ("rose-pine", include_str!("../../assets/themes/rose-pine.json")),
    ("github", include_str!("../../assets/themes/github.json")),
    ("high-contrast", include_str!("../../assets/themes/high-contrast.json")),
    ("nether", include_str!("../../assets/themes/nether.json")),
    ("creeper", include_str!("../../assets/themes/creeper.json")),
    ("enderman", include_str!("../../assets/themes/enderman.json")),
    ("redstone", include_str!("../../assets/themes/redstone.json")),
    ("hacker", include_str!("../../assets/themes/hacker.json")),
    ("vaporwave", include_str!("../../assets/themes/vaporwave.json")),
    ("windows-98", include_str!("../../assets/themes/windows-98.json")),
];

/// Account colours for themes that name none.
pub const IDENTITY: [Rgba; 8] = [Rgba::rgb(0xC8875E), Rgba::rgb(0x8DB07A), Rgba::rgb(0x7F97C2), Rgba::rgb(0xC98FA3), Rgba::rgb(0xCDB98A), Rgba::rgb(0xA486B8), Rgba::rgb(0xA8A29A), Rgba::rgb(0x9FC3D6)];

const TOP_KEYS: [&str; 13] = ["name", "author", "license", "source", "radius", "density", "font", "signature", "identity", "appearance", "colors", "light", "dark"];

/// A colour with alpha, written `#rrggbb` or `#rrggbbaa`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const BLACK: Self = Self::rgb(0x000000);
    pub const WHITE: Self = Self::rgb(0xFFFFFF);

    /// An opaque colour from `0xRRGGBB`.
    pub const fn rgb(hex: u32) -> Self {
        Self { r: (hex >> 16) as u8, g: (hex >> 8) as u8, b: hex as u8, a: 255 }
    }

    /// Reads `#rrggbb` or `#rrggbbaa`.
    pub fn parse(text: &str) -> Option<Self> {
        let hex = text.trim().strip_prefix('#')?;
        if !(hex.len() == 6 || hex.len() == 8) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2).unwrap_or("00"), 16).ok();
        Some(Self { r: byte(0)?, g: byte(2)?, b: byte(4)?, a: if hex.len() == 8 { byte(6)? } else { 255 } })
    }

    /// `#RRGGBB`, plus `AA` when not opaque.
    pub fn hex(self) -> String {
        if self.a == 255 { format!("#{:02X}{:02X}{:02X}", self.r, self.g, self.b) } else { format!("#{:02X}{:02X}{:02X}{:02X}", self.r, self.g, self.b, self.a) }
    }

    /// The same colour at `alpha` (0–1).
    pub fn with_alpha(self, alpha: f32) -> Self {
        Self { a: (alpha.clamp(0., 1.) * 255.).round() as u8, ..self }
    }

    /// The colour `t` (0–1) of the way to `other`, keeping this alpha.
    pub fn mix(self, other: Self, t: f32) -> Self {
        let lerp = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round().clamp(0., 255.) as u8;
        Self { r: lerp(self.r, other.r), g: lerp(self.g, other.g), b: lerp(self.b, other.b), a: self.a }
    }

    fn luminance(self) -> f64 {
        let channel = |v: u8| {
            let v = f64::from(v) / 255.;
            if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * channel(self.r) + 0.7152 * channel(self.g) + 0.0722 * channel(self.b)
    }

    /// WCAG contrast ratio against `other`, ignoring alpha.
    pub fn contrast(self, other: Self) -> f64 {
        let (a, b) = (self.luminance(), other.luminance());
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    /// Black or white, whichever reads better on this colour.
    pub fn ink(self) -> Self {
        if Self::BLACK.contrast(self) >= Self::WHITE.contrast(self) { Self::BLACK } else { Self::WHITE }
    }
}

/// One named colour of a palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Token {
    Surface,
    Sidebar,
    Raised,
    Sunken,
    Control,
    Separator,
    Text1,
    Text2,
    Text3,
    Accent,
    AccentText,
    OnAccent,
    Attention,
    AttentionText,
    OnAttention,
    Danger,
    DangerText,
    OnDanger,
    Success,
    SuccessText,
    FocusRing,
    TileRing,
    Selection,
    Scrim,
}

impl Token {
    /// Every token, in file order.
    pub const ALL: [Token; 24] = [
        Token::Surface,
        Token::Sidebar,
        Token::Raised,
        Token::Sunken,
        Token::Control,
        Token::Separator,
        Token::Text1,
        Token::Text2,
        Token::Text3,
        Token::Accent,
        Token::AccentText,
        Token::OnAccent,
        Token::Attention,
        Token::AttentionText,
        Token::OnAttention,
        Token::Danger,
        Token::DangerText,
        Token::OnDanger,
        Token::Success,
        Token::SuccessText,
        Token::FocusRing,
        Token::TileRing,
        Token::Selection,
        Token::Scrim,
    ];

    const REQUIRED: [Token; 11] = [Token::Surface, Token::Raised, Token::Sunken, Token::Text1, Token::Text2, Token::Text3, Token::Accent, Token::OnAccent, Token::Attention, Token::Danger, Token::Success];

    /// The key in a theme file.
    pub fn key(self) -> &'static str {
        match self {
            Token::Surface => "surface",
            Token::Sidebar => "sidebar",
            Token::Raised => "raised",
            Token::Sunken => "sunken",
            Token::Control => "control",
            Token::Separator => "separator",
            Token::Text1 => "text-1",
            Token::Text2 => "text-2",
            Token::Text3 => "text-3",
            Token::Accent => "accent",
            Token::AccentText => "accent-text",
            Token::OnAccent => "on-accent",
            Token::Attention => "attention",
            Token::AttentionText => "attention-text",
            Token::OnAttention => "on-attention",
            Token::Danger => "danger",
            Token::DangerText => "danger-text",
            Token::OnDanger => "on-danger",
            Token::Success => "success",
            Token::SuccessText => "success-text",
            Token::FocusRing => "focus-ring",
            Token::TileRing => "tile-ring",
            Token::Selection => "selection",
            Token::Scrim => "scrim",
        }
    }

    /// Whether a palette must name this token.
    pub fn required(self) -> bool {
        Self::REQUIRED.contains(&self)
    }

    /// What the token colours, for the schema help.
    pub fn purpose(self) -> String {
        match self {
            Token::Surface => tr("Fensterhintergrund"),
            Token::Sidebar => tr("Seitenleiste"),
            Token::Raised => tr("Kacheln, Listen und Blätter"),
            Token::Sunken => tr("Eingabefelder und leere Kacheln"),
            Token::Control => tr("Tasten und Umschalter"),
            Token::Separator => tr("Trennlinien"),
            Token::Text1 => tr("Haupttext"),
            Token::Text2 => tr("Nebentext"),
            Token::Text3 => tr("Hinweise und Platzhalter"),
            Token::Accent => tr("Hauptaktion und Auswahl"),
            Token::AccentText => tr("Text in Akzentfarbe"),
            Token::OnAccent => tr("Text auf der Akzentfarbe"),
            Token::Attention => tr("Braucht dich"),
            Token::AttentionText => tr("Text für „Braucht dich“"),
            Token::OnAttention => tr("Text auf „Braucht dich“"),
            Token::Danger => tr("Abgestürzt und Löschen"),
            Token::DangerText => tr("Text für Fehler"),
            Token::OnDanger => tr("Text auf der Fehlerfarbe"),
            Token::Success => tr("Spielt"),
            Token::SuccessText => tr("Text für „Spielt“"),
            Token::FocusRing => tr("Tastaturfokus"),
            Token::TileRing => tr("Ring der gewählten Kachel"),
            Token::Selection => tr("Hintergrund gewählter Zeilen"),
            Token::Scrim => tr("Abdunklung hinter Blättern"),
        }
    }
}

/// The contrast every palette is held to: a token against the tokens it sits on.
const RULES: [(Token, &[Token], f64); 16] = [
    (Token::Accent, &[Token::Surface, Token::Raised], 3.),
    (Token::Attention, &[Token::Surface, Token::Raised], 3.),
    (Token::Danger, &[Token::Raised], 3.),
    (Token::Success, &[Token::Raised], 3.),
    (Token::FocusRing, &[Token::Surface, Token::Raised], 3.),
    (Token::TileRing, &[Token::Surface], 3.),
    (Token::Text1, &[Token::Surface, Token::Sidebar, Token::Raised, Token::Sunken, Token::Control], 4.5),
    (Token::Text2, &[Token::Surface, Token::Sidebar, Token::Raised, Token::Sunken, Token::Control], 4.5),
    (Token::Text3, &[Token::Surface, Token::Sidebar, Token::Raised, Token::Sunken], 4.5),
    (Token::AccentText, &[Token::Surface, Token::Raised], 4.5),
    (Token::AttentionText, &[Token::Surface, Token::Raised], 4.5),
    (Token::DangerText, &[Token::Surface, Token::Raised], 4.5),
    (Token::SuccessText, &[Token::Surface, Token::Raised], 4.5),
    (Token::OnAccent, &[Token::Accent], 4.5),
    (Token::OnAttention, &[Token::Attention], 4.5),
    (Token::OnDanger, &[Token::Danger], 4.5),
];

/// Every colour of one appearance, with the gaps filled and contrast made good.
#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    colors: [Rgba; 24],
    /// Colours told apart per account, for chips and the turf band.
    pub identity: Vec<Rgba>,
}

impl Palette {
    /// The colour of `token`.
    pub fn get(&self, token: Token) -> Rgba {
        self.colors[token as usize]
    }

    fn set(&mut self, token: Token, color: Rgba) {
        self.colors[token as usize] = color;
    }
}

/// How much room the layout leaves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Density {
    Compact,
    Comfortable,
    Spacious,
}

/// The one element a theme makes its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signature {
    Plain,
    /// Tiles that need you give off a soft light.
    Glow,
    /// A band in the account's colour tops each tile.
    Turf,
}

/// Where a theme comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    BuiltIn,
    User(PathBuf),
}

/// A checked theme, ready to draw with.
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub id: String,
    pub name: String,
    pub author: Option<String>,
    pub license: Option<String>,
    pub source: Option<String>,
    pub light: Option<Palette>,
    pub dark: Option<Palette>,
    /// Corner radii relative to the default, 0–2.
    pub radius: f32,
    pub density: Density,
    pub font: Option<String>,
    pub signature: Signature,
    /// What was fixed or ignored while reading, in German or English.
    pub warnings: Vec<String>,
    pub origin: Origin,
    text: String,
}

impl Theme {
    /// The palette for the dark or light appearance, or the only one the theme ships.
    pub fn palette(&self, dark: bool) -> &Palette {
        let (wanted, other) = if dark { (&self.dark, &self.light) } else { (&self.light, &self.dark) };
        match (wanted, other) {
            (Some(p), _) | (None, Some(p)) => p,
            (None, None) => unreachable!("a parsed theme ships at least one appearance"),
        }
    }

    /// Whether the theme ships the dark or light appearance.
    pub fn has(&self, dark: bool) -> bool {
        if dark { self.dark.is_some() } else { self.light.is_some() }
    }
}

/// A user file that could not be read as a theme.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Broken {
    pub file: String,
    pub errors: Vec<String>,
}

/// All themes: the built-ins, then the user's in file name order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Library {
    pub themes: Vec<Theme>,
    pub broken: Vec<Broken>,
}

impl Library {
    /// Reads the built-ins and every `*.json` in `dir`; a missing folder means no user themes.
    pub fn load(dir: &Path) -> Self {
        let mut library = Self::default();
        for (id, text) in BUILT_IN {
            if let Ok(theme) = parse(id, text, Origin::BuiltIn) {
                library.themes.push(theme);
            }
        }
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir).map(|entries| entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("json"))).collect()).unwrap_or_default();
        files.sort();
        for path in files {
            let file = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let id = format!("user/{}", path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
            match read(&path).and_then(|text| parse(&id, &text, Origin::User(path.clone()))) {
                Ok(theme) => library.themes.push(theme),
                Err(errors) => library.broken.push(Broken { file, errors }),
            }
        }
        library
    }

    /// The theme with `id`.
    pub fn get(&self, id: &str) -> Option<&Theme> {
        self.themes.iter().find(|t| t.id == id)
    }

    /// The chosen theme, or the default when nothing or something gone is chosen.
    pub fn chosen(&self, id: Option<&str>) -> Option<&Theme> {
        id.and_then(|id| self.get(id)).or_else(|| self.get(DEFAULT)).or_else(|| self.themes.first())
    }
}

/// The folder holding the user's themes.
pub fn dir() -> PathBuf {
    paths::data_dir().join("themes")
}

fn read(path: &Path) -> Result<String, Vec<String>> {
    let too_big = std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_BYTES);
    if too_big {
        return Err(vec![tr("Die Datei ist größer als 256 KB.")]);
    }
    std::fs::read_to_string(path).map_err(|_| vec![tr("Die Datei lässt sich nicht lesen.")])
}

/// Copies `theme` into `dir` as a file of the user's own, named after it, and returns the new file.
pub fn copy_as_template(theme: &Theme, dir: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let name = trf("%@ (Kopie)", &[&theme.name]);
    let stem = slug(&name);
    let mut doc: Map<String, Value> = serde_json::from_str(&theme.text).unwrap_or_default();
    doc.insert("name".into(), Value::String(name));
    let text = render(&doc);
    for n in 1..1000 {
        let path = dir.join(if n == 1 { format!("{stem}.json") } else { format!("{stem}-{n}.json") });
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                file.write_all(text.as_bytes())?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists))
}

fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.to_lowercase().chars() {
        match c {
            'ä' => out.push_str("ae"),
            'ö' => out.push_str("oe"),
            'ü' => out.push_str("ue"),
            'ß' => out.push_str("ss"),
            c if c.is_ascii_alphanumeric() => out.push(c),
            _ if !out.ends_with('-') => out.push('-'),
            _ => {}
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() { "theme".into() } else { out }
}

fn render(doc: &Map<String, Value>) -> String {
    let mut out = String::from("{\n");
    let mut keys: Vec<&String> = TOP_KEYS.iter().filter_map(|k| doc.get_key_value(*k).map(|(k, _)| k)).collect();
    keys.extend(doc.keys().filter(|k| !TOP_KEYS.contains(&k.as_str())));
    for (i, key) in keys.iter().enumerate() {
        let value = &doc[key.as_str()];
        let text = match value {
            Value::Object(colors) => {
                let mut inner: Vec<&String> = Token::ALL.iter().filter_map(|t| colors.get_key_value(t.key()).map(|(k, _)| k)).collect();
                inner.extend(colors.keys().filter(|k| !Token::ALL.iter().any(|t| t.key() == k.as_str())));
                let lines: Vec<String> = inner.iter().map(|k| format!("    {}: {}", Value::String((*k).clone()), colors[k.as_str()])).collect();
                format!("{{\n{}\n  }}", lines.join(",\n"))
            }
            other => other.to_string(),
        };
        out.push_str(&format!("  {}: {text}{}\n", Value::String((*key).clone()), if i + 1 < keys.len() { "," } else { "" }));
    }
    out.push_str("}\n");
    out
}

fn ratio_text(r: f64) -> String {
    let text = format!("{r:.1}");
    if lang() == Lang::De { text.replace('.', ",") } else { text }
}

/// Reads one theme file; the errors say in German or English what to fix.
pub fn parse(id: &str, text: &str, origin: Origin) -> Result<Theme, Vec<String>> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let value: Value = serde_json::from_str(text).map_err(|e| vec![trf("Kein gültiges JSON (Zeile %lld, Spalte %lld).", &[&e.line(), &e.column()])])?;
    let Value::Object(doc) = value else {
        return Err(vec![tr("Die Datei muss ein JSON-Objekt { … } sein.")]);
    };
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    for key in doc.keys().filter(|k| !TOP_KEYS.contains(&k.as_str())) {
        warnings.push(trf("Unbekannter Eintrag „%@“ wird ignoriert.", &[key]));
    }
    let string = |key: &str, errors: &mut Vec<String>| match doc.get(key) {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Some(_) => {
            errors.push(trf("„%@“ muss ein Text sein.", &[&key]));
            None
        }
    };
    let name = string("name", &mut errors);
    if name.is_none() && !doc.contains_key("name") {
        errors.push(tr("Es fehlt „name“, der Name des Themes."));
    }
    let author = string("author", &mut errors);
    let license = string("license", &mut errors);
    let source = string("source", &mut errors);
    let font = string("font", &mut errors);
    let radius = match doc.get("radius") {
        None => 1.,
        Some(v) => match v.as_f64() {
            Some(r) if (0. ..=2.).contains(&r) => r as f32,
            _ => {
                warnings.push(tr("„radius“ muss eine Zahl von 0 bis 2 sein; es gilt 1."));
                1.
            }
        },
    };
    let density = match doc.get("density").and_then(Value::as_str) {
        None if !doc.contains_key("density") => Density::Comfortable,
        Some("compact") => Density::Compact,
        Some("comfortable") => Density::Comfortable,
        Some("spacious") => Density::Spacious,
        _ => {
            warnings.push(tr("„density“ muss „compact“, „comfortable“ oder „spacious“ sein; es gilt „comfortable“."));
            Density::Comfortable
        }
    };
    let signature = match doc.get("signature").and_then(Value::as_str) {
        None if !doc.contains_key("signature") => Signature::Plain,
        Some("none") => Signature::Plain,
        Some("glow") => Signature::Glow,
        Some("turf") => Signature::Turf,
        _ => {
            warnings.push(tr("„signature“ muss „glow“, „turf“ oder „none“ sein; es gilt „none“."));
            Signature::Plain
        }
    };
    let identity = match doc.get("identity") {
        None => IDENTITY.to_vec(),
        Some(Value::Array(items)) => {
            let colors: Vec<Rgba> = items.iter().filter_map(|v| v.as_str().and_then(Rgba::parse)).take(16).collect();
            if colors.len() != items.len().min(16) || colors.is_empty() {
                warnings.push(tr("„identity“ muss eine Liste von Farben wie \"#1A2B3C\" sein; ungültige werden übersprungen."));
            }
            if colors.is_empty() { IDENTITY.to_vec() } else { colors }
        }
        Some(_) => {
            warnings.push(tr("„identity“ muss eine Liste von Farben wie \"#1A2B3C\" sein; ungültige werden übersprungen."));
            IDENTITY.to_vec()
        }
    };
    let (mut light_map, mut dark_map) = (doc.get("light"), doc.get("dark"));
    let (mut light_key, mut dark_key) = ("light", "dark");
    match (doc.get("colors"), doc.get("appearance").and_then(Value::as_str)) {
        (Some(_), _) if light_map.is_some() || dark_map.is_some() => errors.push(tr("Entweder „colors“ mit „appearance“ oder „light“ und „dark“, nicht beides.")),
        (Some(colors), Some("light")) => (light_map, light_key) = (Some(colors), "colors"),
        (Some(colors), Some("dark")) => (dark_map, dark_key) = (Some(colors), "colors"),
        (Some(_), _) => errors.push(tr("Zu „colors“ gehört „appearance“: „light“ oder „dark“.")),
        (None, _) if light_map.is_none() && dark_map.is_none() => errors.push(tr("Es fehlen die Farben: „light“, „dark“ oder „colors“ mit „appearance“.")),
        (None, _) => {}
    }
    let light = light_map.and_then(|m| palette(m, light_key, false, &identity, &mut errors, &mut warnings));
    let dark = dark_map.and_then(|m| palette(m, dark_key, true, &identity, &mut errors, &mut warnings));
    if !errors.is_empty() {
        return Err(errors);
    }
    let Some(name) = name else {
        return Err(vec![tr("Es fehlt „name“, der Name des Themes.")]);
    };
    if light.is_none() && dark.is_none() {
        return Err(vec![tr("Es fehlen die Farben: „light“, „dark“ oder „colors“ mit „appearance“.")]);
    }
    Ok(Theme { id: id.to_string(), name, author, license, source, light, dark, radius, density, font, signature, warnings, origin, text: text.to_string() })
}

fn palette(value: &Value, key: &str, dark: bool, identity: &[Rgba], errors: &mut Vec<String>, warnings: &mut Vec<String>) -> Option<Palette> {
    let side = if dark { tr("Dunkel") } else { tr("Hell") };
    let Value::Object(map) = value else {
        errors.push(trf("„%@“ muss ein Objekt mit Farben sein.", &[&key]));
        return None;
    };
    let mut given: [Option<Rgba>; 24] = [None; 24];
    let mut ok = true;
    for (k, v) in map {
        let Some(token) = Token::ALL.iter().copied().find(|t| t.key() == k) else {
            warnings.push(trf("%@: Unbekannte Farbe „%@“ wird ignoriert.", &[&side, k]));
            continue;
        };
        match v.as_str().and_then(Rgba::parse) {
            Some(color) if color.a < 255 && !matches!(token, Token::Selection | Token::Scrim) => {
                warnings.push(trf("%@: „%@“ darf nicht durchsichtig sein und wird deckend gezeichnet.", &[&side, k]));
                given[token as usize] = Some(color.with_alpha(1.));
            }
            Some(color) => given[token as usize] = Some(color),
            None => {
                errors.push(trf("%@: „%@“ ist keine Farbe wie \"#1A2B3C\".", &[&side, k]));
                ok = false;
            }
        }
    }
    for token in Token::REQUIRED {
        if given[token as usize].is_none() {
            errors.push(trf("%@: Es fehlt „%@“.", &[&side, &token.key()]));
            ok = false;
        }
    }
    if !ok {
        return None;
    }
    let pick = |t: Token| given[t as usize].unwrap_or(Rgba::BLACK);
    let (surface, raised, sunken, text1, accent) = (pick(Token::Surface), pick(Token::Raised), pick(Token::Sunken), pick(Token::Text1), pick(Token::Accent));
    let derived = |t: Token| -> Rgba {
        match t {
            Token::Sidebar => surface.mix(sunken, 0.5),
            Token::Control => raised.mix(text1, 0.08),
            Token::Separator => surface.mix(text1, 0.14),
            Token::AccentText | Token::FocusRing => accent,
            Token::AttentionText => pick(Token::Attention),
            Token::DangerText => pick(Token::Danger),
            Token::SuccessText => pick(Token::Success),
            Token::OnAttention => pick(Token::Attention).ink(),
            Token::OnDanger => pick(Token::Danger).ink(),
            Token::TileRing => given[Token::FocusRing as usize].unwrap_or(accent),
            Token::Selection => accent.with_alpha(if dark { 0.16 } else { 0.12 }),
            Token::Scrim => Rgba::BLACK.with_alpha(if dark { 0.55 } else { 0.28 }),
            other => pick(other),
        }
    };
    let mut p = Palette { colors: [Rgba::BLACK; 24], identity: identity.to_vec() };
    for t in Token::ALL {
        p.set(t, given[t as usize].unwrap_or_else(|| derived(t)));
    }
    for (token, under, need) in RULES {
        let color = p.get(token);
        let backs: Vec<Rgba> = under.iter().map(|&b| p.get(b)).collect();
        let worst = |c: Rgba| backs.iter().map(|&b| c.contrast(b)).fold(f64::MAX, f64::min);
        let before = worst(color);
        if before >= need {
            continue;
        }
        let ink = if worst(Rgba::BLACK) >= worst(Rgba::WHITE) { Rgba::BLACK } else { Rgba::WHITE };
        let fixed = (1..=40).map(|i| color.mix(ink, i as f32 * 0.025)).find(|&c| worst(c) >= need).unwrap_or(ink);
        p.set(token, fixed);
        if given[token as usize].is_some() {
            warnings.push(trf("%@: „%@“ hat nur %@:1 Kontrast (nötig %@:1) und wurde zu %@ angepasst.", &[&side, &token.key(), &ratio_text(before), &ratio_text(need), &fixed.hex()]));
        }
    }
    Some(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r##"{
  "name": "Probe",
  "appearance": "dark",
  "colors": {
    "surface": "#101010", "raised": "#1A1A1A", "sunken": "#080808",
    "text-1": "#F0F0F0", "text-2": "#C0C0C0", "text-3": "#A0A0A0",
    "accent": "#4FA3FF", "on-accent": "#04121F",
    "attention": "#F2B04A", "danger": "#FF6B5E", "success": "#5BD68A"
  }
}"##;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("manycraft-themes-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap_or_default();
        dir
    }

    #[test]
    fn every_built_in_loads_without_warnings() {
        for (id, text) in BUILT_IN {
            match parse(id, text, Origin::BuiltIn) {
                Ok(theme) => assert!(theme.warnings.is_empty(), "{id}: {:?}", theme.warnings),
                Err(errors) => panic!("{id}: {errors:?}"),
            }
        }
    }

    #[test]
    fn every_built_in_names_its_licence() {
        for (id, text) in BUILT_IN {
            let theme = parse(id, text, Origin::BuiltIn).unwrap_or_else(|e| panic!("{id}: {e:?}"));
            assert!(theme.license.is_some() && theme.author.is_some(), "{id}");
        }
    }

    #[test]
    fn a_single_appearance_file_fills_the_rest() {
        let theme = parse("t", MINIMAL, Origin::BuiltIn).unwrap_or_else(|e| panic!("{e:?}"));
        assert!(theme.dark.is_some() && theme.light.is_none());
        let p = theme.palette(false);
        assert_eq!(p.get(Token::FocusRing), Rgba::rgb(0x4FA3FF));
        assert_eq!(p.get(Token::Scrim).a, 140);
        assert_eq!(p.identity.len(), 8);
        assert!(theme.warnings.is_empty(), "{:?}", theme.warnings);
    }

    #[test]
    fn low_contrast_is_fixed_per_token_with_a_warning() {
        let text = MINIMAL.replace("\"text-3\": \"#A0A0A0\"", "\"text-3\": \"#303030\"");
        let theme = parse("t", &text, Origin::BuiltIn).unwrap_or_else(|e| panic!("{e:?}"));
        let p = theme.palette(true);
        for back in [Token::Surface, Token::Sidebar, Token::Raised, Token::Sunken] {
            assert!(p.get(Token::Text3).contrast(p.get(back)) >= 4.5);
        }
        assert_eq!(p.get(Token::Text1), Rgba::rgb(0xF0F0F0));
        assert_eq!(theme.warnings.len(), 1, "{:?}", theme.warnings);
        assert!(theme.warnings[0].contains("text-3"));
    }

    #[test]
    fn a_byte_order_mark_and_see_through_text_are_handled() {
        let bom = format!("{}{MINIMAL}", char::from_u32(0xFEFF).unwrap_or(' '));
        assert!(parse("t", &bom, Origin::BuiltIn).is_ok_and(|t| t.warnings.is_empty()));
        let faint = MINIMAL.replace("\"text-1\": \"#F0F0F0\"", "\"text-1\": \"#F0F0F010\"");
        let theme = parse("t", &faint, Origin::BuiltIn).unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(theme.palette(true).get(Token::Text1).a, 255);
        assert!(theme.warnings.iter().any(|w| w.contains("text-1")), "{:?}", theme.warnings);
        let colors = MINIMAL.replace("\"surface\": \"#101010\"", "\"surface\": 5");
        assert!(parse("t", &colors, Origin::BuiltIn).is_err());
        let wrong = MINIMAL.replace("\"colors\": {", "\"colors\": 5, \"x\": {");
        assert!(parse("t", &wrong, Origin::BuiltIn).is_err_and(|e| e.iter().any(|m| m.contains("colors"))));
    }

    #[test]
    fn broken_files_say_what_is_wrong() {
        assert!(parse("t", "{ \"name\": ", Origin::BuiltIn).is_err_and(|e| e[0].contains("Zeile 1")));
        assert!(parse("t", "[]", Origin::BuiltIn).is_err());
        let no_name = MINIMAL.replace("\"name\": \"Probe\",", "");
        assert!(parse("t", &no_name, Origin::BuiltIn).is_err_and(|e| e.iter().any(|m| m.contains("name"))));
        let bad = MINIMAL.replace("#4FA3FF", "blau");
        assert!(parse("t", &bad, Origin::BuiltIn).is_err_and(|e| e.iter().any(|m| m.contains("accent"))));
        let missing = MINIMAL.replace("\"danger\": \"#FF6B5E\", ", "");
        assert!(parse("t", &missing, Origin::BuiltIn).is_err_and(|e| e.iter().any(|m| m.contains("danger"))));
        let both = MINIMAL.replace("\"appearance\": \"dark\",", "\"appearance\": \"dark\", \"light\": {},");
        assert!(parse("t", &both, Origin::BuiltIn).is_err());
    }

    #[test]
    fn unknown_keys_only_warn() {
        let text = MINIMAL.replace("\"name\": \"Probe\",", "\"name\": \"Probe\", \"farbe\": 1,").replace("\"surface\"", "\"glitzer\": \"#FFFFFF\", \"surface\"");
        let theme = parse("t", &text, Origin::BuiltIn).unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(theme.warnings.len(), 2, "{:?}", theme.warnings);
    }

    #[test]
    fn user_themes_load_after_built_ins_and_broken_ones_are_listed() {
        let dir = temp("load");
        std::fs::write(dir.join("eigen.json"), MINIMAL).unwrap_or_default();
        std::fs::write(dir.join("kaputt.json"), "{").unwrap_or_default();
        std::fs::write(dir.join("notiz.txt"), "x").unwrap_or_default();
        let library = Library::load(&dir);
        assert_eq!(library.themes.len(), BUILT_IN.len() + 1);
        assert_eq!(library.themes.last().map(|t| t.id.as_str()), Some("user/eigen"));
        assert_eq!(library.broken.len(), 1);
        assert_eq!(library.broken[0].file, "kaputt.json");
        assert_eq!(library.chosen(Some("weg")).map(|t| t.id.as_str()), Some(DEFAULT));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_copy_is_a_valid_theme_of_its_own() {
        let dir = temp("copy");
        let library = Library::load(&dir);
        let nord = library.get("nord").unwrap_or_else(|| panic!("nord"));
        let first = copy_as_template(nord, &dir).unwrap_or_else(|e| panic!("{e}"));
        let second = copy_as_template(nord, &dir).unwrap_or_else(|e| panic!("{e}"));
        assert_ne!(first, second);
        let reloaded = Library::load(&dir);
        let copy = reloaded.themes.iter().find(|t| t.origin == Origin::User(first.clone())).unwrap_or_else(|| panic!("copy"));
        assert!(copy.name.contains("Nord") && copy.name != "Nord");
        assert_eq!(copy.palette(true), nord.palette(true));
        let text = std::fs::read_to_string(&first).unwrap_or_default();
        assert!(text.find("\"name\"") < text.find("\"dark\""));
        assert!(text.find("\"surface\"") < text.find("\"text-1\""));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn colours_read_and_write() {
        assert_eq!(Rgba::parse("#1a2B3c"), Some(Rgba::rgb(0x1A2B3C)));
        assert_eq!(Rgba::parse("#00000080").map(|c| c.a), Some(128));
        assert_eq!(Rgba::parse("1A2B3C"), None);
        assert_eq!(Rgba::parse("#12345"), None);
        assert_eq!(Rgba::rgb(0x0B7A74).hex(), "#0B7A74");
        assert!((Rgba::BLACK.contrast(Rgba::WHITE) - 21.).abs() < 0.01);
    }
}
