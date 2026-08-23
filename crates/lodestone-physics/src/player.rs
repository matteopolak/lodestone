//! Player movement core, mirroring vanilla's `LivingEntity`/`Player` tick.
//!
//! The per-tick pipeline reproduced here is (for a non-fluid, non-elytra
//! player), in order:
//!
//! 1. `aiStep` velocity snap-to-zero (`< 9.0E-6` horizontal, `< 0.003` vertical).
//! 2. Jump handling (`jumpFromGround`, including the sprint boost).
//! 3. `travel` → `travelInAir`:
//!    - `moveRelative` adds the friction-influenced input acceleration,
//!    - `move` resolves collision and applies the block speed factor,
//!    - gravity is subtracted from the post-move Y,
//!    - horizontal drag (`blockFriction * 0.91`) and vertical drag (`0.98`) are
//!      applied last.
//!
//! Every width (`f32` vs `f64`) and every operation order matches the reference
//! source, because the server validates the resulting positions.

use crate::collision::{CollisionView, no_collision};
use crate::entity::{
    AirTravelContext, EntityDimensions, EntityMotion, MoveContext, move_entity,
    move_entity_with_nearby, travel_in_air_among_entities,
};
use crate::fluid::apply_fluid_push;
use crate::fluid_state::{FluidState, compute_fluid_state};
use crate::geometry::{Aabb, Vec3d};
use crate::mth::{self};
use crate::pose::{Pose, update_player_pose};
use crate::profile::{FluidModel, InputModel, PhysicsProfile};

/// `Avatar.DEFAULT_EYE_HEIGHT` — the player standing eye offset (`1.62F`), used
/// as the default pose [`eye height`](PlayerState::eye_height). The swimming /
/// crawling / gliding pose lowers it to `0.4`, crouching to `1.27`; [`crate::pose`]
/// owns that mapping and [`tick`] applies it.
pub const DEFAULT_EYE_HEIGHT: f32 = 1.62;

/// `UseEffects` (`world/item/component/UseEffects.java`) — the pair vanilla's
/// `LocalPlayer` consults while `isUsingItem()` is true: how much to scale
/// movement input (`itemUseSpeedMultiplier`, folded into `modifyInput`) and
/// whether sprinting may continue at all (`canSprint`, ANDed into
/// `canStartSprinting` as `!isSlowDueToUsingItem()`).
///
/// Only two values exist in the 26.2 item table, and there is no third to add
/// a case for without a new jar reading: [`Self::DEFAULT`] (`UseEffects.
/// DEFAULT`) is what every item without an explicit override gets — food,
/// potions, the bow, the crossbow, the shield — and the seven spear items
/// (wooden/stone/copper/iron/golden/diamond/netherite, via
/// `Item.Properties.spear(...)`, `Item.java:542`) are the *only* override,
/// [`Self::SPEAR`]. So a caller resolving "which effects apply" needs only
/// "is the held item a spear", not a general item-effects table.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UseEffects {
    /// Whether sprinting may continue while this item is in use
    /// (`UseEffects.canSprint()`).
    pub can_sprint: bool,
    /// The movement-input scale applied while this item is in use
    /// (`UseEffects.speedMultiplier()`).
    pub speed_multiplier: f32,
}

impl UseEffects {
    /// `UseEffects.DEFAULT` — every use-item except a spear: no sprinting,
    /// input scaled to a fifth.
    pub const DEFAULT: Self = Self {
        can_sprint: false,
        speed_multiplier: 0.2,
    };
    /// The spear override (`Item.Properties.spear(...)`) — charging a spear
    /// neither slows movement nor blocks sprinting.
    pub const SPEAR: Self = Self {
        can_sprint: true,
        speed_multiplier: 1.0,
    };

    /// Resolve which [`UseEffects`] a held item's own `use()` would arm,
    /// from its id alone — see this type's own doc for why an id is enough:
    /// the seven spear items (`Item.Properties.spear(...)`) are the *only*
    /// override in the 26.2 item table, and every one of them is named
    /// `*_spear` (`wooden_spear`, `stone_spear`, `copper_spear`,
    /// `iron_spear`, `golden_spear`, `diamond_spear`, `netherite_spear`).
    /// Anything else — including an empty main hand — gets [`Self::DEFAULT`],
    /// matching vanilla's per-item `useEffects` field defaulting to
    /// `UseEffects.DEFAULT` when a `Item.Properties` builder never overrides
    /// it.
    #[must_use]
    pub fn for_item(id: &str) -> Self {
        // Namespace-agnostic on purpose: a resource pack's own custom item
        // could ship under a different namespace, and the *path* is the only
        // part vanilla's own builder call keys off (`Item.Properties.spear`
        // is applied per Java call site, not looked up by namespace).
        let path = id.rsplit_once(':').map_or(id, |(_, path)| path);
        if path.ends_with("_spear") {
            Self::SPEAR
        } else {
            Self::DEFAULT
        }
    }
}

/// Raw player intent for one tick, before any client-side transformation.
///
/// `forward`/`strafe` are the digital movement axes (typically `-1.0`, `0.0` or
/// `1.0`), matching `Input.getMoveVector()` (`y` = forward, `x` = strafe).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MovementInput {
    /// Forward (`+`) / backward (`-`) intent.
    pub forward: f32,
    /// Left (`+`) / right (`-`) strafe intent.
    pub strafe: f32,
    /// Jump key held.
    pub jump: bool,
    /// Sneak (shift) key held.
    pub sneak: bool,
    /// Sprint active this tick.
    pub sprint: bool,
    /// This tick's [`UseEffects`] if an item is being used (charging, eating,
    /// drawing a bow, blocking with a shield, …), or `None` while idle.
    ///
    /// `LocalPlayer.modifyInput` applies `speed_multiplier` between the
    /// `0.98` scale and the sneak scale (`modify_input_unit_square` below);
    /// the sprint half of `UseEffects` (`can_sprint`) is a *separate*
    /// conjunct on `canStartSprinting` that the caller (`lodestone-controller`'s
    /// `compute_movement_intent`) must apply itself before ever constructing
    /// this — by the time `sprint` reaches here it is already gated, matching
    /// how the food-level sprint gate already works.
    pub using_item: Option<UseEffects>,
}

impl MovementInput {
    /// A no-input tick (standing still).
    pub const NONE: Self = Self {
        forward: 0.0,
        strafe: 0.0,
        jump: false,
        sneak: false,
        sprint: false,
        using_item: None,
    };
}

/// Active status effects that influence the movement integration.
///
/// Only the effects that change the *physics* (not just stats) live here.
/// Speed/Slowness are deliberately **absent**: they are attribute modifiers on
/// `MOVEMENT_SPEED`, so they arrive pre-folded into the effective movement speed
/// via the attribute pipeline (see the crate docs' "attribute seam"), not as a
/// physics flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StatusEffects {
    /// `MobEffects.LEVITATION` amplifier (0-based) if active. In `travelInAir`
    /// this *replaces* gravity with `y += (0.05*(amp+1) - y) * 0.2`.
    pub levitation: Option<u32>,
    /// `MobEffects.SLOW_FALLING`. Reduces `getEffectiveGravity()` to
    /// `min(gravity, 0.01)` **while falling** — which, in fluids, is precisely
    /// what revives the otherwise-dead `-0.003` slow-descent clamp.
    pub slow_falling: bool,
    /// `MobEffects.DOLPHINS_GRACE`. Forces the in-water horizontal slow-down to
    /// `0.96F` regardless of sprint state.
    pub dolphins_grace: bool,
    /// `MobEffects.JUMP_BOOST` amplifier (0-based) if active. Per the ruling that
    /// Jump Boost is **not** a `MOVEMENT_SPEED` modifier, it rides its own field:
    /// `getJumpBoostPower()` adds `0.1F * (amp + 1)` to the jump velocity in
    /// `getJumpPower`, *after* the `JUMP_STRENGTH * blockJumpFactor` product.
    pub jump_boost: Option<u32>,
}

/// Mutable player physics state carried across ticks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerState {
    /// World position (feet centre), `Vec3` in vanilla.
    pub position: Vec3d,
    /// Delta movement (velocity), `Vec3` in vanilla.
    pub velocity: Vec3d,
    /// Yaw in degrees; `0` faces `+Z` (south).
    pub yaw: f32,
    /// Pitch in degrees.
    pub pitch: f32,
    /// Whether the player is on the ground, i.e. this tick's move collided
    /// **downward** (`verticalCollisionBelow` in `Entity.move`). This is the flag
    /// the client **transmits** to the server on every movement packet
    /// (`ServerboundMovePlayerPacket`'s `onGround`).
    ///
    /// It is a *distinct decision* from the collision result the server re-runs
    /// from our reported position: if the server ever believes we are unsupported
    /// and not descending in open air, it counts `aboveGroundTickCount` and
    /// disconnects with `multiplayer.disconnect.flying` at `getMaximumFlyingTicks`
    /// (80 ticks at default gravity). Because our position is bit-exact, the
    /// server's own downward collision stays aligned with this flag, so the two
    /// never diverge — but a driver must transmit *this* value unmodified rather
    /// than re-deriving one.
    ///
    /// Vanilla computes it identically in **every** movement mode (walking,
    /// swimming, climbing, falling); there is no bespoke "supported" notion for
    /// swimming or climbing. The **sole override** is `Player.tick`, which forces
    /// `onGround = false` while a **spectator or passenger** (riding a
    /// boat/minecart/horse). This engine still has no riding state — that is a
    /// session fact, not a physics one — and the override now has a real owner:
    /// `lodestone_ecs::player::pin_passenger_to_vehicle` applies it off
    /// `lodestone_ecs::session::Riding` in the same system that snaps the player
    /// onto the seat, so `on_ground` keeps exactly one writer per tick. See the
    /// `spectator_or_passenger_note` contract test in `tests/on_ground.rs` for why
    /// it is not a field here, and for the measured correction that the *server*
    /// never kicks a passenger over this flag (its float check is
    /// `&& !isPassenger()`) — the override is for local readers.
    ///
    /// Note: a player starting from rest reports airborne for exactly one settle
    /// tick, because a tick runs `move()` before applying gravity — matching the
    /// server's own first-tick computation.
    pub on_ground: bool,
    /// Whether the player collided horizontally last tick.
    pub horizontal_collision: bool,
    /// `noJumpDelay` countdown that gates repeated jumps.
    pub no_jump_delay: i32,
    /// `LocalPlayer.autoJumpEnabled` — the client Options Auto-Jump toggle,
    /// defaulting **on** exactly like the `LocalPlayer` field
    /// (`LocalPlayer.java:152-153, 299`).
    ///
    /// **This is the only gate on the detector, and it is now really driven.**
    /// The shell's `Options::auto_jump` reaches it once per tick through
    /// `lodestone_ecs::player::AutoJump` (issue #201): before that the field sat
    /// at its `true` default for the whole session, so the settings toggle was
    /// decorative — the option read OFF and [`update_auto_jump`] still armed
    /// jumps. See that resource's doc for the seam.
    pub auto_jump_enabled: bool,
    /// `LocalPlayer.autoJumpTime` — the one-tick deferral that carries the
    /// auto-jump *decision* (made at the end of `move()` by [`update_auto_jump`])
    /// across to the next `aiStep`, where it is spent as a forced jump
    /// (`LocalPlayer.java:784-789`). `0` means idle.
    pub auto_jump_time: i32,
    /// Whether the player is currently sprinting (affects movement speed).
    pub sprinting: bool,
    /// Whether the player is gliding with an elytra (`isFallFlying()`). When set,
    /// [`tick`] routes to [`tick_elytra`] instead of [`tick_air`] (fluid still
    /// takes precedence, matching vanilla's `travel()` dispatch order).
    pub fall_flying: bool,
    /// Active physics-affecting status effects.
    pub effects: StatusEffects,
    /// Effective `MOVEMENT_SPEED` attribute value handed in by the entity layer
    /// (`lodestone-entity`'s `AttributeInstance.value()`), or `None` to let
    /// physics compute the standalone base+sprint value itself.
    ///
    /// **Reconciled attribute seam.** Vanilla `Player` does, every tick,
    /// `setSpeed((float) getAttributeValue(MOVEMENT_SPEED))` and movement reads
    /// that float via `getSpeed()`. That attribute value already folds in the
    /// transient **sprint** modifier (`AddMultipliedTotal 0.3`) *and* any
    /// Speed/Slowness/Depth-Strider modifiers, computed by the three-stage
    /// `calculateValue()` (AddValue → AddMultipliedBase → AddMultipliedTotal).
    /// Physics must **not** reimplement that maths or re-apply sprint: when this
    /// is `Some(v)`, [`friction_influenced_speed`] uses `v as f32` directly
    /// (reproducing vanilla's `(float)` cast at the same place) and ignores the
    /// `sprinting` flag, so there is no double-count. Pass the raw `f64` — never
    /// a pre-cast `f32` — so the double→float rounding stays inside physics.
    pub movement_speed: Option<f64>,
    /// Pending **"stuck in block" speed multiplier** (`Entity.stuckSpeedMultiplier`),
    /// set last tick by the block we were inside and consumed at the top of the
    /// next move (see [`CollisionView::stuck_multiplier`]). `ZERO` means "not
    /// stuck"; vanilla treats `lengthSqr <= 1.0E-7` as unset. Cobweb, powder snow
    /// and sweet berry bush write this; consumption multiplies the tick's
    /// movement component-wise and then zeroes velocity, exactly as vanilla — the
    /// one-tick delay between entering the block and being slowed is observable
    /// and reproduced.
    pub stuck_speed_multiplier: Vec3d,
    /// The player's current [`Pose`], which decides the **collision box** (via
    /// [`Self::dimensions`]) and the [`eye height`](Self::eye_height).
    ///
    /// **An output of [`tick`], not an input to it.** `Player.updatePlayerPose`
    /// runs at the end of `Player.tick()` and is fit-gated — a pose whose box
    /// would not fit where the player stands is vetoed — so it cannot be
    /// meaningfully set from outside per tick. [`Self::with_pose`] exists to seed
    /// an initial pose (a test fixture, or a session resuming mid-swim); after the
    /// first [`tick`] the machine owns it. See [`crate::pose`] for the state
    /// machine and for why skipping its gate would clip a surfacing swimmer into
    /// a ceiling with nothing to catch them.
    ///
    /// The narrower travel entry points ([`tick_air`], [`tick_water`],
    /// [`tick_lava`], [`tick_elytra`]) are vanilla's `travel`, not `Player.tick`:
    /// they *read* the pose for the box and never write it.
    pub pose: Pose,
    /// Pose **eye height** (`getEyeHeight`), the offset from feet to eye used by
    /// [`crate::compute_fluid_state`] to decide eye-in-fluid. Standing is
    /// `1.62` (`Avatar.DEFAULT_EYE_HEIGHT`); the swimming/crawling/gliding pose is
    /// `0.4`, crouching `1.27`.
    /// `getEyeY()` widens it to `double` and adds `position.y`, reproduced exactly.
    ///
    /// **Derived from [`Self::pose`], and rewritten by every [`tick`].** In
    /// vanilla the two are one record — `refreshDimensions` sets
    /// `this.eyeHeight = newDim.eyeHeight()` in the same three lines that set the
    /// box (`Entity.java:3395-3400`) — and splitting them is observable: a
    /// `0.6`-high box with a `1.62` eye makes a fully submerged swimmer read
    /// `eye_in_water == false`, because `compute_fluid_state`'s cell sweep is
    /// bounded by the *box* and so never reaches the eye's cell. That kills the
    /// fog, the overlay and `updateSwimming`'s entry condition at once.
    ///
    /// It is therefore an **output mirror**, published for the camera and the fog:
    /// nothing inside [`tick`] reads this field. Both places that need an eye
    /// height (`compute_fluid_state`'s eye sweep and `getFluidJumpThreshold`) call
    /// [`Pose::eye_height`] directly, so a caller that overwrites this field
    /// between ticks — as `lodestone-ecs`'s own pose layer currently does — can
    /// mislead the camera but can never desynchronise the eye from the box inside
    /// physics.
    ///
    /// [`Self::with_eye_height`] therefore only usefully models a pose this crate
    /// does not have (`SLEEPING`, `DYING`), and only for a driver that does not
    /// call [`tick`].
    pub eye_height: f32,
    /// **Output.** `isEyeInFluid(WATER)` from the last [`tick`], i.e. the eye
    /// block-column held water spanning the eye Y. Combine with in-water via
    /// [`Self::eye_in_water`] + fluid presence for `isUnderWater`; the shell reads
    /// this for submerged fog, the underwater overlay, and `ambient.underwater.*`.
    pub eye_in_water: bool,
    /// **Output.** `isEyeInFluid(LAVA)` from the last [`tick`].
    pub eye_in_lava: bool,
    /// **Output.** `Entity.isSwimming()` after the last [`tick`]'s
    /// `updateSwimming`: sprint-swimming, entered when sprinting while submerged in
    /// water and sustained while sprinting in water.
    ///
    /// The server derives this **itself**, in its own `Entity.baseTick` →
    /// `updateSwimming`, from `isSprinting()` and its own collision — there is no
    /// swimming bit anywhere on the wire (`Input` is seven booleans:
    /// forward/backward/left/right/jump/shift/sprint, `Input.java`). What a driver
    /// must transmit is the **sprint** edge, via
    /// `ServerboundPlayerCommandPacket(START_SPRINTING/STOP_SPRINTING)`
    /// (`LocalPlayer.sendIsSprintingIfNeeded`, `LocalPlayer.java:303-312`) — the
    /// `Input` packet's `sprint` flag is stored as `lastClientInput` and does *not*
    /// call `setSprinting` (`ServerGamePacketListenerImpl.java:424` vs `:1719`).
    /// Send only sprint and the server's swim pose follows; send the input packet
    /// alone and it never does.
    pub swimming: bool,
    /// **Output.** `LivingEntity.swimAmount` after the last [`tick`] — a `0..1`
    /// ramp toward the swim pose, advanced by `SWIM_AMOUNT_PER_TICK` (`0.09F`,
    /// `LivingEntity.java:174`) per tick and clamped to `[0, 1]`
    /// (`LivingEntity.java:3478-3483`), never snapping the way
    /// [`Self::swimming`] itself does. Vanilla advances this **every tick**,
    /// right after `updateSwimming` decides this tick's [`Self::swimming`] and
    /// before `aiStep`/`travel` runs (`LivingEntity.tick()`,
    /// `LivingEntity.java:2755-2758`), so [`crate::player::tick`] updates it in
    /// that same slot.
    ///
    /// Vanilla uses this to blend the swimming model's body-pitch animation
    /// (`HumanoidModel`/`HumanoidMobRenderer`) — **not** the camera eye height,
    /// which Camera.java smooths independently (see `camera_rig.rs`'s
    /// `EyeHeightSmoother`). Nothing in this crate reads this field; it exists
    /// so a renderer can consume the exact per-tick ramp instead of
    /// re-deriving one from [`Self::swimming`] (which would reintroduce the
    /// snap this field exists to avoid).
    pub swim_amount: f32,
    /// **Output.** The previous tick's [`Self::swim_amount`]
    /// (`swimAmountO`), for a partial-tick interpolated read —
    /// `Mth.lerp(a, swimAmountO, swimAmount)` (`LivingEntity.java:401`).
    /// Because the ramp is monotonic and clamped (unlike the arm-swing's
    /// sawtooth `attack_anim`, whose `getAttackAnim` wraps a negative delta), a
    /// plain `lerp(a, swim_amount_o, swim_amount)` is exactly vanilla's read —
    /// no wrap-around correction needed.
    pub swim_amount_o: f32,
    /// `Attributes.WATER_MOVEMENT_EFFICIENCY` — the Depth Strider attribute, as an
    /// **input** from the equipment layer (like [`Self::movement_speed`]).
    ///
    /// Vanilla's `travelInWater` reads `getAttributeValue(WATER_MOVEMENT_EFFICIENCY)`
    /// (`LivingEntity.java:2509`), halves it when airborne, and then uses it to lerp
    /// the horizontal slow-down toward `0.546_000_06` and the input speed toward
    /// `getSpeed()`. Depth Strider contributes `0.33` per level via its enchantment
    /// effect, so a level-III boot is `0.99`.
    ///
    /// **Default `0.0`, because no caller in this repo can reach the value yet — but
    /// it is closer than it looks, and the gap is a missing accessor, not missing
    /// data.** `lodestone-entity`'s attribute table already knows
    /// `water_movement_efficiency` (default `0.0`, range `0..1`), `v770` has the
    /// attribute type, and `lodestone-client` folds
    /// `ClientEvent::EntityAttributesUpdated` into per-entity
    /// `EntityAttributeSnapshot`s. What is absent is (a) any route from the shell to
    /// the **local player's** attribute set — the shell's `EntitySnapshot` drops the
    /// `attributes` field, and there is no `NetClient` accessor for it — and (b) the
    /// three-stage `calculateValue()` fold from base + modifiers to an effective
    /// value, which the shell also does not do for `MOVEMENT_SPEED` (it recomputes
    /// that itself instead).
    ///
    /// The arithmetic lives in [`tick_water`] so that the value is the *only* missing
    /// piece rather than the whole branch, and so nothing can silently substitute a
    /// plausible number for it.
    pub water_movement_efficiency: f32,
    /// `Abilities.flying` — **creative flight is currently engaged**.
    ///
    /// This is *server authority*, not a local toggle:
    /// `ClientboundPlayerAbilitiesPacket` carries it, and vanilla's client-side
    /// toggle is gated on `abilities.mayfly` (which lives on the driver, not here
    /// — physics never decides whether flight is *allowed*, only what it does).
    /// A driver that lets the player fly without a server grant is a bug, not a
    /// feature: see `docs/creative-flight.md`.
    ///
    /// # What setting this does, in three places and no more
    ///
    /// Flight is **not** a fourth travel mode beside air/water/lava/elytra. It is
    /// a set of modifications to the machinery that already exists, because
    /// `Player.travel` *wraps* `super.travel(input)` rather than replacing it
    /// (`Player.java:1416-1424`):
    ///
    /// 1. **A dispatch suppressor.** `Player.isAffectedByFluids()` is
    ///    `!abilities.flying` (`Player.java:875-878`) and `shouldTravelInFluid`
    ///    requires it, so a flying player **never takes the fluid branch** — flying
    ///    underwater goes through [`tick_air`], not [`tick_water`].
    /// 2. **A speed substitution**, via [`player_flying_speed`] /
    ///    [`Self::flying_speed`].
    /// 3. **A post-travel Y overwrite**: the *pre*-travel Y, times `0.6`,
    ///    *replaces* the post-travel Y. Gravity's contribution during the tick is
    ///    discarded outright rather than damped, and there is **no horizontal
    ///    term** — see [`travel_and_check_inside_blocks`].
    ///
    /// Plus the `!flying` conjuncts tabulated in `docs/creative-flight.md`, each of
    /// which is applied at its own site.
    ///
    /// **Collision stays on.** Creative flight is not noclip; only *spectator*
    /// mode sets `noPhysics`, which this crate does not model at all (see the
    /// module docs).
    pub flying: bool,
    /// `Abilities.flyingSpeed` — the **creative-flight** input speed, server-set,
    /// default `0.05F` (`Abilities.java`'s `DEFAULT_FLYING_SPEED`).
    ///
    /// Consumed by [`player_flying_speed`], which doubles it while sprinting. Note
    /// the vertical fly impulse (`LocalPlayer.aiStep`'s `inputYa *
    /// abilities.getFlyingSpeed() * 3.0F`) uses this **raw**, *un*-doubled value
    /// even while sprinting — that asymmetry is the driver's to reproduce, and
    /// `lodestone-ecs`'s `apply_creative_flight_input` does.
    ///
    /// **Not [`PhysicsProfile::flying_speed`]**, which is the unrelated `0.02F`
    /// non-flying airborne constant. They are 2.5x apart and both are called
    /// "flying speed" in vanilla.
    pub flying_speed: f32,
    /// `Entity.fallDistance` (`Entity.java:245` — a `double`, not a `float`, since
    /// 26.2).
    ///
    /// **Why it is load-bearing.** [`move_entity`] is documented as modelling only
    /// "the parts of `Entity.move` that affect an entity's reported position", and
    /// fall distance used to be squarely outside that: it drives fall *damage*,
    /// which the server owns. `Player.maybeBackOffFromEdge` changes that —
    /// `isAboveGround` consults `fallDistance`, and the back-off moves you, so fall
    /// distance is now a position input.
    ///
    /// **`isAboveGround` (`Player.java:932`) is the only reader in *this* crate,
    /// not the only reader in vanilla.** The others are all outside this crate's
    /// scope today, and are listed because a maintained value is what unblocks
    /// them: `Player.canCriticalAttack` (`Player.java:1033`, `fallDistance > 0.0`
    /// — the crit condition), `LivingEntity.checkFallDamage`'s damage calculation
    /// (`LivingEntity.java:368-370`), and `Block.fallOn` (`Entity.java:1571`).
    ///
    /// **This crate maintains it.** [`tick`]/[`tick_air`]/[`tick_water`]/
    /// [`tick_lava`]/[`tick_elytra`] reproduce every site vanilla touches it:
    ///
    /// * **Accumulation + grounded reset** — `Entity.checkFallDamage`
    ///   (`Entity.java:1564-1582`, reached through `LivingEntity`'s override at
    ///   `LivingEntity.java:363-394`, which spawns landing particles and then calls
    ///   `super.checkFallDamage` unchanged): `if (!isInWater() && ya < 0.0)
    ///   fallDistance -= (float) ya;` then, unconditionally, `if (onGround)
    ///   resetFallDistance();`. Note the `(float)` truncation of the `double` delta
    ///   *before* the subtraction into the `double` field — reproduced here as
    ///   `state.fall_distance -= f64::from(ya as f32)`. In vanilla this call sits
    ///   inside `Entity.move()` itself (`Entity.java:783-784`), gated on
    ///   `isLocalInstanceAuthoritative()` — always `true` for `LocalPlayer`
    ///   (`Entity.java:3594-3596`, `Player.java:1276-1283`,
    ///   `LocalPlayer.java:376`), which is the only player this crate models. The
    ///   `movementLength >= 1.0` clip-through reset that also lives inside `move()`
    ///   (`Entity.java:747-754`) needs a world raycast against
    ///   `ClipContext.Block.FALLDAMAGE_RESETTING` and is **not** modelled — see
    ///   [`crate::entity::move_entity`]'s own scope note.
    /// * **Water reset** — `Entity.updateFluidInteraction`: `if (inWater)
    ///   resetFallDistance();` (`Entity.java:1658-1659`), called from `baseTick`
    ///   (`Entity.java:537`) before `travel`. This crate's dispatch already
    ///   computes the same per-tick fluid summary before choosing [`tick_water`],
    ///   so the reset lands at the top of that function. **The `baseTick` call is
    ///   not the only one**: `LivingEntity.checkFallDamage` calls
    ///   `updateFluidInteraction` again from *inside* `move()`
    ///   (`LivingEntity.java:365`), which is why the water-**entry** tick diverges
    ///   by one tick here — see [`accumulate_fall_distance`], which documents the
    ///   bound and why it cannot move the player.
    /// * **Lava halving** — `Entity.baseTick`: `if (isInLava()) fallDistance *=
    ///   0.5;` (`Entity.java:555-557`), applied at the top of [`tick_lava`] for the
    ///   same reason.
    /// * **Climbable reset** — `LivingEntity.handleOnClimbable`: `if
    ///   (onClimbable()) resetFallDistance();` (`LivingEntity.java:2693-2695`),
    ///   reached only through `travelInAir` (`LivingEntity.java:2666-2669`), so
    ///   only [`tick_air`] applies it — matching vanilla, where a climbable never
    ///   resets fall distance while swimming or gliding.
    /// * **Slow Falling / Levitation reset** — `LivingEntity.aiStep`: `if
    ///   (hasEffect(SLOW_FALLING) || hasEffect(LEVITATION)) resetFallDistance();`
    ///   (`LivingEntity.java:3123-3125`), unconditionally before the `travel()`
    ///   dispatch — applied in [`tick`] before it picks a travel path.
    /// * **Elytra accumulation clamp** — `Entity.checkFallDistanceAccumulation`:
    ///   `if (deltaMovement.y() > -0.5 && fallDistance > 1.0) fallDistance = 1.0;`
    ///   (`Entity.java:2904-2908`), called from `LivingEntity.updateFallFlying`
    ///   (`LivingEntity.java:3183-3184`), itself only reached `if (isFallFlying())`
    ///   in `aiStep` (`LivingEntity.java:3117-3119`) — before the Slow
    ///   Falling/Levitation check and before `travel()`. Applied in [`tick`]
    ///   alongside that check, gated on [`Self::fall_flying`].
    /// * **Stuck-in-block reset** — `Entity.makeStuckInBlock`: `resetFallDistance();
    ///   this.stuckSpeedMultiplier = speedMultiplier;` (`Entity.java:2945-2947`),
    ///   fired every tick `Block.entityInside` finds a stuck-triggering block
    ///   (cobweb, powder snow, sweet berry bush, honey). This crate's
    ///   `update_stuck_multiplier` already reproduces the block scan that feeds
    ///   `stuckSpeedMultiplier`; the reset rides along whenever it finds one.
    ///
    /// **Not modelled, matching pre-existing gaps elsewhere in this crate.** The
    /// mid-`move` water re-evaluation (`LivingEntity.java:365`) is the one gap
    /// that is a *divergence* rather than an absent feature — bounded to the
    /// water-entry tick and unable to affect position, fully documented on
    /// [`accumulate_fall_distance`] and pinned by a test.
    /// Creative flight (`Player.aiStep`: `if (abilities.flying &&
    /// !isPassenger()) resetFallDistance();`, `Player.java:449-451`) does not
    /// apply — see [`tick_air`]'s own doc on `!abilities.flying`, "this crate has
    /// no creative flight". Riding/vehicles do not apply — "this engine has no
    /// riding state" (see the `on_ground` doc on
    /// [`Self::on_ground`]/`tests/on_ground.rs`'s `spectator_or_passenger_note`).
    /// Bubble columns do not apply — see [`tick_water`]'s "Not modelled" list.
    /// **Teleport is a driver responsibility**: this crate has no teleport
    /// primitive of its own (a driver sets [`Self::position`] directly), so a
    /// caller that snaps the position (server correction, respawn, an ender pearl
    /// or chorus fruit consume effect, all of which call `resetFallDistance()` in
    /// vanilla) must also call [`Self::reset_fall_distance`] itself.
    ///
    /// **Sign, verified against the jar rather than assumed.** A negative `ya`
    /// (moving down) makes `fallDistance -= (float) ya` an *increase* — e.g.
    /// `ya = -0.5` gives `fallDistance -= -0.5`, i.e. `+= 0.5`. This is invisible
    /// in any test where the player never leaves the ground, and it is exactly the
    /// input this field exists for.
    pub fall_distance: f64,
    /// `Entity.DATA_TICKS_FROZEN` (`TicksFrozen`) — ticks accumulated standing in
    /// powder snow, `0..=`[`Self::TICKS_REQUIRED_TO_FREEZE`]. Maintained by
    /// [`tick`] (`update_freezing`, issue #212): `+1` (capped) each tick the
    /// swept movement segment finds `CollisionView::is_powder_snow`, `-2`
    /// (floored at `0`) every other tick — `LivingEntity.aiStep`'s freezing
    /// block, `LivingEntity.java:3139-3151`. See [`Self::is_freezing`],
    /// [`Self::is_fully_frozen`], [`Self::percent_frozen`] and
    /// [`Self::should_apply_freeze_damage`] for what a driver reads it through.
    pub frozen_ticks: u32,
    /// `LivingEntity.autoSpinAttackTicks` — the riptide-trident spin-attack
    /// countdown (issue #208), started at `20` by [`apply_riptide`]
    /// (`TridentItem.releaseUsing`: `player.startAutoSpinAttack(20, 8.0F,
    /// itemStack)`, `TridentItem.java:100`) and decremented by [`tick`] every
    /// tick thereafter (`LivingEntity.java:3158-3159`), unconditionally — no
    /// `!flying` gate, matching vanilla. `> 0` is
    /// `LivingEntity.isAutoSpinAttack()` ([`Self::is_auto_spin_attack`]), which
    /// [`crate::pose::desired_pose`] reads to select [`Pose::SpinAttack`].
    ///
    /// **Not modelled**: the attack-damage side (`autoSpinAttackDmg`,
    /// `autoSpinAttackItemStack`, `checkAutoSpinAttack`'s entity-hit sweep) —
    /// this crate applies no damage anywhere, matching [`Self::frozen_ticks`]'s
    /// scope note.
    pub auto_spin_attack_ticks: u32,
}

