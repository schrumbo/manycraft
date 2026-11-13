use std::path::PathBuf;

/// Root folder holding instances, accounts and shared game files. Local, never roaming.
pub fn data_dir() -> PathBuf {
    match dirs::data_local_dir() {
        Some(dir) => dir.join("Manycraft"),
        None => panic!("no local data folder: set HOME (Linux, macOS) or LOCALAPPDATA (Windows)"),
    }
}

/// Folder holding one subfolder per instance.
pub fn instances_dir() -> PathBuf {
    data_dir().join("instances")
}
