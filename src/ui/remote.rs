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
use crate::core::instance::{self as setup, Loader};
use crate::core::modrinth::{self, Defaults, Hit, Page, Sort, Support, Update};
use crate::core::mrpack;
use crate::core::prism::{self, AccountImport, Instance, PrismAccount};
use crate::core::{browser, drive, fleet, install, webauth};

use super::forms::{self, Plain, Problem};

const PROXY_TEST_URL: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
const PROXY_TEST_TIMEOUT: Duration = Duration::from_secs(10);
const CANNED_DELAY: Duration = Duration::from_millis(450);
const CANNED_SEARCH_DELAY: Duration = Duration::from_millis(300);
const CANNED_PAGE: usize = 6;
const CANNED_STEPS: usize = 8;

static SAVING: Mutex<()> = Mutex::new(());

fn saving() -> MutexGuard<'static, ()> {
    SAVING.lock().unwrap_or_else(|e| e.into_inner())
}

/// Counts how often the connection came back; views observe it to retry what failed without one.
#[derive(Default)]
pub struct Back(pub u64);

impl gpui_kit::Global for Back {}

/// Tells every observing view that the connection is back.
pub fn network_back(cx: &mut gpui_kit::App) {
    let next = cx.try_global::<Back>().map_or(0, |b| b.0) + 1;
    cx.set_global(Back(next));
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
    /// The sign-in window is on screen.
    Window,
    Code(CodeStep),
    Approved,
    /// Signed in; carries the account name as Minecraft knows it.
    Done(String),
    Failed(SignInError),
}

/// A device code to enter in the browser, with what the sheet needs to say about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeStep {
    pub user_code: String,
    /// The Microsoft page with the code filled in.
    pub link: String,
    pub expires_in: u64,
    /// Whether the default browser can open the page in a private window.
    pub private: bool,
    /// The sign-in window could not be shown, so the sign-in went on in the browser.
    pub fallback: bool,
    /// The person asked for the browser in the sign-in window, so the page opens at once.
    pub open: bool,
}

/// Where a Microsoft sign-in happens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    /// Manycraft's own sign-in window, or the browser when that is not possible.
    Window,
    Browser,
}

/// Why a sign-in stopped, sorted by what the person can do about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignInError {
    Expired,
    Declined,
    Closed,
    NoClientId,
    NotOwned,
    WrongAccount(String),
    Network,
    Other(Problem),
}

#[derive(Debug)]
struct WindowClosed;

impl std::fmt::Display for WindowClosed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the sign-in window was closed")
    }
}

impl std::error::Error for WindowClosed {}

static NO_WINDOW: AtomicBool = AtomicBool::new(false);

/// Whether a sign-in through `proxy` asked for `via` starts in the sign-in window rather than the browser. Linux always uses the browser.
pub fn uses_window(proxy: Option<&str>, via: Via) -> bool {
    cfg!(any(windows, target_os = "macos")) && via == Via::Window && proxy.is_none() && !NO_WINDOW.load(Ordering::SeqCst)
}

enum Next {
    Tokens(auth::Tokens),
    Browser { open: bool, fallback: bool },
}

