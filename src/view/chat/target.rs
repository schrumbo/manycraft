//! Who a chat line goes to: everyone, one batch or one game, the list to pick from, and the `@name` shorthand.

use uuid::Uuid;

use crate::app::model::{Model, Phase};
use crate::core::i18n::{tr, trf};

/// Recent recipients kept for the top of the list.
pub const RECENT: usize = 4;

/// Who a line goes to.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    /// Every game on a server.
    All,
    Batch(String),
    Instance(String),
}

/// Which part of the list an entry belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Recent,
    Groups,
    Accounts,
}

/// One entry of the recipient list.
#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub target: Target,
    pub label: String,
    /// The batch of a game or the count of a group, shown after the label.
    pub detail: String,
    /// What keeps a game from getting the line, when something does.
    pub state: Option<String>,
    pub reached: usize,
    pub account: Option<Uuid>,
    pub section: Section,
}

/// Whether `target` still exists in `m`.
pub fn exists(m: &Model, target: &Target) -> bool {
    match target {
        Target::All => true,
        Target::Batch(id) => m.batch(id).is_some(),
        Target::Instance(key) => m.games.contains_key(key),
    }
}

/// How many games on a server get a line sent to `target`.
pub fn reached(m: &Model, target: &Target) -> usize {
    match target {
        Target::All => m.games.values().filter(|g| g.phase.on_server()).count(),
        Target::Batch(id) => m.games.values().filter(|g| g.batch_id == *id && g.phase.on_server()).count(),
        Target::Instance(key) => usize::from(m.games.get(key).is_some_and(|g| g.phase.on_server())),
    }
}

/// The batches a line to `target` goes out through, each with the games that get it.
pub fn batches_of(m: &Model, target: &Target) -> Vec<String> {
    let mut out: Vec<String> = match target {
        Target::All => m.games.values().filter(|g| g.phase.on_server()).map(|g| g.batch_id.clone()).collect(),
        Target::Batch(id) => vec![id.clone()],
        Target::Instance(_) => Vec::new(),
    };
    out.sort();
    out.dedup();
    out
}

/// The short name of `target` for the composer: "Alle", the batch or the account.
pub fn name(m: &Model, target: &Target) -> String {
    match target {
        Target::All => tr("Alle"),
        Target::Batch(id) => m.batch(id).map(|b| b.name.clone()).unwrap_or_default(),
        Target::Instance(key) => m.games.get(key).map(|g| g.name.clone()).unwrap_or_default(),
    }
}

/// Why a game does not get chat now, in a word; `None` while it is on a server.
pub fn state(phase: &Phase) -> Option<String> {
    Some(match phase {
        Phase::Queue { .. } | Phase::Playing { .. } => return None,
        Phase::Stopped => tr("aus"),
        Phase::Starting { .. } | Phase::Restarting { .. } => tr("startet"),
        Phase::Menu | Phase::Plain => tr("im Menü"),
        Phase::Connecting { .. } | Phase::NetworkLost { .. } | Phase::NoNetwork => tr("verbindet"),
        _ => tr("getrennt"),
    })
}

fn group(m: &Model, target: Target, section: Section) -> Choice {
    let reached = reached(m, &target);
    let (label, detail) = match &target {
        Target::All => (tr("Alle"), if reached == 1 { tr("1 im Spiel") } else { trf("%lld im Spiel", &[&reached]) }),
        Target::Batch(id) => {
            let total = m.games.values().filter(|g| g.batch_id == *id).count();
            (name(m, &target), trf("%lld von %lld im Spiel", &[&reached, &total]))
        }
        Target::Instance(_) => unreachable!(),
    };
    Choice { target, label, detail, state: (reached == 0).then(|| tr("niemand im Spiel")), reached, account: None, section }
}

fn game(m: &Model, key: &str, section: Section, many: bool) -> Option<Choice> {
    let g = m.games.get(key)?;
    let detail = if many { m.batch(&g.batch_id).map(|b| b.name.clone()).unwrap_or_default() } else { String::new() };
    Some(Choice { target: Target::Instance(key.to_owned()), label: g.name.clone(), detail, state: state(&g.phase), reached: usize::from(g.phase.on_server()), account: Some(g.account_id), section })
}

fn entry(m: &Model, target: &Target, section: Section, many: bool) -> Option<Choice> {
    match target {
        Target::Instance(key) => game(m, key, section, many),
        t if exists(m, t) => Some(group(m, t.clone(), section)),
        _ => None,
    }
}

/// The full list: recent recipients, everyone and each batch, then every game, those on a server first.
pub fn choices(m: &Model, recent: &[Target]) -> Vec<Choice> {
    let many = m.batches.len() > 1;
    let mut out: Vec<Choice> = recent.iter().filter_map(|t| entry(m, t, Section::Recent, many)).take(RECENT).collect();
    out.push(group(m, Target::All, Section::Groups));
    if many {
        out.extend(m.batches.iter().map(|b| group(m, Target::Batch(b.id.clone()), Section::Groups)));
    }
    let mut games: Vec<_> = m.games.values().collect();
    games.sort_by_cached_key(|g| (!g.phase.on_server(), g.name.to_lowercase(), g.key.clone()));
    out.extend(games.iter().filter_map(|g| game(m, &g.key, Section::Accounts, many)));
    out
}

