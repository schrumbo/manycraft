//! Keeps stopped tiles recognisable: saves the last frame of every game and loads saved frames and skin faces, all off
//! the UI thread.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use freya::elements::image::ImageHandle;
use uuid::Uuid;

use super::store::Raw;
use crate::core::account::Account;
use crate::core::{skin, still};
use crate::runner::{Pixels, Update};

const EVERY: Duration = Duration::from_secs(60);
const FACE: u32 = 96;

enum Job {
    Save(String, u32, u32, freya::prelude::Bytes),
    Load(String),
}

/// The worker that writes and reads stills, and what it was asked already.
pub struct Stills {
    jobs: mpsc::Sender<Job>,
    out: async_channel::Sender<Update<Raw>>,
    loaded: HashSet<String>,
    seeking: Arc<Mutex<HashSet<Uuid>>>,
    saved_at: HashMap<String, Instant>,
}

impl Stills {
    /// Starts the worker; loaded stills and faces arrive as updates on `out`.
    pub fn start(out: async_channel::Sender<Update<Raw>>) -> Option<Self> {
        let (jobs, wanted) = mpsc::channel::<Job>();
        let results = out.clone();
        std::thread::Builder::new()
            .name("stills".into())
            .spawn(move || {
                while let Ok(job) = wanted.recv() {
                    match job {
                        Job::Save(key, width, height, rgba) => {
                            let bgra: Vec<u8> = rgba.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], p[3]]).collect();
                            if let Err(e) = still::save(&key, width, height, &bgra) {
                                eprintln!("still: {e:#}");
                            }
                        }
                        Job::Load(key) => {
                            let Some(image) = still::load(&key).and_then(|(w, h, rgba)| Raw::from_rgba(w, h, rgba)) else { continue };
                            if results.send_blocking(Update::Still { key, image }).is_err() {
                                return;
                            }
                        }
                    }
                }
            })
            .ok()?;
        Some(Self { jobs, out, loaded: HashSet::new(), seeking: Arc::default(), saved_at: HashMap::new() })
    }

    /// Keeps `image`, a live frame of `key`, as its still: once a minute while it runs, and at once when `last` is set.
    pub fn save(&mut self, key: &str, image: &ImageHandle, last: bool) {
        let due = match self.saved_at.get(key) {
            _ if last => true,
            Some(at) => at.elapsed() >= EVERY,
            None => {
                self.saved_at.insert(key.to_owned(), Instant::now());
                false
            }
        };
        if last {
            self.saved_at.remove(key);
        } else if due {
            self.saved_at.insert(key.to_owned(), Instant::now());
        }
        let (Ok(width), Ok(height)) = (u32::try_from(image.image.width()), u32::try_from(image.image.height())) else { return };
        if due {
            let _ = self.jobs.send(Job::Save(key.to_owned(), width, height, image.bytes.clone()));
        }
    }

    /// Loads the saved stills of `keys` that were not asked for before, and the faces of `accounts` not being sought already.
    pub fn load(&mut self, keys: Vec<String>, accounts: Vec<Account>) {
        for key in keys {
            if self.loaded.insert(key.clone()) {
                let _ = self.jobs.send(Job::Load(key));
            }
        }
        let accounts: Vec<Account> = {
            let mut seeking = self.seeking.lock().unwrap_or_else(|e| e.into_inner());
            accounts.into_iter().filter(|a| seeking.insert(a.id)).collect()
        };
        if accounts.is_empty() {
            return;
        }
        let (out, seeking, ids) = (self.out.clone(), Arc::clone(&self.seeking), accounts.iter().map(|a| a.id).collect::<Vec<_>>());
        let spawned = std::thread::Builder::new().name("faces".into()).spawn(move || {
            skin::faces(&accounts, |account, face| match Raw::from_rgba(FACE, FACE, upscale(&face)) {
                Some(image) => out.send_blocking(Update::Face { account, image }).is_ok(),
                None => true,
            });
            let mut seeking = seeking.lock().unwrap_or_else(|e| e.into_inner());
            for account in &accounts {
                seeking.remove(&account.id);
            }
        });
        if spawned.is_err() {
            let mut seeking = self.seeking.lock().unwrap_or_else(|e| e.into_inner());
            for id in ids {
                seeking.remove(&id);
            }
        }
    }

    /// Forgets which stills were loaded for games that are gone, so they load again if the game comes back.
    pub fn forget_except(&mut self, keep: &dyn Fn(&str) -> bool) {
        self.loaded.retain(|k| keep(k));
        self.saved_at.retain(|k, _| keep(k));
    }
}

fn upscale(face: &[u8]) -> Vec<u8> {
    let (side, scale) = (skin::SIDE as usize, (FACE / skin::SIDE) as usize);
    let mut big = Vec::with_capacity(FACE as usize * FACE as usize * 4);
    for y in 0..FACE as usize {
        for x in 0..FACE as usize {
            let at = ((y / scale) * side + x / scale) * 4;
            big.extend_from_slice(&face[at..at + 4]);
        }
    }
    big
}
