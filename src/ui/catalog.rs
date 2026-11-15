//! Blocking catalog work for the content browser, run off the UI thread: searches, project pages, pack files.
//! A driven instance gets canned answers and writes canned files instead of touching the network.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};

use crate::core::batch::ModRef;
use crate::core::content::{self, Catalog, Installed, Modrinth};
use crate::core::modrinth::{Dependency, Details, File, Filter, Hashes, Hit, Kind, Page, Search, Shot, Sort, Support, Update, Version};
use crate::core::{drive, paths};

use super::remote::{CANNED_DELAY, canned_hits};

const CANNED_SEARCH_DELAY: Duration = Duration::from_millis(300);
const CANNED_MANY_DELAY: Duration = Duration::from_millis(40);
const CANNED_PAGE: usize = 6;
const CANNED_MANY_PAGE: usize = 50;
const CANNED_MANY: usize = 1000;
const CANNED_SHOT: (u32, u32) = (960, 540);

fn canned() -> bool {
    drive::on() && !drive::live()
}

/// One page of projects for `search` from result `offset` on.
pub fn search(search: &Search, offset: usize) -> Result<Page> {
    if drive::fenced("modrinth search", &search.query) && !drive::live() {
        return canned_search(search, offset);
    }
    Modrinth.search(search, offset)
}

/// A project's page, with the time it was fetched when it comes from the disk.
pub fn details(project_id: &str) -> Result<(Details, Option<SystemTime>)> {
    if drive::fenced("modrinth project", project_id) && !drive::live() {
        std::thread::sleep(CANNED_DELAY);
        let hit = canned_hit(project_id).with_context(|| format!("{project_id} is not on Modrinth"))?;
        return Ok((canned_details(&hit), None));
    }
    Modrinth.details(project_id)
}

/// Every version of a project, with the time they were fetched when they come from the disk.
pub fn versions(project_id: &str, kind: Kind, game_version: &str) -> Result<(Vec<Version>, Option<SystemTime>)> {
    if drive::fenced("modrinth versions", project_id) && !drive::live() {
        std::thread::sleep(CANNED_DELAY);
        return Ok((canned_versions(project_id, kind, game_version), None));
    }
    Modrinth.versions(project_id)
}

/// Titles of projects by id.
pub fn titles(ids: &[String]) -> Result<HashMap<String, String>> {
    if canned() {
        return Ok(every_canned_hit().into_iter().filter(|h| ids.contains(&h.project_id)).map(|h| (h.project_id, h.title)).collect());
    }
    Modrinth.titles(ids)
}

/// The pack files in `dir` the catalog knows.
pub fn installed(dir: &Path) -> Result<Vec<Installed>> {
    if canned() {
        return Ok(canned_installed(dir));
    }
    content::installed(&Modrinth, dir)
}

/// Newer files for installed packs.
pub fn updates(filter: &Filter, current: &[ModRef]) -> Result<Vec<Update>> {
    if canned() {
        return Ok(current.iter().filter(|m| m.file_name.contains("-1.0")).map(|m| Update { current: m.clone(), latest: canned_file(&m.project_id, &m.title) }).collect());
    }
    Modrinth.updates(filter, current)
}

/// Installs the newest file of a project `filter` takes into `dir`, or the newer file of `update`, replacing the
/// old one and keeping it selected. Returns what is installed now.
pub fn install(instance_dir: &Path, dir: &Path, project_id: &str, title: &str, filter: &Filter, update: Option<&Update>) -> Result<Installed> {
    let found = match update {
        Some(u) => u.latest.clone(),
        None if canned() => canned_file(project_id, title),
        None => Modrinth.file_for(project_id, title, filter)?,
    };
    let replacing = update.map(|u| u.current.file_name.as_str());
    let file = if drive::fenced("pack install", &found.file_name) {
        std::thread::sleep(CANNED_DELAY);
        content::place(&found, dir, replacing, |_, path| Ok(std::fs::write(path, b"canned")?))?
    } else {
        content::place(&found, dir, replacing, content::download)?
    };
    if let Some(old) = replacing {
        content::rename_selected(instance_dir, filter.kind, old, &file)?;
    }
    Ok(Installed { file, found })
}

/// Moves a pack to the instance's trash and returns the entry to take it back.
pub fn remove(instance_dir: &Path, dir: &Path, file: &str) -> Result<String> {
    content::remove(instance_dir, dir, file)
}

/// Takes a removed pack back out of the trash.
pub fn put_back(instance_dir: &Path, entry: &str) -> Result<PathBuf> {
    content::put_back(instance_dir, entry)
}

/// Downloads the newest `.mrpack` of a modpack `filter` takes and returns its path.
pub fn fetch_modpack(project_id: &str, title: &str, filter: &Filter) -> Result<PathBuf> {
    if drive::fenced("modpack fetch", project_id) {
        std::thread::sleep(CANNED_DELAY);
        return canned_modpack(project_id, title);
    }
    content::fetch_modpack(&Modrinth.file_for(project_id, title, filter)?)
}

