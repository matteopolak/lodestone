//! The Advancements screen (issue #167) — vanilla's `AdvancementsScreen`,
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
//! inventory screen (#158) uses the same seam for the same reason.
//!
//! **Clipping is done on the CPU.** Vanilla scissors the 234×113 viewport;
//! `render_geometry_scaled` has no scissor, so [`advancements_geometry`] culls
//! any widget whose 26×26 frame falls entirely outside the viewport and clamps
//! the connector lines to it. A widget straddling the edge therefore draws
//! slightly past the frame where vanilla would cut it — the one visible
//! divergence, and the honest one to take rather than growing a scissor rect
//! through four call sites.

use lodestone_assets::ItemAtlas;
use lodestone_game::item::ItemStack;
use lodestone_render::BlockModels;

use crate::container::builder::Builder;
use crate::container::{ContainerBackground, ContainerGeometry, Rect};
use crate::hud::item_icon::IconAssets;

use super::advancement_data::{ADVANCEMENTS, Advancement, AdvancementFrame};
use super::advancement_tree::{TreeLayout, layout_tree};

/// `WINDOW_WIDTH` / `WINDOW_HEIGHT` (`AdvancementsScreen.java:26-27`).
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
/// (`AdvancementTabType.java:19-21`). With five roots every tab is `ABOVE`, so
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

/// `this.x = floor(display.getX() * 28.0F)` (`AdvancementWidget.java:61`).
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
/// (`AdvancementWidget.java:25`, blitted at `:233`/`:235`).
const SPRITE_TITLE_BOX: &str = "advancements/title_box";
/// `AdvancementWidgetType::boxSprite` — the hover tooltip's *title bar*, which
/// splits into an obtained and an unobtained half at the progress fraction.
const SPRITE_BOX_OBTAINED: &str = "advancements/box_obtained";
/// See [`SPRITE_BOX_OBTAINED`].
const SPRITE_BOX_UNOBTAINED: &str = "advancements/box_unobtained";

/// One text line's height, vanilla's font line advance.
const LINE_H: f32 = 9.0;
/// `TITLE_MAX_WIDTH` (`AdvancementWidget.java:38`) — the wrap width for the
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
/// (black) for the wider shadow underneath it (`AdvancementWidget.java:132`).
const LINE_FG: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
/// See [`LINE_FG`].
const LINE_BG: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

/// `extractTooltips`' `fill(0, 0, 234, 113, floor(fade * 255) << 24)`
/// (`AdvancementTab.java:150`): the viewport dims while a widget is hovered.
/// [`FADE_CEILING`] is the alpha it reaches; the ramp is
/// [`AdvancementsState::tick_fade`].
const HOVER_DIM_RGB: [f32; 3] = [0.0, 0.0, 0.0];
/// `Mth.clamp(fade + 0.06F, 0.0F, 0.3F)` (`AdvancementTab.java:98`) — the rise
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
/// text is drawn (`AdvancementWidget.java:82-90`).
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
    /// `scrollY = 56 - (maxY + minY) / 2` (`AdvancementTab.java:122-123`), where
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
/// (`AdvancementWidget.java:155`), so a hidden node appears the moment it is
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

/// `findOptimalLines` (`AdvancementWidget.java:96-115`): wrap at five candidate
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

/// `extractHover` (`AdvancementWidget.java:185-280`), including its two
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

    // Unselected tabs first, then the window, then the selected tab — vanilla's
    // own order (`AdvancementsScreen.java:206-215`), so an unselected tab is
    // partly covered by the window edge and the selected one is not.
    for (i, rect) in layout.tabs.iter().enumerate() {
        if i == layout_tab(layout) {
            continue;
        }
        push_sprite(&mut b, background, tab_sprite(i, false), *rect);
    }
    match background.and_then(|bg| bg.advancements_window_quad(layout.window.x, layout.window.y)) {
        Some(q) => b.bg_sprite(q),
        // The jar-less picture: a flat panel plus a darker viewport well, so the
        // tree still reads against something.
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
    // The tiled per-tab background, inside the window art so the window's own
    // frame is not painted over. Vanilla scissors this; we clamp instead (module
    // doc), which for a 16x16 tile grid means dropping the ring that falls wholly
    // outside and trimming the rest.
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
    push_sprite(
        &mut b,
        background,
        tab_sprite(layout_tab(layout), true),
        layout.tabs[layout_tab(layout)],
    );
    for (rect, sprite, _, _) in &plan.frames {
        push_sprite(&mut b, background, sprite, *rect);
    }
    // The hover tooltip's own sprites: description panel, then the title bar over
    // it, then the icon frame redrawn on top — `extractHover`'s order, and the
    // reason the frame is drawn twice per hovered widget.
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
    let bg_slot_floats = b.bg_verts.len();

    // The connector lines, shadows already ordered before foregrounds by
    // `draw_plan`. On the colour stream, which draws after the background
    // sprites and before the item icons — exactly where vanilla puts them.
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
    // The hover dim over the viewport, before the icons so a hovered
    // advancement's own icon stays bright. Ramped by `tick_fade`, so it also
    // draws for the few frames *after* the pointer leaves a widget.
    if view.fade > 0.0 {
        b.rect_px(
            layout.inside.x,
            layout.inside.y,
            layout.inside.w,
            layout.inside.h,
            [HOVER_DIM_RGB[0], HOVER_DIM_RGB[1], HOVER_DIM_RGB[2], view.fade],
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
    let chrome_floats = b.verts.len();

    // ---- the chrome/icon split ----

    for (_, _, stack, at) in &plan.frames {
        b.draw_stack(&assets, stack, at.0, at.1);
    }
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
    // The tooltip text, and the hovered widget's icon redrawn over its own frame.
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

    // Nothing here draws a carried stack, so the slot stratum runs to the end of
    // every stream and the renderer's carried passes are empty by construction.
    let slot_floats = b.verts.len();
    let slot_item_floats = b.item_verts.len();
    let slot_glint_floats = b.glint_verts.len();
    let slot_model_verts = b.model_verts.len();
    let slot_special = b.special.len();

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

/// The tab-button sprite for `index` — `AdvancementTabType.extractRenderState`
/// (`AdvancementTabType.java:97-107`): the `left` sprite at index 0, the `right`
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

/// `AdvancementTabType.ABOVE`'s `max` (`AdvancementTabType.java:21`).
const TAB_MAX: usize = 8;

/// `AdvancementToast.DISPLAY_TIME` (`AdvancementToast.java:22`), milliseconds.
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
/// (`AdvancementToast.java:63-65`).
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
