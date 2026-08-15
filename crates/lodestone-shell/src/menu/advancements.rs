//! The Advancements screen — vanilla's `AdvancementsScreen`,
//! reached from the pause menu's Advancements button.
//!
//! ## Where the progress comes from
//!
//! The tree *shape* is static data ([`super::advancement_data`], the real 26.2
//! data pack: 126 advancements over 5 roots), and the *progress* is live —
//! `UPDATE_ADVANCEMENTS` decodes into `ClientEvent::AdvancementsUpdated`, folds
//! into `lodestone_ecs::session::SessionAdvancements`, and reaches here as an
//! [`AdvancementProgress`] snapshot built by [`AdvancementProgress::from_store`].
//!
//! **The two halves are joined by id, in one direction only.** The store carries
//! no positions — 26.2's advancement JSON has no `x`/`y` and the server computes
//! them with `TreeNodePosition` — so the layout is always ours, run over
//! [`ADVANCEMENTS`], and the store is only ever *looked up* per id. Rebuilding
//! the forest from the store's own `parent` links would give a second, unpositioned
//! tree that disagrees with the one being drawn.
//!
//! An empty store therefore draws exactly what it drew before the wire landed:
//! every widget unobtained, no progress readouts. That is the true state of a
//! fresh world, not a placeholder.
//!
//! ## Geometry
//!
//! Drawn through [`ContainerGeometry`] and
//! [`ContainerRenderer::render_geometry_scaled`](crate::container::ContainerRenderer),
//! not through [`super::render::MenuFrame`]. That is deliberate: this screen is
//! sprite-and-item-icon work at arbitrary positions, which is what the container
//! path already does (item icons, a GUI sprite atlas, loose panel art, the dim
//! gradient), and what the row/label `MenuFrame` system does not. The creative
//! inventory screen uses the same seam for the same reason.
//!
//! **Clipping is done on the CPU, not via a GPU scissor.**
//! `render_geometry_scaled` has no scissor (unlike vanilla's own
//! `enableScissor`/`disableScissor` bracket around `AdvancementTab.
//! drawWidgets`), so [`advancements_geometry`] clamps every piece of tree
//! content to the `234 x 113` viewport by hand instead: the connector lines
//! (`clamp_to`, a plain `Rect` intersection) and a widget's frame sprite too
//! (`push_sprite_clipped`/`clip_sprite_quad`,
//! which shrinks the sprite's UV rect in lock-step with its destination rect
//! rather than squishing the art to fit). Before that fix a widget crossing
//! the boundary went from wholly undrawn to its full, unclamped `26 x 26`
//! frame the instant [`AdvancementsLayout::widgets`]'s inclusion test
//! (`overlaps`, deliberately permissive so an edge click still lands) turned
//! true — a visible **pop**, not a clip.
//!
//! **The item icon inside each frame is now clipped too**
//! ([`draw_stack_clipped`]) — [`Builder::draw_stack`] itself still has no
//! sub-rect clip primitive (it composites up to four streams: a flat item
//! sprite plus its glint copy, a 3-D block-item mesh, a special-renderer icon,
//! and colour-stream chrome), so the clip lives at this module's own call
//! site instead, applied *after* the fact to whichever vertices `draw_stack`
//! just appended. The flat sprite and the colour-stream chrome (the
//! atlas-less swatch/letter fallback, the durability bar, the stack count)
//! are shrunk to their intersection with the viewport in the same
//! [`clip_sprite_quad`] shape [`push_sprite_clipped`] already uses for a
//! frame, so the two clipping paths agree rather than growing a second
//! convention. The two 3-D paths are not `GuiSpriteQuad`-shaped, but they are
//! not both equally clippable either. A block item's isometric mini-model
//! ([`draw_stack_clipped`]'s `model_verts` stream) is already posed into GUI
//! *pixel* space on the CPU (`gui_item_pose`; the GPU-side `gui_ortho`
//! projection never touches these vertices), so its triangles genuinely
//! straddle `clip` in the same coordinate space `clip` is expressed in and
//! [`clip_model_triangles`] cuts them with a real polygon clip
//! (Sutherland-Hodgman) rather than dropping them — this is the fix for the
//! reported bug, block icons vanishing at the tree's edge while item icons
//! clip cleanly. A chest-shaped special-renderer icon is the one case left
//! unclipped: its mesh is built from a placement matrix inside the GPU-side
//! icon pass with no CPU vertex list to cut, so it alone is still dropped
//! whole when the icon's own bounding square is not wholly inside the
//! viewport — which can only ever draw *fewer* pixels than vanilla, never
//! spill past the edge.
//!
//! **Vanilla draws the connector lines behind every widget, and so do we.**
//! `AdvancementTab.extractContents` calls `root.extractConnectivity` (both
//! the shadow and the foreground pass) for the whole tree *before*
//! `root.extractRenderState` (the frame-then-icon draw), so a line is always
//! the bottom layer. An early version of this code pushed the widget-frame
//! loop before both `bg_slot_floats` and `chrome_floats` — the renderer's
//! early "back" bg pass and the pre-lines part of the colour stream — so a
//! frame always landed in an *earlier* pass or range than the lines crossing
//! it, and a line drew over every card it touched. The measured
//! `task_frame_obtained`/`task_frame_unobtained` sprites are fully opaque
//! under the icon's own footprint (alpha 255 across the whole 16×16 centre,
//! re-verified), so — unlike the lines, which never need to sit *in front of*
//! anything — the frame has to stay strictly behind the icon it frames.
//!
//! Achieving `tiles < lines < frame < icon` inside
//! [`ContainerRenderer`](crate::container::ContainerRenderer)'s original
//! six-pass sequence (dim → bg-back → chrome → model/item → bg-front →
//! carried) forced the frame loop into the bg-front pass and every widget's
//! icon into the carried tier, alongside the hover tooltip's own redraw —
//! which left no pass between "chrome" and "carried" for the hover-dim
//! (`AdvancementsView::fade`) to land in, so it could darken the tile grid,
//! the connector lines and the tab icons but not a widget's own frame or
//! icon. The renderer now carries a **third**, independent "mid" tier —
//! [`ContainerGeometry::mid_bg_verts`]/`mid_verts`/`mid_item_verts`/
//! `mid_glint_verts`, plus `dim2_verts` for the dim itself — built below from
//! a **separate** [`Builder`] (`mid`, not `b`) and drawn by
//! [`ContainerRenderer`](crate::container::ContainerRenderer) in its own
//! passes, positioned after the existing "chrome"/"item" passes and before
//! the existing bg-front/carried passes: frame, then icon, then the dim,
//! then (unmoved) the tooltip. Every field defaults to empty for every other
//! caller (the container screens, the creative menu, the recipe panel), so
//! this is a verified no-op everywhere but here — see
//! [`ContainerRenderer`](crate::container::ContainerRenderer)'s own pass
//! code for the enumeration.
//!
//! One case still does not reach the dim: a widget icon backed by a 3-D
//! block model or a special-renderer icon (a chest) has no "mid"-tier pass —
//! `IconStratum` has only `Slots`/`Carried` and lives in
//! `crate::hud::item_icon`, outside this fix's file ownership — so those
//! stay in the ordinary carried tier, undimmed, a documented, narrower gap
//! than the one this fix closes. See the `mid` builder's own doc at its call
//! site for the drain that keeps those icons visible rather than dropping
//! them.

use lodestone_assets::ItemAtlas;
use lodestone_game::item::ItemStack;
use lodestone_render::{BlockModels, GuiSpriteQuad, ModelVertex};

use crate::container::builder::Builder;
use crate::container::{ContainerBackground, ContainerGeometry, Rect};
use crate::hud::item_icon::IconAssets;

use super::advancement_data::{ADVANCEMENTS, Advancement, AdvancementFrame};
use super::advancement_tree::{TreeLayout, layout_tree};

/// `WINDOW_WIDTH` / `WINDOW_HEIGHT` (`AdvancementsScreen.java`).
pub const WINDOW_W: f32 = 252.0;
/// See [`WINDOW_W`].
pub const WINDOW_H: f32 = 140.0;
/// `WINDOW_INSIDE_X` / `WINDOW_INSIDE_Y` (`:28-29`).
const INSIDE_X: f32 = 9.0;
/// See [`INSIDE_X`].
const INSIDE_Y: f32 = 18.0;
/// `WINDOW_INSIDE_WIDTH` / `WINDOW_INSIDE_HEIGHT` (`:30-31`).
pub const INSIDE_W: f32 = 234.0;
/// See [`INSIDE_W`].
pub const INSIDE_H: f32 = 113.0;
/// `WINDOW_TITLE_X` / `WINDOW_TITLE_Y` (`:32-33`).
const TITLE_X: f32 = 8.0;
/// See [`TITLE_X`].
const TITLE_Y: f32 = 6.0;
/// `graphics.text(..., -12566464, false)` (`:216`) — `0xFF404040`, unshadowed.
const TITLE_COLOUR: [f32; 4] = [0.25, 0.25, 0.25, 1.0];
/// `BACKGROUND_TILE_WIDTH`/`HEIGHT` (`:36-37`); the tile textures really are
/// 16×16 (measured on the real pack, not assumed from the constant).
const TILE: f32 = 16.0;
/// `SCROLL_SPEED = 16.0` (`:40`).
const SCROLL_SPEED: f32 = 16.0;

/// One tab button, `AdvancementTabType.ABOVE` — `28 x 32`, `max 8`
/// (`AdvancementTabType.java`). With five roots every tab is `ABOVE`, so
/// the other three variants are unreachable and deliberately unported; add them
/// the day a data pack ships a ninth root.
const TAB_W: f32 = 28.0;
/// See [`TAB_W`].
const TAB_H: f32 = 32.0;
/// `getX` for `ABOVE`: `(width + 4) * index` (`:137`).
const TAB_PITCH: f32 = TAB_W + 4.0;
/// `getY` for `ABOVE`: `-height + 4` (`:146`).
const TAB_DY: f32 = -TAB_H + 4.0;
/// `extractIcon`'s `ABOVE` nudge: `x += 6; y += 9` (`:116-118`).
const TAB_ICON_DX: f32 = 6.0;
/// See [`TAB_ICON_DX`].
const TAB_ICON_DY: f32 = 9.0;

/// `this.x = floor(display.getX() * 28.0F)` (`AdvancementWidget.java`).
const NODE_PITCH_X: f32 = 28.0;
/// `this.y = floor(display.getY() * 27.0F)` (`:62`).
const NODE_PITCH_Y: f32 = 27.0;
/// `blitSprite(..., xo + x + 3, yo + y, 26, 26)` (`:164`).
const FRAME_SIZE: f32 = 26.0;
/// See [`FRAME_SIZE`].
const FRAME_DX: f32 = 3.0;
/// `fakeItem(icon, xo + x + 8, yo + y + 5)` (`:165`).
const ICON_DX: f32 = 8.0;
/// See [`ICON_DX`].
const ICON_DY: f32 = 5.0;
/// `isMouseOver`'s rect is `26 x 26` from `(x, y)` — **not** offset by
/// [`FRAME_DX`] (`:292-295`). Vanilla's own 3 px disagreement between the art
/// and the hit region, kept.
const HIT_SIZE: f32 = 26.0;

/// `advancements/title_box`, the description panel behind the hover tooltip
/// (`AdvancementWidget.java`, blitted at `:233`/`:235`).
const SPRITE_TITLE_BOX: &str = "advancements/title_box";
/// `AdvancementWidgetType::boxSprite` — the hover tooltip's *title bar*, which
/// splits into an obtained and an unobtained half at the progress fraction.
const SPRITE_BOX_OBTAINED: &str = "advancements/box_obtained";
/// See [`SPRITE_BOX_OBTAINED`].
const SPRITE_BOX_UNOBTAINED: &str = "advancements/box_unobtained";

/// One text line's height, vanilla's font line advance.
const LINE_H: f32 = 9.0;
/// `TITLE_MAX_WIDTH` (`AdvancementWidget.java`) — the wrap width for the
/// title.
const TITLE_MAX_WIDTH: f32 = 163.0;
/// `TITLE_MIN_WIDTH` (`:39`).
const TITLE_MIN_WIDTH: f32 = 80.0;
/// `TITLE_PADDING_LEFT`/`RIGHT`/`TOP`/`BOTTOM` (`:33-37`).
const TITLE_PAD_LEFT: f32 = 3.0;
/// See [`TITLE_PAD_LEFT`].
const TITLE_PAD_RIGHT: f32 = 5.0;
/// See [`TITLE_PAD_LEFT`].
const TITLE_PAD_TOP: f32 = 9.0;
/// See [`TITLE_PAD_LEFT`].
const TITLE_PAD_BOTTOM: f32 = 8.0;
/// `TITLE_X` (`:35`) — where the title text starts inside a right-side box.
const TITLE_TEXT_X: f32 = 32.0;
/// `findOptimalLines`' candidate margins (`:40`), tried in order.
const TEST_SPLIT_OFFSETS: [f32; 5] = [0.0, 10.0, -10.0, 25.0, -25.0];
/// `-16711936` (`:274`/`:276`) — `0xFF00FF00`, the description's green.
const DESCRIPTION_COLOUR: [f32; 4] = [0.0, 1.0, 0.0, 1.0];
/// `-1`, white, for the title and the progress readout.
const HOVER_TEXT_COLOUR: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

/// Every advancement's progress, resolved by id against [`ADVANCEMENTS`].
///
/// Built per frame from `SessionAdvancements` while the screen is open — see the
/// module doc for why the join goes this way round and never the other.
#[derive(Debug, Clone, Default)]
pub struct AdvancementProgress {
    entries: std::collections::HashMap<&'static str, NodeProgress>,
}

/// One advancement's progress, in the units vanilla's readouts use.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct NodeProgress {
    /// Completed requirement **groups** — `AdvancementProgress`'s own
    /// `countCompletedRequirements`, which is an AND-of-ORs count and *not* the
    /// number of obtained criteria.
    pub done: u32,
    /// How many groups the server declared. `0` until a node arrives.
    pub total: u32,
    /// `AdvancementProgress::isDone`.
    pub obtained: bool,
}

impl NodeProgress {
    /// `getPercent()`: completed groups over declared groups, `0.0` with none.
    #[must_use]
    pub fn percent(self) -> f32 {
        if self.total == 0 {
            return 0.0;
        }
        self.done as f32 / self.total as f32
    }
}

impl AdvancementProgress {
    /// Resolve every [`ADVANCEMENTS`] entry against the session store.
    ///
    /// The `total` comes from the **server's** `requirements` when the node
    /// arrived and falls back to the data pack's own `requirement_count`, so a
    /// tree the server has not sent still shows the right denominator.
    #[must_use]
    pub fn from_store(store: &lodestone_game::advancement::AdvancementStore) -> Self {
        let mut entries = std::collections::HashMap::new();
        for advancement in ADVANCEMENTS {
            let Ok(id) = advancement.id.parse::<lodestone_model::Identifier>() else {
                continue;
            };
            let Some(node) = store.get(&id) else { continue };
            let progress = store.progress(&id);
            let done = progress.map_or(0, |p| {
                node.requirements
                    .iter()
                    .filter(|group| group.iter().any(|name| p.is_criterion_done(name)))
                    .count() as u32
            });
            let total = if node.requirements.is_empty() {
                advancement.requirement_count
            } else {
                node.requirements.len() as u32
            };
            entries.insert(
                advancement.id,
                NodeProgress {
                    done,
                    total,
                    obtained: store.completion(&id).unwrap_or(false),
                },
            );
        }
        Self { entries }
    }

