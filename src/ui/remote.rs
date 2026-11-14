//! Blocking network and disk work for the account and batch sheets, run off the UI thread.
//! A driven instance never touches the keychain or the network: it gets canned answers instead.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use uuid::Uuid;

use crate::core::account::{self, Account, AccountKind};
use crate::core::auth::{self, AuthError, WrongAccount};
use crate::core::batch::{self, Background, Batch, ModRef};
use crate::core::modrinth::{self, Defaults, Hit, Update};
use crate::core::prism::{self, AccountImport, Instance, PrismAccount};
use crate::core::{drive, fleet, install};

use super::forms::{self, Plain, Problem};

const PROXY_TEST_URL: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
const PROXY_TEST_TIMEOUT: Duration = Duration::from_secs(10);
const CANNED_DELAY: Duration = Duration::from_millis(450);

static SAVING: Mutex<()> = Mutex::new(());

fn saving() -> MutexGuard<'static, ()> {
    SAVING.lock().unwrap_or_else(|e| e.into_inner())
}

fn is_stale(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.to_string().contains("changed by another Manycraft"))
}

/// Runs a load-modify-save under the saving lock. When another window saved in between, `op` runs once more from a fresh load.
fn saved<T>(mut op: impl FnMut() -> Result<T>) -> Result<T> {
    let _saving = saving();
    match op() {
        Err(e) if is_stale(&e) => op(),
        done => done,
    }
}

fn gone(what: String) -> anyhow::Error {
    Plain(what).into()
}

/// Steps of a Microsoft sign-in as the sheet shows them.
pub enum SignIn {
    Code { user_code: String, uri: String, expires_in: u64 },
    Approved,
    /// Signed in; carries the account name as Minecraft knows it.
    Done(String),
    Failed(SignInError),
}

/// Why a sign-in stopped, sorted by what the person can do about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignInError {
    Expired,
    Declined,
    NoClientId,
    NotOwned,
    WrongAccount(String),
    Network,
    Other(Problem),
}

/// Runs a device-code sign-in through `proxy` and sends each step to `tx`. Once `cancel` is set nothing more is
/// stored or sent. With `expected` only that account may sign in.
pub fn sign_in(proxy: Option<String>, expected: Option<Uuid>, cancel: Arc<AtomicBool>, tx: async_channel::Sender<SignIn>) {
    let send = |step: SignIn| {
        if !cancel.load(Ordering::SeqCst) {
            let _ = tx.send_blocking(step);
        }
    };
    if drive::on() {
        return canned_sign_in(proxy, expected, &cancel, send);
    }
    let run = || -> Result<Account> {
        let flow = auth::Flow::new(proxy.as_deref())?;
        let code = flow.start()?;
        send(SignIn::Code { user_code: code.user_code.clone(), uri: code.verification_uri.clone(), expires_in: code.expires_in });
        let tokens = flow.poll_cancellable(&code, &cancel)?;
        if cancel.load(Ordering::SeqCst) {
            bail!("cancelled");
        }
        send(SignIn::Approved);
        let replace = if expected.is_some() { None } else { proxy.as_deref() };
        let mut held = None;
        auth::save_sign_in(&flow, tokens, replace, expected, &mut || {
            held = Some(saving());
            !cancel.load(Ordering::SeqCst)
        })
    };
    match run() {
        Ok(account) => send(SignIn::Done(account.name)),
        Err(e) => {
            if cancel.load(Ordering::SeqCst) {
                drive::note("sign-in stopped".into());
            }
            send(SignIn::Failed(classify(&e)));
        }
    }
}

fn classify(e: &anyhow::Error) -> SignInError {
    if let Some(auth) = e.downcast_ref::<AuthError>() {
        return match auth {
            AuthError::CodeExpired => SignInError::Expired,
            AuthError::Declined => SignInError::Declined,
            AuthError::InvalidClientId | AuthError::AppNotApproved => SignInError::NoClientId,
            AuthError::GameNotOwned => SignInError::NotOwned,
            other => SignInError::Other(Problem::plain(auth_text(other))),
        };
    }
    if let Some(WrongAccount(name)) = e.downcast_ref::<WrongAccount>() {
        return SignInError::WrongAccount(name.clone());
    }
    if is_network(e) {
        return SignInError::Network;
    }
    SignInError::Other(forms::explain(e))
}

