//! Microsoft sign-in in Manycraft's own sign-in window: a child process of this exe that shows the Microsoft page in a fresh web view
//! and hands back only the authorization code. It talks over its stdin and stdout, one line per message; the code never appears in argv or logs.

use std::fmt;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::auth::{LIVE_CLIENT_ID, LIVE_SCOPE};

/// The hidden subcommand that runs the sign-in window.
pub const SUBCOMMAND: &str = "signin-window";
/// Where Microsoft sends the browser once the sign-in is done.
pub const REDIRECT: &str = "https://login.live.com/oauth20_desktop.srf";
const AUTHORIZE: &str = "https://login.live.com/oauth20_authorize.srf";
/// Prefix of the sign-in window's temporary web view folders.
pub const STORE_PREFIX: &str = "manycraft-signin-";
const SLICE: Duration = Duration::from_millis(100);
const CLOSE_GRACE: Duration = Duration::from_secs(8);
const STALE_STORE: Duration = Duration::from_secs(3600);

/// The Microsoft page that starts an authorization-code sign-in and lets the person pick any account.
pub fn authorize_url() -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("client_id", LIVE_CLIENT_ID)
        .append_pair("response_type", "code")
        .append_pair("scope", LIVE_SCOPE)
        .append_pair("redirect_uri", REDIRECT)
        .append_pair("prompt", "select_account")
        .finish();
    format!("{AUTHORIZE}?{query}")
}

/// The device-code page with the code already filled in.
pub fn link_with_code(verification_uri: &str, user_code: &str) -> String {
    let sep = if verification_uri.contains('?') { '&' } else { '?' };
    let code: String = url::form_urlencoded::byte_serialize(user_code.as_bytes()).collect();
    format!("{verification_uri}{sep}otc={code}")
}

/// How the sign-in ended at the redirect page.
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
#[derive(PartialEq, Eq)]
pub enum Redirect {
    Code(String),
    Error { error: String, description: String },
}

impl fmt::Debug for Redirect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Code(_) => f.write_str("Code(..)"),
            Self::Error { error, description } => f.debug_struct("Error").field("error", error).field("description", description).finish(),
        }
    }
}

/// What a navigation to `address` means: `None` for any page but the redirect page, else the code or the error it carries.
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
pub fn redirect(address: &str) -> Option<Redirect> {
    let parsed = url::Url::parse(address).ok()?;
    let expected = url::Url::parse(REDIRECT).ok()?;
    if parsed.scheme() != expected.scheme() || parsed.host_str() != expected.host_str() || parsed.path() != expected.path() {
        return None;
    }
    let pairs: Vec<(String, String)> = parsed.query_pairs().chain(parsed.fragment().map(|f| url::form_urlencoded::parse(f.as_bytes())).into_iter().flatten()).map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
    let get = |name: &str| pairs.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
    if let Some(error) = get("error") {
        return Some(Redirect::Error { error, description: get("error_description").unwrap_or_default() });
    }
    get("code").filter(|c| !c.is_empty()).map(Redirect::Code)
}

/// What the parent tells the window, as the first line on its stdin.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Request {
    pub url: String,
    pub title: String,
    pub browser_label: String,
    /// Holds back every passkey prompt, for automated runs that must never raise a system dialog.
    #[serde(default)]
    pub quiet: bool,
}

/// One line the window sends back on its stdout.
#[derive(PartialEq, Eq)]
pub enum Reply {
    /// The window is on screen.
    Ready,
    /// The sign-in finished with this authorization code.
    Code(String),
    /// Microsoft redirected with an error instead of a code.
    Error { error: String, description: String },
    /// The person asked to sign in in the browser instead.
    Browser,
    /// The window could not be shown, and why.
    Failed(String),
}

