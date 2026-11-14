//! Keeps stopped tiles recognisable: saves the last frame of every instance and loads saved frames and skin faces, all off the UI thread.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use gpui_kit::*;
use uuid::Uuid;

use super::state::{Update, render_image};
use crate::core::account::Account;
use crate::core::{skin, still};

const EVERY: Duration = Duration::from_secs(60);
const FACE: u32 = 96;

enum Job {
    Save(String, Arc<RenderImage>),
    Load(String),
}

/// The worker that writes and reads stills, and what it was asked already.
pub struct Stills {
    jobs: mpsc::Sender<Job>,
    out: async_channel::Sender<Update>,
    loaded: HashSet<String>,
    seeking: Arc<Mutex<HashSet<Uuid>>>,
    saved_at: HashMap<String, Instant>,
}

impl Global for Stills {}

/// Starts the worker; loaded stills and faces arrive as updates on `out`.
pub fn init(out: async_channel::Sender<Update>, cx: &mut App) {
    let (jobs, wanted) = mpsc::channel::<Job>();
    let results = out.clone();
    let worker = std::thread::Builder::new().name("stills".into()).spawn(move || {
        while let Ok(job) = wanted.recv() {
            match job {
                Job::Save(key, image) => {
                    let size = image.size(0);
                    if let Some(bgra) = image.as_bytes(0) {
                        let _ = still::save(&key, size.width.0 as u32, size.height.0 as u32, bgra);
                    }
                }
                Job::Load(key) => {
                    let Some(image) = still::load(&key).and_then(|(w, h, rgba)| render_image(w, h, rgba)) else { continue };
                    if results.send_blocking(Update::Still { key, image }).is_err() {
                        return;
                    }
                }
            }
        }
    });
    if worker.is_ok() {
        cx.set_global(Stills { jobs, out, loaded: HashSet::new(), seeking: Arc::default(), saved_at: HashMap::new() });
    }
}

/// Keeps `image`, a live frame of `key`, as its still: once a minute while it runs, and at once when `last` is set.
pub fn save(key: &str, image: &Arc<RenderImage>, last: bool, cx: &mut App) {
    if !cx.has_global::<Stills>() {
        return;
    }
    let s = cx.global_mut::<Stills>();
    let due = match s.saved_at.get(key) {
        _ if last => true,
        Some(at) => at.elapsed() >= EVERY,
        None => {
            s.saved_at.insert(key.to_owned(), Instant::now());
            false
        }
    };
    if last {
        s.saved_at.remove(key);
    } else if due {
        s.saved_at.insert(key.to_owned(), Instant::now());
    }
    if due {
        let _ = s.jobs.send(Job::Save(key.to_owned(), Arc::clone(image)));
    }
}

/// Loads the saved stills of `keys` that were not asked for before, and the faces of `accounts` not being sought already.
pub fn load(keys: Vec<String>, accounts: Vec<Account>, cx: &mut App) {
    if !cx.has_global::<Stills>() {
        return;
    }
    let s = cx.global_mut::<Stills>();
    for key in keys {
        if s.loaded.insert(key.clone()) {
            let _ = s.jobs.send(Job::Load(key));
        }
    }
    let accounts: Vec<Account> = {
        let mut seeking = s.seeking.lock().unwrap_or_else(|e| e.into_inner());
        accounts.into_iter().filter(|a| seeking.insert(a.id)).collect()
    };
    if accounts.is_empty() {
        return;
    }
    let (out, seeking, ids) = (s.out.clone(), Arc::clone(&s.seeking), accounts.iter().map(|a| a.id).collect::<Vec<_>>());
    let spawned = std::thread::Builder::new().name("faces".into()).spawn(move || {
        skin::faces(&accounts, |account, face| match render_image(FACE, FACE, upscale(&face)) {
            Some(image) => out.send_blocking(Update::Face { account, image }).is_ok(),
            None => true,
        });
        let mut seeking = seeking.lock().unwrap_or_else(|e| e.into_inner());
        for account in &accounts {
            seeking.remove(&account.id);
        }
    });
    if spawned.is_err() {
        let mut seeking = s.seeking.lock().unwrap_or_else(|e| e.into_inner());
        for id in ids {
            seeking.remove(&id);
        }
    }
}

/// Shows `face`, 8 × 8 RGBA pixels, as the tile face of `account` right away, after its skin changed.
pub fn show_face(account: Uuid, face: &[u8], cx: &mut App) {
    if let Some(s) = cx.try_global::<Stills>()
        && let Some(image) = render_image(FACE, FACE, upscale(face))
    {
        let _ = s.out.try_send(Update::Face { account, image });
    }
}

/// Forgets which stills were loaded for instances that are gone, so they load again if the instance comes back.
pub fn forget_except(keep: &dyn Fn(&str) -> bool, cx: &mut App) {
    if cx.has_global::<Stills>() {
        cx.global_mut::<Stills>().loaded.retain(|k| keep(k));
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