pub(super) fn is_network(e: &anyhow::Error) -> bool {
    e.chain().any(|c| match c.downcast_ref::<ureq::Error>() {
        Some(ureq::Error::StatusCode(code)) => *code >= 500 || *code == 429,
        Some(_) => true,
        None => c.downcast_ref::<std::io::Error>().is_some(),
    })
}

fn auth_text(e: &AuthError) -> String {
    use crate::core::i18n::{tr, trf};
    match e {
        AuthError::SignInAgain => tr("Die gespeicherte Anmeldung gilt nicht mehr. Melde dich erneut an."),
        AuthError::NoXboxProfile => tr("Dieses Konto hat noch kein Xbox-Profil. Melde dich einmal auf xbox.com an, dann klappt es."),
        AuthError::RegionBlocked => tr("Xbox Live ist im Land dieses Kontos nicht verfügbar."),
        AuthError::AdultVerification => tr("Dieses Konto muss erst bei Microsoft das Alter bestätigen."),
        AuthError::ChildAccount => tr("Das ist ein Kinderkonto. Ein Erwachsener muss es erst zu einer Microsoft-Familie hinzufügen."),
        AuthError::XboxRejected(code) => trf("Xbox Live hat die Anmeldung abgelehnt (Code %lld).", &[code]),
        other => other.to_string(),
    }
}

fn canned_sign_in(proxy: Option<String>, expected: Option<Uuid>, cancel: &AtomicBool, send: impl Fn(SignIn)) {
    let outcome = drive::dir().and_then(|d| std::fs::read_to_string(d.join("auth")).ok()).unwrap_or_default().trim().to_string();
    drive::note(format!("sign-in canned {}", if outcome.is_empty() { "ok" } else { &outcome }));
    auth::sleep_unless(cancel, CANNED_DELAY);
    let failure = match outcome.as_str() {
        "noclient" => Some(SignInError::NoClientId),
        "network" => Some(SignInError::Network),
        _ => None,
    };
    if let Some(f) = failure {
        return send(SignIn::Failed(f));
    }
    send(SignIn::Code { user_code: "DRV4-2K9Q".into(), uri: "https://www.microsoft.com/link".into(), expires_in: 900 });
    auth::sleep_unless(cancel, Duration::from_millis(if outcome == "wait" { 600_000 } else { 2500 }));
    if cancel.load(Ordering::SeqCst) {
        drive::note("sign-in stopped".into());
        return;
    }
    let failure = match outcome.as_str() {
        "expired" => Some(SignInError::Expired),
        "declined" => Some(SignInError::Declined),
        "notowned" => Some(SignInError::NotOwned),
        "wrong" if expected.is_some() => Some(SignInError::WrongAccount("JemandAnders".into())),
        "other" => Some(SignInError::Other(Problem::plain(auth_text(&AuthError::ChildAccount)))),
        "technical" => Some(SignInError::Other(forms::explain(&anyhow::anyhow!("error sending request for url (https://login.live.com/oauth20_token.srf): connection reset")))),
        _ => None,
    };
    if let Some(f) = failure {
        return send(SignIn::Failed(f));
    }
    send(SignIn::Approved);
    auth::sleep_unless(cancel, CANNED_DELAY);
    let stored = saved(|| {
        if cancel.load(Ordering::SeqCst) {
            bail!("sign-in was cancelled");
        }
        let mut accounts = account::load()?;
        let existing = expected.and_then(|id| accounts.iter().find(|a| a.id == id).cloned());
        let n = accounts.iter().filter(|a| a.kind == AccountKind::Microsoft).count() + 1;
        let mut account = existing.unwrap_or(Account { id: Uuid::new_v4(), name: canned_name(n), kind: AccountKind::Microsoft, proxy: None, color: None });
        if proxy.is_some() && expected.is_none() {
            account.proxy.clone_from(&proxy);
        }
        account::upsert(&mut accounts, account.clone());
        account::save(&accounts)?;
        Ok(account)
    });
    match stored {
        Ok(account) if !cancel.load(Ordering::SeqCst) => send(SignIn::Done(account.name)),
        Ok(_) => {}
        Err(e) => send(SignIn::Failed(SignInError::Other(forms::explain(&e)))),
    }
}

