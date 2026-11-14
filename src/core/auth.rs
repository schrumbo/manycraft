//! Microsoft device-code login and the Xbox Live → Minecraft token chain.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::account::{self, Account, AccountKind};
use super::net;

const CLIENT_ID_VAR: &str = "MANYCRAFT_MSA_CLIENT_ID";
const LIVE_CLIENT_ID: &str = "00000000402b5328";
const LIVE_SCOPE: &str = "service::user.auth.xboxlive.com::MBI_SSL";
const LIVE_DEVICE_URL: &str = "https://login.live.com/oauth20_connect.srf";
const LIVE_TOKEN_URL: &str = "https://login.live.com/oauth20_token.srf";
const AZURE_SCOPE: &str = "XboxLive.signin offline_access";
const AZURE_DEVICE_URL: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode";
const AZURE_TOKEN_URL: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/token";
const XBL_URL: &str = "https://user.auth.xboxlive.com/user/authenticate";
const XSTS_URL: &str = "https://xsts.auth.xboxlive.com/xsts/authorize";
const MC_LOGIN_URL: &str = "https://api.minecraftservices.com/authentication/login_with_xbox";
const MC_PROFILE_URL: &str = "https://api.minecraftservices.com/minecraft/profile";
const XSTS_RELYING_PARTY: &str = "rp://api.minecraftservices.com/";
const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
const MAX_BODY: u64 = 1 << 20;
const DEFAULT_INTERVAL: u64 = 5;
const SLOW_DOWN_STEP: u64 = 5;
const TRANSPORT_FAILURES: u32 = 3;
const CANCEL_SLICE: Duration = Duration::from_millis(100);
const OFFLINE_LIFETIME: Duration = Duration::from_secs(10 * 365 * 24 * 3600);
const USAGE: &str = "usage: manycraft login [--proxy URL]\n       manycraft login --offline <name>\n       manycraft login --check <name or id>";

/// Why a sign-in failed, for the cases a person can act on. Found with `downcast_ref` on the returned error.
#[derive(Debug, PartialEq, Eq)]
pub enum AuthError {
    InvalidClientId,
    CodeExpired,
    Declined,
    SignInAgain,
    NoXboxProfile,
    RegionBlocked,
    AdultVerification,
    ChildAccount,
    XboxRejected(u64),
    AppNotApproved,
    GameNotOwned,
    Cancelled,
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidClientId => write!(f, "Microsoft rejected the client id: check {CLIENT_ID_VAR} and that the app allows public client flows"),
            Self::CodeExpired => f.write_str("the sign-in code expired, start again"),
            Self::Declined => f.write_str("the sign-in was declined in the browser"),
            Self::SignInAgain => f.write_str("the saved sign-in is no longer valid, sign in again"),
            Self::NoXboxProfile => f.write_str("this Microsoft account has no Xbox profile: sign in once at xbox.com to create one"),
            Self::RegionBlocked => f.write_str("Xbox Live is not available in this account's country or region"),
            Self::AdultVerification => f.write_str("this account needs adult verification before it can use Xbox services"),
            Self::ChildAccount => f.write_str("this is a child account: an adult must add it to a Microsoft family first"),
            Self::XboxRejected(code) => write!(f, "Xbox Live refused the sign-in (code {code})"),
            Self::AppNotApproved => f.write_str("Minecraft services rejected this app registration: the client id must be approved by Mojang (https://aka.ms/AppRegInfo)"),
            Self::GameNotOwned => f.write_str("this account does not own Minecraft: Java Edition, or has no profile yet"),
            Self::Cancelled => f.write_str("the sign-in was cancelled"),
        }
    }
}

impl std::error::Error for AuthError {}

pub(crate) fn sleep_unless(cancel: &AtomicBool, total: Duration) {
    let end = Instant::now() + total;
    while !cancel.load(Ordering::SeqCst) {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return;
        }
        std::thread::sleep(left.min(CANCEL_SLICE));
    }
}

/// A Minecraft session ready to launch with.
pub struct Session {
    pub name: String,
    pub uuid: Uuid,
    pub access_token: String,
    pub xuid: String,
    pub expires_at: SystemTime,
}

impl Session {
    /// True while the token is present and not yet expired at `now`.
    pub fn is_usable(&self, now: SystemTime) -> bool {
        !self.access_token.is_empty() && now < self.expires_at
    }
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session").field("name", &self.name).field("uuid", &self.uuid).field("expires_at", &self.expires_at).finish_non_exhaustive()
    }
}

/// The code a person types at the verification page.
pub struct DeviceCode {
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
    device_code: String,
}

/// Microsoft tokens from a sign-in or refresh. Never printed.
pub struct Tokens {
    access_token: String,
    refresh_token: Option<String>,
}

enum Poll {
    Pending,
    SlowDown,
    Done(Tokens),
}

struct XboxToken {
    token: String,
    uhs: String,
}

struct McToken {
    access_token: String,
    expires_in: u64,
}

struct Profile {
    id: Uuid,
    name: String,
}

struct Endpoints {
    device_url: String,
    token_url: String,
    scope: &'static str,
    rps_prefix: &'static str,
    response_type: Option<&'static str>,
    device_grant: &'static str,
    xbl: String,
    xsts: String,
    mc_login: String,
    mc_profile: String,
}

impl Endpoints {
    fn live() -> Self {
        Self {
            device_url: LIVE_DEVICE_URL.into(),
            token_url: LIVE_TOKEN_URL.into(),
            scope: LIVE_SCOPE,
            rps_prefix: "t=",
            response_type: Some("device_code"),
            device_grant: "device_code",
            xbl: XBL_URL.into(),
            xsts: XSTS_URL.into(),
            mc_login: MC_LOGIN_URL.into(),
            mc_profile: MC_PROFILE_URL.into(),
        }
    }

    fn azure() -> Self {
        Self { device_url: AZURE_DEVICE_URL.into(), token_url: AZURE_TOKEN_URL.into(), scope: AZURE_SCOPE, rps_prefix: "d=", response_type: None, device_grant: DEVICE_GRANT, ..Self::live() }
    }

    fn for_client(client_id: &str) -> Self {
        if client_id == LIVE_CLIENT_ID { Self::live() } else { Self::azure() }
    }
}

/// One sign-in conversation with Microsoft, Xbox and Minecraft services over a single (possibly proxied) agent.
pub struct Flow {
    agent: ureq::Agent,
    ep: Endpoints,
    client_id: String,
}

impl Flow {
    /// Prepares a flow whose every request goes through `proxy`, or the environment's proxy without one.
    pub fn new(proxy: Option<&str>) -> Result<Self> {
        let client_id = client_id();
        Ok(Self { agent: net::agent(proxy)?, ep: Endpoints::for_client(&client_id), client_id })
    }

    /// Asks Microsoft for a device code.
    pub fn start(&self) -> Result<DeviceCode> {
        let mut form = vec![("client_id", self.client_id.as_str()), ("scope", self.ep.scope)];
        form.extend(self.ep.response_type.map(|kind| ("response_type", kind)));
        let reply = post_form(&self.agent, &self.ep.device_url, &form)?;
        device_code(&reply)
    }

