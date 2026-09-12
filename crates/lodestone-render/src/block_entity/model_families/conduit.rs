use crate::entity::ENTITY_FULLBRIGHT;

/// Model name of the conduit's inactive shell — vanilla's own conduit
/// shell model layer, the 6×6×6 layer.
pub const CONDUIT_SHELL: &str = "conduit_shell";

/// Model name of the conduit's active shell ("cage") — vanilla's own
/// conduit cage model layer, the 8×8×8 layer. A distinct mesh from
/// [`CONDUIT_SHELL`], not a scaled reuse — see
/// [`lodestone_assets::block_entity_models::conduit_cage_model`]'s doc.
pub const CONDUIT_CAGE: &str = "conduit_cage";

/// Model name of the conduit's wind plane — vanilla's own conduit wind
/// model layer, one 16×16×16 cube drawn twice per active frame at two
/// different poses sharing this one mesh.
pub const CONDUIT_WIND: &str = "conduit_wind";

/// Model name of the conduit's billboarded eye — vanilla's own conduit eye
/// model layer, the near-planar 8×8 box.
pub const CONDUIT_EYE: &str = "conduit_eye";

/// Vanilla's own inactive-shell texture — `entity/conduit/base`, the inactive shell's
/// sheet.
pub const CONDUIT_SHELL_TEXTURE_STEM: &str = "entity/conduit/base";

/// Vanilla's own active-cage texture — `entity/conduit/cage`, the active
/// cage's sheet.
pub const CONDUIT_CAGE_TEXTURE_STEM: &str = "entity/conduit/cage";

/// Vanilla's own wind texture — `entity/conduit/wind`, used for both wind
/// planes whenever [`ConduitSpawn::animation_phase`] is not `1`.
pub const CONDUIT_WIND_TEXTURE_STEM: &str = "entity/conduit/wind";

/// Vanilla's own vertical-wind texture — `entity/conduit/wind_vertical`,
/// used for both wind planes when [`ConduitSpawn::animation_phase`] **is**
/// `1`. Same UV layout as [`CONDUIT_WIND_TEXTURE_STEM`], different sheet —
/// picking the texture without also picking the plane's own rotation
/// (`wind1_rot`'s `1 =>` arm) draws a plane that spins but never actually
/// turns to face the direction its "vertical" sheet implies.
pub const CONDUIT_WIND_VERTICAL_TEXTURE_STEM: &str = "entity/conduit/wind_vertical";

/// Vanilla's own open-eye texture — `entity/conduit/open_eye`, drawn while
/// [`ConduitSpawn::hunting`].
pub const CONDUIT_OPEN_EYE_TEXTURE_STEM: &str = "entity/conduit/open_eye";

/// Vanilla's own closed-eye texture — `entity/conduit/closed_eye`, drawn
/// otherwise (including the entire inactive branch, which never submits an
/// eye instance at all).
pub const CONDUIT_CLOSED_EYE_TEXTURE_STEM: &str = "entity/conduit/closed_eye";

/// Every conduit sheet, for [`block_entity_texture_stems`]. **Excludes**
/// [`CONDUIT_WIND_TEXTURE_STEM`]/[`CONDUIT_WIND_VERTICAL_TEXTURE_STEM`]'s
/// remaining 21 animation frames — the jar ships each as a `64×704` vertical
/// strip (22 frames at `64×32`, `frametime: 3`, no `interpolate`) and the
/// shell's block-entity loader crops each to its first frame only. See
/// `crates/lodestone-shell/src/gpu/block_entities.rs`'s conduit loading note
/// for why: this pass has no per-material animation uniform the way the block
/// atlas does, and building one is out of scope here. The wind planes are
/// therefore correct in shape, rotation and texture *choice*, and static
/// rather than flowing — a documented simplification, not a silent bug.
#[must_use]
pub fn conduit_texture_stems() -> Vec<&'static str> {
    vec![
        CONDUIT_SHELL_TEXTURE_STEM,
        CONDUIT_CAGE_TEXTURE_STEM,
        CONDUIT_WIND_TEXTURE_STEM,
        CONDUIT_WIND_VERTICAL_TEXTURE_STEM,
        CONDUIT_OPEN_EYE_TEXTURE_STEM,
        CONDUIT_CLOSED_EYE_TEXTURE_STEM,
    ]
}

