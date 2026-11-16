use std::cell::Cell;
use std::collections::{HashMap, HashSet};

use crate::core::content::{self, Entry, Scan, Source};
use crate::core::i18n::{tr, trf};
use crate::core::instance::Instance;

const NAMES_SHOWN: usize = 4;

/// The orders the list can take.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Sort {
    #[default]
    Name,
    Added,
    Updated,
}

thread_local! {
    static SORTED: Cell<Sort> = const { Cell::new(Sort::Name) };
}

impl Sort {
    pub const ALL: [Sort; 3] = [Sort::Name, Sort::Added, Sort::Updated];

    pub fn label(self) -> String {
        match self {
            Self::Name => tr("Name"),
            Self::Added => tr("Hinzugefügt"),
            Self::Updated => tr("Aktualisiert"),
        }
    }

    /// The order the person chose last, for every instance.
    pub fn current() -> Self {
        SORTED.with(Cell::get)
    }

    pub fn remember(self) {
        SORTED.with(|s| s.set(self));
    }
}

/// One line of the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Own,
    Mod(usize),
}

/// What the mods say about each other: duplicates and switched-off requirements.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Analysis {
    pub older: HashMap<String, Vec<String>>,
    pub twin: HashMap<String, String>,
    pub needs: HashMap<String, Vec<String>>,
    pub dups: usize,
}

/// A scan with what is worked out from it once.
#[derive(Clone, Debug, Default)]
pub struct Data {
    pub scan: Scan,
    pub scanned: bool,
    pub analysis: Analysis,
    index: HashMap<String, usize>,
}

impl Data {
    pub fn new(scan: Scan, scanned: bool) -> Self {
        let analysis = analyze(&scan, scanned);
        let index = scan.entries.iter().enumerate().map(|(at, e)| (e.file.clone(), at)).collect();
        Self { scan, scanned, analysis, index }
    }

    /// What the instance lists, before its files are read.
    pub fn fallback(made: &Instance) -> Self {
        Self::new(Scan { entries: fallback(made), ..Scan::default() }, false)
    }

    pub fn entries(&self) -> &[Entry] {
        &self.scan.entries
    }

    pub fn entry(&self, file: &str) -> Option<&Entry> {
        self.index.get(file).and_then(|at| self.scan.entries.get(*at))
    }

    /// The same data with `files` shown on or off.
    pub fn switched(&self, files: &[String], on: bool) -> Self {
        let mut scan = self.scan.clone();
        for e in scan.entries.iter_mut().filter(|e| files.contains(&e.file)) {
            e.enabled = on;
        }
        Self::new(scan, self.scanned)
    }

    /// The same data without `files`.
    pub fn without(&self, files: &[String]) -> Self {
        let mut scan = self.scan.clone();
        scan.entries.retain(|e| !files.contains(&e.file));
        Self::new(scan, self.scanned)
    }

    pub fn label_of(&self, files: &[String]) -> String {
        match files {
            [one] => quoted(&self.entry(one).map_or_else(|| one.clone(), Entry::name)),
            many => count_mods(many.len()),
        }
    }

    /// The names of `files`, sorted.
    pub fn names(&self, files: &[String]) -> Vec<String> {
        let mut names: Vec<String> = files.iter().map(|f| self.entry(f).map_or_else(|| f.clone(), Entry::name)).collect();
        names.sort_by_key(|n| n.to_lowercase());
        names
    }
}

/// The list of an instance as it names its mods, shown until the files are read.
pub fn fallback(made: &Instance) -> Vec<Entry> {
    let off = |file: &str| made.disabled.iter().any(|d| d.eq_ignore_ascii_case(file));
    let listed = made.mods.iter().map(|m| Entry {
        file: m.file_name.clone(),
        on_disk: true,
        enabled: !off(&m.file_name),
        source: Source::Modrinth { project_id: m.project_id.clone(), title: m.title.clone() },
        meta: None,
        icon: None,
        added: None,
        updated: None,
        size: 0,
    });
    let own = made.local_mods.iter().map(|n| Entry { file: n.clone(), on_disk: true, enabled: !off(n), source: Source::Own, meta: None, icon: None, added: None, updated: None, size: 0 });
    listed.chain(own).collect()
}

/// What changes the list of mods: the files an instance names and which are off.
pub fn key_of(made: &Instance) -> String {
    let mut parts: Vec<String> = made.mods.iter().map(|m| m.file_name.to_lowercase()).collect();
    parts.push("|".into());
    parts.extend(made.local_mods.iter().map(|n| n.to_lowercase()));
    parts.push("|".into());
    parts.extend(made.disabled.iter().map(|n| n.to_lowercase()));
    parts.join("\n")
}

