//! What sharing did on its own that a person should hear about: shared mod settings a start broke that came back.

use std::fs;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::Mother;
use crate::core::net;

const FILE: &str = "notes.json";
const KEEP: usize = 20;

/// Shared mod settings a game's start deleted, emptied or broke, which got back what they held before.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    /// When they came back, in milliseconds since 1970 (UTC).
    pub at: i64,
    /// The name of the instance whose start broke them.
    pub instance: String,
    /// The files, by path inside the `config` folder.
    pub files: Vec<String>,
}

pub(super) fn add(mother: &Mother, instance: &str, files: Vec<String>) -> Result<()> {
    let mut all = read(mother);
    all.insert(0, Note { at: chrono::Utc::now().timestamp_millis(), instance: instance.to_owned(), files });
    all.truncate(KEEP);
    net::write_atomic(&mother.path(FILE), &serde_json::to_vec_pretty(&all)?)
}

/// The notes, newest first; none when the file is missing or cannot be read.
pub(super) fn read(mother: &Mother) -> Vec<Note> {
    fs::read(mother.path(FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub(super) fn clear(mother: &Mother) -> Result<()> {
    match fs::remove_file(mother.path(FILE)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}
