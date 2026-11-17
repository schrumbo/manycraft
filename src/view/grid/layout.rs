//! Where the tiles go: one uniform 16:9 size for every tile, S, M, L or the largest that fits the window without
//! scrolling, rows filled from the left and the whole block centred in the leftover space. Fit also stretches its
//! rows to the full width where that costs no scrolling.

/// The tile size the person chose.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TileSize {
    Small,
    Medium,
    Large,
    /// The largest uniform size that shows every tile at once, between S and L.
    #[default]
    Fit,
}

impl TileSize {
    pub const ALL: [TileSize; 4] = [TileSize::Small, TileSize::Medium, TileSize::Large, TileSize::Fit];

    /// One step smaller, from the size now on screen; Fit counts as the size it came to.
    pub fn smaller(self, shown: f32) -> Self {
        let now = if self == Self::Fit { Self::nearest(shown) } else { self };
        match now {
            Self::Large => Self::Medium,
            _ => Self::Small,
        }
    }

    /// One step larger, from the size now on screen.
    pub fn larger(self, shown: f32) -> Self {
        let now = if self == Self::Fit { Self::nearest(shown) } else { self };
        match now {
            Self::Small => Self::Medium,
            _ => Self::Large,
        }
    }

    fn nearest(width: f32) -> Self {
        if width < (SMALL + MEDIUM) / 2. {
            Self::Small
        } else if width < (MEDIUM + LARGE) / 2. {
            Self::Medium
        } else {
            Self::Large
        }
    }

    /// The preview width of a fixed size.
    pub fn width(self) -> Option<f32> {
        match self {
            Self::Small => Some(SMALL),
            Self::Medium => Some(MEDIUM),
            Self::Large => Some(LARGE),
            Self::Fit => None,
        }
    }
}

/// Preview widths of S, M and L; the capture size of a preview follows them.
pub const SMALL: f32 = 160.;
pub const MEDIUM: f32 = 320.;
pub const LARGE: f32 = 640.;
/// Below this width a tile shows one line under its preview, from it on two.
pub const TWO_LINES: f32 = 240.;
/// The height of the caption with one line and with two.
pub const STRIP_ONE: f32 = 28.;
pub const STRIP_TWO: f32 = 44.;
/// The height of the caption under a picture `width` wide, with one line or two.
pub fn strip(width: f32) -> f32 {
    if width < TWO_LINES { STRIP_ONE } else { STRIP_TWO }
}

/// The height of the picture of a tile `width` wide.
pub fn picture_h(width: f32) -> f32 {
    (width * 9. / 16.).floor()
}

/// The height of a whole tile `width` wide.
pub fn tile_height(width: f32) -> f32 {
    picture_h(width) + strip(width)
}

/// The tiles placed in a box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grid {
    pub columns: usize,
    pub rows: usize,
    pub tile_w: f32,
    pub tile_h: f32,
    pub gap: f32,
    /// Where the first column starts, so the block is centred.
    pub left: f32,
    /// The height of all rows with the padding, to scroll when it is more than the box.
    pub content_h: f32,
}

impl Grid {
    /// The top left of tile `index`, inside the scrolled content.
    pub fn origin(&self, index: usize, pad: f32) -> (f32, f32) {
        let (col, row) = (index % self.columns.max(1), index / self.columns.max(1));
        (self.left + col as f32 * (self.tile_w + self.gap), pad + row as f32 * (self.tile_h + self.gap))
    }

    /// The tile under `(x, y)` in content coordinates, if any.
    pub fn hit(&self, count: usize, pad: f32, x: f32, y: f32) -> Option<usize> {
        if x < self.left || y < pad {
            return None;
        }
        let col = ((x - self.left) / (self.tile_w + self.gap)) as usize;
        let row = ((y - pad) / (self.tile_h + self.gap)) as usize;
        let (ox, oy) = (self.left + col as f32 * (self.tile_w + self.gap), pad + row as f32 * (self.tile_h + self.gap));
        let index = row * self.columns + col;
        (col < self.columns && index < count && x < ox + self.tile_w && y < oy + self.tile_h).then_some(index)
    }

    /// The range of rows that intersect the visible band from `top` to `top + height`.
    pub fn visible_rows(&self, pad: f32, top: f32, height: f32) -> std::ops::Range<usize> {
        let step = self.tile_h + self.gap;
        let first = ((top - pad).max(0.) / step) as usize;
        let last = (((top + height - pad).max(0.) / step) as usize + 1).min(self.rows);
        first.min(last)..last
    }
}

