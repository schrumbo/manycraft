//! Account hinzufügen: a Microsoft sign-in or offline names, one at a time or as a pasted list, with an optional proxy.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use freya::clipboard::Clipboard;
use freya::prelude::*;
use freya_core::prelude::NavigationMode;
use uuid::Uuid;

use crate::core::account::{Account, AccountKind};
use crate::core::browser;
use crate::core::i18n::{tr, trf};
use crate::ui::forms::{self, Listed, Problem};
use crate::ui::remote::{self, CodeStep, SignIn, SignInError, Via};
use crate::view::drive::use_spot;
use crate::view::icons::{Icon, icon};
use crate::view::kit::button::Button;
use crate::view::kit::field::Field;
use crate::view::kit::group::note;
use crate::view::kit::sheet::{self, Card, Primary};
use crate::view::kit::progress::Wait;
use crate::view::kit::text::{line, mono, para, strong};
use crate::view::kit::{after, at_least, off_thread, open_external, ring, toast, use_theme};
use crate::view::store::{Store, use_store};
use crate::view::theme::{Theme, control, font, frame, iconsize, space};

const WIDTH: f32 = 440.;
const COPIED_FOR: Duration = Duration::from_secs(2);
const LIST_ROWS: (usize, usize) = (3, 8);
const LIST_PROBLEMS: usize = 4;
const DETAIL_LINES: usize = 8;
const ADDED_SHOWN: usize = 3;
const TILE_WASH: f32 = 0.14;
const ADDED_NOTES: f32 = font::BODY * font::LINE * 2. + font::CAPTION * font::LINE * 2. + space::XS * 2.;
const CODE_PAD: f32 = space::L;

/// How the add-account sheet starts.
#[derive(Clone)]
pub enum Start {
    /// The choice between a Microsoft and an offline account.
    Choose,
    /// The choice with the offline account picked and its name field open.
    Offline,
    /// Straight into the Microsoft sign-in.
    Microsoft,
    /// Signs in the Microsoft accounts with these ids again, one after another.
    Again(Vec<Uuid>),
    /// Signs in Microsoft accounts by these names one after another, such as the ones Prism listed.
    Names(Vec<String>),
}

/// Opens the add-account sheet.
pub fn open(store: Store, start: Start) {
    let boot = match start {
        Start::Choose => Boot::default(),
        Start::Offline => Boot { offline: true, ..Boot::default() },
        Start::Microsoft => Boot { microsoft: true, ..Boot::default() },
        Start::Names(queue) => Boot { microsoft: true, pending: Pending { queue, ..Pending::default() }, ..Boot::default() },
        Start::Again(ids) => {
            let model = store.model.peek();
            let mut accounts: Vec<Account> = ids.iter().filter_map(|id| model.account(*id)).filter(|a| a.kind == AccountKind::Microsoft).cloned().collect();
            if accounts.is_empty() {
                return;
            }
            let first = accounts.remove(0);
            Boot { microsoft: true, pending: Pending { again: Some(first), later: accounts, queue: Vec::new() }, ..Boot::default() }
        }
    };
    sheet::open(store.ui, AddAccount { boot });
}

#[derive(Clone, Default, PartialEq)]
struct Boot {
    microsoft: bool,
    offline: bool,
    pending: Pending,
}

/// The accounts still to sign in: the one being renewed with those after it, or the names asked for.
#[derive(Clone, Default, PartialEq)]
struct Pending {
    again: Option<Account>,
    later: Vec<Account>,
    queue: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Done {
    Finished,
    Next,
    Added,
}

impl Pending {
    fn done(&mut self, name: &str) -> Done {
        if self.again.is_some() {
            if self.later.is_empty() {
                return Done::Finished;
            }
            self.again = Some(self.later.remove(0));
            return Done::Next;
        }
        self.queue.retain(|n| !n.eq_ignore_ascii_case(name));
        Done::Added
    }

    fn skip(&mut self) {
        if self.again.is_some() && !self.later.is_empty() {
            self.again = Some(self.later.remove(0));
        } else if !self.queue.is_empty() {
            self.queue.remove(0);
        }
    }

