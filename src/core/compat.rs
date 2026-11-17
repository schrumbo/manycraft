//! Whether a mod file runs with an instance: the `depends` ranges of its `fabric.mod.json` or `quilt.mod.json`
//! checked against the game, Java and loader versions the instance offers.

use std::cmp::Ordering;
use std::io::{Read, Seek};

use anyhow::Result;
use serde_json::Value;

use super::modmeta;

/// What an instance offers its mods. Unknown parts are not checked.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Target {
    /// The game version as Mojang names it.
    pub minecraft: Option<String>,
    pub java: Option<u32>,
    /// The Fabric Loader version: Fabric's own, or the one Quilt stands in for.
    pub fabricloader: Option<String>,
    pub quilt_loader: Option<String>,
    /// Whether the loader is Quilt, which reads `quilt.mod.json` before `fabric.mod.json`.
    pub quilt: bool,
}

/// The requirement a mod file does not meet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Miss {
    Loader,
    Minecraft,
    Java,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dep {
    Minecraft,
    Java,
    FabricLoader,
    QuiltLoader,
}

impl Dep {
    fn of(id: &str) -> Option<Self> {
        match id {
            "minecraft" => Some(Self::Minecraft),
            "java" => Some(Self::Java),
            "fabricloader" => Some(Self::FabricLoader),
            "quilt_loader" => Some(Self::QuiltLoader),
            _ => None,
        }
    }
}

/// The game, Java and loader ranges a mod file asks for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Needs(Vec<(Dep, Range)>);

impl Needs {
    /// Reads the requirements of a jar: on Quilt from `quilt.mod.json` when it has one, else from `fabric.mod.json`;
    /// none for a jar with neither.
    pub fn read<R: Read + Seek>(jar: R, quilt: bool) -> Result<Self> {
        let mut zip = modmeta::zip(jar)?;
        if quilt && let Some(text) = modmeta::text_in(&mut zip, "quilt.mod.json")? {
            return Ok(Self::quilt(&modmeta::json(&text)?));
        }
        match modmeta::text_in(&mut zip, "fabric.mod.json")? {
            Some(text) => Ok(Self::fabric(&modmeta::json(&text)?)),
            None => Ok(Self::default()),
        }
    }

    fn fabric(root: &Value) -> Self {
        let Some(deps) = root.get("depends").and_then(Value::as_object) else { return Self::default() };
        Self(deps.iter().filter_map(|(id, spec)| Some((Dep::of(&id.to_lowercase())?, fabric_spec(spec)))).collect())
    }

    fn quilt(root: &Value) -> Self {
        let Some(deps) = root.pointer("/quilt_loader/depends").and_then(Value::as_array) else { return Self::default() };
        Self(
            deps.iter()
                .filter_map(|dep| match dep {
                    Value::String(id) => Some((Dep::of(&id.to_lowercase())?, Range::All(Vec::new()))),
                    Value::Object(fields) if fields.get("optional").and_then(Value::as_bool) != Some(true) => {
                        let id = fields.get("id").and_then(Value::as_str)?.to_lowercase();
                        Some((Dep::of(&id)?, fields.get("versions").map_or(Range::All(Vec::new()), quilt_spec)))
                    }
                    _ => None,
                })
                .collect(),
        )
    }

    /// The first requirement `target` does not meet; ranges or versions Manycraft cannot read count as met.
    pub fn check(&self, target: &Target) -> Result<(), Miss> {
        let minecraft = target.minecraft.as_deref().and_then(minecraft);
        let java = target.java.map(|major| Ver { parts: vec![u64::from(major)], pre: None });
        let fabric = target.fabricloader.as_deref().and_then(Ver::parse);
        let quilt = target.quilt_loader.as_deref().and_then(Ver::parse);
        for (dep, range) in &self.0 {
            let (offered, miss) = match dep {
                Dep::Minecraft => (&minecraft, Miss::Minecraft),
                Dep::Java => (&java, Miss::Java),
                Dep::FabricLoader => (&fabric, Miss::Loader),
                Dep::QuiltLoader => (&quilt, Miss::Loader),
            };
            if offered.as_ref().and_then(|v| range.matches(v)) == Some(false) {
                return Err(miss);
            }
        }
        Ok(())
    }
}

fn fabric_spec(spec: &Value) -> Range {
    match spec {
        Value::String(s) => parse(s),
        Value::Array(items) => Range::Any(items.iter().map(|item| item.as_str().map_or(Range::Unknown, parse)).collect()),
        _ => Range::Unknown,
    }
}

