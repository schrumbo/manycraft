//! The master switch and the four kinds of shared things, kept in the sync folder.

use super::*;

const FILE: &str = "state.json";

/// What the sync can share.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Options,
    Config,
    Saves,
    Screenshots,
}

/// Every kind, in the order the Sync page lists them.
pub const KINDS: [Kind; 4] = [Kind::Options, Kind::Config, Kind::Saves, Kind::Screenshots];

impl Kind {
    /// The file or folder name, in an instance and in the sync folder.
    pub(super) fn rel(self) -> &'static str {
        match self {
            Self::Options => OPTIONS_FILE,
            Self::Config => CONFIG,
            Self::Saves => "saves",
            Self::Screenshots => "screenshots",
        }
    }
}

/// Whether the sync is on, what it shares, and the instance the shared state came from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub on: bool,
    pub options: bool,
    pub config: bool,
    pub saves: bool,
    pub screenshots: bool,
    /// The instance whose files a kind taken up later is shared from.
    pub mother: Option<String>,
}

impl State {
    /// Whether `kind` is switched on, whether or not the sync as a whole is.
    pub fn get(&self, kind: Kind) -> bool {
        match kind {
            Kind::Options => self.options,
            Kind::Config => self.config,
            Kind::Saves => self.saves,
            Kind::Screenshots => self.screenshots,
        }
    }

    /// This state with `kind` switched on or off.
    pub fn with(mut self, kind: Kind, on: bool) -> Self {
        match kind {
            Kind::Options => self.options = on,
            Kind::Config => self.config = on,
            Kind::Saves => self.saves = on,
            Kind::Screenshots => self.screenshots = on,
        }
        self
    }
}

/// The state now; sync off when nothing was ever set up.
pub fn state() -> State {
    read(&Mother::current())
}

pub(crate) fn read(mother: &Mother) -> State {
    fs::read(mother.path(FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub(crate) fn exists(mother: &Mother) -> bool {
    mother.path(FILE).is_file()
}

pub(crate) fn write(mother: &Mother, state: &State) -> Result<()> {
    fs::create_dir_all(&mother.root).with_context(|| format!("cannot create {}", mother.root.display()))?;
    net::write_atomic(&mother.path(FILE), &serde_json::to_vec_pretty(state)?)
}

/// What one instance takes part in now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flags {
    pub options: bool,
    pub config: bool,
    pub saves: bool,
    pub screenshots: bool,
}

impl Flags {
    /// Whether the instance takes part in `kind`.
    pub fn get(self, kind: Kind) -> bool {
        match kind {
            Kind::Options => self.options,
            Kind::Config => self.config,
            Kind::Saves => self.saves,
            Kind::Screenshots => self.screenshots,
        }
    }

    pub(crate) fn of(state: &State, out: bool, game: &str) -> Self {
        if !state.on || out {
            return Self::default();
        }
        Self { options: state.options && options_travel(game), config: state.config, saves: state.saves, screenshots: state.screenshots }
    }
}

/// What the instance in folder `dir` takes part in, read from its file and the state; nothing when it cannot be read.
/// Game options only count for games whose options can travel, see [`options_travel`].
pub(crate) fn flags_in(mother: &Mother, dir: &Path) -> Flags {
    let state = read(mother);
    if !state.on {
        return Flags::default();
    }
    let Ok(text) = fs::read_to_string(dir.join(instance::FILE)) else { return Flags::default() };
    let Ok(file) = serde_json::from_str::<serde_json::Value>(&text) else { return Flags::default() };
    let out = file.get("sync_out").and_then(serde_json::Value::as_bool).unwrap_or(false);
    Flags::of(&state, out, file.get("game_version").and_then(serde_json::Value::as_str).unwrap_or_default())
}