/// The entries matching `query`, best first: names that start with it, then names that contain it, then batches; each
/// recipient once.
pub fn filter(all: &[Choice], query: &str) -> Vec<Choice> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return all.to_vec();
    }
    let mut seen = Vec::new();
    let mut ranked: Vec<(u8, usize, &Choice)> = Vec::new();
    for (i, c) in all.iter().enumerate() {
        if seen.contains(&c.target) {
            continue;
        }
        let label = c.label.to_lowercase();
        let rank = if label.starts_with(&q) {
            0
        } else if label.contains(&q) {
            1
        } else if c.detail.to_lowercase().contains(&q) {
            2
        } else {
            continue;
        };
        seen.push(c.target.clone());
        ranked.push((rank, i, c));
    }
    ranked.sort_by_key(|(rank, i, c)| (*rank, c.reached == 0, *i));
    ranked.into_iter().map(|(_, _, c)| c.clone()).collect()
}

/// The name being typed after an `@` at the start of the message, while no space ended it yet.
pub fn at_query(draft: &str) -> Option<&str> {
    let rest = draft.strip_prefix('@')?;
    (!rest.contains(char::is_whitespace)).then_some(rest)
}

/// The `@name` a message starts with, once a space ended it: the name and the message after it.
pub fn at_word(draft: &str) -> Option<(&str, &str)> {
    let (word, text) = draft.strip_prefix('@')?.split_once(char::is_whitespace)?;
    Some((word, text.trim_start()))
}

/// The one recipient `word` names: the entry called exactly that, else the only one whose name contains it.
pub fn named(all: &[Choice], word: &str) -> Option<Target> {
    let word = word.to_lowercase();
    let distinct = |hit: &dyn Fn(&str) -> bool| {
        let mut out: Vec<&Target> = Vec::new();
        for c in all {
            if hit(&c.label.to_lowercase()) && !out.contains(&&c.target) {
                out.push(&c.target);
            }
        }
        out
    };
    match distinct(&|l| l == word).as_slice() {
        [one] => return Some((*one).clone()),
        [] => {}
        _ => return None,
    }
    match distinct(&|l| l.contains(&word)).as_slice() {
        [one] => Some((*one).clone()),
        _ => None,
    }
}

/// An `@name ` the message starts with, once a space ended it and the name picks exactly one recipient: that recipient
/// and the message without it.
pub fn resolve_at(draft: &str, all: &[Choice]) -> Option<(Target, String)> {
    let (word, text) = at_word(draft)?;
    Some((named(all, word)?, text.to_owned()))
}

/// `recent` with `target` moved to the front, at most [`RECENT`] long.
pub fn remember(recent: &[Target], target: &Target) -> Vec<Target> {
    let mut out = vec![target.clone()];
    out.extend(recent.iter().filter(|t| *t != target).cloned());
    out.truncate(RECENT);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choice(target: Target, label: &str, detail: &str, reached: usize) -> Choice {
        Choice { target, label: label.into(), detail: detail.into(), state: None, reached, account: None, section: Section::Accounts }
    }

    fn sample() -> Vec<Choice> {
        vec![
            choice(Target::All, "Alle", "3 im Spiel", 3),
            choice(Target::Batch("farm".into()), "Farm", "2 von 3 im Spiel", 2),
            choice(Target::Instance("a".into()), "Alex", "Farm", 1),
            choice(Target::Instance("b".into()), "Malex", "Farm", 1),
            choice(Target::Instance("c".into()), "Bert", "Farm", 0),
            choice(Target::Instance("d".into()), "Berta", "Farm", 1),
        ]
    }

    #[test]
    fn filter_puts_prefixes_first_then_contains_then_batches() {
        let names = |q| filter(&sample(), q).into_iter().map(|c| c.label).collect::<Vec<_>>();
        assert_eq!(names("ale"), ["Alex", "Malex"]);
        assert_eq!(names("bert"), ["Berta", "Bert"]);
        assert_eq!(names("farm")[0], "Farm");
        assert_eq!(names("  ").len(), 6);
        assert!(names("zzz").is_empty());
    }

    #[test]
    fn filter_lists_a_recent_recipient_once() {
        let mut all = sample();
        all.insert(0, Choice { section: Section::Recent, ..choice(Target::Instance("a".into()), "Alex", "Farm", 1) });
        assert_eq!(filter(&all, "alex").iter().filter(|c| c.label == "Alex").count(), 1);
    }

    #[test]
    fn at_names_pick_one_recipient_once_a_space_ends_them() {
        assert_eq!(at_query("@al"), Some("al"));
        assert_eq!(at_query("@"), Some(""));
        assert_eq!(at_query("@al hi"), None);
        assert_eq!(at_query("hi @al"), None);
        let all = sample();
        assert_eq!(resolve_at("@alex hallo", &all), Some((Target::Instance("a".into()), "hallo".into())));
        assert_eq!(resolve_at("@Bert  /home", &all), Some((Target::Instance("c".into()), "/home".into())));
        assert_eq!(resolve_at("@mal x", &all), Some((Target::Instance("b".into()), "x".into())));
        assert_eq!(resolve_at("@be x", &all), None);
        assert_eq!(resolve_at("@alex", &all), None);
        assert_eq!(resolve_at("@nobody x", &all), None);
        assert_eq!(resolve_at("@spiel x", &all), None);
        let mut recent = all.clone();
        recent.insert(0, Choice { section: Section::Recent, ..choice(Target::Instance("a".into()), "Alex", "Farm", 1) });
        assert_eq!(resolve_at("@alex yo", &recent), Some((Target::Instance("a".into()), "yo".into())));
    }

    #[test]
    fn remember_keeps_the_newest_first_without_repeats() {
        let a = Target::Instance("a".into());
        let b = Target::Instance("b".into());
        let recent = remember(&remember(&remember(&[], &a), &b), &a);
        assert_eq!(recent, [a.clone(), b.clone()]);
        let mut many = Vec::new();
        for k in 0..10 {
            many = remember(&many, &Target::Instance(k.to_string()));
        }
        assert_eq!(many.len(), RECENT);
        assert_eq!(many[0], Target::Instance("9".into()));
    }
}