fn canned_name(n: usize) -> String {
    let named = drive::dir().and_then(|d| std::fs::read_to_string(d.join("names")).ok()).unwrap_or_default();
    named.split_whitespace().nth(n - 1).map_or_else(|| format!("Spieler{n}"), str::to_owned)
}

/// Adds an offline account, refusing a name that is invalid or already saved.
pub fn add_offline(name: &str, proxy: Option<String>) -> Result<Account> {
    saved(|| {
        let mut accounts = account::load()?;
        if let Some(problem) = forms::offline_name_problem(name, &accounts) {
            return Err(gone(problem));
        }
        let mut account = Account::offline(name)?;
        account.proxy.clone_from(&proxy);
        account::upsert(&mut accounts, account.clone());
        account::save(&accounts)?;
        Ok(account)
    })
}

/// Replaces the proxy of account `id`; `None` removes it.
pub fn set_proxy(id: Uuid, proxy: Option<String>) -> Result<()> {
    saved(|| {
        let mut accounts = account::load()?;
        let account = accounts.iter_mut().find(|a| a.id == id).ok_or_else(|| gone(crate::core::i18n::tr("Diesen Account gibt es nicht mehr.")))?;
        account.proxy.clone_from(&proxy);
        account::save(&accounts)
    })
}

/// What `remove_account` took away, kept in memory so `restore_account` can put it back.
pub struct Removed {
    pub account: Account,
    index: usize,
    memberships: Vec<(String, usize)>,
    token: Option<String>,
}

/// Removes account `id` from every batch, then from the saved accounts, then its sign-in from the keychain.
pub fn remove_account(id: Uuid) -> Result<Option<Removed>> {
    let mut memberships: Vec<(String, usize)> = Vec::new();
    saved(|| {
        let mut accounts = account::load()?;
        let Some(at) = accounts.iter().position(|a| a.id == id) else { return Ok(None) };
        let token = if accounts[at].kind == AccountKind::Microsoft && !drive::fenced("keychain read", id) { account::read_token(id).ok().flatten() } else { None };
        for mut b in batch::load_all()? {
            if let Some(place) = b.accounts.iter().position(|a| *a == id) {
                b.accounts.remove(place);
                batch::save(&b)?;
                if !memberships.iter().any(|(known, _)| *known == b.id) {
                    memberships.push((b.id.clone(), place));
                }
            }
        }
        let removed = accounts.remove(at);
        account::save(&accounts)?;
        if removed.kind == AccountKind::Microsoft && !drive::fenced("keychain delete", id) {
            account::delete_token(id)?;
        }
        Ok(Some(Removed { account: removed, index: at, memberships: memberships.clone(), token }))
    })
}

/// Puts back what `remove_account` took: the sign-in first, then the account where it was, then its batches.
/// An account with the same name under another id blocks it; the same id again only gets its batches back.
pub fn restore_account(removed: &Removed) -> Result<()> {
    saved(|| {
        let id = removed.account.id;
        let mut accounts = account::load()?;
        if accounts.iter().any(|a| a.id != id && a.name.eq_ignore_ascii_case(&removed.account.name)) {
            return Err(gone(crate::core::i18n::tr("Ein anderer Account heißt schon so.")));
        }
        if !accounts.iter().any(|a| a.id == id) {
            if let Some(token) = &removed.token
                && !drive::fenced("keychain restore", id)
            {
                account::store_token(id, token)?;
            }
            account::reinsert(&mut accounts, removed.account.clone(), removed.index);
            account::save(&accounts)?;
        }
        for (batch_id, place) in &removed.memberships {
            if let Some(mut b) = batch::load_all()?.into_iter().find(|b| &b.id == batch_id)
                && b.rejoin(id, *place)
            {
                batch::save(&b)?;
            }
        }
        Ok(())
    })
}