    fn skippable(&self) -> bool {
        (self.queue.len() > 1 && self.again.is_none()) || (self.again.is_some() && !self.later.is_empty())
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Check {
    Idle,
    Running,
    Passed,
    Failed(Problem),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Copied {
    Code,
    Link,
}

#[derive(Clone, PartialEq)]
enum Step {
    Choose,
    Offline,
    Microsoft(Ms),
}

#[derive(Clone, PartialEq)]
enum Ms {
    Requesting,
    Window,
    Code(CodeStep),
    Finishing,
    Added(String),
    Failed(SignInError),
}

type Token = Rc<RefCell<Arc<AtomicBool>>>;

#[derive(Clone, Copy)]
struct Flow {
    store: Store,
    step: State<Step>,
    pending: State<Pending>,
    added: State<Vec<String>>,
    copied: State<Option<Copied>>,
    windowed: State<bool>,
    via: State<Via>,
    opened_private: State<bool>,
    private_left_open: State<bool>,
    details: State<bool>,
    name: State<String>,
    proxy: State<String>,
    advanced: State<bool>,
    check: State<Check>,
    saving: State<bool>,
    save_error: State<Option<Problem>>,
    cancel: State<Token>,
}

fn proxy_failure(e: &anyhow::Error) -> Problem {
    Problem { text: tr("Über diesen Proxy kommt kein Internet. Prüfe Adresse, Port und Zugangsdaten."), detail: forms::explain(e).detail }
}

fn first_run(store: Store) -> bool {
    let m = store.model.peek();
    m.loaded && m.instances.is_empty() && m.batches.is_empty()
}

impl Flow {
    fn alive(self) -> bool {
        self.step.try_peek().is_some()
    }

    fn go(mut self, step: Step) {
        self.details.set(false);
        self.step.set(step);
    }

    fn stop(self) {
        self.cancel.peek().borrow().store(true, Ordering::SeqCst);
    }

    fn fresh(self) -> Arc<AtomicBool> {
        let cell = self.cancel.peek().clone();
        let mut slot = cell.borrow_mut();
        slot.store(true, Ordering::SeqCst);
        let token = Arc::new(AtomicBool::new(false));
        *slot = Arc::clone(&token);
        token
    }

    fn chosen_proxy(self) -> Option<String> {
        Some(self.proxy.peek().trim().to_owned()).filter(|p| !p.is_empty())
    }

    fn proxy_problem(self) -> Option<String> {
        forms::proxy_problem(self.proxy.peek().trim())
    }

    fn back(mut self) {
        self.stop();
        self.save_error.set(None);
        self.go(Step::Choose);
    }

    fn start(mut self, via: Via) {
        let token = self.fresh();
        self.copied.set(None);
        self.opened_private.set(false);
        self.via.set(via);
        let proxy = match &self.pending.peek().again {
            Some(account) => account.proxy.clone(),
            None => self.chosen_proxy(),
        };
        self.windowed.set(remote::uses_window(proxy.as_deref(), via));
        self.go(Step::Microsoft(Ms::Requesting));
        let expected = self.pending.peek().again.as_ref().map(|a| a.id);
        let (tx, rx) = async_channel::unbounded();
        let run = Arc::clone(&token);
        if std::thread::Builder::new().name("sign-in".into()).spawn(move || remote::sign_in(proxy, expected, via, run, tx)).is_err() {
            self.go(Step::Microsoft(Ms::Failed(SignInError::Other(Problem::plain(tr("Die Anmeldung ließ sich nicht starten."))))));
            return;
        }
        spawn_in_window(async move {
            while let Ok(step) = rx.recv().await {
                if token.load(Ordering::SeqCst) || !self.alive() {
                    return;
                }
                let done = matches!(step, SignIn::Done(_));
                self.advance(step);
                if done {
                    return;
                }
            }
        });
    }

    fn advance(mut self, step: SignIn) {
        match step {
            SignIn::Window => self.go(Step::Microsoft(Ms::Window)),
            SignIn::Code(step) => {
                let open = step.open;
                self.windowed.set(false);
                self.via.set(Via::Browser);
                self.go(Step::Microsoft(Ms::Code(step)));
                if open {
                    self.open_page();
                }
            }
            SignIn::Approved => self.go(Step::Microsoft(Ms::Finishing)),
            SignIn::Failed(e) => self.go(Step::Microsoft(Ms::Failed(e))),
            SignIn::Done(name) => {
                self.store.reload(true);
                let outcome = self.pending.write().done(&name);
                match outcome {
                    Done::Finished => sheet::close(self.store.ui),
                    Done::Next => self.start(Via::Window),
                    Done::Added => {
                        let left = *self.opened_private.peek();
                        if !left && self.pending.peek().queue.is_empty() && self.added.peek().is_empty() {
                            self.done(trf("%@ hinzugefügt", &[&name]), Start::Microsoft);
                            return;
                        }
                        self.private_left_open.set(left);
                        self.added.write().push(name.clone());
                        self.go(Step::Microsoft(Ms::Added(name)));
                    }
                }
            }
        }
    }

    fn skip(mut self) {
        self.pending.write().skip();
        let via = *self.via.peek();
        self.start(via);
    }

    fn open_page(mut self) {
        let (link, private) = {
            let step = self.step.peek();
            let Step::Microsoft(Ms::Code(code)) = &*step else { return };
            (code.link.clone(), code.private)
        };
        if crate::core::drive::fenced(if private { "open private" } else { "open" }, &link) {
            if private {
                self.opened_private.set(true);
            }
            return;
        }
        if !private {
            open_external("open", link);
            return;
        }
        let target = link.clone();
        let Some(rx) = off_thread("open-private", move || browser::open_private(&target)) else {
            open_external("open", link);
            return;
        };
        spawn_in_window(async move {
            if matches!(rx.recv().await, Ok(Ok(true))) {
                if self.alive() {
                    self.opened_private.set(true);
                }
            } else {
                open_external("open", link);
            }
        });
    }

    fn copy(mut self, text: String, what: Copied) {
        if !crate::core::drive::fenced("clipboard", &text) && Clipboard::set(text).is_err() {
            toast::show(self.store.ui, tr("Kopieren hat nicht geklappt."), None);
            return;
        }
        self.copied.set(Some(what));
        spawn_in_window(async move {
            after(COPIED_FOR).await;
            if self.alive() && *self.copied.peek() == Some(what) {
                self.copied.set(None);
            }
        });
    }

    fn copy_link(self) {
        let link = match &*self.step.peek() {
            Step::Microsoft(Ms::Code(code)) => code.link.clone(),
            _ => return,
        };
        self.copy(link, Copied::Link);
    }

    fn offline_names(self) -> Vec<String> {
        let text = self.name.peek().clone();
        let model = self.store.model.peek();
        if text.contains('\n') {
            return forms::check_names(&text, &model.accounts).into_iter().filter(|(_, v)| *v == Listed::Fine).map(|(n, _)| n).collect();
        }
        let name = text.trim().to_owned();
        forms::offline_name_problem(&name, &model.accounts).is_none().then_some(name).into_iter().collect()
    }

    fn add_offline(mut self) {
        if *self.saving.peek() || !matches!(*self.step.peek(), Step::Offline) {
            return;
        }
        let names = self.offline_names();
        if names.is_empty() {
            return;
        }
        let proxy = self.chosen_proxy();
        self.saving.set(true);
        let Some(rx) = off_thread("add-offline", move || remote::add_offline_many(&names, proxy)) else {
            self.saving.set(false);
            self.save_error.set(Some(Problem::plain(tr("Das Hinzufügen ließ sich nicht starten."))));
            return;
        };
        spawn_in_window(async move {
            let result = rx.recv().await;
            if matches!(&result, Ok(Ok(added)) if !added.is_empty()) {
                self.store.reload(true);
            }
            if !self.alive() {
                return;
            }
            self.saving.set(false);
            match result {
                Ok(Ok(accounts)) if !accounts.is_empty() => {
                    let text = match accounts.as_slice() {
                        [only] => trf("%@ hinzugefügt", &[&only.name]),
                        _ => trf("%lld Offline-Accounts hinzugefügt.", &[&accounts.len()]),
                    };
                    self.done(text, Start::Offline);
                }
                Ok(Ok(_)) => self.save_error.set(Some(Problem::plain(tr("Diese Namen gibt es inzwischen schon.")))),
                Ok(Err(e)) => self.save_error.set(Some(forms::explain(&e))),
                Err(_) => self.save_error.set(Some(Problem::plain(tr("Das Hinzufügen ließ sich nicht abschließen.")))),
            }
        });
    }

    fn run_check(mut self) {
        let proxy = self.proxy.peek().trim().to_owned();
        if *self.check.peek() == Check::Running || proxy.is_empty() || forms::proxy_problem(&proxy).is_some() {
            return;
        }
        self.check.set(Check::Running);
        let asked = proxy.clone();
        let Some(rx) = off_thread("proxy-check", move || remote::test_proxy(&proxy).map_err(|e| proxy_failure(&e))) else {
            self.check.set(Check::Failed(Problem::plain(tr("Die Prüfung ließ sich nicht starten."))));
            return;
        };
        spawn_in_window(async move {
            let result = rx.recv().await;
            if !self.alive() || *self.check.peek() != Check::Running || self.proxy.peek().trim() != asked {
                return;
            }
            self.check.set(match result {
                Ok(Ok(())) => Check::Passed,
                Ok(Err(problem)) => Check::Failed(problem),
                Err(_) => Check::Failed(Problem::plain(tr("Die Prüfung ließ sich nicht abschließen."))),
            });
        });
    }

    fn finish(self) {
        self.stop();
        sheet::close(self.store.ui);
    }

    fn onward(self) {
        self.stop();
        crate::view::sheets::after_first_account(self.store);
    }

    /// Leaves the sheet after accounts were added: on the first run straight on to the first instance, else closed, with
    /// a toast that offers `again` for the next one.
    fn done(self, text: String, again: Start) {
        let store = self.store;
        if first_run(store) {
            self.onward();
        } else {
            self.finish();
        }
        toast::show_action(store.ui, text, tr("Noch einen"), EventHandler::new(move |_| open(store, again.clone())));
    }
}

/// What a step puts into the sheet's frame.
struct Parts {
    title: String,
    message: Option<String>,
    body: Element,
    back: bool,
    secondary: Option<(&'static str, Primary)>,
    primary: Option<(&'static str, Primary)>,
}

fn act(label: String, disabled: Option<String>, mut run: impl FnMut() + 'static) -> Primary {
    Primary { label, disabled, danger: false, busy: false, run: EventHandler::new(move |_| run()) }
}

const SIGN_IN_STEPS: usize = 3;

fn waiting(id: &str, text: String) -> Element {
    Wait::working(id, text).into_element()
}

fn signing_in(at: usize, text: String) -> Element {
    Wait::stepped(format!("account.add.step{at}"), text, at, SIGN_IN_STEPS).into_element()
}

fn notice(t: &Theme, glyph: Icon, color: Color, text: String) -> Element {
    rect()
        .width(Size::fill())
        .horizontal()
        .spacing(space::XS)
        .content(Content::flex())
        .child(rect().padding((space::XXS, 0., 0., 0.)).child(icon(t, glyph, iconsize::S, color)))
        .child(para(t, text, font::CALLOUT, color).width(Size::flex(1.)))
        .into_element()
}

fn details(t: &Theme, id: &str, detail: String, mut open: State<bool>) -> Element {
    let shown = *open.read();
    let label = if shown { tr("Details ausblenden") } else { tr("Details") };
    let toggle = Button::new(format!("{id}.details"), label).plain().glyph(if shown { Icon::ChevronDown } else { Icon::ChevronRight }).on_press(move |_| open.toggle());
    let mut column = rect().width(Size::fill()).spacing(space::XXS).child(rect().horizontal().child(toggle));
    if shown {
        column = column.child(rect().width(Size::fill()).padding((space::XS, space::S)).corner_radius(t.shape.small).background(t.well).child(mono(t, detail, font::CAPTION, t.text2).width(Size::fill()).max_lines(DETAIL_LINES)));
    }
    column.into_element()
}

fn problem_view(t: &Theme, id: &str, problem: &Problem, open: State<bool>) -> Element {
    let mut column = rect().width(Size::fill()).child(notice(t, Icon::CircleAlert, t.danger_text, problem.text.clone()));
    if let Some(detail) = &problem.detail {
        column = column.child(details(t, id, detail.clone(), open));
    }
    column.into_element()
}

fn failure_text(e: &SignInError, again: Option<&Account>) -> (String, String, String, bool) {
    let retry = tr("Erneut versuchen");
    match e {
        SignInError::Expired => (tr("Der Code ist abgelaufen."), tr("Fordere einen neuen Code an und gib ihn innerhalb der Frist ein."), tr("Neuer Code"), false),
        SignInError::Declined => (tr("Die Anmeldung wurde bei Microsoft abgebrochen."), tr("Versuche es erneut, wenn du bereit bist."), retry, false),
        SignInError::Closed => (tr("Das Anmeldefenster wurde geschlossen."), tr("Es wurde nichts gespeichert."), tr("Erneut öffnen"), false),
        SignInError::NoClientId => (
            tr("Die Anmeldung mit Microsoft gibt es in dieser Version noch nicht."),
            if again.is_some() { tr("Eine spätere Version kann Microsoft-Accounts anmelden.") } else { tr("Offline-Accounts kannst du schon hinzufügen.") },
            retry,
            again.is_none(),
        ),
        SignInError::NotOwned => (tr("Dieser Account besitzt Minecraft: Java Edition nicht."), tr("Melde dich mit dem Account an, mit dem du Minecraft gekauft hast."), tr("Anderer Account"), false),
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

fn capped(names: &[&str], shown: usize) -> String {
    let list = names.iter().take(shown).copied().collect::<Vec<_>>().join(", ");
    match names.len().saturating_sub(shown) {
        0 => list,
        more => format!("{list} {}", trf("und %lld weitere", &[&more])),
    }
}

/// How a pasted list of names reads: the counts, the lines that keep a name out, and how many of those stay unlisted.
#[derive(Debug, PartialEq, Eq)]
struct Summary {
    fine: usize,
    counts: String,
    problems: Vec<String>,
    hidden: usize,
}

fn summarize(listed: &[(String, Listed)]) -> Summary {
    let count = |want: fn(&Listed) -> bool| listed.iter().filter(|(_, v)| want(v)).count();
    let fine = count(|v| *v == Listed::Fine);
    let there = count(|v| matches!(v, Listed::Taken | Listed::Twice));
    let invalid = count(|v| matches!(v, Listed::Invalid(_)));
    let mut counts = vec![trf("%lld gültig", &[&fine])];
    if there > 0 {
        counts.push(trf("%lld schon vorhanden", &[&there]));
    }
    if invalid > 0 {
        counts.push(trf("%lld ungültig", &[&invalid]));
    }
    let problems: Vec<String> = listed
        .iter()
        .filter_map(|(name, v)| match v {
            Listed::Fine => None,
            Listed::Taken => Some(trf("„%@“: gibt es schon", &[name])),
            Listed::Twice => Some(trf("„%@“: steht doppelt in der Liste", &[name])),
            Listed::Invalid(why) => Some(trf("„%@“: %@", &[name, why])),
        })
        .collect();
    let hidden = problems.len().saturating_sub(LIST_PROBLEMS);
    Summary { fine, counts: counts.join(" · "), problems: problems.into_iter().take(LIST_PROBLEMS).collect(), hidden }
}

#[derive(PartialEq)]
struct AddAccount {
    boot: Boot,
}

impl Component for AddAccount {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let cell: Token = use_hook(|| Rc::new(RefCell::new(Arc::new(AtomicBool::new(false)))));
        let held = Rc::clone(&cell);
        use_drop(move || held.borrow().store(true, Ordering::SeqCst));
        let boot = &self.boot;
        let flow = Flow {
            store,
            step: use_state(|| if boot.microsoft { Step::Microsoft(Ms::Requesting) } else if boot.offline { Step::Offline } else { Step::Choose }),
            pending: use_state(|| boot.pending.clone()),
            added: use_state(Vec::new),
            copied: use_state(|| None),
            windowed: use_state(|| false),
            via: use_state(|| Via::Window),
            opened_private: use_state(|| false),
            private_left_open: use_state(|| false),
            details: use_state(|| false),
            name: use_state(String::new),
            proxy: use_state(String::new),
            advanced: use_state(|| false),
            check: use_state(|| Check::Idle),
            saving: use_state(|| false),
            save_error: use_state(|| None),
            cancel: use_state(move || cell),
        };
        let choices = [use_a11y(), use_a11y()];
        let microsoft = boot.microsoft;
        use_hook(move || {
            if microsoft {
                flow.start(Via::Window);
            }
        });
        let typed = flow.proxy.read().clone();
        use_side_effect_with_deps(&typed, move |_| {
            let mut check = flow.check;
            if *check.peek() != Check::Idle {
                check.set(Check::Idle);
            }
        });
        let called = flow.name.read().clone();
        use_side_effect_with_deps(&called, move |_| {
            let mut error = flow.save_error;
            if error.peek().is_some() {
                error.set(None);
            }
        });
        let step = flow.step.read().clone();
        let parts = match &step {
            Step::Choose => choose(flow, &t, choices, false),
            Step::Offline => choose(flow, &t, choices, true),
            Step::Microsoft(ms) => microsoft_parts(flow, &t, ms, !microsoft),
        };
        let mut body = rect().width(Size::fill()).spacing(space::M).child(parts.body);
        if parts.back {
            body = body.child(rect().horizontal().child(Button::new("account.add.back", tr("Zurück")).plain().glyph(Icon::Back).on_press(move |_| flow.back())));
        }
        let mut card = Card::new("account.add", parts.title).width(WIDTH).body(body);
        if let Some(message) = parts.message {
            card = card.message(message);
        }
        if let Some((id, p)) = parts.secondary {
            card = card.secondary(p).secondary_id(id);
        }
        if let Some((id, p)) = parts.primary {
            card = card.primary(p).primary_id(id);
        }
        if parts.back {
            card = card.on_escape(move |_| flow.back());
        }
        card
    }
}

fn choose(flow: Flow, t: &Theme, ids: [AccessibilityId; 2], offline: bool) -> Parts {
    let problem = flow.proxy_problem();
    let blocked = problem.is_some();
    let open = *flow.advanced.read() || blocked;
    let filled = !flow.proxy.read().trim().is_empty();
    let [first, second] = ids;
    let cards = rect()
        .width(Size::fill())
        .spacing(space::S)
        .child(Choice {
            id: "account.add.microsoft",
            own: first,
            before: None,
            after: Some(second),
            glyph: Icon::KeyRound,
            title: tr("Microsoft-Account"),
            subtitle: tr("Für alle Server. Du meldest dich bei Microsoft an."),
            disabled: problem.clone(),
            first: !open && !offline,
            selected: false,
            on_press: EventHandler::new(move |_| flow.start(Via::Window)),
        })
        .child(Choice {
            id: "account.add.offline",
            own: second,
            before: Some(first),
            after: None,
            glyph: Icon::User,
            title: tr("Offline-Account"),
            subtitle: tr("Nur für Server ohne Anmeldeprüfung."),
            disabled: problem.clone(),
            first: false,
            selected: offline,
            on_press: EventHandler::new(move |_| {
                let mut step = flow.step;
                step.set_if_modified(Step::Offline);
            }),
        });
    let label = if filled && !open { tr("Erweitert · Proxy gesetzt") } else { tr("Erweitert") };
    let mut advanced = flow.advanced;
    let disclosure = Button::new("account.add.advanced", label).plain().glyph(if open { Icon::ChevronDown } else { Icon::ChevronRight }).on_press(move |_| advanced.set(!open));
    let mut body = rect().width(Size::fill()).spacing(space::S).child(cards);
    let mut primary = None;
    if offline {
        let (form, ready) = offline_form(flow, t);
        body = body.child(rect().width(Size::fill()).padding((space::S, 0., space::XS, 0.)).child(form));
        if filled && !open {
            body = body.child(line(t, trf("Proxy: %@", &[&forms::proxy_display(flow.proxy.read().trim())]), font::CALLOUT, t.text2));
        }
        primary = Some(("account.add.ok", ready));
    }
    body = body.child(rect().horizontal().child(disclosure));
    if open {
        let check = flow.check.read().clone();
        let running = check == Check::Running;
        let reason = if running { Some(tr("Wird geprüft …")) } else if !filled { Some(tr("Erst eine Proxy-Adresse eintragen")) } else { problem.clone() };
        let field = Field::new("account.add.proxy", flow.proxy).placeholder(tr("z. B. socks5://host:1080")).hint(tr("Gilt für Anmeldung und Spiel. Leer lassen für keinen Proxy.")).error(problem).width(Size::flex(1.)).on_submit(move |_| flow.run_check()).auto_focus();
        let test = Button::new("account.add.check", tr("Prüfen")).disabled(reason).on_press(move |_| flow.run_check());
        let mut block = rect()
            .width(Size::fill())
            .spacing(space::XS)
            .child(line(t, tr("Proxy"), font::CALLOUT, t.text2).font_weight(FontWeight::MEDIUM))
            .child(rect().width(Size::fill()).horizontal().spacing(space::S).content(Content::flex()).child(field).child(test));
        match check {
            Check::Idle => {}
            Check::Running => block = block.child(waiting("account.proxy.check", tr("Proxy wird geprüft"))),
            Check::Passed => block = block.child(notice(t, Icon::CircleCheck, t.success_text, tr("Der Proxy erreicht das Internet."))),
            Check::Failed(why) => block = block.child(problem_view(t, "account.add.check", &why, flow.details)),
        }
        body = body.child(block);
    }
    Parts { title: tr("Account hinzufügen"), message: None, body: body.into_element(), back: false, secondary: None, primary }
}

#[derive(Clone, PartialEq)]
struct Choice {
    id: &'static str,
    own: AccessibilityId,
    before: Option<AccessibilityId>,
    after: Option<AccessibilityId>,
    glyph: Icon,
    title: String,
    subtitle: String,
    disabled: Option<String>,
    first: bool,
    selected: bool,
    on_press: EventHandler<()>,
}

impl Component for Choice {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let own = self.own;
        let focus = use_focus(own);
        let mut hovered = use_state(|| false);
        let mut pressed = use_state(|| false);
        let enabled = self.disabled.is_none();
        let spot = use_spot(self.id, "radio", &self.title, enabled);
        let (before, after) = (self.before, self.after);
        let press = self.on_press.clone();
        let (ink, quiet) = if enabled { (t.text1, t.text2) } else { (t.text3, t.text3) };
        let mut card = rect()
            .a11y_id(own)
            .a11y_focusable(enabled)
            .a11y_role(AccessibilityRole::RadioButton)
            .a11y_alt(if self.selected { format!("{}, {}", self.title, tr("ausgewählt")) } else { self.title.clone() })
            .a11y_auto_focus(self.first && enabled)
            .width(Size::fill())
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::M)
            .padding(space::M)
            .corner_radius(t.shape.tile)
            .background(if enabled && *pressed.read() { t.press } else if enabled && *hovered.read() { t.hover } else { Color::TRANSPARENT })
            .border(if self.selected { Border::new().fill(t.accent).width(frame::BOX_EDGE).alignment(BorderAlignment::Inner) } else { Border::new().fill(t.separator).width(1.).alignment(BorderAlignment::Inner) })
            .cursor(if enabled { CursorIcon::Pointer } else { CursorIcon::NotAllowed })
            .content(Content::flex())
            .on_sized(spot)
            .on_pointer_over(move |_| hovered.set(true))
            .on_pointer_out(move |_| {
                hovered.set_if_modified(false);
                pressed.set_if_modified(false);
            })
            .on_mouse_down(move |_| pressed.set(true))
            .on_mouse_up(move |_| pressed.set_if_modified(false));
        if enabled {
            let press = press.clone();
            card = card.on_all_press(move |_| {
                own.request_focus();
                press.call(());
            });
        }
        card = card.on_key_down(move |e: Event<KeyboardEventData>| {
            if !own.is_focused() {
                return;
            }
            if enabled && e.is_press_event() {
                press.call(());
                return;
            }
            let target = match e.key {
                Key::Named(NamedKey::ArrowDown | NamedKey::ArrowRight) => after,
                Key::Named(NamedKey::ArrowUp | NamedKey::ArrowLeft) => before,
                _ => return,
            };
            e.stop_propagation();
            e.prevent_default();
            if let Some(id) = target {
                let mut mode = Platform::get().current_window().navigation_mode;
                mode.set_if_modified(NavigationMode::Keyboard);
                id.request_focus();
            }
        });
        if let Some(border) = ring(&t, focus()) {
            card = card.border(border);
        }
        card.child(rect().width(Size::px(control::L)).height(Size::px(control::L)).center().corner_radius(t.shape.control).background(if enabled { t.accent.with_af32(TILE_WASH) } else { t.control }).child(icon(&t, self.glyph, iconsize::L, if enabled { t.accent_text } else { t.text3 })))
            .child(rect().width(Size::flex(1.)).spacing(space::XXS).child(strong(&t, self.title.clone(), font::BODY, ink)).child(para(&t, self.subtitle.clone(), font::CALLOUT, quiet)))
            .child(if self.selected { icon(&t, Icon::Check, iconsize::M, t.accent_text) } else { icon(&t, Icon::ChevronRight, iconsize::M, t.text3) })
    }
}

fn offline_form(flow: Flow, t: &Theme) -> (Element, Primary) {
    let text = flow.name.read().clone();
    let saving = *flow.saving.read();
    let error = flow.save_error.read().clone();
    let model = flow.store.model.peek();
    let mut body = rect().width(Size::fill()).spacing(space::M);
    let (unready, add_label) = if text.contains('\n') {
        let listed = forms::check_names(&text, &model.accounts);
        let summary = summarize(&listed);
        let rows = (text.split('\n').count()).clamp(LIST_ROWS.0, LIST_ROWS.1);
        let mut column = rect()
            .width(Size::fill())
            .spacing(space::XS)
            .child(Field::new("account.add.list", flow.name).label(tr("Namen")).lines(rows).on_submit(move |_| flow.add_offline()))
            .child(rect().width(Size::fill()).child(para(t, summary.counts.clone(), font::CALLOUT, t.text2)));
        for problem in &summary.problems {
            column = column.child(line(t, problem.clone(), font::CALLOUT, t.attention_text));
        }
        if summary.hidden > 0 {
            column = column.child(line(t, trf("und %lld weitere", &[&summary.hidden]), font::CALLOUT, t.text3));
        }
        let submit = if cfg!(target_os = "macos") { tr("⌘+Enter fügt hinzu") } else { tr("Strg+Enter fügt hinzu") };
        body = body.child(column.child(note(t, tr("Ein Name pro Zeile. Ungültige und vorhandene werden übersprungen."))).child(note(t, submit)));
        ((summary.fine == 0).then(|| tr("In der Liste steht kein gültiger Name.")), trf("%lld hinzufügen", &[&summary.fine]))
    } else {
        let name = text.trim();
        let problem = forms::offline_name_problem(name, &model.accounts);
        let shown = problem.clone().filter(|_| !name.is_empty() && error.is_none());
        let field = Field::new("account.add.name", flow.name).label(tr("Name")).placeholder(tr("z. B. Steve")).hint(tr("3 bis 16 Zeichen: Buchstaben, Ziffern und _. Für mehrere füge eine Liste ein.")).error(shown).on_submit(move |_| flow.add_offline()).auto_focus();
        body = body.child(field);
        (problem, tr("Hinzufügen"))
    };
    drop(model);
    if let Some(why) = &error {
        body = body.child(problem_view(t, "account.add.error", why, flow.details));
    }
    let reason = if saving { Some(tr("Wird hinzugefügt …")) } else { unready };
    (body.into_element(), act(add_label, reason, move || flow.add_offline()))
}

fn microsoft_parts(flow: Flow, t: &Theme, ms: &Ms, can_back: bool) -> Parts {
    let pending = flow.pending.read().clone();
    let added = matches!(ms, Ms::Added(_));
    let (title, message) = match (&pending.again, ms) {
        (Some(a), _) => (tr("Erneut anmelden"), Some(a.name.clone())),
        (None, Ms::Added(name)) => (trf("%@ hinzugefügt", &[name]), None),
        (None, _) => (tr("Mit Microsoft anmelden"), pending.queue.first().map(|n| trf("Für den Account „%@“", &[n]))),
    };
    let mut parts = Parts { title, message, body: rect().into_element(), back: can_back && pending.again.is_none() && !added, secondary: None, primary: None };
    let spacing = space::M;
    match ms {
        Ms::Requesting => {
            let text = if *flow.windowed.read() { tr("Anmeldefenster wird geöffnet …") } else { tr("Code wird angefordert …") };
            parts.body = rect().width(Size::fill()).padding((space::L, 0.)).child(signing_in(1, text)).into_element();
        }
        Ms::Window => {
            let instruction = match pending.again.as_ref().map(|a| &a.name).or(pending.queue.first()) {
                Some(name) => trf("Melde dich im Anmeldefenster mit dem Account „%@“ an. Danach geht es hier von selbst weiter.", &[name]),
                None => tr("Melde dich im Anmeldefenster mit dem Account an, den du hinzufügen willst. Danach geht es hier von selbst weiter."),
            };
            parts.body = rect()
                .width(Size::fill())
                .spacing(spacing)
                .child(para(t, instruction, font::BODY, t.text1))
                .child(signing_in(2, tr("Warte auf deine Anmeldung im Fenster")))
                .child(rect().horizontal().child(Button::new("account.add.browser", tr("Im Browser anmelden")).plain().glyph(Icon::External).on_press(move |_| flow.start(Via::Browser))))
                .into_element();
        }
        Ms::Finishing => {
            parts.body = rect().width(Size::fill()).padding((space::L, 0.)).child(signing_in(3, tr("Anmeldung wird abgeschlossen"))).into_element();
        }
        Ms::Code(code) => {
            let copied = *flow.copied.read();
            let device = code.user_code.clone();
            let copy_code = Button::icon("account.add.copy-code", if copied == Some(Copied::Code) { Icon::Check } else { Icon::Copy }, if copied == Some(Copied::Code) { tr("Kopiert") } else { tr("Code kopieren") }).on_press(move |_| flow.copy(device.clone(), Copied::Code));
            let code_box = rect()
                .width(Size::fill())
                .padding((CODE_PAD, space::S))
                .corner_radius(t.shape.tile)
                .background(t.well)
                .horizontal()
                .cross_align(Alignment::Center)
                .content(Content::flex())
                .child(rect().width(Size::px(control::M)))
                .child(rect().width(Size::flex(1.)).center().child(mono(t, code.user_code.clone(), font::LARGE_TITLE, t.text1).font_weight(FontWeight::SEMI_BOLD)))
                .child(copy_code);
            let minutes = (code.expires_in / 60).max(1);
            let instruction = match pending.queue.first() {
                Some(name) => trf("Öffne die Microsoft-Seite, prüfe den Code und melde dich mit dem Account „%@“ an. Danach geht es hier von selbst weiter.", &[name]),
                None => tr("Öffne die Microsoft-Seite, prüfe den Code und melde dich an. Danach geht es hier von selbst weiter."),
            };
            let hint = if code.private { tr("Die Seite öffnet sich in einem privaten Fenster, unabhängig von deiner Anmeldung im Browser.") } else { tr("Melde dich vorher bei Microsoft ab, wenn du einen anderen Account nehmen willst.") };
            let state = match copied {
                Some(Copied::Code) => notice(t, Icon::Check, t.success_text, tr("Code kopiert. Auf der Seite einfügen.")),
                Some(Copied::Link) => notice(t, Icon::Check, t.success_text, tr("Link kopiert. Der Code steckt schon darin.")),
                None => signing_in(2, tr("Warte auf deine Anmeldung im Browser")),
            };
            let mut body = rect().width(Size::fill()).spacing(spacing);
            if code.fallback {
                body = body.child(para(t, tr("Das Anmeldefenster ließ sich nicht öffnen, deshalb geht es im Browser weiter."), font::CALLOUT, t.text2));
            }
            parts.body = body
                .child(para(t, instruction, font::BODY, t.text1))
                .child(code_box)
                .child(state)
                .child(para(t, hint, font::CALLOUT, t.text2))
                .child(
                    rect()
                        .width(Size::fill())
                        .horizontal()
                        .cross_align(Alignment::Center)
                        .spacing(space::M)
                        .content(Content::flex())
                        .child(rect().width(Size::flex(1.)).child(para(t, trf("Der Code gilt %lld Minuten.", &[&minutes]), font::CAPTION, t.text3)))
                        .child(Button::new("account.add.copy-link", tr("Link kopieren")).plain().glyph(Icon::Link).on_press(move |_| flow.copy_link())),
                )
                .into_element();
            parts.primary = Some(("account.add.open-link", Primary { label: tr("Seite öffnen"), disabled: None, danger: false, busy: false, run: EventHandler::new(move |_| flow.open_page()) }));
        }
        Ms::Added(name) => {
            let next = pending.queue.first().cloned();
            let onward = next.is_none() && pending.again.is_none() && first_run(flow.store);
            let again = match &next {
                Some(next) => trf("Weiter mit %@", &[next]),
                None => tr("Noch einen"),
            };
            let more = act(again, None, move || flow.start(Via::Window));
            if onward {
                parts.secondary = Some(("account.add.again", more));
                parts.primary = Some(("account.add.ok", act(tr("Weiter"), None, move || flow.onward())));
            } else {
                parts.secondary = Some(("account.add.done", act(tr("Fertig"), None, move || flow.finish())));
                parts.primary = Some(("account.add.again", more));
            }
            let all = flow.added.read().clone();
            let before: Vec<&String> = all.iter().filter(|n| *n != name).collect();
            let mut lines = rect().width(Size::fill()).spacing(space::XS);
            if !before.is_empty() {
                lines = lines.child(line(t, trf("Schon hinzugefügt: %@", &[&capped(&before.iter().map(|n| n.as_str()).collect::<Vec<_>>(), ADDED_SHOWN)]), font::CAPTION, t.text3));
            }
            lines = match next {
                Some(next) => {
                    let rest = pending.queue.len();
                    lines.child(para(t, if rest == 1 { trf("Als Nächstes: %@. Das ist der letzte Account.", &[&next]) } else { trf("Als Nächstes: %@. Danach noch %lld.", &[&next, &(rest - 1)]) }, font::BODY, t.text1).max_lines(2))
                }
                None if onward => lines.child(para(t, tr("Als Nächstes legst du die Instanz an, mit der du spielst."), font::CALLOUT, t.text2)),
                None => lines.child(para(t, tr("Du kannst gleich noch einen Microsoft-Account hinzufügen."), font::CALLOUT, t.text2)),
            };
            if *flow.private_left_open.read() {
                lines = lines.child(para(t, tr("Schließ das private Fenster, bevor du den nächsten Account anmeldest."), font::CALLOUT, t.text2));
            }
            if pending.again.is_none()
                && let Some(proxy) = flow.chosen_proxy()
            {
                lines = lines.child(line(t, trf("Der nächste Account nutzt wieder den Proxy %@.", &[&forms::proxy_display(&proxy)]), font::CAPTION, t.text3));
            }
            parts.body = rect().width(Size::fill()).spacing(space::S).padding((space::S, 0.)).child(notice(t, Icon::CircleCheck, t.success_text, trf("Angemeldet als %@.", &[name]))).child(at_least(ADDED_NOTES, lines).width(Size::fill())).into_element();
        }
        Ms::Failed(e) => {
            let (heading, detail, retry, to_offline) = failure_text(e, pending.again.as_ref());
            let technical = match e {
                SignInError::Other(p) => p.detail.clone(),
                _ => None,
            };
            let mut column = rect().width(Size::flex(1.)).spacing(space::XS).child(strong(t, heading, font::BODY, t.text1).max_lines(3)).child(para(t, detail, font::CALLOUT, t.text2));
            if let Some(technical) = technical {
                column = column.child(details(t, "account.add.error", technical, flow.details));
            }
            if matches!(e, SignInError::Closed) {
                column = column.child(rect().horizontal().child(Button::new("account.add.browser", tr("Im Browser anmelden")).plain().glyph(Icon::External).on_press(move |_| flow.start(Via::Browser))));
            }
            if pending.skippable() {
                column = column.child(rect().horizontal().child(Button::new("account.add.skip", tr("Überspringen")).plain().on_press(move |_| flow.skip())));
            }
            parts.primary = Some(if to_offline {
                ("account.add.offline", act(tr("Offline-Account hinzufügen"), None, move || {
                    let mut step = flow.step;
                    step.set(Step::Offline);
                }))
            } else {
                ("account.add.retry", act(retry, None, move || {
                    let via = *flow.via.peek();
                    flow.start(via);
                }))
            });
            let glyph = if matches!(e, SignInError::Network) { Icon::WifiOff } else { Icon::CircleAlert };
            parts.body = rect().width(Size::fill()).horizontal().spacing(space::M).padding((space::S, 0.)).content(Content::flex()).child(icon(t, glyph, iconsize::L, t.danger_text)).child(column).into_element();
        }
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(name: &str) -> Account {
        Account { id: Uuid::nil(), name: name.to_owned(), kind: AccountKind::Microsoft, proxy: None, color: None }
    }

    #[test]
    fn done_without_renewal_takes_the_name_off_the_queue() {
        let mut p = Pending { queue: vec!["Alex".into(), "Sam".into()], ..Pending::default() };
        assert_eq!(p.done("alex"), Done::Added);
        assert_eq!(p.queue, vec!["Sam".to_owned()]);
    }

    #[test]
    fn done_while_renewing_goes_on_to_the_next_account_then_finishes() {
        let mut p = Pending { again: Some(account("A")), later: vec![account("B")], queue: Vec::new() };
        assert_eq!(p.done("A"), Done::Next);
        assert_eq!(p.again.as_ref().map(|a| a.name.as_str()), Some("B"));
        assert_eq!(p.done("B"), Done::Finished);
    }

    #[test]
    fn skip_moves_on_in_either_list() {
        let mut p = Pending { again: Some(account("A")), later: vec![account("B")], queue: Vec::new() };
        assert!(p.skippable());
        p.skip();
        assert_eq!(p.again.as_ref().map(|a| a.name.as_str()), Some("B"));
        assert!(!p.skippable());
        let mut q = Pending { queue: vec!["X".into(), "Y".into()], ..Pending::default() };
        assert!(q.skippable());
        q.skip();
        assert_eq!(q.queue, vec!["Y".to_owned()]);
        assert!(!q.skippable());
        q.skip();
        assert!(q.queue.is_empty());
    }

    #[test]
    fn summary_counts_and_caps_the_problem_lines() {
        let mut listed = vec![("Steve".to_owned(), Listed::Fine), ("Alex".to_owned(), Listed::Taken)];
        for n in 0..5 {
            listed.push((format!("Bad{n}"), Listed::Invalid("zu kurz".into())));
        }
        let s = summarize(&listed);
        assert_eq!(s.fine, 1);
        assert_eq!(s.problems.len(), LIST_PROBLEMS);
        assert_eq!(s.hidden, 6 - LIST_PROBLEMS);
        assert!(s.counts.contains('1'));
    }

    #[test]
    fn capped_names_show_the_first_few_and_count_the_rest() {
        assert_eq!(capped(&["A", "B"], 3), "A, B");
        let all = ["A", "B", "C", "D", "E"];
        let text = capped(&all, 3);
        assert!(text.starts_with("A, B, C ") && text.contains('2') && !text.contains('D'));
    }

    #[test]
    fn wrong_account_failure_names_both_accounts() {
        let (title, detail, _, offline) = failure_text(&SignInError::WrongAccount("Other".into()), Some(&account("Mine")));
        assert!(title.contains("Other") && detail.contains("Mine") && !offline);
    }

    #[test]
    fn missing_client_id_offers_offline_only_for_a_new_account() {
        assert!(failure_text(&SignInError::NoClientId, None).3);
        assert!(!failure_text(&SignInError::NoClientId, Some(&account("Mine"))).3);
    }
}
