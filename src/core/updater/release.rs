//! What GitHub says about releases: which one is newest, and which file fits this install.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const NOTE_CHARS: usize = 160;

/// One file attached to a release.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

/// A published release newer than the running app.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub version: String,
    pub page: String,
    /// The first line of the release notes.
    pub notes: String,
    pub assets: Vec<Asset>,
}

/// How this copy of Manycraft was installed, which decides whether it can replace itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    /// Windows, installed per user by the NSIS installer.
    Installer,
    /// macOS, an app bundle.
    Bundle,
    /// Linux, a single AppImage file.
    AppImage,
    /// A system package (deb); the package manager updates it.
    Package,
    /// A loose binary, a build from source or anything else.
    Unknown,
}

impl Kind {
    /// Whether Manycraft can install an update of this kind by itself.
    pub fn installs_itself(self) -> bool {
        matches!(self, Kind::Installer | Kind::Bundle | Kind::AppImage)
    }
}

#[derive(Deserialize)]
struct Raw {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    assets: Vec<RawAsset>,
}

#[derive(Deserialize)]
struct RawAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

/// The newest release in a GitHub release list that is newer than `current`, skipping drafts, `skipped`, and pre-releases
/// (by flag or by version) unless `prerelease`.
pub fn newest(list: serde_json::Value, current: &str, prerelease: bool, skipped: Option<&str>) -> Result<Option<Release>> {
    let list: Vec<Raw> = match list {
        serde_json::Value::Array(_) => serde_json::from_value(list),
        single => serde_json::from_value(single).map(|r| vec![r]),
    }
    .context("unexpected answer from the release list")?;
    let best = list
        .into_iter()
        .filter(|r| !r.draft && (prerelease || !(r.prerelease || is_prerelease(&r.tag_name))) && is_newer(&r.tag_name, current) && !passed_over(&r.tag_name, skipped))
        .max_by(|a, b| order(&a.tag_name, &b.tag_name));
    Ok(best.map(|r| Release {
        version: r.tag_name.trim().trim_start_matches(['v', 'V']).to_owned(),
        page: r.html_url,
        notes: first_line(r.body.as_deref().unwrap_or_default()),
        assets: r.assets.into_iter().map(|a| Asset { name: a.name, url: a.browser_download_url, size: a.size }).collect(),
    }))
}

/// The first line of release notes that is neither empty nor a heading, without its list marker.
pub fn first_line(body: &str) -> String {
    let line = body.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('#')).unwrap_or_default();
    let line = line.trim_start_matches(['-', '*', '+']).trim();
    match line.char_indices().nth(NOTE_CHARS) {
        Some((cut, _)) => format!("{}…", line[..cut].trim_end()),
        None => line.to_owned(),
    }
}

/// The installer that fits `kind` on `arch` (as `std::env::consts::ARCH` names it), when its checksum and signature are attached too.
pub fn pick<'a>(assets: &'a [Asset], kind: Kind, arch: &str) -> Option<&'a Asset> {
    pick_tagged(assets, kind, arch, super::super::channel::asset_tag())
}

/// [`pick`] for the installers of one channel, whose names end in `tag` before the extension.
pub fn pick_tagged<'a>(assets: &'a [Asset], kind: Kind, arch: &str, tag: &str) -> Option<&'a Asset> {
    let suffix = match (kind, arch) {
        (Kind::Installer, "x86_64") => format!("-windows-x64{tag}.exe"),
        (Kind::Bundle, "x86_64" | "aarch64") => format!("-macos-universal{tag}.dmg"),
        (Kind::AppImage, "x86_64") => format!("-linux-x86_64{tag}.AppImage"),
        _ => return None,
    };
    let found = assets.iter().find(|a| a.name.starts_with("Manycraft") && a.name.ends_with(&suffix))?;
    (sidecar(assets, &found.name, "sha256").is_some() && sidecar(assets, &found.name, "minisig").is_some()).then_some(found)
}

