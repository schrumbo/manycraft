use std::rc::Rc;

use freya_engine::prelude::{
    SkMatrix,
    SkParagraph,
    SkPoint,
    SkRect,
};
use rustc_hash::FxHashMap;
use torin::prelude::{
    Area,
    Size2D,
};

use crate::{
    element::PaintAreaContext,
    node_id::NodeId,
    tree::Tree,
};

/// Above this many damaged areas in one frame, they are merged into their bounding box.
const MAX_RAW_RECTS: usize = 256;

/// Above this many disjoint rectangles after merging, they are merged into their bounding box.
const MAX_RECTS: usize = 16;

/// Damage covering more than this share of the window repaints the whole window.
const FULL_SHARE: f32 = 0.6;

/// What a node painted in a frame, compared between frames to find what changed.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Painted {
    element: *const (),
    area: Area,
    layer: i16,
    opacity: f32,
    blur: Option<f32>,
    rotation: Option<[f32; 9]>,
}

/// Finds the parts of a window that changed since the last frame, so a renderer that keeps the
/// last frame can repaint only those parts.
///
/// Elements that change go through the tree as new elements, so this compares what each node
/// painted in the last frame with what it paints now: its element, the area it paints into
/// (see [`ElementExt::paint_area`](crate::element::ElementExt::paint_area)), its layer and its
/// effects.
#[derive(Debug)]
pub struct DamageTracker {
    painted: FxHashMap<NodeId, Painted>,
    rects: Vec<Area>,
    full: bool,
}

impl Default for DamageTracker {
    fn default() -> Self {
        Self {
            painted: FxHashMap::default(),
            rects: Vec::new(),
            full: true,
        }
    }
}

impl DamageTracker {
    /// A tracker whose first frame repaints everything.
    pub fn new() -> Self {
        Self::default()
    }

    /// Repaint the whole window in the next frame, for example after a resize or when the
    /// kept frame was lost.
    pub fn invalidate(&mut self) {
        self.full = true;
    }

    /// The device-pixel rectangles to repaint in this frame, as found by the last
    /// [`collect`](Self::collect). Empty when nothing changed.
    pub fn rects(&self) -> &[Area] {
        &self.rects
    }

    /// The area node `node_id` paints into in this frame, in device pixels, or `None` when it
    /// paints nothing.
    pub fn painted_area(&self, node_id: &NodeId) -> Option<Area> {
        self.painted.get(node_id).map(|painted| painted.area)
    }

    /// Compares the tree with the last frame and finds the rectangles to repaint in a window of
    /// `size` device pixels.
    pub fn collect(&mut self, tree: &mut Tree, scale_factor: f64, size: Size2D) {
        let text_style_changed = std::mem::take(&mut tree.text_style_changed);
        let mut clip_areas = FxHashMap::default();
        let mut painted =
            FxHashMap::with_capacity_and_hasher(self.painted.len(), Default::default());
        let mut damage = Vec::new();

        for (layer, nodes) in tree.layers.iter() {
            for node_id in nodes {
                let Some(now) = paint_of(tree, *node_id, *layer, scale_factor, &mut clip_areas)
                else {
                    continue;
                };
                match self.painted.remove(node_id) {
                    Some(before) if before == now && !text_style_changed.contains(node_id) => {}
                    Some(before) => {
                        damage.push(before.area);
                        damage.push(now.area);
                    }
                    None => damage.push(now.area),
                }
                painted.insert(*node_id, now);
            }
        }
        damage.extend(self.painted.drain().map(|(_, before)| before.area));
        self.painted = painted;

        let window = Area::from_size(size);
        if self.full {
            self.full = false;
            self.rects = vec![window];
            return;
        }

        let blurs = self
            .painted
            .values()
            .filter(|painted| painted.blur.is_some())
            .map(|painted| painted.area)
            .filter(|area| damage.iter().any(|rect| rect.intersects(area)))
            .collect::<Vec<_>>();
        damage.extend(blurs);

        self.rects = merge(damage, window);
    }
}