/// A gallery picture as a local file.
pub fn shot(url: &str) -> Result<PathBuf> {
    if canned() {
        return canned_shot(url);
    }
    content::shot_file(url)
}

fn canned_search(search: &Search, offset: usize) -> Result<Page> {
    let q = search.query.trim().to_lowercase();
    let many = q == "viele" || q == "many";
    std::thread::sleep(if many { CANNED_MANY_DELAY } else { CANNED_SEARCH_DELAY });
    if q.contains("offline") {
        bail!("failed to lookup address");
    }
    let game = search.filter.game_version.clone().unwrap_or_default();
    let mut hits: Vec<Hit> = if many { many_hits(search.filter.kind, &game) } else { kind_hits(search.filter.kind, &game) };
    if !many {
        hits.retain(|h| h.title.to_lowercase().contains(&q) || h.description.to_lowercase().contains(&q));
    }
    if let Some(category) = &search.category {
        hits.retain(|h| h.categories.contains(category));
    }
    if !search.all {
        hits.retain(|h| h.fits(&search.filter) == Support::Fits);
    }
    match search.sort {
        Sort::Downloads => hits.sort_by_key(|h| std::cmp::Reverse(h.downloads)),
        Sort::Relevance => hits.sort_by_key(|h| !h.title.to_lowercase().starts_with(&q)),
        Sort::Updated => hits.sort_by(|a, b| a.title.cmp(&b.title)),
        Sort::Newest => hits.sort_by(|a, b| b.title.cmp(&a.title)),
    }
    let total = hits.len();
    let page = if many { CANNED_MANY_PAGE } else { CANNED_PAGE };
    Ok(Page { hits: hits.into_iter().skip(offset).take(page).collect(), total, saved: None })
}

fn hit(slug: &str, title: &str, author: &str, description: &str, downloads: u64, versions: &[&str], categories: &[&str]) -> Hit {
    Hit {
        title: title.into(),
        slug: slug.into(),
        project_id: format!("id-{slug}"),
        description: description.into(),
        downloads,
        icon_url: None,
        author: author.into(),
        versions: versions.iter().map(|v| (*v).to_owned()).collect(),
        categories: categories.iter().map(|c| (*c).to_owned()).collect(),
    }
}

