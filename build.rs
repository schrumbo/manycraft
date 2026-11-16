use std::path::{Path, PathBuf};
use std::process::Command;

const AGENT_SOURCES: [&str; 4] = ["src", "build.gradle", "gradle.properties", "settings.gradle"];

fn main() {
    for path in AGENT_SOURCES {
        println!("cargo:rerun-if-changed=agent/{path}");
    }
    println!("cargo:rerun-if-env-changed=MANYCRAFT_AGENT_JAR");
    let jar = match std::env::var_os("MANYCRAFT_AGENT_JAR") {
        Some(path) if path.is_empty() => no_agent(),
        Some(path) => PathBuf::from(path),
        None => build_agent(),
    };
    println!("cargo:rustc-env=MANYCRAFT_AGENT_JAR={}", jar.display());
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

fn module() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR")).join("agent")
}

fn build_agent() -> PathBuf {
    let module = module();
    let jar = module.join("build").join("libs").join("manycraft-agent.jar");
    if !jar.is_file() || newer_sources(&module, &jar, &AGENT_SOURCES) {
        let gradlew = module.join(if cfg!(windows) { "gradlew.bat" } else { "gradlew" });
        let status = Command::new(&gradlew)
            .args(["agentJar", "-q", "--console=plain"])
            .current_dir(&module)
            .status()
            .unwrap_or_else(|e| panic!("cannot run {}: {e}; install JDK 25 or set MANYCRAFT_AGENT_JAR", gradlew.display()));
        assert!(status.success(), "building the companion agent failed; install JDK 25 or set MANYCRAFT_AGENT_JAR");
    }
    assert!(jar.is_file(), "companion agent jar missing at {}", jar.display());
    jar
}

fn no_agent() -> PathBuf {
    let path = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR")).join("no-agent.jar");
    std::fs::write(&path, b"").expect("OUT_DIR is writable");
    path
}

fn newer_sources(module: &Path, jar: &Path, sources: &[&str]) -> bool {
    let Ok(built) = jar.metadata().and_then(|m| m.modified()) else { return true };
    let mut stack: Vec<_> = sources.iter().map(|p| module.join(p)).collect();
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