fn analyze(scan: &Scan, scanned: bool) -> Analysis {
    let mut out = Analysis::default();
    let on: Vec<Entry> = scan.entries.iter().filter(|e| e.enabled).cloned().collect();
    for dup in content::duplicates(&on) {
        out.dups += 1;
        let older = dup.files.get(1..).unwrap_or_default().to_vec();
        for file in &dup.files {
            out.older.insert(file.clone(), older.clone());
            if let Some(twin) = dup.files.iter().find(|f| *f != file) {
                out.twin.insert(file.clone(), twin.clone());
            }
        }
    }
    if scanned {
        out.needs = content::missing_for_enabled(scan);
    }
    out
}

/// The rows for `query` (already trimmed and lowercase) in `sort` order: Modrinth mods, then "Eigene Dateien" with the own jars.
pub fn rows(data: &Data, query: &str, sort: Sort) -> Vec<Row> {
    let entries = data.entries();
    let fits = |e: &Entry| query.is_empty() || e.name().to_lowercase().contains(query) || e.file.to_lowercase().contains(query) || e.id().is_some_and(|id| id.contains(query));
    let mut listed: Vec<usize> = Vec::new();
    let mut own: Vec<usize> = Vec::new();
    for (at, e) in entries.iter().enumerate().filter(|(_, e)| fits(e)) {
        match e.source {
            Source::Modrinth { .. } => listed.push(at),
            Source::Own => own.push(at),
        }
    }
    let order = |list: &mut Vec<usize>| match sort {
        Sort::Name => list.sort_by_cached_key(|at| (entries[*at].name().to_lowercase(), entries[*at].file.to_lowercase())),
        Sort::Added => list.sort_by(|a, b| entries[*b].added.cmp(&entries[*a].added).then_with(|| entries[*a].file.cmp(&entries[*b].file))),
        Sort::Updated => list.sort_by(|a, b| entries[*b].updated.cmp(&entries[*a].updated).then_with(|| entries[*a].file.cmp(&entries[*b].file))),
    };
    order(&mut listed);
    order(&mut own);
    let mut rows: Vec<Row> = listed.into_iter().map(Row::Mod).collect();
    if !own.is_empty() {
        rows.push(Row::Own);
        rows.extend(own.into_iter().map(Row::Mod));
    }
    rows
}

/// The files of the mod rows, in order.
pub fn files(data: &Data, rows: &[Row]) -> Vec<String> {
    rows.iter().filter_map(|r| if let Row::Mod(at) = *r { data.entries().get(at).map(|e| e.file.clone()) } else { None }).collect()
}

/// The row of `file`, for scrolling to it.
pub fn row_of(data: &Data, rows: &[Row], file: &str) -> Option<usize> {
    rows.iter().position(|r| matches!(*r, Row::Mod(at) if data.entries().get(at).is_some_and(|e| e.file == file)))
}

/// The file `delta` rows from `cursor`, clamped to the list; the first or last when there is no cursor.
pub fn step(order: &[String], cursor: Option<&str>, delta: i64) -> Option<String> {
    if order.is_empty() {
        return None;
    }
    let last = order.len() as i64 - 1;
    let at = cursor.and_then(|c| order.iter().position(|f| f == c));
    let next = match at {
        Some(at) => (at as i64).saturating_add(delta).clamp(0, last),
        None if delta < 0 => last,
        None => 0,
    };
    order.get(next as usize).cloned()
}

/// The files from `anchor` to `file` in `order`, and the anchor used; `file` alone when the anchor is not listed.
pub fn range(order: &[String], anchor: Option<&str>, file: &str) -> Option<(Vec<String>, String)> {
    let anchor = anchor.filter(|a| order.iter().any(|f| f == a)).unwrap_or(file).to_owned();
    let a = order.iter().position(|f| *f == anchor)?;
    let b = order.iter().position(|f| f == file)?;
    let (lo, hi) = (a.min(b), a.max(b));
    Some((order.get(lo..=hi)?.to_vec(), anchor))
}

/// The row to move to once `gone` leaves: the next below the last of them, else the nearest above.
pub fn after(order: &[String], gone: &[String]) -> Option<String> {
    let last = order.iter().rposition(|f| gone.contains(f))?;
    order.get(last + 1..).unwrap_or_default().iter().chain(order.get(..last).unwrap_or_default().iter().rev()).find(|f| !gone.contains(f)).cloned()
}

