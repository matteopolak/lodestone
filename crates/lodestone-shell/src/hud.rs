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
use std::time::Instant;

use lodestone_render::{
    BUBBLE_SIZE, BlockModels, GpuAtlas, GuiAtlas, GuiSpriteQuad, ModelVertex, bubble_position,
    bubble_row,
};

use lodestone_assets::ItemAtlas;
use lodestone_model::text::{TextColor, TextSpan};

use item_icon::{ColourStream, IconAssets, IconRenderer, IconSink, SpecialIconDraw};

use crate::overlay::{BossBarView, Sidebar};

/// Text scale every HUD string is drawn at.
pub(crate) const HUD_TEXT_SCALE: f32 = 2.0;

/// Padding between a HUD panel's edge and its content.
pub(crate) const HUD_MARGIN: f32 = 6.0;

/// Padding above and below the chat input's text inside its background strip,
/// in unscaled logical pixels.
///
/// Vanilla's input band is `fill(2, height - 14, width - 2, height - 2, …)`
/// (`ChatScreen.java:272`) around an `EditBox` whose text sits at `height - 12`
/// (`:56`) — 2px above the text and 2px below it. It is scaled by the chat pose
/// scale at every use, alongside the glyph height, so the strip stays wrapped
/// around the text at any chat scale.
const INPUT_STRIP_PAD: f32 = 2.0;

/// Vertical pitch between two HUD text lines.
#[must_use]
pub(crate) fn hud_line_h() -> f32 {
    (font::GLYPH_H as f32 + 2.0) * HUD_TEXT_SCALE
}

/// `DebugScreenOverlay.MARGIN_LEFT`/`MARGIN_RIGHT`/`MARGIN_TOP`, all `2`
/// (`DebugScreenOverlay.java:50-52`).
///
/// **Not [`HUD_MARGIN`]**: the F3 overlay is vanilla's own screen with vanilla's
/// own metrics, and it draws in the already-`gui_scale`-divided logical canvas,
/// so it needs no HUD-side scaling of any kind.
pub(crate) const DEBUG_MARGIN: f32 = 2.0;

/// The F3 overlay's line pitch — vanilla's literal `9`
/// (`DebugScreenOverlay.java:278`), not [`hud_line_h`]'s `(GLYPH_H + 2) *
/// HUD_TEXT_SCALE`.
///
/// The overlay used to use the HUD's pitch at the HUD's 2× text scale, which is
/// what "the text is way too big" was: exactly the mistake the XP level number's
/// own comment records, one screen over.
pub(crate) const DEBUG_LINE_H: f32 = 9.0;

/// The plate behind each F3 overlay line — vanilla's
/// `fill(left - 1, top - 1, left + width + 1, top + height - 1, -1873784752)`,
/// i.e. `0x90505050` (`DebugScreenOverlay.extractLines`). The shell had no plate
/// at all before issue #197, which is why the overlay was unreadable over bright
/// terrain.
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

/// The Tab player-list overlay's panel geometry.
///
/// **Exists so the draw and its gate share one expression rather than two that
/// agree today.** A pixel gate that recomputed `y` from its own copy of this
/// arithmetic would keep passing after the panel moved — a control whose
/// premise is false in the safe-looking direction. `build_inner` constructs one
/// of these and draws from it; the gate constructs one from the same inputs and
/// measures against it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct TabPanel {
    /// Left edge, in logical canvas pixels.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Panel width.
    pub w: f32,
    /// Panel height.
    pub h: f32,
    /// Vertical pitch between lines.
    pub line_h: f32,
    /// Inset from the panel edge to its content.
    pub margin: f32,
    /// How many footer lines the panel was sized for.
    pub footer_len: usize,
}

impl TabPanel {
    /// Lay the panel out for a canvas and a content census.
    ///
    /// `widest_banner` is the widest header/footer line in pixels, measured
    /// with the same font the draw uses; it only ever *widens* the panel, so
    /// passing `0.0` reproduces the pre-banner geometry exactly — which is why
    /// the existing no-banner pixel gate is unaffected by this type existing.
    pub fn new(
        canvas_w: f32,
        canvas_h: f32,
        header_len: usize,
        rows: usize,
        footer_len: usize,
        widest_banner: f32,
    ) -> Self {
        let line_h = hud_line_h();
        let margin = HUD_MARGIN;
        // Counted before `y`, or the panel stops being centred about its own
        // content the moment a server sends a banner. The `+ 1` is the
        // "PLAYERS (n)" caption; `rows.max(1)` keeps an empty list's panel from
        // collapsing, which is the pre-existing behaviour.
        let lines = header_len + 1 + rows.max(1) + footer_len;
        let h = lines as f32 * line_h + margin * 2.0;
        let w = (canvas_w * 0.5)
            .max(if widest_banner > 0.0 {
                widest_banner + margin * 2.0
            } else {
                0.0
            })
            .min((canvas_w - margin * 2.0).max(0.0));
        Self {
            x: (canvas_w * 0.5).floor() - w * 0.5,
            y: (canvas_h - h) * 0.5,
            w,
            h,
            line_h,
            margin,
            footer_len,
        }
    }

    /// Baseline of header line `i`, counting down from the panel's top inset.
    pub fn header_y(&self, i: usize) -> f32 {
        self.y + self.margin + i as f32 * self.line_h
    }

    /// Baseline of footer line `i`, anchored off the panel's **bottom**.
    ///
    /// Deliberately not `header_y(header_len + 1 + rows + i)`: the panel is
    /// sized for `rows.max(1)` while the row loop advances by `rows`, so an
    /// empty player list would pull the footer up into the gap.
    pub fn footer_y(&self, i: usize) -> f32 {
        self.y + self.h - self.margin - (self.footer_len - i) as f32 * self.line_h
    }

    /// x for a line of width `text_w` centred in the panel — the header and
    /// footer alignment, as vanilla does it.
    pub fn centred_x(&self, text_w: f32) -> f32 {
        self.x + (self.w - text_w) * 0.5
    }

    /// x for the left-aligned caption and player rows.
    pub fn left_x(&self) -> f32 {
        self.x + self.margin
    }