    /// One advancement's progress; all-zero for an id the server never sent.
    #[must_use]
    pub fn get(&self, id: &str) -> NodeProgress {
        self.entries.get(id).copied().unwrap_or_default()
    }

    /// Whether an advancement is complete.
    #[must_use]
    pub fn obtained(&self, id: &str) -> bool {
        self.get(id).obtained
    }

    /// How many advancements are complete — the toast queue's own seed check,
    /// and a cheap "has the server told us anything" probe.
    #[must_use]
    pub fn obtained_count(&self) -> usize {
        self.entries.values().filter(|p| p.obtained).count()
    }

    /// Whether the server has sent any node at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Vanilla's `-1` (white) for the foreground connector line and `-16777216`
/// (black) for the wider shadow underneath it (`AdvancementWidget.java`).
const LINE_FG: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
/// See [`LINE_FG`].
const LINE_BG: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

/// `extractTooltips`' `fill(0, 0, 234, 113, floor(fade * 255) << 24)`
/// (`AdvancementTab.java`): the viewport dims while a widget is hovered.
/// [`FADE_CEILING`] is the alpha it reaches; the ramp is
/// [`AdvancementsState::tick_fade`].
const HOVER_DIM_RGB: [f32; 3] = [0.0, 0.0, 0.0];
/// `Mth.clamp(fade + 0.06F, 0.0F, 0.3F)` (`AdvancementTab.java`) — the rise
/// per frame and its ceiling.
const FADE_RISE: f32 = 0.06;
/// See [`FADE_RISE`].
const FADE_CEILING: f32 = 0.3;
/// `Mth.clamp(fade - 0.12F, 0.0F, 1.0F)` (`:100`) — twice as fast out as in.
const FADE_FALL: f32 = 0.12;

/// Every advancements sprite id, for [`crate::container`]'s own GUI atlas — the
/// six `ABOVE` tab variants, the six frames, the description panel and the two
/// title-bar boxes.
pub(crate) const ADVANCEMENT_SPRITES: [&str; 15] = [
    "advancements/tab_above_left",
    "advancements/tab_above_middle",
    "advancements/tab_above_right",
    "advancements/tab_above_left_selected",
    "advancements/tab_above_middle_selected",
    "advancements/tab_above_right_selected",
    "advancements/task_frame_unobtained",
    "advancements/task_frame_obtained",
    "advancements/goal_frame_unobtained",
    "advancements/goal_frame_obtained",
    "advancements/challenge_frame_unobtained",
    "advancements/challenge_frame_obtained",
    SPRITE_TITLE_BOX,
    SPRITE_BOX_OBTAINED,
    SPRITE_BOX_UNOBTAINED,
];

/// `advancements/{task,goal,challenge}_frame_{obtained,unobtained}` for `frame`.
///
/// A free function over the two inputs rather than a method so the `obtained`
/// half has a caller the day `UPDATE_ADVANCEMENTS` decodes — see the module doc.
#[must_use]
pub fn advancement_frame_sprite(frame: AdvancementFrame, obtained: bool) -> &'static str {
    frame.frame_sprite(obtained)
}

/// Vanilla's `advancements.progress` readout (`"%s/%s"`), or `None` when there is
/// only one requirement group — `getMaxProgressWidth` returns `0` there and no
/// text is drawn (`AdvancementWidget.java`).
#[must_use]
pub fn progress_text(done: u32, total: u32) -> Option<String> {
    (total > 1).then(|| format!("{done}/{total}"))
}

/// The five roots, in [`ADVANCEMENTS`] order — which is also tab order.
#[must_use]
pub fn advancement_tabs() -> Vec<&'static Advancement> {
    ADVANCEMENTS.iter().filter(|a| a.parent.is_none()).collect()
}

/// Persisted Advancements-screen UI state.
///
/// The scroll is **per tab**, matching vanilla: each `AdvancementTab` owns its
/// own `scrollX`/`scrollY` and centres itself once, on first draw.
#[derive(Debug, Clone, Default)]
pub struct AdvancementsState {
    /// Index into [`advancement_tabs`].
    pub tab: usize,
    /// Per-tab `(scrollX, scrollY)`, or `None` for a tab that has not been shown
    /// yet — [`Self::scroll_for`] centres it on first read, which is vanilla's
    /// `centered` latch.
    scroll: Vec<Option<(f32, f32)>>,
    /// `AdvancementTab::fade`, in `0.0..=FADE_CEILING`. Shared across tabs
    /// rather than per-tab: only one tab is ever hovered, and vanilla's own
    /// per-tab copy is unobservable because switching tabs also clears the
    /// hover.
    fade: f32,
}

impl AdvancementsState {
    /// The current tab's scroll, centring it if this is its first frame.
    ///
    /// `extractContents`' own initialiser: `scrollX = 117 - (maxX + minX) / 2`,
    /// `scrollY = 56 - (maxY + minY) / 2` (`AdvancementTab.java`), where
    /// the bounds come from every widget's `28 x 27` cell (`addWidget`, `:208-214`).
    pub fn scroll_for(&mut self, tree: &TreeLayout) -> (f32, f32) {
        if self.scroll.len() <= self.tab {
            self.scroll.resize(self.tab + 1, None);
        }
        *self.scroll[self.tab].get_or_insert_with(|| {
            let bounds = tree_bounds(tree);
            (
                117.0 - (bounds.x + bounds.x + bounds.w) / 2.0,
                56.0 - (bounds.y + bounds.y + bounds.h) / 2.0,
            )
        })
    }

    /// `AdvancementTab.scroll` (`:179-187`): pan, clamped so the tree cannot be
    /// dragged away from the viewport, and only on the axis that overflows it.
    pub fn pan(&mut self, tree: &TreeLayout, dx: f32, dy: f32) {
        let (mut sx, mut sy) = self.scroll_for(tree);
        let bounds = tree_bounds(tree);
        if bounds.w > INSIDE_W {
            sx = (sx + dx).clamp(-(bounds.x + bounds.w - INSIDE_W), 0.0);
        }
        if bounds.h > INSIDE_H {
            sy = (sy + dy).clamp(-(bounds.y + bounds.h - INSIDE_H), 0.0);
        }
        self.scroll[self.tab] = Some((sx, sy));
    }

    /// One wheel notch, at vanilla's `SCROLL_SPEED`.
    pub fn scroll_by(&mut self, tree: &TreeLayout, notches: f32) {
        self.pan(tree, 0.0, notches * SCROLL_SPEED);
    }

    /// Switch tabs, keeping each tab's own scroll — vanilla does the same, since
    /// the scroll lives on the `AdvancementTab` and not the screen.
    pub fn select_tab(&mut self, index: usize) {
        self.tab = index;
    }

    /// Advance the hover fade one frame, and return the alpha to dim with.
    ///
    /// `AdvancementTab.extractHovers` (`:97-104`). Deliberately per *frame* and
    /// not per tick, matching vanilla — the ramp is framerate-dependent there
    /// too, and at 60 fps it reaches the ceiling in five frames.
    pub fn tick_fade(&mut self, hovering: bool) -> f32 {
        self.fade = if hovering {
            (self.fade + FADE_RISE).clamp(0.0, FADE_CEILING)
        } else {
            (self.fade - FADE_FALL).clamp(0.0, 1.0)
        };
        self.fade
    }

    /// The current fade alpha without advancing it.
    #[must_use]
    pub fn fade(&self) -> f32 {
        self.fade
    }
}

/// The bounding box of every widget cell in `tree`, in tree-local pixels —
/// vanilla's `minX`/`maxX`/`minY`/`maxY`, each widget contributing a `28 x 27`
/// cell from its own top-left.
fn tree_bounds(tree: &TreeLayout) -> Rect {
    let mut min = (f32::INFINITY, f32::INFINITY);
    let mut max = (f32::NEG_INFINITY, f32::NEG_INFINITY);
    for node in &tree.nodes {
        let (x, y) = node_origin(node.x, node.y);
        min = (min.0.min(x), min.1.min(y));
        max = (max.0.max(x + NODE_PITCH_X), max.1.max(y + NODE_PITCH_Y));
    }
    if !min.0.is_finite() {
        return Rect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 };
    }
    Rect { x: min.0, y: min.1, w: max.0 - min.0, h: max.1 - min.1 }
}

/// `floor(x * 28)`, `floor(y * 27)`.
fn node_origin(x: f32, y: f32) -> (f32, f32) {
    ((x * NODE_PITCH_X).floor(), (y * NODE_PITCH_Y).floor())
}

/// Complete Advancements-screen geometry for one frame, in absolute logical
/// canvas pixels.
#[derive(Debug, Clone)]
pub struct AdvancementsLayout {
    /// The `252 x 140` window.
    pub window: Rect,
    /// The scissored `234 x 113` tree viewport.
    pub inside: Rect,
    /// The five tab buttons, in [`advancement_tabs`] order.
    pub tabs: Vec<Rect>,
    /// Every widget's `26 x 26` hit rect, paired with its index into the tab's
    /// [`TreeLayout::nodes`]. Only widgets at least partly inside
    /// [`inside`](Self::inside) are listed, so a click outside the viewport
    /// cannot select a panned-away advancement.
    pub widgets: Vec<(usize, Rect)>,
    /// The tab's own tree, positioned.
    pub tree: TreeLayout,
    /// This frame's scroll offset.
    pub scroll: (f32, f32),
}

/// Builds [`AdvancementsLayout`] against an explicit `gui_scale` (`0` = auto) —
/// the same triple the draw uses, one expression for both.
///
/// `progress` decides which hidden advancements are visible: vanilla's
/// `extractRenderState` gate is `!isHidden() || progress.isDone()`
/// (`AdvancementWidget.java`), so a hidden node appears the moment it is
/// obtained and its connector appears with it.
#[must_use]
pub fn advancements_layout(
    state: &mut AdvancementsState,
    progress: &AdvancementProgress,
    gui_scale: u32,
    width: u32,
    height: u32,
) -> Option<AdvancementsLayout> {
    let tabs_data = advancement_tabs();
    let root = tabs_data.get(state.tab.min(tabs_data.len().saturating_sub(1)))?;
    let tree = layout_tree(root.id)?;
    let scroll = state.scroll_for(&tree);

    let (cw, ch) = crate::menu::render::logical_canvas(gui_scale, width, height);
    let x = ((cw - WINDOW_W) * 0.5).max(8.0);
    let y = ((ch - WINDOW_H) * 0.5).max(TAB_H);
    let window = Rect { x, y, w: WINDOW_W, h: WINDOW_H };
    let inside = Rect { x: x + INSIDE_X, y: y + INSIDE_Y, w: INSIDE_W, h: INSIDE_H };

    let tabs = (0..tabs_data.len())
        .map(|i| Rect { x: x + TAB_PITCH * i as f32, y: y + TAB_DY, w: TAB_W, h: TAB_H })
        .collect();

    let mut widgets = Vec::new();
    for (i, node) in tree.nodes.iter().enumerate() {
        if !is_visible(node.advancement, progress) {
            continue;
        }
        let (nx, ny) = node_origin(node.x, node.y);
        let rect = Rect {
            x: inside.x + scroll.0 + nx,
            y: inside.y + scroll.1 + ny,
            w: HIT_SIZE,
            h: HIT_SIZE,
        };
        if overlaps(rect, inside) {
            widgets.push((i, rect));
        }
    }

    Some(AdvancementsLayout { window, inside, tabs, widgets, tree, scroll })
}

/// `!display.isHidden() || progress.isDone()`.
fn is_visible(advancement: &Advancement, progress: &AdvancementProgress) -> bool {
    !advancement.hidden || progress.obtained(advancement.id)
}

fn overlaps(a: Rect, b: Rect) -> bool {
    a.x + a.w > b.x && a.x < b.x + b.w && a.y + a.h > b.y && a.y < b.y + b.h
}

fn inside_rect(r: Rect, px: f32, py: f32) -> bool {
    px >= r.x && py >= r.y && px < r.x + r.w && py < r.y + r.h
}

/// What a viewport pixel is over, on the Advancements screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdvancementsHit {
    /// A tab button, by [`advancement_tabs`] index.
    Tab(usize),
    /// A widget, by [`TreeLayout::nodes`] index.
    Widget(usize),
    /// Inside the tree viewport but not on a widget — where a drag pans.
    Viewport,
    /// The window chrome.
    Window,
}

/// Resolves a **physical** viewport cursor position against `layout`, using the
/// same `gui_scale`/`width`/`height` triple the layout was built from.
#[must_use]
pub fn advancements_hit_test(
    layout: &AdvancementsLayout,
    gui_scale: u32,
    width: u32,
    height: u32,
    cursor_x: f32,
    cursor_y: f32,
) -> Option<AdvancementsHit> {
    let scale = crate::config::calculate_gui_scale(gui_scale, width, height).max(1) as f32;
    let (px, py) = (cursor_x / scale, cursor_y / scale);
    for (i, r) in layout.tabs.iter().enumerate() {
        if inside_rect(*r, px, py) {
            return Some(AdvancementsHit::Tab(i));
        }
    }
    if inside_rect(layout.inside, px, py) {
        // Reverse order so a widget drawn later (deeper in the tree) wins the
        // pixel, matching the draw's own z.
        for (i, r) in layout.widgets.iter().rev() {
            if inside_rect(*r, px, py) {
                return Some(AdvancementsHit::Widget(*i));
            }
        }
        return Some(AdvancementsHit::Viewport);
    }
    inside_rect(layout.window, px, py).then_some(AdvancementsHit::Window)
}

/// Everything the draw needs that this module cannot derive: resolved text and
/// live progress.
#[derive(Debug, Clone, Copy)]
pub struct AdvancementsView<'a> {
    /// The selected tab's title, already through the language table.
    pub title: &'a str,
    /// The hovered widget's index into [`TreeLayout::nodes`], if any — vanilla
    /// draws a title box for exactly one widget per frame.
    pub hovered: Option<usize>,
    /// The hovered widget's title, resolved. Empty draws no box.
    pub hovered_title: &'a str,
    /// The hovered widget's description, resolved. May be empty.
    pub hovered_description: &'a str,
    /// The local player's progress, from `SessionAdvancements`.
    pub progress: &'a AdvancementProgress,
    /// This frame's hover fade, from [`AdvancementsState::tick_fade`].
    pub fade: f32,
}

