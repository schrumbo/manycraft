//! The memory one game of each batch was last measured to hold, kept between runs so planning starts from it.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;

use super::{net, paths};

const FILE: &str = "measured.json";

/// MB per game by batch id; empty when nothing was measured yet or the file cannot be read.
pub fn load() -> HashMap<String, u32> {
    load_from(&paths::data_dir())
}

/// Keeps `seen` for the next run.
pub fn save(seen: &HashMap<String, u32>) -> Result<()> {
    save_to(&paths::data_dir(), seen)
}

fn load_from(dir: &Path) -> HashMap<String, u32> {
    std::fs::read(dir.join(FILE)).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
}

fn save_to(dir: &Path, seen: &HashMap<String, u32>) -> Result<()> {
    net::write_atomic(&dir.join(FILE), &serde_json::to_vec_pretty(seen)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_was_measured_comes_back_and_a_damaged_file_counts_as_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_from(dir.path()).is_empty());
        let seen = HashMap::from([("b".to_owned(), 1450)]);
        save_to(dir.path(), &seen).unwrap();
        assert_eq!(load_from(dir.path()), seen);
        std::fs::write(dir.path().join(FILE), b"{oops").unwrap();
        assert!(load_from(dir.path()).is_empty());
    }
}