impl PlayerState {
    /// Constructs a state standing at `position` facing `yaw`.
    #[must_use]
    pub fn at(position: Vec3d, yaw: f32) -> Self {
        Self {
            position,
            velocity: Vec3d::ZERO,
            yaw,
            pitch: 0.0,
            on_ground: false,
            horizontal_collision: false,
            no_jump_delay: 0,
            // Vanilla's `LocalPlayer` field defaults to `true` and is read from
            // Options when the player is created (`LocalPlayer.java:152, 299`).
            auto_jump_enabled: true,
            auto_jump_time: 0,
            sprinting: false,
            fall_flying: false,
            effects: StatusEffects::default(),
            movement_speed: None,
            stuck_speed_multiplier: Vec3d::ZERO,
            pose: Pose::Standing,
            eye_height: DEFAULT_EYE_HEIGHT,
            eye_in_water: false,
            eye_in_lava: false,
            swimming: false,
            swim_amount: 0.0,
            swim_amount_o: 0.0,
            water_movement_efficiency: 0.0,
            flying: false,
            // `Abilities.DEFAULT_FLYING_SPEED`. Seeded to vanilla's default rather
            // than `0.0` so that a driver which sets `flying` but forgets to
            // forward the server's speed flies at vanilla's default rate instead
            // of being unable to move at all — a wrong-but-plausible failure is
            // far easier to see than a silently frozen one.
            flying_speed: 0.05,
            fall_distance: 0.0,
            frozen_ticks: 0,
            auto_spin_attack_ticks: 0,
        }
    }

    /// Returns a copy of this state with creative flight engaged or released, and
    /// the server-reported `Abilities.flyingSpeed` it flies at.
    ///
    /// `flying_speed` is ignored while `flying` is `false` — vanilla's
    /// `Player.getFlyingSpeed()` takes its non-flying arm then — but it is stored
    /// regardless so a toggle does not have to re-supply it.
    #[must_use]
    pub fn with_flight(mut self, flying: bool, flying_speed: f32) -> Self {
        self.flying = flying;
        self.flying_speed = flying_speed;
        self
    }

    /// `Entity.resetFallDistance()` (`Entity.java:2910-2912`) — zeroes
    /// [`Self::fall_distance`].
    ///
    /// Every reset condition this crate's own tick reaches (landing, water,
    /// climbable, Slow Falling/Levitation, a stuck-in-block match) is applied
    /// internally already; this is for the sites that are the *driver's*
    /// responsibility because this crate has no primitive of its own for them —
    /// chiefly a teleport (server correction, respawn, an ender pearl or chorus
    /// fruit landing) that snaps [`Self::position`] outside of [`tick`]. See the
    /// "Not modelled" list on [`Self::fall_distance`].
    pub fn reset_fall_distance(&mut self) {
        self.fall_distance = 0.0;
    }

    /// `Entity.getTicksRequiredToFreeze()` (`Entity.java:2838-2840`) /
    /// `Entity.BASE_TICKS_REQUIRED_TO_FREEZE` (`Entity.java:203`) — `140`, and
    /// unconditionally so for a player: `LivingEntity` does not override the
    /// getter, and vanilla only ever varies it per living-entity subclass (none
    /// of which this crate models).
    pub const TICKS_REQUIRED_TO_FREEZE: u32 = 140;

    /// `Entity.isFullyFrozen()` (`Entity.java:2834-2836`):
    /// `getTicksFrozen() >= getTicksRequiredToFreeze()`.
    #[must_use]
    pub fn is_fully_frozen(&self) -> bool {
        self.frozen_ticks >= Self::TICKS_REQUIRED_TO_FREEZE
    }

    /// `Entity.getPercentFrozen()` (`Entity.java:2829-2832`):
    /// `min(ticksFrozen, ticksToFreeze) / ticksToFreeze`, as an `f32` — the
    /// freezing-overlay vignette's `0..1` ramp. The `min` is redundant given
    /// [`Self::frozen_ticks`] is already capped at [`Self::TICKS_REQUIRED_TO_FREEZE`]
    /// by [`tick`], but kept to mirror the vanilla expression exactly rather than
    /// rely on that invariant holding for every future writer of the field.
    #[must_use]
    pub fn percent_frozen(&self) -> f32 {
        (self.frozen_ticks.min(Self::TICKS_REQUIRED_TO_FREEZE) as f32)
            / (Self::TICKS_REQUIRED_TO_FREEZE as f32)
    }

    /// `LivingEntity.aiStep`'s freeze-damage trigger
    /// (`LivingEntity.java:3147-3149`): `tickCount % 40 == 0 && isFullyFrozen()
    /// && canFreeze()`. `canFreeze()` is unconditionally `true` for a player —
    /// see [`Self::frozen_ticks`]'s "not modelled" note — so this reduces to the
    /// two terms this crate can actually answer, plus the one input it cannot
    /// own: `tick_count` is vanilla's `Entity.tickCount`, an absolute count
    /// against the world's clock that this crate has no field for (every timer
    /// it owns is a countdown). Pass the driver's own tick counter; this
    /// function applies no damage itself — see [`Self::frozen_ticks`]'s doc for
    /// why the amount and its application are out of this crate's scope
    /// entirely.
    #[must_use]
    pub fn should_apply_freeze_damage(&self, tick_count: u64) -> bool {
        tick_count % 40 == 0 && self.is_fully_frozen()
    }

    /// `LivingEntity.isAutoSpinAttack()` (`LivingEntity.java:3278-3280`):
    /// `autoSpinAttackTicks > 0`.
    #[must_use]
    pub fn is_auto_spin_attack(&self) -> bool {
        self.auto_spin_attack_ticks > 0
    }

    /// Returns a copy of this state with the
    /// [`WATER_MOVEMENT_EFFICIENCY`](Self::water_movement_efficiency) attribute
    /// value (Depth Strider) injected.
    #[must_use]
    pub fn with_water_movement_efficiency(mut self, value: f32) -> Self {
        self.water_movement_efficiency = value;
        self
    }

    /// Returns a copy of this state with the pose [`eye height`](Self::eye_height)
    /// set (e.g. `0.4` for the swimming/crawling pose, `1.62` standing).
    ///
    /// Prefer [`Self::with_pose`], which sets the box and the eye together. This
    /// setter survives for the poses [`crate::pose`] does not model and for
    /// drivers that never call [`tick`]; a [`tick`] will overwrite it.
    #[must_use]
    pub fn with_eye_height(mut self, eye_height: f32) -> Self {
        self.eye_height = eye_height;
        self
    }

    /// Returns a copy of this state seeded with `pose`, setting the derived
    /// [`eye height`](Self::eye_height) to match — the pair that vanilla's
    /// `refreshDimensions` always writes together.
    ///
    /// This seeds an *initial* pose. [`tick`] re-decides it every tick through the
    /// fit gate ([`crate::pose::update_player_pose`]), so this is not a way to
    /// hold a pose the world does not admit.
    #[must_use]
    pub fn with_pose(mut self, pose: Pose) -> Self {
        self.pose = pose;
        self.eye_height = pose.eye_height();
        self
    }

    /// The hitbox dimensions for this state's [`pose`](Self::pose) —
    /// `Entity.getDimensions(getPose())` for an `Avatar`.
    ///
    /// This is what the collision sweep is handed, and the whole reason the pose
    /// exists: `0.6 × 1.8` standing, `0.6 × 1.5` crouching, `0.6 × 0.6` swimming
    /// or gliding. `step_height` is pose-independent (the `STEP_HEIGHT`
    /// attribute).
    #[must_use]
    pub fn dimensions(&self) -> EntityDimensions {
        self.pose.dimensions()
    }

    /// Returns a copy of this state with the given status effects applied.
    #[must_use]
    pub fn with_effects(mut self, effects: StatusEffects) -> Self {
        self.effects = effects;
        self
    }

    /// Returns a copy of this state with the entity layer's effective
    /// `MOVEMENT_SPEED` attribute value injected (see [`Self::movement_speed`]).
    #[must_use]
    pub fn with_movement_speed(mut self, value: f64) -> Self {
        self.movement_speed = Some(value);
        self
    }

    /// Returns a copy of this state with the Auto-Jump option engaged or
    /// disabled (see [`Self::auto_jump_enabled`]).
    #[must_use]
    pub fn with_auto_jump(mut self, enabled: bool) -> Self {
        self.auto_jump_enabled = enabled;
        self
    }

    /// The player's bounding box at its current position, **in its current
    /// pose** — `Entity.getBoundingBox()`.
    ///
    /// The hitbox is per-entity data ([`EntityDimensions`]), not version data, so
    /// it does not come from the profile. The `profile` parameter is retained (as
    /// `_profile`) purely for source compatibility with existing callers; it is
    /// unused, and a caller may drop the argument once its call sites are updated.
    ///
    /// Since `makeBoundingBox` anchors `minY` at the feet, a pose change moves
    /// only the top face (and, for poses this crate does not model, the width).
    #[must_use]
    pub fn bounding_box(&self, _profile: &PhysicsProfile) -> Aabb {
        self.dimensions().bounding_box(self.position)
    }
}

/// `getSpeed()` for a player: the `MOVEMENT_SPEED` attribute cast to `float`.
///
/// Walking is the base `0.1F` (widened to `double` when stored, then cast back
/// to `float`); sprinting applies the `+0.3` `ADD_MULTIPLIED_TOTAL` modifier in
/// `double` before the final `float` cast. Reproduced exactly here.
#[must_use]
fn player_speed(profile: &PhysicsProfile, sprinting: bool) -> f32 {
    let base = f64::from(profile.base_movement_speed); // 0.1F widened
    if sprinting {
        (base * (1.0 + f64::from(profile.sprint_speed_modifier))) as f32
    } else {
        base as f32
    }
}

/// Client-side `LocalPlayer.modifyInput` for the modern (1.21+) input pipeline.
///
/// **Version note:** the square-movement normalization
/// (`modifyInputSpeedForSquareMovement`) is a *structural* difference between
/// modern and legacy clients, not a scalar — see [`PhysicsProfile`] docs. This
/// implements the modern form; a 1.8 client would use a different mapping.
#[must_use]
fn modify_input(
    model: InputModel,
    strafe: f32,
    forward: f32,
    using_item: Option<UseEffects>,
    sneak: bool,
    sneak_factor: f32,
) -> (f32, f32) {
    match model {
        InputModel::UnitSquareProjection => {
            modify_input_unit_square(strafe, forward, using_item, sneak, sneak_factor)
        }
        // Structural seam: 1.8 used `moveFlying` (normalise by max(1, magnitude),
        // no unit-square projection). Deliberately not modelled yet — failing
        // loudly here is correct, because silently running the modern transform
        // would produce wrong-but-plausible 1.8 movement. Blocked on the 1.8
        // client restructure + a 1.8 JVM oracle.
        InputModel::LegacyMoveFlying => {
            unimplemented!("1.8 moveFlying input pipeline is not implemented yet")
        }
    }
}

fn modify_input_unit_square(
    strafe: f32,
    forward: f32,
    using_item: Option<UseEffects>,
    sneak: bool,
    sneak_factor: f32,
) -> (f32, f32) {
    if strafe * strafe + forward * forward == 0.0 {
        return (strafe, forward);
    }
    let mut sx = strafe * 0.98;
    let mut sy = forward * 0.98;
    // `LocalPlayer.modifyInput`: `if (isUsingItem() && !isPassenger()) newInput
    // = newInput.scale(itemUseSpeedMultiplier())` — between the `0.98` scale
    // above and the sneak scale below. `!isPassenger()` is dropped: this crate
    // has no riding state (see `PlayerState::on_ground`'s own note on the same
    // gap).
    if let Some(effects) = using_item {
        sx *= effects.speed_multiplier;
        sy *= effects.speed_multiplier;
    }
    if sneak {
        sx *= sneak_factor;
        sy *= sneak_factor;
    }
    // modifyInputSpeedForSquareMovement
    let length = (sx * sx + sy * sy).sqrt();
    if length <= 0.0 {
        return (sx, sy);
    }
    let dir_x = sx / length;
    let dir_y = sy / length;
    let ax = dir_x.abs();
    let ay = dir_y.abs();
    let tan = if ay > ax { ax / ay } else { ay / ax };
    let dist_to_unit_square = (1.0 + tan * tan).sqrt();
    let modified_length = (length * dist_to_unit_square).min(1.0);
    (dir_x * modified_length, dir_y * modified_length)
}

/// `Entity.getInputVector(relativeInput, speed, yRot)` — the yaw-rotated,
/// speed-scaled acceleration that `moveRelative` adds to velocity.
///
/// This is entity-agnostic and public so a mob loop can produce its per-tick
/// velocity *contribution* the same way the player pipeline does — vanilla drives
/// both players and mobs through `moveRelative`, and re-deriving this yaw rotation
/// by hand is a divergence surface (the `Mth.sin`/`Mth.cos` LUT and the exact
/// `f32`/`f64` widths must match). Feed the result (plus gravity) into
/// [`crate::entity::move_entity`] as `motion.velocity`.
///
/// Scope: `strafe`/`forward` are the horizontal relative-movement axes (vanilla's
/// `xxa`/`zza`); the vertical relative component is fixed at `0`, which covers
/// walking mobs. A fully-general `moveRelative(Vec3)` with a vertical input (a
/// swimming or flying mob) can be added when one is wired.
pub fn input_vector(strafe: f32, forward: f32, speed: f32, yaw: f32) -> Vec3d {
    let input = Vec3d::new(f64::from(strafe), 0.0, f64::from(forward));
    let length_sqr = input.length_sqr();
    if length_sqr < 1.0E-7 {
        return Vec3d::ZERO;
    }
    let scaled = if length_sqr > 1.0 {
        input.normalize()
    } else {
        input
    }
    .scale(f64::from(speed));
    let rad = yaw * (core::f32::consts::PI / 180.0);
    let sin = f64::from(mth::sin(f64::from(rad)));
    let cos = f64::from(mth::cos(f64::from(rad)));
    Vec3d::new(
        scaled.x * cos - scaled.z * sin,
        scaled.y,
        scaled.z * cos + scaled.x * sin,
    )
}

/// `Entity.getBlockPosBelowThatAffectsMyMovement()` → `getOnPos(0.500001F)`.
///
/// For the common case (no fence/wall special-casing) this is the block at
/// `(floor(x), floor(y - 0.500001), floor(z))`.
pub(crate) fn friction_block(position: Vec3d) -> (i32, i32, i32) {
    let x = mth::floor(position.x);
    let y = mth::floor(position.y - f64::from(0.500001f32));
    let z = mth::floor(position.z);
    (x, y, z)
}

/// The player's per-tick call into the shared entity move core
/// ([`move_entity`]). Restricted, as vanilla's `Entity.move(MoverType.SELF, …)`
/// is, to the parts that affect a player's reported position: collide, commit
/// position, update collision flags, run `restituteMovementAfterCollisions`, and
/// apply the block speed factor.
///
/// This is a thin wrapper: it lifts the player's motion into an [`EntityMotion`],
/// supplies the player's current-pose hitbox ([`PlayerState::dimensions`]) and a
/// [`MoveContext`] (Slow Falling, and `suppress_bounce` = the player sneaking,
/// which both zeroes the base entity restitution and vetoes the block-bounce
/// branch), runs the shared core, and writes the result back. A mob loop would
/// call [`move_entity`] directly with its own dimensions and velocity — the
/// arithmetic is identical, which is the whole point of the shared core.
///
/// `suppress_bounce` (`isSuppressingBounce()`) and `staying_on_ground_surface`
/// (`isStayingOnGroundSurface()`) are **both** `isShiftKeyDown()` in vanilla, but
/// they are separate parameters here on purpose: they are separate virtual methods
/// serving unrelated rules, our elytra path already disagrees about the first
/// (passing `false`), and collapsing them would make the edge back-off silently
/// inherit whatever that path decided about bouncing.
fn do_move(
    state: &mut PlayerState,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
    suppress_bounce: bool,
    staying_on_ground_surface: bool,
    nearby: &[crate::push::NearbyEntity],
) {
    let mut motion = EntityMotion {
        position: state.position,
        velocity: state.velocity,
        on_ground: state.on_ground,
        horizontal_collision: state.horizontal_collision,
        stuck_speed_multiplier: state.stuck_speed_multiplier,
    };
    let ctx = MoveContext {
        slow_falling: state.effects.slow_falling,
        suppress_bounce,
        // The same two `Player` overrides `tick_air`'s `AirTravelContext` applies,
        // kept in step deliberately: this is the fluid/elytra move wrapper, and a
        // gate that held on one path and not the other would be a divergence
        // between travel modes rather than against vanilla.
        edge_back_off: if state.flying {
            EdgeBackOff::Entity
        } else {
            EdgeBackOff::Player {
                staying_on_ground_surface,
                fall_distance: state.fall_distance,
            }
        },
        // `Player.getBlockSpeedFactor()`: `!flying && !isFallFlying()`. The elytra
        // half is what this reaches in practice — `do_move`'s fluid callers are
        // unreachable while flying, because the dispatch suppressor sends a flying
        // player to `tick_air` instead.
        suppress_block_speed_factor: state.flying || state.fall_flying,
    };
    move_entity_with_nearby(&mut motion, state.dimensions(), view, profile, ctx, nearby);
    state.position = motion.position;
    state.velocity = motion.velocity;
    state.on_ground = motion.on_ground;
    state.horizontal_collision = motion.horizontal_collision;
    state.stuck_speed_multiplier = motion.stuck_speed_multiplier;
}

