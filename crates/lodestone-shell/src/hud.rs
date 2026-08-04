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

use item_icon::{ColourStream, IconAssets, IconRenderer, IconSink, SpecialIconDraw};

use crate::overlay::{BossBarView, Sidebar};

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

    /// The overlay's text lines.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
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
            format!("FPS {:.0} ({:.2} MS)", self.fps, self.frame_ms),
            format!("F/T {:.2}", self.frames_per_tick),
            match self.target {
                Some([x, y, z]) => format!("TARGET {x} {y} {z}"),
                None => "TARGET -".to_string(),
            },
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
            format!(
                "MESH VRAM {} KB WORLD {} KB RSS {} MB",
                self.vram_bytes / 1024,
                self.world_bytes / 1024,
                self.rss_bytes / (1024 * 1024)
            ),
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
    /// Formatted player-list rows, `Some` only while the tab overlay is held.
    pub players: Option<&'a [String]>,
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
            chat_input: None,
            chat_caret_visible: true,
            chat_options: ChatDisplayOptions::default(),
            players: None,
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
            attack_cooldown: None,
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

        let scale = 2.0;
        let margin = 6.0;
        let glyph_h = font::GLYPH_H as f32;
        let line_h = (glyph_h + 2.0) * scale;

        // Debug text, top-left.
        if frame.show_debug {
            let mut debug_lines = frame.stats.lines();
            if let Some((recipes, tags)) = frame.recipe_stats {
                debug_lines.push(format!("recipes={recipes} tags={tags}"));
            }
            for (i, line) in debug_lines.iter().enumerate() {
                let y = margin + i as f32 * line_h;
                b.text(line, margin, y, scale, [0.96, 0.98, 1.0, 1.0]);
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
            b.rect_px(
                0.0,
                input_y - 3.0,
                chat_box_w,
                chat_line_h,
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
        let chat_bottom = if chat_open { input_y } else { b.h - margin };
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
            let mut sub_rows = b.wrap_legacy(display, chat_box_w, chat_pose_scale);
            sub_rows.reverse();
            for sub in sub_rows {
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
                    &sub,
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
        draw_hotbar_items(&mut b, frame);

        // Action bar: a single centred line just above the vitals/XP cluster,
        // fading with the server-driven alpha. Legacy `§` colour codes render.
        if let Some((msg, alpha)) = frame.action_bar.as_ref().filter(|(_, a)| *a > 0.0) {
            let tw = b.legacy_width(msg, scale);
            b.text_legacy(
                msg,
                cx - tw * 0.5,
                bars_y - line_h - 6.0,
                scale,
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
        if let Some((title, subtitle, alpha)) = frame.title.as_ref().filter(|(_, _, a)| *a > 0.0) {
            let ts = scale * 4.0;
            let tw = b.text_width(title, ts);
            let ty = b.h * 0.40;
            b.text(title, (b.w - tw) * 0.5, ty, ts, [1.0, 1.0, 1.0, *alpha]);
            if let Some(sub) = subtitle {
                let ss = scale * 2.0;
                let sw = b.text_width(sub, ss);
                b.text(
                    sub,
                    (b.w - sw) * 0.5,
                    ty + ts * 9.0,
                    ss,
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
            let mut content_w = b.text_width(&side.title, scale);
            for l in &side.lines {
                content_w =
                    content_w.max(b.text_width(&l.label, scale) + 12.0 + b.text_width(&l.score, scale));
            }
            let panel_w = content_w + pad * 2.0;
            let panel_h = (side.lines.len() as f32 + 1.0) * line_h + pad * 2.0;
            let px = b.w - panel_w - margin;
            let py = ((b.h - panel_h) * 0.5).max(margin);
            b.rect_px(px, py, panel_w, panel_h, [0.0, 0.0, 0.0, 0.55]);
            let title_x = px + (panel_w - b.text_width(&side.title, scale)) * 0.5;
            b.text(&side.title, title_x, py + pad, scale, [1.0, 1.0, 1.0, 1.0]);
            for (i, l) in side.lines.iter().enumerate() {
                let y = py + pad + (i as f32 + 1.0) * line_h;
                b.text(&l.label, px + pad, y, scale, [0.85, 0.90, 1.0, 1.0]);
                let sx = px + panel_w - pad - b.text_width(&l.score, scale);
                b.text(&l.score, sx, y, scale, [0.95, 0.35, 0.35, 1.0]);
            }
        }

        // Tab player-list overlay: a centred panel of rows while Tab is held.
        if let Some(players) = frame.players {
            let rows = players.len().max(1);
            let panel_h = (rows as f32 + 1.0) * line_h + margin * 2.0;
            let panel_w = b.w * 0.5;
            let px = cx - panel_w * 0.5;
            let py = (b.h - panel_h) * 0.5;
            b.rect_px(px, py, panel_w, panel_h, [0.0, 0.0, 0.0, 0.7]);
            b.text(
                &format!("PLAYERS ({})", players.len()),
                px + margin,
                py + margin,
                scale,
                [1.0, 1.0, 0.6, 1.0],
            );
            for (i, row) in players.iter().enumerate() {
                let y = py + margin + (i as f32 + 1.0) * line_h;
                b.text(row, px + margin, y, scale, [0.9, 0.95, 1.0, 1.0]);
            }
        }

        Self {
            verts: b.verts,
            sprite_verts: b.sprite_verts,
            item_verts: b.item_verts,
            model_verts: b.model_verts,
            special: b.special,
        }
    }
}

/// Draw the item icons into the nine hotbar cells. Mirrors the slot geometry of
/// both hotbar-draw paths (real GUI atlas at scale 2, or the procedural 22px
/// cells) so icons land centred in the wells either way. A no-op without an item
/// atlas or `hotbar_items`, so headless / jar-less runs are unaffected.
fn draw_hotbar_items(b: &mut Builder, frame: &HudFrame) {
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
            b.item_icon(item, x, icon_y, size);
        }
    }
}

/// The per-frame vitals-cluster animation phases [`HudGeometry::build_inner`]
/// draws with — heart blink/jitter and the hunger wobble (both driven by
/// `tick` below), and (a later addition to this type) the hotbar pop. See
/// `hud/anim.rs` for the vanilla citations and `docs/hud-animations.md` for
/// the port notes.
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
}

impl HudAnim {
    const NONE: Self = Self {
        heart_blink: false,
        display_health: i32::MIN, // unused while `heart_blink` is false and jitter is skipped
        tick: 0,
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
            for mut q in b.gui_geometry("hud/experience_bar_progress", hx, by, bar_w, bar_h) {
                let span = q.uv_max[0] - q.uv_min[0];
                q.dst[2] *= p;
                q.uv_max[0] = q.uv_min[0] + span * p;
                b.push_sprite_quad(q, white);
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
            // vanilla constant `-8323296` reinterpreted as unsigned ARGB.
            let green = [128.0 / 255.0, 1.0, 32.0 / 255.0, 1.0];
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
fn wrap_legacy_with(measure: impl Fn(&str) -> f32, s: &str, max_width_px: f32) -> Vec<String> {
    if max_width_px <= 0.0 || measure(s) <= max_width_px {
        return vec![s.to_string()];
    }
    let mut rows = Vec::new();
    let mut current = String::new();
    let mut pending_code: Option<String> = None;
    for word in s.split(' ') {
        // The last `§`+selector pair inside this word, if any — what a
        // continuation line started *after* this word must be seeded with to
        // keep reading the same colour.
        let mut word_pending = pending_code.clone();
        let mut chars = word.chars();
        while let Some(ch) = chars.next() {
            if ch == '\u{00a7}' {
                if let Some(code) = chars.next() {
                    word_pending = Some(format!("\u{00a7}{code}"));
                }
            }
        }

        let candidate = if current.is_empty() {
            word.to_string()
        } else {
            format!("{current} {word}")
        };
        if measure(&candidate) <= max_width_px {
            current = candidate;
            pending_code = word_pending;
            continue;
        }
        if !current.is_empty() {
            rows.push(std::mem::take(&mut current));
            if let Some(code) = &pending_code {
                current.push_str(code);
            }
        }
        let seeded = format!("{current}{word}");
        if measure(&seeded) <= max_width_px {
            current = seeded;
        } else {
            // The word alone overflows even a fresh line: hard-break it
            // character by character. `§`/selector characters are
            // zero-width, so they never trigger a break by themselves.
            for ch in word.chars() {
                let attempt = format!("{current}{ch}");
                if !current.is_empty() && measure(&attempt) > max_width_px {
                    rows.push(std::mem::take(&mut current));
                    if let Some(code) = &pending_code {
                        current.push_str(code);
                    }
                }
                current.push(ch);
            }
        }
        pending_code = word_pending;
    }
    rows.push(current);
    rows
}

/// The RGB of one of the sixteen legacy `§` colour codes (`0`..=`9`, `a`..=`f`),
/// or `None` for a format/reset code. These are the standard Minecraft chat
/// foreground colours; the shell paints them locally, which is a rendering
/// concern (how to colour a run), not protocol knowledge.
fn legacy_rgb(code: char) -> Option<[f32; 3]> {
    let hex: u32 = match code.to_ascii_lowercase() {
        '0' => 0x000000,
        '1' => 0x0000aa,
        '2' => 0x00aa00,
        '3' => 0x00aaaa,
        '4' => 0xaa0000,
        '5' => 0xaa00aa,
        '6' => 0xffaa00,
        '7' => 0xaaaaaa,
        '8' => 0x555555,
        '9' => 0x5555ff,
        'a' => 0x55ff55,
        'b' => 0x55ffff,
        'c' => 0xff5555,
        'd' => 0xff55ff,
        'e' => 0xffff55,
        'f' => 0xffffff,
        _ => return None,
    };
    Some([
        ((hex >> 16) & 0xff) as f32 / 255.0,
        ((hex >> 8) & 0xff) as f32 / 255.0,
        (hex & 0xff) as f32 / 255.0,
    ])
}

struct Builder<'a> {
    w: f32,
    h: f32,
    verts: Vec<f32>,
    sprite_verts: Vec<f32>,
    item_verts: Vec<f32>,
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
        };
        item_icon::draw_item_icon(&mut sink, &assets, (w, h), slot, x, y, size, self.font);
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
        let anim = HudAnim {
            heart_blink,
            display_health,
            tick,
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
}

const HUD_WGSL: &str = include_str!("shaders/hud.wgsl");

const HUD_SPRITE_WGSL: &str = include_str!("shaders/hud_sprite.wgsl");

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
            title: "Objectives".into(),
            lines: vec![
                SidebarLine {
                    label: "Kills".into(),
                    score: "7".into(),
                },
                SidebarLine {
                    label: "Deaths".into(),
                    score: "2".into(),
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
            title: "Objectives".into(),
            lines: vec![
                SidebarLine {
                    label: "Kills".into(),
                    score: String::new(),
                },
                SidebarLine {
                    label: "Deaths".into(),
                    score: String::new(),
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
