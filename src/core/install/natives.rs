use std::fs::File;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

const MARKER: &str = ".complete";
const LIBRARY_EXTENSIONS: [&str; 4] = ["dll", "so", "dylib", "jnilib"];

/// Extracts the native libraries of `jars` flat into `dest`. A finished folder is left alone;
/// otherwise it is built beside `dest` and renamed into place, so a crash never leaves a half-filled one.
pub fn extract(jars: &[(PathBuf, Vec<String>)], dest: &Path) -> Result<()> {
    if dest.join(MARKER).is_file() {
        return Ok(());
    }
    let parent = dest.parent().context("natives folder has no parent")?;
    std::fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
    let tmp = tempfile::Builder::new().prefix(".natives-").tempdir_in(parent).context("cannot create a temporary natives folder")?;
    for (jar, exclude) in jars {
        extract_jar(jar, exclude, tmp.path()).with_context(|| format!("cannot extract {}", jar.display()))?;
    }
    File::create(tmp.path().join(MARKER)).context("cannot finish the natives folder")?;
    let tmp = tmp.keep();
    if dest.join(MARKER).is_file() {
        std::fs::remove_dir_all(&tmp).ok();
        return Ok(());
    }
    if dest.exists() {
        std::fs::remove_dir_all(dest).ok();
    }
    match std::fs::rename(&tmp, dest) {
        Ok(()) => Ok(()),
        Err(_) if dest.join(MARKER).is_file() => {
            std::fs::remove_dir_all(&tmp).ok();
            Ok(())
        }
        Err(e) => Err(e).with_context(|| format!("cannot create {}", dest.display())),
    }
}

fn extract_jar(jar: &Path, exclude: &[String], dest: &Path) -> Result<()> {
    let mut archive = zip::ZipArchive::new(File::open(jar)?)?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        if entry.is_dir() {
            continue;
        }
        let Some(path) = entry.enclosed_name() else {
            continue;
        };
        let rel = path.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/");
        if exclude.iter().any(|prefix| rel.starts_with(prefix.as_str())) {
            continue;
        }
        let Some(name) = path.file_name().map(std::ffi::OsStr::to_owned) else {
            continue;
        };
        let is_library = Path::new(&name).extension().and_then(|e| e.to_str()).is_some_and(|e| LIBRARY_EXTENSIONS.iter().any(|l| l.eq_ignore_ascii_case(e)));
        if !is_library {
            continue;
        }
        let mut out = File::create(dest.join(&name))?;
        std::io::copy(&mut entry, &mut out)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn jar(path: &Path, entries: &[(&str, &[u8])]) {
        let mut zip = zip::ZipWriter::new(File::create(path).unwrap());
        for (name, data) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
    }

    fn temp_leftovers(dir: &Path) -> usize {
        std::fs::read_dir(dir).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().starts_with(".natives-")).count()
    }

    #[test]
    fn libraries_are_extracted_flat_and_the_rest_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("legacy.jar");
        let modern = dir.path().join("modern.jar");
        jar(&legacy, &[("lwjgl.dll", b"a"), ("META-INF/MANIFEST.MF", b"m"), ("META-INF/evil.dll", b"x")]);
        jar(&modern, &[("windows/x64/org/lwjgl/lwjgl_stb.dll", b"b"), ("windows/x64/org/lwjgl/lwjgl_stb.dll.sha1", b"c")]);
        let dest = dir.path().join("v/natives");
        extract(&[(legacy, vec!["META-INF/".into()]), (modern, Vec::new())], &dest).unwrap();
        assert_eq!(std::fs::read(dest.join("lwjgl.dll")).unwrap(), b"a");
        assert_eq!(std::fs::read(dest.join("lwjgl_stb.dll")).unwrap(), b"b");
        assert!(!dest.join("MANIFEST.MF").exists());
        assert!(!dest.join("evil.dll").exists());
        assert!(!dest.join("lwjgl_stb.dll.sha1").exists());
    }

    #[test]
    fn finished_folder_is_not_touched_and_a_stale_one_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let jar_path = dir.path().join("a.jar");
        jar(&jar_path, &[("a.so", b"1")]);
        let dest = dir.path().join("natives");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("stale.so"), b"old").unwrap();
        extract(&[(jar_path.clone(), Vec::new())], &dest).unwrap();
        assert!(dest.join("a.so").is_file());
        assert!(!dest.join("stale.so").exists());
        std::fs::write(dest.join("marker-check"), b"keep").unwrap();
        extract(&[(jar_path, Vec::new())], &dest).unwrap();
        assert!(dest.join("marker-check").is_file());
        assert_eq!(temp_leftovers(dir.path()), 0);
    }
}