fn kind_hits(kind: Kind, game: &str) -> Vec<Hit> {
    let old = "1.16.5";
    match kind {
        Kind::Mod => canned_hits(game),
        Kind::ResourcePack => vec![
            hit("fresh-animations", "Fresh Animations", "FreshLX", "Brings Minecraft's characters to life with new animations", 9_400_000, &[game], &["minecraft", "tweaks", "vanilla-like"]),
            hit("faithful-32x", "Faithful 32x", "Faithful Resource Pack", "Faithful to the original look, twice the resolution", 6_100_000, &[game], &["minecraft", "simplistic", "vanilla-like"]),
            hit("stay-true", "Stay True", "haydenkeen", "A vanilla-like pack with more variety in every block", 2_900_000, &[game], &["minecraft", "vanilla-like", "decoration"]),
            hit("default-dark-mode", "Default Dark Mode", "nebulr", "Every menu and screen in a calm dark theme", 4_300_000, &[game], &["minecraft", "utility", "themed"]),
            hit("better-leaves", "Better Leaves", "Motschen", "Bushy leaves for every tree", 3_200_000, &[game], &["minecraft", "realistic"]),
            hit("xalis-books", "Enchanted Books Remake", "Xali", "Every enchanted book gets its own cover", 1_800_000, &[game], &["minecraft", "utility"]),
            hit("old-glass", "Clear Glass Classic", "someone", "Clear glass for older versions", 120_000, &[old], &["minecraft", "simplistic"]),
        ],
        Kind::Shader => vec![
            hit("complementary-reimagined", "Complementary Shaders - Reimagined", "EminGT", "Stunning visuals while staying true to the vanilla look", 12_800_000, &[game], &["iris", "optifine", "vanilla-like"]),
            hit("bsl-shaders", "BSL Shaders", "capttatsu", "Bright and colorful with soft shadows", 8_900_000, &[game], &["iris", "optifine", "semi-realistic"]),
            hit("makeup-ultra-fast", "MakeUp - Ultra Fast", "XavierFST", "Fast shaders for weak computers", 3_700_000, &[game], &["iris", "optifine", "vanilla-like"]),
            hit("photon-shader", "Photon", "SixthSurge", "A gameplay-focused shader with realistic lighting", 2_400_000, &[game], &["iris", "realistic"]),
            hit("sildurs-vibrant", "Sildur's Vibrant Shaders", "Sildur", "Vibrant colors and volumetric light", 4_100_000, &[game], &["iris", "optifine", "fantasy"]),
            hit("optifine-glow", "Glow Classic", "someone", "Only for OptiFine", 230_000, &[game], &["optifine", "cartoon"]),
        ],
        Kind::DataPack => vec![
            hit("terralith", "Terralith", "Stardust Labs", "Over 95 new biomes with vanilla blocks", 7_900_000, &[game], &["datapack", "worldgen"]),
            hit("incendium", "Incendium", "Stardust Labs", "A complete overhaul of the Nether", 3_600_000, &[game], &["datapack", "worldgen", "adventure"]),
            hit("veinminer", "Veinminer", "MiraculixxT", "Mine whole ore veins at once", 2_700_000, &[game], &["datapack", "utility", "game-mechanics"]),
            hit("graves", "Graves", "someone", "Your items wait in a grave where you died", 1_900_000, &[game], &["datapack", "utility"]),
            hit("afk-display", "AFK Display", "someone", "Shows who is away from the keyboard", 640_000, &[game], &["datapack", "social"]),
            hit("old-structures", "Classic Structures", "someone", "Structures for older worlds", 90_000, &[old], &["datapack", "worldgen"]),
        ],
        Kind::Modpack => vec![
            hit("fabulously-optimized", "Fabulously Optimized", "robotkoer", "Improved performance and graphics, simple to use", 14_200_000, &[game], &["fabric", "optimization", "lightweight"]),
            hit("simply-optimized", "Simply Optimized", "JustAlittleWolf", "The best performance mods, nothing else", 5_300_000, &[game], &["fabric", "optimization"]),
            hit("additive", "Additive", "Mr_Wolfey", "A vanilla-plus pack with quality of life", 2_200_000, &[game], &["fabric", "quilt", "multiplayer", "adventure"]),
            hit("cobblemon-pack", "Cobblemon Official", "Cobblemon", "Catch, train and battle in Minecraft", 4_800_000, &[game], &["fabric", "adventure"]),
            hit("better-forge", "Better Pack Forge", "someone", "A big pack for Forge", 3_000_000, &[game], &["forge", "kitchen-sink"]),
            hit("old-pack", "Classic Skyblock", "someone", "Skyblock for 1.16", 800_000, &["1.16.5"], &["fabric", "challenging"]),
        ],
    }
}

fn many_hits(kind: Kind, game: &str) -> Vec<Hit> {
    let loader = match kind {
        Kind::Mod | Kind::Modpack => "fabric",
        Kind::ResourcePack => "minecraft",
        Kind::Shader => "iris",
        Kind::DataPack => "datapack",
    };
    (0..CANNED_MANY)
        .map(|n| hit(&format!("many-{n}"), &format!("Projekt {n}"), "someone", "One of many results, for scrolling", (CANNED_MANY - n) as u64 * 1000, &[game], &[loader, "utility"]))
        .collect()
}

fn every_canned_hit() -> Vec<Hit> {
    Kind::ALL.iter().flat_map(|k| kind_hits(*k, "26.3")).collect()
}

fn canned_hit(project_id: &str) -> Option<Hit> {
    every_canned_hit().into_iter().find(|h| h.project_id == project_id).or_else(|| {
        let n: usize = project_id.strip_prefix("id-many-")?.parse().ok()?;
        many_hits(Kind::Mod, "26.3").into_iter().nth(n)
    })
}

fn canned_details(hit: &Hit) -> Details {
    let body = format!(
        "<center>\n\n# {title}\n\n</center>\n\n{description}.\n\n![Banner](https://cdn.modrinth.com/canned/banner.png)\n\n## Was es kann\n\n- Läuft mit **Minecraft 26.3**\n- Kostet kaum Leistung\n- Lässt sich im Spiel einstellen\n\n## Fragen\n\nMehr dazu im [Wiki](https://example.org/wiki). Fehler bitte <b>mit Protokoll</b> melden.",
        title = hit.title,
        description = hit.description
    );
    Details {
        id: hit.project_id.clone(),
        slug: hit.slug.clone(),
        title: hit.title.clone(),
        description: hit.description.clone(),
        body,
        downloads: hit.downloads,
        followers: hit.downloads / 300,
        updated: "2026-11-02T10:15:00Z".into(),
        gallery: (1..=3).map(|n| Shot { url: format!("https://cdn.modrinth.com/canned/{}-{n}.png", hit.slug), title: Some(format!("Bild {n}")), featured: n == 1, ordering: n }).collect(),
        icon_url: None,
    }
}