/// Runs a Microsoft sign-in through `proxy` and sends each step to `tx`. Once `cancel` is set nothing more is
/// stored or sent and the sign-in window closes. With `expected` only that account may sign in. The sign-in window is used
/// only without a proxy, since it cannot route through one; after it failed to open once, the browser is used.
pub fn sign_in(proxy: Option<String>, expected: Option<Uuid>, via: Via, cancel: Arc<AtomicBool>, tx: async_channel::Sender<SignIn>) {
    let send = |step: SignIn| {
        if !cancel.load(Ordering::SeqCst) {
            let _ = tx.send_blocking(step);
        }
    };
    if drive::on() {
        return canned_sign_in(proxy, expected, via, &cancel, send);
    }
    let run = || -> Result<Account> {
        let flow = auth::Flow::new(proxy.as_deref())?;
        let windowed = uses_window(proxy.as_deref(), via) && flow.has_window();
        let next = if windowed { in_window(&flow, &cancel, &send)? } else { Next::Browser { open: false, fallback: false } };
        let tokens = match next {
            Next::Tokens(tokens) => tokens,
            Next::Browser { open, fallback } => in_browser(&flow, &cancel, &send, open, fallback)?,
        };
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

fn window_request() -> webauth::Request {
    use crate::core::i18n::tr;
    webauth::Request { url: webauth::authorize_url(), title: tr("Mit Microsoft anmelden"), browser_label: tr("Im Browser öffnen"), quiet: drive::dir().is_some(), near: super::native::center() }
}

fn in_window(flow: &auth::Flow, cancel: &AtomicBool, send: &impl Fn(SignIn)) -> Result<Next> {
    let Ok(mut window) = webauth::Window::open(&window_request()) else {
        NO_WINDOW.store(true, Ordering::SeqCst);
        return Ok(Next::Browser { open: false, fallback: true });
    };
    let outcome = window.wait(cancel, || send(SignIn::Window));
    std::thread::spawn(move || drop(window));
    match outcome {
        webauth::Outcome::Code(code) => {
            if cancel.load(Ordering::SeqCst) {
                bail!("cancelled");
            }
            send(SignIn::Approved);
            flow.redeem(&code).map(Next::Tokens)
        }
        webauth::Outcome::Denied { error, .. } if error == "access_denied" => Err(AuthError::Declined.into()),
        webauth::Outcome::Denied { error, description } => bail!("Microsoft sign-in failed: {error}: {description}"),
        webauth::Outcome::Browser => Ok(Next::Browser { open: true, fallback: false }),
        webauth::Outcome::Closed => Err(WindowClosed.into()),
        webauth::Outcome::NoWindow(_) => {
            NO_WINDOW.store(true, Ordering::SeqCst);
            Ok(Next::Browser { open: false, fallback: true })
        }
        webauth::Outcome::Cancelled => Err(AuthError::Cancelled.into()),
    }
}

fn in_browser(flow: &auth::Flow, cancel: &AtomicBool, send: &impl Fn(SignIn), open: bool, fallback: bool) -> Result<auth::Tokens> {
    let code = flow.start()?;
    let link = webauth::link_with_code(&code.verification_uri, &code.user_code);
    let private = browser::default().is_some_and(|b| b.private());
    send(SignIn::Code(CodeStep { user_code: code.user_code.clone(), link, expires_in: code.expires_in, private, fallback, open }));
    let tokens = flow.poll_cancellable(&code, cancel)?;
    if cancel.load(Ordering::SeqCst) {
        bail!("cancelled");
    }
    send(SignIn::Approved);
    Ok(tokens)
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
    if e.downcast_ref::<WindowClosed>().is_some() {
        return SignInError::Closed;
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

pub(super) fn auth_text(e: &AuthError) -> String {
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

fn canned_sign_in(proxy: Option<String>, expected: Option<Uuid>, via: Via, cancel: &AtomicBool, send: impl Fn(SignIn)) {
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
    let windowed = uses_window(proxy.as_deref(), via) && outcome != "nowindow";
    let (mut open, mut fallback) = (false, outcome == "nowindow");
    if windowed && outcome == "realwindow" {
        drive::note("sign-in window real".into());
        let ended = match webauth::Window::open(&window_request()) {
            Ok(mut window) => window.wait(cancel, || send(SignIn::Window)),
            Err(e) => webauth::Outcome::NoWindow(e.to_string()),
        };
        drive::note(format!("sign-in window ended {ended:?}"));
        match ended {
            webauth::Outcome::Closed => return send(SignIn::Failed(SignInError::Closed)),
            webauth::Outcome::Denied { .. } => return send(SignIn::Failed(SignInError::Declined)),
            webauth::Outcome::Browser => open = true,
            webauth::Outcome::NoWindow(_) => fallback = true,
            webauth::Outcome::Cancelled => return drive::note("sign-in stopped".into()),
            webauth::Outcome::Code(_) => {}
        }
    } else if windowed {
        drive::note("sign-in window".into());
        send(SignIn::Window);
        auth::sleep_unless(cancel, Duration::from_millis(if outcome == "wait" { 600_000 } else { 2500 }));
        if cancel.load(Ordering::SeqCst) {
            drive::note("sign-in stopped".into());
            return;
        }
        match outcome.as_str() {
            "closed" => return send(SignIn::Failed(SignInError::Closed)),
            "browser" => open = true,
            _ => {}
        }
    }
    if !windowed || open || fallback {
        let code = "DRV4-2K9Q";
        send(SignIn::Code(CodeStep { user_code: code.into(), link: webauth::link_with_code("https://www.microsoft.com/link", code), expires_in: 900, private: outcome != "noprivate", fallback, open }));
        auth::sleep_unless(cancel, Duration::from_millis(if outcome == "wait" || outcome == "browser" { 600_000 } else { 2500 }));
        if cancel.load(Ordering::SeqCst) {
            drive::note("sign-in stopped".into());
            return;
        }
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
        let account = account::upsert(&mut accounts, account);
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
        let account = account::upsert(&mut accounts, account);
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

/// Gives the accounts `ids` colour `color`; ids no longer saved are skipped.
pub fn set_color(ids: &[Uuid], color: u8) -> Result<()> {
    saved(|| {
        let mut accounts = account::load()?;
        for a in accounts.iter_mut().filter(|a| ids.contains(&a.id)) {
            a.color = Some(color);
        }
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

/// One page of Modrinth mods matching `query` (the most downloaded ones when it is empty) from result `offset` on.
/// Unless `all`, only Fabric mods with a version for `game_version`.
pub fn search_page(query: &str, game_version: &str, sort: Sort, all: bool, offset: usize) -> Result<Page> {
    if drive::fenced("modrinth search", query) && !drive::live() {
        std::thread::sleep(CANNED_SEARCH_DELAY);
        if query.contains("offline") {
            bail!("failed to lookup address");
        }
        let q = query.to_lowercase();
        let mut hits: Vec<Hit> = canned_hits(game_version).into_iter().filter(|h| h.title.to_lowercase().contains(&q) || h.description.to_lowercase().contains(&q)).filter(|h| all || h.support(game_version) == Support::Fits).collect();
        if sort == Sort::Downloads {
            hits.sort_by_key(|h| std::cmp::Reverse(h.downloads));
        } else {
            hits.sort_by_key(|h| !h.title.to_lowercase().starts_with(&q));
        }
        let total = hits.len();
        return Ok(Page { hits: hits.into_iter().skip(offset).take(CANNED_PAGE).collect(), total, saved: None });
    }
    modrinth::search_page(query, game_version, sort, all, offset)
}

/// `project_ids` resolved for `game_version`, with their required dependencies.
pub fn resolve(project_ids: &[String], game_version: &str) -> Result<Vec<ModRef>> {
    if drive::fenced("modrinth resolve", project_ids.join(",")) && !drive::live() {
        std::thread::sleep(CANNED_DELAY);
        let hits = canned_hits(game_version);
        let mut out: Vec<ModRef> = Vec::new();
        for id in project_ids {
            let hit = hits.iter().find(|h| &h.project_id == id || &h.slug == id).with_context(|| format!("{id} has no version for {game_version}"))?;
            out.push(canned_ref(&hit.slug, &hit.title, game_version));
            if hit.slug == "iris" {
                out.push(canned_ref("sodium", "Sodium", game_version));
            }
        }
        let mut seen = std::collections::HashSet::new();
        out.retain(|m| seen.insert(m.project_id.clone()));
        return Ok(out);
    }
    modrinth::resolve_with_dependencies(project_ids, game_version)
}

/// The mods every new batch starts with, and why any is missing for `game_version`.
pub fn defaults(game_version: &str) -> Result<Defaults> {
    if drive::fenced("modrinth defaults", game_version) && !drive::live() {
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

/// The instance a batch plays after its sheet is saved.
#[derive(Clone, Debug, PartialEq)]
pub enum InstancePick {
    Existing(String),
    /// A new Fabric instance named like the batch.
    New { game_version: String, mods: Vec<ModRef> },
}

/// What the batch sheet changes; everything else on a batch stays as saved.
#[derive(Clone)]
pub struct BatchEdit {
    pub name: String,
    pub instance: InstancePick,
    pub server: Option<String>,
    /// `None` uses the instance's memory.
    pub memory_override: Option<u32>,
    pub accounts: Vec<Uuid>,
    pub background: Background,
}

/// Applies `edit` to the saved batch `id` as it is on disk now, or creates a batch when `id` is `None`, and returns it.
/// A picked new instance is created first; accounts that no longer exist are left out.
pub fn save_batch(id: Option<&str>, edit: BatchEdit) -> Result<Batch> {
    let made = match &edit.instance {
        InstancePick::New { game_version, mods } => Some(saved(|| {
            let taken: Vec<String> = setup::load_all()?.into_iter().map(|i| i.name).collect();
            let mut made = setup::Instance::new(&setup::free_name(&edit.name, &taken), game_version, Loader::default());
            made.mods.clone_from(mods);
            setup::save(&made)?;
            Ok(made)
        })?),
        InstancePick::Existing(_) => None,
    };
    let result = saved(|| {
        let instances = setup::load_all()?;
        let mut b = match id {
            Some(id) => batch::load_all()?.into_iter().find(|b| b.id == id).ok_or_else(|| gone(crate::core::i18n::tr("Diesen Batch gibt es nicht mehr.")))?,
            None => Batch::new(&edit.name, ""),
        };
        let instance_id = match &edit.instance {
            InstancePick::Existing(id) => instances.iter().find(|i| i.id == *id).map(|i| i.id.clone()).ok_or_else(|| gone(crate::core::i18n::tr("Diese Instanz gibt es nicht mehr.")))?,
            InstancePick::New { .. } => made.as_ref().map(|i| i.id.clone()).context("no new instance")?,
        };
        let known: HashSet<Uuid> = account::load()?.into_iter().map(|a| a.id).collect();
        apply(&mut b, instance_id, edit.clone(), &known);
        batch::save(&b)?;
        Ok(b)
    });
    if result.is_err()
        && let Some(made) = &made
    {
        let _ = setup::delete(made);
    }
    result
}

fn apply(b: &mut Batch, instance_id: String, edit: BatchEdit, known: &HashSet<Uuid>) {
    b.name = edit.name;
    b.instance_id = instance_id;
    b.server = edit.server;
    b.memory_override = edit.memory_override;
    b.accounts = edit.accounts.into_iter().filter(|id| known.contains(id)).collect();
    b.background = edit.background;
}

/// What the instance sheet and page change on an instance; everything else stays as saved.
#[derive(Clone, Debug, PartialEq)]
pub struct InstanceEdit {
    pub name: String,
    pub game_version: String,
    pub loader: Loader,
    pub memory_mb: u32,
    pub jvm_args: Vec<String>,
    pub mods: Vec<ModRef>,
}

impl InstanceEdit {
    /// What `made` holds now.
    pub fn of(made: &setup::Instance) -> Self {
        Self { name: made.name.clone(), game_version: made.game_version.clone(), loader: made.loader.clone(), memory_mb: made.memory_mb, jvm_args: made.jvm_args.clone(), mods: made.mods.clone() }
    }
}

fn find_instance(id: &str) -> Result<setup::Instance> {
    setup::load_all()?.into_iter().find(|i| i.id == id).ok_or_else(|| gone(crate::core::i18n::tr("Diese Instanz gibt es nicht mehr.")))
}

fn taken_name(name: &str, except: Option<&str>) -> Result<()> {
    if setup::load_all()?.iter().any(|i| Some(i.id.as_str()) != except && i.name.trim().eq_ignore_ascii_case(name.trim())) {
        return Err(gone(crate::core::i18n::trf("Es gibt schon eine Instanz „%@“. Wähle einen anderen Namen.", &[&name.trim()])));
    }
    Ok(())
}

fn edited(made: &mut setup::Instance, edit: InstanceEdit) {
    made.name = edit.name.trim().to_owned();
    made.game_version = edit.game_version;
    made.loader = edit.loader;
    made.memory_mb = edit.memory_mb;
    made.jvm_args = edit.jvm_args;
    if made.loader.takes_mods() {
        made.mods = edit.mods;
    } else {
        made.mods.clear();
    }
}

/// Creates an instance from `edit` and returns it.
pub fn create_instance(edit: InstanceEdit) -> Result<setup::Instance> {
    saved(|| {
        taken_name(&edit.name, None)?;
        let mut made = setup::Instance::new(&edit.name, &edit.game_version, edit.loader.clone());
        edited(&mut made, edit.clone());
        setup::save(&made)?;
        Ok(made)
    })
}

/// Saves the fields `edit` changes against `base`, the instance as the caller saw it, onto instance `id` as it is
/// on disk now, and returns it. Refused when one of those fields changed on disk since, or when the mods change
/// after the version or loader did.
pub fn save_instance(id: &str, base: &InstanceEdit, edit: InstanceEdit) -> Result<setup::Instance> {
    saved(|| {
        let mut made = find_instance(id)?;
        let merged = merge(base, &InstanceEdit::of(&made), &edit).ok_or_else(|| gone(crate::core::i18n::tr("Die Instanz wurde inzwischen anders geändert. Sieh nach und versuch es noch einmal.")))?;
        if !merged.name.trim().eq_ignore_ascii_case(made.name.trim()) {
            taken_name(&merged.name, Some(id))?;
        }
        edited(&mut made, merged);
        setup::save(&made)?;
        Ok(made)
    })
}

fn merge(base: &InstanceEdit, now: &InstanceEdit, edit: &InstanceEdit) -> Option<InstanceEdit> {
    fn take<T: PartialEq + Clone>(base: &T, now: &T, edit: &T, out: &mut T) -> bool {
        if edit == base || edit == now {
            return true;
        }
        if now != base {
            return false;
        }
        out.clone_from(edit);
        true
    }
    let mut out = now.clone();
    let fits = take(&base.name, &now.name, &edit.name, &mut out.name)
        && take(&base.game_version, &now.game_version, &edit.game_version, &mut out.game_version)
        && take(&base.loader, &now.loader, &edit.loader, &mut out.loader)
        && take(&base.memory_mb, &now.memory_mb, &edit.memory_mb, &mut out.memory_mb)
        && take(&base.jvm_args, &now.jvm_args, &edit.jvm_args, &mut out.jvm_args)
        && take(&base.mods, &now.mods, &edit.mods, &mut out.mods);
    let moved_under = edit.mods != base.mods && (now.game_version != base.game_version || !now.loader.same_kind(&base.loader));
    let moved_over = (edit.game_version != base.game_version || !edit.loader.same_kind(&base.loader)) && now.mods != base.mods;
    (fits && !moved_under && !moved_over).then_some(out)
}

/// Copies the saved instance `id` with its mods, config and packs, but no worlds, and returns the copy.
pub fn clone_instance(id: &str) -> Result<setup::Instance> {
    saved(|| {
        let source = find_instance(id)?;
        let taken: Vec<String> = setup::load_all()?.into_iter().map(|i| i.name).collect();
        setup::clone(&source, &setup::free_name(&crate::core::i18n::trf("%@ Kopie", &[&source.name]), &taken))
    })
}

/// Moves the saved instance `id` with its games to the trash and returns its trash entry; refused while a batch plays it or a game runs.
pub fn delete_instance(id: &str) -> Result<String> {
    saved(|| fleet::remove_instance(&find_instance(id)?))
}

/// Brings the deleted instance in trash entry `entry` back and returns it; a name taken since gets a number.
pub fn restore_instance(entry: &str) -> Result<setup::Instance> {
    let _saving = saving();
    let mut made = fleet::restore_instance(entry)?;
    let taken: Vec<String> = setup::load_all()?.into_iter().filter(|i| i.id != made.id).map(|i| i.name).collect();
    let name = setup::free_name(&made.name, &taken);
    if name != made.name {
        made.name = name;
        if let Err(e) = setup::save(&made) {
            eprintln!("restored instance keeps its old name: {e:#}");
        }
    }
    Ok(made)
}

/// Moves the saved instance `id` to the trash and returns its trash entry, without waiting for every game to stop;
/// refused while a batch plays it. Callers check first that no game of it runs.
pub fn discard_instance(id: &str) -> Result<String> {
    saved(|| fleet::discard_instance(&find_instance(id)?))
}

/// Remembers `account` as the one that last played instance `id` alone.
pub fn remember_account(id: &str, account: Uuid) -> Result<()> {
    saved(|| {
        let mut made = find_instance(id)?;
        if made.last_account != Some(account) {
            made.last_account = Some(account);
            setup::save(&made)?;
        }
        Ok(())
    })
}

/// Deletes a batch; its instance with worlds, mods and settings stays.
pub fn delete_batch(b: &Batch) -> Result<()> {
    saved(|| fleet::delete_batch(b))
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

/// Imports `instance` as a new instance named `name`; its version and mods stay the Prism instance's.
pub fn import_prism(instance: &Instance, name: &str) -> Result<setup::Instance> {
    saved(|| {
        if drive::fenced("prism import", &instance.id) {
            std::thread::sleep(CANNED_DELAY);
            return canned_import(instance, name);
        }
        prism::import_instance(instance, name).map_err(|e| if format!("{e:#}").contains("exists already") { gone(crate::core::i18n::trf("Es gibt schon eine Instanz „%@“. Wähle einen anderen Namen.", &[&name])) } else { e })
    })
}

/// Imports a `.mrpack`, Prism export or Prism instance folder as a new instance; see `mrpack::import`.
/// A driven instance runs the same checks, then makes an empty one of the pack's name without downloading.
pub fn import_instance(path: &std::path::Path, cancel: &AtomicBool, progress: &(dyn Fn(usize, usize) + Sync)) -> Result<setup::Instance> {
    if !drive::fenced("mrpack import", path.display()) {
        return mrpack::import(path, cancel, progress);
    }
    let name = mrpack::peek(path)?;
    for step in 0..=CANNED_STEPS {
        if cancel.load(Ordering::SeqCst) {
            bail!(mrpack::Refused::Cancelled);
        }
        progress(step, CANNED_STEPS);
        std::thread::sleep(CANNED_DELAY);
    }
    saved(|| {
        let taken: Vec<String> = setup::load_all()?.into_iter().map(|b| b.name).collect();
        let made = setup::Instance::new(&mrpack::free_name(&name, &taken), "1.21.1", Loader::default());
        setup::save(&made)?;
        Ok(made)
    })
}

/// Deletes an instance an import made after it was cancelled too late to stop.
pub fn drop_import(made: &setup::Instance) -> Result<()> {
    setup::delete(made)
}

/// Writes `made` as a `.mrpack` to `out`; a driven instance only records it.
pub fn export_instance(made: &setup::Instance, out: &std::path::Path) -> Result<()> {
    if drive::fenced("mrpack export", out.display()) {
        std::thread::sleep(CANNED_DELAY);
        return Ok(());
    }
    mrpack::export(made, out)
}

/// Newer files for the mods of the saved instance `made`.
pub fn mod_updates(made: &setup::Instance) -> Result<Vec<Update>> {
    if drive::fenced("modrinth updates", &made.name) {
        std::thread::sleep(CANNED_DELAY);
        return Ok(made.mods.iter().take(2).map(canned_update).collect());
    }
    modrinth::updates(made)
}

/// Applies `selection` to the saved instance `id` as it is on disk now and returns it. While a game of it
/// is `running` only its list changes; the files follow at the next start.
pub fn apply_updates(id: &str, selection: &[Update], running: bool) -> Result<setup::Instance> {
    saved(|| {
        let mut b = setup::load_all()?.into_iter().find(|b| b.id == id).ok_or_else(|| gone(crate::core::i18n::tr("Diese Instanz gibt es nicht mehr.")))?;
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

fn canned_import(instance: &Instance, name: &str) -> Result<setup::Instance> {
    if let Some(reason) = &instance.unsupported {
        bail!("{} cannot be imported: {reason}", instance.name);
    }
    if setup::load_all()?.iter().any(|b| b.name.eq_ignore_ascii_case(name)) {
        return Err(gone(crate::core::i18n::trf("Es gibt schon eine Instanz „%@“. Wähle einen anderen Namen.", &[&name])));
    }
    let game = instance.game_version.clone().context("no Minecraft version")?;
    let mut b = setup::Instance::new(name, &game, Loader::Fabric(instance.loader_version.clone()));
    b.mods = vec![canned_ref("fabric-api", "Fabric API", &game), canned_ref("lithium", "Lithium", &game), canned_ref("sodium", "Sodium", &game)];
    if instance.mods > 0 {
        b.local_mods = vec!["my-private-mod-1.0.jar".into(), "friend-build-1.2.jar".into()];
    }
    setup::save(&b)?;
    Ok(b)
}

fn canned_update(m: &ModRef) -> Update {
    let latest = ModRef { version_id: format!("{}-new", m.version_id), file_name: m.file_name.replace(".jar", "-b.jar"), sha1: "1".repeat(40), ..m.clone() };
    Update { current: m.clone(), latest }
}

fn canned_hits(game_version: &str) -> Vec<Hit> {
    let hit = |slug: &str, title: &str, author: &str, description: &str, downloads: u64, support: Support| {
        let (versions, loader) = match support {
            Support::Fits => (game_version, "fabric"),
            Support::OtherVersion => ("1.16.5", "fabric"),
            Support::OtherLoader => (game_version, "forge"),
        };
        Hit {
            title: title.into(),
            slug: slug.into(),
            project_id: format!("id-{slug}"),
            description: description.into(),
            downloads,
            icon_url: None,
            author: author.into(),
            versions: vec![versions.into()],
            categories: vec![loader.into(), "optimization".into()],
        }
    };
    vec![
        hit("sodium", "Sodium", "jellysquid3", "A modern rendering engine for Minecraft which greatly improves frame rates and stutter", 61_200_000, Support::Fits),
        hit("iris", "Iris Shaders", "coderbot", "A modern shader pack loader for Minecraft intended to be compatible with existing OptiFine shader packs", 48_000_000, Support::Fits),
        hit("lithium", "Lithium", "jellysquid3", "No-compromises game logic optimization mod", 35_500_000, Support::Fits),
        hit("modmenu", "Mod Menu", "Prospector", "Adds a mod menu to view the list of mods you have installed", 72_300_000, Support::Fits),
        hit("ferrite-core", "FerriteCore", "malte0811", "Memory usage optimizations", 40_100_000, Support::Fits),
        hit("modernfix", "ModernFix", "embeddedt", "All-in-one mod that improves performance, reduces memory usage, and fixes many bugs", 20_400_000, Support::Fits),
        hit("krypton", "Krypton", "astei", "A mod to optimize the Minecraft networking stack", 9_800_000, Support::Fits),
        hit("fabric-api", "Fabric API", "modmuss50", "Lightweight and modular API providing common hooks and intercompatibility measures", 120_000_000, Support::Fits),
        hit("xaeros-minimap", "Xaero's Minimap", "xaero96", "Displays the map of the area around you", 30_200_000, Support::Fits),
        hit("xaeros-world-map", "Xaero's World Map", "xaero96", "Adds a fullscreen world map you can browse", 22_900_000, Support::OtherVersion),
        hit("appleskin", "AppleSkin", "squeek502", "Food-related HUD improvements", 25_600_000, Support::Fits),
        hit("sodium-legacy", "Sodium Legacy", "someone", "An old Sodium build kept for older versions", 410_000, Support::OtherVersion),
        hit("optifine-bridge", "OptiFine Bridge", "someone", "Shaders and zoom for Forge", 2_100_000, Support::OtherLoader),
        hit("tiny-tweak", "Tiny Tweak", "nobody", "A very small mod", 940, Support::Fits),
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
        let mut b = Batch::new("Farm", "i1");
        b.join_commands = vec!["/home".into()];
        b.memory_override = Some(3000);
        let edit = |memory_override| BatchEdit { name: "Lobby".into(), instance: InstancePick::Existing("i2".into()), server: None, memory_override, accounts: vec![me, gone], background: Background::Balanced };
        apply(&mut b, "i2".into(), edit(None), &HashSet::from([me]));
        assert_eq!((b.name.as_str(), b.instance_id.as_str(), b.memory_override, b.accounts.as_slice()), ("Lobby", "i2", None, &[me][..]));
        assert_eq!(b.join_commands, ["/home"]);
        assert_eq!(b.background, Background::Balanced);
        apply(&mut b, "i2".into(), edit(Some(2048)), &HashSet::from([me]));
        assert_eq!(b.memory_override, Some(2048));
    }

    #[test]
    fn an_instance_edit_keeps_what_the_sheet_does_not_show_and_vanilla_drops_mods() {
        let mut made = setup::Instance::new("Farm", "26.2", Loader::Fabric(Some("0.17".into())));
        made.local_mods = vec!["own.jar".into()];
        made.last_account = Some(Uuid::from_u128(1));
        let id = made.id.clone();
        let mod_ref = ModRef { project_id: "p".into(), version_id: "v".into(), title: "P".into(), file_name: "p.jar".into(), url: String::new(), sha1: String::new() };
        let mut edit = InstanceEdit::of(&made);
        edit.name = "  Lobby ".into();
        edit.mods = vec![mod_ref];
        edited(&mut made, edit.clone());
        assert_eq!((made.id.as_str(), made.name.as_str(), made.mods.len(), made.local_mods.as_slice(), made.last_account), (id.as_str(), "Lobby", 1, &["own.jar".to_owned()][..], Some(Uuid::from_u128(1))));
        edit.loader = Loader::Vanilla;
        edited(&mut made, edit);
        assert!(made.mods.is_empty());
        assert_eq!(made.local_mods, ["own.jar"]);
    }

    #[test]
    fn a_save_keeps_what_changed_on_disk_meanwhile_and_refuses_a_clash() {
        let made = setup::Instance::new("Farm", "26.2", Loader::Fabric(None));
        let base = InstanceEdit::of(&made);
        let mut now = base.clone();
        now.jvm_args = vec!["-Xss2m".into()];
        let mut edit = base.clone();
        edit.memory_mb = 4096;
        let merged = merge(&base, &now, &edit).unwrap();
        assert_eq!((merged.memory_mb, merged.jvm_args.as_slice()), (4096, &["-Xss2m".to_owned()][..]));
        let mut clash = base.clone();
        clash.jvm_args = vec!["-Xss4m".into()];
        assert!(merge(&base, &now, &clash).is_none());
        assert!(merge(&base, &now, &now).is_some());
        let mut moved = base.clone();
        moved.game_version = "26.3".into();
        let mut stale_mods = base.clone();
        stale_mods.mods = vec![ModRef { project_id: "p".into(), version_id: "v".into(), title: "P".into(), file_name: "p.jar".into(), url: String::new(), sha1: String::new() }];
        assert!(merge(&base, &moved, &stale_mods).is_none());
        assert!(merge(&base, &stale_mods, &moved).is_none());
    }

    #[test]
    fn restoring_a_removed_mod_is_refused_after_the_version_or_the_mods_moved() {
        let mod_ref = |id: &str| ModRef { project_id: id.into(), version_id: "v".into(), title: id.into(), file_name: format!("{id}.jar"), url: String::new(), sha1: String::new() };
        let mut made = setup::Instance::new("Farm", "26.2", Loader::Fabric(None));
        made.mods = vec![mod_ref("b")];
        let base = InstanceEdit::of(&made);
        let mut back = base.clone();
        back.mods.insert(0, mod_ref("a"));
        assert_eq!(merge(&base, &base, &back).unwrap().mods, vec![mod_ref("a"), mod_ref("b")]);
        let mut renamed = base.clone();
        renamed.name = "Technik".into();
        assert_eq!(merge(&base, &renamed, &back).unwrap().mods.len(), 2);
        let mut moved = base.clone();
        moved.game_version = "26.3".into();
        assert!(merge(&base, &moved, &back).is_none());
        let mut other_loader = base.clone();
        other_loader.loader = Loader::Quilt(None);
        assert!(merge(&base, &other_loader, &back).is_none());
        let mut added = base.clone();
        added.mods.push(mod_ref("c"));
        assert!(merge(&base, &added, &back).is_none());
    }

    #[test]
    fn errors_sort_into_what_people_can_do() {
        assert_eq!(classify(&AuthError::CodeExpired.into()), SignInError::Expired);
        assert_eq!(classify(&AuthError::Declined.into()), SignInError::Declined);
        assert_eq!(classify(&AuthError::InvalidClientId.into()), SignInError::NoClientId);
        assert_eq!(classify(&AuthError::GameNotOwned.into()), SignInError::NotOwned);
        assert_eq!(classify(&WrongAccount("X".into()).into()), SignInError::WrongAccount("X".into()));
        assert_eq!(classify(&WindowClosed.into()), SignInError::Closed);
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
