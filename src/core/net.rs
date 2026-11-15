use std::io::{ErrorKind, Read, Write};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use sha1::{Digest, Sha1};

const ATTEMPTS: u32 = 3;
const CONNECT: Duration = Duration::from_secs(10);
const PER_READ: Duration = Duration::from_secs(30);
const TOTAL: Duration = Duration::from_secs(15 * 60);
const MAX_JSON: u64 = 64 << 20;
const MAX_FILE: u64 = 1 << 30;
const RENAME_ATTEMPTS: u32 = 12;

/// HTTP agent, routed through `proxy` when given (`socks5://user:pass@host:port`, `http://host:port`),
/// otherwise through the proxy from the environment, if any. Connecting gives up after 10 s, a stalled read after 30 s,
/// and a whole request after 15 minutes.
pub fn agent(proxy: Option<&str>) -> Result<ureq::Agent> {
    let proxy = proxy.map(ureq::Proxy::new).transpose().context("invalid proxy address")?.or_else(ureq::Proxy::try_from_env);
    Ok(ureq::Agent::config_builder()
        .timeout_connect(Some(CONNECT))
        .timeout_per_read(Some(PER_READ))
        .timeout_global(Some(TOTAL))
        .user_agent(concat!("Manycraft/", env!("CARGO_PKG_VERSION")))
        .proxy(proxy)
        .build()
        .into())
}

/// HTTP agent for short calls: the whole request, connect to last byte, ends within `timeout`. Uses the proxy from the environment, if any.
pub fn agent_within(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(timeout.min(CONNECT)))
        .timeout_global(Some(timeout))
        .user_agent(concat!("Manycraft/", env!("CARGO_PKG_VERSION")))
        .proxy(ureq::Proxy::try_from_env())
        .build()
        .into()
}

/// Whether `e` means no connection could be made or kept: no network, unknown host, refused, reset or timed out.
pub fn no_connection(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.downcast_ref::<ureq::Error>().is_some_and(unreachable) || c.downcast_ref::<std::io::Error>().is_some_and(cut))
}

fn cut(e: &std::io::Error) -> bool {
    e.get_ref().and_then(|inner| inner.downcast_ref::<ureq::Error>()).is_some_and(unreachable)
        || matches!(
            e.kind(),
            ErrorKind::ConnectionRefused | ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted | ErrorKind::NotConnected | ErrorKind::TimedOut | ErrorKind::NetworkUnreachable | ErrorKind::HostUnreachable | ErrorKind::NetworkDown
        )
}

fn unreachable(e: &ureq::Error) -> bool {
    match e {
        ureq::Error::HostNotFound | ureq::Error::ConnectionFailed | ureq::Error::Timeout(_) => true,
        ureq::Error::Io(io) => lost(io),
        _ => false,
    }
}

fn refused(e: &ureq::Error) -> bool {
    match e {
        ureq::Error::HostNotFound | ureq::Error::ConnectionFailed | ureq::Error::Timeout(ureq::Timeout::Resolve | ureq::Timeout::Connect) => true,
        ureq::Error::Io(io) => matches!(io.kind(), ErrorKind::ConnectionRefused | ErrorKind::PermissionDenied | ErrorKind::NetworkUnreachable | ErrorKind::HostUnreachable | ErrorKind::NetworkDown | ErrorKind::AddrNotAvailable),
        _ => false,
    }
}

fn lost(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        ErrorKind::ConnectionRefused
            | ErrorKind::ConnectionReset
            | ErrorKind::ConnectionAborted
            | ErrorKind::NotConnected
            | ErrorKind::TimedOut
            | ErrorKind::UnexpectedEof
            | ErrorKind::PermissionDenied
            | ErrorKind::NetworkUnreachable
            | ErrorKind::HostUnreachable
            | ErrorKind::NetworkDown
            | ErrorKind::AddrNotAvailable
    )
}

/// Tells [`super::online`] how a request ended: an answer proves the connection, a failure to connect asks it to check.
pub fn note(outcome: &Result<impl Sized, ureq::Error>) {
    match outcome {
        Ok(_) | Err(ureq::Error::StatusCode(_)) => super::online::reached(),
        Err(e) if unreachable(e) => super::online::suspect(),
        Err(_) => {}
    }
}

