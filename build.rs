use std::path::{Path, PathBuf};
use std::process::Command;

const COMPANION_SOURCES: [&str; 8] = ["common/src", "common/build.gradle", "fabric/src", "fabric/build.gradle", "build.gradle", "gradle/loader.gradle", "gradle.properties", "settings.gradle"];

fn main() {
    for path in COMPANION_SOURCES {
        println!("cargo:rerun-if-changed=mod/{path}");
    }
    println!("cargo:rerun-if-env-changed=MANYCRAFT_COMPANION_JAR");
    let jar = match std::env::var_os("MANYCRAFT_COMPANION_JAR") {
        Some(path) => PathBuf::from(path),
        None => build_companion(),
    };
    println!("cargo:rustc-env=MANYCRAFT_COMPANION_JAR={}", jar.display());
    embed_icon();
}

fn embed_icon() {
    println!("cargo:rerun-if-changed=packaging/icons/manycraft.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("packaging/icons/manycraft.ico");
        res.set("ProductName", "Manycraft");
        res.set("FileDescription", "Manycraft");
        if let Err(e) = res.compile() {
            println!("cargo:warning=cannot embed the exe icon: {e}");
        }
    }
}

fn build_companion() -> PathBuf {
    let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"));
    let module = root.join("mod");
    let version = std::fs::read_to_string(module.join("gradle.properties"))
        .expect("mod/gradle.properties is readable")
        .lines()
        .find_map(|l| l.strip_prefix("version=").map(str::trim).map(str::to_owned))
        .expect("mod/gradle.properties has a version");
    let jar = module.join("fabric").join("build").join("libs").join(format!("manycraft-companion-{version}.jar"));
    if !jar.is_file() || newer_sources(&module, &jar) {
        let gradlew = module.join(if cfg!(windows) { "gradlew.bat" } else { "gradlew" });
        let status = Command::new(&gradlew)
            .args([":fabric:build", "-PfabricOnly", "-q", "--console=plain", "-x", "test"])
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
    let mut stack: Vec<_> = COMPANION_SOURCES.iter().map(|p| module.join(p)).collect();
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