/// The pieces of one frame's tree draw that are pure geometry over
/// [`AdvancementsLayout`]: the tiled background, the connector segments and the
/// per-widget frames. Split out so the ordering rules live in one readable place
/// rather than being interleaved with vertex pushes.
struct DrawPlan {
    /// The 17 x 10 tile grid, as `(x, y)` origins — `extractContents`' own double
    /// loop over `-1..=15` x `-1..=8`, offset by `scroll % 16`.
    tiles: Vec<(f32, f32)>,
    /// The tab's tiled background id, e.g.
    /// `minecraft:gui/advancements/backgrounds/stone`.
    background: Option<&'static str>,
    /// Connector segments, `(rect, is_shadow)`, **shadows first**.
    lines: Vec<(Rect, bool)>,
    /// Per visible widget: the frame rect, its sprite id, its icon stack, and
    /// the icon's top-left.
    frames: Vec<(Rect, &'static str, ItemStack, (f32, f32))>,
}

/// Assembles [`DrawPlan`] for `layout`.
fn draw_plan(layout: &AdvancementsLayout, progress: &AdvancementProgress) -> DrawPlan {
    let (sx, sy) = layout.scroll;
    let origin = (layout.inside.x, layout.inside.y);

    // `left = intScrollX % 16`, `top = intScrollY % 16`, then `-1..=15` by
    // `-1..=8`. Rust's `%` is a remainder like Java's, so a negative scroll gives
    // a negative offset here exactly as it does there — which is the point: the
    // grid starts one tile early so the seam is always off-screen.
    let left = sx.floor() % TILE;
    let top = sy.floor() % TILE;
    let mut tiles = Vec::with_capacity(17 * 10);
    for tx in -1..=15 {
        for ty in -1..=8 {
            tiles.push((origin.0 + left + TILE * tx as f32, origin.1 + top + TILE * ty as f32));
        }
    }

    let mut lines = Vec::new();
    // Two whole-tree passes, shadow then foreground — `extractConnectivity(...,
    // true)` then `(..., false)`. Vanilla runs them as two traversals rather than
    // per node, so no node's foreground line is ever covered by a later node's
    // shadow.
    for shadow in [true, false] {
        for node in &layout.tree.nodes {
            let Some(parent) = node.parent else { continue };
            if !is_visible(node.advancement, progress) {
                continue;
            }
            let (px, py) = node_origin(layout.tree.nodes[parent].x, layout.tree.nodes[parent].y);
            let (nx, ny) = node_origin(node.x, node.y);
            let ox = origin.0 + sx;
            let oy = origin.1 + sy;
            let dep_x = ox + px + 13.0;
            let split_x = ox + px + 26.0 + 4.0;
            let dep_y = oy + py + 13.0;
            let my_x = ox + nx + 13.0;
            let my_y = oy + ny + 13.0;
            if shadow {
                for dy in [-1.0, 0.0, 1.0] {
                    // The middle of the three shadow rows starts one pixel right,
                    // which is `horizontalLine(splitX + 1, ...)` — vanilla's own
                    // asymmetry, and the reason this is a loop with a conditional
                    // rather than three identical calls.
                    let x0 = split_x + if dy == 0.0 { 1.0 } else { 0.0 };
                    push_h(&mut lines, x0, dep_x, dep_y + dy, true);
                    push_h(&mut lines, my_x, split_x - 1.0, my_y + dy, true);
                }
                push_v(&mut lines, split_x - 1.0, my_y, dep_y, true);
                push_v(&mut lines, split_x + 1.0, my_y, dep_y, true);
            } else {
                push_h(&mut lines, split_x, dep_x, dep_y, false);
                push_h(&mut lines, my_x, split_x, my_y, false);
                push_v(&mut lines, split_x, my_y, dep_y, false);
            }
        }
    }
    // Clamp every segment into the viewport, since there is no scissor — see the
    // module doc. A segment entirely outside is dropped.
    lines.retain_mut(|(r, _)| clamp_to(r, layout.inside));

    let mut frames = Vec::new();
    for (i, rect) in &layout.widgets {
        let node = &layout.tree.nodes[*i];
        let Ok(id) = node.advancement.icon.parse::<lodestone_model::Identifier>() else {
            continue;
        };
        frames.push((
            Rect { x: rect.x + FRAME_DX, y: rect.y, w: FRAME_SIZE, h: FRAME_SIZE },
            advancement_frame_sprite(
                node.advancement.frame,
                progress.obtained(node.advancement.id),
            ),
            ItemStack::new(id, 1),
            (rect.x + ICON_DX, rect.y + ICON_DY),
        ));
    }

    DrawPlan {
        tiles,
        background: layout.tree.nodes.first().and_then(|root| root.advancement.background),
        lines,
        frames,
    }
}

fn push_h(out: &mut Vec<(Rect, bool)>, x0: f32, x1: f32, y: f32, shadow: bool) {
    let (lo, hi) = if x0 <= x1 { (x0, x1) } else { (x1, x0) };
    out.push((Rect { x: lo, y, w: (hi - lo).max(1.0), h: 1.0 }, shadow));
}

fn push_v(out: &mut Vec<(Rect, bool)>, x: f32, y0: f32, y1: f32, shadow: bool) {
    let (lo, hi) = if y0 <= y1 { (y0, y1) } else { (y1, y0) };
    out.push((Rect { x, y: lo, w: 1.0, h: (hi - lo).max(1.0) }, shadow));
}

/// Intersect `r` with `clip` in place; `false` if nothing survives.
fn clamp_to(r: &mut Rect, clip: Rect) -> bool {
    let x0 = r.x.max(clip.x);
    let y0 = r.y.max(clip.y);
    let x1 = (r.x + r.w).min(clip.x + clip.w);
    let y1 = (r.y + r.h).min(clip.y + clip.h);
    if x1 <= x0 || y1 <= y0 {
        return false;
    }
    *r = Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 };
    true
}

/// Word-wrap `text` to `max_px` against `measure`. Never returns an empty vector.
///
/// A plain greedy wrap, which is what vanilla's `StringSplitter::splitLines`
/// reduces to for the unstyled, single-`Style` strings this screen hands it.
fn wrap(measure: &dyn Fn(&str) -> f32, text: &str, max_px: f32) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let candidate = if current.is_empty() {
            word.to_string()
        } else {
            format!("{current} {word}")
        };
        if measure(&candidate) <= max_px || current.is_empty() {
            current = candidate;
        } else {
            lines.push(std::mem::take(&mut current));
            current = word.to_string();
        }
    }
    lines.push(current);
    lines
}

/// `findOptimalLines` (`AdvancementWidget.java`): wrap at five candidate
/// widths and keep the one whose longest line lands closest to `preferred`,
/// returning early on anything within 10 px.
///
/// This is what stops a two-word second line — a greedy wrap at one width
/// produces "…and then\nthis", and the offsets let it find a squarer block.
fn find_optimal_lines(
    measure: &dyn Fn(&str) -> f32,
    text: &str,
    preferred: f32,
) -> Vec<String> {
    let mut best: Option<(f32, Vec<String>)> = None;
    for margin in TEST_SPLIT_OFFSETS {
        let split = wrap(measure, text, preferred - margin);
        let longest = split.iter().map(|l| measure(l)).fold(0.0f32, f32::max);
        let distance = (longest - preferred).abs();
        if distance <= 10.0 {
            return split;
        }
        if best.as_ref().is_none_or(|(d, _)| distance < *d) {
            best = Some((distance, split));
        }
    }
    best.map(|(_, split)| split).unwrap_or_default()
}

/// The hover tooltip's wrapped text and its overall box width — vanilla's
/// `AdvancementWidget` constructor (`:55-79`), which computes all of this once
/// per widget. We compute it for the one hovered widget per frame instead, which
/// is the same work spread differently and needs no per-widget cache.
#[derive(Debug, Clone, Default)]
struct HoverText {
    title: Vec<String>,
    description: Vec<String>,
    /// `getProgressText()`, `None` for a single-group advancement.
    progress: Option<String>,
    /// The box width: `longestDescLine + TITLE_PADDING_LEFT + TITLE_PADDING_RIGHT`.
    width: f32,
}

fn hover_text(
    measure: &dyn Fn(&str) -> f32,
    node: &NodeProgress,
    title: &str,
    description: &str,
) -> HoverText {
    let title_lines = wrap(measure, title, TITLE_MAX_WIDTH);
    let title_width = title_lines
        .iter()
        .map(|l| measure(l))
        .fold(0.0f32, f32::max)
        .max(TITLE_MIN_WIDTH);
    // `getMaxProgressWidth`: the width of the *widest possible* readout
    // (`total/total`), plus 8 px of spacing — not the current one, so the box
    // does not resize as criteria tick over.
    let max_progress_width = if node.total <= 1 {
        0.0
    } else {
        measure(&format!("{}/{}", node.total, node.total)) + 8.0
    };
    // `longestDescLine = 29 + titleWidth + maxProgressWidth`, then grown by any
    // description line that overflows it.
    let preferred = 29.0 + title_width + max_progress_width;
    let description_lines = find_optimal_lines(measure, description, preferred);
    let longest = description_lines
        .iter()
        .map(|l| measure(l))
        .fold(preferred, f32::max);
    HoverText {
        title: title_lines,
        description: description_lines,
        progress: progress_text(node.done, node.total),
        width: longest + TITLE_PAD_LEFT + TITLE_PAD_RIGHT,
    }
}

