//! Accounts: the list with proxies and the sheet to add a Microsoft or offline account.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui_kit::base::actions::{Cancel, Confirm};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu};
use gpui_kit::component::FocusableExt;
use gpui_kit::prelude::*;
use gpui_kit::*;
use uuid::Uuid;

use super::actions::{AddAccount, AddMicrosoft, ChangeSkin, Reload, SignInAgain, SignInEach, UndoRemoveAccount};
use super::drive::Mark;
use super::forms::{self, Problem};
use super::identity;
use super::motion::Soft;
use super::remote::{self, CodeStep, SignIn, SignInError, Via};
use super::sheet::{self, Dismissed, Look};
use super::toolbar;
use super::theme::{Theme, control, font, iconsize, radius, shell, space, tile, tint, with_alpha};
use super::widgets::{KeyNav, TakeFocus, icon, primary_button, tip};
use crate::core::account::{Account, AccountKind};
use crate::core::{browser, drive};
use crate::core::i18n::{tr, trf};
use crate::core::prism::AccountImport;
use crate::core::skins;

const COPIED_FOR: Duration = Duration::from_secs(2);
pub(super) const UNDO_FOR: Duration = Duration::from_secs(10);
const ADDED_NOTES: f32 = font::BODY * font::LINE * 2. + font::CAPTION * font::LINE * 2. + space::XS * 2.;

/// Registers the account actions.
pub fn init(cx: &mut App) {
    cx.on_action(|_: &AddAccount, cx| {
        sheet::in_window(cx, |window, cx| open_add(None, false, Vec::new(), window, cx));
    });
    cx.on_action(|_: &AddMicrosoft, cx| {
        sheet::in_window(cx, |window, cx| open_add(None, true, Vec::new(), window, cx));
    });
    cx.on_action(|action: &SignInEach, cx| {
        let names = action.0.clone();
        sheet::in_window(cx, move |window, cx| open_add(None, true, names, window, cx));
    });
    cx.on_action(|_: &UndoRemoveAccount, cx| {
        if let Some(page) = cx.try_global::<Page>().map(|p| p.0.clone()) {
            page.update(cx, |s, cx| s.undo_newest(cx));
        }
    });
    cx.on_action(|action: &SignInAgain, cx| {
        let Ok(id) = Uuid::parse_str(&action.0) else { return };
        let account = super::model::get(cx).read(cx).account(id).cloned();
        if let Some(account) = account.filter(|a| a.kind == AccountKind::Microsoft) {
            sheet::in_window(cx, move |window, cx| open_add(Some(account), false, Vec::new(), window, cx));
        }
    });
}

/// Opens the add-account sheet. `queue` names Microsoft accounts to sign in one after another, e.g. the ones Prism listed.
fn open_add(again: Option<Account>, microsoft: bool, queue: Vec<String>, window: &mut Window, cx: &mut App) {
    if sheet::busy(window, cx) {
        return;
    }
    let view = cx.new(|cx| {
        let mut sheet = AddAccountSheet::new(again, queue, window, cx);
        if microsoft && matches!(sheet.step, Step::Choose) {
            sheet.start_microsoft(cx);
        }
        sheet
    });
    sheet::show(view, sheet::NARROW, window, cx);
}

/// The Accounts page the main window shows.
pub fn page(window: &mut Window, cx: &mut App) -> impl IntoElement {
    if let Some(page) = cx.try_global::<Page>() {
        return page.0.clone();
    }
    let view = cx.new(|cx| AccountsPage::new(window, cx));
    cx.set_global(Page(view.clone()));
    view
}

/// Shows the accounts page's Prism import, as its button does.
pub fn import_prism(window: &mut Window, cx: &mut App) {
    page(window, cx);
    if let Some(page) = cx.try_global::<Page>().map(|p| p.0.clone()) {
        page.update(cx, |s, cx| s.import_prism(cx));
    }
}

/// A removed account that can still be brought back.
pub struct Removal {
    pub id: u64,
    pub at: Instant,
    pub name: String,
    pub failed: bool,
    pub shown: bool,
}

/// The removals that can still be undone, oldest first.
pub fn removals(cx: &App) -> Vec<Removal> {
    let Some(page) = cx.try_global::<Page>() else { return Vec::new() };
    page.0.read(cx).removed.items.iter().map(|(id, o)| Removal { id: *id, at: o.at, name: o.removed.account.name.clone(), failed: o.failed, shown: !o.hidden }).collect()
}

/// Brings the removed account `id` back.
pub fn undo_removal(id: u64, cx: &mut App) {
    if let Some(page) = cx.try_global::<Page>().map(|p| p.0.clone()) {
        page.update(cx, |s, cx| s.undo_remove(id, cx));
    }
}

/// Keeps the notice of removal `id` and its undo while the pointer is over it, and gives it a fresh time on leaving.
pub fn hover_removal(id: u64, on: bool, cx: &mut App) {
    if let Some(page) = cx.try_global::<Page>().map(|p| p.0.clone()) {
        page.update(cx, |s, cx| s.hover_removal(id, on, cx));
    }
}

/// Hides the notice of removal `id`; it can still be undone until its time is up.
pub fn hide_removal(id: u64, cx: &mut App) {
    if let Some(page) = cx.try_global::<Page>().map(|p| p.0.clone()) {
        page.update(cx, |s, cx| {
            if let Some((_, o)) = s.removed.items.iter_mut().find(|(i, _)| *i == id) {
                o.hidden = true;
                o.hovered = false;
                cx.notify();
            }
        });
        super::undo::changed(cx);
    }
}

/// The Microsoft accounts from the last Prism import that still have to sign in.
pub fn waiting(cx: &App) -> Vec<String> {
    let Some(page) = cx.try_global::<Page>() else { return Vec::new() };
    let Some(done) = page.0.read(cx).prism_done.as_ref() else { return Vec::new() };
    let m = super::model::get(cx).read(cx);
    done.needs_sign_in.iter().filter(|n| !m.accounts.iter().any(|a| a.name.eq_ignore_ascii_case(n))).cloned().collect()
}

struct Page(Entity<AccountsPage>);

impl Global for Page {}

/// Draws the Accounts page again, after an account's head changed.
pub fn redraw(cx: &mut App) {
    if let Some(page) = cx.try_global::<Page>().map(|p| p.0.clone()) {
        page.update(cx, |_, cx| cx.notify());
    }
}

/// Moves the Accounts page's filter into focus, if it shows one.
pub fn focus_filter(window: &mut Window, cx: &mut App) {
    let Some(page) = cx.try_global::<Page>().map(|p| p.0.clone()) else { return };
    page.update(cx, |s, cx| s.focus_filter(window, cx));
}

/// Whether the Accounts page shows a filter to focus.
pub fn has_filter(cx: &App) -> bool {
    cx.try_global::<Page>().is_some_and(|p| p.0.read(cx).filter_shown())
}

struct Offer {
    removed: remote::Removed,
    failed: bool,
    at: Instant,
    hidden: bool,
    hovered: bool,
    round: u64,
}

pub(super) struct Pending<T> {
    pub(super) items: Vec<(u64, T)>,
    next: u64,
}

impl<T> Pending<T> {
    pub(super) fn new() -> Self {
        Self { items: Vec::new(), next: 0 }
    }

    pub(super) fn push(&mut self, item: T) -> u64 {
        self.next += 1;
        self.items.push((self.next, item));
        self.next
    }

    pub(super) fn take(&mut self, id: u64) -> Option<T> {
        let at = self.items.iter().position(|(i, _)| *i == id)?;
        Some(self.items.remove(at).1)
    }

    pub(super) fn newest(&self) -> Option<u64> {
        self.items.last().map(|(id, _)| *id)
    }

}

#[derive(Clone, Debug, PartialEq)]
enum Check {
    Idle,
    Running,
    Passed,
    Failed(Problem),
}

fn check_line(check: &Check, open: bool, toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static, t: &Theme) -> Option<Div> {
    match check {
        Check::Idle => None,
        Check::Running => Some(sheet::waiting(tr("Proxy wird geprüft …"), t)),
        Check::Passed => Some(sheet::status("circle-check", t.success_text, tr("Der Proxy erreicht das Internet."))),
        Check::Failed(why) => Some(sheet::problem("proxy-check", "circle-alert", t.danger_text, why, open, toggle, t)),
    }
}

fn start_check<V: 'static>(proxy: String, cx: &mut Context<V>, apply: impl Fn(&mut V, Check) + 'static) {
    let task = cx.background_executor().spawn(async move { remote::test_proxy(&proxy).map_err(|e| proxy_failure(&e)) });
    cx.spawn(async move |this, cx| {
        let result = task.await;
        let _ = this.update(cx, |v, cx| {
            apply(v, result.map_or_else(Check::Failed, |()| Check::Passed));
            cx.notify();
        });
    })
    .detach();
}

