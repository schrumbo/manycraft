//! What the vault answers: its own discovery document, an allowlist of Mojang endpoints with the real login swapped in
//! for the stand-in, and a refusal for everything else.

use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::http::{self, Request, Response};
use super::keys::Cell;
use super::server::Handler;
use super::{Refresh, StandIn};

/// Where the game's login library fetches the discovery document.
pub(super) const DISCOVERY_PATH: &str = "/minecraft/client";

const SESSION: &str = "https://sessionserver.mojang.com";
const SERVICES: &str = "https://api.minecraftservices.com";
const ACCOUNTS: &str = "https://api.mojang.com";
const MAX_REPLY: u64 = 4 * 1024 * 1024;
const RENEW_AHEAD: Duration = Duration::from_secs(5 * 60);
const IO_TIMEOUT: Duration = Duration::from_secs(30);

/// Where a request carries the login.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Carrier {
    /// Public: forwarded without any login.
    None,
    /// `Authorization: Bearer`.
    Bearer,
    /// The `accessToken` field of a JSON body (joining a server).
    Body,
}

struct Route {
    methods: &'static [&'static str],
    path: &'static str,
    host: &'static str,
    carrier: Carrier,
}

const fn route(methods: &'static [&'static str], path: &'static str, host: &'static str, carrier: Carrier) -> Route {
    Route { methods, path, host, carrier }
}

const GET: &[&str] = &["GET"];
const POST: &[&str] = &["POST"];
const CHANGE: &[&str] = &["GET", "POST", "PUT"];

/// Every endpoint the vault forwards; a trailing `{}` is one path segment of a name or id.
const ROUTES: &[Route] = &[
    route(POST, "/session/minecraft/join", SESSION, Carrier::Body),
    route(GET, "/session/minecraft/hasJoined", SESSION, Carrier::None),
    route(GET, "/session/minecraft/profile/{}", SESSION, Carrier::None),
    route(GET, "/publickeys", SERVICES, Carrier::None),
    route(CHANGE, "/player/attributes", SERVICES, Carrier::Bearer),
    route(POST, "/player/certificates", SERVICES, Carrier::Bearer),
    route(POST, "/player/report", SERVICES, Carrier::Bearer),
    route(GET, "/privacy/blocklist", SERVICES, Carrier::Bearer),
    route(GET, "/privileges", SERVICES, Carrier::Bearer),
    route(CHANGE, "/friends", SERVICES, Carrier::Bearer),
    route(CHANGE, "/presence", SERVICES, Carrier::Bearer),
    route(POST, "/events", SERVICES, Carrier::Bearer),
    route(POST, "/profiles/minecraft", ACCOUNTS, Carrier::None),
    route(GET, "/users/profiles/minecraft/{}", ACCOUNTS, Carrier::None),
    route(GET, "/minecraft/profile/lookup/name/{}", SERVICES, Carrier::None),
    route(POST, "/minecraft/profile/lookup/bulk/byname", SERVICES, Carrier::None),
];

fn find(method: &str, path: &str) -> Option<&'static Route> {
    ROUTES.iter().find(|r| r.methods.contains(&method) && matches(r.path, path))
}