/// "„name“"
pub fn quoted(name: &str) -> String {
    trf("„%@“", &[&name])
}

/// Up to four quoted names, then how many more.
pub fn listing(names: &[String]) -> String {
    let shown: Vec<String> = names.iter().take(NAMES_SHOWN).map(|n| quoted(n)).collect();
    match names.len().checked_sub(NAMES_SHOWN).filter(|more| *more > 0) {
        Some(1) => trf("%@ und 1 weiterer", &[&shown.join(", ")]),
        Some(more) => trf("%@ und %lld weitere", &[&shown.join(", "), &more]),
        None => shown.join(", "),
    }
}

pub fn count_mods(n: usize) -> String {
    if n == 1 { tr("1 Mod") } else { trf("%lld Mods", &[&n]) }
}

/// The line above the list: how many, how many are off, how many are doubled.
pub fn count_text(data: &Data, shown: usize, filtering: bool) -> String {
    let total = data.entries().len();
    let coming = if data.scanned { data.entries().iter().filter(|e| !e.on_disk && e.enabled).count() } else { 0 };
    if filtering {
        trf("%lld von %@", &[&shown, &count_mods(total)])
    } else if coming > 0 && coming == total {
        if total == 1 { tr("1 Mod kommt beim nächsten Start dazu") } else { trf("%lld Mods kommen beim nächsten Start dazu", &[&total]) }
    } else {
        let mut parts = vec![count_mods(total)];
        if coming > 0 {
            parts.push(if coming == 1 { tr("1 kommt beim nächsten Start dazu") } else { trf("%lld kommen beim nächsten Start dazu", &[&coming]) });
        }
        let off = data.entries().iter().filter(|e| !e.enabled).count();
        if off > 0 {
            parts.push(trf("%lld aus", &[&off]));
        }
        let dups = data.analysis.dups;
        if dups > 0 {
            parts.push(if dups == 1 { tr("1 doppelt") } else { trf("%lld doppelt", &[&dups]) });
        }
        parts.join(" · ")
    }
}

/// The second line of a row and whether it warns.
pub fn caption(data: &Data, e: &Entry) -> (String, bool) {
    if let Some(twin) = data.analysis.twin.get(&e.file).and_then(|f| data.entry(f)) {
        let shown = twin.version().map_or_else(|| twin.file.clone(), str::to_owned);
        (trf("Doppelt: auch %@ ist da.", &[&shown]), true)
    } else if let Some(missing) = data.analysis.needs.get(&e.file) {
        (trf("Braucht %@, ist aus.", &[&listing(&data.names(missing))]), true)
    } else if !e.enabled {
        (trf("Aus · %@", &[&e.file]), false)
    } else {
        (e.file.clone(), false)
    }
}

/// "Name Version (aus)" for every mod, sorted by name.
pub fn copy_text(entries: &[Entry]) -> Option<String> {
    let mut lines: Vec<(String, String)> = entries
        .iter()
        .map(|e| {
            let mut line = match e.version() {
                Some(v) if !v.is_empty() => format!("{} {v}", e.name()),
                _ => e.name(),
            };
            if !e.enabled {
                line.push_str(&trf(" (%@)", &[&tr("aus")]));
            }
            (e.name().to_lowercase(), line)
        })
        .collect();
    if lines.is_empty() {
        return None;
    }
    lines.sort();
    Some(lines.into_iter().map(|(_, l)| l).collect::<Vec<_>>().join("\n"))
}

/// How many rounds the troubleshooter takes for `on` enabled mods.
pub fn rounds(on: usize) -> usize {
    (usize::BITS - on.saturating_sub(1).leading_zeros()) as usize
}

