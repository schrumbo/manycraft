use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use sha1::{Digest, Sha1};

const ATTEMPTS: u32 = 3;
const MAX_JSON: u64 = 64 << 20;
const MAX_FILE: u64 = 1 << 30;
const RENAME_ATTEMPTS: u32 = 5;

/// HTTP agent, routed through `proxy` when given (`socks5://user:pass@host:port`, `http://host:port`),
/// otherwise through the proxy from the environment, if any.
pub fn agent(proxy: Option<&str>) -> Result<ureq::Agent> {
    let proxy = proxy.map(ureq::Proxy::new).transpose().context("invalid proxy address")?.or_else(ureq::Proxy::try_from_env);
    Ok(ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_per_read(Some(Duration::from_secs(30)))
        .user_agent(concat!("Manycraft/", env!("CARGO_PKG_VERSION")))
        .proxy(proxy)
        .build()
        .into())
}

/// Fetches `url` and parses the body as JSON, retrying transient failures.
pub fn get_json<T: serde::de::DeserializeOwned>(agent: &ureq::Agent, url: &str) -> Result<T> {
    let bytes = retrying(url, || {
        let mut res = agent.get(url).call().map_err(Failure::from)?;
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
        let mut res = agent.get(url).call().map_err(Failure::from)?;
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
                std::thread::sleep(Duration::from_millis(500 << n));
            }
            Err(Failure::RateLimited(e)) => {
                last = Some(e);
                std::thread::sleep(Duration::from_secs(5 << n));
            }
        }
    }
    Err(last.map_or_else(|| anyhow::anyhow!("download failed"), |e| e.context(format!("download failed: {url}"))))
}

#[cfg(test)]
mod tests {
    use super::*;

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