    /// Waits until the code is entered and approved, honouring Microsoft's polling interval.
    pub fn poll(&self, code: &DeviceCode) -> Result<Tokens> {
        self.poll_cancellable(code, &AtomicBool::new(false))
    }

    /// Like [`Flow::poll`], but stops with [`AuthError::Cancelled`] within a fraction of a second once `cancel` is set.
    pub fn poll_cancellable(&self, code: &DeviceCode, cancel: &AtomicBool) -> Result<Tokens> {
        self.poll_loop(code, |d| sleep_unless(cancel, d), || cancel.load(Ordering::SeqCst))
    }

    #[cfg(test)]
    fn poll_with(&self, code: &DeviceCode, sleep: impl FnMut(Duration)) -> Result<Tokens> {
        self.poll_loop(code, sleep, || false)
    }

    fn poll_loop(&self, code: &DeviceCode, mut sleep: impl FnMut(Duration), cancelled: impl Fn() -> bool) -> Result<Tokens> {
        let url = &self.ep.token_url;
        let form = [("grant_type", self.ep.device_grant), ("client_id", &self.client_id), ("device_code", &code.device_code)];
        let mut interval = code.interval.max(1);
        let mut waited = 0;
        let mut failures = 0;
        loop {
            if waited + interval > code.expires_in {
                return Err(AuthError::CodeExpired.into());
            }
            sleep(Duration::from_secs(interval));
            if cancelled() {
                return Err(AuthError::Cancelled.into());
            }
            waited += interval;
            let reply = match post_form(&self.agent, url, &form) {
                Ok(reply) => reply,
                Err(e) => {
                    failures += 1;
                    if failures >= TRANSPORT_FAILURES {
                        return Err(e);
                    }
                    continue;
                }
            };
            if reply.status >= 500 || reply.status == 429 {
                failures += 1;
                if failures >= TRANSPORT_FAILURES {
                    return Err(microsoft_failure(&reply));
                }
                continue;
            }
            failures = 0;
            match token_poll(&reply)? {
                Poll::Done(tokens) => return Ok(tokens),
                Poll::Pending => {}
                Poll::SlowDown => interval += SLOW_DOWN_STEP,
            }
        }
    }

    /// Trades a saved refresh token for fresh Microsoft tokens; the new refresh token replaces the old one.
    pub fn refresh(&self, refresh_token: &str) -> Result<Tokens> {
        let form = [("grant_type", "refresh_token"), ("client_id", &self.client_id), ("refresh_token", refresh_token), ("scope", self.ep.scope)];
        token_refresh(&post_form(&self.agent, &self.ep.token_url, &form)?)
    }

    /// Runs the Xbox Live → XSTS → Minecraft chain and fetches the profile.
    pub fn session(&self, tokens: &Tokens) -> Result<Session> {
        let xbl = xbox_token("Xbox Live", &post_json(&self.agent, &self.ep.xbl, &json!({
            "Properties": {"AuthMethod": "RPS", "SiteName": "user.auth.xboxlive.com", "RpsTicket": format!("{}{}", self.ep.rps_prefix, tokens.access_token)},
            "RelyingParty": "http://auth.xboxlive.com",
            "TokenType": "JWT",
        }))?)?;
        let xsts = xbox_token("Xbox security service", &post_json(&self.agent, &self.ep.xsts, &json!({
            "Properties": {"SandboxId": "RETAIL", "UserTokens": [xbl.token]},
            "RelyingParty": XSTS_RELYING_PARTY,
            "TokenType": "JWT",
        }))?)?;
        let mc = minecraft_login(&post_json(&self.agent, &self.ep.mc_login, &json!({
            "identityToken": format!("XBL3.0 x={};{}", xsts.uhs, xsts.token),
        }))?)?;
        let profile = profile(&get_bearer(&self.agent, &self.ep.mc_profile, &mc.access_token)?)?;
        Ok(assemble(profile, mc, SystemTime::now()))
    }
}

/// The client id from `MANYCRAFT_MSA_CLIENT_ID` at runtime, else the one built in, else Microsoft's Xbox Live id.
pub fn client_id() -> String {
    pick_client_id(std::env::var(CLIENT_ID_VAR).ok().as_deref(), option_env!("MANYCRAFT_MSA_CLIENT_ID"))
}

/// Gets a fresh session for a saved account from the refresh token in the keychain, and keeps the rotated token.
pub fn session_for(account: &Account) -> Result<Session> {
    if account.kind == AccountKind::Offline {
        return Ok(offline_session(account, SystemTime::now()));
    }
    let flow = Flow::new(account.proxy.as_deref())?;
    let tokens = refresh_saved(&flow, account)?;
    let session = flow.session(&tokens)?;
    if session.uuid != account.id {
        bail!("the saved sign-in belongs to {}, not to {}", session.name, account.name);
    }
    Ok(session)
}

fn refresh_lock(id: Uuid) -> Arc<Mutex<()>> {
    static LOCKS: Mutex<Vec<(Uuid, Arc<Mutex<()>>)>> = Mutex::new(Vec::new());
    let mut locks = LOCKS.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((_, lock)) = locks.iter().find(|(known, _)| *known == id) {
        return Arc::clone(lock);
    }
    let lock = Arc::new(Mutex::new(()));
    locks.push((id, Arc::clone(&lock)));
    lock
}

/// Trades the keychain's refresh token of `account` for fresh tokens and keeps the rotated one.
/// One refresh per account at a time: two at once would both spend the same token.
fn refresh_saved(flow: &Flow, account: &Account) -> Result<Tokens> {
    let lock = refresh_lock(account.id);
    let _turn = lock.lock().unwrap_or_else(PoisonError::into_inner);
    let stored = account::read_token(account.id)?.ok_or(AuthError::SignInAgain)?;
    let mut tokens = flow.refresh(&stored)?;
    if let Some(next) = tokens.refresh_token.take().filter(|next| *next != stored) {
        account::store_token(account.id, &next)?;
    }
    Ok(tokens)
}

/// Checks that the saved sign-in of `account` still works by refreshing it, and keeps the rotated token.
/// Fails with [`AuthError::SignInAgain`] when the person has to sign in again; other errors are transient. Offline accounts always pass.
pub fn refresh_account(account: &Account) -> Result<()> {
    if account.kind == AccountKind::Offline {
        return Ok(());
    }
    refresh_saved(&Flow::new(account.proxy.as_deref())?, account).map(|_| ())
}

fn offline_session(account: &Account, now: SystemTime) -> Session {
    Session { name: account.name.clone(), uuid: account.id, access_token: "0".into(), xuid: String::new(), expires_at: now + OFFLINE_LIFETIME }
}

/// `manycraft login`: signs in an account from the terminal, or with `--check` tests a saved one.
pub fn cli(args: &[String]) -> i32 {
    let outcome = match parse_args(args) {
        Ok(Command::Login { proxy }) => login(proxy.as_deref()),
        Ok(Command::Offline { name }) => login_offline(&name),
        Ok(Command::Check { who }) => check(&who),
        Err(message) => {
            eprintln!("{message}\n{USAGE}");
            return 2;
        }
    };
    match outcome {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e:#}");
            1
        }
    }
}

