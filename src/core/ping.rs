//! Whether a Minecraft server is up: one status ping, the request the server list sends, without signing in.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use super::proxy::{self, Proxy};

const TIMEOUT: Duration = Duration::from_secs(3);
const PROTOCOL: i32 = 767;

/// Whether `server` answers a status ping, through `proxy` when given. Blocks for a few seconds at most: call it off the UI thread.
pub fn answers(server: &str, proxy: Option<&Proxy>) -> bool {
    let Ok((host, _)) = super::fleet::split_server(server) else { return false };
    let Ok(addrs) = super::fleet::resolve_all(server) else { return false };
    addrs.into_iter().take(2).any(|addr| {
        let stream = match proxy {
            Some(p) => proxy::tunnel(p, addr),
            None => TcpStream::connect_timeout(&addr, TIMEOUT).ok(),
        };
        stream.is_some_and(|s| status(s, &host, addr.port()))
    })
}

fn status(mut stream: TcpStream, host: &str, port: u16) -> bool {
    if stream.set_read_timeout(Some(TIMEOUT)).is_err() || stream.set_write_timeout(Some(TIMEOUT)).is_err() {
        return false;
    }
    let mut body = Vec::new();
    varint(&mut body, 0);
    varint(&mut body, PROTOCOL);
    varint(&mut body, i32::try_from(host.len()).unwrap_or(0));
    body.extend_from_slice(host.as_bytes());
    body.extend_from_slice(&port.to_be_bytes());
    varint(&mut body, 1);
    let mut packet = Vec::new();
    varint(&mut packet, i32::try_from(body.len()).unwrap_or(0));
    packet.extend_from_slice(&body);
    packet.extend_from_slice(&[1, 0]);
    if stream.write_all(&packet).is_err() {
        return false;
    }
    matches!((read_varint(&mut stream), read_varint(&mut stream)), (Some(len), Some(0)) if len > 1)
}

fn varint(out: &mut Vec<u8>, value: i32) {
    let mut v = value as u32;
    loop {
        if v & !0x7f == 0 {
            out.push(v as u8);
            return;
        }
        out.push((v & 0x7f | 0x80) as u8);
        v >>= 7;
    }
}

fn read_varint(stream: &mut impl Read) -> Option<i32> {
    let mut value = 0u32;
    for i in 0..5 {
        let mut byte = [0u8; 1];
        stream.read_exact(&mut byte).ok()?;
        value |= u32::from(byte[0] & 0x7f) << (7 * i);
        if byte[0] & 0x80 == 0 {
            return Some(value as i32);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::net::{SocketAddr, TcpListener};

    use super::*;

    fn server(answer: bool) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let Ok((mut s, _)) = listener.accept() else { return };
            let len = read_varint(&mut s).unwrap();
            let mut body = vec![0u8; usize::try_from(len).unwrap()];
            s.read_exact(&mut body).unwrap();
            assert_eq!(body[0], 0, "a handshake");
            assert_eq!(*body.last().unwrap(), 1, "asks for the status");
            let mut request = [0u8; 2];
            s.read_exact(&mut request).unwrap();
            assert_eq!(request, [1, 0]);
            if answer {
                let json = br#"{"version":{"name":"1.21","protocol":767}}"#;
                let mut body = vec![0];
                varint(&mut body, json.len() as i32);
                body.extend_from_slice(json);
                let mut packet = Vec::new();
                varint(&mut packet, body.len() as i32);
                packet.extend_from_slice(&body);
                s.write_all(&packet).unwrap();
            }
        });
        format!("127.0.0.1:{port}")
    }

    #[test]
    fn varints_round_trip() {
        for v in [0, 1, 127, 128, 300, 25565, 767, i32::MAX, -1] {
            let mut out = Vec::new();
            varint(&mut out, v);
            assert_eq!(read_varint(&mut out.as_slice()), Some(v));
        }
    }

    #[test]
    fn a_server_that_answers_the_status_ping_is_up() {
        assert!(answers(&server(true), None));
    }

    #[test]
    fn a_silent_or_closed_port_is_down() {
        assert!(!answers(&server(false), None));
        let closed = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        assert!(!answers(&closed.to_string(), None));
        assert!(!answers("not a server", None));
    }

    #[test]
    fn the_ping_goes_through_a_socks_proxy() {
        let target = server(true);
        let to: SocketAddr = target.parse().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut hello = [0u8; 3];
            s.read_exact(&mut hello).unwrap();
            s.write_all(&[5, 0]).unwrap();
            let mut connect = [0u8; 10];
            s.read_exact(&mut connect).unwrap();
            assert_eq!(&connect[..4], &[5, 1, 0, 1]);
            assert_eq!(u16::from_be_bytes([connect[8], connect[9]]), to.port());
            s.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0]).unwrap();
            let mut up = TcpStream::connect(to).unwrap();
            let mut down = s.try_clone().unwrap();
            let mut back = up.try_clone().unwrap();
            std::thread::spawn(move || std::io::copy(&mut back, &mut down));
            let _ = std::io::copy(&mut s, &mut up);
        });
        let p = proxy::parse(&format!("socks5://127.0.0.1:{port}")).unwrap();
        assert!(answers(&target, Some(&p)));
    }
}
