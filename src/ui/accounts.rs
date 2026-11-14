//! Accounts: the list with proxies and the sheet to add a Microsoft or offline account.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui_kit::base::actions::{Cancel, Confirm};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::*;
use uuid::Uuid;

use super::actions::{AddAccount, AddMicrosoft, Reload, SignInAgain};
use super::drive::Mark;
use super::forms;
use super::motion::Soft;
use super::remote::{self, SignIn, SignInError};
use super::sheet::{self, Dismissed, Look};
use super::theme::{Theme, control, font, iconsize, radius, space, tint, with_alpha};
use super::widgets::{KeyNav, icon, primary_button, tip};
use crate::core::account::{Account, AccountKind};
use crate::core::drive;
use crate::core::i18n::{tr, trf};
use crate::core::prism::AccountImport;

const COPIED_FOR: Duration = Duration::from_secs(2);

/// Registers the account actions.
pub fn init(cx: &mut App) {
    cx.on_action(|_: &AddAccount, cx| {
        sheet::in_window(cx, |window, cx| open_add(None, false, window, cx));
    });
    cx.on_action(|_: &AddMicrosoft, cx| {
        sheet::in_window(cx, |window, cx| open_add(None, true, window, cx));
    });
    cx.on_action(|action: &SignInAgain, cx| {
        let Ok(id) = Uuid::parse_str(&action.0) else { return };
        let account = super::model::get(cx).read(cx).account(id).cloned();
        if let Some(account) = account.filter(|a| a.kind == AccountKind::Microsoft) {
            sheet::in_window(cx, move |window, cx| open_add(Some(account), false, window, cx));
        }
    });
}

