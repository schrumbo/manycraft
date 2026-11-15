//! The page of one project in the content browser: description, gallery, versions and what it needs.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::SystemTime;

use gpui_kit::component::text::TextView;
use gpui_kit::prelude::*;
use gpui_kit::*;

use super::batch_sheet::Load;
use super::catalog;
use super::drive::Mark;
use super::forms::{self, Problem};
use super::instance_sheet::InstanceSheet;
use super::mod_browser::{saved_when, skeleton_row};
use super::sheet::{self, Look};
use super::theme::{Theme, font, iconsize, radius, space, tint};
use super::widgets::{TakeFocus, icon, segment, segmented};
use crate::core::packs;
use crate::core::i18n::{Lang, lang, tr, trf};
use crate::core::modrinth::{self, Details, Hit, Kind, Version};

const SHOT_HEIGHT: f32 = 270.;
const MAX_VERSIONS: usize = 60;
const GAME_VERSIONS_SHOWN: usize = 4;
const VERSION_ROW: f32 = 44.;
const SKELETON_LINES: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    About,
    Gallery,
    Versions,
    Needs,
}

const SECTIONS: [Section; 4] = [Section::About, Section::Gallery, Section::Versions, Section::Needs];

const LINE_STEP: f32 = 40.;

/// One open project page: what was loaded for it and which part is shown.
pub(super) struct Project {
    hit: Hit,
    kind: Kind,
    section: Section,
    details: Load<(Details, SharedString)>,
    saved: Option<SystemTime>,
    versions: Load<Vec<Version>>,
    titles: HashMap<String, String>,
    shots: HashMap<String, Load<PathBuf>>,
    scroll: ScrollHandle,
}

fn section_name(section: Section) -> String {
    match section {
        Section::About => tr("Beschreibung"),
        Section::Gallery => tr("Galerie"),
        Section::Versions => tr("Versionen"),
        Section::Needs => tr("Abhängigkeiten"),
    }
}

fn day(iso: &str) -> String {
    let Ok(at) = chrono::DateTime::parse_from_rfc3339(iso) else { return iso.chars().take(10).collect() };
    let at = at.with_timezone(&chrono::Local);
    if lang() == Lang::En { at.format("%b %-d, %Y").to_string() } else { at.format("%-d.%-m.%Y").to_string() }
}

fn problem_of(e: &anyhow::Error) -> Problem {
    if super::remote::is_network(e) {
        Problem { text: tr("Keine Verbindung zu Modrinth. Prüf dein Internet und versuch es nochmal."), detail: Some(format!("{e:#}")) }
    } else {
        forms::explain_as(tr("Die Seite ließ sich nicht laden."), e)
    }
}

impl InstanceSheet {
    pub(super) fn open_project(&mut self, hit: Hit, window: &mut Window, cx: &mut Context<Self>) {
        let kind = self.browser.kind;
        self.browser.project = Some(Project { hit: hit.clone(), kind, section: Section::About, details: Load::Loading, saved: None, versions: Load::Loading, titles: HashMap::new(), shots: HashMap::new(), scroll: ScrollHandle::new() });
        self.browser.query.update(cx, |q, cx| q.take_focus(window, cx));
        self.project_load(cx);
        cx.notify();
    }