/// One conduit's fully-resolved per-frame animation/activation state — the
/// output of [`conduit_frame_scan`] plus a per-instance tick tracker folded
/// through [`conduit_advance`], [`conduit_active_rotation_value`],
/// [`conduit_anim_time`] and [`conduit_animation_phase`] by the caller. This
/// struct carries already-resolved numbers, not a live clock or a block
/// store — the same split [`BellSpawn::shake`] makes from `BellShakes`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConduitSpawn {
    /// Block position.
    pub pos: [i32; 3],
    /// Vanilla's own active check — [`ConduitFrame::is_active`].
    pub active: bool,
    /// Vanilla's own hunting check — [`ConduitFrame::is_hunting`]. Only
    /// meaningful (and only ever read) while `active`; vanilla's own hunting
    /// flag can theoretically be true while its active flag is false for one
    /// frame of hysteresis, but the inactive branch never reads it, so
    /// [`Self::resolve_conduit`]/[`BlockEntityModelSet::resolve_conduit`] does
    /// not either.
    pub hunting: bool,
    /// [`conduit_active_rotation_value`]'s output — see that function's doc
    /// for why the same number is read as degrees in one branch and radians
    /// in the other.
    pub active_rotation_value: f32,
    /// [`conduit_anim_time`] — the block entity's tick counter plus the
    /// partial tick, feeds [`conduit_bob`].
    pub anim_time: f32,
    /// [`conduit_animation_phase`] — `0`, `1` or `2`.
    pub animation_phase: u8,
    /// Packed sky/block light. Pass [`ENTITY_FULLBRIGHT`] only when there is
    /// genuinely no world to sample.
    pub light: u8,
}

impl Default for ConduitSpawn {
    fn default() -> Self {
        ConduitSpawn {
            pos: [0, 0, 0],
            active: false,
            hunting: false,
            active_rotation_value: 0.0,
            anim_time: 0.0,
            animation_phase: 0,
            light: ENTITY_FULLBRIGHT,
        }
    }
}

impl ConduitSpawn {
    /// An inactive, resting, full-bright conduit at `pos` — the minimum a
    /// hermetic gate needs.
    #[must_use]
    pub fn at(pos: [i32; 3]) -> Self {
        ConduitSpawn {
            pos,
            ..Default::default()
        }
    }
}

/// Total cells [`conduit_frame_scan`]'s 5×5×5 pass ever tests, regardless of
/// which blocks are placed — vanilla's own "hunting" threshold value,
/// which is exactly this count: "hunting" is not a majority threshold, it is
/// *every* candidate cell filled. Checked by
/// `conduit_frame_candidate_count_is_42_and_matches_min_kill_size` as an
/// outside-arithmetic control on the geometry alone, independent of the block
/// predicate.
pub const CONDUIT_FRAME_CANDIDATE_COUNT: u32 = 42;

/// One conduit's activation frame — vanilla's own shape-update effect-block
/// count, plus the two booleans it and the hunting update derive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConduitFrame {
    /// The effect-block count. `0` whenever the inner 3×3×3 was not entirely
    /// water — vanilla's own shape update clears the list and returns before
    /// the 5×5×5 pass ever runs in that case, so a zero here does not
    /// distinguish "no water" from "water but zero frame blocks"; nothing
    /// downstream needs to.
    pub effect_block_count: u32,
}

impl ConduitFrame {
    /// Vanilla's own "active" threshold — the shape update's own return
    /// value.
    #[must_use]
    pub fn is_active(self) -> bool {
        self.effect_block_count >= 16
    }

    /// Vanilla's own "hunting" threshold — the hunting update's own
    /// condition (effect-block count >= 42). Exactly
    /// [`CONDUIT_FRAME_CANDIDATE_COUNT`], so this requires *every* candidate
    /// cell filled, not a majority.
    #[must_use]
    pub fn is_hunting(self) -> bool {
        self.effect_block_count >= 42
    }
}