    /// Horizontal centre of the panel, for a gate asking *where* a line sits.
    pub fn centre_x(&self) -> f32 {
        self.x + self.w * 0.5
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
    /// Smoothed frames per second.
    pub fps: f32,
    /// Last frame time in milliseconds.
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
    /// Uploaded (non-empty) mesh sections.
    pub section_count: usize,
    /// Quads currently resident.
    pub quads: usize,
    /// Approximate mesh VRAM in bytes.
    pub vram_bytes: usize,
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
    /// (`Sim::difficulty`) — `None` until the first report arrives (issue
    /// #411). `ServerDifficulty` reached a real, tested ECS fold in `44485e4`
    /// but nothing in the shell read it; this is that last hop.
    pub difficulty: Option<(lodestone_model::Difficulty, bool)>,
    /// Sky and block light at the player's feet, as the client's own world
    /// reports them — `None` before login or for an unloaded section, which is
    /// the honest "no data" state and is drawn as such.
    ///
    /// Issue #197 asked for the "light-level pie chart"; **26.2 does not have
    /// one.** `DebugScreenEntries` registers a `minecraft:light` *text* entry
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
}

/// All-caps display name for a [`lodestone_model::Difficulty`], matching the
/// debug overlay's own `LODESTONE`/`XYZ`/`FACING`-style convention rather than
/// vanilla's `options.difficulty.*` translation strings (this overlay has no
/// translation table to draw from — see the module doc's "jar-less" path).
fn difficulty_name(d: lodestone_model::Difficulty) -> &'static str {
    match d {
        lodestone_model::Difficulty::Peaceful => "PEACEFUL",
        lodestone_model::Difficulty::Easy => "EASY",
        lodestone_model::Difficulty::Normal => "NORMAL",
        lodestone_model::Difficulty::Hard => "HARD",
    }
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
    /// Vanilla's own split is mechanical in 26.2 (`DebugScreenOverlay`
    /// balances `regularLines` at `mid = (n + 1) / 2` and keeps named groups
    /// contiguous), but issue #197 asks for the *semantic* split the layout
    /// reads as — player/world on the left, engine internals on the right — so
    /// the assignment is by hand here. That is deliberate: a mechanical halve
    /// would reshuffle both columns every time a line is added or removed.
    #[must_use]
    pub fn left_lines(&self) -> Vec<String> {
        vec![
            "LODESTONE".to_string(),
            format!(
                "XYZ {:.2} {:.2} {:.2}",
                self.position[0], self.position[1], self.position[2]
            ),
            format!(
                "CHUNK {} {} {}",
                (self.position[0] as i64).div_euclid(16),
                (self.position[1] as i64),
                (self.position[2] as i64).div_euclid(16)
            ),
            format!(
                "FACING {} ({:.1}/{:.1})",
                self.facing(),
                self.yaw,
                self.pitch
            ),
            match self.target {
                Some([x, y, z]) => format!("TARGET {x} {y} {z}"),
                None => "TARGET -".to_string(),
            },
            // `DebugEntryLight`'s line, with vanilla's own raw/sky/block shape.
            // `getRawBrightness` is the max of the two, which is what the
            // renderer actually samples.
            match self.light {
                Some((sky, block)) => {
                    format!("LIGHT {} ({sky} SKY, {block} BLOCK)", sky.max(block))
                }
                None => "LIGHT -".to_string(),
            },
            match self.difficulty {
                Some((d, locked)) => format!(
                    "DIFFICULTY {}{}",
                    difficulty_name(d),
                    if locked { " (LOCKED)" } else { "" }
                ),
                None => "DIFFICULTY -".to_string(),
            },
            self.status.to_uppercase(),
        ]
    }

    /// The **right** column: frame timing and render-engine internals — the
    /// half vanilla fills from `getSystemInformation`'s descendants.
    #[must_use]
    pub fn right_lines(&self) -> Vec<String> {
        let mut out = vec![
            format!("FPS {:.0} ({:.2} MS)", self.fps, self.frame_ms),
            format!("F/T {:.2}", self.frames_per_tick),
            format!(
                "CHUNKS {} SECTIONS {} QUADS {}",
                self.chunk_count, self.section_count, self.quads
            ),
            format!(
                "LIVE COLS {} DROPS {} ENTITIES {}",
                self.live_columns, self.mesh_drops, self.entities_drawn
            ),
            format!(
                "PARTICLES {}/{} UNRESOLVED {}",
                self.particles_drawn, self.particles_alive, self.particles_unresolved
            ),
            // The occlusion-cull split. `ACTIVE`/`OFF` is the load-bearing
            // token — see `DebugStats::occlusion_active`: the other four
            // numbers cannot distinguish an open surface from a dead graph, and
            // every failure mode here draws *more* rather than less.
            format!(
                "OCCL {} NODES {} CULL {} SHADOW {} WALKS {}",
                self.occlusion_graph_sections,
                self.sections_culled_occlusion,
                self.sections_occlusion_shadow,
                if self.occlusion_active { "ACTIVE" } else { "OFF" },
                self.occlusion_walks
            ),
            format!(
                "MESH VRAM {} KB WORLD {} KB RSS {} MB",
                self.vram_bytes / 1024,
                self.world_bytes / 1024,
                self.rss_bytes / (1024 * 1024)
            ),
        ];
        if !self.adapter.is_empty() {
            // A blank spacer, then the fixed adapter block — the same
            // separated-group shape `DebugScreenOverlay` uses for its own named
            // groups. Empty lines are skipped by the draw, which is what makes
            // the spacer a gap rather than an empty plate.
            out.push(String::new());
            out.extend(self.adapter.iter().cloned());
        }
        out
    }

