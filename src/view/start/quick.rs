use freya::prelude::*;

use crate::app::event::Event as AppEvent;
use crate::core::i18n::{tr, trf};
use crate::core::instance::{Instance, Loader, free_name};
use crate::core::versions::{self, GameVersion};
use crate::ui::forms;
use crate::ui::remote::{self, InstanceEdit};
use crate::view::commands::{Want, want, want_blocked};
use crate::view::icons::Icon;
use crate::view::kit::button::Button;
use crate::view::kit::select::Select;
use crate::view::kit::text::{caption, strong};
use crate::view::kit::{off_ui, toast, use_theme};
use crate::view::library::{Infos, QUICK};
use crate::view::store::{Store, use_store};
use crate::view::theme::{font, frame, space};

#[derive(Clone, PartialEq)]
enum Versions {
    Loading,
    Ready(Vec<String>),
    Failed,
}

fn is_quick(i: &Instance) -> bool {
    i.loader == Loader::Vanilla && (i.group.as_deref() == Some(QUICK) || i.name.trim().eq_ignore_ascii_case(&format!("Minecraft {}", i.game_version)))
}

/// The instance made by Schnell spielen for plain `version`, wherever the person moved it, when it exists already.
pub fn existing<'a>(list: &'a [Instance], version: &str) -> Option<&'a Instance> {
    list.iter().find(|i| is_quick(i) && i.game_version == version)
}

/// The version Schnell spielen offers first: the one of its instances played last, else the newest of `versions`.
pub fn default_version(list: &[Instance], infos: &Infos, versions: &[String]) -> Option<String> {
    let last = list.iter().filter(|i| is_quick(i) && versions.contains(&i.game_version)).filter_map(|i| infos.get(&i.id).and_then(|n| n.played).map(|at| (i, at))).max_by_key(|(_, at)| *at);
    last.map(|(i, _)| i.game_version.clone()).or_else(|| versions.first().cloned())
}

fn releases(list: &[GameVersion]) -> Vec<String> {
    versions::shown(list, false, None, "").into_iter().map(|v| v.id).collect()
}

fn play(store: Store, version: String, busy: State<bool>) {
    let mut busy = busy;
    let found = existing(&store.model.peek().instances, &version).map(|i| i.id.clone());
    if let Some(found) = found {
        store.dispatch(AppEvent::PlayInstance { instance: found, account: None });
        return;
    }
    if *busy.peek() {
        return;
    }
    busy.set(true);
    let names: Vec<String> = store.model.peek().instances.iter().map(|i| i.name.clone()).collect();
    spawn_in_window(async move {
        let edit = InstanceEdit { name: free_name(&format!("Minecraft {version}"), &names), game_version: version, loader: Loader::Vanilla, memory_mb: None, jvm_args: Vec::new(), options: Default::default(), mods: Vec::new() };
        let made = off_ui(move || remote::create_instance(edit)).await;
        if busy.try_peek().is_some() {
            busy.set(false);
        }
        match made {
            Some(Ok(made)) => {
                let id = made.id.clone();
                if store.model.peek().instance(&id).is_none() {
                    store.dispatch(AppEvent::InstanceAdded(made));
                }
                store.dispatch(AppEvent::SetGroup { instance: id.clone(), group: Some(QUICK.to_owned()) });
                store.dispatch(AppEvent::PlayInstance { instance: id, account: None });
            }
            Some(Err(e)) => {
                toast::show(store.ui, forms::explain_as(tr("Nicht angelegt."), &e).text, None);
            }
            None => {
                toast::show(store.ui, forms::unexplained(), None);
            }
        }
    });
}

/// Starts plain Minecraft in the version picked, without making the person set an instance up.
#[derive(PartialEq)]
pub struct QuickPlay {
    pub infos: Infos,
}

