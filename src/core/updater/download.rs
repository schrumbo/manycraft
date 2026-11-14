//! Downloads that pick up where an interrupted one stopped (HTTP Range).

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};

const ATTEMPTS: u32 = 4;

/// Downloads `url` into `part`, keeping what an earlier attempt already wrote there. Stops above `max` bytes.
/// `progress` hears the bytes on disk and the full size when the server tells it.
pub fn resumable(agent: &ureq::Agent, url: &str, part: &Path, max: u64, progress: &mut dyn FnMut(u64, Option<u64>)) -> Result<()> {
    let mut last = None;
    for _ in 0..ATTEMPTS {
        match attempt(agent, url, part, max, progress) {
            Ok(()) => return Ok(()),
            Err(Fault::Fatal(e)) => return Err(e),
            Err(Fault::Again(e)) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("download failed")))
}

enum Fault {
    Again(anyhow::Error),
    Fatal(anyhow::Error),
}

fn attempt(agent: &ureq::Agent, url: &str, part: &Path, max: u64, progress: &mut dyn FnMut(u64, Option<u64>)) -> Result<(), Fault> {
    let have = std::fs::metadata(part).map_or(0, |m| m.len());
    let mut request = agent.get(url);
    if have > 0 {
        request = request.header("Range", format!("bytes={have}-"));
    }
    let mut res = match request.call() {
        Ok(res) => res,
        Err(ureq::Error::StatusCode(416)) if have > 0 => return Ok(()),
        Err(e @ (ureq::Error::Io(_) | ureq::Error::Timeout(_) | ureq::Error::ConnectionFailed | ureq::Error::HostNotFound)) => return Err(Fault::Again(e.into())),
        Err(ureq::Error::StatusCode(code)) if code >= 500 => return Err(Fault::Again(anyhow::anyhow!("server answered {code}"))),
        Err(e) => return Err(Fault::Fatal(e.into())),
    };
    let resumed = have > 0 && res.status() == 206 && res.headers().get("content-range").and_then(|v| v.to_str().ok()).is_some_and(|v| v.starts_with(&format!("bytes {have}-")));
    if res.status() == 206 && !resumed {
        let _ = std::fs::remove_file(part);
        return Err(Fault::Again(anyhow::anyhow!("the server sent the wrong part of the file")));
    }
    let start = if resumed { have } else { 0 };
    let total = res.body().content_length().map(|n| n + start);
    if total.is_some_and(|t| t > max) {
        return Err(Fault::Fatal(anyhow::anyhow!("the download is larger than expected")));
    }
    let mut file = OpenOptions::new().create(true).write(true).append(resumed).truncate(!resumed).open(part).with_context(|| format!("cannot write {}", part.display())).map_err(Fault::Fatal)?;
    let mut reader = res.body_mut().with_config().limit(max.saturating_sub(start).saturating_add(1)).reader();
    let mut done = start;
    let mut buf = vec![0; 64 * 1024];
    progress(done, total);
    loop {
        let n = reader.read(&mut buf).map_err(|e| Fault::Again(e.into()))?;
        if n == 0 {
            break;
        }
        done += n as u64;
        if done > max {
            drop(file);
            let _ = std::fs::remove_file(part);
            return Err(Fault::Fatal(anyhow::anyhow!("the download is larger than expected")));
        }
        file.write_all(&buf[..n]).with_context(|| format!("cannot write {}", part.display())).map_err(Fault::Fatal)?;
        progress(done, total);
    }
    file.sync_all().with_context(|| format!("cannot write {}", part.display())).map_err(Fault::Fatal)?;
    if total.is_some_and(|t| done < t) {
        return Err(Fault::Again(anyhow::anyhow!("the connection ended early")));
    }
    Ok(())
}

/// Fetches a small text file such as a checksum or a signature.
pub fn text(agent: &ureq::Agent, url: &str) -> Result<String> {
    let mut res = agent.get(url).call().with_context(|| format!("cannot load {url}"))?;
    let text = res.body_mut().with_config().limit(64 * 1024).read_to_string().with_context(|| format!("cannot load {url}"))?;
    if text.trim().is_empty() {
        bail!("{url} is empty");
    }
    Ok(text)
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::io::BufRead;
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// A tiny HTTP server for tests: serves `body`, honours `Range: bytes=N-` when `ranges`, and cuts the first `cut_once` answer short.
    pub struct Server {
        pub url: String,
        pub ranges_seen: Arc<Mutex<Vec<Option<String>>>>,
    }

    pub fn serve(body: Vec<u8>, ranges: bool, cut_once: Option<usize>) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/file", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        std::thread::spawn(move || {
            let mut cut = cut_once;
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let mut range = None;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
                        range = v.trim().trim_end_matches('-').parse::<usize>().ok();
                    }
                }
                log.lock().unwrap().push(range.map(|r| r.to_string()));
                let from = if ranges { range.unwrap_or(0) } else { 0 };
                let rest = &body[from.min(body.len())..];
                let head = if from > 0 {
                    format!("HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {from}-{}/{}\r\nConnection: close\r\n\r\n", rest.len(), body.len() - 1, body.len())
                } else {
                    format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", rest.len())
                };
                let _ = stream.write_all(head.as_bytes());
                let send = cut.take().map_or(rest.len(), |c| c.min(rest.len()));
                let _ = stream.write_all(&rest[..send]);
                let _ = stream.flush();
            }
        });
        Server { url, ranges_seen: seen }
    }

    fn agent() -> ureq::Agent {
        ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(10))).build().into()
    }

    fn body() -> Vec<u8> {
        (0..200_000u32).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn a_cut_download_resumes_where_it_stopped() {
        let server = serve(body(), true, Some(70_000));
        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join("x.part");
        let mut seen = Vec::new();
        resumable(&agent(), &server.url, &part, 1 << 20, &mut |done, total| seen.push((done, total))).unwrap();
        assert_eq!(std::fs::read(&part).unwrap(), body());
        let ranges = server.ranges_seen.lock().unwrap().clone();
        assert_eq!(ranges, vec![None, Some("70000".into())]);
        assert_eq!(seen.last(), Some(&(200_000, Some(200_000))));
    }

    #[test]
    fn a_server_without_ranges_starts_over() {
        let server = serve(body(), false, None);
        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join("x.part");
        std::fs::write(&part, vec![9u8; 5000]).unwrap();
        resumable(&agent(), &server.url, &part, 1 << 20, &mut |_, _| {}).unwrap();
        assert_eq!(std::fs::read(&part).unwrap(), body());
    }

    #[test]
    fn a_finished_part_is_left_alone() {
        let server = serve(body(), true, None);
        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join("x.part");
        std::fs::write(&part, body()).unwrap();
        resumable(&agent(), &server.url, &part, 1 << 20, &mut |_, _| {}).unwrap();
        assert_eq!(std::fs::read(&part).unwrap(), body());
    }

    #[test]
    fn an_oversized_download_is_refused() {
        let server = serve(body(), true, None);
        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join("x.part");
        assert!(resumable(&agent(), &server.url, &part, 1000, &mut |_, _| {}).is_err());
        assert!(std::fs::metadata(&part).map_or(0, |m| m.len()) <= 1000);
    }
}
