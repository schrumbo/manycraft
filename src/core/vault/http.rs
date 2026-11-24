//! Just enough HTTP/1.1 for the game's login library: one request per connection, bodies by length or in chunks.

use std::io::{self, BufRead, BufReader, Read, Write};

use anyhow::{Context, Result, bail};

const MAX_HEAD: usize = 16 * 1024;
/// The largest request body the vault takes.
pub(super) const MAX_BODY: usize = 1024 * 1024;

/// One request from the game.
#[derive(Debug)]
pub(super) struct Request {
    pub method: String,
    pub path: String,
    pub query: Option<String>,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    /// The value of the header `name`, ignoring case.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// The answer to the game.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Response {
    pub status: u16,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn text(status: u16, text: &str) -> Self {
        Self { status, content_type: Some("text/plain; charset=utf-8".to_owned()), body: text.as_bytes().to_vec() }
    }

    pub fn json(status: u16, body: Vec<u8>) -> Self {
        Self { status, content_type: Some("application/json; charset=utf-8".to_owned()), body }
    }
}

/// Reads one request; fails on anything malformed or larger than the limits.
pub(super) fn read(stream: impl Read) -> Result<Request> {
    let mut reader = BufReader::new(stream.take((MAX_HEAD + MAX_BODY + MAX_HEAD) as u64));
    let mut head = Vec::new();
    loop {
        let before = head.len();
        reader.read_until(b'\n', &mut head).context("the request could not be read")?;
        if head.len() == before {
            bail!("the request ended early");
        }
        if head.len() > MAX_HEAD {
            bail!("the request head is too long");
        }
        if head.ends_with(b"\r\n\r\n") || head.ends_with(b"\n\n") {
            break;
        }
    }
    let text = std::str::from_utf8(&head).context("the request head is not text")?;
    let mut lines = text.lines();
    let mut start = lines.next().unwrap_or_default().split(' ');
    let (Some(method), Some(target), Some(version)) = (start.next(), start.next(), start.next()) else { bail!("the request line is malformed") };
    if !version.starts_with("HTTP/1.") || !target.starts_with('/') {
        bail!("the request line is malformed");
    }
    let mut headers = Vec::new();
    for line in lines.take_while(|l| !l.is_empty()) {
        let (name, value) = line.split_once(':').context("a request header is malformed")?;
        headers.push((name.trim().to_owned(), value.trim().to_owned()));
    }
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path.to_owned(), Some(query.to_owned())),
        None => (target.to_owned(), None),
    };
    let mut request = Request { method: method.to_owned(), path, query, headers, body: Vec::new() };
    if request.header("transfer-encoding").is_some_and(|t| t.to_ascii_lowercase().contains("chunked")) {
        request.body = chunked(&mut reader)?;
    } else if let Some(length) = request.header("content-length") {
        let length: usize = length.parse().context("the request length is malformed")?;
        if length > MAX_BODY {
            bail!("the request body is too large");
        }
        request.body = vec![0; length];
        reader.read_exact(&mut request.body).context("the request body ended early")?;
    }
    Ok(request)
}

fn chunked(reader: &mut impl BufRead) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).context("a request chunk could not be read")?;
        let size = usize::from_str_radix(line.trim().split(';').next().unwrap_or_default(), 16).context("a request chunk is malformed")?;
        if size == 0 {
            loop {
                line.clear();
                if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
                    return Ok(body);
                }
            }
        }
        if size > MAX_BODY - body.len() {
            bail!("the request body is too large");
        }
        let start = body.len();
        body.resize(start + size, 0);
        reader.read_exact(&mut body[start..]).context("a request chunk ended early")?;
        let mut end = [0u8; 2];
        reader.read_exact(&mut end).context("a request chunk ended early")?;
    }
}

/// Writes `response` and asks the game to close the connection.
pub(super) fn write(mut stream: impl Write, response: &Response) -> io::Result<()> {
    let mut head = format!("HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n", response.status, reason(response.status), response.body.len());
    if let Some(kind) = response.content_type.as_deref().filter(|k| !k.chars().any(char::is_control)) {
        head.push_str(&format!("Content-Type: {kind}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(&response.body)?;
    stream.flush()
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        429 => "Too Many Requests",
        502 => "Bad Gateway",
        _ => "Status",
    }
}