/// One frame of hover-tooltip geometry, in absolute canvas pixels.
struct HoverPlan {
    /// The `advancements/title_box` panel behind the description, absent when
    /// there is no description.
    panel: Option<Rect>,
    /// The title bar, as one or two `(rect, obtained)` pieces — the split is
    /// vanilla's progress bar, and a partially-complete advancement really does
    /// draw two different sprites butted together.
    bars: Vec<(Rect, bool)>,
    /// The icon frame, redrawn over the bar.
    frame: (Rect, &'static str),
    /// The icon's top-left.
    icon_at: (f32, f32),
    /// The title block's top-left.
    title_at: (f32, f32),
    /// The progress readout's top-left, already right-aligned.
    progress_at: Option<(f32, f32)>,
    /// The description block's top-left.
    description_at: (f32, f32),
}

/// `extractHover` (`AdvancementWidget.java`), including its two
/// flips: `leftSide` when the box would run off the screen's right edge, and
/// `topSide` when the description would run past the viewport's bottom.
fn hover_plan(
    layout: &AdvancementsLayout,
    hovered: usize,
    text: &HoverText,
    frame: AdvancementFrame,
    node: &NodeProgress,
    measure: &dyn Fn(&str) -> f32,
    canvas_w: f32,
) -> Option<HoverPlan> {
    let (_, rect) = layout.widgets.iter().find(|(i, _)| *i == hovered)?;
    let width = text.width;

    let title_bar_h = LINE_H * text.title.len() as f32 + TITLE_PAD_TOP + TITLE_PAD_BOTTOM;
    let title_top = rect.y + ((FRAME_SIZE - title_bar_h) / 2.0).floor();
    let title_bar_bottom = title_top + title_bar_h;
    let description_text_h = LINE_H * text.description.len() as f32;
    // `6 + descriptionTextHeight` — the panel's own vertical padding.
    let description_h = 6.0 + description_text_h;
    let left_side = rect.x + width + FRAME_SIZE >= canvas_w;
    let top_side = title_bar_bottom + description_h >= layout.inside.y + INSIDE_H;

    // The four-way split at `:196-220`. `firstHalfWidth < 2` and `> width - 2`
    // both collapse to a single full-width bar, so a barely-started or
    // nearly-finished advancement does not draw a 1 px sliver.
    let amount = node.percent();
    let mut first_w = (amount * width).floor();
    let (first_obtained, second_obtained, frame_obtained) = if amount >= 1.0 {
        first_w = width / 2.0;
        (true, true, true)
    } else if first_w < 2.0 {
        first_w = width / 2.0;
        (false, false, false)
    } else if first_w > width - 2.0 {
        first_w = width / 2.0;
        (true, true, false)
    } else {
        (true, false, false)
    };

    let title_left = if left_side {
        rect.x - width + FRAME_SIZE + 6.0
    } else {
        rect.x
    };
    let panel_top = if top_side {
        title_bar_bottom - (title_bar_h + description_h)
    } else {
        title_top
    };

    let mut bars = Vec::new();
    if first_obtained == second_obtained {
        bars.push((
            Rect { x: title_left, y: title_top, w: width, h: title_bar_h },
            first_obtained,
        ));
    } else {
        bars.push((
            Rect { x: title_left, y: title_top, w: first_w, h: title_bar_h },
            first_obtained,
        ));
        bars.push((
            Rect {
                x: title_left + first_w,
                y: title_top,
                w: width - first_w,
                h: title_bar_h,
            },
            second_obtained,
        ));
    }

    let description_left = title_left + TITLE_PAD_RIGHT;
    let title_at = if left_side {
        (description_left, title_top + TITLE_PAD_TOP)
    } else {
        (rect.x + TITLE_TEXT_X, title_top + TITLE_PAD_TOP)
    };
    // Right-aligned, so the *current* readout's own width is what matters — not
    // the widest-possible one the box was sized from.
    let progress_at = text.progress.as_ref().map(|p| {
        let pw = measure(p);
        if left_side {
            (rect.x - pw, title_top + TITLE_PAD_TOP)
        } else {
            (rect.x + width - pw - TITLE_PAD_RIGHT, title_top + TITLE_PAD_TOP)
        }
    });
    let description_at = if top_side {
        (description_left, title_top - description_text_h + 1.0)
    } else {
        (description_left, title_bar_bottom)
    };

    Some(HoverPlan {
        panel: (!text.description.iter().all(String::is_empty)).then_some(Rect {
            x: title_left,
            y: panel_top,
            w: width,
            h: title_bar_h + description_h,
        }),
        bars,
        frame: (
            Rect { x: rect.x + FRAME_DX, y: rect.y, w: FRAME_SIZE, h: FRAME_SIZE },
            advancement_frame_sprite(frame, frame_obtained),
        ),
        icon_at: (rect.x + ICON_DX, rect.y + ICON_DY),
        title_at,
        progress_at,
        description_at,
    })
}

/// Builds one frame of Advancements-screen geometry.
///
/// Returns a [`ContainerGeometry`] for
/// [`ContainerRenderer::render_geometry_scaled`](crate::container::ContainerRenderer)
/// — see the module doc for why this screen goes through the container path.
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn advancements_geometry(
    layout: &AdvancementsLayout,
    view: AdvancementsView<'_>,
    gui_scale: u32,
    width: u32,
    height: u32,
    items: Option<&ItemAtlas>,
    models: Option<&BlockModels>,
    font: Option<&crate::hud::VanillaFont>,
    background: Option<&ContainerBackground>,
) -> ContainerGeometry {
    let assets = IconAssets { items, models };
    let (w, h) = crate::menu::render::logical_canvas(gui_scale, width, height);
    let mut b = Builder::new(w, h, font);
    let plan = draw_plan(layout, view.progress);
    // One measure closure for every text decision below, so the wrap, the box
    // width and the right-alignment all agree. With no font attached every string
    // measures zero, which collapses the box to its `TITLE_MIN_WIDTH` floor
    // rather than to nothing.
    let measure = |s: &str| font.map_or(0.0, |f| f.width(s, 1.0));
    let hover = view.hovered.and_then(|i| {
        let node = layout.tree.nodes.get(i)?;
        let progress = view.progress.get(node.advancement.id);
        let text = hover_text(&measure, &progress, view.hovered_title, view.hovered_description);
        let plan = hover_plan(
            layout,
            i,
            &text,
            node.advancement.frame,
            &progress,
            &measure,
            w,
        )?;
        Some((text, plan))
    });

    // The same full-canvas dim every in-game screen draws, in its own leading
    // pass — `ContainerGeometry::dim_vertex_count`.
    b.gradient_rect_px(
        0.0,
        0.0,
        w,
        h,
        [16.0 / 255.0, 16.0 / 255.0, 16.0 / 255.0, 192.0 / 255.0],
        [16.0 / 255.0, 16.0 / 255.0, 16.0 / 255.0, 208.0 / 255.0],
    );
    let dim_floats = b.verts.len();

    // Unselected tabs first — vanilla's own order (`AdvancementsScreen.java:
    // 206-215`), so an unselected tab is partly covered by the window edge and
    // the selected one is not.
    for (i, rect) in layout.tabs.iter().enumerate() {
        if i == layout_tab(layout) {
            continue;
        }
        push_sprite(&mut b, background, tab_sprite(i, false), *rect);
    }
    // The tiled per-tab background, drawn **before** the window art, or the
    // window's own baked-in inner shadow and border would be erased.
    //
    // `AdvancementTab.extractContents` (stratum 1, `AdvancementsScreen.
    // extractInside`) draws the tile grid; `AdvancementsScreen.extractWindow`
    // (stratum 2, drawn *after* via `graphics.nextStratum()`) draws
    // `window.png` on top of it. That ordering is load-bearing, not
    // cosmetic: `window.png` is not an opaque frame with a transparent hole —
    // measured on the real 26.2 asset, its pixels from `x = WINDOW_INSIDE_X`
    // (9) inward carry a **translucent black gradient**, alpha 171 at the
    // very edge fading to 0 by roughly `x = 16` (`(0,0,0,171)` at column 9,
    // `(0,0,0,0)` by column ~16, sampled at `y = 25`) — vanilla's inner
    // shadow, baked into the texture rather than drawn as a separate quad,
    // and it only *reads* as a shadow if something opaque is already there
    // for it to composite over. Drawing the window **before** the tiles (the
    // previous order here) let the opaque tile grid painted last cover that
    // gradient completely — the border still showed (it is fully opaque,
    // outside the tile clip rect either way), but the shadow at the seam
    // never did. Vanilla scissors this to the viewport; we clamp instead
    // (module doc), which for a 16x16 tile grid means dropping the ring that
    // falls wholly outside and trimming the rest.
    if let (Some(bg), Some(id)) = (background, plan.background) {
        for (tx, ty) in &plan.tiles {
            let mut dst = Rect { x: *tx, y: *ty, w: TILE, h: TILE };
            let full = dst;
            if !clamp_to(&mut dst, layout.inside) {
                continue;
            }
            if let Some(q) = bg.advancements_tile_quad(id, full, dst) {
                b.bg_sprite(q);
            }
        }
    } else {
        b.rect_px(
            layout.inside.x,
            layout.inside.y,
            layout.inside.w,
            layout.inside.h,
            [0.10, 0.10, 0.12, 1.0],
        );
    }
    match background.and_then(|bg| bg.advancements_window_quad(layout.window.x, layout.window.y)) {
        Some(q) => b.bg_sprite(q),
        // The jar-less picture: a flat panel plus a darker viewport well, so the
        // tree still reads against something. No separate shadow quad here
        // either — see the comment above: vanilla's is baked into `window.
        // png`, which this fallback (by construction) does not have.
        None => {
            b.rect_px(
                layout.window.x,
                layout.window.y,
                layout.window.w,
                layout.window.h,
                [0.08, 0.075, 0.065, 0.94],
            );
        }
    }
    push_sprite(
        &mut b,
        background,
        tab_sprite(layout_tab(layout), true),
        layout.tabs[layout_tab(layout)],
    );
    // `bg_slot_vertex_count`'s split, **before** both the hover tooltip's own
    // sprites and every widget's own frame. Everything pushed to `bg_verts`
    // up to this marker — the tile grid, the window art, both tab sprites —
    // draws in the renderer's early "back" bg pass; everything pushed after
    // it waits for the "front" bg pass, which the renderer runs once the
    // "chrome" colour pass (the connector lines, next) and every icon have
    // already drawn.
    let bg_slot_floats = b.bg_verts.len();

    // The connector lines, shadows already ordered before foregrounds by
    // `draw_plan`. On the colour stream, in the "chrome" pass the renderer
    // runs right after the "back" bg pass above and before every icon pass
    // below — see the widget-frame loop's own comment for why that keeps a
    // line behind every frame it crosses.
    let (shadow_ink, line_ink) = (LINE_BG, LINE_FG);
    for (rect, shadow) in &plan.lines {
        b.rect_px(
            rect.x,
            rect.y,
            rect.w,
            rect.h,
            if *shadow { shadow_ink } else { line_ink },
        );
    }
    if !view.title.is_empty() {
        b.label(
            view.title,
            layout.window.x + TITLE_X,
            layout.window.y + TITLE_Y,
            1.0,
            TITLE_COLOUR,
        );
    }
    // `chrome_vertex_count`'s split: connector lines and the title above it.
    // Everything below — the widget frames included — draws in a later pass,
    // or a later range of this same colour stream.
    let chrome_floats = b.verts.len();

    // ---- the chrome/icon split ----

    let tab_roots = advancement_tabs();
    for (i, rect) in layout.tabs.iter().enumerate() {
        let Some(root) = tab_roots.get(i) else { continue };
        let Ok(id) = root.icon.parse::<lodestone_model::Identifier>() else {
            continue;
        };
        b.draw_stack(
            &assets,
            &ItemStack::new(id, 1),
            rect.x + TAB_ICON_DX,
            rect.y + TAB_ICON_DY,
        );
    }
    // `slot_item_vertex_count`/`slot_model_vertex_count`/`slot_glint_vertex_
    // count`/`slot_special_count`/`slot_vertex_count`'s split — five markers
    // taken together, marking "end of tab-icon content, start of carried" on
    // each of `b`'s own four streams. Nothing below pushes to `b` directly
    // any more except the drained 3-D/special leftovers (see the comment on
    // `mid.model_verts`/`mid.special` below) and the tooltip's own content
    // further down — every widget's own frame, flat-sprite icon and glint now
    // go to the **separate** `mid` builder instead, so this split still
    // correctly means "tab icons before, carried tooltip content after" for
    // `b` itself.
    let slot_item_floats = b.item_verts.len();
    let slot_model_verts = b.model_verts.len();
    let slot_glint_floats = b.glint_verts.len();
    let slot_special = b.special.len();
    let slot_floats = b.verts.len();

    // ---- the "mid" tier: frame, then icon, then the hover-dim --------------
    //
    // A **separate** builder, not `b` — see `ContainerGeometry::mid_bg_verts`'s
    // doc for why this needs its own renderer pass rather than a range split
    // of an existing stream. Vanilla draws a connector line *behind* every
    // widget (`AdvancementTab.extractContents` runs both
    // `root.extractConnectivity` passes over the whole tree before
    // `root.extractRenderState`, the frame-then-icon draw); the frame loop
    // below runs after both `bg_slot_floats` and `chrome_floats` above, so a
    // real sprite frame (the `Some` arm, on `mid.bg_verts`) still lands in a
    // pass the renderer runs after the "chrome" colour pass the lines are in,
    // and a jar-less fallback frame (the `None` arm, on `mid.verts`) still
    // lands after every line in submission order. Either way: lines first,
    // frame after — unchanged from before this fix.
    //
    // The measured `task_frame_obtained`/`task_frame_unobtained` sprites are
    // fully opaque under the icon's own footprint (alpha 255 across the whole
    // 16x16 centre — re-verified, not assumed), so the icon loop runs
    // **after** the frame loop into the same builder: a real sprite icon
    // (`mid.item_verts`/`mid.glint_verts`) and a jar-less fallback icon
    // (`mid.verts`) both still draw over the frame, never under it.
    //
    // The hover-dim goes down **last**, into its own wholly separate
    // `dim2_verts` stream rather than `mid.verts` — a plain colour rect
    // sharing `mid`'s content would still be one pass, and one pass has no
    // room for "under the icon, over nothing else": `ContainerRenderer` draws
    // `dim2_verts` in its own pass, positioned after this whole `mid` block
    // and before the tooltip's own content (`bg_verts`'s remaining "front"
    // range, and everything still pushed to `b` below), which is what lets
    // it darken a widget's own frame and icon without also darkening the
    // hover tooltip.
    let mut mid = Builder::new(w, h, font);
    for (rect, sprite, _, _) in &plan.frames {
        push_sprite_clipped(&mut mid, background, sprite, *rect, layout.inside);
    }
    // Every widget's own icon, clipped to the viewport. See
    // [`draw_stack_clipped`]'s doc for the clip itself.
    for (_, _, stack, at) in &plan.frames {
        draw_stack_clipped(&mut mid, &assets, stack, at.0, at.1, layout.inside, (w, h));
    }
    // `mid.model_verts`/`mid.special` (a widget icon backed by a 3-D block
    // model or a special-renderer chest icon) have nowhere to draw in the
    // "mid" tier — `IconStratum` has only `Slots`/`Carried` and lives in
    // `crate::hud::item_icon`, outside this fix's file ownership — so they
    // are drained back into `b`'s own carried tier instead of being dropped.
    // That keeps every icon visible (nothing is lost), at the cost that a
    // 3-D or special-renderer widget icon stays undimmed by the hover-dim, a
    // documented, narrower gap than the one this fix closes. `slot_model_
    // verts`/`slot_special` above were captured before this point, so the
    // drained content still lands correctly in `b`'s carried range.
    b.model_verts.extend(mid.model_verts.drain(..));
    b.special.extend(mid.special.drain(..));
    let dim2_verts = {
        let mut dim = Builder::new(w, h, font);
        if view.fade > 0.0 {
            dim.rect_px(
                layout.inside.x,
                layout.inside.y,
                layout.inside.w,
                layout.inside.h,
                [HOVER_DIM_RGB[0], HOVER_DIM_RGB[1], HOVER_DIM_RGB[2], view.fade],
            );
        }
        dim.verts
    };

    // ---- the tooltip's own tier --------------------------------------------
    //
    // Everything from here down is the tooltip, and everything above (the
    // tile grid, the connector lines, both tab icons, and now the whole `mid`
    // block: every widget's own frame and icon plus the hover-dim) is not.
    // [`push_sprite_clipped`]'s viewport clip already keeps tree content from
    // ever overlapping the window/tab chrome, so this only has to be *later*
    // than the `mid` block and the renderer's `dim2_verts` pass, which it is
    // by construction: `b`'s own `bg_verts` push below still lands in
    // `ContainerRenderer`'s existing (now `mid`-tier-then-)front-bg pass —
    // unmoved, still after `bg_slot_floats` — and `b`'s own carried-tier
    // pushes (text, icon redraw) still land after the five markers captured
    // above `mid` — unmoved too. Both of those passes run strictly after the
    // three new ones the `mid` block feeds, so nothing here needs its own new
    // marker.

    // The hover tooltip's own sprites: description panel, then the title bar
    // over it, then the icon frame redrawn on top — `extractHover`'s order,
    // and the reason the frame is drawn twice per hovered widget. Pushed
    // *after* `bg_slot_floats`, so the renderer draws these in the existing
    // front-bg pass, which the fix positioned after the new `mid`-tier frame
    // pass and the new `dim2_verts` pass — both of which have already run by
    // the time this executes, so this panel/bars/frame-redraw draws over the
    // (now correctly dimmed) tree content underneath it and stays undimmed
    // itself. It only ever overlaps its own hovered widget's own frame+icon
    // (both in the `mid` tier, drawn earlier) and its own redrawn icon
    // (pushed in the carried tier below, after this), never a *different*
    // widget's icon.
    if let Some((_, hover)) = &hover {
        if let Some(panel) = hover.panel {
            push_sprite(&mut b, background, SPRITE_TITLE_BOX, panel);
        }
        for (rect, obtained) in &hover.bars {
            let sprite = if *obtained {
                SPRITE_BOX_OBTAINED
            } else {
                SPRITE_BOX_UNOBTAINED
            };
            push_sprite(&mut b, background, sprite, *rect);
        }
        push_sprite(&mut b, background, hover.frame.1, hover.frame.0);
    }

    // The tooltip text, and the hovered widget's icon redrawn over its own
    // frame — both pushed after every "slot" marker above, so both land in
    // the carried pass and draw after the panel.
    if let Some((text, hover)) = &hover {
        for (i, line) in text.title.iter().enumerate() {
            b.label(
                line,
                hover.title_at.0,
                hover.title_at.1 + LINE_H * i as f32,
                1.0,
                HOVER_TEXT_COLOUR,
            );
        }
        if let (Some(readout), Some(at)) = (&text.progress, hover.progress_at) {
            b.label(readout, at.0, at.1, 1.0, HOVER_TEXT_COLOUR);
        }
        for (i, line) in text.description.iter().enumerate() {
            b.label(
                line,
                hover.description_at.0,
                hover.description_at.1 + LINE_H * i as f32,
                1.0,
                DESCRIPTION_COLOUR,
            );
        }
        let icon = layout
            .tree
            .nodes
            .get(view.hovered.unwrap_or(0))
            .and_then(|n| n.advancement.icon.parse::<lodestone_model::Identifier>().ok());
        if let Some(id) = icon {
            b.draw_stack(
                &assets,
                &ItemStack::new(id, 1),
                hover.icon_at.0,
                hover.icon_at.1,
            );
        }
    }

    ContainerGeometry {
        bg_slot_vertex_count: bg_slot_floats / crate::hud::SPRITE_FLOATS_PER_VERTEX,
        dim_vertex_count: dim_floats / COLOUR_FLOATS_PER_VERTEX,
        chrome_vertex_count: chrome_floats / COLOUR_FLOATS_PER_VERTEX,
        slot_vertex_count: slot_floats / COLOUR_FLOATS_PER_VERTEX,
        slot_item_vertex_count: slot_item_floats / crate::hud::SPRITE_FLOATS_PER_VERTEX,
        slot_glint_vertex_count: slot_glint_floats / crate::hud::SPRITE_FLOATS_PER_VERTEX,
        slot_model_vertex_count: slot_model_verts,
        slot_special_count: slot_special,
        verts: b.verts,
        item_verts: b.item_verts,
        glint_verts: b.glint_verts,
        model_verts: b.model_verts,
        special: b.special,
        bg_verts: b.bg_verts,
        // The "mid" tier: every widget's own frame (real sprite or jar-less
        // fallback) and flat-sprite icon, plus the hover-dim itself — see the
        // `mid` builder's own doc above and `ContainerGeometry::mid_bg_verts`.
        mid_bg_verts: mid.bg_verts,
        mid_verts: mid.verts,
        mid_item_verts: mid.item_verts,
        mid_glint_verts: mid.glint_verts,
        dim2_verts,
        widget_rect: Some(layout.window),
        // No inventory avatar — `AdvancementsScreen` is not `InventoryScreen`.
        // See `ContainerGeometry::player_avatar`.
        player_avatar: None,
    }
}

/// `[x, y, r, g, b, a]` — `container`'s own colour-stream stride, restated here
/// because it is module-private there and this screen fills the same struct.
const COLOUR_FLOATS_PER_VERTEX: usize = 6;

/// The selected tab, clamped into range the same way [`advancements_layout`]
/// clamps it.
fn layout_tab(layout: &AdvancementsLayout) -> usize {
    let count = layout.tabs.len();
    layout
        .tree
        .nodes
        .first()
        .and_then(|root| advancement_tabs().iter().position(|t| t.id == root.advancement.id))
        .unwrap_or(0)
        .min(count.saturating_sub(1))
}

fn push_sprite(
    b: &mut Builder<'_>,
    background: Option<&ContainerBackground>,
    id: &str,
    rect: Rect,
) {
    match background.and_then(|bg| bg.sprite_quad_for(id, rect.x, rect.y, rect.w, rect.h)) {
        Some(q) => b.bg_sprite(q),
        None => b.rect_px(rect.x, rect.y, rect.w, rect.h, [0.24, 0.21, 0.17, 1.0]),
    }
}

/// [`push_sprite`], clamped to `clip` — without this, entries popped fully in
/// and out at the viewport edge, because [`AdvancementsLayout
/// ::widgets`]'s *inclusion* test (`overlaps`, in [`advancements_layout`]) is
/// the only gate a widget's frame art passed through, and that test is
/// deliberately permissive (any overlap at all counts, so a click at the very
/// edge still lands). Once included, the frame's **full** `26 x 26` sprite
/// drew unclamped — so a widget at the boundary either was not drawn at all,
/// or was drawn whole and spilling past the window's own art. Vanilla
/// scissors the `234 x 113` viewport around exactly this draw
/// (`AdvancementWidget.draw`, called from inside `AdvancementTab.
/// drawWidgets`'s `enableScissor`/`disableScissor` bracket); this ports that
/// by shrinking the sprite's own destination rect **and** its sampled UV rect
/// in lock-step, so the visible sliver still samples the right part of the
/// art rather than being squished to fit — see [`clip_sprite_quad`].
fn push_sprite_clipped(
    b: &mut Builder<'_>,
    background: Option<&ContainerBackground>,
    id: &str,
    rect: Rect,
    clip: Rect,
) {
    match background.and_then(|bg| bg.sprite_quad_for(id, rect.x, rect.y, rect.w, rect.h)) {
        Some(q) => {
            if let Some(clipped) = clip_sprite_quad(q, clip) {
                b.bg_sprite(clipped);
            }
        }
        None => {
            let mut dst = rect;
            if clamp_to(&mut dst, clip) {
                b.rect_px(dst.x, dst.y, dst.w, dst.h, [0.24, 0.21, 0.17, 1.0]);
            }
        }
    }
}

/// Shrinks a [`GuiSpriteQuad`] to its intersection with `clip`, adjusting the
/// sampled UV rect **proportionally** so the visible sub-rect still samples
/// the correct part of the sprite. `None` when nothing survives, mirroring
/// [`clamp_to`]'s `bool` for a plain [`Rect`].
///
/// Valid because a `GuiSpriteQuad` (unlike a nine-slice sprite) maps its
/// whole `dst` rect to `[uv_min, uv_max]` **uniformly** — the same
/// fraction-of-declared-size principle a nine-slice sprite's own border-quad
/// fix uses, applied here to a plain quad instead: the fraction of `dst`
/// kept on each axis is the same fraction of the UV span kept.
#[must_use]
fn clip_sprite_quad(q: GuiSpriteQuad, clip: Rect) -> Option<GuiSpriteQuad> {
    let [x, y, w, h] = q.dst;
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let x0 = x.max(clip.x);
    let y0 = y.max(clip.y);
    let x1 = (x + w).min(clip.x + clip.w);
    let y1 = (y + h).min(clip.y + clip.h);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let (fx0, fx1) = ((x0 - x) / w, (x1 - x) / w);
    let (fy0, fy1) = ((y0 - y) / h, (y1 - y) / h);
    let u_span = q.uv_max[0] - q.uv_min[0];
    let v_span = q.uv_max[1] - q.uv_min[1];
    Some(GuiSpriteQuad {
        dst: [x0, y0, x1 - x0, y1 - y0],
        uv_min: [q.uv_min[0] + fx0 * u_span, q.uv_min[1] + fy0 * v_span],
        uv_max: [q.uv_min[0] + fx1 * u_span, q.uv_min[1] + fy1 * v_span],
    })
}

/// The GUI-pixel square [`Builder::draw_stack`] draws a slot icon into —
/// `container`'s own private `CELL` restated here, the same way this module
/// already restates [`COLOUR_FLOATS_PER_VERTEX`] because the original is
/// module-private where it lives.
const ICON_SIZE: f32 = 16.0;

/// Whether `inner` lies wholly inside `outer` — the coarse containment test
/// [`draw_stack_clipped`] uses for the two icon streams it cannot sub-clip.
fn rect_wholly_inside(outer: Rect, inner: Rect) -> bool {
    inner.x >= outer.x
        && inner.y >= outer.y
        && inner.x + inner.w <= outer.x + outer.w
        && inner.y + inner.h <= outer.y + outer.h
}

/// [`Builder::draw_stack`], clipped to `clip` — a sub-rect clip primitive in
/// the same shape as [`clip_sprite_quad`] rather than a second convention:
/// `draw_stack` has no clip seam of its own (it
/// composites up to four streams — a flat item sprite plus its glint copy,
/// a 3-D block-item mesh, a special-renderer block-entity icon, and
/// colour-stream chrome), so this snapshots every stream's length, lets
/// `draw_stack` run, and clips exactly the vertices it just appended.
///
/// The flat sprite and glint streams, and the colour-stream chrome, are
/// `GuiSpriteQuad`-shaped (an axis-aligned destination rect, sampled
/// uniformly) exactly like a frame sprite, so [`clip_quads_from`] reuses
/// [`clip_sprite_quad`] itself for the first two and a plain [`clamp_to`]
/// for the third (no UV to preserve there).
///
/// **The block-item mesh is a third shape, and it is not axis-aligned — but
/// it is not un-clippable either.** [`push_item_model`](crate::hud::item_icon)
/// (via `lodestone_render::gui_item_pose`) already poses every
/// [`ModelVertex`] into **GUI pixel space** on the CPU, before this ever
/// reaches the GPU — `gui_ortho` (model-pixel-space -> NDC) is a
/// shader-side `view_proj` uniform, never applied here — so
/// `position[0]`/`position[1]` are already in the exact pixel coordinates
/// `clip` is expressed in, and a straddling triangle can be *cut* the same
/// way a straddling sprite quad is, via [`clip_model_triangles`]. Only the
/// **special-renderer** stream (a block-entity icon such as a chest) has no
/// CPU vertex list at all — its mesh is built from a placement matrix inside
/// the GPU-side icon pass, outside this module's ownership — so that one
/// alone is still dropped whole when its `ICON_SIZE` bounding square is not
/// wholly inside `clip`: strictly *fewer* pixels than vanilla ever draws,
/// never a spill past the edge, which is the containment property this
/// exists to guarantee.
fn draw_stack_clipped(
    b: &mut Builder<'_>,
    assets: &IconAssets<'_>,
    stack: &ItemStack,
    x: f32,
    y: f32,
    clip: Rect,
    canvas: (f32, f32),
) {
    let before = (
        b.verts.len(),
        b.item_verts.len(),
        b.glint_verts.len(),
        b.model_verts.len(),
        b.special.len(),
    );
    b.draw_stack(assets, stack, x, y);

    clip_model_triangles(&mut b.model_verts, before.3, clip);

    if b.special.len() > before.4
        && !rect_wholly_inside(clip, Rect { x, y, w: ICON_SIZE, h: ICON_SIZE })
    {
        b.special.truncate(before.4);
    }

    clip_quads_from(&mut b.item_verts, before.1, crate::hud::SPRITE_FLOATS_PER_VERTEX, true, canvas, clip);
    clip_quads_from(&mut b.glint_verts, before.2, crate::hud::SPRITE_FLOATS_PER_VERTEX, true, canvas, clip);
    clip_quads_from(&mut b.verts, before.0, COLOUR_FLOATS_PER_VERTEX, false, canvas, clip);
}

/// Clips every already-emitted block-model triangle on `verts[from..]` to
/// `clip`, in place, by polygon-clipping each triangle against the clip
/// rect's four edges ([`clip_triangle`], Sutherland-Hodgman) and
/// fan-triangulating whatever convex polygon survives. `verts[from..]` is a
/// flat triangle list — [`push_item_model`](crate::hud::item_icon) already
/// resolves `mesh.indices` into one, so every run of three is one triangle
/// with no shared index buffer to keep in step.
///
/// This is the same "already-written vertices, clip after the fact" shape as
/// [`clip_quads_from`], but does not need that function's NDC round-trip
/// (`canvas`/`px`/`py`): a [`ModelVertex`]'s `position` is GUI pixel space
/// already (see [`draw_stack_clipped`]'s doc), not NDC.
fn clip_model_triangles(verts: &mut Vec<ModelVertex>, from: usize, clip: Rect) {
    let tail = verts.split_off(from);
    for tri in tail.chunks_exact(3) {
        let poly = clip_triangle(tri[0], tri[1], tri[2], clip);
        // Fan triangulation about the polygon's first vertex — valid because
        // Sutherland-Hodgman always returns a convex polygon.
        for i in 1..poly.len().saturating_sub(1) {
            verts.push(poly[0]);
            verts.push(poly[i]);
            verts.push(poly[i + 1]);
        }
    }
}

/// Sutherland-Hodgman: clips one triangle against `clip`'s four half-planes
/// in turn (left, right, top, bottom), returning the surviving convex
/// polygon — empty when the triangle is wholly outside, the original three
/// vertices (in the same winding) when wholly inside, and 3..=7 vertices for
/// a genuine straddle. Each half-plane is `sign * position[axis] >=
/// sign * boundary`, so flipping `sign` turns a "greater-than" test into a
/// "less-than" one against the same `boundary` value.
#[must_use]
fn clip_triangle(a: ModelVertex, b: ModelVertex, c: ModelVertex, clip: Rect) -> Vec<ModelVertex> {
    let mut poly = vec![a, b, c];
    let edges: [(usize, f32, f32); 4] = [
        (0, 1.0, clip.x),
        (0, -1.0, clip.x + clip.w),
        (1, 1.0, clip.y),
        (1, -1.0, clip.y + clip.h),
    ];
    for (axis, sign, boundary) in edges {
        if poly.is_empty() {
            break;
        }
        let inside = |v: &ModelVertex| sign * v.position[axis] >= sign * boundary;
        let mut out = Vec::with_capacity(poly.len() + 1);
        for i in 0..poly.len() {
            let cur = poly[i];
            let prev = poly[(i + poly.len() - 1) % poly.len()];
            let (cur_in, prev_in) = (inside(&cur), inside(&prev));
            if cur_in != prev_in {
                out.push(lerp_model_vertex(prev, cur, axis, boundary));
            }
            if cur_in {
                out.push(cur);
            }
        }
        poly = out;
    }
    poly
}

/// The point on segment `prev -> cur` where `position[axis]` crosses
/// `boundary`, with every continuous attribute (`position`, `uv`, `ao`)
/// linearly interpolated to match — valid with no perspective correction
/// because nothing between here and the GPU's `gui_ortho` divides by `w`;
/// this is a plain affine cut. The packed integer fields
/// (`light`/`tint`/`anim`/`cutout_bypass`/`tint_rgb_override`) are constant
/// across every vertex of one baked quad — they encode per-face/per-block
/// state, never a per-vertex gradient — so a cut vertex just inherits them
/// from `prev` (`..prev`) rather than interpolating a value that cannot
/// disagree with itself.
#[must_use]
fn lerp_model_vertex(prev: ModelVertex, cur: ModelVertex, axis: usize, boundary: f32) -> ModelVertex {
    let t = (boundary - prev.position[axis]) / (cur.position[axis] - prev.position[axis]);
    let lerp = |p: f32, c: f32| p + (c - p) * t;
    ModelVertex {
        position: [
            lerp(prev.position[0], cur.position[0]),
            lerp(prev.position[1], cur.position[1]),
            lerp(prev.position[2], cur.position[2]),
        ],
        uv: [lerp(prev.uv[0], cur.uv[0]), lerp(prev.uv[1], cur.uv[1])],
        ao: lerp(prev.ao, cur.ao),
        ..prev
    }
}

/// Clips every already-emitted flat quad on `verts[from..]` to `clip`, in
/// place. Each quad is six vertices at `floats_per_vertex` floats each — the
/// emission order [`crate::hud::item_icon::push_sprite_quad`] and
/// `ColourStream::rect` both use (`v0=(x0,y0)`, `v1=(x1,y0)`, `v2=(x1,y1)`,
/// duplicated at `v3`/`v4`, `v5=(x0,y1)`) — so the destination rect (and,
/// when `has_uv`, the UV rect) can be read straight back out of the NDC
/// vertices already written, the same trick
/// `a_frame_straddling_the_viewport_edge_draws_only_its_visible_sliver`
/// uses to decode a single rect's width, generalised here to a whole quad
/// (and, for a UV-carrying stream, handed to [`clip_sprite_quad`] itself —
/// the exact function a frame sprite clips through, so the two paths agree).
/// A quad the clip drops entirely is not re-emitted, so `verts` may shrink.
fn clip_quads_from(
    verts: &mut Vec<f32>,
    from: usize,
    floats_per_vertex: usize,
    has_uv: bool,
    canvas: (f32, f32),
    clip: Rect,
) {
    let stride = floats_per_vertex * 6;
    let (vw, vh) = canvas;
    let px = |ndc: f32| (ndc + 1.0) * vw / 2.0;
    let py = |ndc: f32| (1.0 - ndc) * vh / 2.0;
    let tail = verts.split_off(from);
    for chunk in tail.chunks_exact(stride) {
        let fpv = floats_per_vertex;
        let (x0, y0) = (px(chunk[0]), py(chunk[1]));
        let x1 = px(chunk[fpv]);
        let y1 = py(chunk[fpv * 2 + 1]);
        if has_uv {
            let q = GuiSpriteQuad {
                dst: [x0, y0, x1 - x0, y1 - y0],
                uv_min: [chunk[2], chunk[3]],
                uv_max: [chunk[fpv + 2], chunk[fpv * 2 + 3]],
            };
            let tint = [chunk[4], chunk[5], chunk[6], chunk[7]];
            if let Some(clipped) = clip_sprite_quad(q, clip) {
                crate::hud::item_icon::push_sprite_quad(verts, vw, vh, clipped, tint);
            }
        } else {
            let mut r = Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 };
            let colour = [chunk[2], chunk[3], chunk[4], chunk[5]];
            if clamp_to(&mut r, clip) {
                let mut cs = crate::hud::item_icon::ColourStream { verts: &mut *verts, w: vw, h: vh };
                cs.rect(r.x, r.y, r.w, r.h, colour);
            }
        }
    }
}

