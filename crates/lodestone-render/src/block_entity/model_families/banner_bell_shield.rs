/// Model name of the bell body/rim rig.
pub const BELL: &str = "bell";

/// The jar sheet a bell draws with — vanilla's own bell-texture constant,
/// resolved from `"bell/bell_body"`.
/// Single stem, no material variants: unlike chest, a bell's sheet never
/// changes with block state or NBT.
pub const BELL_TEXTURE_STEM: &str = "entity/bell/bell_body";

/// The one bell sheet stem, for [`block_entity_texture_stems`] — mirrors
/// [`skull_texture_stems`]'s shape even though there is only one entry, so a
/// future material split (there is none today) has one function to widen
/// rather than a call site to find.
#[must_use]
pub fn bell_texture_stems() -> Vec<&'static str> {
    vec![BELL_TEXTURE_STEM]
}

/// A bell's shake direction — vanilla's own per-instance shake-direction
/// field, the four horizontal directions a player (or projectile) can hit a
/// bell from. `Option<BellShakeDirection>` (not a fifth "none" variant)
/// mirrors the jar's own nullable direction field, and matches how
/// [`BellSpawn::shake`] spells "at rest".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BellShakeDirection {
    /// North.
    North,
    /// South.
    South,
    /// East.
    East,
    /// West.
    West,
}

/// The bell body's `(x_rot, z_rot)` in radians for a shake in progress —
/// vanilla's own model pose:
///
/// ```text
/// baseRot = sin(ticks / PI) / (4 + ticks / 3)
/// NORTH: xRot = -baseRot   SOUTH: xRot = +baseRot
/// EAST:  zRot = -baseRot   WEST:  zRot = +baseRot
/// ```
///
/// `direction = None` returns `(0.0, 0.0)` without evaluating `base_rot` at
/// all, mirroring vanilla's own model pose function's null-direction guard
/// rather than computing the ratio and multiplying by a zero
/// that never appears in the real formula — there is no direction to carry a
/// sign for a bell at rest, so a literal port has nothing to multiply.
///
/// `ticks` is vanilla's raw per-block tick counter (`0..50`, **not** eased or
/// clamped here) plus partial tick, exactly as vanilla's own per-frame
/// render-state extraction passes it — unlike [`chest_lid_openness`], there is
/// only one transform here because vanilla itself has only one; its model
/// pose computes the angle directly from ticks with no separate easing pass.
#[must_use]
pub fn bell_shake_angle(direction: Option<BellShakeDirection>, ticks: f32) -> (f32, f32) {
    let Some(direction) = direction else {
        return (0.0, 0.0);
    };
    let base_rot = (ticks / std::f32::consts::PI).sin() / (4.0 + ticks / 3.0);
    match direction {
        BellShakeDirection::North => (-base_rot, 0.0),
        BellShakeDirection::South => (base_rot, 0.0),
        BellShakeDirection::East => (0.0, -base_rot),
        BellShakeDirection::West => (0.0, base_rot),
    }
}

/// Model name of a standing banner's pole+bar body.
pub const BANNER_BODY: &str = "banner_body";
/// Model name of a standing banner's flag.
pub const BANNER_FLAG: &str = "banner_flag";
/// Model name of a **wall** banner's bar — vanilla's own wall-body-layer
/// construction, which has no pole at all.
pub const BANNER_WALL_BODY: &str = "banner_wall_body";
/// Model name of a **wall** banner's flag — the same cube as [`BANNER_FLAG`]'s at
/// a different rest pose.
pub const BANNER_WALL_FLAG: &str = "banner_wall_flag";

/// How a banner is attached to the world, and therefore which pair of meshes and
/// which placement angle it uses.
///
/// The same shape as [`SkullOrientation`], and for the same reason: the two forms
/// carry *different data* (a 16-way rotation segment against a four-way facing),
/// so a shared `angle: f32` field would let a caller hand a wall banner a segment
/// and get a plausible eighth-turn error. Unlike a skull, both forms here also
/// select a different **mesh**, since vanilla's own wall-body-layer
/// construction drops the pole.
///
/// No `Eq`/`Hash`, unlike [`SkullOrientation`]: the wall arm carries a yaw as an
/// `f32`, matching every other placement input in this module rather than
/// re-encoding four directions as an enum only this type would use.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BannerAttachment {
    /// A standing banner, with the `ROTATION` property.
    Ground {
        /// The `ROTATION` block-state property, `0..16` — vanilla's own
        /// 16-step rotation segment, segment `0` being north.
        rotation_segment: u8,
    },
    /// A wall banner, with `FACING`.
    Wall {
        /// Vanilla's own facing-to-yaw mapping of the wall banner's `FACING`
        /// property (south `0`, west `90`, north `180`, east `270`) —
        /// [`horizontal_facing_yaw`]'s convention.
        facing_yaw_deg: f32,
    },
}

