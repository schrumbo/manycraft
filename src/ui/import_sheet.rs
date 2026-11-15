//! The sheet that imports instances of other launchers: the launchers found with their instances, a choice among them and what to take along.

use std::collections::HashSet;
use std::path::PathBuf;

use gpui_kit::base::actions::{Cancel, Confirm};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::*;

use super::actions::ImportFromLauncher;
use super::batch_sheet::body_max;
use super::drive::Mark;
use super::motion::Soft;
use super::sheet::{self, Dismissed, Look};
use super::theme::{Theme, control, font, iconsize, radius, space, tint};
use super::widgets::{ContainScroll, KeyNav, icon};
use super::{remote, transfer};
use crate::core::drive;
use crate::core::i18n::{tr, trf};
use crate::core::launchers::{Found, Library, Take, Unsupported};

const FILTER_FROM: usize = 8;
const LIST_MIN: f32 = 160.;
const LIST_MAX: f32 = 340.;
const OPTIONS_HEIGHT: f32 = 120.;

/// Registers the action that opens the import sheet.
pub fn init(cx: &mut App) {
    cx.on_action(|_: &ImportFromLauncher, cx| {
        sheet::in_window(cx, |window, cx| {
            if sheet::busy(window, cx) {
                return;
            }
            let view = cx.new(|cx| ImportSheet::new(window, cx));
            sheet::show(view, sheet::WIDE, window, cx);
        });
    });
}

struct ImportSheet {
    libs: Vec<Library>,
    loading: bool,
    chosen: HashSet<String>,
    cursor: Option<String>,
    filter: Entity<InputState>,
    settings: bool,
    worlds: bool,
    note: Option<String>,
    scroll: ScrollHandle,
    root: FocusHandle,
    _subs: Vec<Subscription>,
}

impl EventEmitter<Dismissed> for ImportSheet {}