/// Clips `damage` to `window`, rounds it out to whole pixels and merges overlapping rectangles.
fn merge(damage: Vec<Area>, window: Area) -> Vec<Area> {
    let mut rects = damage
        .into_iter()
        .filter_map(|rect| rect.round_out().intersection(&window))
        .filter(|rect| !rect.is_empty())
        .collect::<Vec<_>>();

    if rects.len() > MAX_RAW_RECTS {
        rects = vec![bounding(&rects)];
    }

    let mut merged = true;
    while merged {
        merged = false;
        let mut i = 0;
        while i < rects.len() {
            let mut j = i + 1;
            while j < rects.len() {
                if touches(&rects[i], &rects[j]) {
                    let other = rects.swap_remove(j);
                    rects[i] = rects[i].union(&other);
                    merged = true;
                } else {
                    j += 1;
                }
            }
            i += 1;
        }
    }

    if rects.len() > MAX_RECTS {
        rects = vec![bounding(&rects)];
    }

    let covered: f32 = rects.iter().map(Area::area).sum();
    if covered > window.area() * FULL_SHARE {
        return vec![window];
    }
    rects
}

fn touches(a: &Area, b: &Area) -> bool {
    a.min_x() <= b.max_x() && b.min_x() <= a.max_x() && a.min_y() <= b.max_y() && b.min_y() <= a.max_y()
}

fn bounding(rects: &[Area]) -> Area {
    rects
        .iter()
        .copied()
        .reduce(|a, b| a.union(&b))
        .unwrap_or_default()
}

/// What node `node_id` paints in this frame, mirroring what [`RenderPipeline`](crate::render_pipeline::RenderPipeline) draws.
fn paint_of(
    tree: &Tree,
    node_id: NodeId,
    layer: i16,
    scale_factor: f64,
    clip_areas: &mut FxHashMap<NodeId, Area>,
) -> Option<Painted> {
    let layout_node = tree.layout.get(&node_id)?;
    if layout_node.hidden {
        return None;
    }
    let element = tree.elements.get(&node_id)?;
    let text_style_state = tree.text_style_state.get(&node_id)?;
    let mut area = element.paint_area(PaintAreaContext {
        layout_node,
        text_style_state,
        scale_factor,
    });
    let mut painted = Painted {
        element: Rc::as_ptr(element) as *const (),
        area,
        layer,
        opacity: 1.,
        blur: None,
        rotation: None,
    };

    if let Some(effect_state) = tree.effect_state.get(&node_id) {
        area = scale_transformed_area(tree, area, &effect_state.scales);

        if !effect_state.rotations.is_empty() {
            let mut matrix = SkMatrix::new_identity();
            for id in effect_state.rotations.iter() {
                let rotated = tree.layout.get(id)?.visible_area();
                let effect = tree.effect_state.get(id)?;
                let origin = effect.transform_origin.origin(&rotated);
                let mut rotation = SkMatrix::new_identity();
                rotation.set_rotate(
                    effect.rotation.unwrap_or_default(),
                    Some(SkPoint {
                        x: origin.x,
                        y: origin.y,
                    }),
                );
                matrix = SkMatrix::concat(&matrix, &rotation);
            }
            let (rect, _) = matrix.map_rect(SkRect::new(
                area.min_x(),
                area.min_y(),
                area.max_x(),
                area.max_y(),
            ));
            area = Area::new(
                (rect.left, rect.top).into(),
                (rect.width(), rect.height()).into(),
            );
            let mut values = [0.; 9];
            matrix.get_9(&mut values);
            painted.rotation = Some(values);
        }

        for clip_node_id in effect_state.clips.iter() {
            let clip_area = match clip_areas.get(clip_node_id) {
                Some(clip_area) => *clip_area,
                None => {
                    let clip_layout_node = tree.layout.get(clip_node_id)?;
                    let clip_effect = tree.effect_state.get(clip_node_id)?;
                    let clip_area = scale_transformed_area(
                        tree,
                        clip_layout_node.visible_area(),
                        &clip_effect.scales,
                    );
                    clip_areas.insert(*clip_node_id, clip_area);
                    clip_area
                }
            };
            area = area.intersection(&clip_area)?;
        }

        painted.opacity = effect_state.opacities.iter().product();
        painted.blur = effect_state.blur;
    }

    if area.is_empty() {
        return None;
    }
    painted.area = area;
    Some(painted)
}