enum Command {
    Login { proxy: Option<String> },
    Offline { name: String },
    Check { who: String },
}

fn parse_args(args: &[String]) -> Result<Command, String> {
    match args {
        [] => Ok(Command::Login { proxy: None }),
        [flag, url] if flag == "--proxy" => Ok(Command::Login { proxy: Some(url.clone()) }),
        [flag, name] if flag == "--offline" => Ok(Command::Offline { name: name.clone() }),
        [flag, who] if flag == "--check" => Ok(Command::Check { who: who.clone() }),
        [flag] if ["--proxy", "--offline", "--check"].contains(&flag.as_str()) => Err(format!("{flag} needs a value")),
        _ => Err("unknown arguments".into()),
    }
}

fn login(proxy: Option<&str>) -> Result<()> {
    let flow = Flow::new(proxy)?;
    account::load()?;
    let code = flow.start()?;
    println!("Open {} and enter the code {}", code.verification_uri, code.user_code);
    println!("Waiting for sign-in, the code lasts {} min ...", code.expires_in / 60);
    let tokens = flow.poll(&code)?;
    let account = save_sign_in(&flow, tokens, proxy, None, &mut || true)?;
    println!("Signed in as {}", account.name);
    Ok(())
}

/// Finishes a device-code sign-in: fetches the profile, stores the refresh token in the keychain and saves the account,
/// keeping its proxy unless `proxy` replaces it. With `expected`, a sign-in as anyone else is refused before anything is stored;
/// when `proceed` says no right before storing, nothing is stored either.
pub fn save_sign_in(flow: &Flow, mut tokens: Tokens, proxy: Option<&str>, expected: Option<Uuid>, proceed: &mut dyn FnMut() -> bool) -> Result<Account> {
    let refresh = tokens.refresh_token.take().context("Microsoft granted no lasting sign-in")?;
    let session = flow.session(&tokens)?;
    if expected.is_some_and(|id| id != session.uuid) {
        return Err(WrongAccount(session.name).into());
    }
    if !proceed() {
        bail!("sign-in was cancelled");
    }
    let mut retried = false;
    loop {
        let saved = || -> Result<Account> {
            let mut accounts = account::load()?;
            account::store_token(session.uuid, &refresh)?;
            let account = signed_in(&accounts, session.uuid, &session.name, proxy);
            let account = account::upsert(&mut accounts, account);
            account::save(&accounts)?;
            Ok(account)
        };
        match saved() {
            Err(e) if !retried && format!("{e:#}").contains("changed by another Manycraft") => retried = true,
            done => return done,
        }
    }
}

fn signed_in(accounts: &[Account], id: Uuid, name: &str, proxy: Option<&str>) -> Account {
    let old = accounts.iter().find(|a| a.id == id);
    Account { id, name: name.to_owned(), kind: AccountKind::Microsoft, proxy: proxy.map(str::to_owned).or_else(|| old.and_then(|a| a.proxy.clone())), color: old.and_then(|a| a.color) }
}

/// A sign-in again that signed in a different Microsoft account, named here, than the one it was for.
#[derive(Debug, PartialEq, Eq)]
pub struct WrongAccount(pub String);

impl fmt::Display for WrongAccount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "signed in as {}, a different account", self.0)
    }
}

impl std::error::Error for WrongAccount {}

fn login_offline(name: &str) -> Result<()> {
    let account = Account::offline(name)?;
    let mut accounts = account::load()?;
    println!("Offline account {} ({})", account.name, account.id);
    if accounts.iter().any(|a| a.id == account.id) {
        return Ok(());
    }
    account::upsert(&mut accounts, account);
    account::save(&accounts)
}

/// The saved account named `who` (case-insensitive) or with that id in any spelling.
pub fn find_account<'a>(accounts: &'a [Account], who: &str) -> Result<&'a Account> {
    let id = Uuid::parse_str(who).ok();
    let mut matches = accounts.iter().filter(|a| id == Some(a.id) || a.name.eq_ignore_ascii_case(who));
    let account = matches.next().with_context(|| format!("no saved account {who}"))?;
    if matches.next().is_some() {
        bail!("several accounts are named {who}, use the id");
    }
    Ok(account)
}

fn check(who: &str) -> Result<()> {
    let accounts = account::load()?;
    let session = session_for(find_account(&accounts, who)?)?;
    let now = SystemTime::now();
    if !session.is_usable(now) {
        bail!("no usable session for {}", session.name);
    }
    let minutes = session.expires_at.duration_since(now).map_or(0, |d| d.as_secs() / 60);
    println!("Session obtained for {} ({}), xuid {}, valid for {minutes} min", session.name, session.uuid, session.xuid);
    Ok(())
}

fn pick_client_id(runtime: Option<&str>, built: Option<&str>) -> String {
    [runtime, built]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|id| !id.is_empty())
        .unwrap_or(LIVE_CLIENT_ID)
        .to_owned()
}

struct Reply {
    status: u16,
    body: Vec<u8>,
}

impl Reply {
    fn is_ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    fn json<T: serde::de::DeserializeOwned>(&self, what: &str) -> Result<T> {
        serde_json::from_slice(&self.body).with_context(|| format!("unexpected answer from {what}"))
    }
}

fn host(url: &str) -> &str {
    url.split('/').nth(2).unwrap_or(url)
}

fn post_form(agent: &ureq::Agent, url: &str, form: &[(&str, &str)]) -> Result<Reply> {
    let res = agent.post(url).config().http_status_as_error(false).build().send_form(form.iter().copied());
    reply(url, res)
}

fn post_json(agent: &ureq::Agent, url: &str, body: &serde_json::Value) -> Result<Reply> {
    let res = agent.post(url).header("Accept", "application/json").config().http_status_as_error(false).build().send_json(body);
    reply(url, res)
}

fn get_bearer(agent: &ureq::Agent, url: &str, token: &str) -> Result<Reply> {
    let res = agent.get(url).header("Authorization", format!("Bearer {token}")).config().http_status_as_error(false).build().call();
    reply(url, res)
}

fn reply(url: &str, res: Result<ureq::http::Response<ureq::Body>, ureq::Error>) -> Result<Reply> {
    let mut res = res.with_context(|| format!("cannot reach {}", host(url)))?;
    let status = res.status().as_u16();
    let body = res.body_mut().with_config().limit(MAX_BODY).read_to_vec().with_context(|| format!("cannot read the answer from {}", host(url)))?;
    Ok(Reply { status, body })
}

#[derive(Deserialize)]
struct OauthError {
    error: String,
    #[serde(default)]
    error_description: String,
}

fn oauth_error(reply: &Reply) -> Option<OauthError> {
    serde_json::from_slice(&reply.body).ok()
}

fn microsoft_failure(reply: &Reply) -> anyhow::Error {
    match oauth_error(reply) {
        Some(e) if e.error == "invalid_client" || e.error == "unauthorized_client" => AuthError::InvalidClientId.into(),
        Some(e) => anyhow!("Microsoft sign-in failed: {}: {}", e.error, e.error_description.lines().next().unwrap_or_default()),
        None => anyhow!("Microsoft sign-in failed with status {}", reply.status),
    }
}

fn default_interval() -> u64 {
    DEFAULT_INTERVAL
}

