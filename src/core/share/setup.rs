//! Setting sharing up in one go: the state of one instance becomes the shared one, chosen instances take it over, and the
//! last setup can be taken back. Nothing is deleted: what a setup replaces is kept in the history first.

use super::*;

const FILE: &str = "setups.json";
const RECORDS: usize = 5;

/// What a setup asks for.
#[derive(Clone, Debug)]
pub struct Setup {
    /// The instance whose state becomes the shared one.
    pub base: String,
    /// What is shared from now on.
    pub items: Sharing,
    /// The instances that start sharing now, besides the base; their own copies go to the history.
    pub replace: Vec<String>,
}

/// What a setup did.
#[derive(Clone, Debug)]
pub struct Done {
    /// The instances it changed, as saved.
    pub instances: Vec<Instance>,
    /// Items the base had nothing of, so the shared state stays as it was.
    pub nothing: Vec<Item>,
}

/// The last setup, for the line that takes it back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    /// When it ran, in milliseconds since 1970 (UTC).
    pub at: i64,
    pub base: String,
    pub items: Sharing,
    /// How many instances it changed, the base included.
    pub instances: usize,
    /// Whether it was taken back.
    pub undone: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Target {
    id: String,
    before: Sharing,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Record {
    at: i64,
    base: String,
    base_name: String,
    items: Sharing,
    targets: Vec<Target>,
    #[serde(default)]
    undone: bool,
}

pub(super) fn records(mother: &Mother) -> Vec<Record> {
    fs::read(mother.path(FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn write_records(mother: &Mother, all: &[Record]) -> Result<()> {
    net::write_atomic(&mother.path(FILE), &serde_json::to_vec_pretty(all)?)
}

/// The last setup, `None` when there was none.
pub fn last_setup() -> Option<Summary> {
    last_in(&Mother::current())
}

pub(crate) fn last_in(mother: &Mother) -> Option<Summary> {
    let r = records(mother).into_iter().next()?;
    Some(Summary { at: r.at, base: r.base_name, items: r.items, instances: r.targets.len(), undone: r.undone })
}

/// Makes the state of the instance `request.base` the shared one for the chosen items and lets the base and
/// `request.replace` share them. Same-named settings of the base replace the shared ones and everything else stays;
/// the server list is replaced whole. An item the base already shares is its own state and stays. `running` names
/// instances with a game; their `config` folder changes at the next start.
pub fn setup(request: &Setup, running: &[String]) -> Result<Done> {
    setup_in(&Mother::current(), &instance::instances_dir(), request, running)
}

pub(crate) fn setup_in(mother: &Mother, root: &Path, request: &Setup, running: &[String]) -> Result<Done> {
    let all = instance::load_all_in(root)?;
    let base = all.iter().find(|i| i.id == request.base).cloned().with_context(|| tr("Diese Instanz gibt es nicht mehr."))?;
    let items: Vec<Item> = ITEMS.into_iter().filter(|i| request.items.get(*i)).collect();
    if items.is_empty() {
        bail!(tr("Wähle aus, was synchronisiert werden soll."));
    }
    if items.contains(&Item::Options) && !travels(&base, Item::Options) {
        bail!(tr("Die Spiel-Optionen dieser Instanz lassen sich nicht synchronisieren, denn sie ist älter als Minecraft 1.13."));
    }
    let mut targets: Vec<Instance> = vec![base.clone()];
    for id in &request.replace {
        if let Some(found) = all.iter().find(|i| &i.id == id)
            && !targets.iter().any(|t| t.id == found.id)
        {
            targets.push(found.clone());
        }
    }
    let own = Own { id: base.id.clone(), name: base.name.clone(), dir: instance::dir_in(root, &base.id) };
    let (at, nothing) = crate::core::sync::with_templates(|| {
        let _held = mother.lock()?;
        kept(mother, |history| {
            let nothing = adopt(mother, &own, base.sync, &items, history)?;
            Ok((history.stamp(), nothing))
        })
    })?;
    let mut log = records(mother);
    log.insert(0, Record { at, base: base.id.clone(), base_name: base.name.clone(), items: request.items, targets: targets.iter().map(|t| Target { id: t.id.clone(), before: t.sync }).collect(), undone: false });
    log.truncate(RECORDS);
    write_records(mother, &log)?;
    let mut done = Vec::new();
    let mut failed = false;
    for mut target in targets {
        let own = Own { id: target.id.clone(), name: target.name.clone(), dir: instance::dir_in(root, &target.id) };
        for item in &items {
            if travels(&target, *item) {
                target.sync = target.sync.with(*item, true);
            }
        }
        done.push(save(root, target)?);
        if failed {
            continue;
        }
        if let Err(e) = join_in(mother, &own, running.contains(&own.id), true) {
            eprintln!("{} switches its shared files at the next start: {e:#}", own.name);
            failed = true;
        }
    }
    Ok(Done { instances: done, nothing })
}

/// Writes the state `own` has of `items` into the sync folder, each replaced version kept, and returns the items it had
/// nothing of. An item `flags` says it shares is its state already.
fn adopt(mother: &Mother, own: &Own, flags: Sharing, items: &[Item], history: &mut History) -> Result<Vec<Item>> {
    let mut nothing = Vec::new();
    for item in items {
        if flags.get(*item) {
            history.based(*item, own, false);
            continue;
        }
        let files: Vec<(String, Vec<u8>)> = match item {
            Item::Options => match read_text(&own.file(OPTIONS_FILE))? {
                Some(text) if keys::readable(&text) && !keys::legacy(&text) => {
                    let shared = keys::shared(&text);
                    if shared.is_empty() {
                        Vec::new()
                    } else {
                        let theirs = read_text(&mother.path(OPTIONS_FILE))?.filter(|t| keys::readable(t));
                        vec![(OPTIONS_FILE.to_owned(), theirs.map_or(shared.clone(), |t| keys::merge(&t, &shared)).into_bytes())]
                    }
                }
                Some(_) => bail!(tr("Die Spiel-Optionen dieser Instanz lassen sich nicht lesen.")),
                None => Vec::new(),
            },
            Item::Servers => read_file(&own.file(SERVERS_FILE))?.filter(|b| !b.is_empty()).map(|b| (SERVERS_FILE.to_owned(), b)).into_iter().collect(),
            Item::Config if is_link(&own.file(CONFIG)) => Vec::new(),
            Item::Config => own_files(own, Item::Config)?,
        };
        if files.is_empty() {
            nothing.push(*item);
            continue;
        }
        history.based(*item, own, false);
        put_locked(mother, history, own, files)?;
    }
    Ok(nothing)
}

/// Takes back the last setup: the shared states it replaced come back, and each instance it changed gets its own files
/// from before and its old choices again. Settings written to the shared files since stay in the history.
pub fn undo_setup(running: &[String]) -> Result<Vec<Instance>> {
    undo_in(&Mother::current(), &instance::instances_dir(), running)
}

pub(crate) fn undo_in(mother: &Mother, root: &Path, running: &[String]) -> Result<Vec<Instance>> {
    let mut log = records(mother);
    let Some(record) = log.first().filter(|r| !r.undone).cloned() else { bail!(tr("Es gibt nichts rückgängig zu machen.")) };
    for item in ITEMS.into_iter().filter(|i| record.items.get(*i)) {
        for version in history::replaced_at(mother, item, record.at)? {
            restore_in(mother, &version)?;
        }
    }
    let mut out = Vec::new();
    for target in &record.targets {
        let Ok(made) = find(root, &target.id) else { continue };
        let own = Own { id: made.id.clone(), name: made.name.clone(), dir: instance::dir_in(root, &made.id) };
        let turned: Vec<Item> = ITEMS.into_iter().filter(|i| record.items.get(*i) && !target.before.get(*i) && made.sync.get(*i) && travels(&made, *i)).collect();
        let mut next = made.sync;
        for item in &turned {
            next = next.with(*item, false);
        }
        let saved = set_in(mother, root, &made.id, next, running.contains(&made.id))?;
        give_back(mother, &own, &turned)?;
        out.push(saved);
    }
    log[0].undone = true;
    write_records(mother, &log)?;
    Ok(out)
}

/// Writes the newest own copy of each of `items` the history kept for `own` back into its folder; a `config` folder
/// that is still a link stays, since a write there would reach the shared files.
fn give_back(mother: &Mother, own: &Own, items: &[Item]) -> Result<()> {
    for item in items {
        let files = history::own_latest(mother, &own.id, *item)?;
        if *item == Item::Config && is_link(&own.file(CONFIG)) {
            continue;
        }
        for (rel, bytes) in files {
            net::write_atomic(&own.file(&rel), &bytes)?;
        }
    }
    Ok(())
}

/// Where a shared item stands: who it came from and who changed it last, each with the time in milliseconds since 1970
/// (UTC), and whether the sync folder holds it at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Standing {
    pub item: Item,
    pub base: Option<(String, i64)>,
    pub changed: Option<(String, i64)>,
    pub present: bool,
}

/// How each item stands, in the order of [`ITEMS`].
pub fn standing() -> Vec<Standing> {
    standing_in(&Mother::current())
}

pub(crate) fn standing_in(mother: &Mother) -> Vec<Standing> {
    ITEMS
        .into_iter()
        .map(|item| {
            let (base, changed) = history::origin(mother, item).unwrap_or_default();
            let present = match item {
                Item::Config => files_in(&mother.path(CONFIG)).is_ok_and(|f| !f.is_empty()),
                _ => mother.path(item.rel()).is_file(),
            };
            Standing { item, base, changed, present }
        })
        .collect()
}

/// What a new instance shares: everything, unless the person turned that off.
pub fn for_new() -> Sharing {
    if crate::core::settings::active().sync_new { Sharing::ALL } else { Sharing::NONE }
}
