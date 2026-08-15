//! The heads-up display: a crosshair and an F3-style debug overlay.
//!
//! The overlay is the shell's instrument panel — position, facing, FPS, frame
//! time, chunk/section/quad counts, VRAM and process memory — so it is the first
//! thing that reveals whether the pipeline is actually fast and the first thing
//! that shows a regression. The same [`DebugStats`] is also printed to stdout on
//! a timer, so headless and windowed runs both produce evidence.
//!
//! Rendering has two streams. Text, the crosshair, and overlay chrome are
//! emitted as solid-colour quads in one dynamic vertex buffer (positions in NDC,
//! RGBA per vertex). The survival vitals — hotbar, XP bar, hearts, hunger — draw
//! from the vanilla GUI sprite atlas once [`HudRenderer::attach_gui`] supplies
//! one, via a second textured vertex stream; without an atlas (jar-less or
//! headless runs) they fall back to procedural quads on the colour stream. Both
//! streams are flat `Vec<f32>`s so they need no `bytemuck::Pod` derive (which the
//! workspace's `deny(unsafe_code)` would reject) and draw in a `Load` pass over
//! the terrain with no depth.

mod anim;
mod font;
pub(crate) mod item_icon;
pub mod vanilla_font;

pub use font::glyph_rows;
pub use vanilla_font::VanillaFont;
/// The hotbar's per-slot draw record. The container screen builds the same
/// record for every menu slot, so the type itself lives in [`item_icon`]; this
/// is the name the hotbar has always used for it.
pub use item_icon::ItemIcon as HotbarSlot;

use std::sync::Arc;
use crate::platform::Instant;

use lodestone_render::{
    BUBBLE_SIZE, BlockModels, GpuAtlas, GuiAtlas, GuiSpriteQuad, ModelVertex, bubble_position,
    bubble_row,
};

use lodestone_assets::ItemAtlas;
use lodestone_model::text::{TextColor, TextSpan};

use item_icon::{ColourStream, IconAssets, IconRenderer, IconSink, SpecialIconDraw};

use crate::overlay::{BossBarView, Sidebar};

/// Padding between a HUD panel's edge and its content.
pub(crate) const HUD_MARGIN: f32 = 6.0;

/// The gap between the hotbar's bottom edge and the bottom of the screen.
///
/// **Zero, because vanilla's hotbar is flush.** `Hud.extractItemHotbar` blits it
/// at `(guiWidth/2 - 91, guiHeight - 22, 182, 22)` and the selection at
/// `(…, guiHeight - 23, 24, 23)`; there is no bottom margin anywhere in that
/// method. This was [`HUD_MARGIN`] (6), which floated the whole cluster 6 px up.
///
/// **Not [`HUD_MARGIN`], and not a shared constant with it.** They answer
/// different questions — that one is the chat/debug text inset, this one is a
/// vanilla blit coordinate — and folding them together is what made correcting
/// one look like it would move the other. A named zero rather than a deleted
/// term so the next reader can see the decision was made rather than forgotten.
///
/// `pub` because the two hotbar item-icon pixel gates derive their read-back
/// rects from it. Each of them restated a `6.0` of its own and both went red the
/// moment this changed — which is the argument for one name rather than three.
pub const HOTBAR_MARGIN: f32 = 0.0;

/// `Hud.extractPlayerHealth`'s `yLineBase`, as a distance up from the bottom of
/// the screen: `int yLineBase = graphics.guiHeight() - 39`.
///
/// This is the hearts row's top *and* the hunger row's top (`extractFood` is
/// passed `yLineBase` unchanged), and every other row in the cluster is derived
/// from it — see [`vitals_line_base`].
///
/// **It is unconditional in vanilla.** It does not move for the XP bar, the game
/// mode, or anything else. This used to be computed by stacking upward from a
/// `cluster_top` that *did* move with the XP bar, which put the hearts 3 px too
/// high with an XP bar and 4 px too low without one — two different wrong
/// answers, neither of them 39.
const VITALS_LINE_BASE_FROM_BOTTOM: f32 = 39.0;

/// The vitals cluster's baseline row (hearts and hunger) for a canvas `canvas_h`
/// logical pixels tall — `Hud.extractPlayerHealth`'s `yLineBase`.
///
/// Public because the air-row pixel gate derives its screen rect from it. That
/// gate's own history is why: it hardcoded `lh - 39.0` once, which silently
/// assumed a stack shape the fixture did not have, and reported 0 px for a row
/// that was drawing perfectly. One expression, both callers.
#[must_use]
pub fn vitals_line_base(canvas_h: f32) -> f32 {
    canvas_h - VITALS_LINE_BASE_FROM_BOTTOM
}

/// The vertical pitch between two rows of the vitals cluster — the `10` in
/// vanilla's `yLineArmor = yLineBase - … - 10` and `yLineAir = yLineBase - 10`.
///
/// Written as the 9 px icon plus a 1 px gap, which is what it is, so the icon
/// size and the pitch cannot drift apart.
const VITALS_ROW_PITCH: f32 = 10.0;

/// Padding above and below the chat input's text inside its background strip,
/// in unscaled logical pixels.
///
/// Vanilla's input band is `fill(2, height - 14, width - 2, height - 2, …)`
/// (`ChatScreen.java`) around an `EditBox` whose text sits at `height - 12`
/// (`:56`) — 2px above the text and 2px below it. It is scaled by the chat pose
/// scale at every use, alongside the glyph height, so the strip stays wrapped
/// around the text at any chat scale.
const INPUT_STRIP_PAD: f32 = 2.0;

/// `DebugScreenOverlay.MARGIN_LEFT`/`MARGIN_RIGHT`/`MARGIN_TOP`, all `2`.
///
/// `extractLines` spends them as `left = alignLeft ? 2 : guiWidth() - 2 - width`
/// and `top = 2 + height * i`, so the same `2` is the left inset, the right
/// inset and the top inset.
///
/// **Not [`HUD_MARGIN`]**: the F3 overlay is vanilla's own screen with vanilla's
/// own metrics, and it draws in the already-`gui_scale`-divided logical canvas,
/// so it needs no HUD-side scaling of any kind.
pub(crate) const DEBUG_MARGIN: f32 = 2.0;

/// The F3 overlay's line pitch — vanilla's literal `int height = 9` in
/// `DebugScreenOverlay.extractLines`.
///
/// It is both the pitch (`top = 2 + height * i`) and the plate's own height
/// (`fill(…, top - 1, …, top + height - 1, …)` spans exactly `height` rows), so
/// consecutive plates tile with no seam and no overlap.
///
/// The overlay used to draw at an ad-hoc HUD-wide pitch of double this, which is
/// what "the text is way too big" was: exactly the mistake the XP level number's
/// own comment records, one screen over. `docs/hud-text-scale.md` has the fuller
/// history; the ad-hoc pitch itself is gone now that chat (its last consumer)
/// draws at vanilla's own metrics too.
pub(crate) const DEBUG_LINE_H: f32 = 9.0;

/// The plate behind each F3 overlay line — vanilla's
/// `fill(left - 1, top - 1, left + width + 1, top + height - 1, -1873784752)`,
/// i.e. `0x90505050` (`DebugScreenOverlay.extractLines`) — mid grey at 56%
/// alpha. Without it the overlay is unreadable over bright terrain, which is
/// what the shell shipped before it had one.
pub(crate) const DEBUG_LINE_BG: [f32; 4] = [
    0x50 as f32 / 255.0,
    0x50 as f32 / 255.0,
    0x50 as f32 / 255.0,
    0x90 as f32 / 255.0,
];

/// The F3 overlay's ink — vanilla's `-2039584`, i.e. `0xFFE0E0E0`, drawn
/// **without** a shadow (`extractLines` passes `shadow = false`).
pub(crate) const DEBUG_LINE_INK: [f32; 4] =
    [0xE0 as f32 / 255.0, 0xE0 as f32 / 255.0, 0xE0 as f32 / 255.0, 1.0];

/// The Tab player-list overlay's line pitch — vanilla's literal `9`
/// (`PlayerTabOverlay.extractRenderState`, which advances `yo` by `9` per row and
/// fills each slot `8` tall inside it).
///
/// **Vanilla's own metrics, not an ad-hoc HUD-wide pitch.** The tab overlay is a
/// vanilla *screen-space* draw in the already-`gui_scale`-divided logical canvas,
/// exactly like the F3 overlay above, so it uses vanilla's own metrics at scale
/// `1.0`. Drawing it at double that pitch is what "the text is way too big"
/// means, one screen over.
pub(crate) const TAB_LINE_H: f32 = 9.0;

/// The tab overlay's text scale — vanilla metrics, so `1.0`. See [`TAB_LINE_H`].
pub(crate) const TAB_TEXT_SCALE: f32 = 1.0;

/// The scoreboard sidebar's line pitch — `Hud.displayScoreboardSidebar`'s
/// literal `9` (`int height = entriesCount * 9;`, and each row's `y` advances
/// by that same `9` walking backwards from `bottom`).
///
/// **Vanilla's own metrics, not an ad-hoc HUD-wide pitch.** Exactly the same
/// exemption as [`TAB_LINE_H`]: this draws in the `gui_scale`-divided logical
/// canvas at vanilla's own font metrics — drawing it at double that pitch is
/// what made the sidebar panel twice vanilla's size.
pub(crate) const SIDEBAR_LINE_H: f32 = 9.0;

/// The sidebar's text scale — vanilla metrics, so `1.0`. See [`SIDEBAR_LINE_H`].
pub(crate) const SIDEBAR_TEXT_SCALE: f32 = 1.0;

/// The sidebar's edge inset — `Hud.displayScoreboardSidebar`'s literal `3` in
/// `int left = guiWidth() - width - 3;` and `int right = guiWidth() - 3 + 2;`.
const SIDEBAR_EDGE_MARGIN: f32 = 3.0;

/// `Options.getBackgroundColor(0.3F)`'s body-plate alpha, with
/// `backgroundForChatOnly` at its default (so the passed `0.3F` default is what
/// renders, not the user's chat background opacity option).
const SIDEBAR_BODY_BG_ALPHA: f32 = 0.3;

/// `Options.getBackgroundColor(0.4F)`'s header-plate alpha — see
/// [`SIDEBAR_BODY_BG_ALPHA`].
const SIDEBAR_HEADER_BG_ALPHA: f32 = 0.4;

/// `ChatFormatting.RED` (`0xFF5555`) — `StyledFormat.SIDEBAR_DEFAULT`'s colour,
/// the score column's default when a server sends no per-entry
/// [`lodestone_game::scoreboard::NumberFormat::Styled`] override.
const SIDEBAR_SCORE_DEFAULT: [f32; 3] = [1.0, 0x55 as f32 / 255.0, 0x55 as f32 / 255.0];

/// `BossHealthOverlay.BAR_WIDTH`/`BAR_HEIGHT` — every boss bar is this fixed
/// native size, never a fraction of the canvas width.
const BOSS_BAR_WIDTH: f32 = 182.0;
const BOSS_BAR_HEIGHT: f32 = 5.0;

/// `BossHealthOverlay.extractRenderState`'s `int yOffset = 12;` — the first
/// bar's top.
const BOSS_BAR_TOP: f32 = 12.0;

/// `BossHealthOverlay.extractRenderState`'s per-bar stride: `yOffset += 10 +
/// 9;` — 10 for the bar's own row pitch, 9 for the title above it.
const BOSS_BAR_STEP: f32 = 19.0;

/// The boss bar title's text scale — vanilla's `graphics.text(font, msg, x,
/// y, -1)` takes no pose scale at all, exactly like the action bar and the
/// held-item name (see `docs/hud-text-scale.md`).
const BOSS_BAR_TEXT_SCALE: f32 = 1.0;

/// `PlayerTabOverlay.MAX_ROWS_PER_COL`.
pub(crate) const TAB_MAX_ROWS_PER_COL: usize = 20;

/// Horizontal gap between two columns — the literal `5` in
/// `xo = xxo + col * slotWidth + col * 5`.
const TAB_COL_GAP: f32 = 5.0;

/// The 9 px a row reserves for its 8×8 player face, plus the 1 px vanilla leaves
/// between the face and the name (`xo += 9` after the face blit).
const TAB_HEAD_W: f32 = 9.0;

/// The per-row slack in vanilla's slot-width estimate — the literal `13` in
/// `cols * ((showHead ? 9 : 0) + maxNameWidth + widthForScore + 13)`. It is what
/// leaves room for the 10 px ping icon plus a pixel either side.
const TAB_ROW_SLACK: f32 = 13.0;

/// The margin vanilla keeps clear either side — the `screenWidth - 50` cap on
/// both the slot-width estimate and the header/footer wrap width.
const TAB_SCREEN_INSET: f32 = 50.0;

/// The overlay's top edge — `yyo = 10`.
const TAB_TOP: f32 = 10.0;

/// The ping icon's drawn size and its offset from the slot's right edge —
/// `blitSprite(sprite, xo + slotWidth - 11, yo, 10, 8)`.
const TAB_PING_W: f32 = 10.0;
/// See [`TAB_PING_W`].
const TAB_PING_H: f32 = 8.0;
/// See [`TAB_PING_W`].
const TAB_PING_INSET: f32 = 11.0;

/// The plate behind the header, the rows and the footer — vanilla's
/// `Integer.MIN_VALUE`, i.e. `0x80000000`: black at alpha `128`.
const TAB_PLATE: [f32; 4] = [0.0, 0.0, 0.0, 0x80 as f32 / 255.0];

/// The per-row fill — `options.getBackgroundColor(553648127)`, i.e. `0x20FFFFFF`:
/// **white** at alpha `32`, not another black wash. Getting this wrong is what
/// makes the rows read as one flat block instead of a striped list.
const TAB_ROW_FILL: [f32; 4] = [1.0, 1.0, 1.0, 0x20 as f32 / 255.0];

/// A row's ink. Opaque white, or `0x90FFFFFF` for a spectator
/// (`-1862270977`).
const TAB_INK: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
/// See [`TAB_INK`].
const TAB_INK_SPECTATOR: [f32; 4] = [1.0, 1.0, 1.0, 0x90 as f32 / 255.0];

/// The Tab player-list overlay's geometry, transcribed from
/// `PlayerTabOverlay.extractRenderState`.
///
/// **Exists so the draw and its gate share one expression rather than two that
/// agree today.** A pixel gate that recomputed `y` from its own copy of this
/// arithmetic would keep passing after the panel moved — a control whose premise
/// is false in the safe-looking direction. `build_inner` constructs one of these
/// and draws from it; a gate constructs one from the same inputs and measures
/// against it.
///
/// Every division below is vanilla's **integer** division, floored here for that
/// reason: `slot_w` in particular is `min(...) / cols`, and letting it stay
/// fractional would put column 1 half a pixel off vanilla at most widths.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct TabPanel {
    /// Number of columns the rows are split into.
    pub cols: usize,
    /// Rows **per column** — vanilla's `rows`, which is also the stride the
    /// `col = i / rows` / `row = i % rows` pair indexes with.
    pub rows: usize,
    /// One column's width.
    pub slot_w: f32,
    /// Left edge of column 0 — vanilla's `xxo`.
    pub x: f32,
    /// Top of the **rows** block, after any header — vanilla's `yyo` at the point
    /// the row loop starts.
    pub rows_top: f32,
    /// Top of the header block, or `rows_top` when there is no header.
    pub header_top: f32,
    /// Top of the footer block. Only meaningful when there is a footer.
    pub footer_top: f32,
    /// The widest thing on screen — vanilla's `maxLineWidth`, which is the row
    /// block's own width *widened* by any header or footer line that overflows
    /// it. Every plate spans this, centred on the screen.
    pub max_line_width: f32,
    /// The screen (logical canvas) width the layout was built for.
    pub screen_w: f32,
    /// How many header lines were laid out.
    pub header_len: usize,
    /// How many footer lines were laid out.
    pub footer_len: usize,
}

impl TabPanel {
    /// Lay the overlay out for a logical canvas and a content census.
    ///
    /// `max_name_width` and `widest_banner` must be measured with the **same**
    /// font and scale the draw uses; they are the only inputs vanilla takes from
    /// its font, and passing a differently-measured pair is how a layout and its
    /// draw silently disagree.
    pub fn new(
        screen_w: f32,
        slots: usize,
        show_head: bool,
        max_name_width: f32,
        header_len: usize,
        footer_len: usize,
        widest_banner: f32,
    ) -> Self {
        // `for (cols = 1; rows > 20; rows = (slots + cols - 1) / cols) { cols++; }`
        //
        // Read the loop in Java's own order — condition, body, *then* update —
        // or the arithmetic comes out one column wrong: `cols` is incremented
        // **before** `rows` is recomputed. 20 slots stay in one column of 20; 21
        // become two columns of 11 (not 20 + 1); 41 become three of 14.
        let mut cols = 1usize;
        let mut rows = slots;
        while rows > TAB_MAX_ROWS_PER_COL {
            cols += 1;
            rows = slots.div_ceil(cols);
        }
        let head_w = if show_head { TAB_HEAD_W } else { 0.0 };
        // `widthForScore` is 0: the tab list's score column needs a scoreboard
        // display objective, which this overlay is not given.
        let estimate = cols as f32 * (head_w + max_name_width + TAB_ROW_SLACK);
        let slot_w = (estimate.min(screen_w - TAB_SCREEN_INSET) / cols as f32).floor();
        let block_w = slot_w * cols as f32 + (cols as f32 - 1.0) * TAB_COL_GAP;
        let x = (screen_w * 0.5).floor() - (block_w * 0.5).floor();
        let max_line_width = block_w.max(widest_banner);
        // The header block occupies `header_len * 9`, then vanilla's bare `yyo++`
        // — one pixel of air between the header plate and the row plate.
        let header_top = TAB_TOP;
        let rows_top = if header_len > 0 {
            TAB_TOP + header_len as f32 * TAB_LINE_H + 1.0
        } else {
            TAB_TOP
        };
        // `yyo += rows * 9 + 1` before the footer plate.
        let footer_top = rows_top + rows as f32 * TAB_LINE_H + 1.0;
        Self {
            cols,
            rows,
            slot_w,
            x,
            rows_top,
            header_top,
            footer_top,
            max_line_width,
            screen_w,
            header_len,
            footer_len,
        }
    }

    /// Left edge of a plate — `screenWidth / 2 - maxLineWidth / 2 - 1`.
    pub fn plate_x(&self) -> f32 {
        (self.screen_w * 0.5).floor() - (self.max_line_width * 0.5).floor() - 1.0
    }

    /// A plate's width. Vanilla's `fill` runs to
    /// `screenWidth / 2 + maxLineWidth / 2 + 1`, so this is that minus
    /// [`plate_x`](Self::plate_x).
    pub fn plate_w(&self) -> f32 {
        (self.screen_w * 0.5).floor() + (self.max_line_width * 0.5).floor() + 1.0 - self.plate_x()
    }

    /// Top-left of row `i`'s slot, in column-major order — vanilla's
    /// `col = i / rows`, `row = i % rows`.
    ///
    /// Column-major is the whole reason `rows` is a field: a row-major reading
    /// (`col = i % cols`) produces a list that reads across instead of down, and
    /// on a single-column list the two are indistinguishable — which is why the
    /// gate for this has to use more than 20 players.
    pub fn slot_origin(&self, i: usize) -> [f32; 2] {
        let col = i / self.rows.max(1);
        let row = i % self.rows.max(1);
        [
            self.x + col as f32 * (self.slot_w + TAB_COL_GAP),
            self.rows_top + row as f32 * TAB_LINE_H,
        ]
    }

    /// Baseline of header line `i`.
    pub fn header_y(&self, i: usize) -> f32 {
        self.header_top + i as f32 * TAB_LINE_H
    }

    /// Baseline of footer line `i`.
    pub fn footer_y(&self, i: usize) -> f32 {
        self.footer_top + i as f32 * TAB_LINE_H
    }

    /// x for a line of width `text_w` centred on the screen — vanilla centres
    /// the header and footer on `screenWidth`, **not** on the row block.
    pub fn centred_x(&self, text_w: f32) -> f32 {
        (self.screen_w * 0.5).floor() - (text_w * 0.5).floor()
    }
}

/// Everything the debug overlay shows for one frame.
#[derive(Debug, Clone, Default)]
pub struct DebugStats {
    /// Player feet position (world blocks).
    pub position: [f64; 3],
    /// Yaw / pitch in degrees.
    pub yaw: f32,
    /// Pitch in degrees.
    pub pitch: f32,
    /// Frames presented in the last completed one-second window — a **count**,
    /// not a reciprocal, matching vanilla's own `Minecraft.runTick` counter.
    /// Deliberately not smoothed: an EMA over a per-second count would lag a
    /// real rate change without making the figure any more stable.
    pub fps: f32,
    /// Time spent *producing* the last frame, in milliseconds — from frame
    /// start to the end of our own submission, which **excludes** the frame
    /// limiter's wait.
    ///
    /// So this is deliberately **not** `1000.0 / fps`, and the two diverge by
    /// exactly the wait whenever a cap is active: at a 10 fps cap a frame that
    /// takes 2 ms of work still leaves ~98 ms of waiting. Reading one as the
    /// other's reciprocal is what hid a counter that reported 20,000 fps under
    /// a 10 fps cap, so the overlay labels them as separate quantities.
    pub frame_ms: f32,
    /// Loaded chunk columns.
    pub chunk_count: usize,
    /// Columns resident in the *live client-owned* world, read through
    /// [`crate::net::NetClient::loaded_chunks`]. Distinct from `chunk_count`
    /// (the locally rendered world): while connected, `0` here is the
    /// chunk-blackout signal, and a non-zero count is the section-read seam
    /// proving live world data is reaching the shell.
    pub live_columns: usize,
    /// Live columns that failed to mesh (guard rejected or all-air centre on a
    /// column the server reports loaded). Mirrors [`crate::sim::Sim`]'s
    /// `mesh_drops` counter; shown next to `LIVE COLS` so a recurrence of the
    /// silent-drop defect class is visible at a glance. Healthy sessions read `0`.
    pub mesh_drops: u64,
    /// Mesh sections **drawn this frame** (`RenderStats::sections_drawn`), i.e.
    /// post-cull — not the resident count, which is `RenderState::section_count`.
    /// The doc said "uploaded" and the value never was; the two differ by every
    /// section behind you, so this moves when you turn on the spot and that is
    /// correct for a drawn counter.
    pub section_count: usize,
    /// Quads **drawn this frame** (`RenderStats::total_quads`), post-cull, for
    /// the reason [`Self::section_count`] gives. Residency is
    /// `RenderState::total_quads`.
    pub quads: usize,
    /// Exact bytes of GPU mesh storage occupied by resident sections
    /// (`RenderStats::vram_bytes`). A pure function of residency: unlike the two
    /// counters above it must **not** move when the camera merely rotates.
    pub vram_bytes: usize,
    /// Bytes of GPU mesh storage the driver is holding, arena blocks whole
    /// (`RenderStats::vram_reserved_bytes`). Always `>= vram_bytes`; shown beside
    /// it because only the pair separates healthy span reuse from fragmentation.
    pub vram_reserved_bytes: usize,
    /// Resident process memory in bytes (0 if unavailable).
    pub rss_bytes: usize,
    /// Heap bytes owned by loaded world chunks (`World::heap_bytes`). Per the
    /// §12.24 ruling this is the single honest world-memory number — it reads
    /// the same whether the world is locally generated or client-owned.
    pub world_bytes: usize,
    /// Physics ticks per rendered frame since start (fixed-timestep health;
    /// vanilla runs 20 ticks/s, so at 50 FPS this settles near 0.4).
    pub frames_per_tick: f32,
    /// The block currently targeted by the view ray, if any.
    pub target: Option<[i32; 3]>,
    /// Entity instances drawn this frame (post-frustum-cull). `0` while
    /// disconnected or when no mobs are in view.
    pub entities_drawn: usize,
    /// Sections in the occlusion graph — [`crate::gpu::RenderStats::
    /// occlusion_graph_sections`], which is strictly **more** than
    /// [`Self::section_count`] because it includes sections with no geometry. A
    /// value that tracks `SECTIONS` instead means the fully-solid sections are
    /// missing and the walk has no floor to see.
    pub occlusion_graph_sections: usize,
    /// Sections the occlusion graph rejected this frame
    /// ([`crate::gpu::RenderStats::sections_culled_occlusion`]).
    ///
    /// **Zero is often correct.** At a near-horizontal camera the frustum has
    /// already removed the subsurface and the graph has nothing left to take;
    /// it only shows up looking steeply down or underground (measured 191 → 59
    /// sections at pitch 75). Read it next to [`Self::occlusion_active`], never
    /// alone.
    pub sections_culled_occlusion: usize,
    /// Sections the walk **would** have culled but drew anyway, because the graph
    /// is in shadow mode ([`crate::gpu::RenderStats::sections_occlusion_shadow`]).
    /// The soak counter: what flipping the cull on would remove, on the world you
    /// are standing in, while nothing can disappear yet.
    pub sections_occlusion_shadow: usize,
    /// Whether the graph is actually culling this frame
    /// ([`crate::gpu::RenderStats::occlusion_active`]).
    ///
    /// **The load-bearing one on this line.** Every failure mode of this cull
    /// draws *more*, so a zero cull count cannot by itself tell an open surface
    /// from a graph that refused to walk — without this flag on screen, a
    /// silently-dead graph looks identical to a correct one on a clear day.
    pub occlusion_active: bool,
    /// Camera walks this **session** (cumulative, not per frame —
    /// [`crate::gpu::RenderStats::occlusion_walks`]).
    ///
    /// Cumulative on purpose: the claim the invalidation cadence makes is that
    /// this does *not* increment while you turn on the spot (8-block cell
    /// crossings, frustum decoupled from reachability), and only a counter read
    /// across two frames can express that. A number rising while you stand still
    /// is a bug, not activity.
    pub occlusion_walks: u64,
    /// Live particles in the simulation this frame.
    pub particles_alive: usize,
    /// Particle billboards actually submitted to the GPU.
    pub particles_drawn: usize,
    /// Live particles whose sprite could not be resolved, so they were not
    /// drawn. Reported rather than dropped silently: a zero draw count against
    /// a non-zero alive count is exactly the "renders nothing, reports fine"
    /// state this counter exists to make visible.
    pub particles_unresolved: usize,
    /// A short connection/status line ("local world", "connecting…", …).
    pub status: String,
    /// The world difficulty and lock state, as the server last reported it
    /// (`Sim::difficulty`) — `None` until the first report arrives.
    /// `ServerDifficulty` reached a real, tested ECS fold in `44485e4` but
    /// nothing in the shell read it; this is that last hop.
    pub difficulty: Option<(lodestone_model::Difficulty, bool)>,
    /// Sky and block light at the player's feet, as the client's own world
    /// reports them — `None` before login or for an unloaded section, which is
    /// the honest "no data" state and is drawn as such.
    ///
    /// There is no "light-level pie chart" to draw: **26.2 does not have one.**
    /// `DebugScreenEntries` registers a `minecraft:light_levels` *text* entry
    /// (`DebugEntryLight`) that prints `Client Light: <raw> (<sky> sky, <block>
    /// block)`, and the pie was removed. So this reproduces the entry that
    /// actually exists rather than a chart that no longer does — see
    /// `docs/debug-overlay.md`.
    ///
    /// `(sky, block)`, each `0..=15`.
    pub light: Option<(u8, u8)>,
    /// Fixed lines describing the graphics adapter and backend, resolved **once**
    /// from `wgpu::Adapter::get_info()` when the GPU comes up.
    ///
    /// The owner's steer on this overlay was "not 1:1 — show information that is
    /// useful for *this* implementation", and this is the concrete half of it:
    /// vanilla's right column is full of JVM-shaped fields (GC, Java version,
    /// allocation rate) that have no analogue here, and printing a number we do
    /// not have is the same fabrication `menu::options` already refuses for an
    /// option we do not honour. The adapter, its backend and its reported limits
    /// *are* true of this client, and one of them has already caused a crash
    /// class: `max_bind_groups` reads 4 in a browser and 8 on this Mac, which is
    /// why the model shader is pinned at four groups.
    ///
    /// Empty off a GPU-less run (every headless gate), which draws no lines
    /// rather than placeholders.
    pub adapter: Vec<String>,
    /// The dimension the local player is in, as the server named it —
    /// `minecraft:overworld`, `minecraft:the_nether`, or a data pack's own id.
    ///
    /// The last line of vanilla's `position` group is
    /// `minecraft.level.dimension().identifier() + " FC: " + chunks.size()`, and
    /// this is its identifier half. Read from the local player's
    /// `lodestone_ecs::session::ServerDimension` component in `sim/step.rs`, so
    /// it follows a portal trip: that fold updates on `Respawned` as well as
    /// `Login`, which is the whole reason the too-bright-Nether bug is fixed.
    ///
    /// **`None` before login draws no line at all**, matching vanilla — whose
    /// entire `position` group is absent when there is no camera entity. It is
    /// not `-`, because unlike `Difficulty:` or `Client Light:` vanilla's line
    /// has no prefix to hang a placeholder off.
    pub dimension: Option<String>,
    /// Whether the F3+B entity-hitbox overlay is on.
    ///
    /// Mirrors `WindowApp::debug_hitboxes`, the `Arc<AtomicBool>` the world-line
    /// source closure actually reads. **Copied per frame rather than derived from
    /// a local guess** — the `Debug overlays:` line exists to report the state
    /// that decides whether boxes draw, and a second source of truth for it is
    /// how a hint that lies gets shipped.
    pub hitboxes_shown: bool,
    /// Whether the F3+G chunk-border overlay is on. See
    /// [`Self::hitboxes_shown`].
    pub chunk_borders_shown: bool,
}

/// Display name for a [`lodestone_model::Difficulty`] — vanilla's own
/// serialized keys (`Difficulty`'s `PEACEFUL(0, "peaceful")` … `HARD(3,
/// "hard")`), lowercase.
///
/// **Not the translated `options.difficulty.*` component**, which this overlay
/// has no translation table to draw from (see the module doc's "jar-less"
/// path). Lowercase rather than shouted because that is the F3 overlay's own
/// convention for an enum: `DebugEntryPosition` prints `Direction.toString()`,
/// which is the lowercase `name`, and the dimension as `minecraft:overworld`.
fn difficulty_name(d: lodestone_model::Difficulty) -> &'static str {
    match d {
        lodestone_model::Difficulty::Peaceful => "peaceful",
        lodestone_model::Difficulty::Easy => "easy",
        lodestone_model::Difficulty::Normal => "normal",
        lodestone_model::Difficulty::Hard => "hard",
    }
}

/// `DebugScreenOverlay.formatChart` — `formatKeybind(…) + " " + name + " " +
/// (status ? "visible" : "hidden")`, with `formatKeybind` bracketing the chord as
/// `"[" + modifier + "+" + key + "]"`.
///
/// The chord arrives as a literal (`"F3+B"`) rather than as a lookup, because
/// unlike vanilla's these two are not `KeyMapping`s — they are hardcoded in
/// `app/input.rs` behind the `KeyGate::debug_held` flag, so there is no
/// `getTranslatedKeyMessage` to ask and no unbound case to handle. **If they ever
/// become rebindable this must read the binding**, or the hint will name the old
/// key with total confidence.
fn format_toggle(chord: &str, name: &str, shown: bool) -> String {
    format!(
        "[{chord}] {name} {}",
        if shown { "visible" } else { "hidden" }
    )
}

/// `Mth.wrapDegrees(float)` — `angle % 360`, pulled into `[-180, 180)`.
///
/// `DebugEntryPosition` wraps both angles before printing them, so a player who
/// has spun twice reads `-12.3` rather than `708.0`. Rust's `%` and Java's `%`
/// agree on sign for floats, so this is the same two branches.
fn wrap_degrees(angle: f32) -> f32 {
    let mut wrapped = angle % 360.0;
    if wrapped >= 180.0 {
        wrapped -= 360.0;
    }
    if wrapped < -180.0 {
        wrapped += 360.0;
    }
    wrapped
}

impl DebugStats {
    /// Compass facing derived from yaw (Minecraft convention: yaw 0 = south, +Z).
    #[must_use]
    pub fn facing(&self) -> &'static str {
        // Normalise to [0,360).
        let y = self.yaw.rem_euclid(360.0);
        // south=0/360, west=90, north=180, east=270 (yaw increases clockwise).
        match y {
            v if !(45.0..315.0).contains(&v) => "south (+Z)",
            v if v < 135.0 => "west (-X)",
            v if v < 225.0 => "north (-Z)",
            _ => "east (+X)",
        }
    }

    /// The two halves of vanilla's `Facing:` line — `Direction.toString()` (the
    /// lowercase enum `name`) and `DebugEntryPosition`'s own `faceString`.
    ///
    /// The thresholds are [`Self::facing`]'s, which are already vanilla's:
    /// `Direction.fromYRot` is `from2DDataValue(floor(yRot / 90 + 0.5) & 3)`
    /// with `0 = SOUTH, 1 = WEST, 2 = NORTH, 3 = EAST`, and that flips exactly
    /// at yaw 45/135/225/315. Kept separate from `facing` because that method's
    /// `south (+Z)` shorthand is [`Self::one_line`]'s stdout format and is not
    /// what the overlay draws.
    #[must_use]
    pub fn facing_parts(&self) -> (&'static str, &'static str) {
        let y = self.yaw.rem_euclid(360.0);
        match y {
            v if !(45.0..315.0).contains(&v) => ("south", "Towards positive Z"),
            v if v < 135.0 => ("west", "Towards negative X"),
            v if v < 225.0 => ("north", "Towards negative Z"),
            _ => ("east", "Towards positive X"),
        }
    }

    /// The player's block position — `Entity.blockPosition()`, i.e. `Mth.floor`
    /// of each coordinate.
    ///
    /// **Not `as i64`.** A cast truncates toward zero, so it maps `-0.5` to `0`
    /// and puts a player just west of the origin in chunk `0` instead of chunk
    /// `-1`; every line below that divides or masks a coordinate inherits the
    /// error, and it is invisible at the origin.
    #[must_use]
    pub fn block_position(&self) -> [i64; 3] {
        [
            self.position[0].floor() as i64,
            self.position[1].floor() as i64,
            self.position[2].floor() as i64,
        ]
    }

    /// The overlay's text lines, in one flat list.
    ///
    /// Kept as the concatenation of [`Self::left_lines`] and
    /// [`Self::right_lines`] so nothing that wanted "every line" has to know
    /// about the column split, and so the two-column draw cannot silently drop
    /// a line: adding one to either column changes this too.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let mut all = self.left_lines();
        all.extend(self.right_lines());
        all
    }

    /// The **left** column: the player and the world around them.
    ///
    /// # How the column split was decided
    ///
    /// An earlier note here recorded a *deliberate* refusal to follow vanilla,
    /// on the grounds that "vanilla's split is mechanical, and a mechanical
    /// halve would reshuffle both columns every time a line is added".
    /// **That is superseded.** The premise was half right and the conclusion
    /// does not follow from it. `DebugScreenOverlay.extractRenderState` does not
    /// halve *lines*; it halves within three **categories**, and the categories
    /// are semantic:
    ///
    /// | category | how a line gets there | how it is placed |
    /// |---|---|---|
    /// | priority | `addPriorityLine` | into whichever column is currently shorter |
    /// | regular | `addLine` | the flat list halved at `mid = (n + 1) / 2` |
    /// | group | `addToGroup(id, …)` | whole named groups, halved by *group count* |
    ///
    /// So the thing that decides a line's column is which category its entry
    /// used, and each category block is separated from the next by a `""`
    /// spacer. Reproducing *that* is what makes the overlay look like vanilla's,
    /// and it is stable in exactly the way the old note wanted: adding a line to
    /// a group cannot move any other line across columns.
    ///
    /// What is **not** reproduced is running vanilla's halve over *our* entry
    /// set, because our set differs (no JVM entries, extra engine ones) and the
    /// arithmetic would then put `XYZ:` on the right — further from vanilla's
    /// screen, not closer. The category→column assignment below is therefore
    /// still by hand, but it is now *derived from vanilla's own default-profile
    /// output* rather than chosen freely: with `DebugScreenProfile.DEFAULT` the
    /// enabled entries are `3d_crosshair`, `fps`, `game_version`, `memory`,
    /// `player_position`, `player_section_position`,
    /// `simple_performance_impactors`, `system_specs` and `tps` (sorted by
    /// `Identifier.compareTo`, which compares *path* first), and vanilla's
    /// algorithm puts the fps line, the perf-impactor lines, the memory group
    /// and the position group on the **left**, and the version line, the tps
    /// line and the system group on the **right**. Ours match that placement.
    ///
    /// Order *within* a column is vanilla's, and so are the format strings —
    /// see `docs/debug-overlay.md` for the per-line ported/replaced/dropped
    /// table.
    #[must_use]
    pub fn left_lines(&self) -> Vec<String> {
        let [bx, by, bz] = self.block_position();
        // `ChunkPos.containing` / `SectionPos.blockToSectionCoord`, both `>> 4`.
        let (cx, cy, cz) = (bx >> 4, by >> 4, bz >> 4);
        let (facing, face_hint) = self.facing_parts();
        let mut out = vec![
            // `DebugEntryFps`: `"%d fps T: %s%s"`, a *priority* line, and the
            // first one added — so it lands left, because `addPriorityLine`
            // fills the shorter column and both start empty.
            //
            // `T:` is the framerate-limit target and the parenthetical after it
            // is the swapchain present mode. This shell now honours both (see
            // `app::pacing::effective_target_fps` and `Options::enable_vsync`),
            // so porting the `T:` half is no longer blocked on fabricating a
            // limit we do not enforce — it is simply unported, and wants the
            // target threaded onto `DebugStats` alongside the two fields here.
            //
            // The slot meanwhile carries the frame time we measure, labelled
            // `work` because it is **not** `1000 / fps`: it excludes the
            // limiter's wait, so under a cap the two legitimately disagree.
            // Leaving it unlabelled invited exactly that misreading — see both
            // fields' own docs.
            format!("{:.0} fps ({:.2} ms work)", self.fps, self.frame_ms),
            String::new(),
            // `DebugEntryLight`'s group, verbatim: `"Client Light: " +
            // rawBrightness + " (" + sky + " sky, " + block + " block)"`.
            // `getRawBrightness` is the max of the two, which is what the
            // renderer actually samples.
            match self.light {
                Some((sky, block)) => {
                    format!("Client Light: {} ({sky} sky, {block} block)", sky.max(block))
                }
                None => "Client Light: -".to_string(),
            },
            // Vanilla's neighbouring entry is `DebugEntryLocalDifficulty`,
            // `"Local Difficulty: %.2f // %.2f"` — a *server*-side scalar folded
            // from inhabited time and moon brightness, which we do not compute.
            // This is the world difficulty the server reported instead, so the
            // prefix deliberately omits `Local`.
            match self.difficulty {
                Some((d, locked)) => format!(
                    "Difficulty: {}{}",
                    difficulty_name(d),
                    if locked { " (locked)" } else { "" }
                ),
                None => "Difficulty: -".to_string(),
            },
            String::new(),
            // `DebugEntryPosition`'s group. The four format strings are
            // vanilla's, including the asymmetric `%.3f / %.5f / %.3f` (Y gets
            // five places because a step height or a fluid offset lives in the
            // fourth), the `r.X.Z.mca` region hint, and the `%02d` pad on the
            // section-relative triple.
            format!(
                "XYZ: {:.3} / {:.5} / {:.3}",
                self.position[0], self.position[1], self.position[2]
            ),
            format!("Block: {bx} {by} {bz}"),
            format!(
                "Chunk: {cx} {cy} {cz} [{} {} in r.{}.{}.mca]",
                cx & 31,
                cz & 31,
                cx >> 5,
                cz >> 5
            ),
            format!(
                "Facing: {facing} ({face_hint}) ({:.1} / {:.1})",
                wrap_degrees(self.yaw),
                wrap_degrees(self.pitch)
            ),
        ];
        // The fifth and last line `DebugEntryPosition` adds to its group is
        // `level.dimension().identifier() + " FC: " + chunks.size()`. The
        // identifier is real here; `FC` is `ServerLevel.getForceLoadedChunks`,
        // which the client has no view of, so the suffix is dropped rather than
        // printed as a `0` we did not measure. Absent rather than `-` before
        // login: vanilla omits its whole position group when there is no camera
        // entity, and this line has no prefix to hang a placeholder off.
        if let Some(dimension) = &self.dimension {
            out.push(dimension.clone());
        }
        out.extend([
            // `DebugEntrySectionPosition`, which joins the *position* group and
            // is therefore drawn after everything `DebugEntryPosition` added.
            format!("Section-relative: {:02} {:02} {:02}", bx & 15, by & 15, bz & 15),
            // `DebugEntryLookingAt.BlockStateInfo`'s first line, whose prefix is
            // the literal `"Targeted Block"` and whose separators are commas.
            // The block state and its properties are the rest of that group and
            // are not plumbed here — see the doc's table.
            match self.target {
                Some([x, y, z]) => format!("Targeted Block: {x}, {y}, {z}"),
                None => "Targeted Block: -".to_string(),
            },
            String::new(),
            // Vanilla closes the left column with its chart-keybind block, gated
            // on `isOverlayVisible()`:
            //
            //   Debug charts: [F3+2] Profiler hidden; [F3+1] FPS + TPS hidden;
            //   [F3+3] Ping hidden; [F3+4] Lightmap hidden
            //   To edit: press [F3+I]
            //
            // built from `formatChart` = `[mod+key] Name visible|hidden`. None of
            // those four charts exists here, but the two world overlays that do
            // are toggled by exactly this kind of chord and had no on-screen
            // state at all — so this is vanilla's shape carrying our real
            // toggles. `To edit:` is dropped: there is no entry-enable screen to
            // point at, and a hint naming a chord that does nothing is worse than
            // no hint.
            //
            // The booleans are copied per frame from the `Arc<AtomicBool>`s the
            // draw itself reads (`WindowApp::debug_hitboxes` /
            // `debug_chunk_borders`, flipped in `app/lifecycle.rs` and consumed
            // by `install_debug_lines_source`'s closure), never re-derived — the
            // line's whole job is to report the state that decides whether the
            // boxes draw.
            format!(
                "Debug overlays: {}; {}",
                format_toggle("F3+B", "Hitboxes", self.hitboxes_shown),
                format_toggle("F3+G", "Chunk borders", self.chunk_borders_shown)
            ),
        ]);
        out
    }

    /// The **right** column: the client's identity, then the render engine —
    /// where vanilla puts its version line, its server/tps line and its
    /// `system` group.
    ///
    /// See [`Self::left_lines`] for why each block sits in this column.
    #[must_use]
    pub fn right_lines(&self) -> Vec<String> {
        let mut out = vec![
            // `DebugEntryVersion`'s priority line, `"Minecraft " + version +
            // " (" + launched + "/" + brand + ")"`. It is the *second* priority
            // line added, so vanilla's shorter-column rule sends it right.
            format!("Lodestone {}", env!("CARGO_PKG_VERSION")),
            String::new(),
        ];
        // `DebugEntryTps`'s slot — `"\"%s\" server%s, %.0f tx, %.0f rx"` remote,
        // `"Integrated server @ %.1f/%.1f ms…"` in singleplayer. We have neither
        // a smoothed server tick time nor packet-rate counters, so this carries
        // the session status ("local world", "connecting…").
        //
        // Skipped when empty because vanilla's entry adds nothing at all with no
        // connection — pushing the blank instead would put two spacers in a row,
        // which draws as a double gap rather than as an absent line.
        if !self.status.is_empty() {
            out.push(self.status.clone());
        }
        out.extend([
            // `LevelExtractor.sectionStatistics`, `"C: %d/%d %sD: %d, %s"` —
            // rendered sections over total, then the view distance and the
            // dispatcher's queue. Ours is drawn-over-graph-nodes (the occlusion
            // graph is the closest thing here to vanilla's `ViewArea.size()`),
            // plus the two counters vanilla has no field for.
            format!(
                "C: {}/{} sections, {} columns, {} quads",
                self.section_count, self.occlusion_graph_sections, self.chunk_count, self.quads
            ),
            // `LevelExtractor.entityStatistics`, `"E: " + rendered + "/" + total
            // + ", SD: " + simulationDistance`. We track only the drawn count.
            format!("E: {}", self.entities_drawn),
            // `DebugEntryParticleRenderStats`, `"P: " + countParticles()`. The
            // unresolved count is ours and stays on the line: a zero draw
            // against a non-zero alive count is the "renders nothing, reports
            // fine" state that counter exists to expose.
            format!(
                "P: {}/{}, {} unresolved",
                self.particles_drawn, self.particles_alive, self.particles_unresolved
            ),
            String::new(),
            // No vanilla counterpart from here to the memory block: these are
            // this engine's own instruments, in a group of their own the way
            // `addToGroup` would give them one.
            //
            // `F/T` is fixed-timestep health (vanilla runs 20 ticks/s, so at
            // 50 fps this settles near 0.4). `Live cols`/`drops` is the
            // silent-mesh-drop detector. `Occl`'s `active`/`off` is the
            // load-bearing token — see `DebugStats::occlusion_active`: the other
            // four numbers cannot tell an open surface from a dead graph,
            // because every failure mode of that cull draws *more*.
            format!("F/T: {:.2}", self.frames_per_tick),
            format!(
                "Live cols: {}, drops: {}",
                self.live_columns, self.mesh_drops
            ),
            format!(
                "Occl: {}, nodes: {}, cull: {}, shadow: {}, walks: {}",
                if self.occlusion_active { "active" } else { "off" },
                self.occlusion_graph_sections,
                self.sections_culled_occlusion,
                self.sections_occlusion_shadow,
                self.occlusion_walks
            ),
            String::new(),
            // Vanilla's `memory` group is `DebugEntryMemory`'s three JVM heap
            // lines (`Mem:`, `Allocation rate:`, `Allocated:`) plus
            // `DebugEntryDetailedMemory`'s heap/non-heap pair. All five are JVM
            // facts and none has an analogue here, so the group is rebuilt from
            // the three real numbers this process can measure. `Mem:` keeps
            // vanilla's prefix but drops its `%2d%%` — that percentage is
            // `used / maxMemory` and there is no `-Xmx` to divide by.
            format!("Mem: {} MiB (RSS)", self.rss_bytes / (1024 * 1024)),
            format!("World: {} KiB", self.world_bytes / 1024),
            // `Mesh VRAM live/reserved`: the first is the spans handed out to
            // resident sections, the second the arena blocks the driver is
            // holding. Both are residency figures measured off the real
            // `wgpu::Buffer` sizes — a camera rotation must leave this line
            // unchanged, which is what distinguishes real load/unload churn from
            // the cull-derived estimate this used to print. KiB rather than
            // vanilla's MiB on purpose: the live figure's sawtooth is the signal
            // and MiB granularity flattens it.
            format!(
                "Mesh VRAM: {}/{} KiB",
                self.vram_bytes / 1024,
                self.vram_reserved_bytes / 1024
            ),
        ]);
        if !self.adapter.is_empty() {
            // Vanilla's `system` group — `DebugEntrySystemSpecs` — is `Java:`,
            // `CPU:`, `Display:`, the device name and the backend/driver pair.
            // The first is dropped as a JVM fact; the rest of this block is the
            // adapter, its backend and its reported limits, which are true of
            // this client and are what the group is *for*. Empty lines are
            // skipped by the draw, which is what makes the spacer a gap rather
            // than an empty plate.
            out.push(String::new());
            out.extend(self.adapter.iter().cloned());
        }
        out
    }

    /// One-line stdout summary (primary evidence in headless / logged runs).
    #[must_use]
    pub fn one_line(&self) -> String {
        format!(
            "pos=({:.1},{:.1},{:.1}) facing={} f/t={:.2} target={} fps={:.0} frame={:.2}ms chunks={} live_cols={} drops={} entities={} particles={}/{}+{}unres sections={} quads={} vram={}/{}KB world={}KB rss={}MB {}",
            self.position[0],
            self.position[1],
            self.position[2],
            self.facing(),
            self.frames_per_tick,
            match self.target {
                Some([x, y, z]) => format!("{x},{y},{z}"),
                None => "-".to_string(),
            },
            self.fps,
            self.frame_ms,
            self.chunk_count,
            self.live_columns,
            self.mesh_drops,
            self.entities_drawn,
            self.particles_drawn,
            self.particles_alive,
            self.particles_unresolved,
            self.section_count,
            self.quads,
            self.vram_bytes / 1024,
            self.vram_reserved_bytes / 1024,
            self.world_bytes / 1024,
            self.rss_bytes / (1024 * 1024),
            self.status,
        )
    }
}

/// Resident set size (physical memory) of this process in bytes, or 0 only if
/// the platform genuinely cannot report it.
///
/// Reads the real per-process figure via [`memory_stats`], which uses
/// `task_info` on macOS and `/proc/self/statm` on Linux. The shell stays within
/// the workspace's `deny(unsafe_code)` because the syscall FFI lives inside that
/// crate. Previously this returned 0 on every non-Linux host, so the HUD's
/// memory gauge read a flat zero on macOS — a signal that looks like evidence
/// and isn't (§12). The [`rss_is_observable`](tests) test guards against a
/// regression back to that.
/// On wasm32 there is no process and no `task_info`, but there **is** a real
/// figure with the same meaning: the module's linear memory, which is the whole of
/// its heap and the only thing it can grow. `memory_size(0)` returns it in 64 KiB
/// pages. That is a genuine measurement rather than a stub — which matters here
/// specifically, because this function's whole history is that returning a flat 0
/// made the gauge look like evidence when it was not (§12), and a browser stub
/// would have reintroduced exactly that.
///
/// It is not identical to native RSS: linear memory is *reserved* address space
/// that the engine has committed, so it never shrinks after a
/// `memory.grow`, whereas RSS can fall. Read it as a high-water mark.
#[must_use]
pub fn process_rss_bytes() -> usize {
    #[cfg(not(target_arch = "wasm32"))]
    {
        memory_stats::memory_stats().map_or(0, |m| m.physical_mem)
    }
    #[cfg(target_arch = "wasm32")]
    {
        /// wasm's page size, fixed by the spec at 64 KiB.
        const WASM_PAGE_BYTES: usize = 65536;
        core::arch::wasm32::memory_size(0) * WASM_PAGE_BYTES
    }
}

/// Bytes per HUD vertex: 2 position floats + 4 colour floats.
const FLOATS_PER_VERTEX: usize = 6;

/// Floats per textured-sprite vertex: position (x, y in NDC), atlas UV (u, v),
/// and an RGBA tint. The GUI sprite stream is separate from the colour stream
/// so the existing colour pipeline is untouched.
pub(crate) const SPRITE_FLOATS_PER_VERTEX: usize = 8;

/// Vanilla's `recipe.toast.title` (`assets/minecraft/lang/en_us.json`, read
/// from the real `client.jar` rather than transcribed from memory — note the
/// parenthesised plural, which a paraphrase loses).
pub const RECIPE_TOAST_TITLE: &str = "New Recipe(s) Unlocked!";
/// Vanilla's `recipe.toast.description`, same source.
pub const RECIPE_TOAST_DESCRIPTION: &str = "Check your recipe book";
/// `RecipeToast.BACKGROUND_SPRITE` (`RecipeToast.java`) — `toast/recipe`,
/// which really is present in 26.2's GUI atlas
/// (`assets/minecraft/textures/gui/sprites/toast/recipe.png`), so the sprite
/// path is reachable rather than permanently falling back.
pub const RECIPE_TOAST_SPRITE: &str = "toast/recipe";
/// `ToastManager`'s slide duration in milliseconds — the bare `600L` at
/// `ToastManager.java` (it has no named constant there).
pub const RECIPE_TOAST_SLIDE_MS: u64 = 600;

/// One recipe-unlock toast to draw this frame, resolved from
/// [`lodestone_game::recipe::RecipeToastQueue::displayed_entry`] by whoever owns
/// the clock (`app.rs`).
///
/// # Geometry, read from the record rather than a call site
///
/// Every number below comes from `Toast.java`/`RecipeToast.java` in
/// `.cache/mc/26.2/client-src`, checked against the **definitions**:
///
/// - `Toast.width() == 160`, `Toast.height() == 32` (`Toast.java`; the
///   `DEFAULT_WIDTH`/`SLOT_HEIGHT` constants at `:14-15` carry the same values).
/// - `xPos(screenWidth, visiblePortion) == screenWidth - width() *
///   visiblePortion` (`Toast.java`). This is **not** a fixed right
///   margin: it is the slide-in, and at `visiblePortion == 1.0` the toast's
///   left edge sits exactly `160` from the right edge of the screen.
/// - `yPos(firstSlotIndex) == firstSlotIndex * height()` (`Toast.java`),
///   so the *first* toast is flush with the top of the screen at `y == 0`, not
///   inset by a margin. We only ever draw one, so `firstSlotIndex == 0`.
/// - Contents (`RecipeToast.extractRenderState`, `RecipeToast.java`), all
///   toast-local: background sprite over the full `160×32`; title at `(30, 7)`
///   colour `-11534256` (`0xFF500050`); description at `(30, 18)` colour
///   `-16777216` (opaque black); the crafting-station icon at `(3, 3)` under a
///   `scale(0.6)` that applies to the *position too*, so it lands at
///   `(1.8, 1.8)` at `9.6px`; the unlocked item's icon at `(8, 8)`, unscaled.
#[derive(Debug, Clone)]
pub struct RecipeToastView {
    /// The crafting station's icon — the small scaled corner item
    /// (`RecipeToast.Entry::categoryItem`, `RecipeToast.java`).
    pub station: HotbarSlot,
    /// The newly unlocked recipe's result icon (`Entry::unlockedItem`).
    pub unlocked: HotbarSlot,
    /// `ToastManager.ToastInstance::visiblePortion` (`ToastManager.java`,
    /// used at `:266`): `1.0` fully on screen, `0.0` entirely off the right
    /// edge. Callers with no animation state should pass `1.0`.
    pub visible_portion: f32,
}

/// `toast/advancement`, the completion toast's background sprite
/// (`AdvancementToast.java`).
pub const ADVANCEMENT_TOAST_SPRITE: &str = "toast/advancement";

/// One advancement-completion toast (issue #167).
///
/// `AdvancementToast.extractRenderState` (`AdvancementToast.java`), the
/// single-title-line branch: the type's own heading at `(30, 7)` in yellow — or
/// `0xFFFF88FF` for a challenge — the advancement's title at `(30, 18)` in white,
/// and its icon at `(8, 8)` unscaled, all over the same `160×32` background
/// [`RecipeToastView`] uses.
///
/// **The multi-line branch is not modelled.** Vanilla alternates between the
/// heading and the wrapped title every 1500 ms when the title does not fit 125 px;
/// every one of 26.2's own 126 titles does fit, so the alternation is unreachable
/// with the shipped data pack and a title longer than that degrades to its first
/// line rather than growing a second animation clock.
#[derive(Debug, Clone)]
pub struct AdvancementToastView {
    /// "Advancement Made!" / "Goal Reached!" / "Challenge Complete!", resolved.
    pub heading: String,
    /// The heading's colour — challenge advancements get their own.
    pub heading_colour: [f32; 4],
    /// The advancement's own title, resolved.
    pub title: String,
    /// Its icon, `None` for an id the atlas key parser rejects.
    pub icon: Option<HotbarSlot>,
    /// See [`RecipeToastView::visible_portion`].
    pub visible_portion: f32,
}

/// Everything the HUD draws for one frame, bundled so the geometry builder and
/// the GPU renderer take one argument that can grow without churning every call
/// site. Borrows so building it per frame allocates nothing beyond the `chat`
/// slice the caller already has.
#[derive(Debug)]
pub struct HudFrame<'a> {
    /// The debug-overlay stats (drawn only when `show_debug`).
    pub stats: &'a DebugStats,
    /// Whether the F3 debug overlay is visible.
    pub show_debug: bool,
    /// Whether to draw the centre aiming reticle. Suppressed while any screen is
    /// up (pause, chat, a container).
    ///
    /// Vanilla is *not* the authority for that suppression — settled, not just
    /// suspected (issue #71). Read directly rather than from memory:
    /// `Hud.extractRenderState` (`Hud.java`, `.cache/mc/26.2/client-src`)
    /// calls `extractCrosshair` whenever the HUD itself is not F1-hidden and the
    /// active screen is not a `LevelLoadingScreen` — there is no
    /// `screen() == null` guard on this call, unlike the sibling
    /// `extractSubtitleOverlay` three lines below it, which does gate on
    /// `screen() == null || screen().isInGameUi()`. And `extractCrosshair` itself
    /// (`Hud.java`) gates only on `options.getCameraType().isFirstPerson()`
    /// and not being in spectator mode (or, in spectator, aiming at a
    /// `MenuProvider` via `canRenderCrosshairForSpectator`). So a vanilla
    /// crosshair stays visible — dimmed only by whatever the screen itself draws
    /// on top of it afterward — behind a pause menu, an inventory, or chat.
    ///
    /// We hide it outright instead, a confirmed divergence. The draw-order half
    /// vanilla relies on already exists on this side, just not wired to the
    /// crosshair: `container.rs`'s dim gradient (issue #61's leftover) draws
    /// *after* the HUD pass and paints over it uniformly, which is exactly what
    /// dims [`Self::hotbar`] for free while a container is open. Matching
    /// vanilla for the crosshair is therefore a gating change in `app.rs`
    /// (`crosshair = self.ui.is_playing()` would need to become something
    /// shaped like [`Self::hotbar`]'s `world_hud`), not a rendering one — but
    /// doing that correctly also needs vanilla's `isFirstPerson()` / spectator /
    /// `canRenderCrosshairForSpectator` gate folded in, or a third-person or
    /// spectator session would grow a crosshair vanilla never draws there. That
    /// is a distinct, larger change than this issue asked for ("settle whether
    /// vanilla hides the crosshair behind a screen", not "make it pixel-exact"),
    /// so behaviour is left as-is; this comment is the settled answer plus the
    /// pointer for whoever picks up the rest.
    ///
    /// **This flag is about the crosshair and nothing else.** It used to double as
    /// the hotbar's gate — one boolean answering two questions — which is exactly
    /// how the hotbar came to vanish behind the pause menu (issue #61). See
    /// [`Self::hotbar`].
    pub crosshair: bool,
    /// Recent chat lines, oldest-first; drawn bottom-left. Each is a legacy
    /// `§`-code string paired with its **age in seconds**, which drives the
    /// vanilla fade-out (older lines dim, then vanish, while the box is closed).
    pub chat: &'a [(&'a str, f32)],
    /// Sound-subtitle captions (issue #198), **oldest first** — vanilla's own
    /// order, with row 0 at the bottom of the stack. Drawn bottom-right, above the
    /// hotbar, and empty whenever `showSubtitles` is off or nothing is audible.
    pub sound_subtitles: &'a [crate::audio::subtitles::SubtitleCaption],
    /// The in-progress chat input line, `Some` only while the chat box is open.
    pub chat_input: Option<&'a str>,
    /// Whether the input line's blinking append-caret is in its "on" phase
    /// this frame; only meaningful while `chat_input` is `Some`. Vanilla
    /// blinks it every 300ms (`TextCursorUtils.CURSOR_BLINK_INTERVAL_MS`,
    /// `.cache/mc/26.2/client-src/net/minecraft/client/gui/components/TextCursorUtils.java`,
    /// `isCursorVisible(millis) == (millis / 300) % 2 == 0`) — the caller
    /// computes this from a wall clock with that same formula so this pure
    /// geometry module owns no clock of its own. Defaults to always-visible
    /// (see [`HudFrame::new`]) so every pre-existing test keeps drawing a
    /// caret without having to know about blinking.
    pub chat_caret_visible: bool,
    /// The grey preview of the highlighted suggestion, drawn straight after the
    /// caret — vanilla's `EditBox.suggestion`, set by
    /// `SuggestionsList.select`. `None` whenever there is no popup, or the
    /// highlighted candidate is not an extension of what is typed.
    pub chat_suggestion_ghost: Option<&'a str>,
    /// The command-suggestion dropdown, `Some` only while it is up. See
    /// [`SuggestionPopup`] and [`draw_command_suggestions`].
    pub chat_suggestions: Option<SuggestionPopup<'a>>,
    /// The persisted Chat Settings values that shape the scrollback/input
    /// draw — see [`ChatDisplayOptions`]. Defaults to vanilla's own defaults
    /// (see [`HudFrame::new`]), so a caller that never sets this renders
    /// exactly as the fields alone would suggest, not as some other implicit
    /// baseline.
    pub chat_options: ChatDisplayOptions,
    /// Where the chat log's wrapped rows are persisted between frames — see
    /// [`ChatWrapCache`]. `None` (every hermetic test, and any caller with no
    /// frame-to-frame state) wraps from scratch, which is correct, just not
    /// free; the running app always supplies one.
    pub chat_wrap: Option<&'a ChatWrapCache>,
    /// The player list to draw, `Some` only while the tab overlay is held.
    ///
    /// **This used to be `Option<&[String]>`** — a flat list of pre-formatted
    /// `"NAME  30ms"` rows — and that was the defect, not the plumbing:
    /// `PLAYER_INFO_UPDATE` was decoded, folded, and reaching pixels the whole
    /// time, so `cargo xtask connectedness` reported the wire green before and
    /// after. What the flattening threw away was the game mode, the styled
    /// display name and the latency *band*, and what it invented was a
    /// `"PLAYERS (n)"` caption vanilla has no equivalent of. Carry
    /// [`crate::tablist::TabListView`] and let the draw do vanilla's layout.
    pub players: Option<&'a crate::tablist::TabListView>,
    /// The scoreboard sidebar to draw on the right edge, `Some` when displayed.
    pub sidebar: Option<&'a Sidebar>,
    /// Active boss bars, drawn stacked at the top-centre in render order.
    pub boss_bars: &'a [BossBarView],
    /// Whether this player can be hurt at all — vanilla's
    /// `MultiPlayerGameMode.canHurtPlayer()`, which is `localPlayerMode.isSurvival()`
    /// and therefore `SURVIVAL || ADVENTURE`. Creative **and spectator** are both
    /// false, which is why this is not a `GameMode::Creative` test: naming the mode
    /// would leave a spectator with a heart row vanilla never draws.
    ///
    /// `Hud.extractHotbarAndDecorations` calls `extractPlayerHealth` only under this
    /// predicate, and that one call draws the *whole* left/right column — the armour
    /// bar, the hearts, the hunger row and the air bubbles. So one flag gates all
    /// four here too, and all four are now present: [`Self::armour`] joined this gate
    /// rather than getting one of its own, which is what this field's own note asked
    /// for while it was the missing fifth of five.
    ///
    /// It also stands in for vanilla's `hasExperience()`, which gates the XP bar and
    /// the level number through `nextContextualInfoState`: in 26.2 both methods have
    /// the identical body (`localPlayerMode.isSurvival()`), so one boolean carries
    /// both. **Split this into two fields if they ever diverge upstream** — the
    /// questions are genuinely different even where today's answers are not.
    ///
    /// Finally it supplies the signal `held_item`'s 14 px shift needed
    /// (`extractSelectedItemName`'s `y += 14` when `!canHurtPlayer()`), because with
    /// no vitals row below it the label drops into the space they vacated.
    ///
    /// Defaults to `true`, so every caller that predates this field — and every
    /// hermetic test that sets `health`/`food`/`xp` directly — draws exactly as it
    /// did before.
    pub can_hurt_player: bool,
    /// Current player health in `0..=20`, `Some` only on a live survival server.
    pub health: Option<f32>,
    /// Armour points in `0..=20` — vanilla's `LivingEntity.getArmorValue()`, which
    /// is `Mth.floor(getAttributeValue(Attributes.ARMOR))` and **not** a per-item
    /// table. `Some` once the local player carries a server-fed attribute snapshot;
    /// `None` off a live server, which draws nothing.
    ///
    /// `Some(0)` is a real state — a live player wearing nothing — and also draws
    /// nothing, because `extractArmor` is wrapped in `if (armor > 0)`: vanilla shows
    /// **no** row at all rather than ten empty icons. Both cases therefore agree, and
    /// the `Option` exists only so a caller that has not wired the attribute through
    /// is distinguishable from one reporting a real zero.
    ///
    /// The scale is 20 points = 10 icons, same as hearts, but the units are armour
    /// points rather than half-hearts: full diamond is exactly 20 and the registry
    /// clamps `minecraft:armor` to `0..=30`, so a value above 20 saturates the row at
    /// ten full icons rather than drawing an eleventh.
    pub armour: Option<i32>,
    /// Current food level in `0..=20`, `Some` only on a live survival server.
    pub food: Option<i32>,
    /// Current food saturation (the hidden reserve that drains before `food`
    /// itself does), `Some` only on a live survival server. Drives the
    /// hunger-row wobble while it is empty (`Hud.java`,
    /// `getSaturationLevel() <= 0.0`) — `None` is treated as "not empty" (the
    /// row stays flush), which is also this field's default, so a caller that
    /// has not wired it through yet (see `docs/hud-animations.md`) draws
    /// exactly as before this field existed rather than guessing at a real
    /// saturation value.
    pub saturation: Option<f32>,
    /// `(air, max_air, eye_in_water)`, `Some` only on a live survival server.
    /// Drives the underwater bubble row (`lodestone_render::bubble_row`) —
    /// `Some` does not by itself mean the row draws; [`bubble_row_visible`]
    /// (full air and not underwater draws nothing, matching vanilla's own
    /// guard) decides that per-frame, same as vanilla never showing bubbles at
    /// full air on dry land.
    ///
    /// [`bubble_row_visible`]: lodestone_render::bubble_row_visible
    pub air: Option<(i32, i32, bool)>,
    /// The selected hotbar slot in `0..9`, `Some` whenever a **world** is on
    /// screen — including behind the pause menu, the chat box and a container.
    /// Drawn as a 9-cell bar at the bottom centre with the selected cell
    /// highlighted.
    ///
    /// This used to say "`Some` while in active play", and the call site agreed
    /// with it, so the hotbar disappeared the moment any screen opened
    /// (issue #61). Vanilla draws the hotbar under `readyForLevelRendering`
    /// (`GameRenderer.java` → `Gui.java`) and gates it on game
    /// mode only (`Hud.java`); the *screen* then paints its translucent
    /// background over the top, and that is the whole of the difference. See
    /// `app::hud_follows_world`, and [`Self::crosshair`] for the one element we
    /// do deliberately hide.
    pub hotbar: Option<usize>,
    /// The nine hotbar item stacks (`0..9`), `Some` on a live server once the
    /// player inventory has been folded. Each slot is `Some(HotbarSlot)` when
    /// occupied. Icons are drawn from the [`ItemAtlas`] supplied to
    /// [`HudRenderer::attach_items`]; without that atlas the wells stay empty.
    pub hotbar_items: Option<&'a [Option<HotbarSlot>]>,
    /// The XP bar `(level, progress 0..=1)`, `Some` once the server has sent
    /// experience. Drawn as a green progress bar above the hotbar with the level
    /// centred above it. Off a live server this is `None` — no bar is drawn.
    pub xp: Option<(i32, f32)>,
    /// The title/subtitle overlay `(title, subtitle, alpha)`, drawn large and
    /// centred with a server-driven fade. `None` when no title is showing.
    ///
    /// Both strings are **styled spans**, and the reason is the same one
    /// [`crate::overlay::Sidebar`] carries: `Sim::title_overlay` used to flatten
    /// through `Text::to_legacy_string`, which can express only the sixteen colours
    /// that have a `§` code. A server's hex title therefore arrived here white, and
    /// no amount of work in the *renderer* could recover it — the loss was one
    /// layer above. Spans keep [`TextColor`] itself, so hex survives to the quad.
    pub title: Option<(Vec<TextSpan>, Option<Vec<TextSpan>>, f32)>,
    /// The action-bar message `(text, alpha)`, drawn just above the hotbar
    /// cluster with a fade. `None` when nothing is showing. Spans rather than a
    /// `String` for the reason [`Self::title`] gives.
    pub action_bar: Option<(Vec<TextSpan>, f32)>,
    /// The held-item name highlight `(name, alpha)` — a `§`-coded, already-
    /// styled string (see `lodestone_game::item::styled_hover_name`) and the
    /// opacity from `lodestone_game::player_state::HeldItemHighlight::alpha`.
    /// `None`/`alpha <= 0.0` draws nothing. Unlike [`Self::action_bar`] and
    /// [`Self::title`], the *timer* this alpha comes from is not
    /// server-driven — it is a purely client-side reaction to the selected
    /// hotbar item's identity changing (issue #126), so a caller populates
    /// this from whatever owns that timer each frame rather than from a
    /// decoded packet.
    ///
    /// Vanilla shifts this label down 14 px when `!canHurtPlayer()`
    /// (creative/spectator have no health/hunger row for it to clear); the signal is
    /// [`Self::can_hurt_player`] and the draw site is in
    /// [`HudGeometry::build_inner`].
    pub held_item: Option<(String, f32)>,
    /// `(recipes, tags)` loaded into the local recipe corpus (see
    /// `crate::resources::load_recipe_book`), appended to the debug overlay as
    /// one extra line when `Some`. `None` before the corpus has loaded or on a
    /// jar-less run — the line is omitted rather than showing a misleading
    /// `0 0`, the same convention [`Self::hotbar_items`] uses for "not yet
    /// known" versus "known empty".
    pub recipe_stats: Option<(usize, usize)>,
    /// `(distance_to_border, warning_distance, warning_strength)` for the
    /// folded world border, appended to the debug overlay as one extra line
    /// when `Some`.
    ///
    /// `None` until the server has actually sent a border packet
    /// (`WorldBorder::initialized`), so an unbounded default border omits the
    /// line rather than drawing a meaningless `2.999e7` — the same
    /// "omit rather than mislead" convention [`Self::recipe_stats`] uses.
    ///
    /// **This is a diagnostic, not the real consumer.** Vanilla's border
    /// warning is a blue tint applied to the vignette in
    /// `Hud.extractVignette` (`Hud.java`), which needs a
    /// multiply-blend `RenderPipelines.VIGNETTE` equivalent and
    /// `misc/vignette.png` — neither of which exists in `lodestone-render`
    /// yet. This line is the same "did the datum actually reach the running
    /// client" signal `recipe_stats` plays for the corpus loader, and the
    /// strength it prints is the *exact* value that overlay will consume.
    pub border_debug: Option<(f64, f64, f32)>,
    /// The player's spawn point, appended to the debug overlay as one extra line
    /// when the server has reported one.
    ///
    /// `None` when `SpawnPoint::is_reported()` is false, which is the honest
    /// distinction the compass needs too — see
    /// [`Sim::spawn_point`](crate::sim::Sim::spawn_point).
    pub spawn_debug: Option<lodestone_model::BlockPos>,
    /// `(map count, the lowest-numbered map's explored fraction)` from
    /// `SessionMaps`, for the F3 overlay.
    ///
    /// **This is the fold's only reader today, and it is deliberately a
    /// diagnostic rather than the map's own picture.** `MAP_ITEM_DATA` decodes
    /// and `MapStore` blits its sub-rectangle patches correctly; what is missing
    /// is the *renderer* — a per-map 128x128 dynamic texture plus the held/framed
    /// quad, which is a texture-and-bind-group job of its own (see
    /// `docs/filled-map-item.md`). Same shape as [`Self::border_debug`] and
    /// [`Self::spawn_debug`], and for the same reason: a fold with no reader at
    /// all cannot be told apart from a fold that never runs.
    pub map_debug: Option<(usize, f32)>,
    /// The attack-cooldown fraction (`0.0..=1.0`, full strength at `1.0`) the
    /// crosshair indicator fills to — `Sim::attack_strength_scale`'s value,
    /// vanilla's `getAttackStrengthScale(0.0F)`. Drawn only while
    /// [`Self::crosshair`] is also set (see that field), and only once the
    /// atlas resolves the two indicator sprites — see the crosshair draw site
    /// in [`HudGeometry::build_inner`] for exactly which vanilla condition is
    /// and is not modelled (no hotbar-style variant, no full-charge "ready"
    /// icon; `docs/combat.md` names the cut). `None` draws nothing, the
    /// pre-#121 behaviour.
    pub attack_cooldown: Option<f32>,
    /// The recipe-unlock toast to draw top-right, `Some` only while
    /// [`lodestone_game::recipe::RecipeToastQueue`] has a live entry (issue
    /// #163). See [`RecipeToastView`] for the geometry and its vanilla
    /// citations.
    ///
    /// **Honestly degraded today**: the queue this comes from is only ever
    /// filled by the `recipe_book_add` decode, which does not exist yet
    /// (`crates/protocol/v770` — tracked on #436), so on a live server this
    /// stays `None`. That is deliberate: the consumer side is wired so the
    /// toast appears the moment the decode lands, and no fake producer was
    /// added to make it light up early.
    pub recipe_toast: Option<RecipeToastView>,
    /// The advancement-completion toast (issue #167), `Some` while one is inside
    /// its 5000 ms window. Drawn in the same top-right slot as
    /// [`Self::recipe_toast`] — vanilla's `ToastManager` stacks them, and this
    /// client only ever has one queue live at a time.
    pub advancement_toast: Option<AdvancementToastView>,
}

impl<'a> HudFrame<'a> {
    /// A frame that draws just the debug overlay and crosshair — the default
    /// single-player / pre-connect HUD, and a concise base for tests.
    #[must_use]
    pub fn new(stats: &'a DebugStats) -> Self {
        Self {
            stats,
            show_debug: true,
            crosshair: true,
            chat: &[],
            sound_subtitles: &[],
            chat_input: None,
            chat_caret_visible: true,
            chat_suggestion_ghost: None,
            chat_suggestions: None,
            chat_options: ChatDisplayOptions::default(),
            chat_wrap: None,
            players: None,
            sidebar: None,
            boss_bars: &[],
            can_hurt_player: true,
            health: None,
            armour: None,
            food: None,
            saturation: None,
            air: None,
            hotbar: None,
            hotbar_items: None,
            xp: None,
            title: None,
            action_bar: None,
            held_item: None,
            recipe_stats: None,
            border_debug: None,
            spawn_debug: None,
            map_debug: None,
            attack_cooldown: None,
            recipe_toast: None,
            advancement_toast: None,
        }
    }
}

/// Vanilla's `MultiPlayerGameMode.canHurtPlayer()`, the predicate
/// [`HudFrame::can_hurt_player`] carries.
///
/// Its body is `localPlayerMode.isSurvival()`, and `GameType.isSurvival()` is
/// `this == SURVIVAL || this == ADVENTURE` — so **both** creative and spectator are
/// false. Naming a mode instead (`mode == Creative`) is the tempting wrong version:
/// it agrees on three of the four values and leaves a spectator with a heart row
/// vanilla never draws.
///
/// `None` — no live connection, or a login whose game mode has not arrived — reads
/// as `true`, matching the pre-connect HUD and [`HudFrame::new`]'s own default.
#[must_use]
pub fn can_hurt_player(mode: Option<lodestone_model::GameMode>) -> bool {
    use lodestone_model::GameMode;
    match mode {
        Some(GameMode::Creative | GameMode::Spectator) => false,
        Some(GameMode::Survival | GameMode::Adventure) | None => true,
    }
}

/// Which of the three armour sprites one of the ten armour-row icons shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmourIcon {
    /// `hud/armor_full` — two whole armour points.
    Full,
    /// `hud/armor_half` — the one odd point at the frontier.
    Half,
    /// `hud/armor_empty` — the dark backing past the frontier.
    Empty,
}

impl ArmourIcon {
    /// The GUI sprite id, so the draw and any gate name the sprite once.
    #[must_use]
    pub fn sprite_id(self) -> &'static str {
        match self {
            Self::Full => "hud/armor_full",
            Self::Half => "hud/armor_half",
            Self::Empty => "hud/armor_empty",
        }
    }
}

/// Vanilla's `Hud.extractArmor` icon choice for icon `i` of ten, at `armour` points.
///
/// Transcribed from the three sibling `if`s rather than restated as arithmetic,
/// because the tempting restatement is wrong and the wrong version agrees with this
/// one on every *even* input:
///
/// ```text
/// if (i * 2 + 1 <  armor) FULL
/// if (i * 2 + 1 == armor) HALF
/// if (i * 2 + 1 >  armor) EMPTY
/// ```
///
/// The frontier is the **odd** threshold `2i + 1`, so at `armour = 15` this yields 7
/// full, 1 half, 2 empty, while the plausible `full = ceil(armour / 2)` reading — or
/// the off-by-one `i * 2 < armour` — yields 8 full and **no half at all**. An even
/// input cannot tell those apart, which is why the gate for this drives odd values.
///
/// `i` past nine and an `armour` above 20 both saturate: vanilla's loop is a fixed
/// `0..10` and the registry clamps `minecraft:armor` to `0..=30`, so a value over 20
/// fills the row rather than growing it.
#[must_use]
pub fn armour_icon(i: usize, armour: i32) -> ArmourIcon {
    let threshold = i as i32 * 2 + 1;
    if threshold < armour {
        ArmourIcon::Full
    } else if threshold == armour {
        ArmourIcon::Half
    } else {
        ArmourIcon::Empty
    }
}

/// Which heart sprite one of the ten heart containers shows **over** its backing,
/// or `None` for a container left empty.
///
/// A separate type from [`ArmourIcon`] because the empty case really is different:
/// an unfilled heart draws the container sprite and nothing on top of it, where an
/// unfilled armour icon draws its own `hud/armor_empty`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeartFill {
    /// `hud/heart/full` — both halves of this container.
    Full,
    /// `hud/heart/half` — the odd half at the frontier.
    Half,
}

impl HeartFill {
    /// The GUI sprite id, so the draw and any gate name the sprite once.
    #[must_use]
    pub fn sprite_id(self) -> &'static str {
        match self {
            Self::Full => "hud/heart/full",
            Self::Half => "hud/heart/half",
        }
    }
}

/// Vanilla's `Hud.extractHearts` fill choice for heart `i` of ten, at `health`
/// **hit points** (not halves) — `None` for a container with nothing drawn over it.
///
/// # The `ceil` is the whole function, and it is why this is a named symbol
///
/// Vanilla never compares the raw float. `extractPlayerHealth` computes
/// `currentHealth = Mth.ceil(player.getHealth())` **once**, hands that `int` to
/// `extractHearts`, and the fill is two integer comparisons against it:
///
/// ```text
/// int halves = containerIndex * 2;
/// if (halves < currentHealth) {
///    boolean halfHeart = halves + 1 == currentHealth;
///    extractHeart(type, …, halfHeart);
/// }
/// ```
///
/// So the composition — ceil, then an integer frontier — is the thing that has to be
/// right, and it had no name here before, which is exactly how the two halves came
/// apart: the ghost-overlay row of the same draw loop already used the
/// `halves + 1 ==` shape against an integer, while the fill row compared
/// `health - 2i` against `2.0`/`1.0` as floats. Both readings agree on every **even**
/// hit point and diverge at every odd half, in both directions:
///
/// | health | vanilla | the float reading |
/// |---|---|---|
/// | 0.5 | `ceil` 1 → one **half** heart | nothing at all — an empty bar while alive |
/// | 1.5 | `ceil` 2 → one **full** heart | a half heart |
/// | 19.5 | `ceil` 20 → **ten full** hearts | nine full and a half |
/// | 2.0, 20.0 | full hearts | identical |
///
/// The first row is the live player report ("sometimes i get to 0 hearts but im still
/// alive"): under the ceiling an empty bar is reachable only at *exactly* 0, which is
/// death. Any gate written at an integer health measures only that this function runs.
///
/// `health` is clamped at zero rather than trusted, because `hurt` overshoot can
/// report a small negative and `Mth.ceil` of that would light a heart.
#[must_use]
pub fn heart_fill(i: usize, health: f32) -> Option<HeartFill> {
    let current = health.max(0.0).ceil() as i32;
    let halves = i as i32 * 2;
    if halves >= current {
        return None;
    }
    Some(if halves + 1 == current {
        HeartFill::Half
    } else {
        HeartFill::Full
    })
}

/// Vanilla's Chat Settings (plus one Accessibility-screen field it shares)
/// values that shape how the scrollback and input line draw —
/// `net.minecraft.client.Options`'s `chat*` fields
/// (`.cache/mc/26.2/client-src/net/minecraft/client/Options.java`).
/// `Copy` for the same reason [`crate::config::Options`] is: cheap to read
/// once per frame with no borrow to fight.
///
/// Deliberately **not** every vanilla chat option: `chatVisibility` (System/
/// Hidden filtering), `chatColors`' link-adjacent siblings `chatLinks`/
/// `chatLinksPrompt`, and `chatDelay` all live upstream of this draw layer —
/// the first needs a per-line message-source tag `ChatLog::recent` currently
/// flattens away, the other three need click/rate-limit plumbing this HUD has
/// none of. Landing an option field with no reader is the exact defect this
/// repo's own `CLAUDE.md` calls the dominant one, so those stay out until
/// something upstream can actually consume them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChatDisplayOptions {
    /// `options.chat.scale` (`Options.java`), `0.0..=1.0` — vanilla's
    /// `ChatComponent.getScale`. This is the *entire* pose scale every chat
    /// draw multiplies by ([`chat_pose_scale`]); there is no HUD-side factor
    /// layered on top, matching `ChatComponent.extractRenderState`'s
    /// `pose.scale(scale, scale)`.
    pub scale: f32,
    /// `options.chat.width` (`Options.java`), `0.0..=1.0`. Fed
    /// through [`chat_width_px`] (vanilla's `ChatComponent.getWidth`,
    /// `ChatComponent.java`) to size the chat box.
    pub width_pct: f32,
    /// `options.chat.height.unfocused` (`Options.java`), `0.0..=1.0`
    /// — box height while the chat box is **closed**.
    pub height_pct_unfocused: f32,
    /// `options.chat.height.focused` (`Options.java`), `0.0..=1.0` —
    /// box height while the chat box is **open**.
    pub height_pct_focused: f32,
    /// `options.chat.line_spacing` (`Options.java`), `0.0..=1.0`:
    /// extra fraction of a line's height inserted between chat rows
    /// (`ChatComponent.java`, `entryHeight = messageHeight * (spacing +
    /// 1.0)`).
    pub line_spacing: f32,
    /// `options.chat.opacity` (`Options.java`), `0.0..=1.0`. Text
    /// alpha is `text_opacity * 0.9 + 0.1` (`ChatComponent.java`) — never
    /// fully transparent, matching vanilla.
    pub text_opacity: f32,
    /// `options.accessibility.text_background_opacity` (`Options.java`),
    /// `0.0..=1.0`. Used directly as the per-line background fill alpha
    /// (`ChatComponent.java`).
    pub background_opacity: f32,
    /// `options.chat.color` (`Options.java`). `false` strips every
    /// legacy `§` code before drawing a scrollback line
    /// (`ComponentRenderUtils.stripColor`, `ComponentRenderUtils.java`) —
    /// it never touches the input line, which cannot carry codes
    /// ([`crate::chat::ChatInput::push_char`] filters `§` on the way in).
    pub colors: bool,
}

impl Default for ChatDisplayOptions {
    fn default() -> Self {
        Self {
            scale: 1.0,
            width_pct: 1.0,
            height_pct_unfocused: 70.0 / 160.0,
            height_pct_focused: 1.0,
            line_spacing: 0.0,
            text_opacity: 1.0,
            background_opacity: 0.5,
            colors: true,
        }
    }
}

/// Vanilla's `ChatComponent.getWidth` (`ChatComponent.java`): maps the
/// `0.0..=1.0` `chatWidth` option onto `40.0..=320.0` **screen** pixels — the
/// same logical-canvas unit [`crate::menu::render::logical_canvas`] returns
/// (see [`HudGeometry::build_inner`]'s own doc on why that canvas *is*
/// vanilla's `guiScaledWidth`/`Height`), so this is directly comparable to
/// `Builder::w` with no further conversion.
#[must_use]
pub fn chat_width_px(pct: f32) -> f32 {
    (pct * 280.0 + 40.0).floor()
}

/// As [`chat_width_px`], vanilla's `ChatComponent.getHeight`
/// (`ChatComponent.java`): maps `0.0..=1.0` onto `20.0..=180.0` screen
/// pixels.
#[must_use]
pub fn chat_height_px(pct: f32) -> f32 {
    (pct * 160.0 + 20.0).floor()
}

/// The scale factor every chat draw — scrollback, input line, suggestion popup
/// — multiplies its geometry by: vanilla's `chatScale` option alone
/// (`ChatComponent.getScale`, `ChatComponent.java`), matching
/// `ChatComponent.extractRenderState`'s `pose.scale(scale, scale)` with no
/// further HUD-side factor.
///
/// A free function rather than a local `let` because
/// [`suggestion_layout`] is called from outside [`HudGeometry::build_inner`]
/// (the pointer hit-test) and the two must resolve the same number.
#[must_use]
pub fn chat_pose_scale(opts: ChatDisplayOptions) -> f32 {
    opts.scale.max(0.0)
}

/// The top of the chat input line's glyph row, in logical-canvas pixels —
/// vanilla's `EditBox` at `this.height - 12`.
///
/// Shared by the input draw and [`suggestion_layout`] for the reason that whole
/// function exists: the popup is placed *relative to this line*, so a second
/// spelling of it would let the two drift apart by exactly the amount nobody
/// notices in a screenshot.
#[must_use]
pub fn chat_input_top(canvas_h: f32, pose_scale: f32) -> f32 {
    canvas_h - HUD_MARGIN - font::GLYPH_H as f32 * pose_scale
}

/// The scrollback's own anchor — vanilla's `ChatComponent.extractRenderState`
/// (`ChatComponent.java`): `final int chatBottom = Mth.floor((screenHeight -
/// 40) / scale);`, computed in the pose's *local* (unscaled-by-chat-scale)
/// coordinates and then carried back to screen/canvas pixels by the very
/// `pose.scale(scale, scale)` that local value is drawn under — so the real
/// canvas-pixel anchor is `floor((canvas_h - 40) / scale) * scale`. Every
/// message row's bottom edge is `chatBottom - lineIndex * entryHeight`
/// (`lineIndex == 0` for the newest), so this is where the newest row's
/// bottom edge lands.
///
/// **Independent of the input box.** `extractRenderState` computes this one
/// expression before it ever branches on `displayMode.foreground`, and the
/// `EditBox` (`this.height - 12`, `ChatScreen.init`) is a wholly separate
/// literal in a different class — vanilla never derives one from the other.
/// So this takes `canvas_h` and the chat scale only, not [`chat_input_top`]:
/// coupling the two (as this HUD used to, computing `chat_bottom` from
/// `input_y` while the box was open) is what silently made the vanilla gap
/// disappear, since that coupling forces the newest row flush against the
/// input strip's own top edge regardless of what `40` says. Used unconditionally,
/// open or closed, for the same reason — vanilla's `chatBottom` does not
/// change with `displayMode` either.
///
/// At the vanilla-default `chatScale` of `1.0` this is simply `canvas_h -
/// 40.0`: a fixed, real 40 logical-canvas-pixel headroom above wherever the
/// input box happens to sit, not a restated `0`.
#[must_use]
pub fn chat_bottom(canvas_h: f32, pose_scale: f32) -> f32 {
    if pose_scale <= 0.0 {
        return canvas_h - 40.0;
    }
    ((canvas_h - 40.0) / pose_scale).floor() * pose_scale
}

/// Vanilla's `CommandSuggestions.LINE_HEIGHT` is `12`, decomposed: the 9px font
/// draws at `rect.getY() + 2 + 12 * i`, so the row is 2px of lead, the glyph,
/// and 1px of trail. Ours keeps the padding and substitutes this HUD's own glyph
/// height, rather than restating `12` against a 7px font.
const SUGGESTION_ROW_PAD_TOP: f32 = 2.0;
/// See [`SUGGESTION_ROW_PAD_TOP`] — `12 - 2 - 9`.
const SUGGESTION_ROW_PAD_BOTTOM: f32 = 1.0;
/// The gap between the popup's bottom edge and the input line —
/// `SuggestionsList`'s `y - 3 - rows * 12` when `anchorToBottom` is set, which
/// `ChatScreen.init` does.
const SUGGESTION_LIST_GAP: f32 = 3.0;
/// The 1px left inset the row text draws at (`rect.getX() + 1`), which is also
/// why the rect is `maxWidth + 1` wide and starts one pixel left of the anchor
/// (`listX = x - 1` for an unbordered `EditBox`, and `ChatScreen` sets
/// `setBordered(false)`).
const SUGGESTION_TEXT_INSET: f32 = 1.0;

/// `CommandSuggestions.fillColor` as `ChatScreen.init` passes it:
/// `-805306368` == `0xD0000000`.
const SUGGESTION_FILL: [f32; 4] = [0.0, 0.0, 0.0, 208.0 / 255.0];
/// The highlighted row's text colour — `-256` == `0xFFFFFF00`.
const SUGGESTION_TEXT_SELECTED: [f32; 4] = [1.0, 1.0, 0.0, 1.0];
/// Every other row's text colour — `-5592406` == `0xFFAAAAAA`.
const SUGGESTION_TEXT_UNSELECTED: [f32; 4] = [170.0 / 255.0, 170.0 / 255.0, 170.0 / 255.0, 1.0];
/// `EditBox.extractRenderState`'s ghost-suffix colour — `-8355712` ==
/// `0xFF808080`, drawn at `cursorX - 1`.
const SUGGESTION_GHOST: [f32; 4] = [0.5019608, 0.5019608, 0.5019608, 1.0];

/// Pixel width of `s` at `scale`, in whichever font is attached — the real
/// vanilla proportional advances when there is one, the fixed 5×7 debug advance
/// otherwise.
///
/// Free-standing rather than a `Builder` method so the pointer hit-test can
/// measure identically to the draw ([`HudRenderer::suggestion_layout`]);
/// `Builder::text_width` is a thin wrapper over it, which is what makes that
/// identity structural rather than two copies of one `match`.
fn measure_text(font: Option<&VanillaFont>, s: &str, scale: f32) -> f32 {
    match font {
        Some(f) => f.width(s, scale),
        None => item_icon::text_w(s, scale),
    }
}

/// What the suggestion popup needs from `chat::SuggestionsList` to lay itself
/// out and draw. Built by the caller each frame; `None` on [`HudFrame`] is "no
/// popup", which is the only gate the draw has.
#[derive(Debug, Clone, Copy)]
pub struct SuggestionPopup<'a> {
    /// The input line the candidates replace a tail of — needed because the
    /// popup's x anchor is the pixel `line[..start]` ends at, vanilla's
    /// `input.getScreenX(suggestions.getRange().getStart())`.
    pub line: &'a str,
    /// Byte offset into [`Self::line`] the candidate text replaces from.
    pub start: usize,
    /// Every candidate, in row order.
    pub candidates: &'a [crate::chat::Candidate],
    /// The highlighted row's index into [`Self::candidates`].
    pub selected: usize,
    /// The first *visible* row's index into [`Self::candidates`].
    pub offset: usize,
    /// The pointer, in logical-canvas pixels, when it is over the window.
    ///
    /// Used for the tooltip only. Hover *selection* is a state change and
    /// belongs to the event loop, which resolves the row through
    /// [`SuggestionLayout::row_at`] against this same layout.
    pub cursor: Option<(f32, f32)>,
}

/// The popup's resolved rect — vanilla's `SuggestionsList.rect`, plus the row
/// pitch a hit-test needs.
///
/// Vanilla computes this **once**, in `showSuggestions`, and every later mouse
/// event tests the stored value. Here it is recomputed from the same expression
/// the draw uses, which is strictly closer to the pixels: a resize between the
/// show and the click cannot leave the hit-test aiming at a stale rect.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SuggestionLayout {
    /// Left edge.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Width, including the 1px text inset.
    pub w: f32,
    /// Height — `rows * row_h`.
    pub h: f32,
    /// One row's pitch, vanilla's `LINE_HEIGHT` at this frame's chat scale.
    pub row_h: f32,
    /// How many rows are visible — `min(candidates, SUGGESTION_LINE_LIMIT)`.
    pub rows: usize,
}

impl SuggestionLayout {
    /// Whether `(x, y)` is inside the rect — `Rect2i.contains`.
    #[must_use]
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    /// The candidate index under `(x, y)`, or `None` outside the rect —
    /// `SuggestionsList.mouseClicked`'s `(y - rect.getY()) / 12 + offset`,
    /// guarded by that method's own `line < suggestionList.size()`.
    ///
    /// `offset` is the caller's, not stored here, because the layout is rebuilt
    /// every frame while the window position is list state.
    ///
    /// **One named narrowing.** Vanilla's *hover* test is one pixel stricter
    /// than its *click* test on every edge (`mouseX > rect.getX()` versus
    /// `Rect2i.contains`'s `>=`), so in vanilla the outermost pixel ring
    /// click-selects but does not hover-select. Both go through this method
    /// here; the difference is one pixel of hover on a row you can still click.
    #[must_use]
    pub fn row_at(&self, x: f32, y: f32, offset: usize, candidates: usize) -> Option<usize> {
        if !self.contains(x, y) || self.row_h <= 0.0 {
            return None;
        }
        let row = ((y - self.y) / self.row_h).floor().max(0.0) as usize + offset;
        (row < candidates).then_some(row)
    }
}

/// Lay the popup out — `CommandSuggestions.showSuggestions` plus
/// `SuggestionsList`'s constructor, against this HUD's own chat geometry.
///
/// `text_width` measures at the frame's chat pose scale, i.e. it is exactly what
/// the draw will use to place glyphs; passing anything else is how a rect ends
/// up describing a box the text does not fit.
///
/// `canvas_w`/`canvas_h` are logical-canvas pixels — the
/// `crate::menu::render::logical_canvas` space vanilla calls
/// `guiScaledWidth`/`guiScaledHeight`.
#[must_use]
pub fn suggestion_layout(
    canvas_w: f32,
    canvas_h: f32,
    pose_scale: f32,
    popup: &SuggestionPopup<'_>,
    text_width: impl Fn(&str) -> f32,
) -> SuggestionLayout {
    let rows = popup.candidates.len().min(crate::chat::SUGGESTION_LINE_LIMIT);
    let max_w = popup
        .candidates
        .iter()
        .map(|c| text_width(&c.text))
        .fold(0.0_f32, f32::max);
    let row_h = (SUGGESTION_ROW_PAD_TOP + font::GLYPH_H as f32 + SUGGESTION_ROW_PAD_BOTTOM)
        * pose_scale;
    // `input.getScreenX(range.getStart())` — the pixel the replaced span starts
    // at, measured through the *same* metrics the line was drawn with. The
    // `min(start, len)` is defensive against a server-supplied `start`; the
    // char-boundary case cannot reach here because `ChatCompletion::show`
    // rejects it.
    let head_end = popup.start.min(popup.line.len());
    let anchor = HUD_MARGIN + text_width(popup.line.get(..head_end).unwrap_or(""));
    // Vanilla clamps to `0 ..= getScreenX(0) + innerWidth - maxWidth`, and for
    // the chat box that collapses to `screenWidth - maxWidth`: the input is at
    // x=4 with `innerWidth == width - 4`. So this is "do not run off the right
    // edge", not a chat-box-width clamp — the popup is a `Screen` widget and is
    // not bound by the `chatWidth` option.
    let x = anchor.clamp(0.0, (canvas_w - max_w).max(0.0)) - SUGGESTION_TEXT_INSET * pose_scale;
    let bottom = chat_input_top(canvas_h, pose_scale) - SUGGESTION_LIST_GAP * pose_scale;
    SuggestionLayout {
        x,
        y: bottom - rows as f32 * row_h,
        w: max_w + SUGGESTION_TEXT_INSET * pose_scale,
        h: rows as f32 * row_h,
        row_h,
        rows,
    }
}

/// Builds the HUD vertex stream (positions in NDC, RGBA per vertex) for a given
/// viewport. Pure, so it is unit-testable without a GPU.
#[derive(Debug)]
pub struct HudGeometry {
    /// Flat `[x, y, r, g, b, a]` per vertex.
    pub verts: Vec<f32>,
    /// Flat `[x, y, u, v, r, g, b, a]` per textured GUI-sprite vertex. Empty
    /// unless a [`GuiAtlas`] was supplied to [`HudGeometry::build_with_gui`].
    pub sprite_verts: Vec<f32>,
    /// Flat `[x, y, u, v, r, g, b, a]` per textured **item**-sprite vertex, drawn
    /// from the separate [`ItemAtlas`] texture. Empty unless an item atlas and
    /// [`HudFrame::hotbar_items`] were both supplied.
    pub item_verts: Vec<f32>,
    /// The **enchantment-glint** copies of [`item_verts`](Self::item_verts):
    /// one quad per flat sprite layer of every enchanted stack, same rect and
    /// same atlas UVs, drawn on its own pipeline over the icon (issue #452).
    /// Empty when nothing on screen is enchanted.
    pub glint_verts: Vec<f32>,
    /// The 3-D **block-item** icons: baked model geometry already posed into GUI
    /// pixel space on the CPU, in the wide [`ModelVertex`] format the shared
    /// [`ModelPipeline`] consumes. Non-indexed (six vertices per quad, expanded
    /// from the mesh's indices) to match the other two streams' `draw(0..n)`.
    ///
    /// Pre-multiplying the pose here is what collapses the whole hotbar to **one
    /// buffer and one draw**: the GUI path has to emit vertices anyway, so
    /// transforming them costs nothing over uploading them untransformed and
    /// paying a per-slot uniform + draw call. Empty unless a [`BlockModels`] was
    /// supplied and at least one slot holds an item with 3-D geometry.
    pub model_verts: Vec<ModelVertex>,
    /// The **special-renderer** icons (chests and the rest of the ex-
    /// `builtin/entity` family): not vertices, but which baked block-entity mesh
    /// and sheet to draw and the GUI-space placement to draw it under. The meshes
    /// are resident from attach time, so a slot costs a handful of matrices.
    ///
    /// `pub(crate)` rather than `pub` because [`SpecialIconDraw`] is: this is an
    /// internal hand-off to [`IconRenderer::upload`], not part of the geometry a
    /// caller inspects, and nothing outside the crate constructs a
    /// [`HudGeometry`].
    pub(crate) special: Vec<SpecialIconDraw>,
}

impl HudGeometry {
    /// Number of vertices.
    #[must_use]
    pub fn vertex_count(&self) -> usize {
        self.verts.len() / FLOATS_PER_VERTEX
    }

    /// Number of textured GUI-sprite vertices.
    #[must_use]
    pub fn sprite_vertex_count(&self) -> usize {
        self.sprite_verts.len() / SPRITE_FLOATS_PER_VERTEX
    }

    /// Number of textured item-sprite vertices.
    #[must_use]
    pub fn item_vertex_count(&self) -> usize {
        self.item_verts.len() / SPRITE_FLOATS_PER_VERTEX
    }

    /// Number of 3-D item-model vertices (three per triangle, six per quad).
    #[must_use]
    pub fn model_vertex_count(&self) -> usize {
        self.model_verts.len()
    }

    /// Build the whole HUD for `width`×`height` pixels from a [`HudFrame`],
    /// drawing the survival vitals (hotbar, XP, hearts, hunger) as procedural
    /// quads. This is the jar-less / headless path.
    #[must_use]
    pub fn build(frame: &HudFrame, width: u32, height: u32) -> Self {
        Self::build_inner(
            frame,
            width,
            height,
            crate::config::AUTO_GUI_SCALE,
            None,
            None,
            None,
            None,
            HudAnim::NONE,
        )
    }

    /// Like [`build`](Self::build), but with vanilla text: proportional advances
    /// and the drop shadow, from the real `ascii.png`. Everything else is
    /// identical.
    ///
    /// Kept separate from [`build`](Self::build) deliberately — `build` must stay
    /// jar-free and byte-deterministic, because it is what the geometry unit
    /// tests and the jar-less fallback path use.
    #[must_use]
    pub fn build_with_font(
        frame: &HudFrame,
        width: u32,
        height: u32,
        font: &VanillaFont,
    ) -> Self {
        Self::build_inner(
            frame,
            width,
            height,
            crate::config::AUTO_GUI_SCALE,
            None,
            None,
            None,
            Some(font),
            HudAnim::NONE,
        )
    }

    /// Like [`build`](Self::build), but draws the survival vitals from the real
    /// vanilla GUI atlas (hearts, hunger, XP bar, hotbar frame + selection)
    /// instead of procedural quads. Everything else (debug text, chat, sidebar,
    /// crosshair, …) is identical and still emitted to the colour stream.
    #[must_use]
    pub fn build_with_gui(frame: &HudFrame, width: u32, height: u32, gui: &GuiAtlas) -> Self {
        Self::build_inner(
            frame,
            width,
            height,
            crate::config::AUTO_GUI_SCALE,
            Some(gui),
            None,
            None,
            None,
            HudAnim::NONE,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build_inner(
        frame: &HudFrame,
        width: u32,
        height: u32,
        gui_scale: u32,
        gui: Option<&GuiAtlas>,
        items: Option<&ItemAtlas>,
        models: Option<&BlockModels>,
        font: Option<&VanillaFont>,
        anim: HudAnim,
    ) -> Self {
        // `width`/`height` are the **physical** framebuffer, straight from
        // `winit::inner_size()` — already DPI-scaled, exactly what
        // `crate::menu::render::logical_canvas` expects. Dividing it down to the
        // logical canvas here, and laying every fixed pixel constant below into
        // that smaller space, is the whole fix for "the HUD draws at half size on
        // a Retina display": the constants themselves never change, only the
        // canvas they are laid into. Reuses the exact helper `menu/render.rs`
        // already uses for the menu screens, rather than a second scale
        // computation that could disagree with it. `gui_scale` is the resolved
        // `Options.gui_scale`, threaded in by the caller — `build`/`build_with_font`/
        // `build_with_gui` above pass `AUTO_GUI_SCALE` explicitly since they are
        // the jar-less/headless/test paths, which have no persisted option to
        // read; `render_with_item_models` (the real windowed path) passes the
        // live value from `menu::nav::MenuNav::gui_scale()` via `app.rs`.
        let (w, h) = crate::menu::render::logical_canvas(gui_scale, width, height);
        let mut b = Builder::new(w, h, gui, items, models, font);

        let margin = HUD_MARGIN;
        let glyph_h = font::GLYPH_H as f32;

        // The F3 overlay, in vanilla's **two columns**: player and world on the
        // left, engine internals on the right, each line sitting on its own
        // translucent plate. `DebugStats::left_lines` records why each block of
        // lines sits in the column it does.
        //
        // The right column is right-aligned at `w - margin - text_width(line)`,
        // which is vanilla's `guiWidth() - 2 - font.width(line)`
        // (`DebugScreenOverlay.extractLines`), so a long line grows leftwards
        // instead of off the screen. The width has to come from `b.text_width`,
        // the same measure the draw itself uses — a restated constant would
        // misalign the moment the vanilla font is or is not loaded.
        //
        // **Vanilla's own metrics, not an ad-hoc HUD-wide one.** The overlay
        // used to draw at double vanilla's size, which is exactly the mistake
        // the XP level number's own comment records one screen over:
        // this function already draws in the `gui_scale`-divided logical canvas,
        // so a ×2 on the text made it twice vanilla's size relative to
        // everything around it. `DebugScreenOverlay` draws at scale 1 with
        // `MARGIN_LEFT == MARGIN_RIGHT == MARGIN_TOP == 2` and a line height of
        // `9` — see [`DEBUG_MARGIN`] and [`DEBUG_LINE_H`].
        let debug_scale = 1.0;
        let debug_margin = DEBUG_MARGIN;
        let debug_line_h = DEBUG_LINE_H;
        if frame.show_debug {
            let mut left = frame.stats.left_lines();
            let mut right = frame.stats.right_lines();
            // The four conditional diagnostics live on the frame rather than on
            // `DebugStats`, so they cannot be part of either column function.
            // Each opens with a spacer so it reads as its own `addToGroup` block
            // instead of running on from the group above, and each is worded in
            // the overlay's `Key: value` style rather than the `k=v` shorthand
            // they used to carry — the whole point of this pass is that one
            // screen does not mix two conventions.
            let mut left_group_open = false;
            let mut right_group_open = false;
            let open = |lines: &mut Vec<String>, opened: &mut bool| {
                if !*opened {
                    lines.push(String::new());
                    *opened = true;
                }
            };
            if let Some((recipes, tags)) = frame.recipe_stats {
                open(&mut right, &mut right_group_open);
                right.push(format!("Recipes: {recipes}, tags: {tags}"));
            }
            if let Some((dist, warn_at, strength)) = frame.border_debug {
                open(&mut right, &mut right_group_open);
                right.push(format!(
                    "Border: {dist:.1} away, warns at {warn_at:.1} ({strength:.2})"
                ));
            }
            if let Some((count, explored)) = frame.map_debug {
                open(&mut right, &mut right_group_open);
                right.push(format!(
                    "Maps: {count}, {:.0}% explored",
                    explored * 100.0
                ));
            }
            if let Some(spawn) = frame.spawn_debug {
                open(&mut left, &mut left_group_open);
                left.push(format!("Spawn: {} {} {}", spawn.x, spawn.y, spawn.z));
            }
            // Vanilla fills a plate behind every non-empty line *before* drawing
            // any text (`extractLines` does two passes for exactly this reason),
            // so a later line's plate cannot cover an earlier line's glyphs.
            for (column, lines) in [(false, &left), (true, &right)] {
                for (i, line) in lines.iter().enumerate() {
                    if line.is_empty() {
                        continue;
                    }
                    let tw = b.text_width(line, debug_scale);
                    let x = if column {
                        w - debug_margin - tw
                    } else {
                        debug_margin
                    };
                    let y = debug_margin + i as f32 * debug_line_h;
                    b.rect_px(x - 1.0, y - 1.0, tw + 2.0, debug_line_h, DEBUG_LINE_BG);
                }
            }
            for (column, lines) in [(false, &left), (true, &right)] {
                for (i, line) in lines.iter().enumerate() {
                    if line.is_empty() {
                        continue;
                    }
                    let x = if column {
                        w - debug_margin - b.text_width(line, debug_scale)
                    } else {
                        debug_margin
                    };
                    let y = debug_margin + i as f32 * debug_line_h;
                    b.text(line, x, y, debug_scale, DEBUG_LINE_INK);
                }
            }
        }

        // Chat, bottom-left: an optional input line at the very bottom, with the
        // received log stacked above it. Received lines carry legacy `§` colour
        // codes (rendered as coloured runs) and fade out with age like vanilla
        // once the box is closed; while it's open, the full history stays lit.
        //
        // `opts` is [`ChatDisplayOptions`] — see that type for the vanilla
        // field each knob reproduces. `chat_pose_scale` is vanilla's own
        // `chatScale` option alone, exactly as `ChatComponent.extractRenderState`
        // applies it (`pose.scale(scale, scale)`, `ChatComponent.java`) — no
        // further HUD-side factor. Called through the free function rather than
        // recomputed inline so this draw and [`HudRenderer::suggestion_layout`]'s
        // pointer hit-test (called from outside this function) cannot resolve
        // two different numbers from two copies of the same formula.
        let chat_open = frame.chat_input.is_some();
        let opts = frame.chat_options;
        let chat_pose_scale = chat_pose_scale(opts);
        // Vanilla's unscaled per-line stride is 9px
        // (`ChatComponent.MESSAGE_BOTTOM_TO_MESSAGE_TOP`/`messageHeight`,
        // `ChatComponent.java`); `glyph_h + 2.0` is this HUD's own 5×7
        // analogue. `entryHeight = messageHeight * (lineSpacing + 1.0)`
        // (`ChatComponent.java`) is computed *before* the pose scale is
        // applied, so line-spacing multiplies the base stride and
        // `chat_pose_scale` multiplies the whole result, matching that order.
        let chat_line_h = (glyph_h + 2.0) * (1.0 + opts.line_spacing.max(0.0)) * chat_pose_scale;
        // `chat_width_px`/`chat_height_px` are vanilla's own
        // `ChatComponent.getWidth`/`getHeight` formulas, in the same
        // logical-canvas pixel unit as `b.w`/`b.h` (see their doc comments),
        // so no further conversion is needed to compare them against `b.w`.
        let chat_box_w = chat_width_px(opts.width_pct.clamp(0.0, 1.0)).min(b.w);
        let chat_height_pct = if chat_open {
            opts.height_pct_focused
        } else {
            opts.height_pct_unfocused
        };
        let chat_box_h = chat_height_px(chat_height_pct.clamp(0.0, 1.0));
        // `textOpacity = chatOpacity * 0.9 + 0.1` (`ChatComponent.java`) —
        // never fully transparent even at `chatOpacity == 0.0`.
        let chat_text_opacity = opts.text_opacity.clamp(0.0, 1.0).mul_add(0.9, 0.1);
        let chat_bg_opacity = opts.background_opacity.clamp(0.0, 1.0);
        // Through [`chat_input_top`], not a second `b.h - margin - …`, because
        // `suggestion_layout` places the dropdown relative to this row and is
        // called from outside this function too (the pointer hit-test).
        let input_y = chat_input_top(b.h, chat_pose_scale);
        if let Some(input) = frame.chat_input {
            // A translucent strip so text stays legible over bright terrain.
            // Vanilla's real `EditBox` has no equivalent knob of its own; this
            // reuses `chat_bg_opacity` rather than inventing an unread
            // constant, since it is the same "background behind chat text"
            // concept as the scrollback rows just below.
            // Derived from the *same* `input_y` and `chat_pose_scale` the text
            // draw below uses, so the strip and the glyphs cannot disagree.
            // Vanilla's band is `fill(2, height - 14, width - 2, height - 2, …)`
            // (`ChatScreen.java`) with the `EditBox`'s text at `height - 12`
            // (`:56`) — i.e. symmetric 2px padding around the text — so the
            // padding is 2 units, scaled with everything else.
            //
            // The previous version was `input_y - 3.0` tall by `chat_line_h`, and
            // was wrong twice: the `-3.0` was **unscaled** while the height was
            // scaled, so the band drifted off the text as chat scale rose; and it
            // began *above* `input_y`, which is where the scrollback's own
            // translucent rows end, so the two blacks overlapped and that seam
            // rendered at double opacity while the last rows of the glyph box had
            // no background at all.
            b.rect_px(
                0.0,
                input_y - INPUT_STRIP_PAD * chat_pose_scale,
                chat_box_w,
                glyph_h * chat_pose_scale + 2.0 * INPUT_STRIP_PAD * chat_pose_scale,
                [0.0, 0.0, 0.0, chat_bg_opacity],
            );
            // No leading `>` — vanilla's `ChatScreen`/`EditBox` draws no
            // prompt glyph at all, just the typed text and a caret. A
            // trailing underscore stands in for vanilla's append-caret
            // (`TextCursorUtils.extractAppendCursor`,
            // `TextCursorUtils.java`, drawn because the shell's
            // `ChatInput` only ever edits at the end of the line, vanilla's
            // "cursor at end" case); `chat_caret_visible` blinks it at
            // vanilla's real 300ms rate (see [`HudFrame::chat_caret_visible`]).
            // The typed line itself is always plain (input filters `§`), so a
            // flat, non-legacy draw is right, and at **full** opacity — vanilla
            // never multiplies the input `EditBox`'s own text by `chatOpacity`,
            // which only governs the scrollback below.
            b.text(input, margin, input_y, chat_pose_scale, [1.0, 1.0, 1.0, 1.0]);
            // The highlighted suggestion, previewed in grey **behind** the
            // caret — `EditBox.extractRenderState`'s
            // `graphics.text(font, suggestion, cursorX - 1, textY, -8355712,
            // this.textShadow)`. Three things a previous fix here got wrong,
            // all because it measured the pen against `{input}_` or against
            // `font.width(value)` alone instead of vanilla's real `cursorX`:
            //
            // 1. **`cursorX` is `font.width(value)` — the typed text alone,
            //    not the text plus the caret glyph.** The caret contributes
            //    *no* advance in vanilla, because there it is a separately
            //    blinking overlay rectangle, never part of the measured
            //    string. Reserving the caret glyph's own width (`{input}_`)
            //    landed the ghost one whole underscore-width too far right,
            //    *permanently* — stable, and wrong: the owner's own report
            //    was "it's supposed to be behind [the caret], not pushing it
            //    to the right".
            // 2. **`cursorX` is `font.width(value) + 1`, not `font.width(value)`
            //    alone.** `EditBox.extractWidgetRenderState` computes
            //    `drawX += this.font.width(charSequence) + 1;` *before* setting
            //    `cursorX = drawX` — a full pixel reserved after the typed text,
            //    present in the **appended** (non-insert) case this shell always
            //    hits (see the `!insert` note below). This crate's own menu
            //    `EditBox` port already carries this exact `+ 1.0`
            //    (`crates/lodestone-shell/src/menu/edit_box.rs`'s
            //    `draw_state_with`); this draw site had not. Missing it made the
            //    ghost (`cursorX - 1`) land one pixel *short* of vanilla's real
            //    position — flush against, and in practice overlapping, the
            //    typed text's last glyph — and made the caret (`cursorX`) sit
            //    flush against the text instead of the one clear pixel vanilla
            //    leaves for it. The two errors netted to the same wrong ghost
            //    position from two different mistakes: reserving the caret's
            //    width (error 1, since fixed) versus never reserving vanilla's
            //    own one-pixel gap (error 2, fixed here).
            // 3. **The suggestion must be drawn *before* the caret, not
            //    after**, so the caret glyph composites on top and the two
            //    overlap by design (`EditBox.java`'s render order is text →
            //    hint → suggestion → highlight → cursor). The previous fix
            //    drew `{input}{caret}` as one string, then the ghost after —
            //    on top of the caret, backwards from vanilla either way.
            //
            // `!insert` gate: vanilla's `insert = cursorPos < value.length() ||
            // value.length() >= maxLength`. This shell's `ChatInput` only ever
            // edits at the end of the line (see the caret comment above), so
            // the first disjunct is always false here; the second is real —
            // `ChatInput::push_char` caps a line at 256 — so the suggestion is
            // suppressed once the line is full, matching vanilla rather than
            // overlapping the last few glyphs.
            //
            // The `+ 1` gap is conditional in vanilla, not unconditional:
            // `drawX += font.width(charSequence) + 1;` sits *inside*
            // `if (!displayed.isEmpty())` (`EditBox.extractWidgetRenderState`),
            // so an empty line reserves no pixel at all and `cursorX` stays at
            // the text origin — `text_width("")` is already `0.0`, but adding
            // `chat_pose_scale` unconditionally would still reserve a pixel
            // vanilla does not for that one case, so the `is_empty` guard
            // matters even though the width term alone would not have.
            let full = input.chars().count() >= 256;
            let cursor_x = if input.is_empty() {
                margin
            } else {
                margin + b.text_width(input, chat_pose_scale) + chat_pose_scale
            };
            if !full && let Some(ghost) = frame.chat_suggestion_ghost {
                b.text(
                    ghost,
                    cursor_x - chat_pose_scale,
                    input_y,
                    chat_pose_scale,
                    SUGGESTION_GHOST,
                );
            }
            // The caret, drawn **last** so it composites on top of both the
            // typed text and any ghost suggestion — the literal fix for the
            // draw-order bug above. A trailing underscore stands in for
            // vanilla's append-caret
            // (`TextCursorUtils.extractAppendCursor`,
            // `TextCursorUtils.java`); `chat_caret_visible` blinks it at
            // vanilla's real 300ms rate (see [`HudFrame::chat_caret_visible`]).
            // Drawn at `cursor_x` itself (vanilla's plain `cursorX`, the append
            // form) — one pixel right of the ghost's `cursor_x - chat_pose_scale`,
            // which is vanilla's own one-pixel gap between the two, not a
            // restated offset.
            if frame.chat_caret_visible {
                b.text("_", cursor_x, input_y, chat_pose_scale, [1.0, 1.0, 1.0, 1.0]);
            }
        }
        // The scrollback stacks upward from here — vanilla's own `chatBottom`
        // (see [`chat_bottom`]'s doc), unconditional on whether the box is
        // open. This used to be `input_y - INPUT_STRIP_PAD * chat_pose_scale`
        // while open, which coupled the scrollback's anchor to the input
        // strip's own top edge; vanilla never does that (`chatBottom` and the
        // `EditBox`'s `height - 12` are two independent literals in two
        // different classes), and the coupling is what erased vanilla's real
        // ~26px headroom between the newest message and the input box,
        // leaving them flush.
        let chat_bottom = chat_bottom(b.h, chat_pose_scale);
        // How many visual rows fit the configured box height — vanilla's
        // `ChatComponent.getLinesPerPage` (`ChatComponent.java`,
        // `height / lineHeight`), derived from the same `chat_box_h`/
        // `chat_line_h` the draw below actually uses, not a restated
        // constant.
        let max_visual_rows = (chat_box_h / chat_line_h).floor().max(1.0) as usize;
        let mut row_i = 0usize;
        // Each logical entry can wrap into several visual rows, all sharing
        // that entry's age/alpha. Vanilla stacks a wrapped message's *last*
        // split line nearest the bottom edge and its earlier lines above it
        // (`ChatComponent.addMessageToDisplayQueue`'s per-line `addFirst`,
        // `ChatComponent.java`, combined with `forEachLine`'s
        // `lineIndex → chatBottom - lineIndex * entryHeight`,
        // `ChatComponent.java`) — reversing each entry's own wrapped
        // rows before stacking reproduces that order.
        'entries: for (line, age) in frame.chat.iter().rev() {
            // While open, every line is fully lit; while closed, lines fade over
            // their last two seconds of a ten-second life and then disappear.
            let alpha = if chat_open {
                1.0
            } else {
                chat_line_alpha(*age)
            };
            if alpha <= 0.0 {
                // Older-than-visible lines end the stack: everything above is
                // older still, so there is nothing more to draw.
                break;
            }
            // `options.chat.color == false` strips every legacy code before
            // wrapping/drawing (`ComponentRenderUtils.stripColor`) rather than
            // just ignoring them while drawing, matching vanilla.
            let stripped = if opts.colors { None } else { Some(strip_legacy(line)) };
            let display: &str = stripped.as_deref().unwrap_or(line);
            // Wrapped once per message, not once per frame (issue #527 (a)):
            // the cache keys on the display text plus this frame's box width
            // and pose scale, so a frame with no new line, no resize and no
            // options edit performs zero wraps. Without a cache attached this
            // is the old behaviour, just spelled through the same call.
            let sub_rows = match frame.chat_wrap {
                Some(cache) => cache.rows(display, chat_box_w, chat_pose_scale, |t| {
                    b.wrap_legacy(t, chat_box_w, chat_pose_scale)
                }),
                None => std::rc::Rc::from(b.wrap_legacy(display, chat_box_w, chat_pose_scale)),
            };
            for sub in sub_rows.iter().rev() {
                if row_i >= max_visual_rows {
                    break 'entries;
                }
                let y = chat_bottom - (row_i as f32 + 1.0) * chat_line_h;
                if y < margin {
                    break 'entries;
                }
                b.rect_px(
                    0.0,
                    y - 1.0,
                    chat_box_w,
                    chat_line_h,
                    [0.0, 0.0, 0.0, chat_bg_opacity * alpha],
                );
                b.text_legacy(
                    sub,
                    margin,
                    y,
                    chat_pose_scale,
                    [0.92, 0.94, 1.0],
                    alpha * chat_text_opacity,
                );
                row_i += 1;
            }
        }

        // The command-suggestion dropdown, last in the chat overlay because it
        // overlaps both the input line above and the scrollback below —
        // `ChatScreen.extractRenderState` calls `commandSuggestions
        // .extractRenderState` after `super`, i.e. after every widget including
        // the `EditBox`. `draw_command_suggestions`' own doc holds the table of
        // what must still composite above it, and `SUGGESTION_LAYERS` the order
        // inside it.
        if let Some(popup) = frame.chat_suggestions.as_ref() {
            let layout =
                suggestion_layout(b.w, b.h, chat_pose_scale, popup, |s| {
                    b.text_width(s, chat_pose_scale)
                });
            draw_command_suggestions(&mut b, popup, layout, chat_pose_scale, &SUGGESTION_LAYERS);
        }

        // Crosshair: a white plus at the centre.
        //
        // `arm`/`thick` reproduce vanilla's actual ink, not its sprite's bounding
        // box. `Hud.extractCrosshair` (`Hud.java`) blits the 15x15
        // `hud/crosshair` sprite (`assets/minecraft/textures/gui/sprites/hud/
        // crosshair.png`) at `((guiWidth-15)/2, (guiHeight-15)/2, 15, 15)` — but
        // that box is mostly transparent padding. Read directly off the PNG's own
        // pixels: only rows/columns 3..=11 of the 15x15 grid are opaque, a
        // single-pixel-thick "+" spanning 9 of the 15 px, centred in the box (and
        // therefore already centred on `(cx, cy)` here). This used to draw the
        // *sprite's* 16-wide, 2-thick bounding box solid instead of that 9-wide,
        // 1-thick mark — a ~3.5x ink-area crosshair at every GUI scale, on both
        // targets (this call has no `wasm32` branch), which is the whole of the
        // "crosshair is too big" report. Hand-drawn rather than a real `b.sprite`
        // blit for the same reason the attack-indicator fallback below is: this
        // stays a plain colour-stream quad pair, so it still draws with no GUI
        // atlas attached (a jar-less/headless run), which `b.sprite` would not,
        // and the two tests asserting an exact 12-vert/2-quad crosshair
        // (`geometry_has_crosshair_and_text`, `hiding_the_debug_overlay_removes_its_geometry`)
        // stay meaningful rather than moving to a different vertex stream.
        if frame.crosshair {
            let (cx, cy) = (b.w * 0.5, b.h * 0.5);
            let arm = 4.5;
            let thick = 1.0;
            let col = [1.0, 1.0, 1.0, 0.85];
            b.rect_px(cx - arm, cy - thick * 0.5, arm * 2.0, thick, col);
            b.rect_px(cx - thick * 0.5, cy - arm, thick, arm * 2.0, col);

            // Attack-strength (cooldown) indicator: a small fill bar just below
            // the crosshair — vanilla's `Hud.extractCrosshair`'s
            // `CROSSHAIR_ATTACK_INDICATOR_{BACKGROUND,PROGRESS}_SPRITE` branch
            // (`Hud.java`, `.cache/mc/26.2/client-src`), gated there on
            // `AttackIndicatorStatus::CROSSHAIR` (issue #121 scopes this shell
            // to that variant only — no options-menu toggle exists yet, and no
            // hotbar-style variant). Native 16x4, anchored at vanilla's own
            // `(guiWidth/2 - 8, guiHeight/2 - 7 + 16)` — here `(cx - 8, cy + 9)`
            // against this canvas's own centre, which this block already
            // computed for the plus above.
            //
            // `b.sprite`/`b.gui_geometry` are no-op-safe with no atlas attached
            // (see `sprite_vitals`'s doc on the same pattern), so a
            // jar-less/headless run draws nothing here rather than needing a
            // second procedural implementation — the same choice already made
            // for the underwater bubble row (`bubble_row`, below).
            //
            // Vanilla hides this entirely once `attackStrengthScale >= 1.0`
            // *unless* a slow weapon (delay > 5 ticks) is aimed at a living,
            // in-range target, in which case a distinct "ready" icon
            // (`CROSSHAIR_ATTACK_INDICATOR_FULL_SPRITE`) replaces it
            // (`Hud.java`). That icon needs the crosshair's target
            // entity plus its liveness/range/weapon-delay, none of which
            // `HudFrame` carries — deliberately out of scope per
            // `docs/combat.md`'s crits/sweep cut for the same issue. At full
            // charge this draws nothing, matching vanilla's non-"ready" case.
            if let Some(raw_scale) = frame.attack_cooldown {
                let scale = raw_scale.clamp(0.0, 1.0);
                if scale < 1.0 {
                    let iw = 16.0;
                    let ih = 4.0;
                    let ix = cx - iw * 0.5;
                    let iy = cy + 9.0;
                    let white = [1.0, 1.0, 1.0, 1.0];
                    b.sprite(
                        "hud/crosshair_attack_indicator_background",
                        ix,
                        iy,
                        iw,
                        ih,
                        white,
                    );
                    if scale > 0.0 {
                        // Crop by shrinking both the destination width and the
                        // sampled UV span, exactly `sprite_vitals`' XP-bar-progress
                        // idiom — reveals the fill pattern instead of squashing it.
                        for mut q in
                            b.gui_geometry("hud/crosshair_attack_indicator_progress", ix, iy, iw, ih)
                        {
                            let span = q.uv_max[0] - q.uv_min[0];
                            q.dst[2] *= scale;
                            q.uv_max[0] = q.uv_min[0] + span * scale;
                            b.push_sprite_quad(q, white);
                        }
                    }
                }
            }
        }

        // The hotbar (bottom-centre) and the survival pip rows above it. The
        // hotbar draws whenever we're in active play; the pips only on a live
        // survival server that reports health/food.
        let cx = b.w * 0.5;
        // With the vanilla GUI atlas attached, the vitals cluster (hotbar, XP,
        // hearts, hunger) draws from real sprites; without it — jar-less runs and
        // the headless negative-control path — it falls back to the procedural
        // quads below. Both branches return `bars_y`, the anchor the action bar
        // sits above, so the rest of the HUD is oblivious to which drew.
        let bars_y = if b.gui.is_some() {
            sprite_vitals(&mut b, frame, &anim)
        } else {
            let pip = 8.0;
            let gap = 2.0;
            let row_w = 10.0 * (pip + gap);

            // Hotbar: a 9-cell bar with the selected cell ringed in white. Item
            // icons are deferred (no item atlas yet) so the cells are empty wells —
            // the frame and selection are real, the contents explicitly aren't.
            let hotbar_top = if let Some(sel) = frame.hotbar {
                let sel = sel.min(8);
                let cell = 22.0;
                let hw = 9.0 * cell;
                let hx = cx - hw * 0.5;
                let hy = b.h - HOTBAR_MARGIN - cell;
                b.rect_px(
                    hx - 2.0,
                    hy - 2.0,
                    hw + 4.0,
                    cell + 4.0,
                    [0.0, 0.0, 0.0, 0.55],
                );
                for i in 0..9 {
                    let sx = hx + i as f32 * cell;
                    b.rect_px(
                        sx + 1.0,
                        hy + 1.0,
                        cell - 2.0,
                        cell - 2.0,
                        [0.28, 0.28, 0.30, 0.5],
                    );
                }
                // A 2px white ring around the selected cell (four edges).
                let sx = hx + sel as f32 * cell;
                let bw = 2.0;
                let col = [0.95, 0.97, 1.0, 0.95];
                b.rect_px(sx - 1.0, hy - 1.0, cell + 2.0, bw, col);
                b.rect_px(sx - 1.0, hy + cell + 1.0 - bw, cell + 2.0, bw, col);
                b.rect_px(sx - 1.0, hy - 1.0, bw, cell + 2.0, col);
                b.rect_px(sx + cell + 1.0 - bw, hy - 1.0, bw, cell + 2.0, col);
                hy
            } else {
                b.h - HOTBAR_MARGIN
            };

            // XP bar: a full-hotbar-width green progress bar just above the hotbar,
            // with the level number centred above it (vanilla green). Drawn only
            // once the server has sent experience (`frame.xp`); off a live server
            // this is `None` and nothing draws, keeping the gauge honest.
            // The same two gates the sprite path applies, for the same reasons — see
            // [`HudFrame::can_hurt_player`].
            //
            // This used to yield a `vitals_base` the pip rows stacked off, so an XP
            // bar and its level number pushed the hearts up. Both branches now
            // agree with [`sprite_vitals`] and take [`vitals_line_base`] instead —
            // vanilla's `yLineBase` does not move for the XP bar, and having the
            // two paths disagree about that was the reason the air-row gate could
            // not derive one rect for both.
            if let Some((level, progress)) = frame.xp.filter(|_| frame.can_hurt_player) {
                let bar_w = 9.0 * 22.0;
                let bx = cx - bar_w * 0.5;
                let bar_h = 4.0;
                let by = hotbar_top - bar_h - 5.0;
                b.rect_px(bx, by, bar_w, bar_h, [0.0, 0.0, 0.0, 0.7]);
                let fill = bar_w * progress.clamp(0.0, 1.0);
                if fill > 0.0 {
                    b.rect_px(bx, by, fill, bar_h, [0.47, 0.82, 0.16, 1.0]);
                }
                if level > 0 {
                    // Vanilla metrics, not this function's ambient `scale`/
                    // `line_h` — the same fix [`sprite_vitals`]'s own copy of
                    // this number already documents: `scale` here made it
                    // twice vanilla's size, and `line_h` is this HUD's 5×7
                    // debug-font stride, not `ContextualBar`'s real `6px` gap
                    // above the bar's top (`by - 6.0`).
                    let s = level.to_string();
                    let tw = b.text_width(&s, 1.0);
                    b.text(
                        &s,
                        cx - tw * 0.5,
                        by - 6.0,
                        1.0,
                        [0.44, 0.92, 0.20, 1.0],
                    );
                }
            }

            // Health / food pip rows, on vanilla's own `yLineBase` — see
            // [`vitals_line_base`]. Each row is 10 pips of 2 units; a pip lights the
            // moment any of its two units is present (a deliberate simplification —
            // no half-pip art yet).
            let bars_y = vitals_line_base(b.h);
            // The armour row, one row above the hearts and on the same left anchor,
            // mirroring [`sprite_vitals`]'s placement so the jar-less fallback and
            // the real thing agree about which side and which line it is on. `pips`
            // has no half-pip art (see the note below), so this row shows armour
            // rounded up to the pip, exactly as the health row already does — the
            // half-icon distinction only reaches pixels on the sprite path, and
            // [`armour_icon`] is the one place it is decided.
            //
            // `bars_y` is deliberately unchanged: it is the anchor the action bar and
            // the rest of the HUD hang off, and vanilla's own action bar sits at a
            // constant `guiHeight - 68` regardless of how many vitals rows are up.
            if frame.can_hurt_player
                && let Some(armour) = frame.armour
                && armour > 0
            {
                b.pips(
                    armour as f32,
                    cx - row_w - 8.0,
                    bars_y - pip - 2.0,
                    pip,
                    gap,
                    [0.72, 0.76, 0.82, 1.0],
                );
            }
            if frame.can_hurt_player && let Some(hp) = frame.health {
                b.pips(
                    hp,
                    cx - row_w - 8.0,
                    bars_y,
                    pip,
                    gap,
                    [0.86, 0.15, 0.16, 1.0],
                );
            }
            if frame.can_hurt_player && let Some(food) = frame.food {
                b.pips(
                    food as f32,
                    cx + 8.0,
                    bars_y,
                    pip,
                    gap,
                    [0.78, 0.60, 0.20, 1.0],
                );
            }

            bars_y
        };

        // Item icons sit inside the hotbar cells, drawn over whichever hotbar
        // frame (real atlas or procedural) was emitted above.
        draw_hotbar_items(&mut b, frame, &anim);

        // Action bar: a single centred line above the vitals cluster, fading with
        // the server-driven alpha. Legacy `§` colour codes render.
        //
        // Unscaled: `extractOverlayMessage` makes **no** `pose().scale()` call at
        // all, like the held-item name below. This used `scale`, which is 2.0 —
        // and since `logical_canvas` has already divided by the GUI scale, that
        // was a flat 2x on top of vanilla's own factor. See
        // `docs/hud-text-scale.md`.
        //
        // `guiHeight - 72`, absolute, the way the held-item name below already
        // reads its own `guiHeight - 59`: `extractOverlayMessage` translates the
        // pose to `(guiWidth / 2, guiHeight - 68)` and then draws at `y = -4`, and
        // it takes no game-mode or vitals-row branch of any kind. This used to
        // hang off `bars_y`, which meant it moved with the vitals cluster — so
        // correcting `yLineBase` above would otherwise have dragged it a further
        // 3 px away from vanilla rather than leaving it alone.
        //
        // Not ported: `textWithBackdrop`'s translucent panel behind the glyphs.
        if let Some((msg, alpha)) = frame.action_bar.as_ref().filter(|(_, a)| *a > 0.0) {
            // `spans_width`/`text_spans`, not the `legacy_width`/`text_legacy`
            // pair: a `§` string cannot express a hex colour, so the producer now
            // hands over spans and the measure has to be the one that matches the
            // draw or a centred line lands at the wrong `x`.
            let tw = b.spans_width(msg, 1.0);
            b.text_spans(
                msg,
                cx - tw * 0.5,
                b.h - 72.0,
                1.0,
                [1.0, 1.0, 1.0],
                *alpha,
            );
        }

        // Held-item name (issue #126): the selected hotbar item's styled name,
        // above the hotbar, fading with a server-independent client timer.
        // Unlike the action bar and title, vanilla draws this **unscaled**
        // (`Hud.java`, a plain `graphics.textWithBackdrop` call, no
        // ×2) — the same "vanilla's own draw never scales the font" lesson
        // the XP level number's fix (issue #256) already established two
        // blocks up in [`sprite_vitals`]. Using `scale` here would repeat
        // that exact defect on a second piece of HUD text.
        if let Some((name, alpha)) = frame.held_item.as_ref().filter(|(_, a)| *a > 0.0) {
            let tw = b.legacy_width(name, 1.0);
            let x = (b.w - tw) * 0.5;
            // `extractSelectedItemName`: `y = guiHeight - 59`, then `y += 14` when
            // `!canHurtPlayer()`, because creative and spectator have no
            // health/hunger row for the label to clear.
            let y = b.h - 59.0 + if frame.can_hurt_player { 0.0 } else { 14.0 };
            b.text_legacy(name, x, y, 1.0, [1.0, 1.0, 1.0], *alpha);
        }

        // Title / subtitle: a large centred overlay mid-screen, fading with the
        // server-driven alpha. Drawn only while a server-sent title is active,
        // so it costs nothing off a server that sends none.
        // `extractTitle` (`Hud.java`) translates once to the screen centre
        // (`:376`), then draws each string at an offset *inside* its own pose
        // scale — title `scale(4.0)` at `y = -10` (`:378,381`), subtitle
        // `scale(2.0)` at `y = 5` (`:385,387`). Multiplied out, those are the two
        // anchors below.
        //
        // Vanilla's factors are used **whole**. Multiplying them by this HUD's
        // `scale` drew both at 2x (`logical_canvas` has already applied the GUI
        // scale, so `scale` is a second application) and, worse, made the
        // subtitle's offset depend on the *title's* scale via `ty + ts * 9.0` —
        // so correcting the scale alone would have moved the subtitle. The
        // position was independently wrong too: `b.h * 0.40` is not `h/2 - 40`.
        if let Some((title, subtitle, alpha)) = frame.title.as_ref().filter(|(_, _, a)| *a > 0.0) {
            const TITLE_POSE: f32 = 4.0;
            const SUBTITLE_POSE: f32 = 2.0;
            let cy = b.h * 0.5;
            // Spans, so a hex-coloured title keeps its colour — see
            // [`HudFrame::title`]. `spans_width` is the measurement half of
            // `text_spans`, and using the other pair here would shift every
            // centred glyph.
            let tw = b.spans_width(title, TITLE_POSE);
            b.text_spans(
                title,
                (b.w - tw) * 0.5,
                cy - 10.0 * TITLE_POSE,
                TITLE_POSE,
                [1.0, 1.0, 1.0],
                *alpha,
            );
            if let Some(sub) = subtitle {
                let sw = b.spans_width(sub, SUBTITLE_POSE);
                b.text_spans(
                    sub,
                    (b.w - sw) * 0.5,
                    cy + 5.0 * SUBTITLE_POSE,
                    SUBTITLE_POSE,
                    [1.0, 1.0, 1.0],
                    *alpha,
                );
            }
        }

        // Boss bars: stacked title-over-bar at the top-centre —
        // `BossHealthOverlay.extractRenderState`/`extractBar`, ported at
        // vanilla's own fixed 182×5 native size and `BOSS_BAR_TEXT_SCALE`
        // (`1.0`) rather than this function's ambient `scale`/`line_h`, the
        // same exemption as [`SIDEBAR_LINE_H`]. An empty slice draws nothing,
        // so this costs zero verts off a server that sends none.
        //
        // Four clauses, each a real vanilla `blitSprite`, all untinted
        // (`color = -1` in `extractBar`'s private overload) since every
        // colour is its own pre-baked sprite rather than a tinted greyscale
        // one:
        //   1. the background plate, full 182px, `bb.color.background_sprite_id()`
        //   2. the background notch overlay, also full 182px, only when the
        //      bar's overlay style is not `Progress`
        //   3. the progress fill, `bb.color.progress_sprite_id()`, **cropped**
        //      (not scaled) to `lerp_discrete_width(progress, 182)` px — see
        //      that function's doc for why this differs from a plain
        //      `progress * 182`
        //   4. the progress notch overlay, cropped to the same width as (3),
        //      again only when the overlay style is not `Progress`
        // (2) and (4) draw on top of (1) and (3) respectively, exactly
        // `extractBar`'s draw order.
        if !frame.boss_bars.is_empty() {
            let bscale = BOSS_BAR_TEXT_SCALE;
            let bar_x = b.w * 0.5 - BOSS_BAR_WIDTH * 0.5;
            let mut y_offset = BOSS_BAR_TOP;
            let white = [1.0, 1.0, 1.0, 1.0];
            for bb in frame.boss_bars {
                let yo = y_offset;
                let tw = b.spans_width(&bb.title, bscale);
                b.text_spans(
                    &bb.title,
                    b.w * 0.5 - tw * 0.5,
                    yo - 9.0,
                    bscale,
                    [1.0, 1.0, 1.0],
                    1.0,
                );

                // (1) background plate, full width.
                b.sprite(
                    bb.color.background_sprite_id(),
                    bar_x,
                    yo,
                    BOSS_BAR_WIDTH,
                    BOSS_BAR_HEIGHT,
                    white,
                );
                // (2) background notch overlay, full width, on top of (1).
                if let Some(id) = bb.overlay.background_sprite_id() {
                    b.sprite(id, bar_x, yo, BOSS_BAR_WIDTH, BOSS_BAR_HEIGHT, white);
                }

                let width_px = crate::overlay::lerp_discrete_width(
                    bb.progress.clamp(0.0, 1.0),
                    BOSS_BAR_WIDTH as i32,
                );
                if width_px > 0 {
                    let frac = width_px as f32 / BOSS_BAR_WIDTH;
                    // (3) progress fill, cropped to `frac` — shrink both the
                    // destination width and the sampled UV span (as the XP
                    // bar's fill does above), so the bar reveals its own
                    // pattern instead of squashing it into a narrower box.
                    for mut q in
                        b.gui_geometry(bb.color.progress_sprite_id(), bar_x, yo, BOSS_BAR_WIDTH, BOSS_BAR_HEIGHT)
                    {
                        let span = q.uv_max[0] - q.uv_min[0];
                        q.dst[2] *= frac;
                        q.uv_max[0] = q.uv_min[0] + span * frac;
                        b.push_sprite_quad(q, white);
                    }
                    // (4) progress notch overlay, cropped the same way, on
                    // top of (3).
                    if let Some(id) = bb.overlay.progress_sprite_id() {
                        for mut q in b.gui_geometry(id, bar_x, yo, BOSS_BAR_WIDTH, BOSS_BAR_HEIGHT) {
                            let span = q.uv_max[0] - q.uv_min[0];
                            q.dst[2] *= frac;
                            q.uv_max[0] = q.uv_min[0] + span * frac;
                            b.push_sprite_quad(q, white);
                        }
                    }
                }
                y_offset += BOSS_BAR_STEP;
                if y_offset >= b.h / 3.0 {
                    break;
                }
            }
        }

        // Scoreboard sidebar — `Hud.displayScoreboardSidebar`, ported at vanilla's
        // own metrics (`SIDEBAR_LINE_H`/`SIDEBAR_TEXT_SCALE`) rather than this
        // function's ambient `scale`/`line_h`, exactly the exemption
        // [`TAB_LINE_H`] documents for the tab list. `width` is the widest of the
        // title and every `name [+ ": " + score]` row (the spacer only counts
        // when the row actually has a score — vanilla's
        // `scoreWidth > 0 ? spacerWidth + scoreWidth : 0`); `bottom` sits at
        // `guiHeight() / 2 + height / 3`, which is a deliberate top bias, not a
        // symmetric centring — porting it as `h/2` would silently "fix" a
        // vanilla quirk. Absent when nothing is displayed.
        if let Some(side) = frame.sidebar {
            let sscale = SIDEBAR_TEXT_SCALE;
            let spacer_w = b.text_width(": ", sscale);
            let title_w = b.spans_width(&side.title, sscale);
            let mut width = title_w;
            for l in &side.lines {
                let score_w = b.spans_width(&l.score, sscale);
                let extra = if score_w > 0.0 { spacer_w + score_w } else { 0.0 };
                width = width.max(b.spans_width(&l.label, sscale) + extra);
            }
            let entries = side.lines.len() as f32;
            let height = entries * SIDEBAR_LINE_H;
            let bottom = b.h / 2.0 + height / 3.0;
            let left = b.w - width - SIDEBAR_EDGE_MARGIN;
            let right = b.w - SIDEBAR_EDGE_MARGIN + 2.0;
            let header_y = bottom - height;
            let plate_x = left - 2.0;
            let plate_w = right - plate_x;
            b.rect_px(
                plate_x,
                header_y - 10.0,
                plate_w,
                9.0,
                [0.0, 0.0, 0.0, SIDEBAR_HEADER_BG_ALPHA],
            );
            b.rect_px(
                plate_x,
                header_y - 1.0,
                plate_w,
                bottom - (header_y - 1.0),
                [0.0, 0.0, 0.0, SIDEBAR_BODY_BG_ALPHA],
            );
            let title_x = left + width / 2.0 - title_w / 2.0;
            b.text_spans(&side.title, title_x, header_y - 9.0, sscale, [1.0, 1.0, 1.0], 1.0);
            for (i, l) in side.lines.iter().enumerate() {
                let y = bottom - (entries - i as f32) * SIDEBAR_LINE_H;
                b.text_spans(&l.label, left, y, sscale, [1.0, 1.0, 1.0], 1.0);
                let score_w = b.spans_width(&l.score, sscale);
                b.text_spans(&l.score, right - score_w, y, sscale, SIDEBAR_SCORE_DEFAULT, 1.0);
            }
        }

        // The Tab player-list overlay — `PlayerTabOverlay.extractRenderState`,
        // ported rather than approximated.
        //
        // Read as vanilla's own draw order, because this GUI path has no depth
        // compare and submission order is the only z there is: the header plate
        // and its lines, the row plate, then per row a translucent slot fill, the
        // name, and the ping bars, then the footer plate and its lines.
        //
        // Everything here is at `TAB_TEXT_SCALE`/`TAB_LINE_H` — vanilla's own
        // metrics in the logical canvas — and *not* the HUD's 2× pitch that the
        // rest of `build_inner` uses. See `TAB_LINE_H`.
        if let Some(players) = frame.players {
            let tab_scale = TAB_TEXT_SCALE;
            // The two font measurements vanilla takes, through the same
            // `spans_width`/`text_width` the draw uses. `max_name_width` sizes
            // the column; the banner width only ever *widens* the plates.
            let max_name_width = players
                .rows
                .iter()
                .map(|row| b.spans_width(&row.name, tab_scale))
                .fold(0.0f32, f32::max);
            let widest_banner = players
                .header
                .iter()
                .chain(players.footer.iter())
                .map(|l| b.spans_width(l, tab_scale))
                .fold(0.0f32, f32::max);
            let panel = TabPanel::new(
                b.w,
                players.len(),
                players.show_head,
                max_name_width,
                players.header.len(),
                players.footer.len(),
                widest_banner,
            );
            let plate_x = panel.plate_x();
            let plate_w = panel.plate_w();

            // The header plate spans `yyo - 1 ..= yyo + n * 9`, so it is one
            // pixel taller than the lines it holds. Drawn only when the server
            // actually sent a header: a vanilla server sends none unless
            // something sets one, and fabricating one to fill the space is what
            // this overlay must not do.
            if !players.header.is_empty() {
                b.rect_px(
                    plate_x,
                    panel.header_top - 1.0,
                    plate_w,
                    players.header.len() as f32 * TAB_LINE_H + 1.0,
                    TAB_PLATE,
                );
                for (i, line) in players.header.iter().enumerate() {
                    let x = panel.centred_x(b.spans_width(line, tab_scale));
                    b.text_spans(
                        line,
                        x,
                        panel.header_y(i),
                        tab_scale,
                        [TAB_INK[0], TAB_INK[1], TAB_INK[2]],
                        TAB_INK[3],
                    );
                }
            }

            // The row plate is drawn unconditionally, sized to `rows` — the rows
            // *per column*, not the player count, so a two-column list gets one
            // plate half as tall as a naive `slots * 9` would make it.
            b.rect_px(
                plate_x,
                panel.rows_top - 1.0,
                plate_w,
                panel.rows as f32 * TAB_LINE_H + 1.0,
                TAB_PLATE,
            );

            for (i, row) in players.rows.iter().enumerate() {
                let [sx, sy] = panel.slot_origin(i);
                // `fill(xo, yo, xo + slotWidth, yo + 8, background)` — 8 tall
                // inside a 9 px pitch, which is what leaves the 1 px gap between
                // rows that makes the list read as a list.
                b.rect_px(sx, sy, panel.slot_w, TAB_LINE_H - 1.0, TAB_ROW_FILL);
                let name_x = if players.show_head { sx + TAB_HEAD_W } else { sx };
                let ink = if row.spectator { TAB_INK_SPECTATOR } else { TAB_INK };
                b.text_spans(&row.name, name_x, sy, tab_scale, [ink[0], ink[1], ink[2]], ink[3]);
                // The signal bars, right-aligned inside the slot. Vanilla's
                // `extractPingIcon` subtracts the head offset back off `xo`, so
                // the icon is measured from the **slot's** left edge and does not
                // move when a head is drawn.
                b.sprite(
                    row.ping_sprite,
                    sx + panel.slot_w - TAB_PING_INSET,
                    sy,
                    TAB_PING_W,
                    TAB_PING_H,
                    TAB_INK,
                );
            }

            if !players.footer.is_empty() {
                b.rect_px(
                    plate_x,
                    panel.footer_top - 1.0,
                    plate_w,
                    players.footer.len() as f32 * TAB_LINE_H + 1.0,
                    TAB_PLATE,
                );
                for (i, line) in players.footer.iter().enumerate() {
                    let x = panel.centred_x(b.spans_width(line, tab_scale));
                    b.text_spans(
                        line,
                        x,
                        panel.footer_y(i),
                        tab_scale,
                        [TAB_INK[0], TAB_INK[1], TAB_INK[2]],
                        TAB_INK[3],
                    );
                }
            }
        }

        // Sound-subtitle captions, bottom-right (issue #198).
        if !frame.sound_subtitles.is_empty() {
            draw_sound_subtitles(&mut b, frame.sound_subtitles);
        }

        // Recipe-unlock toast, top-right (issue #163). Drawn last so it lands
        // over the sidebar/tab overlays, matching vanilla's own toast layer,
        // which `ToastManager.render` composites after the HUD entirely.
        if let Some(toast) = &frame.recipe_toast {
            draw_recipe_toast(&mut b, toast);
        }
        // The advancement-completion toast (issue #167), same slot and layer.
        if let Some(toast) = &frame.advancement_toast {
            draw_advancement_toast(&mut b, toast);
        }

        Self {
            verts: b.verts,
            sprite_verts: b.sprite_verts,
            item_verts: b.item_verts,
            glint_verts: b.glint_verts,
            model_verts: b.model_verts,
            special: b.special,
        }
    }
}

/// The recipe-unlock toast's rect in **logical canvas pixels**, as
/// `(x, y, w, h)` — `Toast::xPos`/`yPos` with `firstSlotIndex == 0`
/// (`Toast.java`).
///
/// This exists so the draw and any gate measuring it share **one** expression.
/// A gate that restated `canvas_w - 160.0` would silently stop describing the
/// draw the moment the slide-in is threaded through, which is exactly the
/// failure mode a HUD gate here already hit once by hardcoding a `cluster_top`
/// the draw computed from a moving anchor.
#[must_use]
pub fn recipe_toast_rect(canvas_w: f32, visible_portion: f32) -> (f32, f32, f32, f32) {
    let tw = lodestone_game::recipe::RECIPE_TOAST_WIDTH as f32;
    let th = lodestone_game::recipe::RECIPE_TOAST_HEIGHT as f32;
    (canvas_w - tw * visible_portion, 0.0, tw, th)
}

/// Draw one recipe-unlock toast. Geometry and colours are cited on
/// [`RecipeToastView`]; this function is only the transcription.
///
/// The background prefers the real `toast/recipe` sprite and falls back to a
/// flat fill when no GUI atlas is attached — the same jar-less degradation
/// every other element in this module uses, and the reason a coverage gate can
/// run headless.
/// Vanilla's `SubtitleOverlay` layout constants, all in logical GUI pixels
/// (`SubtitleOverlay.java`).
mod subtitle_layout {
    /// The row's text height; `halfHeight` is the integer half of it.
    pub(super) const ROW_H: f32 = 9.0;
    /// `guiHeight - 35` for row 0 — the bottom row's centre line.
    pub(super) const BOTTOM_INSET: f32 = 35.0;
    /// `row * (height + 1)`: rows stack upward 10px apart.
    pub(super) const ROW_STEP: f32 = ROW_H + 1.0;
    /// `guiWidth - halfWidth - 2`: the block's right edge sits 2px in.
    pub(super) const RIGHT_INSET: f32 = 2.0;
    /// The background plate's 1px bleed on every side.
    pub(super) const PLATE_PAD: f32 = 1.0;
}

/// Vanilla's sound-subtitle overlay: one right-aligned plate per live caption,
/// stacked upward from just above the hotbar, oldest at the bottom.
///
/// Ported from `SubtitleOverlay.extractRenderState`
/// (`SubtitleOverlay.java`). Two details are load-bearing:
///
/// * **Every row is the same width**, `max(text widths)` plus the width of
///   `"<"`, `">"` and two spaces — so the arrow columns exist on every plate,
///   and a row with no arrow does not shrink. Sizing each plate to its own text
///   makes a ragged stack that looks like a layout bug.
/// * **The text is centred inside that width**, not left-aligned — the plate is
///   right-aligned, its contents are not.
fn draw_sound_subtitles(b: &mut Builder, captions: &[crate::audio::subtitles::SubtitleCaption]) {
    use crate::audio::subtitles::SubtitleArrow;
    use subtitle_layout::{BOTTOM_INSET, PLATE_PAD, RIGHT_INSET, ROW_H, ROW_STEP};

    // Vanilla draws at a fixed 1.0 scale under the GUI transform; `b`'s canvas is
    // already the logical (gui-scaled) one, so "1 logical pixel" here is exactly
    // vanilla's own unit and no extra factor belongs in this function.
    let scale = 1.0;
    let arrow_w = b.text_width("<", scale) + b.text_width(">", scale) + b.text_width("  ", scale);
    let width = captions
        .iter()
        .map(|c| b.text_width(&c.text, scale))
        .fold(0.0f32, f32::max)
        + arrow_w;
    let half_w = (width / 2.0).floor();
    let half_h = (ROW_H / 2.0).floor();
    let cx = b.w - half_w - RIGHT_INSET;

    for (row, caption) in captions.iter().enumerate() {
        let cy = b.h - BOTTOM_INSET - row as f32 * ROW_STEP;
        // `getBackgroundColor(0.8F)`: black at 80%, the non-chat text plate.
        b.rect_px(
            cx - half_w - PLATE_PAD,
            cy - half_h - PLATE_PAD,
            half_w * 2.0 + PLATE_PAD * 2.0,
            ROW_H + PLATE_PAD * 2.0,
            [0.0, 0.0, 0.0, 0.8],
        );
        // Brightness fades, alpha does not — see `audio::subtitles`' module doc.
        let ink = [
            caption.brightness,
            caption.brightness,
            caption.brightness,
            1.0,
        ];
        let text_y = cy - half_h;
        match caption.arrow {
            Some(SubtitleArrow::Right) => {
                let w = b.text_width(">", scale);
                b.text(">", cx + half_w - w, text_y, scale, ink);
            }
            Some(SubtitleArrow::Left) => {
                b.text("<", cx - half_w, text_y, scale, ink);
            }
            None => {}
        }
        let tw = b.text_width(&caption.text, scale);
        b.text(&caption.text, cx - (tw / 2.0).floor(), text_y, scale, ink);
    }
}

fn draw_recipe_toast(b: &mut Builder, toast: &RecipeToastView) {
    let (tx, ty, tw, th) = recipe_toast_rect(b.w, toast.visible_portion);

    // `blitSprite(BACKGROUND_SPRITE, 0, 0, width(), height())`
    // (`RecipeToast.java`).
    let quads = b.gui_geometry(RECIPE_TOAST_SPRITE, tx, ty, tw, th);
    if quads.is_empty() {
        // Jar-less: vanilla's toast art is an opaque light panel, so a flat
        // fill keeps the text legible rather than leaving it on the world.
        b.rect_px(tx, ty, tw, th, [0.86, 0.86, 0.86, 1.0]);
    } else {
        for q in quads {
            b.push_sprite_quad(q, [1.0, 1.0, 1.0, 1.0]);
        }
    }

    // `-11534256 == 0xFF500050` and `-16777216 == 0xFF000000`
    // (`RecipeToast.java`). Unscaled, and unshadowed (the trailing
    // `false`).
    const TITLE_COLOUR: [f32; 4] = [0x50 as f32 / 255.0, 0.0, 0x50 as f32 / 255.0, 1.0];
    const DESCRIPTION_COLOUR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
    b.text(RECIPE_TOAST_TITLE, tx + 30.0, ty + 7.0, 1.0, TITLE_COLOUR);
    b.text(
        RECIPE_TOAST_DESCRIPTION,
        tx + 30.0,
        ty + 18.0,
        1.0,
        DESCRIPTION_COLOUR,
    );

    // The station badge is drawn under `pose().scale(0.6)`, which scales the
    // *position* as well as the size — so `fakeItem(categoryItem, 3, 3)` lands
    // at `(1.8, 1.8)` with a `9.6px` icon, not at `(3, 3)` with a small one.
    // Transcribing this as `(3, 3)` is the same class of mistake as reading a
    // Java record's positional fields in the wrong order.
    const ICON: f32 = 16.0;
    const STATION_SCALE: f32 = 0.6;
    b.item_icon(
        &toast.station,
        tx + 3.0 * STATION_SCALE,
        ty + 3.0 * STATION_SCALE,
        ICON * STATION_SCALE,
    );
    // `fakeItem(unlockedItem, 8, 8)` (`RecipeToast.java`), unscaled.
    b.item_icon(&toast.unlocked, tx + 8.0, ty + 8.0, ICON);
}

/// Draw one advancement-completion toast. Cited on [`AdvancementToastView`].
fn draw_advancement_toast(b: &mut Builder, toast: &AdvancementToastView) {
    let (tx, ty, tw, th) = recipe_toast_rect(b.w, toast.visible_portion);

    let quads = b.gui_geometry(ADVANCEMENT_TOAST_SPRITE, tx, ty, tw, th);
    if quads.is_empty() {
        // Jar-less: vanilla's advancement toast art is a dark plate with a light
        // border, so a dark fill keeps the yellow heading and white title legible.
        b.rect_px(tx, ty, tw, th, [0.05, 0.05, 0.08, 0.94]);
    } else {
        for q in quads {
            b.push_sprite_quad(q, [1.0, 1.0, 1.0, 1.0]);
        }
    }

    b.text(&toast.heading, tx + 30.0, ty + 7.0, 1.0, toast.heading_colour);
    b.text(&toast.title, tx + 30.0, ty + 18.0, 1.0, [1.0, 1.0, 1.0, 1.0]);
    if let Some(icon) = &toast.icon {
        // `fakeItem(iconItem, 8, 8)` (`AdvancementToast.java`), unscaled.
        b.item_icon(icon, tx + 8.0, ty + 8.0, 16.0);
    }
}

/// Draw the item icons into the nine hotbar cells. Mirrors the slot geometry of
/// both hotbar-draw paths (real GUI atlas at scale 2, or the procedural 22px
/// cells) so icons land centred in the wells either way. A no-op without an item
/// atlas or `hotbar_items`, so headless / jar-less runs are unaffected.
fn draw_hotbar_items(b: &mut Builder, frame: &HudFrame, anim: &HudAnim) {
    let Some(slots) = frame.hotbar_items else {
        return;
    };
    let cx = b.w * 0.5;
    // (first icon origin x, icon origin y, cell pitch, icon size) for the active
    // hotbar layout. Vanilla insets the 16px icon 3px into each 20px native slot.
    let (icon0_x, icon_y, pitch, size) = if b.gui.is_some() {
        // Native sprite pixels, laid straight into the already-scale-divided
        // canvas — see the "GUI Scale" note on `sprite_vitals`, which this
        // mirrors exactly (same hotbar rect, same reasoning for dropping the
        // old hardcoded ×2).
        let hw = 182.0;
        let hh = 22.0;
        let hx = cx - hw * 0.5;
        let hy = b.h - hh - HOTBAR_MARGIN;
        (hx + 3.0, hy + 3.0, 20.0, 16.0)
    } else {
        let cell = 22.0;
        let hw = 9.0 * cell;
        let hx = cx - hw * 0.5;
        let hy = b.h - HOTBAR_MARGIN - cell;
        (hx + 3.0, hy + 3.0, cell, 16.0)
    };
    for (i, slot) in slots.iter().enumerate().take(9) {
        if let Some(item) = slot {
            let x = icon0_x + i as f32 * pitch;
            let pop = anim.hotbar_pop.get(i).copied().unwrap_or(0.0);
            b.item_icon_popped(item, x, icon_y, size, pop);
        }
    }
}

/// The per-frame vitals-cluster animation phases [`HudGeometry::build_inner`]
/// draws with — heart blink/jitter, the hunger wobble and the hotbar pop.
/// See `hud/anim.rs` for the vanilla citations and `docs/hud-animations.md`
/// for the port notes.
///
/// [`HudAnim::NONE`] is idle (every field at its settled value) and is what
/// [`HudGeometry::build`]/[`HudGeometry::build_with_font`]/
/// [`HudGeometry::build_with_gui`] pass — the pure, jar-less, deterministic
/// entry points every pre-existing geometry test calls — so none of those
/// three grow a wall-clock dependency, and every one of them keeps drawing
/// pixel-identically to before this type existed. Only
/// [`HudRenderer::render_with_item_models`] threads a live value in, computed
/// from [`HudRenderer`]'s own cross-frame animation state.
#[derive(Debug, Clone, Copy)]
struct HudAnim {
    /// Vanilla's heart-row `blink` (`Hud.java`).
    heart_blink: bool,
    /// Vanilla's `displayHealth` (`Hud.java`) — the "ghost" heart
    /// overlay's total. Equal to the current health while idle.
    display_health: i32,
    /// The wall-tick index this frame resolved to (see `hud/anim::wall_tick`)
    /// — the input the pure per-container/per-pip jitter functions need.
    tick: i64,
    /// Per-hotbar-slot pop amount, vanilla's `5.0 → 0.0` scale, `0.0` =
    /// settled/idle (see `hud/anim::HotbarPop`).
    hotbar_pop: [f32; 9],
    /// Level-up flash strength: `1.0` at the moment of the gain, decaying to
    /// `0.0` (see `hud/anim::XpFlash`, issue #30).
    ///
    /// **Read `XpFlash`'s doc before treating this as a parity value** — 26.2
    /// has no XP-bar flash, and this is the effect the issue asked for rather
    /// than a port of one.
    xp_flash: f32,
}

impl HudAnim {
    const NONE: Self = Self {
        heart_blink: false,
        display_health: i32::MIN, // unused while `heart_blink` is false and jitter is skipped
        tick: 0,
        hotbar_pop: [0.0; 9],
        xp_flash: 0.0,
    };
}

/// Draw the survival vitals cluster — hotbar frame, selection highlight, XP bar
/// (background + progress), hearts, and hunger — from the vanilla GUI atlas.
/// Returns `bars_y`, the top of the hearts/hunger row, which the action bar sits
/// above. Layout mirrors the procedural fallback closely so toggling the atlas
/// on or off does not visibly jump the HUD. A no-op-safe: [`Builder::sprite`]
/// draws nothing for a missing sprite, so a partial atlas degrades gracefully.
fn sprite_vitals(b: &mut Builder, frame: &HudFrame, anim: &HudAnim) -> f32 {
    // Native sprite pixels, laid straight into `b.w`/`b.h` — the
    // already-scale-divided logical canvas `HudGeometry::build_inner` computes
    // via `logical_canvas`. This used to hardcode a ×2 ("vanilla GUI Scale 2")
    // on every sprite dimension here, from before there was any real scale
    // computation; now that the canvas itself is divided by the *actual*
    // effective scale, that hardcode would double-apply it — sprites here would
    // render at 2× the size of the hotbar cells and text around them, which are
    // laid out in plain logical pixels with no such multiplier. Dropping it is
    // what keeps this cluster at the same visual size as everything else at any
    // scale, not just the one this used to assume. At an integer scale the atlas
    // sampler's Nearest magnification still replicates texels exactly, so
    // on-screen pixels equal jar pixels — which the GPU gate checks.
    let white = [1.0, 1.0, 1.0, 1.0];
    let cx = b.w * 0.5;

    // Hotbar (182x22 native), centred at the bottom, with the 24x23 selection
    // sprite over the chosen slot.
    let hw = 182.0;
    let hh = 22.0;
    let hx = cx - hw * 0.5;
    let hy = b.h - hh - HOTBAR_MARGIN;
    if let Some(sel) = frame.hotbar {
        b.sprite("hud/hotbar", hx, hy, hw, hh, white);
        // Vanilla draws the selection at native offset (slot*20 - 1, -1) from the
        // hotbar origin; the sprite is 24x23 so it overhangs the 20px slot pitch.
        //
        // **The vertical asymmetry is vanilla's, and a report that the bottom edge
        // is "cut off" is a faithful absence rather than a defect.** Both blits
        // from `Hud.extractItemHotbar`, verbatim: the bar at
        // `(centre - 91, guiHeight - 22, 182, 22)` and the selection at
        // `(centre - 91 - 1 + slot * 20, guiHeight - 22 - 1, 24, 23)`. PNG headers
        // read out of the 26.2 jar agree — `hud/hotbar` is 182x22 and
        // `hud/hotbar_selection` is 24x23. So the bar occupies rows `H-22..H-1` and
        // the selection `H-23..H-1`: **one pixel of overhang at the top and none at
        // the bottom, because 23 = 22 + 1 and the offset is -1 on the top only.**
        // There is no bottom overhang to lose, at any GUI scale, and this holds in
        // exact integer arithmetic — so neither rasterisation rounding nor a clip
        // rect can be blamed for it. The relationship below (`sh = 23.0` at
        // `hy - 1.0` over an `hh = 22.0` bar at `hy`) is the same one.
        //
        // The bottom margin that used to float this 6 px up is now
        // [`HOTBAR_MARGIN`] — zero, matching the blits quoted above. That is a
        // separate fix from this asymmetry and the two should not be conflated:
        // the asymmetry is vanilla's and stays.
        let sel = sel.min(8) as f32;
        let sw = 24.0;
        let sh = 23.0;
        let sx = hx + sel * 20.0 - 1.0;
        let sy = hy - 1.0;
        b.sprite("hud/hotbar_selection", sx, sy, sw, sh, white);
    }

    // XP bar (182x5), just above the hotbar: full background, then the progress
    // sprite cropped left-to-right to its filled fraction.
    //
    // The gap above the hotbar is vanilla's own arithmetic, not a guess:
    // `ContextualBar.MARGIN_BOTTOM` (24) is the hotbar's 22px height plus a 2px
    // gap, and `ContextualBar.top` is `guiScaledHeight - MARGIN_BOTTOM - HEIGHT`
    // (`ContextualBar.java`) — i.e. the bar sits *2px* above the
    // hotbar sprite, not 4. `hy` is already this cluster's hotbar-top in the
    // same logical-pixel space vanilla's `guiHeight` is in, so subtracting from
    // it (rather than restating an absolute `b.h`-based constant) is what keeps
    // this correct if the cluster's own bottom margin ever changes — the same
    // "derive from the expression the draw uses" rule the XP number below now
    // follows too.
    let bar_w = 182.0;
    let bar_h = 5.0;
    // With [`HOTBAR_MARGIN`] at vanilla's zero this resolves to
    // `guiHeight - 22 - 5 - 2 == guiHeight - 29`, which is exactly
    // `ContextualBar.top`'s `guiScaledHeight - 24 - 5`. It was 6 px off before,
    // for the single reason that `hy` was.
    //
    // The bar no longer feeds anything else's placement. It used to raise a
    // `cluster_top` that the hearts row stacked off, which made the hearts move
    // depending on whether the player had XP — vanilla's `yLineBase` is a
    // constant (see [`VITALS_LINE_BASE_FROM_BOTTOM`]) and takes no such branch.
    let xp_top = frame.xp.map(|_| hy - bar_h - 2.0);
    // `nextContextualInfoState` reaches `ContextualInfo.EXPERIENCE` only when
    // `gameMode.hasExperience()`, so creative and spectator draw neither the bar nor
    // the level number — see [`HudFrame::can_hurt_player`].
    if let (true, Some((level, progress)), Some(by)) = (frame.can_hurt_player, frame.xp, xp_top) {
        b.sprite("hud/experience_bar_background", hx, by, bar_w, bar_h, white);
        let p = progress.clamp(0.0, 1.0);
        if p > 0.0 {
            // Crop by shrinking both the destination width and the sampled UV
            // span, so the bar reveals its pattern instead of squashing it.
            //
            // The level-up flash (issue #30) rides the *fill*'s vertex tint. The
            // sprite is already near-white, so the visible part of the effect is
            // the level number below; brightening the fill too is what stops the
            // number looking like it flashed on its own. `white` unchanged at
            // `xp_flash == 0.0`, so an idle frame is byte-identical to before.
            let fill = anim::flash_toward_white(white, anim.xp_flash);
            for mut q in b.gui_geometry("hud/experience_bar_progress", hx, by, bar_w, bar_h) {
                let span = q.uv_max[0] - q.uv_min[0];
                q.dst[2] *= p;
                q.uv_max[0] = q.uv_min[0] + span * p;
                b.push_sprite_quad(q, fill);
            }
        }
        // The level number (vanilla green), centred above the bar.
        //
        // Player report: "the xp bar number is too big and too high." Both
        // were real, and both were this block:
        //
        // * **Too big** — `scale` was `2.0`. This function already draws in
        //   the scale-divided logical canvas (see the doc comment atop
        //   `sprite_vitals`), the same space the 182px-wide bar itself is laid
        //   out in, so a ×2 on the text alone made it twice vanilla's size
        //   relative to everything around it. Vanilla's own draw
        //   (`ContextualBar.extractExperienceLevel`, below) never scales the
        //   font at all.
        // * **Too high** — `by - line_h` used a *font-metrics* gap
        //   (`(GLYPH_H + 2) * scale`, i.e. 20px at the old scale of 2), not
        //   vanilla's real one. `ContextualBar.extractExperienceLevel`
        //   (`ContextualBar.java`) places the text at
        //   `y = guiHeight - 24 - 9 - 2`, and the bar itself sits at
        //   `guiHeight - 24 - 5`: the text's top is exactly `6` logical px
        //   above the bar's top, full stop — not a value derived from glyph
        //   height. Written as `by - 6.0` here for the same reason the bar
        //   gap above is written from `hy` rather than restated: it is the one
        //   expression that cannot drift out of sync with where the bar
        //   itself actually landed.
        //
        // Vanilla also does not use its usual single-shadow text path here: it
        // calls `graphics.text(font, str, x, y, colour, false)` — shadow
        // `false` — **five** times: four unshadowed black copies offset ±1px
        // on each axis (the outline), then one unshadowed copy in
        // `0x80FF20` (`ContextualBar.java`). `Builder::text` would add
        // its own automatic drop shadow on top of a hand-rolled outline, so
        // this uses [`Builder::text_plain`] for all five passes, matching
        // vanilla's `shadow = false` exactly.
        if level > 0 {
            let s = level.to_string();
            let tw = b.text_width(&s, 1.0);
            let tx = cx - tw * 0.5;
            let ty = by - 6.0;
            let black = [0.0, 0.0, 0.0, 1.0];
            // `0x80FF20` (`ARGB.color(255, 0x80, 0xFF, 0x20)`), the literal
            // vanilla constant `-8323296` reinterpreted as unsigned ARGB —
            // brightened toward white for the level-up flash's duration (issue
            // #30). The mix is in this raw-byte space on purpose; see
            // `anim::flash_toward_white`.
            let green = anim::flash_toward_white([128.0 / 255.0, 1.0, 32.0 / 255.0, 1.0], anim.xp_flash);
            b.text_plain(&s, tx + 1.0, ty, 1.0, black);
            b.text_plain(&s, tx - 1.0, ty, 1.0, black);
            b.text_plain(&s, tx, ty + 1.0, 1.0, black);
            b.text_plain(&s, tx, ty - 1.0, 1.0, black);
            b.text_plain(&s, tx, ty, 1.0, green);
        }
    }

    // Hearts (health) left, hunger right, one row above the cluster. Each icon
    // is 9x9 native, stepped 8px (vanilla spacing); a container/empty backing is
    // drawn first, then a full or half overlay per two points.
    // All three rows sit behind vanilla's single `canHurtPlayer()` gate on
    // `extractPlayerHealth` — hearts, hunger and the bubble row are drawn by that one
    // call, so creative and spectator show none of them. See
    // [`HudFrame::can_hurt_player`].
    let icon = 9.0;
    let step = 8.0;
    // `yLineBase`, from vanilla's own expression rather than by stacking upward
    // from the hotbar. See [`vitals_line_base`]: this used to be
    // `cluster_top - icon - 4.0`, and `cluster_top` moved with the XP bar, so the
    // hearts landed on two different rows depending on the player's game mode and
    // on neither of vanilla's.
    let row_y = vitals_line_base(b.h);

    // The armour row, one 10px line **above** the hearts and sharing their left
    // anchor — `extractArmor`'s `xo = xLeft + i * 8` against the hearts' own
    // `xLeft`, and `yLineArmor = yLineBase - (numHealthRows - 1) * healthRowHeight
    // - 10`.
    //
    // `numHealthRows` is `ceil((maxHealth + absorption) / 2 / 10)`, i.e. **1** for
    // every player with vanilla's 20 max health and no absorption, which collapses
    // that term to zero and leaves a flat `-10`. `HudFrame` carries neither max
    // health nor absorption (see the hearts' own critical-jitter note right below,
    // which narrows the same way), so this is a documented narrowing rather than a
    // silent one: a player with a raised max health would get a second heart row in
    // vanilla and push their armour row further up, and ours will not until those
    // two fields exist. [`VITALS_ROW_PITCH`] is that 10, shared with the air row
    // below so the two cannot drift apart.
    //
    // Drawn *before* the hearts because vanilla's `extractArmor` call precedes
    // `extractHearts`, and left as a separate `if` rather than folded into the
    // hearts' block because vanilla gates it on `armor > 0` alone — a player with
    // armour and no health packet yet still has an armour row.
    if frame.can_hurt_player
        && let Some(armour) = frame.armour
        && armour > 0
    {
        let armour_row_y = row_y - VITALS_ROW_PITCH;
        for i in 0..10 {
            let x = hx + i as f32 * step;
            b.sprite(
                armour_icon(i, armour).sprite_id(),
                x,
                armour_row_y,
                icon,
                icon,
                white,
            );
        }
    }

    if frame.can_hurt_player && let Some(hp) = frame.health {
        let hp = hp.max(0.0);
        let current = hp.ceil() as i32;
        // The container background flashes to the "_blinking" sprite variant
        // for the same alternating windows the ghost overlay below uses —
        // vanilla draws it for *every* container regardless of that
        // container's own fill state (`Hud.java`).
        let container = if anim.heart_blink {
            "hud/heart/container_blinking"
        } else {
            "hud/heart/container"
        };
        // Critical-health y-jitter (`Hud.java`): `currentHealth +
        // absorption <= 4`. Absorption is not modelled in `HudFrame` yet, so
        // this gates on health alone — a documented narrowing, not a silent
        // one.
        let critical = current <= 4;
        for i in 0..10 {
            let x = hx + i as f32 * step;
            let y = if critical {
                row_y + anim::heart_jitter(anim.tick, i)
            } else {
                row_y
            };
            b.sprite(container, x, y, icon, icon, white);
            // The "ghost" of health about to be lost, forced onto the
            // blinking sprite variant regardless of the fill state below —
            // vanilla's `blink && halves < oldHealth` (`Hud.java`).
            let halves = i * 2;
            if anim.heart_blink && (halves as i32) < anim.display_health {
                let half = (halves as i32 + 1) == anim.display_health;
                let ghost = if half {
                    "hud/heart/half_blinking"
                } else {
                    "hud/heart/full_blinking"
                };
                b.sprite(ghost, x, y, icon, icon, white);
            }
            // The fill, on vanilla's **integer** frontier rather than the raw
            // float — see [`heart_fill`], which is where the `Mth.ceil` and the
            // two integer comparisons live and where the live "0 hearts but still
            // alive" report is written up. Extracted rather than inlined because
            // the *composition* (ceil, then frontier) is the thing that was wrong
            // and an unnamed composition has nothing to point a gate at: the ghost
            // overlay directly above already used the integer `halves + 1 ==`
            // shape while this row compared floats, and nothing could see that the
            // two rows of one loop had come apart.
            if let Some(fill) = heart_fill(i, hp) {
                b.sprite(fill.sprite_id(), x, y, icon, icon, white);
            }
        }
    }
    if frame.can_hurt_player && let Some(food) = frame.food {
        let food_f = food.max(0) as f32;
        // Hunger-empty wobble (`Hud.java`): `frame.saturation` is
        // `None` off a build that has not wired it through yet (see
        // `HudFrame::saturation`'s doc) — treated as "not empty", so the row
        // stays flush rather than guessing.
        let saturation = frame.saturation.unwrap_or(1.0);
        for i in 0..10 {
            // Hunger fills right-to-left in vanilla.
            let x = hx + hw - icon - i as f32 * step;
            let y = row_y + anim::hunger_wobble(anim.tick, food, saturation, i);
            b.sprite("hud/food_empty", x, y, icon, icon, white);
            let units = food_f - i as f32 * 2.0;
            if units >= 2.0 {
                b.sprite("hud/food_full", x, y, icon, icon, white);
            } else if units >= 1.0 {
                b.sprite("hud/food_half", x, y, icon, icon, white);
            }
        }
    }

    // Air bubbles, one row above hearts/hunger, on the same right edge
    // (`hx + hw`) the hunger row uses — vanilla's shared `xRight`.
    //
    // # `yLineBase - 10` is the whole answer, and reading only
    // # `extractPlayerHealth` says otherwise
    //
    // Three terms, and the third cancels the second. Hand-expanded from the
    // 26.2 source for a player with no mounted vehicle, `H == guiHeight`:
    //
    // | step | in | out |
    // |---|---|---|
    // | `extractPlayerHealth`: `yLineAir = yLineBase - 10` | `H-39` | `H-49` |
    // | `if (vehicleHearts == 0) { extractFood(…); yLineAir -= 10; }` | `H-49` | `H-59` |
    // | `extractAirBubbles` → `getAirBubbleYLine(0, H-59)` | `H-59` | **`H-49`** |
    //
    // The last row is the one that is easy to miss: `getAirBubbleYLine` computes
    // `rowOffset = getVisibleVehicleHeartRows(hearts) - 1`, and
    // `getVisibleVehicleHeartRows(0)` is `ceil(0 / 10.0) == 0`, so `rowOffset` is
    // **-1** and `yLineAir - rowOffset * 10` *adds* the ten straight back. The
    // second subtraction is real but unobservable without a vehicle; its purpose
    // is the mounted case, where no food row draws (`vehicleHearts != 0`) and a
    // 20-heart mount gives `rowOffset == 1`, moving the bubbles up to `H-59` to
    // clear the vehicle-health row that replaced the food.
    //
    // So the bubbles share a line with the armour row — armour on the left
    // (`xLeft`), bubbles on the right (`xRight`) — which is what vanilla looks
    // like. A "correction" to `yLineBase - 20` reads as obviously right from
    // `extractPlayerHealth` alone and is wrong; this table is here so the next
    // person re-derives it rather than re-deciding it.
    //
    // Mounted vehicles are not modelled (`HudFrame` carries no vehicle), so the
    // `rowOffset >= 1` branch has nothing to drive it — a documented narrowing.
    if frame.can_hurt_player && let Some((air, max_air, eye_in_water)) = frame.air {
        let air_row_y = row_y - VITALS_ROW_PITCH;
        // `wobble` is vanilla's `tickCount % 2 == 0` (a 0/1px jitter vanilla
        // applies to a fully-empty row's last bubble) — no per-frame tick
        // parity is piped into `HudFrame` yet, so this always reads `false`.
        // Purely cosmetic, deliberately left unwired rather than approximated.
        let wobble = false;
        for (i, slot) in bubble_row(air, max_air, eye_in_water, wobble)
            .into_iter()
            .enumerate()
        {
            let Some(sprite_id) = slot.sprite_id() else {
                continue;
            };
            let (x, y) = bubble_position(i, hx + hw, air_row_y);
            b.sprite(sprite_id, x, y, BUBBLE_SIZE, BUBBLE_SIZE, white);
        }
    }

    row_y
}

/// One composite layer of the suggestion popup, in the order
/// [`SUGGESTION_LAYERS`] lists them.
///
/// Split out because the popup has to sit *over* the chat scrollback and *under*
/// its own tooltip, and a bare call sequence records neither: the ordering is
/// data here, so a reader can see it and a future layer can be inserted at a
/// named position instead of by moving a statement. This is the popup's own
/// order and nothing wider — see [`draw_command_suggestions`]'s doc for what
/// must composite above the whole widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SuggestionLayer {
    /// The per-row translucent fills — `graphics.fill(..., fillColor)`.
    RowFills,
    /// The 1px dotted edges that appear only when the list is scrollable.
    ScrollHints,
    /// The candidate texts, highlighted row in yellow.
    RowTexts,
    /// The hovered candidate's `Message`, when it has one.
    Tooltip,
}

/// The popup's composite order, bottom-most first.
///
/// `RowFills` before `ScrollHints` because the hints draw a `fillColor` band in
/// the 1px gutters *outside* the rows and then stipple white over it, so a row
/// fill submitted afterwards would paint over the stipple where they abut.
const SUGGESTION_LAYERS: [SuggestionLayer; 4] = [
    SuggestionLayer::RowFills,
    SuggestionLayer::ScrollHints,
    SuggestionLayer::RowTexts,
    SuggestionLayer::Tooltip,
];

/// Draw the command-suggestion dropdown — `SuggestionsList.extractRenderState`.
///
/// # What must composite above this, and why the call site is where it is
///
/// The popup is submitted **after** the chat input line and the whole
/// scrollback, because it overlaps both. Everything that must still sit on top
/// of it, in the order it draws:
///
/// | above the popup | why |
/// |---|---|
/// | this widget's own tooltip | [`SuggestionLayer::Tooltip`], last in [`SUGGESTION_LAYERS`] |
/// | the F3 debug overlay | vanilla draws `DebugScreenOverlay` after every screen |
/// | a container screen's cursor stack and item tooltip | separate pass, separate geometry type — see `container.rs` |
///
/// Only the first is this function's business; the other two composite in later
/// passes and need nothing from here. **The F3 overlay is currently submitted
/// *first* in [`HudGeometry::build_inner`] and therefore draws underneath**,
/// which is a pre-existing divergence for the whole HUD rather than one this
/// widget introduces — named here because this is the table a reader will check.
fn draw_command_suggestions(
    b: &mut Builder,
    popup: &SuggestionPopup<'_>,
    layout: SuggestionLayout,
    pose_scale: f32,
    layers: &[SuggestionLayer],
) {
    if layout.rows == 0 {
        return;
    }
    let px = pose_scale.max(1.0);
    let has_previous = popup.offset > 0;
    let has_next = popup.candidates.len() > popup.offset + layout.rows;
    for layer in layers {
        match layer {
            SuggestionLayer::RowFills => {
                for i in 0..layout.rows {
                    b.rect_px(
                        layout.x,
                        layout.y + layout.row_h * i as f32,
                        layout.w,
                        layout.row_h,
                        SUGGESTION_FILL,
                    );
                }
            }
            // Vanilla draws a `fillColor` band in the 1px gutter above *and*
            // below whenever the list is scrollable **either** way, then
            // stipples white into whichever end has more rows behind it —
            // `if (limited)` covers both bands, and the two `if`s inside it
            // cover one each. The asymmetry is deliberate: the band alone is
            // what makes the box look clipped rather than ended.
            SuggestionLayer::ScrollHints if has_previous || has_next => {
                b.rect_px(layout.x, layout.y - px, layout.w, px, SUGGESTION_FILL);
                b.rect_px(layout.x, layout.y + layout.h, layout.w, px, SUGGESTION_FILL);
                let white = [1.0, 1.0, 1.0, 1.0];
                // `for (x = 0; x < width; x++) if (x % 2 == 0)` — every other
                // pixel column, so the stipple pitch scales with the box.
                let mut x = 0.0;
                while x < layout.w {
                    if has_previous {
                        b.rect_px(layout.x + x, layout.y - px, px, px, white);
                    }
                    if has_next {
                        b.rect_px(layout.x + x, layout.y + layout.h, px, px, white);
                    }
                    x += 2.0 * px;
                }
            }
            SuggestionLayer::ScrollHints => {}
            SuggestionLayer::RowTexts => {
                for i in 0..layout.rows {
                    let Some(candidate) = popup.candidates.get(popup.offset + i) else {
                        continue;
                    };
                    let colour = if popup.offset + i == popup.selected {
                        SUGGESTION_TEXT_SELECTED
                    } else {
                        SUGGESTION_TEXT_UNSELECTED
                    };
                    b.text(
                        &candidate.text,
                        layout.x + SUGGESTION_TEXT_INSET * pose_scale,
                        layout.y + layout.row_h * i as f32 + SUGGESTION_ROW_PAD_TOP * pose_scale,
                        pose_scale,
                        colour,
                    );
                }
            }
            // `graphics.setTooltipForNextFrame(font, fromMessage(tooltip),
            // mouseX, mouseY)`, gated on `hovered` — so it is the *pointer*
            // that reveals a tooltip, never the keyboard selection, and it
            // shows the **selected** row's message rather than the hovered
            // one's (they are the same row whenever the pointer moved, which is
            // the only way `hovered` becomes true with a stale selection).
            //
            // Only the placement and the text are ported. Vanilla's
            // `TooltipRenderUtil` border gradient is not modelled; this is a
            // flat panel, and that is a cosmetic narrowing rather than a
            // behavioural one.
            SuggestionLayer::Tooltip => {
                let Some((mx, my)) = popup.cursor else {
                    continue;
                };
                if !layout.contains(mx, my) {
                    continue;
                }
                let Some(text) = popup
                    .candidates
                    .get(popup.selected)
                    .and_then(|c| c.tooltip.as_deref())
                else {
                    continue;
                };
                let pad = 3.0 * pose_scale;
                let tw = b.text_width(text, pose_scale);
                let th = font::GLYPH_H as f32 * pose_scale;
                // `renderTooltip`'s own offset from the cursor.
                let tx = (mx + 12.0 * pose_scale).min((b.w - tw - pad * 2.0).max(0.0));
                let ty = (my - 12.0 * pose_scale).max(0.0);
                b.rect_px(tx, ty, tw + pad * 2.0, th + pad * 2.0, SUGGESTION_FILL);
                b.text(text, tx + pad, ty + pad, pose_scale, [1.0, 1.0, 1.0, 1.0]);
            }
        }
    }
}

/// A chat line is fully lit for most of its life, then fades over its last
/// [`CHAT_FADE_SECS`] before disappearing at [`CHAT_VISIBLE_SECS`] — matching
/// vanilla's "recent messages fade out when the box is closed" behaviour. Only
/// used while the chat box is closed; open, every line is drawn at full alpha.
fn chat_line_alpha(age: f32) -> f32 {
    const CHAT_VISIBLE_SECS: f32 = 10.0;
    const CHAT_FADE_SECS: f32 = 2.0;
    if age <= CHAT_VISIBLE_SECS - CHAT_FADE_SECS {
        1.0
    } else if age >= CHAT_VISIBLE_SECS {
        0.0
    } else {
        (CHAT_VISIBLE_SECS - age) / CHAT_FADE_SECS
    }
}

/// The visible characters of a legacy `§`-coded string: each `§`+selector pair
/// is dropped. Both text paths draw codes zero-width, so measuring the raw
/// string over-counts by two characters per code and pushes centred lines left.
fn strip_legacy(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch == '\u{00a7}' {
            if chars.next().is_none() {
                break;
            }
            continue;
        }
        out.push(ch);
    }
    out
}

/// Greedy word-wrap of a legacy `§`-coded line into rows that each fit
/// `max_width_px`, measured by calling `measure` on each candidate row.
/// [`Builder::wrap_legacy`] binds `measure` to real vanilla proportional
/// glyph advances (when a [`VanillaFont`] is attached) or the fixed 5×7
/// advance otherwise — this free function takes the measure as a parameter
/// precisely so its wrap *decisions* can be tested against a hand-specified
/// width table with no `Builder`, atlas, or jar involved.
///
/// Mirrors vanilla's own reflow in shape (`GuiMessage.splitLines`, invoked
/// from `ChatComponent.addMessageToDisplayQueue`, `ChatComponent.java`):
/// break on a space when the next word would overflow, and hard-break a
/// single word that alone exceeds the width so nothing can escape the box. A
/// `§` colour/format code seen before a break is carried onto the
/// continuation line, because a code resets formatting to just itself
/// (`Text::from_legacy`'s legacy semantics) — tracking only the single most
/// recent one is therefore sufficient to keep the colour continuous across
/// the wrap.
///
/// Never returns an empty vector: an empty `s` yields one empty row, and a
/// `max_width_px <= 0.0` (or a line that already fits) is returned as a
/// single unwrapped row rather than looping forever trying to shrink it.
///
/// Builds every candidate row **in place** in one reusable buffer, pushing and
/// truncating rather than `format!`-ing a fresh `String` per word (and per
/// character on a hard break) — the wrap *decisions* are identical, only the
/// allocation count changes. `pending_code` is likewise the selector `char`
/// alone rather than an owned two-character `String`. Combined with
/// [`ChatWrapCache`], which persists the result so a frame with no new message
/// re-wraps nothing at all, this is issue #527's half (a).
fn wrap_legacy_with(measure: impl Fn(&str) -> f32, s: &str, max_width_px: f32) -> Vec<String> {
    if max_width_px <= 0.0 || measure(s) <= max_width_px {
        return vec![s.to_string()];
    }
    let mut rows = Vec::new();
    let mut current = String::new();
    let mut pending_code: Option<char> = None;
    // Flush `current` as a finished row and re-seed the buffer with the colour
    // code in force, keeping `current`'s allocation across rows.
    let flush = |rows: &mut Vec<String>, current: &mut String, code: Option<char>| {
        rows.push(current.clone());
        current.clear();
        if let Some(c) = code {
            current.push('\u{00a7}');
            current.push(c);
        }
    };
    for word in s.split(' ') {
        // The last `§`+selector pair inside this word, if any — what a
        // continuation line started *after* this word must be seeded with to
        // keep reading the same colour.
        let mut word_pending = pending_code;
        let mut chars = word.chars();
        while let Some(ch) = chars.next() {
            if ch == '\u{00a7}' {
                if let Some(code) = chars.next() {
                    word_pending = Some(code);
                }
            }
        }

        let before_word = current.len();
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
        if measure(&current) <= max_width_px {
            pending_code = word_pending;
            continue;
        }
        current.truncate(before_word);
        if !current.is_empty() {
            flush(&mut rows, &mut current, pending_code);
        }
        let seed_len = current.len();
        current.push_str(word);
        if measure(&current) > max_width_px {
            // The word alone overflows even a fresh line: hard-break it
            // character by character. `§`/selector characters are
            // zero-width, so they never trigger a break by themselves.
            current.truncate(seed_len);
            for ch in word.chars() {
                let was_empty = current.is_empty();
                current.push(ch);
                if !was_empty && measure(&current) > max_width_px {
                    current.pop();
                    flush(&mut rows, &mut current, pending_code);
                    current.push(ch);
                }
            }
        }
        pending_code = word_pending;
    }
    rows.push(current);
    rows
}

/// Persisted wrapped-row cache for the chat log — vanilla's `GuiMessage.Line`s,
/// which `GuiMessage.splitLines` fills **once, when the message arrives**
/// (`ChatComponent.addMessageToDisplayQueue`) rather than once per frame.
///
/// ## What it is
///
/// The wrap is a pure function of `(display text, chat box width, chat pose
/// scale, font)`. The text changes on a `SYSTEM_CHAT`/`PLAYER_CHAT` packet; the
/// width and scale change on a resize or an options edit. Nothing in that set
/// changes per frame, so without a cache the whole log is re-wrapped every
/// frame — the defect issue #527 (a) reports.
///
/// ## How to change it
///
/// The geometry key is the whole invalidation story: any *new* input the wrap
/// starts depending on must join `width`/`scale` here, or the cache will serve
/// a stale layout. The font is not keyed because a resource reload rebuilds the
/// owning `App`. Entries are `Rc<[String]>` so a hit is a refcount bump rather
/// than a per-row `String` clone; the map is cleared wholesale when it grows
/// past [`Self::MAX_ENTRIES`] rather than evicted by age, which is adequate for
/// a bounded chat log and keeps the type free of ordering state.
#[derive(Debug, Default)]
pub struct ChatWrapCache {
    inner: std::cell::RefCell<ChatWrapInner>,
}

#[derive(Debug, Default)]
struct ChatWrapInner {
    /// The geometry the cached rows were wrapped for. `None` before the first
    /// wrap; any mismatch clears `rows`.
    geometry: Option<(u32, u32)>,
    rows: std::collections::HashMap<String, std::rc::Rc<[String]>>,
}

impl ChatWrapCache {
    /// Cleared wholesale past this many distinct lines.
    const MAX_ENTRIES: usize = 256;

    /// The wrapped rows for `text` at this geometry, computing them with `wrap`
    /// only on a miss.
    fn rows(
        &self,
        text: &str,
        width_px: f32,
        scale: f32,
        wrap: impl FnOnce(&str) -> Vec<String>,
    ) -> std::rc::Rc<[String]> {
        // Bit patterns, not the floats: the key must be `Hash`/`Eq`, and an
        // exact-bits comparison is the right test here anyway — these are
        // recomputed from the same expressions every frame, so equal geometry
        // is bit-equal geometry.
        let geometry = (width_px.to_bits(), scale.to_bits());
        let mut inner = self.inner.borrow_mut();
        if inner.geometry != Some(geometry) {
            inner.geometry = Some(geometry);
            inner.rows.clear();
        }
        if let Some(hit) = inner.rows.get(text) {
            return std::rc::Rc::clone(hit);
        }
        if inner.rows.len() >= Self::MAX_ENTRIES {
            inner.rows.clear();
        }
        let rows: std::rc::Rc<[String]> = wrap(text).into();
        inner.rows.insert(text.to_string(), std::rc::Rc::clone(&rows));
        rows
    }
}

#[cfg(test)]
mod chat_wrap_cache_tests {
    use super::ChatWrapCache;

    /// The whole point of the cache: a repeat frame with the same line at the
    /// same geometry must not call the wrapper again, and a geometry change
    /// must. The counter is the assertion — a test that only compared the
    /// returned rows would pass with no cache at all.
    #[test]
    fn a_repeat_line_at_the_same_geometry_wraps_exactly_once() {
        let cache = ChatWrapCache::default();
        let wraps = std::cell::Cell::new(0usize);
        let wrap = |_: &str| {
            wraps.set(wraps.get() + 1);
            vec!["hello".to_string(), "world".to_string()]
        };

        let first = cache.rows("hello world", 80.0, 1.0, &wrap);
        assert_eq!(wraps.get(), 1, "the first call must wrap");
        let second = cache.rows("hello world", 80.0, 1.0, &wrap);
        assert_eq!(wraps.get(), 1, "the second call at the same geometry must not");
        assert_eq!(&*first, &*second);

        // A different line at the same geometry is a genuine miss.
        let _ = cache.rows("another line", 80.0, 1.0, &wrap);
        assert_eq!(wraps.get(), 2);

        // A resize or a chat-scale change invalidates everything: the same
        // text must be re-wrapped at the new width.
        let _ = cache.rows("hello world", 120.0, 1.0, &wrap);
        assert_eq!(wraps.get(), 3, "a width change must invalidate the cache");
        let _ = cache.rows("hello world", 120.0, 2.0, &wrap);
        assert_eq!(wraps.get(), 4, "a scale change must invalidate the cache");
    }
}

/// The RGB of one of the sixteen legacy `§` colour codes (`0`..=`9`, `a`..=`f`),
/// or `None` for a format/reset code. These are the standard Minecraft chat
/// foreground colours; the shell paints them locally, which is a rendering
/// concern (how to colour a run), not protocol knowledge.
///
/// This used to hold its own transcription of the sixteen hex constants. It now
/// delegates to [`TextColor::rgb`], which is the same table sourced from
/// vanilla's `TextColor.java` — one copy, so the two cannot drift, and so a
/// `TextColor`-carrying draw path (`vanilla_font::draw_spans`) and this
/// `§`-carrying one are guaranteed to agree on what "gold" means.
fn legacy_rgb(code: char) -> Option<[f32; 3]> {
    TextColor::from_legacy_code(code).map(vanilla_font::text_color_rgb)
}

struct Builder<'a> {
    w: f32,
    h: f32,
    verts: Vec<f32>,
    sprite_verts: Vec<f32>,
    item_verts: Vec<f32>,
    /// The enchantment-glint copies of `item_verts`; see [`IconSink::glint`].
    glint_verts: Vec<f32>,
    model_verts: Vec<ModelVertex>,
    /// Special-renderer (block-entity) icons; see [`HudGeometry::special`].
    special: Vec<SpecialIconDraw>,
    gui: Option<&'a GuiAtlas>,
    items: Option<&'a ItemAtlas>,
    /// The baked model set, for items whose inventory icon is a 3-D mini-block
    /// rather than a flat sprite. `None` on jar-less / demo runs, where those
    /// slots stay empty wells exactly as before.
    models: Option<&'a BlockModels>,
    /// The vanilla proportional font. `None` on jar-less / demo runs and in every
    /// pure `HudGeometry::build*` call, where text falls back to the fixed-advance
    /// 5×7 debug font. Measurement and drawing read the *same* field, so a layout
    /// can never be computed against a font other than the one that draws.
    font: Option<&'a VanillaFont>,
}

impl<'a> Builder<'a> {
    fn new(
        w: f32,
        h: f32,
        gui: Option<&'a GuiAtlas>,
        items: Option<&'a ItemAtlas>,
        models: Option<&'a BlockModels>,
        font: Option<&'a VanillaFont>,
    ) -> Self {
        Self {
            w,
            h,
            verts: Vec::new(),
            sprite_verts: Vec::new(),
            item_verts: Vec::new(),
            glint_verts: Vec::new(),
            model_verts: Vec::new(),
            special: Vec::new(),
            gui,
            items,
            models,
            font,
        }
    }

    /// Pixel width of `s` at `scale` in whichever font [`Builder::text`] will
    /// draw with. Every centring and right-alignment site must use this.
    fn text_width(&self, s: &str, scale: f32) -> f32 {
        measure_text(self.font, s, scale)
    }

    /// Pixel width of a `§`-coded string at `scale`, codes counted as zero-width.
    fn legacy_width(&self, s: &str, scale: f32) -> f32 {
        match self.font {
            Some(f) => f.legacy_width(s, scale),
            None => item_icon::text_w(&strip_legacy(s), scale),
        }
    }

    /// Pixel width of a styled span list at `scale` — the measurement partner of
    /// [`text_spans`](Self::text_spans), so a right-aligned styled cell lands on
    /// the same pen positions the draw will use.
    fn spans_width(&self, spans: &[TextSpan], scale: f32) -> f32 {
        match self.font {
            Some(f) => f.spans_width(spans, scale),
            None => spans
                .iter()
                .map(|s| item_icon::text_w(&s.text, scale))
                .sum(),
        }
    }

    /// Greedy word-wrap of a legacy `§`-coded line into rows that each fit
    /// `max_width_px` at `scale`, measured with whichever metrics
    /// [`Builder::legacy_width`] reports — real vanilla proportional glyph
    /// advances when a [`VanillaFont`] is attached, the fixed 5×7 advance
    /// otherwise. Mirrors vanilla's own reflow in shape (`GuiMessage.splitLines`,
    /// invoked from `ChatComponent.addMessageToDisplayQueue`,
    /// `ChatComponent.java`): break on a space when the next word
    /// would overflow, and hard-break a single word that alone exceeds the
    /// width so nothing can escape the box. A `§` colour/format code seen
    /// before a break is carried onto the continuation line, because a code
    /// resets formatting to just itself
    /// (`Text::from_legacy`'s legacy semantics) — tracking only the single
    /// most recent one is therefore sufficient to keep the colour continuous
    /// across the wrap.
    ///
    /// Never returns an empty vector: an empty `s` yields one empty row, and a
    /// `max_width_px <= 0.0` (or a line that already fits) is returned as a
    /// single unwrapped row rather than looping forever trying to shrink it.
    ///
    /// A thin wrapper over [`wrap_legacy_with`] bound to this `Builder`'s own
    /// [`Builder::legacy_width`] — see that function for the algorithm. Kept
    /// separate so the wrap logic itself can be unit-tested against an
    /// injected width table (real proportional advances) without needing a
    /// GPU, an atlas, or a loaded jar.
    fn wrap_legacy(&self, s: &str, max_width_px: f32, scale: f32) -> Vec<String> {
        wrap_legacy_with(|t| self.legacy_width(t, scale), s, max_width_px)
    }

    /// Draw one hotbar slot's icon into the `size`×`size` rect at `(x, y)`: the
    /// icon itself, its durability bar, and its stack count.
    ///
    /// Delegates to the shared [`item_icon::draw_item_icon`], which is the one
    /// implementation the container screen also uses; see that module for how
    /// the two icon kinds reach two different streams.
    fn item_icon(&mut self, slot: &HotbarSlot, x: f32, y: f32, size: f32) {
        let assets = IconAssets {
            items: self.items,
            models: self.models,
        };
        let (w, h) = (self.w, self.h);
        let mut sink = IconSink {
            colour: ColourStream {
                verts: &mut self.verts,
                w,
                h,
            },
            sprite: &mut self.item_verts,
            model: &mut self.model_verts,
            special: &mut self.special,
            glint: &mut self.glint_verts,
        };
        item_icon::draw_item_icon(&mut sink, &assets, (w, h), slot, x, y, size, self.font);
    }

    /// As [`Builder::item_icon`], but the icon squashes/stretches through
    /// vanilla's pickup "pop" animation first — `pop` is
    /// `hud::anim::HotbarPop`'s `5.0 → 0.0` amount, `0.0` (idle) drawing
    /// pixel-identically to [`Builder::item_icon`] (see
    /// [`item_icon::draw_item_icon_popped`] for the vanilla citations).
    fn item_icon_popped(&mut self, slot: &HotbarSlot, x: f32, y: f32, size: f32, pop: f32) {
        let assets = IconAssets {
            items: self.items,
            models: self.models,
        };
        let (w, h) = (self.w, self.h);
        let mut sink = IconSink {
            colour: ColourStream {
                verts: &mut self.verts,
                w,
                h,
            },
            sprite: &mut self.item_verts,
            model: &mut self.model_verts,
            special: &mut self.special,
            glint: &mut self.glint_verts,
        };
        item_icon::draw_item_icon_popped(&mut sink, &assets, (w, h), slot, x, y, size, self.font, pop);
    }

    /// Emit a GUI sprite scaled into the pixel rect `(x, y, w, h)`, tinted by
    /// `c`. A no-op when no atlas is attached or the id is unknown, so callers
    /// need not branch.
    fn sprite(&mut self, id: &str, x: f32, y: f32, w: f32, h: f32, c: [f32; 4]) {
        for q in self.gui_geometry(id, x, y, w, h) {
            self.push_sprite_quad(q, c);
        }
    }

    /// The raw textured quads for a sprite, for callers that post-process them
    /// (for example cropping the XP progress bar to its filled fraction). Empty
    /// when no atlas is attached or the id is unknown.
    fn gui_geometry(&self, id: &str, x: f32, y: f32, w: f32, h: f32) -> Vec<GuiSpriteQuad> {
        match self.gui {
            Some(gui) => gui.geometry(id, x, y, w, h),
            None => Vec::new(),
        }
    }

    /// Push one textured quad (two triangles) from an absolute-pixel destination
    /// rect and its atlas UVs, tinted by `c`.
    fn push_sprite_quad(&mut self, q: GuiSpriteQuad, c: [f32; 4]) {
        item_icon::push_sprite_quad(&mut self.sprite_verts, self.w, self.h, q, c);
    }

    /// Emit a pixel-space rectangle as two triangles in NDC.
    fn rect_px(&mut self, x: f32, y: f32, w: f32, h: f32, c: [f32; 4]) {
        self.colour().rect(x, y, w, h, c);
    }

    /// A handle onto the colour stream, for the shared pixel-space primitives.
    fn colour(&mut self) -> ColourStream<'_> {
        ColourStream {
            w: self.w,
            h: self.h,
            verts: &mut self.verts,
        }
    }

    /// Emit a row of 10 pips representing a `0..=20` gauge (health/food). A pip
    /// lights once any of its two units is present; empty pips render as a dark
    /// slot so the gauge width reads at a glance.
    fn pips(&mut self, units: f32, x: f32, y: f32, pip: f32, gap: f32, full: [f32; 4]) {
        let empty = [0.12, 0.12, 0.14, 0.8];
        for i in 0..10 {
            let lit = units > (i as f32) * 2.0;
            let col = if lit { full } else { empty };
            self.rect_px(x + i as f32 * (pip + gap), y, pip, pip, col);
        }
    }

    /// Emit a string starting at pixel `(x, y)` (top-left of first glyph).
    ///
    /// With a [`VanillaFont`] attached this is vanilla text: proportional
    /// advances, real `ascii.png` glyphs and the 1 px drop shadow. Without one it
    /// is the fixed-advance 5×7 debug font, unshadowed, exactly as before.
    fn text(&mut self, s: &str, x: f32, y: f32, scale: f32, c: [f32; 4]) {
        match self.font {
            Some(f) => {
                let (w, h) = (self.w, self.h);
                f.draw(
                    &mut ColourStream {
                        verts: &mut self.verts,
                        w,
                        h,
                    },
                    s,
                    x,
                    y,
                    scale,
                    c,
                );
            }
            None => self.colour().text(s, x, y, scale, c),
        }
    }

    /// Emit a string with **no** drop shadow, the string's top-left at
    /// `(x, y)`. `ContextualBar.extractExperienceLevel`
    /// (`Hud.java`/`ContextualBar.java`) builds the XP level number's
    /// outline out of four unshadowed offset copies plus one unshadowed centre
    /// copy — passing `shadow = false` to `graphics.text` every time — so a
    /// caller reproducing that outline must use this, not [`text`](Self::text):
    /// `text` always adds vanilla's *automatic* 1px shadow on top of whatever
    /// is drawn, which would layer a second, unwanted shadow under the
    /// hand-rolled one. The fixed-advance debug font (no [`VanillaFont`]
    /// attached) was already unshadowed, so that branch is unchanged.
    fn text_plain(&mut self, s: &str, x: f32, y: f32, scale: f32, c: [f32; 4]) {
        match self.font {
            Some(f) => {
                let (w, h) = (self.w, self.h);
                f.draw_plain(
                    &mut ColourStream {
                        verts: &mut self.verts,
                        w,
                        h,
                    },
                    s,
                    x,
                    y,
                    scale,
                    c,
                );
            }
            None => self.colour().text(s, x, y, scale, c),
        }
    }

    /// Draw a single glyph with its top-left at `(x, y)`. Space and unknown
    /// handling match [`font::glyph_rows`]; blanks emit no quads.
    fn glyph(&mut self, ch: char, x: f32, y: f32, scale: f32, c: [f32; 4]) {
        self.colour().glyph(ch, x, y, scale, c);
    }

    /// Emit a string carrying legacy `§` colour/format codes as coloured runs.
    /// Colour codes (`§0`..=`§f`) recolour the following text; `§r` resets to
    /// `base`. With a [`VanillaFont`] attached, the five format codes
    /// (`§k`/`l`/`m`/`n`/`o`) draw real bold/italic/underline/strikethrough/
    /// obfuscated geometry (issue #117; see `hud/vanilla_font.rs`'s module
    /// docs). Without one — the fixed-advance debug font — they fall back to
    /// the pre-#117 behaviour: consumed, not styled, since that font has no
    /// styled glyph variants at all. Each code pair is **zero-width** either
    /// way (beyond whatever geometry the style itself adds, e.g. bold's `+1`
    /// advance), matching vanilla's "`§` codes are 2 chars / 0 width", so
    /// coloured and plain text of the same visible length line up exactly.
    /// `alpha` scales every run for the fade-out.
    fn text_legacy(&mut self, s: &str, x: f32, y: f32, scale: f32, base: [f32; 3], alpha: f32) {
        if let Some(f) = self.font {
            let (w, h) = (self.w, self.h);
            f.draw_legacy(
                &mut ColourStream {
                    verts: &mut self.verts,
                    w,
                    h,
                },
                s,
                x,
                y,
                scale,
                base,
                alpha,
            );
            return;
        }
        let advance = (font::GLYPH_W as f32 + 1.0) * scale;
        let mut cursor = x;
        let mut rgb = base;
        let mut chars = s.chars();
        while let Some(ch) = chars.next() {
            if ch == '\u{00a7}' {
                // A colour/format code: consume the following selector, adjust
                // state, and advance the cursor by nothing.
                match chars.next() {
                    Some(code) => {
                        if let Some(c) = legacy_rgb(code) {
                            rgb = c;
                        } else if code.eq_ignore_ascii_case(&'r') {
                            rgb = base;
                        }
                        // Format codes (k/l/m/n/o) and unknowns: swallowed.
                    }
                    None => break,
                }
                continue;
            }
            self.glyph(ch, cursor, y, scale, [rgb[0], rgb[1], rgb[2], alpha]);
            cursor += advance;
        }
    }

    /// Emit a list of styled spans as coloured runs — the structured twin of
    /// [`text_legacy`](Self::text_legacy).
    ///
    /// Prefer this over `text_legacy` for anything that starts life as a
    /// [`Text`](lodestone_model::text::Text). Flattening to a `§` string first
    /// is lossy in a way that is invisible at the call site: a
    /// [`TextColor::Rgb`] has no legacy code, so `Text::to_legacy_string`
    /// silently drops it and the run renders in `base`. Spans keep the
    /// `TextColor`, so the hex colours modern servers actually send survive to
    /// the quad.
    ///
    /// A span with no colour of its own draws in `base`; `alpha` scales every
    /// run, for fades.
    fn text_spans(
        &mut self,
        spans: &[TextSpan],
        x: f32,
        y: f32,
        scale: f32,
        base: [f32; 3],
        alpha: f32,
    ) {
        if let Some(f) = self.font {
            let (w, h) = (self.w, self.h);
            f.draw_spans(
                &mut ColourStream {
                    verts: &mut self.verts,
                    w,
                    h,
                },
                spans,
                x,
                y,
                scale,
                base,
                alpha,
            );
            return;
        }
        // Jar-less debug font: fixed advance, colour only. Mirrors
        // `text_legacy`'s fallback, which likewise cannot style a glyph.
        let advance = (font::GLYPH_W as f32 + 1.0) * scale;
        let mut cursor = x;
        for span in spans {
            let rgb = span.style.color.map_or(base, vanilla_font::text_color_rgb);
            for ch in span.text.chars() {
                self.glyph(ch, cursor, y, scale, [rgb[0], rgb[1], rgb[2], alpha]);
                cursor += advance;
            }
        }
    }
}

/// GPU renderer for the HUD: a simple coloured-quad pipeline plus a growable
/// dynamic vertex buffer.
#[derive(Debug)]
pub struct HudRenderer {
    pipeline: wgpu::RenderPipeline,
    buffer: wgpu::Buffer,
    capacity_floats: usize,
    gui: Option<GuiHud>,
    /// The flat item atlas and the 3-D block-item pass, shared verbatim with the
    /// container screen. Both halves start detached.
    icons: IconRenderer,
    /// The vanilla proportional font, resolved once per process from the same
    /// `client.jar` as the other atlases. `None` on a jar-less run, where the
    /// fixed-advance debug font draws instead.
    ///
    /// Unlike the atlases this needs **no GPU resources**, so it is resolved in
    /// [`HudRenderer::new`] rather than through an `attach_*` call — there is
    /// nothing for a caller to supply. [`HudRenderer::attach_font`] exists to
    /// override it (a resource pack, or a gate pinning a specific pack).
    font: Option<Arc<VanillaFont>>,
    /// The wall-clock origin every vitals animation's tick index is measured
    /// from — see `hud/anim.rs`'s module doc for why a wall clock stands in
    /// for the real 20Hz game tick here. Fixed at construction so a fresh
    /// renderer (a gate, a reconnect) starts at tick 0 rather than inheriting
    /// whatever the process's own uptime happens to be.
    anim_start: Instant,
    /// Cross-frame heart blink/ghost state (`hud/anim::HeartAnim`).
    heart_anim: anim::HeartAnim,
    /// Cross-frame per-slot hotbar pop timers (`hud/anim::HotbarPop`).
    hotbar_pop: anim::HotbarPop,
    /// Cross-frame level-up flash state (`hud/anim::XpFlash`, issue #30).
    xp_flash: anim::XpFlash,
    /// Colour-stream buffer for the **recipe-book panel** pass
    /// ([`HudRenderer::render_recipe_book_panel`]), created lazily on the first
    /// frame the panel is open.
    ///
    /// Deliberately *not* [`Self::buffer`]: the panel draws after the HUD's own
    /// encoder has been submitted, and re-uploading the shared buffer would be
    /// correct only by virtue of `wgpu`'s queue ordering. A separate buffer
    /// makes that independence structural instead of subtle, for the cost of
    /// one allocation on the first open.
    recipe_panel_buffer: Option<wgpu::Buffer>,
    /// Capacity of [`Self::recipe_panel_buffer`], in floats.
    recipe_panel_capacity_floats: usize,
    /// The recipe-book panel's **textured** stream — vanilla's real
    /// `recipe_book/**` art, resolved against [`Self::gui`]'s atlas.
    ///
    /// Its own buffer rather than [`GuiHud::buffer`]: that one holds the HUD's
    /// own sprite verts for the same frame, and although the two draws are
    /// separately submitted (so a shared buffer would happen to work today),
    /// sharing it makes the panel's art silently dependent on the HUD having
    /// already been submitted this frame. One buffer per stream is the same
    /// choice [`Self::recipe_panel_buffer`] already makes for the colour stream.
    recipe_panel_sprite_buffer: Option<wgpu::Buffer>,
    /// Capacity of [`Self::recipe_panel_sprite_buffer`], in floats.
    recipe_panel_sprite_capacity_floats: usize,
}

/// The GPU resources for drawing HUD sprites from the vanilla GUI atlas: the
/// uploaded atlas texture, its textured pipeline + bind group, and a dynamic
/// vertex buffer. Present only once [`HudRenderer::attach_gui`] has run; absent
/// on jar-less / headless runs, where the HUD falls back to procedural quads.
#[derive(Debug)]
struct GuiHud {
    atlas: Arc<GuiAtlas>,
    #[allow(dead_code)]
    gpu: GpuAtlas,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    buffer: wgpu::Buffer,
    capacity_floats: usize,
}

impl HudRenderer {
    /// Build the HUD pipeline for a target of `color_format`.
    #[must_use]
    pub fn new(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("hud-shader"),
            source: wgpu::ShaderSource::Wgsl(HUD_WGSL.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("hud-layout"),
            bind_group_layouts: &[],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("hud-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: (FLOATS_PER_VERTEX * 4) as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 0,
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 8,
                            shader_location: 1,
                        },
                    ],
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: color_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let capacity_floats = 4096;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("hud-verts"),
            size: (capacity_floats * 4) as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            buffer,
            capacity_floats,
            gui: None,
            icons: IconRenderer::new(),
            font: VanillaFont::shared(),
            anim_start: Instant::now(),
            heart_anim: anim::HeartAnim::new(),
            hotbar_pop: anim::HotbarPop::new(),
            xp_flash: anim::XpFlash::new(),
            recipe_panel_buffer: None,
            recipe_panel_capacity_floats: 0,
            recipe_panel_sprite_buffer: None,
            recipe_panel_sprite_capacity_floats: 0,
        }
    }

    /// Whether vanilla text is in play. `false` means every string on screen is
    /// the fixed-advance 5×7 fallback — the state a jar-less run is in.
    ///
    /// A gate that means to measure vanilla text **must assert this**: without
    /// it, a missing jar silently degrades to the debug font and every
    /// "text drew something" assertion still passes.
    #[must_use]
    pub fn font_attached(&self) -> bool {
        self.font.is_some()
    }

    /// The vanilla font, for a caller that builds its **own** geometry and needs
    /// to lay out text with the same metrics this HUD does — the recipe-book
    /// panel's search box is the first (`app/redraw.rs`).
    ///
    /// Returns the `Arc` rather than a borrow so the caller can hold it across a
    /// `&mut self.render` borrow, which is the same constraint that made
    /// `recipe_toast_view` a free function.
    #[must_use]
    pub fn font(&self) -> Option<Arc<VanillaFont>> {
        self.font.clone()
    }

    /// Override the font the HUD draws with (a resource pack, or a gate pinning
    /// one specific pack). [`HudRenderer::new`] already resolves the vanilla
    /// default, so this is only needed to *replace* it.
    pub fn attach_font(&mut self, font: Arc<VanillaFont>) {
        self.font = Some(font);
    }

    /// Drop back to the fixed-advance debug font. The executed negative control
    /// for every proportional-width assertion: with this called, a gate that
    /// claims to see vanilla advances must fail.
    pub fn detach_font(&mut self) {
        self.font = None;
    }

    /// The command-suggestion popup's rect for a frame of this size — what the
    /// pointer is hit-tested against.
    ///
    /// This exists on the *renderer* rather than as a free function because the
    /// rect depends on glyph advances, and only the renderer knows which font is
    /// attached. It resolves the identical [`suggestion_layout`] the draw does,
    /// through the identical [`measure_text`], so a click can never land on a
    /// row the player is not looking at.
    ///
    /// `framebuffer_width`/`framebuffer_height` are **physical** pixels and
    /// `gui_scale` the raw option (`0` = auto), matching every other hit-test
    /// entry point here; the returned rect is in logical-canvas pixels, so
    /// convert the cursor with [`Self::canvas_cursor`] before testing it.
    #[must_use]
    pub fn suggestion_layout(
        &self,
        framebuffer_width: u32,
        framebuffer_height: u32,
        gui_scale: u32,
        opts: ChatDisplayOptions,
        popup: &SuggestionPopup<'_>,
    ) -> SuggestionLayout {
        let (w, h) =
            crate::menu::render::logical_canvas(gui_scale, framebuffer_width, framebuffer_height);
        let pose = chat_pose_scale(opts);
        let font = self.font.as_deref();
        suggestion_layout(w, h, pose, popup, |s| measure_text(font, s, pose))
    }

    /// A physical-pixel cursor position in the logical-canvas pixels
    /// [`Self::suggestion_layout`] returns its rect in — the framebuffer divided
    /// by the effective integer GUI scale, vanilla's `guiScaled*`.
    #[must_use]
    pub fn canvas_cursor(
        framebuffer_width: u32,
        framebuffer_height: u32,
        gui_scale: u32,
        cursor: (f32, f32),
    ) -> (f32, f32) {
        let scale =
            crate::config::calculate_gui_scale(gui_scale, framebuffer_width, framebuffer_height)
                .max(1) as f32;
        (cursor.0 / scale, cursor.1 / scale)
    }

    /// Attach the vanilla GUI sprite atlas so the survival vitals (hearts,
    /// hunger, XP bar, hotbar frame + selection) render from real textures.
    /// Uploads the atlas, builds the textured pipeline, and binds it. Without
    /// this call the HUD keeps its procedural fallback — the jar-less runtime
    /// behaviour and the headless negative control the GPU gate exercises.
    pub fn attach_gui(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_format: wgpu::TextureFormat,
        atlas: Arc<GuiAtlas>,
    ) {
        let sp = item_icon::build_sprite_pipeline(
            device,
            queue,
            atlas.atlas(),
            HUD_SPRITE_WGSL,
            color_format,
            4096,
            "hud-sprite",
        );
        self.gui = Some(GuiHud {
            atlas,
            gpu: sp.gpu,
            pipeline: sp.pipeline,
            bind_group: sp.bind_group,
            buffer: sp.buffer,
            capacity_floats: sp.capacity_floats,
        });
    }

    /// Attach the flat item-sprite [`ItemAtlas`] so hotbar slots draw real item
    /// icons. Without this call the wells stay empty — the jar-less / headless
    /// behaviour. Delegates to the shared [`IconRenderer`]; the container screen
    /// has the identical call on `ContainerRenderer`.
    pub fn attach_items(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_format: wgpu::TextureFormat,
        atlas: Arc<ItemAtlas>,
    ) {
        self.icons
            .attach_items(device, queue, color_format, atlas, "hud-item");
    }

    /// Attach the 2-D GUI enchantment-glint pass, so an enchanted hotbar item
    /// shimmers (issue #452). Must follow [`Self::attach_items`] — the pass masks
    /// itself against the item atlas — and is a no-op otherwise.
    pub fn attach_glint(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_format: wgpu::TextureFormat,
        img: &lodestone_assets::Image,
    ) {
        self.icons
            .attach_glint(device, queue, color_format, img, "hud-glint");
    }

    /// Push vanilla's **Glint Speed**/**Glint Strength** accessibility options to
    /// the 2-D GUI glint pass, so an enchanted hotbar item shimmers at the
    /// player's chosen rate and opacity.
    ///
    /// This is the third of the three glint sites and the one that was missed:
    /// the world and hand passes share `crate::gpu::RenderState::glint_options`,
    /// while the GUI icon pass is a separate pipeline with its own uniform, so
    /// pushing to the first two left an enchanted item shimmering correctly in the
    /// world and in hand but at vanilla's default in a slot. The container screen
    /// has the identical call on `ContainerRenderer` — **both** are needed, since
    /// each owns its own [`IconRenderer`].
    ///
    /// Called once per presented frame from `app/redraw.rs` beside
    /// `RenderState::set_glint_options`, not once at attach time: the value can
    /// change in the settings screen while a container is open.
    pub fn set_glint_options(&mut self, speed: f64, strength: f32) {
        self.icons.set_glint_options(speed, strength);
    }

    /// This frame's GUI glint speed and strength as the uniform will see them —
    /// already clamped. Exists so a gate can predict what the shader gets rather
    /// than what was pushed; see [`item_icon::IconRenderer::glint_options`].
    #[must_use]
    pub fn glint_options(&self) -> (f64, f32) {
        self.icons.glint_options()
    }

    /// Attach the GPU side of the **3-D block-item** icon pass, so hotbar slots
    /// holding a block draw vanilla's isometric mini-block instead of an empty
    /// well.
    ///
    /// Every resource is *borrowed from the world renderer* rather than created;
    /// see [`item_icon::IconRenderer::attach_item_models`] for why each sharing
    /// is load-bearing.
    ///
    /// The **CPU** geometry is not captured here: it is passed per frame to
    /// [`render_with_item_models`](Self::render_with_item_models), because a
    /// per-slot lookup of the nine visible stacks is cheaper than cloning ~750
    /// items' quads. (`BlockModels::items` can now enumerate them, for consumers
    /// that do want an attach-time snapshot.)
    ///
    /// Without this call the icons simply do not draw — the jar-less / demo
    /// behaviour, and the negative control the pixel gate exercises.
    pub fn attach_item_models(
        &mut self,
        device: &wgpu::Device,
        color_format: wgpu::TextureFormat,
        atlas_view: &wgpu::TextureView,
        atlas_sampler: &wgpu::Sampler,
        palette: &wgpu::Buffer,
        anim: &wgpu::Buffer,
    ) {
        self.icons.attach_item_models(
            device,
            color_format,
            atlas_view,
            atlas_sampler,
            palette,
            anim,
            "hud-item-model",
        );
    }

    /// The flat item atlas attached by [`Self::attach_items`], if any.
    ///
    /// Exists so a caller building geometry that draws item icons — the
    /// recipe-book panel ([`Self::render_recipe_book_panel`]) — can ask for the
    /// atlas it will be drawn against instead of re-loading a second copy or
    /// threading one through as a new field. `None` on a jar-less run, which is
    /// the icon-less fallback path and the pixel gate's negative control.
    #[must_use]
    pub fn item_atlas(&self) -> Option<Arc<ItemAtlas>> {
        self.icons.item_atlas()
    }

    /// How many block-entity sheets the **special-renderer** icon pass has
    /// loaded — `0` until the first frame containing a chest (the pass is built
    /// lazily) and `0` forever on a jar-less run.
    ///
    /// Exists for the pixel gate, and it is not ornamental: a coverage-only
    /// assertion cannot tell "no chest in any slot" from "no pack, so a chest
    /// could never draw", and those two fail in opposite directions. The same
    /// distinction `RenderStats::block_entity_sheets_loaded` draws for the world
    /// pass.
    #[must_use]
    pub fn special_icon_sheets(&self) -> usize {
        self.icons.special_sheet_count()
    }

    /// Draw the HUD over the current frame contents (a `Load` pass, no depth).
    ///
    /// Convenience wrapper over [`render_with_item_models`](Self::render_with_item_models)
    /// with no model set, no depth attachment, and [`AUTO_GUI_SCALE`](crate::config::AUTO_GUI_SCALE)
    /// (this call has no access to the persisted `Options.gui_scale` — its
    /// callers are the headless HUD gates and the scoreboard/tab-list overlays,
    /// none of which own one). Kept as the plain entry point so those existing
    /// callers are unchanged.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        frame: &HudFrame,
        width: u32,
        height: u32,
    ) {
        self.render_with_item_models(
            device,
            queue,
            view,
            None,
            frame,
            None,
            crate::config::AUTO_GUI_SCALE,
            width,
            height,
        );
    }

    /// Draw the HUD, including the **3-D block-item** icons.
    ///
    /// `models` supplies the baked item geometry (`None` falls back to flat
    /// sprites only), and `depth` is a depth attachment matching the target size
    /// — normally
    /// [`RenderState::depth_view`](crate::gpu::RenderState::depth_view). Both are
    /// needed for a mini-block to draw; either being `None` degrades to the
    /// previous behaviour rather than erroring. `gui_scale` is the resolved
    /// `Options.gui_scale` (`0` = auto) — `app.rs`'s real windowed call site
    /// passes `menu::nav::MenuNav::gui_scale()` so a manual scale setting
    /// resizes the HUD exactly as it already resizes the menu screens.
    ///
    /// # Pass structure
    ///
    /// Three passes, in this order, all loading the existing colour:
    ///
    /// 1. **sprites** (no depth) — hotbar frame, vitals, flat item icons;
    /// 2. **item models** (depth, **cleared**) — the isometric mini-blocks;
    /// 3. **colour** (no depth) — text, stack counts, durability bars.
    ///
    /// The middle pass needs its own depth attachment and therefore its own pass.
    /// It *clears* depth rather than loading it: the world's depth is still
    /// resident from the terrain pass and would occlude a GUI item sitting at
    /// clip depth ~0.5. Nothing later in the frame reads depth, so clearing it
    /// here is free. Keeping it strictly between 1 and 3 is what leaves stack
    /// counts and durability bars on top of the icon rather than buried in it.
    #[allow(clippy::too_many_arguments)]
    pub fn render_with_item_models(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        depth: Option<&wgpu::TextureView>,
        frame: &HudFrame,
        models: Option<&BlockModels>,
        gui_scale: u32,
        width: u32,
        height: u32,
    ) {
        // With the GUI atlas attached, the vitals come back as textured sprite
        // verts; otherwise the whole HUD is the procedural colour stream. The
        // item atlas, when attached, feeds the separate item-sprite stream.
        let gui_atlas = self.gui.as_ref().map(|g| Arc::clone(&g.atlas));
        let item_atlas = self.icons.item_atlas();
        // Only ask for model geometry when there is somewhere to draw it: no
        // attached pass or no depth attachment means the vertices could not be
        // rendered, and building them would be pure waste.
        let want_models = self.icons.models_attached() && depth.is_some();
        let font = self.font.clone();
        // The vitals-cluster animation phases for this frame — see
        // `hud/anim.rs`. `tick` is the one place a wall clock enters; every
        // state machine it feeds is otherwise a pure function of that integer.
        let tick = anim::wall_tick(self.anim_start);
        let (heart_blink, display_health) = self.heart_anim.tick(tick, frame.health.unwrap_or(0.0));
        // `Option`, not `.unwrap_or(&[])`. Collapsing "the hotbar is hidden
        // this frame" and "the hotbar is genuinely empty" into the same empty
        // slice is exactly what made returning from a deeper menu (Options)
        // fire the pickup pop on every slot; see
        // `hud::anim::HotbarPop::tick`'s own doc.
        let hotbar_pop = self.hotbar_pop.tick(tick, frame.hotbar_items);
        let xp_flash = self.xp_flash.tick(tick, frame.xp.map(|(level, _)| level));
        let anim = HudAnim {
            heart_blink,
            display_health,
            tick,
            hotbar_pop,
            xp_flash,
        };
        let geo = HudGeometry::build_inner(
            frame,
            width,
            height,
            gui_scale,
            gui_atlas.as_deref(),
            item_atlas.as_deref(),
            models.filter(|_| want_models),
            font.as_deref(),
            anim,
        );
        // `geo.special` counts too. A hotbar holding nothing but a chest, with the
        // procedural frame suppressed, produces zero vertices in all four other
        // streams — bailing here would make the whole chest-icon chain
        // unreachable in exactly the configuration the pixel gate renders.
        if geo.verts.is_empty()
            && geo.sprite_verts.is_empty()
            && geo.item_verts.is_empty()
            && geo.model_verts.is_empty()
            && geo.special.is_empty()
        {
            return;
        }

        // Grow + upload the colour stream.
        if !geo.verts.is_empty() {
            if geo.verts.len() > self.capacity_floats {
                self.capacity_floats = geo.verts.len().next_power_of_two();
                self.buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("hud-verts"),
                    size: (self.capacity_floats * 4) as wgpu::BufferAddress,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
            }
            queue.write_buffer(&self.buffer, 0, bytemuck::cast_slice(&geo.verts));
        }

        // Grow + upload the sprite stream (only when an atlas is attached).
        if !geo.sprite_verts.is_empty()
            && let Some(g) = self.gui.as_mut()
        {
            if geo.sprite_verts.len() > g.capacity_floats {
                g.capacity_floats = geo.sprite_verts.len().next_power_of_two();
                g.buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("hud-sprite-verts"),
                    size: (g.capacity_floats * 4) as wgpu::BufferAddress,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
            }
            queue.write_buffer(&g.buffer, 0, bytemuck::cast_slice(&geo.sprite_verts));
        }

        // Grow + upload both icon streams, and rewrite the model pass's GUI
        // camera for the current target size. Counts come back zero for a half
        // that is not attached, so the draws below need no further branching.
        //
        // `upload` feeds `width`/`height` straight to `gui_ortho`, the
        // projection that turns the 3-D block-item vertices' GUI-pixel-space
        // positions into clip space. Those vertices were posed by
        // `HudGeometry::build_inner` above, in the *logical* canvas (physical
        // framebuffer divided by the effective GUI scale) — so the projection
        // must be built for that same logical size, not the raw physical one,
        // or the model pass and the flat-sprite/colour passes it shares a
        // frame with would disagree about how big a "GUI pixel" is.
        let (logical_w, logical_h) = crate::menu::render::logical_canvas(gui_scale, width, height);
        let (item_count, model_count) = self.icons.upload(
            device,
            queue,
            &geo.item_verts,
            &geo.model_verts,
            &geo.special,
            // The hotbar has no carried stack, so every special icon is in the
            // slot stratum — see `IconRenderer::upload`'s `special_carried_from`.
            geo.special.len(),
            logical_w.max(1.0) as u32,
            logical_h.max(1.0) as u32,
            "hud-item-verts",
        );
        let glint_count = self
            .icons
            .upload_glint(device, queue, &geo.glint_verts, "hud-glint-verts");

        let colour_count = geo.vertex_count() as u32;
        let sprite_count = geo.sprite_vertex_count() as u32;
        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("hud") });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("hud-pass"),
                color_attachments: &[Some(item_icon::load_colour_attachment(view))],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            // GUI sprites (hotbar frame, vitals) first, then flat item icons over
            // the frame.
            if let Some(g) = &self.gui
                && sprite_count > 0
            {
                pass.set_pipeline(&g.pipeline);
                pass.set_bind_group(0, &g.bind_group, &[]);
                pass.set_vertex_buffer(0, g.buffer.slice(..));
                pass.draw(0..sprite_count, 0..1);
            }
            self.icons.draw_sprites(&mut pass, item_count);
            // The glint over the icons it belongs to, in the same pass so it
            // lands on top of them.
            self.icons.draw_glint_range(&mut pass, 0..glint_count);
        }

        // The 3-D block items, in their own pass because they are the only part
        // of the HUD that needs a depth buffer. One draw for the whole hotbar.
        self.icons.draw_models(
            &mut encoder,
            view,
            depth,
            model_count,
            "hud-item-model-pass",
        );

        // The colour stream (text, stack counts) last, so it lands on top of both
        // kinds of icon.
        if colour_count > 0 {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("hud-colour-pass"),
                color_attachments: &[Some(item_icon::load_colour_attachment(view))],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_vertex_buffer(0, self.buffer.slice(..));
            pass.draw(0..colour_count, 0..1);
        }
        queue.submit(std::iter::once(encoder.finish()));
    }

    /// Draw one frame of the **recipe-book panel** (issue #163) as its own pass,
    /// over whatever is already in `view`.
    ///
    /// This is the call that stops
    /// [`crate::container::recipe_book_panel_geometry_with_icons`] being an
    /// island: that function and its whole layout/hit-test family were built,
    /// unit-tested and reached zero pixels because nothing drew the vertices.
    /// Its own doc says "`app.rs` draws this in its own pass" — but the pipeline
    /// a colour/sprite/model triple needs already exists *here*, and
    /// `ContainerRenderer` exposes no entry point taking a prebuilt
    /// [`crate::container::RecipeBookPanelGeometry`], so the pass lives on the
    /// renderer that already owns matching pipelines rather than growing a
    /// fourth copy of them in `app.rs`.
    ///
    /// The streams are byte-compatible by construction, not by coincidence:
    /// `RecipeBookPanelGeometry`'s colour verts come from the same shared
    /// [`item_icon::ColourStream`] the HUD's do (6 floats, position already in
    /// NDC), and its item verts from the same `item_icon::push_sprite_quad`
    /// (8 floats). Both match [`HUD_WGSL`]/[`HUD_SPRITE_WGSL`]'s vertex layouts
    /// exactly.
    ///
    /// `gui_scale`/`width`/`height` must be the **same triple** the geometry and
    /// its layout were built from — see
    /// [`crate::container::recipe_book_panel_geometry`]'s own warning about what
    /// a mismatched triple does (every vertex lands outside the `[-1, 1]` clip
    /// range and the panel draws nothing at all).
    ///
    /// # Pass order
    ///
    /// Four passes, in the order
    /// [`ContainerRenderer::render_with_icons_scaled`](crate::container::ContainerRenderer)
    /// already uses for the main panel:
    ///
    /// 1. `verts[..chrome]` — panel, tabs, buttons, slot wells, page arrows.
    /// 2. the 3-D block-item models.
    /// 3. the flat item sprites.
    /// 4. `verts[chrome..]` — stack-count digits, durability bars, and the
    ///    jar-less fallback swatches.
    ///
    /// **The split is load-bearing and this used to be wrong.** The geometry
    /// previously kept one unsplit colour stream drawn entirely in pass 1, so a
    /// recipe result's count digits were submitted before its icon and vanished
    /// underneath it — the owner-reported "the item counts are behind the items
    /// (at least the blocks)". The "at least" is the tell: a flat item sprite is
    /// mostly transparent around its edges so some digits bled through, whereas
    /// a 3-D block model fills the bottom-right corner opaquely and hid them
    /// completely.
    ///
    /// There is no depth compare on this path, so submission order is the *only*
    /// thing deciding z. Collapsing these four passes back into fewer, or
    /// drawing all of `verts` in pass 1, reproduces the bug — see
    /// [`crate::container::RecipeBookPanelGeometry::chrome_vertex_count`].
    #[allow(clippy::too_many_arguments)]
    pub fn render_recipe_book_panel(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        depth: Option<&wgpu::TextureView>,
        geo: &crate::container::RecipeBookPanelGeometry,
        gui_scale: u32,
        width: u32,
        height: u32,
    ) {
        if geo.verts.is_empty()
            && geo.item_verts.is_empty()
            && geo.model_verts.is_empty()
            && geo.sprites.is_empty()
        {
            return;
        }

        if !geo.verts.is_empty() {
            if geo.verts.len() > self.recipe_panel_capacity_floats {
                self.recipe_panel_capacity_floats = geo.verts.len().next_power_of_two();
                self.recipe_panel_buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("hud-recipe-panel-verts"),
                    size: (self.recipe_panel_capacity_floats * 4) as wgpu::BufferAddress,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
            }
            if let Some(buffer) = &self.recipe_panel_buffer {
                queue.write_buffer(buffer, 0, bytemuck::cast_slice(&geo.verts));
            }
        }

        // Same logical-canvas expression the geometry itself used, so the
        // model pass's GUI projection agrees with the vertices it is drawing.
        let (logical_w, logical_h) = crate::menu::render::logical_canvas(gui_scale, width, height);

        // Resolve vanilla's real `recipe_book/**` art against whatever GUI atlas
        // is bound. This is the whole of the texture fix: `GuiAtlas` already
        // stitches every `gui/sprites/**` in the pack, so the sprites needed no
        // new atlas, pipeline or bind group — only ids and destination rects,
        // which the geometry carries (see `RecipeBookSprite`).
        //
        // Unknown ids resolve to nothing and are skipped, so a pack missing one
        // sprite loses that sprite and not the panel. On a jar-less run
        // `self.gui` is `None` and this is empty, leaving the flat-fill fallback
        // in `verts[..chrome]` as the whole picture — which is exactly what
        // every existing headless geometry gate measures.
        let panel_sprite_verts: Vec<f32> = match &self.gui {
            Some(g) => {
                let mut out = Vec::new();
                for s in &geo.sprites {
                    // A `src` is a fixed sub-rect of a larger sheet (the panel
                    // page); `None` is the ordinary whole-sprite blit, which
                    // must go through `geometry` so the sprite's own
                    // `GuiScaling` is honoured.
                    match s.src {
                        Some(src) => {
                            if let Some(q) = g.atlas.subregion_quad_declared(
                                s.id,
                                crate::container::RECIPE_PANEL_DECLARED,
                                src,
                                s.dst,
                            ) {
                                item_icon::push_sprite_quad(
                                    &mut out,
                                    logical_w,
                                    logical_h,
                                    q,
                                    [1.0, 1.0, 1.0, 1.0],
                                );
                            }
                        }
                        None => {
                            let [x, y, w, h] = s.dst;
                            for q in g.atlas.geometry(s.id, x, y, w, h) {
                                item_icon::push_sprite_quad(
                                    &mut out,
                                    logical_w,
                                    logical_h,
                                    q,
                                    [1.0, 1.0, 1.0, 1.0],
                                );
                            }
                        }
                    }
                }
                out
            }
            None => Vec::new(),
        };
        if !panel_sprite_verts.is_empty() {
            if panel_sprite_verts.len() > self.recipe_panel_sprite_capacity_floats {
                self.recipe_panel_sprite_capacity_floats =
                    panel_sprite_verts.len().next_power_of_two();
                self.recipe_panel_sprite_buffer =
                    Some(device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("hud-recipe-panel-art-verts"),
                        size: (self.recipe_panel_sprite_capacity_floats * 4)
                            as wgpu::BufferAddress,
                        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    }));
            }
            if let Some(buffer) = &self.recipe_panel_sprite_buffer {
                queue.write_buffer(buffer, 0, bytemuck::cast_slice(&panel_sprite_verts));
            }
        }
        let panel_art_count = (panel_sprite_verts.len() / SPRITE_FLOATS_PER_VERTEX) as u32;

        let (item_count, model_count) = self.icons.upload(
            device,
            queue,
            &geo.item_verts,
            &geo.model_verts,
            &geo.special,
            // The panel has no carried stack, so every special icon (none, in
            // the current corpus) is in the slot stratum — the same argument
            // `render_with_item_models` makes for the hotbar.
            geo.special.len(),
            logical_w.max(1.0) as u32,
            logical_h.max(1.0) as u32,
            "hud-recipe-panel-item-verts",
        );

        let colour_count = geo.vertex_count() as u32;
        // Clamped against what the stream actually holds, so a geometry built
        // by an older producer (or a hand-built one in a test) can never make
        // this draw a range past the end of its own buffer.
        let chrome_count = (geo.chrome_vertex_count as u32).min(colour_count);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("hud-recipe-panel"),
        });
        // Pass 1: chrome, under everything — **but only when the real art did
        // not resolve.**
        //
        // The flat fills are the jar-less fallback and nothing else (see
        // `RecipeBookPanelGeometry::sprites` and the palette's own doc), and
        // drawing them *under* the art is not free: vanilla's `recipe_book.png`
        // page has **transparent rounded corners**, so an opaque near-black
        // rectangle behind it shows through at all four of them and the panel
        // reads as a square with dark corner pixels. That is the owner's "the
        // rounded corners have pixels filling them in to be square" report — the
        // fill is not covered by the sprite, it is *revealed* by it.
        //
        // Keyed on `panel_art_count`, not on `self.gui.is_some()`: a pack that
        // carries the atlas but none of the `recipe_book/**` ids resolves no
        // sprites, and that run still wants the fallback rather than an invisible
        // panel.
        if chrome_count > 0
            && panel_art_count == 0
            && let Some(buffer) = &self.recipe_panel_buffer
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("hud-recipe-panel-colour-pass"),
                color_attachments: &[Some(item_icon::load_colour_attachment(view))],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_vertex_buffer(0, buffer.slice(..));
            pass.draw(0..chrome_count, 0..1);
        }
        // Pass 1b: vanilla's real art, over the flat-fill fallback. The panel
        // page is fully opaque, so with an atlas bound this hides the fallback
        // entirely rather than blending with it — which is why the fallback
        // palette can stay unchanged and still be the right jar-less picture.
        if panel_art_count > 0
            && let Some(g) = &self.gui
            && let Some(buffer) = &self.recipe_panel_sprite_buffer
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("hud-recipe-panel-art-pass"),
                color_attachments: &[Some(item_icon::load_colour_attachment(view))],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&g.pipeline);
            pass.set_bind_group(0, &g.bind_group, &[]);
            pass.set_vertex_buffer(0, buffer.slice(..));
            pass.draw(0..panel_art_count, 0..1);
        }
        // Pass 2: the 3-D block-item models, over the wells.
        self.icons.draw_models(
            &mut encoder,
            view,
            depth,
            model_count,
            "hud-recipe-panel-item-model-pass",
        );
        // Passes 3 and 4: flat sprites, then the icon-overlay colour range —
        // count digits and durability bars — which must land over *both* kinds
        // of icon. Sharing one pass matches
        // `ContainerRenderer::render_with_icons_scaled`'s own
        // `container-item-pass`.
        if item_count > 0 || colour_count > chrome_count {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("hud-recipe-panel-sprite-pass"),
                color_attachments: &[Some(item_icon::load_colour_attachment(view))],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.icons.draw_sprites(&mut pass, item_count);
            if colour_count > chrome_count
                && let Some(buffer) = &self.recipe_panel_buffer
            {
                pass.set_pipeline(&self.pipeline);
                pass.set_vertex_buffer(0, buffer.slice(..));
                pass.draw(chrome_count..colour_count, 0..1);
            }
        }
        queue.submit(std::iter::once(encoder.finish()));
    }
}

const HUD_WGSL: &str = include_str!("shaders/hud.wgsl");

const HUD_SPRITE_WGSL: &str = include_str!("shaders/hud_sprite.wgsl");

/// The 2-D GUI enchantment glint (issue #452). Shares `hud_sprite.wgsl`'s vertex
/// layout — see `item_icon::GuiGlint` for why it cannot share
/// `lodestone_render`'s own glint pipeline.
const HUD_GLINT_WGSL: &str = include_str!("shaders/hud_glint.wgsl");

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_assets::ResourceLocation;

    #[test]
    fn rss_is_observable() {
        // The memory gauge must read a real, non-zero RSS on the host running
        // the tests. A zero here is exactly the broken-gauge regression the fix
        // addressed: a HUD field that reads 0 gets believed. A live process
        // always has a resident set, so >1 MiB is a safe, non-vacuous floor.
        let rss = process_rss_bytes();
        assert!(
            rss > 1 << 20,
            "process RSS should be observable (>1 MiB), got {rss} bytes — memory gauge is broken"
        );
    }

    #[test]
    fn facing_from_yaw() {
        let mut s = DebugStats {
            yaw: 0.0,
            ..Default::default()
        };
        assert_eq!(s.facing(), "south (+Z)");
        s.yaw = 90.0;
        assert_eq!(s.facing(), "west (-X)");
        s.yaw = 180.0;
        assert_eq!(s.facing(), "north (-Z)");
        s.yaw = 270.0;
        assert_eq!(s.facing(), "east (+X)");
        s.yaw = -90.0;
        assert_eq!(s.facing(), "east (+X)");
    }

    /// `ServerDifficulty` reaches a real, tested ECS fold (`lodestone-client`'s
    /// `apply_routes_difficulty_changed_through_the_real_path`) but the F3
    /// overlay once drew nothing for it. This pins the exact text so a
    /// regression back to "no line at all" or a swapped lock state is visible
    /// in a diff, not just "some line changed somewhere".
    ///
    /// **Re-derived** when the overlay was reformatted against vanilla's own
    /// strings: the prefix was `DIFFICULTY` and the names were shouted, and both
    /// are now vanilla's lowercase serialized keys (`Difficulty`'s
    /// `PEACEFUL(0, "peaceful")` …). Every assertion below changed for that
    /// reason and for no other — the *shape* (a line always present, `-` before
    /// the first report, a lock suffix, all four names) is unchanged.
    #[test]
    fn debug_overlay_shows_difficulty_and_lock_state() {
        // Found by content, not position: `lines()` is a growing list of
        // independent facts, so pinning an index here would make this test
        // brittle to an unrelated line being added or reordered, which is
        // exactly the kind of accidental coupling `CLAUDE.md` warns a gate
        // should not have.
        fn difficulty_line(stats: &DebugStats) -> String {
            stats
                .lines()
                .into_iter()
                .find(|l| l.starts_with("Difficulty:"))
                .expect("the F3 overlay must always carry a Difficulty line")
        }

        let no_report = DebugStats::default();
        assert_eq!(
            difficulty_line(&no_report),
            "Difficulty: -",
            "before the server's first report, the line must say so plainly rather \
             than defaulting to a difficulty the server never sent"
        );

        let unlocked = DebugStats {
            difficulty: Some((lodestone_model::Difficulty::Easy, false)),
            ..Default::default()
        };
        assert_eq!(difficulty_line(&unlocked), "Difficulty: easy");

        let locked = DebugStats {
            difficulty: Some((lodestone_model::Difficulty::Hard, true)),
            ..Default::default()
        };
        assert_eq!(difficulty_line(&locked), "Difficulty: hard (locked)");

        // Every variant name, so a mis-mapped match arm (e.g. Peaceful reading
        // as Easy) cannot hide behind only testing one value.
        for (d, name) in [
            (lodestone_model::Difficulty::Peaceful, "peaceful"),
            (lodestone_model::Difficulty::Easy, "easy"),
            (lodestone_model::Difficulty::Normal, "normal"),
            (lodestone_model::Difficulty::Hard, "hard"),
        ] {
            let stats = DebugStats {
                difficulty: Some((d, false)),
                ..Default::default()
            };
            assert_eq!(difficulty_line(&stats), format!("Difficulty: {name}"));
        }
    }

    /// The F3 overlay's plate, ink and pitch, against the literals in
    /// `DebugScreenOverlay`.
    ///
    /// # Where the expected values come from
    ///
    /// `extractLines` is four numbers: `int height = 9`, the two margins spent as
    /// `left = alignLeft ? 2 : guiWidth() - 2 - width` and `top = 2 + height * i`,
    /// `graphics.fill(…, -1873784752)` and `graphics.text(…, -2039584, false)`.
    /// The two colours below are those **signed Java `int`s, transcribed as
    /// written and unpacked here** rather than restated as four floats — a
    /// channel swap or a dropped alpha then fails, which is the failure a
    /// hand-copied `[0x50/255.0, …]` array cannot see because it *is* the
    /// hypothesis.
    #[test]
    fn debug_overlay_plate_and_ink_match_vanillas_fill_literals() {
        /// `DebugScreenOverlay.extractLines`' `graphics.fill(…, -1873784752)`.
        const VANILLA_PLATE_ARGB: i32 = -1_873_784_752;
        /// Its `graphics.text(…, -2039584, false)`.
        const VANILLA_INK_ARGB: i32 = -2_039_584;

        fn unpack_argb(argb: i32) -> [f32; 4] {
            let bits = argb as u32;
            let channel = |shift: u32| ((bits >> shift) & 0xFF) as f32 / 255.0;
            [channel(16), channel(8), channel(0), channel(24)]
        }

        // Collected, not asserted in place: a colour that is wrong in three
        // channels should report three channels, not the first one.
        let mut mismatches: Vec<String> = Vec::new();
        for (label, expected, actual) in [
            ("plate", unpack_argb(VANILLA_PLATE_ARGB), DEBUG_LINE_BG),
            ("ink", unpack_argb(VANILLA_INK_ARGB), DEBUG_LINE_INK),
        ] {
            for (channel, (e, a)) in expected.iter().zip(actual.iter()).enumerate() {
                if (e - a).abs() > f32::EPSILON {
                    mismatches.push(format!(
                        "{label} channel {channel}: expected {e}, got {a}"
                    ));
                }
            }
        }
        assert!(
            mismatches.is_empty(),
            "F3 overlay colours diverged from DebugScreenOverlay's own literals: {mismatches:?}"
        );

        // The plate must be opaque enough to read over snow and translucent
        // enough to see terrain through, which is the whole reason it is
        // `0x90` and not `0xFF` or `0x40`. Stated as the byte so a future
        // "make it darker" cannot pass by rounding.
        assert!(
            (DEBUG_LINE_BG[3] - 0x90 as f32 / 255.0).abs() < f32::EPSILON,
            "the plate alpha is vanilla's 0x90, not {}",
            DEBUG_LINE_BG[3]
        );

        assert_eq!(
            DEBUG_LINE_H, 9.0,
            "vanilla's `int height = 9` is both the line pitch and the plate height"
        );
        assert_eq!(DEBUG_MARGIN, 2.0, "MARGIN_LEFT/RIGHT/TOP are all 2");
    }

    /// Every ported line of the F3 overlay, character for character, against the
    /// format strings in `DebugEntryPosition`, `DebugEntrySectionPosition`,
    /// `DebugEntryLight` and `DebugEntryLookingAt.BlockStateInfo`.
    ///
    /// # Why this position
    ///
    /// `[-0.5, 70.25, 88.75]`. Each component is doing work, and an origin-ish
    /// position would have measured nothing:
    ///
    /// | component | what it discriminates |
    /// |---|---|
    /// | `x = -0.5` | **floor vs truncate.** `Entity.blockPosition()` is `Mth.floor`, so this is block `-1` in chunk `-1`; an `as i64` cast gives block `0` in chunk `0`. `0 0 0` cannot tell those apart, and the truncating version shipped. |
    /// | `x` negative | the region hint's arithmetic shift and mask (`-1 & 31 == 31`, `-1 >> 5 == -1`) and the `%02d` section-relative pad (`-1 & 15 == 15`) |
    /// | fractional `y` and `z` | vanilla's asymmetric `%.3f / %.5f / %.3f` — a uniform `%.2f`, or space separators, differ visibly |
    /// | `y = 70.25` | section Y is `70 >> 4 == 4`, not the block Y the `Chunk:` line used to print |
    /// | `yaw = 405` | `Mth.wrapDegrees` — prints `45.0`, not `405.0` |
    /// | `the_nether`, not `overworld` | a hardcoded dimension default, which is what this line read before it was wired to `ServerDimension` |
    /// | hitboxes **on**, borders **off** | the two `Debug overlays:` states are deliberately *different*. Equal booleans are the one input a transposed pair survives, and they are adjacent same-typed fields — the cheapest bug in the file |
    ///
    /// Each expectation is paired with the value the **superseded** formatting
    /// produced, and the gate fails if the two ever coincide: an input where
    /// both hypotheses agree is not a test.
    #[test]
    fn debug_overlay_ported_lines_match_vanillas_format_strings() {
        let stats = DebugStats {
            position: [-0.5, 70.25, 88.75],
            yaw: 405.0,
            pitch: 12.34,
            light: Some((4, 11)),
            target: Some([-1, 70, 87]),
            dimension: Some("minecraft:the_nether".to_string()),
            hitboxes_shown: true,
            chunk_borders_shown: false,
            ..Default::default()
        };
        let lines = stats.lines();

        // (what it is, vanilla's format applied by hand, what the old format
        // produced for the same input). The third column is the wrong
        // hypothesis, present so the gate can prove the input separates them.
        let cases = [
            (
                "XYZ",
                "XYZ: -0.500 / 70.25000 / 88.750",
                "XYZ -0.50 70.25 88.75",
            ),
            // No old counterpart existed for `Block:`; the truncating cast is
            // the wrong hypothesis instead.
            ("Block", "Block: -1 70 88", "Block: 0 70 88"),
            (
                "Chunk",
                "Chunk: -1 4 5 [31 5 in r.-1.0.mca]",
                "CHUNK 0 70 5",
            ),
            (
                "Facing",
                "Facing: west (Towards negative X) (45.0 / 12.3)",
                "FACING west (-X) (405.0/12.3)",
            ),
            (
                "Section-relative",
                "Section-relative: 15 06 08",
                "Section-relative: -1 6 8",
            ),
            (
                "Client Light",
                "Client Light: 11 (4 sky, 11 block)",
                "LIGHT 11 (4 SKY, 11 BLOCK)",
            ),
            // The identifier half of vanilla's last `position`-group line. The
            // wrong hypothesis is the overworld default this used to be absent
            // for entirely — a line that reads a constant is the defect class,
            // not the missing line.
            (
                "minecraft:the_nether",
                "minecraft:the_nether",
                "minecraft:overworld",
            ),
            // `formatChart`'s shape, carrying the two toggles that exist here.
            // The wrong hypothesis is the *transposed* pair, which is why the
            // fixture sets the two booleans differently.
            (
                "Debug overlays",
                "Debug overlays: [F3+B] Hitboxes visible; [F3+G] Chunk borders hidden",
                "Debug overlays: [F3+B] Hitboxes hidden; [F3+G] Chunk borders visible",
            ),
            (
                "Targeted Block",
                "Targeted Block: -1, 70, 87",
                "TARGET -1 70 87",
            ),
        ];

        let mut failures: Vec<String> = Vec::new();
        for (label, expected, superseded) in cases {
            if expected == superseded {
                failures.push(format!(
                    "{label}: the chosen input cannot separate the two \
                     hypotheses — both read {expected:?}"
                ));
                continue;
            }
            if !lines.iter().any(|l| l == expected) {
                let got = lines
                    .iter()
                    .find(|l| {
                        l.split(&[':', ' '][..]).next() == expected.split(&[':', ' '][..]).next()
                    })
                    .cloned()
                    .unwrap_or_else(|| "<no line with that prefix>".to_string());
                failures.push(format!("{label}: expected {expected:?}, got {got:?}"));
            }
            if lines.iter().any(|l| l == superseded) {
                failures.push(format!(
                    "{label}: still drawing the superseded format {superseded:?}"
                ));
            }
        }
        assert!(
            failures.is_empty(),
            "{} of {} ported lines are wrong:\n  {}\nall lines: {:#?}",
            failures.len(),
            cases.len(),
            failures.join("\n  "),
            lines
        );

        // Before login there is no dimension, and vanilla's whole `position`
        // group is absent in that state — so the line goes rather than becoming
        // a placeholder. The assertion above is this one's control: the same
        // detector found the line present with `Some`, so a `false` here is
        // absence and not a broken search.
        let pre_login = DebugStats::default();
        assert!(
            !pre_login
                .lines()
                .iter()
                .any(|l| l.contains("minecraft:") || l == "-"),
            "with no dimension reported the overlay must draw no dimension line \
             at all, got {:#?}",
            pre_login.lines()
        );

        // And the toggle line survives the pre-login state reading `hidden` for
        // both — the default, and the only value that could hide a wire that
        // never runs. Asserted so the line's *presence* is not conditional on
        // state the way the dimension's is.
        assert!(
            pre_login.lines().iter().any(|l| l
                == "Debug overlays: [F3+B] Hitboxes hidden; [F3+G] Chunk borders hidden"),
            "the Debug overlays line is unconditional, got {:#?}",
            pre_login.lines()
        );
    }

    /// The column structure: vanilla's own category blocks, separated by the
    /// `""` spacers `extractRenderState` inserts, and `lines()` still the exact
    /// concatenation of the two columns.
    ///
    /// The concatenation property is the reason `lines()` exists — it is what
    /// stops a line being added to one column and silently missing from every
    /// consumer of the flat list — so it is asserted directly rather than
    /// assumed.
    #[test]
    fn debug_overlay_columns_carry_vanillas_spacers_and_concatenate() {
        let stats = DebugStats {
            status: "local world".into(),
            adapter: vec!["Apple M5".into(), "Metal".into()],
            ..Default::default()
        };
        let left = stats.left_lines();
        let right = stats.right_lines();

        let mut expected = left.clone();
        expected.extend(right.clone());
        assert_eq!(
            stats.lines(),
            expected,
            "`lines()` must stay the concatenation of the two columns, or a line \
             added to a column goes missing from every flat-list consumer"
        );

        // Vanilla's first priority line goes left and the second goes right,
        // because `addPriorityLine` fills whichever column is shorter and both
        // start empty. So the fps line heads the left column and the version
        // line heads the right one — not the other way round.
        assert!(
            left[0].ends_with("ms work)") && left[0].contains(" fps "),
            "the fps line must head the left column, got {:?}",
            left[0]
        );
        assert!(
            right[0].starts_with("Lodestone "),
            "the version line must head the right column, got {:?}",
            right[0]
        );

        // A spacer between category blocks, in both columns — the visible
        // difference between vanilla's grouped layout and one dense stack. A
        // count with a verdict on the count, not an eyeball.
        for (name, column) in [("left", &left), ("right", &right)] {
            let spacers = column.iter().filter(|l| l.is_empty()).count();
            assert!(
                spacers >= 2,
                "the {name} column needs at least two group spacers, found {spacers} in {column:#?}"
            );
            assert!(
                !column.last().expect("a non-empty column").is_empty(),
                "a trailing spacer draws nothing and only pads `lines()` — the \
                 {name} column must not end with one"
            );
        }

        // The adapter block is the `system` group: a spacer, then the lines.
        let adapter_start = right
            .iter()
            .position(|l| l == "Apple M5")
            .expect("the adapter lines must reach the right column");
        assert_eq!(
            right[adapter_start - 1], "",
            "the adapter block must open with a spacer so it reads as its own group"
        );
    }

    #[test]
    fn geometry_has_crosshair_and_text() {
        let stats = DebugStats {
            position: [1.0, 64.0, 2.0],
            status: "local world".into(),
            ..Default::default()
        };
        let geo = HudGeometry::build(&HudFrame::new(&stats), 320, 240);
        // Crosshair alone is 2 quads = 12 verts; text adds far more.
        assert!(geo.vertex_count() > 100, "expected glyphs + crosshair");
        assert_eq!(geo.verts.len() % FLOATS_PER_VERTEX, 0);
    }

    #[test]
    fn empty_string_advances_without_panicking() {
        let stats = DebugStats::default();
        let _ = HudGeometry::build(&HudFrame::new(&stats), 1, 1);
    }

    #[test]
    fn hotbar_items_draw_count_on_colour_stream_without_atlas() {
        // With `hotbar_items` populated but no item atlas attached, the flat
        // icons cannot draw (item_verts stays empty), but the stack-count number
        // still renders to the colour stream and nothing panics.
        let stats = DebugStats::default();
        let base = HudGeometry::build(&HudFrame::new(&stats), 640, 480).vertex_count();

        let slots = [
            Some(HotbarSlot {
                item: ResourceLocation::parse("minecraft:stone").unwrap(),
                count: 64,
                damage: None,
                max_damage: None,
                enchanted: false,
                dyed_color: None,
                potion_color: None,
            }),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        ];
        let mut frame = HudFrame::new(&stats);
        frame.hotbar = Some(0);
        frame.hotbar_items = Some(&slots);
        let geo = HudGeometry::build(&frame, 640, 480);

        assert!(
            geo.item_verts.is_empty(),
            "no item atlas attached, so no item-sprite geometry"
        );
        assert!(
            geo.vertex_count() > base,
            "the '64' stack count must add colour-stream verts"
        );
    }

    #[test]
    fn hiding_the_debug_overlay_removes_its_geometry() {
        let stats = DebugStats {
            status: "local world".into(),
            ..Default::default()
        };
        let mut frame = HudFrame::new(&stats);
        let with = HudGeometry::build(&frame, 640, 480).vertex_count();
        frame.show_debug = false;
        let without = HudGeometry::build(&frame, 640, 480).vertex_count();
        // Only the crosshair (2 quads = 12 verts) should remain.
        assert!(without < with, "F3 off must drop the overlay glyphs");
        assert_eq!(without, 12, "just the crosshair survives");
    }

    /// Decodes a colour-stream vertex buffer's NDC positions back to the pixel
    /// space `ColourStream::rect` built them from, returning `(min_x, max_x,
    /// min_y, max_y)`. The exact inverse of `to_ndc` in
    /// `hud/item_icon.rs`'s `ColourStream::rect`.
    fn ndc_bounds(verts: &[f32], w: f32, h: f32) -> (f32, f32, f32, f32) {
        let (mut min_x, mut max_x, mut min_y, mut max_y) =
            (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
        for v in verts.chunks_exact(FLOATS_PER_VERTEX) {
            let px = (v[0] + 1.0) * 0.5 * w;
            let py = (1.0 - v[1]) * 0.5 * h;
            min_x = min_x.min(px);
            max_x = max_x.max(px);
            min_y = min_y.min(py);
            max_y = max_y.max(py);
        }
        (min_x, max_x, min_y, max_y)
    }

    /// Pins the crosshair to vanilla's real ink, not its sprite's bounding box —
    /// the draw site's own doc has the pixel-by-pixel read of
    /// `hud/crosshair.png`. Two hypotheses, computed from outside constants
    /// rather than guessed: vanilla's real 9px-long, 1px-thick "+" (correct), and
    /// this draw's own pre-fix 16px/2px-thick bar (the bug this test would have
    /// caught). At `gui_scale == 1` — the floor `320x240` this repo already uses
    /// elsewhere for that reason — physical and logical pixels coincide, so the
    /// measured span is the real on-screen footprint, not a scaled derivative.
    #[test]
    fn crosshair_span_matches_vanillas_real_ink_not_the_old_wrong_hypothesis() {
        let stats = DebugStats::default();
        let mut frame = HudFrame::new(&stats);
        frame.show_debug = false;
        let (w, h) = (320.0_f32, 240.0_f32);
        let geo = HudGeometry::build(&frame, w as u32, h as u32);
        assert_eq!(geo.vertex_count(), 12, "precondition: only the crosshair draws");

        let (min_x, max_x, min_y, max_y) = ndc_bounds(&geo.verts, w, h);
        let (span_x, span_y) = (max_x - min_x, max_y - min_y);
        let (correct, wrong) = (9.0_f32, 16.0_f32);
        let mut mismatches = Vec::new();
        if (span_x - correct).abs() >= 0.01 {
            mismatches.push(format!("horizontal span {span_x} (want {correct})"));
        }
        if (span_y - correct).abs() >= 0.01 {
            mismatches.push(format!("vertical span {span_y} (want {correct})"));
        }
        assert!(
            mismatches.is_empty(),
            "crosshair does not match vanilla's real ink: {mismatches:?} — \
             note the wrong hypothesis this used to draw was {wrong}px"
        );
        // The wrong hypothesis is a real, distinct number — if `correct` and
        // `wrong` ever coincided this assertion would be measuring nothing.
        assert!((correct - wrong).abs() > 1.0);

        // Still centred on the canvas — only the size should have changed.
        let (cx, cy) = ((min_x + max_x) * 0.5, (min_y + max_y) * 0.5);
        assert!((cx - w * 0.5).abs() < 0.01, "crosshair should stay x-centred");
        assert!((cy - h * 0.5).abs() < 0.01, "crosshair should stay y-centred");
    }

    /// The same shape one GUI-scale step up: vanilla's `calculateScale` picks 3
    /// for a 1280x720 framebuffer at `AUTO_GUI_SCALE` (height-bound:
    /// `720/(3+1) < 240`, `720/(3+0) >= 240` at the previous step — see
    /// `calculate_gui_scale`'s own doc), so every logical-pixel constant this
    /// draw site uses should come out scaled by exactly 3, including the
    /// crosshair's real 9px span becoming 27.
    #[test]
    fn crosshair_span_scales_with_gui_scale_not_just_at_the_floor() {
        let stats = DebugStats::default();
        let mut frame = HudFrame::new(&stats);
        frame.show_debug = false;
        let (w, h) = (1280.0_f32, 720.0_f32);
        assert_eq!(
            crate::config::calculate_gui_scale(crate::config::AUTO_GUI_SCALE, w as u32, h as u32),
            3,
            "precondition: this framebuffer must resolve to gui_scale 3"
        );
        let geo = HudGeometry::build(&frame, w as u32, h as u32);
        let (min_x, max_x, min_y, max_y) = ndc_bounds(&geo.verts, w, h);
        let (span_x, span_y) = (max_x - min_x, max_y - min_y);
        assert!(
            (span_x - 27.0).abs() < 0.05,
            "crosshair horizontal span should scale to 27px at gui_scale 3, got {span_x}"
        );
        assert!(
            (span_y - 27.0).abs() < 0.05,
            "crosshair vertical span should scale to 27px at gui_scale 3, got {span_y}"
        );
    }

    /// Twelve candidates named so their widths are equal, so the layout
    /// arithmetic below is not also measuring a proportional font.
    fn popup_candidates(n: usize) -> Vec<crate::chat::Candidate> {
        (0..n)
            .map(|i| crate::chat::Candidate {
                text: format!("cand{i:02}"),
                tooltip: None,
            })
            .collect()
    }

    /// The dropdown reaches pixels, and does so **inside its own rect** — the
    /// island check for the whole widget.
    ///
    /// Counting vertices alone would pass for a popup drawn off-screen or on top
    /// of the hotbar, which is the failure this repo keeps hitting. So the
    /// assertion is on *where*: every quad the popup adds must have its corners
    /// inside the rect `suggestion_layout` resolved (plus the 1px scroll-hint
    /// gutters, which are outside the rows by construction). Mismatches are
    /// collected and asserted as a set, so one stray quad does not hide the
    /// others.
    ///
    /// The negative control is the same frame with `chat_suggestions: None`, run
    /// and compared, not described.
    #[test]
    fn the_suggestion_popup_draws_inside_the_rect_the_layout_resolved() {
        let stats = DebugStats::default();
        let candidates = popup_candidates(12);
        let (w, h) = (640u32, 480u32);
        let base_frame = HudFrame {
            crosshair: false,
            show_debug: false,
            chat_input: Some("ca"),
            chat_caret_visible: false,
            ..HudFrame::new(&stats)
        };
        // The control: identical frame, no popup. Run, not asserted about.
        let control = HudGeometry::build(&base_frame, w, h);

        let popup = SuggestionPopup {
            line: "ca",
            start: 0,
            candidates: &candidates,
            selected: 0,
            offset: 0,
            cursor: None,
        };
        let with = HudGeometry::build(
            &HudFrame {
                chat_suggestions: Some(popup),
                ..base_frame
            },
            w,
            h,
        );
        assert!(
            with.vertex_count() > control.vertex_count(),
            "the popup must add geometry — {} vs {}",
            with.vertex_count(),
            control.vertex_count()
        );

        // Re-derive the rect from the same function the draw called, with the
        // same measure: no font attached here, so `item_icon::text_w`.
        let (cw, ch) = crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, w, h);
        let pose = chat_pose_scale(ChatDisplayOptions::default());
        let layout = suggestion_layout(cw, ch, pose, &popup, |s| measure_text(None, s, pose));
        assert_eq!(
            layout.rows,
            crate::chat::SUGGESTION_LINE_LIMIT,
            "12 candidates must be windowed to 10 rows — otherwise the cap is untested"
        );

        // Every vertex the popup added, in canvas pixels. `verts` is NDC over the
        // logical canvas, so undo that rather than restating a pixel formula.
        let px = |x: f32| (x + 1.0) * 0.5 * cw;
        let py = |y: f32| (1.0 - y) * 0.5 * ch;
        let gutter = pose.max(1.0);
        let mut outside = Vec::new();
        for chunk in with.verts[control.verts.len()..].chunks(FLOATS_PER_VERTEX) {
            let (x, y) = (px(chunk[0]), py(chunk[1]));
            let inside_x = x >= layout.x - 0.5 && x <= layout.x + layout.w + 0.5;
            let inside_y =
                y >= layout.y - gutter - 0.5 && y <= layout.y + layout.h + gutter + 0.5;
            if !(inside_x && inside_y) {
                outside.push((x, y));
            }
        }
        assert!(
            outside.is_empty(),
            "{} of the popup's own vertices landed outside its rect \
             (x {}..{}, y {}..{}): {:?}",
            outside.len(),
            layout.x,
            layout.x + layout.w,
            layout.y - gutter,
            layout.y + layout.h + gutter,
            &outside[..outside.len().min(8)]
        );

        // And the rect really is above the input line rather than over it — the
        // `anchorToBottom` placement, which a sign error would invert.
        assert!(
            layout.y + layout.h <= chat_input_top(ch, pose),
            "the popup's bottom ({}) must sit at or above the input line's top ({})",
            layout.y + layout.h,
            chat_input_top(ch, pose)
        );
    }

    /// The regression this chat-scale fix can specifically introduce: the draw
    /// (`HudGeometry::build_inner`) and the pointer hit-test
    /// (`HudRenderer::suggestion_layout`, exercised headlessly here through the
    /// same free functions it calls — a GPU-free `wgpu::Device` cannot be
    /// constructed in this test, so this is the identical code path minus the
    /// device handle) both resolve `chat_pose_scale`. Before this fix
    /// `HudGeometry::build_inner` recomputed `HUD_TEXT_SCALE * opts.scale`
    /// inline instead of calling [`chat_pose_scale`], so the two *could* have
    /// drifted apart the moment either copy changed; now `build_inner` calls
    /// the same function the hit-test does, structurally.
    ///
    /// Run at **two** non-coincident chat scales — `1.0` (default) and `0.5`
    /// — because a bug that only shows up away from the default (say, a stray
    /// `HUD_TEXT_SCALE` reintroduced on one side only) would pass at `1.0` if
    /// the two formulas happened to agree there by construction and diverge
    /// everywhere else.
    #[test]
    fn the_hit_test_rect_and_the_drawn_popup_agree_at_two_different_chat_scales() {
        let stats = DebugStats::default();
        let candidates = popup_candidates(12);
        let (w, h) = (640u32, 480u32);

        for chat_scale in [1.0_f32, 0.5] {
            let opts = ChatDisplayOptions {
                scale: chat_scale,
                ..ChatDisplayOptions::default()
            };
            let base_frame = HudFrame {
                crosshair: false,
                show_debug: false,
                chat_input: Some("ca"),
                chat_caret_visible: false,
                chat_options: opts,
                ..HudFrame::new(&stats)
            };
            let control = HudGeometry::build(&base_frame, w, h);

            let popup = SuggestionPopup {
                line: "ca",
                start: 0,
                candidates: &candidates,
                selected: 0,
                offset: 0,
                cursor: None,
            };
            let with = HudGeometry::build(
                &HudFrame {
                    chat_suggestions: Some(popup),
                    ..base_frame
                },
                w,
                h,
            );

            // The "hit-test region": exactly what `HudRenderer::suggestion_layout`
            // computes (`logical_canvas` → `chat_pose_scale(opts)` →
            // `suggestion_layout`), not a restatement.
            let (cw, ch) =
                crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, w, h);
            let pose = chat_pose_scale(opts);
            let layout =
                suggestion_layout(cw, ch, pose, &popup, |s| measure_text(None, s, pose));

            // The "drawn region": every vertex the popup actually added.
            let px = |x: f32| (x + 1.0) * 0.5 * cw;
            let py = |y: f32| (1.0 - y) * 0.5 * ch;
            let gutter = pose.max(1.0);
            let mut outside = Vec::new();
            for chunk in with.verts[control.verts.len()..].chunks(FLOATS_PER_VERTEX) {
                let (x, y) = (px(chunk[0]), py(chunk[1]));
                let inside_x = x >= layout.x - 0.5 && x <= layout.x + layout.w + 0.5;
                let inside_y =
                    y >= layout.y - gutter - 0.5 && y <= layout.y + layout.h + gutter + 0.5;
                if !(inside_x && inside_y) {
                    outside.push((x, y));
                }
            }
            assert!(
                outside.is_empty(),
                "chat_scale {chat_scale}: {} of the popup's own vertices landed \
                 outside the hit-test rect (x {}..{}, y {}..{}): {:?}",
                outside.len(),
                layout.x,
                layout.x + layout.w,
                layout.y - gutter,
                layout.y + layout.h + gutter,
                &outside[..outside.len().min(8)]
            );
        }
    }

    /// `row_at` maps a pointer to the candidate the player is looking at.
    ///
    /// The inputs are chosen so the two plausible readings disagree: with
    /// `offset == 2` a hit on the **first visible row** must report candidate
    /// `2`, not `0`, and the last visible row must report `11` rather than `9`.
    /// An implementation that forgot `+ offset` agrees with the truth only at
    /// `offset == 0`, which is why the scrolled case is the one asserted.
    #[test]
    fn a_pointer_resolves_to_the_candidate_under_it_including_when_scrolled() {
        let candidates = popup_candidates(12);
        let popup = SuggestionPopup {
            line: "ca",
            start: 0,
            candidates: &candidates,
            selected: 0,
            offset: 2,
            cursor: None,
        };
        let pose = chat_pose_scale(ChatDisplayOptions::default());
        let layout = suggestion_layout(640.0, 480.0, pose, &popup, |s| measure_text(None, s, pose));
        let mid_x = layout.x + layout.w * 0.5;

        assert_eq!(
            layout.row_at(mid_x, layout.y + layout.row_h * 0.5, 2, 12),
            Some(2),
            "the first visible row is candidate 2 once the window has scrolled"
        );
        assert_eq!(
            layout.row_at(mid_x, layout.y + layout.row_h * 1.5, 2, 12),
            Some(3)
        );
        assert_eq!(
            layout.row_at(mid_x, layout.y + layout.row_h * 9.5, 2, 12),
            Some(11),
            "and the last visible row is the last candidate"
        );
        // Outside, on each of the four edges.
        assert_eq!(layout.row_at(mid_x, layout.y - 1.0, 2, 12), None);
        assert_eq!(
            layout.row_at(mid_x, layout.y + layout.h + 1.0, 2, 12),
            None
        );
        assert_eq!(
            layout.row_at(layout.x - 1.0, layout.y + layout.row_h * 0.5, 2, 12),
            None
        );
        assert_eq!(
            layout.row_at(
                layout.x + layout.w + 1.0,
                layout.y + layout.row_h * 0.5,
                2,
                12
            ),
            None
        );
    }

    #[test]
    fn chat_input_and_log_add_geometry() {
        let stats = DebugStats::default();
        let base = HudGeometry::build(&HudFrame::new(&stats), 640, 480).vertex_count();
        let chat = [("<a> hi", 0.0_f32), ("<b> yo", 0.0)];
        let frame = HudFrame {
            chat: &chat,
            chat_input: Some("hello"),
            ..HudFrame::new(&stats)
        };
        let with_chat = HudGeometry::build(&frame, 640, 480).vertex_count();
        assert!(with_chat > base, "chat log + input line must add geometry");
    }

    /// Regression gate for the player report's third defect: `hud.rs` used to
    /// draw `format!("> {input}_")` unconditionally, so a `>` prompt appeared
    /// that vanilla's own `ChatScreen`/`EditBox` never draws. An empty input
    /// with the caret off must therefore draw *nothing* beyond its background
    /// strip, and turning the caret on must add exactly one `_` glyph — a
    /// negative control (caret off) plus a positive one (caret on) rather than
    /// eyeballing a vertex-count increase.
    #[test]
    fn no_stray_prompt_prefix_and_caret_blinks() {
        let stats = DebugStats::default();
        let caret_off = HudGeometry::build(
            &HudFrame {
                crosshair: false,
                show_debug: false,
                chat_input: Some(""),
                chat_caret_visible: false,
                ..HudFrame::new(&stats)
            },
            640,
            480,
        );
        // Only the input row's own translucent background rect (one quad =
        // 6 vertices) may be here — no `>` , no space, nothing.
        assert_eq!(
            caret_off.vertex_count(),
            6,
            "an empty input with the caret off must draw only its background strip"
        );

        let caret_on = HudGeometry::build(
            &HudFrame {
                crosshair: false,
                show_debug: false,
                chat_input: Some(""),
                chat_caret_visible: true,
                ..HudFrame::new(&stats)
            },
            640,
            480,
        );
        // `_`'s bitmap (`font::glyph_rows('_')`) lights only its bottom row's
        // 5 bits — exactly 5 quads = 30 vertices, not a guess.
        assert_eq!(
            caret_on.vertex_count(),
            caret_off.vertex_count() + 30,
            "chat_caret_visible must toggle exactly one `_` glyph (5 lit pixels)"
        );
    }

    /// Owner report: "the inline autocomplete suggestion gets offset by the
    /// ticking underscore, when it should not move." The ghost's pen used to
    /// be measured from `{input}{caret}`'s live width, and `caret` is `""`
    /// half of every blink cycle — so the ghost's x shifted by the caret
    /// glyph's own advance every ~300ms. Fixed by always measuring against
    /// `{input}_` regardless of the actual blink state.
    ///
    /// This predicts the *old* (buggy) pens from first principles — via the
    /// same jar-less `measure_text` the popup gate above uses — and asserts
    /// they really would have differed, which is what makes the "now equal"
    /// assertion below a discriminating regression rather than a vacuous one
    /// (a font where `_` measured zero-width could satisfy equality by
    /// accident either way).
    #[test]
    fn suggestion_ghost_pen_does_not_move_with_the_caret_blink() {
        let stats = DebugStats::default();
        let (w, h) = (640u32, 480u32);
        let pose = chat_pose_scale(ChatDisplayOptions::default());
        let underscore_w = measure_text(None, "_", pose);
        assert!(
            underscore_w > 0.0,
            "the fallback font must give `_` a real width, or this test cannot \
             discriminate the fix from the bug it replaces"
        );

        let ghost_min_x = |g: &HudGeometry| -> f32 {
            let mut min_x = f32::INFINITY;
            for chunk in g.verts.chunks(FLOATS_PER_VERTEX) {
                if chunk[2..6] == SUGGESTION_GHOST {
                    min_x = min_x.min(chunk[0]);
                }
            }
            assert!(min_x.is_finite(), "no SUGGESTION_GHOST-coloured vertex found");
            min_x
        };

        let frame = |caret_visible: bool| HudFrame {
            crosshair: false,
            show_debug: false,
            chat_input: Some("he"),
            chat_caret_visible: caret_visible,
            chat_suggestion_ghost: Some("llo"),
            ..HudFrame::new(&stats)
        };
        let on = HudGeometry::build(&frame(true), w, h);
        let off = HudGeometry::build(&frame(false), w, h);

        // What the pre-fix formula (`margin + text_width({input}{caret})`)
        // would have produced: the two pens differ by exactly `_`'s own
        // advance, since that is the only difference between the two
        // measured strings.
        let old_pen_on = measure_text(None, "he_", pose);
        let old_pen_off = measure_text(None, "he", pose);
        assert!(
            (old_pen_on - old_pen_off - underscore_w).abs() < 1e-4,
            "sanity check on the reproduction itself: the old pens must differ \
             by exactly the caret glyph's width"
        );

        assert_eq!(
            ghost_min_x(&on),
            ghost_min_x(&off),
            "the suggestion ghost's x must not move when the caret blinks"
        );
    }

    /// The landed blink-invariance fix above made the ghost's pen *stable*,
    /// but stable at the wrong x: one whole underscore-width too far right,
    /// permanently. **The discriminating assertion is the absolute x, not
    /// stability** — a gate that only re-runs the blink-invariance check
    /// above would pass on the regression this predicts and rejects.
    ///
    /// `HudGeometry`'s `verts` are in **NDC** (`ColourStream::rect`'s own doc:
    /// "positions in NDC"), not pixels, so the pixel-space prediction below is
    /// converted through the same `to_ndc` vanilla-canvas math the draw uses
    /// — via [`crate::menu::render::logical_canvas`], the one function that
    /// resolves a framebuffer size to the logical canvas every layout site
    /// (including this draw) measures against.
    #[test]
    fn suggestion_ghost_sits_at_cursor_x_minus_one_not_after_the_caret() {
        let stats = DebugStats::default();
        let (w, h) = (640u32, 480u32);
        let pose = chat_pose_scale(ChatDisplayOptions::default());
        let (logical_w, _) =
            crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, w, h);
        let to_ndc_x = |px: f32| 2.0 * px / logical_w - 1.0;

        let ghost_min_x = |g: &HudGeometry| -> f32 {
            let mut min_x = f32::INFINITY;
            for chunk in g.verts.chunks(FLOATS_PER_VERTEX) {
                if chunk[2..6] == SUGGESTION_GHOST {
                    min_x = min_x.min(chunk[0]);
                }
            }
            assert!(min_x.is_finite(), "no SUGGESTION_GHOST-coloured vertex found");
            min_x
        };

        // The ghost text starts with `A`, not `llo` as the other gates in this
        // module use — deliberately: this fallback font's `l` glyph has a
        // **blank leading column** (`font::glyph_rows('l')`'s column 0 is
        // unlit in all seven rows), so "leftmost lit pixel" would measure one
        // glyph-column right of the real pen for any string starting with
        // `l`. `A` lights column 0 on at least one row, so its own leftmost
        // lit pixel *is* the pen position — which is what this test needs to
        // assert an exact x. The other gates in this module only compare
        // *relative* ghost positions, where that per-glyph offset cancels out
        // and does not matter.
        let frame = HudFrame {
            crosshair: false,
            show_debug: false,
            chat_input: Some("he"),
            chat_caret_visible: true,
            chat_suggestion_ghost: Some("Allo"),
            ..HudFrame::new(&stats)
        };
        let geo = HudGeometry::build(&frame, w, h);

        // Vanilla's `cursorX - 1`, `cursorX` being `EditBox
        // .extractWidgetRenderState`'s `drawX` *after* `drawX +=
        // font.width(charSequence) + 1;` — the typed text's width, no caret
        // glyph folded in (unlike the older, already-fixed `font.width("he_")`
        // bug), **plus vanilla's own reserved pixel**, which the `- 1` then
        // exactly cancels: `(font.width("he") + 1) - 1 == font.width("he")`.
        // So the *correct* ghost position is flush with the raw text width —
        // no further arithmetic on top of it, which is exactly the
        // discriminating case: a formula that forgets vanilla's `+ 1` (this
        // draw site's own bug until now) computes `font.width("he") - 1`
        // instead, landing the ghost one pixel *short*, overlapping into the
        // text's last glyph rather than sitting flush against it.
        let expected = to_ndc_x(HUD_MARGIN + measure_text(None, "he", pose));
        let missing_caret_width_hypothesis =
            to_ndc_x(HUD_MARGIN + measure_text(None, "he_", pose) - pose);
        let missing_plus_one_hypothesis =
            to_ndc_x(HUD_MARGIN + measure_text(None, "he", pose) - pose);
        for (name, hypothesis) in [
            ("the caret-width bug", missing_caret_width_hypothesis),
            ("the missing `+ 1` bug", missing_plus_one_hypothesis),
        ] {
            assert!(
                (expected - hypothesis).abs() > 1e-3,
                "sanity check on the reproduction: {name}'s hypothesis must be \
                 discriminably far from the correct one, or a coincidence could \
                 pass either way"
            );
        }
        assert!(
            (ghost_min_x(&geo) - expected).abs() < 1e-4,
            "ghost x (NDC) = {}, expected cursorX - 1 = {expected} (the \
             caret-width bug would have placed it at \
             {missing_caret_width_hypothesis}, the missing-`+ 1` bug at \
             {missing_plus_one_hypothesis})",
            ghost_min_x(&geo)
        );
    }

    /// **The owner's report**: "the inline completion (grey text) is missing
    /// the pixel gap after the last character... it touches the regular text
    /// which is wrong." Established by direct comparison against
    /// `crates/lodestone-shell/src/menu/edit_box.rs`'s `draw_state_with`,
    /// which already carries vanilla's `+ 1.0`
    /// (`EditBox.extractWidgetRenderState`'s `drawX += font.width
    /// (charSequence) + 1;`) — this draw site did not.
    ///
    /// **Why the assertion is against the text's own right edge, not the
    /// caret.** A first attempt at this gate measured `caret_x - ghost_x` and
    /// found it passed under a deliberate re-neuter of the `+ 1` fix —
    /// because both the ghost (`cursor_x - pose`) and the caret (`cursor_x`)
    /// move together with `cursor_x`, so the gap *between them* is `pose`
    /// regardless of whether `cursor_x` itself carries vanilla's `+ 1`. The
    /// bug is a shift of the whole `{ghost, caret}` pair relative to the
    /// *text*, not a change in their separation from each other — so only a
    /// measurement against the text's own (independently computed) right edge
    /// can see it. Before the fix, `ghost_x` sat a full `pose` *before* the
    /// text's right edge (overlapping the last glyph, `font.width(value) - 1`
    /// instead of vanilla's `(font.width(value) + 1) - 1 ==
    /// font.width(value)`); after it, `ghost_x` sits flush with the text's
    /// right edge, matching `EditBox.java`'s own cancellation exactly — not a
    /// visible pixel of daylight, but no longer overlapping into the glyph
    /// either, which is the actual "touches" the report named.
    #[test]
    fn the_ghost_sits_flush_with_the_text_not_overlapping_its_last_glyph() {
        let stats = DebugStats::default();
        let (w, h) = (640u32, 480u32);
        let pose = chat_pose_scale(ChatDisplayOptions::default());
        let (logical_w, _) =
            crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, w, h);
        let to_px_x = |ndc_x: f32| (ndc_x + 1.0) * 0.5 * logical_w;

        let ghost_min_px = |g: &HudGeometry| -> f32 {
            let mut min_x = f32::INFINITY;
            for chunk in g.verts.chunks(FLOATS_PER_VERTEX) {
                if chunk[2..6] == SUGGESTION_GHOST {
                    min_x = min_x.min(to_px_x(chunk[0]));
                }
            }
            assert!(min_x.is_finite(), "no SUGGESTION_GHOST-coloured vertex found");
            min_x
        };

        // Two pairwise-distinct inputs, different lengths, so a formula that
        // (incorrectly) makes the offset depend on the text's own width
        // cannot pass by coincidence at a single length.
        for input in ["he", "cats"] {
            let frame = HudFrame {
                crosshair: false,
                show_debug: false,
                chat_input: Some(input),
                chat_caret_visible: true,
                chat_suggestion_ghost: Some("Allo"),
                ..HudFrame::new(&stats)
            };
            let geo = HudGeometry::build(&frame, w, h);
            let ghost_x = ghost_min_px(&geo);

            // The text's own right edge, computed from the font's advance
            // metric alone (`measure_text`) — not through `cursor_x`, the
            // value the draw itself derives the ghost from, so this cannot
            // pass by restating the code under test.
            let text_right_edge = HUD_MARGIN + measure_text(None, input, pose);
            let offset = ghost_x - text_right_edge;
            // The bug this replaces: `font.width(value) - 1` (the missing
            // `+ 1` never reserved, so the ghost lands one pixel *inside* the
            // text's last glyph instead of flush with its advance edge). A
            // constant, not a second measurement, so the two hypotheses can
            // never coincide by construction (`0.0 - (-pose)` is always
            // `pose`, well past the tolerance below).
            let overlap_hypothesis = -pose;

            assert!(
                offset.abs() < pose * 0.25,
                "input {input:?}: the ghost must sit flush with the text's own \
                 right edge (vanilla's `(font.width(value) + 1) - 1 == \
                 font.width(value)`), not offset from it: measured {offset:.3}px"
            );
            assert!(
                (offset - overlap_hypothesis).abs() > pose * 0.5,
                "input {input:?}: measured offset {offset:.3}px is too close to \
                 the missing-`+ 1` bug's prediction of overlapping the text's \
                 last glyph by {overlap_hypothesis:.3}px to discriminate the \
                 fix from the bug it replaces"
            );
        }
    }

    /// The other half of the fix: the caret must draw **after** (on top of)
    /// the suggestion, not before — `EditBox.java`'s render order is text →
    /// hint → suggestion → highlight → cursor. `HudGeometry::build` appends
    /// vertices in draw order, so "after" is observable as "later in `verts`".
    #[test]
    fn caret_draws_after_the_suggestion_so_it_composites_on_top() {
        let stats = DebugStats::default();
        let (w, h) = (640u32, 480u32);

        let frame = HudFrame {
            crosshair: false,
            show_debug: false,
            chat_input: Some("he"),
            chat_caret_visible: true,
            chat_suggestion_ghost: Some("llo"),
            ..HudFrame::new(&stats)
        };
        let geo = HudGeometry::build(&frame, w, h);

        let last_index_with_color = |target: [f32; 4]| -> Option<usize> {
            geo.verts
                .chunks(FLOATS_PER_VERTEX)
                .enumerate()
                .filter(|(_, chunk)| chunk[2..6] == target)
                .map(|(i, _)| i)
                .max()
        };
        let ghost_last = last_index_with_color(SUGGESTION_GHOST)
            .expect("the ghost must draw when chat_suggestion_ghost is Some");
        // The caret shares the input text's own white and the same input
        // row, so identify it as a white quad **inside that row's glyph box**
        // appearing after the ghost — restricted to the row so an unrelated
        // white element elsewhere in the frame (this test does not disable
        // every HUD element) cannot produce a false pass. The box spans the
        // full glyph height, not just `input_y` exactly: `_`'s own bitmap
        // (`font::glyph_rows('_')`) only lights the bottom row, so its quad's
        // y sits `6 * pose` px below `input_y`, not at it. `input_y` and the
        // span are converted to NDC the same way
        // [`suggestion_ghost_sits_at_cursor_x_minus_one_not_after_the_caret`]
        // converts x, using the logical (not raw framebuffer) canvas height
        // `chat_input_top` itself is measured against.
        let pose = chat_pose_scale(ChatDisplayOptions::default());
        let (_, logical_h) =
            crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, w, h);
        let input_y_ndc = 1.0 - 2.0 * chat_input_top(logical_h, pose) / logical_h;
        let glyph_h_ndc = 2.0 * (font::GLYPH_H as f32 * pose) / logical_h;
        let white = [1.0_f32, 1.0, 1.0, 1.0];
        let caret_after_ghost = geo.verts.chunks(FLOATS_PER_VERTEX).enumerate().skip(ghost_last + 1).any(
            |(_, chunk)| {
                chunk[2..6] == white
                    && chunk[1] <= input_y_ndc + 1e-3
                    && chunk[1] >= input_y_ndc - glyph_h_ndc - 1e-3
            },
        );
        assert!(
            caret_after_ghost,
            "the caret's white quad, on the input's own row, must appear \
             after the ghost's grey quad in draw order, so it composites on \
             top"
        );
    }

    /// Vanilla's `!insert` gate: a full line (256 chars, `ChatInput::push_char`'s
    /// own cap) suppresses the suggestion entirely, matching
    /// `EditBox`'s own `insert = cursorPos < value.length() || value.length()
    /// >= maxLength` — this shell's chat caret is always at the end (see the
    /// draw's own comment), so only the length half of that disjunction can
    /// ever apply here.
    #[test]
    fn suggestion_is_suppressed_once_the_chat_line_is_full() {
        let stats = DebugStats::default();
        let (w, h) = (640u32, 480u32);
        let full_line: String = "x".repeat(256);

        let frame = HudFrame {
            crosshair: false,
            show_debug: false,
            chat_input: Some(full_line.as_str()),
            chat_caret_visible: true,
            chat_suggestion_ghost: Some("llo"),
            ..HudFrame::new(&stats)
        };
        let geo = HudGeometry::build(&frame, w, h);
        let has_ghost = geo
            .verts
            .chunks(FLOATS_PER_VERTEX)
            .any(|chunk| chunk[2..6] == SUGGESTION_GHOST);
        assert!(!has_ghost, "a full line must draw no suggestion ghost");
    }

    /// Predicts the exact geometry of a hard-wrapped chat line from first
    /// principles (box width, the fixed fallback font's per-char advance, and
    /// `a`'s own lit-pixel count), rather than merely asserting "it wrapped" —
    /// CLAUDE.md's *magnitude* species of vacuous test is a predicate that
    /// would pass for any wrap width; this one would fail for a wrong one.
    #[test]
    fn a_long_line_with_no_spaces_hard_wraps_at_the_predicted_row_count() {
        let stats = DebugStats::default();
        let line = "a".repeat(70);
        let chat = [(line.as_str(), 0.0_f32)];
        let geo = HudGeometry::build(
            &HudFrame {
                crosshair: false,
                show_debug: false,
                chat: &chat,
                ..HudFrame::new(&stats)
            },
            640,
            480,
        );
        // The default chat box is `chat_width_px(1.0) == 320`px wide (capped
        // at `b.w == 640`, so uncapped here). With no `VanillaFont` attached,
        // `Builder::legacy_width` falls back to `item_icon::text_w`:
        // `(GLYPH_W + 1) * scale` per char, and the chat pose scale is
        // vanilla's `chatScale` alone (`chat_pose_scale`,
        // `ChatComponent.getScale`), `1.0` at the default, so each `a` costs
        // `6 * 1.0 == 6`px. `floor(320 / 6) == 53` fit the first row; the
        // remaining `70 - 53 == 17` spill to a second — two rows.
        //
        // 70 chars, not 30: at this HUD's now-deleted ad-hoc 2× pitch each
        // `a` would have cost `12`px (`floor(320 / 12) == 26` per row), which
        // wraps 70 chars into **three** rows (26 + 26 + 18), not two — a
        // whole extra row, not a rounding-sized difference, so this input
        // cannot coincide between the two hypotheses the way a shorter line
        // could.
        //
        // `a`'s bitmap (`font::glyph_rows('a')`) lights `0+0+3+1+4+2+4 == 14`
        // pixels; each lit pixel is one quad (`ColourStream::glyph`)
        // of 6 vertices, so all 70 `a`s cost
        // `70 * 14 * 6 == 5880` vertices regardless of how they are split
        // across rows — the row *count* shows up only in the background
        // strips, one 6-vertex rect each.
        assert_eq!(
            geo.vertex_count(),
            5880 + 2 * 6,
            "expected exactly two wrapped rows' worth of geometry at vanilla's \
             chatScale-only pose (one row would be 5880 + 6, three — the \
             deleted ad-hoc 2× pitch's prediction — would be 5880 + 18)"
        );
    }

    /// Direct, GPU-free gate on [`wrap_legacy_with`]'s wrap *decision*, using a
    /// hand-specified width table rather than the fixed 5×7 fallback — the
    /// fallback is itself fixed-advance, so it cannot exercise the
    /// variable-width case the real vanilla font (attached only when a jar is
    /// present) actually draws with. `i`/`W`'s widths below are vanilla's own,
    /// documented in `crate::hud::vanilla_font`'s module doc ("`i` is 2 px
    /// wide … `W` and `M` are 6"); the competing "flat character count"
    /// hypothesis uses this shell's own real fixed-advance constant
    /// (`(font::GLYPH_W + 1) * 1.0 == 6`) rather than an invented
    /// number, so both sides of the comparison are real, citable code.
    #[test]
    fn wrap_uses_real_per_glyph_widths_not_a_flat_character_count() {
        let real_width = |s: &str| -> f32 {
            s.chars()
                .map(|c| match c {
                    'i' => 2.0,
                    'W' => 6.0,
                    _ => 0.0,
                })
                .sum()
        };
        let flat_count_width = |s: &str| -> f32 { s.chars().count() as f32 * 6.0 };

        // Five narrow glyphs then five wide ones, no spaces, so the wrap is a
        // pure hard-break character-index decision with no word-boundary
        // logic muddying which hypothesis "wins".
        let s = "iiiiiWWWWW";
        let max_width_px = 20.0;

        // Real cumulative widths: 2,4,6,8,10 (the five `i`s), then 16, 22 …
        // for the `W`s — the largest prefix at or under 20px is "iiiiiW"
        // (16px); the next `W` would make 22px.
        let real_rows = wrap_legacy_with(real_width, s, max_width_px);
        assert_eq!(
            real_rows.first().map(String::as_str),
            Some("iiiiiW"),
            "real per-glyph widths must break after the 6th character: {real_rows:?}"
        );

        // The flat hypothesis charges every character 6px regardless of
        // glyph, so only `floor(20 / 6) == 3` fit before the 4th overflows —
        // three characters, not six.
        let flat_rows = wrap_legacy_with(flat_count_width, s, max_width_px);
        assert_eq!(
            flat_rows.first().map(String::as_str),
            Some("iii"),
            "a flat character-count hypothesis must break after the 3rd character: {flat_rows:?}"
        );

        let real_break = real_rows[0].chars().count();
        let flat_break = flat_rows[0].chars().count();
        assert_eq!(real_break, 6, "predicted real-width break index");
        assert_eq!(flat_break, 3, "predicted flat character-count break index");
        assert_eq!(
            real_break - flat_break,
            3,
            "the two hypotheses must diverge by a real, non-zero margin, or this test \
             cannot tell a real-width wrap from a character-count one"
        );
    }

    /// Proves `chat_options.colors` is read, not merely stored. `§c` is
    /// zero-width whether it recolours or is stripped, so the two frames'
    /// vertex *counts* are equal by construction — the option's whole effect
    /// is on colour, so the control that actually matters is `verts`
    /// (positions **and** colours) differing.
    #[test]
    fn chat_colors_option_strips_legacy_codes_when_off() {
        let stats = DebugStats::default();
        let coded = [("\u{00a7}chi", 0.0_f32)];
        let frame = |colors: bool| HudFrame {
            crosshair: false,
            show_debug: false,
            chat: &coded,
            chat_options: ChatDisplayOptions {
                colors,
                ..ChatDisplayOptions::default()
            },
            ..HudFrame::new(&stats)
        };
        let with_colors = HudGeometry::build(&frame(true), 640, 480);
        let without_colors = HudGeometry::build(&frame(false), 640, 480);
        assert_eq!(
            with_colors.vertex_count(),
            without_colors.vertex_count(),
            "the code is zero-width either way, so geometry *count* must match"
        );
        assert_ne!(
            with_colors.verts, without_colors.verts,
            "chat_colors=false must actually strip the colour, not just round-trip the option"
        );
    }

    /// Proves `chat_options.background_opacity` is read with the right
    /// *magnitude*, not merely that changing it changes something — the
    /// species of vacuous test CLAUDE.md calls out (a hurt-overlay gate once
    /// passed 3440/3440 while only checking the *sign* of a change, not how
    /// much). Row 0's background rect is emitted before any of its text
    /// glyphs, so its first vertex's alpha channel is `verts[5]` — no
    /// filtering, no averaging, the exact float the draw call passed in.
    #[test]
    fn chat_background_opacity_sets_the_exact_row_alpha() {
        let stats = DebugStats::default();
        let chat = [("hi", 0.0_f32)];
        for bg in [0.1_f32, 0.5, 1.0] {
            let geo = HudGeometry::build(
                &HudFrame {
                    crosshair: false,
                    show_debug: false,
                    chat: &chat,
                    chat_options: ChatDisplayOptions {
                        background_opacity: bg,
                        ..ChatDisplayOptions::default()
                    },
                    ..HudFrame::new(&stats)
                },
                640,
                480,
            );
            let alpha = geo.verts[5];
            assert!(
                (alpha - bg).abs() < 1e-5,
                "row background alpha must equal chat_background_opacity ({bg}), got {alpha}"
            );
        }
    }

    /// As [`chat_background_opacity_sets_the_exact_row_alpha`], for
    /// `chat_options.text_opacity`: `hi` carries no `§` code, so its colour
    /// stays `base` throughout `Builder::text_legacy`'s fallback path and
    /// every glyph pixel's alpha is exactly the `alpha` parameter passed in —
    /// here, `text_opacity * 0.9 + 0.1` (`ChatComponent.java`) at a fresh
    /// line's fade of `1.0`.
    #[test]
    fn chat_text_opacity_sets_the_exact_glyph_alpha() {
        let stats = DebugStats::default();
        let chat = [("hi", 0.0_f32)];
        for op in [0.0_f32, 0.5, 1.0] {
            let geo = HudGeometry::build(
                &HudFrame {
                    crosshair: false,
                    show_debug: false,
                    chat: &chat,
                    chat_options: ChatDisplayOptions {
                        text_opacity: op,
                        ..ChatDisplayOptions::default()
                    },
                    ..HudFrame::new(&stats)
                },
                640,
                480,
            );
            let expected = op.mul_add(0.9, 0.1);
            // `verts[0..36)` is row 0's background rect (6 vertices); `h`'s
            // bitmap (`font::glyph_rows('h')`) lights bit 0 of its very top
            // row, so the next quad emitted is that pixel — its alpha is
            // `verts[41]` (the 6th float of the 2nd vertex block).
            let alpha = geo.verts[41];
            assert!(
                (alpha - expected).abs() < 1e-5,
                "text_opacity {op}: expected glyph alpha {expected}, got {alpha}"
            );
        }
    }

    /// As the two magnitude gates above, for `chat_options.width_pct`, via
    /// vanilla's own `ChatComponent.getWidth` algebra
    /// (`pct * 280.0 + 40.0`, floored) computed independently here rather
    /// than by calling [`chat_width_px`] — so a bug shared between the two
    /// could not cancel out.
    #[test]
    fn chat_width_option_sizes_the_box_to_the_predicted_pixel_width() {
        let stats = DebugStats::default();
        let chat = [("hi", 0.0_f32)];
        // `b.w == 320` at this canvas size:
        // `logical_canvas(AUTO_GUI_SCALE, 640, 480) == (320, 240)` (height
        // binds at `calculate_gui_scale(0, 640, 480) == 2`).
        const CANVAS_W: f32 = 320.0;
        for (pct, expected_px) in [(1.0_f32, 320.0_f32), (0.5, 180.0), (0.0, 40.0)] {
            let geo = HudGeometry::build(
                &HudFrame {
                    crosshair: false,
                    show_debug: false,
                    chat: &chat,
                    chat_options: ChatDisplayOptions {
                        width_pct: pct,
                        ..ChatDisplayOptions::default()
                    },
                    ..HudFrame::new(&stats)
                },
                640,
                480,
            );
            // Row 0's background rect starts at `x == 0`, so its second
            // vertex `(x + w, y)` (`ColourStream::rect`) converted to NDC is
            // `2 * w / b.w - 1` — `verts[6]`.
            let x1_ndc = geo.verts[6];
            let expected_ndc = 2.0 * expected_px / CANVAS_W - 1.0;
            assert!(
                (x1_ndc - expected_ndc).abs() < 1e-4,
                "pct {pct}: expected box width {expected_px}px (x1 {expected_ndc}), got x1 {x1_ndc}"
            );
        }
    }

    /// As the width gate above, for `chat_options.scale`: it must exactly
    /// double the on-screen row height when set to `2.0`, not merely change
    /// it by some amount.
    #[test]
    fn chat_scale_option_doubles_the_row_height_exactly() {
        let stats = DebugStats::default();
        let chat = [("hi", 0.0_f32)];
        let frame = |scale: f32| HudFrame {
            crosshair: false,
            show_debug: false,
            chat: &chat,
            chat_options: ChatDisplayOptions {
                scale,
                ..ChatDisplayOptions::default()
            },
            ..HudFrame::new(&stats)
        };
        let default_geo = HudGeometry::build(&frame(1.0), 640, 480);
        let doubled_geo = HudGeometry::build(&frame(2.0), 640, 480);
        // Row 0's rect vertex 0 (`y0`) and vertex 2 (`y1`, the 3rd vertex —
        // floats 12..18) give its height in NDC: `verts[1] - verts[13]`.
        let height = |g: &HudGeometry| g.verts[1] - g.verts[13];
        let default_h = height(&default_geo);
        let doubled_h = height(&doubled_geo);
        assert!(default_h > 0.0, "sanity: the rect must have positive height");
        assert!(
            (doubled_h - 2.0 * default_h).abs() < 1e-5,
            "chat_scale=2.0 must exactly double the row height: default {default_h}, doubled {doubled_h}"
        );
    }

    /// Proves `chat_options.height_pct_unfocused` is read as a genuine *cap*
    /// on visible rows, not just stored: at `0.0` (`chat_height_px(0.0) ==
    /// 20`px against a `9`px vanilla-metrics default row — vanilla's own
    /// `messageHeight`, `ChatComponent.java`) exactly two rows fit
    /// (`floor(20 / 9) == 2`), so a five-line log must render identically to
    /// a two-line log, not five.
    #[test]
    fn chat_height_option_caps_the_number_of_visible_rows() {
        let stats = DebugStats::default();
        let chat = [
            ("a", 0.0_f32),
            ("b", 0.0),
            ("c", 0.0),
            ("d", 0.0),
            ("e", 0.0),
        ];
        let capped = HudGeometry::build(
            &HudFrame {
                crosshair: false,
                show_debug: false,
                chat: &chat,
                chat_options: ChatDisplayOptions {
                    height_pct_unfocused: 0.0,
                    ..ChatDisplayOptions::default()
                },
                ..HudFrame::new(&stats)
            },
            640,
            480,
        );
        let two_lines_uncapped = HudGeometry::build(
            &HudFrame {
                crosshair: false,
                show_debug: false,
                chat: &chat[3..],
                ..HudFrame::new(&stats)
            },
            640,
            480,
        );
        assert_eq!(
            capped.vertex_count(),
            two_lines_uncapped.vertex_count(),
            "height_pct_unfocused == 0.0 must cap the scrollback to exactly two rows \
             at vanilla's 9px row height"
        );
        let uncapped = HudGeometry::build(
            &HudFrame {
                crosshair: false,
                show_debug: false,
                chat: &chat,
                ..HudFrame::new(&stats)
            },
            640,
            480,
        );
        assert!(
            uncapped.vertex_count() > capped.vertex_count(),
            "the default (uncapped-enough-for-5-lines) height must show more than the capped one"
        );
    }

    #[test]
    fn chat_colour_codes_are_zero_width_and_recolour_runs() {
        let stats = DebugStats::default();
        // A `§c` prefix must not add glyph geometry (codes are 2 chars / 0 width):
        // "§chi" and "hi" draw the same number of lit pixels.
        let plain = [("hi", 0.0_f32)];
        let coded = [("\u{00a7}chi", 0.0_f32)];
        let plain_geo = HudGeometry::build(
            &HudFrame {
                crosshair: false,
                show_debug: false,
                chat: &plain,
                ..HudFrame::new(&stats)
            },
            640,
            480,
        );
        let coded_geo = HudGeometry::build(
            &HudFrame {
                crosshair: false,
                show_debug: false,
                chat: &coded,
                ..HudFrame::new(&stats)
            },
            640,
            480,
        );
        assert_eq!(
            plain_geo.vertex_count(),
            coded_geo.vertex_count(),
            "a colour code must draw no glyphs of its own"
        );
        // …but the pixels must be a different colour, so the code isn't ignored.
        assert_ne!(
            plain_geo.verts, coded_geo.verts,
            "a colour code must recolour the run, not merely be stripped"
        );
    }

    #[test]
    fn chat_lines_fade_out_with_age_when_closed() {
        let stats = DebugStats::default();
        // A fresh line draws; a line older than the visible window draws nothing.
        let fresh = [("hello", 0.0_f32)];
        let stale = [("hello", 30.0_f32)];
        let fresh_n = HudGeometry::build(
            &HudFrame {
                crosshair: false,
                show_debug: false,
                chat: &fresh,
                ..HudFrame::new(&stats)
            },
            640,
            480,
        )
        .vertex_count();
        let stale_n = HudGeometry::build(
            &HudFrame {
                crosshair: false,
                show_debug: false,
                chat: &stale,
                ..HudFrame::new(&stats)
            },
            640,
            480,
        )
        .vertex_count();
        assert!(fresh_n > 0, "a fresh chat line must be visible");
        assert_eq!(
            stale_n, 0,
            "a line past its lifetime must vanish when closed"
        );

        // Opening the box (a chat_input present) resurrects the stale line.
        let opened = HudGeometry::build(
            &HudFrame {
                crosshair: false,
                show_debug: false,
                chat: &stale,
                chat_input: Some(""),
                ..HudFrame::new(&stats)
            },
            640,
            480,
        )
        .vertex_count();
        assert!(
            opened > 0,
            "an open chat box shows history regardless of age"
        );
    }

    #[test]
    fn health_pips_scale_with_value() {
        let stats = DebugStats::default();
        let mut frame = HudFrame::new(&stats);
        frame.crosshair = false;
        frame.show_debug = false;
        frame.health = Some(0.0);
        let empty = HudGeometry::build(&frame, 640, 480);
        frame.health = Some(20.0);
        let full = HudGeometry::build(&frame, 640, 480);
        // Ten pips are always drawn (lit or dark), so the *count* is identical —
        // the gauge width reads regardless of value. This guards that a zero-HP
        // frame still renders the empty slots rather than nothing (which would
        // read as "no HUD" instead of "no health").
        assert_eq!(
            empty.vertex_count(),
            full.vertex_count(),
            "ten pip quads regardless of value"
        );
        assert_eq!(empty.vertex_count(), 10 * 6, "10 pips × 6 verts each");
        // …but the *colours* must differ: a full bar's lit pips can't share the
        // empty bar's dark colour, or the gauge would never actually read HP.
        assert_ne!(
            empty.verts, full.verts,
            "full vs empty must recolour the pips, not just redraw them"
        );
    }

    /// The discriminating input for the two readings of "hide the hearts in
    /// creative": `GameType.isSurvival()` is `SURVIVAL || ADVENTURE`, so **spectator**
    /// is the value where the mode-naming hypothesis (`mode == Creative`) and the real
    /// predicate disagree. Adventure is the second such value in the other direction.
    #[test]
    fn can_hurt_player_is_isSurvival_and_not_a_creative_test() {
        use lodestone_model::GameMode;
        assert!(can_hurt_player(Some(GameMode::Survival)));
        // `isSurvival()` returns true for ADVENTURE too — an adventure-mode player is
        // hurtable and keeps the whole column.
        assert!(can_hurt_player(Some(GameMode::Adventure)));
        assert!(!can_hurt_player(Some(GameMode::Creative)));
        // The one that separates the hypotheses.
        assert!(!can_hurt_player(Some(GameMode::Spectator)));
        // Pre-connect / pre-login: the survival layout, matching `HudFrame::new`.
        assert!(can_hurt_player(None));
    }

    /// [`armour_icon`] against `Hud.extractArmor`'s three `if`s, at inputs where the
    /// **wrong** reading gives a different answer.
    ///
    /// The wrong reading is the one anybody would write from the screenshot rather
    /// than the record: `full = ceil(armour / 2)` with a half only on an odd
    /// remainder — or equivalently the off-by-one `i * 2 < armour`. It agrees with
    /// the real predicate on **every even input**, so a gate at 8 or 20 measures that
    /// the code runs. The discriminating inputs are odd, and every one below is
    /// checked against both hypotheses: at 15 the truth is 7 full + 1 half + 2 empty
    /// and the wrong reading says 8 full + 0 half + 2 empty.
    ///
    /// Asserted as the full ten-icon **sequence**, not as counts, because counts
    /// alone cannot see a half drawn at the wrong index — and mismatches are
    /// collected rather than asserted inside the loop, so a neuter reports every arm
    /// instead of the first.
    #[test]
    fn armour_icons_follow_extract_armor_at_odd_values() {
        use ArmourIcon::{Empty, Full, Half};
        // Hand-expanded from `if (i * 2 + 1 </==/> armor)`, one row per input.
        // `armour = 1` is the smallest drawn row; 30 is the registry's clamp ceiling
        // and must saturate at ten rather than grow.
        let cases: [(i32, [ArmourIcon; 10]); 6] = [
            (1, [Half, Empty, Empty, Empty, Empty, Empty, Empty, Empty, Empty, Empty]),
            (7, [Full, Full, Full, Half, Empty, Empty, Empty, Empty, Empty, Empty]),
            (15, [Full, Full, Full, Full, Full, Full, Full, Half, Empty, Empty]),
            (19, [Full, Full, Full, Full, Full, Full, Full, Full, Full, Half]),
            (20, [Full; 10]),
            (30, [Full; 10]),
        ];
        let mut mismatches: Vec<String> = Vec::new();
        for (armour, expected) in cases {
            let got: Vec<ArmourIcon> = (0..10).map(|i| armour_icon(i, armour)).collect();
            if got != expected {
                mismatches.push(format!("armour {armour}: expected {expected:?}, got {got:?}"));
            }
            // The wrong hypothesis, evaluated at the same input so the test records
            // that the two really do differ here rather than asserting they do.
            let wrong: Vec<ArmourIcon> = (0..10)
                .map(|i| {
                    if (i as i32) * 2 < armour {
                        Full
                    } else {
                        Empty
                    }
                })
                .collect();
            if armour % 2 == 1 && wrong == expected {
                mismatches.push(format!(
                    "armour {armour} is not a discriminating input: the ceil()/off-by-one \
                     reading gives the same ten icons, so this row measures only that \
                     the function runs"
                ));
            }
        }
        // Zero draws nothing at the call site (`armour > 0`), but the predicate itself
        // must still be total — an all-empty row, never a panic or a stray half.
        if (0..10).map(|i| armour_icon(i, 0)).any(|c| c != Empty) {
            mismatches.push("armour 0 must be ten empty icons".to_string());
        }
        assert!(
            mismatches.is_empty(),
            "armour icon selection diverges from Hud.extractArmor:\n  {}",
            mismatches.join("\n  ")
        );
    }

    /// [`heart_fill`] against `Hud.extractHearts`, at the **half** healths where the
    /// `Mth.ceil` reading and the float reading give different sprites.
    ///
    /// Live player report: *"sometimes i get to 0 hearts but im still alive - im
    /// assuming vanilla maybe rounds up while we just round either way"*. He was
    /// right, and 0.5 is the input that proves it — but it is not the only one, which
    /// is why this drives four healths rather than that one. Vanilla ceils **up**, so
    /// the divergence runs in both directions: 0.5 gains a half heart the float
    /// reading never drew, while 1.5 and 19.5 promote a *half* to a **full**.
    ///
    /// Every expectation below is hand-expanded from `currentHealth = Mth.ceil(health)`
    /// and `halves < currentHealth` / `halves + 1 == currentHealth`, and the float
    /// reading this replaced is evaluated at the same input so each row *records* that
    /// the two really differ instead of asserting they do. The two integer healths are
    /// deliberately included as the coincident controls: they must agree, and a gate
    /// written at 1.0 or 20.0 alone would measure only that the function runs.
    ///
    /// Asserted as the ten-sprite **sequence** and collected rather than asserted
    /// inside the loop, so a neuter reports every arm instead of the first — a count
    /// of filled hearts cannot see 19.5, where both readings draw ten sprites and only
    /// the tenth one's identity differs.
    #[test]
    fn heart_fill_follows_extract_hearts_at_half_healths() {
        use HeartFill::{Full, Half};
        // `None` is a container with nothing over it. Rows hand-expanded from the
        // record, not from this module.
        let cases: [(f32, [Option<HeartFill>; 10]); 6] = [
            // Dead: the only health at which the bar is legitimately empty.
            (0.0, [None; 10]),
            // The report. `ceil(0.5) == 1`, so `halves + 1 == 1` at i = 0: a half.
            (0.5, [Some(Half), None, None, None, None, None, None, None, None, None]),
            // Coincident control — both readings say one half heart.
            (1.0, [Some(Half), None, None, None, None, None, None, None, None, None]),
            // `ceil(1.5) == 2`, so i = 0 is `halves + 1 == 1 != 2`: a **full** heart.
            (1.5, [Some(Full), None, None, None, None, None, None, None, None, None]),
            // The top of the bar. `ceil(19.5) == 20`: ten full, no half at i = 9.
            (19.5, [Some(Full); 10]),
            // Coincident control at the top.
            (20.0, [Some(Full); 10]),
        ];
        let mut mismatches: Vec<String> = Vec::new();
        for (health, expected) in cases {
            let got: Vec<Option<HeartFill>> = (0..10).map(|i| heart_fill(i, health)).collect();
            if got != expected {
                mismatches.push(format!("health {health}: expected {expected:?}, got {got:?}"));
            }
            // The reading this replaced, evaluated here so a row that stops
            // discriminating says so rather than passing quietly.
            let float_reading: Vec<Option<HeartFill>> = (0..10)
                .map(|i| {
                    let units = health.max(0.0) - i as f32 * 2.0;
                    if units >= 2.0 {
                        Some(Full)
                    } else if units >= 1.0 {
                        Some(Half)
                    } else {
                        None
                    }
                })
                .collect();
            let half_health = (health.fract() - 0.5).abs() < 1e-6;
            if half_health && float_reading == expected {
                mismatches.push(format!(
                    "health {health} is not a discriminating input: the float \
                     `health - 2i` reading gives the same ten sprites, so this row \
                     measures only that the function runs"
                ));
            }
            if !half_health && float_reading != expected {
                mismatches.push(format!(
                    "health {health} was chosen as a coincident control but the two \
                     readings disagree ({float_reading:?} vs {expected:?}) — the \
                     control's premise is false"
                ));
            }
        }
        // A negative health (hurt overshoot) must read as dead, not ceil to a heart.
        if heart_fill(0, -0.5).is_some() {
            mismatches.push("a negative health must fill no heart".to_string());
        }
        assert!(
            mismatches.is_empty(),
            "heart fill diverges from Hud.extractHearts:\n  {}",
            mismatches.join("\n  ")
        );
    }

    /// The armour row is gated by the **same** `canHurtPlayer()` flag as the hearts,
    /// hunger and bubble rows — vanilla reaches all four through one
    /// `extractPlayerHealth` call — and by vanilla's own `armor > 0`, which draws
    /// **no** row rather than ten empty icons.
    ///
    /// Counts are predicted from `Builder::pips`' own shape, which
    /// `health_pips_scale_with_value` independently pins at ten quads of six vertices:
    /// so one armour row is `10 * 6 = 60` vertices on top of an otherwise identical
    /// frame. The three arms that must be identical to the baseline are the ones a
    /// direction-only assertion would miss — `Some(0)` in particular, where the
    /// tempting "draw the empty backing anyway" reading adds 60 and vanilla adds 0.
    #[test]
    fn the_armour_row_costs_one_pip_row_and_only_when_worn() {
        let stats = DebugStats::default();
        let build = |can_hurt: bool, armour: Option<i32>| {
            let mut frame = HudFrame::new(&stats);
            frame.crosshair = false;
            frame.show_debug = false;
            frame.can_hurt_player = can_hurt;
            frame.health = Some(20.0);
            frame.food = Some(20);
            frame.armour = armour;
            HudGeometry::build(&frame, 640, 480).vertex_count()
        };
        let baseline = build(true, None);
        let mut mismatches: Vec<String> = Vec::new();
        // `None` (never wired), `Some(0)` (live, wearing nothing) and creative all
        // draw exactly the baseline; a worn value adds one row and nothing else.
        for (label, can_hurt, armour, expected) in [
            ("not wired", true, None, baseline),
            ("live, unarmoured", true, Some(0), baseline),
            ("full diamond", true, Some(20), baseline + 60),
            ("half icon at 15", true, Some(15), baseline + 60),
            ("creative, armoured", false, Some(20), build(false, None)),
        ] {
            let got = build(can_hurt, armour);
            if got != expected {
                mismatches.push(format!(
                    "{label}: expected {expected} vertices, got {got} (baseline {baseline})"
                ));
            }
        }
        assert!(
            mismatches.is_empty(),
            "armour row wiring diverges:\n  {}",
            mismatches.join("\n  ")
        );
    }

    /// Vanilla's gate is `canHurtPlayer()` — `SURVIVAL || ADVENTURE` — and one call
    /// to `extractPlayerHealth` behind it draws hearts, hunger and the bubble row,
    /// while `hasExperience()` (the same body) gates the XP bar.
    ///
    /// The counts here are predicted, not observed: on the procedural branch (no GUI
    /// atlas attached) `Builder::pips` emits exactly ten quads per row at six
    /// vertices each, which `health_pips_scale_with_value` above independently pins at
    /// `10 * 6`. So a survival frame carrying both rows is `2 * 10 * 6` and a creative
    /// frame is `0`. The wrong hypothesis — "gate on `GameMode::Creative`" — would
    /// leave the spectator arm at 120, so the spectator case below is the one that
    /// separates the two readings; a creative-only test passes under either.
    #[test]
    fn creative_and_spectator_hide_the_whole_vitals_column() {
        let stats = DebugStats::default();
        // `HudFrame` is not `Copy` (it carries owned strings), so each arm builds its
        // own rather than cloning one.
        let build = |can_hurt: bool, vitals: bool, hotbar: Option<usize>| {
            let mut frame = HudFrame::new(&stats);
            frame.crosshair = false;
            frame.show_debug = false;
            frame.can_hurt_player = can_hurt;
            frame.xp = Some((7, 0.5));
            frame.hotbar = hotbar;
            if vitals {
                frame.health = Some(20.0);
                frame.food = Some(20);
            }
            HudGeometry::build(&frame, 640, 480).vertex_count()
        };

        // Survival / adventure: two pip rows on top of whatever the XP bar itself
        // costs — asserted by *subtracting* an XP-only frame rather than by predicting
        // the bar's own vertex budget, which is not what this test is about.
        assert_eq!(
            build(true, true, None) - build(true, false, None),
            2 * 10 * 6,
            "ten health pips and ten hunger pips, six vertices each"
        );

        // `can_hurt_player == false` must take the XP bar and its level number with
        // it, not just the two pip rows — so the whole cluster is gone, not merely
        // shortened. With no hotbar, zero is the honest total.
        assert_eq!(
            build(false, true, None),
            0,
            "no hearts, no hunger, no XP bar when the player cannot be hurt"
        );

        // The control: a `0` above would also be what a frame drawing nothing at all
        // reports, so prove the hotbar — which vanilla draws in every game mode, and
        // which this gate must not touch — still lands.
        assert!(
            build(false, true, Some(0)) > 0,
            "the hotbar is not behind canHurtPlayer(); vanilla draws it in creative"
        );
    }

    /// `extractSelectedItemName` places the held-item label at `guiHeight - 59`, then
    /// `y += 14` when `!canHurtPlayer()`. Predicted as an exact delta rather than
    /// "it moved": at 480 px tall with GUI scale forced to 1 the logical canvas is the
    /// physical one, so the two y values are `421` and `435`.
    #[test]
    fn the_held_item_label_drops_exactly_fourteen_pixels_in_creative() {
        let stats = DebugStats::default();
        let lowest_y = |can_hurt: bool| {
            let mut frame = HudFrame::new(&stats);
            frame.crosshair = false;
            frame.show_debug = false;
            frame.can_hurt_player = can_hurt;
            frame.held_item = Some(("Diamond Sword".to_string(), 1.0));
            HudGeometry::build(&frame, 640, 480)
                .verts
                .chunks(FLOATS_PER_VERTEX)
                .map(|v| v[1])
                .fold(f32::NEG_INFINITY, f32::max)
        };
        let survival = lowest_y(true);
        let creative = lowest_y(false);
        // `verts` are clip space, y **up**, so a label lower on screen has the smaller
        // value. The expected delta is derived from the same `logical_canvas` the draw
        // lays out in rather than from a hardcoded scale: 14 logical px over a canvas
        // `h` tall spans `2 * 14 / h` of the `-1..=1` range.
        let (_, canvas_h) =
            crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, 640, 480);
        let expected = 2.0 * 14.0 / canvas_h;
        assert!(
            (survival - creative - expected).abs() < 1e-4,
            "expected a 14px drop ({expected} in clip space), got {survival} -> {creative}"
        );
    }

    #[test]
    fn hotbar_draws_and_selection_moves_the_highlight() {
        let stats = DebugStats::default();
        let mut frame = HudFrame::new(&stats);
        frame.crosshair = false;
        frame.show_debug = false;

        // No hotbar → no hotbar geometry.
        frame.hotbar = None;
        let none = HudGeometry::build(&frame, 640, 480).vertex_count();

        // A hotbar adds a real run of geometry (panel + 9 cells + a 4-edge ring).
        frame.hotbar = Some(0);
        let sel0 = HudGeometry::build(&frame, 640, 480);
        assert!(
            sel0.vertex_count() > none,
            "an on-screen hotbar must add geometry, got {} vs {none}",
            sel0.vertex_count()
        );

        // Moving the selection keeps the vertex *count* identical (same panel,
        // 9 cells, 4-edge ring) but must move the ring — so the bytes differ. A
        // selection that never relocates the highlight would render as a hotbar
        // that ignores the held slot.
        frame.hotbar = Some(4);
        let sel4 = HudGeometry::build(&frame, 640, 480);
        assert_eq!(
            sel0.vertex_count(),
            sel4.vertex_count(),
            "selecting a different slot must not change the vertex count"
        );
        assert_ne!(
            sel0.verts, sel4.verts,
            "the selection ring must move to the newly-selected slot"
        );
    }

    /// A view of `n` players called `P0..P{n-1}`, all survival, all full bars.
    fn tab_view(n: usize) -> crate::tablist::TabListView {
        crate::tablist::TabListView {
            rows: (0..n)
                .map(|i| crate::tablist::TabListRow {
                    name: crate::overlay::plain_spans(format!("P{i}")),
                    ping_sprite: "icon/ping_5",
                    spectator: false,
                })
                .collect(),
            header: Vec::new(),
            footer: Vec::new(),
            show_head: false,
        }
    }

    #[test]
    fn tab_overlay_lists_players() {
        let stats = DebugStats::default();
        let view = tab_view(2);
        let frame = HudFrame {
            players: Some(&view),
            ..HudFrame::new(&stats)
        };
        let with = HudGeometry::build(&frame, 640, 480).vertex_count();
        let without = HudGeometry::build(&HudFrame::new(&stats), 640, 480).vertex_count();
        assert!(with > without, "the tab overlay's plate + names add geometry");
    }

    /// **The column split, at the threshold.**
    ///
    /// `for (cols = 1; rows > 20; rows = (slots + cols - 1) / cols) { cols++; }`
    /// has to be read in Java's own order — condition, body, then update — so
    /// `cols` is bumped *before* `rows` is recomputed.
    ///
    /// The discriminating input is **21**, and the number that discriminates is
    /// `rows`, not `cols`. A plausible misreading — "columns of 20, so
    /// `cols = ceil(slots / 20)` and `rows = 20`" — agrees about `cols` at every
    /// input tried here and answers `20` where the truth is `11`. That is the
    /// difference between an overlay 11 rows tall and one 20 rows tall with nine
    /// empty rows of plate hanging below it, so `cols` alone is not a test.
    #[test]
    fn the_column_split_matches_vanillas_own_loop_at_the_threshold() {
        let panel = |slots: usize| TabPanel::new(640.0, slots, false, 40.0, 0, 0, 0.0);
        // One player: one column of one. Not one column of 20.
        assert_eq!((panel(1).cols, panel(1).rows), (1, 1));
        // MAX_ROWS_PER_COL exactly: still one column, because the guard is
        // `rows > 20` and not `rows >= 20`.
        assert_eq!(
            (panel(TAB_MAX_ROWS_PER_COL).cols, panel(TAB_MAX_ROWS_PER_COL).rows),
            (1, TAB_MAX_ROWS_PER_COL)
        );
        // One more, and it splits into two columns of **11** — ceil(21 / 2).
        assert_eq!((panel(21).cols, panel(21).rows), (2, 11));
        // 41 needs three passes of the loop: 41 → 21 → 14.
        assert_eq!((panel(41).cols, panel(41).rows), (3, 14));
        // And vanilla's own cap, which is 80 rather than a round 100: four
        // columns of 20.
        let full = panel(crate::tablist::MAX_TAB_ROWS);
        assert_eq!((full.cols, full.rows), (4, 20));
    }

    /// Slots fill **column-major** — `col = i / rows`, `row = i % rows`.
    ///
    /// Twenty-one players, so `rows == 11`: index 10 is the bottom of column 0
    /// and index 11 is the *top* of column 1. A row-major reading
    /// (`col = i % cols`) would put index 1 there instead, and on any list of 20
    /// or fewer the two readings are indistinguishable — which is why this gate
    /// has to cross the split.
    #[test]
    fn slots_fill_column_major_so_the_list_reads_downwards() {
        let panel = TabPanel::new(640.0, 21, false, 40.0, 0, 0, 0.0);
        assert_eq!(panel.rows, 11);
        let [x0, y0] = panel.slot_origin(0);
        let [x10, y10] = panel.slot_origin(10);
        let [x11, y11] = panel.slot_origin(11);
        // Column 0 runs the full 11 rows down.
        assert_eq!(x10, x0);
        assert_eq!(y10, y0 + 10.0 * TAB_LINE_H);
        // Index 11 starts column 1, back at the top.
        assert_eq!(y11, y0);
        assert_eq!(x11, x0 + panel.slot_w + 5.0);
    }

    /// The header pushes the rows down by `header_len * 9 + 1` — the bare `yyo++`
    /// after the header loop is a real pixel of air and is easy to drop.
    ///
    /// With no header the rows start at vanilla's `yyo = 10` unchanged, which is
    /// the control: a layout that always added the gap would fail here.
    #[test]
    fn a_header_offsets_the_rows_by_its_own_height_plus_one() {
        let bare = TabPanel::new(640.0, 3, false, 40.0, 0, 0, 0.0);
        assert_eq!(bare.rows_top, 10.0);
        let with_header = TabPanel::new(640.0, 3, false, 40.0, 2, 0, 0.0);
        assert_eq!(with_header.rows_top, 10.0 + 2.0 * TAB_LINE_H + 1.0);
        // `yyo += rows * 9 + 1` before the footer plate, counted from wherever the
        // rows actually began.
        assert_eq!(
            with_header.footer_top,
            with_header.rows_top + 3.0 * TAB_LINE_H + 1.0
        );
    }

    /// A header or footer wider than the row block **widens the plates**, and a
    /// narrow one does not shrink them — vanilla's `maxLineWidth` starts at the
    /// block width and only ever takes a `max`.
    #[test]
    fn a_wide_banner_widens_the_plate_and_a_narrow_one_leaves_it_alone() {
        let bare = TabPanel::new(640.0, 3, false, 40.0, 0, 0, 0.0);
        let narrow = TabPanel::new(640.0, 3, false, 40.0, 1, 0, 4.0);
        assert_eq!(narrow.max_line_width, bare.max_line_width);
        let wide = TabPanel::new(640.0, 3, false, 40.0, 1, 0, bare.max_line_width + 60.0);
        assert_eq!(wide.max_line_width, bare.max_line_width + 60.0);
        // …and the plate really does grow with it, rather than the width being
        // computed and dropped.
        assert!(wide.plate_w() > bare.plate_w());
    }

    /// Nothing is drawn for a header or footer the server did not send.
    ///
    /// Byte-identical geometry, not merely "less": vanilla only measures and only
    /// fills when the component is non-null, so an absent banner must not leave a
    /// plate, a gap, or a single vertex behind. A vanilla server sends neither
    /// unless something sets one, so this is the *common* case and not an edge.
    #[test]
    fn an_absent_header_and_footer_draw_nothing_at_all() {
        let stats = DebugStats::default();
        let build = |view: &crate::tablist::TabListView| {
            HudGeometry::build(
                &HudFrame {
                    players: Some(view),
                    ..HudFrame::new(&stats)
                },
                640,
                480,
            )
            .verts
        };
        let bare = tab_view(3);
        let mut banner = tab_view(3);
        banner.header = vec![crate::overlay::plain_spans("Welcome")];
        banner.footer = vec![crate::overlay::plain_spans("Bye")];
        let with = build(&banner);
        let without = build(&bare);
        assert!(
            with.len() > without.len(),
            "control: a supplied banner must add geometry — {} floats against {}",
            with.len(),
            without.len()
        );
        // The real assertion: the no-banner frame is the same frame it always
        // was, to the float.
        let again = build(&tab_view(3));
        assert_eq!(without, again);
    }

    #[test]
    fn sidebar_draws_title_and_scored_rows() {
        use crate::overlay::{Sidebar, SidebarLine};
        let stats = DebugStats::default();
        let base = HudFrame {
            crosshair: false,
            show_debug: false,
            ..HudFrame::new(&stats)
        };
        let base_verts = HudGeometry::build(&base, 640, 480).vertex_count();

        let side = Sidebar {
            title: crate::overlay::plain_spans("Objectives"),
            lines: vec![
                SidebarLine {
                    label: crate::overlay::plain_spans("Kills"),
                    score: crate::overlay::plain_spans("7"),
                },
                SidebarLine {
                    label: crate::overlay::plain_spans("Deaths"),
                    score: crate::overlay::plain_spans("2"),
                },
            ],
        };
        let frame = HudFrame {
            sidebar: Some(&side),
            ..HudFrame {
                crosshair: false,
                show_debug: false,
                ..HudFrame::new(&stats)
            }
        };
        let with = HudGeometry::build(&frame, 640, 480);
        assert!(
            with.vertex_count() > base_verts,
            "a displayed sidebar must add the panel, title and rows"
        );

        // Anti-vacuity: the score text itself must be drawn, not just the panel.
        // Dropping the scores (blank strings) must reduce the geometry, so a
        // regression that stops rendering scores can't pass this.
        let scoreless = Sidebar {
            title: crate::overlay::plain_spans("Objectives"),
            lines: vec![
                SidebarLine {
                    label: crate::overlay::plain_spans("Kills"),
                    score: Vec::new(),
                },
                SidebarLine {
                    label: crate::overlay::plain_spans("Deaths"),
                    score: Vec::new(),
                },
            ],
        };
        let frame_scoreless = HudFrame {
            sidebar: Some(&scoreless),
            ..HudFrame {
                crosshair: false,
                show_debug: false,
                ..HudFrame::new(&stats)
            }
        };
        let without_scores = HudGeometry::build(&frame_scoreless, 640, 480).vertex_count();
        assert!(
            with.vertex_count() > without_scores,
            "the score glyphs must contribute geometry, not just the labels"
        );
    }

    /// Encode a solid-colour RGBA PNG so a `MemorySource` can stand in for a
    /// real jar in a hermetic test — the same trick
    /// `lodestone_render::gui_atlas`'s own tests use (no GPU, no disk).
    fn solid_png(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
        let mut data = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut data, w, h);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("png header");
            let pixels: Vec<u8> = (0..(w * h)).flat_map(|_| rgba).collect();
            writer.write_image_data(&pixels).expect("png data");
        }
        data
    }

    /// A minimal synthetic pack covering the boss-bar sprite ids exercised
    /// below: two colours' background/progress plates plus one notch-overlay
    /// pair, at vanilla's real 182×5 native size
    /// (`.cache/mc/26.2/client-src/assets/.../gui/sprites/boss_bar/*.png`).
    /// Content is an arbitrary flat colour per id — this is a **geometry**
    /// gate (does the draw reach the atlas and land the right rect?), not a
    /// pixel-colour gate, so what matters is that each id is *present* and
    /// distinct, not what it looks like.
    fn boss_bar_synthetic_atlas() -> GuiAtlas {
        let mut src = lodestone_assets::MemorySource::new("boss-bar-test");
        for (id, rgba) in [
            ("boss_bar/purple_background", [60, 20, 90, 255]),
            ("boss_bar/purple_progress", [170, 60, 220, 255]),
            ("boss_bar/red_background", [90, 20, 20, 255]),
            ("boss_bar/red_progress", [220, 40, 40, 255]),
            ("boss_bar/notched_6_background", [10, 10, 10, 255]),
            ("boss_bar/notched_6_progress", [250, 250, 250, 255]),
        ] {
            src.insert(
                format!("assets/minecraft/textures/gui/sprites/{id}.png"),
                solid_png(182, 5, rgba),
            );
        }
        let manager = lodestone_assets::ResourceManager::new(vec![
            Box::new(src) as Box<dyn lodestone_assets::ResourceSource>
        ]);
        GuiAtlas::build(&manager).expect("synthetic boss-bar atlas must build")
    }

    /// The bounding box (logical pixels) of each 6-vertex (two-triangle)
    /// quad in `verts`, in emission order. `push_sprite_quad` always emits
    /// exactly one quad per call, so grouping by six vertices recovers the
    /// draw's own call order — one entry per `b.sprite`/`b.push_sprite_quad`
    /// invocation.
    fn quad_boxes(verts: &[f32], cw: f32, ch: f32) -> Vec<(f32, f32, f32, f32)> {
        let px = |x: f32| (x + 1.0) * 0.5 * cw;
        let py = |y: f32| (1.0 - y) * 0.5 * ch;
        verts
            .chunks(SPRITE_FLOATS_PER_VERTEX * 6)
            .map(|quad| {
                let mut x0 = f32::MAX;
                let mut y0 = f32::MAX;
                let mut x1 = f32::MIN;
                let mut y1 = f32::MIN;
                for v in quad.chunks(SPRITE_FLOATS_PER_VERTEX) {
                    let (x, y) = (px(v[0]), py(v[1]));
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x);
                    y1 = y1.max(y);
                }
                (x0, y0, x1, y1)
            })
            .collect()
    }

    /// The boss bar's four vanilla clauses (`BossHealthOverlay.extractBar`,
    /// `.cache/mc/26.2/client-src`), each pinned to the layer that actually
    /// emits geometry (`HudGeometry::sprite_verts`, via a real
    /// [`GuiAtlas`]) rather than to [`crate::overlay::BossBarView`]'s model —
    /// the gap this whole fix closes was that every existing gate stopped one
    /// layer above this and a flat `rect_px` reached the screen instead.
    ///
    /// 1. background plate, full 182px, drawn even at zero progress
    /// 2. background notch overlay, full 182px, only when the overlay style
    ///    is not `Progress`
    /// 3. progress fill, **cropped** (not scaled) to
    ///    [`crate::overlay::lerp_discrete_width`] — checked at a half-full
    ///    bar (the brief's own discriminating case) and at `0.2`, which the
    ///    naive `round(progress * 182)` hypothesis gets wrong (36px, not 37)
    /// 4. progress notch overlay, cropped the same way, only when the
    ///    overlay style is not `Progress`
    #[test]
    fn boss_bar_reaches_the_sprite_geometry_layer_not_just_the_model() {
        use crate::overlay::{BossBarView, lerp_discrete_width};
        use lodestone_game::bossbar::{BossBarColor, BossBarOverlay};

        let atlas = boss_bar_synthetic_atlas();
        let stats = DebugStats::default();
        let (w, h) = (640u32, 480u32);
        let (cw, ch) = crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, w, h);
        let bar_x = cw * 0.5 - BOSS_BAR_WIDTH * 0.5;
        let yo = BOSS_BAR_TOP;

        let render = |progress: f32, overlay: BossBarOverlay| -> Vec<(f32, f32, f32, f32)> {
            let bars = [BossBarView {
                title: crate::overlay::plain_spans("Ender Dragon"),
                progress,
                color: BossBarColor::Purple,
                overlay,
            }];
            let frame = HudFrame {
                boss_bars: &bars,
                crosshair: false,
                show_debug: false,
                ..HudFrame::new(&stats)
            };
            let geo = HudGeometry::build_with_gui(&frame, w, h, &atlas);
            quad_boxes(&geo.sprite_verts, cw, ch)
        };

        let mut wrong = Vec::new();
        let mut check = |name: String, got: f32, want: f32| {
            if (got - want).abs() > 0.5 {
                wrong.push(format!("{name}: got {got:.2}, want {want:.2}"));
            }
        };

        // -- clause 1: background, full width, drawn even at zero progress,
        // and clauses 3/4 correctly absent (no fill, overlay is Progress).
        let empty = render(0.0, BossBarOverlay::Progress);
        assert_eq!(
            empty.len(),
            1,
            "progress 0.0 with the Progress overlay must draw only the \
             background plate: got {empty:?}"
        );
        check("empty bg x0".into(), empty[0].0, bar_x);
        check("empty bg x1".into(), empty[0].2, bar_x + BOSS_BAR_WIDTH);
        check("empty bg y0".into(), empty[0].1, yo);
        check("empty bg y1".into(), empty[0].3, yo + BOSS_BAR_HEIGHT);

        // -- clause 3 at full progress: the fill spans the whole 182px too.
        let full = render(1.0, BossBarOverlay::Progress);
        assert_eq!(full.len(), 2, "full progress must draw background + fill: got {full:?}");
        check("full bg x0".into(), full[0].0, bar_x);
        check("full bg x1".into(), full[0].2, bar_x + BOSS_BAR_WIDTH);
        check("full fill x0".into(), full[1].0, bar_x);
        check("full fill x1".into(), full[1].2, bar_x + BOSS_BAR_WIDTH);

        // -- clause 3, the discriminating cases: a half-full bar's fill must
        // cover the *predicted partial* width — not zero, not full, and (at
        // 0.2) not the naive `progress * 182` scale either.
        for (progress, want_px) in [(0.5_f32, 91_i32), (0.2_f32, 37_i32)] {
            let quads = render(progress, BossBarOverlay::Progress);
            assert_eq!(
                quads.len(),
                2,
                "progress {progress} must draw background + fill: got {quads:?}"
            );
            let predicted = lerp_discrete_width(progress, BOSS_BAR_WIDTH as i32);
            assert_eq!(predicted, want_px, "lerp_discrete_width regressed for {progress}");
            let fill = quads[1];
            check(format!("progress {progress} fill x0"), fill.0, bar_x);
            check(format!("progress {progress} fill x1"), fill.2, bar_x + want_px as f32);
            check(format!("progress {progress} fill y0"), fill.1, yo);
            check(format!("progress {progress} fill y1"), fill.3, yo + BOSS_BAR_HEIGHT);
        }

        // -- clauses 2 + 4: the notch overlay draws only when the overlay
        // style is not `Progress`, doubling the quad count at both ends.
        let notched_empty = render(0.0, BossBarOverlay::Notched6);
        assert_eq!(
            notched_empty.len(),
            2,
            "zero progress with a notch overlay must draw background + \
             background-notch, no fill: got {notched_empty:?}"
        );
        let notched_full = render(1.0, BossBarOverlay::Notched6);
        assert_eq!(
            notched_full.len(),
            4,
            "full progress with a notch overlay must draw all four clauses: \
             got {notched_full:?}"
        );

        assert!(wrong.is_empty(), "{wrong:?}");
    }

    #[test]
    fn one_line_is_stable() {
        let stats = DebugStats {
            position: [0.5, 40.0, -3.5],
            fps: 60.0,
            frame_ms: 16.6,
            ..Default::default()
        };
        let line = stats.one_line();
        assert!(line.contains("fps=60"));
        assert!(line.contains("frame=16.60ms"));
    }

    /// Clear `view` to an opaque `rgb` background (Rgba8Unorm is linear, so the
    /// byte value lands verbatim). Used to give the HUD's `Load` pass a known
    /// backdrop for pixel readback.
    #[cfg(test)]
    fn clear_view(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        rgb: [u8; 3],
    ) {
        let color = wgpu::Color {
            r: f64::from(rgb[0]) / 255.0,
            g: f64::from(rgb[1]) / 255.0,
            b: f64::from(rgb[2]) / 255.0,
            a: 1.0,
        };
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("clear"),
        });
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clear-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }
        queue.submit(std::iter::once(encoder.finish()));
    }

    /// Pixel-readback proof that server chat *reaches pixels*, not merely that a
    /// frame counter ticks. Renders the HUD (chat only — no crosshair, overlay,
    /// hotbar or vitals) over a known grey backdrop and inspects the bottom-left
    /// chat region. The discriminator is luminance: the translucent backing
    /// panel is *darker* than the grey background, the near-white glyphs are
    /// *brighter* — so text and panel are counted separately and a blank-but-
    /// panelled line cannot masquerade as rendered text.
    ///
    /// Three frames make the assertion two-sided:
    /// * no message → the region is untouched background (zero of both);
    /// * a whitespace-only line → the panel draws (dark pixels) but no glyphs;
    /// * a real line → glyphs add bright pixels the panel-only frame lacks.
    #[test]
    #[ignore = "requires a GPU adapter"]
    fn chat_text_reaches_pixels() {
        use lodestone_render::{HeadlessTarget, RenderTarget};

        let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
            "headless GPU test opted in via --ignored but no wgpu adapter is available; \
             run on a host with a GPU (or a software adapter such as \
             LIBGL_ALWAYS_SOFTWARE=1 / WGPU_BACKEND=gl), don't 'skip' — a silent pass here \
             would assert nothing",
        );
        let device = ctx.device();
        let queue = ctx.queue();
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let (w, h) = (480u32, 320u32);
        let mut target = HeadlessTarget::new(device, w, h, format);
        let mut hud = HudRenderer::new(device, format);
        let stats = DebugStats::default();

        const BG: u8 = 128;
        // Count bright (glyph) and dark (panel) pixels in the bottom-left chat
        // region, well clear of the bottom-centre hotbar/vitals (which are off
        // anyway) and the top-left debug overlay.
        let x_max = (w as f32 * 0.55) as u32;
        let y_min = (h as f32 * 0.60) as u32;

        let mut render = |chat: &[(&str, f32)]| -> (usize, usize) {
            let frame = target.acquire().expect("headless acquire");
            clear_view(device, queue, frame.view(), [BG, BG, BG]);
            let hud_frame = HudFrame {
                show_debug: false,
                crosshair: false,
                chat,
                ..HudFrame::new(&stats)
            };
            hud.render(device, queue, frame.view(), &hud_frame, w, h);
            let pixels = target.read_texels(device, queue);
            let (mut bright, mut dark) = (0usize, 0usize);
            for y in y_min..h {
                for x in 0..x_max {
                    let i = ((y * w + x) * 4) as usize;
                    let avg = (u32::from(pixels[i])
                        + u32::from(pixels[i + 1])
                        + u32::from(pixels[i + 2]))
                        / 3;
                    if avg > u32::from(BG) + 30 {
                        bright += 1;
                    } else if avg + 30 < u32::from(BG) {
                        dark += 1;
                    }
                }
            }
            (bright, dark)
        };

        let (blank_bright, blank_dark) = render(&[]);
        let (panel_bright, panel_dark) = render(&[(" ", 0.0)]);
        let (text_bright, text_dark) = render(&[("chat works", 0.0)]);

        eprintln!("=== chat readback (headless) ===");
        eprintln!("blank  bright={blank_bright} dark={blank_dark}");
        eprintln!("panel  bright={panel_bright} dark={panel_dark}");
        eprintln!("text   bright={text_bright} dark={text_dark}");

        // No message: pure background — neither panel nor glyphs.
        assert_eq!(
            (blank_bright, blank_dark),
            (0, 0),
            "with no chat, the chat region must be untouched background"
        );
        // A line draws its translucent backing panel (dark) but a space has no
        // glyphs, so almost no bright pixels.
        assert!(panel_dark > 0, "a chat line must draw its backing panel");
        assert!(
            panel_bright < 50,
            "a whitespace-only line must not paint glyph pixels, got {panel_bright}"
        );
        // The glyphs of a real line add bright pixels the panel-only frame lacks
        // — this is the assertion that fails if the text were blank.
        assert!(
            text_dark > 0,
            "the text line must also draw its backing panel"
        );
        assert!(
            text_bright > panel_bright + 150,
            "chat glyphs must reach pixels over the bare panel: text_bright={text_bright}, \
             panel_bright={panel_bright}"
        );
    }

    /// Pixel-readback proof that the **XP bar** reaches pixels once the server
    /// has sent experience — the same "prove it's on screen, with a control"
    /// discipline as the chat gate. The discriminator is *green dominance*: the
    /// vanilla XP fill (and the level digits) are green (`G` well above `R`/`B`),
    /// which the grey background, grey hotbar wells, red health and gold food
    /// pips all fail, so a green-dominant pixel can only be the XP bar.
    ///
    /// Two frames make it two-sided:
    /// * `xp = None` (no server experience) → zero green pixels, no bar;
    /// * `xp = Some((level, progress))` → a run of green fill + digit pixels.
    ///
    /// The control is the load-bearing half: it fails if the bar ever draws
    /// without server-sent experience (the §12.24 "plausible gauge" trap).
    #[test]
    #[ignore = "requires a GPU adapter"]
    fn xp_bar_reaches_pixels() {
        use lodestone_render::{HeadlessTarget, RenderTarget};

        let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
            "headless GPU test opted in via --ignored but no wgpu adapter is available; \
             run on a host with a GPU (or a software adapter such as \
             LIBGL_ALWAYS_SOFTWARE=1 / WGPU_BACKEND=gl), don't 'skip' — a silent pass here \
             would assert nothing",
        );
        let device = ctx.device();
        let queue = ctx.queue();
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let (w, h) = (480u32, 320u32);
        let mut target = HeadlessTarget::new(device, w, h, format);
        let mut hud = HudRenderer::new(device, format);
        let stats = DebugStats::default();

        const BG: u8 = 128;
        // The XP bar and level digits live at the bottom-centre; scan a generous
        // bottom band there. Hotbar/vitals/crosshair are all off so nothing else
        // paints here.
        let x0 = (w as f32 * 0.20) as u32;
        let x1 = (w as f32 * 0.80) as u32;
        let y0 = (h as f32 * 0.78) as u32;

        let mut render = |xp: Option<(i32, f32)>| -> usize {
            let frame = target.acquire().expect("headless acquire");
            clear_view(device, queue, frame.view(), [BG, BG, BG]);
            let hud_frame = HudFrame {
                show_debug: false,
                crosshair: false,
                xp,
                ..HudFrame::new(&stats)
            };
            hud.render(device, queue, frame.view(), &hud_frame, w, h);
            let pixels = target.read_texels(device, queue);
            let mut green = 0usize;
            for y in y0..h {
                for x in x0..x1 {
                    let i = ((y * w + x) * 4) as usize;
                    let (r, g, b) = (
                        u32::from(pixels[i]),
                        u32::from(pixels[i + 1]),
                        u32::from(pixels[i + 2]),
                    );
                    // Green-dominant: clearly more green than red or blue, and
                    // brighter than the grey background so unblended greys and
                    // the gold food pips (high red) are excluded.
                    if g > r + 40 && g > b + 40 && g > u32::from(BG) {
                        green += 1;
                    }
                }
            }
            green
        };

        let no_xp = render(None);
        let with_xp = render(Some((5, 0.5)));

        eprintln!("=== xp bar readback (headless) ===");
        eprintln!("no_xp green={no_xp}");
        eprintln!("with_xp green={with_xp}");

        // Control: off a live server (no experience) the bar must not draw.
        assert_eq!(
            no_xp, 0,
            "without server experience the XP bar must not draw a single green pixel"
        );
        // A half-full level-5 bar paints a wide green fill plus the green level
        // digit — hundreds of pixels. This fails if the bar were blank.
        assert!(
            with_xp > 150,
            "the XP bar's green fill must reach pixels once experience arrives, got {with_xp}"
        );
    }

    /// GPU gate for the player report this fix addresses: "the boss bar ...
    /// is just a solid rectangle and doesn't use the texture pack for it at
    /// all." Runs through the **real vanilla `client.jar`** atlas
    /// (`GuiAtlas::build`), not a synthetic one, because the bug's own
    /// symptom — a flat fill instead of `BossHealthOverlay`'s real
    /// per-colour sprite art — can only be told apart from a correct draw by
    /// looking at the *actual shipped pixels*, which
    /// [`boss_bar_reaches_the_sprite_geometry_layer_not_just_the_model`]
    /// (synthetic solid-colour sprites, no GPU) structurally cannot see.
    ///
    /// Deliberately colour-agnostic per CLAUDE.md's "you cannot predict an
    /// exact composited byte through `ALPHA_BLENDING` on this backend":
    /// every threshold below is **measured from this gate's own renders**
    /// (a background-only frame vs a full-fill frame), not a hand-picked RGB
    /// value, and every assertion is a magnitude/direction claim with
    /// tolerance, never an exact byte.
    #[test]
    #[ignore = "requires a GPU adapter and the vanilla client.jar"]
    fn boss_bar_paints_real_sprite_art_not_a_flat_rectangle() {
        use lodestone_game::bossbar::{BossBarColor, BossBarOverlay};
        use lodestone_render::{HeadlessTarget, RenderTarget};

        use crate::overlay::{BossBarView, lerp_discrete_width, plain_spans};

        let manager = crate::resources::vanilla_manager().expect(
            "GPU gate opted in via --ignored but no vanilla client.jar was found; set \
             LODESTONE_ASSETS to a pack root containing client.jar, or populate \
             .cache/mc/<ver>/client.jar — do NOT skip, a silent pass here asserts nothing",
        );
        let atlas =
            Arc::new(GuiAtlas::build(&manager).expect("build the GUI atlas from client.jar"));

        let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
            "headless GPU test opted in via --ignored but no wgpu adapter is available; \
             run on a host with a GPU, don't 'skip' — a silent pass here asserts nothing",
        );
        let device = ctx.device();
        let queue = ctx.queue();
        let format = wgpu::TextureFormat::Rgba8Unorm;
        // Same (480, 320) the XP gates above use, for the same reason: it is
        // where `calculate_gui_scale(AUTO, w, h) == 1`, so the logical canvas
        // this module lays `BOSS_BAR_WIDTH`/`BOSS_BAR_TOP` into is the
        // physical target 1:1 and the pixel math below needs no scale term.
        let (w, h) = (480u32, 320u32);
        let mut target = HeadlessTarget::new(device, w, h, format);
        let stats = DebugStats::default();

        let mut hud = HudRenderer::new(device, format);
        hud.attach_gui(device, queue, format, atlas);

        const BG: u8 = 128;
        let mut render = |progress: Option<f32>| -> Vec<u8> {
            let bars = [BossBarView {
                title: plain_spans(""),
                progress: progress.unwrap_or(0.0),
                color: BossBarColor::Purple,
                overlay: BossBarOverlay::Progress,
            }];
            let frame = target.acquire().expect("headless acquire");
            clear_view(device, queue, frame.view(), [BG, BG, BG]);
            let hud_frame = HudFrame {
                show_debug: false,
                crosshair: false,
                hotbar: None,
                health: None,
                food: None,
                xp: None,
                boss_bars: if progress.is_some() { &bars } else { &[] },
                ..HudFrame::new(&stats)
            };
            hud.render(device, queue, frame.view(), &hud_frame, w, h);
            target.read_texels(device, queue)
        };

        let bar_x = (w as f32 * 0.5 - BOSS_BAR_WIDTH * 0.5).round() as u32;
        let yo = BOSS_BAR_TOP as u32;
        // Row 2 of the 5-row bar: constant along X in the raw sprite (a
        // horizontal bevel varies by row, not by column — measured directly
        // off `.cache/mc/26.2/client-src`'s `purple_progress.png`), so it is
        // the row to use for the fill/background boundary scan below.
        let mid_row = yo + 2;
        let x_probe = bar_x + 90; // interior column, well clear of the sprite's rounded corners

        let sample = |pixels: &[u8], x: u32, y: u32| -> (i32, i32, i32) {
            let i = ((y * w + x) * 4) as usize;
            (i32::from(pixels[i]), i32::from(pixels[i + 1]), i32::from(pixels[i + 2]))
        };
        let painted = |pixels: &[u8], x: u32, y: u32| -> bool {
            let (r, g, b) = sample(pixels, x, y);
            (r - i32::from(BG)).abs() + (g - i32::from(BG)).abs() + (b - i32::from(BG)).abs() > 30
        };

        let none = render(None);
        let bg_only = render(Some(0.0));
        let full = render(Some(1.0));
        let half = render(Some(0.5));

        // -- negative control: no active boss bar paints nothing at the rect.
        let mut wrong = Vec::new();
        for dy in 0..5 {
            if painted(&none, x_probe, yo + dy) {
                wrong.push(format!(
                    "row {dy}: with no boss bar the rect must stay background, \
                     got {:?}",
                    sample(&none, x_probe, yo + dy)
                ));
            }
        }

        // -- the bug this fixes: a flat rect is one solid colour top to
        // bottom; vanilla's real sprite has a highlight/shadow bevel across
        // its 5 rows. This is the assertion that falls straight out under
        // the pre-fix `rect_px` draw and is the direct pixel-level check of
        // the player's own report.
        let rows: Vec<(i32, i32, i32)> = (0..5).map(|dy| sample(&bg_only, x_probe, yo + dy)).collect();
        let all_identical = rows.windows(2).all(|w| w[0] == w[1]);
        if all_identical {
            wrong.push(format!(
                "the boss bar's 5 rows are all one solid colour — this is the \
                 reported bug (a flat rectangle, no sprite art): rows={rows:?}"
            ));
        }

        // -- clause 3 reaches real pixels, and its width is *measured*, not
        // guessed: the fill must be visibly brighter (blue channel) than the
        // bare background at the same column, or nothing below is
        // meaningful.
        let bg_b = sample(&bg_only, x_probe, mid_row).2;
        let full_b = sample(&full, x_probe, mid_row).2;
        if full_b <= bg_b + 20 {
            wrong.push(format!(
                "the progress fill must be visibly brighter than the bare background \
                 at a filled column (blue channel): background={bg_b}, full={full_b}"
            ));
        }

        // -- the half-full bar's fill edge lands at the *predicted* partial
        // column, not at the background's own full-182px edge and not at
        // zero — the threshold is this gate's own measured midpoint between
        // background and full fill, not a hand-picked byte value.
        let threshold = (bg_b + full_b) / 2;
        let mut half_edge = None;
        for dx in 0..BOSS_BAR_WIDTH as u32 {
            if sample(&half, bar_x + dx, mid_row).2 > threshold {
                half_edge = Some(dx);
            }
        }
        let predicted_edge = lerp_discrete_width(0.5, BOSS_BAR_WIDTH as i32) as u32;
        match half_edge {
            Some(edge) => {
                let diff = (edge as i32 - predicted_edge as i32).abs();
                if diff > 4 {
                    wrong.push(format!(
                        "half-full bar's fill edge should land near the predicted \
                         {predicted_edge}px column, got {edge}px (diff {diff})"
                    ));
                }
            }
            None => wrong.push("a half-full bar must still show some fill".to_string()),
        }

        eprintln!("=== boss bar sprite-art gate (headless) ===");
        eprintln!("bg_only rows @ x={x_probe}: {rows:?}");
        eprintln!("bg_b={bg_b} full_b={full_b} threshold={threshold}");
        eprintln!("half_edge={half_edge:?} predicted_edge={predicted_edge}");

        assert!(wrong.is_empty(), "{wrong:?}");
    }

    /// GPU gate for a live player report: "the xp bar number is too big and too
    /// high." Both halves of that sentence are magnitude claims, not sign
    /// claims, so this predicts vanilla's real numbers and requires the
    /// measurement to land on them — the CLAUDE.md "magnitude species" repair,
    /// not a "some digit painted somewhere" check.
    ///
    /// Runs through the **real vanilla atlas + font** (`HudRenderer::attach_gui`,
    /// `VanillaFont::shared` via `HudRenderer::new`), because
    /// [`xp_bar_reaches_pixels`] above only exercises the jar-less procedural
    /// fallback and would not have caught this: the player was looking at
    /// `sprite_vitals`, a different code path with its own (until now,
    /// independently wrong) scale and offset.
    ///
    /// Two independent renders isolate each claim instead of restating the
    /// source's own constants as the expected value:
    ///
    /// * **"too high"**: render the fill alone (`level: 0, progress: 1.0` — no
    ///   digit, since the digit only draws `if level > 0`) to find the bar's own
    ///   top row from its pixels, then render the digit alone (`level: 5,
    ///   progress: 0.0` — no fill, since the fill only draws `if p > 0.0`) to
    ///   find the digit's top row. The **gap** between them is what
    ///   `ContextualBar.extractExperienceLevel` vs `ContextualBar.top`
    ///   (`ContextualBar.java`) fixes at vanilla's `6` logical px —
    ///   independent of wherever the cluster's own bottom margin happens to
    ///   place the bar, so this cannot pass by coincidentally agreeing with our
    ///   own `by`.
    /// * **"too big"**: the digit-alone render's ink bounding box width, against
    ///   the *real jar font's* advance for `"5"` at scale 1 (correct hypothesis)
    ///   and at scale 2 (the old bug's hypothesis, exactly double) — both
    ///   computed from [`VanillaFont::from_manager`], outside the code under
    ///   test.
    #[test]
    #[ignore = "requires a GPU adapter and the vanilla client.jar"]
    fn xp_level_number_is_the_right_size_and_the_right_distance_above_the_bar() {
        use lodestone_render::{HeadlessTarget, RenderTarget};

        let manager = crate::resources::vanilla_manager().expect(
            "GPU gate opted in via --ignored but no vanilla client.jar was found; set \
             LODESTONE_ASSETS to a pack root containing client.jar, or populate \
             .cache/mc/<ver>/client.jar — do NOT skip, a silent pass here asserts nothing",
        );
        let atlas =
            Arc::new(GuiAtlas::build(&manager).expect("build the GUI atlas from client.jar"));
        let font = VanillaFont::from_manager(&manager).expect("build the vanilla font");

        let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
            "headless GPU test opted in via --ignored but no wgpu adapter is available; \
             run on a host with a GPU, don't 'skip' — a silent pass here asserts nothing",
        );
        let device = ctx.device();
        let queue = ctx.queue();
        let format = wgpu::TextureFormat::Rgba8Unorm;
        // Chosen for `calculate_gui_scale(AUTO, 480, 320) == 1` (see
        // `hud_vitals_draw_the_real_heart_sprite`'s comment), so the logical
        // canvas `sprite_vitals` lays out into is the physical target 1:1 and no
        // scale multiplication enters the pixel math below.
        let (w, h) = (480u32, 320u32);
        let mut target = HeadlessTarget::new(device, w, h, format);
        let stats = DebugStats::default();

        let mut hud = HudRenderer::new(device, format);
        hud.attach_gui(device, queue, format, atlas);
        assert!(
            hud.font_attached(),
            "this gate measures vanilla font metrics; the fixed-advance fallback \
             would make every width prediction below meaningless"
        );

        const BG: u8 = 40;
        let x0 = (w as f32 * 0.20) as u32;
        let x1 = (w as f32 * 0.80) as u32;
        let y0 = (h as f32 * 0.50) as u32;

        // Bounding box of green-dominant pixels in the scan band, or `None` if
        // nothing painted there.
        let mut render_bbox = |xp: Option<(i32, f32)>| -> Option<(u32, u32, u32, u32)> {
            let frame = target.acquire().expect("headless acquire");
            clear_view(device, queue, frame.view(), [BG, BG, BG]);
            let hud_frame = HudFrame {
                show_debug: false,
                crosshair: false,
                xp,
                hotbar: None,
                health: None,
                food: None,
                ..HudFrame::new(&stats)
            };
            hud.render(device, queue, frame.view(), &hud_frame, w, h);
            let pixels = target.read_texels(device, queue);
            let (mut min_x, mut max_x, mut min_y, mut max_y) = (u32::MAX, 0u32, u32::MAX, 0u32);
            let mut found = false;
            for y in y0..h {
                for x in x0..x1 {
                    let i = ((y * w + x) * 4) as usize;
                    let (r, g, b) = (
                        u32::from(pixels[i]),
                        u32::from(pixels[i + 1]),
                        u32::from(pixels[i + 2]),
                    );
                    if g > r + 40 && g > b + 40 && g > u32::from(BG) {
                        found = true;
                        min_x = min_x.min(x);
                        max_x = max_x.max(x);
                        min_y = min_y.min(y);
                        max_y = max_y.max(y);
                    }
                }
            }
            found.then_some((min_x, max_x, min_y, max_y))
        };

        // **The order of these two renders is load-bearing, and it was wrong.**
        //
        // This gate used to render the bar (`level: 0`) first and the digit
        // (`level: 5`) second, and it was red for a reason nothing in it could
        // reveal: `XpFlash::tick` sees `0 → 5` across those two frames as a
        // **level-up**, and a flash at full strength runs the digit's green
        // through `flash_toward_white(…, 1.0)`, i.e. paints it pure white. The
        // digit reached pixels perfectly — 947 painted texels against the bar's
        // 906, its own bounding box six rows higher — and not one of them was
        // green-dominant, so the `expect` below fired.
        //
        // A *world*-species failure in CLAUDE.md's table: the flash landed after
        // this gate, and the gate's premise had been "no such subsystem exists".
        // Reading the test could not show it, because the flaw was in the input.
        //
        // Rendering the digit **first** fixes it without weakening anything:
        // `XpFlash` only triggers when it is already `primed` by a previous
        // frame, so the first render of a fresh `HudRenderer` never flashes, and
        // the following `5 → 0` is a decrease, which never flashes either.
        // Digit alone: no fill (`progress: 0.0`), a single glyph (`level: 5`).
        let digit =
            render_bbox(Some((5, 0.0))).expect("the level digit must paint green pixels");
        // Fill alone: no digit (`level: 0`), full bar (`progress: 1.0`).
        let bar = render_bbox(Some((0, 1.0))).expect("a full XP bar must paint green pixels");
        // Negative control: neither renders without server experience.
        let none = render_bbox(None);

        let (bar_x0, bar_x1, bar_y0, bar_y1) = bar;
        let (digit_x0, digit_x1, digit_y0, digit_y1) = digit;
        let digit_width = digit_x1 - digit_x0 + 1;
        let gap = bar_y0 as i32 - digit_y0 as i32;

        let w1 = font.width("5", 1.0);
        let w2 = font.width("5", 2.0);

        eprintln!("=== xp level-number magnitude gate ===");
        eprintln!("bar bbox    = x[{bar_x0}..{bar_x1}] y[{bar_y0}..{bar_y1}]");
        eprintln!("digit bbox  = x[{digit_x0}..{digit_x1}] y[{digit_y0}..{digit_y1}]");
        eprintln!("digit_width = {digit_width}, gap(bar_top - digit_top) = {gap}");
        eprintln!("real font width('5'): scale1={w1:.1} scale2={w2:.1}");

        assert!(
            none.is_none(),
            "without server experience neither the bar nor the digit may paint, got {none:?}"
        );

        // "too high": vanilla's real gap is exactly 6 logical px
        // (`ContextualBar.java` bar top, `:34-40` text y). The old bug's
        // `line_h` was `(GLYPH_H + 2) * 2 == 18`, three times too far — a wide
        // enough margin that a few px of font-glyph internal padding cannot
        // produce a false pass.
        assert!(
            (4..=10).contains(&gap),
            "the level digit must sit ~6 logical px above the bar's top row \
             (vanilla `ContextualBar`), got a gap of {gap} — bar_top={bar_y0} digit_top={digit_y0}"
        );

        // "too big": the digit's ink must match the real font's scale-1 advance,
        // not scale-2's (which is exactly double).
        assert!(
            (digit_width as f32) < w2 - 1.0,
            "the level digit is as wide as scale 2 predicts ({w2:.1}px) — the old \
             `let scale = 2.0;` bug is back, got digit_width={digit_width}"
        );
        assert!(
            (digit_width as f32) <= w1 + 2.0,
            "the level digit is wider than scale 1's real font advance ({w1:.1}px) \
             allows, got digit_width={digit_width}"
        );
    }

    /// GPU gate: the **title/subtitle** overlay and the **action bar** must reach
    /// pixels once a server sends them, and must paint **nothing** when empty.
    ///
    /// This is the "show me pixels, with a control" shape, applied to the text
    /// path (the strongest control per the director's template): an empty overlay
    /// and a populated one must give measurably different coverage inside the
    /// widget's own rect, or the text path has proven nothing.
    ///
    /// Two independent bands are scanned — the title's mid-screen rect and the
    /// action bar's lower-centre rect — and each state paints only its own band.
    /// That isolation is a second control: a blanket-fill or wrong-clear bug would
    /// light the *other* band and fail. Everything else (hotbar, vitals,
    /// crosshair, debug) is off so nothing else paints in either band.
    #[test]
    #[ignore = "requires a GPU adapter; run with --ignored"]
    fn title_and_action_bar_reach_pixels() {
        use lodestone_render::{HeadlessTarget, RenderTarget};

        let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
            "headless GPU test opted in via --ignored but no wgpu adapter is available; \
             run on a host with a GPU (or a software adapter such as \
             LIBGL_ALWAYS_SOFTWARE=1 / WGPU_BACKEND=gl), don't 'skip' — a silent pass here \
             would assert nothing",
        );
        let device = ctx.device();
        let queue = ctx.queue();
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let (w, h) = (480u32, 320u32);
        let mut target = HeadlessTarget::new(device, w, h, format);
        let mut hud = HudRenderer::new(device, format);
        let stats = DebugStats::default();

        const BG: u8 = 128;
        // Title band: mid-screen, centred (title draws at y≈0.40h, tall). Action
        // band: lower-centre, above the (absent) hotbar/vitals. x kept central so
        // the bottom-left chat feed never intrudes.
        let xa = (w as f32 * 0.15) as u32;
        let xb = (w as f32 * 0.85) as u32;
        let title_y0 = (h as f32 * 0.30) as u32;
        let title_y1 = (h as f32 * 0.64) as u32;
        let act_y0 = (h as f32 * 0.78) as u32;
        let act_y1 = (h as f32 * 0.96) as u32;

        // Count near-white text texels (white glyphs on the grey clear) in a band.
        let bright_in = |pixels: &[u8], y0: u32, y1: u32| -> usize {
            let mut n = 0usize;
            for y in y0..y1 {
                for x in xa..xb {
                    let i = ((y * w + x) * 4) as usize;
                    let (r, g, b) = (pixels[i], pixels[i + 1], pixels[i + 2]);
                    if r > BG + 40 && g > BG + 40 && b > BG + 40 {
                        n += 1;
                    }
                }
            }
            n
        };

        let mut render = |title: Option<(Vec<TextSpan>, Option<Vec<TextSpan>>, f32)>,
                          action_bar: Option<(Vec<TextSpan>, f32)>|
         -> (usize, usize) {
            let frame = target.acquire().expect("headless acquire");
            clear_view(device, queue, frame.view(), [BG, BG, BG]);
            let hud_frame = HudFrame {
                show_debug: false,
                crosshair: false,
                title,
                action_bar,
                ..HudFrame::new(&stats)
            };
            hud.render(device, queue, frame.view(), &hud_frame, w, h);
            let pixels = target.read_texels(device, queue);
            (
                bright_in(&pixels, title_y0, title_y1),
                bright_in(&pixels, act_y0, act_y1),
            )
        };

        let (empty_title, empty_act) = render(None, None);
        let (shown_title, title_leak_act) = render(
            Some((
                crate::overlay::plain_spans("TITLE"),
                Some(crate::overlay::plain_spans("subtitle")),
                1.0,
            )),
            None,
        );
        let (act_leak_title, shown_act) =
            render(None, Some((crate::overlay::plain_spans("Action bar!"), 1.0)));

        eprintln!("=== title/action-bar readback (headless) ===");
        eprintln!("empty:  title_band={empty_title} act_band={empty_act}");
        eprintln!("title:  title_band={shown_title} act_band={title_leak_act}");
        eprintln!("action: title_band={act_leak_title} act_band={shown_act}");

        // Controls: with no server title/action-bar, neither band paints a pixel.
        assert_eq!(
            (empty_title, empty_act),
            (0, 0),
            "an empty HUD must not paint the title or action-bar rects"
        );
        // The title's large glyphs + subtitle cover hundreds of texels.
        assert!(
            shown_title > 100,
            "a server-sent title must reach pixels in its rect, got {shown_title}"
        );
        // The action-bar line is smaller but still tens of texels of white text.
        assert!(
            shown_act > 40,
            "a server-sent action bar must reach pixels in its rect, got {shown_act}"
        );
        // Isolation control: each widget paints only its own band. A blanket-fill
        // or wrong-clear bug would light the other band and trip these.
        assert_eq!(
            title_leak_act, 0,
            "the title overlay must not bleed into the action-bar rect"
        );
        assert_eq!(
            act_leak_title, 0,
            "the action bar must not bleed into the title rect"
        );
    }

    /// **The closing gate for the HUD-textures island**: proves the survival
    /// vitals draw from the *actual vanilla heart sprite in `client.jar`*, not
    /// the procedural fallback, by comparing rendered pixels texel-for-texel
    /// against the jar art — then EXECUTES the negative control (no atlas
    /// attached) and confirms the same assertion *fails*. A gate never watched
    /// fail proves nothing; "it draws" is not a gate.
    ///
    /// sRGB note: the atlas uploads as `Rgba8UnormSrgb` and we render into an
    /// `Rgba8UnormSrgb` target, so the sample→tint→store roundtrip re-encodes
    /// back to ~the source bytes. We compare only *opaque* source texels — the
    /// heart's transparent corners show the backdrop and carry no identity — and
    /// at an integer 2× scale each texel maps to a clean 2×2 Nearest block.
    #[test]
    #[ignore = "requires a GPU adapter and the vanilla client.jar"]
    fn hud_vitals_draw_the_real_heart_sprite() {
        use lodestone_assets::Image;
        use lodestone_render::{HeadlessTarget, RenderTarget};

        let manager = crate::resources::vanilla_manager().expect(
            "GPU gate opted in via --ignored but no vanilla client.jar was found; set \
             LODESTONE_ASSETS to a pack root containing client.jar, or populate \
             .cache/mc/<ver>/client.jar — do NOT skip, a silent pass here asserts nothing",
        );
        let atlas =
            Arc::new(GuiAtlas::build(&manager).expect("build the GUI atlas from client.jar"));

        // The source art we must reproduce on screen.
        let heart_png = manager
            .read("assets/minecraft/textures/gui/sprites/hud/heart/full.png")
            .expect("client.jar must carry hud/heart/full.png");
        let heart = Image::decode_png(&heart_png).expect("decode hud/heart/full.png");
        assert_eq!(
            (heart.width, heart.height),
            (9, 9),
            "the heart sprite is 9x9 native"
        );

        let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
            "headless GPU test opted in via --ignored but no wgpu adapter is available; \
             run on a host with a GPU, don't 'skip' — a silent pass here asserts nothing",
        );
        let device = ctx.device();
        let queue = ctx.queue();
        // sRGB target so the sampler's linear decode is re-encoded on store,
        // letting opaque texels land near the source PNG bytes.
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let (w, h) = (480u32, 320u32);
        let mut target = HeadlessTarget::new(device, w, h, format);
        let stats = DebugStats::default();

        // A backdrop that is neither red (heart) nor grey, so an opaque heart
        // texel can never be mistaken for the background.
        const BG: [u8; 3] = [24, 96, 176];

        // Only health on: no hotbar, XP or hunger, so the hearts sit at a
        // location we can compute exactly. `(w, h) = (480, 320)` is chosen
        // specifically so `calculate_gui_scale(AUTO, 480, 320) == 1` — below
        // vanilla's 320-logical-pixel-wide floor at any scale above 1 — so the
        // logical canvas `HudGeometry::build_inner` lays `sprite_vitals` into
        // is identical to this physical target and no scale multiplication
        // enters the picture here at all. `sprite_vitals` draws hearts at their
        // native 9×9 size (no more hardcoded ×2 — see its own doc comment), the
        // first at `xLeft == guiWidth/2 - 91` on vanilla's own `yLineBase`.
        //
        // **`y0` was the hardcoded `h - 19`, and that was correct for the wrong
        // reason.** The hearts used to be stacked upward from a `cluster_top`
        // that moved with the hotbar and the XP bar, and this fixture supplies
        // neither, so `h - 6 - 9 - 4` happened to be `h - 19`. Vanilla's
        // `yLineBase` is `guiHeight - 39` and takes no such branch, so correcting
        // the draw moved the row 20 px and this gate failed — correctly. Derived
        // through [`vitals_line_base`] now, the same call the draw makes.
        let hud_frame = HudFrame {
            show_debug: false,
            crosshair: false,
            health: Some(20.0),
            food: None,
            xp: None,
            hotbar: None,
            ..HudFrame::new(&stats)
        };
        let s = 1u32;
        let cx = w / 2;
        let x0 = cx - 91;
        let y0 = vitals_line_base(h as f32) as u32;

        // Render one frame with `hud`, read it back, and score how many *opaque*
        // heart texels match the jar sprite within tolerance after the 2× Nearest
        // downsample.
        let mut score = |hud: &mut HudRenderer, tag: &str| -> (usize, usize) {
            let frame = target.acquire().expect("headless acquire");
            clear_view(device, queue, frame.view(), BG);
            hud.render(device, queue, frame.view(), &hud_frame, w, h);
            let pixels = target.read_texels(device, queue);
            const TOL: i32 = 24;
            let (mut opaque, mut matched) = (0usize, 0usize);
            for ty in 0..9u32 {
                for tx in 0..9u32 {
                    let si = ((ty * 9 + tx) * 4) as usize;
                    if heart.rgba[si + 3] < 250 {
                        continue; // transparent corner — no identity
                    }
                    opaque += 1;
                    let px = x0 + tx * s + s / 2;
                    let py = y0 + ty * s + s / 2;
                    let di = ((py * w + px) * 4) as usize;
                    let dr = i32::from(pixels[di]) - i32::from(heart.rgba[si]);
                    let dg = i32::from(pixels[di + 1]) - i32::from(heart.rgba[si + 1]);
                    let db = i32::from(pixels[di + 2]) - i32::from(heart.rgba[si + 2]);
                    if dr.abs() <= TOL && dg.abs() <= TOL && db.abs() <= TOL {
                        matched += 1;
                    }
                }
            }
            eprintln!("{tag}: matched {matched}/{opaque} opaque heart texels");
            (matched, opaque)
        };

        // Positive: atlas attached → real heart sprite → high match.
        let mut lit = HudRenderer::new(device, format);
        lit.attach_gui(device, queue, format, Arc::clone(&atlas));
        let (pos_matched, opaque) = score(&mut lit, "vanilla-atlas");
        assert!(
            opaque > 20,
            "the heart sprite must have a solid opaque body, got {opaque}"
        );

        // Negative control, EXECUTED: no atlas → procedural fallback → the same
        // region does NOT reproduce the jar heart, so the match collapses.
        let mut dark = HudRenderer::new(device, format);
        let (neg_matched, _) = score(&mut dark, "procedural-fallback (negative control)");

        let pos_frac = pos_matched as f32 / opaque as f32;
        let neg_frac = neg_matched as f32 / opaque as f32;
        eprintln!("=== heart-sprite gate: vanilla={pos_frac:.2} fallback={neg_frac:.2} ===");

        // Load-bearing: vanilla pixels match the jar; the fallback fails the very
        // same check; and the delta is wide enough that no coincidence passes
        // both.
        assert!(
            pos_frac > 0.80,
            "with the vanilla atlas the rendered hearts must reproduce hud/heart/full.png, \
             got {pos_matched}/{opaque}"
        );
        assert!(
            neg_frac < 0.40,
            "negative control failed to fail: the procedural fallback reproduced the jar \
             heart sprite ({neg_matched}/{opaque}) — the gate would be vacuous"
        );
        assert!(
            pos_frac - neg_frac > 0.40,
            "vanilla vs fallback delta too small to prove the atlas is what reaches pixels: \
             vanilla={pos_frac:.2} fallback={neg_frac:.2}"
        );
    }

    /// The scoreboard sidebar's two background plates, predicted from
    /// `Hud.displayScoreboardSidebar` (`.cache/mc/26.2/client-src`) rather than
    /// eyeballed — the *magnitude* species this repo warns against otherwise.
    /// Content is chosen so the 1x/2x hypotheses diverge everywhere (title
    /// 30px vs 60px; row widths 54/36 vs 108/72, never coinciding after a
    /// clamp) and so the two rows' label/score lengths are pairwise-distinct,
    /// which a transposed measurement could not survive.
    #[test]
    fn sidebar_panel_lands_on_vanillas_own_geometry_not_a_2x_pitch() {
        let plain = |s: &str| {
            vec![TextSpan {
                text: s.to_string(),
                style: lodestone_model::text::TextStyle::default(),
            }]
        };
        let line = |label: &str, score: &str| crate::overlay::SidebarLine {
            label: plain(label),
            score: plain(score),
        };
        let sidebar = Sidebar {
            title: plain("Kills"),
            lines: vec![line("Alice", "11"), line("Bob", "7")],
        };
        let stats = DebugStats::default();
        let frame = HudFrame {
            show_debug: false,
            crosshair: false,
            sidebar: Some(&sidebar),
            ..HudFrame::new(&stats)
        };
        let (w, h) = (640u32, 480u32);
        let geo = HudGeometry::build(&frame, w, h);
        let (cw, ch) = crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, w, h);

        // Independently hand-derived from `Hud.displayScoreboardSidebar` and
        // the shell's fixed-advance jar-less font (`(GLYPH_W + 1) * scale` per
        // visible char, `GLYPH_W == 5`) — not by calling the code under test.
        let str_w = |s: &str| s.chars().count() as f32 * (font::GLYPH_W as f32 + 1.0);
        let spacer_w = str_w(": ");
        let title_w = str_w("Kills");
        let row0_w = str_w("Alice") + spacer_w + str_w("11");
        let row1_w = str_w("Bob") + spacer_w + str_w("7");
        let width = title_w.max(row0_w).max(row1_w);
        assert!(
            (width - 54.0).abs() < f32::EPSILON,
            "hand check: expected width 54.0, derived {width} \
             (title {title_w}, row0 {row0_w}, row1 {row1_w})"
        );
        let height = 2.0 * 9.0;
        let bottom = ch / 2.0 + height / 3.0;
        let left = cw - width - 3.0;
        let right = cw - 3.0 + 2.0;
        let header_y = bottom - height;
        let plate_x = left - 2.0;
        let plate_w = right - plate_x;

        let px = |x: f32| (x + 1.0) * 0.5 * cw;
        let py = |y: f32| (1.0 - y) * 0.5 * ch;
        let mut header_bounds: Option<(f32, f32, f32, f32)> = None;
        let mut body_bounds: Option<(f32, f32, f32, f32)> = None;
        for chunk in geo.verts.chunks(FLOATS_PER_VERTEX) {
            let (x, y) = (px(chunk[0]), py(chunk[1]));
            let (r, g, b, a) = (chunk[2], chunk[3], chunk[4], chunk[5]);
            if r == 0.0 && g == 0.0 && b == 0.0 && (a - SIDEBAR_HEADER_BG_ALPHA).abs() < 1e-4 {
                let e = header_bounds.get_or_insert((x, y, x, y));
                *e = (e.0.min(x), e.1.min(y), e.2.max(x), e.3.max(y));
            } else if r == 0.0 && g == 0.0 && b == 0.0 && (a - SIDEBAR_BODY_BG_ALPHA).abs() < 1e-4 {
                let e = body_bounds.get_or_insert((x, y, x, y));
                *e = (e.0.min(x), e.1.min(y), e.2.max(x), e.3.max(y));
            }
        }
        let header = header_bounds
            .expect("the header plate must draw at exactly SIDEBAR_HEADER_BG_ALPHA");
        let body = body_bounds.expect("the body plate must draw at exactly SIDEBAR_BODY_BG_ALPHA");

        let mut mismatches = Vec::new();
        let mut check = |name: &str, got: f32, want: f32| {
            if (got - want).abs() > 0.5 {
                mismatches.push(format!("{name}: got {got:.2}, want {want:.2}"));
            }
        };
        check("header x0", header.0, plate_x);
        check("header x1", header.2, plate_x + plate_w);
        check("header y0", header.1, header_y - 10.0);
        check("header y1", header.3, header_y - 1.0);
        check("body x0", body.0, plate_x);
        check("body x1", body.2, plate_x + plate_w);
        check("body y0", body.1, header_y - 1.0);
        check("body y1", body.3, bottom);
        assert!(
            mismatches.is_empty(),
            "sidebar panel diverged from vanilla's own geometry: {mismatches:?}"
        );
    }

    /// The boss bar's fixed native rect —
    /// `BossHealthOverlay.BAR_WIDTH`/`BAR_HEIGHT` (182×5,
    /// `.cache/mc/26.2/client-src`) and `extractRenderState`'s `yOffset`
    /// arithmetic — not a canvas-relative width or this HUD's ambient 2×
    /// text pitch.
    #[test]
    fn boss_bar_lands_on_vanillas_fixed_182x5_rect_not_a_canvas_fraction() {
        use lodestone_game::bossbar::{BossBarColor, BossBarOverlay};

        // Zero progress so the background plate is the *only* sprite quad —
        // this test's job is placement, not the fill's width (covered by
        // `boss_bar_reaches_the_sprite_geometry_layer_not_just_the_model`).
        let bars = vec![BossBarView {
            title: vec![TextSpan {
                text: "Boss".to_string(),
                style: lodestone_model::text::TextStyle::default(),
            }],
            progress: 0.0,
            color: BossBarColor::Red,
            overlay: BossBarOverlay::Progress,
        }];
        let stats = DebugStats::default();
        let frame = HudFrame {
            show_debug: false,
            crosshair: false,
            boss_bars: &bars,
            ..HudFrame::new(&stats)
        };
        let (w, h) = (640u32, 480u32);
        let atlas = boss_bar_synthetic_atlas();
        let geo = HudGeometry::build_with_gui(&frame, w, h, &atlas);
        let (cw, ch) = crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, w, h);

        let bar_x = cw * 0.5 - BOSS_BAR_WIDTH * 0.5;
        let yo = BOSS_BAR_TOP;

        let quads = quad_boxes(&geo.sprite_verts, cw, ch);
        assert_eq!(
            quads.len(),
            1,
            "zero progress with the Progress overlay must draw exactly the \
             background plate: got {quads:?}"
        );
        let bg = quads[0];
        let mut mismatches = Vec::new();
        let mut check = |name: &str, got: f32, want: f32| {
            if (got - want).abs() > 0.5 {
                mismatches.push(format!("{name}: got {got:.2}, want {want:.2}"));
            }
        };
        check("bar x0", bg.0, bar_x);
        check("bar x1", bg.2, bar_x + BOSS_BAR_WIDTH);
        check("bar y0", bg.1, yo);
        check("bar y1", bg.3, yo + BOSS_BAR_HEIGHT);
        assert!(
            mismatches.is_empty(),
            "boss bar diverged from vanilla's fixed 182x5 rect: {mismatches:?}"
        );
    }
}

/// Gate for the recipe-unlock toast draw (issue #163).
///
/// The toast timing (`RecipeToastQueue`) landed unit-tested in `lodestone-game`
/// and reached zero pixels because `hud.rs` never rendered it. This measures the
/// draw, in the rect `Toast.java` itself specifies.
#[cfg(test)]
mod recipe_toast_gate {
    use super::*;

    /// A frame with the debug overlay and crosshair off, so the **only** thing
    /// that can paint is the toast.
    ///
    /// This matters: a control asserting "nothing paints here" is worthless if
    /// something else already does, and this repo has burned a cycle on exactly
    /// that (a sky control that failed at 3.5% because the first-person bare arm
    /// was drawing, a premise false since long before the feature existed). The
    /// `no_toast_frame_paints_nothing_in_the_toast_rect` test below *verifies*
    /// this premise rather than assuming it.
    fn bare_frame<'a>(stats: &'a DebugStats) -> HudFrame<'a> {
        let mut f = HudFrame::new(stats);
        f.show_debug = false;
        f.crosshair = false;
        f
    }

    fn icon(name: &str) -> HotbarSlot {
        HotbarSlot {
            item: lodestone_assets::ResourceLocation::parse(name).expect("valid id"),
            count: 1,
            damage: None,
            max_damage: None,
            enchanted: false,
            dyed_color: None,
            potion_color: None,
        }
    }

    const W: u32 = 640;
    const H: u32 = 480;

    /// Covered sample cells and their bounding box inside `rect`, an
    /// `(x0, y0, x1, y1)` NDC box — same CPU rasteriser the panel gate uses.
    /// Returns the box because a bare fraction cannot distinguish a
    /// uniform-but-wrong frame from a localised blob.
    fn coverage(
        verts: &[f32],
        rect: (f32, f32, f32, f32),
        res: usize,
    ) -> (usize, usize, Option<(f32, f32, f32, f32)>) {
        let (rx0, ry0, rx1, ry1) = rect;
        let to_ndc = |i: usize| -1.0 + 2.0 * (i as f32 + 0.5) / res as f32;
        let (mut covered, mut inside) = (0usize, 0usize);
        let mut bbox: Option<(f32, f32, f32, f32)> = None;
        for gy in 0..res {
            for gx in 0..res {
                let (px, py) = (to_ndc(gx), to_ndc(gy));
                if px < rx0 || px > rx1 || py < ry0 || py > ry1 {
                    continue;
                }
                inside += 1;
                let mut hit = false;
                for tri in verts.chunks_exact(FLOATS_PER_VERTEX * 3) {
                    let (ax, ay) = (tri[0], tri[1]);
                    let (bx, by) = (tri[FLOATS_PER_VERTEX], tri[FLOATS_PER_VERTEX + 1]);
                    let (cx, cy) = (tri[FLOATS_PER_VERTEX * 2], tri[FLOATS_PER_VERTEX * 2 + 1]);
                    let d = (bx - ax) * (cy - ay) - (cx - ax) * (by - ay);
                    if d.abs() < f32::EPSILON {
                        continue;
                    }
                    let w0 = ((bx - px) * (cy - py) - (cx - px) * (by - py)) / d;
                    let w1 = ((cx - px) * (ay - py) - (ax - px) * (cy - py)) / d;
                    let w2 = 1.0 - w0 - w1;
                    if w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0 {
                        hit = true;
                        break;
                    }
                }
                if hit {
                    covered += 1;
                    bbox = Some(match bbox {
                        None => (px, py, px, py),
                        Some((x0, y0, x1, y1)) => (x0.min(px), y0.min(py), x1.max(px), y1.max(py)),
                    });
                }
            }
        }
        (covered, inside, bbox)
    }

    /// The toast rect in NDC, from [`recipe_toast_rect`] — **the same expression
    /// the draw calls**, never a restated `canvas_w - 160.0`.
    fn toast_rect_ndc(visible_portion: f32) -> (f32, f32, f32, f32) {
        let (cw, ch) = crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, W, H);
        let (x, y, tw, th) = recipe_toast_rect(cw, visible_portion);
        (
            2.0 * x / cw - 1.0,
            1.0 - 2.0 * (y + th) / ch,
            2.0 * (x + tw) / cw - 1.0,
            1.0 - 2.0 * y / ch,
        )
    }

    /// **The control's premise, verified rather than assumed**: with no toast,
    /// nothing at all paints in the toast's rect.
    ///
    /// If this ever fails, every "the toast drew" assertion below is measuring
    /// somebody else's pixels and must be re-derived.
    #[test]
    fn no_toast_frame_paints_nothing_in_the_toast_rect() {
        let stats = DebugStats::default();
        let geo = HudGeometry::build(&bare_frame(&stats), W, H);
        let (covered, inside, bbox) = coverage(&geo.verts, toast_rect_ndc(1.0), 96);
        assert!(inside > 0, "the toast rect must contain sample points");
        assert_eq!(
            covered, 0,
            "something other than the toast already paints the top-right \
             {inside}-cell rect (bbox {bbox:?}) — the positive gate's premise is \
             false and its rect must be re-derived"
        );
    }

    /// The toast covers its own rect — `Toast.java`'s `xPos`/`yPos`/`width`/
    /// `height`, at rest.
    #[test]
    fn a_recipe_toast_covers_toast_javas_own_rect() {
        let stats = DebugStats::default();
        let mut frame = bare_frame(&stats);
        frame.recipe_toast = Some(RecipeToastView {
            station: icon("minecraft:crafting_table"),
            unlocked: icon("minecraft:torch"),
            visible_portion: 1.0,
        });
        let geo = HudGeometry::build(&frame, W, H);
        let rect = toast_rect_ndc(1.0);
        let (covered, inside, bbox) = coverage(&geo.verts, rect, 96);
        let fraction = covered as f32 / inside as f32;
        assert!(
            fraction > 0.9,
            "the toast must fill its rect: covered {covered}/{inside} \
             ({fraction:.3}) in rect {rect:?}, covered bbox {bbox:?}"
        );
    }

    /// The toast is anchored to the **right edge and the very top**, not inset
    /// by a margin.
    ///
    /// This is the assertion that catches a transcription of `yPos` as "some
    /// top margin": `yPos(firstSlotIndex) == firstSlotIndex * height()`
    /// (`Toast.java`), and with one toast `firstSlotIndex == 0`, so the
    /// top edge is `y == 0` exactly. Predicted from the definition, not the
    /// call site.
    #[test]
    fn the_toast_is_anchored_to_the_top_right_corner() {
        let (cw, _) = crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, W, H);
        let (x, y, tw, th) = recipe_toast_rect(cw, 1.0);
        assert_eq!(y, 0.0, "the first toast sits flush with the top of the screen");
        assert_eq!(tw, 160.0, "Toast::width()");
        assert_eq!(th, 32.0, "Toast::height()");
        assert_eq!(
            x + tw,
            cw,
            "at full visibility the toast's right edge is the screen's right edge"
        );
        // The slide is a *scaling of the width*, so half-visible puts the left
        // edge exactly 80 logical pixels from the right edge — and this differs
        // from the wrong hypothesis (a fixed x that ignores visible_portion),
        // which would still report `cw - 160`.
        let (half_x, _, _, _) = recipe_toast_rect(cw, 0.5);
        assert_eq!(half_x, cw - 80.0, "xPos scales the width by visible_portion");
        assert_ne!(
            half_x,
            cw - 160.0,
            "a fixed-margin transcription would fail to move at all"
        );
    }
}
