use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use super::download::{Downloader, Job, join_safe, read_json};
use super::meta::{Arch, Artifact, Os};
use super::{Progress, Stage};
use crate::core::net;

const ALL_URL: &str = "https://launchermeta.mojang.com/v1/products/java-runtime/2ec0cc96c44e5a76b9c8b7c39df7210883d12871/all.json";
const MARKER: &str = ".complete";

#[derive(Deserialize)]
struct Entry {
    manifest: Artifact,
}

#[derive(Deserialize)]
struct Manifest {
    files: HashMap<String, FileEntry>,
}

#[derive(Deserialize)]
struct FileEntry {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    executable: bool,
    #[serde(default)]
    downloads: Option<FileDownloads>,
    #[serde(default)]
    target: Option<String>,
}

#[derive(Deserialize)]
struct FileDownloads {
    #[serde(default)]
    raw: Option<Artifact>,
}

/// Every Java runtime Mojang offers, by platform and component.
pub struct Index(HashMap<String, HashMap<String, Vec<Entry>>>);

/// A Java runtime Mojang offers for this platform, identified by the hash of its file list.
#[derive(Clone, Debug)]
pub struct Runtime {
    pub component: String,
    manifest: Artifact,
    sha1: String,
}

/// Mojang's keys for the Java runtimes of a platform, best match first. Where Mojang ships no native
/// ARM runtime, the x64 one runs under emulation.
pub fn platform_keys(os: Os, arch: Arch) -> &'static [&'static str] {
    match (os, arch) {
        (Os::Windows, Arch::X64) => &["windows-x64"],
        (Os::Windows, Arch::Arm64) => &["windows-arm64", "windows-x64"],
        (Os::Windows, Arch::X86) => &["windows-x86"],
        (Os::Osx, Arch::X64) => &["mac-os"],
        (Os::Osx, Arch::Arm64) => &["mac-os-arm64", "mac-os"],
        (Os::Linux, Arch::X64) => &["linux"],
        (Os::Linux, Arch::X86) => &["linux-i386"],
        _ => &[],
    }
}

/// Java launcher inside a runtime folder.
pub fn executable(os: Os) -> &'static str {
    match os {
        Os::Windows => "bin/javaw.exe",
        Os::Osx => "jre.bundle/Contents/Home/bin/java",
        Os::Linux => "bin/java",
    }
}

/// Asks Mojang which Java runtimes it currently offers; fails on a platform it has none for.
pub fn index(agent: &ureq::Agent) -> Result<Index> {
    let keys = platform_keys(Os::current(), Arch::current());
    if keys.is_empty() {
        bail!("Mojang provides no Java runtime for this platform");
    }
    Ok(Index(net::get_json(agent, ALL_URL)?))
}

/// The runtime `component` for this platform from `index`.
pub fn pick(index: &Index, component: &str) -> Result<Runtime> {
    let keys = platform_keys(Os::current(), Arch::current());
    let entry = keys.iter().find_map(|key| index.0.get(*key)?.get(component)?.first());
    let Some(entry) = entry else {
        bail!("no Java runtime {component} for this platform");
    };
    let sha1 = entry.manifest.sha1.clone().context("Java runtime has no checksum")?;
    Ok(Runtime { component: component.to_owned(), manifest: entry.manifest.clone(), sha1 })
}

/// Installs `runtime` below `runtimes/<component>/<hash>` and returns its Java launcher.
/// A finished folder is never modified, so running games keep the files they loaded.
pub fn ensure(dl: &Downloader, runtimes: &Path, runtime: &Runtime, progress: &(dyn Fn(Progress) + Sync)) -> Result<PathBuf> {
    let os = Os::current();
    let component_dir = join_safe(runtimes, &runtime.component)?;
    let root = join_safe(&component_dir, &runtime.sha1)?;
    let java = root.join(executable(os));
    if root.join(MARKER).is_file() && java.is_file() {
        return Ok(java);
    }
    let manifest_path = component_dir.join(format!("{}.json", runtime.sha1));
    dl.one(&Job::new(runtime.manifest.url.clone(), manifest_path.clone(), Some(runtime.sha1.clone()), runtime.manifest.size).strict())?;
    let manifest: Manifest = read_json(&manifest_path)?;
    let mut jobs = Vec::new();
    let mut executables = Vec::new();
    let mut links = Vec::new();
    for (rel, file) in &manifest.files {
        let path = join_safe(&root, rel)?;
        match file.kind.as_str() {
            "directory" => std::fs::create_dir_all(&path).with_context(|| format!("cannot create {}", path.display()))?,
            "file" => {
                let Some(raw) = file.downloads.as_ref().and_then(|d| d.raw.as_ref()) else {
                    continue;
                };
                if file.executable {
                    executables.push(path.clone());
                }
                jobs.push(Job::new(raw.url.clone(), path, raw.sha1.clone(), raw.size));
            }
            "link" => {
                if let Some(target) = &file.target {
                    links.push((path, target.clone()));
                }
            }
            _ => {}
        }
    }
    dl.all(Stage::Runtime, &jobs, progress)?;
    finish(&executables, &links)?;
    if !java.is_file() {
        bail!("Java runtime {} has no launcher at {}", runtime.component, java.display());
    }
    File::create(root.join(MARKER)).context("cannot finish the Java runtime")?;
    Ok(java)
}