/// `Entity.restituteMovementAfterCollisions` — the post-collision velocity
/// rewrite that zeroes blocked axes and produces slime/bed bounces.
///
/// `current` is the pre-collision velocity (`deltaMovement`, still `== delta`
/// here); `resolved` is the movement actually achieved. A `LivingEntity` has
/// zero base bounciness, so horizontal wall bounces never happen for a player;
/// the only live branch is the vertical land-bounce off a bouncy block.
#[allow(clippy::too_many_arguments)]
pub(crate) fn restitute_movement_after_collisions(
    current: Vec3d,
    resolved: Vec3d,
    x_collision: bool,
    z_collision: bool,
    vertical_collision: bool,
    vertical_collision_below: bool,
    position: Vec3d,
    slow_falling: bool,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
    suppress_bounce: bool,
) -> Vec3d {
    // restitution starts at getEntityBounciness() (0.0 for a player), or 0 while
    // sneaking.
    let mut restitution: f64 = 0.0;
    let mut vx = current.x;
    let mut vy = current.y;
    let mut vz = current.z;
    if x_collision {
        vx = -current.x * restitution;
    }
    if z_collision {
        vz = -current.z * restitution;
    }

    if vertical_collision {
        if vertical_collision_below {
            // Block at getOnPosLegacy() == getOnPos(0.2), from the post-move pos.
            let ex = mth::floor(position.x);
            let ey = mth::floor(position.y - f64::from(0.2f32));
            let ez = mth::floor(position.z);
            let block_bounciness = f64::from(view.bounce_restitution(ex, ey, ez));
            let effective_gravity =
                effective_gravity(f64::from(profile.gravity), current.y <= 0.0, slow_falling);
            // `!(-current.y < effGravity)`: only a fast-enough landing bounces (a
            // resting entity does not jitter). Kept as vanilla's negated `<` rather
            // than `>=` so the NaN edge matches its float expression exactly.
            #[allow(clippy::neg_cmp_op_on_partial_ord)]
            let fast_enough = !(-current.y < effective_gravity);
            restitution = if fast_enough && !suppress_bounce {
                restitution.max(block_bounciness)
            } else {
                0.0
            };
        }

        let (gravity_compensation, effective_drag) = if restitution > 0.0 {
            let portion_with_movement = resolved.y / current.y;
            let effective_gravity =
                effective_gravity(f64::from(profile.gravity), current.y <= 0.0, slow_falling);
            (
                portion_with_movement * effective_gravity,
                mth::lerp_f64(portion_with_movement, 1.0, f64::from(0.98f32)),
            )
        } else {
            (0.0, 1.0)
        };
        vy = (gravity_compensation - current.y) * effective_drag * restitution;
    }

    Vec3d::new(vx, vy, vz)
}

/// `Mth.equal(a, b)` → `Math.abs(b - a) < 1.0E-5F`.
pub(crate) fn mth_equal(a: f64, b: f64) -> bool {
    (b - a).abs() < f64::from(1.0e-5f32)
}

/// Which `maybeBackOffFromEdge` override the entity being moved has.
///
/// `Entity.maybeBackOffFromEdge(Vec3, MoverType)` (`Entity.java:1099-1101`) is a
/// virtual hook whose **base implementation is the identity** — `return delta`.
/// `Player` (`Player.java:880-927`) is the only override in the tree: it is the
/// sneak-at-a-ledge back-off that stops you walking off a drop while shift is
/// held. Modelling the override as a variant rather than a bare `bool` makes a
/// mob *structurally* unable to acquire player-only behaviour, which is the same
/// property vanilla gets from the class hierarchy.
///
/// [`Self::Entity`] is the [`Default`], so [`crate::MoveContext::default()`] — what
/// a mob or a dropped item passes — is inert by construction.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum EdgeBackOff {
    /// `Entity.maybeBackOffFromEdge` — the base implementation, `return delta`.
    /// Every non-player entity.
    #[default]
    Entity,
    /// `Player.maybeBackOffFromEdge` — the sneak-at-a-ledge back-off.
    Player {
        /// `Player.isStayingOnGroundSurface()` (`Player.java:300-302`), which is
        /// exactly `isShiftKeyDown()` — the raw shift key, not the crouch *pose*
        /// and not `isCrouching()`.
        ///
        /// The two ends of the wire read the same boolean from different places
        /// and it matters that they agree: the client uses
        /// `LocalPlayer.isShiftKeyDown()`, overridden to return
        /// `this.input.keyPresses.shift()` directly
        /// (`client-src/.../LocalPlayer.java:674-676`), while the server reads the
        /// `SHIFT_KEY_DOWN` shared flag set from `ServerboundPlayerInputPacket`
        /// (`ServerGamePacketListenerImpl.java:427`). A driver that sneaks
        /// *locally* without sending the input packet manufactures the very
        /// disagreement this rule exists to prevent, only inverted — see
        /// `docs/edge-back-off.md`.
        staying_on_ground_surface: bool,
        /// `Entity.fallDistance` (`Entity.java:245`, a `double` since 26.2).
        ///
        /// **An input this crate does not maintain**, like
        /// [`PlayerState::water_movement_efficiency`]. It is read by exactly one
        /// place — `Player.isAboveGround`'s airborne branch — and only when
        /// `on_ground` is `false`.
        fall_distance: f64,
    },
}

/// `Player.maybeBackOffFromEdge(Vec3, MoverType)` (`Player.java:880-927`) — the
/// sneak-at-a-ledge back-off, called from inside `Entity.move` on the *candidate*
/// delta before collision resolution.
///
/// # Why this is a desync rule and not a feel rule
///
/// `ServerGamePacketListenerImpl.handleMovePlayer` replays the movement we claim
/// through `this.player.move(MoverType.PLAYER, …)`
/// (`ServerGamePacketListenerImpl.java:1134`) — and `MoverType.PLAYER` is one of
/// the two mover types this rule's own gate admits. It then compares the replay's
/// result against the position we claimed and teleports us back if
/// `movedDist > 0.0625` (`:1146-1153`), i.e. **0.25 blocks in a single packet with
/// no accumulator**. Because the intervening `yDist` clamp zeroes Y
/// unconditionally (`:1137-1139`), that comparison is **purely horizontal** — and
/// this rule modifies exactly and only the horizontal components. A client that
/// sneaks near a ledge without modelling it claims a position past where the
/// server's own replay puts it, and gets corrected.
///
/// # The gate, transcribed
///
/// ```text
/// !this.abilities.flying
///   && !(delta.y > 0.0)
///   && (moverType == MoverType.SELF || moverType == MoverType.PLAYER)
///   && this.isStayingOnGroundSurface()
///   && this.isAboveGround(maxDownStep)
/// ```
///
/// Two of the five are satisfied by construction here and so are not parameters:
///
/// * **`!abilities.flying`** — this crate does not model creative flight at all
///   (`travelInAir` applies gravity unconditionally), so a flying driver does not
///   route through [`tick`]. This is the same standing argument
///   [`update_swimming`] makes for `Player.updateSwimming`'s flying override.
/// * **the mover type** — [`crate::move_entity`] *is* `move(MoverType.SELF, …)`.
///   The excluded types are `PISTON` (which this crate has no equivalent of) and
///   the mover types used for vehicle/knockback pushes.
///
/// `maxDownStep` is `this.maxUpStep()`, i.e. the resolved **`STEP_HEIGHT`
/// attribute** (`LivingEntity.java:3975`), whose `RangedAttribute` default is
/// `0.6` (`Attributes.java:98-100`) — *not* a literal. It arrives as
/// [`EntityDimensions::step_height`], which is documented as the post-modifier
/// value, so a step-height modifier is honoured for free. Hard-coding `0.6` would
/// agree today and silently diverge the moment anything modifies the attribute.
///
/// # The stepping loop
///
/// Vanilla does **not** clamp the delta once; it walks it toward zero in `0.05`
/// increments, re-probing after each step, and stops at the first candidate the
/// probe says will not fall. There are **three** loops, and X and Z are
/// *independent first, then joint*:
///
/// 1. X alone, probing `canFallAtLeast(deltaX, 0.0, …)`.
/// 2. Z alone, probing `canFallAtLeast(0.0, deltaZ, …)`, starting from the
///    original `delta.z` (**not** from anything loop 1 produced).
/// 3. X and Z **together**, probing `canFallAtLeast(deltaX, deltaZ, …)` and
///    stepping both, starting from whatever loops 1 and 2 left behind.
///
/// The third loop is what handles an outside corner, where neither the pure-X nor
/// the pure-Z move leaves the ledge but the diagonal does. It also differs
/// structurally from the first two: those `break` the instant a component is
/// zeroed, whereas the diagonal loop zeroes one component and keeps stepping the
/// other in the same iteration, exiting only when the `!= 0.0` guards fail.
///
/// The step magnitude is fixed at `Math.signum(delta) * 0.05` **computed once**,
/// before any stepping, so it never changes sign mid-loop. `Y` is passed through
/// untouched.
///
/// # What the caller must know
///
/// This rewrites the *local* candidate delta only. Vanilla never calls
/// `setDeltaMovement` here, so `getDeltaMovement()` — the vector
/// `restituteMovementAfterCollisions` later reads — keeps its **un-backed-off**
/// value. A player pressed against a ledge by the back-off therefore keeps
/// accumulating horizontal velocity: the back-off is invisible to
/// `xCollision`/`zCollision` too, because those compare against the *backed-off*
/// delta (`Entity.java:766-767`), so a fully-cancelled component reads as "no
/// collision" and is never zeroed. That is vanilla behaviour and it is observable
/// the moment you release shift.
#[must_use]
pub(crate) fn maybe_back_off_from_edge(
    delta: Vec3d,
    back_off: EdgeBackOff,
    bounding_box: Aabb,
    on_ground: bool,
    step_height: f32,
    view: &dyn CollisionView,
) -> Vec3d {
    let EdgeBackOff::Player {
        staying_on_ground_surface,
        fall_distance,
    } = back_off
    else {
        return delta;
    };

    // `float maxDownStep = this.maxUpStep();` — kept at `f32` width, and widened
    // at each use exactly where vanilla's implicit promotion happens.
    let max_down_step = step_height;

    // Vanilla's gate, in vanilla's order and shape: the whole body sits inside one
    // `if`, and the method falls through to `return delta` when it does not hold.
    // `!(delta.y > 0.0)` is kept as written rather than folded to `delta.y <= 0.0`
    // so a NaN Y takes the same branch it does in Java.
    #[allow(
        clippy::neg_cmp_op_on_partial_ord,
        reason = "transcribed from `!(delta.y > 0.0)`; the NaN branch differs from `<=`"
    )]
    if !(delta.y > 0.0)
        && staying_on_ground_surface
        && is_above_ground(bounding_box, on_ground, fall_distance, max_down_step, view)
    {
        let mut delta_x = delta.x;
        let mut delta_z = delta.z;
        // `Math.signum(…) * 0.05`, computed **once** so the step cannot change
        // sign mid-loop. Java's `signum`, not Rust's — see [`mth::java_signum`].
        let step_x = mth::java_signum(delta_x) * 0.05;
        let step_z = mth::java_signum(delta_z) * 0.05;
        let min_height = f64::from(max_down_step);

        // Loop 1: X alone.
        while delta_x != 0.0 && can_fall_at_least(bounding_box, delta_x, 0.0, min_height, view) {
            if delta_x.abs() <= 0.05 {
                delta_x = 0.0;
                break;
            }
            delta_x -= step_x;
        }

        // Loop 2: Z alone, from the *original* `delta.z`.
        while delta_z != 0.0 && can_fall_at_least(bounding_box, 0.0, delta_z, min_height, view) {
            if delta_z.abs() <= 0.05 {
                delta_z = 0.0;
                break;
            }
            delta_z -= step_z;
        }

        // Loop 3: both together — the outside-corner case. No `break`: one
        // component may be zeroed while the other keeps stepping in the same
        // iteration, and the `!= 0.0` guards are what end it.
        while delta_x != 0.0
            && delta_z != 0.0
            && can_fall_at_least(bounding_box, delta_x, delta_z, min_height, view)
        {
            if delta_x.abs() <= 0.05 {
                delta_x = 0.0;
            } else {
                delta_x -= step_x;
            }

            if delta_z.abs() <= 0.05 {
                delta_z = 0.0;
            } else {
                delta_z -= step_z;
            }
        }

        return Vec3d::new(delta_x, delta.y, delta_z);
    }

    delta
}

/// `Player.isAboveGround(float)` (`Player.java:931-933`).
///
/// ```text
/// this.onGround() || this.fallDistance < maxDownStep
///                    && !this.canFallAtLeast(0.0, 0.0, maxDownStep - this.fallDistance)
/// ```
///
/// Note the **shrinking** probe depth in the airborne branch: the further you have
/// already fallen, the less additional drop is required to disqualify the
/// back-off. So `fall_distance` errs in a known direction — supplying `0.0` for a
/// genuinely-falling entity probes the *full* step height, which is a strictly
/// weaker `canFallAtLeast`, so the gate opens *more* often than vanilla's. That
/// only reaches the airborne branch: while `on_ground` is set the value is
/// unread, and the server resets `fallDistance` to `0.0` on every grounded tick
/// (`Entity.checkFallDamage`, `Entity.java:1569-1581`), so the grounded case — the
/// entire bridging / sneak-placing use case — is exact with the default.
#[must_use]
fn is_above_ground(
    bounding_box: Aabb,
    on_ground: bool,
    fall_distance: f64,
    max_down_step: f32,
    view: &dyn CollisionView,
) -> bool {
    on_ground
        || fall_distance < f64::from(max_down_step)
            && !can_fall_at_least(
                bounding_box,
                0.0,
                0.0,
                f64::from(max_down_step) - fall_distance,
                view,
            )
}

/// `Player.canFallAtLeast(double, double, double)` (`Player.java:935-950`) —
/// whether a box offset by `(dx, dz)` and hanging `min_height` below the feet
/// would meet nothing.
///
/// The probe box is **not** uniformly shrunk, and getting this wrong is the
/// difference between stopping at a ledge and stopping a whole box-width early:
///
/// ```text
/// minX + 1.0E-7 + deltaX  ..  maxX - 1.0E-7 + deltaX   // inset on both sides
/// minY - minHeight - 1.0E-7  ..  minY                  // grown *downward*, top at the feet
/// minZ + 1.0E-7 + deltaZ  ..  maxZ - 1.0E-7 + deltaZ   // inset on both sides
/// ```
///
/// Horizontally it is inset (so merely *touching* a neighbouring column does not
/// count); vertically the bottom is pushed `1e-7` further **down** — an expansion,
/// not a shrink — while the top sits exactly at the feet plane. Because overlap is
/// the strict `min < max` test, the block you are standing on still registers
/// (its top face is at `minY`, and `minY - h - 1e-7 < minY` holds), which is what
/// makes standing on solid ground report "cannot fall".
///
/// The consequence for the caller is that this is a **whole-box** test: it only
/// reports true once the entire inset footprint clears every collider. A sneaking
/// player therefore walks until their box is flush with the ledge and their
/// footprint has left the supporting block, not until their centre crosses it.
///
/// **Scope.** Vanilla calls `level.noCollision(this, box)`, which is
/// `noBlockCollision && noEntityCollision && noBorderCollision`
/// (`CollisionGetter.java:51-53`). [`no_collision`] is the **block half only** —
/// this crate has no entity list and no world border — so a box that vanilla would
/// consider blocked by another entity or by the border reads as free here. Same
/// documented limitation as the fluid hop-out check.
#[must_use]
fn can_fall_at_least(
    bounding_box: Aabb,
    delta_x: f64,
    delta_z: f64,
    min_height: f64,
    view: &dyn CollisionView,
) -> bool {
    // Left-to-right association preserved: `(min + 1e-7) + delta`, and
    // `(minY - minHeight) - 1e-7`.
    no_collision(
        view,
        Aabb::new(
            bounding_box.min_x + 1.0e-7 + delta_x,
            bounding_box.min_y - min_height - 1.0e-7,
            bounding_box.min_z + 1.0e-7 + delta_z,
            bounding_box.max_x - 1.0e-7 + delta_x,
            bounding_box.min_y,
            bounding_box.max_z - 1.0e-7 + delta_z,
        ),
    )
}

/// `LivingEntity.jumpFromGround()` including the sprint boost.
/// `TridentItem.releaseUsing`'s riptide impulse (`TridentItem.java:88-104`),
/// issue #208.
///
/// # What this function is, and is not, responsible for
///
/// Vanilla reaches this arithmetic only after three gates this function does
/// **not** evaluate, because none of them is physics state:
/// `EnchantmentHelper.getTridentSpinAttackStrength(itemStack, player) > 0.0F`
/// (equipment/enchantment data), `timeHeld >= THROW_THRESHOLD_TIME` (`10` ticks
/// — item-use duration, owned by the interaction layer), and
/// `player.isInWaterOrRain()` (fluid presence **or** weather — this crate's
/// [`CollisionView::is_water`] answers the first half but has no weather
/// concept for the second). A driver must evaluate all three itself and call
/// this only once, on the release edge, with `strength` already resolved to
/// the enchantment's value (`> 0.0`).
///
/// What *is* physics, and what this function does:
///
/// 1. **The impulse.** `xd/yd/zd` from yaw/pitch via the sine table
///    (`Mth.sin`/`Mth.cos`, bit-exact — see [`mth`]), normalised and scaled by
///    `strength`, added to velocity via `Entity.push` (a plain add,
///    `Entity.java:1919-1924`).
/// 2. **The spin-attack state.** `startAutoSpinAttack(20, 8.0F, itemStack)`
///    reduces, for this crate, to setting
///    [`PlayerState::auto_spin_attack_ticks`] to `20` — the damage/held-item
///    half is not modelled (see that field's doc).
/// 3. **The on-ground pop-up.** `if (player.onGround()) player.move(SELF, (0,
///    1.1999999, 0))` — a real collision-resolving move (so a riptide launched
///    under a low ceiling stops at the ceiling), reproduced via
///    [`move_entity`] with a synthetic, zeroed `stuck_speed_multiplier`: this
///    is a one-off supplementary move outside the tick's own, so nothing
///    tick-pending should be consumed by it, and vanilla's raw `move()` call
///    here is likewise independent of `travel()`'s own stuck-multiplier
///    handling.
pub fn apply_riptide(
    state: &mut PlayerState,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
    strength: f32,
) {
    let yaw_rad = f64::from(state.yaw) * (core::f64::consts::PI / 180.0);
    let pitch_rad = f64::from(state.pitch) * (core::f64::consts::PI / 180.0);
    let mut xd = -mth::sin(yaw_rad) * mth::cos(pitch_rad);
    let mut yd = -mth::sin(pitch_rad);
    let mut zd = mth::cos(yaw_rad) * mth::cos(pitch_rad);
    let dist = (xd * xd + yd * yd + zd * zd).sqrt();
    xd *= strength / dist;
    yd *= strength / dist;
    zd *= strength / dist;
    state.velocity = state
        .velocity
        .add(Vec3d::new(f64::from(xd), f64::from(yd), f64::from(zd)));

    state.auto_spin_attack_ticks = 20;

    if state.on_ground {
        let mut motion = EntityMotion {
            position: state.position,
            velocity: Vec3d::new(0.0, 1.199_999_9, 0.0),
            on_ground: state.on_ground,
            horizontal_collision: state.horizontal_collision,
            stuck_speed_multiplier: Vec3d::ZERO,
        };
        move_entity(
            &mut motion,
            state.dimensions(),
            view,
            profile,
            MoveContext::default(),
        );
        state.position = motion.position;
    }
}

/// The strength `apply_riptide` wants for one level of Riptide, straight out of
/// the enchantment's own data file.
///
/// `data/minecraft/enchantment/riptide.json` declares
/// `minecraft:trident_spin_attack_strength` as `{"type": "minecraft:add",
/// "value": {"type": "minecraft:linear", "base": 1.5,
/// "per_level_above_first": 0.75}}`, and `EnchantmentHelper.
/// getTridentSpinAttackStrength` sums that effect over the stack's
/// enchantments — so a single Riptide N contributes exactly
/// `1.5 + 0.75 * (N - 1)`: **1.5 / 2.25 / 3.0** for I / II / III.
///
/// Read from the data file rather than from a recollected constant: the
/// per-level term is `0.75`, not the `0.5` a `1.5, 2.0, 2.5` ladder would
/// imply, and the difference at Riptide III is a full block per tick.
///
/// `level == 0` (no Riptide) is `0.0`, which is exactly the `> 0.0F` test
/// `TridentItem.releaseUsing` gates the whole launch on.
#[must_use]
pub fn riptide_spin_attack_strength(level: u32) -> f32 {
    if level == 0 {
        return 0.0;
    }
    1.5f32 + 0.75f32 * (level as f32 - 1.0)
}

/// `LivingEntity.canGlide()` (`LivingEntity.java:3205-3217`) with `Player`'s
/// override (`Player.java:1428-1430`), issue #206.
///
/// `!flying && !onGround && !isPassenger && !hasEffect(LEVITATION)` plus "some
/// equipment slot holds a glider". The last conjunct is **not** physics state —
/// vanilla walks `EquipmentSlot.VALUES` looking for a `DataComponents.GLIDER`
/// component — so the caller resolves it and passes the answer in, the same
/// division of labour [`apply_riptide`] uses for the enchantment gate.
///
/// This engine has no riding state on [`PlayerState`], so the `!isPassenger()`
/// conjunct is the caller's too (the ECS driver holds it as a component); it is
/// vacuous here.
#[must_use]
pub fn can_glide(state: &PlayerState, glider_equipped: bool) -> bool {
    !state.flying
        && !state.on_ground
        && state.effects.levitation.is_none()
        && glider_equipped
}

/// `Player.tryToStartFallFlying()` (`Player.java:1463-1474`), issue #206 — the
/// **client-predicted** start of an elytra glide.
///
/// ```text
/// if (!this.isFallFlying() && this.canGlide() && !this.isInWater()) {
///    this.startFallFlying();   // setSharedFlag(7, true)
///    return true;
/// }
/// ```
///
/// Returns whether the glide started, which is exactly the bit
/// `LocalPlayer.aiStep` turns into one `START_FALL_FLYING` player command
/// (`LocalPlayer.java:850-852`) — so a driver should send that command if and
/// only if this returned `true`.
///
/// **The start is client-authoritative and the stop is not.** Vanilla's client
/// sets the shared flag itself here and only then tells the server; the *end* of
/// a glide is decided server-side by `LivingEntity.updateFallFlying`'s
/// `!canGlide()` branch (`LivingEntity.java:3183-3189`) and synced back as
/// entity data. A client that models the start but not the stop glides forever
/// once it lands, so a driver must also run [`update_fall_flying`] each tick.
pub fn try_start_fall_flying(
    state: &mut PlayerState,
    glider_equipped: bool,
    in_water: bool,
) -> bool {
    if state.fall_flying || !can_glide(state, glider_equipped) || in_water {
        return false;
    }
    state.fall_flying = true;
    true
}

/// The half of `LivingEntity.updateFallFlying()` that ends a glide
/// (`LivingEntity.java:3183-3189`), issue #206.
///
/// ```text
/// if (!this.level().isClientSide()) {
///    if (!this.canGlide()) {
///       this.setSharedFlag(7, false);
/// ```
///
/// Vanilla runs this **server-side only** and syncs the cleared flag; we run it
/// on the client because this client has no server that tracks glide state
/// (issue #206's residue names exactly that gap). Predicting the stop locally is
/// the safe direction: the dominant trigger is `onGround`, and a client that
/// keeps `fall_flying` set after touching down routes every subsequent tick
/// through [`tick_elytra`] and can never walk again.
///
/// The elytra-durability and `ELYTRA_GLIDE` game-event halves of vanilla's
/// method are deliberately not modelled — both are `!isClientSide` effects on
/// server-owned item state.
pub fn update_fall_flying(state: &mut PlayerState, glider_equipped: bool) {
    if state.fall_flying && !can_glide(state, glider_equipped) {
        state.fall_flying = false;
    }
}