/// Places `count` tiles of `size` in a box of `width` by `height`, with `gap` between tiles and `pad` around them.
pub fn layout(count: usize, size: TileSize, width: f32, height: f32, gap: f32, pad: f32) -> Grid {
    let inner = (width - pad * 2.).max(SMALL);
    let tile_w = match size.width() {
        Some(w) => w.min(inner.max(SMALL)),
        None => fit(count, inner, (height - pad * 2.).max(0.), gap),
    }
    .floor();
    let columns = (((inner + gap) / (tile_w + gap)).floor() as usize).max(1);
    let columns = if count == 0 { columns } else { columns.min(count) };
    let rows = count.div_ceil(columns.max(1));
    let tall = |w: f32| pad * 2. + rows as f32 * tile_height(w) + rows.saturating_sub(1) as f32 * gap;
    let wide = ((inner - columns.saturating_sub(1) as f32 * gap) / columns.max(1) as f32).min(LARGE).floor();
    let tile_w = if size == TileSize::Fit && count > 0 && wide > tile_w && (tall(wide) <= height || tall(tile_w) > height) { wide } else { tile_w };
    let tile_h = tile_height(tile_w);
    let block = columns as f32 * tile_w + columns.saturating_sub(1) as f32 * gap;
    let left = pad + ((inner - block) / 2.).max(0.).floor();
    let content_h = pad * 2. + rows as f32 * tile_h + rows.saturating_sub(1) as f32 * gap;
    Grid { columns, rows, tile_w, tile_h, gap, left, content_h }
}

fn fit(count: usize, inner_w: f32, inner_h: f32, gap: f32) -> f32 {
    if count == 0 {
        return MEDIUM;
    }
    let mut best = SMALL;
    for columns in 1..=count {
        let rows = count.div_ceil(columns) as f32;
        let by_width = (inner_w - (columns - 1) as f32 * gap) / columns as f32;
        let room = (inner_h - (rows - 1.) * gap) / rows;
        let two = (room - STRIP_TWO) * 16. / 9.;
        let by_height = if two >= TWO_LINES { two } else { ((room - STRIP_ONE) * 16. / 9.).min(TWO_LINES - 1.) };
        let w = by_width.min(by_height).min(LARGE);
        if w > best {
            best = w;
        }
        if by_width < SMALL {
            break;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_sizes_keep_their_width_and_centre_the_block() {
        let g = layout(10, TileSize::Medium, 1200., 800., 12., 16.);
        assert_eq!(g.tile_w, MEDIUM);
        assert_eq!(g.columns, 3);
        assert_eq!(g.rows, 4);
        let block = 3. * MEDIUM + 2. * 12.;
        assert_eq!(g.left, 16. + ((1200. - 32. - block) / 2.).floor());
    }

    #[test]
    fn the_last_row_does_not_stretch() {
        let g = layout(4, TileSize::Small, 600., 400., 8., 16.);
        assert_eq!(g.columns, 3);
        assert_eq!(g.origin(3, 16.), (g.left, 16. + g.tile_h + 8.));
        assert_eq!(g.tile_w, SMALL);
    }

    #[test]
    fn fit_shows_every_tile_without_scrolling_when_it_can() {
        for count in [1, 2, 5, 12, 30] {
            let g = layout(count, TileSize::Fit, 1440. - 232., 900. - 56., 12., 16.);
            assert!(g.content_h <= 900. - 56., "{count}: {} > box", g.content_h);
            assert!(g.tile_w >= SMALL && g.tile_w <= LARGE);
        }
    }

    #[test]
    fn fit_never_goes_below_small_and_scrolls_instead() {
        let g = layout(400, TileSize::Fit, 1200., 800., 8., 16.);
        assert_eq!(g.tile_w, SMALL);
        assert!(g.content_h > 800.);
    }

    #[test]
    fn fit_fills_the_width_when_it_scrolls_anyway() {
        let g = layout(400, TileSize::Fit, 1250., 800., 8., 16.);
        assert!(g.tile_w > SMALL);
        assert_eq!(g.left, 16.);
        let block = g.columns as f32 * g.tile_w + (g.columns - 1) as f32 * 8.;
        assert!(1250. - 32. - block < g.columns as f32);
    }

    #[test]
    fn fit_fills_the_width_only_while_nothing_starts_to_scroll() {
        for count in [2, 3, 7, 12, 30] {
            let g = layout(count, TileSize::Fit, 1208., 844., 12., 16.);
            assert!(g.content_h <= 844., "{count}: {} > box", g.content_h);
        }
    }

    #[test]
    fn fit_caps_one_tile_at_large() {
        let g = layout(1, TileSize::Fit, 3000., 2000., 12., 16.);
        assert_eq!(g.tile_w, LARGE);
        assert_eq!(g.columns, 1);
    }

    #[test]
    fn hits_land_on_tiles_and_miss_the_gaps() {
        let g = layout(5, TileSize::Small, 800., 600., 10., 16.);
        let (x, y) = g.origin(4, 16.);
        assert_eq!(g.hit(5, 16., x + 1., y + 1.), Some(4));
        assert_eq!(g.hit(5, 16., x + g.tile_w + 4., y + 1.), None);
        assert_eq!(g.hit(5, 16., 0., 0.), None);
    }

    #[test]
    fn visible_rows_cover_the_band() {
        let g = layout(100, TileSize::Small, 900., 600., 10., 16.);
        let rows = g.visible_rows(16., g.tile_h * 3., 300.);
        assert!(rows.start <= 3 && rows.end > 3 && rows.end <= g.rows);
    }

    #[test]
    fn steps_go_from_the_size_on_screen() {
        assert_eq!(TileSize::Fit.larger(300.), TileSize::Large);
        assert_eq!(TileSize::Fit.smaller(300.), TileSize::Small);
        assert_eq!(TileSize::Large.larger(0.), TileSize::Large);
        assert_eq!(TileSize::Small.smaller(0.), TileSize::Small);
    }
}