/// The `<name>.<ext>` file next to `name`.
pub fn sidecar<'a>(assets: &'a [Asset], name: &str, ext: &str) -> Option<&'a Asset> {
    assets.iter().find(|a| a.name.strip_prefix(name).and_then(|rest| rest.strip_prefix('.')) == Some(ext))
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Tag {
    Number(u64),
    Text(String),
}

struct Version {
    core: Vec<u64>,
    pre: Option<Vec<Tag>>,
}

fn parse(version: &str) -> Option<Version> {
    let trimmed = version.trim().trim_start_matches(['v', 'V']);
    let trimmed = trimmed.split('+').next().unwrap_or(trimmed);
    let (core, pre) = match trimmed.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (trimmed, None),
    };
    let core = core.split('.').map(|p| p.parse().ok()).collect::<Option<Vec<u64>>>()?;
    let pre = match pre {
        Some(pre) => Some(pre.split('.').map(|p| if p.is_empty() { None } else { Some(p.parse().map_or_else(|_| Tag::Text(p.to_owned()), Tag::Number)) }).collect::<Option<Vec<Tag>>>()?),
        None => None,
    };
    Some(Version { core, pre })
}

fn compare(a: &Version, b: &Version) -> std::cmp::Ordering {
    let len = a.core.len().max(b.core.len());
    let pad = |v: &Version| {
        let mut core = v.core.clone();
        core.resize(len, 0);
        core
    };
    pad(a).cmp(&pad(b)).then_with(|| match (&a.pre, &b.pre) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (Some(_), None) => std::cmp::Ordering::Less,
        (Some(a), Some(b)) => a.cmp(b),
    })
}