fn jump_from_ground(state: &mut PlayerState, view: &dyn CollisionView, profile: &PhysicsProfile) {
    // getJumpPower(): JUMP_STRENGTH * multiplier(1) * getBlockJumpFactor() + getJumpBoostPower().
    // The block-jump-factor product and the boost are separate terms in one
    // float expression; honey reduces the former (0.5), Jump Boost adds the latter.
    let block_jump_factor = block_jump_factor(state.position, view);
    let jump_power =
        profile.jump_power * block_jump_factor + jump_boost_power(state.effects.jump_boost);
    if jump_power <= 1.0e-5 {
        return;
    }
    let v = state.velocity;
    state.velocity = Vec3d::new(v.x, f64::from(jump_power).max(v.y), v.z);
    if state.sprinting {
        let angle = state.yaw * (core::f32::consts::PI / 180.0);
        let boost = profile.sprint_jump_boost;
        state.velocity = state.velocity.add(Vec3d::new(
            f64::from(-mth::sin(f64::from(angle))) * boost,
            0.0,
            f64::from(mth::cos(f64::from(angle))) * boost,
        ));
    }
}

/// `LivingEntity.getJumpBoostPower()` — `0.1F * (amp + 1)` as a `float`, or `0`.
fn jump_boost_power(jump_boost: Option<u32>) -> f32 {
    match jump_boost {
        Some(amp) => 0.1f32 * (amp as f32 + 1.0f32),
        None => 0.0f32,
    }
}

/// `Entity.getBlockJumpFactor()`: the jump factor of the block at the feet, or
/// the block below when the feet block is neutral (`== 1.0`). Honey is `0.5`.
fn block_jump_factor(position: Vec3d, view: &dyn CollisionView) -> f32 {
    let here_x = mth::floor(position.x);
    let here_y = mth::floor(position.y);
    let here_z = mth::floor(position.z);
    let here = view.jump_factor(here_x, here_y, here_z);
    if here == 1.0 {
        let (bx, by, bz) = friction_block(position);
        view.jump_factor(bx, by, bz)
    } else {
        here
    }
}

/// `LocalPlayer.canAutoJump()` (`LocalPlayer.java:1117-1125`) — the gate before
/// any auto-jump detection.
///
/// `isStayingOnGroundSurface()` is just `isShiftKeyDown()`
/// (`Player.java:300-302`), so a sneaking player never auto-jumps. `isMoving()`
/// reads the *input* move-vector, not the actual movement — the detector still
/// fires for a standing player pressing forward into a step. This engine has no
/// riding state, so the `!isPassenger()` conjunct is vacuous.
fn can_auto_jump(state: &PlayerState, input: MovementInput, view: &dyn CollisionView) -> bool {
    let (mv_x, mv_y) = normalized_move_vector(input);
    state.auto_jump_enabled
        && state.auto_jump_time <= 0
        && state.on_ground
        && !input.sneak
        && mv_x * mv_x + mv_y * mv_y > 0.0
        && block_jump_factor(state.position, view) >= 1.0
}

/// `LocalPlayer.updateAutoJump(float, float)` (`LocalPlayer.java:1001-1097`) —
/// the client-side auto-jump detector. Called at the end of `move()` with the
/// **actual** x/z deltas the sweep produced (post-collision, cast to `float`).
///
/// When it decides the player is about to walk into a stepable obstacle (rise
/// strictly greater than `0.5` and at most the jump height, clear headroom, and
/// the movement generally facing the player) it arms
/// [`PlayerState::auto_jump_time`], which the next tick's prologue spends as a
/// forced jump — the one-tick deferral `LocalPlayer` spells
/// `this.input.makeJump()` (`LocalPlayer.java:784-789`).
///
/// This is **client-only** steering: the server never sees the detector, only
/// the resulting jump, so an exact port matters for *feel* (and for the
/// reference JVM oracle), not for anti-cheat.
fn update_auto_jump(
    state: &mut PlayerState,
    input: MovementInput,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
    xa: f32,
    za: f32,
) {
    if !can_auto_jump(state, input, view) {
        return;
    }

    // `position()` at this point is the post-move feet position (move() has
    // completed); the look-ahead is anchored there.
    let move_begin = state.position;
    let mut move_diff = Vec3d::new(f64::from(xa), 0.0, f64::from(za));
    let move_end = move_begin.add(move_diff);
    let current_speed = effective_speed(profile, state);
    let mut move_dist_sq = move_diff.length_sqr() as f32;

    // The player barely moved this tick (standing still, or fully blocked): fall
    // back to the *intended* input vector — `input.getMoveVector()` (the
    // normalised strafe/forward) rotated into world space by the yaw.
    if move_dist_sq <= 0.001 {
        let (mv_x, mv_y) = normalized_move_vector(input);
        let input_xa = current_speed * mv_x;
        let input_za = current_speed * mv_y;
        let angle = state.yaw * (core::f32::consts::PI / 180.0);
        let sin = mth::sin(f64::from(angle));
        let cos = mth::cos(f64::from(angle));
        move_diff = Vec3d::new(
            f64::from(input_xa * cos - input_za * sin),
            move_diff.y,
            f64::from(input_za * cos + input_xa * sin),
        );
        move_dist_sq = move_diff.length_sqr() as f32;
        if move_dist_sq <= 0.001 {
            return;
        }
    }

    // `Mth.invSqrt(moveDistSq)` — JOML's fast inverse sqrt, not `1/sqrt`.
    let move_dist_inverted = mth::inv_sqrt_f32(move_dist_sq);
    let move_dir = move_diff.scale(f64::from(move_dist_inverted));

    // `Entity.getForward()` — `Vec3.directionFromRotation(xRot, yRot)`
    // (`Entity.java:2603-2605`, `Vec3.java:267-273`).
    let facing = forward_direction(state.pitch, state.yaw);
    let facing_dot = (facing.x * move_dir.x + facing.z * move_dir.z) as f32;
    if facing_dot < -0.15 {
        return;
    }

    // `BlockPos.containing(getX(), boundingBox.maxY, getZ())` and the cell one
    // above: both must be free of collision for the jump to have headroom.
    let bb = state.bounding_box(profile);
    let cx = mth::floor(state.position.x);
    let cz = mth::floor(state.position.z);
    let mut ceiling_y = mth::floor(bb.max_y);
    if !block_has_no_collision(view, cx, ceiling_y, cz)
        || !block_has_no_collision(view, cx, ceiling_y + 1, cz)
    {
        return;
    }

    // `1.2F` plus `(amplifier + 1) * 0.75F` per Jump Boost level.
    let mut jump_height = 1.2f32;
    if let Some(amp) = state.effects.jump_boost {
        jump_height += (amp as f32 + 1.0) * 0.75;
    }

    // `Math.max(currentSpeed * 7.0F, 1.0F / moveDistInverted)` — both floats
    // widened to double for the max; the result stays a double.
    let look_ahead_dist =
        f64::from(current_speed * 7.0f32).max(f64::from(1.0f32 / move_dist_inverted));

    let seg_begin = move_begin;
    let seg_end = move_end.add(move_dir.scale(look_ahead_dist));
    let dims = state.dimensions();
    let player_width = dims.width;
    let player_height = dims.height;

    // `new AABB(segBegin, segEnd + (0, playerHeight, 0)).inflate(playerWidth, 0, playerWidth)`
    // — the constructor normalises min/max per coordinate (`AABB.java:22-29`).
    let far = seg_end.add(Vec3d::new(0.0, f64::from(player_height), 0.0));
    let test_box = Aabb::new(
        move_begin.x.min(far.x) - f64::from(player_width),
        move_begin.y.min(far.y),
        move_begin.z.min(far.z) - f64::from(player_width),
        move_begin.x.max(far.x) + f64::from(player_width),
        move_begin.y.max(far.y),
        move_begin.z.max(far.z) + f64::from(player_width),
    );

    // The probe segments are the swept line raised by `0.51F` (just above a
    // stair's low face, so a low step is *seen* but its near face is not
    // mistaken for a tall wall), offset left/right by `rightDir = moveDir × up`,
    // scaled by `width * 0.5F`.
    let seg_begin = seg_begin.add(Vec3d::new(0.0, f64::from(0.51f32), 0.0));
    let seg_end = seg_end.add(Vec3d::new(0.0, f64::from(0.51f32), 0.0));
    let right_dir = Vec3d::new(-move_dir.z, 0.0, move_dir.x);
    let right_offset = right_dir.scale(f64::from(player_width * 0.5));
    let left_begin = seg_begin.subtract(right_offset);
    let left_end = seg_end.subtract(right_offset);
    let right_begin = seg_begin.add(right_offset);
    let right_end = seg_end.add(right_offset);

    // `level().getCollisions(this, testBox)` — every block collision box the
    // swept box touches, over `BlockCollisions`' cursor range
    // (`Mth.floor(box ± 1.0E-7) ∓ 1 .. floor(box ± 1.0E-7) ± 1`,
    // `BlockCollisions.java:51-58`). A box only counts if it intersects either
    // probe segment.
    let mut boxes: Vec<Aabb> = Vec::new();
    let x0 = mth::floor(test_box.min_x - 1.0e-7) - 1;
    let x1 = mth::floor(test_box.max_x + 1.0e-7) + 1;
    let y0 = mth::floor(test_box.min_y - 1.0e-7) - 1;
    let y1 = mth::floor(test_box.max_y + 1.0e-7) + 1;
    let z0 = mth::floor(test_box.min_z - 1.0e-7) - 1;
    let z1 = mth::floor(test_box.max_z + 1.0e-7) + 1;
    for x in x0..=x1 {
        for y in y0..=y1 {
            for z in z0..=z1 {
                view.collision_boxes(x, y, z, &mut boxes);
            }
        }
    }

    // Java's `Float.MIN_VALUE` sentinel is the smallest *positive* subnormal
    // (`1.0E-45`), not the most-negative float — Rust's `f32::MIN` is the wrong
    // sentinel and would compare unequal to every real height.
    let no_obstacle = f32::from_bits(1);
    let mut obstacle_height = no_obstacle;

    for box_ in &boxes {
        if segment_intersects(box_, left_begin, left_end)
            || segment_intersects(box_, right_begin, right_end)
        {
            obstacle_height = box_.max_y as f32;
            // `box.getCenter()` is `Mth.lerp(0.5, min, max)` per axis
            // (`AABB.java:447-449`); `BlockPos.containing` floors it.
            let center = Vec3d::new(
                mth::lerp_f64(0.5, box_.min_x, box_.max_x),
                mth::lerp_f64(0.5, box_.min_y, box_.max_y),
                mth::lerp_f64(0.5, box_.min_z, box_.max_z),
            );
            let obstacle_x = mth::floor(center.x);
            let obstacle_y = mth::floor(center.y);
            let obstacle_z = mth::floor(center.z);

            // `for (int steps = 1; steps < jumpHeight; steps++)` — an int
            // compared against a float, so the body runs once for the default
            // `1.2` and twice only when Jump Boost pushes the height past `2`.
            for steps in 1..(jump_height.ceil() as i32) {
                let above_y = obstacle_y + steps;
                if !block_has_no_collision(view, obstacle_x, above_y, obstacle_z) {
                    // `(float)shape.max(Y) + abovePos.getY()` — the block-local
                    // top (uncapped) plus the block's own y, in float math.
                    obstacle_height =
                        view.collision_top(obstacle_x, above_y, obstacle_z) as f32 + above_y as f32;
                    if f64::from(obstacle_height) - state.position.y > f64::from(jump_height) {
                        return;
                    }
                }
                if steps > 1 {
                    ceiling_y += 1;
                    if !block_has_no_collision(view, cx, ceiling_y, cz) {
                        return;
                    }
                }
            }
            break;
        }
    }

    if obstacle_height != no_obstacle {
        // `(float)(obstacleHeight - getY())` then the double-sided gate
        // `!(ydelta <= 0.5F) && !(ydelta > jumpHeight)`.
        let ydelta = (f64::from(obstacle_height) - state.position.y) as f32;
        if ydelta > 0.5 && ydelta <= jump_height {
            state.auto_jump_time = 1;
        }
    }
}

/// `KeyboardInput`'s `moveVector` — `Vec2(leftImpulse, forwardImpulse)`
/// normalised with `Mth.sqrt` and a `1.0E-4F` zero-threshold
/// (`Vec2.normalized()`, `Vec2.java:59-62`). `x` is the strafe axis and `y` the
/// forward axis, matching `MovementInput`'s `strafe`/`forward`.
fn normalized_move_vector(input: MovementInput) -> (f32, f32) {
    let dist = (input.strafe * input.strafe + input.forward * input.forward).sqrt();
    if dist < 1.0e-4f32 {
        (0.0, 0.0)
    } else {
        (input.strafe / dist, input.forward / dist)
    }
}

/// `Entity.getForward()` — `Vec3.directionFromRotation(xRot, yRot)`
/// (`Entity.java:2603-2605`, `Vec3.java:267-273`). Only `x` and `z` are read by
/// the auto-jump dot product, but the whole expression is reproduced so the
/// bits match the reference — the `-PI` yaw term and the `-cos(pitch)` x scale
/// are real float arithmetic, not simplifiable.
fn forward_direction(pitch: f32, yaw: f32) -> Vec3d {
    let y_arg = -yaw * (core::f32::consts::PI / 180.0) - core::f32::consts::PI;
    let x_arg = -pitch * (core::f32::consts::PI / 180.0);
    let y_cos = mth::cos(f64::from(y_arg));
    let y_sin = mth::sin(f64::from(y_arg));
    let x_cos = -mth::cos(f64::from(x_arg));
    let x_sin = mth::sin(f64::from(x_arg));
    Vec3d::new(
        f64::from(y_sin * x_cos),
        f64::from(x_sin),
        f64::from(y_cos * x_cos),
    )
}

/// `BlockState.getCollisionShape(...).isEmpty()` for one cell — true when the
/// block appends no collision boxes (air, most plants).
fn block_has_no_collision(view: &dyn CollisionView, x: i32, y: i32, z: i32) -> bool {
    let mut boxes = Vec::new();
    view.collision_boxes(x, y, z, &mut boxes);
    boxes.is_empty()
}

/// `AABB.intersects(Vec3, Vec3)` (`AABB.java:249-253`) — the min/max of each
/// coordinate pair, then the strict overlap test (a flush contact is *not* an
/// intersection).
fn segment_intersects(box_: &Aabb, first: Vec3d, second: Vec3d) -> bool {
    let min_x = first.x.min(second.x);
    let min_y = first.y.min(second.y);
    let min_z = first.z.min(second.z);
    let max_x = first.x.max(second.x);
    let max_y = first.y.max(second.y);
    let max_z = first.z.max(second.z);
    box_.min_x < max_x
        && box_.max_x > min_x
        && box_.min_y < max_y
        && box_.max_y > min_y
        && box_.min_z < max_z
        && box_.max_z > min_z
}

/// `LivingEntity.aiStep`'s `noJumpDelay` countdown, run at the top of every
/// travel path before the velocity snap-to-zero.
fn decrement_no_jump_delay(state: &mut PlayerState) {
    if state.no_jump_delay > 0 {
        state.no_jump_delay -= 1;
    }
}

/// `aiStep`'s velocity snap-to-zero prologue: the horizontal components
/// collapse to zero below `9.0e-6` (a squared distance) and the vertical
/// component collapses below `0.003`. Byte-identical across every travel path
/// (air, water, lava, elytra), so it is factored out once rather than
/// reproduced per path.
fn snap_small_velocity(v: Vec3d) -> Vec3d {
    let mut dx = v.x;
    let mut dy = v.y;
    let mut dz = v.z;
    if v.horizontal_distance_sqr() < 9.0e-6 {
        dx = 0.0;
        dz = 0.0;
    }
    if v.y.abs() < 0.003 {
        dy = 0.0;
    }
    Vec3d::new(dx, dy, dz)
}

/// The sprint-flag write plus the client-side input transform shared by the
/// air, water and lava travel paths: `state.sprinting = input.sprint` then
/// `LocalPlayer.modifyInput`.
fn set_sprint_and_modify_input(
    state: &mut PlayerState,
    input: MovementInput,
    profile: &PhysicsProfile,
) -> (f32, f32) {
    state.sprinting = input.sprint;
    modify_input(
        profile.input_model,
        input.strafe,
        input.forward,
        input.using_item,
        input.sneak,
        profile.sneaking_speed,
    )
}

/// Advances the player by exactly one tick of on-land (non-fluid) movement.
///
/// Fluid, ladder, and elytra handling live in dedicated entry points; this is
/// the common walking/sprinting/jumping/falling path that dominates real play.
pub fn tick_air(
    state: &mut PlayerState,
    input: MovementInput,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
) {
    tick_air_among_entities(state, input, view, profile, &[]);
}

fn tick_air_among_entities(
    state: &mut PlayerState,
    input: MovementInput,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
    nearby: &[crate::push::NearbyEntity],
) {
    // `LocalPlayer.aiStep`'s auto-jump spend (`LocalPlayer.java:784-789`): a
    // decision made by [`update_auto_jump`] at the end of the *previous* tick's
    // move is spent here as a forced jump, one tick later, via the same
    // `this.input.makeJump()` deferral vanilla uses. The forced flag flows
    // through the ordinary jump block below (and into `AirTravelContext`'s
    // `jumping`, which is exactly what `makeJump` does in vanilla).
    let mut input = input;
    if state.auto_jump_time > 0 {
        state.auto_jump_time -= 1;
        input.jump = true;
    }

    // --- aiStep prologue: velocity snap-to-zero -------------------------------
    decrement_no_jump_delay(state);
    state.velocity = snap_small_velocity(state.velocity);

    // --- input transformation (client-side) -----------------------------------
    let (xxa, zza) = set_sprint_and_modify_input(state, input, profile);

    // --- jump -----------------------------------------------------------------
    // `LivingEntity.aiStep`'s jump block is `if (this.jumping &&
    // this.isAffectedByFluids())` (`LivingEntity.java:3089`) — and
    // `Player.isAffectedByFluids()` is `!abilities.flying`. So a **flying player
    // never jumps from the ground at all**; the `else` arm runs and clears
    // `noJumpDelay`.
    //
    // This conjunct is easy to miss because `isAffectedByFluids` reads like it
    // should only guard the *fluid* sub-branches, and because vanilla's own
    // creative-flight feel is unaffected in the common case (you are rarely
    // `onGround` while flying). It matters at ground level: without it, holding
    // jump while hovering just above the floor would add `jumpFromGround`'s `0.42`
    // *on top of* the `flyingSpeed * 3` vertical impulse the driver applies, and
    // launch the player at three times vanilla's climb rate.
    //
    // The one-shot hop vanilla *does* perform when flight is **engaged** while
    // standing (`LocalPlayer.aiStep`'s `if (abilities.flying && this.onGround())
    // this.jumpFromGround();`) is the driver's, not this function's — it fires on
    // the toggle edge, before `super.aiStep()`.
    let affected_by_fluids = !state.flying;
    if affected_by_fluids && input.jump && state.on_ground && state.no_jump_delay == 0 {
        jump_from_ground(state, view, profile);
        state.no_jump_delay = 10;
    } else if !affected_by_fluids || !input.jump {
        state.no_jump_delay = 0;
    }

    // `LivingEntity.handleOnClimbable`'s `resetFallDistance()` — evaluated once,
    // pre-move, and reused (`travel_in_air` below re-derives the same `climbing`
    // test for its own velocity clamp; both read the pre-move position, so the two
    // checks agree). Only `travelInAir` reaches `handleOnClimbable`
    // (`LivingEntity.java:2666-2669`), so this is `tick_air`-only, matching vanilla.
    //
    // `Player.onClimbable()` is `abilities.flying ? false : super.onClimbable()`
    // (`Player.java:2025`), so flight detaches the player from ladders entirely —
    // both this reset and `travel_in_air`'s clamp/steady-climb.
    if on_climbable(state, view) {
        state.fall_distance = 0.0;
    }

    // --- travelInAir ----------------------------------------------------------
    // The gravity + drag + collision core is the entity-agnostic `travel_in_air`
    // seam (shared with mobs); the player supplies only the transformed input,
    // `getSpeed()`, and its per-situation flags. Thread the player's motion state
    // through `EntityMotion` and back so the arithmetic is byte-identical.
    let old_x = state.position.x;
    let old_y = state.position.y;
    let old_z = state.position.z;
    let mut motion = EntityMotion {
        position: state.position,
        velocity: state.velocity,
        on_ground: state.on_ground,
        horizontal_collision: state.horizontal_collision,
        stuck_speed_multiplier: state.stuck_speed_multiplier,
    };
    let ctx = AirTravelContext {
        yaw: state.yaw,
        jumping: input.jump,
        levitation: state.effects.levitation,
        slow_falling: state.effects.slow_falling,
        suppress_ladder_slide: input.sneak,
        suppress_bounce: input.sneak,
        omnidirectional_air_mover: false,
        discard_friction: false,
        // `Player.maybeBackOffFromEdge` — a player always has the override; the
        // shift key and the fall distance are what decide whether it does anything.
        //
        // …except while flying: the override's own first conjunct is
        // `!this.abilities.flying` (`Player.java:882`), so a flying player gets the
        // *base* implementation, which is the identity. This is the conjunct
        // `EdgeBackOff`'s doc records as "satisfied by construction here" because
        // this crate had no flight — that argument has now expired, so the gate is
        // real rather than vacuous, and a sneaking player can fly off a ledge.
        edge_back_off: if state.flying {
            EdgeBackOff::Entity
        } else {
            EdgeBackOff::Player {
                staying_on_ground_surface: input.sneak,
                fall_distance: state.fall_distance,
            }
        },
        // `Player.getFlyingSpeed()` — sprint- and flight-dependent, so it cannot be
        // a profile constant. This is also what fixes the non-flying sprint-jump
        // case, which was reading a flat `0.02`.
        flying_speed: Some(player_flying_speed(state, profile)),
        // `Player.getBlockSpeedFactor()` (`Player.java:1855`) — suppressed while
        // flying **or** gliding.
        suppress_block_speed_factor: state.flying || state.fall_flying,
        // `Player.onClimbable()` (`Player.java:2025`).
        suppress_climbable: state.flying,
    };
    travel_in_air_among_entities(
        &mut motion,
        state.dimensions(),
        (xxa, zza),
        effective_speed(profile, state),
        ctx,
        view,
        profile,
        nearby,
    );
    state.position = motion.position;
    state.velocity = motion.velocity;
    state.on_ground = motion.on_ground;
    state.horizontal_collision = motion.horizontal_collision;
    state.stuck_speed_multiplier = motion.stuck_speed_multiplier;

    // `Entity.move()`'s `checkFallDamage(movement.y, onGround, …)` call
    // (`Entity.java:783-784`) — not in water on this path (see
    // `accumulate_fall_distance`'s doc).
    accumulate_fall_distance(state, state.position.y - old_y, false);

    // `LocalPlayer.move()` calls `updateAutoJump(deltaX, deltaZ)` after
    // `super.move()` (`LocalPlayer.java:981-990`) — the detector sees the
    // *actual* post-collision delta (cast to `float`), not the pre-collision
    // intent, and arms the one-tick-deferred jump spent at the top of the next
    // tick. This is a client-only steering effect: the server only ever sees the
    // resulting jump, whose velocity the crate already reproduces bit-exactly.
    update_auto_jump(
        state,
        input,
        view,
        profile,
        (state.position.x - old_x) as f32,
        (state.position.z - old_z) as f32,
    );
}

/// `LivingEntity.handleOnClimbable(Vec3)` — the pre-move clamp applied while on
/// a ladder/vine.
///
/// The clamp bounds are the **`float`** literals `-0.15F`/`0.15F`, promoted to
/// `double` for `Mth.clamp(double, double, double)`. `(double)0.15F` is
/// `0.15000000596046448`, *not* `0.15`, so the widened bound is observable at
/// the last ULP — we widen through `f32` exactly like vanilla rather than
/// writing `0.15_f64`. The sneak-hold (`yd = 0` when descending) applies to
/// ladders/vines but not scaffolding.
pub(crate) fn handle_on_climbable(delta: Vec3d, sneaking: bool) -> Vec3d {
    let bound = f64::from(0.15f32);
    let xd = mth::clamp_f64(delta.x, -bound, bound);
    let zd = mth::clamp_f64(delta.z, -bound, bound);
    let mut yd = delta.y.max(-bound);
    if yd < 0.0 && sneaking {
        yd = 0.0;
    }
    Vec3d::new(xd, yd, zd)
}

/// `LivingEntity.getEffectiveGravity()` — Slow Falling reduces gravity to
/// `min(gravity, 0.01)` while descending; otherwise it is the base gravity.
///
/// The `0.01` is a `double` literal and `min` uses the pre-move delta-Y sign
/// (`getDeltaMovement().y <= 0.0`). In fluids this is what makes the `-0.003`
/// clamp reachable: it shifts `baseGravity/16` off the `0.005` that makes the
/// clamp dead at default gravity.
#[must_use]
pub(crate) fn effective_gravity(base_gravity: f64, falling: bool, slow_falling: bool) -> f64 {
    if falling && slow_falling {
        base_gravity.min(0.01)
    } else {
        base_gravity
    }
}

