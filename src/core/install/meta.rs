use std::collections::HashMap;

use serde::{Deserialize, Serialize};

const LIBRARIES_URL: &str = "https://libraries.minecraft.net/";

/// Operating system as Mojang names it in rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Windows,
    Osx,
    Linux,
}

impl Os {
    pub fn current() -> Self {
        if cfg!(windows) {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::Osx
        } else {
            Os::Linux
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Os::Windows => "windows",
            Os::Osx => "osx",
            Os::Linux => "linux",
        }
    }

    fn natives_name(self) -> &'static str {
        match self {
            Os::Osx => "macos",
            other => other.name(),
        }
    }
}

/// CPU architecture as Mojang names it in rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arch {
    X86,
    X64,
    Arm64,
}

impl Arch {
    pub fn current() -> Self {
        if cfg!(target_arch = "aarch64") {
            Arch::Arm64
        } else if cfg!(target_arch = "x86") {
            Arch::X86
        } else {
            Arch::X64
        }
    }

    fn name(self) -> &'static str {
        match self {
            Arch::X86 => "x86",
            Arch::X64 => "x86_64",
            Arch::Arm64 => "arm64",
        }
    }

    fn bits(self) -> &'static str {
        match self {
            Arch::X86 => "32",
            _ => "64",
        }
    }

    fn natives_suffix(self) -> &'static str {
        match self {
            Arch::X86 => "-x86",
            Arch::X64 => "",
            Arch::Arm64 => "-arm64",
        }
    }
}

/// Platform and enabled launcher features that rules are evaluated against.
#[derive(Clone, Debug)]
pub struct Env {
    pub os: Os,
    pub arch: Arch,
    pub features: Vec<&'static str>,
}