fn order(a: &str, b: &str) -> std::cmp::Ordering {
    match (parse(a), parse(b)) {
        (Some(a), Some(b)) => compare(&a, &b),
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

/// Whether `candidate` is a higher version than `current`, a release above its own pre-releases and `1.0.0-beta.2` above `1.0.0-beta.1`; unreadable versions never are.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    matches!((parse(candidate), parse(current)), (Some(a), Some(b)) if compare(&a, &b).is_gt())
}

/// Whether two versions name the same release, `v0.5` and `0.5.0` alike.
pub fn same(a: &str, b: &str) -> bool {
    matches!((parse(a), parse(b)), (Some(a), Some(b)) if compare(&a, &b).is_eq())
}

/// Whether `version` stays unoffered after going back from `skipped`: that version itself, and any pre-release not above it.
pub fn passed_over(version: &str, skipped: Option<&str>) -> bool {
    skipped.is_some_and(|s| same(version, s) || (is_prerelease(version) && !is_newer(version, s)))
}

/// Whether `version` carries a pre-release tag, like `0.10.0-beta.2`.
pub fn is_prerelease(version: &str) -> bool {
    parse(version).is_some_and(|v| v.pre.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn raw(tag: &str, draft: bool, pre: bool) -> serde_json::Value {
        json!({
            "tag_name": tag,
            "html_url": format!("https://github.com/schrumbo/manycraft/releases/tag/{tag}"),
            "draft": draft,
            "prerelease": pre,
            "body": "## Manycraft\n\nFaster grid.\n\n- more",
            "assets": [{ "name": "a", "browser_download_url": "https://example.invalid/a", "size": 3 }],
        })
    }

    fn asset(name: &str) -> Asset {
        Asset { name: name.into(), url: format!("https://example.invalid/{name}"), size: 1 }
    }

    #[test]
    fn versions_compare_numerically() {
        assert!(is_newer("v0.10.0", "0.9.9"));
        assert!(is_newer("1.0", "0.9.9"));
        assert!(is_newer("0.2.1", "0.2"));
        assert!(!is_newer("0.2.0", "0.2"));
        assert!(!is_newer("v0.2.0", "0.2.0"));
        assert!(!is_newer("0.1.9", "0.2.0"));
        assert!(is_newer("0.3.0-beta.1", "0.2.0"));
        assert!(is_newer("0.10.0-beta.2", "0.10.0-beta.1"));
        assert!(is_newer("0.10.0-beta.10", "0.10.0-beta.9"));
        assert!(is_newer("0.10.0", "0.10.0-beta.9"));
        assert!(is_newer("0.10.0-rc.1", "0.10.0-beta.9"));
        assert!(!is_newer("0.10.0-beta.1", "0.10.0"));
        assert!(!is_newer("0.10.0-beta.1", "0.10.0-beta.1"));
        assert!(!is_newer("0.10.0-beta.1", "0.10.0-beta.2"));
        assert!(!is_newer("0.10.0-", "0.9.0"));
        assert!(same("v0.10.0-beta.1", "0.10.0-beta.1"));
        assert!(!same("0.10.0-beta.1", "0.10.0-beta.2"));
        assert!(!same("0.10.0-beta.1", "0.10.0"));
        assert!(same("v0.5", "0.5.0"));
        assert!(!same("0.5.1", "0.5.0"));
    }

    #[test]
    fn pre_releases_order_like_semver() {
        assert!(is_newer("0.10.0-beta.1", "0.9.4"));
        assert!(is_newer("0.10.0-beta.2", "0.10.0-beta.1"));
        assert!(is_newer("0.10.0-beta.10", "0.10.0-beta.9"));
        assert!(is_newer("0.10.0-rc.1", "0.10.0-beta.9"));
        assert!(is_newer("0.10.0", "0.10.0-beta.9"));
        assert!(is_newer("v0.10.0", "0.10.0-beta.2"));
        assert!(!is_newer("0.10.0-beta.2", "0.10.0"));
        assert!(!is_newer("0.10.0-beta.1", "0.10.0-beta.2"));
        assert!(!is_newer("0.10.0-beta.1", "0.10.0-beta.1"));
        assert!(!is_newer("0.9.4", "0.10.0-beta.1"));
        assert!(!is_newer("0.10.0-", "0.9.0"));
        assert!(!is_newer("0.10.0-beta..1", "0.9.0"));
        assert!(same("v0.10.0-beta.1", "0.10.0-beta.1"));
        assert!(same("0.10.0-beta.1+build.7", "0.10.0-beta.1"));
        assert!(!same("0.10.0-beta.1", "0.10.0-beta.2"));
        assert!(!same("0.10.0-beta.2", "0.10.0"));
        assert!(is_prerelease("v0.10.0-beta.2") && !is_prerelease("0.10.0") && !is_prerelease("0.9.4+build") && !is_prerelease("nightly"));
    }

    #[test]
    fn an_opted_in_install_walks_from_the_release_through_the_betas_to_the_final() {
        let beta = |n: u32| raw(&format!("v0.10.0-beta.{n}"), false, true);
        let betas = json!([beta(2), beta(1), raw("v0.9.4", false, false), raw("v0.9.3", false, false)]);
        assert_eq!(newest(betas.clone(), "0.9.4", true, None).unwrap().unwrap().version, "0.10.0-beta.2");
        assert_eq!(newest(betas.clone(), "0.10.0-beta.1", true, None).unwrap().unwrap().version, "0.10.0-beta.2");
        assert_eq!(newest(betas.clone(), "0.10.0-beta.2", true, None).unwrap(), None);
        assert_eq!(newest(json!([beta(1), beta(2)]), "0.9.4", true, None).unwrap().unwrap().version, "0.10.0-beta.2", "list order does not matter");
        let released = json!([raw("v0.10.0", false, false), beta(3), beta(2), raw("v0.9.4", false, false)]);
        assert_eq!(newest(released.clone(), "0.9.4", true, None).unwrap().unwrap().version, "0.10.0");
        assert_eq!(newest(released.clone(), "0.10.0-beta.2", true, None).unwrap().unwrap().version, "0.10.0");
        assert_eq!(newest(released.clone(), "0.10.0-beta.2", false, None).unwrap().unwrap().version, "0.10.0");
        assert_eq!(newest(released, "0.10.0", true, None).unwrap(), None);
    }

    #[test]
    fn without_opting_in_no_pre_release_is_ever_offered() {
        let beta = |n: u32| raw(&format!("v0.10.0-beta.{n}"), false, true);
        let list = json!([beta(2), beta(1), raw("v0.9.4", false, false), raw("v0.9.3", false, false)]);
        assert_eq!(newest(list.clone(), "0.9.3", false, None).unwrap().unwrap().version, "0.9.4");
        assert_eq!(newest(list, "0.9.4", false, None).unwrap(), None);
        let unflagged = json!([raw("v0.10.0-beta.3", false, false), raw("v0.9.4", false, false)]);
        assert_eq!(newest(unflagged.clone(), "0.9.3", false, None).unwrap().unwrap().version, "0.9.4", "a pre-release version counts even without the flag");
        assert_eq!(newest(unflagged, "0.9.4", true, None).unwrap().unwrap().version, "0.10.0-beta.3");
        assert_eq!(newest(json!([beta(2), raw("v0.9.4", false, false)]), "0.9.4", true, Some("0.10.0-beta.2")).unwrap(), None, "a version gone back from stays skipped");
    }

    #[test]
    fn going_back_from_a_beta_passes_over_the_older_betas_too() {
        let beta = |n: u32| raw(&format!("v0.10.0-beta.{n}"), false, true);
        let list = json!([beta(2), beta(1), raw("v0.9.4", false, false)]);
        assert_eq!(newest(list, "0.9.4", true, Some("0.10.0-beta.2")).unwrap(), None, "beta.1 is not offered after leaving beta.2");
        let later = json!([raw("v0.10.0", false, false), beta(3), beta(1)]);
        assert_eq!(newest(later.clone(), "0.9.4", true, Some("0.10.0-beta.2")).unwrap().unwrap().version, "0.10.0");
        assert_eq!(newest(json!([beta(3), beta(1)]), "0.9.4", true, Some("0.10.0-beta.2")).unwrap().unwrap().version, "0.10.0-beta.3", "a newer beta is offered again");
        assert!(passed_over("0.10.0-beta.1", Some("0.10.0-beta.1")) && !passed_over("0.9.5", Some("0.10.0-beta.2")) && !passed_over("0.10.0-beta.1", None));
    }

    #[test]
    fn a_beta_with_standard_names_installs_like_any_update() {
        let names = ["Manycraft-Setup-0.10.0-beta.2-windows-x64.exe", "Manycraft-0.10.0-beta.2-macos-universal.dmg", "Manycraft-0.10.0-beta.2-linux-x86_64.AppImage"];
        let all: Vec<Asset> = names.iter().flat_map(|n| [n.to_string(), format!("{n}.sha256"), format!("{n}.minisig")]).map(|n| asset(&n)).collect();
        assert_eq!(pick_tagged(&all, Kind::Installer, "x86_64", "").map(|a| a.name.as_str()), Some(names[0]));
        assert_eq!(pick_tagged(&all, Kind::Bundle, "aarch64", "").map(|a| a.name.as_str()), Some(names[1]));
        assert_eq!(pick_tagged(&all, Kind::AppImage, "x86_64", "").map(|a| a.name.as_str()), Some(names[2]));
    }

    #[test]
    fn unreadable_versions_are_never_newer() {
        assert!(!is_newer("nightly", "0.2.0"));
        assert!(!is_newer("0.3.x", "0.2.0"));
        assert!(!is_newer("", "0.2.0"));
        assert!(!is_newer("0.3.0", "dev"));
        assert!(!is_newer("ü.ö", "0.2.0"));
        assert!(!same("x", "x"));
    }

    #[test]
    fn the_newest_published_release_wins() {
        let list = json!([raw("v0.6.0", false, false), raw("v0.8.0", true, false), raw("v0.7.0", false, true), raw("v0.4.0", false, false)]);
        let found = newest(list.clone(), "0.5.0", false, None).unwrap().unwrap();
        assert_eq!(found.version, "0.6.0");
        assert_eq!(found.notes, "Faster grid.");
        assert_eq!(found.assets.len(), 1);
        assert_eq!(newest(list, "0.5.0", true, None).unwrap().unwrap().version, "0.7.0");
    }

    #[test]
    fn nothing_newer_is_none_and_skipped_versions_stay_skipped() {
        let list = json!([raw("v0.6.0", false, false), raw("v0.5.0", false, false)]);
        assert_eq!(newest(list.clone(), "0.6.0", true, None).unwrap(), None);
        assert_eq!(newest(list.clone(), "0.5.0", false, Some("0.6")).unwrap(), None);
        assert_eq!(newest(json!(raw("v0.6.0", false, false)), "0.5.0", false, None).unwrap().unwrap().version, "0.6.0");
        assert!(newest(json!({ "message": "Not Found" }), "0.5.0", false, None).is_err());
    }

    #[test]
    fn first_line_skips_headings_and_markers() {
        assert_eq!(first_line("## Manycraft 0.6.0\n\nFirst public preview.\nMore"), "First public preview.");
        assert_eq!(first_line("# x\n\n- Grid of live previews\n"), "Grid of live previews");
        assert_eq!(first_line(""), "");
        let long = "ä".repeat(400);
        let cut = first_line(&long);
        assert!(cut.ends_with('…') && cut.chars().count() == NOTE_CHARS + 1);
    }

    #[test]
    fn each_install_gets_its_own_file() {
        let all: Vec<Asset> = [
            "Manycraft-Setup-0.6.0-windows-x64.exe",
            "Manycraft-Setup-0.6.0-windows-x64.exe.sha256",
            "Manycraft-Setup-0.6.0-windows-x64.exe.minisig",
            "Manycraft-0.6.0-macos-universal.dmg",
            "Manycraft-0.6.0-macos-universal.dmg.sha256",
            "Manycraft-0.6.0-macos-universal.dmg.minisig",
            "Manycraft-0.6.0-linux-x86_64.AppImage",
            "Manycraft-0.6.0-linux-x86_64.AppImage.sha256",
            "Manycraft-0.6.0-linux-x86_64.AppImage.minisig",
            "manycraft_0.6.0-1_amd64.deb",
        ]
        .into_iter()
        .map(asset)
        .collect();
        let name = |kind, arch| pick_tagged(&all, kind, arch, "").map(|a| a.name.as_str());
        assert_eq!(name(Kind::Installer, "x86_64"), Some("Manycraft-Setup-0.6.0-windows-x64.exe"));
        assert_eq!(name(Kind::Installer, "aarch64"), None);
        assert_eq!(name(Kind::Bundle, "aarch64"), Some("Manycraft-0.6.0-macos-universal.dmg"));
        assert_eq!(name(Kind::Bundle, "x86_64"), Some("Manycraft-0.6.0-macos-universal.dmg"));
        assert_eq!(name(Kind::AppImage, "x86_64"), Some("Manycraft-0.6.0-linux-x86_64.AppImage"));
        assert_eq!(name(Kind::AppImage, "aarch64"), None);
        assert_eq!(name(Kind::Package, "x86_64"), None);
        assert_eq!(name(Kind::Unknown, "x86_64"), None);
        assert_eq!(sidecar(&all, "Manycraft-0.6.0-macos-universal.dmg", "minisig").map(|a| a.name.as_str()), Some("Manycraft-0.6.0-macos-universal.dmg.minisig"));
    }

    fn beta_release() -> serde_json::Value {
        let names = [
            "Manycraft-Setup-0.10.0-beta.1-windows-x64-beta.exe",
            "Manycraft-Setup-0.10.0-beta.1-windows-x64-beta.exe.sha256",
            "Manycraft-Setup-0.10.0-beta.1-windows-x64-beta.exe.minisig",
            "Manycraft-0.10.0-beta.1-macos-universal-beta.dmg",
            "Manycraft-0.10.0-beta.1-macos-universal-beta.dmg.sha256",
            "Manycraft-0.10.0-beta.1-macos-universal-beta.dmg.minisig",
            "Manycraft-0.10.0-beta.1-linux-x86_64-beta.AppImage",
            "Manycraft-0.10.0-beta.1-linux-x86_64-beta.AppImage.sha256",
            "Manycraft-0.10.0-beta.1-linux-x86_64-beta.AppImage.minisig",
        ];
        let assets: Vec<_> = names.iter().map(|n| json!({ "name": n, "browser_download_url": format!("https://example.invalid/{n}"), "size": 9 })).collect();
        let mut release = raw("v0.10.0-beta.1", false, true);
        release["assets"] = json!(assets);
        release
    }

    #[test]
    fn a_stable_install_is_never_offered_the_beta() {
        let list = json!([beta_release(), raw("v0.9.3", false, false), raw("v0.9.2", false, false)]);
        assert_eq!(newest(list.clone(), "0.9.2", false, None).unwrap().unwrap().version, "0.9.3");
        assert_eq!(newest(list.clone(), "0.9.3", false, None).unwrap(), None);
        let opted_in = newest(list, "0.9.3", true, None).unwrap().unwrap();
        assert_eq!(opted_in.version, "0.10.0-beta.1");
        for (kind, arch) in [(Kind::Installer, "x86_64"), (Kind::Bundle, "aarch64"), (Kind::Bundle, "x86_64"), (Kind::AppImage, "x86_64")] {
            assert_eq!(pick_tagged(&opted_in.assets, kind, arch, ""), None);
            assert!(pick_tagged(&opted_in.assets, kind, arch, "-beta").is_some());
        }
    }

    #[test]
    fn a_beta_follows_newer_betas_and_the_release_that_ends_them() {
        let two = || {
            let mut r = beta_release();
            r["tag_name"] = json!("v0.10.0-beta.2");
            r
        };
        let list = json!([two(), beta_release(), raw("v0.9.3", false, false)]);
        assert_eq!(newest(list.clone(), "0.10.0-beta.1", true, None).unwrap().unwrap().version, "0.10.0-beta.2");
        assert_eq!(newest(list.clone(), "0.10.0-beta.2", true, None).unwrap(), None);
        assert_eq!(newest(list.clone(), "0.10.0-beta.1", true, Some("0.10.0-beta.2")).unwrap(), None);
        let with_final = json!([raw("v0.10.0", false, false), two()]);
        assert_eq!(newest(with_final, "0.10.0-beta.2", true, None).unwrap().unwrap().version, "0.10.0");
    }

    #[test]
    fn a_beta_never_picks_a_stable_installer_and_the_other_way_round() {
        let stable: Vec<Asset> = ["Manycraft-Setup-0.10.0-windows-x64.exe", "Manycraft-Setup-0.10.0-windows-x64.exe.sha256", "Manycraft-Setup-0.10.0-windows-x64.exe.minisig"].into_iter().map(asset).collect();
        assert_eq!(pick_tagged(&stable, Kind::Installer, "x86_64", "-beta"), None);
        assert!(pick_tagged(&stable, Kind::Installer, "x86_64", "").is_some());
    }

    #[test]
    fn an_unsigned_installer_is_not_picked() {
        let all = vec![asset("Manycraft-Setup-0.6.0-windows-x64.exe"), asset("Manycraft-Setup-0.6.0-windows-x64.exe.sha256")];
        assert_eq!(pick_tagged(&all, Kind::Installer, "x86_64", ""), None);
        assert!(Kind::Installer.installs_itself() && !Kind::Package.installs_itself() && !Kind::Unknown.installs_itself());
    }
}