/// `LivingEntity.getFluidFallingAdjustedMovement(gravity, falling, movement)`.
///
/// Applies the buoyant slow-descent: normally `y - baseGravity/16`, but when
/// already sinking near terminal it clamps to `-0.003` (the famous slow-sink).
/// When sprinting, gravity is not applied at all (vanilla returns `movement`).
#[must_use]
fn fluid_falling_adjusted_movement(
    base_gravity: f64,
    is_falling: bool,
    sprinting: bool,
    movement: Vec3d,
) -> Vec3d {
    if base_gravity != 0.0 && !sprinting {
        let gravity_step = base_gravity / 16.0;
        let yd = if is_falling
            && (movement.y - 0.005).abs() >= 0.003
            && (movement.y - gravity_step).abs() < 0.003
        {
            -0.003
        } else {
            movement.y - gravity_step
        };
        Vec3d::new(movement.x, yd, movement.z)
    } else {
        movement
    }
}

/// `Entity.getFluidJumpThreshold()` (`Entity.java:3692-3694`) —
/// `getEyeHeight() < 0.4 ? 0.0 : 0.4`.
///
/// The pose feeds back into movement here: the swimming pose's eye height is
/// **exactly** `0.4` (`Avatar.java:28`), and `0.4 < 0.4` is false, so a swimming
/// player keeps the `0.4` threshold. Only a pose shorter than that (no vanilla
/// player pose is) collapses the threshold to zero.
#[must_use]
fn fluid_jump_threshold(eye_height: f32) -> f64 {
    if eye_height < 0.4 { 0.0 } else { 0.4 }
}

/// `LivingEntity.onClimbable()` reduced to the block test this engine models: the
/// CLIMBABLE tag on the block at the feet block position.
/// `Player.onClimbable()` is `abilities.flying ? false : super.onClimbable()`
/// (`Player.java:2025`) — flight detaches the player from ladders and vines.
fn on_climbable(state: &PlayerState, view: &dyn CollisionView) -> bool {
    !state.flying && on_climbable_here(state, view)
}

#[allow(clippy::doc_markdown)]
fn on_climbable_here(state: &PlayerState, view: &dyn CollisionView) -> bool {
    view.is_climbable(
        mth::floor(state.position.x),
        mth::floor(state.position.y),
        mth::floor(state.position.z),
    )
}

/// `Entity.checkFallDamage(ya, onGround, onState, pos)`, restricted to the
/// accumulation and the grounded reset — `LivingEntity`'s override
/// (`LivingEntity.java:363-394`) adds only landing particles and a
/// server/`onChangedBlock` call before delegating to this via `super`
/// (`LivingEntity.java:390`), neither of which affects position or
/// `fallDistance` itself.
///
/// `ya` is `movement.y` from vanilla's `Entity.move()` (`Entity.java:783-784`):
/// the actual Y position delta this move achieved, *not* the pre-move velocity.
/// Callers pass `state.position.y - old_y`, captured immediately before the move.
///
/// `in_water` is vanilla's `isInWater()` (`wasTouchingWater`) — callers pass a
/// constant matching which travel path they are in (only [`tick_water`] can have
/// it `true`; the dispatch in [`travel_and_check_inside_blocks`] guarantees the
/// other three paths are only reached when it is `false`).
///
/// **That constant is an approximation, and this is the subsystem's one known
/// divergence.** `isInWater()` is *not* frozen for the tick: it reads the cached
/// `wasTouchingWater` (`Entity.java:1605-1607`), and `updateFluidInteraction`
/// rewrites that cache from **two** call sites — `Entity.baseTick`
/// (`Entity.java:537`, pre-`travel`, the one this crate's dispatch reproduces)
/// and `LivingEntity.checkFallDamage` (`LivingEntity.java:365`), which runs
/// *inside* `move()` against the **post-move** position under
/// `if (!this.isInWater())`. So on the tick a fall first enters water vanilla
/// resets mid-`move` (`Entity.java:1658-1659`) and then skips the accumulation
/// below, ending that tick at exactly `0.0`, whereas this crate is still on the
/// `tick_air` path and accumulates the descent.
///
/// The divergence is bounded to that single tick and **cannot move the player**:
/// this call happens at the *end* of the move, after
/// `Player.maybeBackOffFromEdge` has already read the old value, and the next
/// tick's dispatch re-derives the summary from the same post-move position
/// vanilla used, so [`tick_water`]'s reset lands before any gate reads it. It is
/// therefore observable only to an external reader between ticks (a future
/// fall-damage predictor). Closing it would cost a second
/// [`crate::fluid_state::compute_fluid_state`] on every air tick.
/// `tests/fall_distance.rs`'s
/// `water_entry_tick_is_the_one_known_divergence_and_it_lasts_exactly_one_tick`
/// pins both halves of that claim.
///
/// The `(float)` cast is vanilla's, not an approximation: `fallDistance` is a
/// `double` field but the tick's `ya` is truncated to `float` precision *before*
/// the subtraction (`Entity.java:1566`).
fn accumulate_fall_distance(state: &mut PlayerState, ya: f64, in_water: bool) {
    if !in_water && ya < 0.0 {
        state.fall_distance -= f64::from(ya as f32);
    }
    if state.on_ground {
        state.fall_distance = 0.0;
    }
}

/// `Entity.checkFallDistanceAccumulation()` (`Entity.java:2904-2908`) — clamps
/// `fallDistance` to at most `1.0` while not descending fast. Called only from
/// `LivingEntity.updateFallFlying`, itself only reached `if (isFallFlying())` in
/// `aiStep`, ahead of the Slow Falling/Levitation reset and `travel()`
/// (`LivingEntity.java:3117-3125`) — so this reads `state.velocity` as it stood
/// at the *end of the previous* tick, exactly as vanilla's pre-`travel()`
/// placement does.
fn check_fall_distance_accumulation(state: &mut PlayerState) {
    if state.velocity.y > -0.5 && state.fall_distance > 1.0 {
        state.fall_distance = 1.0;
    }
}

/// `LivingEntity.aiStep`'s **jump** block for an entity standing in fluid
/// (`LivingEntity.java:3088-3113`).
///
/// This is the sinking-vs-swimming decision, and it is *not* "jump means `+0.04`
/// in water". Vanilla compares the fluid's **height** against
/// [`fluid_jump_threshold`]:
///
/// * shallow enough (`onGround && !(height > threshold)`, or not in water at all)
///   ⇒ an ordinary `jumpFromGround()` — you jump out of a puddle normally;
/// * otherwise ⇒ `jumpInLiquid()`, the `+0.04F` swim-up impulse.
///
/// Modelling this needs a real fluid height, which is why the summary is passed
/// in rather than re-derived from the coarse presence booleans: with a
/// [`CollisionView::fluid_at`]-capable world the height is exact, and without one
/// a present cell reads as full (`1.0`), which is above the `0.4` threshold and so
/// lands on the swim-up branch — the pre-existing behaviour.
fn apply_fluid_jump(
    state: &mut PlayerState,
    input: MovementInput,
    fluid: &FluidState,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
) {
    if !input.jump {
        state.no_jump_delay = 0;
        return;
    }
    // `isInLava() ? getFluidHeight(LAVA) : getFluidHeight(WATER)`.
    let in_lava = fluid.in_lava();
    let fluid_height = if in_lava {
        fluid.lava_height
    } else {
        fluid.water_height
    };
    let in_water_and_has_height = fluid.in_water() && fluid_height > 0.0;
    // `getEyeHeight()` is *always* `getDimensions(getPose()).eyeHeight()` in
    // vanilla — one record, no way for the two to disagree. Read it from the pose
    // rather than from [`PlayerState::eye_height`] so an out-of-band write to that
    // field cannot make the box and the eye disagree here either.
    let threshold = fluid_jump_threshold(state.pose.eye_height());
    // The outer test is vanilla's `!(fluidHeight > threshold)` and the two inner
    // ones are its `<=` — transcribed as written rather than normalised, because the
    // two forms differ on NaN and the source is the specification here.
    #[allow(clippy::neg_cmp_op_on_partial_ord)]
    let not_above_threshold = !(fluid_height > threshold);
    // `isInShallowFluid(LAVA)`.
    let shallow_lava = fluid.lava_height <= threshold;
    let jump_in_liquid = |state: &mut PlayerState| {
        state.velocity = state.velocity.add(Vec3d::new(0.0, f64::from(0.04f32), 0.0));
    };

    if !in_water_and_has_height || (state.on_ground && not_above_threshold) {
        if in_lava && !(state.on_ground && shallow_lava) {
            jump_in_liquid(state);
        } else if (state.on_ground || (in_water_and_has_height && fluid_height <= threshold))
            && state.no_jump_delay == 0
        {
            jump_from_ground(state, view, profile);
            state.no_jump_delay = 10;
        }
    } else {
        jump_in_liquid(state);
    }
}

/// `LivingEntity.jumpOutOfFluid(oldY)` (`LivingEntity.java:2556-2561`) — the hop
/// that carries a swimmer *out* of the water onto the ledge they just swam into.
///
/// Runs at the end of both fluid travel branches. When the tick's move collided
/// horizontally and the box would be **free of blocks and of liquid** if lifted to
/// `movement.y + 0.6 - y + oldY`, vertical velocity is replaced by a flat `0.3F`.
/// Without it a player pressed against a shoreline swims into the wall forever;
/// this is the single most visible piece of water movement after buoyancy.
///
/// `isFree` is `noCollision(box) && !containsAnyLiquid(box)` (`Entity.java:664-670`)
/// — the liquid half is what stops it firing repeatedly while still submerged.
fn jump_out_of_fluid(
    state: &mut PlayerState,
    old_y: f64,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
) {
    if !state.horizontal_collision {
        return;
    }
    let movement = state.velocity;
    let lift = movement.y + f64::from(0.6f32) - state.position.y + old_y;
    let probe = state
        .bounding_box(profile)
        .moved(movement.x, lift, movement.z);
    let free = crate::collision::no_collision(view, probe)
        && !crate::collision::contains_any_liquid(view, probe);
    if free {
        state.velocity = Vec3d::new(movement.x, f64::from(0.3f32), movement.z);
    }
}

/// One tick of in-water movement (`travel` → `travelInFluid` → `travelInWater`,
/// `LivingEntity.java:2494-2530`), plus the in-water parts of `aiStep` that
/// precede it.
///
/// In order: the `baseTick` flow-current push, `LocalPlayer.aiStep`'s
/// sneak-to-sink (`goDownInWater`, `-0.04F`), the velocity snap-to-zero prologue,
/// the shallow-vs-deep jump decision ([`apply_fluid_jump`]), `Player.travel`'s
/// swim look-descent (blending vertical velocity toward the look angle), the
/// slow-down/input-speed terms (sprint, Depth Strider, Dolphin's Grace), the
/// collision move, the ladder clamp, the `multiply(slowDown, 0.8F, slowDown)`
/// drag, buoyancy ([`fluid_falling_adjusted_movement`]) and finally
/// [`jump_out_of_fluid`].
///
/// # Not modelled
///
/// * Bubble-column impulses are **not** here, and never were vanilla's to put
///   here: `BubbleColumnBlock.entityInside` is reached from
///   `applyEffectsFromBlocks`, which `LivingEntity.aiStep` calls *after*
///   `travel()` (`LivingEntity.java:3130` then `:3134`). They live one level up,
///   in [`apply_bubble_column`], beside the other `entityInside` effect this
///   crate models ([`update_stuck_multiplier`]). Issue #199.
/// * `WATER_MOVEMENT_EFFICIENCY` has no source in this repo — see
///   [`PlayerState::water_movement_efficiency`]. The arithmetic is here; the value
///   is `0.0`.
pub fn tick_water(
    state: &mut PlayerState,
    input: MovementInput,
    fluid: &FluidState,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
) {
    tick_water_among_entities(state, input, fluid, view, profile, &[]);
}

fn tick_water_among_entities(
    state: &mut PlayerState,
    input: MovementInput,
    fluid: &FluidState,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
    nearby: &[crate::push::NearbyEntity],
) {
    match profile.fluid_model {
        FluidModel::Modern => {}
        // Structural seam: 1.8 fluid handling is a different branch (no swimming
        // pose, no falling-adjusted clamp). Not modelled yet — fail loudly rather
        // than run modern water math for a 1.8 profile.
        FluidModel::Legacy1_8 => {
            unimplemented!("1.8 fluid movement is not implemented yet")
        }
    }
    // --- baseTick: `updateFluidInteraction`'s `if (inWater) resetFallDistance()`
    // (`Entity.java:1658-1659`) ------------------------------------------------
    // This function is only reached when the per-tick fluid summary already says
    // `in_water()` (see `travel_and_check_inside_blocks`'s dispatch), so the
    // condition is unconditionally true here — matching vanilla, where the same
    // predicate decides both the reset and the `travelInFluid` dispatch.
    state.fall_distance = 0.0;
    // --- baseTick: fluid current push (`updateFluidInteraction`) ---------------
    // Vanilla applies the flow current in `baseTick`, before `aiStep`/`travel`
    // within the same tick, so it lands here (ahead of the snap-to-zero prologue)
    // and its result is what the prologue and the accel step then see.
    apply_fluid_push(
        state,
        view,
        crate::fluid::FluidKind::Water,
        profile.water_push_scale,
        profile,
    );
    // --- LocalPlayer.aiStep: sneak to sink (`goDownInWater`, -0.04F) -----------
    // `LocalPlayer.java:855-857` runs this *before* `super.aiStep()`, so it lands
    // ahead of the snap-to-zero prologue below. (The placement is numerically
    // inert at this magnitude — `0.04` clears the `0.003` collapse from either
    // side — but it is where vanilla puts it, and a later `goDownInWater` variant
    // with a smaller impulse would not be inert.) This is the deliberate-sink half
    // of "sinking versus swimming": without it the only way down is to release
    // jump and wait for buoyancy.
    if input.sneak {
        state.velocity = state
            .velocity
            .add(Vec3d::new(0.0, -f64::from(0.04f32), 0.0));
    }
    // --- aiStep prologue: velocity snap-to-zero (identical to the air path) ----
    decrement_no_jump_delay(state);
    state.velocity = snap_small_velocity(state.velocity);

    let (xxa, zza) = set_sprint_and_modify_input(state, input, profile);

    // --- aiStep jump: shallow water jumps, deep water swims up -----------------
    apply_fluid_jump(state, input, fluid, view, profile);

    // --- Player.travel: the swim look-descent (Player.java:1401-1415) ---------
    // Runs in `Player.travel()`, which wraps `super.travel()` (`LivingEntity.
    // travel` → `travelInFluid` → `travelInWater`, i.e. the rest of this
    // function) — so it modifies `deltaMovement.y` *before* `travelInWater`'s
    // own physics ever sees it, which is why this sits ahead of the `isFalling`/
    // `oldY` capture below rather than after it.
    //
    // While swimming, blend vertical velocity toward the look direction's Y
    // component (steeper multiplier `0.085` looking notably down, `< -0.2`;
    // `0.06` otherwise) whenever looking level-or-down, holding jump, or still
    // submerged at head height — so releasing jump and looking up lets a
    // swimmer coast to the surface instead of being pulled back down, but
    // looking down (or still being underwater) glides them lower. Both
    // constants are read directly from `Player.java:1408`, not from any
    // secondhand recollection of them.
    if state.swimming {
        let look_angle_y = calculate_view_vector(state.pitch, state.yaw).y;
        let multiplier = if look_angle_y < -0.2 { 0.085 } else { 0.06 };
        // `BlockPos.containing(x, y + 1.0 - 0.1, z)` — roughly head height,
        // floored to a block position; `!isEmpty()` is "any fluid present".
        let head_submerged = view
            .fluid_at(
                mth::floor(state.position.x),
                mth::floor(state.position.y + 1.0 - 0.1),
                mth::floor(state.position.z),
            )
            .is_some();
        if look_angle_y <= 0.0 || input.jump || head_submerged {
            let vy = state.velocity.y;
            state.velocity = Vec3d::new(
                state.velocity.x,
                vy + (look_angle_y - vy) * multiplier,
                state.velocity.z,
            );
        }
    }

    // --- travelInFluid / travelInWater ----------------------------------------
    // `isFalling` and `oldY` are read at the top of `travelInFluid`, i.e. *after*
    // the jump block above has already altered velocity.
    let is_falling = state.velocity.y <= 0.0;
    let old_y = state.position.y;
    let base_gravity = effective_gravity(
        f64::from(profile.gravity),
        is_falling,
        state.effects.slow_falling,
    );

    // `LivingEntity.travelInWater`, in vanilla's order: the sprint/walk base, then
    // the Depth Strider lerp, then Dolphin's Grace *overriding* the result. The
    // order is observable — Grace wins outright, but the Strider term still moves
    // `speed` even when Grace has flattened `slowDown`.
    let mut slow_down = if state.sprinting {
        profile.water_sprint_slow_down
    } else {
        profile.water_slow_down
    };
    let mut speed = profile.fluid_input_speed;
    let mut water_walker = state.water_movement_efficiency;
    if !state.on_ground {
        water_walker *= 0.5f32;
    }
    if water_walker > 0.0 {
        slow_down += (0.546_000_06f32 - slow_down) * water_walker;
        speed += (effective_speed(profile, state) - speed) * water_walker;
    }
    if state.effects.dolphins_grace {
        slow_down = 0.96f32;
    }

    let accel = input_vector(xxa, zza, speed, state.yaw);
    state.velocity = state.velocity.add(accel);
    do_move(state, view, profile, input.sneak, input.sneak, nearby);

    // `Entity.move()`'s `checkFallDamage` call. `in_water` is `true` — the reset
    // above already zeroed `fall_distance` for this whole tick, and vanilla's own
    // `!isInWater()` guard would block any accumulation here too, so this is only
    // reachable for its grounded-reset half (e.g. touching a submerged floor).
    accumulate_fall_distance(state, state.position.y - old_y, true);

    // `if (horizontalCollision && onClimbable()) movement = (x, 0.2, z)` — a ladder
    // still lifts you while submerged, and it does so *before* the water drag.
    let mut movement = state.velocity;
    if state.horizontal_collision && on_climbable(state, view) {
        movement = Vec3d::new(movement.x, 0.2, movement.z);
    }

    let movement = movement.multiply_each(
        f64::from(slow_down),
        f64::from(0.8f32),
        f64::from(slow_down),
    );
    state.velocity =
        fluid_falling_adjusted_movement(base_gravity, is_falling, state.sprinting, movement);
    jump_out_of_fluid(state, old_y, view, profile);
}

/// One tick of movement while submerged in lava (`travelInLava`,
/// `LivingEntity.java:2539-2555`).
///
/// Lava is a *different branch* from water, not a retuned one: input speed is a
/// flat `0.02F`, and gravity is applied as an extra `-baseGravity/4` term
/// regardless of depth. What differs by depth is the post-move velocity scale:
///
/// * **deep** (`!isInShallowFluid(LAVA)`) ⇒ a flat `scale(0.5)` on all three
///   axes, with no buoyant falling-adjustment at all;
/// * **shallow** (`isInShallowFluid(LAVA)`, i.e. `lava_height <=
///   `[`fluid_jump_threshold`]) ⇒ `multiply(0.5, 0.8, 0.5)` (a *different* Y
///   factor from deep's implicit `0.5`) followed by
///   [`fluid_falling_adjusted_movement`] — the same buoyant slow-descent water
///   always gets, which deep lava never does.
///
/// The predicate and both arms were ported from the jar directly (not from a
/// summary): `isInShallowFluid` is `getFluidHeight(tag) <=
/// getFluidJumpThreshold()`, already used by [`apply_fluid_jump`] for the jump
/// decision, so this reuses the same [`FluidState::lava_height`] /
/// [`fluid_jump_threshold`] inputs rather than adding a parallel predicate.
///
/// `fallDistance` does **not** participate in this predicate or either arm —
/// `isFalling` here is `getDeltaMovement().y <= 0.0`, not a fall-distance
/// comparison, despite the "fall-distance or depth comparison" pattern this
/// file's sibling gravity code might suggest. Confirmed by reading
/// `travelInFluid`/`travelInLava`/`getFluidFallingAdjustedMovement` directly:
/// none of the three references `fallDistance`.
pub fn tick_lava(
    state: &mut PlayerState,
    input: MovementInput,
    fluid: &FluidState,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
) {
    tick_lava_among_entities(state, input, fluid, view, profile, &[]);
}

fn tick_lava_among_entities(
    state: &mut PlayerState,
    input: MovementInput,
    fluid: &FluidState,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
    nearby: &[crate::push::NearbyEntity],
) {
    match profile.fluid_model {
        FluidModel::Modern => {}
        FluidModel::Legacy1_8 => {
            unimplemented!("1.8 fluid movement is not implemented yet")
        }
    }
    // baseTick's `if (isInLava()) fallDistance *= 0.5;` (`Entity.java:555-557`).
    // This function is only reached when the per-tick fluid summary already says
    // `in_lava()`, matching vanilla's `isInLava()` predicate deciding both this
    // halving and the `travelInLava` dispatch.
    state.fall_distance *= 0.5;
    // baseTick fluid current push (see `tick_water`); lava uses its own scale.
    apply_fluid_push(
        state,
        view,
        crate::fluid::FluidKind::Lava,
        profile.lava_push_scale,
        profile,
    );
    decrement_no_jump_delay(state);
    state.velocity = snap_small_velocity(state.velocity);

    let (xxa, zza) = set_sprint_and_modify_input(state, input, profile);

    // aiStep's jump block: in *shallow* lava while on the ground you jump out
    // normally; only deep lava gets `jumpInLiquid`'s +0.04 (see `apply_fluid_jump`).
    apply_fluid_jump(state, input, fluid, view, profile);

    // `isFalling`/`baseGravity` are read here, at the top of `travelInFluid`
    // (`LivingEntity.java:2495-2497`), i.e. after the jump block above has
    // already altered velocity but before moveRelative adds this tick's input
    // acceleration. `getEffectiveGravity()` folds in the Slow Falling clamp
    // exactly as `tick_water` computes it a few lines above; lava shares the
    // same `travelInFluid` call site, so it must apply the same clamp.
    let is_falling = state.velocity.y <= 0.0;
    let base_gravity = effective_gravity(
        f64::from(profile.gravity),
        is_falling,
        state.effects.slow_falling,
    );
    let old_y = state.position.y;

    // moveRelative(0.02) → move → shallow/deep branch → -baseGravity/4.
    let accel = input_vector(xxa, zza, profile.fluid_input_speed, state.yaw);
    state.velocity = state.velocity.add(accel);
    do_move(state, view, profile, input.sneak, input.sneak, nearby);

    // `Entity.move()`'s `checkFallDamage` call. Not water on this path.
    accumulate_fall_distance(state, state.position.y - old_y, false);

    // `isInShallowFluid(LAVA)` (`LivingEntity.java:2542-2548`): shallow gets the
    // same buoyant falling-adjustment water always gets, on top of a
    // Y-asymmetric `multiply(0.5, 0.8, 0.5)`; deep gets a flat `scale(0.5)`
    // with no adjustment at all. Reuses the same threshold/height inputs
    // `apply_fluid_jump` already reads for the jump decision.
    let threshold = fluid_jump_threshold(state.pose.eye_height());
    let shallow_lava = fluid.lava_height <= threshold;
    if shallow_lava {
        let movement = state.velocity.multiply_each(0.5, f64::from(0.8f32), 0.5);
        state.velocity =
            fluid_falling_adjusted_movement(base_gravity, is_falling, state.sprinting, movement);
    } else {
        state.velocity = state.velocity.scale(0.5);
    }
    if base_gravity != 0.0 {
        state.velocity = state
            .velocity
            .add(Vec3d::new(0.0, -base_gravity / 4.0, 0.0));
    }
    jump_out_of_fluid(state, old_y, view, profile);
}

/// Advances the player by one tick, dispatching to the water, lava, or air path
/// exactly as `travel` → `shouldTravelInFluid`/`travelInFluid` does: water takes
/// precedence over lava, and both over air.
/// `Entity.calculateViewVector(xRot, yRot)` — the look-direction unit vector.
///
/// The trig comes from the **`Mth` LUT** (`float`), and each component is a
/// `float` product widened to `double` by the `Vec3` constructor. The
/// degrees→radians factor is `(float)(Math.PI / 180.0)` — the division happens
/// in `double` *then* narrows to `float`, which is a different bit pattern from
/// the input path's `(float)Math.PI / 180.0F`; we mirror the exact form.
fn calculate_view_vector(pitch: f32, yaw: f32) -> Vec3d {
    let deg_to_rad = (core::f64::consts::PI / 180.0) as f32;
    let real_x_rot = pitch * deg_to_rad;
    let real_y_rot = -yaw * deg_to_rad;
    let y_cos = mth::cos(f64::from(real_y_rot));
    let y_sin = mth::sin(f64::from(real_y_rot));
    let x_cos = mth::cos(f64::from(real_x_rot));
    let x_sin = mth::sin(f64::from(real_x_rot));
    Vec3d::new(
        f64::from(y_sin * x_cos),
        f64::from(-x_sin),
        f64::from(y_cos * x_cos),
    )
}

