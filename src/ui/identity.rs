//! Who an account is at a glance: its colour chip, and the menu that changes the colour.

use gpui_kit::component::menu::PopupMenu;
use gpui_kit::prelude::*;
use gpui_kit::*;

use super::actions::SetColor;
use super::theme::{self, iconsize, radius, space, tile, tint};
use crate::core::account::COLORS;
use crate::core::i18n::tr;

const NAMES: [&str; COLORS.len()] = ["Violett", "Pink", "Limette", "Mint", "Sand", "Indigo", "Kupfer", "Silber"];

/// The name of account colour `index`.
pub fn name(index: u8) -> String {
    tr(NAMES[usize::from(index) % NAMES.len()])
}

/// Account colour `index` as a fill.
pub fn fill(index: u8) -> Hsla {
    theme::account_fill(index)
}

fn ink(index: u8) -> Hsla {
    let c = COLORS[usize::from(index) % COLORS.len()];
    let luma = 0.299 * ((c >> 16) & 0xff) as f32 + 0.587 * ((c >> 8) & 0xff) as f32 + 0.114 * (c & 0xff) as f32;
    if luma > tile::CHIP_LUMA { black().opacity(tint::CHIP_INK) } else { white() }
}

/// The chip of an account named `name` in colour `color`, `side` wide: its skin face on the colour when there is one, else the first letter of the name.
pub fn chip(name: &str, color: u8, face: Option<ImageSource>, side: f32, text: f32) -> Div {
    let base = div().size(px(side)).flex_none().rounded(px(radius::SMALL)).bg(fill(color)).flex().items_center().justify_center().overflow_hidden();
    match face {
        Some(face) => base.p(px(tile::CHIP_RIM)).child(img(face).size_full().rounded(px(radius::SMALL / 2.))),
        None => base.text_size(px(text)).font_weight(FontWeight::BOLD).text_color(ink(color)).child(name.chars().next().map(|c| c.to_uppercase().collect::<String>()).unwrap_or_default()),
    }
}

/// Adds the account colours to `menu` with `current` checked; picking one gives every account in `accounts` (ids) that colour.
pub fn colors(mut menu: PopupMenu, accounts: Vec<String>, current: Option<u8>) -> PopupMenu {
    for n in 0..COLORS.len() as u8 {
        let label = name(n);
        menu = menu.menu_element_with_check(current == Some(n), Box::new(SetColor { accounts: accounts.clone(), color: n }), move |_, _| {
            div().flex().items_center().gap(px(space::S)).child(div().size(px(iconsize::S)).flex_none().rounded(px(radius::FULL)).bg(fill(n))).child(label.clone())
        });
    }
    menu
}