/// Newest finished runtime of `component` already on disk, for starting without a connection.
pub fn local(runtimes: &Path, component: &str) -> Option<PathBuf> {
    let dir = join_safe(runtimes, component).ok()?;
    let java = executable(Os::current());
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|e| {
            let marker = e.path().join(MARKER);
            let finished = marker.metadata().and_then(|m| m.modified()).ok()?;
            Some((finished, e.path().join(java)))
        })
        .filter(|(_, java)| java.is_file())
        .max_by_key(|(finished, _): &(SystemTime, PathBuf)| *finished)
        .map(|(_, java)| java)
}

#[cfg(unix)]
fn finish(executables: &[PathBuf], links: &[(PathBuf, String)]) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    for path in executables {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).with_context(|| format!("cannot make {} executable", path.display()))?;
    }
    for (path, target) in links {
        if path.symlink_metadata().is_err() {
            std::os::unix::fs::symlink(target, path).with_context(|| format!("cannot link {}", path.display()))?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn finish(_executables: &[PathBuf], _links: &[(PathBuf, String)]) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_keys_match_mojangs_names() {
        assert_eq!(platform_keys(Os::Windows, Arch::X64), ["windows-x64"]);
        assert_eq!(platform_keys(Os::Osx, Arch::X64), ["mac-os"]);
        assert_eq!(platform_keys(Os::Linux, Arch::X64), ["linux"]);
        assert!(platform_keys(Os::Linux, Arch::Arm64).is_empty());
    }

    #[test]
    fn arm_falls_back_to_the_x64_runtime() {
        assert_eq!(platform_keys(Os::Osx, Arch::Arm64), ["mac-os-arm64", "mac-os"]);
        assert_eq!(platform_keys(Os::Windows, Arch::Arm64), ["windows-arm64", "windows-x64"]);
    }

    #[test]
    fn java_path_follows_the_os_layout() {
        assert_eq!(executable(Os::Windows), "bin/javaw.exe");
        assert_eq!(executable(Os::Osx), "jre.bundle/Contents/Home/bin/java");
        assert_eq!(executable(Os::Linux), "bin/java");
    }

    #[test]
    fn runtime_manifest_parses_files_directories_and_links() {
        let json = r#"{"files":{"bin":{"type":"directory"},"bin/java":{"type":"file","executable":true,"downloads":{"raw":{"sha1":"a","size":1,"url":"u"},"lzma":{"sha1":"b","size":1,"url":"v"}}},"lib/x":{"type":"link","target":"../y"}}}"#;
        let m: Manifest = serde_json::from_str(json).unwrap();
        assert_eq!(m.files.len(), 3);
        assert!(m.files["bin/java"].executable);
        assert_eq!(m.files["bin/java"].downloads.as_ref().unwrap().raw.as_ref().unwrap().url, "u");
        assert_eq!(m.files["lib/x"].target.as_deref(), Some("../y"));
    }

    #[test]
    fn a_runtime_is_picked_from_the_index_for_this_platform() {
        let key = platform_keys(Os::current(), Arch::current())[0];
        let json = format!(r#"{{"{key}":{{"comp":[{{"manifest":{{"sha1":"abc","size":1,"url":"u"}}}}],"empty":[]}},"elsewhere":{{"other":[{{"manifest":{{"sha1":"x","size":1,"url":"v"}}}}]}}}}"#);
        let index = Index(serde_json::from_str(&json).unwrap());
        let runtime = pick(&index, "comp").unwrap();
        assert_eq!((runtime.component.as_str(), runtime.sha1.as_str(), runtime.manifest.url.as_str()), ("comp", "abc", "u"));
        assert!(pick(&index, "empty").is_err());
        assert!(pick(&index, "other").is_err());
    }

    fn fake_runtime(runtimes: &Path, hash: &str, finished: bool) {
        let java = runtimes.join("comp").join(hash).join(executable(Os::current()));
        std::fs::create_dir_all(java.parent().unwrap()).unwrap();
        std::fs::write(&java, b"j").unwrap();
        if finished {
            std::fs::write(runtimes.join("comp").join(hash).join(MARKER), b"").unwrap();
        }
    }

    #[test]
    fn local_runtime_needs_the_finished_marker() {
        let dir = tempfile::tempdir().unwrap();
        assert!(local(dir.path(), "comp").is_none());
        fake_runtime(dir.path(), "half", false);
        assert!(local(dir.path(), "comp").is_none());
        fake_runtime(dir.path(), "done", true);
        let java = local(dir.path(), "comp").unwrap();
        assert!(java.starts_with(dir.path().join("comp").join("done")));
    }

    #[test]
    fn finished_runtime_is_returned_without_network() {
        let dir = tempfile::tempdir().unwrap();
        fake_runtime(dir.path(), "abc123", true);
        let dl = Downloader::new(net::agent(None).unwrap()).unwrap();
        let runtime = Runtime { component: "comp".into(), manifest: Artifact { url: "http://127.0.0.1:1/m".into(), ..Artifact::default() }, sha1: "abc123".into() };
        let java = ensure(&dl, dir.path(), &runtime, &|_| {}).unwrap();
        assert!(java.is_file());
        assert_eq!(dl.fetched(), 0);
    }
}