/// `LivingEntity.updateFallFlyingMovement(Vec3)` — the elytra glide update.
///
/// Preserves vanilla's exact operation order and its two distinct trig sources:
/// the look vector uses the `Mth` LUT (`float`), while `liftForce` and the
/// nose-up lift use `java.lang.Math` (`double`) `cos`/`sin`. The final drag is
/// `multiply(0.99F, 0.98F, 0.99F)` with each `float` widened to `double`.
fn update_fall_flying_movement(
    state: &PlayerState,
    profile: &PhysicsProfile,
    movement: Vec3d,
) -> Vec3d {
    let look = calculate_view_vector(state.pitch, state.yaw);
    let lean_angle = state.pitch * ((core::f64::consts::PI / 180.0) as f32);
    let look_hor_len = (look.x * look.x + look.z * look.z).sqrt();
    let move_hor_len = (movement.x * movement.x + movement.z * movement.z).sqrt();
    let gravity = effective_gravity(
        f64::from(profile.gravity),
        movement.y <= 0.0,
        state.effects.slow_falling,
    );
    // liftForce = Mth.square(Math.cos(leanAngle)) — real double cos, not the LUT.
    let cos_lean = f64::from(lean_angle).cos();
    let lift_force = mth::square_f64(cos_lean);

    let mut mx = movement.x;
    let mut my = movement.y;
    let mut mz = movement.z;

    my += gravity * (-1.0 + lift_force * 0.75);

    if my < 0.0 && look_hor_len > 0.0 {
        let convert = my * -0.1 * lift_force;
        mx += look.x * convert / look_hor_len;
        my += convert;
        mz += look.z * convert / look_hor_len;
    }

    if lean_angle < 0.0 && look_hor_len > 0.0 {
        // -Mth.sin(leanAngle): the LUT sine again, negated and widened.
        let convert = move_hor_len * f64::from(-mth::sin(f64::from(lean_angle))) * 0.04;
        mx += -look.x * convert / look_hor_len;
        my += convert * 3.2;
        mz += -look.z * convert / look_hor_len;
    }

    if look_hor_len > 0.0 {
        mx += (look.x / look_hor_len * move_hor_len - mx) * 0.1;
        mz += (look.z / look_hor_len * move_hor_len - mz) * 0.1;
    }

    Vec3d::new(
        mx * f64::from(0.99f32),
        my * f64::from(0.98f32),
        mz * f64::from(0.99f32),
    )
}

/// `LivingEntity.travelFallFlying` (client path) — one tick of elytra flight.
///
/// Direction comes purely from the look angle; WASD `input` is ignored while
/// gliding (except that landing on a climbable hands control back to
/// [`tick_air`] and ends the glide, mirroring vanilla). The `aiStep` small-
/// velocity collapse runs first, exactly as for the other travel modes.
pub fn tick_elytra(
    state: &mut PlayerState,
    input: MovementInput,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
) {
    tick_elytra_among_entities(state, input, view, profile, &[]);
}

fn tick_elytra_among_entities(
    state: &mut PlayerState,
    input: MovementInput,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
    nearby: &[crate::push::NearbyEntity],
) {
    // onClimbable: vanilla stops fall-flying and reverts to the walking path.
    if view.is_climbable(
        mth::floor(state.position.x),
        mth::floor(state.position.y),
        mth::floor(state.position.z),
    ) {
        state.fall_flying = false;
        tick_air_among_entities(state, input, view, profile, nearby);
        return;
    }

    decrement_no_jump_delay(state);

    // aiStep velocity collapse (players use the horizontal-distance test).
    let collapsed = snap_small_velocity(state.velocity);

    state.velocity = update_fall_flying_movement(state, profile, collapsed);
    let old_y = state.position.y;
    do_move(state, view, profile, false, input.sneak, nearby);

    // `Entity.move()`'s `checkFallDamage` call. Not water on this path.
    accumulate_fall_distance(state, state.position.y - old_y, false);
}

/// `FireworkRocketEntity.tick`'s glide-boost impulse
/// (`FireworkRocketEntity.java:122-137`), issue #206 — the elytra speed boost
/// from using a firework rocket while gliding.
///
/// # What this function is, and is not, responsible for
///
/// Vanilla applies this every tick a firework rocket entity is **attached** to
/// a fall-flying player (`attachedToEntity != null && attachedToEntity.isFallFlying()`,
/// `FireworkRocketEntity.java:122-123`). The rocket is its own entity, ticked
/// independently by the level's normal entity loop — spawning it on right-
/// click, tracking the attachment, and its `life` counter (which decides how
/// many ticks the boost lasts before the rocket explodes,
/// `FireworkRocketEntity.java:186-215`) are entity/item state this crate has
/// no model of and does not attempt here. A driver must spawn/track the
/// rocket (or an equivalent per-use counter) and call this once per tick for
/// as long as vanilla's attached rocket would still be ticking, with
/// [`PlayerState::fall_flying`] already `true` — this function does not check
/// it, the same way [`tick_elytra`] is only ever reached through its caller's
/// own `fall_flying` dispatch.
///
/// **Ordering vanilla does not pin either.** The rocket ticks as an ordinary
/// entity in the level's entity-iteration order, which is not defined relative
/// to the player's own tick — so there is no "before or after `tick_elytra`"
/// answer to reproduce; call this whenever the driver's own entity-tick order
/// places the rocket.
///
/// What *is* physics, reproduced exactly: `movement.add(lookAngle * 0.1 +
/// (lookAngle * 1.5 - movement) * 0.5)`, component-wise, all in `double`
/// (`Vec3` arithmetic — no `float` narrowing anywhere in the source line).
/// `lookAngle` is `Entity.getLookAngle()` = [`calculate_view_vector`] at the
/// player's current pitch/yaw.
pub fn apply_firework_boost(state: &mut PlayerState) {
    let look = calculate_view_vector(state.pitch, state.yaw);
    let m = state.velocity;
    state.velocity = m.add(Vec3d::new(
        look.x * 0.1 + (look.x * 1.5 - m.x) * 0.5,
        look.y * 0.1 + (look.y * 1.5 - m.y) * 0.5,
        look.z * 0.1 + (look.z * 1.5 - m.z) * 0.5,
    ));
}

/// `AABB.collidedAlongVector` (`AABB.java:401-417`), specialised to one unit
/// cell and to a **boolean** result: vanilla returns the hit point
/// (`Optional<Vec3>`), but every caller here only ever asks "did it collide" —
/// `Entity.collidedWithShapeMovingFrom` immediately reduces it to
/// `.isPresent()`-shaped logic, and that is the only vanilla caller relevant to
/// the blocks this module's swept scan cares about (every stuck-multiplier and
/// powder-snow cell presents `Shapes.block()` to `getEntityInsideCollisionShape`,
/// so `insideBlock` reduces to exactly this segment test).
///
/// `from`/`to` are the entity's **box centres** at the start and end of the
/// move — equal to `AABB.collidedAlongVector`'s own `this.getCenter()` /
/// `from.add(vector)`, since `vector = to_pos - from_pos` and the box is rigidly
/// attached to the entity position, so translating the centre by the position
/// delta is the same point as the centre of a box built at the new position.
/// `half` is the entity's own half-extents (`getXsize() * 0.5` etc.).
///
/// Implemented as the standard box-slab test rather than porting
/// `AABB.getDirection`'s face-by-face walk: the two compute the same boolean
/// (intersect / no-intersect) over the same inflated box, and no caller here
/// reads *which* face or *where* the hit point was — the one thing
/// `getDirection` computes that this does not.
fn segment_hits_cell(from: Vec3d, to: Vec3d, half: Vec3d, x: i32, y: i32, z: i32) -> bool {
    // `inflate(size * 0.5 - 1.0E-7)`.
    const EPS: f64 = 1.0e-7;
    let min_x = f64::from(x) - (half.x - EPS);
    let max_x = f64::from(x) + 1.0 + (half.x - EPS);
    let min_y = f64::from(y) - (half.y - EPS);
    let max_y = f64::from(y) + 1.0 + (half.y - EPS);
    let min_z = f64::from(z) - (half.z - EPS);
    let max_z = f64::from(z) + 1.0 + (half.z - EPS);

    let d = to.subtract(from);
    let mut t_min = 0.0f64;
    let mut t_max = 1.0f64;
    for (d_a, from_a, lo, hi) in [
        (d.x, from.x, min_x, max_x),
        (d.y, from.y, min_y, max_y),
        (d.z, from.z, min_z, max_z),
    ] {
        if d_a.abs() < EPS {
            // A stationary (or single-axis-flat) segment: the boolean test
            // degenerates to plain containment on this axis, which subsumes
            // vanilla's separate `inflated.contains(to) || inflated.contains(from)`
            // pre-check — no need to special-case `from == to` above this loop.
            if from_a < lo || from_a > hi {
                return false;
            }
        } else {
            let inv = 1.0 / d_a;
            let (t1, t2) = {
                let a = (lo - from_a) * inv;
                let b = (hi - from_a) * inv;
                if a <= b { (a, b) } else { (b, a) }
            };
            t_min = t_min.max(t1);
            t_max = t_max.min(t2);
            if t_min > t_max {
                return false;
            }
        }
    }
    true
}

/// Enumerates the cells `Entity.checkInsideBlocks(from, to, …)` visits, for
/// every consumer of that one vanilla sweep in this module
/// ([`update_stuck_multiplier`], [`update_freezing`]).
///
/// Vanilla walks a 3D DDA from `from` to `to` (`BlockGetter.forEachBlockIntersectedBetween`,
/// capped at 16 iterations) and tests each visited cell with
/// [`segment_hits_cell`]-equivalent logic. Porting the DDA itself buys nothing a
/// caller here needs — every block these two functions ask about
/// (`stuck_multiplier`, `is_powder_snow`) is a single-cell yes/no answer, order-
/// independent for the uniform volumes they occupy (see
/// [`update_stuck_multiplier`]'s "last one wins" note). So this scans the
/// **union** of the pre- and post-move bounding boxes instead: a conservative
/// superset of the cells the DDA would reach (same start box, same end box,
/// every intermediate cell along a straight segment between two boxes is
/// spatially between them), narrowed to the cells the entity's box *actually*
/// swept through via [`segment_hits_cell`] rather than accepted just for being
/// in that superset's bounds.
///
/// This is what closes the defect the resting-box-only version had: a mover
/// fast enough to pass *through* a thin stuck-in-block layer or powder-snow
/// pocket without ever resting inside it is now caught, because the swept
/// centre-to-centre segment crosses the cell even though neither endpoint box
/// does.
fn for_each_swept_cell(old_bb: Aabb, new_bb: Aabb, mut f: impl FnMut(i32, i32, i32)) {
    let half = Vec3d::new(
        (new_bb.max_x - new_bb.min_x) * 0.5,
        (new_bb.max_y - new_bb.min_y) * 0.5,
        (new_bb.max_z - new_bb.min_z) * 0.5,
    );
    let from_center = Vec3d::new(
        (old_bb.min_x + old_bb.max_x) * 0.5,
        (old_bb.min_y + old_bb.max_y) * 0.5,
        (old_bb.min_z + old_bb.max_z) * 0.5,
    );
    let to_center = Vec3d::new(
        (new_bb.min_x + new_bb.max_x) * 0.5,
        (new_bb.min_y + new_bb.max_y) * 0.5,
        (new_bb.min_z + new_bb.max_z) * 0.5,
    );
    // Same `1.0e-5` deflation as the resting-box version this replaces, and as
    // `apply_bubble_column` still uses independently — see that function's
    // "Divergences" note for why the constant is shared but the enumeration no
    // longer is.
    let min_x = mth::floor(old_bb.min_x.min(new_bb.min_x) + 1.0e-5);
    let max_x = mth::floor(old_bb.max_x.max(new_bb.max_x) - 1.0e-5);
    let min_y = mth::floor(old_bb.min_y.min(new_bb.min_y) + 1.0e-5);
    let max_y = mth::floor(old_bb.max_y.max(new_bb.max_y) - 1.0e-5);
    let min_z = mth::floor(old_bb.min_z.min(new_bb.min_z) + 1.0e-5);
    let max_z = mth::floor(old_bb.max_z.max(new_bb.max_z) - 1.0e-5);
    for x in min_x..=max_x {
        for y in min_y..=max_y {
            for z in min_z..=max_z {
                if segment_hits_cell(from_center, to_center, half, x, y, z) {
                    f(x, y, z);
                }
            }
        }
    }
}

/// `Entity.checkInsideBlocks` → `Block.entityInside` → `makeStuckInBlock`: after
/// the tick's movement, record the stuck-speed multiplier of whatever block the
/// swept movement segment intersects, for the *next* move to consume. This is
/// what produces the observable one-tick lag between entering a cobweb and being
/// grabbed by it.
///
/// **Sweeps the segment, via [`for_each_swept_cell`], rather than sampling only
/// the final resting position.** #216: the old version floored/ceiled the
/// post-move box alone, so a mover fast enough to pass through a cobweb or
/// powder-snow cell without ending inside it took no impulse at all — the
/// resting-box-only approximation the `apply_bubble_column` "Divergences" note
/// used to describe as deliberately shared between the two. It no longer is;
/// see that note.
///
/// Blocks are *assigned* in vanilla, not accumulated, so the last intersected
/// block wins; for the uniform volumes these blocks form, iteration order is
/// immaterial.
fn update_stuck_multiplier(
    state: &mut PlayerState,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
    pre_move_position: Vec3d,
) {
    let new_bb = state.bounding_box(profile);
    let old_bb = state.dimensions().bounding_box(pre_move_position);
    let mut found = Vec3d::ZERO;
    for_each_swept_cell(old_bb, new_bb, |x, y, z| {
        if let Some(m) = view.stuck_multiplier(x, y, z) {
            found = m;
        }
    });
    // `Entity.makeStuckInBlock`: `resetFallDistance(); this.stuckSpeedMultiplier =
    // speedMultiplier;` (`Entity.java:2945-2947`) — the reset rides along with
    // every call that finds a stuck-triggering block (cobweb, powder snow, sweet
    // berry bush, honey), which is every tick `Block.entityInside` sees one, not
    // just the first.
    if found != Vec3d::ZERO {
        state.fall_distance = 0.0;
    }
    state.stuck_speed_multiplier = found;
}

/// Issue #212. `LivingEntity.aiStep`'s freezing block (`LivingEntity.java:3139-3151`)
/// plus the increment half of `InsideBlockEffectType.FREEZE`
/// (`InsideBlockEffectType.java:6-11`, reached from
/// `PowderSnowBlock.entityInside`, `PowderSnowBlock.java:63-83`, via the same
/// `checkInsideBlocks` sweep [`update_stuck_multiplier`] models):
///
/// ```text
/// FREEZE effect, applied once per tick the swept segment finds powder snow:
///   setIsInPowderSnow(true);
///   if (canFreeze()) ticksFrozen = min(ticksRequiredToFreeze, ticksFrozen + 1);
///
/// end-of-tick, unconditional:
///   if (!isInPowderSnow || !canFreeze()) ticksFrozen = max(0, ticksFrozen - 2);
/// ```
///
/// Collapsed into one function because this crate has no per-tick "flag set
/// earlier, read later" bookkeeping to spare: `is_in_powder_snow` is computed
/// and consumed in the same call, which is equivalent since nothing else reads
/// it between the two vanilla call sites.
///
/// `canFreeze()` is `!is(EntityTypeTags.FREEZE_IMMUNE_ENTITY_TYPES)`
/// (`Entity.java:3886-3888`); the player entity type is never a member (only
/// certain mobs — striders, blazes, snow golems and others — are), so it is
/// unconditionally `true` for every [`PlayerState`] and not modelled as a
/// field.
///
/// **What this does not do: apply freeze damage.** Vanilla's own trigger,
/// `tickCount % 40 == 0 && isFullyFrozen() && canFreeze()`
/// (`LivingEntity.java:3147-3149`), needs an absolute server tick count this
/// crate has no notion of — every other timer here is a countdown
/// ([`PlayerState::no_jump_delay`], [`PlayerState::fall_distance`]'s resets),
/// never a count against a global clock, and this crate applies no damage
/// anywhere (fall damage is server-owned too; see [`PlayerState::fall_distance`]'s
/// doc). [`PlayerState::should_apply_freeze_damage`] exposes the *predicate* —
/// correct, tested, and ready for a driver that does own a tick counter — so
/// the only missing piece is the counter itself, not the rule.
fn update_freezing(
    state: &mut PlayerState,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
    pre_move_position: Vec3d,
) {
    let new_bb = state.bounding_box(profile);
    let old_bb = state.dimensions().bounding_box(pre_move_position);
    let mut in_powder_snow = false;
    for_each_swept_cell(old_bb, new_bb, |x, y, z| {
        if view.is_powder_snow(x, y, z) {
            in_powder_snow = true;
        }
    });
    state.frozen_ticks = if in_powder_snow {
        (state.frozen_ticks + 1).min(PlayerState::TICKS_REQUIRED_TO_FREEZE)
    } else {
        state.frozen_ticks.saturating_sub(2)
    };
}

/// `BubbleColumnBlock.entityInside` → `Entity.onInsideBubbleColumn` /
/// `onAboveBubbleColumn` (`Entity.java:2851-2898`) — the soul-sand lift and the
/// magma-block drain. Issue #199.
///
/// # The four constants, and the branch that picks between them
///
/// Vanilla decides *capped or not* per cell, in `BubbleColumnBlock.entityInside`
/// (`BubbleColumnBlock.java:56-64`): the cell **above** this one is inspected, and
/// if its collision shape is empty **and** its fluid state is empty — i.e. it is
/// open air — the entity is "above" the column and gets the stronger, surface-
/// launch pair. Anything else (more column, water, a solid lid) is the "inside"
/// pair.
///
/// | | `drag=false` (soul sand, up) | `drag=true` (magma, down) |
/// |---|---|---|
/// | inside | `min(0.7, vy + 0.06)` | `max(-0.3, vy - 0.03)` |
/// | above (air over the cell) | `min(1.8, vy + 0.1)` | `max(-0.9, vy - 0.03)` |
///
/// All four are `double` arithmetic on `Vec3.y` against `double` literals, so
/// there is no `f32` narrowing anywhere in this function.
///
/// Note the asymmetry in the drag-down column: the **step** is `-0.03` in both
/// rows and only the *clamp* differs (`-0.3` inside, `-0.9` above). It is the
/// push-up column whose step changes too (`+0.06` → `+0.1`). Transcribing this as
/// "the above case is three times stronger" would be wrong in one of the two
/// columns, which is why the table is written out rather than summarised.
///
/// # One impulse *per cell*, not per tick
///
/// This is the part that surprises. `Entity.checkInsideBlocks` visits every block
/// the movement intersects, dedupes by *position* (`visitedBlocks`), and calls
/// `entityInside` on each — and `BubbleColumnBlock` applies its impulse
/// **immediately**, inside that callback, rather than deferring it to the
/// `InsideBlockEffectApplier` the way fire and freezing do. So a standing player
/// (a `1.8`-high box) inside a column spans two cells and takes **two** impulses
/// every tick. The clamp is what keeps that from diverging: repeated
/// `min(0.7, vy + 0.06)` converges on `0.7` instead of running away. Applying the
/// impulse once per tick would reach the same terminal velocity but climb to it at
/// half the rate, which is exactly the kind of sub-tick divergence the server's
/// movement check accumulates.
///
/// # `resetFallDistance` on `inside` only
///
/// `handleOnInsideBubbleColumn` ends with `entity.resetFallDistance()`
/// (`Entity.java:2897`); `handleOnAboveBubbleColumn` ends with
/// `sendBubbleColumnParticles` and **no** reset (`:2865`). The asymmetry is
/// reproduced here faithfully, but **no test pins it, because it is currently
/// unobservable and cannot honestly be made observable.** A bubble column *is* a
/// water source cell (`BubbleColumnBlock.java:73-75`), so any player this function
/// finds in one has already been through [`tick_water`], which zeroes
/// `fall_distance` unconditionally at its top. The only world that would separate
/// the two is a bubble column in a cell that does not hold water — which vanilla
/// cannot represent, so a test fixture asserting it would be the *world* species of
/// vacuous test: green, and measuring a scene the game cannot produce. The
/// asymmetry is coded correctly so that it is already right if the classification
/// ever changes; it is deliberately unproven until something can see it.
///
/// # Divergences, deliberate
///
/// * **Cell enumeration is the post-move box, not the swept path — no longer
///   shared with [`update_stuck_multiplier`].** Vanilla walks
///   `forEachBlockIntersectedBetween(from, to, …)`, so a player moving fast enough
///   to pass *through* a column cell without ending inside it still takes that
///   cell's impulse. This function still enumerates the cells of the post-move
///   bounding box only. **This used to be "exactly the approximation
///   `update_stuck_multiplier` makes for the same vanilla call", deliberately kept
///   consistent; it no longer is.** #216 gave `update_stuck_multiplier` (and
///   [`update_freezing`]) a real swept-segment test via
///   [`for_each_swept_cell`], because a fast mover grazing a cobweb or a thin
///   powder-snow layer without resting inside it is the exact defect a resting-box
///   sample cannot see — and bubble columns were left as they were, since nothing
///   in this issue asked for them and the case this function serves (a player
///   *riding* a column, displacement at most `0.7`/tick, well under a cell) does
///   not need it. So as of #216 this is a **known, not a deliberately mirrored**,
///   approximation — worth revisiting with the same fix if a launched-through-a-
///   column case ever needs it.
/// * **The deflation constant is `1.0e-5`, not vanilla's widened `1.0E-5F`.**
///   Vanilla deflates the target box by a *float* literal, which widens to
///   `1.0000000116860974e-5`. This function uses the `f64` `1.0e-5` that
///   [`update_stuck_multiplier`] and [`for_each_swept_cell`] also use, for the
///   same reason: they all model the same vanilla sweep and must not gratuitously
///   disagree on the constant, even though the enumeration itself has now
///   diverged. The difference from vanilla's widened float can only change an
///   answer when a box edge lies within `1.2e-13` of a cell boundary.
/// * **`Player`'s `!abilities.flying` gate is not applied.** Both overrides
///   (`Player.java:310-321`) skip the impulse entirely for a flying player. This
///   crate has no abilities state, so the conjunct is vacuously true — see
///   [`tick`]'s note. A driver with a flight mode must not route a flying player
///   through [`tick`].
fn apply_bubble_column(
    state: &mut PlayerState,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
) {
    let bb = state.bounding_box(profile);
    // Same cell enumeration as `update_stuck_multiplier` — see this function's
    // "Divergences" note for why the two are deliberately identical.
    let min_x = mth::floor(bb.min_x + 1.0e-5);
    let max_x = mth::floor(bb.max_x - 1.0e-5);
    let min_y = mth::floor(bb.min_y + 1.0e-5);
    let max_y = mth::floor(bb.max_y - 1.0e-5);
    let min_z = mth::floor(bb.min_z + 1.0e-5);
    let max_z = mth::floor(bb.max_z - 1.0e-5);
    for x in min_x..=max_x {
        for y in min_y..=max_y {
            for z in min_z..=max_z {
                let Some(drag_down) = view.bubble_column(x, y, z) else {
                    continue;
                };
                let vy = state.velocity.y;
                if cell_is_open_air(view, x, y + 1, z) {
                    // `Entity.handleOnAboveBubbleColumn`. The particle burst is
                    // server-side only (`sendBubbleColumnParticles` checks
                    // `instanceof ServerLevel`) and carries no motion, so nothing
                    // is owed here; note it does *not* reset fall distance.
                    state.velocity.y = if drag_down {
                        (vy - 0.03).max(-0.9)
                    } else {
                        (vy + 0.1).min(1.8)
                    };
                } else {
                    // `Entity.handleOnInsideBubbleColumn`.
                    state.velocity.y = if drag_down {
                        (vy - 0.03).max(-0.3)
                    } else {
                        (vy + 0.06).min(0.7)
                    };
                    state.fall_distance = 0.0;
                }
            }
        }
    }
}

