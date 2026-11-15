use std::path::{Path, PathBuf};
use std::process::Command;

const COMPANION_SOURCES: [&str; 8] = ["common/src", "common/build.gradle", "fabric/src", "fabric/build.gradle", "build.gradle", "gradle/loader.gradle", "gradle.properties", "settings.gradle"];
const LOADERS: [(&str, &str); 2] = [("neoforge", "MANYCRAFT_COMPANION_NEOFORGE_JAR"), ("forge", "MANYCRAFT_COMPANION_FORGE_JAR")];

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
    for (loader, var) in LOADERS {
        println!("cargo:rerun-if-changed=mod/{loader}/src");
        println!("cargo:rerun-if-changed=mod/{loader}/build.gradle");
        println!("cargo:rerun-if-env-changed={var}");
        let jar = match std::env::var_os(var) {
            Some(path) if path.is_empty() => no_companion(),
            Some(path) => PathBuf::from(path),
            None => build_loader(loader).unwrap_or_else(|why| {
                println!("cargo:warning=building without the {loader} companion mod: {why}");
                no_companion()
            }),
        };
        println!("cargo:rustc-env={var}={}", jar.display());
    }
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
    PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR")).join("mod")
}

fn version(module: &Path) -> String {
    std::fs::read_to_string(module.join("gradle.properties"))
        .expect("mod/gradle.properties is readable")
        .lines()
        .find_map(|l| l.strip_prefix("version=").map(str::trim).map(str::to_owned))
        .expect("mod/gradle.properties has a version")
}

fn gradlew(module: &Path) -> PathBuf {
    module.join(if cfg!(windows) { "gradlew.bat" } else { "gradlew" })
}

fn build_companion() -> PathBuf {
    let module = module();
    let jar = module.join("fabric").join("build").join("libs").join(format!("manycraft-companion-{}.jar", version(&module)));
    if !jar.is_file() || newer_sources(&module, &jar, &COMPANION_SOURCES) {
        let gradlew = gradlew(&module);
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

fn build_loader(loader: &str) -> Result<PathBuf, String> {
    let module = module();
    let jar = module.join(loader).join("build").join("libs").join(format!("manycraft-companion-{loader}-{}.jar", version(&module)));
    let src = format!("{loader}/src");
    let gradle = format!("{loader}/build.gradle");
    let mut sources = COMPANION_SOURCES.to_vec();
    sources.retain(|s| !s.starts_with("fabric/"));
    sources.extend([src.as_str(), gradle.as_str()]);
    if !jar.is_file() || newer_sources(&module, &jar, &sources) {
        let status = Command::new(gradlew(&module))
            .args([&format!(":{loader}:build"), "-q", "--console=plain", "-x", "test"])
            .current_dir(&module)
            .status()
            .map_err(|e| format!("cannot run Gradle: {e}"))?;
        if !status.success() {
            return Err(format!("Gradle failed with {status}; set {loader} jar paths through the environment to use prebuilt ones"));
        }
    }
    if jar.is_file() { Ok(jar) } else { Err(format!("{} is missing", jar.display())) }
}

fn no_companion() -> PathBuf {
    let path = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR")).join("no-companion.jar");
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
