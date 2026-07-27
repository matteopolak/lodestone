//! Version-parameterised physics constants.
//!
//! The project's usual rule is "duplicate behaviour per version", but the
//! movement *integration* has been numerically stable since 1.8 (gravity `0.08`,
//! drag `0.91`/`0.98`, jump `0.42`, the `0.21600002F` ground-acceleration
//! constant). So the core is shared and version-free, parameterised by this
//! profile.
//!
//! # What genuinely varies by version
//!
//! Careful reading of the reference source shows the version-varying pieces that
//! *can* be expressed as profile scalars are limited to defaults such as the
//! sneak speed factor (`0.3` via `SNEAKING_SPEED`). The core arithmetic constants
//! do not change.
//!
//! # What is per-*entity*, not per-version (moved out)
//!
//! The collision hitbox (width/height) and the auto-step height were previously
//! fields here, but they are keyed on entity *type*, not on game version — a
//! zombie and a player share a version and not a hitbox. They now live in
//! [`crate::entity::EntityDimensions`], a per-call input to
//! [`crate::entity::move_entity`], and are supplied by the caller. This is the
//! category error in the "what cannot be a scalar" note below, in reverse: there,
//! per-version *behaviour* was smuggled into a scalar; housing per-*entity* data
//! on the per-version profile was the same mistake pointing the other way.
//!
//! # What canNOT be expressed as a profile scalar (architectural finding)
//!
//! Two things are *structural*, not scalar. They are therefore expressed as
//! **enum selectors on the profile** ([`InputModel`], [`FluidModel`]) rather than
//! numbers — a profile that could only carry scalars would run the modern
//! algorithm for 1.8 and look fully configured while being wrong, which is the
//! worst failure mode available here. Making the branch type-level forces every
//! profile to declare which algorithm it wants, and the 1.8 arms currently
//! `unimplemented!()` so the gap is loud, not silent:
//!
//! * **The client input pipeline.** Modern clients apply
//!   `modifyInputSpeedForSquareMovement` (a per-direction unit-square projection)
//!   inside `LocalPlayer.modifyInput`; 1.8's `moveFlying` normalised the raw
//!   input by `max(1, magnitude)` instead. This changes the *shape* of the input
//!   transform, not a coefficient — see [`InputModel`].
//! * **Fluid movement.** `getFluidFallingAdjustedMovement` (the `-0.003`
//!   slow-descent clamp) and the whole swimming/pose system are modern additions
//!   with no 1.8 analogue. Water physics is a different algorithm, not a retuned
//!   one — see [`FluidModel`]. The modern submerged path is implemented in
//!   [`crate::player::tick_water`]; its constants (`0.8`/`0.9` slow-down, `0.02`
//!   input speed) live here as scalars while the *branching* stays structural.
//!
//! These are called out here (rather than hidden) because they are the real
//! boundary of the "physics is version-free" decision.

/// Selects the **client input-transformation algorithm**. This is a *structural*
/// choice (a different function), not a scalar knob, so it lives as an enum: a
/// profile that could only carry numbers would silently run modern math for 1.8
/// and look fully configured while being wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputModel {
    /// 1.9+ pipeline: `LocalPlayer.modifyInput` →
    /// `modifyInputSpeedForSquareMovement`, projecting the input onto a unit
    /// square per direction. This is the validated path.
    UnitSquareProjection,
    /// 1.8 pipeline: `EntityLivingBase.moveFlying` normalised the raw
    /// strafe/forward by `max(1, magnitude)` with **no** unit-square projection.
    /// Not yet implemented or bit-validated — it is deliberately left as an
    /// explicit branch (blocked on the restructured 1.8 client and a 1.8 JVM
    /// oracle) so 1.8 movement fails loudly instead of quietly using 1.9+ math.
    LegacyMoveFlying,
}

/// Selects the **fluid-movement algorithm**. Also structural: modern Minecraft
/// has swimming poses, `getFluidFallingAdjustedMovement`, and separate
/// water/lava travel branches that 1.8 lacks entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FluidModel {
    /// Modern `travelInWater`/`travelInLava` with the falling-adjusted clamp and
    /// sprint/efficiency terms. This is the path [`crate::player::tick_water`]
    /// implements and validates.
    Modern,
    /// 1.8 in-fluid handling (a simpler single branch). Not yet implemented or
    /// validated; present so the seam is type-level rather than a hidden
    /// assumption.
    Legacy1_8,
}