/// Fetches `url` and parses the body as JSON, retrying transient failures.
pub fn get_json<T: serde::de::DeserializeOwned>(agent: &ureq::Agent, url: &str) -> Result<T> {
    serde_json::from_slice(&get_bytes(agent, url)?).with_context(|| format!("unexpected response from {url}"))
}

/// Fetches the body of `url`, at most 64 MB, retrying transient failures.
pub fn get_bytes(agent: &ureq::Agent, url: &str) -> Result<Vec<u8>> {
    retrying(url, || {
        let mut res = called(agent.get(url).call())?;
        res.body_mut().with_config().limit(MAX_JSON).read_to_vec().map_err(Failure::from)
    })
}

fn called<T>(outcome: Result<T, ureq::Error>) -> Result<T, Failure> {
    note(&outcome);
    outcome.map_err(Failure::from)
}

/// Posts `body` as JSON to `url` and parses the response as JSON, retrying transient failures.
pub fn post_json<T: serde::de::DeserializeOwned>(agent: &ureq::Agent, url: &str, body: &serde_json::Value) -> Result<T> {
    let bytes = retrying(url, || {
        let mut res = called(agent.post(url).send_json(body))?;
        res.body_mut().with_config().limit(MAX_JSON).read_to_vec().map_err(Failure::from)
    })?;
    serde_json::from_slice(&bytes).with_context(|| format!("unexpected response from {url}"))
}

/// Downloads `url` to `path` unless a file with the expected SHA-1 is already there.
/// Without a hash, an existing file is kept. Streams to a unique temporary file and renames it into place.
pub fn fetch_verified(agent: &ureq::Agent, url: &str, path: &Path, sha1: Option<&str>) -> Result<()> {
    if path.is_file() {
        match sha1 {
            None => return Ok(()),
            Some(expected) if file_sha1(path)?.eq_ignore_ascii_case(expected) => return Ok(()),
            Some(_) => {}
        }
    }
    let dir = path.parent().context("path has no parent")?;
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let tmp = retrying(url, || {
        let mut res = called(agent.get(url).call())?;
        let mut reader = res.body_mut().with_config().limit(MAX_FILE).reader();
        let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(|e| Failure::Fatal(e.into()))?;
        let mut hasher = Sha1::new();
        let mut buf = vec![0; 64 * 1024];
        loop {
            let n = reader.read(&mut buf).map_err(|e| match e.downcast::<ureq::Error>() {
                Ok(e) => Failure::from(e),
                Err(e) => Failure::Retry(e.into()),
            })?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            tmp.write_all(&buf[..n]).map_err(|e| Failure::Fatal(e.into()))?;
        }
        if let Some(expected) = sha1 {
            let got = hex(&hasher.finalize());
            if !got.eq_ignore_ascii_case(expected) {
                return Err(Failure::Retry(anyhow::anyhow!("SHA-1 {got}, expected {expected}")));
            }
        }
        Ok(tmp)
    })?;
    persist(tmp, path)
}

/// Copies `source` to `path` when its SHA-1 is `sha1`, through a unique temporary file in the same folder.
/// False, with `path` untouched, when the hash differs.
pub fn copy_verified(source: &Path, path: &Path, sha1: &str) -> Result<bool> {
    let mut from = std::fs::File::open(source).with_context(|| format!("cannot read {}", source.display()))?;
    let dir = path.parent().context("path has no parent")?;
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir).with_context(|| format!("cannot write {}", path.display()))?;
    let mut hasher = Sha1::new();
    let mut buf = vec![0; 64 * 1024];
    loop {
        let n = from.read(&mut buf).with_context(|| format!("cannot read {}", source.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        tmp.write_all(&buf[..n]).with_context(|| format!("cannot write {}", path.display()))?;
    }
    if !hex(&hasher.finalize()).eq_ignore_ascii_case(sha1) {
        return Ok(false);
    }
    persist(tmp, path)?;
    Ok(true)
}

/// Writes `bytes` to `path` through a unique temporary file in the same folder, so readers never see a partial file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().context("path has no parent")?;
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir).with_context(|| format!("cannot write {}", path.display()))?;
    tmp.write_all(bytes).with_context(|| format!("cannot write {}", path.display()))?;
    persist(tmp, path)
}