impl Component for QuickPlay {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let mut versions = use_state(|| Versions::Loading);
        let mut tries = use_state(|| 0u32);
        let mut chosen = use_state(|| None::<String>);
        let busy = use_state(|| false);
        use_side_effect_with_deps(&*tries.read(), move |_| {
            spawn_in_window(async move {
                let next = match off_ui(remote::catalog).await {
                    Some(Ok(list)) if !list.is_empty() => Versions::Ready(releases(&list)),
                    _ => Versions::Failed,
                };
                if versions.try_peek().is_some() {
                    versions.set(next);
                }
            });
        });
        let m = store.model.read();
        let accounts = m.accounts.len();
        let list = m.instances.clone();
        drop(m);
        let mut body = rect().width(Size::flex(1.)).spacing(space::XXS).child(strong(&t, tr("Schnell spielen"), font::BODY, t.text1)).child(caption(&t, tr("Minecraft ohne Mods, ohne Einrichtung.")));
        let mut actions = rect().horizontal().cross_align(Alignment::Center).spacing(space::S);
        match versions.read().clone() {
            Versions::Loading => {
                actions = actions
                    .child(Select::new("start.quick.version", tr("Minecraft-Version"), vec![tr("Lädt …")], 0).width(frame::QUICK_SELECT).disabled(Some(tr("Die Versionen werden geladen."))))
                    .child(Button::new("start.quick.play", tr("Spielen")).glyph(Icon::Play).primary().tall().busy(true).disabled(Some(tr("Die Versionen werden geladen."))));
            }
            Versions::Failed => {
                body = body.child(caption(&t, tr("Versionen nicht geladen. Prüfe die Verbindung.")).color(t.attention_text));
                actions = actions.child(Button::new("start.quick.retry", tr("Erneut versuchen")).glyph(Icon::Refresh).tall().on_press(move |_| {
                    versions.set(Versions::Loading);
                    *tries.write() += 1;
                }));
            }
            Versions::Ready(ids) => {
                let picked = chosen.read().clone().filter(|v| ids.contains(v)).or_else(|| default_version(&list, &self.infos, &ids)).unwrap_or_default();
                let at = ids.iter().position(|v| *v == picked).unwrap_or(0);
                let options = ids.clone();
                actions = actions.child(Select::new("start.quick.version", tr("Minecraft-Version"), ids, at).width(frame::QUICK_SELECT).on_choose(move |i| chosen.set(options.get(i).cloned())));
                actions = if accounts == 0 {
                    actions.child(Button::new("start.quick.play", tr("Account hinzufügen")).glyph(Icon::UserPlus).primary().tall().disabled(want_blocked(&Want::AddAccount)).on_press(move |_| {
                        want(store, Want::AddAccount);
                    }))
                } else {
                    let tip = trf("Minecraft %@ ohne Mods starten", &[&picked]);
                    actions.child(Button::new("start.quick.play", tr("Spielen")).glyph(Icon::Play).primary().tall().busy(*busy.read()).tip(tip).disabled(busy.read().then(|| tr("Die Instanz wird angelegt."))).on_press(move |_| play(store, picked.clone(), busy)))
                };
            }
        }
        rect().width(Size::fill()).padding((space::M, space::L)).horizontal().cross_align(Alignment::Center).spacing(space::L).background(t.raised).corner_radius(t.shape.tile).child(body).content(Content::flex()).child(actions)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::rc::Rc;
    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::view::library::Info;

    fn quick(version: &str) -> Instance {
        let mut i = Instance::new(&format!("Minecraft {version}"), version, Loader::Vanilla);
        i.group = Some(QUICK.to_owned());
        i
    }

    #[test]
    fn only_a_plain_instance_of_the_quick_group_is_reused() {
        let mut modded = quick("1.21.4");
        modded.loader = Loader::Fabric(None);
        let mut renamed = quick("1.21.4");
        renamed.name = "Meine Welt".into();
        renamed.group = None;
        let mut moved = quick("1.21.4");
        moved.group = Some("Server".into());
        let list = vec![modded, renamed, moved.clone()];
        assert_eq!(existing(&list, "1.21.4").map(|i| i.id.clone()), Some(moved.id));
        assert!(existing(&list, "1.20.1").is_none());
    }

    #[test]
    fn the_last_played_quick_version_comes_first_else_the_newest() {
        let (a, b) = (quick("1.20.1"), quick("1.21.4"));
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let map: HashMap<String, Info> = [(a.id.clone(), Info { played: Some(base + Duration::from_secs(9)), made: None }), (b.id.clone(), Info { played: Some(base), made: None })].into();
        let infos: Infos = Rc::new(map);
        let versions = vec!["26.3".to_owned(), "1.21.4".to_owned(), "1.20.1".to_owned()];
        assert_eq!(default_version(&[a, b], &infos, &versions).as_deref(), Some("1.20.1"));
        assert_eq!(default_version(&[], &infos, &versions).as_deref(), Some("26.3"));
        assert_eq!(default_version(&[quick("1.20.1")], &Rc::new(HashMap::new()), &versions).as_deref(), Some("26.3"));
        assert_eq!(default_version(&[], &infos, &[]), None);
    }
}