/// The tab-button sprite for `index` — `AdvancementTabType.extractRenderState`
/// (`AdvancementTabType.java`): the `left` sprite at index 0, the `right`
/// one at `max - 1`, `middle` otherwise, where `max` is the **type**'s capacity
/// (8 for `ABOVE`) and not the number of tabs shown. With five roots that means
/// no tab ever draws the `right` variant — vanilla's own behaviour, and it looks
/// right because the middle sprite is symmetric.
#[must_use]
pub fn tab_sprite(index: usize, selected: bool) -> &'static str {
    match (index, selected) {
        (0, false) => "advancements/tab_above_left",
        (0, true) => "advancements/tab_above_left_selected",
        (i, false) if i == TAB_MAX - 1 => "advancements/tab_above_right",
        (i, true) if i == TAB_MAX - 1 => "advancements/tab_above_right_selected",
        (_, false) => "advancements/tab_above_middle",
        (_, true) => "advancements/tab_above_middle_selected",
    }
}

/// `AdvancementTabType.ABOVE`'s `max` (`AdvancementTabType.java`).
const TAB_MAX: usize = 8;

/// `AdvancementToast.DISPLAY_TIME` (`AdvancementToast.java`), milliseconds.
pub const TOAST_DISPLAY_MS: u64 = 5000;

