//! Who a chat line goes to: everyone, one batch or one game, the list to pick from, and the `@name` shorthand. The list
//! belongs to the page: all batches offer everyone and every running game grouped by batch, one batch offers itself and its
//! own running games.

use uuid::Uuid;

use crate::app::model::{Game, Model, Phase};
use crate::core::i18n::{tr, trf};

/// Who a line goes to.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    /// Every game on a server.
    All,
    Batch(String),
    Instance(String),
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
    /// The batch heading the entry stands under, when the list groups by batch.
    pub heading: Option<String>,
}

/// Whether `target` is a recipient of the page showing `scope`: everyone on the page of all batches, the batch on its
/// own page, a game on either when it belongs there.
pub fn on_page(m: &Model, target: &Target, scope: Option<&str>) -> bool {
    match (target, scope) {
        (Target::All, scope) => scope.is_none(),
        (Target::Batch(id), scope) => scope == Some(id.as_str()),
        (Target::Instance(key), None) => m.games.contains_key(key),
        (Target::Instance(key), Some(batch)) => m.games.get(key).is_some_and(|g| g.batch_id == batch),
    }
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
        Target::All => m.games.values().filter(|g| g.phase.can_chat()).count(),
        Target::Batch(id) => m.games.values().filter(|g| g.batch_id == *id && g.phase.can_chat()).count(),
        Target::Instance(key) => usize::from(m.games.get(key).is_some_and(|g| g.phase.can_chat())),
    }
}

/// The batches a line to `target` goes out through, each with the games that get it.
pub fn batches_of(m: &Model, target: &Target) -> Vec<String> {
    let mut out: Vec<String> = match target {
        Target::All => m.games.values().filter(|g| g.phase.can_chat()).map(|g| g.batch_id.clone()).collect(),
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
        Target::Batch(id) => m.batch(id).map(|b| trf("Alle in %@", &[&b.name])).unwrap_or_default(),
        Target::Instance(key) => m.games.get(key).map(|g| g.name.clone()).unwrap_or_default(),
    }
}

/// Why a game does not get chat now, in a word; `None` while it is on a server.
pub fn state(phase: &Phase) -> Option<String> {
    Some(match phase {
        Phase::Queue { .. } | Phase::Playing { .. } => return None,
        Phase::Dead { .. } => tr("gestorben"),
        Phase::Stopped => tr("aus"),
        Phase::Starting { .. } | Phase::Restarting { .. } => tr("startet"),
        Phase::Menu | Phase::Plain => tr("im Menü"),
        Phase::Connecting { .. } | Phase::NetworkLost { .. } | Phase::ServerAway { .. } | Phase::NoNetwork => tr("verbindet"),
        _ => tr("getrennt"),
    })
}

fn group(m: &Model, target: Target) -> Choice {
    let reached = reached(m, &target);
    let (label, detail) = match &target {
        Target::All => (tr("Alle"), if reached == 1 { tr("1 im Spiel") } else { trf("%lld im Spiel", &[&reached]) }),
        Target::Batch(id) => {
            let total = m.games.values().filter(|g| g.batch_id == *id).count();
            (name(m, &target), trf("%lld von %lld im Spiel", &[&reached, &total]))
        }
        Target::Instance(_) => unreachable!(),
    };
    Choice { target, label, detail, state: (reached == 0).then(|| tr("niemand im Spiel")), reached, account: None, heading: None }
}

fn game(g: &Game, heading: Option<String>) -> Choice {
    Choice { target: Target::Instance(g.key.clone()), label: g.name.clone(), detail: String::new(), state: state(&g.phase), reached: usize::from(g.phase.can_chat()), account: Some(g.account_id), heading }
}

