//! The banner after a game of the instance crashed or did not start: what happened, the reason when a crash report names
//! one, and the way to the logs.

use std::time::SystemTime;

use freya::prelude::*;
use uuid::Uuid;

use super::{Ended, Tab, facts};
use crate::core::i18n::tr;
use crate::core::logs::{self, Kind};
use crate::view::icons::{Icon, icon};
use crate::view::kit::button::Button;
use crate::view::kit::text::{para, strong};
use crate::view::kit::{off_ui, use_theme};
use crate::view::store::use_store;
use crate::view::theme::{font, frame, iconsize, space};

/// The reason of the newest crash report written since the log of the last game began, when there is one.
fn reason(dir: &std::path::Path, account: Option<Uuid>) -> Option<String> {
    let (_, _, files) = super::logs_lines::scan(dir, &[], account);
    let since = files.iter().find(|f| f.kind == Kind::Latest).and_then(|f| logs::born(&f.path)).and_then(|at| at.duration_since(SystemTime::UNIX_EPOCH).ok()).and_then(|d| i64::try_from(d.as_secs()).ok());
    files.into_iter().filter(|f| f.kind == Kind::Crash && since.is_none_or(|s| f.modified >= s - 1)).max_by_key(|f| f.modified).and_then(|f| f.reason)
}

/// "Abgestürzt" or "Start fehlgeschlagen" with its reason and Protokoll ansehen.
#[derive(PartialEq)]
pub struct Banner {
    pub id: String,
}

impl Component for Banner {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let mut tab = store.ui.instance.tab;
        let mut found = use_state(|| None::<(String, Option<String>)>);
        let f = facts(&store.model.read(), &self.id);
        let crashed = f.as_ref().is_some_and(|f| f.ended == Some(Ended::Crashed));
        let key = format!("{}|{:?}|{}", self.id, f.as_ref().and_then(|f| f.solo), f.as_ref().and_then(|f| f.trouble.clone()).unwrap_or_default());
        let (dir, account) = (f.as_ref().map(|f| f.made.dir()), f.as_ref().and_then(|f| f.solo));
        use_side_effect_with_deps(&(key.clone(), crashed), move |(key, crashed)| {
            let (key, dir) = (key.clone(), dir.clone());
            if !*crashed || dir.is_none() {
                return;
            }
            spawn(async move {
                let Some(dir) = dir else { return };
                if let Some(read) = off_ui(move || reason(&dir, account)).await {
                    found.set(Some((key, read)));
                }
            });
        });
        let Some(f) = f.filter(|f| matches!(f.ended, Some(Ended::Crashed | Ended::Failed))) else { return rect().into_element() };
        let title = if crashed { tr("Abgestürzt") } else { f.trouble.clone().unwrap_or_else(|| tr("Start fehlgeschlagen")) };
        let hint = found.read().as_ref().filter(|(k, _)| *k == key).and_then(|(_, h)| h.clone()).filter(|_| crashed);
        let mut text = rect().width(Size::flex(1.)).spacing(space::XXS).child(strong(&t, title, font::CALLOUT, t.text1));
        if let Some(hint) = hint {
            text = text.child(para(&t, hint, font::CALLOUT, t.text2));
        }
        rect()
            .width(Size::fill())
            .padding(space::M)
            .spacing(space::S)
            .horizontal()
            .cross_align(Alignment::Center)
            .corner_radius(t.shape.control)
            .background(t.attention.with_af32(frame::TILE_WASH))
            .child(icon(&t, Icon::Alert, iconsize::M, t.attention_text))
            .child(text)
            .content(Content::flex())
            .child(Button::new("instance.crash.logs", tr("Protokoll ansehen")).on_press(move |_| tab.set(Tab::Logs)))
            .into_element()
    }
}