/// Numeric knobs for the movement core. All fields carry vanilla's exact widths.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PhysicsProfile {
    /// Base `MOVEMENT_SPEED` attribute (`0.1F`).
    pub base_movement_speed: f32,
    /// Sprint `ADD_MULTIPLIED_TOTAL` modifier amount (`0.3F`).
    pub sprint_speed_modifier: f32,
    /// `SNEAKING_SPEED` default (`0.3`).
    pub sneaking_speed: f32,
    /// Base gravity (`DEFAULT_BASE_GRAVITY = 0.08`).
    pub gravity: f32,
    /// Horizontal air-drag base (`0.91F`), multiplied by block friction.
    pub air_drag: f32,
    /// Vertical air-drag base (`0.98F`).
    pub vertical_air_drag: f32,
    /// `AIR_DRAG_MODIFIER` attribute default (`1.0`).
    pub air_drag_modifier: f32,
    /// `FRICTION_MODIFIER` attribute default (`1.0`).
    pub friction_modifier: f32,
    /// Ground-acceleration constant (`0.21600002F`).
    pub ground_accel: f32,
    /// Flying (in-air, not sprinting attribute) input speed (`0.02F`).
    pub flying_speed: f32,
    /// Jump power (`JUMP_STRENGTH = 0.42F`, with unit block/boost factors).
    pub jump_power: f32,
    /// Sprint-jump horizontal boost magnitude (`0.2`).
    ///
    /// This is an `f64` because vanilla writes it as the `double` literal `0.2`
    /// (`Mth.cos(angle) * 0.2`), not `0.2F`. Storing it as `f32` and widening
    /// gives `0.20000000298…` and drifts the reported Z by ~3e-9 per jump.
    pub sprint_jump_boost: f64,
    /// Water horizontal slow-down when not sprinting (`0.8F`).
    pub water_slow_down: f32,
    /// Water horizontal slow-down when sprinting (`0.9F`).
    pub water_sprint_slow_down: f32,
    /// Base input speed used by `moveRelative` in fluids (`0.02F`).
    pub fluid_input_speed: f32,
    /// Water flow-current push scale (`Entity.updateFluidInteraction`, the
    /// `0.014` `double` applied to the accumulated current in water).
    pub water_push_scale: f64,
    /// Lava flow-current push scale — the overworld value
    /// (`0.0023333333333333335`). The nether uses `0.007` (`FAST_LAVA`), an
    /// *environment* attribute rather than a version difference, so a caller in
    /// the nether passes `0.007` explicitly to [`crate::fluid::apply_fluid_push`].
    pub lava_push_scale: f64,
    /// Structural selector for the client input transform (see [`InputModel`]).
    pub input_model: InputModel,
    /// Structural selector for fluid movement (see [`FluidModel`]).
    pub fluid_model: FluidModel,
}

impl PhysicsProfile {
    /// Profile for modern Java Edition (verified against the 26.2 reference
    /// source; also valid for 1.21.x, which shares these constants).
    #[must_use]
    pub const fn mc_1_21() -> Self {
        Self {
            base_movement_speed: 0.1,
            sprint_speed_modifier: 0.3,
            sneaking_speed: 0.3,
            gravity: 0.08,
            air_drag: 0.91,
            vertical_air_drag: 0.98,
            air_drag_modifier: 1.0,
            friction_modifier: 1.0,
            ground_accel: 0.216_000_02,
            flying_speed: 0.02,
            jump_power: 0.42,
            sprint_jump_boost: 0.2,
            water_slow_down: 0.8,
            water_sprint_slow_down: 0.9,
            fluid_input_speed: 0.02,
            water_push_scale: 0.014,
            lava_push_scale: 0.002_333_333_333_333_333_5,
            input_model: InputModel::UnitSquareProjection,
            fluid_model: FluidModel::Modern,
        }
    }

    /// Profile for 1.8.9. The shared movement constants are identical; the
    /// differences (input pipeline, fluids) are *structural* and selected by the
    /// [`InputModel`]/[`FluidModel`] enums below, not by any scalar — see the
    /// module docs.
    #[must_use]
    pub const fn mc_1_8() -> Self {
        // Scalars are intentionally identical to `mc_1_21`: the numeric core has
        // not changed. The version difference is expressed as a structural
        // branch through `input_model`/`fluid_model`, so a caller cannot end up
        // with 1.8 movement that silently runs the modern arithmetic.
        Self {
            input_model: InputModel::LegacyMoveFlying,
            fluid_model: FluidModel::Legacy1_8,
            ..Self::mc_1_21()
        }
    }
}

impl Default for PhysicsProfile {
    fn default() -> Self {
        Self::mc_1_21()
    }
}