impl fmt::Debug for Reply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ready => f.write_str("Ready"),
            Self::Code(_) => f.write_str("Code(..)"),
            Self::Error { error, description } => f.debug_struct("Error").field("error", error).field("description", description).finish(),
            Self::Browser => f.write_str("Browser"),
            Self::Failed(why) => f.debug_tuple("Failed").field(why).finish(),
        }
    }
}

impl Reply {
    /// The reply as one protocol line, without the line break.
    #[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
    pub fn line(&self) -> String {
        let one = |s: &str| s.replace(['\r', '\n'], " ");
        match self {
            Self::Ready => "ready".into(),
            Self::Code(code) => format!("code {}", one(code)),
            Self::Error { error, description } => format!("error {} {}", one(error).replace(' ', "_"), one(description)).trim_end().to_string(),
            Self::Browser => "browser".into(),
            Self::Failed(why) => format!("failed {}", one(why)).trim_end().to_string(),
        }
    }

    /// Reads one protocol line; anything else is ignored.
    pub fn parse(line: &str) -> Option<Self> {
        let line = line.trim_end_matches(['\r', '\n']);
        let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
        match word {
            "ready" => Some(Self::Ready),
            "code" if !rest.is_empty() => Some(Self::Code(rest.to_string())),
            "error" => {
                let (error, description) = rest.split_once(' ').unwrap_or((rest, ""));
                Some(Self::Error { error: error.to_string(), description: description.to_string() })
            }
            "browser" => Some(Self::Browser),
            "failed" => Some(Self::Failed(rest.to_string())),
            _ => None,
        }
    }

    /// Converts a redirect into the reply that carries it.
    #[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
    pub fn from_redirect(redirect: Redirect) -> Self {
        match redirect {
            Redirect::Code(code) => Self::Code(code),
            Redirect::Error { error, description } => Self::Error { error, description },
        }
    }
}

/// How a sign-in in the window ended, as the parent sees it.
#[derive(PartialEq, Eq)]
pub enum Outcome {
    Code(String),
    Denied { error: String, description: String },
    Browser,
    /// The person closed the window.
    Closed,
    /// The window never came up; carries why.
    NoWindow(String),
    Cancelled,
}

impl fmt::Debug for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Code(_) => f.write_str("Code(..)"),
            Self::Denied { error, description } => f.debug_struct("Denied").field("error", error).field("description", description).finish(),
            Self::Browser => f.write_str("Browser"),
            Self::Closed => f.write_str("Closed"),
            Self::NoWindow(why) => f.debug_tuple("NoWindow").field(why).finish(),
            Self::Cancelled => f.write_str("Cancelled"),
        }
    }
}

/// A running sign-in window. Dropping it closes the window.
pub struct Window {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<Reply>,
}

impl Window {
    /// Starts this exe's sign-in window for `request`.
    pub fn open(request: &Request) -> Result<Self> {
        let exe = std::env::current_exe().context("cannot find Manycraft's own program file")?;
        let mut command = Command::new(exe);
        command.arg(SUBCOMMAND).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        hide_console(&mut command);
        sweep(&std::env::temp_dir(), SystemTime::now());
        Self::start(command, request)
    }