/// The list of the page showing `scope`. The page of all batches offers everyone, then each batch's running games under
/// the batch's name; a batch's page offers "Alle in <batch>", then its running games. Games that are off are not offered.
pub fn choices(m: &Model, scope: Option<&str>) -> Vec<Choice> {
    let running = |g: &&Game| g.phase.is_active();
    match scope.filter(|b| m.batch(b).is_some()) {
        Some(batch) => {
            let mut games: Vec<&Game> = m.games.values().filter(|g| g.batch_id == batch).filter(running).collect();
            games.sort_by_cached_key(|g| (g.name.to_lowercase(), g.key.clone()));
            let mut out = vec![group(m, Target::Batch(batch.to_owned()))];
            out.extend(games.into_iter().map(|g| game(g, None)));
            out
        }
        None => {
            let grouped = m.batches.len() > 1;
            let mut out = vec![group(m, Target::All)];
            for b in &m.batches {
                let mut games: Vec<&Game> = m.games.values().filter(|g| g.batch_id == b.id).filter(running).collect();
                games.sort_by_cached_key(|g| (g.name.to_lowercase(), g.key.clone()));
                out.extend(games.into_iter().map(|g| game(g, grouped.then(|| b.name.clone()))));
            }
            out
        }
    }
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
        } else if c.heading.as_deref().is_some_and(|h| h.to_lowercase().contains(&q)) || c.detail.to_lowercase().contains(&q) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::batch::Batch;

    fn choice(target: Target, label: &str, heading: Option<&str>, reached: usize) -> Choice {
        Choice { target, label: label.into(), detail: String::new(), state: None, reached, account: None, heading: heading.map(Into::into) }
    }

    fn sample() -> Vec<Choice> {
        vec![
            choice(Target::All, "Alle", None, 3),
            choice(Target::Instance("a".into()), "Alex", Some("Farm"), 1),
            choice(Target::Instance("b".into()), "Malex", Some("Farm"), 1),
            choice(Target::Instance("c".into()), "Bert", Some("Farm"), 0),
            choice(Target::Instance("d".into()), "Berta", Some("Lobby"), 1),
        ]
    }

    fn world() -> Model {
        let mut m = Model::default();
        m.batches = vec![Batch::new("Farm", "i"), Batch::new("Lobby", "i")];
        m.batches[0].id = "farm".into();
        m.batches[1].id = "lobby".into();
        for (key, batch, name, phase) in [
            ("k1", "farm", "Zed", Phase::Menu),
            ("k2", "farm", "Alex", Phase::Menu),
            ("k3", "farm", "Off", Phase::Stopped),
            ("k4", "lobby", "Berta", Phase::Menu),
            ("k5", "lobby", "Gone", Phase::Stopped),
        ] {
            m.games.insert(key.into(), Game { phase, ..Game::new(key.into(), batch.into(), "i".into(), uuid::Uuid::from_u128(1), name.into()) });
        }
        m
    }

    fn labels(list: &[Choice]) -> Vec<String> {
        list.iter().map(|c| c.label.clone()).collect()
    }

    #[test]
    fn filter_puts_prefixes_first_then_contains_then_batches() {
        let names = |q| filter(&sample(), q).into_iter().map(|c| c.label).collect::<Vec<_>>();
        assert_eq!(names("ale"), ["Alex", "Malex"]);
        assert_eq!(names("bert"), ["Berta", "Bert"]);
        assert_eq!(names("lobby"), ["Berta"]);
        assert_eq!(names("  ").len(), 5);
        assert!(names("zzz").is_empty());
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
    }

    #[test]
    fn all_batches_offer_everyone_and_the_running_games_grouped_by_batch() {
        crate::core::i18n::init("de");
        let list = choices(&world(), None);
        assert_eq!(labels(&list), ["Alle", "Alex", "Zed", "Berta"]);
        assert_eq!(list[0].target, Target::All);
        assert_eq!(list[1].heading.as_deref(), Some("Farm"));
        assert_eq!(list[3].heading.as_deref(), Some("Lobby"));
    }

    #[test]
    fn a_single_batch_needs_no_headings() {
        let mut m = world();
        m.batches.truncate(1);
        m.games.retain(|_, g| g.batch_id == "farm");
        assert!(choices(&m, None).iter().all(|c| c.heading.is_none()));
    }

    #[test]
    fn a_batch_offers_itself_and_only_its_own_running_games() {
        crate::core::i18n::init("de");
        let list = choices(&world(), Some("farm"));
        assert_eq!(labels(&list), ["Alle in Farm", "Alex", "Zed"]);
        assert_eq!(list[0].target, Target::Batch("farm".into()));
        assert!(list.iter().all(|c| c.heading.is_none() && c.target != Target::All));
    }

    #[test]
    fn a_batch_that_is_gone_falls_back_to_the_page_of_all() {
        assert_eq!(choices(&world(), Some("nope"))[0].target, Target::All);
    }

    #[test]
    fn a_recipient_belongs_to_the_page_it_was_picked_on() {
        let m = world();
        let (all, batch, game) = (Target::All, Target::Batch("farm".into()), Target::Instance("k4".into()));
        assert!(on_page(&m, &all, None) && !on_page(&m, &all, Some("farm")));
        assert!(on_page(&m, &batch, Some("farm")) && !on_page(&m, &batch, None) && !on_page(&m, &batch, Some("lobby")));
        assert!(on_page(&m, &game, None) && on_page(&m, &game, Some("lobby")) && !on_page(&m, &game, Some("farm")));
        assert!(!on_page(&m, &Target::Instance("gone".into()), None));
    }
}