/// The area an element paints into when it draws `paragraph` with its top left corner at
/// `(x, y)`: its visible area, plus text that overflows it, glyphs that reach out of their line
/// and text shadows.
pub(crate) fn text_paint_area(paragraph: &SkParagraph, x: f32, y: f32, context: &PaintAreaContext) -> Area {
    let scale_factor = context.scale_factor as f32;
    let longest = paragraph.longest_line();
    let max_width = paragraph.max_width();
    let (width, overflow) = if max_width.is_finite() {
        (max_width.max(longest), (longest - max_width).max(0.))
    } else {
        (longest, 0.)
    };
    let text = Area::new(
        (x - overflow, y).into(),
        (width + overflow * 2., paragraph.height()).into(),
    );
    let style = context.text_style_state;
    let mut reach = f32::from(style.font_size) * scale_factor * 0.5;
    for shadow in style.text_shadows.iter() {
        let spread = shadow.offset.0.abs().max(shadow.offset.1.abs()) + shadow.blur_sigma as f32 * 3.;
        reach = reach.max(spread * scale_factor);
    }
    context
        .layout_node
        .visible_area()
        .union(&text)
        .inflate(reach + 1., reach + 1.)
}

/// Transforms an area with the accumulated scale effects of the given nodes.
pub(crate) fn scale_transformed_area(tree: &Tree, mut area: Area, scale_node_ids: &[NodeId]) -> Area {
    for node_id in scale_node_ids {
        let (Some(layout_node), Some(effect)) =
            (tree.layout.get(node_id), tree.effect_state.get(node_id))
        else {
            continue;
        };
        let Some(scale) = effect.scale else {
            continue;
        };
        let node_area = layout_node.visible_area();
        let origin = effect.transform_origin.origin(&node_area);

        area = area.translate(-origin.to_vector());
        area = area.scale(scale.x, scale.y);
        area = area.translate(origin.to_vector());
    }
    area
}

#[cfg(test)]
mod tests {
    use freya_engine::prelude::{
        FontCollection,
        FontMgr,
    };
    use torin::size::Size;

    use super::*;
    use crate::{
        integration::{
            NodesState,
            Runner,
        },
        prelude::*,
    };

    struct Frames {
        runner: Runner,
        tree: Tree,
        tracker: DamageTracker,
        fonts: FontCollection,
    }

    impl Frames {
        fn new(app: impl Fn() -> Element + 'static) -> (Self, State<u8>) {
            let mut runner = Runner::new(app);
            let state = runner.provide_root_context(|| State::create(0u8));
            let frames = Self {
                runner,
                tree: Tree::default(),
                tracker: DamageTracker::new(),
                fonts: FontCollection::new(),
            };
            (frames, state)
        }

        fn next(&mut self) -> Vec<Area> {
            self.tree.apply_mutations(self.runner.sync_and_update(), 1.0);
            let (events_sender, _events_receiver) = futures_channel::mpsc::unbounded();
            self.tree.measure_layout(
                WINDOW.size,
                &mut self.fonts,
                &FontMgr::default(),
                &events_sender,
                &mut NodesState::default(),
                1.0,
                &[],
            );
            self.tracker.collect(&mut self.tree, 1.0, WINDOW.size);
            self.tracker.rects().to_vec()
        }
    }