fn matches(pattern: &str, path: &str) -> bool {
    match pattern.strip_suffix("{}") {
        Some(prefix) => path.strip_prefix(prefix).is_some_and(|rest| (1..=64).contains(&rest.len()) && rest.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')),
        None => pattern == path,
    }
}

/// The discovery document of a vault at `base`: every endpoint points back at the vault, texture addresses stay Mojang's.
fn discovery(base: &str) -> Vec<u8> {
    let at = |path: &str| json!({ "uri": format!("{base}{path}") });
    let doc = json!({
        "environment": "prod",
        "product": "minecraft",
        "discovery": {
            "authentication": { "endpoints": { "getPublicKeys": at("/publickeys"), "loginXbox": at("/authentication/login_with_xbox") } },
            "session": { "endpoints": {
                "getProfileById": at("/session/minecraft/profile/{profileId}"),
                "verify": at("/session/minecraft/hasJoined"),
                "join": at("/session/minecraft/join"),
            } },
            "player": { "endpoints": {
                "updatePresence": at("/presence"),
                "sendReport": at("/player/report"),
                "getAttributes": at("/player/attributes"),
                "getFriends": at("/friends"),
                "getCertificates": at("/player/certificates"),
                "updateAttributes": at("/player/attributes"),
                "updateFriends": at("/friends"),
                "getBlocklist": at("/privacy/blocklist"),
            } },
            "profiles": { "endpoints": {
                "getManyByName": at("/profiles/minecraft"),
                "getByName": at("/users/profiles/minecraft/{name}"),
                "getTexture": { "validUris": ["https://textures.minecraft.net/texture/{textureId}", "http://textures.minecraft.net/texture/{textureId}"] },
            } },
            "telemetry": { "endpoints": { "sendEvents": at("/events") } },
        },
    });
    doc.to_string().into_bytes()
}

/// One forwarded request to Mojang. Debug output leaves out the headers and the body, which carry the login.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct Call {
    pub method: String,
    pub url: String,
    pub headers: Vec<(&'static str, String)>,
    pub body: Vec<u8>,
}

impl std::fmt::Debug for Call {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Call").field("method", &self.method).field("url", &self.url).finish_non_exhaustive()
    }
}

/// Mojang's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Reply {
    pub status: u16,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
}

/// Sends forwarded requests on to Mojang.
pub(super) trait Upstream: Send + Sync {
    fn send(&self, call: &Call) -> Result<Reply>;
}

/// Mojang over TLS, through the account's proxy when it has one; never follows redirects.
pub(super) struct Mojang(ureq::Agent);

impl Mojang {
    pub(super) fn new(proxy: Option<&str>) -> Result<Self> {
        Ok(Self(super::super::net::agent_https(proxy)?))
    }
}

impl Upstream for Mojang {
    fn send(&self, call: &Call) -> Result<Reply> {
        let mut request = ureq::http::Request::builder().method(call.method.as_str()).uri(call.url.as_str());
        for (name, value) in &call.headers {
            request = request.header(*name, value.as_str());
        }
        let request = request.body(call.body.clone()).context("the request cannot be forwarded")?;
        let request = self.0.configure_request(request).max_redirects(0).http_status_as_error(false).https_only(true).build();
        let mut response = self.0.run(request).context("Mojang did not answer")?;
        let content_type = response.headers().get("content-type").and_then(|v| v.to_str().ok()).map(str::to_owned);
        let body = response.body_mut().with_config().limit(MAX_REPLY).read_to_vec().context("Mojang's answer could not be read")?;
        Ok(Reply { status: response.status().as_u16(), content_type, body })
    }
}

/// The state of one vault: its stand-in, the account's login and where requests go.
pub(super) struct Vaulted {
    stand_in: StandIn,
    base: String,
    key: Arc<Cell>,
    upstream: Arc<dyn Upstream>,
    refresh: Option<Refresh>,
}

impl Vaulted {
    pub(super) fn new(stand_in: StandIn, base: String, key: Arc<Cell>, upstream: Arc<dyn Upstream>, refresh: Option<Refresh>) -> Self {
        Self { stand_in, base, key, upstream, refresh }
    }

