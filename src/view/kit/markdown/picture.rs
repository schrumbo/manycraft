//! Pictures inside a document: the Markdown and HTML forms are found in a line, fetched once off the UI thread and drawn at their own size.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::OnceLock;

use freya::elements::image::ImageHandle;
use freya::engine::prelude::AlphaType;
use freya::prelude::*;

use super::{link, run_len, safe_link, code_span};
use crate::core::packs;
use crate::view::kit::{off_ui, open_external, use_theme};
use crate::view::theme::{font, space};

const AT_ONCE: usize = 4;
const KEPT: usize = 24;
const UNMEASURED: f32 = 640.;
const PLACEHOLDER: (f32, f32) = (96., 20.);

/// A picture of a document: where it is, what it shows in words, where a click on it goes and how wide its author drew it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pic {
    pub url: String,
    pub alt: String,
    pub link: Option<String>,
    pub width: Option<u32>,
}

fn image_at(c: &[char], at: usize) -> Option<(Pic, usize)> {
    if c.get(at) != Some(&'!') || c.get(at + 1) != Some(&'[') {
        return None;
    }
    let (alt, target, mut next) = link(c, at + 1)?;
    let url = safe_link(&target)?;
    let mut width = None;
    let tail: String = c[next..c.len().min(next + 16)].iter().collect();
    if let Some(rest) = tail.strip_prefix("{width=")
        && let Some((digits, _)) = rest.split_once('}')
    {
        width = digits.parse().ok();
        next += "{width=".len() + digits.chars().count() + 1;
    }
    Some((Pic { url, alt: c[alt].iter().collect::<String>().trim().to_owned(), link: None, width }, next))
}

fn linked_at(c: &[char], at: usize) -> Option<(Pic, usize)> {
    if c.get(at) != Some(&'[') {
        return None;
    }
    let (inner, href, next) = link(c, at)?;
    let inside = &c[inner];
    let (mut pic, used) = image_at(inside, 0)?;
    if inside[used..].iter().any(|ch| !ch.is_whitespace()) {
        return None;
    }
    pic.link = safe_link(&href);
    Some((pic, next))
}

fn picture_at(c: &[char], at: usize) -> Option<(Pic, usize)> {
    image_at(c, at).or_else(|| linked_at(c, at))
}

/// The pictures of `line` when it holds nothing else.
pub(super) fn only_pictures(line: &str) -> Option<Vec<Pic>> {
    if !line.contains("![") {
        return None;
    }
    let c: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        if c[i].is_whitespace() {
            i += 1;
            continue;
        }
        let (pic, next) = picture_at(&c, i)?;
        out.push(pic);
        i = next;
    }
    (!out.is_empty()).then_some(out)
}

/// `line` with each picture in it replaced by its alt text, for a line that has words around them.
pub(super) fn without_pictures(line: &str) -> String {
    if !line.contains("![") {
        return line.to_owned();
    }
    let c: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < c.len() {
        if c[i] == '`' {
            let end = code_span(&c, i).map_or(i + run_len(&c, i, '`'), |(_, end)| end);
            out.extend(c[i..end].iter());
            i = end;
        } else if let Some((pic, next)) = picture_at(&c, i) {
            match (&pic.link, pic.alt.is_empty()) {
                (Some(href), false) => out.push_str(&format!("[{}]({href})", pic.alt)),
                (None, false) => out.push_str(&pic.alt),
                _ => {}
            }
            i = next;
        } else {
            out.push(c[i]);
            i += 1;
        }
    }
    out
}

#[derive(Clone)]
struct Loaded {
    handle: ImageHandle,
    w: u32,
    h: u32,
}

#[derive(Clone)]
enum Fetch {
    Loading,
    Ready(Loaded),
    Failed,
}

thread_local! {
    static SEEN: RefCell<HashMap<String, Option<Loaded>>> = RefCell::new(HashMap::new());
}

fn seen(url: &str) -> Option<Option<Loaded>> {
    SEEN.with(|s| s.borrow().get(url).cloned())
}

fn keep(url: String, loaded: Option<Loaded>) {
    SEEN.with(|s| {
        let mut s = s.borrow_mut();
        if s.len() >= KEPT {
            s.clear();
        }
        s.insert(url, loaded);
    });
}

struct Permit(&'static async_channel::Receiver<()>);

impl Drop for Permit {
    fn drop(&mut self) {
        let _ = self.0.try_recv();
    }
}

fn gate() -> &'static (async_channel::Sender<()>, async_channel::Receiver<()>) {
    static GATE: OnceLock<(async_channel::Sender<()>, async_channel::Receiver<()>)> = OnceLock::new();
    GATE.get_or_init(|| async_channel::bounded(AT_ONCE))
}

async fn fetch(url: String) -> Option<Loaded> {
    let (hold, release) = gate();
    hold.send(()).await.ok()?;
    let _permit = Permit(release);
    let (w, h, raw) = off_ui(move || {
        let path = packs::readme_picture(&url).ok()?;
        let img = image::open(path).ok()?.to_rgba8();
        Some((img.width(), img.height(), img.into_raw()))
    })
    .await
    .flatten()?;
    Some(Loaded { handle: ImageHandle::from_rgba(w, h, Bytes::from(raw), AlphaType::Unpremul)?, w, h })
}