fn quilt_spec(spec: &Value) -> Range {
    match spec {
        Value::String(s) => parse(s),
        Value::Array(items) => Range::Any(items.iter().map(quilt_spec).collect()),
        Value::Object(fields) => match (fields.get("any"), fields.get("all")) {
            (Some(Value::Array(items)), None) => Range::Any(items.iter().map(quilt_spec).collect()),
            (None, Some(Value::Array(items))) => Range::All(items.iter().map(quilt_spec).collect()),
            _ => Range::Unknown,
        },
        _ => Range::Unknown,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Eq,
    Ge,
    Gt,
    Le,
    Lt,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Range {
    Any(Vec<Range>),
    All(Vec<Range>),
    Cmp(Op, Ver),
    Unknown,
}

impl Range {
    fn matches(&self, v: &Ver) -> Option<bool> {
        match self {
            Range::Any(items) if items.is_empty() => Some(true),
            Range::Any(items) => {
                let found: Vec<Option<bool>> = items.iter().map(|r| r.matches(v)).collect();
                if found.contains(&Some(true)) {
                    Some(true)
                } else if found.iter().all(|f| *f == Some(false)) {
                    Some(false)
                } else {
                    None
                }
            }
            Range::All(items) => {
                let found: Vec<Option<bool>> = items.iter().map(|r| r.matches(v)).collect();
                if found.contains(&Some(false)) {
                    Some(false)
                } else if found.contains(&None) {
                    None
                } else {
                    Some(true)
                }
            }
            Range::Cmp(op, bound) => {
                let order = v.cmp(bound);
                Some(match op {
                    Op::Eq => order == Ordering::Equal,
                    Op::Ge => order != Ordering::Less,
                    Op::Gt => order == Ordering::Greater,
                    Op::Le => order != Ordering::Greater,
                    Op::Lt => order == Ordering::Less,
                })
            }
            Range::Unknown => None,
        }
    }
}

fn parse(spec: &str) -> Range {
    let mut alternatives: Vec<Range> = spec.split("||").map(|alt| Range::All(alt.split_whitespace().map(term).collect())).collect();
    if alternatives.len() == 1 { alternatives.remove(0) } else { Range::Any(alternatives) }
}

fn term(t: &str) -> Range {
    if t == "*" {
        return Range::All(Vec::new());
    }
    let ops: [(&str, Option<Op>); 7] = [(">=", Some(Op::Ge)), ("<=", Some(Op::Le)), (">", Some(Op::Gt)), ("<", Some(Op::Lt)), ("=", Some(Op::Eq)), ("~", None), ("^", None)];
    let (prefix, op, rest) = ops.iter().find_map(|(p, op)| t.strip_prefix(p).map(|rest| (*p, *op, rest))).unwrap_or(("", Some(Op::Eq), t));
    let core = rest.split(['-', '+']).next().unwrap_or_default();
    let wild = core.split('.').position(|part| matches!(part, "x" | "X" | "*"));
    if let Some(at) = wild {
        let fixed: Option<Vec<u64>> = core.split('.').take(at).map(|p| p.parse().ok()).collect();
        return match (op, fixed) {
            (Some(Op::Eq), Some(fixed)) if fixed.is_empty() => Range::All(Vec::new()),
            (Some(Op::Eq), Some(fixed)) => between(fixed.clone(), next(&fixed, fixed.len() - 1)),
            _ => Range::Unknown,
        };
    }
    let Some(v) = Ver::parse(rest) else { return Range::Unknown };
    match (prefix, op) {
        ("~", _) => {
            let upper = next(&v.parts, 1);
            Range::All(vec![Range::Cmp(Op::Ge, v), Range::Cmp(Op::Lt, lowest(upper))])
        }
        ("^", _) => {
            let upper = next(&v.parts, 0);
            Range::All(vec![Range::Cmp(Op::Ge, v), Range::Cmp(Op::Lt, lowest(upper))])
        }
        (_, Some(op)) => Range::Cmp(op, v),
        _ => Range::Unknown,
    }
}

fn between(from: Vec<u64>, to: Vec<u64>) -> Range {
    Range::All(vec![Range::Cmp(Op::Ge, lowest(from)), Range::Cmp(Op::Lt, lowest(to))])
}

fn next(parts: &[u64], at: usize) -> Vec<u64> {
    let mut out: Vec<u64> = (0..=at).map(|i| parts.get(i).copied().unwrap_or(0)).collect();
    out[at] = out[at].saturating_add(1);
    out
}

fn lowest(parts: Vec<u64>) -> Ver {
    Ver { parts, pre: Some(Vec::new()) }
}

/// A semantic version: numbers, then an optional pre-release that sorts below the plain version; build metadata is ignored.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Ver {
    parts: Vec<u64>,
    pre: Option<Vec<String>>,
}

impl Ver {
    fn parse(s: &str) -> Option<Self> {
        let s = s.split('+').next()?;
        let (core, pre) = match s.split_once('-') {
            Some((core, pre)) => (core, Some(if pre.is_empty() { Vec::new() } else { pre.split('.').map(str::to_owned).collect() })),
            None => (s, None),
        };
        let parts: Option<Vec<u64>> = core.split('.').map(|p| p.parse().ok()).collect();
        Some(Self { parts: parts.filter(|p| !p.is_empty())?, pre })
    }
}

impl Ord for Ver {
    fn cmp(&self, other: &Self) -> Ordering {
        let len = self.parts.len().max(other.parts.len());
        for i in 0..len {
            let order = self.parts.get(i).unwrap_or(&0).cmp(other.parts.get(i).unwrap_or(&0));
            if order != Ordering::Equal {
                return order;
            }
        }
        match (&self.pre, &other.pre) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(a), Some(b)) => {
                for (x, y) in a.iter().zip(b) {
                    let order = match (x.parse::<u64>(), y.parse::<u64>()) {
                        (Ok(x), Ok(y)) => x.cmp(&y),
                        (Ok(_), Err(_)) => Ordering::Less,
                        (Err(_), Ok(_)) => Ordering::Greater,
                        (Err(_), Err(_)) => x.cmp(y),
                    };
                    if order != Ordering::Equal {
                        return order;
                    }
                }
                a.len().cmp(&b.len())
            }
        }
    }
}