/// Scans a conduit's activation frame around `pos` —
/// vanilla's own shape-update scan, clause by clause:
///
/// 1. The inner 3×3×3 (`ox,oy,oz` each `-1..=1`, all 27 cells, including the
///    conduit's own position) must be **entirely water** —
///    vanilla's own water-at check, which tests the fluid state's water tag:
///    true for a source, for flowing water, and for a waterlogged block. A
///    single non-water cell anywhere in the inner cube returns an *empty*
///    frame immediately, matching vanilla's early `return false` — so
///    `is_water` is checked over the **whole** inner cube before
///    `is_valid_frame_block` is consulted at all. Skipping this clause (or
///    checking it lazily, cell-by-cell mixed with the outer scan) is exactly
///    the "one implemented conjunct" trap: a room built entirely of
///    prismarine with one stray air pocket in the conduit's own inner cube
///    would activate, and vanilla's would not.
/// 2. Only once (1) holds: scan the 5×5×5 region (`-2..=2` each axis) and, at
///    each of the (up to) [`CONDUIT_FRAME_CANDIDATE_COUNT`] cells on the three
///    axis-aligned "plus" rings — `(ax>1||ay>1||az>1)` *and* one of
///    `ox==0 && (ay==2||az==2)` / `oy==0 && (ax==2||az==2)` /
///    `oz==0 && (ax==2||ay==2)` — count how many hold one of the four frame
///    blocks: `minecraft:prismarine`, `minecraft:prismarine_bricks`,
///    `minecraft:sea_lantern`, `minecraft:dark_prismarine`
///    (vanilla's own valid-frame-block set).
///
/// Both closures receive **absolute** world positions (`pos` already added),
/// so a caller needs no coordinate math of its own — see
/// `crate::block_entities::conduit_spawn` in the shell for the real block-store
/// adapter this is built to be called from.
#[must_use]
pub fn conduit_frame_scan(
    pos: [i32; 3],
    mut is_water: impl FnMut([i32; 3]) -> bool,
    mut is_valid_frame_block: impl FnMut([i32; 3]) -> bool,
) -> ConduitFrame {
    for ox in -1..=1 {
        for oy in -1..=1 {
            for oz in -1..=1 {
                let test = [pos[0] + ox, pos[1] + oy, pos[2] + oz];
                if !is_water(test) {
                    return ConduitFrame::default();
                }
            }
        }
    }

    let mut count = 0u32;
    for ox in -2i32..=2 {
        for oy in -2i32..=2 {
            for oz in -2i32..=2 {
                let (ax, ay, az) = (ox.abs(), oy.abs(), oz.abs());
                let outside_inner = ax > 1 || ay > 1 || az > 1;
                let on_plus_ring = (ox == 0 && (ay == 2 || az == 2))
                    || (oy == 0 && (ax == 2 || az == 2))
                    || (oz == 0 && (ax == 2 || ay == 2));
                if outside_inner
                    && on_plus_ring
                    && is_valid_frame_block([pos[0] + ox, pos[1] + oy, pos[2] + oz])
                {
                    count += 1;
                }
            }
        }
    }
    ConduitFrame {
        effect_block_count: count,
    }
}

/// One client tick's counter bookkeeping for a placed conduit —
/// vanilla's own client-tick update's non-scan half: the tick counter always
/// advances by one, the active-rotation counter only advances while active.
/// The 40-tick-periodic shape rescan ([`conduit_frame_scan`]) is the caller's
/// job; this only advances the two counters the animation math below reads.
/// Returns `(tick_count, active_rotation_ticks)`.
#[must_use]
pub fn conduit_advance(
    tick_count: u32,
    active_rotation_ticks: u32,
    active: bool,
) -> (u32, u32) {
    let tick_count = tick_count.wrapping_add(1);
    let active_rotation_ticks = if active {
        active_rotation_ticks.wrapping_add(1)
    } else {
        active_rotation_ticks
    };
    (tick_count, active_rotation_ticks)
}