#[derive(PartialEq)]
struct Picture {
    pic: Pic,
    room: f32,
}

impl Component for Picture {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let mut state = use_state(|| seen(&self.pic.url).map_or(Fetch::Loading, |p| p.map_or(Fetch::Failed, Fetch::Ready)));
        use_side_effect_with_deps(&self.pic.url, move |url| {
            if !matches!(*state.peek(), Fetch::Loading) {
                return;
            }
            let url = url.clone();
            spawn(async move {
                let got = fetch(url.clone()).await;
                keep(url, got.clone());
                state.set(got.map_or(Fetch::Failed, Fetch::Ready));
            });
        });
        let body = match state.read().clone() {
            Fetch::Loading => rect().width(Size::px(PLACEHOLDER.0)).height(Size::px(PLACEHOLDER.1)).corner_radius(t.shape.small).background(t.hover).into_element(),
            Fetch::Failed if self.pic.alt.is_empty() => rect().into_element(),
            Fetch::Failed => paragraph().font_size(font::CALLOUT).color(t.text3).span(Span::new(self.pic.alt.clone())).into_element(),
            Fetch::Ready(p) => {
                let (w, h) = (p.w as f32, p.h as f32);
                let shown = self.pic.width.map_or(w, |x| (x as f32).min(w)).min(self.room);
                rect()
                    .corner_radius(t.shape.small)
                    .overflow(Overflow::Clip)
                    .child(image(p.handle).width(Size::px(shown)).height(Size::px(shown * h / w)).aspect_ratio(AspectRatio::None))
                    .into_element()
            }
        };
        match self.pic.link.clone() {
            Some(url) => rect().cursor(CursorIcon::Pointer).on_press(move |_| open_external("project link", url.clone())).child(body).into_element(),
            None => body,
        }
    }
}

#[derive(PartialEq)]
struct Pictures {
    pics: Vec<Pic>,
}

impl Component for Pictures {
    fn render(&self) -> impl IntoElement {
        let mut room = use_state(|| UNMEASURED);
        rect()
            .width(Size::fill())
            .horizontal()
            .spacing(space::S)
            .content(Content::wrap_spacing(space::S))
            .on_sized(move |e: Event<SizedEventData>| room.set_if_modified(e.area.width()))
            .children(self.pics.iter().enumerate().map(|(i, pic)| rect().key(format!("{i}:{}", pic.url)).child(Picture { pic: pic.clone(), room: *room.read() }).into_element()))
    }
}

/// A row of pictures that wraps; each is drawn at its own size, or the width it was given, but never wider than the row.
pub(super) fn view(pics: &[Pic]) -> Element {
    Pictures { pics: pics.to_vec() }.into_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_multibyte_width_does_not_panic() {
        let linked = only_pictures("[![a](https://x.org/a.png){width=日}](https://x.org)").unwrap();
        assert_eq!(linked[0].link.as_deref(), Some("https://x.org"));
        let found = only_pictures("![a](https://x.org/a.png){width=日}").unwrap();
        assert_eq!(found[0].width, None);
    }

    fn pic(url: &str, alt: &str) -> Pic {
        Pic { url: url.into(), alt: alt.into(), link: None, width: None }
    }

    #[test]
    fn a_line_of_pictures_is_all_pictures() {
        let got = only_pictures("![A](https://x.org/a.png) ![B](https://x.org/b.png)").unwrap();
        assert_eq!(got, [pic("https://x.org/a.png", "A"), pic("https://x.org/b.png", "B")]);
    }

    #[test]
    fn a_picture_in_a_link_keeps_the_link() {
        let got = only_pictures("[![Logo](https://x.org/a.png)](https://x.org)").unwrap();
        assert_eq!(got, [Pic { link: Some("https://x.org".into()), ..pic("https://x.org/a.png", "Logo") }]);
    }

    #[test]
    fn a_width_hint_is_read() {
        let got = only_pictures("![](https://x.org/a.png){width=200}").unwrap();
        assert_eq!(got[0].width, Some(200));
    }

    #[test]
    fn words_next_to_a_picture_make_it_text() {
        assert!(only_pictures("see ![A](https://x.org/a.png)").is_none());
        assert_eq!(without_pictures("see ![A](https://x.org/a.png) now"), "see A now");
        assert_eq!(without_pictures("see [![A](https://x.org/a.png)](https://x.org) now"), "see [A](https://x.org) now");
    }

    #[test]
    fn a_picture_that_is_no_web_address_is_not_one() {
        assert!(only_pictures("![A](file:///etc/passwd)").is_none());
        assert!(only_pictures("![A](./local.png)").is_none());
        assert_eq!(without_pictures("![A](./local.png)"), "![A](./local.png)");
    }

    #[test]
    fn code_keeps_its_picture_markup() {
        assert_eq!(without_pictures("use `![A](https://x.org/a.png)` here"), "use `![A](https://x.org/a.png)` here");
    }

    #[test]
    fn plain_text_is_not_pictures() {
        assert!(only_pictures("hello").is_none());
        assert!(only_pictures("").is_none());
    }
}