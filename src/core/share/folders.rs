//! The folders the sync links: one physical copy in the sync folder, a link in every instance that takes part.

use super::*;

/// Makes the folder `path` a link to `shared` when `on` and a real folder holding a copy of it when not. A real folder
/// that stands in the way moves aside to `<name>.before-sync` first; one that is a real folder already while off stays.
pub(super) fn settle(path: &Path, shared: &Path, on: bool) -> Result<()> {
    let linked = is_link(path) && points_at(path, shared);
    match (on, linked) {
        (true, false) => join(path, shared),
        (false, true) => leave(path, shared),
        (false, false) if fs::symlink_metadata(path).is_err() => recover(path),
        _ => Ok(()),
    }
}

fn join(path: &Path, shared: &Path) -> Result<()> {
    fs::create_dir_all(shared).with_context(|| format!("cannot create {}", shared.display()))?;
    let aside = if is_link(path) {
        super::super::fleet::remove_link(path)?;
        None
    } else if fs::symlink_metadata(path).is_ok() {
        let free = first_free(path, "before-sync")?;
        fs::rename(path, &free).with_context(|| tr("Ein Ordner der Instanz ist gerade in Benutzung. Schließ das Spiel und versuch es noch einmal."))?;
        Some(free)
    } else {
        None
    };
    if let Err(e) = super::super::fleet::create_link(shared, path) {
        if let Some(free) = &aside {
            let _ = fs::rename(free, path);
        }
        return Err(e).with_context(|| format!("cannot link {} to {}", path.display(), shared.display()));
    }
    Ok(())
}

fn leave(path: &Path, shared: &Path) -> Result<()> {
    let fresh = first_free(path, "own-new")?;
    if let Err(e) = copy_tree(shared, &fresh) {
        let _ = fs::remove_dir_all(&fresh);
        return Err(e);
    }
    super::super::fleet::remove_link(path)?;
    fs::rename(&fresh, path).with_context(|| format!("cannot move {} to {}", fresh.display(), path.display()))
}

fn recover(path: &Path) -> Result<()> {
    let fresh = path.with_file_name(format!("{}.own-new", path.file_name().context("path has no name")?.to_string_lossy()));
    if fs::symlink_metadata(&fresh).is_ok() {
        fs::rename(&fresh, path).with_context(|| format!("cannot move {} to {}", fresh.display(), path.display()))?;
    }
    Ok(())
}

/// `<name>.<suffix>` next to `path`, or the first `<name>.<suffix>-N` nothing is at.
pub(super) fn first_free(path: &Path, suffix: &str) -> Result<PathBuf> {
    let name = path.file_name().context("path has no name")?.to_string_lossy().into_owned();
    std::iter::once(format!("{name}.{suffix}"))
        .chain((2..).map(|n| format!("{name}.{suffix}-{n}")))
        .map(|n| path.with_file_name(n))
        .find(|p| fs::symlink_metadata(p).is_err())
        .context("no free name")
}

/// Copies every file and folder of `from` to `to`; links inside are not followed.
pub(super) fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to).with_context(|| format!("cannot create {}", to.display()))?;
    for entry in fs::read_dir(from).with_context(|| format!("cannot read {}", from.display()))? {
        let entry = entry.with_context(|| format!("cannot read {}", from.display()))?;
        let kind = entry.file_type().with_context(|| format!("cannot read {}", entry.path().display()))?;
        let dest = to.join(entry.file_name());
        if kind.is_dir() && !is_link(&entry.path()) {
            copy_tree(&entry.path(), &dest)?;
        } else if kind.is_file() && entry.file_name() != "session.lock" {
            fs::copy(entry.path(), &dest).with_context(|| format!("cannot copy {}", entry.path().display()))?;
        }
    }
    Ok(())
}

/// Moves what is in the real folder `from` into `to`, but what `to` has by that name already, which stays.
pub(super) fn move_children(from: &Path, to: &Path) -> Result<()> {
    if is_link(from) || !from.is_dir() {
        return Ok(());
    }
    fs::create_dir_all(to).with_context(|| format!("cannot create {}", to.display()))?;
    for entry in fs::read_dir(from).with_context(|| format!("cannot read {}", from.display()))? {
        let entry = entry.with_context(|| format!("cannot read {}", from.display()))?;
        let dest = to.join(entry.file_name());
        if fs::symlink_metadata(&dest).is_err() {
            fs::rename(entry.path(), &dest).with_context(|| tr("Ein Ordner der Instanz ist gerade in Benutzung. Schließ das Spiel und versuch es noch einmal."))?;
        }
    }
    Ok(())
}

/// The game folders of the instance in `dir`, one per account that played.
pub(super) fn game_dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(instance::games_dir(dir)) else { return Vec::new() };
    entries.filter_map(Result::ok).filter(|e| e.file_type().is_ok_and(|t| t.is_dir())).map(|e| e.path()).collect()
}

pub(super) fn points_at(link: &Path, target: &Path) -> bool {
    matches!((fs::canonicalize(link), fs::canonicalize(target)), (Ok(a), Ok(b)) if a == b)
}

#[cfg(windows)]
pub(super) fn is_link(path: &Path) -> bool {
    junction::exists(path).unwrap_or(false) || fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

#[cfg(not(windows))]
pub(super) fn is_link(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}