impl PartialOrd for Ver {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn minecraft(game: &str) -> Option<Ver> {
    let numeric = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let Some((base, tail)) = game.split_once('-') else {
        return Ver::parse(game);
    };
    let release = Ver::parse(base).filter(|v| v.pre.is_none())?;
    let tagged = [("snapshot-", "alpha"), ("pre-", "pre"), ("rc-", "rc"), ("rc", "rc")];
    let (tag, n) = tagged.iter().find_map(|(prefix, tag)| tail.strip_prefix(prefix).filter(|n| numeric(n)).map(|n| (*tag, n)))?;
    Some(Ver { parts: release.parts, pre: Some(vec![tag.to_owned(), n.to_owned()]) })
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    use super::*;

    fn ok(spec: &str, version: &str) -> Option<bool> {
        parse(spec).matches(&Ver::parse(version).unwrap())
    }

    #[test]
    fn plain_comparisons_follow_semver_order() {
        assert_eq!(ok(">=0.20.0", "0.19.5"), Some(false));
        assert_eq!(ok(">=0.20.0", "0.20.0"), Some(true));
        assert_eq!(ok(">=0.20", "0.20.0"), Some(true));
        assert_eq!(ok(">0.16", "0.16.0"), Some(false));
        assert_eq!(ok("<=0.19.2", "0.19.2"), Some(true));
        assert_eq!(ok("<0.20", "0.20.0-beta.9"), Some(true));
        assert_eq!(ok("=1.2.3", "1.2.3+build.7"), Some(true));
        assert_eq!(ok("1.2.3", "1.2.4"), Some(false));
        assert_eq!(ok(">=0.17.0", "0.17.0-beta.1"), Some(false));
    }

    #[test]
    fn spaces_join_with_and_and_bars_with_or() {
        assert_eq!(ok(">=0.16 <0.20", "0.19.5"), Some(true));
        assert_eq!(ok(">=0.16 <0.20", "0.20.0"), Some(false));
        assert_eq!(ok(">=0.16 <0.20", "0.15.11"), Some(false));
        assert_eq!(ok("<0.10 || >=0.19", "0.19.5"), Some(true));
        assert_eq!(ok("<0.10 || >=0.20", "0.19.5"), Some(false));
    }

    #[test]
    fn star_tilde_caret_and_wildcards() {
        assert_eq!(ok("*", "0.1.0"), Some(true));
        assert_eq!(ok("~1.2.3", "1.2.9"), Some(true));
        assert_eq!(ok("~1.2.3", "1.3.0"), Some(false));
        assert_eq!(ok("~1.2.3", "1.3.0-beta.1"), Some(false));
        assert_eq!(ok("~1.2.3", "1.2.2"), Some(false));
        assert_eq!(ok("^1.2.3", "1.9.0"), Some(true));
        assert_eq!(ok("^1.2.3", "2.0.0"), Some(false));
        assert_eq!(ok("1.2.x", "1.2.7"), Some(true));
        assert_eq!(ok("1.2.x", "1.3.0"), Some(false));
        assert_eq!(ok("1.x", "1.21.4"), Some(true));
        assert_eq!(ok("x", "9.0"), Some(true));
    }