/// The jar sheet a banner's *body* (pole/bar) and *flag* both draw with for
/// their opaque pass — vanilla's own banner-base sprite constant, resolved
/// to `entity/banner/banner_base`. This is the plain wood/cloth texture,
/// never a pattern mask: vanilla's own banner-submission function passes
/// this one sprite id to *both* the body and flag model draws before its
/// pattern pass draws anything coloured over the flag a second time.
pub const BANNER_BASE_TEXTURE_STEM: &str = "entity/banner/banner_base";

/// The one banner sheet stem, for [`block_entity_texture_stems`] — mirrors
/// [`bell_texture_stems`]'s shape: one stem shared by all four banner models
/// (standing and wall, body and flag), because vanilla's own
/// banner-submission function passes the same base sprite to every one of
/// them.
#[must_use]
pub fn banner_texture_stems() -> Vec<&'static str> {
    vec![BANNER_BASE_TEXTURE_STEM]
}

/// Model name of a shield's `plate`+`handle` mesh (`lodestone_assets::
/// block_entity_models::shield_model`) — see that function's doc for why a
/// shield, unlike a banner, has only one mesh, reused for every draw.
pub const SHIELD: &str = "shield";

/// Vanilla's own no-pattern shield-base sprite constant —
/// `entity/shield/shield_base_nopattern`,
/// the plain wood-and-iron sheet every shield with no `minecraft:base_color`
/// and no stored `minecraft:banner_patterns` layer draws (the common case,
/// straight off a crafting table).
pub const SHIELD_BASE_NO_PATTERN_TEXTURE_STEM: &str = "entity/shield/shield_base_nopattern";

/// Vanilla's own shield-base sprite constant — `entity/shield/shield_base`,
/// the sheet a shield with a base colour or at least one loom pattern draws
/// its **opaque** pass
/// with (a plain grey canvas the translucent pattern layers tint and mask
/// over, mirroring [`BANNER_BASE_TEXTURE_STEM`]'s role for a banner).
pub const SHIELD_BASE_TEXTURE_STEM: &str = "entity/shield/shield_base";

/// Whether a shield stack's translucent pattern pass draws at all —
/// vanilla's own has-patterns check: `true` when
/// the stack carries a `minecraft:base_color` **or** at least one stored
/// `minecraft:banner_patterns` layer. `false` is the common case (a shield
/// straight off a crafting table), which draws only the opaque
/// [`SHIELD_BASE_NO_PATTERN_TEXTURE_STEM`] sheet and nothing translucent —
/// [`shield_pattern_layers`](crate::banner_pattern::shield_pattern_layers)
/// is never even called for it, matching vanilla's own early-out rather than
/// calling it and discarding an always-present base mask the way a banner's
/// own `layers` list (never empty) does.
#[must_use]
pub fn shield_has_patterns(base_color: Option<&str>, pattern_count: usize) -> bool {
    base_color.is_some() || pattern_count > 0
}

/// `(model, texture)` for a shield item's **opaque** base draw — the one
/// `plate`+`handle` mesh ([`SHIELD`]), textured by whether the stack has
/// anything translucent to draw over it. Shared by every surface that draws
/// a shield (the first-person hand, the GUI icon) the same way
/// [`banner_item_rig`] is shared by a banner's two surfaces — see this
/// module's shield section for why there is no second mesh the way a
/// banner's `flag` is.
#[must_use]
pub fn shield_item_rig(has_patterns: bool) -> (&'static str, &'static str) {
    let texture = if has_patterns {
        SHIELD_BASE_TEXTURE_STEM
    } else {
        SHIELD_BASE_NO_PATTERN_TEXTURE_STEM
    };
    (SHIELD, texture)
}

/// Both sheets a shield can draw its opaque pass with, for
/// [`block_entity_texture_stems`]'s preload list — unlike
/// [`banner_texture_stems`]'s single stem, a shield's *runtime* state (has a
/// base colour or a pattern, or not) picks between two, so both must be
/// preloaded rather than only the default [`BlockEntityModelEntry::texture`]
/// a gate with no item state draws.
#[must_use]
pub fn shield_texture_stems() -> Vec<&'static str> {
    vec![SHIELD_BASE_TEXTURE_STEM, SHIELD_BASE_NO_PATTERN_TEXTURE_STEM]
}
