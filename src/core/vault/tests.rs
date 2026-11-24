use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use anyhow::{Result, bail};
use uuid::Uuid;

use super::routes::{Call, Reply, Upstream};
use super::*;

const REAL: &str = "REAL-MINECRAFT-ACCESS-TOKEN";

#[derive(Default)]
struct Fake {
    calls: Mutex<Vec<Call>>,
    refuse: Mutex<Vec<String>>,
    down: bool,
}

impl Upstream for Fake {
    fn send(&self, call: &Call) -> Result<Reply> {
        if self.down {
            bail!("unreachable");
        }
        self.calls.lock().unwrap().push(call.clone());
        let carried = call.headers.iter().any(|(_, v)| self.refuse.lock().unwrap().iter().any(|r| v.contains(r.as_str()))) || self.refuse.lock().unwrap().iter().any(|r| String::from_utf8_lossy(&call.body).contains(r.as_str()));
        Ok(Reply { status: if carried { 401 } else { 200 }, content_type: Some("application/json".into()), body: br#"{"ok":true}"#.to_vec() })
    }
}

fn open(upstream: &Arc<Fake>, account: Uuid, refresh: Option<Refresh>) -> Vault {
    Vault::open(Login::Discovery, account, Arc::clone(upstream) as Arc<dyn Upstream>, None, refresh).unwrap()
}

fn signed_in(upstream: &Arc<Fake>) -> (Vault, Uuid) {
    let account = Uuid::new_v4();
    let vault = open(upstream, account, None);
    publish(account, REAL, SystemTime::now() + Duration::from_secs(3600));
    (vault, account)
}

fn exchange(address: SocketAddr, request: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(address).unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    read_reply(&mut stream)
}

fn read_reply(stream: &mut TcpStream) -> (u16, String) {
    let mut reply = String::new();
    stream.read_to_string(&mut reply).unwrap();
    let status = reply.split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, reply.split_once("\r\n\r\n").map(|(_, b)| b.to_owned()).unwrap_or_default())
}

fn get(vault: &Vault, path: &str, bearer: Option<&str>) -> (u16, String) {
    let auth = bearer.map(|b| format!("Authorization: Bearer {b}\r\n")).unwrap_or_default();
    exchange(vault.http.address(), &format!("GET {path} HTTP/1.1\r\nHost: x\r\n{auth}\r\n"))
}

fn post(vault: &Vault, path: &str, body: &str) -> (u16, String) {
    exchange(vault.http.address(), &format!("POST {path} HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()))
}

fn bearer_of(call: &Call) -> Option<&str> {
    call.headers.iter().find(|(n, _)| *n == "authorization").map(|(_, v)| v.as_str())
}

#[test]
fn discovery_points_every_endpoint_at_the_vault_and_keeps_texture_addresses() {
    let fake = Arc::new(Fake::default());
    let (vault, _) = signed_in(&fake);
    let (status, body) = get(&vault, "/minecraft/client", None);
    assert_eq!(status, 200);
    let doc: serde_json::Value = serde_json::from_str(&body).unwrap();
    let base = format!("http://{}", vault.http.address());
    let mut uris = 0;
    for (_, service) in doc["discovery"].as_object().unwrap() {
        for (_, endpoint) in service["endpoints"].as_object().unwrap() {
            if let Some(uri) = endpoint["uri"].as_str() {
                assert!(uri.starts_with(&base), "{uri}");
                uris += 1;
            }
        }
    }
    assert!(uris >= 15);
    assert_eq!(doc["discovery"]["profiles"]["endpoints"]["getTexture"]["validUris"][0], "https://textures.minecraft.net/texture/{textureId}");
    assert!(doc["discovery"].get("signaling").is_none());
    assert!(fake.calls.lock().unwrap().is_empty());
}

#[test]
fn the_stand_in_is_swapped_for_the_real_login_in_the_bearer_header() {
    let fake = Arc::new(Fake::default());
    let (vault, _) = signed_in(&fake);
    let (status, body) = get(&vault, "/player/attributes", Some(vault.stand_in().as_str()));
    assert_eq!((status, body.as_str()), (200, r#"{"ok":true}"#));
    let calls = fake.calls.lock().unwrap();
    assert_eq!(calls[0].url, "https://api.minecraftservices.com/player/attributes");
    assert_eq!(bearer_of(&calls[0]), Some(format!("Bearer {REAL}").as_str()));
}

#[test]
fn joining_a_server_gets_the_real_login_in_the_body() {
    let fake = Arc::new(Fake::default());
    let (vault, _) = signed_in(&fake);
    let body = format!(r#"{{"accessToken":"{}","selectedProfile":"abc","serverId":"-1f"}}"#, vault.stand_in().as_str());
    assert_eq!(post(&vault, "/session/minecraft/join", &body).0, 200);
    let calls = fake.calls.lock().unwrap();
    let sent: serde_json::Value = serde_json::from_slice(&calls[0].body).unwrap();
    assert_eq!(calls[0].url, "https://sessionserver.mojang.com/session/minecraft/join");
    assert_eq!((sent["accessToken"].as_str(), sent["serverId"].as_str()), (Some(REAL), Some("-1f")));
}

#[test]
fn requests_without_the_stand_in_never_reach_mojang() {
    let fake = Arc::new(Fake::default());
    let (vault, _) = signed_in(&fake);
    assert_eq!(get(&vault, "/player/attributes", None).0, 401);
    assert_eq!(get(&vault, "/player/attributes", Some("guess")).0, 401);
    assert_eq!(get(&vault, "/player/attributes", Some(&vault.stand_in().as_str()[1..])).0, 401);
    assert_eq!(post(&vault, "/session/minecraft/join", r#"{"accessToken":"guess"}"#).0, 401);
    assert_eq!(post(&vault, "/session/minecraft/join", "not json").0, 400);
    assert!(fake.calls.lock().unwrap().is_empty());
}

#[test]
fn only_allowed_endpoints_are_forwarded() {
    let fake = Arc::new(Fake::default());
    let (vault, _) = signed_in(&fake);
    let stand_in = vault.stand_in().as_str().to_owned();
    for path in ["/minecraft/profile/skins", "/minecraft/profile/name/Other", "/authentication/login_with_xbox", "/player/attributes/../../x", "/session/minecraft/profile/a/b", "/", "/events/extra"] {
        assert_eq!(get(&vault, path, Some(&stand_in)).0, 404, "{path}");
    }
    assert_eq!(exchange(vault.http.address(), &format!("DELETE /player/certificates HTTP/1.1\r\nAuthorization: Bearer {stand_in}\r\n\r\n")).0, 404);
    assert!(fake.calls.lock().unwrap().is_empty());
}

#[test]
fn public_endpoints_pass_without_any_login() {
    let fake = Arc::new(Fake::default());
    let (vault, _) = signed_in(&fake);
    let stand_in = vault.stand_in().as_str().to_owned();
    assert_eq!(get(&vault, "/session/minecraft/hasJoined?username=Dev&serverId=1", Some(&stand_in)).0, 200);
    assert_eq!(get(&vault, "/session/minecraft/profile/0123abcd?unsigned=false", None).0, 200);
    let calls = fake.calls.lock().unwrap();
    assert_eq!(calls[0].url, "https://sessionserver.mojang.com/session/minecraft/hasJoined?username=Dev&serverId=1");
    assert_eq!(calls[1].url, "https://sessionserver.mojang.com/session/minecraft/profile/0123abcd?unsigned=false");
    assert!(calls.iter().all(|c| bearer_of(c).is_none()));
}

#[test]
fn an_account_without_a_login_is_refused_without_asking_mojang() {
    let fake = Arc::new(Fake::default());
    let vault = open(&fake, Uuid::new_v4(), None);
    assert_eq!(get(&vault, "/player/attributes", Some(vault.stand_in().as_str())).0, 401);
    assert!(fake.calls.lock().unwrap().is_empty());
}

#[test]
fn a_login_that_runs_out_is_renewed_and_the_new_one_used() {
    let fake = Arc::new(Fake::default());
    let account = Uuid::new_v4();
    let renewed = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&renewed);
    let vault = open(&fake, account, Some(Box::new(move || {
        count.fetch_add(1, Ordering::SeqCst);
        publish(account, "RENEWED", SystemTime::now() + Duration::from_secs(3600));
        Ok(())
    })));
    publish(account, REAL, SystemTime::now() + Duration::from_secs(10));
    assert_eq!(get(&vault, "/player/attributes", Some(vault.stand_in().as_str())).0, 200);
    assert_eq!(get(&vault, "/privacy/blocklist", Some(vault.stand_in().as_str())).0, 200);
    assert_eq!(renewed.load(Ordering::SeqCst), 1);
    assert!(fake.calls.lock().unwrap().iter().all(|c| bearer_of(c) == Some("Bearer RENEWED")));
}

#[test]
fn a_login_mojang_refuses_is_renewed_once_and_retried() {
    let fake = Arc::new(Fake::default());
    fake.refuse.lock().unwrap().push(REAL.into());
    let account = Uuid::new_v4();
    let vault = open(&fake, account, Some(Box::new(move || {
        publish(account, "RENEWED", SystemTime::now() + Duration::from_secs(3600));
        Ok(())
    })));
    publish(account, REAL, SystemTime::now() + Duration::from_secs(3600));
    let body = format!(r#"{{"accessToken":"{}"}}"#, vault.stand_in().as_str());
    assert_eq!(post(&vault, "/session/minecraft/join", &body).0, 200);
    let calls = fake.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert!(String::from_utf8_lossy(&calls[1].body).contains("RENEWED"));
}

#[test]
fn a_refresh_reaches_running_vaults_of_the_account_only() {
    let fake = Arc::new(Fake::default());
    let (vault, account) = signed_in(&fake);
    let (other, _) = signed_in(&fake);
    publish(account, "AFTER-REFRESH", SystemTime::now() + Duration::from_secs(3600));
    get(&vault, "/player/attributes", Some(vault.stand_in().as_str()));
    get(&other, "/player/attributes", Some(other.stand_in().as_str()));
    let calls = fake.calls.lock().unwrap();
    assert_eq!(bearer_of(&calls[0]), Some("Bearer AFTER-REFRESH"));
    assert_eq!(bearer_of(&calls[1]), Some(format!("Bearer {REAL}").as_str()));
}

#[test]
fn mojang_being_down_is_a_bad_gateway() {
    let fake = Arc::new(Fake { down: true, ..Fake::default() });
    let (vault, _) = signed_in(&fake);
    assert_eq!(get(&vault, "/publickeys", None).0, 502);
}

#[test]
fn chunked_and_oversized_bodies() {
    let fake = Arc::new(Fake::default());
    let (vault, _) = signed_in(&fake);
    let body = format!(r#"{{"accessToken":"{}"}}"#, vault.stand_in().as_str());
    let (a, b) = body.split_at(10);
    let chunked = format!("POST /session/minecraft/join HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{a}\r\n{:x}\r\n{b}\r\n0\r\n\r\n", a.len(), b.len());
    assert_eq!(exchange(vault.http.address(), &chunked).0, 200);
    assert_eq!(exchange(vault.http.address(), "POST /events HTTP/1.1\r\nContent-Length: 99999999\r\n\r\n").0, 400);
}

#[test]
fn the_vault_closes_with_its_game() {
    let fake = Arc::new(Fake::default());
    let (vault, _) = signed_in(&fake);
    let address = vault.http.address();
    drop(vault);
    assert!(TcpStream::connect_timeout(&address, Duration::from_millis(500)).is_err());
}

#[test]
fn debug_output_never_shows_the_stand_in() {
    let fake = Arc::new(Fake::default());
    let (vault, _) = signed_in(&fake);
    let shown = format!("{vault:?} {:?}", vault.stand_in());
    assert!(!shown.contains(vault.stand_in().as_str()), "{shown}");
}

fn socks_sign_in(relay: SocketAddr, password: &str) -> (TcpStream, u8) {
    let mut s = TcpStream::connect(relay).unwrap();
    s.write_all(&[5, 2, 0, 2]).unwrap();
    let mut chosen = [0u8; 2];
    s.read_exact(&mut chosen).unwrap();
    assert_eq!(chosen, [5, 2]);
    let mut auth = vec![1, u8::try_from(relay::USER.len()).unwrap()];
    auth.extend_from_slice(relay::USER.as_bytes());
    auth.push(u8::try_from(password.len()).unwrap());
    auth.extend_from_slice(password.as_bytes());
    s.write_all(&auth).unwrap();
    let mut status = [0u8; 2];
    s.read_exact(&mut status).unwrap();
    (s, status[1])
}

fn password(vault: &Vault) -> String {
    vault.relay.as_ref().unwrap().1.as_str().to_owned()
}

fn socks_connect(vault: &Vault, atyp: u8, address: &[u8], port: u16) -> (TcpStream, u8) {
    let (mut s, status) = socks_sign_in(vault.relay().unwrap(), &password(vault));
    assert_eq!(status, 0);
    let mut request = vec![5, 1, 0, atyp];
    if atyp == 3 {
        request.push(u8::try_from(address.len()).unwrap());
    }
    request.extend_from_slice(address);
    request.extend_from_slice(&port.to_be_bytes());
    s.write_all(&request).unwrap();
    let mut reply = [0u8; 10];
    s.read_exact(&mut reply).unwrap();
    (s, reply[1])
}

fn relayed(dial: impl Fn(&proxy::Destination) -> Option<TcpStream> + Send + Sync + 'static) -> (Vault, Arc<Fake>) {
    let fake = Arc::new(Fake::default());
    let vault = Vault::open(Login::Discovery, Uuid::new_v4(), Arc::clone(&fake) as Arc<dyn Upstream>, Some(Arc::new(dial)), None).unwrap();
    (vault, fake)
}

#[test]
fn the_relay_serves_the_vault_locally() {
    let (vault, _) = relayed(|_| panic!("the vault must not go through the account's proxy"));
    let port = vault.http.address().port();
    for (atyp, address) in [(1, vec![127, 0, 0, 1]), (3, b"localhost".to_vec()), (3, b"127.0.0.1".to_vec())] {
        let (mut s, code) = socks_connect(&vault, atyp, &address, port);
        assert_eq!(code, 0);
        s.write_all(b"GET /minecraft/client HTTP/1.1\r\n\r\n").unwrap();
        assert_eq!(read_reply(&mut s).0, 200);
    }
}

#[test]
fn the_relay_sends_everything_else_through_the_account_proxy() {
    let target = TcpListener::bind("127.0.0.1:0").unwrap();
    let target_port = target.local_addr().unwrap().port();
    std::thread::spawn(move || {
        while let Ok((mut s, _)) = target.accept() {
            let _ = s.write_all(b"hello from the server");
        }
    });
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    let (vault, _) = relayed(move |to| {
        log.lock().unwrap().push(to.clone());
        TcpStream::connect(("127.0.0.1", target_port)).ok()
    });
    let (mut s, code) = socks_connect(&vault, 3, b"mc.example.net", 25565);
    assert_eq!(code, 0);
    let mut greeting = String::new();
    s.read_to_string(&mut greeting).unwrap();
    assert_eq!(greeting, "hello from the server");
    let (_, code) = socks_connect(&vault, 1, &[127, 0, 0, 1], target_port);
    assert_eq!(code, 0);
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0], proxy::Destination::Name("mc.example.net".into(), 25565));
    assert_eq!(seen[1], proxy::Destination::Address(SocketAddr::from(([127, 0, 0, 1], target_port))));
}

#[test]
fn the_relay_never_connects_directly_when_the_proxy_fails() {
    let (vault, _) = relayed(|_| None);
    let (_, code) = socks_connect(&vault, 1, &[1, 2, 3, 4], 25565);
    assert_eq!(code, 5);
}

#[test]
fn the_relay_refuses_what_is_not_a_plain_connect() {
    let (vault, _) = relayed(|_| None);
    let (mut s, status) = socks_sign_in(vault.relay().unwrap(), &password(&vault));
    assert_eq!(status, 0);
    s.write_all(&[5, 2, 0, 1, 1, 2, 3, 4, 0, 80]).unwrap();
    let mut reply = [0u8; 10];
    s.read_exact(&mut reply).unwrap();
    assert_eq!(reply[..2], [5, 7]);
}

#[test]
fn the_relay_lets_in_only_the_game_that_knows_its_password() {
    let (vault, _) = relayed(|_| panic!("a stranger must not reach the account's proxy"));
    let mut s = TcpStream::connect(vault.relay().unwrap()).unwrap();
    s.write_all(&[5, 1, 0]).unwrap();
    let mut chosen = [0u8; 2];
    s.read_exact(&mut chosen).unwrap();
    assert_eq!(chosen, [5, 0xff]);
    let wrong = "0".repeat(password(&vault).len());
    assert_eq!(socks_sign_in(vault.relay().unwrap(), &wrong).1, 1);
    assert!(vault.relay_url().unwrap().contains(&password(&vault)));
}

#[test]
fn a_renewal_that_fails_waits_before_it_is_tried_again() {
    let fake = Arc::new(Fake::default());
    let account = Uuid::new_v4();
    let tried = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&tried);
    let vault = open(&fake, account, Some(Box::new(move || {
        count.fetch_add(1, Ordering::SeqCst);
        bail!("Microsoft is down")
    })));
    publish(account, REAL, SystemTime::now() + Duration::from_secs(10));
    for _ in 0..3 {
        assert_eq!(get(&vault, "/player/attributes", Some(vault.stand_in().as_str())).0, 200);
    }
    assert_eq!(tried.load(Ordering::SeqCst), 1);
}

#[test]
fn a_login_that_needs_a_new_sign_in_is_not_renewed_again_until_one_arrives() {
    let fake = Arc::new(Fake::default());
    fake.refuse.lock().unwrap().push(REAL.into());
    let account = Uuid::new_v4();
    let tried = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&tried);
    let vault = open(&fake, account, Some(Box::new(move || {
        count.fetch_add(1, Ordering::SeqCst);
        Err(crate::core::auth::AuthError::SignInAgain.into())
    })));
    publish(account, REAL, SystemTime::now() + Duration::from_secs(3600));
    let body = format!(r#"{{"accessToken":"{}"}}"#, vault.stand_in().as_str());
    for _ in 0..3 {
        assert_eq!(post(&vault, "/session/minecraft/join", &body).0, 401);
    }
    assert_eq!(tried.load(Ordering::SeqCst), 1);
    publish(account, "RENEWED", SystemTime::now() + Duration::from_secs(3600));
    assert_eq!(post(&vault, "/session/minecraft/join", &body).0, 200);
}

#[test]
fn a_huge_chunk_is_refused_without_a_panic() {
    let fake = Arc::new(Fake::default());
    let (vault, _) = signed_in(&fake);
    let (status, _) = exchange(vault.http.address(), "POST /session/minecraft/join HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n1\r\nA\r\nffffffffffffffff\r\n");
    assert_eq!(status, 400);
}

#[test]
fn login_libraries_by_version() {
    let at = |v: &str| Login::of(&[PathBuf::from("libs/x.jar"), PathBuf::from(format!("libraries/com/mojang/authlib/{v}/authlib-{v}.jar"))]);
    assert_eq!(at("10.0.77"), Some(Login::Discovery));
    assert_eq!(at("12.0.81"), Some(Login::Discovery));
    assert_eq!(at("9.0.75"), Some(Login::Hosts(&["session", "services", "profiles"])));
    assert_eq!(at("6.0.57"), Some(Login::Hosts(&["session", "services"])));
    assert_eq!(at("5.0.47"), Some(Login::Hosts(&["account", "session", "services"])));
    assert_eq!(at("3.18.38"), Some(Login::Hosts(&["auth", "account", "session", "services"])));
    assert_eq!(at("1.6.25"), Some(Login::Hosts(&["auth", "account", "session", "services"])));
    assert_eq!(at("1.5.25"), None);
    assert_eq!(Login::of(&[PathBuf::from("a.jar")]), None);
    assert_eq!(Login::Discovery.jvm_args("http://127.0.0.1:5"), ["-Dminecraft.api.discovery.host=http://127.0.0.1:5/minecraft/client"]);
    assert_eq!(Login::Hosts(&["session", "services"]).jvm_args("http://h:1"), ["-Dminecraft.api.session.host=http://h:1", "-Dminecraft.api.services.host=http://h:1"]);
}

#[test]
fn versions_that_may_hide_the_login() {
    for v in ["1.16", "1.16.5", "1.20.1", "1.21.4", "26.3", "26.4-snapshot-7", "20w06a", "24w14a", "something new"] {
        assert!(may_hide(v), "{v}");
    }
    for v in ["1.15.2", "1.12.2", "1.8.9", "b1.7.3", "a1.2.6", "c0.30", "rd-132211", "19w45a"] {
        assert!(!may_hide(v), "{v}");
    }
}