    fn two_squares() -> Element {
        let state = use_consume::<State<u8>>();
        let value = *state.read();
        let square = |color: Color, width: f32| {
            rect()
                .width(Size::px(width))
                .height(Size::px(100.))
                .background(color)
                .into_element()
        };
        let second = match value {
            0 => Some(square(Color::BLUE, 100.)),
            1 => Some(square(Color::GREEN, 100.)),
            2 => Some(square(Color::BLUE, 300.)),
            _ => None,
        };
        rect()
            .width(Size::px(500.))
            .height(Size::px(500.))
            .children([square(Color::RED, 100.)].into_iter().chain(second))
            .into_element()
    }

    #[test]
    fn first_frame_repaints_everything_and_a_still_frame_nothing() {
        let (mut frames, _) = Frames::new(two_squares);
        assert_eq!(frames.next(), vec![WINDOW]);
        assert!(frames.next().is_empty());
    }

    #[test]
    fn a_changed_element_damages_only_its_own_area() {
        let (mut frames, mut state) = Frames::new(two_squares);
        frames.next();
        state.set(1);
        assert_eq!(frames.next(), vec![area(0., 99., 101., 102.)]);
    }

    #[test]
    fn a_resized_element_damages_its_old_and_new_area() {
        let (mut frames, mut state) = Frames::new(two_squares);
        frames.next();
        state.set(2);
        assert_eq!(frames.next(), vec![area(0., 99., 301., 102.)]);
    }

    #[test]
    fn a_removed_element_damages_where_it_was() {
        let (mut frames, mut state) = Frames::new(two_squares);
        frames.next();
        state.set(3);
        assert_eq!(frames.next(), vec![area(0., 99., 101., 102.)]);
    }

    #[test]
    fn invalidating_repaints_everything_once() {
        let (mut frames, _) = Frames::new(two_squares);
        frames.next();
        frames.tracker.invalidate();
        assert_eq!(frames.next(), vec![WINDOW]);
        assert!(frames.next().is_empty());
    }

    fn area(x: f32, y: f32, w: f32, h: f32) -> Area {
        Area::new((x, y).into(), (w, h).into())
    }

    const WINDOW: Area = Area::new(
        torin::prelude::Point2D::new(0., 0.),
        Size2D::new(1000., 1000.),
    );

    #[test]
    fn merges_overlapping_and_keeps_disjoint() {
        let rects = merge(
            vec![
                area(10., 10., 20., 20.),
                area(25., 25., 20., 20.),
                area(500., 500., 10., 10.),
            ],
            WINDOW,
        );
        assert_eq!(rects.len(), 2);
        assert!(rects.contains(&area(10., 10., 35., 35.)));
        assert!(rects.contains(&area(500., 500., 10., 10.)));
    }

    #[test]
    fn rounds_out_to_whole_pixels_and_clips_to_the_window() {
        let rects = merge(vec![area(10.4, 10.6, 5.2, 5.2), area(990.5, 990.5, 50., 50.)], WINDOW);
        assert!(rects.contains(&area(10., 10., 6., 6.)));
        assert!(rects.contains(&area(990., 990., 10., 10.)));
    }

    #[test]
    fn drops_damage_outside_the_window() {
        assert!(merge(vec![area(2000., 2000., 10., 10.)], WINDOW).is_empty());
    }

    #[test]
    fn large_damage_repaints_the_whole_window() {
        assert_eq!(merge(vec![area(0., 0., 900., 900.)], WINDOW), vec![WINDOW]);
    }

    #[test]
    fn many_rects_collapse_into_their_bounds() {
        let damage = (0..20).map(|i| area(i as f32 * 40., 0., 10., 10.)).collect();
        assert_eq!(merge(damage, WINDOW), vec![area(0., 0., 770., 10.)]);
    }

    #[test]
    fn merging_is_transitive() {
        let rects = merge(
            vec![
                area(0., 0., 10., 10.),
                area(100., 0., 10., 10.),
                area(5., 0., 100., 10.),
            ],
            WINDOW,
        );
        assert_eq!(rects, vec![area(0., 0., 110., 10.)]);
    }
}