fn proxy_failure(e: &anyhow::Error) -> Problem {
    Problem { text: tr("Über diesen Proxy kommt kein Internet. Prüfe Adresse, Port und Zugangsdaten."), detail: forms::explain(e).detail }
}

fn input_value(input: &Entity<InputState>, cx: &App) -> String {
    input.read(cx).value().trim().to_string()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Copied {
    Code,
    Link,
}

enum Step {
    Choose,
    Microsoft(Ms),
    Offline,
}

enum Ms {
    Requesting,
    Window,
    Code(CodeStep),
    Finishing,
    Added(String),
    Failed(SignInError),
}

struct AddAccountSheet {
    step: Step,
    again: Option<Account>,
    name: Entity<InputState>,
    proxy: Entity<InputState>,
    advanced: bool,
    check: Check,
    saving: bool,
    save_error: Option<Problem>,
    details: bool,
    queue: Vec<String>,
    added: Vec<String>,
    copied: Option<Copied>,
    windowed: bool,
    via: Via,
    opened_private: bool,
    private_left_open: bool,
    cancel: Arc<AtomicBool>,
    first: FocusHandle,
    main: FocusHandle,
    focus_next: bool,
    root: FocusHandle,
    _subs: Vec<Subscription>,
    _pump: Option<Task<()>>,
}

impl EventEmitter<Dismissed> for AddAccountSheet {}

impl Drop for AddAccountSheet {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

impl AddAccountSheet {
    fn new(again: Option<Account>, queue: Vec<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder(tr("z. B. Steve")));
        let proxy = cx.new(|cx| InputState::new(window, cx).placeholder(tr("z. B. socks5://host:1080")));
        let subs = vec![
            cx.subscribe(&name, |s: &mut Self, _, e: &InputEvent, cx| {
                if matches!(e, InputEvent::Change) {
                    s.save_error = None;
                    cx.notify();
                }
            }),
            cx.subscribe(&proxy, |s: &mut Self, _, e: &InputEvent, cx| {
                if matches!(e, InputEvent::Change) {
                    s.check = Check::Idle;
                    cx.notify();
                }
            }),
        ];
        let mut sheet = Self {
            step: Step::Choose,
            again,
            name,
            proxy,
            advanced: false,
            check: Check::Idle,
            saving: false,
            save_error: None,
            details: false,
            queue,
            added: Vec::new(),
            copied: None,
            windowed: false,
            via: Via::Window,
            opened_private: false,
            private_left_open: false,
            cancel: Arc::new(AtomicBool::new(false)),
            first: cx.focus_handle().tab_index(0).tab_stop(true),
            main: cx.focus_handle().tab_index(0).tab_stop(true),
            focus_next: true,
            root: cx.focus_handle(),
            _subs: subs,
            _pump: None,
        };
        if sheet.again.is_some() {
            sheet.start_microsoft(cx);
        }
        sheet
    }

    fn proxy_text(&self, cx: &App) -> String {
        input_value(&self.proxy, cx)
    }

    fn proxy_problem(&self, cx: &App) -> Option<String> {
        forms::proxy_problem(&self.proxy_text(cx))
    }

    fn chosen_proxy(&self, cx: &App) -> Option<String> {
        Some(self.proxy_text(cx)).filter(|p| !p.is_empty())
    }

    fn go(&mut self, step: Step, cx: &mut Context<Self>) {
        self.step = step;
        self.focus_next = true;
        self.details = false;
        cx.notify();
    }

    fn finish(&mut self, cx: &mut Context<Self>) {
        self.cancel.store(true, Ordering::SeqCst);
        cx.emit(Dismissed);
    }

    fn skip(&mut self, cx: &mut Context<Self>) {
        if !self.queue.is_empty() {
            self.queue.remove(0);
        }
        self.start(self.via, cx);
    }

    fn start_microsoft(&mut self, cx: &mut Context<Self>) {
        self.start(Via::Window, cx);
    }

    fn start(&mut self, via: Via, cx: &mut Context<Self>) {
        self.cancel.store(true, Ordering::SeqCst);
        self.cancel = Arc::new(AtomicBool::new(false));
        self.copied = None;
        self.opened_private = false;
        self.via = via;
        let proxy = match &self.again {
            Some(account) => account.proxy.clone(),
            None => self.chosen_proxy(cx),
        };
        self.windowed = remote::uses_window(proxy.as_deref(), via);
        self.go(Step::Microsoft(Ms::Requesting), cx);
        let expected = self.again.as_ref().map(|a| a.id);
        let (tx, rx) = async_channel::unbounded();
        let cancel = self.cancel.clone();
        std::thread::spawn(move || remote::sign_in(proxy, expected, via, cancel, tx));
        let cancel = self.cancel.clone();
        self._pump = Some(cx.spawn(async move |this, cx| {
            while let Ok(step) = rx.recv().await {
                if cancel.load(Ordering::SeqCst) {
                    return;
                }
                let done = matches!(step, SignIn::Done(_));
                let _ = this.update(cx, |s, cx| s.advance(step, cx));
                if done {
                    return;
                }
            }
        }));
    }

    fn advance(&mut self, step: SignIn, cx: &mut Context<Self>) {
        match step {
            SignIn::Window => self.go(Step::Microsoft(Ms::Window), cx),
            SignIn::Code(step) => {
                let open = step.open;
                self.windowed = false;
                self.via = Via::Browser;
                self.go(Step::Microsoft(Ms::Code(step)), cx);
                if open {
                    self.open_page(cx);
                }
            }
            SignIn::Approved => self.go(Step::Microsoft(Ms::Finishing), cx),
            SignIn::Failed(e) => self.go(Step::Microsoft(Ms::Failed(e)), cx),
            SignIn::Done(name) => {
                cx.dispatch_action(&Reload);
                if self.again.is_some() {
                    return self.finish(cx);
                }
                self.private_left_open = self.opened_private;
                self.queue.retain(|n| !n.eq_ignore_ascii_case(&name));
                self.added.push(name.clone());
                self.go(Step::Microsoft(Ms::Added(name)), cx);
            }
        }
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        self.cancel.store(true, Ordering::SeqCst);
        self._pump = None;
        self.save_error = None;
        self.go(Step::Choose, cx);
    }

    fn open_page(&mut self, cx: &mut Context<Self>) {
        let Step::Microsoft(Ms::Code(step)) = &self.step else { return };
        let (link, private) = (step.link.clone(), step.private);
        if drive::fenced(if private { "open private" } else { "open" }, &link) {
            self.opened_private |= private;
            return;
        }
        if !private {
            return cx.open_url(&link);
        }
        let target = link.clone();
        let opened = cx.background_executor().spawn(async move { browser::open_private(&target) });
        cx.spawn(async move |this, cx| {
            if matches!(opened.await, Ok(true)) {
                let _ = this.update(cx, |s, _| s.opened_private = true);
            } else {
                cx.update(|cx| cx.open_url(&link));
            }
        })
        .detach();
    }

    fn copy(&mut self, text: String, what: Copied, cx: &mut Context<Self>) {
        if !drive::fenced("clipboard", &text) {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
        self.copied = Some(what);
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COPIED_FOR).await;
            let _ = this.update(cx, |s, cx| {
                if s.copied == Some(what) {
                    s.copied = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn copy_link(&mut self, cx: &mut Context<Self>) {
        let Step::Microsoft(Ms::Code(step)) = &self.step else { return };
        let link = step.link.clone();
        self.copy(link, Copied::Link, cx);
    }

    fn add_offline(&mut self, cx: &mut Context<Self>) {
        let name = input_value(&self.name, cx);
        let accounts = super::model::get(cx).read(cx).accounts.clone();
        if self.saving || forms::offline_name_problem(&name, &accounts).is_some() {
            return;
        }
        self.saving = true;
        cx.notify();
        let proxy = self.chosen_proxy(cx);
        let task = cx.background_executor().spawn(async move { remote::add_offline(&name, proxy) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                s.saving = false;
                match result {
                    Ok(_) => {
                        cx.dispatch_action(&Reload);
                        s.finish(cx);
                    }
                    Err(e) => {
                        s.save_error = Some(forms::explain(&e));
                        s.details = false;
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn confirm(&mut self, window: &Window, cx: &mut Context<Self>) {
        if !sheet::typing(window) {
            cx.propagate();
            return;
        }
        match self.step {
            Step::Offline => self.add_offline(cx),
            Step::Choose if self.proxy_problem(cx).is_none() && !self.proxy_text(cx).is_empty() => self.run_check(cx),
            _ => {}
        }
    }

    fn run_check(&mut self, cx: &mut Context<Self>) {
        if self.check == Check::Running || self.proxy_problem(cx).is_some() || self.proxy_text(cx).is_empty() {
            return;
        }
        self.check = Check::Running;
        cx.notify();
        start_check(self.proxy_text(cx), cx, |s: &mut Self, c| s.check = c);
    }

    fn apply_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window.focused(cx).is_none() {
            window.focus(&self.root, cx);
        }
        if !std::mem::take(&mut self.focus_next) {
            return;
        }
        match &self.step {
            Step::Offline => self.name.update(cx, |s, cx| s.take_focus(window, cx)),
            Step::Choose => {
                let first = self.first.clone();
                window.on_next_frame(move |window, cx| window.focus(&first, cx));
            }
            Step::Microsoft(Ms::Window | Ms::Code(_) | Ms::Added(_) | Ms::Failed(_)) => window.focus(&self.main, cx),
            Step::Microsoft(_) => {}
        }
    }

    fn render_choose(&mut self, cx: &mut Context<Self>, t: &Theme) -> (Div, Vec<AnyElement>, Vec<AnyElement>) {
        let problem = self.proxy_problem(cx);
        let blocked = problem.is_some();
        let option = |id: &'static str, icon_name: &'static str, title: String, subtitle: String, focus: Option<&FocusHandle>| {
            let row = div()
                .id(id)
                .mark("option", title.clone())
                .w_full()
                .flex()
                .items_center()
                .gap(px(space::M))
                .px(px(space::M))
                .py(px(space::M))
                .rounded(px(radius::CARD))
                .border_1()
                .border_color(t.separator)
                .when_some(focus, |d, f| d.track_focus(f));
            let row = if blocked { row.opacity(tint::DISABLED) } else { row.cursor_pointer().key_nav().soft(t.ink.opacity(tint::HOVER), radius::CARD) };
            row.child(div().size(px(control::L)).flex_none().rounded(px(radius::CONTROL)).bg(with_alpha(t.accent, tint::ATTENTION)).flex().items_center().justify_center().child(icon(icon_name, iconsize::L, t.accent_text)))
                .child(div().flex_1().min_w_0().flex().flex_col().gap(px(space::XXS)).child(div().text_size(px(font::BODY)).font_weight(FontWeight::SEMIBOLD).text_color(t.primary).child(title)).child(div().text_size(px(font::CALLOUT)).text_color(t.secondary).child(subtitle)))
                .child(icon("chevron-right", iconsize::M, t.tertiary))
        };
        let microsoft = option("choose-microsoft", "key-round", tr("Microsoft-Konto"), tr("Für alle Server. Du meldest dich bei Microsoft an."), Some(&self.first))
            .when(!blocked, |d| d.on_click(cx.listener(|s, _, _, cx| s.start_microsoft(cx))));
        let offline = option("choose-offline", "user", tr("Offline-Konto"), tr("nur für Server ohne Online-Mode"), None).when(!blocked, |d| d.on_click(cx.listener(|s, _, _, cx| s.go(Step::Offline, cx))));
        let open = self.advanced || blocked;
        let filled = !self.proxy_text(cx).is_empty();
        let label = if filled && !open { tr("Erweitert · Proxy gesetzt") } else { tr("Erweitert") };
        let disclosure = div()
            .id("advanced")
            .mark("disclosure", tr("Erweitert"))
            .flex()
            .items_center()
            .gap(px(space::XS))
            .h(px(control::S))
            .px(px(space::XS))
            .ml(px(-space::XS))
            .rounded(px(radius::SMALL))
            .text_size(px(font::CALLOUT))
            .text_color(t.secondary)
            .cursor_pointer()
            .key_nav()
            .soft(t.ink.opacity(tint::HOVER), radius::SMALL)
            .child(icon(if open { "chevron-down" } else { "chevron-right" }, iconsize::S, t.secondary))
            .child(label)
            .on_click(cx.listener(move |s, _, window, cx| {
                s.advanced = !open;
                if s.advanced {
                    s.proxy.update(cx, |p, cx| p.take_focus(window, cx));
                }
                cx.notify();
            }));
        let proxy_row = div()
            .flex()
            .items_center()
            .gap(px(space::S))
            .child(div().flex_1().min_w_0().child(Input::new(&self.proxy)))
            .child(sheet::button("proxy-test", tr("Testen"), None, Look::Secondary, !filled || blocked, self.check == Check::Running, t).on_click(cx.listener(|s, _, _, cx| s.run_check(cx))));
        let proxy_field = div()
            .flex()
            .flex_col()
            .gap(px(space::XS))
            .child(sheet::label(tr("Proxy"), t))
            .child(proxy_row)
            .child(sheet::note(Some(tr("Gilt für Anmeldung und Spiel. Leer lassen für keinen Proxy.")), problem, t))
            .children(check_line(&self.check, self.details, cx.listener(|s, _, _, cx| s.toggle_details(cx)), t));
        let body = div().flex().flex_col().gap(px(space::S)).child(microsoft).child(offline).child(div().pt(px(space::S)).child(disclosure)).when(open, |d| d.child(proxy_field));
        let cancel = sheet::button("cancel", tr("Abbrechen"), None, Look::Secondary, false, false, t).on_click(cx.listener(|s, _, _, cx| s.finish(cx)));
        (body, Vec::new(), vec![cancel.into_any_element()])
    }

    fn render_offline(&mut self, cx: &mut Context<Self>, t: &Theme) -> (Div, Vec<AnyElement>, Vec<AnyElement>) {
        let name = input_value(&self.name, cx);
        let accounts = super::model::get(cx).read(cx).accounts.clone();
        let problem = forms::offline_name_problem(&name, &accounts);
        let shown = problem.clone().filter(|_| !name.is_empty() && self.save_error.is_none());
        let mut body = div().flex().flex_col().gap(px(space::M)).child(sheet::field(tr("Name"), Input::new(&self.name), Some(tr("3 bis 16 Zeichen: Buchstaben, Ziffern und _")), shown, t));
        if let Some(why) = &self.save_error {
            body = body.child(sheet::problem("add-offline-error", "circle-alert", t.danger_text, why, self.details, cx.listener(|s, _, _, cx| s.toggle_details(cx)), t));
        }
        if let Some(proxy) = self.chosen_proxy(cx) {
            body = body.child(div().text_size(px(font::CALLOUT)).text_color(t.secondary).child(trf("Proxy: %@", &[&forms::proxy_display(&proxy)])));
        }
        let back = sheet::button("back", tr("Zurück"), Some("chevron-left"), Look::Secondary, false, false, t).on_click(cx.listener(|s, _, _, cx| s.back(cx)));
        let cancel = sheet::button("cancel", tr("Abbrechen"), None, Look::Secondary, false, false, t).on_click(cx.listener(|s, _, _, cx| s.finish(cx)));
        let add = sheet::button("add-offline", tr("Hinzufügen"), None, Look::Primary, problem.is_some(), self.saving, t).on_click(cx.listener(|s, _, _, cx| s.add_offline(cx)));
        (body, vec![back.into_any_element()], vec![cancel.into_any_element(), add.into_any_element()])
    }

    fn toggle_details(&mut self, cx: &mut Context<Self>) {
        self.details = !self.details;
        cx.notify();
    }

    fn render_microsoft(&mut self, cx: &mut Context<Self>, t: &Theme) -> (Div, Vec<AnyElement>, Vec<AnyElement>) {
        let added = matches!(self.step, Step::Microsoft(Ms::Added(_)));
        let mut trailing = vec![sheet::button("cancel", if added { tr("Fertig") } else { tr("Abbrechen") }, None, Look::Secondary, false, false, t).on_click(cx.listener(|s, _, _, cx| s.finish(cx))).into_any_element()];
        let mut leading = if self.again.is_none() && !added { vec![sheet::button("back", tr("Zurück"), Some("chevron-left"), Look::Secondary, false, false, t).on_click(cx.listener(|s, _, _, cx| s.back(cx))).into_any_element()] } else { Vec::new() };
        let Step::Microsoft(ms) = &self.step else { return (div(), leading, trailing) };
        let body = match ms {
            Ms::Requesting => div().py(px(space::L)).child(sheet::waiting(if self.windowed { tr("Anmeldefenster wird geöffnet …") } else { tr("Code wird angefordert …") }, t)),
            Ms::Window => {
                let instruction = match self.again.as_ref().map(|a| &a.name).or(self.queue.first()) {
                    Some(name) => trf("Melde dich im Anmeldefenster mit dem Konto „%@“ an. Danach geht es hier von selbst weiter.", &[name]),
                    None => tr("Melde dich im Anmeldefenster mit dem Konto an, das du hinzufügen willst. Danach geht es hier von selbst weiter."),
                };
                div()
                    .flex()
                    .flex_col()
                    .gap(px(space::M))
                    .child(div().text_size(px(font::BODY)).text_color(t.primary).child(instruction))
                    .child(div().id("sign-in-window").track_focus(&self.main).mark("status", tr("Warte auf deine Anmeldung im Fenster …")).child(sheet::waiting(tr("Warte auf deine Anmeldung im Fenster …"), t)))
                    .child(div().flex().ml(px(-space::S)).child(sheet::button("use-browser", tr("Im Browser anmelden"), Some("external-link"), Look::Plain, false, false, t).on_click(cx.listener(|s, _, _, cx| s.start(Via::Browser, cx)))))
            }
            Ms::Finishing => div().py(px(space::L)).child(sheet::waiting(tr("Anmeldung wird abgeschlossen …"), t)),
            Ms::Code(step) => {
                let (user_code, expires_in) = (&step.user_code, step.expires_in);
                let code = user_code.clone();
                let copied = self.copied == Some(Copied::Code);
                let copy = sheet::icon_button("copy-code", if copied { "check" } else { "copy" }, if copied { tr("Kopiert") } else { tr("Code kopieren") }, if copied { t.success_text } else { t.secondary }, t).on_click(cx.listener(move |s, _, _, cx| s.copy(code.clone(), Copied::Code, cx)));
                let code_box = div()
                    .id("device-code")
                    .mark("code", user_code.clone())
                    .relative()
                    .flex()
                    .items_center()
                    .justify_center()
                    .py(px(space::L))
                    .rounded(px(radius::CARD))
                    .bg(t.raised)
                    .child(div().font_family(Theme::mono_family()).text_size(px(font::LARGE_TITLE)).font_weight(FontWeight::SEMIBOLD).text_color(t.primary).child(user_code.clone()))
                    .child(div().absolute().right(px(space::S)).top_0().bottom_0().flex().items_center().child(copy));
                let minutes = (expires_in / 60).max(1);
                let instruction = match self.queue.first() {
                    Some(name) => trf("Öffne die Microsoft-Seite, prüfe den Code und melde dich mit dem Konto „%@“ an. Danach geht es hier von selbst weiter.", &[name]),
                    None => tr("Öffne die Microsoft-Seite, prüfe den Code und melde dich an. Danach geht es hier von selbst weiter."),
                };
                let hint = if step.private {
                    tr("Die Seite öffnet sich in einem privaten Fenster, unabhängig von deiner Anmeldung im Browser.")
                } else {
                    tr("Melde dich vorher bei Microsoft ab, wenn du ein anderes Konto nehmen willst.")
                };
                let status = match self.copied {
                    Some(Copied::Code) => sheet::status("check", t.success_text, tr("Code kopiert. Auf der Seite einfügen.")),
                    Some(Copied::Link) => sheet::status("check", t.success_text, tr("Link kopiert. Der Code steckt schon darin.")),
                    None => sheet::waiting(tr("Warte auf deine Anmeldung im Browser …"), t),
                };
                trailing.push(sheet::button("open-page", tr("Seite öffnen"), Some("external-link"), Look::Primary, false, false, t).track_focus(&self.main).on_click(cx.listener(|s, _, _, cx| s.open_page(cx))).into_any_element());
                div()
                    .flex()
                    .flex_col()
                    .gap(px(space::M))
                    .children(step.fallback.then(|| div().id("window-fallback").mark("note", tr("Das Anmeldefenster ließ sich nicht öffnen, deshalb geht es im Browser weiter.")).text_size(px(font::CALLOUT)).text_color(t.secondary).child(tr("Das Anmeldefenster ließ sich nicht öffnen, deshalb geht es im Browser weiter."))))
                    .child(div().text_size(px(font::BODY)).text_color(t.primary).child(instruction))
                    .child(code_box)
                    .child(status)
                    .child(div().id("browser-hint").mark("note", hint.clone()).text_size(px(font::CALLOUT)).text_color(t.secondary).child(hint))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(px(space::M))
                            .child(div().min_w_0().text_size(px(font::CAPTION)).text_color(t.tertiary).child(trf("Der Code gilt %lld Minuten.", &[&minutes])))
                            .child(sheet::button("copy-link", tr("Link kopieren"), Some("link"), Look::Plain, false, false, t).on_click(cx.listener(|s, _, _, cx| s.copy_link(cx)))),
                    )
            }
            Ms::Added(name) => {
                let next = self.queue.first().cloned();
                let more = match &next {
                    Some(next) => sheet::button("sign-in-more", trf("Weiter mit %@", &[next]), Some("arrow-right"), Look::Primary, false, false, t),
                    None => sheet::button("sign-in-more", tr("Noch einen"), Some("plus"), Look::Primary, false, false, t),
                };
                trailing.push(more.track_focus(&self.main).on_click(cx.listener(|s, _, _, cx| s.start_microsoft(cx))).into_any_element());
                let before: Vec<&String> = self.added.iter().filter(|n| *n != name).collect();
                let mut more_lines = div().min_h(px(ADDED_NOTES)).flex().flex_col().gap(px(space::XS));
                if !before.is_empty() {
                    more_lines = more_lines.child(div().truncate().text_size(px(font::CAPTION)).text_color(t.tertiary).child(trf("Schon hinzugefügt: %@", &[&before.iter().map(|n| n.as_str()).collect::<Vec<_>>().join(", ")])));
                }
                more_lines = match next {
                    Some(next) => {
                        let rest = self.queue.len();
                        more_lines.child(div().line_clamp(2).text_size(px(font::BODY)).text_color(t.primary).child(if rest == 1 { trf("Als Nächstes: %@. Das ist der letzte Prism-Account.", &[&next]) } else { trf("Als Nächstes: %@. Danach noch %lld.", &[&next, &(rest - 1)]) }))
                    }
                    None => more_lines.child(div().text_size(px(font::CALLOUT)).text_color(t.secondary).child(tr("Du kannst gleich noch ein Microsoft-Konto hinzufügen."))),
                };
                if self.private_left_open {
                    more_lines = more_lines.child(div().id("close-private").mark("note", tr("Schließ das private Fenster, bevor du das nächste Konto anmeldest.")).text_size(px(font::CALLOUT)).text_color(t.secondary).child(tr("Schließ das private Fenster, bevor du das nächste Konto anmeldest.")));
                }
                if self.again.is_none() && let Some(proxy) = self.chosen_proxy(cx) {
                    more_lines = more_lines.child(div().truncate().text_size(px(font::CAPTION)).text_color(t.tertiary).child(trf("Der nächste Account nutzt wieder den Proxy %@.", &[&forms::proxy_display(&proxy)])));
                }
                div().flex().flex_col().gap(px(space::S)).py(px(space::S)).child(div().id("signed-in").mark("status", name.clone()).child(sheet::status("circle-check", t.success_text, trf("Angemeldet als %@.", &[name])))).child(more_lines)
            }
            Ms::Failed(e) => {
                let (title, detail, retry, offline) = failure_text(e, self.again.as_ref());
                let technical = match e {
                    SignInError::Other(p) => p.detail.clone(),
                    _ => None,
                };
                let skip = (self.queue.len() > 1 && self.again.is_none()).then(|| div().flex().child(sheet::button("skip", tr("Überspringen"), None, Look::Plain, false, false, t).on_click(cx.listener(|s, _, _, cx| s.skip(cx)))));
                let to_browser = matches!(e, SignInError::Closed).then(|| div().flex().ml(px(-space::S)).child(sheet::button("use-browser", tr("Im Browser anmelden"), Some("external-link"), Look::Plain, false, false, t).on_click(cx.listener(|s, _, _, cx| s.start(Via::Browser, cx)))));
                if offline {
                    trailing.push(sheet::button("to-offline", tr("Offline-Konto hinzufügen"), None, Look::Primary, false, false, t).track_focus(&self.main).on_click(cx.listener(|s, _, _, cx| s.go(Step::Offline, cx))).into_any_element());
                } else {
                    trailing.push(sheet::button("retry", retry, Some("refresh-cw"), Look::Primary, false, false, t).track_focus(&self.main).on_click(cx.listener(|s, _, _, cx| s.start(s.via, cx))).into_any_element());
                }
                let icon_name = if matches!(e, SignInError::Network) { "wifi-off" } else { "circle-alert" };
                div()
                    .flex()
                    .gap(px(space::M))
                    .py(px(space::S))
                    .child(icon(icon_name, iconsize::L, t.danger_text))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(space::XS))
                            .child(div().id("sign-in-error").mark("error", title.clone()).text_size(px(font::BODY)).font_weight(FontWeight::SEMIBOLD).text_color(t.primary).child(title))
                            .child(div().text_size(px(font::CALLOUT)).text_color(t.secondary).child(detail))
                            .children(technical.map(|d| sheet::details("sign-in-technical", d, self.details, cx.listener(|s, _, _, cx| s.toggle_details(cx)), t)))
                            .children(to_browser)
                            .children(skip),
                    )
            }
        };
        (body, leading, trailing)
    }
}

fn failure_text(e: &SignInError, again: Option<&Account>) -> (String, String, String, bool) {
    let retry = tr("Erneut versuchen");
    match e {
        SignInError::Expired => (tr("Der Code ist abgelaufen."), tr("Fordere einen neuen Code an und gib ihn innerhalb der Frist ein."), tr("Neuer Code"), false),
        SignInError::Declined => (tr("Die Anmeldung wurde bei Microsoft abgebrochen."), tr("Versuche es erneut, wenn du bereit bist."), retry, false),
        SignInError::Closed => (tr("Das Anmeldefenster wurde geschlossen."), tr("Es wurde nichts gespeichert."), tr("Erneut öffnen"), false),
        SignInError::NoClientId => (
            tr("Die Anmeldung mit Microsoft gibt es in dieser Version noch nicht."),
            if again.is_some() { tr("Eine spätere Version kann Microsoft-Konten anmelden.") } else { tr("Offline-Konten kannst du schon hinzufügen.") },
            retry,
            again.is_none(),
        ),
        SignInError::NotOwned => (tr("Dieses Konto besitzt Minecraft: Java Edition nicht."), tr("Melde dich mit dem Konto an, mit dem du Minecraft gekauft hast."), tr("Anderes Konto"), false),
        SignInError::WrongAccount(name) => (
            trf("Du hast dich als %@ angemeldet.", &[name]),
            trf("Melde dich mit %@ an, um diesen Account zu erneuern.", &[&again.map_or_else(String::new, |a| a.name.clone())]),
            retry,
            false,
        ),
        SignInError::Network => (tr("Keine Verbindung zu Microsoft."), tr("Prüfe die Internetverbindung und den Proxy, dann versuche es erneut."), retry, false),
        SignInError::Other(why) => (tr("Die Anmeldung hat nicht geklappt."), if why.text == forms::unexplained() { tr("Versuche es noch einmal. Bleibt es so, helfen die Details bei der Fehlersuche.") } else { why.text.clone() }, retry, false),
    }
}

impl Render for AddAccountSheet {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.apply_focus(window, cx);
        let t = Theme::get(cx);
        let (title, subtitle) = match (&self.step, &self.again) {
            (Step::Microsoft(_), Some(a)) => (tr("Erneut anmelden"), Some(a.name.clone())),
            (Step::Microsoft(Ms::Added(_)), None) => (trf("Konto %lld hinzugefügt", &[&self.added.len()]), None),
            (Step::Microsoft(_), None) => (tr("Mit Microsoft anmelden"), self.queue.first().map(|n| trf("Für das Prism-Konto „%@“", &[n]))),
            (Step::Offline, _) => (tr("Offline-Konto"), Some(tr("Ohne Anmeldung, nur mit Namen. Für Server ohne Online-Mode."))),
            (Step::Choose, _) => (tr("Account hinzufügen"), None),
        };
        let (body, leading, trailing) = match self.step {
            Step::Choose => self.render_choose(cx, &t),
            Step::Offline => self.render_offline(cx, &t),
            Step::Microsoft(_) => self.render_microsoft(cx, &t),
        };
        div()
            .id("add-account-sheet")
            .track_focus(&self.root)
            .w_full()
            .flex()
            .flex_col()
            .text_color(t.primary)
            .on_action(cx.listener(|s, _: &Confirm, window, cx| s.confirm(window, cx)))
            .on_action(cx.listener(|s, _: &Cancel, _, cx| {
                if matches!(s.step, Step::Choose) || s.again.is_some() {
                    cx.propagate();
                } else if matches!(s.step, Step::Microsoft(Ms::Added(_))) {
                    s.finish(cx);
                } else {
                    s.back(cx);
                }
            }))
            .child(sheet::header(title, subtitle, &t))
            .child(div().px(px(space::XL)).pb(px(space::L)).child(body))
            .child(sheet::footer(leading, trailing, &t))
    }
}

struct Editing {
    id: Uuid,
    input: Entity<InputState>,
    check: Check,
    saving: bool,
    _sub: Subscription,
}

struct AccountsPage {
    list: ListState,
    accounts: Vec<Account>,
    usage: HashMap<Uuid, Vec<String>>,
    shown: Vec<usize>,
    stale: bool,
    header_key: String,
    edit_key: String,
    edited: Option<Uuid>,
    heads_for: Vec<Uuid>,
    editing: Option<Editing>,
    removing: Option<Uuid>,
    removed: Pending<Offer>,
    error: Option<Problem>,
    details: HashSet<&'static str>,
    prism_found: bool,
    prism_busy: bool,
    prism_done: Option<AccountImport>,
    filter: Entity<InputState>,
    root: FocusHandle,
    _model: Subscription,
    _filter: Subscription,
}

impl AccountsPage {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = super::model::get(cx);
        let _model = cx.observe(&model, |s, _, cx| {
            s.stale = true;
            cx.notify();
        });
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder(tr("Accounts filtern")));
        let _filter = cx.subscribe(&filter, |s, _, e: &InputEvent, cx| {
            if matches!(e, InputEvent::Change) {
                s.stale = true;
                cx.notify();
            }
        });
        let mut page = Self {
            list: ListState::new(1, ListAlignment::Top, px(OVERDRAW)),
            accounts: Vec::new(),
            usage: HashMap::new(),
            shown: Vec::new(),
            stale: true,
            header_key: String::new(),
            edit_key: String::new(),
            edited: None,
            heads_for: Vec::new(),
            editing: None,
            removing: None,
            removed: Pending::new(),
            error: None,
            details: HashSet::new(),
            prism_found: false,
            prism_busy: false,
            prism_done: None,
            filter,
            root: cx.focus_handle(),
            _model,
            _filter,
        };
        page.look_for_prism(cx);
        page
    }

    fn toggle(&mut self, key: &'static str, cx: &mut Context<Self>) {
        if !self.details.remove(key) {
            self.details.insert(key);
        }
        cx.notify();
    }

    fn fail(&mut self, problem: Problem) {
        self.error = Some(problem);
        self.details.remove("error");
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if !std::mem::take(&mut self.stale) {
            return;
        }
        let model = super::model::get(cx);
        let (accounts, usage) = {
            let m = model.read(cx);
            let mut usage: HashMap<Uuid, Vec<String>> = HashMap::new();
            for b in &m.batches {
                for id in &b.accounts {
                    usage.entry(*id).or_default().push(b.name.clone());
                }
            }
            (m.accounts.clone(), usage)
        };
        let query = if accounts.len() > FILTER_FROM { input_value(&self.filter, cx).to_lowercase() } else { String::new() };
        let shown: Vec<usize> = accounts.iter().enumerate().filter(|(_, a)| query.is_empty() || a.name.to_lowercase().contains(&query)).map(|(i, _)| i).collect();
        let old: Vec<Uuid> = self.shown.iter().filter_map(|i| self.accounts.get(*i)).map(|a| a.id).collect();
        let new: Vec<Uuid> = shown.iter().map(|i| accounts[*i].id).collect();
        self.accounts = accounts;
        self.usage = usage;
        self.shown = shown;
        self.sync_rows(&old, &new);
        if self.editing.as_ref().is_some_and(|e| !self.accounts.iter().any(|a| a.id == e.id)) {
            self.editing = None;
        }
        self.load_heads(cx);
    }

    fn sync_rows(&self, old: &[Uuid], new: &[Uuid]) {
        if old.is_empty() || new.is_empty() {
            self.list.reset(1 + new.len().max(1));
            return;
        }
        let front = old.iter().zip(new).take_while(|(a, b)| a == b).count();
        let back = old[front..].iter().rev().zip(new[front..].iter().rev()).take_while(|(a, b)| a == b).count();
        if front == old.len() && front == new.len() {
            return;
        }
        self.list.splice(1 + front..1 + old.len() - back, new.len() - front - back);
    }

    fn touch(&self, id: Uuid) {
        if let Some(at) = self.shown.iter().position(|i| self.accounts[*i].id == id) {
            self.list.splice(1 + at..2 + at, 1);
        }
    }

    fn sync_heights(&mut self, cx: &App) {
        let header = format!(
            "{:?}",
            (
                self.error.clone(),
                self.details.contains("error"),
                self.prism_done.as_ref().map(|d| (d.added.len(), d.needs_sign_in.len(), d.skipped.len())),
                self.accounts.len(),
                self.prism_found,
            )
        );
        if header != self.header_key {
            self.header_key = header;
            self.list.splice(0..1, 1);
        }
        let edit = self.editing.as_ref().map(|e| (e.id, e.check.clone(), forms::proxy_problem(&input_value(&e.input, cx)).is_some(), self.details.contains("check")));
        let key = format!("{edit:?}");
        if key != self.edit_key {
            self.edit_key = key;
            if let Some(old) = self.edited {
                self.touch(old);
            }
            self.edited = edit.map(|e| e.0);
            if let Some(new) = self.edited {
                self.touch(new);
            }
        }
    }

    fn load_heads(&mut self, cx: &mut Context<Self>) {
        let ids: Vec<Uuid> = self.accounts.iter().map(|a| a.id).collect();
        if ids == self.heads_for {
            return;
        }
        self.heads_for = ids;
        sheet::warm_heads(self.accounts.clone(), cx);
    }

    fn look_for_prism(&mut self, cx: &mut Context<Self>) {
        let task = cx.background_executor().spawn(async { remote::prism_accounts().map_or(0, |found| found.len()) });
        cx.spawn(async move |this, cx| {
            let count = task.await;
            let _ = this.update(cx, |s, cx| {
                s.prism_found = count > 0;
                cx.notify();
            });
        })
        .detach();
    }

    fn import_prism(&mut self, cx: &mut Context<Self>) {
        if self.prism_busy {
            return;
        }
        self.prism_busy = true;
        self.error = None;
        cx.notify();
        let task = cx.background_executor().spawn(async { remote::import_prism_accounts() });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                s.prism_busy = false;
                match result {
                    Ok(done) => {
                        s.prism_done = Some(done);
                        cx.dispatch_action(&Reload);
                    }
                    Err(e) => s.fail(forms::explain_as(tr("Aus Prism nicht übernommen."), &e)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn prism_button(&self, cx: &mut Context<Self>, t: &Theme) -> Stateful<Div> {
        sheet::button("accounts-prism", tr("Aus Prism übernehmen"), Some("download"), Look::Secondary, false, self.prism_busy, t).on_click(cx.listener(|s, _, _, cx| s.import_prism(cx)))
    }

    fn prism_result(&self, accounts: &[Account], cx: &mut Context<Self>, t: &Theme) -> Option<Stateful<Div>> {
        let done = self.prism_done.as_ref()?;
        let waiting: Vec<&String> = done.needs_sign_in.iter().filter(|n| !accounts.iter().any(|a| a.name.eq_ignore_ascii_case(n))).collect();
        let existing = done.skipped.iter().filter(|(_, why)| why == "already added").count();
        let refused: Vec<String> = done
            .skipped
            .iter()
            .filter(|(_, why)| why != "already added")
            .map(|(name, _)| trf("„%@“ übersprungen: %@", &[name, &forms::offline_name_problem(name, &[]).unwrap_or_else(|| tr("Der Name ist ungültig."))]))
            .collect();
        let added = done.added.len();
        let summary = match (added, waiting.len()) {
            (0, 0) if refused.is_empty() => tr("Aus Prism gibt es nichts Neues. Alle Namen sind schon hier."),
            (0, _) => tr("Keine Offline-Accounts aus Prism übernommen."),
            (1, _) => tr("1 Offline-Account aus Prism übernommen."),
            _ => trf("%lld Offline-Accounts aus Prism übernommen.", &[&added]),
        };
        let mut card = div()
            .id("prism-result")
            .mark("panel", tr("Aus Prism übernommen"))
            .flex()
            .flex_col()
            .gap(px(space::S))
            .px(px(space::L))
            .py(px(space::M))
            .rounded(px(radius::CARD))
            .bg(t.content)
            .border_1()
            .border_color(t.separator)
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(space::S))
                    .child(div().flex_1().min_w_0().child(sheet::status(if added > 0 { "circle-check" } else { "info" }, if added > 0 { t.success_text } else { t.secondary }, summary)))
                    .child(sheet::icon_button("prism-dismiss", "x", tr("Ausblenden"), t.secondary, t).on_click(cx.listener(|s, _, window, cx| {
                        s.prism_done = None;
                        window.focus(&s.root, cx);
                        cx.notify();
                    }))),
            );
        if !waiting.is_empty() {
            let names: Vec<String> = waiting.iter().map(|n| (*n).clone()).collect();
            let all = names.clone();
            let intro = div().flex().items_start().gap(px(space::S)).child(div().flex_1().min_w_0().child(sheet::note(Some(tr("Diese Microsoft-Accounts brauchen eine neue Anmeldung. Anmeldungen aus Prism werden nicht übernommen.")), None, t))).when(names.len() > 1, |d| {
                d.child(sheet::button("prism-signin-all", tr("Alle nacheinander anmelden"), Some("log-in"), Look::Secondary, false, false, t).on_click(move |_, window, cx| open_add(None, true, all.clone(), window, cx)))
            });
            card = card.child(intro);
            for (at, name) in names.iter().enumerate() {
                let mut order = names.clone();
                order.rotate_left(at);
                card = card.child(
                    div()
                        .id(SharedString::from(format!("prism-wait-{name}")))
                        .mark("row", name.clone())
                        .flex()
                        .items_center()
                        .gap(px(space::M))
                        .child(icon("key-round", iconsize::S, t.secondary))
                        .child(div().flex_1().min_w_0().truncate().text_size(px(font::BODY)).font_weight(FontWeight::MEDIUM).child(name.clone()))
                        .child(sheet::button(SharedString::from(format!("prism-signin-{name}")), tr("Anmelden"), Some("log-in"), Look::Secondary, false, false, t).on_click(move |_, window, cx| open_add(None, true, order.clone(), window, cx))),
                );
            }
        }
        if existing > 0 && (added > 0 || !done.needs_sign_in.is_empty()) {
            card = card.child(div().text_size(px(font::CAPTION)).text_color(t.tertiary).child(if existing == 1 { tr("1 Name war schon vorhanden.") } else { trf("%lld Namen waren schon vorhanden.", &[&existing]) }));
        }
        for line in refused {
            card = card.child(sheet::status("circle-alert", t.danger_text, line));
        }
        Some(card)
    }

    fn edit_proxy(&mut self, account: &Account, window: &mut Window, cx: &mut Context<Self>) {
        let value = account.proxy.clone().unwrap_or_default();
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(tr("z. B. socks5://host:1080")).default_value(value));
        let sub = cx.subscribe(&input, |s: &mut Self, _, e: &InputEvent, cx| match e {
            InputEvent::Change => {
                if let Some(edit) = s.editing.as_mut() {
                    edit.check = Check::Idle;
                    cx.notify();
                }
            }
            InputEvent::PressEnter { .. } => s.save_proxy(cx),
            _ => {}
        });
        let focus = input.clone();
        window.on_next_frame(move |window, cx| focus.update(cx, |s, cx| s.take_focus(window, cx)));
        self.details.remove("check");
        self.editing = Some(Editing { id: account.id, input, check: Check::Idle, saving: false, _sub: sub });
        cx.notify();
    }

    fn save_proxy(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.editing.as_mut() else { return };
        let value = input_value(&edit.input, cx);
        if edit.saving || forms::proxy_problem(&value).is_some() {
            return;
        }
        edit.saving = true;
        cx.notify();
        let id = edit.id;
        let proxy = Some(value).filter(|p| !p.is_empty());
        let task = cx.background_executor().spawn(async move { remote::set_proxy(id, proxy) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                match result {
                    Ok(()) => {
                        if s.editing.as_ref().is_some_and(|e| e.id == id) {
                            s.editing = None;
                        }
                        cx.dispatch_action(&Reload);
                    }
                    Err(e) => {
                        if let Some(edit) = s.editing.as_mut().filter(|e| e.id == id) {
                            edit.saving = false;
                        }
                        s.fail(forms::explain_as(tr("Der Proxy wurde nicht gespeichert."), &e));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn test_proxy(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.editing.as_mut() else { return };
        let value = input_value(&edit.input, cx);
        if value.is_empty() || forms::proxy_problem(&value).is_some() || edit.check == Check::Running {
            return;
        }
        edit.check = Check::Running;
        self.details.remove("check");
        cx.notify();
        start_check(value, cx, |s: &mut Self, c| {
            if let Some(edit) = s.editing.as_mut() {
                edit.check = c;
            }
        });
    }

    fn ask_remove(&mut self, account: &Account, batches: &[String], window: &mut Window, cx: &mut Context<Self>) {
        let title = trf("„%@“ entfernen?", &[&account.name]);
        let mut message = if batches.is_empty() { String::new() } else { trf("Der Account wird auch aus %@ entfernt.", &[&batches.join(", ")]) };
        if account.kind == AccountKind::Microsoft {
            if !message.is_empty() {
                message.push(' ');
            }
            message.push_str(&tr("Die gespeicherte Anmeldung wird gelöscht; zum Wiederherstellen meldest du dich neu an."));
        }
        if message.is_empty() {
            message = tr("Die Spielordner dieses Accounts bleiben erhalten.");
        }
        let id = account.id;
        let page = cx.entity().downgrade();
        sheet::ask(window, cx, title, message, tr("Entfernen"), move |window, cx| {
            let _ = page.update(cx, |s, cx| {
                window.focus(&s.root, cx);
                s.remove(id, cx);
            });
        });
    }

    fn remove(&mut self, id: Uuid, cx: &mut Context<Self>) {
        self.removing = Some(id);
        cx.notify();
        let task = cx.background_executor().spawn(async move { remote::remove_account(id) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                s.removing = None;
                match result {
                    Ok(Some(removed)) => s.offer_undo(removed, false, cx),
                    Ok(None) => {}
                    Err(e) => s.fail(forms::explain_as(tr("Der Account wurde nicht vollständig entfernt."), &e)),
                }
                cx.dispatch_action(&Reload);
                cx.notify();
            });
        })
        .detach();
    }

    fn offer_undo(&mut self, removed: remote::Removed, failed: bool, cx: &mut Context<Self>) {
        let id = self.removed.push(Offer { removed, failed, at: Instant::now(), hidden: false, hovered: false, round: 0 });
        self.arm_removal(id, cx);
        cx.notify();
        super::undo::changed(cx);
    }

    fn arm_removal(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some((_, offer)) = self.removed.items.iter_mut().find(|(i, _)| *i == id) else { return };
        offer.round += 1;
        let round = offer.round;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(UNDO_FOR).await;
            let _ = this.update(cx, |s, cx| {
                if s.removed.items.iter().any(|(i, o)| *i == id && o.round == round && !o.hovered) {
                    s.removed.take(id);
                    cx.notify();
                    super::undo::changed(cx);
                }
            });
        })
        .detach();
    }

    fn hover_removal(&mut self, id: u64, on: bool, cx: &mut Context<Self>) {
        let Some((_, offer)) = self.removed.items.iter_mut().find(|(i, _)| *i == id) else { return };
        if offer.hovered == on {
            return;
        }
        offer.hovered = on;
        offer.round += 1;
        if !on {
            self.arm_removal(id, cx);
        }
    }

    fn undo_newest(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.removed.newest() {
            self.undo_remove(id, cx);
        }
    }

    fn undo_remove(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(offer) = self.removed.take(id) else { return };
        cx.notify();
        super::undo::changed(cx);
        let removed = Arc::new(offer.removed);
        let task = cx.background_executor().spawn({
            let removed = removed.clone();
            async move { remote::restore_account(&removed) }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                if let Err(e) = result {
                    s.fail(forms::explain_as(tr("Der Account wurde nicht vollständig zurückgeholt."), &e));
                    if let Ok(removed) = Arc::try_unwrap(removed) {
                        s.offer_undo(removed, true, cx);
                    }
                }
                cx.dispatch_action(&Reload);
                cx.notify();
            });
        })
        .detach();
    }

    fn filter_shown(&self) -> bool {
        self.accounts.len() > FILTER_FROM
    }

    fn focus_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.filter_shown() {
            return;
        }
        self.list.scroll_to_reveal_item(0);
        self.filter.update(cx, |f, cx| f.take_focus(window, cx));
        cx.notify();
    }

    fn avatar(account: &Account, cx: &App) -> impl IntoElement {
        let (ids, color) = (vec![account.id.to_string()], account.color());
        Button::new(SharedString::from(format!("color-{}", account.id.simple())))
            .ghost()
            .focus_ring(super::theme::rings(cx))
            .p(px(space::XXS))
            .rounded(px(radius::SMALL + space::XXS))
            .child(identity::chip(&account.name, color, skins::head(account).map(ImageSource::from), control::M - space::XXS * 2., font::CALLOUT))
            .tooltip(tr("Farbe"))
            .dropdown_menu(move |menu, _, _| identity::colors(menu, ids.clone(), Some(color)))
    }

    fn row(&mut self, account: &Account, batches: &[String], last: bool, cx: &mut Context<Self>, t: &Theme) -> Div {
        let id = account.id;
        let key = id.simple().to_string();
        let kind = if account.kind == AccountKind::Microsoft { tr("Microsoft") } else { tr("Offline") };
        let usage = if batches.is_empty() { tr("in keinem Batch") } else { trf("in %@", &[&batches.join(", ")]) };
        let removing = self.removing == Some(id);
        let editing = self.editing.as_ref().is_some_and(|e| e.id == id);
        let proxy_chip = account.proxy.as_deref().map(|p| {
            div()
                .id(SharedString::from(format!("proxy-chip-{key}")))
                .flex()
                .items_center()
                .gap(px(space::XXS))
                .px(px(space::XS))
                .rounded(px(radius::SMALL))
                .bg(t.raised)
                .text_size(px(font::CAPTION))
                .text_color(t.secondary)
                .tooltip(tip(forms::proxy_display(p)))
                .child(icon("globe", iconsize::XS, t.secondary))
                .child(tr("Proxy"))
        });
        let actions = if removing {
            div().child(sheet::waiting(tr("Wird entfernt …"), t))
        } else {
            let a = account.clone();
            let b = batches.to_vec();
            div()
                .flex()
                .items_center()
                .gap(px(space::XXS))
                .when(account.kind == AccountKind::Microsoft, |d| {
                    let target = id.to_string();
                    let skin = target.clone();
                    d.child(sheet::icon_button(SharedString::from(format!("again-{key}")), "log-in", tr("Erneut anmelden"), t.secondary, t).on_click(move |_, window, cx| window.dispatch_action(Box::new(SignInAgain(target.clone())), cx)))
                        .child(sheet::icon_button(SharedString::from(format!("skin-{key}")), "shirt", tr("Skin ändern"), t.secondary, t).on_click(move |_, window, cx| window.dispatch_action(Box::new(ChangeSkin(skin.clone())), cx)))
                })
                .child({
                    let a = account.clone();
                    sheet::icon_button(SharedString::from(format!("proxy-{key}")), "globe", tr("Proxy bearbeiten"), if editing { t.accent_text } else { t.secondary }, t).on_click(cx.listener(move |s, _, window, cx| s.edit_proxy(&a, window, cx)))
                })
                .child(sheet::icon_button(SharedString::from(format!("remove-{key}")), "trash", tr("Entfernen"), t.secondary, t).on_click(cx.listener(move |s, _, window, cx| s.ask_remove(&a, &b, window, cx))))
        };
        let line = div()
            .id(SharedString::from(format!("account-{key}")))
            .mark("row", account.name.clone())
            .flex()
            .items_center()
            .gap(px(space::M))
            .px(px(space::L))
            .py(px(space::S))
            .min_h(px(control::L + space::M))
            .child(Self::avatar(account, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().flex().items_center().gap(px(space::S)).child(div().truncate().text_size(px(font::BODY)).font_weight(FontWeight::MEDIUM).text_color(t.primary).child(account.name.clone())).children(proxy_chip))
                    .child(div().truncate().text_size(px(font::CALLOUT)).text_color(t.secondary).child(format!("{kind} · {usage}"))),
            )
            .child(actions)
            .context_menu({
                let (ids, color, microsoft) = (vec![id.to_string()], account.color(), account.kind == AccountKind::Microsoft);
                move |menu, window, cx| {
                    let ids = ids.clone();
                    let menu = if microsoft { menu.menu(tr("Skin ändern…"), Box::new(ChangeSkin(id.to_string()))).separator() } else { menu };
                    menu.submenu(tr("Farbe"), window, cx, move |m, _, _| identity::colors(m, ids.clone(), Some(color)))
                }
            });
        let mut row = div().flex().flex_col().when(!last || editing, |d| d.border_b_1().border_color(t.separator)).child(line);
        if editing && let Some(edit) = self.editing.as_ref() {
            let value = input_value(&edit.input, cx);
            let problem = forms::proxy_problem(&value);
            let blocked = problem.is_some();
            let (check, saving) = (edit.check.clone(), edit.saving);
            let editor = div()
                .id("proxy-editor")
                .px(px(space::L))
                .pb(px(space::M))
                .pl(px(space::L + control::M + space::M))
                .flex()
                .flex_col()
                .gap(px(space::XS))
                .on_action(cx.listener(|s, _: &Confirm, window, cx| {
                    if sheet::typing(window) {
                        s.save_proxy(cx);
                    } else {
                        cx.propagate();
                    }
                }))
                .on_action(cx.listener(|s, _: &Cancel, _, cx| {
                    s.editing = None;
                    cx.notify();
                }))
                .on_key_down(cx.listener(|s, e: &KeyDownEvent, _, cx| {
                    if e.keystroke.key == "escape" {
                        s.editing = None;
                        cx.stop_propagation();
                        cx.notify();
                    }
                }))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(space::S))
                        .child(div().flex_1().min_w_0().child(Input::new(&edit.input)))
                        .child(sheet::button("proxy-row-test", tr("Testen"), None, Look::Secondary, value.is_empty() || blocked, check == Check::Running, t).on_click(cx.listener(|s, _, _, cx| s.test_proxy(cx))))
                        .child(sheet::button("proxy-row-cancel", tr("Abbrechen"), None, Look::Secondary, false, false, t).on_click(cx.listener(|s, _, _, cx| {
                            s.editing = None;
                            cx.notify();
                        })))
                        .child(sheet::button("proxy-row-save", tr("Sichern"), None, Look::Primary, blocked, saving, t).on_click(cx.listener(|s, _, _, cx| s.save_proxy(cx)))),
                )
                .child(sheet::note(Some(tr("Gilt für Anmeldung und Spiel. Leer lassen für keinen Proxy.")), problem, t))
                .children(check_line(&check, self.details.contains("check"), cx.listener(|s, _, _, cx| s.toggle("check", cx)), t));
            row = row.child(editor);
        }
        row
    }

    fn header(&mut self, cx: &mut Context<Self>, t: &Theme) -> Div {
        let count = self.accounts.len();
        let error = self.error.clone().map(|e| {
            div()
                .flex()
                .items_start()
                .gap(px(space::S))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_start()
                        .gap(px(space::S))
                        .px(px(space::M))
                        .py(px(space::S))
                        .rounded(px(radius::CONTROL))
                        .bg(with_alpha(t.danger, tint::ATTENTION))
                        .child(div().flex_1().min_w_0().child(sheet::problem("accounts-error", "circle-alert", t.danger_text, &e, self.details.contains("error"), cx.listener(|s, _, _, cx| s.toggle("error", cx)), t))),
                )
                .child(sheet::icon_button("dismiss-error", "x", tr("Ausblenden"), t.secondary, t).on_click(cx.listener(|s, _, window, cx| {
                    s.error = None;
                    window.focus(&s.root, cx);
                    cx.notify();
                })))
        });
        let prism = self.prism_result(&self.accounts, cx, t);
        let filter = count > FILTER_FROM;
        let any = error.is_some() || prism.is_some() || filter;
        div()
            .flex()
            .flex_col()
            .gap(px(space::L))
            .when(any, |d| d.pb(px(space::L)))
            .children(error)
            .children(prism)
            .when(filter, |d| d.child(Input::new(&self.filter).prefix(icon("search", iconsize::S, t.tertiary)).cleanable(true)))
    }

    fn bar(&mut self, cx: &mut Context<Self>, t: &Theme) -> Div {
        let count = self.accounts.len();
        let count_label = if count == 1 { tr("1 Account") } else { trf("%lld Accounts", &[&count]) };
        let add = primary_button("accounts-add", tr("Account hinzufügen"), "plus", false, t).on_click(|_, window, cx| window.dispatch_action(Box::new(AddAccount), cx));
        div()
            .h(px(shell::TOOLBAR))
            .flex_none()
            .pl(px(toolbar::lead()))
            .pr(px(tile::INSET))
            .flex()
            .items_center()
            .gap(px(space::S))
            .border_b_1()
            .border_color(t.separator)
            .child(toolbar::slot())
            .child(div().flex_1().min_w_0().flex().items_baseline().gap(px(space::S)).child(div().id("accounts-title").mark("heading", tr("Accounts")).text_size(px(font::TITLE3)).font_weight(FontWeight::SEMIBOLD).child(tr("Accounts"))).child(div().min_w_0().truncate().text_size(px(font::CALLOUT)).text_color(t.tertiary).child(count_label)))
            .when(self.prism_found, |d| d.child(self.prism_button(cx, t)))
            .child(add)
    }

    fn item(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let t = Theme::get(cx);
        if ix == 0 {
            let header = self.header(cx, &t);
            return framed(header, true, self.shown.is_empty()).into_any_element();
        }
        let card = div().bg(t.content).border_x_1().border_color(t.separator).overflow_hidden();
        let Some(at) = self.shown.get(ix - 1).copied() else {
            let query = input_value(&self.filter, cx);
            let none = card.border_y_1().rounded(px(radius::CARD)).child(div().px(px(space::L)).py(px(space::M)).text_size(px(font::CALLOUT)).text_color(t.secondary).child(trf("Kein Account passt zu „%@“.", &[&query])));
            return framed(none, false, true).into_any_element();
        };
        let account = self.accounts[at].clone();
        let batches = self.usage.get(&account.id).cloned().unwrap_or_default();
        let (first, last) = (ix == 1, ix == self.shown.len());
        let content = self.row(&account, &batches, last, cx, &t);
        let card = card.when(first, |d| d.border_t_1().rounded_t(px(radius::CARD))).when(last, |d| d.border_b_1().rounded_b(px(radius::CARD))).child(content);
        framed(card, false, last).into_any_element()
    }
}