impl ImportSheet {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder(tr("Instanzen filtern")));
        let subs = vec![cx.subscribe(&filter, |s: &mut Self, _, e: &InputEvent, cx| {
            if matches!(e, InputEvent::Change) {
                s.cursor = s.first_key(cx);
                cx.notify();
            }
        })];
        let task = cx.background_executor().spawn(async { remote::launcher_libraries() });
        cx.spawn(async move |this, cx| {
            let libs = task.await;
            let _ = this.update(cx, |s, cx| {
                s.loading = false;
                s.libs = libs;
                let all: Vec<String> = s.libs.iter().flat_map(|l| &l.instances).filter(|i| i.importable()).map(Found::key).collect();
                if let [only] = all.as_slice() {
                    s.chosen.insert(only.clone());
                }
                s.cursor = all.first().cloned();
                cx.notify();
            });
        })
        .detach();
        Self { libs: Vec::new(), loading: true, chosen: HashSet::new(), cursor: None, filter, settings: true, worlds: false, note: None, scroll: ScrollHandle::new(), root: cx.focus_handle(), _subs: subs }
    }

    fn total(&self) -> usize {
        self.libs.iter().map(|l| l.instances.len()).sum()
    }

    fn query(&self, cx: &App) -> String {
        if self.total() > FILTER_FROM { self.filter.read(cx).value().trim().to_lowercase() } else { String::new() }
    }

    fn visible(&self, cx: &App) -> Vec<(usize, Vec<Found>)> {
        let query = self.query(cx);
        self.libs
            .iter()
            .enumerate()
            .map(|(at, l)| (at, l.instances.iter().filter(|i| query.is_empty() || i.name.to_lowercase().contains(&query)).cloned().collect::<Vec<_>>()))
            .filter(|(_, items)| !items.is_empty())
            .collect()
    }

    fn keys(&self, cx: &App) -> Vec<String> {
        self.visible(cx).into_iter().flat_map(|(_, items)| items).filter(Found::importable).map(|i| i.key()).collect()
    }

    fn first_key(&self, cx: &App) -> Option<String> {
        self.keys(cx).into_iter().next()
    }

    fn toggle(&mut self, key: &str, cx: &mut Context<Self>) {
        if !self.chosen.remove(key) {
            self.chosen.insert(key.to_owned());
        }
        self.cursor = Some(key.to_owned());
        cx.notify();
    }

    fn toggle_library(&mut self, at: usize, cx: &mut Context<Self>) {
        let keys: Vec<String> = self.visible(cx).into_iter().filter(|(l, _)| *l == at).flat_map(|(_, items)| items).filter(Found::importable).map(|i| i.key()).collect();
        if keys.iter().all(|k| self.chosen.contains(k)) {
            for key in &keys {
                self.chosen.remove(key);
            }
        } else {
            self.chosen.extend(keys);
        }
        cx.notify();
    }

    fn select_all(&mut self, cx: &mut Context<Self>) {
        let keys = self.keys(cx);
        if keys.iter().all(|k| self.chosen.contains(k)) {
            for key in &keys {
                self.chosen.remove(key);
            }
        } else {
            self.chosen.extend(keys);
        }
        cx.notify();
    }

    fn step(&mut self, delta: i32, cx: &mut Context<Self>) {
        let keys = self.keys(cx);
        if keys.is_empty() {
            return;
        }
        let at = self.cursor.as_ref().and_then(|c| keys.iter().position(|k| k == c));
        let next = match at {
            Some(at) => (at as i32 + delta).clamp(0, keys.len() as i32 - 1) as usize,
            None => 0,
        };
        self.cursor = Some(keys[next].clone());
        let flat = self.row_index(&keys[next], cx);
        self.scroll.scroll_to_item(flat);
        cx.notify();
    }

    fn row_index(&self, key: &str, cx: &App) -> usize {
        let mut at = 0;
        for (_, items) in self.visible(cx) {
            at += 1;
            for item in items {
                if item.key() == key {
                    return at;
                }
                at += 1;
            }
        }
        0
    }

    fn picked(&self) -> Vec<Found> {
        self.libs.iter().flat_map(|l| &l.instances).filter(|i| i.importable() && self.chosen.contains(&i.key())).cloned().collect()
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        let items = self.picked();
        if items.is_empty() {
            return;
        }
        transfer::import_found(items, Take { settings: self.settings, worlds: self.worlds }, cx);
        cx.emit(Dismissed);
    }

    fn choose_folder(&mut self, cx: &mut Context<Self>) {
        if drive::on() {
            if let Some(folder) = drive::answer("open folder").and_then(|p| p.into_iter().next()) {
                self.scan_folder(folder, cx);
            }
            return;
        }
        let prompt = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some(tr("Wählen").into()) });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = prompt.await
                && let Some(folder) = paths.into_iter().next()
            {
                let _ = this.update(cx, |s, cx| s.scan_folder(folder, cx));
            }
        })
        .detach();
    }

    fn scan_folder(&mut self, folder: PathBuf, cx: &mut Context<Self>) {
        let task = cx.background_executor().spawn(async move { remote::launcher_folder(&folder) });
        cx.spawn(async move |this, cx| {
            let found = task.await;
            let _ = this.update(cx, |s, cx| {
                s.loading = false;
                let fresh: Vec<String> = found.iter().flat_map(|l| &l.instances).filter(|i| i.importable()).map(Found::key).collect();
                s.note = found.is_empty().then(|| tr("In diesem Ordner liegt keine Instanz, die sich übernehmen lässt."));
                for lib in found {
                    match s.libs.iter_mut().find(|l| l.launcher == lib.launcher && l.root == lib.root) {
                        Some(have) => *have = lib,
                        None => s.libs.push(lib),
                    }
                }
                s.chosen.extend(fresh.iter().cloned());
                s.cursor = fresh.first().cloned().or_else(|| s.cursor.take());
                cx.notify();
            });
        })
        .detach();
    }

    fn reason(why: &Unsupported) -> String {
        match why {
            Unsupported::NoVersion => tr("Ohne Minecraft-Version."),
            Unsupported::Loader(loader) => trf("Der Mod-Loader %@ wird nicht unterstützt.", &[&loader]),
            Unsupported::Unreadable => tr("Die Instanz ist unvollständig oder beschädigt."),
        }
    }

    fn subtitle(found: &Found) -> String {
        let line = found.version_line();
        match found.mods {
            0 => line,
            1 => format!("{line} · {}", tr("1 Mod")),
            n => format!("{line} · {}", trf("%lld Mods", &[&n])),
        }
    }

    fn render_row(&self, found: &Found, cx: &mut Context<Self>, t: &Theme) -> AnyElement {
        let key = found.key();
        let reason = found.unsupported.as_ref().map(Self::reason);
        let chosen = self.chosen.contains(&key);
        let at_cursor = self.cursor.as_deref() == Some(key.as_str());
        let subtitle = reason.clone().unwrap_or_else(|| Self::subtitle(found));
        let row = div()
            .id(SharedString::from(format!("launcher-row-{key}")))
            .mark("row", found.name.clone())
            .w_full()
            .px(px(space::M))
            .py(px(space::S))
            .flex()
            .items_center()
            .gap(px(space::M))
            .rounded(px(radius::CONTROL))
            .when(at_cursor, |d| d.bg(t.ink.opacity(tint::HOVER)));
        let text = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(space::XXS))
            .child(div().truncate().text_size(px(font::BODY)).font_weight(FontWeight::SEMIBOLD).child(found.name.clone()))
            .child(div().truncate().text_size(px(font::CALLOUT)).text_color(t.secondary).child(subtitle));
        if let Some(why) = reason {
            return sheet::unavailable_over(SharedString::from(format!("launcher-row-locked-{key}")), row.opacity(tint::DISABLED).child(icon("circle-alert", iconsize::M, t.tertiary)).child(text), why).into_any_element();
        }
        let toggled = key.clone();
        let clicked = key.clone();
        row.cursor_pointer()
            .soft(t.ink.opacity(tint::HOVER), radius::CONTROL)
            .on_click(cx.listener(move |s, _, _, cx| s.toggle(&clicked, cx)))
            .child(Checkbox::new(SharedString::from(format!("launcher-check-{key}"))).checked(chosen).tab_stop(false).on_click(cx.listener(move |s, _: &bool, _, cx| {
                s.toggle(&toggled, cx);
                cx.stop_propagation();
            })))
            .child(text)
            .into_any_element()
    }

    fn render_list(&mut self, cx: &mut Context<Self>, t: &Theme, max: f32) -> Div {
        let shown = self.visible(cx);
        if shown.is_empty() {
            let query = self.filter.read(cx).value().trim().to_string();
            let line = if self.total() == 0 {
                tr("Kein anderer Launcher gefunden. Wähle den Ordner eines Launchers oder einer Instanz.")
            } else {
                trf("Keine Instanz passt zu „%@“. Lösche den Filter oder suche anders.", &[&query])
            };
            return div().py(px(space::L)).text_size(px(font::CALLOUT)).text_color(t.secondary).child(line);
        }
        let mut list = div().id("launcher-list").flex().flex_col().gap(px(space::XXS)).max_h(px(max)).overflow_y_scroll().track_scroll(&self.scroll).contain_scroll(&self.scroll);
        for (at, items) in shown {
            let all = items.iter().filter(|i| i.importable()).all(|i| self.chosen.contains(&i.key()));
            let any = items.iter().any(Found::importable);
            let name = self.libs[at].launcher.name();
            let head = div()
                .px(px(space::M))
                .pt(px(space::S))
                .flex()
                .items_center()
                .gap(px(space::S))
                .child(div().flex_1().min_w_0().truncate().text_size(px(font::CALLOUT)).font_weight(FontWeight::SEMIBOLD).text_color(t.secondary).child(name))
                .when(any, |d| {
                    d.child(
                        sheet::button(SharedString::from(format!("launcher-all-{at}")), if all { tr("Keine") } else { tr("Alle") }, None, Look::Plain, false, false, t)
                            .h(px(control::S))
                            .on_click(cx.listener(move |s, _, _, cx| s.toggle_library(at, cx))),
                    )
                });
            list = list.child(head);
            for item in &items {
                list = list.child(self.render_row(item, cx, t));
            }
        }
        div().child(list)
    }

    fn option(&self, id: &'static str, label: String, on: bool, flip: fn(&mut Self), cx: &mut Context<Self>, t: &Theme) -> Stateful<Div> {
        div()
            .id(id)
            .mark("checkbox", label.clone())
            .self_start()
            .h(px(control::S))
            .px(px(space::XS))
            .ml(px(-space::XS))
            .flex()
            .items_center()
            .gap(px(space::XS))
            .rounded(px(radius::INNER))
            .cursor_pointer()
            .key_nav()
            .soft(t.ink.opacity(tint::HOVER), radius::INNER)
            .on_click(cx.listener(move |s, _, _, cx| {
                flip(s);
                cx.notify();
            }))
            .child(Checkbox::new(SharedString::from(format!("{id}-check"))).checked(on).tab_stop(false).on_click(cx.listener(move |s, _: &bool, _, cx| {
                flip(s);
                cx.notify();
                cx.stop_propagation();
            })))
            .child(div().text_size(px(font::BODY)).child(label))
    }
}