fn open_add(again: Option<Account>, microsoft: bool, window: &mut Window, cx: &mut App) {
    if sheet::busy(window, cx) {
        return;
    }
    let view = cx.new(|cx| {
        let mut sheet = AddAccountSheet::new(again, window, cx);
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

struct Page(Entity<AccountsPage>);

impl Global for Page {}

#[derive(Clone, PartialEq)]
enum Check {
    Idle,
    Running,
    Passed,
    Failed(String),
}

fn check_line(check: &Check, t: &Theme) -> Option<Div> {
    match check {
        Check::Idle => None,
        Check::Running => Some(sheet::waiting(tr("Proxy wird geprüft …"), t)),
        Check::Passed => Some(sheet::status("circle-check", t.success_text, tr("Der Proxy erreicht das Internet."))),
        Check::Failed(why) => Some(sheet::status("circle-alert", t.danger_text, trf("Kein Internet über diesen Proxy: %@. Prüfe Adresse, Port und Zugangsdaten.", &[why]))),
    }
}

fn start_check<V: 'static>(proxy: String, cx: &mut Context<V>, apply: impl Fn(&mut V, Check) + 'static) {
    let task = cx.background_executor().spawn(async move { remote::test_proxy(&proxy).map_err(|e| short_error(&e)) });
    cx.spawn(async move |this, cx| {
        let result = task.await;
        let _ = this.update(cx, |v, cx| {
            apply(v, result.map_or_else(Check::Failed, |()| Check::Passed));
            cx.notify();
        });
    })
    .detach();
}

fn short_error(e: &anyhow::Error) -> String {
    e.root_cause().to_string()
}

fn input_value(input: &Entity<InputState>, cx: &App) -> String {
    input.read(cx).value().trim().to_string()
}

enum Step {
    Choose,
    Microsoft(Ms),
    Offline,
}

enum Ms {
    Requesting,
    Code { user_code: String, uri: String, expires_in: u64 },
    Finishing,
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
    save_error: Option<String>,
    copied: bool,
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
    fn new(again: Option<Account>, window: &mut Window, cx: &mut Context<Self>) -> Self {
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
            copied: false,
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
        cx.notify();
    }

    fn start_microsoft(&mut self, cx: &mut Context<Self>) {
        self.cancel.store(true, Ordering::SeqCst);
        self.cancel = Arc::new(AtomicBool::new(false));
        self.copied = false;
        self.go(Step::Microsoft(Ms::Requesting), cx);
        let proxy = match &self.again {
            Some(account) => account.proxy.clone(),
            None => self.chosen_proxy(cx),
        };
        let expected = self.again.as_ref().map(|a| a.id);
        let (tx, rx) = async_channel::unbounded();
        let cancel = self.cancel.clone();
        std::thread::spawn(move || remote::sign_in(proxy, expected, cancel, tx));
        let cancel = self.cancel.clone();
        self._pump = Some(cx.spawn(async move |this, cx| {
            while let Ok(step) = rx.recv().await {
                if cancel.load(Ordering::SeqCst) {
                    return;
                }
                let done = matches!(step, SignIn::Done);
                let _ = this.update(cx, |s, cx| s.advance(step, cx));
                if done {
                    return;
                }
            }
        }));
    }

    fn advance(&mut self, step: SignIn, cx: &mut Context<Self>) {
        match step {
            SignIn::Code { user_code, uri, expires_in } => self.go(Step::Microsoft(Ms::Code { user_code, uri, expires_in }), cx),
            SignIn::Approved => self.go(Step::Microsoft(Ms::Finishing), cx),
            SignIn::Failed(e) => self.go(Step::Microsoft(Ms::Failed(e)), cx),
            SignIn::Done => {
                cx.dispatch_action(&Reload);
                cx.emit(Dismissed);
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
        let Step::Microsoft(Ms::Code { user_code, uri, .. }) = &self.step else { return };
        let (code, uri) = (user_code.clone(), uri.clone());
        self.copy(code, cx);
        if !drive::fenced("open", &uri) {
            cx.open_url(&uri);
        }
    }

    fn copy(&mut self, code: String, cx: &mut Context<Self>) {
        if !drive::fenced("clipboard", &code) {
            cx.write_to_clipboard(ClipboardItem::new_string(code));
        }
        self.copied = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COPIED_FOR).await;
            let _ = this.update(cx, |s, cx| {
                s.copied = false;
                cx.notify();
            });
        })
        .detach();
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
                        cx.emit(Dismissed);
                    }
                    Err(e) => {
                        s.save_error = Some(e.to_string());
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
            Step::Offline => self.name.update(cx, |s, cx| s.focus(window, cx)),
            Step::Choose => {
                let first = self.first.clone();
                window.on_next_frame(move |window, cx| window.focus(&first, cx));
            }
            Step::Microsoft(Ms::Code { .. } | Ms::Failed(_)) => window.focus(&self.main, cx),
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
        let microsoft = option("choose-microsoft", "key-round", tr("Microsoft-Konto"), tr("Für alle Server. Du meldest dich im Browser an."), Some(&self.first))
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
                    s.proxy.update(cx, |p, cx| p.focus(window, cx));
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
            .children(check_line(&self.check, t));
        let body = div().flex().flex_col().gap(px(space::S)).child(microsoft).child(offline).child(div().pt(px(space::S)).child(disclosure)).when(open, |d| d.child(proxy_field));
        let cancel = sheet::button("cancel", tr("Abbrechen"), None, Look::Secondary, false, false, t).on_click(cx.listener(|_, _, _, cx| cx.emit(Dismissed)));
        (body, Vec::new(), vec![cancel.into_any_element()])
    }

    fn render_offline(&mut self, cx: &mut Context<Self>, t: &Theme) -> (Div, Vec<AnyElement>, Vec<AnyElement>) {
        let name = input_value(&self.name, cx);
        let accounts = super::model::get(cx).read(cx).accounts.clone();
        let problem = forms::offline_name_problem(&name, &accounts);
        let shown = self.save_error.clone().or(problem.clone().filter(|_| !name.is_empty()));
        let mut body = div().flex().flex_col().gap(px(space::M)).child(sheet::field(tr("Name"), Input::new(&self.name), Some(tr("3 bis 16 Zeichen: Buchstaben, Ziffern und _")), shown, t));
        if let Some(proxy) = self.chosen_proxy(cx) {
            body = body.child(div().text_size(px(font::CALLOUT)).text_color(t.secondary).child(trf("Proxy: %@", &[&forms::proxy_display(&proxy)])));
        }
        let back = sheet::button("back", tr("Zurück"), Some("chevron-left"), Look::Secondary, false, false, t).on_click(cx.listener(|s, _, _, cx| s.back(cx)));
        let cancel = sheet::button("cancel", tr("Abbrechen"), None, Look::Secondary, false, false, t).on_click(cx.listener(|_, _, _, cx| cx.emit(Dismissed)));
        let add = sheet::button("add-offline", tr("Hinzufügen"), None, Look::Primary, problem.is_some(), self.saving, t).on_click(cx.listener(|s, _, _, cx| s.add_offline(cx)));
        (body, vec![back.into_any_element()], vec![cancel.into_any_element(), add.into_any_element()])
    }

    fn render_microsoft(&mut self, cx: &mut Context<Self>, t: &Theme) -> (Div, Vec<AnyElement>, Vec<AnyElement>) {
        let mut trailing = vec![sheet::button("cancel", tr("Abbrechen"), None, Look::Secondary, false, false, t).on_click(cx.listener(|_, _, _, cx| cx.emit(Dismissed))).into_any_element()];
        let leading = if self.again.is_none() { vec![sheet::button("back", tr("Zurück"), Some("chevron-left"), Look::Secondary, false, false, t).on_click(cx.listener(|s, _, _, cx| s.back(cx))).into_any_element()] } else { Vec::new() };
        let Step::Microsoft(ms) = &self.step else { return (div(), leading, trailing) };
        let body = match ms {
            Ms::Requesting => div().py(px(space::L)).child(sheet::waiting(tr("Code wird angefordert …"), t)),
            Ms::Finishing => div().py(px(space::L)).child(sheet::waiting(tr("Anmeldung wird abgeschlossen …"), t)),
            Ms::Code { user_code, expires_in, .. } => {
                let code = user_code.clone();
                let copy = sheet::icon_button("copy-code", if self.copied { "check" } else { "copy" }, if self.copied { tr("Kopiert") } else { tr("Code kopieren") }, if self.copied { t.success_text } else { t.secondary }, t).on_click(cx.listener(move |s, _, _, cx| s.copy(code.clone(), cx)));
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
                trailing.push(sheet::button("open-page", tr("Seite öffnen"), Some("external-link"), Look::Primary, false, false, t).track_focus(&self.main).on_click(cx.listener(|s, _, _, cx| s.open_page(cx))).into_any_element());
                div()
                    .flex()
                    .flex_col()
                    .gap(px(space::M))
                    .child(div().text_size(px(font::BODY)).text_color(t.primary).child(tr("Öffne die Microsoft-Seite und gib diesen Code ein. Danach geht es hier von selbst weiter.")))
                    .child(code_box)
                    .child(if self.copied { sheet::status("check", t.success_text, tr("Code kopiert. Auf der Seite einfügen.")) } else { sheet::waiting(tr("Warte auf deine Anmeldung im Browser …"), t) })
                    .child(div().text_size(px(font::CAPTION)).text_color(t.tertiary).child(trf("Der Code gilt %lld Minuten.", &[&minutes])))
            }
            Ms::Failed(e) => {
                let (title, detail, retry, offline) = failure_text(e, self.again.as_ref());
                if offline {
                    trailing.push(sheet::button("to-offline", tr("Offline-Konto hinzufügen"), None, Look::Primary, false, false, t).track_focus(&self.main).on_click(cx.listener(|s, _, _, cx| s.go(Step::Offline, cx))).into_any_element());
                } else {
                    trailing.push(sheet::button("retry", retry, Some("refresh-cw"), Look::Primary, false, false, t).track_focus(&self.main).on_click(cx.listener(|s, _, _, cx| s.start_microsoft(cx))).into_any_element());
                }
                let icon_name = if matches!(e, SignInError::Network) { "wifi-off" } else { "circle-alert" };
                div()
                    .flex()
                    .gap(px(space::M))
                    .py(px(space::S))
                    .child(icon(icon_name, iconsize::L, t.danger_text))
                    .child(div().flex_1().min_w_0().flex().flex_col().gap(px(space::XS)).child(div().id("sign-in-error").mark("error", title.clone()).text_size(px(font::BODY)).font_weight(FontWeight::SEMIBOLD).text_color(t.primary).child(title)).child(div().text_size(px(font::CALLOUT)).text_color(t.secondary).child(detail)))
            }
        };
        (body, leading, trailing)
    }
}

fn failure_text(e: &SignInError, again: Option<&Account>) -> (String, String, String, bool) {
    let retry = tr("Erneut versuchen");
    match e {
        SignInError::Expired => (tr("Der Code ist abgelaufen."), tr("Fordere einen neuen Code an und gib ihn innerhalb der Frist ein."), tr("Neuer Code"), false),
        SignInError::Declined => (tr("Die Anmeldung wurde im Browser abgebrochen."), tr("Versuche es erneut, wenn du bereit bist."), retry, false),
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
        SignInError::Other(detail) => (tr("Die Anmeldung hat nicht geklappt."), detail.clone(), retry, false),
    }
}

impl Render for AddAccountSheet {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.apply_focus(window, cx);
        let t = Theme::get(cx);
        let (title, subtitle) = match (&self.step, &self.again) {
            (Step::Microsoft(_), Some(a)) => (tr("Erneut anmelden"), Some(a.name.clone())),
            (Step::Microsoft(_), None) => (tr("Mit Microsoft anmelden"), None),
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
    editing: Option<Editing>,
    removing: Option<Uuid>,
    error: Option<String>,
    prism_found: bool,
    prism_busy: bool,
    prism_done: Option<AccountImport>,
    filter: Entity<InputState>,
    _model: Subscription,
    _filter: Subscription,
}

impl AccountsPage {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = super::model::get(cx);
        let _model = cx.observe(&model, |_, _, cx| cx.notify());
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder(tr("Accounts filtern")));
        let _filter = cx.subscribe(&filter, |_, _, e: &InputEvent, cx| {
            if matches!(e, InputEvent::Change) {
                cx.notify();
            }
        });
        let mut page = Self { editing: None, removing: None, error: None, prism_found: false, prism_busy: false, prism_done: None, filter, _model, _filter };
        page.look_for_prism(cx);
        page
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
                    Err(e) => s.error = Some(trf("Aus Prism nicht übernommen: %@", &[&e])),
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
            .map(|(name, why)| trf("„%@“ übersprungen: %@", &[name, &forms::offline_name_problem(name, &[]).unwrap_or_else(|| why.clone())]))
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
                    .child(sheet::icon_button("prism-dismiss", "x", tr("Ausblenden"), t.secondary, t).on_click(cx.listener(|s, _, _, cx| {
                        s.prism_done = None;
                        cx.notify();
                    }))),
            );
        if !waiting.is_empty() {
            card = card.child(sheet::note(Some(tr("Diese Microsoft-Accounts brauchen eine neue Anmeldung. Anmeldungen aus Prism werden nicht übernommen.")), None, t));
            for name in waiting {
                card = card.child(
                    div()
                        .id(SharedString::from(format!("prism-wait-{name}")))
                        .mark("row", name.clone())
                        .flex()
                        .items_center()
                        .gap(px(space::M))
                        .child(icon("key-round", iconsize::S, t.secondary))
                        .child(div().flex_1().min_w_0().truncate().text_size(px(font::BODY)).font_weight(FontWeight::MEDIUM).child(name.clone()))
                        .child(sheet::button(SharedString::from(format!("prism-signin-{name}")), tr("Anmelden"), Some("log-in"), Look::Secondary, false, false, t).on_click(|_, window, cx| window.dispatch_action(Box::new(AddMicrosoft), cx))),
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
        window.on_next_frame(move |window, cx| focus.update(cx, |s, cx| s.focus(window, cx)));
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
                        s.error = Some(trf("Der Proxy wurde nicht gespeichert: %@", &[&e]));
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
        sheet::ask(window, cx, title, message, tr("Entfernen"), move |_, cx| {
            let _ = page.update(cx, |s, cx| s.remove(id, cx));
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
                if let Err(e) = result {
                    s.error = Some(trf("Der Account wurde nicht vollständig entfernt: %@", &[&e]));
                }
                cx.dispatch_action(&Reload);
                cx.notify();
            });
        })
        .detach();
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
                    d.child(sheet::icon_button(SharedString::from(format!("again-{key}")), "log-in", tr("Erneut anmelden"), t.secondary, t).on_click(move |_, window, cx| window.dispatch_action(Box::new(SignInAgain(target.clone())), cx)))
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
            .child(
                div()
                    .size(px(control::M))
                    .flex_none()
                    .rounded(px(radius::FULL))
                    .bg(t.raised)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon(if account.kind == AccountKind::Microsoft { "key-round" } else { "user" }, iconsize::S, t.secondary)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().flex().items_center().gap(px(space::S)).child(div().truncate().text_size(px(font::BODY)).font_weight(FontWeight::MEDIUM).text_color(t.primary).child(account.name.clone())).children(proxy_chip))
                    .child(div().truncate().text_size(px(font::CALLOUT)).text_color(t.secondary).child(format!("{kind} · {usage}"))),
            )
            .child(actions);
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
                .children(check_line(&check, t));
            row = row.child(editor);
        }
        row
    }
}