fn framed(child: impl IntoElement, top: bool, bottom: bool) -> Div {
    div().w_full().flex().justify_center().child(div().w_full().max_w(px(PAGE_MAX)).px(px(space::XL)).when(top, |d| d.pt(px(space::XL))).when(bottom, |d| d.pb(px(space::XL))).child(child))
}

impl Render for AccountsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::get(cx);
        self.refresh(cx);
        let root = div().id("accounts-page").size_full().bg(t.window).text_color(t.primary).font_family(Theme::font_family()).text_size(px(font::BODY)).track_focus(&self.root);
        if self.accounts.is_empty() {
            let add = primary_button("accounts-add", tr("Account hinzufügen"), "plus", false, &t).on_click(|_, window, cx| window.dispatch_action(Box::new(AddAccount), cx));
            let title = tr("Hier ist es noch still");
            return root
                .overflow_y_scroll()
                .relative()
                .flex()
                .items_center()
                .justify_center()
                .px(px(space::XL))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(space::M))
                        .child(icon("users", iconsize::HERO, t.tertiary))
                        .child(div().id("accounts-empty").mark("heading", title.clone()).text_size(px(font::TITLE2)).font_weight(FontWeight::SEMIBOLD).child(title))
                        .child(div().max_w(px(control::READING)).text_center().text_size(px(font::CALLOUT)).text_color(t.secondary).child(tr("Füge ein Microsoft-Konto hinzu oder ein Offline-Konto für Server ohne Online-Mode.")))
                        .child(div().mt(px(space::S)).flex().items_center().gap(px(space::S)).child(add).when(self.prism_found, |d| d.child(self.prism_button(cx, &t))))
                        .children(self.prism_result(&self.accounts, cx, &t).map(|c| div().mt(px(space::S)).w_full().max_w(px(control::READING)).child(c))),
                )
                .into_any_element();
        }
        self.sync_heights(cx);
        let bar = self.bar(cx, &t);
        root.flex().flex_col().child(bar).child(div().flex_1().min_h_0().child(list(self.list.clone(), cx.processor(|s, ix: usize, _, cx| s.item(ix, cx))).size_full())).into_any_element()
    }
}

const PAGE_MAX: f32 = 760.;
const FILTER_FROM: usize = 12;
const OVERDRAW: f32 = 480.;

#[cfg(test)]
mod tests {
    use super::Pending;

    #[test]
    fn expiring_one_notice_keeps_the_others() {
        let mut p = Pending::new();
        let a = p.push("a");
        let b = p.push("b");
        assert_eq!(p.take(a), Some("a"));
        assert_eq!(p.newest(), Some(b));
        assert_eq!(p.take(a), None);
    }

    #[test]
    fn undoing_picks_the_notice_asked_for() {
        let mut p = Pending::new();
        let a = p.push(1);
        let b = p.push(2);
        let c = p.push(3);
        assert_eq!(p.take(b), Some(2));
        assert_eq!(p.newest(), Some(c));
        assert_eq!(p.take(c), Some(3));
        assert_eq!(p.newest(), Some(a));
        assert!(!p.items.is_empty());
    }

    #[test]
    fn a_reoffered_notice_gets_a_fresh_id() {
        let mut p = Pending::new();
        let a = p.push("a");
        p.take(a);
        let again = p.push("a");
        assert_ne!(a, again);
        assert_eq!(p.take(a), None);
    }
}
