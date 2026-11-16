use std::collections::HashMap;
use std::time::Duration;

use super::super::effect::Effect;
use super::super::event::Event;
use super::super::model::Model;
use super::super::session::Shot;
use crate::core::ipc::ToMod;
use crate::core::settings::PreviewRate;

/// How long the inputs must rest before new rates go out.
pub const SETTLE: Duration = Duration::from_millis(150);
const HOT_MS: u32 = 250;
const SHOWN_MS: u32 = 1000;
const SLOWEST_MS: u32 = 5000;
const PAUSED_MS: u32 = 30_000;
const BUDGET: u32 = 60;
const HOT_MAX: usize = 4;
const WIDTHS: [u32; 3] = [160, 320, 640];

/// The preview rate every connected game should get now.
pub fn plan(m: &Model, rate: PreviewRate) -> HashMap<String, Shot> {
    let s = &m.session;
    let width = WIDTHS.into_iter().find(|w| *w >= s.watch.width).unwrap_or(WIDTHS[2]);
    let width = if s.watch.width == 0 { WIDTHS[1] } else { width };
    let paused = Shot { interval_ms: PAUSED_MS, width };
    if s.watch.away || m.playing.is_some() {
        return s.pids.keys().map(|k| (k.clone(), paused)).collect();
    }
    let mut hot: Vec<&String> = Vec::new();
    for key in s.watch.hovered.iter().chain(&s.watch.focused) {
        if hot.len() < HOT_MAX && s.visible.contains(key) && s.pids.contains_key(key) && !hot.contains(&key) {
            hot.push(key);
        }
    }
    let (hot_ms, shown_ms) = match rate {
        PreviewRate::Saving => (HOT_MS * 2, SHOWN_MS * 2),
        PreviewRate::Normal => (HOT_MS, SHOWN_MS),
        PreviewRate::Smooth => (HOT_MS, SHOWN_MS / 2),
    };
    let left = BUDGET.saturating_sub(hot.len() as u32 * (1000 / hot_ms)).max(1);
    let shown = s.pids.keys().filter(|k| s.visible.contains(*k) && !hot.contains(k)).count() as u32;
    let shown_ms = if shown * 1000 / shown_ms > left { shown_ms.max(shown.div_ceil(left) * 1000) } else { shown_ms }.min(SLOWEST_MS);
    s.pids
        .keys()
        .map(|k| {
            let interval_ms = if hot.contains(&k) {
                hot_ms
            } else if s.visible.contains(k) {
                shown_ms
            } else {
                PAUSED_MS
            };
            (k.clone(), Shot { interval_ms, width })
        })
        .collect()
}

/// Works out the rates again after an event and, when they changed, sends them once the inputs rested for [`SETTLE`].
pub(super) fn replan(m: &mut Model, rate: PreviewRate, out: &mut Vec<Effect>) {
    let s = &mut m.session;
    s.shots.retain(|k, _| s.pids.contains_key(k));
    let next = plan(m, rate);
    let s = &mut m.session;
    if next == s.planned {
        return;
    }
    s.planned = next;
    s.shots_seq += 1;
    out.push(Effect::After { delay: SETTLE, event: Box::new(Event::ShotsDue { generation: s.shots_seq }) });
}

