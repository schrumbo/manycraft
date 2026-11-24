//! Map pictures: the game's map colours and the texture a filled map shows.

use super::assets::Texture;
use super::entity::MapPicture;

/// The game's base map colours by id, RGB (`MapColor`).
const BASE: [u32; 62] = [
    0x000000, 0x7FB238, 0xF7E9A3, 0xC7C7C7, 0xFF0000, 0xA0A0FF, 0xA7A7A7, 0x007C00,
    0xFFFFFF, 0xA4A8B8, 0x976D4D, 0x707070, 0x4040FF, 0x8F7748, 0xFFFCF5, 0xD87F33,
    0xB24CD8, 0x6699D8, 0xE5E533, 0x7FCC19, 0xF27FA5, 0x4C4C4C, 0x999999, 0x4C7F99,
    0x7F3FB2, 0x334CB2, 0x664C33, 0x667F33, 0x993333, 0x191919, 0xFAEE4D, 0x5CDBD5,
    0x4A80FF, 0x00D93A, 0x815631, 0x700200, 0xD1B1A1, 0x9F5224, 0x95576C, 0x706C8A,
    0xBA8524, 0x677535, 0xA04D4E, 0x392923, 0x876B62, 0x575C5C, 0x7A4958, 0x4C3E5C,
    0x4C3223, 0x4C522A, 0x8E3C2E, 0x251610, 0xBD3031, 0x943F61, 0x5C191D, 0x167E86,
    0x3A8E8C, 0x562C3E, 0x14B485, 0x646464, 0xD8AF93, 0x7FA796,
];

/// The game's brightness of the four shades of a base colour, out of 255.
const SHADES: [u32; 4] = [180, 220, 255, 135];

/// The RGBA colour of a packed map colour (base colour × 4 + shade); transparent for none.
pub fn color(packed: u8) -> [u8; 4] {
    let base = usize::from(packed >> 2);
    match BASE.get(base) {
        Some(&c) if base > 0 => {
            let m = SHADES[usize::from(packed & 3)];
            let [r, g, b] = [16, 8, 0].map(|s| ((c >> s & 255) * m / 255) as u8);
            [r, g, b, 255]
        }
        _ => [0; 4],
    }
}

/// The picture as a texture without mip levels, as the game uploads it.
pub fn texture(picture: &MapPicture) -> Texture {
    Texture { size: 128, levels: vec![picture.colors.iter().map(|&c| color(c)).collect()] }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_colours_follow_the_game_table() {
        assert_eq!(color(0), [0; 4]);
        assert_eq!(color(3), [0; 4]);
        assert_eq!(color(4 + 2), [127, 178, 56, 255]);
        assert_eq!(color(4), [89, 125, 39, 255]);
        assert_eq!(color(255), [0; 4]);
    }
}