impl Env {
    pub fn current(features: Vec<&'static str>) -> Self {
        Env { os: Os::current(), arch: Arch::current(), features }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Allow,
    Disallow,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct OsRule {
    pub name: Option<String>,
    pub arch: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Rule {
    pub action: Action,
    #[serde(default)]
    pub os: Option<OsRule>,
    #[serde(default)]
    pub features: HashMap<String, bool>,
}

impl Rule {
    fn matches(&self, env: &Env) -> bool {
        let os_ok = self.os.as_ref().is_none_or(|os| {
            os.name.as_deref().is_none_or(|n| n == env.os.name()) && os.arch.as_deref().is_none_or(|a| a == env.arch.name())
        });
        let features_ok = self.features.iter().all(|(name, wanted)| env.features.contains(&name.as_str()) == *wanted);
        os_ok && features_ok
    }
}

/// No rules allow; otherwise the last matching rule decides and no match disallows.
pub fn allowed(rules: &[Rule], env: &Env) -> bool {
    rules.is_empty() || rules.iter().rfind(|r| r.matches(env)).is_some_and(|r| r.action == Action::Allow)
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Artifact {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub sha1: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    pub url: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LibraryDownloads {
    #[serde(default)]
    pub artifact: Option<Artifact>,
    #[serde(default)]
    pub classifiers: HashMap<String, Artifact>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Extract {
    #[serde(default)]
    pub exclude: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Library {
    pub name: String,
    #[serde(default)]
    pub downloads: Option<LibraryDownloads>,
    #[serde(default)]
    pub rules: Vec<Rule>,
    #[serde(default)]
    pub natives: HashMap<String, String>,
    #[serde(default)]
    pub extract: Option<Extract>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub sha1: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
}

/// A file below the libraries folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dep {
    pub path: String,
    pub url: String,
    pub sha1: Option<String>,
    pub size: Option<u64>,
}

impl Dep {
    fn new(artifact: &Artifact, name: &str) -> Option<Dep> {
        let path = artifact.path.clone().or_else(|| maven_path(name))?;
        Some(Dep { path, url: artifact.url.clone(), sha1: artifact.sha1.clone(), size: artifact.size })
    }
}

/// A jar whose native libraries are extracted next to the game.
#[derive(Clone, Debug)]
pub struct NativeJar {
    pub dep: Dep,
    pub exclude: Vec<String>,
}

/// Libraries that apply on a platform.
#[derive(Clone, Debug, Default)]
pub struct Resolved {
    pub classpath: Vec<Dep>,
    pub natives: Vec<NativeJar>,
}

impl Library {
    fn classifier(&self) -> Option<&str> {
        self.name.split(':').nth(3).map(|c| c.split_once('@').map_or(c, |(c, _)| c))
    }

    fn key(&self) -> String {
        let mut parts = self.name.split(':');
        let group = parts.next().unwrap_or_default();
        let artifact = parts.next().unwrap_or_default();
        match self.classifier() {
            Some(c) => format!("{group}:{artifact}:{c}"),
            None => format!("{group}:{artifact}"),
        }
    }

    fn explicit_artifact(&self) -> Option<&Artifact> {
        self.downloads.as_ref().and_then(|d| d.artifact.as_ref())
    }

    fn artifact(&self) -> Option<Artifact> {
        if let Some(artifact) = self.explicit_artifact() {
            return Some(artifact.clone());
        }
        let path = maven_path(&self.name)?;
        let base = self.url.as_deref().unwrap_or(LIBRARIES_URL).trim_end_matches('/');
        Some(Artifact { url: format!("{base}/{path}"), path: Some(path), sha1: self.sha1.clone(), size: self.size })
    }

    fn exclude(&self) -> Vec<String> {
        self.extract.as_ref().map(|e| e.exclude.clone()).unwrap_or_default()
    }

    fn is_natives_for(&self, env: &Env) -> bool {
        let expected = format!("natives-{}{}", env.os.natives_name(), env.arch.natives_suffix());
        self.classifier() == Some(expected.as_str())
    }
}

fn push_unique(deps: &mut Vec<Dep>, dep: Dep) {
    if !deps.iter().any(|d| d.path == dep.path) {
        deps.push(dep);
    }
}

/// Splits the libraries that apply on `env` into classpath entries and jars to extract natives from.
pub fn resolve(libraries: &[Library], env: &Env) -> Resolved {
    let mut out = Resolved::default();
    for lib in libraries.iter().filter(|l| allowed(&l.rules, env)) {
        if lib.natives.is_empty() {
            if let Some(dep) = lib.artifact().and_then(|a| Dep::new(&a, &lib.name)) {
                if lib.is_natives_for(env) {
                    out.natives.push(NativeJar { dep: dep.clone(), exclude: lib.exclude() });
                }
                push_unique(&mut out.classpath, dep);
            }
            continue;
        }
        if let Some(dep) = lib.explicit_artifact().and_then(|a| Dep::new(a, &lib.name)) {
            push_unique(&mut out.classpath, dep);
        }
        let classifier = lib.natives.get(env.os.name()).map(|c| c.replace("${arch}", env.arch.bits()));
        let native = classifier.and_then(|c| lib.downloads.as_ref()?.classifiers.get(&c)).and_then(|a| Dep::new(a, &lib.name));
        if let Some(dep) = native {
            out.natives.push(NativeJar { dep, exclude: lib.exclude() });
        }
    }
    out
}

/// Repository path of a Maven coordinate `group:artifact:version[:classifier][@ext]`.
pub fn maven_path(coords: &str) -> Option<String> {
    let (coords, ext) = coords.split_once('@').unwrap_or((coords, "jar"));
    let mut parts = coords.split(':');
    let group = parts.next()?.replace('.', "/");
    let artifact = parts.next()?;
    let version = parts.next()?;
    let file = match parts.next() {
        Some(classifier) => format!("{artifact}-{version}-{classifier}.{ext}"),
        None => format!("{artifact}-{version}.{ext}"),
    };
    Some(format!("{group}/{artifact}/{version}/{file}"))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Value {
    One(String),
    Many(Vec<String>),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Argument {
    Plain(String),
    Ruled { rules: Vec<Rule>, value: Value },
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Arguments {
    #[serde(default)]
    pub jvm: Vec<Argument>,
    #[serde(default)]
    pub game: Vec<Argument>,
}

/// Arguments whose rules allow them on `env`, flattened.
pub fn expand(arguments: &[Argument], env: &Env) -> Vec<String> {
    let mut out = Vec::new();
    for argument in arguments {
        match argument {
            Argument::Plain(s) => out.push(s.clone()),
            Argument::Ruled { rules, value } if allowed(rules, env) => match value {
                Value::One(s) => out.push(s.clone()),
                Value::Many(list) => out.extend(list.iter().cloned()),
            },
            Argument::Ruled { .. } => {}
        }
    }
    out
}

/// Replaces `${name}` with its value; unknown names stay as written.
pub fn substitute(template: &str, vars: &HashMap<&str, String>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some((head, tail)) = rest.split_once("${") {
        out.push_str(head);
        let Some((name, after)) = tail.split_once('}') else {
            out.push_str("${");
            rest = tail;
            break;
        };
        match vars.get(name) {
            Some(value) => out.push_str(value),
            None => {
                out.push_str("${");
                out.push_str(name);
                out.push('}');
            }
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetIndexRef {
    pub id: String,
    #[serde(default)]
    pub sha1: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    pub url: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct VersionDownloads {
    #[serde(default)]
    pub client: Option<Artifact>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JavaVersion {
    pub component: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LogFile {
    pub id: String,
    #[serde(default)]
    pub sha1: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    pub url: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LogClient {
    pub argument: String,
    pub file: LogFile,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Logging {
    #[serde(default)]
    pub client: Option<LogClient>,
}

/// A version JSON, from Mojang or from Fabric; merged ones carry both.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionJson {
    pub id: String,
    #[serde(default)]
    pub inherits_from: Option<String>,
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub main_class: Option<String>,
    #[serde(default)]
    pub arguments: Option<Arguments>,
    #[serde(default)]
    pub minecraft_arguments: Option<String>,
    #[serde(default)]
    pub libraries: Vec<Library>,
    #[serde(default)]
    pub asset_index: Option<AssetIndexRef>,
    #[serde(default)]
    pub downloads: Option<VersionDownloads>,
    #[serde(default)]
    pub java_version: Option<JavaVersion>,
    #[serde(default)]
    pub logging: Option<Logging>,
}

/// Lays `over` (the Fabric profile) onto `base` (vanilla): Fabric's main class and id win,
/// libraries with the same group, artifact and classifier are replaced, arguments are appended.
pub fn merge(base: VersionJson, over: VersionJson) -> VersionJson {
    let replaced: Vec<String> = over.libraries.iter().map(Library::key).collect();
    let mut libraries: Vec<Library> = base.libraries.into_iter().filter(|l| !replaced.contains(&l.key())).collect();
    libraries.extend(over.libraries);
    let arguments = match (base.arguments, over.arguments) {
        (Some(mut a), Some(b)) => {
            a.jvm.extend(b.jvm);
            a.game.extend(b.game);
            Some(a)
        }
        (a, b) => a.or(b),
    };
    VersionJson {
        id: over.id,
        inherits_from: None,
        kind: over.kind.or(base.kind),
        main_class: over.main_class.or(base.main_class),
        arguments,
        minecraft_arguments: over.minecraft_arguments.or(base.minecraft_arguments),
        libraries,
        asset_index: over.asset_index.or(base.asset_index),
        downloads: over.downloads.or(base.downloads),
        java_version: over.java_version.or(base.java_version),
        logging: over.logging.or(base.logging),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VANILLA: &str = include_str!("../testdata/install/vanilla.json");
    const FABRIC: &str = include_str!("../testdata/install/fabric.json");

    fn env(os: Os, arch: Arch) -> Env {
        Env { os, arch, features: Vec::new() }
    }

    fn vanilla() -> VersionJson {
        serde_json::from_str(VANILLA).unwrap()
    }

    fn fabric() -> VersionJson {
        serde_json::from_str(FABRIC).unwrap()
    }

    fn paths(deps: &[Dep]) -> Vec<&str> {
        deps.iter().map(|d| d.path.as_str()).collect()
    }

    #[test]
    fn no_rules_allow() {
        assert!(allowed(&[], &env(Os::Linux, Arch::X64)));
    }

    #[test]
    fn os_rule_matches_name_and_arch() {
        let rules: Vec<Rule> = serde_json::from_str(r#"[{"action":"allow","os":{"name":"osx"}},{"action":"disallow","os":{"arch":"x86"}}]"#).unwrap();
        assert!(allowed(&rules, &env(Os::Osx, Arch::X64)));
        assert!(!allowed(&rules, &env(Os::Linux, Arch::X64)));
        assert!(!allowed(&rules, &env(Os::Osx, Arch::X86)));
    }

    #[test]
    fn last_matching_rule_wins() {
        let rules: Vec<Rule> = serde_json::from_str(r#"[{"action":"allow"},{"action":"disallow","os":{"name":"windows"}}]"#).unwrap();
        assert!(allowed(&rules, &env(Os::Linux, Arch::X64)));
        assert!(!allowed(&rules, &env(Os::Windows, Arch::X64)));
    }

    #[test]
    fn feature_rule_needs_the_feature() {
        let rules: Vec<Rule> = serde_json::from_str(r#"[{"action":"allow","features":{"is_quick_play_multiplayer":true}}]"#).unwrap();
        let mut e = env(Os::Linux, Arch::X64);
        assert!(!allowed(&rules, &e));
        e.features.push("is_quick_play_multiplayer");
        assert!(allowed(&rules, &e));
    }

    #[test]
    fn placeholders_are_substituted() {
        let vars = HashMap::from([("name", "Dev".to_owned()), ("dir", "/g".to_owned())]);
        assert_eq!(substitute("--user ${name} --dir=${dir}/x", &vars), "--user Dev --dir=/g/x");
        assert_eq!(substitute("${nope} ${name", &vars), "${nope} ${name");
        assert_eq!(substitute("plain", &vars), "plain");
    }

    #[test]
    fn substituted_values_are_not_expanded_again() {
        let vars = HashMap::from([("a", "${b}".to_owned()), ("b", "x".to_owned())]);
        assert_eq!(substitute("${a}", &vars), "${b}");
    }

    #[test]
    fn maven_coordinates_become_paths() {
        assert_eq!(maven_path("net.fabricmc:fabric-loader:0.20.1").unwrap(), "net/fabricmc/fabric-loader/0.20.1/fabric-loader-0.20.1.jar");
        assert_eq!(
            maven_path("org.lwjgl:lwjgl:3.4.3:natives-windows").unwrap(),
            "org/lwjgl/lwjgl/3.4.3/lwjgl-3.4.3-natives-windows.jar"
        );
        assert_eq!(maven_path("a.b:c:1@zip").unwrap(), "a/b/c/1/c-1.zip");
        assert!(maven_path("broken").is_none());
    }

    #[test]
    fn libraries_are_filtered_by_os() {
        let v = vanilla();
        let linux = resolve(&v.libraries, &env(Os::Linux, Arch::X64));
        let windows = resolve(&v.libraries, &env(Os::Windows, Arch::X64));
        assert!(paths(&linux.classpath).contains(&"org/lwjgl/lwjgl/3.4.3/lwjgl-3.4.3-natives-linux.jar"));
        assert!(!paths(&linux.classpath).contains(&"org/lwjgl/lwjgl/3.4.3/lwjgl-3.4.3-natives-windows.jar"));
        assert!(paths(&windows.classpath).contains(&"org/lwjgl/lwjgl/3.4.3/lwjgl-3.4.3-natives-windows.jar"));
        assert!(!paths(&windows.classpath).contains(&"ca/weblite/java-objc-bridge/1.1/java-objc-bridge-1.1.jar"));
    }

    #[test]
    fn modern_natives_follow_the_architecture() {
        let v = vanilla();
        let lwjgl = |e: &Env| -> Vec<String> { resolve(&v.libraries, e).natives.into_iter().map(|n| n.dep.path).filter(|p| p.contains("lwjgl")).collect() };
        assert_eq!(lwjgl(&env(Os::Windows, Arch::Arm64)), ["org/lwjgl/lwjgl/3.4.3/lwjgl-3.4.3-natives-windows-arm64.jar"]);
        assert_eq!(lwjgl(&env(Os::Windows, Arch::X64)), ["org/lwjgl/lwjgl/3.4.3/lwjgl-3.4.3-natives-windows.jar"]);
        assert_eq!(lwjgl(&env(Os::Osx, Arch::X64)), ["org/lwjgl/lwjgl/3.4.3/lwjgl-3.4.3-natives-macos.jar"]);
    }

    #[test]
    fn legacy_native_classifiers_are_selected_per_os() {
        let v = vanilla();
        let win = resolve(&v.libraries, &env(Os::Windows, Arch::X64));
        let jar = win.natives.iter().find(|n| n.dep.path.contains("legacy-platform")).unwrap();
        assert_eq!(jar.dep.path, "legacy/legacy-platform/1.0/legacy-platform-1.0-natives-windows-64.jar");
        assert_eq!(jar.exclude, ["META-INF/"]);
        assert!(!paths(&win.classpath).iter().any(|p| p.contains("natives-windows-64")));
        let mac = resolve(&v.libraries, &env(Os::Osx, Arch::Arm64));
        assert!(mac.natives.iter().any(|n| n.dep.path.ends_with("legacy-platform-1.0-natives-osx.jar")));
        assert!(paths(&mac.classpath).contains(&"legacy/legacy-platform/1.0/legacy-platform-1.0.jar"));
    }

    #[test]
    fn duplicate_classpath_entries_are_dropped() {
        let lib: Library = serde_json::from_str(r#"{"name":"a:b:1"}"#).unwrap();
        let resolved = resolve(&[lib.clone(), lib], &env(Os::Linux, Arch::X64));
        assert_eq!(resolved.classpath.len(), 1);
        assert_eq!(resolved.classpath[0].url, "https://libraries.minecraft.net/a/b/1/b-1.jar");
    }

    #[test]
    fn fabric_libraries_get_maven_urls_and_paths() {
        let resolved = resolve(&fabric().libraries, &env(Os::Linux, Arch::X64));
        let loader = resolved.classpath.iter().find(|d| d.path.contains("fabric-loader")).unwrap();
        assert_eq!(loader.url, "https://maven.fabricmc.net/net/fabricmc/fabric-loader/0.20.1/fabric-loader-0.20.1.jar");
        assert_eq!(loader.sha1.as_deref(), Some("abc"));
    }

    #[test]
    fn merge_lets_fabric_win_per_artifact() {
        let merged = merge(vanilla(), fabric());
        assert_eq!(merged.id, "fabric-loader-0.20.1-26.3");
        assert_eq!(merged.main_class.as_deref(), Some("net.fabricmc.loader.impl.launch.knot.KnotClient"));
        assert!(merged.inherits_from.is_none());
        let asm: Vec<&str> = merged.libraries.iter().filter(|l| l.name.starts_with("org.ow2.asm:asm:")).map(|l| l.name.as_str()).collect();
        assert_eq!(asm, ["org.ow2.asm:asm:9.10.1"]);
        assert!(merged.libraries.iter().any(|l| l.name == "org.lwjgl:lwjgl:3.4.3:natives-linux"));
        assert_eq!(merged.libraries.last().unwrap().name, "net.fabricmc:fabric-loader:0.20.1");
        assert_eq!(merged.java_version.unwrap().component, "java-runtime-epsilon");
        let args = merged.arguments.unwrap();
        assert!(args.jvm.len() > 2);
        assert!(expand(&args.jvm, &env(Os::Linux, Arch::X64)).contains(&"-DFabricMarker=1".to_owned()));
    }

    #[test]
    fn merged_json_survives_a_round_trip() {
        let merged = merge(vanilla(), fabric());
        let back: VersionJson = serde_json::from_slice(&serde_json::to_vec(&merged).unwrap()).unwrap();
        assert_eq!(back.libraries.len(), merged.libraries.len());
        assert_eq!(back.main_class, merged.main_class);
    }
}
