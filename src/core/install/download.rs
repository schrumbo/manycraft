use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result, bail};
use rayon::prelude::*;

use super::{Progress, Stage};
use crate::core::net;

const WORKERS: usize = 16;

/// One file to fetch into the shared game folders.
#[derive(Clone, Debug)]
pub struct Job {
    pub url: String,
    pub path: PathBuf,
    pub sha1: Option<String>,
    pub size: Option<u64>,
    pub strict: bool,
}

impl Job {
    pub fn new(url: String, path: PathBuf, sha1: Option<String>, size: Option<u64>) -> Self {
        Job { url, path, sha1, size, strict: false }
    }

    /// Checks the hash of an existing file even when its size matches; for files that can change under the same name.
    pub fn strict(mut self) -> Self {
        self.strict = true;
        self
    }
}

/// Bounded parallel downloader that counts the files it actually fetched.
pub struct Downloader {
    agent: ureq::Agent,
    pool: rayon::ThreadPool,
    fetched: AtomicUsize,
}

impl Downloader {
    pub fn new() -> Result<Self> {
        let pool = rayon::ThreadPoolBuilder::new().num_threads(WORKERS).build().context("cannot start download workers")?;
        Ok(Downloader { agent: net::agent(None)?, pool, fetched: AtomicUsize::new(0) })
    }

    pub fn agent(&self) -> &ureq::Agent {
        &self.agent
    }

    pub fn fetched(&self) -> usize {
        self.fetched.load(Ordering::Relaxed)
    }

    /// Fetches `job` unless the file is already there; true when it was downloaded.
    pub fn one(&self, job: &Job) -> Result<bool> {
        if present(job) {
            return Ok(false);
        }
        std::fs::remove_file(&job.path).ok();
        net::fetch_verified(&self.agent, &job.url, &job.path, job.sha1.as_deref())?;
        self.fetched.fetch_add(1, Ordering::Relaxed);
        Ok(true)
    }

    /// Runs all jobs on the worker pool, reporting progress after each file.
    pub fn all(&self, stage: Stage, jobs: &[Job], progress: &(dyn Fn(Progress) + Sync)) -> Result<()> {
        let total = jobs.len();
        progress(Progress { stage, done: 0, total });
        let done = AtomicUsize::new(0);
        self.pool.install(|| {
            jobs.par_iter().try_for_each(|job| {
                self.one(job)?;
                progress(Progress { stage, done: done.fetch_add(1, Ordering::Relaxed) + 1, total });
                Ok(())
            })
        })
    }
}

fn present(job: &Job) -> bool {
    let Ok(meta) = std::fs::metadata(&job.path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    let hash_matches = || job.sha1.as_ref().is_none_or(|sha1| net::file_sha1(&job.path).is_ok_and(|h| h.eq_ignore_ascii_case(sha1)));
    match job.size {
        Some(size) => meta.len() == size && (!job.strict || hash_matches()),
        None => hash_matches(),
    }
}

/// `base` joined with `rel`, refusing anything that would leave `base`.
pub fn join_safe(base: &Path, rel: &str) -> Result<PathBuf> {
    let rel = Path::new(rel);
    if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
        bail!("unsafe path {}", rel.display());
    }
    Ok(base.join(rel))
}

/// Parses the JSON file at `path`. A file that does not parse is removed so the next run fetches it again.
pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    match serde_json::from_slice(&bytes) {
        Ok(value) => Ok(value),
        Err(e) => {
            std::fs::remove_file(path).ok();
            bail!("{} was damaged and has been removed, run again: {e}", path.display())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ABC_SHA1: &str = "a9993e364706816aba3e25717850c26c9cd0d89d";

    fn file(content: &[u8]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.bin");
        std::fs::write(&path, content).unwrap();
        (dir, path)
    }

    fn job(path: &Path, sha1: Option<&str>, size: Option<u64>) -> Job {
        Job::new("http://127.0.0.1:1/x".into(), path.to_path_buf(), sha1.map(str::to_owned), size)
    }

    #[test]
    fn join_safe_keeps_paths_inside_the_base() {
        let base = Path::new("base");
        assert_eq!(join_safe(base, "a/b.jar").unwrap(), base.join("a").join("b.jar"));
        assert!(join_safe(base, "../x").is_err());
        assert!(join_safe(base, "a/../../x").is_err());
        assert!(join_safe(base, "/etc/passwd").is_err());
    }

    #[test]
    fn existing_files_are_not_fetched() {
        let (_dir, path) = file(b"abc");
        let dl = Downloader::new().unwrap();
        assert!(!dl.one(&job(&path, None, Some(3))).unwrap());
        assert!(!dl.one(&job(&path, Some(ABC_SHA1), None)).unwrap());
        assert!(!dl.one(&job(&path, None, None)).unwrap());
        assert!(!dl.one(&job(&path, Some(ABC_SHA1), Some(3)).strict()).unwrap());
        assert_eq!(dl.fetched(), 0);
    }

    #[test]
    fn wrong_size_is_not_present() {
        let (dir, path) = file(b"abc");
        assert!(!present(&job(&path, None, Some(99))));
        assert!(!present(&job(&dir.path().join("missing"), None, Some(3))));
    }

    #[test]
    fn strict_jobs_notice_a_changed_hash_at_the_same_size() {
        let (_dir, path) = file(b"abd");
        let loose = job(&path, Some(ABC_SHA1), Some(3));
        assert!(present(&loose));
        assert!(!present(&loose.strict()));
    }

    #[test]
    fn damaged_json_is_removed() {
        let (_dir, path) = file(b"{not json");
        assert!(read_json::<serde_json::Value>(&path).is_err());
        assert!(!path.exists());
    }
}
