use std::time::Duration;

use anyhow::Result;
use image::{Rgba, RgbaImage};
use uuid::Uuid;

use super::{AddError, Fetched, Origin, Outfit, Owned, now, keep_texture};
use crate::core::account::Account;
use crate::core::auth::AuthError;
use crate::core::drive;
use crate::core::skin_change::{Applied, Cape, Model, SkinError, Wardrobe};

const DELAY: Duration = Duration::from_millis(450);
const SKIN: &str = "ca11ed";
const CAPES: [(&str, &str, &str, [u8; 3], [u8; 3]); 3] = [
    ("canned-migrator", "Migrator", "ca0001", [0x6b, 0x3f, 0x1f], [0xd9, 0xa4, 0x41]),
    ("canned-pan", "Pan", "ca0002", [0x2f, 0x7d, 0x3a], [0xf2, 0xe2, 0x9a]),
    ("canned-common", "Common", "ca0003", [0x3a, 0x4c, 0x8c], [0xe0, 0xe0, 0xe0]),
];

fn outcome() -> String {
    drive::dir().and_then(|d| std::fs::read_to_string(d.join("skin")).ok()).unwrap_or_default().trim().to_owned()
}

pub(super) fn skin(shirt: [u8; 3], trousers: [u8; 3]) -> RgbaImage {
    let mut s = RgbaImage::new(64, 64);
    let mut fill = |x0: u32, y0: u32, w: u32, h: u32, c: [u8; 3]| {
        for y in y0..y0 + h {
            for x in x0..x0 + w {
                let shade = if (x + y) % 3 == 0 { 12 } else { 0 };
                s.put_pixel(x, y, Rgba([c[0].saturating_sub(shade), c[1].saturating_sub(shade), c[2].saturating_sub(shade), 255]));
            }
        }
    };
    fill(0, 0, 32, 16, [0xc6, 0x8d, 0x6b]);
    fill(8, 0, 16, 8, [0x3b, 0x26, 0x15]);
    fill(0, 8, 32, 2, [0x3b, 0x26, 0x15]);
    fill(16, 16, 24, 16, shirt);
    fill(40, 16, 16, 16, shirt);
    fill(44, 26, 12, 6, [0xc6, 0x8d, 0x6b]);
    fill(0, 16, 16, 16, trousers);
    fill(16, 48, 16, 16, trousers);
    fill(32, 48, 16, 16, shirt);
    fill(36, 58, 12, 6, [0xc6, 0x8d, 0x6b]);
    fill(9, 12, 2, 1, [0xff, 0xff, 0xff]);
    fill(13, 12, 2, 1, [0xff, 0xff, 0xff]);
    fill(10, 12, 1, 1, [0x4a, 0x3a, 0x8f]);
    fill(13, 12, 1, 1, [0x4a, 0x3a, 0x8f]);
    s
}

fn worn_skin() -> RgbaImage {
    skin([0x2b, 0x9c, 0xa8], [0x35, 0x3f, 0x8c])
}

fn cape(c: [u8; 3], stripe: [u8; 3]) -> RgbaImage {
    RgbaImage::from_fn(64, 32, |x, y| {
        let band = (6..9).contains(&y) || (x + y) % 7 == 0;
        let p = if band { stripe } else { c };
        Rgba([p[0], p[1], p[2], 255])
    })
}

fn variant(seed: &str) -> RgbaImage {
    let sum = seed.bytes().fold(7u32, |a, b| a.wrapping_mul(31).wrapping_add(u32::from(b)));
    let pick = |shift: u32| ((sum >> shift) & 0xff) as u8;
    skin([pick(0) | 0x40, pick(8) | 0x40, pick(16) | 0x40], [pick(4) & 0x7f, pick(12) & 0x7f, pick(20) | 0x30])
}

pub(super) fn fetched(input: &str, origin: Origin, from: Option<String>) -> Result<Fetched> {
    std::thread::sleep(DELAY);
    match (outcome().as_str(), origin) {
        ("noplayer", Origin::Player) => return Err(AddError::NoSuchPlayer.into()),
        ("notimage", Origin::Link) => return Err(AddError::NotAnImage.into()),
        ("ratelimit", _) => return Err(AddError::RateLimited.into()),
        _ => {}
    }
    let png = crate::core::skin_change::png(&variant(input))?;
    let name = input.rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or(input).trim_end_matches(".png").to_owned();
    Ok(Fetched { png, model: if input.len() % 2 == 0 { Model::Classic } else { Model::Slim }, name, origin, from })
}

pub(super) fn outfit(account: &Account) -> Result<Outfit> {
    std::thread::sleep(DELAY);
    match outcome().as_str() {
        "loadfail" => return Err(SkinError::Unavailable(503).into()),
        "signin" => return Err(AuthError::SignInAgain.into()),
        _ => {}
    }
    keep_texture(SKIN, &worn_skin())?;
    let all = outcome() != "nocapes";
    let mut capes = Vec::new();
    for (id, alias, texture, base, stripe) in CAPES.iter().filter(|_| all) {
        keep_texture(texture, &cape(*base, *stripe))?;
        capes.push(Owned { id: (*id).into(), alias: (*alias).into(), texture: (*texture).into() });
    }
    drive::note(format!("skin canned outfit {}", account.name));
    Ok(Outfit { skin: Some(SKIN.into()), model: Model::Classic, cape: all.then(|| "canned-pan".to_owned()), capes, fetched: now() })
}

pub(super) fn apply(before: Option<&Outfit>, upload: Option<(&[u8], Model)>, cape: Option<Option<&str>>) -> Applied {
    std::thread::sleep(DELAY);
    let outcome = outcome();
    drive::note(format!("skin canned {}", if outcome.is_empty() { "ok" } else { &outcome }));
    if outcome == "ratelimit" {
        return Applied { wardrobe: None, skin_done: false, error: Some(SkinError::RateLimited.into()) };
    }
    let worn = match cape {
        Some(chosen) => chosen.map(str::to_owned),
        None => before.and_then(|b| b.cape.clone()),
    };
    let capes = if outcome == "nocapes" { &CAPES[..0] } else { &CAPES[..] };
    let texture = match upload {
        Some(_) => Uuid::new_v4().simple().to_string(),
        None => before.and_then(|b| b.skin.clone()).unwrap_or_else(|| SKIN.into()),
    };
    let wardrobe = Wardrobe {
        skin_url: Some(format!("http://textures.minecraft.net/texture/{texture}")),
        model: upload.map(|u| u.1).or(before.map(|b| b.model)).unwrap_or_default(),
        capes: capes
            .iter()
            .map(|(id, alias, tex, _, _)| Cape { id: (*id).into(), alias: (*alias).into(), url: format!("http://textures.minecraft.net/texture/{tex}"), active: worn.as_deref() == Some(*id) })
            .collect(),
    };
    let error = (outcome == "capefail" && cape.is_some()).then(|| SkinError::CapeRejected.into());
    Applied { wardrobe: Some(wardrobe), skin_done: upload.is_some(), error }
}