/// `BubbleColumnBlock.entityInside`'s `nothingAbove` test
/// (`BubbleColumnBlock.java:58`): `stateAbove.getCollisionShape(…).isEmpty() &&
/// stateAbove.getFluidState().isEmpty()`.
///
/// The fluid half is what makes the common cases fall out correctly without any
/// special-casing: a bubble column's own `getFluidState` is a **water source**
/// (`BubbleColumnBlock.java:73-75`), and this engine classifies the block as water
/// for the same reason (`docs/fluid-classification.md`'s
/// `UNCONDITIONAL_WATER_BLOCKS`), so a cell with more column above it reports
/// `false` here and takes the *inside* branch. Only the cell at the very top of a
/// column, with real air over it, reports `true`.
///
/// **One vanilla quirk deliberately not reproduced.** Vanilla calls
/// `stateAbove.getCollisionShape(level, pos)` — passing `pos`, the *lower* cell,
/// while asking the *upper* cell's state. It is a genuine mismatch in the game's
/// own source, not a transcription slip here, and it is unobservable for every
/// block whose shape does not vary with position. This seam is position-keyed and
/// cannot express the quirk anyway.
fn cell_is_open_air(view: &dyn CollisionView, x: i32, y: i32, z: i32) -> bool {
    let mut boxes = Vec::new();
    view.collision_boxes(x, y, z, &mut boxes);
    boxes.is_empty() && !view.is_water(x, y, z) && !view.is_lava(x, y, z)
}

/// Advances the player one tick: dispatches to the fluid/elytra/air travel path
/// exactly as vanilla's `LivingEntity.travel()`, records any stuck-in-block
/// multiplier for the next tick to consume (`Entity.checkInsideBlocks`), and
/// finally re-decides the **pose** through vanilla's fit gate
/// ([`crate::pose::update_player_pose`]).
///
/// The pose runs last because `Player.updatePlayerPose` is the last statement of
/// `Player.tick()` (`Player.java:284`), after `super.tick()` has done all the
/// moving. So this tick's movement used the pose decided at the end of the
/// *previous* tick, and the fit gate probes the post-move position.
pub fn tick(
    state: &mut PlayerState,
    input: MovementInput,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
) {
    travel_and_check_inside_blocks(state, input, view, profile, &[]);
    // `Player.updatePlayerPose()` with no entity snapshot: the block half of the
    // fit gate. See `tick_among_entities` for the full predicate.
    update_player_pose(state, input, view, &[]);
}

/// Everything vanilla's `super.tick()` does to a player's motion — `baseTick`'s
/// fluid/swim summary, then `travel` — up to but excluding the pose decision.
///
/// Split out so [`tick`] and [`tick_among_entities`] can share it while keeping
/// vanilla's ordering: `pushEntities` is the end of `aiStep`, *inside*
/// `super.tick()`, and therefore **before** `updatePlayerPose`. That order is
/// observable, because the push's pair test reads `getBoundingBox()` — which the
/// pose sizes.
fn travel_and_check_inside_blocks(
    state: &mut PlayerState,
    input: MovementInput,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
    nearby: &[crate::push::NearbyEntity],
) {
    // The position *before* this tick's travel dispatch moves it — the "from"
    // half of vanilla's `checkInsideBlocks(from, to, …)` segment, needed by
    // [`update_stuck_multiplier`] and [`update_freezing`] to sweep the tick's
    // movement instead of sampling only where it ends. Captured here, ahead of
    // every branch below, because every one of them can write `state.position`.
    let pre_move_position = state.position;

    // `Entity.baseTick` computes the fluid summary from the *pre-move* box, before
    // `travel` reads `isInWater`/`isInLava`. Do the same: one source of truth for
    // eye/box submersion, recorded on the state for the swimming pose and for the
    // shell's fog / overlay / ambient-sound consumers.
    //
    // **Both the box and the eye come from the pose**, never from
    // [`PlayerState::eye_height`]. They are one `EntityDimensions` record in
    // vanilla and cannot disagree; deriving both here means an out-of-band write to
    // that field cannot desynchronise them either. `tests/pose_dimensions.rs`
    // measures what the disagreement would cost: a `0.6` box with a `1.62` eye
    // reports dry eyes twenty blocks under water, because this sweep is bounded by
    // the box and never visits the eye's cell.
    let fluid = compute_fluid_state(
        state.bounding_box(profile),
        state.position,
        state.pose.eye_height(),
        view,
    );
    state.eye_in_water = fluid.eye_in_water;
    state.eye_in_lava = fluid.eye_in_lava;
    // `Player.updateSwimming` (`Player.java:1433-1439`) forces `setSwimming(false)`
    // while flying instead of delegating to `super.updateSwimming()`, so a player
    // who dives, starts sprint-swimming and then takes off drops the swim pose on
    // the very next tick rather than flying around with a `0.4` eye height.
    state.swimming = !state.flying
        && update_swimming(
            state.swimming,
            state.sprinting,
            &fluid,
            view,
            state.position,
        );
    // `LivingEntity.updateSwimAmount()` — see its doc for why this sits here,
    // between `updateSwimming` and the travel dispatch below.
    update_swim_amount(state);

    // `LivingEntity.aiStep`: `if (isFallFlying()) updateFallFlying();`
    // (`LivingEntity.java:3117-3119`), which is `checkFallDistanceAccumulation`'s
    // only call site for a player. Runs before the Slow Falling/Levitation check
    // and before `travel()`, on the velocity as it stood at the end of the
    // *previous* tick — see `check_fall_distance_accumulation`'s doc.
    if state.fall_flying {
        check_fall_distance_accumulation(state);
    }
    // `LivingEntity.aiStep`: `if (hasEffect(SLOW_FALLING) || hasEffect(LEVITATION))
    // resetFallDistance();` (`LivingEntity.java:3123-3125`), unconditionally before
    // the `travel()` dispatch below, regardless of which path it picks.
    if state.effects.slow_falling || state.effects.levitation.is_some() {
        state.fall_distance = 0.0;
    }

    // `Player.aiStep`: `if (abilities.flying && !isPassenger()) resetFallDistance();`
    // (`Player.java:449-451`) — **before** `super.aiStep()`, so before the travel
    // dispatch below, alongside the Slow Falling reset rather than after the move.
    if state.flying {
        state.fall_distance = 0.0;
    }

    // `Player.travel` (`Player.java:1416-1424`) captures `getDeltaMovement().y`
    // *before* delegating to `super.travel(input)` and then **overwrites** the
    // post-travel Y with `originalMovementY * 0.6`.
    //
    // # Why the capture reads through `snap_small_velocity`
    //
    // Vanilla's read happens inside `travel()`, which `LivingEntity.aiStep` calls
    // *after* its own velocity snap-to-zero prologue — and that prologue lives
    // inside our per-path `tick_air`/`tick_elytra`, not out here. Applying the
    // snap non-destructively to derive the capture is **exactly** vanilla's value,
    // because the only other thing aiStep writes to velocity between the snap and
    // `travel()` is `jumpFromGround()`, and that whole block is gated on
    // `isAffectedByFluids()` (`LivingEntity.java:3089`) — `!flying` for a player.
    // So while flying nothing else can intervene, and `flying` is the only case
    // this value is read in.
    //
    // Getting it wrong here is quiet rather than loud: capturing *before* the snap
    // leaves a flying player at rest with a residual Y of `1.8e-3 * 0.6^n` that
    // decays geometrically but never reaches zero, is invisible in position
    // (the snap re-zeroes it every tick before the move), and then shows up as a
    // ~0.001 offset the first time they press jump. `tests/travel_seam.rs`'s
    // `flight_y_overwrite_reads_the_post_snap_velocity` pins it.
    let original_movement_y = state.flying.then(|| snap_small_velocity(state.velocity).y);

    // `LivingEntity.travel`'s dispatch (`LivingEntity.java:2432-2444`), with
    // `shouldTravelInFluid`'s `isAffectedByFluids()` conjunct
    // (`LivingEntity.java:2436-2438`) — which `Player` overrides to `!flying`
    // (`Player.java:875-878`).
    //
    // **This suppressor is what makes flight a wrapper rather than a fourth
    // travel mode**, and it is the single strongest discriminator between the two
    // shapes: a player flying through water takes `travelInAir`, so they keep
    // moving at flight speed with no `0.8` water slow-down, no buoyancy and no
    // sneak-to-sink. Note it does **not** gate the elytra arm: `canGlide()` is
    // `!flying && super.canGlide()` (`Player.java:1429`), which stops a flying
    // player *starting* a glide, but `isFallFlying()` is a server-owned entity-data
    // bit and vanilla's dispatch still honours it if both are somehow set.
    let affected_by_fluids = !state.flying;
    if affected_by_fluids && fluid.in_water() {
        tick_water_among_entities(state, input, &fluid, view, profile, nearby);
    } else if affected_by_fluids && fluid.in_lava() {
        tick_lava_among_entities(state, input, &fluid, view, profile, nearby);
    } else if state.fall_flying {
        tick_elytra_among_entities(state, input, view, profile, nearby);
    } else {
        tick_air_among_entities(state, input, view, profile, nearby);
    }
    // The Y overwrite, closing `Player.travel`. `with(Direction.Axis.Y, …)` — the
    // X and Z the dispatch produced are kept untouched. There is **no** horizontal
    // drag term in this branch, however much "0.6" looks like one.
    if let Some(y) = original_movement_y {
        state.velocity.y = y * 0.6;
    }
    // `LivingEntity.aiStep`'s `applyEffectsFromBlocks()` (`LivingEntity.java:3134`),
    // immediately after the `travel()` dispatch above (`:3130`) and before
    // `pushEntities()` (`:3163`). Both calls below are `Block.entityInside` effects
    // reached from that one sweep, which is why they sit together here.
    //
    // Their order is unobservable: vanilla visits each cell once and a cell is
    // either a bubble column or a stuck-in-block, never both, so the two never see
    // the same cell. Beyond that they touch disjoint state — `apply_bubble_column`
    // writes `velocity.y` and `update_stuck_multiplier` writes
    // `stuck_speed_multiplier`, neither reads what the other writes, and the only
    // field they share is `fall_distance`, which both only ever set to `0.0`.
    //
    // Both are `!flying` for a player: `Player.onAboveBubbleColumn` /
    // `onInsideBubbleColumn` skip `super` entirely (`Player.java:309-321`), and
    // `Player.makeStuckInBlock` likewise (`Player.java:1514-1518`). So a flying
    // player is neither lifted by a soul-sand column nor grabbed by a cobweb.
    if !state.flying {
        apply_bubble_column(state, view, profile);
        update_stuck_multiplier(state, view, profile, pre_move_position);
    }
    // `LivingEntity.aiStep`'s freezing block (`LivingEntity.java:3139-3151`), run
    // unconditionally — **not** inside the `!flying` gate above. Vanilla's
    // `Player.makeStuckInBlock` override is what suppresses the two calls above
    // while flying; nothing overrides `InsideBlockEffectType.FREEZE`
    // (`InsideBlockEffectType.java:6-11`), which `PowderSnowBlock.entityInside`
    // applies to every `LivingEntity` standing in the block regardless of
    // `abilities.flying`. A creative-flying player drifting through powder snow
    // still accumulates `frozen_ticks`, even with no stuck-multiplier drag.
    update_freezing(state, view, profile, pre_move_position);

    // `LivingEntity.aiStep`'s "push" block: `if (autoSpinAttackTicks > 0)
    // autoSpinAttackTicks--;` (`LivingEntity.java:3158-3159`), unconditional —
    // no `!flying` gate in vanilla either, so a riptide launch that carries a
    // player into creative flight still counts down normally. The
    // entity-hit sweep (`checkAutoSpinAttack`) that runs alongside it is not
    // modelled — see [`PlayerState::auto_spin_attack_ticks`]'s scope note.
    if state.auto_spin_attack_ticks > 0 {
        state.auto_spin_attack_ticks -= 1;
    }
}

/// [`tick`] followed by one pass of `LivingEntity.pushEntities` against `nearby`.
///
/// This is the whole of `LivingEntity.aiStep`'s ordering for entity interaction:
/// `travel` first (`LivingEntity.java:3130`), the crowd push last (`:3163`). So the
/// impulse a neighbour delivers this tick is integrated on the **next** one, and
/// this tick's collision sweep never sees it.
///
/// Passing an empty `nearby` is bit-for-bit [`tick`]: movement takes the ordinary
/// block-only entry point and [`crate::push::apply_entity_push`] returns
/// immediately. A mixed snapshot may carry both hard colliders (boats, shulkers,
/// eligible happy ghasts) and living crowd-push producers; the two independent
/// flags on [`crate::push::NearbyEntity`] keep those mechanisms separate.
pub fn tick_among_entities(
    state: &mut PlayerState,
    input: MovementInput,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
    nearby: &[crate::push::NearbyEntity],
    self_flags: crate::push::PushSelf,
) {
    travel_and_check_inside_blocks(state, input, view, profile, nearby);
    crate::push::apply_entity_push(state, view, profile, nearby, self_flags);
    // The pose comes *after* the push, because `pushEntities` is the tail of
    // `aiStep` (inside `super.tick()`) and `updatePlayerPose` is the tail of
    // `Player.tick()`. `nearby` also supplies the entity term of the fit gate —
    // vacuous unless one of them is a boat, a shulker or a happy ghast.
    update_player_pose(state, input, view, nearby);
}

/// `Entity.updateSwimming()` (`Entity.java:1644-1652`) — the sprint-swimming pose
/// state machine.
///
/// Entering requires being **under water** (eye submerged) *and* the block at the
/// feet holding water; once swimming, it is sustained merely by sprinting while
/// **in** water (box touching water), so you keep swimming as you break the
/// surface. Passenger/vehicle state is not modelled here (this engine has none),
/// matching the `!isPassenger()` guard being vacuously true.
///
/// `Player.updateSwimming` (`Player.java:1433-1439`) adds one override: a *flying*
/// player is never swimming. This engine has no flight, so a driver with a
/// free-fly/creative-flight mode must clear [`PlayerState::swimming`] itself while
/// flying rather than relying on this function — it is only reached from [`tick`],
/// which a flying driver does not call.
fn update_swimming(
    swimming: bool,
    sprinting: bool,
    fluid: &FluidState,
    view: &dyn CollisionView,
    position: Vec3d,
) -> bool {
    if swimming {
        sprinting && fluid.in_water()
    } else {
        sprinting && fluid.under_water() && water_at_block(view, position)
    }
}

/// `level.getFluidState(blockPosition).is(WATER)` — water at the entity's own
/// block position (`floor` of each coordinate), fine-then-coarse like the rest of
/// the fluid state.
fn water_at_block(view: &dyn CollisionView, position: Vec3d) -> bool {
    let bx = mth::floor(position.x);
    let by = mth::floor(position.y);
    let bz = mth::floor(position.z);
    match view.fluid_at(bx, by, bz) {
        Some(cell) => cell.kind == crate::fluid::FluidKind::Water,
        None => view.is_water(bx, by, bz),
    }
}

/// `LivingEntity.updateSwimAmount()` (`LivingEntity.java:3478-3483`) — advances
/// the `0..1` swim-pose ramp by `SWIM_AMOUNT_PER_TICK` (`0.09F`) toward `1.0`
/// while `swimming`, or back toward `0.0` otherwise, clamping at both ends.
///
/// Called immediately after [`update_swimming`] decides this tick's swim flag,
/// mirroring vanilla's exact call order: `LivingEntity.tick()` calls
/// `updateSwimAmount()` right after `super.tick()` (where `Entity.baseTick`'s
/// `updateSwimming()` lives) and *before* `aiStep()` (where `travel` — and
/// therefore the look-descent in [`tick_water`] — runs). So this ramp always
/// reflects `swimming` as of the **start** of the current tick, one step ahead
/// of the travel branch that consumes `swimming` directly.
fn update_swim_amount(state: &mut PlayerState) {
    const SWIM_AMOUNT_PER_TICK: f32 = 0.09;
    state.swim_amount_o = state.swim_amount;
    state.swim_amount = if state.swimming {
        (state.swim_amount + SWIM_AMOUNT_PER_TICK).min(1.0)
    } else {
        (state.swim_amount - SWIM_AMOUNT_PER_TICK).max(0.0)
    };
}

/// `LivingEntity.getFrictionInfluencedSpeed(blockFriction)`.
/// The player's effective walk speed for `getFrictionInfluencedSpeed`, i.e.
/// vanilla's `getSpeed()`. Uses the injected attribute value when present
/// (sprint + Speed/Slowness already folded in by the entity layer), reproducing
/// the `(float)` cast; otherwise computes the standalone base+sprint value.
fn effective_speed(profile: &PhysicsProfile, state: &PlayerState) -> f32 {
    match state.movement_speed {
        Some(v) => v as f32,
        None => player_speed(profile, state.sprinting),
    }
}

/// Player-shaped wrapper over [`friction_influenced_speed_value`] retained for
/// the speed unit tests, which assert against a `PlayerState`.
#[cfg(test)]
fn friction_influenced_speed(
    profile: &PhysicsProfile,
    state: &PlayerState,
    block_friction: f32,
) -> f32 {
    friction_influenced_speed_value(
        effective_speed(profile, state),
        block_friction,
        state.on_ground,
        player_flying_speed(state, profile),
        profile,
    )
}

/// `LivingEntity.getFrictionInfluencedSpeed(float)` (LivingEntity.java:2710),
/// entity-agnostic core. `speed` is the caller's `getSpeed()` — the player's
/// movement-speed attribute or a mob's AI-supplied speed. On the ground the
/// `0.21600002F / friction^3` factor rescales it (all in `float`); airborne it
/// is discarded entirely for `flying_speed`, the caller's already-resolved
/// `getFlyingSpeed()`. `speed` therefore only reaches the result on the ground,
/// exactly as vanilla.
///
/// **`flying_speed` is a parameter, not `profile.flying_speed`, because vanilla's
/// `getFlyingSpeed()` is virtual and the `Player` override is stateful** — it
/// depends on `abilities.flying`, `abilities.flyingSpeed` and `isSprinting()`
/// (`Player.java:1974-1980`). Reading a profile constant here silently
/// implemented only the "not flying, not sprinting" corner of it; see
/// [`player_flying_speed`] and `PhysicsProfile::airborne_sprint_speed`.
#[must_use]
pub(crate) fn friction_influenced_speed_value(
    speed: f32,
    block_friction: f32,
    on_ground: bool,
    flying_speed: f32,
    profile: &PhysicsProfile,
) -> f32 {
    if on_ground {
        if block_friction > 0.6 {
            let cubed = block_friction * block_friction * block_friction;
            speed * (profile.ground_accel / cubed)
        } else {
            speed
        }
    } else {
        flying_speed
    }
}