/// Newly-completed advancements, queued for the HUD toast.
///
/// ## The seed is the whole design
///
/// Vanilla's `ClientAdvancements` fires a toast from `onUpdateAdvancementProgress`,
/// which the server only calls for a *change*. Our side sees a snapshot instead,
/// and the join packet's `reset` batch carries every advancement already earned —
/// so a naive "obtained now, not obtained last frame" test would fire sixty toasts
/// at once on entering a long-played world. The first non-empty observation is
/// therefore adopted silently, and only later transitions toast.
#[derive(Debug, Clone, Default)]
pub struct AdvancementToastQueue {
    obtained: std::collections::HashSet<&'static str>,
    pending: std::collections::VecDeque<&'static Advancement>,
    /// Whether the join batch has been adopted. Stays `false` while the store is
    /// empty, so a session that has not received `UPDATE_ADVANCEMENTS` yet does
    /// not treat the *first* real advancement as a seed.
    seeded: bool,
    shown_at_ms: Option<u64>,
}

impl AdvancementToastQueue {
    /// Fold this frame's progress snapshot.
    pub fn observe(&mut self, progress: &AdvancementProgress) {
        if progress.is_empty() {
            return;
        }
        let now: std::collections::HashSet<&'static str> = ADVANCEMENTS
            .iter()
            .filter(|a| progress.obtained(a.id))
            .map(|a| a.id)
            .collect();
        if !self.seeded {
            self.seeded = true;
            self.obtained = now;
            return;
        }
        for advancement in ADVANCEMENTS {
            if now.contains(&advancement.id) && !self.obtained.contains(&advancement.id) {
                self.pending.push_back(advancement);
            }
        }
        self.obtained = now;
    }

    /// The advancement whose toast should be on screen at `now_ms`, retiring one
    /// that has had its [`TOAST_DISPLAY_MS`].
    pub fn current(&mut self, now_ms: u64) -> Option<&'static Advancement> {
        match self.shown_at_ms {
            Some(started) if now_ms.saturating_sub(started) >= TOAST_DISPLAY_MS => {
                self.pending.pop_front();
                self.shown_at_ms = None;
            }
            Some(_) => {}
            None => {}
        }
        let front = *self.pending.front()?;
        if self.shown_at_ms.is_none() {
            self.shown_at_ms = Some(now_ms);
        }
        Some(front)
    }

    /// How many toasts are waiting, the shown one included.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// Whether nothing is queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