    /// The answer to `request`.
    pub(super) fn answer(&self, request: &Request) -> Response {
        if request.method == "GET" && request.path == DISCOVERY_PATH {
            return Response::json(200, discovery(&self.base));
        }
        let Some(route) = find(&request.method, &request.path) else { return self.refuse(request, 404, "not an allowed endpoint") };
        let mut url = format!("{}{}", route.host, request.path);
        if let Some(query) = &request.query {
            url.push('?');
            url.push_str(query);
        }
        let mut headers: Vec<(&'static str, String)> = ["content-type", "accept"].into_iter().filter_map(|n| request.header(n).map(|v| (n, v.to_owned()))).collect();
        match route.carrier {
            Carrier::None => self.forward(request, &Call { method: request.method.clone(), url, headers, body: request.body.clone() }),
            Carrier::Bearer => {
                if !request.header("authorization").and_then(|v| v.strip_prefix("Bearer ")).is_some_and(|t| self.is_stand_in(t)) {
                    return self.refuse(request, 401, "no stand-in");
                }
                let index = headers.len();
                headers.push(("authorization", String::new()));
                let call = Call { method: request.method.clone(), url, headers, body: request.body.clone() };
                self.with_login(request, |token| {
                    let mut call = call.clone();
                    call.headers[index].1 = format!("Bearer {token}");
                    call
                })
            }
            Carrier::Body => {
                let Ok(Value::Object(mut body)) = serde_json::from_slice::<Value>(&request.body) else { return self.refuse(request, 400, "not a JSON object") };
                if !body.get("accessToken").and_then(Value::as_str).is_some_and(|t| self.is_stand_in(t)) {
                    return self.refuse(request, 401, "no stand-in");
                }
                self.with_login(request, |token| {
                    body.insert("accessToken".to_owned(), Value::String(token.to_owned()));
                    Call { method: request.method.clone(), url: url.clone(), headers: headers.clone(), body: Value::Object(body.clone()).to_string().into_bytes() }
                })
            }
        }
    }

    fn is_stand_in(&self, presented: &str) -> bool {
        self.stand_in.matches(presented.as_bytes())
    }

    fn with_login(&self, request: &Request, mut call: impl FnMut(&str) -> Call) -> Response {
        let Some(token) = self.login(None) else { return self.refuse(request, 401, "no login for this account") };
        match self.upstream.send(&call(&token)) {
            Ok(reply) if reply.status == 401 && self.refresh.is_some() => match self.login(Some(&token)).filter(|next| *next != token) {
                Some(next) => self.forward(request, &call(&next)),
                None => passed(reply),
            },
            outcome => self.reply(request, outcome),
        }
    }

    /// The login to use: the current one while it stays valid a while longer, else a renewed one. `failed` is a login
    /// Mojang just refused, which is renewed even when it looks valid.
    fn login(&self, failed: Option<&str>) -> Option<String> {
        let fresh = |at: SystemTime| self.key.current(at).filter(|t| Some(t.as_str()) != failed);
        if let Some(token) = fresh(SystemTime::now() + RENEW_AHEAD) {
            return Some(token);
        }
        if let Some(refresh) = &self.refresh {
            self.key.renew(refresh, || fresh(SystemTime::now() + RENEW_AHEAD).is_some());
        }
        fresh(SystemTime::now())
    }

    fn forward(&self, request: &Request, call: &Call) -> Response {
        self.reply(request, self.upstream.send(call))
    }

    fn reply(&self, request: &Request, outcome: Result<Reply>) -> Response {
        match outcome {
            Ok(reply) => passed(reply),
            Err(e) => {
                eprintln!("vault: {} {} failed: {e:#}", request.method, self.shown(&request.path));
                Response::text(502, "Mojang did not answer")
            }
        }
    }

    fn refuse(&self, request: &Request, status: u16, why: &str) -> Response {
        eprintln!("vault: refused {} {}: {why}", self.shown(&request.method), self.shown(&request.path));
        Response::text(status, "refused by Manycraft")
    }

    /// `text` fit for a log line: short, printable, without the stand-in.
    fn shown(&self, text: &str) -> String {
        let clean: String = text.replace(self.stand_in.as_str(), "<stand-in>").chars().filter(|c| !c.is_control()).take(120).collect();
        clean
    }
}

fn passed(reply: Reply) -> Response {
    Response { status: reply.status, content_type: reply.content_type, body: reply.body }
}

/// Answers each connection of the vault with `vaulted`.
pub(super) fn handler(vaulted: Vaulted) -> Handler {
    let vaulted = Arc::new(vaulted);
    Arc::new(move |mut stream: TcpStream| {
        let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
        let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
        let response = match http::read(&mut stream) {
            Ok(request) => vaulted.answer(&request),
            Err(_) => Response::text(400, "bad request"),
        };
        let _ = http::write(&mut stream, &response);
    })
}