/// `Player.getFlyingSpeed()` (`Player.java:1974-1980`) — the airborne input speed
/// `getFrictionInfluencedSpeed` substitutes for `getSpeed()`.
///
/// ```text
/// if (this.abilities.flying && !this.isPassenger()) {
///    return this.isSprinting() ? this.abilities.getFlyingSpeed() * 2.0F : this.abilities.getFlyingSpeed();
/// } else {
///    return this.isSprinting() ? 0.025999999F : 0.02F;
/// }
/// ```
///
/// **Four values, not one.** Both arms are sprint-dependent, which is the part
/// that was missing here entirely: the airborne branch used to return a flat
/// `0.02F`, so a sprint-**jump** — every sprint-jump, with no creative flight
/// anywhere in sight — accelerated 30% short of vanilla. The literal is
/// `0.025999999F` and not `0.026F`; see
/// [`PhysicsProfile::airborne_sprint_speed`].
///
/// `!isPassenger()` is vacuously true: this engine has no riding state (the same
/// standing argument [`PlayerState::on_ground`] makes). A driver that adds
/// vehicles must suppress flight for a passenger itself — vanilla falls to the
/// *non*-flying arm there, it does not merely skip the doubling.
#[must_use]
pub fn player_flying_speed(state: &PlayerState, profile: &PhysicsProfile) -> f32 {
    if state.flying {
        if state.sprinting {
            state.flying_speed * 2.0
        } else {
            state.flying_speed
        }
    } else if state.sprinting {
        profile.airborne_sprint_speed
    } else {
        profile.flying_speed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// All seven 26.2 spear items resolve to the sprint-preserving override;
    /// every other id — including a non-spear weapon and an empty-hand
    /// sentinel — resolves to `DEFAULT`. Pairwise-distinct items on each side
    /// so a producer that always returns one constant cannot pass this.
    #[test]
    fn for_item_picks_out_exactly_the_seven_spears() {
        for spear in [
            "minecraft:wooden_spear",
            "minecraft:stone_spear",
            "minecraft:copper_spear",
            "minecraft:iron_spear",
            "minecraft:golden_spear",
            "minecraft:diamond_spear",
            "minecraft:netherite_spear",
        ] {
            assert_eq!(
                UseEffects::for_item(spear),
                UseEffects::SPEAR,
                "{spear} must resolve to UseEffects::SPEAR"
            );
        }
        for other in ["minecraft:bow", "minecraft:apple", "minecraft:shield"] {
            assert_eq!(
                UseEffects::for_item(other),
                UseEffects::DEFAULT,
                "{other} must resolve to UseEffects::DEFAULT"
            );
        }
        // Namespace-agnostic — see the method's own doc for why.
        assert_eq!(
            UseEffects::for_item("mymodpack:crystal_spear"),
            UseEffects::SPEAR
        );
    }

    #[test]
    fn player_speed_walk_and_sprint_bits() {
        let p = PhysicsProfile::mc_1_21();
        assert_eq!(player_speed(&p, false), 0.1f32);
        // Sprint speed derived via the attribute math: 0.13000001f (0x3e051eb9).
        assert_eq!(player_speed(&p, true).to_bits(), 0x3e05_1eb9);
    }

    #[test]
    fn friction_influenced_speed_default_ground_is_getspeed() {
        // On default 0.6 friction, the 0.216.../f^3 factor is exactly 1.0f.
        let p = PhysicsProfile::mc_1_21();
        let mut s = PlayerState::at(Vec3d::new(0.0, 0.0, 0.0), 0.0);
        s.on_ground = true;
        let bf = mth::compute_modified_friction(0.6, 1.0);
        assert_eq!(friction_influenced_speed(&p, &s, bf), 0.1f32);
    }

    #[test]
    fn injected_attribute_speed_replaces_not_stacks_with_sprint() {
        // Reconciled attribute seam. The entity layer's MOVEMENT_SPEED value
        // already folds sprint in, so a Some(v) override must be used verbatim
        // (as f32) even while `sprinting` is true — never re-multiplied here.
        let p = PhysicsProfile::mc_1_21();
        let bf = mth::compute_modified_friction(0.6, 1.0);

        // base 0.1 + sprint (AddMultipliedTotal 0.3) + Speed I (AddMultipliedTotal
        // 0.2), all one class => 0.1 * (1+0.3) * (1+0.2), per calculateValue().
        let attr = 0.1_f64 * (1.0 + 0.3) * (1.0 + 0.2);
        let mut s = PlayerState::at(Vec3d::new(0.0, 0.0, 0.0), 0.0).with_movement_speed(attr);
        s.on_ground = true;
        s.sprinting = true; // must be ignored while the override is present

        assert_eq!(friction_influenced_speed(&p, &s, bf), attr as f32);
        // Guard against the folding failure: it is NOT the sprint-stacked value.
        assert_ne!(friction_influenced_speed(&p, &s, bf), (attr * 1.3) as f32);
    }

    #[test]
    fn no_override_falls_back_to_standalone_sprint() {
        let p = PhysicsProfile::mc_1_21();
        let bf = mth::compute_modified_friction(0.6, 1.0);
        let mut s = PlayerState::at(Vec3d::new(0.0, 0.0, 0.0), 0.0);
        s.on_ground = true;
        s.sprinting = true;
        assert_eq!(
            friction_influenced_speed(&p, &s, bf).to_bits(),
            player_speed(&p, true).to_bits()
        );
    }

    struct WaterEverywhere;
    impl CollisionView for WaterEverywhere {
        fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<Aabb>) {}
        fn is_water(&self, _x: i32, _y: i32, _z: i32) -> bool {
            true
        }
    }

    struct LavaEverywhere;
    impl CollisionView for LavaEverywhere {
        fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<Aabb>) {}
        fn is_lava(&self, _x: i32, _y: i32, _z: i32) -> bool {
            true
        }
    }

    /// A hand-built world: explicit solid cells, explicit water cells with a real
    /// `getAmount()`, so the fluid **height** branches (jump threshold, hop-out)
    /// are exercised rather than the coarse full-cell fallback.
    #[derive(Default)]
    struct Pool {
        solid: std::collections::HashSet<(i32, i32, i32)>,
        water: std::collections::HashMap<(i32, i32, i32), u8>,
    }

    impl Pool {
        fn solid(&mut self, x: i32, y: i32, z: i32) -> &mut Self {
            self.solid.insert((x, y, z));
            self
        }
        fn water(&mut self, x: i32, y: i32, z: i32, amount: u8) -> &mut Self {
            self.water.insert((x, y, z), amount);
            self
        }
        fn floor(&mut self, y: i32) -> &mut Self {
            for x in -2..=2 {
                for z in -2..=2 {
                    self.solid.insert((x, y, z));
                }
            }
            self
        }
    }

    impl CollisionView for Pool {
        fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
            if self.solid.contains(&(x, y, z)) {
                out.push(Aabb::new(
                    f64::from(x),
                    f64::from(y),
                    f64::from(z),
                    f64::from(x) + 1.0,
                    f64::from(y) + 1.0,
                    f64::from(z) + 1.0,
                ));
            }
        }
        fn is_water(&self, x: i32, y: i32, z: i32) -> bool {
            self.water.contains_key(&(x, y, z))
        }
        fn fluid_at(&self, x: i32, y: i32, z: i32) -> Option<crate::fluid::FluidCell> {
            self.water
                .get(&(x, y, z))
                .map(|&amount| crate::fluid::FluidCell {
                    kind: crate::fluid::FluidKind::Water,
                    amount,
                    falling: false,
                })
        }
        fn blocks_motion(&self, x: i32, y: i32, z: i32) -> bool {
            self.solid.contains(&(x, y, z))
        }
    }

    #[test]
    fn fluid_jump_threshold_boundary_is_the_swimming_eye_height() {
        // `Entity.getFluidJumpThreshold()` = `getEyeHeight() < 0.4 ? 0.0 : 0.4`
        // (Entity.java:3692-3694). The swimming pose's eye height is *exactly*
        // 0.4 (Avatar.java:28), so it sits on the false side of a strict `<` and
        // keeps the 0.4 threshold. Coding this as `<=` would collapse a swimmer's
        // threshold to zero and turn every swim-up into a standing jump.
        assert_eq!(fluid_jump_threshold(DEFAULT_EYE_HEIGHT), 0.4);
        assert_eq!(fluid_jump_threshold(0.4), 0.4);
        assert_eq!(fluid_jump_threshold(0.399), 0.0);
    }

    #[test]
    fn shallow_water_jumps_but_deep_water_swims_up() {
        // The sinking-versus-swimming decision (`LivingEntity.aiStep`, the jump
        // block at LivingEntity.java:3088-3113). Expected magnitudes come from
        // vanilla constants, not from this port:
        //   * shallow  -> jumpFromGround, JUMP_STRENGTH = 0.42F, then the water
        //     tick's own `* 0.8F` vertical drag and `- gravity/16` buoyancy step
        //     => 0.42*0.8 - 0.005 = 0.331
        //   * deep     -> jumpInLiquid, +0.04F  => 0.04*0.8 - 0.005 = 0.027
        // A single order of magnitude apart, so the branch cannot be mistaken.
        let p = PhysicsProfile::mc_1_21();

        // amount 3 => own height 3/9 = 0.333 < the 0.4 threshold: a puddle.
        let mut shallow = Pool::default();
        shallow.floor(0).water(0, 1, 0, 3);
        let mut s = PlayerState::at(Vec3d::new(0.5, 1.0, 0.5), 0.0);
        s.on_ground = true;
        let jump = MovementInput {
            jump: true,
            ..MovementInput::NONE
        };
        tick(&mut s, jump, &shallow, &p);
        assert!(
            (s.velocity.y - (0.42 * 0.8 - 0.005)).abs() < 1.0e-6,
            "shallow water must produce a real jump, got vy = {}",
            s.velocity.y
        );

        // amount 8 => 8/9 = 0.888 > 0.4: deep enough to swim in.
        let mut deep = Pool::default();
        deep.floor(0).water(0, 1, 0, 8).water(0, 2, 0, 8);
        let mut s = PlayerState::at(Vec3d::new(0.5, 1.0, 0.5), 0.0);
        s.on_ground = true;
        tick(&mut s, jump, &deep, &p);
        assert!(
            (s.velocity.y - (0.04 * 0.8 - 0.005)).abs() < 1.0e-6,
            "deep water must swim up, not jump, got vy = {}",
            s.velocity.y
        );
    }

    #[test]
    fn sneaking_sinks_and_not_sneaking_barely_does() {
        // `LocalPlayer.aiStep` -> `goDownInWater()` (LivingEntity.java:2395-2397):
        // shift while in water adds -0.04F. Expected values from vanilla constants:
        // the tick's vertical drag is 0.8F and buoyancy is -gravity/16 = -0.005, so
        //   no shift: 0.0  * 0.8 - 0.005 = -0.005
        //   shift:   -0.04 * 0.8 - 0.005 = -0.037
        // The pair is the point: without `goDownInWater` both read -0.005 and the
        // only way down is to release jump and wait.
        let p = PhysicsProfile::mc_1_21();
        let view = WaterEverywhere;

        let mut idle = PlayerState::at(Vec3d::new(0.5, 95.0, 0.5), 0.0);
        tick(&mut idle, MovementInput::NONE, &view, &p);
        assert!(
            (idle.velocity.y - (-0.005)).abs() < 1.0e-8,
            "idle sink vy = {}",
            idle.velocity.y
        );

        let mut sinking = PlayerState::at(Vec3d::new(0.5, 95.0, 0.5), 0.0);
        tick(
            &mut sinking,
            MovementInput {
                sneak: true,
                ..MovementInput::NONE
            },
            &view,
            &p,
        );
        assert!(
            (sinking.velocity.y - (-0.04 * 0.8 - 0.005)).abs() < 1.0e-8,
            "shift-sink vy = {}",
            sinking.velocity.y
        );
        assert!(
            sinking.position.y < idle.position.y,
            "shift must actually move the player down further"
        );
    }

    #[test]
    fn swimming_into_a_ledge_hops_out_of_the_water() {
        // `LivingEntity.jumpOutOfFluid` (LivingEntity.java:2556-2561): a horizontal
        // collision plus a lifted box that is free of blocks *and* of liquid
        // replaces vertical velocity with a flat 0.3F. The expected value is that
        // literal, straight from the source.
        //
        // Geometry: a one-deep pool (water only at y = 1) with a shore block at
        // z = 1, and the player floating with its feet near the surface so the box
        // still overlaps the shore block. Swimming +Z (yaw 0 faces +Z) presses into
        // the shore.
        let p = PhysicsProfile::mc_1_21();
        let forward = MovementInput {
            forward: 1.0,
            ..MovementInput::NONE
        };

        let mut pool = Pool::default();
        pool.floor(0).water(0, 1, 0, 8).solid(0, 1, 1);
        let mut s = PlayerState::at(Vec3d::new(0.5, 1.9, 0.5), 0.0);
        let mut hopped = false;
        for _ in 0..60 {
            tick(&mut s, forward, &pool, &p);
            if (s.velocity.y - f64::from(0.3f32)).abs() < 1.0e-12 {
                hopped = true;
                break;
            }
        }
        assert!(hopped, "never hopped out; final state {s:?}");

        // Control: the identical pool with no shore block. `jumpOutOfFluid` is
        // gated on `horizontalCollision`, so with nothing to swim into the same
        // detector must never fire — proving the assertion above is not just
        // "0.3 appears sometimes".
        let mut open = Pool::default();
        open.floor(0).water(0, 1, 0, 8);
        let mut s = PlayerState::at(Vec3d::new(0.5, 1.9, 0.5), 0.0);
        for _ in 0..60 {
            tick(&mut s, forward, &open, &p);
            assert!(
                (s.velocity.y - f64::from(0.3f32)).abs() > 1.0e-12,
                "open water must never hop: vy = {}",
                s.velocity.y
            );
        }
    }

    #[test]
    fn depth_strider_attribute_speeds_up_swimming() {
        // `travelInWater` lerps both the horizontal slow-down (toward 0.546_000_06)
        // and the input speed (toward `getSpeed()`) by
        // `getAttributeValue(WATER_MOVEMENT_EFFICIENCY)` (LivingEntity.java:2507-2517).
        // No caller can reach that attribute value yet (see the field docs on
        // `PlayerState::water_movement_efficiency` for exactly what is missing), so
        // this drives it directly: the *arithmetic* is what is under test, and the
        // direction it must move in (faster) is fixed by the source, not by this port.
        //
        // Note the halving when airborne (`if (!onGround()) waterWalker *= 0.5F`):
        // a swimmer is airborne, so a level-III boot (0.99) acts as 0.495.
        let p = PhysicsProfile::mc_1_21();
        let view = WaterEverywhere;
        let forward = MovementInput {
            forward: 1.0,
            ..MovementInput::NONE
        };

        let travel = |efficiency: f32| {
            let mut s = PlayerState::at(Vec3d::new(0.5, 95.0, 0.5), 0.0)
                .with_water_movement_efficiency(efficiency);
            for _ in 0..40 {
                tick(&mut s, forward, &view, &p);
            }
            s.position.z - 0.5
        };

        let bare = travel(0.0);
        let strider = travel(0.99);
        assert!(
            strider > bare * 1.5,
            "Depth Strider must materially speed up swimming: {strider} vs {bare}"
        );
    }

    #[test]
    fn lava_sink_converges_to_terminal() {
        // First-principles anchor (not the oracle): the deep-lava step is
        // `vy = 0.5*vy - baseGravity/4`, so terminal solves `0.5*vy = -0.02`,
        // i.e. vy = -0.04. Different from water's -0.025 — a different branch.
        let p = PhysicsProfile::mc_1_21();
        let view = LavaEverywhere;
        let mut s = PlayerState::at(Vec3d::new(0.5, 95.0, 0.5), 0.0);
        for _ in 0..400 {
            tick(&mut s, MovementInput::NONE, &view, &p);
        }
        assert!(
            (s.velocity.y - (-0.04)).abs() < 1.0e-9,
            "terminal vy = {}",
            s.velocity.y
        );
    }

    #[test]
    fn levitation_makes_player_rise() {
        // First-principles anchor: Levitation replaces gravity with a pull toward
        // 0.05*(amp+1) > 0, so with no other input the player must gain height.
        struct Air;
        impl CollisionView for Air {
            fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<Aabb>) {}
        }
        let p = PhysicsProfile::mc_1_21();
        let mut s = PlayerState::at(Vec3d::new(0.5, 100.0, 0.5), 0.0).with_effects(StatusEffects {
            levitation: Some(0),
            ..StatusEffects::default()
        });
        for _ in 0..40 {
            tick(&mut s, MovementInput::NONE, &Air, &p);
        }
        assert!(s.position.y > 100.5, "levitation y = {}", s.position.y);
        assert!(s.velocity.y > 0.0, "levitation vy = {}", s.velocity.y);
    }

    #[test]
    fn slow_falling_revives_the_dead_water_clamp() {
        // The satisfying test: at default gravity the -0.003 fluid clamp is dead
        // (proven by `fluid_clamp_is_dead_at_default_gravity`). Slow Falling drops
        // effective gravity to 0.01 while descending, moving baseGravity/16 off
        // 0.005 so the clamp becomes reachable. Confirm effective_gravity and that
        // the clamp fires at least once during a slow-falling submerged sink.
        assert_eq!(effective_gravity(0.08, true, true), 0.01);
        assert_eq!(effective_gravity(0.08, false, true), 0.08); // not falling: base
        let p = PhysicsProfile::mc_1_21();
        let view = WaterEverywhere;
        let mut s = PlayerState::at(Vec3d::new(0.5, 95.0, 0.5), 0.0).with_effects(StatusEffects {
            slow_falling: true,
            ..StatusEffects::default()
        });
        let mut clamp_fired = false;
        for _ in 0..120 {
            tick(&mut s, MovementInput::NONE, &view, &p);
            if s.velocity.y == -0.003 {
                clamp_fired = true;
            }
        }
        assert!(clamp_fired, "slow-falling never revived the -0.003 clamp");
    }

    #[test]
    fn water_sink_converges_to_terminal() {
        // First-principles anchor (not the oracle): steady state solves
        // vy = 0.8*vy - baseGravity/16, i.e. vy = -0.005 / 0.2 = -0.025.
        let p = PhysicsProfile::mc_1_21();
        let view = WaterEverywhere;
        let mut s = PlayerState::at(Vec3d::new(0.5, 95.0, 0.5), 0.0);
        for _ in 0..400 {
            tick(&mut s, MovementInput::NONE, &view, &p);
        }
        assert!(
            (s.velocity.y - (-0.025)).abs() < 1.0e-6,
            "terminal vy = {}",
            s.velocity.y
        );
    }

    #[test]
    fn tick_sets_swimming_and_eye_in_water_when_sprinting_submerged() {
        // The real consumer of the eye-in-fluid state inside physics:
        // `updateSwimming`. A sprinting player fully under water enters the
        // sprint-swimming pose, and the eye/box submersion flags are recorded on
        // the state for the shell (fog / overlay / ambient sound) to read.
        let p = PhysicsProfile::mc_1_21();
        let view = WaterEverywhere;
        let mut s = PlayerState::at(Vec3d::new(0.5, 95.0, 0.5), 0.0);
        s.sprinting = true;
        tick(&mut s, MovementInput::NONE, &view, &p);
        assert!(s.eye_in_water, "eye is submerged");
        assert!(s.swimming, "sprinting + underwater => swimming pose");
    }

    #[test]
    fn tick_does_not_swim_when_sprinting_in_air() {
        // Negative control: sprinting with no water must not set the swim pose,
        // and must not spuriously report the eye in water.
        struct Air;
        impl CollisionView for Air {
            fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<Aabb>) {}
        }
        let p = PhysicsProfile::mc_1_21();
        let mut s = PlayerState::at(Vec3d::new(0.5, 100.0, 0.5), 0.0);
        s.sprinting = true;
        tick(&mut s, MovementInput::NONE, &Air, &p);
        assert!(!s.eye_in_water && !s.swimming);
    }

    #[test]
    fn swimming_is_sustained_while_sprinting_in_water_even_when_eye_surfaces() {
        // Once swimming, the pose persists on `sprinting && isInWater()` alone —
        // you keep swimming as you break the surface (eye leaves the water) until
        // you stop sprinting or leave the water. Uses a one-block-deep pool so the
        // box is in water but the eye (feet + 1.62) is above it.
        struct ShallowPool;
        impl CollisionView for ShallowPool {
            fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<Aabb>) {}
            fn is_water(&self, _x: i32, y: i32, _z: i32) -> bool {
                y == 94
            }
        }
        let p = PhysicsProfile::mc_1_21();
        let view = ShallowPool;
        let mut s = PlayerState::at(Vec3d::new(0.5, 94.0, 0.5), 0.0);
        s.sprinting = true;
        s.swimming = true; // already swimming from a prior submerged tick
        tick(&mut s, MovementInput::NONE, &view, &p);
        assert!(!s.eye_in_water, "eye is above the one-deep pool");
        assert!(s.swimming, "swim pose sustained by sprinting-in-water");
    }

    #[test]
    fn fluid_clamp_is_dead_at_default_gravity() {
        // With baseGravity/16 == 0.005, the two clamp conditions
        // (|y-0.005| >= 0.003 AND |y-0.005| < 0.003) are mutually exclusive, so
        // the -0.003 slow-sink never fires. Verify it degrades to y - 0.005.
        let g = f64::from(PhysicsProfile::mc_1_21().gravity);
        for &y in &[-0.01, -0.005, 0.0, 0.005, 0.02] {
            let out = fluid_falling_adjusted_movement(g, true, false, Vec3d::new(0.0, y, 0.0));
            assert_eq!(out.y, y - g / 16.0, "y = {y}");
        }
    }

    #[test]
    fn fluid_clamp_fires_under_reduced_gravity() {
        // Under slow-falling-style reduced gravity, baseGravity/16 != 0.005, so
        // the clamp can engage near terminal. Pick movement.y so both hold.
        let base = 0.01_f64; // baseGravity/16 = 0.000625
        let y = 0.001_f64; // |y-0.005|=0.004 >= 0.003, |y-0.000625|=0.000375 < 0.003
        let out = fluid_falling_adjusted_movement(base, true, false, Vec3d::new(0.0, y, 0.0));
        assert_eq!(out.y, -0.003);
    }

    #[test]
    fn profile_selects_structural_input_model_per_version() {
        // The 1.8-vs-modern difference is a *branch*, not a scalar: the profiles
        // must declare different `InputModel`s even though their numbers match.
        assert_eq!(
            PhysicsProfile::mc_1_21().input_model,
            InputModel::UnitSquareProjection
        );
        assert_eq!(
            PhysicsProfile::mc_1_8().input_model,
            InputModel::LegacyMoveFlying
        );
        assert_eq!(PhysicsProfile::mc_1_21().fluid_model, FluidModel::Modern);
        assert_eq!(PhysicsProfile::mc_1_8().fluid_model, FluidModel::Legacy1_8);
    }

    #[test]
    fn modern_input_path_is_selected_and_pure() {
        // The validated modern arm must be reachable through the enum dispatch and
        // produce the same result as the underlying unit-square function.
        let via_enum = modify_input(InputModel::UnitSquareProjection, 1.0, 1.0, None, false, 0.3);
        let direct = modify_input_unit_square(1.0, 1.0, None, false, 0.3);
        assert_eq!(via_enum.0.to_bits(), direct.0.to_bits());
        assert_eq!(via_enum.1.to_bits(), direct.1.to_bits());
    }

    #[test]
    fn use_item_default_scales_straight_input_by_the_vanilla_0_2_constant() {
        // `UseEffects.DEFAULT.speedMultiplier` = `0.2F` (`world/item/component/
        // UseEffects.java`), applied inside `modifyInput` between the existing
        // `0.98` scale and the sneak scale. Straight-forward input has no
        // diagonal clamp to interact with, so the predicted magnitude is
        // exactly the product of the two outside-sourced record constants —
        // not a rounded guess.
        let (sx, sy) = modify_input_unit_square(0.0, 1.0, Some(UseEffects::DEFAULT), false, 0.3);
        assert_eq!(sx, 0.0);
        let expected = 1.0f32 * 0.98 * 0.2;
        assert!(
            (sy - expected).abs() < 1.0e-6,
            "expected {expected}, got {sy}"
        );
    }

    #[test]
    fn spear_use_effects_apply_no_slowdown_the_discriminating_item() {
        // Every use-item except a spear gets `UseEffects.DEFAULT`
        // (`speed_multiplier = 0.2`); the seven spear items are the *only*
        // override, at `1.0` (`Item.Properties.spear(...)`, `Item.java:542`).
        // A fixture corpus containing only food cannot tell "the multiplier is
        // applied" from "the multiplier is applied to the wrong item set" —
        // the spear is the one input where the two hypotheses diverge.
        let baseline = modify_input_unit_square(0.0, 1.0, None, false, 0.3);
        let spear = modify_input_unit_square(0.0, 1.0, Some(UseEffects::SPEAR), false, 0.3);
        assert_eq!(baseline.1.to_bits(), spear.1.to_bits());
    }

    #[test]
    fn use_item_and_sneak_scales_combine_rather_than_one_overriding_the_other() {
        // The wrong hypothesis this guards against: an implementation that
        // treats sneaking and using-an-item as mutually exclusive branches
        // (`if sneak { .. } else if using_item { .. }`) instead of two
        // independent multiplies. Three distinct, independently-derived
        // magnitudes separate "both apply" from either single-gate reading —
        // this is not a sign check, it lands on one specific value.
        let (_, sy) = modify_input_unit_square(0.0, 1.0, Some(UseEffects::DEFAULT), true, 0.3);
        let both = 1.0f32 * 0.98 * 0.2 * 0.3;
        let sneak_only = 1.0f32 * 0.98 * 0.3;
        let use_item_only = 1.0f32 * 0.98 * 0.2;
        assert!(
            (sy - both).abs() < 1.0e-6,
            "expected both scales combined ({both}), got {sy} (sneak-only would \
             be {sneak_only}, use-item-only would be {use_item_only})"
        );
    }

    #[test]
    fn use_item_scale_applies_before_the_unit_square_clamp_not_after() {
        // `LocalPlayer.modifyInput` applies `itemUseSpeedMultiplier` to the raw
        // strafe/forward *before* `modifyInputSpeedForSquareMovement`'s
        // `length * dist_to_unit_square` clamp to `1.0`. A diagonal input is
        // the discriminating shape: full-magnitude diagonal input saturates
        // that clamp, but a 5x-reduced one (0.2 multiplier) typically does
        // not — so scaling *before* the clamp and scaling the *already-
        // clamped* output afterward land on genuinely different magnitudes,
        // not just a uniform 0.2x of each other.
        let unreduced = modify_input_unit_square(1.0, 1.0, None, false, 0.3);
        let unreduced_len = (unreduced.0 * unreduced.0 + unreduced.1 * unreduced.1).sqrt();
        assert!(
            (unreduced_len - 1.0).abs() < 1.0e-5,
            "control: an un-reduced diagonal must saturate the unit-square \
             clamp, got length {unreduced_len}"
        );

        let (rx, ry) = modify_input_unit_square(1.0, 1.0, Some(UseEffects::DEFAULT), false, 0.3);
        // Derived independently (not from this function): raw sx = sy = 0.98,
        // scaled to 0.196 each *before* the unit-square transform, giving a
        // pre-clamp length of 0.196*sqrt(2) ≈ 0.27719 — under 1.0, so the
        // clamp never engages and the diagonal's `dist_to_unit_square` factor
        // (sqrt(2)) is folded in below full saturation.
        let expected = 0.277_185_9_f32;
        assert!(
            (rx - expected).abs() < 1.0e-5 && (ry - expected).abs() < 1.0e-5,
            "expected ({expected}, {expected}), got ({rx}, {ry})"
        );
        // The wrong hypothesis (scale the already-clamped unit-diagonal output
        // by 0.2 afterward) would read {0.14142, 0.14142} instead — well
        // outside tolerance of the correct value, so this assertion cannot be
        // satisfied by both hypotheses at once.
        let wrong_hypothesis = unreduced.0 * 0.2;
        assert!(
            (rx - wrong_hypothesis).abs() > 0.02,
            "the pre-projection scale must diverge from naively scaling the \
             clamped output; got {rx}, wrong hypothesis would be {wrong_hypothesis}"
        );
    }

    #[test]
    #[should_panic(expected = "1.8 moveFlying input pipeline")]
    fn legacy_input_fails_loudly_not_silently() {
        // The whole point of the seam: a 1.8 profile must NOT silently run modern
        // math. Until the 1.8 pipeline is modelled and JVM-validated, it panics.
        let _ = modify_input(InputModel::LegacyMoveFlying, 1.0, 1.0, None, false, 0.3);
    }

    #[test]
    #[should_panic(expected = "1.8 fluid movement")]
    fn legacy_fluid_fails_loudly_not_silently() {
        let p = PhysicsProfile::mc_1_8();
        let view = WaterEverywhere;
        let mut s = PlayerState::at(Vec3d::new(0.5, 95.0, 0.5), 0.0);
        tick_water(&mut s, MovementInput::NONE, &FluidState::NONE, &view, &p);
    }

    #[test]
    fn jump_boost_power_is_tenth_per_level_as_float() {
        // getJumpBoostPower() = 0.1F*(amp+1) in float. Amp 0 (Jump Boost I) => 0.1F;
        // amp 1 (Jump Boost II) => 0.2F. The float literal matters (0.1 is inexact).
        assert_eq!(jump_boost_power(None), 0.0f32);
        assert_eq!(jump_boost_power(Some(0)).to_bits(), 0.1f32.to_bits());
        assert_eq!(
            jump_boost_power(Some(1)).to_bits(),
            (0.1f32 * 2.0).to_bits()
        );
    }

    #[test]
    fn slime_reverses_downward_velocity_and_sneak_cancels_it() {
        // First-principles anchor (not the oracle): a full slime cube has
        // bounce_restitution 1.0, so a player landing on it leaves with upward
        // velocity; the same fall while sneaking rests instead (vy path -> ~0).
        struct SlimeFloor;
        impl CollisionView for SlimeFloor {
            fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
                if y == 0 {
                    out.push(Aabb::new(
                        f64::from(x),
                        f64::from(y),
                        f64::from(z),
                        f64::from(x) + 1.0,
                        f64::from(y) + 1.0,
                        f64::from(z) + 1.0,
                    ));
                }
            }
            fn bounce_restitution(&self, _x: i32, y: i32, _z: i32) -> f32 {
                if y == 0 { 1.0 } else { 0.0 }
            }
        }
        let p = PhysicsProfile::mc_1_21();

        let mut bounced = false;
        let mut s = PlayerState::at(Vec3d::new(0.5, 6.0, 0.5), 0.0);
        for _ in 0..40 {
            tick(&mut s, MovementInput::NONE, &SlimeFloor, &p);
            if s.velocity.y > 0.05 {
                bounced = true;
                break;
            }
        }
        assert!(bounced, "player never bounced off slime");

        let mut peak: f64 = 1.0;
        let mut s = PlayerState::at(Vec3d::new(0.5, 6.0, 0.5), 0.0);
        let sneak = MovementInput {
            forward: 0.0,
            strafe: 0.0,
            jump: false,
            sneak: true,
            sprint: false,
            using_item: None,
        };
        for _ in 0..80 {
            tick(&mut s, sneak, &SlimeFloor, &p);
            assert!(
                s.velocity.y <= 0.05,
                "sneak failed to cancel the bounce: vy = {}",
                s.velocity.y
            );
            peak = peak.max(s.position.y);
        }
        // Never launched back above the drop height once landed.
        assert!(peak <= 6.0, "sneaking player gained height: {peak}");
    }
}