fn canned_versions(project_id: &str, kind: Kind, game: &str) -> Vec<Version> {
    let loaders: Vec<String> = match kind {
        Kind::Mod | Kind::Modpack => vec!["fabric".into(), "quilt".into()],
        Kind::ResourcePack => vec!["minecraft".into()],
        Kind::Shader => vec!["iris".into(), "optifine".into()],
        Kind::DataPack => vec!["datapack".into()],
    };
    let slug = project_id.trim_start_matches("id-");
    let deps = if kind == Kind::Mod && slug != "fabric-api" {
        vec![
            Dependency { project_id: Some("id-fabric-api".into()), version_id: None, dependency_type: "required".into() },
            Dependency { project_id: Some("id-modmenu".into()), version_id: None, dependency_type: "optional".into() },
        ]
    } else {
        Vec::new()
    };
    [("2.0", "release", "2026-11-02", vec![game.to_owned()]), ("1.9-beta.2", "beta", "2026-10-20", vec![game.to_owned()]), ("1.8", "release", "2026-09-01", vec![game.to_owned(), "26.2".into()]), ("1.0", "release", "2026-03-14", vec!["26.2".into(), "26.1".into()])]
        .into_iter()
        .map(|(number, kind_of, date, games)| Version {
            id: format!("v-{slug}-{number}"),
            project_id: project_id.to_owned(),
            name: number.into(),
            version_number: number.into(),
            version_type: kind_of.into(),
            date_published: format!("{date}T12:00:00Z"),
            game_versions: games,
            loaders: loaders.clone(),
            files: vec![File { url: format!("https://cdn.modrinth.com/canned/{slug}-{number}.zip"), filename: format!("{slug}-{number}.zip"), primary: true, hashes: Hashes { sha1: "0".repeat(40) }, size: 1 << 20 }],
            dependencies: deps.clone(),
        })
        .collect()
}

fn canned_file(project_id: &str, title: &str) -> ModRef {
    let slug = project_id.trim_start_matches("id-");
    ModRef { project_id: project_id.to_owned(), version_id: format!("v-{slug}-2.0"), title: title.to_owned(), file_name: format!("{slug}-2.0.zip"), url: format!("https://cdn.modrinth.com/canned/{slug}-2.0.zip"), sha1: "0".repeat(40) }
}

fn canned_installed(dir: &Path) -> Vec<Installed> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let hits = every_canned_hit();
    let mut out: Vec<Installed> = Vec::new();
    let mut names: Vec<String> = entries.filter_map(Result::ok).filter_map(|e| e.file_name().to_str().map(str::to_owned)).collect();
    names.sort();
    for name in names {
        let Some(hit) = hits.iter().find(|h| name.starts_with(&format!("{}-", h.slug)) && name.ends_with(".zip")) else { continue };
        if out.iter().any(|i| i.found.project_id == hit.project_id) {
            continue;
        }
        let version = name.trim_start_matches(&format!("{}-", hit.slug)).trim_end_matches(".zip");
        let found = ModRef { project_id: hit.project_id.clone(), version_id: format!("v-{}-{version}", hit.slug), title: hit.title.clone(), file_name: name.clone(), url: String::new(), sha1: "0".repeat(40) };
        out.push(Installed { file: name, found });
    }
    out
}

fn canned_modpack(project_id: &str, title: &str) -> Result<PathBuf> {
    use std::io::Write;
    let slug = project_id.trim_start_matches("id-");
    let path = paths::data_dir().join("cache").join("modpacks").join(format!("{slug}.mrpack"));
    std::fs::create_dir_all(path.parent().context("no cache folder")?)?;
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&path)?);
    zip.start_file("modrinth.index.json", zip::write::SimpleFileOptions::default())?;
    let index = serde_json::json!({ "formatVersion": 1, "game": "minecraft", "versionId": "2.0", "name": title, "files": [], "dependencies": { "minecraft": "26.3", "fabric-loader": "0.17.2" } });
    zip.write_all(index.to_string().as_bytes())?;
    zip.finish()?;
    Ok(path)
}

fn canned_shot(url: &str) -> Result<PathBuf> {
    let seed = url.bytes().fold(7u32, |a, b| a.wrapping_mul(31).wrapping_add(u32::from(b)));
    let path = paths::data_dir().join("cache").join("gallery").join(format!("canned-{seed:08x}.png"));
    if path.is_file() {
        return Ok(path);
    }
    std::fs::create_dir_all(path.parent().context("no cache folder")?)?;
    let (w, h) = CANNED_SHOT;
    let base = [(seed & 0xff) as u8, ((seed >> 8) & 0xff) as u8, ((seed >> 16) & 0xff) as u8];
    let picture = image::RgbImage::from_fn(w, h, |x, y| {
        let shade = |c: u8, t: u32| (u32::from(c) / 2 + t * 110 / w.max(h)) as u8;
        image::Rgb([shade(base[0], x), shade(base[1], y), shade(base[2], x + y)])
    });
    picture.save(&path)?;
    Ok(path)
}
