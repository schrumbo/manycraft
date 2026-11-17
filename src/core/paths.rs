use std::path::PathBuf;

/// Root folder holding instances, accounts and shared game files. Local, never roaming.
/// `MANYCRAFT_HOME` replaces it, so driven and test instances stay out of the real data.
pub fn data_dir() -> PathBuf {
    if let Some(dir) = home_override() {
        return dir;
    }
    match dirs::data_local_dir() {
        Some(dir) => dir.join(super::channel::data_folder()),
        None => panic!("no local data folder: set HOME (Linux, macOS) or LOCALAPPDATA (Windows)"),
    }
}

/// The folder `MANYCRAFT_HOME` names; an empty value counts as unset.
pub fn home_override() -> Option<PathBuf> {
    std::env::var_os("MANYCRAFT_HOME").filter(|d| !d.is_empty()).map(PathBuf::from)
}

/// Folder holding one subfolder per instance.
pub fn instances_dir() -> PathBuf {
    data_dir().join("instances")
}