fn device_code(reply: &Reply) -> Result<DeviceCode> {
    #[derive(Deserialize)]
    struct Raw {
        user_code: String,
        device_code: String,
        verification_uri: String,
        expires_in: u64,
        #[serde(default = "default_interval")]
        interval: u64,
    }
    if !reply.is_ok() {
        return Err(microsoft_failure(reply));
    }
    let raw: Raw = reply.json("Microsoft")?;
    Ok(DeviceCode { user_code: raw.user_code, verification_uri: raw.verification_uri, expires_in: raw.expires_in, interval: raw.interval, device_code: raw.device_code })
}

fn tokens(reply: &Reply) -> Result<Tokens> {
    #[derive(Deserialize)]
    struct Raw {
        access_token: String,
        refresh_token: Option<String>,
    }
    let raw: Raw = reply.json("Microsoft")?;
    Ok(Tokens { access_token: raw.access_token, refresh_token: raw.refresh_token })
}

fn token_poll(reply: &Reply) -> Result<Poll> {
    if reply.is_ok() {
        return tokens(reply).map(Poll::Done);
    }
    match oauth_error(reply).as_ref().map(|e| e.error.as_str()) {
        Some("authorization_pending") => Ok(Poll::Pending),
        Some("slow_down") => Ok(Poll::SlowDown),
        Some("expired_token") => Err(AuthError::CodeExpired.into()),
        Some("authorization_declined" | "access_denied") => Err(AuthError::Declined.into()),
        _ => Err(microsoft_failure(reply)),
    }
}

fn token_refresh(reply: &Reply) -> Result<Tokens> {
    if reply.is_ok() {
        return tokens(reply);
    }
    match oauth_error(reply).as_ref().map(|e| e.error.as_str()) {
        Some("invalid_grant" | "interaction_required") => Err(AuthError::SignInAgain.into()),
        _ => Err(microsoft_failure(reply)),
    }
}

fn xbox_failure(service: &str, reply: &Reply) -> anyhow::Error {
    #[derive(Deserialize)]
    struct Raw {
        #[serde(rename = "XErr")]
        code: Option<u64>,
    }
    match serde_json::from_slice::<Raw>(&reply.body).ok().and_then(|raw| raw.code) {
        Some(2_148_916_233) => AuthError::NoXboxProfile.into(),
        Some(2_148_916_235) => AuthError::RegionBlocked.into(),
        Some(2_148_916_236 | 2_148_916_237) => AuthError::AdultVerification.into(),
        Some(2_148_916_238) => AuthError::ChildAccount.into(),
        Some(code) => AuthError::XboxRejected(code).into(),
        None => anyhow!("{service} answered with status {}", reply.status),
    }
}

fn xbox_token(service: &str, reply: &Reply) -> Result<XboxToken> {
    #[derive(Deserialize)]
    struct Raw {
        #[serde(rename = "Token")]
        token: String,
        #[serde(rename = "DisplayClaims")]
        claims: Claims,
    }
    #[derive(Deserialize)]
    struct Claims {
        xui: Vec<Xui>,
    }
    #[derive(Deserialize)]
    struct Xui {
        uhs: String,
    }
    if !reply.is_ok() {
        return Err(xbox_failure(service, reply));
    }
    let raw: Raw = reply.json(service)?;
    let uhs = raw.claims.xui.into_iter().next().map(|x| x.uhs).with_context(|| format!("{service} answered without a user hash"))?;
    Ok(XboxToken { token: raw.token, uhs })
}

fn minecraft_login(reply: &Reply) -> Result<McToken> {
    #[derive(Deserialize)]
    struct Raw {
        access_token: String,
        expires_in: u64,
    }
    match reply.status {
        403 => return Err(AuthError::AppNotApproved.into()),
        _ if !reply.is_ok() => bail!("Minecraft services refused the sign-in with status {}", reply.status),
        _ => {}
    }
    let raw: Raw = reply.json("Minecraft services")?;
    Ok(McToken { access_token: raw.access_token, expires_in: raw.expires_in })
}

fn profile(reply: &Reply) -> Result<Profile> {
    #[derive(Deserialize)]
    struct Raw {
        id: String,
        name: String,
    }
    match reply.status {
        404 => return Err(AuthError::GameNotOwned.into()),
        _ if !reply.is_ok() => bail!("Minecraft services refused the profile request with status {}", reply.status),
        _ => {}
    }
    let raw: Raw = reply.json("Minecraft services")?;
    Ok(Profile { id: Uuid::parse_str(&raw.id).context("Minecraft services sent an invalid profile id")?, name: raw.name })
}

fn assemble(profile: Profile, mc: McToken, now: SystemTime) -> Session {
    Session {
        name: profile.name,
        uuid: profile.id,
        xuid: xuid(&mc.access_token),
        expires_at: now + Duration::from_secs(mc.expires_in),
        access_token: mc.access_token,
    }
}

fn xuid(jwt: &str) -> String {
    #[derive(Deserialize)]
    struct Claims {
        xuid: Option<String>,
    }
    jwt.split('.')
        .nth(1)
        .and_then(base64url_decode)
        .and_then(|bytes| serde_json::from_slice::<Claims>(&bytes).ok())
        .and_then(|claims| claims.xuid)
        .unwrap_or_default()
}