impl Render for ImportSheet {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if window.focused(cx).is_none() {
            window.focus(&self.root, cx);
        }
        let t = Theme::get(cx);
        let max = (body_max(window.viewport_size().height.as_f32()) - OPTIONS_HEIGHT).clamp(LIST_MIN, LIST_MAX);
        let count = self.picked().len();
        let body = if self.loading { sheet::waiting(tr("Suche Launcher …"), &t).py(px(space::L)) } else { self.render_list(cx, &t, max) };
        let filter = (self.total() > FILTER_FROM).then(|| Input::new(&self.filter).prefix(icon("search", iconsize::S, t.tertiary)).cleanable(true));
        let note = self.note.clone().map(|n| sheet::status("circle-alert", t.attention_text, n));
        let options = div()
            .px(px(space::XL))
            .pt(px(space::M))
            .pb(px(space::L))
            .flex()
            .flex_col()
            .gap(px(space::XS))
            .child(sheet::label(tr("Mitnehmen"), &t))
            .child(self.option("take-settings", tr("Einstellungen und Ressourcenpakete"), self.settings, |s| s.settings = !s.settings, cx, &t))
            .child(self.option("take-worlds", tr("Welten"), self.worlds, |s| s.worlds = !s.worlds, cx, &t))
            .child(sheet::note(self.worlds.then(|| tr("Die Welten landen im Spielordner des Accounts, der die Instanz zuerst startet.")), None, &t));
        let label = match count {
            0 => tr("Importieren"),
            1 => tr("Instanz importieren"),
            n => trf("%lld Instanzen importieren", &[&n]),
        };
        let leading = vec![sheet::button("launcher-folder", tr("Ordner wählen…"), Some("folder-open"), Look::Secondary, false, false, &t).on_click(cx.listener(|s, _, _, cx| s.choose_folder(cx))).into_any_element()];
        let trailing = vec![
            sheet::button("launcher-cancel", tr("Abbrechen"), None, Look::Secondary, false, false, &t).on_click(cx.listener(|_, _, _, cx| cx.emit(Dismissed))).into_any_element(),
            sheet::button_unless("launcher-import", label, None, Look::Primary, (count == 0).then(|| tr("Wähle mindestens eine Instanz.").into()), false, &t).on_click(cx.listener(|s, _, _, cx| s.start(cx))).into_any_element(),
        ];
        let me = cx.entity().downgrade();
        div()
            .id("import-sheet")
            .track_focus(&self.root)
            .capture_key_down(move |e: &KeyDownEvent, window, cx| {
                let key = e.keystroke.key.as_str();
                let typing = sheet::typing(window);
                let m = e.keystroke.modifiers;
                let handled = me.update(cx, |s, cx| match key {
                    "up" | "down" if !m.modified() => {
                        s.step(if key == "up" { -1 } else { 1 }, cx);
                        true
                    }
                    "space" if !typing && !m.modified() => {
                        if let Some(at) = s.cursor.clone() {
                            s.toggle(&at, cx);
                        }
                        true
                    }
                    "a" if !typing && (if cfg!(target_os = "macos") { m.platform } else { m.control }) && !m.shift && !m.alt => {
                        s.select_all(cx);
                        true
                    }
                    _ => false,
                });
                if handled.unwrap_or(false) {
                    cx.stop_propagation();
                }
            })
            .w_full()
            .flex()
            .flex_col()
            .text_color(t.primary)
            .on_action(cx.listener(|s, _: &Confirm, _, cx| s.start(cx)))
            .on_action(cx.listener(|_, _: &Cancel, _, cx| cx.propagate()))
            .child(sheet::header(tr("Aus anderem Launcher importieren"), Some(tr("Kopiert Instanzen. Der andere Launcher bleibt unverändert, Accounts werden nicht übernommen.")), &t))
            .child(div().px(px(space::XL)).flex().flex_col().gap(px(space::S)).children(filter).children(note).child(body))
            .child(options)
            .child(sheet::footer(leading, trailing, &t))
    }
}

