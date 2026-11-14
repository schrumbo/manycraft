//! Account proxies: parsing `socks5://user:pass@host:1080` or `http://host:8080` and checking that one answers.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use super::fleet::split_server;

const CHECK_TIMEOUT: Duration = Duration::from_secs(5);
const CHECK_TARGET: &str = "sessionserver.mojang.com:443";

/// How a proxy speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Socks,
    Http,
}

/// A parsed proxy address. Debug output hides the password.
#[derive(Clone, PartialEq, Eq)]
pub struct Proxy {
    pub kind: Kind,
    pub host: String,
    pub port: u16,
    pub user: Option<String>,
    pub pass: Option<String>,
}

impl std::fmt::Debug for Proxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Proxy").field("kind", &self.kind).field("host", &self.host).field("port", &self.port).field("user", &self.user).field("pass", &self.pass.as_ref().map(|_| "<redacted>")).finish()
    }
}

/// Why a proxy failed its check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// Nothing answered on its address.
    Unreachable,
    /// It answered but did not accept the sign-in.
    Rejected,
}

/// Parses a proxy address: `socks5://`, `socks5h://`, `socks://`, `http://` or `https://`, with optional `user:pass@` and port.
pub fn parse(url: &str) -> Result<Proxy> {
    let (scheme, rest) = url.trim().split_once("://").context("a proxy looks like socks5://host:1080 or http://host:8080")?;
    let kind = match scheme.to_ascii_lowercase().as_str() {
        "socks" | "socks5" | "socks5h" => Kind::Socks,
        "http" | "https" => Kind::Http,
        other => bail!("proxy type {other} is not supported"),
    };
    let rest = rest.trim_end_matches('/');
    let (auth, address) = match rest.rsplit_once('@') {
        Some((auth, address)) => (Some(auth), address),
        None => (None, rest),
    };
    let (user, pass) = match auth.map(|a| a.split_once(':').unwrap_or((a, ""))) {
        Some((user, pass)) => (Some(percent_decode(user)), Some(percent_decode(pass)).filter(|p| !p.is_empty())),
        None => (None, None),
    };
    let (host, port) = split_server(address).context("the proxy address is invalid")?;
    let port = port.unwrap_or(if kind == Kind::Socks { 1080 } else { 80 });
    Ok(Proxy { kind, host, port, user, pass })
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(byte) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Opens the proxy and signs in, without tunnelling anywhere (an HTTP proxy is asked for a tunnel to Mojang's session server).
/// Blocks for a few seconds at most: call it off the UI thread.
pub fn check(proxy: &Proxy) -> Result<(), Fault> {
    let mut stream = connect(proxy).ok_or(Fault::Unreachable)?;
    let outcome = match proxy.kind {
        Kind::Socks => socks_hello(&mut stream, proxy),
        Kind::Http => http_connect(&mut stream, proxy),
    };
    outcome.ok_or(Fault::Unreachable).and_then(|accepted| if accepted { Ok(()) } else { Err(Fault::Rejected) })
}

fn connect(proxy: &Proxy) -> Option<TcpStream> {
    let stream = (proxy.host.as_str(), proxy.port).to_socket_addrs().ok()?.find_map(|addr| TcpStream::connect_timeout(&addr, CHECK_TIMEOUT).ok())?;
    stream.set_read_timeout(Some(CHECK_TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(CHECK_TIMEOUT)).ok()?;
    Some(stream)
}

fn socks_hello(stream: &mut TcpStream, proxy: &Proxy) -> Option<bool> {
    let signs_in = proxy.user.is_some();
    let hello: &[u8] = if signs_in { &[5, 2, 0, 2] } else { &[5, 1, 0] };
    stream.write_all(hello).ok()?;
    let mut reply = [0u8; 2];
    stream.read_exact(&mut reply).ok()?;
    if reply[0] != 5 {
        return None;
    }
    match reply[1] {
        0 => Some(true),
        2 if signs_in => {
            let (user, pass) = (proxy.user.as_deref().unwrap_or_default().as_bytes(), proxy.pass.as_deref().unwrap_or_default().as_bytes());
            let mut request = vec![1, u8::try_from(user.len()).ok()?];
            request.extend_from_slice(user);
            request.push(u8::try_from(pass.len()).ok()?);
            request.extend_from_slice(pass);
            stream.write_all(&request).ok()?;
            stream.read_exact(&mut reply).ok()?;
            Some(reply[1] == 0)
        }
        _ => Some(false),
    }
}

fn http_connect(stream: &mut TcpStream, proxy: &Proxy) -> Option<bool> {
    let mut request = format!("CONNECT {CHECK_TARGET} HTTP/1.1\r\nHost: {CHECK_TARGET}\r\n");
    if let Some(user) = &proxy.user {
        let login = format!("{user}:{}", proxy.pass.as_deref().unwrap_or_default());
        request.push_str(&format!("Proxy-Authorization: Basic {}\r\n", base64(login.as_bytes())));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).ok()?;
    let mut buf = [0u8; 64];
    let n = stream.read(&mut buf).ok()?;
    let line = String::from_utf8_lossy(&buf[..n]);
    let status: u16 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(status != 407 && status != 401)
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[(n >> (18 - 6 * i) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;

    use super::*;

    fn proxy(kind: Kind, port: u16, login: Option<(&str, &str)>) -> Proxy {
        Proxy { kind, host: "127.0.0.1".into(), port, user: login.map(|l| l.0.to_owned()), pass: login.map(|l| l.1.to_owned()) }
    }

    fn serve(handler: impl FnOnce(TcpStream) + Send + 'static) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                handler(stream);
            }
        });
        port
    }

    fn socks_server(want: Option<(&'static str, &'static str)>) -> u16 {
        serve(move |mut s| {
            let mut head = [0u8; 2];
            s.read_exact(&mut head).unwrap();
            let mut methods = vec![0u8; usize::from(head[1])];
            s.read_exact(&mut methods).unwrap();
            match want {
                None => s.write_all(&[5, 0]).unwrap(),
                Some((user, pass)) => {
                    if !methods.contains(&2) {
                        return s.write_all(&[5, 0xff]).unwrap();
                    }
                    s.write_all(&[5, 2]).unwrap();
                    let mut v = [0u8; 2];
                    s.read_exact(&mut v).unwrap();
                    let mut u = vec![0u8; usize::from(v[1])];
                    s.read_exact(&mut u).unwrap();
                    let mut l = [0u8; 1];
                    s.read_exact(&mut l).unwrap();
                    let mut p = vec![0u8; usize::from(l[0])];
                    s.read_exact(&mut p).unwrap();
                    let ok = u == user.as_bytes() && p == pass.as_bytes();
                    s.write_all(&[1, u8::from(!ok)]).unwrap();
                }
            }
        })
    }

    fn http_server(status: &'static str, seen: std::sync::mpsc::Sender<String>) -> u16 {
        serve(move |mut s| {
            let mut buf = [0u8; 512];
            let n = s.read(&mut buf).unwrap();
            seen.send(String::from_utf8_lossy(&buf[..n]).into_owned()).unwrap();
            s.write_all(format!("HTTP/1.1 {status}\r\n\r\n").as_bytes()).unwrap();
        })
    }

    #[test]
    fn urls_parse_with_defaults_and_escapes() {
        let p = parse("socks5://us%40r:p%3Ass@10.1.1.1:1081").unwrap();
        assert_eq!((p.kind, p.host.as_str(), p.port, p.user.as_deref(), p.pass.as_deref()), (Kind::Socks, "10.1.1.1", 1081, Some("us@r"), Some("p:ss")));
        let p = parse("socks5://host").unwrap();
        assert_eq!((p.host.as_str(), p.port, p.user, p.pass), ("host", 1080, None, None));
        let p = parse("SOCKS5://u@[::1]:9/").unwrap();
        assert_eq!((p.host.as_str(), p.port, p.user.as_deref(), p.pass), ("::1", 9, Some("u"), None));
        assert_eq!(parse("http://proxy.example").unwrap().port, 80);
        assert!(parse("host:1080").is_err());
        assert!(parse("ftp://x:1").is_err());
    }

    #[test]
    fn errors_and_debug_never_show_the_password() {
        let err = format!("{:#}", parse("socks5://u:secret@host:x").err().unwrap());
        assert!(!err.contains("secret"), "{err}");
        let shown = format!("{:?}", parse("socks5://u:secret@host:1").unwrap());
        assert!(!shown.contains("secret") && shown.contains("<redacted>"), "{shown}");
    }

    #[test]
    fn base64_matches_the_standard_alphabet() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"user:pass"), "dXNlcjpwYXNz");
    }

    #[test]
    fn nothing_listening_is_unreachable() {
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        assert_eq!(check(&proxy(Kind::Socks, port, None)), Err(Fault::Unreachable));
    }

    #[test]
    fn a_socks_proxy_without_sign_in_is_fine() {
        assert_eq!(check(&proxy(Kind::Socks, socks_server(None), None)), Ok(()));
    }

    #[test]
    fn a_socks_proxy_checks_the_sign_in() {
        assert_eq!(check(&proxy(Kind::Socks, socks_server(Some(("u", "p"))), Some(("u", "p")))), Ok(()));
        assert_eq!(check(&proxy(Kind::Socks, socks_server(Some(("u", "p"))), Some(("u", "wrong")))), Err(Fault::Rejected));
        assert_eq!(check(&proxy(Kind::Socks, socks_server(Some(("u", "p"))), None)), Err(Fault::Rejected));
    }

    #[test]
    fn something_that_is_not_a_socks_proxy_is_unreachable() {
        let port = serve(|mut s| {
            let mut buf = [0u8; 8];
            let _ = s.read(&mut buf);
            let _ = s.write_all(b"HTTP/1.1 400 Bad\r\n\r\n");
        });
        assert_eq!(check(&proxy(Kind::Socks, port, None)), Err(Fault::Unreachable));
    }

    #[test]
    fn an_http_proxy_is_asked_for_a_tunnel_with_its_sign_in() {
        let (tx, rx) = std::sync::mpsc::channel();
        assert_eq!(check(&proxy(Kind::Http, http_server("200 Connection established", tx), Some(("user", "pass")))), Ok(()));
        let request = rx.recv().unwrap();
        assert!(request.starts_with("CONNECT sessionserver.mojang.com:443"), "{request}");
        assert!(request.contains("Proxy-Authorization: Basic dXNlcjpwYXNz"), "{request}");
        let (tx, _rx) = std::sync::mpsc::channel();
        assert_eq!(check(&proxy(Kind::Http, http_server("407 Proxy Authentication Required", tx), None)), Err(Fault::Rejected));
        let (tx, _rx) = std::sync::mpsc::channel();
        assert_eq!(check(&proxy(Kind::Http, http_server("403 Forbidden", tx), None)), Ok(()));
    }
}
