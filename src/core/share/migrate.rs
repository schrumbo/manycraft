//! Takes the sync of beta.10 and beta.11 over: instances that shared something keep taking part, the rest stay out.

use super::*;

/// What one instance shared before, kept in its file under `sync`.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(default)]
struct Legacy {
    options: bool,
    config: bool,
    servers: bool,
}

impl Legacy {
    fn any(self) -> bool {
        self.options || self.config || self.servers
    }
}

const OBSOLETE: [&str; 3] = ["history", "setups.json", "notes.json"];
const SERVERS_FILE: &str = super::super::sync::SERVERS_FILE;

/// Takes over the old settings once: with any instance sharing, the sync is on with the kinds that were on, the shared
/// files stay the shared state, instances that shared nothing stay out, and an instance that shared its server list
/// gets a real one. Old history and notes go. Does nothing once a state exists.
pub fn migrate() -> Result<()> {
    migrate_in(&Mother::current(), &instance::instances_dir()).map(|_| ())
}

pub(crate) fn migrate_in(mother: &Mother, root: &Path) -> Result<bool> {
    if state::exists(mother) {
        return Ok(false);
    }
    for name in OBSOLETE {
        let path = mother.path(name);
        let _ = if path.is_dir() { fs::remove_dir_all(&path) } else { fs::remove_file(&path) };
    }
    let all = instance::load_all_in(root)?;
    let legacy = |i: &Instance| i.kept.get("sync").cloned().and_then(|v| serde_json::from_value::<Legacy>(v).ok()).unwrap_or_default();
    let on: Vec<Legacy> = all.iter().map(legacy).filter(|l| l.any()).collect();
    if on.is_empty() {
        return Ok(false);
    }
    let mother_id = all.iter().find(|i| legacy(i).any()).map(|i| i.id.clone());
    state::write(mother, &State { on: true, options: on.iter().any(|l| l.options), config: on.iter().any(|l| l.config), saves: false, screenshots: false, mother: mother_id })?;
    for mut made in all {
        let before = legacy(&made);
        let dir = instance::dir_in(root, &made.id);
        if before.servers {
            let target = dir.join(SERVERS_FILE);
            if fs::symlink_metadata(&target).is_err()
                && let Some(bytes) = read_file(&mother.path(SERVERS_FILE))?
            {
                net::write_atomic(&target, &bytes)?;
            }
        }
        made.sync_out = !before.any();
        made.kept.remove("sync");
        instance::save_in(root, &made)?;
    }
    let _ = fs::remove_file(mother.path(SERVERS_FILE));
    Ok(true)
}
