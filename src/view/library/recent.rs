use freya::prelude::*;

use super::RECENT;
use super::groups::Header;
use crate::core::bin;
use crate::core::i18n::tr;
use crate::view::kit::off_thread;
use crate::view::sheets::trash::TrashPage;
use crate::view::store::use_store;

/// The last group of the library: what was deleted in the last seven days, listed once opened. Without anything in
/// the Papierkorb the group is not there, unless it was asked for.
#[derive(PartialEq)]
pub struct Recent;

impl Component for Recent {
    fn render(&self) -> impl IntoElement {
        let store = use_store();
        let mut count = use_state(|| 0usize);
        let m = store.model.read();
        let shape = (m.batches.len(), m.instances.len());
        drop(m);
        use_side_effect_with_deps(&shape, move |_| {
            let Some(rx) = off_thread("library-bin", bin::list) else { return };
            spawn_in_window(async move {
                let Ok(items) = rx.recv().await else { return };
                if let Some(mut n) = count.try_write() {
                    *n = items.map_or(0, |i| i.len());
                }
            });
        });
        let open = !store.ui.library.folded.read().contains(RECENT);
        let n = *count.read();
        if n == 0 && !open {
            return rect().into_element();
        }
        let mut block = rect().width(Size::fill()).spacing(crate::view::theme::space::S).child(Header { key: RECENT.to_owned(), title: tr("Zuletzt gelöscht"), count: (n > 0).then_some(n), open });
        if open {
            block = block.child(TrashPage { embedded: true });
        }
        block.into_element()
    }
}