    /// One-line stdout summary (primary evidence in headless / logged runs).
    #[must_use]
    pub fn one_line(&self) -> String {
        format!(
            "pos=({:.1},{:.1},{:.1}) facing={} f/t={:.2} target={} fps={:.0} frame={:.2}ms chunks={} live_cols={} drops={} entities={} particles={}/{}+{}unres sections={} quads={} vram={}KB world={}KB rss={}MB {}",
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
#[must_use]
pub fn process_rss_bytes() -> usize {
    memory_stats::memory_stats().map_or(0, |m| m.physical_mem)
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
/// `RecipeToast.BACKGROUND_SPRITE` (`RecipeToast.java:16`) — `toast/recipe`,
/// which really is present in 26.2's GUI atlas
/// (`assets/minecraft/textures/gui/sprites/toast/recipe.png`), so the sprite
/// path is reachable rather than permanently falling back.
pub const RECIPE_TOAST_SPRITE: &str = "toast/recipe";
/// `ToastManager`'s slide duration in milliseconds — the bare `600L` at
/// `ToastManager.java:243,252,257` (it has no named constant there).
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
/// - `Toast.width() == 160`, `Toast.height() == 32` (`Toast.java:39-45`; the
///   `DEFAULT_WIDTH`/`SLOT_HEIGHT` constants at `:14-15` carry the same values).
/// - `xPos(screenWidth, visiblePortion) == screenWidth - width() *
///   visiblePortion` (`Toast.java:31-33`). This is **not** a fixed right
///   margin: it is the slide-in, and at `visiblePortion == 1.0` the toast's
///   left edge sits exactly `160` from the right edge of the screen.
/// - `yPos(firstSlotIndex) == firstSlotIndex * height()` (`Toast.java:35-37`),
///   so the *first* toast is flush with the top of the screen at `y == 0`, not
///   inset by a margin. We only ever draw one, so `firstSlotIndex == 0`.
/// - Contents (`RecipeToast.extractRenderState`, `RecipeToast.java:55-65`), all
///   toast-local: background sprite over the full `160×32`; title at `(30, 7)`
///   colour `-11534256` (`0xFF500050`); description at `(30, 18)` colour
///   `-16777216` (opaque black); the crafting-station icon at `(3, 3)` under a
///   `scale(0.6)` that applies to the *position too*, so it lands at
///   `(1.8, 1.8)` at `9.6px`; the unlocked item's icon at `(8, 8)`, unscaled.
#[derive(Debug, Clone)]
pub struct RecipeToastView {
    /// The crafting station's icon — the small scaled corner item
    /// (`RecipeToast.Entry::categoryItem`, `RecipeToast.java:85`).
    pub station: HotbarSlot,
    /// The newly unlocked recipe's result icon (`Entry::unlockedItem`).
    pub unlocked: HotbarSlot,
    /// `ToastManager.ToastInstance::visiblePortion` (`ToastManager.java:199`,
    /// used at `:266`): `1.0` fully on screen, `0.0` entirely off the right
    /// edge. Callers with no animation state should pass `1.0`.
    pub visible_portion: f32,
}

/// `toast/advancement`, the completion toast's background sprite
/// (`AdvancementToast.java:21`).
pub const ADVANCEMENT_TOAST_SPRITE: &str = "toast/advancement";

/// One advancement-completion toast (issue #167).
///
/// `AdvancementToast.extractRenderState` (`AdvancementToast.java:57-86`), the
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
    /// `Hud.extractRenderState` (`Hud.java:218-243`, `.cache/mc/26.2/client-src`)
    /// calls `extractCrosshair` whenever the HUD itself is not F1-hidden and the
    /// active screen is not a `LevelLoadingScreen` — there is no
    /// `screen() == null` guard on this call, unlike the sibling
    /// `extractSubtitleOverlay` three lines below it, which does gate on
    /// `screen() == null || screen().isInGameUi()`. And `extractCrosshair` itself
    /// (`Hud.java:439-470`) gates only on `options.getCameraType().isFirstPerson()`
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
    /// `.cache/mc/26.2/client-src/net/minecraft/client/gui/components/TextCursorUtils.java:9,20-22`,
    /// `isCursorVisible(millis) == (millis / 300) % 2 == 0`) — the caller
    /// computes this from a wall clock with that same formula so this pure
    /// geometry module owns no clock of its own. Defaults to always-visible
    /// (see [`HudFrame::new`]) so every pre-existing test keeps drawing a
    /// caret without having to know about blinking.
    pub chat_caret_visible: bool,
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
    /// Formatted player-list rows, `Some` only while the tab overlay is held.
    pub players: Option<&'a [String]>,
    /// The server's tab-list header, one entry per line, drawn centred **above**
    /// the player rows. Empty when the server sent none — see
    /// [`crate::tablist::banner_lines`] for why this is a possibly-empty slice
    /// rather than an `Option`, and [`crate::sim::Sim::tab_banner`] for what it
    /// closes.
    ///
    /// Read only when [`Self::players`] is `Some`: there is no panel to hang a
    /// header on otherwise.
    pub tab_header: &'a [String],
    /// The server's tab-list footer, drawn centred **below** the player rows.
    /// Same shape and same gating as [`Self::tab_header`].
    pub tab_footer: &'a [String],
    /// The scoreboard sidebar to draw on the right edge, `Some` when displayed.
    pub sidebar: Option<&'a Sidebar>,
    /// Active boss bars, drawn stacked at the top-centre in render order.
    pub boss_bars: &'a [BossBarView],
    /// Current player health in `0..=20`, `Some` only on a live survival server.
    pub health: Option<f32>,
    /// Current food level in `0..=20`, `Some` only on a live survival server.
    pub food: Option<i32>,
    /// Current food saturation (the hidden reserve that drains before `food`
    /// itself does), `Some` only on a live survival server. Drives the
    /// hunger-row wobble while it is empty (`Hud.java:977-979`,
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
    /// (`GameRenderer.java:377,389` → `Gui.java:152-156`) and gates it on game
    /// mode only (`Hud.java:534-562`); the *screen* then paints its translucent
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
    pub title: Option<(String, Option<String>, f32)>,
    /// The action-bar message `(text, alpha)`, drawn just above the hotbar
    /// cluster with a fade. `None` when nothing is showing.
    pub action_bar: Option<(String, f32)>,
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
    /// **Known gap**: vanilla shifts this label down 14px when
    /// `!canHurtPlayer()` (creative/spectator, no health/hunger row to clear)
    /// — see the draw site in [`HudGeometry::build_inner`]. No game-mode
    /// signal reaches [`HudFrame`] yet, so only the survival position draws;
    /// creative/spectator gets the survival Y for now.
    pub held_item: Option<(String, f32)>,
    /// `(recipes, tags)` loaded into the local recipe corpus (see
    /// `crate::resources::load_recipe_book`), appended to the debug overlay as
    /// one extra line when `Some`. `None` before the corpus has loaded or on a
    /// jar-less run — the line is omitted rather than showing a misleading
    /// `0 0`, the same convention [`Self::hotbar_items`] uses for "not yet
    /// known" versus "known empty".
    pub recipe_stats: Option<(usize, usize)>,
    /// `(distance_to_border, warning_distance, warning_strength)` for the
    /// folded world border (issue #436), appended to the debug overlay as one
    /// extra line when `Some`.
    ///
    /// `None` until the server has actually sent a border packet
    /// (`WorldBorder::initialized`), so an unbounded default border omits the
    /// line rather than drawing a meaningless `2.999e7` — the same
    /// "omit rather than mislead" convention [`Self::recipe_stats`] uses.
    ///
    /// **This is a diagnostic, not the real consumer.** Vanilla's border
    /// warning is a blue tint applied to the vignette in
    /// `Hud.extractVignette` (`Hud.java:1057-1078`), which needs a
    /// multiply-blend `RenderPipelines.VIGNETTE` equivalent and
    /// `misc/vignette.png` — neither of which exists in `lodestone-render`
    /// yet. This line is the same "did the datum actually reach the running
    /// client" signal `recipe_stats` plays for the corpus loader, and the
    /// strength it prints is the *exact* value that overlay will consume.
    pub border_debug: Option<(f64, f64, f32)>,
    /// The player's spawn point (issue #436), appended to the debug overlay as
    /// one extra line when the server has reported one.
    ///
    /// `None` when `SpawnPoint::is_reported()` is false, which is the honest
    /// distinction the compass needs too — see
    /// [`Sim::spawn_point`](crate::sim::Sim::spawn_point).
    pub spawn_debug: Option<lodestone_model::BlockPos>,
    /// `(map count, the lowest-numbered map's explored fraction)` from
    /// `SessionMaps` (issue #184), for the F3 overlay.
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
            chat_options: ChatDisplayOptions::default(),
            chat_wrap: None,
            players: None,
            tab_header: &[],
            tab_footer: &[],
            sidebar: None,
            boss_bars: &[],
            health: None,
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

/// Vanilla's Chat Settings (plus one Accessibility-screen field it shares)
/// values that shape how the scrollback and input line draw —
/// `net.minecraft.client.Options`'s `chat*` fields
/// (`.cache/mc/26.2/client-src/net/minecraft/client/Options.java:271-404,508`).
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
    /// `options.chat.scale` (`Options.java:363-370`), `0.0..=1.0`. A
    /// pose-scale multiplier layered on top of this HUD's own fixed 2×
    /// legibility factor — see the draw site in [`HudGeometry::build_inner`]
    /// for how the two combine.
    pub scale: f32,
    /// `options.chat.width` (`Options.java:371-378`), `0.0..=1.0`. Fed
    /// through [`chat_width_px`] (vanilla's `ChatComponent.getWidth`,
    /// `ChatComponent.java:416-420`) to size the chat box.
    pub width_pct: f32,
    /// `options.chat.height.unfocused` (`Options.java:379-386`), `0.0..=1.0`
    /// — box height while the chat box is **closed**.
    pub height_pct_unfocused: f32,
    /// `options.chat.height.focused` (`Options.java:387-394`), `0.0..=1.0` —
    /// box height while the chat box is **open**.
    pub height_pct_focused: f32,
    /// `options.chat.line_spacing` (`Options.java:292-294`), `0.0..=1.0`:
    /// extra fraction of a line's height inserted between chat rows
    /// (`ChatComponent.java:154`, `entryHeight = messageHeight * (spacing +
    /// 1.0)`).
    pub line_spacing: f32,
    /// `options.chat.opacity` (`Options.java:284-291`), `0.0..=1.0`. Text
    /// alpha is `text_opacity * 0.9 + 0.1` (`ChatComponent.java:149`) — never
    /// fully transparent, matching vanilla.
    pub text_opacity: f32,
    /// `options.accessibility.text_background_opacity` (`Options.java:305-312`),
    /// `0.0..=1.0`. Used directly as the per-line background fill alpha
    /// (`ChatComponent.java:150,167`).
    pub background_opacity: f32,
    /// `options.chat.color` (`Options.java:508`). `false` strips every
    /// legacy `§` code before drawing a scrollback line
    /// (`ComponentRenderUtils.stripColor`, `ComponentRenderUtils.java:21`) —
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

/// Vanilla's `ChatComponent.getWidth` (`ChatComponent.java:416-420`): maps the
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
/// (`ChatComponent.java:422-426`): maps `0.0..=1.0` onto `20.0..=180.0` screen
/// pixels.
#[must_use]
pub fn chat_height_px(pct: f32) -> f32 {
    (pct * 160.0 + 20.0).floor()
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

        let scale = HUD_TEXT_SCALE;
        let margin = HUD_MARGIN;
        let glyph_h = font::GLYPH_H as f32;
        let line_h = hud_line_h();

        // The F3 overlay, in vanilla's **two columns** (issue #197): player and
        // world on the left, engine internals on the right, each line sitting on
        // its own translucent fill.
        //
        // The right column is right-aligned at `w - margin - text_width(line)`,
        // which is vanilla's `guiWidth() - 2 - font.width(line)`
        // (`DebugScreenOverlay.extractLines`), so a long line grows leftwards
        // instead of off the screen. The width has to come from `b.text_width`,
        // the same measure the draw itself uses — a restated constant would
        // misalign the moment the vanilla font is or is not loaded.
        //
        // **Vanilla's own metrics, not the HUD's.** The overlay used to draw at
        // `HUD_TEXT_SCALE` (2.0) with `hud_line_h()` (18 px), which is exactly
        // the mistake the XP level number's own comment records one screen over:
        // this function already draws in the `gui_scale`-divided logical canvas,
        // so a ×2 on the text made it twice vanilla's size relative to
        // everything around it. `DebugScreenOverlay` draws at scale 1 with
        // `MARGIN_LEFT == MARGIN_RIGHT == MARGIN_TOP == 2` and a line height of
        // `9` (`DebugScreenOverlay.java:50-52`, `:278`).
        let debug_scale = 1.0;
        let debug_margin = DEBUG_MARGIN;
        let debug_line_h = DEBUG_LINE_H;
        if frame.show_debug {
            let mut left = frame.stats.left_lines();
            let mut right = frame.stats.right_lines();
            // The three conditional diagnostics are engine-side, so they join
            // the right column.
            if let Some((recipes, tags)) = frame.recipe_stats {
                right.push(format!("recipes={recipes} tags={tags}"));
            }
            if let Some((dist, warn_at, strength)) = frame.border_debug {
                right.push(format!(
                    "border dist={dist:.1} warn_at={warn_at:.1} warning={strength:.2}"
                ));
            }
            if let Some(spawn) = frame.spawn_debug {
                left.push(format!("spawn {} {} {}", spawn.x, spawn.y, spawn.z));
            }
            if let Some((count, explored)) = frame.map_debug {
                right.push(format!("maps={count} explored={:.0}%", explored * 100.0));
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
        // field each knob reproduces. `chat_pose_scale` folds this HUD's own
        // fixed 2× legibility factor (`scale`, defined above — shared with the
        // debug overlay) together with the `chatScale` option exactly the way
        // vanilla layers its own pose scale on top of the font's native size
        // (`pose.scale(scale, scale)`, `ChatComponent.java:161`): at the
        // default `opts.scale == 1.0` this is byte-identical to the pre-options
        // behaviour, so an untouched install looks exactly as it did before
        // these fields existed.
        let chat_open = frame.chat_input.is_some();
        let opts = frame.chat_options;
        let chat_pose_scale = scale * opts.scale.max(0.0);
        // Vanilla's unscaled per-line stride is 9px
        // (`ChatComponent.MESSAGE_BOTTOM_TO_MESSAGE_TOP`/`messageHeight`,
        // `ChatComponent.java:151,154`); `glyph_h + 2.0` is this HUD's own 5×7
        // analogue. `entryHeight = messageHeight * (lineSpacing + 1.0)`
        // (`ChatComponent.java:154`) is computed *before* the pose scale is
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
        // `textOpacity = chatOpacity * 0.9 + 0.1` (`ChatComponent.java:149`) —
        // never fully transparent even at `chatOpacity == 0.0`.
        let chat_text_opacity = opts.text_opacity.clamp(0.0, 1.0).mul_add(0.9, 0.1);
        let chat_bg_opacity = opts.background_opacity.clamp(0.0, 1.0);
        let input_y = b.h - margin - glyph_h * chat_pose_scale;
        if let Some(input) = frame.chat_input {
            // A translucent strip so text stays legible over bright terrain.
            // Vanilla's real `EditBox` has no equivalent knob of its own; this
            // reuses `chat_bg_opacity` rather than inventing an unread
            // constant, since it is the same "background behind chat text"
            // concept as the scrollback rows just below.
            // Derived from the *same* `input_y` and `chat_pose_scale` the text
            // draw below uses, so the strip and the glyphs cannot disagree.
            // Vanilla's band is `fill(2, height - 14, width - 2, height - 2, …)`
            // (`ChatScreen.java:272`) with the `EditBox`'s text at `height - 12`
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
            // `TextCursorUtils.java:15-17`, drawn because the shell's
            // `ChatInput` only ever edits at the end of the line, vanilla's
            // "cursor at end" case); `chat_caret_visible` blinks it at
            // vanilla's real 300ms rate (see [`HudFrame::chat_caret_visible`]).
            // The typed line itself is always plain (input filters `§`), so a
            // flat, non-legacy draw is right, and at **full** opacity — vanilla
            // never multiplies the input `EditBox`'s own text by `chatOpacity`,
            // which only governs the scrollback below.
            let caret = if frame.chat_caret_visible { "_" } else { "" };
            b.text(
                &format!("{input}{caret}"),
                margin,
                input_y,
                chat_pose_scale,
                [1.0, 1.0, 1.0, 1.0],
            );
        }
        // The scrollback stacks upward from here, so while the input is open this
        // must be the **top of the input strip**, not the text's own top. Using
        // `input_y` let the strip's padding overlap the last scrollback row, and
        // two translucent blacks over one another read as a brighter seam.
        let chat_bottom = if chat_open {
            input_y - INPUT_STRIP_PAD * chat_pose_scale
        } else {
            b.h - margin
        };
        // How many visual rows fit the configured box height — vanilla's
        // `ChatComponent.getLinesPerPage` (`ChatComponent.java:434-436`,
        // `height / lineHeight`), derived from the same `chat_box_h`/
        // `chat_line_h` the draw below actually uses, not a restated
        // constant.
        let max_visual_rows = (chat_box_h / chat_line_h).floor().max(1.0) as usize;
        let mut row_i = 0usize;
        // Each logical entry can wrap into several visual rows, all sharing
        // that entry's age/alpha. Vanilla stacks a wrapped message's *last*
        // split line nearest the bottom edge and its earlier lines above it
        // (`ChatComponent.addMessageToDisplayQueue`'s per-line `addFirst`,
        // `ChatComponent.java:288-297`, combined with `forEachLine`'s
        // `lineIndex → chatBottom - lineIndex * entryHeight`,
        // `ChatComponent.java:164-168`) — reversing each entry's own wrapped
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

        // Crosshair: a white plus at the centre.
        if frame.crosshair {
            let (cx, cy) = (b.w * 0.5, b.h * 0.5);
            let arm = 8.0;
            let thick = 2.0;
            let col = [1.0, 1.0, 1.0, 0.85];
            b.rect_px(cx - arm, cy - thick * 0.5, arm * 2.0, thick, col);
            b.rect_px(cx - thick * 0.5, cy - arm, thick, arm * 2.0, col);

            // Attack-strength (cooldown) indicator: a small fill bar just below
            // the crosshair — vanilla's `Hud.extractCrosshair`'s
            // `CROSSHAIR_ATTACK_INDICATOR_{BACKGROUND,PROGRESS}_SPRITE` branch
            // (`Hud.java:447-465`, `.cache/mc/26.2/client-src`), gated there on
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
            // (`Hud.java:450-465`). That icon needs the crosshair's target
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
                let hy = b.h - margin - cell;
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
                b.h - margin
            };

            // XP bar: a full-hotbar-width green progress bar just above the hotbar,
            // with the level number centred above it (vanilla green). Drawn only
            // once the server has sent experience (`frame.xp`); off a live server
            // this is `None` and nothing draws, keeping the gauge honest.
            let vitals_base = if let Some((level, progress)) = frame.xp {
                let bar_w = 9.0 * 22.0;
                let bx = cx - bar_w * 0.5;
                let bar_h = 4.0;
                let by = hotbar_top - bar_h - 5.0;
                b.rect_px(bx, by, bar_w, bar_h, [0.0, 0.0, 0.0, 0.7]);
                let fill = bar_w * progress.clamp(0.0, 1.0);
                if fill > 0.0 {
                    b.rect_px(bx, by, fill, bar_h, [0.47, 0.82, 0.16, 1.0]);
                }
                let level_gap = if level > 0 {
                    let s = level.to_string();
                    let tw = b.text_width(&s, scale);
                    b.text(
                        &s,
                        cx - tw * 0.5,
                        by - line_h,
                        scale,
                        [0.44, 0.92, 0.20, 1.0],
                    );
                    line_h
                } else {
                    0.0
                };
                by - level_gap
            } else {
                hotbar_top
            };

            // Health / food pip rows, sitting just above the hotbar (or the XP bar
            // when one is drawn). Each row is 10 pips of 2 units; a pip lights the
            // moment any of its two units is present (a deliberate simplification —
            // no half-pip art yet).
            let bars_y = vitals_base - pip - 4.0;
            if let Some(hp) = frame.health {
                b.pips(
                    hp,
                    cx - row_w - 8.0,
                    bars_y,
                    pip,
                    gap,
                    [0.86, 0.15, 0.16, 1.0],
                );
            }
            if let Some(food) = frame.food {
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

        // Action bar: a single centred line just above the vitals/XP cluster,
        // fading with the server-driven alpha. Legacy `§` colour codes render.
        // Unscaled: `extractOverlayMessage` (`Hud.java:327-355`) makes **no**
        // `pose().scale()` call at all, like the held-item name below. This used
        // `scale`, which is 2.0 — and since `logical_canvas` has already divided
        // by the GUI scale, that was a flat 2x on top of vanilla's own factor.
        // See `docs/hud-text-scale.md`.
        if let Some((msg, alpha)) = frame.action_bar.as_ref().filter(|(_, a)| *a > 0.0) {
            let tw = b.legacy_width(msg, 1.0);
            b.text_legacy(
                msg,
                cx - tw * 0.5,
                bars_y - line_h - 6.0,
                1.0,
                [1.0, 1.0, 1.0],
                *alpha,
            );
        }

        // Held-item name (issue #126): the selected hotbar item's styled name,
        // above the hotbar, fading with a server-independent client timer.
        // Unlike the action bar and title, vanilla draws this **unscaled**
        // (`Hud.java:632-645`, a plain `graphics.textWithBackdrop` call, no
        // ×2) — the same "vanilla's own draw never scales the font" lesson
        // the XP level number's fix (issue #256) already established two
        // blocks up in [`sprite_vitals`]. Using `scale` here would repeat
        // that exact defect on a second piece of HUD text.
        if let Some((name, alpha)) = frame.held_item.as_ref().filter(|(_, a)| *a > 0.0) {
            let tw = b.legacy_width(name, 1.0);
            let x = (b.w - tw) * 0.5;
            // `Hud.java:634,636`: `y = guiHeight - 59`, `+14` when
            // `!canHurtPlayer()` (creative/spectator hide the health/hunger
            // row). No game-mode signal reaches this frame yet — see
            // [`HudFrame::held_item`]'s doc for the gap — so only the
            // survival position is modelled.
            let y = b.h - 59.0;
            b.text_legacy(name, x, y, 1.0, [1.0, 1.0, 1.0], *alpha);
        }

        // Title / subtitle: a large centred overlay mid-screen, fading with the
        // server-driven alpha. Drawn only while a server-sent title is active,
        // so it costs nothing off a server that sends none.
        // `extractTitle` (`Hud.java:374-390`) translates once to the screen centre
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
            let tw = b.text_width(title, TITLE_POSE);
            b.text(
                title,
                (b.w - tw) * 0.5,
                cy - 10.0 * TITLE_POSE,
                TITLE_POSE,
                [1.0, 1.0, 1.0, *alpha],
            );
            if let Some(sub) = subtitle {
                let sw = b.text_width(sub, SUBTITLE_POSE);
                b.text(
                    sub,
                    (b.w - sw) * 0.5,
                    cy + 5.0 * SUBTITLE_POSE,
                    SUBTITLE_POSE,
                    [1.0, 1.0, 1.0, *alpha],
                );
            }
        }

        // Boss bars: stacked title-over-bar at the top-centre. The fill is
        // tinted by the bar's colour and clamped progress; an empty slice draws
        // nothing, so this costs zero verts off a server that sends none.
        if !frame.boss_bars.is_empty() {
            let bar_w = b.w * 0.4;
            let bar_h = 6.0;
            let bx = (b.w - bar_w) * 0.5;
            for (i, bb) in frame.boss_bars.iter().enumerate() {
                let top = margin + i as f32 * (line_h + bar_h + 6.0);
                let tw = b.text_width(&bb.title, scale);
                b.text(
                    &bb.title,
                    (b.w - tw) * 0.5,
                    top,
                    scale,
                    [1.0, 1.0, 1.0, 1.0],
                );
                let bar_y = top + line_h;
                b.rect_px(bx, bar_y, bar_w, bar_h, [0.08, 0.08, 0.10, 0.75]);
                let fill = bar_w * bb.progress.clamp(0.0, 1.0);
                let c = bb.color;
                b.rect_px(bx, bar_y, fill, bar_h, [c[0], c[1], c[2], 0.95]);
            }
        }

        // Scoreboard sidebar: a right-edge, vertically-centred panel. The title
        // is centred; each row puts its label at the left and the score in red,
        // right-aligned — vanilla's layout. Absent when nothing is displayed.
        if let Some(side) = frame.sidebar {
            let pad = 4.0;
            let mut content_w = b.spans_width(&side.title, scale);
            for l in &side.lines {
                content_w = content_w
                    .max(b.spans_width(&l.label, scale) + 12.0 + b.spans_width(&l.score, scale));
            }
            let panel_w = content_w + pad * 2.0;
            let panel_h = (side.lines.len() as f32 + 1.0) * line_h + pad * 2.0;
            let px = b.w - panel_w - margin;
            let py = ((b.h - panel_h) * 0.5).max(margin);
            b.rect_px(px, py, panel_w, panel_h, [0.0, 0.0, 0.0, 0.55]);
            // These three colours were the *only* colours the sidebar could ever
            // show; they are now **base** colours, used for a run the server left
            // uncoloured and overridden per span wherever it did colour one.
            let title_x = px + (panel_w - b.spans_width(&side.title, scale)) * 0.5;
            b.text_spans(&side.title, title_x, py + pad, scale, [1.0, 1.0, 1.0], 1.0);
            for (i, l) in side.lines.iter().enumerate() {
                let y = py + pad + (i as f32 + 1.0) * line_h;
                b.text_spans(&l.label, px + pad, y, scale, [0.85, 0.90, 1.0], 1.0);
                let sx = px + panel_w - pad - b.spans_width(&l.score, scale);
                b.text_spans(&l.score, sx, y, scale, [0.95, 0.35, 0.35], 1.0);
            }
        }

        // Tab player-list overlay: a centred panel of rows while Tab is held,
        // with the server's header above and footer below (issue #436's island
        // sweep). Vanilla centres both about the panel and stacks them outside
        // the rows (`PlayerTabOverlay.render`); the "PLAYERS (n)" caption is
        // this client's own affordance and stays between them.
        if let Some(players) = frame.players {
            let header = frame.tab_header;
            let footer = frame.tab_footer;
            // Measured with the same font the draw uses. `text_width`'s own doc
            // says every centring site must go through it, and this is the
            // input that decides whether the panel has to widen at all.
            let widest_banner = header
                .iter()
                .chain(footer.iter())
                .map(|l| b.text_width(l, scale))
                .fold(0.0f32, f32::max);
            let panel = TabPanel::new(b.w, b.h, header.len(), players.len(), footer.len(), widest_banner);
            b.rect_px(panel.x, panel.y, panel.w, panel.h, [0.0, 0.0, 0.0, 0.7]);

            for (i, line) in header.iter().enumerate() {
                let x = panel.centred_x(b.text_width(line, scale));
                b.text(line, x, panel.header_y(i), scale, [1.0, 1.0, 1.0, 1.0]);
            }
            // The caption and the rows continue straight on from the header, so
            // they index the same ladder rather than a second one.
            b.text(
                &format!("PLAYERS ({})", players.len()),
                panel.left_x(),
                panel.header_y(header.len()),
                scale,
                [1.0, 1.0, 0.6, 1.0],
            );
            for (i, row) in players.iter().enumerate() {
                let y = panel.header_y(header.len() + 1 + i);
                b.text(row, panel.left_x(), y, scale, [0.9, 0.95, 1.0, 1.0]);
            }
            for (i, line) in footer.iter().enumerate() {
                let x = panel.centred_x(b.text_width(line, scale));
                b.text(line, x, panel.footer_y(i), scale, [1.0, 1.0, 1.0, 1.0]);
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
/// (`Toast.java:31-37`).
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
/// (`SubtitleOverlay.java:95-104`).
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
/// (`SubtitleOverlay.java:31-115`). Two details are load-bearing:
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
    // (`RecipeToast.java:56`).
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
    // (`RecipeToast.java:57-58`). Unscaled, and unshadowed (the trailing
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
    // `fakeItem(unlockedItem, 8, 8)` (`RecipeToast.java:64`), unscaled.
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
        // `fakeItem(iconItem, 8, 8)` (`AdvancementToast.java:84`), unscaled.
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
    let margin = 6.0;
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
        let hy = b.h - hh - margin;
        (hx + 3.0, hy + 3.0, 20.0, 16.0)
    } else {
        let cell = 22.0;
        let hw = 9.0 * cell;
        let hx = cx - hw * 0.5;
        let hy = b.h - margin - cell;
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
    /// Vanilla's heart-row `blink` (`Hud.java:766`).
    heart_blink: bool,
    /// Vanilla's `displayHealth` (`Hud.java:777,782`) — the "ghost" heart
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
    let margin = 6.0;

    // Hotbar (182x22 native), centred at the bottom, with the 24x23 selection
    // sprite over the chosen slot.
    let hw = 182.0;
    let hh = 22.0;
    let hx = cx - hw * 0.5;
    let hy = b.h - hh - margin;
    let mut cluster_top = b.h - margin;
    if let Some(sel) = frame.hotbar {
        b.sprite("hud/hotbar", hx, hy, hw, hh, white);
        // Vanilla draws the selection at native offset (slot*20 - 1, -1) from the
        // hotbar origin; the sprite is 24x23 so it overhangs the 20px slot pitch.
        let sel = sel.min(8) as f32;
        let sw = 24.0;
        let sh = 23.0;
        let sx = hx + sel * 20.0 - 1.0;
        let sy = hy - 1.0;
        b.sprite("hud/hotbar_selection", sx, sy, sw, sh, white);
        cluster_top = hy;
    }

    // XP bar (182x5), just above the hotbar: full background, then the progress
    // sprite cropped left-to-right to its filled fraction.
    //
    // The gap above the hotbar is vanilla's own arithmetic, not a guess:
    // `ContextualBar.MARGIN_BOTTOM` (24) is the hotbar's 22px height plus a 2px
    // gap, and `ContextualBar.top` is `guiScaledHeight - MARGIN_BOTTOM - HEIGHT`
    // (`ContextualBar.java:13-14,26-28`) — i.e. the bar sits *2px* above the
    // hotbar sprite, not 4. `hy` is already this cluster's hotbar-top in the
    // same logical-pixel space vanilla's `guiHeight` is in, so subtracting from
    // it (rather than restating an absolute `b.h`-based constant) is what keeps
    // this correct if the cluster's own bottom margin ever changes — the same
    // "derive from the expression the draw uses" rule the XP number below now
    // follows too.
    let bar_w = 182.0;
    let bar_h = 5.0;
    if let Some((level, progress)) = frame.xp {
        let by = hy - bar_h - 2.0;
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
        //   (`ContextualBar.java:34-40`) places the text at
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
        // `0x80FF20` (`ContextualBar.java:34-40`). `Builder::text` would add
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
        cluster_top = by;
    }

    // Hearts (health) left, hunger right, one row above the cluster. Each icon
    // is 9x9 native, stepped 8px (vanilla spacing); a container/empty backing is
    // drawn first, then a full or half overlay per two points.
    let icon = 9.0;
    let step = 8.0;
    let row_y = cluster_top - icon - 4.0;
    if let Some(hp) = frame.health {
        let hp = hp.max(0.0);
        let current = hp.ceil() as i32;
        // The container background flashes to the "_blinking" sprite variant
        // for the same alternating windows the ghost overlay below uses —
        // vanilla draws it for *every* container regardless of that
        // container's own fill state (`Hud.java:871`).
        let container = if anim.heart_blink {
            "hud/heart/container_blinking"
        } else {
            "hud/heart/container"
        };
        // Critical-health y-jitter (`Hud.java:863-865`): `currentHealth +
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
            // vanilla's `blink && halves < oldHealth` (`Hud.java:882-885`).
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
            let units = hp - i as f32 * 2.0;
            if units >= 2.0 {
                b.sprite("hud/heart/full", x, y, icon, icon, white);
            } else if units >= 1.0 {
                b.sprite("hud/heart/half", x, y, icon, icon, white);
            }
        }
    }
    if let Some(food) = frame.food {
        let food_f = food.max(0) as f32;
        // Hunger-empty wobble (`Hud.java:977-979`): `frame.saturation` is
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

    // Air bubbles, one row above hearts/hunger — vanilla's `yLineAir =
    // yLineBase - 10` (`Hud.java:791,806`): the row sits `AIR_BUBBLE_SIZE` (9)
    // plus a 1px gap above the health/hunger line, not on top of it. Anchored
    // at the same right edge (`hx + hw`) the hunger row uses, matching
    // vanilla's shared `xRight`.
    if let Some((air, max_air, eye_in_water)) = frame.air {
        let air_row_y = row_y - icon - 1.0;
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
/// from `ChatComponent.addMessageToDisplayQueue`, `ChatComponent.java:284-285`):
/// break on a space when the next word would overflow, and hard-break a
/// single word that alone exceeds the width so nothing can escape the box. A
/// `§` colour/format code seen before a break is carried onto the
/// continuation line, because a code resets formatting to just itself
/// (`lodestone-model/src/text.rs:626-644`) — tracking only the single most
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
        match self.font {
            Some(f) => f.width(s, scale),
            None => item_icon::text_w(s, scale),
        }
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
    /// `ChatComponent.java:284-285`): break on a space when the next word
    /// would overflow, and hard-break a single word that alone exceeds the
    /// width so nothing can escape the box. A `§` colour/format code seen
    /// before a break is carried onto the continuation line, because a code
    /// resets formatting to just itself
    /// (`lodestone-model/src/text.rs:626-644`) — tracking only the single
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
    /// (`Hud.java:552-554`/`ContextualBar.java`) builds the XP level number's
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
        let hotbar_pop = self
            .hotbar_pop
            .tick(tick, frame.hotbar_items.unwrap_or(&[]));
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
                            if let Some(q) = g.atlas.subregion_quad(s.id, src, s.dst) {
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

    /// Issue #411: `ServerDifficulty` reaches a real, tested ECS fold
    /// (`lodestone-client`'s `apply_routes_difficulty_changed_through_the_real_path`)
    /// but the F3 overlay drew nothing for it. This pins the exact text so a
    /// regression back to "no line at all" or a swapped lock state is visible
    /// in a diff, not just "some line changed somewhere".
    #[test]
    fn debug_overlay_shows_difficulty_and_lock_state() {
        // Found by content, not position: `lines()` is a growing list of
        // independent facts (`DIFFICULTY` sits between the VRAM/RSS line and
        // the status line, not at a fixed index), so pinning an index here
        // would make this test brittle to an unrelated line being added or
        // reordered, which is exactly the kind of accidental coupling
        // `CLAUDE.md` warns a gate should not have.
        fn difficulty_line(stats: &DebugStats) -> String {
            stats
                .lines()
                .into_iter()
                .find(|l| l.starts_with("DIFFICULTY"))
                .expect("the F3 overlay must always carry a DIFFICULTY line")
        }

        let no_report = DebugStats::default();
        assert_eq!(
            difficulty_line(&no_report),
            "DIFFICULTY -",
            "before the server's first report, the line must say so plainly rather \
             than defaulting to a difficulty the server never sent"
        );

        let unlocked = DebugStats {
            difficulty: Some((lodestone_model::Difficulty::Easy, false)),
            ..Default::default()
        };
        assert_eq!(difficulty_line(&unlocked), "DIFFICULTY EASY");

        let locked = DebugStats {
            difficulty: Some((lodestone_model::Difficulty::Hard, true)),
            ..Default::default()
        };
        assert_eq!(difficulty_line(&locked), "DIFFICULTY HARD (LOCKED)");

        // Every variant name, so a mis-mapped match arm (e.g. Peaceful reading
        // as Easy) cannot hide behind only testing one value.
        for (d, name) in [
            (lodestone_model::Difficulty::Peaceful, "PEACEFUL"),
            (lodestone_model::Difficulty::Easy, "EASY"),
            (lodestone_model::Difficulty::Normal, "NORMAL"),
            (lodestone_model::Difficulty::Hard, "HARD"),
        ] {
            let stats = DebugStats {
                difficulty: Some((d, false)),
                ..Default::default()
            };
            assert_eq!(difficulty_line(&stats), format!("DIFFICULTY {name}"));
        }
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

    /// Predicts the exact geometry of a hard-wrapped chat line from first
    /// principles (box width, the fixed fallback font's per-char advance, and
    /// `a`'s own lit-pixel count), rather than merely asserting "it wrapped" —
    /// CLAUDE.md's *magnitude* species of vacuous test is a predicate that
    /// would pass for any wrap width; this one would fail for a wrong one.
    #[test]
    fn a_long_line_with_no_spaces_hard_wraps_at_the_predicted_row_count() {
        let stats = DebugStats::default();
        let line = "a".repeat(30);
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
        // `(GLYPH_W + 1) * scale` per char (`hud/item_icon.rs:616-618`), and
        // the chat pose scale defaults to `scale(2.0) * chat_options.scale
        // (1.0) == 2.0`, so each `a` costs `6 * 2.0 == 12`px. `floor(320 /
        // 12) == 26` fit the first row; the remaining `30 - 26 == 4` spill to
        // a second — two rows, not one, and not three.
        //
        // `a`'s bitmap (`font::glyph_rows('a')`) lights `0+0+3+1+4+2+4 == 14`
        // pixels; each lit pixel is one quad (`ColourStream::glyph`,
        // `hud/item_icon.rs:580-599`) of 6 vertices, so all 30 `a`s cost
        // `30 * 14 * 6 == 2520` vertices regardless of how they are split
        // across rows — the row *count* shows up only in the background
        // strips, one 6-vertex rect each.
        assert_eq!(
            geo.vertex_count(),
            2520 + 2 * 6,
            "expected exactly two wrapped rows' worth of geometry (one row would be \
             2520 + 6, three would be 2520 + 18)"
        );
    }

    /// Direct, GPU-free gate on [`wrap_legacy_with`]'s wrap *decision*, using a
    /// hand-specified width table rather than the fixed 5×7 fallback — the
    /// fallback is itself fixed-advance, so it cannot exercise the
    /// variable-width case the real vanilla font (attached only when a jar is
    /// present) actually draws with. `i`/`W`'s widths below are vanilla's own,
    /// documented at `hud/vanilla_font.rs:9` ("`i` is 2 px wide … `W` and `M`
    /// are 6"); the competing "flat character count" hypothesis uses this
    /// shell's own real fixed-advance constant (`(GLYPH_W + 1) * 1.0 == 6`,
    /// `hud/font.rs:20`,`hud/item_icon.rs:616-618`) rather than an invented
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
    /// here, `text_opacity * 0.9 + 0.1` (`ChatComponent.java:149`) at a fresh
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
    /// 20`px against an `18`px default row) only one row fits, so a five-line
    /// log must render identically to a one-line log, not five.
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
        let one_line_uncapped = HudGeometry::build(
            &HudFrame {
                crosshair: false,
                show_debug: false,
                chat: &chat[4..],
                ..HudFrame::new(&stats)
            },
            640,
            480,
        );
        assert_eq!(
            capped.vertex_count(),
            one_line_uncapped.vertex_count(),
            "height_pct_unfocused == 0.0 must cap the scrollback to exactly one row"
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

    #[test]
    fn tab_overlay_lists_players() {
        let stats = DebugStats::default();
        let names = vec!["Alice  12ms".to_string(), "Bob  30ms".to_string()];
        let frame = HudFrame {
            players: Some(&names),
            ..HudFrame::new(&stats)
        };
        let with = HudGeometry::build(&frame, 640, 480).vertex_count();
        let without = HudGeometry::build(&HudFrame::new(&stats), 640, 480).vertex_count();
        assert!(with > without, "the tab panel + names add geometry");
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

    #[test]
    fn boss_bar_fill_tracks_progress_and_colour() {
        use crate::overlay::BossBarView;
        let stats = DebugStats::default();
        let base = HudFrame {
            crosshair: false,
            show_debug: false,
            ..HudFrame::new(&stats)
        };
        let base_verts = HudGeometry::build(&base, 640, 480).vertex_count();

        let full = [BossBarView {
            title: "Ender Dragon".into(),
            progress: 1.0,
            color: [0.6, 0.2, 0.8],
        }];
        let frame_full = HudFrame {
            boss_bars: &full,
            ..HudFrame {
                crosshair: false,
                show_debug: false,
                ..HudFrame::new(&stats)
            }
        };
        let with_full = HudGeometry::build(&frame_full, 640, 480);
        assert!(
            with_full.vertex_count() > base_verts,
            "a boss bar must add its title and bar geometry"
        );

        // A zero-progress bar still draws the background track (same vert count),
        // but the fill colour disappears — so the verts must differ. This guards
        // that the fill actually tracks progress rather than being cosmetic.
        let empty = [BossBarView {
            title: "Ender Dragon".into(),
            progress: 0.0,
            color: [0.6, 0.2, 0.8],
        }];
        let frame_empty = HudFrame {
            boss_bars: &empty,
            ..HudFrame {
                crosshair: false,
                show_debug: false,
                ..HudFrame::new(&stats)
            }
        };
        let with_empty = HudGeometry::build(&frame_empty, 640, 480);
        assert_ne!(
            with_full.verts, with_empty.verts,
            "full vs empty progress must change the drawn fill"
        );
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
    ///   (`ContextualBar.java:26-28,34-40`) fixes at vanilla's `6` logical px —
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

        // Fill alone: no digit (`level: 0`), full bar (`progress: 1.0`).
        let bar = render_bbox(Some((0, 1.0))).expect("a full XP bar must paint green pixels");
        // Digit alone: no fill (`progress: 0.0`), a single glyph (`level: 5`).
        let digit =
            render_bbox(Some((5, 0.0))).expect("the level digit must paint green pixels");
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
        // (`ContextualBar.java:26-28` bar top, `:34-40` text y). The old bug's
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

        let mut render = |title: Option<(String, Option<String>, f32)>,
                          action_bar: Option<(String, f32)>|
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
        let (shown_title, title_leak_act) =
            render(Some(("TITLE".into(), Some("subtitle".into()), 1.0)), None);
        let (act_leak_title, shown_act) = render(None, Some(("Action bar!".into(), 1.0)));

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
        // native 9×9 size (no more hardcoded ×2 — see its own doc comment); with
        // the cluster anchored at the bottom, the first heart is at
        // `(cx - 91, h - 19)` and spans 9×9 px.
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
        let y0 = h - 19;

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
    /// (`Toast.java:35-37`), and with one toast `firstSlotIndex == 0`, so the
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