#[cfg(test)]
mod tests {
    use super::ImportSheet;
    use crate::core::instance::Loader;
    use crate::core::launchers::{Found, Launcher, Unsupported};

    fn found(loader: Loader, mods: usize) -> Found {
        Found { launcher: Launcher::Prism, id: "a".into(), name: "A".into(), game_dir: "x".into(), game_version: Some("1.21.1".into()), loader, mods, unsupported: None }
    }

    #[test]
    fn the_subtitle_names_version_loader_and_mod_count() {
        assert_eq!(ImportSheet::subtitle(&found(Loader::Fabric(Some("0.16.5".into())), 12)), "Minecraft 1.21.1 · Fabric 0.16.5 · 12 Mods");
        assert_eq!(ImportSheet::subtitle(&found(Loader::Vanilla, 0)), "Minecraft 1.21.1");
        assert_eq!(ImportSheet::subtitle(&found(Loader::NeoForge(None), 1)), "Minecraft 1.21.1 · NeoForge · 1 Mod");
    }

    #[test]
    fn every_unsupported_reason_has_a_text() {
        for why in [Unsupported::NoVersion, Unsupported::Loader("liteloader".into()), Unsupported::Unreadable] {
            assert!(!ImportSheet::reason(&why).is_empty());
        }
    }
}