impl Render for AccountsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::get(cx);
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
        if self.editing.as_ref().is_some_and(|e| !accounts.iter().any(|a| a.id == e.id)) {
            self.editing = None;
        }
        let root = div().id("accounts-page").size_full().overflow_y_scroll().bg(t.window).text_color(t.primary).font_family(Theme::font_family()).text_size(px(font::BODY));
        let add = primary_button("accounts-add", tr("Account hinzufügen"), "plus", false, &t).on_click(|_, window, cx| window.dispatch_action(Box::new(AddAccount), cx));
        if accounts.is_empty() {
            let title = tr("Noch keine Accounts");
            return root.flex().items_center().justify_center().px(px(space::XL)).child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(space::M))
                    .child(icon("users", iconsize::HERO, t.tertiary))
                    .child(div().id("accounts-empty").mark("heading", title.clone()).text_size(px(font::TITLE2)).font_weight(FontWeight::SEMIBOLD).child(title))
                    .child(div().max_w(px(control::READING)).text_center().text_size(px(font::CALLOUT)).text_color(t.secondary).child(tr("Füge ein Microsoft-Konto hinzu oder ein Offline-Konto für Server ohne Online-Mode.")))
                    .child(div().mt(px(space::S)).flex().items_center().gap(px(space::S)).child(add).when(self.prism_found, |d| d.child(self.prism_button(cx, &t))))
                    .children(self.prism_result(&accounts, cx, &t).map(|c| div().mt(px(space::S)).w_full().max_w(px(control::READING)).child(c))),
            );
        }
        let count = accounts.len();
        let none: Vec<String> = Vec::new();
        let many = count > FILTER_FROM;
        let query = if many { input_value(&self.filter, cx).to_lowercase() } else { String::new() };
        let shown: Vec<&Account> = accounts.iter().filter(|a| query.is_empty() || a.name.to_lowercase().contains(&query)).collect();
        let mut list = div().flex().flex_col().rounded(px(radius::CARD)).bg(t.content).border_1().border_color(t.separator).overflow_hidden();
        for (i, account) in shown.iter().enumerate() {
            let batches = usage.get(&account.id).unwrap_or(&none);
            list = list.child(self.row(account, batches, i + 1 == shown.len(), cx, &t));
        }
        if shown.is_empty() {
            list = list.child(div().px(px(space::L)).py(px(space::M)).text_size(px(font::CALLOUT)).text_color(t.secondary).child(trf("Kein Account passt zu „%@“.", &[&query])));
        }
        let error = self.error.clone().map(|e| {
            div()
                .flex()
                .items_start()
                .gap(px(space::S))
                .child(div().flex_1().min_w_0().child(sheet::notice("circle-alert", t.danger, t.danger_text, e)))
                .child(sheet::icon_button("dismiss-error", "x", tr("Ausblenden"), t.secondary, &t).on_click(cx.listener(|s, _, _, cx| {
                    s.error = None;
                    cx.notify();
                })))
        });
        let count_label = if count == 1 { tr("1 Account") } else { trf("%lld Accounts", &[&count]) };
        root.child(
            div()
                .mx_auto()
                .w_full()
                .max_w(px(PAGE_MAX))
                .px(px(space::XL))
                .py(px(space::XL))
                .flex()
                .flex_col()
                .gap(px(space::L))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(space::M))
                        .child(div().flex_1().flex().items_baseline().gap(px(space::S)).child(div().id("accounts-title").mark("heading", tr("Accounts")).text_size(px(font::TITLE)).font_weight(FontWeight::BOLD).child(tr("Accounts"))).child(div().text_size(px(font::CALLOUT)).text_color(t.tertiary).child(count_label)))
                        .when(self.prism_found, |d| d.child(self.prism_button(cx, &t)))
                        .child(add),
                )
                .children(error)
                .children(self.prism_result(&accounts, cx, &t))
                .when(many, |d| d.child(Input::new(&self.filter).prefix(icon("search", iconsize::S, t.tertiary)).cleanable(true)))
                .child(list),
        )
    }
}

const PAGE_MAX: f32 = 760.;
const FILTER_FROM: usize = 12;