    fn start(mut command: Command, request: &Request) -> Result<Self> {
        let mut child = command.spawn().context("cannot start the sign-in window")?;
        let mut stdin = child.stdin.take().context("the sign-in window has no input")?;
        let stdout = child.stdout.take().context("the sign-in window has no output")?;
        writeln!(stdin, "{}", serde_json::to_string(request)?).and_then(|()| stdin.flush()).context("cannot talk to the sign-in window")?;
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut bytes = Vec::new();
            while reader.read_until(b'\n', &mut bytes).is_ok_and(|n| n > 0) {
                let line = String::from_utf8_lossy(&bytes).trim_end().to_string();
                bytes.clear();
                if let Some(reply) = Reply::parse(&line)
                    && tx.send(reply).is_err()
                {
                    break;
                }
            }
        });
        Ok(Self { child, stdin: Some(stdin), lines })
    }

    /// Waits until the person finishes, leaves or closes the window, or `cancel` is set. `on_ready` runs once the window is on screen.
    pub fn wait(&mut self, cancel: &AtomicBool, mut on_ready: impl FnMut()) -> Outcome {
        let mut ready = false;
        loop {
            if cancel.load(Ordering::SeqCst) {
                self.close();
                return Outcome::Cancelled;
            }
            match self.lines.recv_timeout(SLICE) {
                Ok(Reply::Ready) => {
                    if !ready {
                        on_ready();
                    }
                    ready = true;
                }
                Ok(Reply::Code(code)) => return Outcome::Code(code),
                Ok(Reply::Error { error, description }) => return Outcome::Denied { error, description },
                Ok(Reply::Browser) => return Outcome::Browser,
                Ok(Reply::Failed(why)) => return Outcome::NoWindow(why),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) if ready => return Outcome::Closed,
                Err(RecvTimeoutError::Disconnected) => {
                    let status = self.child.wait().map(|s| s.to_string()).unwrap_or_else(|e| e.to_string());
                    return Outcome::NoWindow(format!("the sign-in window stopped: {status}"));
                }
            }
        }
    }

    /// Closes the window: its input ends, so it quits and cleans up; one that hangs is killed after a grace period.
    pub fn close(&mut self) {
        drop(self.stdin.take());
        let end = Instant::now() + CLOSE_GRACE;
        while Instant::now() < end {
            match self.child.try_wait() {
                Ok(Some(_)) | Err(_) => return,
                Ok(None) => std::thread::sleep(SLICE),
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        self.close();
    }
}

fn sweep(temp: &Path, now: SystemTime) {
    let Ok(entries) = std::fs::read_dir(temp) else { return };
    for entry in entries.flatten() {
        let old = entry.metadata().and_then(|m| m.modified()).is_ok_and(|t| now.duration_since(t).is_ok_and(|age| age > STALE_STORE));
        if old && entry.file_name().to_string_lossy().starts_with(STORE_PREFIX) && entry.file_type().is_ok_and(|t| t.is_dir()) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(windows)]
fn hide_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console(_command: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorize_url_asks_for_a_code_with_the_live_client() {
        let url = url::Url::parse(&authorize_url()).unwrap();
        assert_eq!(url.host_str(), Some("login.live.com"));
        assert_eq!(url.path(), "/oauth20_authorize.srf");
        let pairs: Vec<(String, String)> = url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
        let get = |k: &str| pairs.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert_eq!(get("client_id"), Some("00000000402b5328"));
        assert_eq!(get("response_type"), Some("code"));
        assert_eq!(get("scope"), Some("service::user.auth.xboxlive.com::MBI_SSL"));
        assert_eq!(get("redirect_uri"), Some(REDIRECT));
        assert_eq!(get("prompt"), Some("select_account"));
    }

    #[test]
    fn link_carries_the_code() {
        assert_eq!(link_with_code("https://www.microsoft.com/link", "AB12-CD34"), "https://www.microsoft.com/link?otc=AB12-CD34");
        assert_eq!(link_with_code("https://example.com/x?y=1", "A B"), "https://example.com/x?y=1&otc=A+B");
    }

    #[test]
    fn redirect_with_a_code() {
        assert_eq!(redirect("https://login.live.com/oauth20_desktop.srf?code=M.C540_BAY.2.U.abc-def&lc=1031"), Some(Redirect::Code("M.C540_BAY.2.U.abc-def".into())));
        assert_eq!(redirect("https://login.live.com/oauth20_desktop.srf?lc=1031&code=a%2Bb%3D"), Some(Redirect::Code("a+b=".into())));
        assert_eq!(redirect("https://login.live.com/oauth20_desktop.srf#code=frag"), Some(Redirect::Code("frag".into())));
    }

    #[test]
    fn redirect_with_an_error() {
        assert_eq!(
            redirect("https://login.live.com/oauth20_desktop.srf?error=access_denied&error_description=The+user+has+denied+access+to+the+scopes+requested+by+the+client+application.&lc=1031"),
            Some(Redirect::Error { error: "access_denied".into(), description: "The user has denied access to the scopes requested by the client application.".into() })
        );
        assert_eq!(redirect("https://login.live.com/oauth20_desktop.srf?error=server_error&code=x"), Some(Redirect::Error { error: "server_error".into(), description: String::new() }));
    }

    #[test]
    fn other_pages_are_not_the_redirect() {
        assert_eq!(redirect("https://login.live.com/oauth20_authorize.srf?code=x"), None);
        assert_eq!(redirect("https://login.live.com/oauth20_desktop.srf"), None);
        assert_eq!(redirect("https://login.live.com/oauth20_desktop.srf?code="), None);
        assert_eq!(redirect("http://login.live.com/oauth20_desktop.srf?code=x"), None);
        assert_eq!(redirect("https://login.live.com.evil.example/oauth20_desktop.srf?code=x"), None);
        assert_eq!(redirect("https://evil.example/?u=https://login.live.com/oauth20_desktop.srf?code=x"), None);
        assert_eq!(redirect("not a url"), None);
    }

    #[test]
    fn replies_round_trip_as_lines() {
        for reply in [
            Reply::Ready,
            Reply::Code("M.C540_BAY.2.U.abc".into()),
            Reply::Error { error: "access_denied".into(), description: "The user said no.".into() },
            Reply::Error { error: "x".into(), description: String::new() },
            Reply::Browser,
            Reply::Failed("WebView2 is missing".into()),
        ] {
            assert_eq!(Reply::parse(&reply.line()), Some(reply));
        }
    }

    #[test]
    fn reply_lines_stay_one_line() {
        assert_eq!(Reply::Failed("a\nb\r\nc".into()).line(), "failed a b  c");
        assert_eq!(Reply::Error { error: "bad thing".into(), description: "two\nlines".into() }.line(), "error bad_thing two lines");
        assert_eq!(Reply::parse("code abc\r\n"), Some(Reply::Code("abc".into())));
        assert_eq!(Reply::parse("code"), None);
        assert_eq!(Reply::parse("[1104/120000.123:ERROR:something] noise"), None);
        assert_eq!(Reply::parse(""), None);
    }

    #[test]
    fn debug_output_hides_the_code() {
        assert!(!format!("{:?}", Reply::Code("secret".into())).contains("secret"));
        assert!(!format!("{:?}", Outcome::Code("secret".into())).contains("secret"));
        assert!(!format!("{:?}", Redirect::Code("secret".into())).contains("secret"));
        assert_eq!(format!("{:?}", Redirect::Code("secret".into())), "Code(..)");
        assert!(format!("{:?}", Redirect::Error { error: "access_denied".into(), description: String::new() }).contains("access_denied"));
    }

    #[test]
    fn sweeping_removes_only_old_window_folders() {
        let temp = tempfile::tempdir().unwrap();
        let old = temp.path().join(format!("{STORE_PREFIX}old"));
        let other = temp.path().join("someone-else");
        let file = temp.path().join(format!("{STORE_PREFIX}file"));
        std::fs::create_dir(&old).unwrap();
        std::fs::create_dir(&other).unwrap();
        std::fs::write(&file, "x").unwrap();
        sweep(temp.path(), SystemTime::now());
        assert!(old.exists());
        sweep(temp.path(), SystemTime::now() + STALE_STORE + Duration::from_secs(60));
        assert!(!old.exists() && other.exists() && file.exists());
    }

    #[test]
    fn older_requests_are_not_quiet() {
        let request: Request = serde_json::from_str(r#"{"url":"u","title":"t","browser_label":"b"}"#).unwrap();
        assert!(!request.quiet);
    }

    #[test]
    fn request_is_one_json_line() {
        let request = Request { url: authorize_url(), title: "Bei Microsoft\nanmelden".into(), browser_label: "Im Browser öffnen".into(), quiet: true };
        let line = serde_json::to_string(&request).unwrap();
        assert!(!line.contains('\n'));
        assert_eq!(serde_json::from_str::<Request>(&line).unwrap(), request);
    }

    fn script(body: &str) -> Command {
        if cfg!(windows) {
            let mut c = Command::new("powershell");
            c.args(["-NoProfile", "-NonInteractive", "-Command", body]);
            c
        } else {
            let mut c = Command::new("sh");
            c.args(["-c", body]);
            c
        }
    }

    #[test]
    fn a_window_that_ignores_the_close_is_killed_after_the_grace() {
        let mut stuck = window(if cfg!(windows) { "Write-Output ready; Start-Sleep 60" } else { "echo ready; sleep 60" });
        assert!(matches!(stuck.lines.recv_timeout(Duration::from_secs(30)), Ok(Reply::Ready)));
        let pid = stuck.child.id();
        let started = Instant::now();
        drop(stuck);
        let took = started.elapsed();
        assert!(took + Duration::from_millis(500) >= CLOSE_GRACE && took < CLOSE_GRACE + Duration::from_secs(5), "{took:?} for {pid}");
    }

    fn window(body: &str) -> Window {
        let mut command = script(body);
        command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        Window::start(command, &Request { url: "u".into(), title: "t".into(), browser_label: "b".into(), quiet: false }).unwrap()
    }

    #[test]
    fn child_protocol_code_after_ready() {
        let mut w = window(if cfg!(windows) { "[Console]::In.ReadLine() | Out-Null; 'ready'; 'code abc'" } else { "read l; echo ready; echo 'code abc'" });
        let mut readies = 0;
        assert_eq!(w.wait(&AtomicBool::new(false), || readies += 1), Outcome::Code("abc".into()));
        assert_eq!(readies, 1);
    }

    #[test]
    fn child_protocol_closed_after_ready_and_never_ready() {
        let mut w = window(if cfg!(windows) { "'ready'" } else { "echo ready" });
        assert_eq!(w.wait(&AtomicBool::new(false), || {}), Outcome::Closed);
        let mut w = window("exit 3");
        assert!(matches!(w.wait(&AtomicBool::new(false), || {}), Outcome::NoWindow(_)));
        let mut w = window(if cfg!(windows) { "'failed no web view'" } else { "echo 'failed no web view'" });
        assert_eq!(w.wait(&AtomicBool::new(false), || {}), Outcome::NoWindow("no web view".into()));
    }

    #[test]
    fn child_protocol_browser_and_denied() {
        let mut w = window(if cfg!(windows) { "'ready'; 'browser'" } else { "echo ready; echo browser" });
        assert_eq!(w.wait(&AtomicBool::new(false), || {}), Outcome::Browser);
        let mut w = window(if cfg!(windows) { "'ready'; 'error access_denied no'" } else { "echo ready; echo 'error access_denied no'" });
        assert_eq!(w.wait(&AtomicBool::new(false), || {}), Outcome::Denied { error: "access_denied".into(), description: "no".into() });
    }

    #[test]
    fn cancelling_closes_a_window_that_waits_for_its_input_to_end() {
        let mut w = window(if cfg!(windows) { "'ready'; while ($null -ne [Console]::In.ReadLine()) {}" } else { "echo ready; cat > /dev/null" });
        let cancel = AtomicBool::new(false);
        let started = Instant::now();
        assert_eq!(w.wait(&cancel, || cancel.store(true, Ordering::SeqCst)), Outcome::Cancelled);
        assert!(w.child.try_wait().unwrap().is_some());
        assert!(started.elapsed() < CLOSE_GRACE + Duration::from_secs(5));
    }
}