/// Checks that `proxy` reaches the internet: any HTTP answer through it counts.
pub fn test_proxy(proxy: &str) -> Result<()> {
    if drive::fenced("proxy test", super::forms::proxy_display(proxy)) {
        std::thread::sleep(CANNED_DELAY);
        if proxy.contains("fail") {
            bail!("connection refused");
        }
        return Ok(());
    }
    let agent: ureq::Agent = ureq::Agent::config_builder().proxy(Some(ureq::Proxy::new(proxy)?)).timeout_global(Some(PROXY_TEST_TIMEOUT)).build().into();
    match agent.head(PROXY_TEST_URL).call() {
        Ok(_) | Err(ureq::Error::StatusCode(_)) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Minecraft release versions, newest first.
pub fn versions() -> Result<Vec<String>> {
    if drive::fenced("versions", "canned") {
        std::thread::sleep(CANNED_DELAY);
        return Ok(["26.3", "26.2", "26.1", "1.21.10", "1.21.9", "1.21.8", "1.21.4", "1.20.6", "1.20.4", "1.20.1", "1.19.4", "1.18.2", "1.16.5", "1.12.2", "1.8.9"].map(String::from).to_vec());
    }
    install::releases()
}

/// Fabric mods on Modrinth matching `query` for `game_version`.
pub fn search(query: &str, game_version: &str) -> Result<Vec<Hit>> {
    if drive::fenced("modrinth search", query) {
        std::thread::sleep(CANNED_DELAY);
        if query.contains("offline") {
            bail!("failed to lookup address");
        }
        let q = query.to_lowercase();
        return Ok(canned_hits().into_iter().filter(|h| h.title.to_lowercase().contains(&q) || h.description.to_lowercase().contains(&q)).collect());
    }
    modrinth::search(query, game_version)
}

/// `project_ids` resolved for `game_version`, with their required dependencies.
pub fn resolve(project_ids: &[String], game_version: &str) -> Result<Vec<ModRef>> {
    if drive::fenced("modrinth resolve", project_ids.join(",")) {
        std::thread::sleep(CANNED_DELAY);
        let hits = canned_hits();
        let mut out: Vec<ModRef> = Vec::new();
        for id in project_ids {
            let hit = hits.iter().find(|h| &h.project_id == id || &h.slug == id).with_context(|| format!("{id} has no version for {game_version}"))?;
            if hit.slug == "iris" {
                out.push(canned_ref("sodium", "Sodium", game_version));
            }
            out.push(canned_ref(&hit.slug, &hit.title, game_version));
        }
        let mut seen = std::collections::HashSet::new();
        out.retain(|m| seen.insert(m.project_id.clone()));
        return Ok(out);
    }
    modrinth::resolve_with_dependencies(project_ids, game_version)
}

/// The mods every new batch starts with, and why any is missing for `game_version`.
pub fn defaults(game_version: &str) -> Result<Defaults> {
    if drive::fenced("modrinth defaults", game_version) {
        std::thread::sleep(CANNED_DELAY);
        let mut mods = vec![canned_ref("fabric-api", "Fabric API", game_version), canned_ref("lithium", "Lithium", game_version), canned_ref("ferrite-core", "FerriteCore", game_version), canned_ref("modernfix", "ModernFix", game_version)];
        let mut notes = Vec::new();
        if game_version.starts_with("1.8") || game_version.starts_with("1.12") {
            mods.truncate(1);
            notes.push(format!("skipped lithium: no Fabric version for {game_version}"));
        } else {
            notes.push(format!("skipped krypton: no Fabric version for {game_version}"));
        }
        return Ok(Defaults { mods, notes });
    }
    modrinth::default_mods(game_version)
}

/// The mods of a batch moved to `game_version`: each mod that exists there, with dependencies, and the titles of those that do not.
pub fn move_mods(mods: &[ModRef], game_version: &str) -> Result<(Vec<ModRef>, Vec<String>)> {
    let mut kept = Vec::new();
    let mut missing = Vec::new();
    for m in mods {
        match resolve(std::slice::from_ref(&m.project_id), game_version) {
            Ok(_) => kept.push(m.project_id.clone()),
            Err(e) if is_network(&e) => return Err(e),
            Err(_) => missing.push(m.title.clone()),
        }
    }
    let moved = if kept.is_empty() { Vec::new() } else { resolve(&kept, game_version)? };
    Ok((moved, missing))
}

/// What the batch sheet changes; everything else on a batch stays as saved.
#[derive(Clone)]
pub struct BatchEdit {
    pub name: String,
    pub game_version: String,
    pub server: Option<String>,
    /// `None` keeps the saved memory.
    pub memory_mb: Option<u32>,
    pub accounts: Vec<Uuid>,
    pub mods: Vec<ModRef>,
    pub background: Background,
}

/// Applies `edit` to the saved batch `id` as it is on disk now, or creates a batch when `id` is `None`.
/// Accounts that no longer exist are left out.
pub fn save_batch(id: Option<&str>, edit: BatchEdit) -> Result<()> {
    saved(|| {
        let mut b = match id {
            Some(id) => batch::load_all()?.into_iter().find(|b| b.id == id).ok_or_else(|| gone(crate::core::i18n::tr("Diesen Batch gibt es nicht mehr.")))?,
            None => Batch::new(&edit.name, &edit.game_version),
        };
        let known: HashSet<Uuid> = account::load()?.into_iter().map(|a| a.id).collect();
        apply(&mut b, edit.clone(), &known);
        batch::save(&b)
    })
}

fn apply(b: &mut Batch, edit: BatchEdit, known: &HashSet<Uuid>) {
    if b.game_version != edit.game_version {
        b.loader_version = None;
    }
    b.name = edit.name;
    b.game_version = edit.game_version;
    b.server = edit.server;
    if let Some(mb) = edit.memory_mb {
        b.memory_mb = mb;
    }
    b.accounts = edit.accounts.into_iter().filter(|id| known.contains(id)).collect();
    b.mods = edit.mods;
    b.background = edit.background;
}

/// Deletes a batch and keeps its shared files next to its game folders.
pub fn delete_batch(b: &Batch) -> Result<()> {
    saved(|| fleet::delete_batch(b).map(|_| ()))
}

/// The Prism Launcher instances on this machine, Fabric ones first.
pub fn prism_instances() -> Result<Vec<Instance>> {
    if drive::fenced("prism instances", "canned") {
        std::thread::sleep(CANNED_DELAY);
        return Ok(canned_instances());
    }
    let mut found = prism::instances()?;
    found.sort_by_key(|i| i.unsupported.is_some());
    Ok(found)
}

/// Imports `instance` as a new batch and applies the name, server, memory and accounts of `edit`; its version and mods stay the instance's.
/// A batch that was imported but could not be completed is removed again.
pub fn import_prism(instance: &Instance, edit: BatchEdit) -> Result<Batch> {
    saved(|| {
        let mut b = if drive::fenced("prism import", &instance.id) {
            std::thread::sleep(CANNED_DELAY);
            canned_import(instance, &edit.name)?
        } else {
            prism::import_instance(instance, &edit.name).map_err(|e| if format!("{e:#}").contains("exists already") { gone(crate::core::i18n::trf("Es gibt schon einen Batch „%@“. Wähle einen anderen Namen.", &[&edit.name])) } else { e })?
        };
        let finish = |b: &mut Batch| -> Result<()> {
            let known: HashSet<Uuid> = account::load()?.into_iter().map(|a| a.id).collect();
            b.server.clone_from(&edit.server);
            if let Some(mb) = edit.memory_mb {
                b.memory_mb = mb;
            }
            b.accounts = edit.accounts.iter().copied().filter(|id| known.contains(id)).collect();
            b.background = edit.background;
            batch::save(b)
        };
        if let Err(e) = finish(&mut b) {
            let _ = batch::delete(&b);
            return Err(e);
        }
        Ok(b)
    })
}

/// Newer files for the mods of the saved batch `batch`.
pub fn mod_updates(batch: &Batch) -> Result<Vec<Update>> {
    if drive::fenced("modrinth updates", &batch.name) {
        std::thread::sleep(CANNED_DELAY);
        return Ok(batch.mods.iter().take(2).map(canned_update).collect());
    }
    modrinth::updates(batch)
}

/// Applies `selection` to the saved batch `id` as it is on disk now and returns it. While the batch `running`
/// only its list changes; the files follow at the next start.
pub fn apply_updates(id: &str, selection: &[Update], running: bool) -> Result<Batch> {
    saved(|| {
        let mut b = batch::load_all()?.into_iter().find(|b| b.id == id).ok_or_else(|| gone(crate::core::i18n::tr("Diesen Batch gibt es nicht mehr.")))?;
        if running || drive::fenced("modrinth apply", selection.len()) {
            modrinth::update_mods_later(&mut b, selection)?;
        } else {
            modrinth::update_mods(&mut b, selection)?;
        }
        Ok(b)
    })
}

/// The accounts Prism Launcher lists, names and kinds only.
pub fn prism_accounts() -> Result<Vec<PrismAccount>> {
    if drive::fenced("prism accounts", "canned") {
        return Ok(canned_prism_accounts());
    }
    prism::accounts()
}

/// Saves Prism's offline accounts; Microsoft ones come back as names that must sign in.
pub fn import_prism_accounts() -> Result<AccountImport> {
    saved(|| {
        if !drive::fenced("prism accounts import", "canned") {
            return prism::import_accounts();
        }
        std::thread::sleep(CANNED_DELAY);
        let mut known = account::load()?;
        let done = prism::import_accounts_into(&mut known, &canned_prism_accounts());
        if !done.added.is_empty() {
            account::save(&known)?;
        }
        Ok(done)
    })
}

fn canned_prism_accounts() -> Vec<PrismAccount> {
    let one = |name: &str, kind| PrismAccount { name: name.into(), kind };
    vec![one("Steve", AccountKind::Offline), one("Notch", AccountKind::Microsoft), one("AltTwo", AccountKind::Microsoft), one("Farm_Bot3", AccountKind::Offline), one("Ünicode", AccountKind::Offline)]
}

fn canned_instances() -> Vec<Instance> {
    let one = |id: &str, name: &str, game: &str, mods: usize, unsupported: Option<&str>| Instance {
        id: id.into(),
        name: name.into(),
        dir: std::path::PathBuf::from("canned").join(id),
        game_dir: std::path::PathBuf::from("canned").join(id).join(".minecraft"),
        game_version: Some(game.into()),
        loader_version: unsupported.is_none().then(|| "0.16.5".to_owned()),
        mods,
        icon: "default".into(),
        unsupported: unsupported.map(Into::into),
    };
    vec![
        one("farm-alts", "Farm Alts", "26.2", 12, None),
        one("creative", "Creative Build", "1.21.4", 7, None),
        one("old-forge", "Old Forge Pack", "1.20.1", 41, Some("uses Forge, only Fabric is supported")),
        one("vanilla", "Vanilla", "1.21.1", 0, Some("no mod loader, only Fabric instances can be imported")),
    ]
}

fn canned_import(instance: &Instance, name: &str) -> Result<Batch> {
    if let Some(reason) = &instance.unsupported {
        bail!("{} cannot be imported: {reason}", instance.name);
    }
    if batch::load_all()?.iter().any(|b| b.name.eq_ignore_ascii_case(name)) {
        return Err(gone(crate::core::i18n::trf("Es gibt schon einen Batch „%@“. Wähle einen anderen Namen.", &[&name])));
    }
    let game = instance.game_version.clone().context("no Minecraft version")?;
    let mut b = Batch::new(name, &game);
    b.loader_version.clone_from(&instance.loader_version);
    b.mods = vec![canned_ref("fabric-api", "Fabric API", &game), canned_ref("lithium", "Lithium", &game), canned_ref("sodium", "Sodium", &game)];
    if instance.mods > 0 {
        b.local_mods = vec!["my-private-mod-1.0.jar".into(), "friend-build-1.2.jar".into()];
    }
    batch::save(&b)?;
    Ok(b)
}

fn canned_update(m: &ModRef) -> Update {
    let latest = ModRef { version_id: format!("{}-new", m.version_id), file_name: m.file_name.replace(".jar", "-b.jar"), sha1: "1".repeat(40), ..m.clone() };
    Update { current: m.clone(), latest }
}

fn canned_hits() -> Vec<Hit> {
    let hit = |slug: &str, title: &str, description: &str, downloads: u64| Hit { title: title.into(), slug: slug.into(), project_id: format!("id-{slug}"), description: description.into(), downloads, icon_url: None };
    vec![
        hit("sodium", "Sodium", "A modern rendering engine for Minecraft which greatly improves frame rates and stutter", 61_200_000),
        hit("iris", "Iris Shaders", "A modern shader pack loader for Minecraft intended to be compatible with existing OptiFine shader packs", 48_000_000),
        hit("lithium", "Lithium", "No-compromises game logic optimization mod", 35_500_000),
        hit("modmenu", "Mod Menu", "Adds a mod menu to view the list of mods you have installed", 72_300_000),
        hit("ferrite-core", "FerriteCore", "Memory usage optimizations", 40_100_000),
        hit("modernfix", "ModernFix", "All-in-one mod that improves performance, reduces memory usage, and fixes many bugs", 20_400_000),
        hit("krypton", "Krypton", "A mod to optimize the Minecraft networking stack", 9_800_000),
        hit("fabric-api", "Fabric API", "Lightweight and modular API providing common hooks and intercompatibility measures", 120_000_000),
        hit("xaeros-minimap", "Xaero's Minimap", "Displays the map of the area around you", 30_200_000),
        hit("appleskin", "AppleSkin", "Food-related HUD improvements", 25_600_000),
        hit("tiny-tweak", "Tiny Tweak", "A very small mod", 940),
    ]
}

fn canned_ref(slug: &str, title: &str, game_version: &str) -> ModRef {
    ModRef {
        project_id: format!("id-{slug}"),
        version_id: format!("v-{slug}-{game_version}"),
        title: title.into(),
        file_name: format!("{slug}-{game_version}.jar"),
        url: format!("https://cdn.modrinth.com/canned/{slug}.jar"),
        sha1: "0".repeat(40),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_edit_keeps_what_the_sheet_does_not_show() {
        let (me, gone) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let mut b = Batch::new("Farm", "26.2");
        b.loader_version = Some("0.17".into());
        b.memory_mb = 3000;
        b.jvm_args = vec!["-XX:+UseZGC".into()];
        let edit = |version: &str, memory_mb| BatchEdit { name: "Lobby".into(), game_version: version.into(), server: None, memory_mb, accounts: vec![me, gone], mods: Vec::new(), background: Background::Balanced };
        apply(&mut b, edit("26.2", None), &HashSet::from([me]));
        assert_eq!((b.name.as_str(), b.memory_mb, b.loader_version.as_deref(), b.accounts.as_slice()), ("Lobby", 3000, Some("0.17"), &[me][..]));
        assert_eq!(b.jvm_args, ["-XX:+UseZGC"]);
        assert_eq!(b.background, Background::Balanced);
        apply(&mut b, edit("26.3", Some(2048)), &HashSet::from([me]));
        assert_eq!((b.memory_mb, b.loader_version.as_deref()), (2048, None));
    }

    #[test]
    fn errors_sort_into_what_people_can_do() {
        assert_eq!(classify(&AuthError::CodeExpired.into()), SignInError::Expired);
        assert_eq!(classify(&AuthError::Declined.into()), SignInError::Declined);
        assert_eq!(classify(&AuthError::InvalidClientId.into()), SignInError::NoClientId);
        assert_eq!(classify(&AuthError::GameNotOwned.into()), SignInError::NotOwned);
        assert_eq!(classify(&WrongAccount("X".into()).into()), SignInError::WrongAccount("X".into()));
        let io: anyhow::Error = anyhow::Error::from(std::io::Error::other("reset")).context("cannot reach Microsoft");
        assert_eq!(classify(&io), SignInError::Network);
        assert!(matches!(classify(&AuthError::ChildAccount.into()), SignInError::Other(_)));
        let keychain = classify(&anyhow::anyhow!("system keychain is not available"));
        assert!(matches!(&keychain, SignInError::Other(p) if p.detail.as_deref().is_some_and(|d| d.contains("keychain")) && !p.text.contains("keychain")), "{keychain:?}");
    }

    #[test]
    fn a_save_that_met_another_window_runs_again_from_a_fresh_load() {
        let stale = || anyhow::anyhow!("accounts.json was changed by another Manycraft since it was read; reload and try again");
        let mut calls = 0;
        let done = saved(|| {
            calls += 1;
            if calls == 1 { Err(stale()) } else { Ok(calls) }
        });
        assert_eq!(done.unwrap(), 2);
        let mut calls = 0;
        assert!(
            saved::<()>(|| {
                calls += 1;
                Err(stale())
            })
            .is_err()
        );
        assert_eq!(calls, 2);
        let mut calls = 0;
        assert!(
            saved::<()>(|| {
                calls += 1;
                bail!("disk full")
            })
            .is_err()
        );
        assert_eq!(calls, 1);
    }

    #[test]
    fn only_outages_count_as_network_trouble() {
        let status = |code| anyhow::Error::from(ureq::Error::StatusCode(code)).context("modrinth");
        assert!(is_network(&status(503)) && is_network(&status(429)));
        assert!(!is_network(&status(404)) && !is_network(&anyhow::anyhow!("no version for 26.3")));
    }
}
