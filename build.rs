use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=mod/src");
    println!("cargo:rerun-if-changed=mod/build.gradle");
    println!("cargo:rerun-if-changed=mod/gradle.properties");
    println!("cargo:rerun-if-env-changed=MANYCRAFT_COMPANION_JAR");
    let jar = match std::env::var_os("MANYCRAFT_COMPANION_JAR") {
        Some(path) => PathBuf::from(path),
        None => build_companion(),
    };
    println!("cargo:rustc-env=MANYCRAFT_COMPANION_JAR={}", jar.display());
}

fn build_companion() -> PathBuf {
    let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"));
    let module = root.join("mod");
    let version = std::fs::read_to_string(module.join("gradle.properties"))
        .expect("mod/gradle.properties is readable")
        .lines()
        .find_map(|l| l.strip_prefix("version=").map(str::trim).map(str::to_owned))
        .expect("mod/gradle.properties has a version");
    let jar = module.join("build").join("libs").join(format!("manycraft-companion-{version}.jar"));
    if !jar.is_file() || newer_sources(&module, &jar) {
        let gradlew = module.join(if cfg!(windows) { "gradlew.bat" } else { "gradlew" });
        let status = Command::new(&gradlew)
            .args(["build", "-q", "--console=plain", "-x", "test"])
            .current_dir(&module)
            .status()
            .unwrap_or_else(|e| panic!("cannot run {}: {e}; install JDK 25 or set MANYCRAFT_COMPANION_JAR", gradlew.display()));
        assert!(status.success(), "building the companion mod failed; install JDK 25 or set MANYCRAFT_COMPANION_JAR");
    }
    assert!(jar.is_file(), "companion mod jar missing at {}", jar.display());
    jar
}

fn newer_sources(module: &Path, jar: &Path) -> bool {
    let Ok(built) = jar.metadata().and_then(|m| m.modified()) else { return true };
    let mut stack = vec![module.join("src"), module.join("build.gradle"), module.join("gradle.properties")];
    while let Some(path) = stack.pop() {
        if path.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&path) {
                stack.extend(entries.flatten().map(|e| e.path()));
            }
        } else if path.metadata().and_then(|m| m.modified()).is_ok_and(|t| t > built) {
            return true;
        }
    }
    false
}