fn persist(mut tmp: tempfile::NamedTempFile, path: &Path) -> Result<()> {
    tmp.as_file_mut().sync_all().with_context(|| format!("cannot write {}", path.display()))?;
    for attempt in 1..=RENAME_ATTEMPTS {
        match tmp.persist(path) {
            Ok(_) => return Ok(()),
            Err(e) if e.error.kind() == std::io::ErrorKind::PermissionDenied && attempt < RENAME_ATTEMPTS => {
                tmp = e.file;
                std::thread::sleep(Duration::from_millis(100 * u64::from(attempt)));
            }
            Err(e) => return Err(e.error).with_context(|| format!("cannot replace {}", path.display())),
        }
    }
    bail!("cannot replace {}", path.display())
}

/// Lowercase hex SHA-1 of the file at `path`.
pub fn file_sha1(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha1::new();
    std::io::copy(&mut file, &mut hasher)?;
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

enum Failure {
    Retry(anyhow::Error),
    RateLimited(anyhow::Error),
    Fatal(anyhow::Error),
}

impl From<ureq::Error> for Failure {
    fn from(e: ureq::Error) -> Self {
        match e {
            _ if refused(&e) => Failure::Fatal(e.into()),
            ureq::Error::StatusCode(429) => Failure::RateLimited(e.into()),
            ureq::Error::StatusCode(code) if code >= 500 => Failure::Retry(e.into()),
            ureq::Error::Io(_) | ureq::Error::Timeout(_) | ureq::Error::ConnectionFailed | ureq::Error::HostNotFound => Failure::Retry(e.into()),
            _ => Failure::Fatal(e.into()),
        }
    }
}

fn retrying<T>(url: &str, mut attempt: impl FnMut() -> Result<T, Failure>) -> Result<T> {
    let mut last = None;
    for n in 0..ATTEMPTS {
        match attempt() {
            Ok(v) => return Ok(v),
            Err(Failure::Fatal(e)) => return Err(e.context(format!("download failed: {url}"))),
            Err(Failure::Retry(e)) => {
                last = Some(e);
                if n + 1 < ATTEMPTS {
                    std::thread::sleep(Duration::from_millis(500 << n));
                }
            }
            Err(Failure::RateLimited(e)) => {
                last = Some(e);
                if n + 1 < ATTEMPTS {
                    std::thread::sleep(Duration::from_secs(5 << n));
                }
            }
        }
    }
    Err(last.map_or_else(|| anyhow::anyhow!("download failed"), |e| e.context(format!("download failed: {url}"))))
}

/// Agents whose name lookups are counted and answered by the test, for exercising requests without a network.
#[cfg(test)]
pub(crate) mod fake {
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use ureq::unversioned::resolver::{ResolvedSocketAddrs, Resolver};
    use ureq::unversioned::transport::{DefaultConnector, NextTimeout};

    /// Where every host name leads: nowhere (no network) or one local address.
    #[derive(Debug)]
    pub struct Lookups {
        to: Option<SocketAddr>,
        count: AtomicUsize,
    }

    impl Lookups {
        /// How many requests tried to reach a host.
        pub fn count(&self) -> usize {
            self.count.load(Ordering::SeqCst)
        }
    }

    #[derive(Debug)]
    struct Shared(Arc<Lookups>);

    impl Resolver for Shared {
        fn resolve(&self, _uri: &ureq::http::Uri, _config: &ureq::config::Config, _timeout: NextTimeout) -> Result<ResolvedSocketAddrs, ureq::Error> {
            self.0.count.fetch_add(1, Ordering::SeqCst);
            let to = self.0.to.ok_or(ureq::Error::HostNotFound)?;
            let mut found = self.empty();
            found.push(to);
            Ok(found)
        }
    }

    /// An agent like [`super::agent`] whose requests go to `to`, or find no host when `None`, with `total` as the whole-request limit.
    pub fn agent(to: Option<SocketAddr>, total: std::time::Duration) -> (ureq::Agent, Arc<Lookups>) {
        let lookups = Arc::new(Lookups { to, count: AtomicUsize::new(0) });
        let config = ureq::Agent::config_builder().timeout_connect(Some(total)).timeout_global(Some(total)).build();
        (ureq::Agent::with_parts(config, DefaultConnector::new(), Shared(Arc::clone(&lookups))), lookups)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::time::Instant;

    #[test]
    fn no_network_fails_at_once_without_retries() {
        let (agent, lookups) = fake::agent(None, Duration::from_secs(5));
        let started = Instant::now();
        let e = get_json::<serde_json::Value>(&agent, "https://example.invalid/x.json").unwrap_err();
        assert!(no_connection(&e), "{e:#}");
        assert_eq!(lookups.count(), 1);
        assert!(started.elapsed() < Duration::from_millis(400), "{:?}", started.elapsed());
    }

    #[test]
    fn a_silent_server_ends_within_the_total_timeout() {
        let silent = TcpListener::bind("127.0.0.1:0").unwrap();
        let (agent, _) = fake::agent(Some(silent.local_addr().unwrap()), Duration::from_millis(300));
        let started = Instant::now();
        let e = get_json::<serde_json::Value>(&agent, "http://example.invalid/x.json").unwrap_err();
        assert!(no_connection(&e), "{e:#}");
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    }

    #[test]
    fn a_refused_connection_is_no_connection_and_not_retried() {
        let closed = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        let (agent, lookups) = fake::agent(Some(closed), Duration::from_secs(5));
        let e = get_json::<serde_json::Value>(&agent, "http://example.invalid/x.json").unwrap_err();
        assert!(no_connection(&e), "{e:#}");
        assert_eq!(lookups.count(), 1);
    }

    #[test]
    fn a_download_cut_short_leaves_no_file() {
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in server.incoming().take(ATTEMPTS as usize) {
                let mut stream = stream.unwrap();
                let mut buf = [0; 1024];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\nonly a part");
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lib.jar");
        let (agent, _) = fake::agent(Some(addr), Duration::from_secs(5));
        assert!(fetch_verified(&agent, "http://example.invalid/lib.jar", &path, None).is_err());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn local_file_errors_are_not_no_connection() {
        let denied = anyhow::Error::from(std::io::Error::from(ErrorKind::PermissionDenied)).context("cannot replace a file");
        assert!(!no_connection(&denied));
        assert!(!no_connection(&anyhow::Error::from(std::io::Error::from(ErrorKind::UnexpectedEof))));
        assert!(no_connection(&anyhow::Error::from(std::io::Error::from(ErrorKind::ConnectionReset))));
        assert!(no_connection(&anyhow::Error::from(std::io::Error::other(ureq::Error::HostNotFound))));
    }

    #[test]
    fn errors_without_a_connection_problem_are_not_no_connection() {
        assert!(!no_connection(&anyhow::anyhow!("unexpected response")));
        assert!(!no_connection(&anyhow::Error::from(ureq::Error::StatusCode(404))));
        assert!(no_connection(&anyhow::Error::from(ureq::Error::HostNotFound).context("cannot download")));
    }

    #[test]
    fn sha1_matches_known_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a");
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(file_sha1(&path).unwrap(), "a9993e364706816aba3e25717850c26c9cd0d89d");
    }

    #[test]
    fn existing_file_with_right_hash_is_not_downloaded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.bin");
        std::fs::write(&path, b"abc").unwrap();
        let agent = agent(None).unwrap();
        fetch_verified(&agent, "http://127.0.0.1:1/unreachable", &path, Some("A9993E364706816ABA3E25717850C26C9CD0D89D")).unwrap();
    }

    #[test]
    fn atomic_write_creates_parents_and_leaves_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x/y/z.json");
        write_atomic(&path, b"{}").unwrap();
        write_atomic(&path, b"[]").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"[]");
        assert_eq!(std::fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn parallel_writes_to_one_path_never_corrupt_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("same.json");
        std::thread::scope(|s| {
            for i in 0..16u8 {
                let path = &path;
                s.spawn(move || write_atomic(path, &[i; 4096]).unwrap());
            }
        });
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.len(), 4096);
        assert!(bytes.iter().all(|b| *b == bytes[0]));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn invalid_proxy_is_an_error() {
        assert!(agent(Some("socks9://nope")).is_err());
    }
}