/// Sends the planned rates that differ from what each game was last told, when no newer change came since `generation`.
pub(super) fn due(m: &mut Model, generation: u64, out: &mut Vec<Effect>) {
    let s = &mut m.session;
    if generation != s.shots_seq {
        return;
    }
    let mut keys: Vec<&String> = s.planned.keys().collect();
    keys.sort_unstable();
    for key in keys {
        let shot = s.planned[key];
        if s.shots.get(key) != Some(&shot) {
            s.shots.insert(key.clone(), shot);
            out.push(Effect::Send { key: key.clone(), message: ToMod::Profile { preview_interval_ms: shot.interval_ms, preview_width: shot.width } });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::time::Instant;

    use super::super::super::model::{Game, Model, Phase};
    use super::super::super::session::Watch;
    use super::super::testing::{later, run, settings};
    use super::*;
    use crate::core::settings::Settings;

    fn fleet(n: usize) -> (Model, Vec<String>) {
        let mut m = Model::default();
        let keys: Vec<String> = (0..n).map(|i| format!("g{i:03}")).collect();
        for (i, key) in keys.iter().enumerate() {
            m.games.insert(key.clone(), Game { phase: Phase::Menu, ..Game::new(key.clone(), "b".into(), "i".into(), uuid::Uuid::from_u128(i as u128), key.clone()) });
            m.session.pids.insert(key.clone(), i as u32 + 1);
        }
        (m, keys)
    }

    fn show(m: &mut Model, keys: &[String]) {
        m.session.visible = keys.iter().cloned().collect::<HashSet<_>>();
    }

    fn interval(m: &Model, rate: PreviewRate, key: &str) -> u32 {
        plan(m, rate)[key].interval_ms
    }

    fn sent(fx: &[Effect]) -> Vec<(&str, u32, u32)> {
        fx.iter()
            .filter_map(|e| match e {
                Effect::Send { key, message: ToMod::Profile { preview_interval_ms, preview_width } } => Some((key.as_str(), *preview_interval_ms, *preview_width)),
                _ => None,
            })
            .collect()
    }

    fn shots(fx: &[Effect]) -> Vec<(Duration, &Event)> {
        later(fx).into_iter().filter(|(_, e)| matches!(e, Event::ShotsDue { .. })).collect()
    }

    #[test]
    fn the_hovered_tile_refreshes_fast_others_on_screen_each_second_the_rest_pauses() {
        let (mut m, keys) = fleet(5);
        show(&mut m, &keys[..3]);
        m.session.watch.hovered = Some(keys[0].clone());
        assert_eq!(interval(&m, PreviewRate::Normal, &keys[0]), 250);
        assert_eq!(interval(&m, PreviewRate::Normal, &keys[1]), 1000);
        assert_eq!(interval(&m, PreviewRate::Normal, &keys[4]), 30_000);
        m.session.watch.hovered = Some(keys[4].clone());
        assert_eq!(interval(&m, PreviewRate::Normal, &keys[4]), 30_000, "a hovered tile off screen stays paused");
    }

    #[test]
    fn selected_and_focused_tiles_count_as_hot_up_to_a_few() {
        let (mut m, keys) = fleet(10);
        show(&mut m, &keys);
        m.session.watch.focused = keys.clone();
        let hot = keys.iter().filter(|k| interval(&m, PreviewRate::Normal, k) == 250).count();
        assert_eq!(hot, HOT_MAX);
    }

    #[test]
    fn a_minimized_manager_or_a_played_game_pauses_every_preview() {
        let (mut m, keys) = fleet(3);
        show(&mut m, &keys);
        m.session.watch.hovered = Some(keys[0].clone());
        m.session.watch.away = true;
        assert!(keys.iter().all(|k| interval(&m, PreviewRate::Smooth, k) == 30_000));
        m.session.watch.away = false;
        m.playing = Some(keys[1].clone());
        assert!(keys.iter().all(|k| interval(&m, PreviewRate::Smooth, k) == 30_000));
    }

    #[test]
    fn many_tiles_on_screen_share_sixty_previews_a_second() {
        let (mut m, keys) = fleet(200);
        show(&mut m, &keys);
        m.session.watch.hovered = Some(keys[0].clone());
        let p = plan(&m, PreviewRate::Normal);
        assert_eq!(p[&keys[1]].interval_ms, 4000, "199 tiles in the 56 a second left beside the hovered one");
        let per_second: f64 = p.values().map(|s| 1000. / f64::from(s.interval_ms)).sum();
        assert!(per_second <= 60.5, "{per_second}");
        let (mut m, keys) = fleet(400);
        show(&mut m, &keys);
        assert_eq!(interval(&m, PreviewRate::Normal, &keys[0]), 5000, "never slower than one every five seconds");
        let (mut m, keys) = fleet(60);
        show(&mut m, &keys);
        assert_eq!(interval(&m, PreviewRate::Normal, &keys[0]), 1000);
    }

    #[test]
    fn the_rate_setting_halves_or_doubles_and_the_hot_tile_stays_at_four_a_second() {
        let (mut m, keys) = fleet(4);
        show(&mut m, &keys);
        m.session.watch.hovered = Some(keys[0].clone());
        assert_eq!((interval(&m, PreviewRate::Saving, &keys[0]), interval(&m, PreviewRate::Saving, &keys[1])), (500, 2000));
        assert_eq!((interval(&m, PreviewRate::Smooth, &keys[0]), interval(&m, PreviewRate::Smooth, &keys[1])), (250, 500));
        let (mut m, keys) = fleet(40);
        show(&mut m, &keys);
        assert_eq!(interval(&m, PreviewRate::Smooth, &keys[0]), 1000, "80 a second do not fit, so a second each");
    }

    #[test]
    fn capture_width_follows_the_tile_in_three_sizes() {
        let (mut m, keys) = fleet(1);
        let width = |m: &mut Model, w: u32| {
            m.session.watch.width = w;
            plan(m, PreviewRate::Normal)[&keys[0]].width
        };
        assert_eq!(width(&mut m, 0), 320);
        assert_eq!(width(&mut m, 150), 160);
        assert_eq!(width(&mut m, 161), 320);
        assert_eq!(width(&mut m, 404), 640);
        assert_eq!(width(&mut m, 2400), 640);
    }

    #[test]
    fn rates_go_out_once_the_inputs_rest_and_only_when_they_changed() {
        let s: Settings = settings();
        let (mut m, keys) = fleet(2);
        let now = Instant::now();
        let fx = run(&mut m, &s, now, Event::Visible(keys.iter().cloned().collect()));
        let waits = shots(&fx);
        assert!(matches!(waits.as_slice(), [(SETTLE, Event::ShotsDue { generation: 1 })]));
        let hover = Watch { hovered: Some(keys[0].clone()), width: 300, ..Watch::default() };
        let fx = run(&mut m, &s, now, Event::Watch(hover.clone()));
        assert!(matches!(shots(&fx).as_slice(), [(_, Event::ShotsDue { generation: 2 })]));
        assert!(sent(&run(&mut m, &s, now, Event::ShotsDue { generation: 1 })).is_empty(), "an older plan does nothing");
        let fx = run(&mut m, &s, now, Event::ShotsDue { generation: 2 });
        assert_eq!(sent(&fx), vec![(keys[0].as_str(), 250, 320), (keys[1].as_str(), 1000, 320)]);
        assert!(shots(&run(&mut m, &s, now, Event::Watch(hover))).is_empty(), "nothing changed, nothing planned");
        let away = Watch { away: true, width: 300, ..Watch::default() };
        run(&mut m, &s, now, Event::Watch(away));
        let fx = run(&mut m, &s, now, Event::ShotsDue { generation: 3 });
        assert_eq!(sent(&fx), vec![(keys[0].as_str(), 30_000, 320), (keys[1].as_str(), 30_000, 320)]);
    }

    #[test]
    fn a_game_that_connects_again_hears_its_rate_again() {
        let s = settings();
        let (mut m, keys) = fleet(1);
        let now = Instant::now();
        run(&mut m, &s, now, Event::Visible(keys.iter().cloned().collect()));
        assert_eq!(sent(&run(&mut m, &s, now, Event::ShotsDue { generation: 1 })).len(), 1);
        m.session.pids.clear();
        run(&mut m, &s, now, Event::Reports(Vec::new()));
        let fx = run(&mut m, &s, now, Event::Reports(vec![super::super::super::event::Report::Connected { key: keys[0].clone(), pid: 3 }]));
        let Some((_, Event::ShotsDue { generation })) = later(&fx).into_iter().find(|(_, e)| matches!(e, Event::ShotsDue { .. })) else { panic!("no plan after the reconnect") };
        let generation = *generation;
        assert_eq!(sent(&run(&mut m, &s, now, Event::ShotsDue { generation })), vec![(keys[0].as_str(), 1000, 320)]);
    }
}