/// Vanilla's own active-rotation accessor with its rotation-speed constant
/// (`-0.0375`), folded with the partial tick exactly as vanilla's own
/// render-state extraction does: the partial tick is added only while
/// active, so the inactive spin advances one **whole** step per tick with no
/// sub-tick smoothing.
///
/// # The returned number is not one unit — read both call sites before "fixing" this
///
/// This returns the raw `(counter + partial) * -0.0375` vanilla calls its
/// own active-rotation state, and vanilla's own submit step reads it two
/// **different** ways depending on which branch runs:
///
/// * **Inactive**: rotate about Y by `state * (PI / 180.0)` (`java.lang.Math`'s
///   own PI) — treated as **degrees**, converted once. See
///   [`conduit_inactive_y_rot_radians`].
/// * **Active**: `rotation = state * (180.0F / PI); … rotate about the axis
///   by rotation * (PI / 180.0)` — multiplied by `180/π` and then immediately
///   back by `π/180`, which is the identity; the two conversions cancel and
///   the axis rotation ends up using the raw value **as radians**,
///   unconverted. See [`conduit_active_axis_rotation_radians`].
///
/// So the same field is degrees in one branch and radians in the other in the
/// jar itself. This is transcribed literally rather than "simplified" to one
/// unit, because simplifying it is exactly how a port gets the active spin's
/// speed wrong by a factor of `180/π` (≈57×) while the inactive spin still
/// looks right — the inactive branch's own conversion masks the bug.
#[must_use]
pub fn conduit_active_rotation_value(
    active_rotation_ticks: u32,
    partial_tick: f32,
    active: bool,
) -> f32 {
    let counter = if active {
        active_rotation_ticks as f32 + partial_tick
    } else {
        active_rotation_ticks as f32
    };
    counter * -0.0375
}

/// Reads [`conduit_active_rotation_value`]'s output as **degrees** and
/// converts — the inactive branch's `rotationY(state.activeRotation * (PI/180))`.
#[must_use]
pub fn conduit_inactive_y_rot_radians(active_rotation_value: f32) -> f32 {
    active_rotation_value.to_radians()
}

/// Reads [`conduit_active_rotation_value`]'s output **directly as radians** —
/// the active branch's `rotation * (PI/180)` after its own `* (180/PI)`,
/// which cancel. See [`conduit_active_rotation_value`]'s doc for the full
/// derivation; this function exists so the cancellation is written down once
/// rather than re-derived (or "simplified away") at every call site.
#[must_use]
pub fn conduit_active_axis_rotation_radians(active_rotation_value: f32) -> f32 {
    active_rotation_value
}

/// The block entity's tick counter plus the partial tick — vanilla's own
/// render-state extraction's anim-time field. Feeds [`conduit_bob`].
#[must_use]
pub fn conduit_anim_time(tick_count: u32, partial_tick: f32) -> f32 {
    tick_count as f32 + partial_tick
}

/// The block entity's tick counter / 66 % 3 — **integer** division, so this steps once
/// every 66 ticks regardless of the fractional partial tick baked into
/// [`conduit_anim_time`] (which this does *not* take; it reads the raw tick
/// counter directly). Selects which of the two wind planes' extra rotation
/// applies in [`BlockEntityModelSet::resolve_conduit`], and which sprite
/// (`wind` vs `wind_vertical`) both planes draw with.
#[must_use]
pub fn conduit_animation_phase(tick_count: u32) -> u8 {
    ((tick_count / 66) % 3) as u8
}

/// The bob-height fold shared by the cage and the eye — vanilla's own
/// submit-step local `hh`, used as `0.3 + hh * 0.2`.
///
/// **Not** vanilla's animation-tick same-named local — that one
/// adds a `+35`-tick phase offset and a trailing `* 0.3F` this one does not
/// have (`hh = (hh*hh+hh) * 0.3`, for a *particle* spawn position, not
/// geometry). The two share a name and a shape, which is exactly how a port
/// conflates them; only the version transcribed here belongs in the renderer.
#[must_use]
pub fn conduit_bob(anim_time: f32) -> f32 {
    let hh = (anim_time * 0.1).sin() / 2.0 + 0.5;
    hh * hh + hh
}