    fn project_load(&mut self, cx: &mut Context<Self>) {
        let Some(page) = &mut self.browser.project else { return };
        page.details = Load::Loading;
        page.versions = Load::Loading;
        let (id, kind, version) = (page.hit.project_id.clone(), page.kind, self.version.clone());
        let asked = id.clone();
        let details = cx.background_executor().spawn(async move { catalog::details(&asked).map(|(d, saved)| (SharedString::from(packs::readable(&d.body)), d, saved)) });
        let asked = id.clone();
        let versions = cx.background_executor().spawn(async move { catalog::versions(&asked, kind, &version) });
        cx.spawn(async move |this, cx| {
            let details = details.await;
            let _ = this.update(cx, |s, cx| {
                if let Some(page) = s.browser.project.as_mut().filter(|p| p.hit.project_id == id) {
                    page.details = match details {
                        Ok((text, d, saved)) => {
                            page.saved = saved;
                            Load::Ready((d, text))
                        }
                        Err(e) => Load::Failed(problem_of(&e)),
                    };
                    if page.section == Section::Gallery {
                        s.project_shots(cx);
                    }
                }
                cx.notify();
            });
            let versions = versions.await;
            let wanted = this.update(cx, |s, cx| {
                let mut wanted = Vec::new();
                if let Some(page) = s.browser.project.as_mut().filter(|p| p.hit.project_id == id) {
                    page.versions = match versions {
                        Ok((list, _)) => {
                            wanted = list.iter().flat_map(|v| v.dependencies.iter().filter_map(|d| d.project_id.clone())).collect();
                            wanted.sort();
                            wanted.dedup();
                            Load::Ready(list)
                        }
                        Err(e) => Load::Failed(problem_of(&e)),
                    };
                }
                cx.notify();
                wanted
            });
            let Ok(wanted) = wanted else { return };
            if wanted.is_empty() {
                return;
            }
            let titles = cx.background_executor().spawn(async move { catalog::titles(&wanted).unwrap_or_default() }).await;
            let _ = this.update(cx, |s, cx| {
                if let Some(page) = s.browser.project.as_mut().filter(|p| p.hit.project_id == id) {
                    page.titles = titles;
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn project_shots(&mut self, cx: &mut Context<Self>) {
        let Some(page) = &mut self.browser.project else { return };
        let Load::Ready((details, _)) = &page.details else { return };
        let urls: Vec<String> = details.gallery.iter().map(|g| g.url.clone()).filter(|u| !page.shots.contains_key(u)).collect();
        for url in &urls {
            page.shots.insert(url.clone(), Load::Loading);
        }
        let id = page.hit.project_id.clone();
        for url in urls {
            let asked = url.clone();
            let task = cx.background_executor().spawn(async move { catalog::shot(&asked) });
            let id = id.clone();
            cx.spawn(async move |this, cx| {
                let result = task.await;
                let _ = this.update(cx, |s, cx| {
                    if let Some(page) = s.browser.project.as_mut().filter(|p| p.hit.project_id == id) {
                        page.shots.insert(url, result.map_or_else(|e| Load::Failed(forms::explain_as(tr("Das Bild ließ sich nicht laden."), &e)), Load::Ready));
                    }
                    cx.notify();
                });
            })
            .detach();
        }
    }

    /// Scrolls the open project page by `lines`, or by `pages` of its visible height.
    pub(super) fn project_scroll(&mut self, lines: f32, pages: f32, cx: &mut Context<Self>) {
        let Some(page) = &self.browser.project else { return };
        let step = lines * LINE_STEP + pages * (page.scroll.bounds().size.height.as_f32() - LINE_STEP).max(LINE_STEP);
        let (at, max) = (page.scroll.offset(), page.scroll.max_offset());
        page.scroll.set_offset(point(at.x, (at.y - px(step)).clamp(-max.y, px(0.))));
        cx.notify();
    }

    pub(super) fn close_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.browser.project = None;
        self.browse_focus(window, cx);
        cx.notify();
    }

    pub(super) fn project_primary(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(hit) = self.browser.project.as_ref().map(|p| p.hit.clone()) else { return };
        let act = self.browse_act(&hit);
        self.browse_run(hit, act, cx);
        self.refocus_browser(window, cx);
    }

    fn project_section(&mut self, section: Section, cx: &mut Context<Self>) {
        let Some(page) = &mut self.browser.project else { return };
        page.section = section;
        page.scroll.set_offset(point(px(0.), px(0.)));
        if section == Section::Gallery {
            self.project_shots(cx);
        }
        cx.notify();
    }

    pub(super) fn render_project(&mut self, cx: &mut Context<Self>, t: &Theme) -> AnyElement {
        let Some(page) = &self.browser.project else { return div().into_any_element() };
        let hit = page.hit.clone();
        let section = page.section;
        let face = self.face(&hit.project_id, t);
        let (downloads, updated, saved) = match &page.details {
            Load::Ready((d, _)) => (d.downloads, Some(day(&d.updated)), page.saved),
            _ => (hit.downloads, None, None),
        };
        let mut facts = vec![trf("%@ Downloads", &[&forms::downloads_short(downloads)])];
        if let Some(when) = updated {
            facts.push(trf("Aktualisiert am %@", &[&when]));
        }
        if let Some(at) = saved {
            facts.push(trf("Gespeichert um %@", &[&saved_when(at)]));
        }
        let author = (!hit.author.is_empty()).then(|| div().min_w_0().truncate().text_size(px(font::CALLOUT)).text_color(t.secondary).child(trf("von %@", &[&hit.author])));
        let heading = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(space::XXS))
            .child(div().flex().items_baseline().gap(px(space::S)).child(div().min_w_0().truncate().text_size(px(font::TITLE3)).font_weight(FontWeight::SEMIBOLD).child(hit.title.clone())).children(author))
            .child(div().truncate().text_size(px(font::CAPTION)).text_color(t.tertiary).child(facts.join(" · ")));
        let trailing = self.browse_trailing(None, &hit, t, cx);
        let back = sheet::button("project-back", tr("Ergebnisse"), Some("chevron-left"), Look::Plain, false, false, t).tooltip(super::widgets::tip(trf("Zurück zu den Ergebnissen · %@", &[&super::mod_browser::keys("escape", false)]))).on_click(cx.listener(|s, _, window, cx| s.close_project(window, cx)));
        let segments: Vec<Stateful<Div>> = SECTIONS.iter().map(|s| segment(SharedString::from(format!("project-{}", section_name(*s))), section_name(*s), *s == section, true, t)).collect();
        let pick = cx.listener(|s, n: &usize, _, cx| s.project_section(SECTIONS[*n], cx));
        let sections = segmented("project-sections", segments, move |n, window, cx| pick(&n, window, cx), t).w_full();
        let body = match section {
            Section::About => self.render_about(cx, t),
            Section::Gallery => self.render_gallery(t),
            Section::Versions => self.render_versions(cx, t),
            Section::Needs => self.render_needs(cx, t),
        };
        let scroll = self.browser.project.as_ref().map(|p| p.scroll.clone()).unwrap_or_default();
        div()
            .id("project-page")
            .mark("project", hit.title.clone())
            .size_full()
            .flex()
            .flex_col()
            .child(div().px(px(space::XS)).pt(px(space::XS)).flex().child(back))
            .child(div().px(px(space::M)).py(px(space::S)).flex().items_center().gap(px(space::M)).child(face).child(heading).child(trailing))
            .child(div().px(px(space::M)).pb(px(space::S)).child(sections))
            .child(div().h(px(1.)).bg(t.separator))
            .child(div().id("project-body").track_scroll(&scroll).flex_1().min_h_0().overflow_y_scroll().px(px(space::M)).py(px(space::M)).child(body))
            .into_any_element()
    }

    fn loading_lines(t: &Theme) -> AnyElement {
        div().flex().flex_col().gap(px(space::M)).children((0..SKELETON_LINES).map(|_| skeleton_row(t))).into_any_element()
    }

    fn render_failed(&mut self, key: &'static str, why: &Problem, cx: &mut Context<Self>, t: &Theme) -> AnyElement {
        div()
            .flex()
            .items_center()
            .gap(px(space::S))
            .child(div().flex_1().min_w_0().child(self.problem(key, "wifi-off", t.danger_text, why, cx, t)))
            .child(sheet::button(SharedString::from(format!("{key}-retry")), tr("Erneut versuchen"), Some("refresh-cw"), Look::Secondary, false, false, t).on_click(cx.listener(|s, _, _, cx| s.project_load(cx))))
            .into_any_element()
    }

    fn render_about(&mut self, cx: &mut Context<Self>, t: &Theme) -> AnyElement {
        let Some(page) = &self.browser.project else { return div().into_any_element() };
        match &page.details {
            Load::Loading => Self::loading_lines(t),
            Load::Failed(why) => {
                let why = why.clone();
                self.render_failed("project-error", &why, cx, t)
            }
            Load::Ready((d, text)) if text.trim().is_empty() => quiet(if d.description.is_empty() { tr("Keine Beschreibung.") } else { d.description.clone() }, t),
            Load::Ready((d, text)) => div()
                .flex()
                .flex_col()
                .gap(px(space::M))
                .text_size(px(font::BODY))
                .text_color(t.primary)
                .child(div().text_color(t.secondary).child(d.description.clone()))
                .child(TextView::markdown(SharedString::from(format!("project-text-{}", d.id)), text.clone()).selectable(true))
                .into_any_element(),
        }
    }

    fn render_gallery(&mut self, t: &Theme) -> AnyElement {
        let Some(page) = &self.browser.project else { return div().into_any_element() };
        let Load::Ready((details, _)) = &page.details else { return Self::loading_lines(t) };
        if details.gallery.is_empty() {
            return quiet(tr("Zu diesem Projekt gibt es keine Bilder."), t);
        }
        let mut shots = details.gallery.clone();
        shots.sort_by_key(|s| (!s.featured, s.ordering));
        let icons = self.browser.icons.clone();
        div()
            .flex()
            .flex_col()
            .gap(px(space::L))
            .children(shots.into_iter().map(|shot| {
                let frame = div().w_full().h(px(SHOT_HEIGHT)).rounded(px(radius::CONTROL)).overflow_hidden().bg(t.raised).flex().items_center().justify_center();
                let frame = match page.shots.get(&shot.url) {
                    Some(Load::Ready(path)) => frame.child(img(path.clone()).image_cache(&icons).size_full().object_fit(ObjectFit::Contain)),
                    Some(Load::Failed(_)) => frame.child(icon("image-off", iconsize::L, t.tertiary)),
                    _ => frame.bg(t.ink.opacity(tint::HOVER)),
                };
                let caption = shot.title.clone().filter(|c| !c.trim().is_empty()).map(|c| div().text_size(px(font::CALLOUT)).text_color(t.secondary).child(c));
                div().flex().flex_col().gap(px(space::XS)).child(frame).children(caption)
            }))
            .into_any_element()
    }

    fn render_versions(&mut self, cx: &mut Context<Self>, t: &Theme) -> AnyElement {
        let Some(page) = &self.browser.project else { return div().into_any_element() };
        let list = match &page.versions {
            Load::Loading => return Self::loading_lines(t),
            Load::Failed(why) => {
                let why = why.clone();
                return self.render_failed("versions-error", &why, cx, t);
            }
            Load::Ready(list) => list.clone(),
        };
        if list.is_empty() {
            return quiet(tr("Von diesem Projekt gibt es keine Versionen."), t);
        }
        let filter = self.browse_filter();
        let chosen = modrinth::newest_for(&list, &filter).map(|v| v.id.clone());
        let have = self.installed_version(&page.hit.project_id);
        let rest = list.len().saturating_sub(MAX_VERSIONS);
        let rows = list.into_iter().take(MAX_VERSIONS).map(|v| {
            let badge = match v.version_type.as_str() {
                "beta" => Some(tr("Beta")),
                "alpha" => Some(tr("Alpha")),
                _ => None,
            };
            let mut games: Vec<String> = v.game_versions.iter().rev().take(GAME_VERSIONS_SHOWN).cloned().collect();
            if v.game_versions.len() > GAME_VERSIONS_SHOWN {
                games.push("…".to_owned());
            }
            let detail = format!("{} · {} · {}", day(&v.date_published), games.join(", "), v.loaders.join(", "));
            let mark = if have.as_ref() == Some(&v.id) {
                Some(sheet::status("check", t.success_text, tr("Installiert")))
            } else if chosen.as_ref() == Some(&v.id) {
                Some(sheet::status("arrow-down", t.secondary, if have.is_some() { tr("Neueste passende") } else { tr("Wird installiert") }))
            } else {
                None
            };
            div()
                .h(px(VERSION_ROW))
                .flex()
                .items_center()
                .gap(px(space::S))
                .border_b_1()
                .border_color(t.separator)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().flex().items_center().gap(px(space::S)).child(div().min_w_0().truncate().text_size(px(font::BODY)).font_weight(FontWeight::MEDIUM).child(v.version_number.clone())).children(badge.map(|b| div().px(px(space::XS)).rounded(px(radius::SMALL)).bg(t.ink.opacity(tint::HOVER)).text_size(px(font::CAPTION)).text_color(t.secondary).child(b))))
                        .child(div().truncate().text_size(px(font::CAPTION)).text_color(t.tertiary).child(detail)),
                )
                .children(mark)
        });
        let link = format!("https://modrinth.com/{}/{}/versions", page.kind.project_type(), page.hit.slug);
        let more = (rest > 0).then(|| {
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(space::M))
                .child(quiet(trf("und %lld ältere", &[&rest]), t))
                .child(sheet::button("project-all-versions", tr("Alle auf Modrinth"), Some("external-link"), Look::Secondary, false, false, t).on_click(move |_, _, cx| cx.open_url(&link)))
        });
        div().flex().flex_col().children(rows).children(more).into_any_element()
    }

    fn render_needs(&mut self, cx: &mut Context<Self>, t: &Theme) -> AnyElement {
        let Some(page) = &self.browser.project else { return div().into_any_element() };
        let list = match &page.versions {
            Load::Loading => return Self::loading_lines(t),
            Load::Failed(why) => {
                let why = why.clone();
                return self.render_failed("needs-error", &why, cx, t);
            }
            Load::Ready(list) => list,
        };
        let filter = self.browse_filter();
        let Some(version) = modrinth::newest_for(list, &filter).or_else(|| list.first()) else { return quiet(tr("Braucht nichts weiter."), t) };
        let needs: Vec<(String, String)> = version
            .dependencies
            .iter()
            .filter(|d| d.dependency_type != "embedded")
            .filter_map(|d| {
                let id = d.project_id.clone()?;
                Some((id, d.dependency_type.clone()))
            })
            .collect();
        if needs.is_empty() {
            return quiet(tr("Braucht nichts weiter."), t);
        }
        let mod_kind = page.kind == Kind::Mod;
        let rows = needs.into_iter().map(|(id, kind)| {
            let title = page.titles.get(&id).cloned().unwrap_or_else(|| id.clone());
            let have = self.mods.iter().any(|m| m.project_id == id);
            let (state, color) = match kind.as_str() {
                _ if have => (tr("Installiert"), t.success_text),
                "required" if mod_kind => (tr("Wird mitinstalliert"), t.secondary),
                "required" => (tr("Benötigt"), t.secondary),
                "incompatible" => (tr("Verträgt sich nicht"), t.danger_text),
                _ => (tr("Optional"), t.tertiary),
            };
            div()
                .h(px(VERSION_ROW))
                .flex()
                .items_center()
                .gap(px(space::M))
                .border_b_1()
                .border_color(t.separator)
                .child(self.face(&id, t))
                .child(div().flex_1().min_w_0().truncate().text_size(px(font::BODY)).child(title))
                .child(div().flex_none().text_size(px(font::CALLOUT)).text_color(color).child(state))
        });
        div().flex().flex_col().children(rows.collect::<Vec<_>>()).into_any_element()
    }
}

fn quiet(text: String, t: &Theme) -> AnyElement {
    div().py(px(space::L)).text_size(px(font::CALLOUT)).text_color(t.secondary).child(text).into_any_element()
}