/// The files of `set` that are listed and chosen.
pub fn chosen(order: &[String], selected: &HashSet<String>) -> Vec<String> {
    order.iter().filter(|f| selected.contains(*f)).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::modmeta::Meta;
    use std::time::{Duration, SystemTime};

    fn entry(file: &str, source: Source) -> Entry {
        Entry { file: file.into(), on_disk: true, enabled: true, source, meta: None, icon: None, added: None, updated: None, size: 0 }
    }

    fn listed(file: &str, title: &str) -> Entry {
        entry(file, Source::Modrinth { project_id: file.into(), title: title.into() })
    }

    fn data(entries: Vec<Entry>) -> Data {
        Data::new(Scan { entries, ..Scan::default() }, true)
    }

    fn names(d: &Data, rows: &[Row]) -> Vec<String> {
        rows.iter().map(|r| match *r {
            Row::Own => "-".to_owned(),
            Row::Mod(at) => d.entries()[at].file.clone(),
        }).collect()
    }

    #[test]
    fn modrinth_mods_come_first_then_own_files_after_a_header() {
        let d = data(vec![entry("zeta.jar", Source::Own), listed("b.jar", "Beta"), listed("a.jar", "alpha"), entry("mine.jar", Source::Own)]);
        let rows = rows(&d, "", Sort::Name);
        assert_eq!(names(&d, &rows), ["a.jar", "b.jar", "-", "mine.jar", "zeta.jar"]);
    }

    #[test]
    fn no_own_files_means_no_header() {
        let d = data(vec![listed("a.jar", "A")]);
        assert_eq!(rows(&d, "", Sort::Name), [Row::Mod(0)]);
    }

    #[test]
    fn filter_matches_name_file_and_id_in_any_case() {
        let mut meta = entry("x.jar", Source::Own);
        meta.meta = Some(Meta { id: "sodium-extra".into(), name: "Quiet".into(), ..Meta::default() });
        let d = data(vec![listed("a.jar", "Sodium"), listed("lithium-1.jar", "Other"), meta]);
        assert_eq!(names(&d, &rows(&d, "sod", Sort::Name)), ["a.jar", "-", "x.jar"]);
        assert_eq!(names(&d, &rows(&d, "lithium", Sort::Name)), ["lithium-1.jar"]);
        assert!(rows(&d, "nothing", Sort::Name).is_empty());
    }

    #[test]
    fn newest_first_for_dates_with_file_as_tie_break() {
        let at = |s| Some(SystemTime::UNIX_EPOCH + Duration::from_secs(s));
        let mut a = listed("a.jar", "A");
        let mut b = listed("b.jar", "B");
        let mut c = listed("c.jar", "C");
        a.added = at(5);
        b.added = at(9);
        c.added = at(5);
        b.updated = at(1);
        let d = data(vec![a, b, c]);
        assert_eq!(names(&d, &rows(&d, "", Sort::Added)), ["b.jar", "a.jar", "c.jar"]);
        assert_eq!(names(&d, &rows(&d, "", Sort::Updated)), ["b.jar", "a.jar", "c.jar"]);
    }

    #[test]
    fn the_older_of_two_files_with_one_id_is_marked() {
        let with = |file: &str, version: &str| {
            let mut e = listed(file, "Mod");
            e.meta = Some(Meta { id: "mod".into(), name: "Mod".into(), version: version.into(), ..Meta::default() });
            e
        };
        let d = data(vec![with("mod-1.jar", "1.0"), with("mod-2.jar", "2.0"), listed("other.jar", "Other")]);
        assert_eq!(d.analysis.dups, 1);
        assert_eq!(d.analysis.older["mod-2.jar"], ["mod-1.jar"]);
        assert_eq!(d.analysis.twin["mod-1.jar"], "mod-2.jar");
        let (text, warn) = caption(&d, d.entry("mod-1.jar").unwrap());
        assert!(warn);
        assert_eq!(text, "Doppelt: auch 2.0 ist da.");
        assert_eq!(count_text(&d, 3, false), "3 Mods · 1 doppelt");
    }

    #[test]
    fn a_disabled_dependency_is_listed_after_the_scan_only() {
        let mut lib = listed("lib.jar", "Lib");
        lib.enabled = false;
        lib.meta = Some(Meta { id: "lib".into(), name: "Lib".into(), ..Meta::default() });
        let mut app = listed("app.jar", "App");
        app.meta = Some(Meta { id: "app".into(), name: "App".into(), depends: vec!["lib".into()], ..Meta::default() });
        let entries = vec![lib, app];
        let d = Data::new(Scan { entries: entries.clone(), ..Scan::default() }, true);
        assert_eq!(d.analysis.needs["app.jar"], ["lib.jar"]);
        let (text, warn) = caption(&d, d.entry("app.jar").unwrap());
        assert!(warn);
        assert_eq!(text, "Braucht „Lib“, ist aus.");
        assert_eq!(caption(&d, d.entry("lib.jar").unwrap()), ("Aus · lib.jar".to_owned(), false));
        let early = Data::new(Scan { entries, ..Scan::default() }, false);
        assert!(early.analysis.needs.is_empty());
    }

    #[test]
    fn switching_and_removing_make_new_data() {
        let d = data(vec![listed("a.jar", "A"), listed("b.jar", "B")]);
        let off = d.switched(&["a.jar".to_owned()], false);
        assert!(d.entry("a.jar").is_some_and(|e| e.enabled));
        assert!(off.entry("a.jar").is_some_and(|e| !e.enabled));
        let gone = d.without(&["a.jar".to_owned()]);
        assert!(gone.entry("a.jar").is_none());
        assert_eq!(gone.entries().len(), 1);
    }

    #[test]
    fn counts_and_names_read_plainly() {
        assert_eq!(count_mods(1), "1 Mod");
        assert_eq!(count_mods(7), "7 Mods");
        let five: Vec<String> = ["a", "b", "c", "d", "e"].iter().map(|s| (*s).to_owned()).collect();
        assert_eq!(listing(&five[..1]), "„a“");
        assert_eq!(listing(&five[..4]), "„a“, „b“, „c“, „d“");
        assert_eq!(listing(&five), "„a“, „b“, „c“, „d“ und 1 weiterer");
        let mut off = listed("b.jar", "B");
        off.enabled = false;
        let d = data(vec![listed("a.jar", "A"), off]);
        assert_eq!(count_text(&d, 2, false), "2 Mods · 1 aus");
        assert_eq!(count_text(&d, 1, true), "1 von 2 Mods");
    }

    #[test]
    fn mods_not_yet_on_disk_come_with_the_next_start() {
        let mut missing = listed("a.jar", "A");
        missing.on_disk = false;
        let d = data(vec![missing.clone()]);
        assert_eq!(count_text(&d, 1, false), "1 Mod kommt beim nächsten Start dazu");
        let d = data(vec![missing, listed("b.jar", "B")]);
        assert_eq!(count_text(&d, 2, false), "2 Mods · 1 kommt beim nächsten Start dazu");
    }

    #[test]
    fn the_copied_list_is_sorted_by_name_and_marks_what_is_off() {
        let mut zed = listed("z.jar", "Zed");
        zed.enabled = false;
        let mut sodium = listed("s.jar", "sodium");
        sodium.meta = Some(Meta { id: "sodium".into(), name: "sodium".into(), version: "1.2".into(), ..Meta::default() });
        let text = copy_text(&[zed, listed("a.jar", "Alpha"), sodium]).unwrap();
        assert_eq!(text, "Alpha\nsodium 1.2\nZed (aus)");
        assert!(copy_text(&[]).is_none());
    }

    fn list(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("{i}.jar")).collect()
    }

    #[test]
    fn stepping_clamps_and_starts_at_the_ends() {
        let order = list(5);
        assert_eq!(step(&order, Some("2.jar"), 1).as_deref(), Some("3.jar"));
        assert_eq!(step(&order, Some("4.jar"), 1).as_deref(), Some("4.jar"));
        assert_eq!(step(&order, Some("1.jar"), -10).as_deref(), Some("0.jar"));
        assert_eq!(step(&order, None, 1).as_deref(), Some("0.jar"));
        assert_eq!(step(&order, None, -1).as_deref(), Some("4.jar"));
        assert_eq!(step(&order, Some("2.jar"), i64::MAX / 2).as_deref(), Some("4.jar"));
        assert_eq!(step(&order, Some("2.jar"), i64::MIN / 2).as_deref(), Some("0.jar"));
        assert!(step(&[], None, 1).is_none());
    }

    #[test]
    fn a_range_runs_from_the_anchor_either_way() {
        let order = list(6);
        assert_eq!(range(&order, Some("1.jar"), "3.jar"), Some((order[1..=3].to_vec(), "1.jar".to_owned())));
        assert_eq!(range(&order, Some("4.jar"), "2.jar"), Some((order[2..=4].to_vec(), "4.jar".to_owned())));
        assert_eq!(range(&order, Some("gone.jar"), "2.jar"), Some((vec!["2.jar".to_owned()], "2.jar".to_owned())));
        assert_eq!(range(&order, None, "9.jar"), None);
    }

    #[test]
    fn after_deleting_the_cursor_moves_below_else_above() {
        let order = list(4);
        assert_eq!(after(&order, &["1.jar".to_owned()]).as_deref(), Some("2.jar"));
        assert_eq!(after(&order, &["3.jar".to_owned()]).as_deref(), Some("2.jar"));
        assert_eq!(after(&order, &["2.jar".to_owned(), "3.jar".to_owned()]).as_deref(), Some("1.jar"));
        assert_eq!(after(&order, &order), None);
    }

    #[test]
    fn rounds_are_the_halvings_needed() {
        assert_eq!(rounds(2), 1);
        assert_eq!(rounds(3), 2);
        assert_eq!(rounds(8), 3);
        assert_eq!(rounds(9), 4);
        assert_eq!(rounds(0), 0);
    }
}