fn base64url_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0;
    for c in text.trim_end_matches('=').bytes() {
        let value = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        };
        acc = acc << 6 | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn one_refresh_lock_per_account() {
        let (a, b) = (Uuid::from_u128(0xa1), Uuid::from_u128(0xb2));
        assert!(Arc::ptr_eq(&super::refresh_lock(a), &super::refresh_lock(a)));
        assert!(!Arc::ptr_eq(&super::refresh_lock(a), &super::refresh_lock(b)));
    }

    #[test]
    fn offline_accounts_need_no_refresh_and_unsaved_microsoft_ones_do_not_pass() {
        assert!(super::refresh_account(&Account::offline("Alt").unwrap()).is_ok());
        let unsaved = Account { id: Uuid::from_u128(0xdead_beef), name: "Nobody".into(), kind: AccountKind::Microsoft, proxy: None, color: None };
        assert!(super::refresh_account(&unsaved).is_err());
    }

    use std::collections::{HashMap, VecDeque};
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use super::*;

    macro_rules! fixture {
        ($name:literal) => {
            include_str!(concat!("testdata/auth/", $name, ".json"))
        };
    }

    const NOTCH_ID: &str = "0e9e3a6c-6a1f-4b2b-9d57-2b1ac6f3c1d4";

    fn reply(status: u16, body: &str) -> Reply {
        Reply { status, body: body.as_bytes().to_vec() }
    }

    fn fail<T>(result: Result<T>) -> anyhow::Error {
        match result {
            Ok(_) => panic!("expected an error"),
            Err(e) => e,
        }
    }

    fn kind(e: &anyhow::Error) -> Option<&AuthError> {
        e.downcast_ref::<AuthError>()
    }

    #[test]
    fn device_code_parses() {
        let code = device_code(&reply(200, fixture!("devicecode"))).unwrap();
        assert_eq!(code.user_code, "A1B2C3D4");
        assert_eq!(code.verification_uri, "https://www.microsoft.com/link");
        assert_eq!(code.device_code, "FAKE-DEVICE-CODE-0000");
        assert_eq!((code.expires_in, code.interval), (900, 5));
    }

    #[test]
    fn device_code_without_interval_defaults_to_five_seconds() {
        let code = device_code(&reply(200, fixture!("devicecode_no_interval"))).unwrap();
        assert_eq!(code.interval, 5);
    }

    #[test]
    fn device_code_errors_map_to_invalid_client() {
        for body in [fixture!("token_invalid_client"), fixture!("token_unauthorized_client"), fixture!("token_public_client")] {
            assert_eq!(kind(&fail(device_code(&reply(400, body)))), Some(&AuthError::InvalidClientId));
        }
    }

    #[test]
    fn device_code_other_failure_names_the_cause() {
        let text = fail(device_code(&reply(500, fixture!("token_other")))).to_string();
        assert!(text.contains("server_error") && text.contains("AADSTS50000"), "{text}");
        assert!(fail(device_code(&reply(502, "<html>bad gateway</html>"))).to_string().contains("502"));
        assert!(fail(device_code(&reply(200, "{}"))).to_string().contains("unexpected answer"));
    }

    #[test]
    fn poll_answers_classify() {
        assert!(matches!(token_poll(&reply(400, fixture!("token_pending"))).unwrap(), Poll::Pending));
        assert!(matches!(token_poll(&reply(400, fixture!("token_slow_down"))).unwrap(), Poll::SlowDown));
        match token_poll(&reply(200, fixture!("token_ok"))).unwrap() {
            Poll::Done(t) => {
                assert_eq!(t.access_token, "FAKE-MSA-ACCESS-TOKEN");
                assert_eq!(t.refresh_token.as_deref(), Some("FAKE-MSA-REFRESH-TOKEN"));
            }
            _ => panic!("expected tokens"),
        }
        assert_eq!(kind(&fail(token_poll(&reply(400, fixture!("token_expired"))))), Some(&AuthError::CodeExpired));
        assert_eq!(kind(&fail(token_poll(&reply(400, fixture!("token_declined"))))), Some(&AuthError::Declined));
        assert_eq!(kind(&fail(token_poll(&reply(400, fixture!("token_invalid_client"))))), Some(&AuthError::InvalidClientId));
        assert!(kind(&fail(token_poll(&reply(400, fixture!("token_other"))))).is_none());
        assert!(fail(token_poll(&reply(503, ""))).to_string().contains("503"));
    }

    #[test]
    fn a_success_status_with_garbage_is_not_a_token() {
        assert!(token_poll(&reply(200, "nope")).is_err());
    }

    #[test]
    fn refresh_parses_and_maps_errors() {
        let t = token_refresh(&reply(200, fixture!("token_ok"))).unwrap();
        assert_eq!(t.refresh_token.as_deref(), Some("FAKE-MSA-REFRESH-TOKEN"));
        assert!(token_refresh(&reply(200, fixture!("token_ok_no_refresh"))).unwrap().refresh_token.is_none());
        assert_eq!(kind(&fail(token_refresh(&reply(400, fixture!("token_invalid_grant"))))), Some(&AuthError::SignInAgain));
        assert_eq!(kind(&fail(token_refresh(&reply(400, fixture!("token_invalid_client"))))), Some(&AuthError::InvalidClientId));
        assert!(kind(&fail(token_refresh(&reply(400, fixture!("token_other"))))).is_none());
    }

    #[test]
    fn xbox_tokens_parse() {
        let xbl = xbox_token("Xbox Live", &reply(200, fixture!("xbl_ok"))).unwrap();
        assert_eq!((xbl.token.as_str(), xbl.uhs.as_str()), ("FAKE-XBL-TOKEN", "1234567890123456789"));
        let xsts = xbox_token("XSTS", &reply(200, fixture!("xsts_ok"))).unwrap();
        assert_eq!(xsts.token, "FAKE-XSTS-TOKEN");
    }

    #[test]
    fn xbox_reply_without_user_hash_is_an_error() {
        assert!(fail(xbox_token("XSTS", &reply(200, fixture!("xsts_empty_claims")))).to_string().contains("user hash"));
    }

    #[test]
    fn xsts_error_codes_map() {
        let cases = [
            (fixture!("xsts_2148916233"), AuthError::NoXboxProfile),
            (fixture!("xsts_2148916235"), AuthError::RegionBlocked),
            (fixture!("xsts_2148916236"), AuthError::AdultVerification),
            (fixture!("xsts_2148916237"), AuthError::AdultVerification),
            (fixture!("xsts_2148916238"), AuthError::ChildAccount),
            (fixture!("xsts_2148916227"), AuthError::XboxRejected(2_148_916_227)),
        ];
        for (body, expected) in cases {
            assert_eq!(kind(&fail(xbox_token("XSTS", &reply(401, body)))), Some(&expected), "{body}");
        }
    }

    #[test]
    fn xbox_error_without_code_reports_the_status() {
        let text = fail(xbox_token("Xbox Live", &reply(400, "oops"))).to_string();
        assert!(text.contains("Xbox Live") && text.contains("400"), "{text}");
    }

    #[test]
    fn minecraft_login_parses_and_maps_errors() {
        let mc = minecraft_login(&reply(200, fixture!("mc_login_ok"))).unwrap();
        assert_eq!(mc.expires_in, 86400);
        assert!(mc.access_token.starts_with("eyJ"));
        assert_eq!(kind(&fail(minecraft_login(&reply(403, fixture!("mc_login_forbidden"))))), Some(&AuthError::AppNotApproved));
        let text = fail(minecraft_login(&reply(401, fixture!("mc_login_unauthorized")))).to_string();
        assert!(text.contains("401"), "{text}");
    }

    #[test]
    fn profile_parses_and_maps_errors() {
        let p = profile(&reply(200, fixture!("profile_ok"))).unwrap();
        assert_eq!(p.name, "TestPlayer");
        assert_eq!(p.id.to_string(), NOTCH_ID);
        assert_eq!(kind(&fail(profile(&reply(404, fixture!("profile_not_found"))))), Some(&AuthError::GameNotOwned));
        assert!(fail(profile(&reply(401, "{}"))).to_string().contains("401"));
        assert!(fail(profile(&reply(200, r#"{"id":"xyz","name":"A"}"#))).to_string().contains("invalid profile id"));
    }

    #[test]
    fn session_takes_xuid_from_the_minecraft_token() {
        let mc = minecraft_login(&reply(200, fixture!("mc_login_ok"))).unwrap();
        let profile = profile(&reply(200, fixture!("profile_ok"))).unwrap();
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let session = assemble(profile, mc, now);
        assert_eq!(session.xuid, "2535428674381920");
        assert_eq!(session.name, "TestPlayer");
        assert_eq!(session.uuid.to_string(), NOTCH_ID);
        assert_eq!(session.expires_at, now + Duration::from_secs(86400));
        assert!(session.is_usable(now));
        assert!(!session.is_usable(session.expires_at));
    }

    #[test]
    fn opaque_minecraft_token_gives_empty_xuid() {
        let mc = minecraft_login(&reply(200, fixture!("mc_login_opaque_token"))).unwrap();
        assert_eq!(xuid(&mc.access_token), "");
        assert_eq!(xuid("a.b.c"), "");
        assert_eq!(xuid(""), "");
    }

    #[test]
    fn offline_session_is_name_uuid_and_a_zero_token() {
        let account = Account::offline("Notch").unwrap();
        let now = SystemTime::now();
        let session = session_for(&account).unwrap();
        assert_eq!((session.name.as_str(), session.access_token.as_str(), session.xuid.as_str()), ("Notch", "0", ""));
        assert_eq!(session.uuid.to_string(), "b50ad385-829d-3141-a216-7e7d7539ba7f");
        assert!(session.is_usable(now + Duration::from_secs(5 * 365 * 24 * 3600)));
        assert_eq!(offline_session(&account, now).expires_at, now + OFFLINE_LIFETIME);
    }

    #[test]
    fn empty_token_is_never_usable() {
        let session = Session { name: "A".into(), uuid: Uuid::nil(), access_token: String::new(), xuid: String::new(), expires_at: SystemTime::now() + Duration::from_secs(60) };
        assert!(!session.is_usable(SystemTime::now()));
    }

    #[test]
    fn debug_output_hides_the_token() {
        let session = Session { name: "A".into(), uuid: Uuid::nil(), access_token: "SECRET-TOKEN".into(), xuid: "42".into(), expires_at: SystemTime::UNIX_EPOCH };
        assert!(!format!("{session:?}").contains("SECRET-TOKEN"));
    }

    #[test]
    fn base64url_decodes_with_and_without_padding() {
        assert_eq!(base64url_decode("aGVsbG8").unwrap(), b"hello");
        assert_eq!(base64url_decode("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64url_decode("Pz8_Pz8-").unwrap(), b"?????>");
        assert!(base64url_decode("a+b").is_none());
    }

    #[test]
    fn client_id_prefers_runtime_then_build_time() {
        assert_eq!(pick_client_id(Some(" run "), Some("built")), "run");
        assert_eq!(pick_client_id(None, Some("built")), "built");
        assert_eq!(pick_client_id(Some("  "), Some("built")), "built");
        assert_eq!(pick_client_id(None, None), LIVE_CLIENT_ID);
        assert_eq!(pick_client_id(Some(""), Some("")), LIVE_CLIENT_ID);
    }

    #[test]
    fn argument_parsing() {
        let args = |a: &[&str]| a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert!(matches!(parse_args(&args(&[])), Ok(Command::Login { proxy: None })));
        assert!(matches!(parse_args(&args(&["--proxy", "socks5://h:1"])), Ok(Command::Login { proxy: Some(p) }) if p == "socks5://h:1"));
        assert!(matches!(parse_args(&args(&["--offline", "Alex"])), Ok(Command::Offline { name }) if name == "Alex"));
        assert!(parse_args(&args(&["--offline"])).is_err());
        assert!(matches!(parse_args(&args(&["--check", "Alex"])), Ok(Command::Check { who }) if who == "Alex"));
        assert!(parse_args(&args(&["--check"])).is_err());
        assert!(parse_args(&args(&["--proxy"])).is_err());
        assert!(parse_args(&args(&["--nope"])).is_err());
        assert!(parse_args(&args(&["--check", "a", "b"])).is_err());
    }

    #[test]
    fn every_error_has_a_message_naming_the_fix() {
        let all = [
            AuthError::InvalidClientId,
            AuthError::CodeExpired,
            AuthError::Declined,
            AuthError::SignInAgain,
            AuthError::NoXboxProfile,
            AuthError::RegionBlocked,
            AuthError::AdultVerification,
            AuthError::ChildAccount,
            AuthError::XboxRejected(7),
            AuthError::AppNotApproved,
            AuthError::GameNotOwned,
        ];
        let texts: Vec<String> = all.iter().map(ToString::to_string).collect();
        for (i, a) in texts.iter().enumerate() {
            assert!(!a.is_empty());
            assert!(texts.iter().skip(i + 1).all(|b| a != b));
        }
        assert!(texts[8].contains('7'));
        assert!(texts[0].contains(CLIENT_ID_VAR));
    }

    struct Seen {
        path: String,
        headers: String,
        body: String,
    }

    type Routes = Vec<(&'static str, Vec<(u16, &'static str)>)>;

    fn serve(routes: Routes) -> (String, Arc<Mutex<Vec<Seen>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        let mut queues: HashMap<&str, VecDeque<(u16, &str)>> = routes.into_iter().map(|(path, replies)| (path, replies.into())).collect();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let path = line.split_whitespace().nth(1).unwrap().to_owned();
                let mut headers = String::new();
                let mut length = 0;
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = v.trim().parse().unwrap();
                    }
                    headers.push_str(&line.to_ascii_lowercase());
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                log.lock().unwrap().push(Seen { path: path.clone(), headers, body: String::from_utf8(body).unwrap() });
                let (status, text) = match queues.get_mut(path.as_str()) {
                    Some(q) if q.len() > 1 => q.pop_front().unwrap(),
                    Some(q) => *q.front().unwrap(),
                    None => (404, "{}"),
                };
                let head = format!("HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", text.len());
                stream.write_all(head.as_bytes()).unwrap();
                stream.write_all(text.as_bytes()).unwrap();
            }
        });
        (base, seen)
    }

    fn flow(base: &str) -> Flow {
        Flow {
            agent: net::agent(None).unwrap(),
            ep: Endpoints {
                device_url: format!("{base}/oauth/devicecode"),
                token_url: format!("{base}/oauth/token"),
                scope: AZURE_SCOPE,
                rps_prefix: "d=",
                response_type: None,
                device_grant: DEVICE_GRANT,
                xbl: format!("{base}/xbl"),
                xsts: format!("{base}/xsts"),
                mc_login: format!("{base}/mc/login"),
                mc_profile: format!("{base}/mc/profile"),
            },
            client_id: "test-client".into(),
        }
    }

    fn live_flow(base: &str) -> Flow {
        let azure = flow(base);
        Flow {
            ep: Endpoints { device_url: azure.ep.device_url.clone(), token_url: azure.ep.token_url.clone(), xbl: azure.ep.xbl.clone(), ..Endpoints::live() },
            client_id: LIVE_CLIENT_ID.into(),
            ..azure
        }
    }

    #[test]
    fn endpoints_follow_the_client_id() {
        let live = Endpoints::for_client(LIVE_CLIENT_ID);
        assert_eq!((live.device_url.as_str(), live.token_url.as_str()), (LIVE_DEVICE_URL, LIVE_TOKEN_URL));
        assert_eq!(live.rps_prefix, "t=");
        let azure = Endpoints::for_client("custom");
        assert_eq!((azure.device_url.as_str(), azure.token_url.as_str()), (AZURE_DEVICE_URL, AZURE_TOKEN_URL));
        assert_eq!(azure.rps_prefix, "d=");
    }

    #[test]
    fn live_flow_sends_the_live_form_fields_and_t_ticket() {
        let (base, seen) = serve(vec![
            ("/oauth/devicecode", vec![(200, fixture!("devicecode"))]),
            ("/oauth/token", vec![(400, fixture!("token_pending")), (200, fixture!("token_ok")), (200, fixture!("token_ok"))]),
            ("/xbl", vec![(200, fixture!("xbl_ok"))]),
            ("/xsts", vec![(401, fixture!("xsts_2148916233"))]),
        ]);
        let flow = live_flow(&base);
        let device = flow.start().unwrap();
        let start = &requests(&seen, "/oauth/devicecode")[0].1;
        assert!(start.contains("client_id=00000000402b5328") && start.contains("response_type=device_code"), "{start}");
        assert!(start.contains("scope=service%3A%3Auser.auth.xboxlive.com%3A%3AMBI_SSL"), "{start}");
        let tokens = flow.poll_with(&device, |_| {}).unwrap();
        let poll = &requests(&seen, "/oauth/token")[0].1;
        assert!(poll.contains("client_id=00000000402b5328") && poll.contains("device_code=FAKE-DEVICE-CODE-0000"), "{poll}");
        assert!(poll.contains("grant_type=device_code&"), "{poll}");
        flow.refresh("OLD").unwrap();
        let refresh = &requests(&seen, "/oauth/token")[2].1;
        assert!(refresh.contains("grant_type=refresh_token") && refresh.contains("refresh_token=OLD") && refresh.contains("MBI_SSL"), "{refresh}");
        assert!(!refresh.contains("offline_access"), "{refresh}");
        let _ = flow.session(&tokens);
        let body: serde_json::Value = serde_json::from_str(&requests(&seen, "/xbl")[0].1).unwrap();
        assert_eq!(body["Properties"]["RpsTicket"], "t=FAKE-MSA-ACCESS-TOKEN");
    }

    #[test]
    fn access_denied_stops_polling_like_a_declined_code() {
        assert_eq!(kind(&fail(token_poll(&reply(400, r#"{"error":"access_denied"}"#)))), Some(&AuthError::Declined));
    }

    fn code(expires_in: u64) -> DeviceCode {
        DeviceCode { user_code: "A1B2C3D4".into(), verification_uri: "u".into(), expires_in, interval: 5, device_code: "DEV".into() }
    }

    fn requests(seen: &Arc<Mutex<Vec<Seen>>>, path: &str) -> Vec<(String, String)> {
        seen.lock().unwrap().iter().filter(|s| s.path == path).map(|s| (s.headers.clone(), s.body.clone())).collect()
    }

    #[test]
    fn full_flow_against_a_local_server() {
        let (base, seen) = serve(vec![
            ("/oauth/devicecode", vec![(200, fixture!("devicecode"))]),
            ("/oauth/token", vec![(400, fixture!("token_pending")), (400, fixture!("token_slow_down")), (200, fixture!("token_ok"))]),
            ("/xbl", vec![(200, fixture!("xbl_ok"))]),
            ("/xsts", vec![(200, fixture!("xsts_ok"))]),
            ("/mc/login", vec![(200, fixture!("mc_login_ok"))]),
            ("/mc/profile", vec![(200, fixture!("profile_ok"))]),
        ]);
        let flow = flow(&base);

        let device = flow.start().unwrap();
        let start = &requests(&seen, "/oauth/devicecode")[0].1;
        assert!(start.contains("client_id=test-client") && start.contains("scope=XboxLive.signin"), "{start}");
        assert!(start.contains("offline_access"), "{start}");

        let mut sleeps = Vec::new();
        let tokens = flow.poll_with(&device, |d| sleeps.push(d.as_secs())).unwrap();
        assert_eq!(sleeps, [5, 5, 10]);
        let polls = requests(&seen, "/oauth/token");
        assert_eq!(polls.len(), 3);
        assert!(polls[0].1.contains("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code"), "{}", polls[0].1);
        assert!(polls[0].1.contains("device_code=FAKE-DEVICE-CODE-0000") && polls[0].1.contains("client_id=test-client"));

        let session = flow.session(&tokens).unwrap();
        assert_eq!(session.name, "TestPlayer");
        assert_eq!(session.uuid.to_string(), NOTCH_ID);
        assert_eq!(session.xuid, "2535428674381920");
        assert!(session.is_usable(SystemTime::now()));

        let (headers, body) = &requests(&seen, "/xbl")[0];
        assert!(headers.contains("accept: application/json") && headers.contains("content-type: application/json"), "{headers}");
        let body: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(body["Properties"]["AuthMethod"], "RPS");
        assert_eq!(body["Properties"]["SiteName"], "user.auth.xboxlive.com");
        assert_eq!(body["Properties"]["RpsTicket"], "d=FAKE-MSA-ACCESS-TOKEN");
        assert_eq!(body["RelyingParty"], "http://auth.xboxlive.com");
        assert_eq!(body["TokenType"], "JWT");

        let body: serde_json::Value = serde_json::from_str(&requests(&seen, "/xsts")[0].1).unwrap();
        assert_eq!(body["Properties"]["SandboxId"], "RETAIL");
        assert_eq!(body["Properties"]["UserTokens"], json!(["FAKE-XBL-TOKEN"]));
        assert_eq!(body["RelyingParty"], "rp://api.minecraftservices.com/");

        let body: serde_json::Value = serde_json::from_str(&requests(&seen, "/mc/login")[0].1).unwrap();
        assert_eq!(body["identityToken"], "XBL3.0 x=1234567890123456789;FAKE-XSTS-TOKEN");

        let (headers, _) = &requests(&seen, "/mc/profile")[0];
        assert!(headers.contains(&format!("authorization: bearer {}", session.access_token.to_ascii_lowercase())), "{headers}");
    }

    #[test]
    fn signing_in_as_someone_else_is_refused_before_anything_is_stored() {
        let (base, seen) = serve(vec![
            ("/xbl", vec![(200, fixture!("xbl_ok"))]),
            ("/xsts", vec![(200, fixture!("xsts_ok"))]),
            ("/mc/login", vec![(200, fixture!("mc_login_ok"))]),
            ("/mc/profile", vec![(200, fixture!("profile_ok"))]),
        ]);
        let tokens = Tokens { access_token: "A".into(), refresh_token: Some("R".into()) };
        let e = save_sign_in(&flow(&base), tokens, None, Some(Uuid::from_u128(1)), &mut || panic!("asked to store")).unwrap_err();
        assert_eq!(e.downcast_ref::<WrongAccount>(), Some(&WrongAccount("TestPlayer".into())));
        assert!(e.to_string().contains("TestPlayer"), "{e}");
        assert_eq!(requests(&seen, "/mc/profile").len(), 1);
        let tokens = Tokens { access_token: "A".into(), refresh_token: Some("R".into()) };
        let cancelled = save_sign_in(&flow(&base), tokens, None, None, &mut || false).unwrap_err();
        assert!(cancelled.to_string().contains("cancelled"), "{cancelled}");
        let none = Tokens { access_token: "A".into(), refresh_token: None };
        assert!(save_sign_in(&flow(&base), none, None, Some(Uuid::from_u128(1)), &mut || panic!("asked to store")).unwrap_err().downcast_ref::<WrongAccount>().is_none());
    }

    #[test]
    fn signing_in_again_keeps_the_proxy_unless_a_new_one_is_given() {
        let (me, other) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let saved = [
            Account { id: other, name: "Other".into(), kind: AccountKind::Microsoft, proxy: Some("socks5://other:1".into()), color: None },
            Account { id: me, name: "Old".into(), kind: AccountKind::Microsoft, proxy: Some("socks5://mine:1".into()), color: None },
        ];
        let again = signed_in(&saved, me, "New", None);
        assert_eq!((again.name.as_str(), again.proxy.as_deref()), ("New", Some("socks5://mine:1")));
        assert_eq!(signed_in(&saved, me, "New", Some("http://new:2")).proxy.as_deref(), Some("http://new:2"));
        assert_eq!(signed_in(&saved, Uuid::from_u128(3), "Fresh", None).proxy, None);
    }

    #[test]
    fn refresh_sends_the_saved_token_and_scope() {
        let (base, seen) = serve(vec![("/oauth/token", vec![(200, fixture!("token_ok"))])]);
        let tokens = flow(&base).refresh("OLD-REFRESH").unwrap();
        assert_eq!(tokens.refresh_token.as_deref(), Some("FAKE-MSA-REFRESH-TOKEN"));
        let body = &requests(&seen, "/oauth/token")[0].1;
        assert!(body.contains("grant_type=refresh_token") && body.contains("refresh_token=OLD-REFRESH") && body.contains("client_id=test-client"), "{body}");
        assert!(body.contains("offline_access"), "{body}");
    }

    #[test]
    fn revoked_refresh_token_asks_to_sign_in_again() {
        let (base, _) = serve(vec![("/oauth/token", vec![(400, fixture!("token_invalid_grant"))])]);
        assert_eq!(kind(&fail(flow(&base).refresh("OLD"))), Some(&AuthError::SignInAgain));
    }

    #[test]
    fn declined_code_stops_polling() {
        let (base, seen) = serve(vec![("/oauth/token", vec![(400, fixture!("token_pending")), (400, fixture!("token_declined"))])]);
        let e = fail(flow(&base).poll_with(&code(900), |_| {}));
        assert_eq!(kind(&e), Some(&AuthError::Declined));
        assert_eq!(requests(&seen, "/oauth/token").len(), 2);
    }

    #[test]
    fn polling_stops_when_cancelled_and_asks_no_more() {
        let (base, seen) = serve(vec![("/oauth/token", vec![(400, fixture!("token_pending"))])]);
        let sleeps = std::cell::Cell::new(0);
        let e = fail(flow(&base).poll_loop(&code(900), |_| sleeps.set(sleeps.get() + 1), || sleeps.get() >= 2));
        assert_eq!(kind(&e), Some(&AuthError::Cancelled));
        assert_eq!(requests(&seen, "/oauth/token").len(), 1);
    }

    #[test]
    fn an_unset_flag_waits_the_whole_time() {
        let started = Instant::now();
        sleep_unless(&AtomicBool::new(false), Duration::from_millis(300));
        assert!(started.elapsed() >= Duration::from_millis(300));
    }

    #[test]
    fn a_set_flag_cuts_the_wait_short() {
        let cancel = AtomicBool::new(true);
        let started = Instant::now();
        sleep_unless(&cancel, Duration::from_secs(30));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn polling_gives_up_when_the_code_lifetime_is_spent() {
        let (base, seen) = serve(vec![("/oauth/token", vec![(400, fixture!("token_pending"))])]);
        let mut sleeps = Vec::new();
        let e = fail(flow(&base).poll_with(&code(12), |d| sleeps.push(d.as_secs())));
        assert_eq!(kind(&e), Some(&AuthError::CodeExpired));
        assert_eq!(sleeps, [5, 5]);
        assert_eq!(requests(&seen, "/oauth/token").len(), 2);
    }

    #[test]
    fn polling_survives_two_network_failures_but_not_three() {
        let dead = {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            format!("http://{}", listener.local_addr().unwrap())
        };
        let mut sleeps = 0;
        let e = fail(flow(&dead).poll_with(&code(900), |_| sleeps += 1));
        assert!(e.to_string().contains("cannot reach"), "{e}");
        assert_eq!(sleeps, 3);
    }

    #[test]
    fn polling_rides_out_server_hiccups() {
        let (base, seen) = serve(vec![("/oauth/token", vec![(503, "<html>unavailable</html>"), (429, "{}"), (400, fixture!("token_pending")), (503, ""), (200, fixture!("token_ok"))])]);
        let tokens = flow(&base).poll_with(&code(900), |_| {}).unwrap();
        assert_eq!(tokens.access_token, "FAKE-MSA-ACCESS-TOKEN");
        assert_eq!(requests(&seen, "/oauth/token").len(), 5);
    }

    #[test]
    fn polling_stops_after_three_server_errors_in_a_row() {
        let (base, seen) = serve(vec![("/oauth/token", vec![(503, "")])]);
        let e = fail(flow(&base).poll_with(&code(900), |_| {}));
        assert!(e.to_string().contains("503"), "{e}");
        assert_eq!(requests(&seen, "/oauth/token").len(), 3);
    }

    #[test]
    fn check_finds_accounts_by_any_id_spelling_or_name() {
        let account = Account::offline("Notch").unwrap();
        let accounts = [account.clone()];
        let find = |who: &str| find_account(&accounts, who).map(|a| a.name.clone());
        for who in ["Notch", "notch", "b50ad385-829d-3141-a216-7e7d7539ba7f", "B50AD385-829D-3141-A216-7E7D7539BA7F", "b50ad385829d3141a2167e7d7539ba7f"] {
            assert_eq!(find(who).unwrap(), "Notch", "{who}");
        }
        assert!(find("Other").is_err());
        let twins = [Account::offline("Same").unwrap(), Account { id: Uuid::from_u128(9), ..Account::offline("same").unwrap() }];
        assert!(find_account(&twins, "SAME").is_err());
        assert_eq!(find_account(&twins, &Uuid::from_u128(9).to_string()).unwrap().id, Uuid::from_u128(9));
    }

    #[test]
    fn chain_stops_at_the_first_failing_step() {
        let (base, seen) = serve(vec![("/xbl", vec![(200, fixture!("xbl_ok"))]), ("/xsts", vec![(401, fixture!("xsts_2148916233"))])]);
        let tokens = Tokens { access_token: "T".into(), refresh_token: None };
        assert_eq!(kind(&fail(flow(&base).session(&tokens))), Some(&AuthError::NoXboxProfile));
        assert!(requests(&seen, "/mc/login").is_empty());
    }

    #[test]
    fn unowned_game_surfaces_from_the_chain() {
        let (base, _) = serve(vec![
            ("/xbl", vec![(200, fixture!("xbl_ok"))]),
            ("/xsts", vec![(200, fixture!("xsts_ok"))]),
            ("/mc/login", vec![(200, fixture!("mc_login_ok"))]),
            ("/mc/profile", vec![(404, fixture!("profile_not_found"))]),
        ]);
        let tokens = Tokens { access_token: "T".into(), refresh_token: None };
        assert_eq!(kind(&fail(flow(&base).session(&tokens))), Some(&AuthError::GameNotOwned));
    }
}