    #[test]
    fn what_cannot_be_read_counts_as_met() {
        assert_eq!(ok(">=banana", "0.1"), None);
        assert_eq!(ok(">=banana || >=2", "1.0"), None);
        assert_eq!(ok(">=banana >=2", "1.0"), Some(false));
    }

    #[test]
    fn minecraft_names_become_fabric_style_semver() {
        let mc = |g: &str| minecraft(g).unwrap();
        assert_eq!(mc("26.4-snapshot-7"), Ver::parse("26.4-alpha.7").unwrap());
        assert_eq!(mc("26.3-pre-1"), Ver::parse("26.3-pre.1").unwrap());
        assert_eq!(mc("26.3-rc-3"), Ver::parse("26.3-rc.3").unwrap());
        assert_eq!(mc("1.21.4"), Ver::parse("1.21.4").unwrap());
        assert!(minecraft("25w14a").is_none() && minecraft("1.21-pre1").is_none());
        let snapshot = mc("26.4-snapshot-7");
        assert_eq!(parse("~26.4-").matches(&snapshot), Some(true));
        assert_eq!(parse("~26.3").matches(&snapshot), Some(false));
        assert_eq!(parse(">=26.4-alpha.8").matches(&snapshot), Some(false));
        assert_eq!(parse("~26.4-").matches(&mc("26.4")), Some(true));
        assert_eq!(parse("~26.4-").matches(&mc("26.5-snapshot-1")), Some(false));
    }

    fn jar(entries: &[(&str, &str)]) -> Cursor<Vec<u8>> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, text) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(text.as_bytes()).unwrap();
        }
        Cursor::new(zip.finish().unwrap().into_inner())
    }

    const FABRIC_API: &str = r#"{"schemaVersion":1,"id":"fabric-api","depends":{"fabricloader":">=0.20.0","java":">=25","minecraft":"~26.4-"}}"#;

    fn quilt_on(game: &str, provides: &str) -> Target {
        Target { minecraft: Some(game.into()), java: Some(25), fabricloader: Some(provides.into()), quilt_loader: Some("0.30.2".into()), quilt: true }
    }

    #[test]
    fn fabric_api_for_26_4_does_not_run_on_quilt_that_provides_0_19() {
        let needs = Needs::read(jar(&[("fabric.mod.json", FABRIC_API)]), true).unwrap();
        assert_eq!(needs.check(&quilt_on("26.4-snapshot-7", "0.19.5")), Err(Miss::Loader));
        assert_eq!(needs.check(&quilt_on("26.4-snapshot-7", "0.20.1")), Ok(()));
        assert_eq!(needs.check(&quilt_on("26.3", "0.20.1")), Err(Miss::Minecraft));
        let old_java = Target { java: Some(21), ..quilt_on("26.4-snapshot-7", "0.20.1") };
        assert_eq!(needs.check(&old_java), Err(Miss::Java));
        assert_eq!(needs.check(&Target::default()), Ok(()));
    }

    #[test]
    fn quilt_reads_quilt_mod_json_first_and_fabric_only_fabric_mod_json() {
        let quilt = r#"{"schema_version":1,"quilt_loader":{"id":"both","version":"1","depends":[
            "minecraft",
            {"id":"quilt_loader","versions":">=0.31.0"},
            {"id":"java","versions":{"any":[">=99"]},"optional":true},
            {"id":"fabricloader","versions":[">=0.10 <0.11", ">=0.19"]}
        ]}}"#;
        let both = || jar(&[("fabric.mod.json", FABRIC_API), ("quilt.mod.json", quilt)]);
        let on_quilt = Needs::read(both(), true).unwrap();
        assert_eq!(on_quilt.check(&quilt_on("26.3", "0.19.5")), Err(Miss::Loader));
        let newer = Target { quilt_loader: Some("0.31.0".into()), ..quilt_on("26.3", "0.19.5") };
        assert_eq!(on_quilt.check(&newer), Ok(()));
        let fabric = Target { minecraft: Some("26.4-snapshot-7".into()), fabricloader: Some("0.19.5".into()), ..Target::default() };
        assert_eq!(Needs::read(both(), false).unwrap().check(&fabric), Err(Miss::Loader));
    }

    #[test]
    fn a_jar_without_metadata_needs_nothing_and_a_broken_one_is_an_error() {
        assert_eq!(Needs::read(jar(&[("a.txt", "x")]), true).unwrap(), Needs::default());
        assert!(Needs::read(Cursor::new(b"not a jar".to_vec()), false).is_err());
    }
}