/// The `advancements.toast.{task,goal,challenge}` heading for a frame type, and
/// its colour: `-30465` (`0xFFFF88FF`) for a challenge, `-256` (yellow) otherwise
/// (`AdvancementToast.java`).
#[must_use]
pub fn toast_heading(frame: AdvancementFrame) -> (&'static str, &'static str, [f32; 4]) {
    const CHALLENGE: [f32; 4] = [1.0, 0x88 as f32 / 255.0, 1.0, 1.0];
    const OTHER: [f32; 4] = [1.0, 1.0, 0.0, 1.0];
    match frame {
        AdvancementFrame::Task => ("advancements.toast.task", "Advancement Made!", OTHER),
        AdvancementFrame::Goal => ("advancements.toast.goal", "Goal Reached!", OTHER),
        AdvancementFrame::Challenge => {
            ("advancements.toast.challenge", "Challenge Complete!", CHALLENGE)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn there_are_five_tabs_and_each_lays_out() {
        let tabs = advancement_tabs();
        assert_eq!(tabs.len(), 5);
        let mut state = AdvancementsState::default();
        let progress = AdvancementProgress::default();
        for i in 0..tabs.len() {
            state.select_tab(i);
            let layout = advancements_layout(&mut state, &progress, 2, 1280, 720)
                .unwrap_or_else(|| panic!("tab {i} has no layout"));
            assert!(!layout.tree.nodes.is_empty());
            assert_eq!(layout.tabs.len(), 5);
            // Every tab centres its tree, so at least one widget must land inside
            // the viewport — the check that would catch a scroll expression that
            // parks the tree off-screen.
            assert!(!layout.widgets.is_empty(), "tab {i} shows no widgets");
        }
    }

    #[test]
    fn a_click_on_each_tab_resolves_to_that_tab() {
        let mut state = AdvancementsState::default();
        let progress = AdvancementProgress::default();
        let layout = advancements_layout(&mut state, &progress, 1, 1280, 720).expect("a layout");
        for (i, r) in layout.tabs.iter().enumerate() {
            let hit = advancements_hit_test(
                &layout,
                1,
                1280,
                720,
                r.x + r.w * 0.5,
                r.y + r.h * 0.5,
            );
            assert_eq!(hit, Some(AdvancementsHit::Tab(i)), "tab {i} is unclickable");
        }
    }

    #[test]
    fn a_click_on_a_widget_resolves_to_that_advancement() {
        let mut state = AdvancementsState::default();
        let progress = AdvancementProgress::default();
        let layout = advancements_layout(&mut state, &progress, 1, 1280, 720).expect("a layout");
        let mut resolved = 0;
        for (i, r) in &layout.widgets {
            // Probe the centre of the part that is actually *inside* the
            // viewport. A widget straddling the edge is listed (it draws) but only
            // its visible half is clickable, which is the behaviour to check
            // rather than work around.
            let mut visible = *r;
            assert!(clamp_to(&mut visible, layout.inside), "a listed widget is off-screen");
            let hit = advancements_hit_test(
                &layout,
                1,
                1280,
                720,
                visible.x + visible.w * 0.5,
                visible.y + visible.h * 0.5,
            );
            match hit {
                Some(AdvancementsHit::Widget(found)) => {
                    // Widgets can overlap, so the answer need not be `i` — but it
                    // must be a real widget of this tab.
                    assert!(layout.widgets.iter().any(|(j, _)| *j == found));
                    resolved += 1;
                }
                // The centre of the visible sliver can still land on a *different*
                // widget drawn over it; what must never happen is resolving to
                // nothing inside the viewport.
                other => panic!("widget {i} resolved to {other:?}"),
            }
        }
        // The 234x113 viewport fits roughly 8 columns of 3 rows, so a centred tree
        // shows a handful — the number is a floor against "nothing is clickable",
        // not a claim about how many vanilla would show.
        assert!(resolved >= 5, "only {resolved} widgets were clickable");
    }

    // -- clipping, not culling -----------------------------------------------

    #[test]
    fn clip_sprite_quad_shrinks_dst_and_uv_in_lock_step() {
        // A 26x26 sprite spanning the whole atlas UV range, straddling a clip
        // rect's right edge by exactly half its width and half its height.
        let q = GuiSpriteQuad {
            dst: [100.0, 100.0, 26.0, 26.0],
            uv_min: [0.0, 0.0],
            uv_max: [1.0, 1.0],
        };
        let clip = Rect { x: 0.0, y: 0.0, w: 113.0, h: 113.0 };
        let clipped = clip_sprite_quad(q, clip).expect("must survive — it overlaps");
        // Destination: only the left/top half survives (100..113 of 100..126).
        assert_eq!(clipped.dst, [100.0, 100.0, 13.0, 13.0]);
        // UV: the *same* fraction (0.5) is kept on each axis — this is the
        // property a naive "shrink dst, leave UV alone" implementation gets
        // wrong, which would stretch the visible half across the whole sprite
        // instead of showing its actual left/top half.
        assert_eq!(clipped.uv_min, [0.0, 0.0]);
        assert_eq!(clipped.uv_max, [0.5, 0.5]);
    }

    #[test]
    fn clip_sprite_quad_leaves_a_wholly_contained_quad_unchanged() {
        let q = GuiSpriteQuad {
            dst: [10.0, 10.0, 26.0, 26.0],
            uv_min: [0.25, 0.25],
            uv_max: [0.75, 0.75],
        };
        let clip = Rect { x: 0.0, y: 0.0, w: 200.0, h: 200.0 };
        assert_eq!(clip_sprite_quad(q, clip), Some(q));
    }

    #[test]
    fn clip_sprite_quad_drops_a_quad_wholly_outside() {
        let q = GuiSpriteQuad {
            dst: [500.0, 500.0, 26.0, 26.0],
            uv_min: [0.0, 0.0],
            uv_max: [1.0, 1.0],
        };
        let clip = Rect { x: 0.0, y: 0.0, w: 113.0, h: 113.0 };
        assert_eq!(clip_sprite_quad(q, clip), None);
        // Control: the same quad moved onto the clip rect must survive, so the
        // rejection above is measuring position and not always answering `None`.
        let overlapping = GuiSpriteQuad {
            dst: [50.0, 50.0, 26.0, 26.0],
            ..q
        };
        assert!(clip_sprite_quad(overlapping, clip).is_some());
    }

    /// The magnitude assertion for the reported bug itself: a widget frame
    /// that only *just* crosses into the viewport must draw a **small**
    /// sliver, not the full `26 x 26` frame unclamped. Exercised on the
    /// jar-less fallback path (`push_sprite_clipped`'s `None` arm — every
    /// test in this module goes through it, since none attaches a real
    /// `ContainerBackground`), decoding the emitted rect's pixel width back
    /// out of its NDC vertices rather than trusting a vertex *count*, which
    /// is six either way and would not discriminate clipped from unclamped.
    #[test]
    fn a_frame_straddling_the_viewport_edge_draws_only_its_visible_sliver() {
        let inside = Rect { x: 9.0, y: 18.0, w: INSIDE_W, h: INSIDE_H };
        // One pixel of a 26x26 frame poking in from the right edge.
        let straddling = Rect { x: inside.x + inside.w - 1.0, y: inside.y + 5.0, w: FRAME_SIZE, h: FRAME_SIZE };
        let canvas_w = 400.0;
        let mut b = Builder::new(canvas_w, 300.0, None);
        push_sprite_clipped(&mut b, None, "advancements/frame_unobtained", straddling, inside);
        assert_eq!(b.verts.len(), 36, "one flat rect: 6 vertices x 6 floats each");
        // Vertex 0 is (x0, y0), vertex 1 is (x1, y0) — `ColourStream::rect`'s own
        // emission order — so `verts[6] - verts[0]` is the rect's NDC width,
        // and `* canvas_w / 2.0` undoes `to_ndc`'s `2*px/w - 1` back to pixels.
        let clipped_px_w = (b.verts[6] - b.verts[0]) * canvas_w / 2.0;
        assert!(
            (clipped_px_w - 1.0).abs() < 0.01,
            "expected a 1 px-wide sliver, got {clipped_px_w} px"
        );

        // The control: the unclamped `push_sprite` path draws the full 26 px
        // width for the exact same input, so the assertion above is measuring the
        // clip and not merely "some rect got drawn".
        let mut b_full = Builder::new(canvas_w, 300.0, None);
        push_sprite(&mut b_full, None, "advancements/frame_unobtained", straddling);
        let full_px_w = (b_full.verts[6] - b_full.verts[0]) * canvas_w / 2.0;
        assert!(
            (full_px_w - FRAME_SIZE).abs() < 0.01,
            "control: the unclamped path must draw the full {FRAME_SIZE} px, got {full_px_w}"
        );
        assert!(clipped_px_w < full_px_w, "the clip must actually shrink the rect");
    }

    // -- icon clipping: draw_stack_clipped must never spill past the edge ----

    /// Decodes every vertex `(px, py)` a flat colour-stream buffer holds,
    /// undoing `ColourStream`'s own `to_ndc` — the same read-back
    /// `a_frame_straddling_the_viewport_edge_draws_only_its_visible_sliver`
    /// already trusts for one rect's width, generalised to every vertex so a
    /// whole icon draw (swatch, label, count) can be checked at once rather
    /// than assuming which one call pushed it. Reads points, not vertex
    /// *counts* or a probe rect sampling only vertices — the vertex-sampling
    /// trap a coverage probe falls into when a quad *encloses* the probe
    /// rather than sitting inside it does not apply here, because every
    /// point this decodes is a real emitted vertex to begin with.
    fn decode_colour_px(verts: &[f32], canvas_w: f32, canvas_h: f32) -> Vec<(f32, f32)> {
        verts
            .chunks_exact(COLOUR_FLOATS_PER_VERTEX)
            .map(|v| {
                let px = (v[0] + 1.0) * canvas_w / 2.0;
                let py = (1.0 - v[1]) * canvas_h / 2.0;
                (px, py)
            })
            .collect()
    }

    /// The discriminating gate for icon clipping: an icon whose
    /// atlas-less swatch straddles the viewport's right edge must draw
    /// **nothing** past it. Every point is checked, not just a sampled
    /// bounding box, and every mismatch is collected rather than asserted
    /// inside the loop — a single `assert!` per point would report only the
    /// first escapee and hide how many there really were.
    #[test]
    fn an_icon_straddling_the_viewport_edge_draws_nothing_outside_it() {
        let inside = Rect { x: 9.0, y: 18.0, w: INSIDE_W, h: INSIDE_H };
        let (canvas_w, canvas_h) = (400.0, 300.0);
        let assets = IconAssets { items: None, models: None };
        let id: lodestone_model::Identifier = "minecraft:stick".parse().expect("a valid id");
        let stack = ItemStack::new(id, 1);

        // The fallback swatch is a 10x10 rect at (icon_x + 3, icon_y + 3)
        // (`Builder::draw_stack_counted`'s `_` arm) — place the icon so that
        // rect straddles the viewport's right edge by exactly one pixel,
        // mirroring the frame test just above.
        let icon_x = inside.x + inside.w - 1.0 - 3.0;
        let icon_y = inside.y + 5.0;

        let mut b = Builder::new(canvas_w, canvas_h, None);
        draw_stack_clipped(&mut b, &assets, &stack, icon_x, icon_y, inside, (canvas_w, canvas_h));
        let pts = decode_colour_px(&b.verts, canvas_w, canvas_h);
        assert!(!pts.is_empty(), "the straddling icon drew nothing at all");
        let escaped: Vec<(f32, f32)> = pts
            .iter()
            .copied()
            .filter(|&(px, py)| {
                px < inside.x - 0.01
                    || py < inside.y - 0.01
                    || px > inside.x + inside.w + 0.01
                    || py > inside.y + inside.h + 0.01
            })
            .collect();
        assert!(escaped.is_empty(), "vertices escaped the viewport: {escaped:?}");

        // The control: the same stack at the same position through plain
        // `Builder::draw_stack` (no clip) must spill past the edge, proving
        // the assertion above is measuring the clip and not "nothing draws
        // here" — [`Builder::draw_stack`] has no clip primitive of its own,
        // which is [`draw_stack_clipped`]'s whole reason to exist.
        let mut b_full = Builder::new(canvas_w, canvas_h, None);
        b_full.draw_stack(&assets, &stack, icon_x, icon_y);
        let full_pts = decode_colour_px(&b_full.verts, canvas_w, canvas_h);
        let full_escaped = full_pts
            .iter()
            .filter(|&&(px, _)| px > inside.x + inside.w + 0.01)
            .count();
        assert!(
            full_escaped > 0,
            "control: the unclamped draw must spill past the right edge"
        );
    }

    /// The completeness control the straddling gate above needs: an icon
    /// placed **wholly inside** the viewport must still draw in full. A gate
    /// that only ever checked containment would pass just as well if
    /// [`draw_stack_clipped`] silently dropped every icon outright.
    #[test]
    fn an_icon_wholly_inside_the_viewport_draws_completely() {
        let inside = Rect { x: 9.0, y: 18.0, w: INSIDE_W, h: INSIDE_H };
        let (canvas_w, canvas_h) = (400.0, 300.0);
        let assets = IconAssets { items: None, models: None };
        let id: lodestone_model::Identifier = "minecraft:stick".parse().expect("a valid id");
        let stack = ItemStack::new(id, 1);
        let (icon_x, icon_y) = (inside.x + 20.0, inside.y + 20.0);

        let mut b = Builder::new(canvas_w, canvas_h, None);
        draw_stack_clipped(&mut b, &assets, &stack, icon_x, icon_y, inside, (canvas_w, canvas_h));

        let mut b_full = Builder::new(canvas_w, canvas_h, None);
        b_full.draw_stack(&assets, &stack, icon_x, icon_y);

        assert_eq!(
            b.verts.len(),
            b_full.verts.len(),
            "a fully-inside icon must draw exactly as much as the unclamped path"
        );
        let mismatches: Vec<(usize, f32, f32)> = b
            .verts
            .iter()
            .zip(b_full.verts.iter())
            .enumerate()
            .filter(|(_, (a, c))| (**a - **c).abs() >= 0.01)
            .map(|(i, (a, c))| (i, *a, *c))
            .collect();
        assert!(
            mismatches.is_empty(),
            "a fully-inside icon must be pixel-identical to the unclamped draw, \
             but (index, clipped, unclamped) differ at {mismatches:?}"
        );
    }

    // -- block-icon clipping: the reported bug -------------------------------

    /// A 16x16 axis-aligned quad (two triangles, vertex 0/1/2 then 0/2/3 —
    /// the same winding [`push_item_model`](crate::hud::item_icon) leaves
    /// its own quads in) posed at `(x, y)` in GUI pixel space, with `uv`
    /// mapped **linearly** across the rect (`(0,0)` at the top-left corner,
    /// `(1,1)` at the bottom-right) so a clip's surviving UV range can be
    /// checked against a predicted fraction, not just its surviving
    /// position. `light`/`tint`/`anim`/`cutout_bypass`/`tint_rgb_override`
    /// are all set to distinguishable non-default values so a test can
    /// confirm they survive a cut unchanged rather than silently zeroing.
    fn model_quad(x: f32, y: f32, w: f32, h: f32) -> Vec<ModelVertex> {
        let corner = |u: f32, v: f32| ModelVertex {
            position: [x + u * w, y + v * h, 0.0],
            uv: [u, v],
            ao: 1.0,
            light: 0xAB,
            tint: 7,
            anim: 0,
            cutout_bypass: 0,
            tint_rgb_override: [11, 22, 33, 255],
        };
        let (v0, v1, v2, v3) = (corner(0.0, 0.0), corner(1.0, 0.0), corner(1.0, 1.0), corner(0.0, 1.0));
        vec![v0, v1, v2, v0, v2, v3]
    }

    /// The shoelace area of every triangle in `verts` (a flat triangle
    /// list), summed — the same "read the geometry back, do not trust a
    /// vertex count" principle [`decode_colour_px`] uses, generalised to an
    /// area rather than a set of points so a *partial* survivor can be told
    /// apart from a full or an empty one by a single predicted number.
    fn triangle_list_area(verts: &[ModelVertex]) -> f32 {
        verts
            .chunks_exact(3)
            .map(|tri| {
                let [ax, ay] = [tri[0].position[0], tri[0].position[1]];
                let [bx, by] = [tri[1].position[0], tri[1].position[1]];
                let [cx, cy] = [tri[2].position[0], tri[2].position[1]];
                ((bx - ax) * (cy - ay) - (cx - ax) * (by - ay)).abs() * 0.5
            })
            .sum()
    }

    /// The magnitude assertion for the reported bug itself: a block-model
    /// icon that only *just* crosses into the viewport must draw a
    /// **predicted partial** area — not the full `16x16 = 256` (the
    /// unclamped control) and not `0` (the pre-fix "drop the mesh whole"
    /// behaviour) — mirroring
    /// `a_frame_straddling_the_viewport_edge_draws_only_its_visible_sliver`'s
    /// shape for the flat-sprite path. The quad spans `x: 110..126`, cut by
    /// `clip`'s right edge at `x = 113`, so the surviving strip is exactly
    /// `3 x 16 = 48`.
    #[test]
    fn a_block_icon_straddling_the_viewport_edge_draws_a_predicted_partial_area() {
        let clip = Rect { x: 0.0, y: 0.0, w: 113.0, h: 113.0 };
        let quad = model_quad(110.0, 50.0, 16.0, 16.0);

        let mut clipped = quad.clone();
        clip_model_triangles(&mut clipped, 0, clip);

        let clipped_area = triangle_list_area(&clipped);
        assert!(
            (clipped_area - 48.0).abs() < 0.01,
            "expected the 3x16=48 surviving strip, got {clipped_area}"
        );

        // Controls: the two wrong hypotheses this test exists to rule out.
        let full_area = triangle_list_area(&quad);
        assert!(
            (full_area - 256.0).abs() < 0.01,
            "control: the unclamped mesh must cover the full 16x16=256, got {full_area}"
        );
        assert!(
            clipped_area > 0.0,
            "the icon was dropped whole — the pre-fix bug this test exists to catch"
        );
        assert!(
            clipped_area < full_area,
            "the icon drew unclamped — the clip had no effect"
        );

        // Containment: no surviving vertex may sit past the clip's right edge.
        let escaped: Vec<[f32; 3]> = clipped
            .iter()
            .map(|v| v.position)
            .filter(|p| p[0] > clip.x + clip.w + 1e-4)
            .collect();
        assert!(escaped.is_empty(), "vertices escaped the viewport: {escaped:?}");

        // Not merely snapped/scaled: the cut vertices' U must read back as
        // the true fraction-of-width at the cut (110..113 is 3/16 = 0.1875
        // of the quad), not 1.0 (which a "stretch the visible sliver back to
        // the full sprite" bug would produce) or 0.0 (a "snap to the left
        // edge" bug).
        let cut_us: Vec<f32> = clipped
            .iter()
            .filter(|v| (v.position[0] - clip.x - clip.w).abs() < 1e-3)
            .map(|v| v.uv[0])
            .collect();
        assert!(!cut_us.is_empty(), "the clip produced no vertex exactly on the cut edge");
        for u in cut_us {
            assert!(
                (u - 0.1875).abs() < 1e-4,
                "a cut vertex's U must read back as 0.1875 (the true 3/16 fraction), got {u}"
            );
        }

        // The packed per-face attributes must survive the cut unchanged —
        // `lerp_model_vertex`'s `..prev` half, not just its interpolated half.
        for v in &clipped {
            assert_eq!(v.light, 0xAB, "light must not change across a cut");
            assert_eq!(v.tint, 7, "tint must not change across a cut");
            assert_eq!(v.tint_rgb_override, [11, 22, 33, 255], "tint_rgb_override must not change across a cut");
        }
    }

    /// The completeness control the straddling gate above needs: a
    /// wholly-inside mesh must survive with its area (and vertex count)
    /// completely unchanged, mirroring
    /// `clip_sprite_quad_leaves_a_wholly_contained_quad_unchanged`. A clip
    /// that always shrank geometry, straddling or not, would still pass a
    /// gate that only ever checked "not the full unclamped area".
    #[test]
    fn clip_model_triangles_leaves_a_wholly_contained_mesh_unchanged() {
        let clip = Rect { x: 0.0, y: 0.0, w: 200.0, h: 200.0 };
        let quad = model_quad(10.0, 10.0, 16.0, 16.0);
        let mut clipped = quad.clone();
        clip_model_triangles(&mut clipped, 0, clip);
        assert_eq!(clipped, quad, "a wholly-inside mesh must be pixel-identical to the unclamped mesh");
    }

    /// The reciprocal control: a mesh wholly outside `clip` must vanish
    /// entirely (empty, not zero-area triangles left lying around), and the
    /// same mesh moved back onto the clip rect must survive — proving the
    /// rejection above is measuring position, not always answering empty,
    /// mirroring `clip_sprite_quad_drops_a_quad_wholly_outside`.
    #[test]
    fn clip_model_triangles_drops_a_mesh_wholly_outside() {
        let clip = Rect { x: 0.0, y: 0.0, w: 113.0, h: 113.0 };
        let mut far = model_quad(500.0, 500.0, 16.0, 16.0);
        clip_model_triangles(&mut far, 0, clip);
        assert!(far.is_empty(), "a wholly-outside mesh must leave no vertices behind");

        let mut overlapping = model_quad(100.0, 100.0, 16.0, 16.0);
        clip_model_triangles(&mut overlapping, 0, clip);
        assert!(
            !overlapping.is_empty(),
            "control: a mesh moved onto the clip rect must survive"
        );
    }

    // -- z-order: connector lines must draw behind every widget --------------

    /// Reconstructs the pixel-space rect of every flat colour-stream quad in
    /// `verts` — the same read-back [`clip_quads_from`] uses internally,
    /// exposed here so a z-order test can find a specific quad by its rect
    /// rather than by counting vertices, which is six either way and would
    /// not discriminate submission order.
    fn decode_colour_quads(verts: &[f32], canvas_w: f32, canvas_h: f32) -> Vec<Rect> {
        let px = |ndc: f32| (ndc + 1.0) * canvas_w / 2.0;
        let py = |ndc: f32| (1.0 - ndc) * canvas_h / 2.0;
        verts
            .chunks_exact(COLOUR_FLOATS_PER_VERTEX * 6)
            .map(|chunk| {
                let x0 = px(chunk[0]);
                let y0 = py(chunk[1]);
                let x1 = px(chunk[COLOUR_FLOATS_PER_VERTEX]);
                let y1 = py(chunk[COLOUR_FLOATS_PER_VERTEX * 2 + 1]);
                Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 }
            })
            .collect()
    }

    fn rects_close(a: Rect, b: Rect) -> bool {
        (a.x - b.x).abs() < 0.05
            && (a.y - b.y).abs() < 0.05
            && (a.w - b.w).abs() < 0.05
            && (a.h - b.h).abs() < 0.05
    }

    /// The magnitude assertion for the ordering fix: for a widget
    /// whose connector line genuinely crosses its own frame — any non-root
    /// widget qualifies, since the line's own terminal point sits at the
    /// widget's centre, well inside its `26 x 26` frame — the frame must draw
    /// after the line. Exercised on the jar-less fallback path, like every
    /// other geometry test in this module (`push_sprite_clipped`'s `None`
    /// arm).
    ///
    /// **Restated for the hover-dim fix**: a widget's own frame no longer
    /// shares `verts` with the connector lines at all — it now lands on
    /// [`ContainerGeometry::mid_verts`], a wholly separate stream
    /// `ContainerRenderer` draws through its own `container-mid-item-pass`,
    /// positioned after the chrome pass the line is in (see
    /// `menu::advancements`'s module doc and
    /// `ContainerGeometry::mid_bg_verts`'s doc for why). So "line before
    /// frame" is no longer a shared-vertex-index claim; it is a claim about
    /// *which stream* each landed on, backed by the renderer's own pass
    /// ordering. This test checks both halves: the line is still on `verts`,
    /// inside the chrome range, and the frame is on `mid_verts` and **not**
    /// on `verts` at all — the negative half a same-stream regression would
    /// fail.
    #[test]
    fn a_connector_line_draws_before_the_frame_it_crosses() {
        let mut state = AdvancementsState::default();
        let progress = AdvancementProgress::default();
        let layout = advancements_layout(&mut state, &progress, 1, 1280, 720).expect("a layout");
        let plan = draw_plan(&layout, &progress);

        // A widget with a parent, so at least one line segment reaches into
        // it — the root has no incoming line and would not discriminate.
        let frame_rect = layout
            .widgets
            .iter()
            .find(|(i, _)| layout.tree.nodes[*i].parent.is_some())
            .map(|(_, rect)| Rect {
                x: rect.x + FRAME_DX,
                y: rect.y,
                w: FRAME_SIZE,
                h: FRAME_SIZE,
            })
            .expect("the tree has at least one non-root widget on screen");

        // A foreground line segment whose rect actually overlaps that frame —
        // the terminal segment at the widget's own centre always does, since
        // `my_x`/`my_y` sit at `node + 13` and the frame spans
        // `node - FRAME_DX .. node - FRAME_DX + 26`.
        let crossing = plan
            .lines
            .iter()
            .find(|(r, shadow)| !*shadow && overlaps(*r, frame_rect))
            .map(|(r, _)| *r)
            .expect("no connector line crosses the chosen widget's frame");

        let view = AdvancementsView {
            title: "",
            hovered: None,
            hovered_title: "",
            hovered_description: "",
            progress: &progress,
            fade: 0.0,
        };
        let geo = advancements_geometry(&layout, view, 1, 1280, 720, None, None, None, None);
        let (canvas_w, canvas_h) = crate::menu::render::logical_canvas(1, 1280, 720);
        let verts_quads = decode_colour_quads(&geo.verts, canvas_w, canvas_h);
        let mid_quads = decode_colour_quads(&geo.mid_verts, canvas_w, canvas_h);

        let line_index = verts_quads.iter().position(|q| rects_close(*q, crossing)).unwrap_or_else(|| {
            panic!("the crossing line's own rect never appeared in the colour stream: {crossing:?} in {verts_quads:?}")
        });
        assert!(
            (line_index + 1) * 6 <= geo.chrome_vertex_count,
            "the crossing line (quad {line_index}, ending at vertex {}) must land \
             inside the chrome range (0..{}) — otherwise it is not actually in the \
             pass the renderer draws before the frame's own `mid_verts` pass",
            (line_index + 1) * 6,
            geo.chrome_vertex_count
        );

        let frame_in_mid = mid_quads.iter().any(|q| rects_close(*q, frame_rect));
        assert!(
            frame_in_mid,
            "the widget's own frame rect never appeared on `mid_verts`: \
             {frame_rect:?} in {mid_quads:?}"
        );

        // The negative half: a same-stream regression (the frame landing
        // back on the shared `verts` the lines are on, the pre-fix
        // architecture) would satisfy the two checks above just as well if
        // this were missing — `mid_quads` searches a *different* Vec, so
        // finding the frame there says nothing about whether it also still
        // exists on `verts`.
        let frame_in_verts = verts_quads.iter().any(|q| rects_close(*q, frame_rect));
        assert!(
            !frame_in_verts,
            "the widget's own frame rect ({frame_rect:?}) appeared on the shared \
             `verts` colour stream — it must draw only through `mid_verts`'s own \
             pass now, never sharing `verts` with the connector lines"
        );
    }

    // -- z-order: the tooltip must draw over every icon ----------------------

    #[test]
    fn hovering_a_widget_pushes_its_tooltip_into_the_carried_pass() {
        let mut state = AdvancementsState::default();
        let progress = AdvancementProgress::default();
        let layout = advancements_layout(&mut state, &progress, 1, 1280, 720).expect("a layout");
        let (hovered, _) = *layout.widgets.first().expect("at least one on-screen widget");

        let view = AdvancementsView {
            title: "Test Tab",
            hovered: Some(hovered),
            hovered_title: "Hovered Advancement",
            hovered_description: "A description long enough to need a panel.",
            progress: &progress,
            fade: 0.3,
        };
        // `background: None` throughout this module's tests (no atlas
        // attached), which matters here specifically: `push_sprite`'s
        // jar-less fallback degrades *every* sprite — the tooltip panel/bars/
        // frame-redraw included — to `Builder::rect_px`, i.e. the plain
        // colour stream, never `bg_verts`. So `bg_slot_vertex_count` cannot
        // be exercised from this test; what *is* exercised, and is the part
        // that used to be broken, is that every one of the tooltip's own
        // draws — the panel/bars/frame-redraw fallback rects *and* the
        // tooltip text *and* the icon-redraw fallback — lands after
        // `slot_vertex_count`, i.e. in the carried pass, alongside the
        // tooltip's own bg-front-pass content.
        //
        // **Restated a second time for the hover-dim fix.** A widget's own
        // icon no longer lives in this carried range at all — it moved to
        // `mid_verts`/`mid_item_verts`/`mid_glint_verts` (see the module doc
        // and `ContainerGeometry::mid_bg_verts`'s doc), so `verts`'s carried
        // range is now **exactly** the tooltip's own content, nothing else.
        let geo = advancements_geometry(&layout, view, 1, 1280, 720, None, None, None, None);
        assert!(
            geo.slot_vertex_count < geo.verts.len() / COLOUR_FLOATS_PER_VERTEX,
            "the tooltip's own draws (panel/bars/frame-redraw fallback rects, \
             the hover dim's colour range boundary, and the tooltip text) must \
             land after slot_vertex_count, in the carried pass — otherwise \
             they draw in the same pass as, and can end up under, every \
             widget's own icon"
        );

        // The control: with nothing hovered, the carried range is now
        // genuinely **empty** — the widget-icon content that used to live
        // here (before this fix moved it to `mid_verts`) no longer does, and
        // the tooltip itself draws nothing when idle. So the discriminating
        // claim is the strongest form available: hovering must take the
        // carried range from zero to non-zero, not merely "add more" on top
        // of an always-non-empty baseline.
        let no_hover = AdvancementsView {
            hovered: None,
            fade: 0.0,
            ..view
        };
        let geo_idle = advancements_geometry(&layout, no_hover, 1, 1280, 720, None, None, None, None);
        let hovered_carried_floats = geo.verts.len() - geo.slot_vertex_count * COLOUR_FLOATS_PER_VERTEX;
        let idle_carried_floats =
            geo_idle.verts.len() - geo_idle.slot_vertex_count * COLOUR_FLOATS_PER_VERTEX;
        assert_eq!(
            idle_carried_floats, 0,
            "idle (nothing hovered) must draw nothing in the carried range at \
             all — a widget's own icon no longer lives here post-fix, so \
             {idle_carried_floats} idle carried floats means something is back \
             on `verts`'s carried range that this fix was supposed to move out"
        );
        assert!(
            hovered_carried_floats > idle_carried_floats,
            "hovering must add tooltip content on top of the (now empty) idle \
             baseline: {hovered_carried_floats} carried floats hovered vs \
             {idle_carried_floats} idle"
        );
    }

    #[test]
    fn panning_is_clamped_to_the_tree() {
        let mut state = AdvancementsState::default();
        let progress = AdvancementProgress::default();
        let layout = advancements_layout(&mut state, &progress, 1, 1280, 720).expect("a layout");
        let tree = layout.tree.clone();
        let bounds = tree_bounds(&tree);
        let before = state.scroll_for(&tree);
        for _ in 0..50 {
            state.pan(&tree, 500.0, 500.0);
        }
        let after_right = state.scroll_for(&tree);
        for _ in 0..50 {
            state.pan(&tree, -500.0, -500.0);
        }
        let after_left = state.scroll_for(&tree);

        // An axis the tree does not overflow does not pan at all — vanilla's
        // `canScrollHorizontally`/`canScrollVertically` gate, and it is what stops
        // a short tree being dragged out of its own window.
        if bounds.w <= INSIDE_W {
            assert_eq!(after_right.0, before.0);
            assert_eq!(after_left.0, before.0);
        } else {
            assert_eq!(after_right.0, 0.0, "panning right past the end did not stop at 0");
            assert!(
                (after_left.0 + (bounds.x + bounds.w - INSIDE_W)).abs() < 0.001,
                "panning left past the end did not stop at the tree's far edge: {}",
                after_left.0
            );
        }
        if bounds.h <= INSIDE_H {
            assert_eq!(after_right.1, before.1);
        } else {
            assert_eq!(after_right.1, 0.0);
        }
    }

    #[test]
    fn the_tile_grid_covers_the_whole_viewport() {
        let mut state = AdvancementsState::default();
        let progress = AdvancementProgress::default();
        let layout = advancements_layout(&mut state, &progress, 1, 1280, 720).expect("a layout");
        let plan = draw_plan(&layout, &progress);
        assert!(plan.background.is_some(), "the root carries a background");
        // 17 columns x 10 rows, and the union must cover the viewport with the
        // seam off-screen at both ends.
        assert_eq!(plan.tiles.len(), 17 * 10);
        let min_x = plan.tiles.iter().map(|t| t.0).fold(f32::INFINITY, f32::min);
        let max_x = plan.tiles.iter().map(|t| t.0).fold(f32::NEG_INFINITY, f32::max);
        assert!(min_x <= layout.inside.x);
        assert!(max_x + TILE >= layout.inside.x + layout.inside.w);
    }

    #[test]
    fn every_connector_line_stays_inside_the_viewport() {
        let mut state = AdvancementsState::default();
        let progress = AdvancementProgress::default();
        for i in 0..advancement_tabs().len() {
            state.select_tab(i);
            let layout = advancements_layout(&mut state, &progress, 1, 1280, 720).expect("a layout");
            let plan = draw_plan(&layout, &progress);
            assert!(!plan.lines.is_empty(), "tab {i} has no connectors");
            for (r, _) in &plan.lines {
                assert!(
                    r.x >= layout.inside.x - 0.001
                        && r.y >= layout.inside.y - 0.001
                        && r.x + r.w <= layout.inside.x + layout.inside.w + 0.001
                        && r.y + r.h <= layout.inside.y + layout.inside.h + 0.001,
                    "a connector escaped the viewport: {r:?}"
                );
            }
        }
    }

    #[test]
    fn progress_text_is_omitted_for_a_single_requirement() {
        assert_eq!(progress_text(0, 1), None);
        assert_eq!(progress_text(0, 9).as_deref(), Some("0/9"));
    }

    /// Build a store the way the wire does — one `AdvancementsUpdated` with the
    /// nodes the server sent and their criterion times — so the join is exercised
    /// through the same record `v770`'s decode produces.
    fn store_with(
        entries: &[(&str, &[&[&str]], &[&str])],
    ) -> lodestone_game::advancement::AdvancementStore {
        use lodestone_model::event::{AdvancementEntry, ClientEvent};
        let mut added = Vec::new();
        let mut progress = Vec::new();
        for (id, requirements, done) in entries {
            let parsed: lodestone_model::Identifier = id.parse().expect("a valid id");
            added.push(AdvancementEntry {
                id: parsed.clone(),
                parent: None,
                display: None,
                requirements: requirements
                    .iter()
                    .map(|group| group.iter().map(|n| (*n).to_string()).collect())
                    .collect(),
                sends_telemetry_event: false,
            });
            let criteria = requirements
                .iter()
                .flat_map(|group| group.iter())
                .map(|name| {
                    (
                        (*name).to_string(),
                        done.contains(name).then_some(1_700_000_000_000_i64),
                    )
                })
                .collect();
            progress.push((parsed, criteria));
        }
        let mut store = lodestone_game::advancement::AdvancementStore::default();
        store.apply(&ClientEvent::AdvancementsUpdated {
            reset: true,
            added,
            removed: Vec::new(),
            progress,
            show_advancements: true,
        });
        store
    }

    /// The join itself: a completed advancement draws its `*_obtained` frame and
    /// reports its group count, and an untouched one does neither.
    #[test]
    fn a_completed_advancement_draws_its_obtained_frame() {
        let store = store_with(&[
            ("minecraft:story/root", &[&["a"]], &["a"]),
            ("minecraft:story/mine_stone", &[&["x"], &["y"]], &["x"]),
        ]);
        let progress = AdvancementProgress::from_store(&store);
        assert!(progress.obtained("minecraft:story/root"));
        assert!(!progress.obtained("minecraft:story/mine_stone"));
        assert_eq!(progress.obtained_count(), 1);
        let partial = progress.get("minecraft:story/mine_stone");
        assert_eq!((partial.done, partial.total), (1, 2));
        assert_eq!(progress_text(partial.done, partial.total).as_deref(), Some("1/2"));
        // An id the server never sent reads as all-zero, not as obtained.
        assert_eq!(progress.get("minecraft:story/smelt_iron"), NodeProgress::default());

        let mut state = AdvancementsState::default();
        state.select_tab(
            advancement_tabs()
                .iter()
                .position(|t| t.id == "minecraft:story/root")
                .expect("story is a tab"),
        );
        let layout = advancements_layout(&mut state, &progress, 1, 1280, 720).expect("a layout");
        let plan = draw_plan(&layout, &progress);
        let sprites: Vec<&str> = plan.frames.iter().map(|(_, s, _, _)| *s).collect();
        assert!(
            sprites.contains(&"advancements/task_frame_obtained"),
            "the completed root drew no obtained frame: {sprites:?}"
        );
        assert!(
            sprites.contains(&"advancements/task_frame_unobtained"),
            "everything drew obtained: {sprites:?}"
        );
    }

    /// A `display.hidden` advancement is invisible until obtained, then appears —
    /// [`is_visible`] is the one gate both [`advancements_layout`] and
    /// [`draw_plan`] consult, so a widget and its connector can never disagree.
    #[test]
    fn a_hidden_advancement_appears_once_obtained() {
        const HIDDEN: &str = "minecraft:nether/all_effects";
        let hidden = ADVANCEMENTS
            .iter()
            .find(|a| a.id == HIDDEN)
            .expect("the data pack still carries it");
        assert!(hidden.hidden, "the fixture stopped being a hidden advancement");
        assert!(!is_visible(hidden, &AdvancementProgress::default()));
        let progress = AdvancementProgress::from_store(&store_with(&[(HIDDEN, &[&["all"]], &["all"])]));
        assert!(is_visible(hidden, &progress));
        // An ordinary advancement is visible either way.
        let plain = ADVANCEMENTS
            .iter()
            .find(|a| a.id == "minecraft:nether/root")
            .expect("the nether root");
        assert!(is_visible(plain, &AdvancementProgress::default()));
    }

    /// The join batch is adopted silently and only a later completion toasts —
    /// the difference between one toast and sixty on entering an old world.
    #[test]
    fn the_toast_queue_seeds_silently_then_fires() {
        let joined = AdvancementProgress::from_store(&store_with(&[(
            "minecraft:story/root",
            &[&["a"]],
            &["a"],
        )]));
        let mut queue = AdvancementToastQueue::default();
        queue.observe(&AdvancementProgress::default());
        queue.observe(&joined);
        assert!(queue.is_empty(), "the join batch toasted");

        let later = AdvancementProgress::from_store(&store_with(&[
            ("minecraft:story/root", &[&["a"]], &["a"]),
            ("minecraft:story/mine_stone", &[&["x"]], &["x"]),
        ]));
        queue.observe(&later);
        assert_eq!(queue.len(), 1);
        assert_eq!(
            queue.current(0).map(|a| a.id),
            Some("minecraft:story/mine_stone")
        );
        // Still the same toast a moment later, gone once its window closes.
        assert!(queue.current(TOAST_DISPLAY_MS - 1).is_some());
        assert!(queue.current(TOAST_DISPLAY_MS).is_none());
    }

    /// The fade rises to vanilla's `0.3` ceiling and falls twice as fast.
    #[test]
    fn the_hover_fade_ramps_to_its_ceiling_and_back() {
        let mut state = AdvancementsState::default();
        assert_eq!(state.tick_fade(true), FADE_RISE);
        for _ in 0..20 {
            state.tick_fade(true);
        }
        assert_eq!(state.fade(), FADE_CEILING);
        // `0.3 - 0.12 - 0.12 - 0.12` clamps at zero on the third frame out.
        state.tick_fade(false);
        state.tick_fade(false);
        assert!(state.fade() > 0.0);
        state.tick_fade(false);
        assert_eq!(state.fade(), 0.0);
    }
}
