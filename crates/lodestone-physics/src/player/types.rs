/// Vanilla's standing eye offset (`1.62F`), used as the default pose
/// [`eye height`](PlayerState::eye_height). The swimming / crawling / gliding
/// pose lowers it to `0.4`, crouching to `1.27`; [`crate::pose`] owns that
/// mapping and [`tick`] applies it.
pub const DEFAULT_EYE_HEIGHT: f32 = 1.62;

/// The pair vanilla's own client movement code consults while the player is
/// using an item: how much to scale movement input (folded into the input
/// modifier) and whether sprinting may continue at all (ANDed into the "may
/// start sprinting" check as "not slowed by item use").
///
/// Only two values exist in the 26.2 item table, and there is no third to add
/// a case for without a new jar reading: [`Self::DEFAULT`] is what every item
/// without an explicit override gets — food, potions, the bow, the crossbow,
/// the shield — and the seven spear items (wooden/stone/copper/iron/golden/
/// diamond/netherite, via vanilla's own per-item builder override) are the
/// *only* override, [`Self::SPEAR`]. So a caller resolving "which effects
/// apply" needs only "is the held item a spear", not a general item-effects
/// table.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UseEffects {
    /// Whether sprinting may continue while this item is in use.
    pub can_sprint: bool,
    /// The movement-input scale applied while this item is in use.
    pub speed_multiplier: f32,
}

impl UseEffects {
    /// Vanilla's default: every use-item except a spear gets no sprinting,
    /// input scaled to a fifth.
    pub const DEFAULT: Self = Self {
        can_sprint: false,
        speed_multiplier: 0.2,
    };
    /// Vanilla's spear override — charging a spear neither slows movement
    /// nor blocks sprinting.
    pub const SPEAR: Self = Self {
        can_sprint: true,
        speed_multiplier: 1.0,
    };

    /// Resolve which [`UseEffects`] a held item's own use-action would arm,
    /// from its id alone — see this type's own doc for why an id is enough:
    /// the seven spear items are the *only* override in the 26.2 item table,
    /// and every one of them is named `*_spear` (`wooden_spear`,
    /// `stone_spear`, `copper_spear`, `iron_spear`, `golden_spear`,
    /// `diamond_spear`, `netherite_spear`). Anything else — including an
    /// empty main hand — gets [`Self::DEFAULT`], matching vanilla's own
    /// per-item use-effects field defaulting when nothing overrides it.
    #[must_use]
    pub fn for_item(id: &str) -> Self {
        // Namespace-agnostic on purpose: a resource pack's own custom item
        // could ship under a different namespace, and the *path* is the only
        // part vanilla's own per-item override keys off (applied per item
        // definition, not looked up by namespace).
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
/// `1.0`), matching vanilla's own move-vector convention (`y` = forward,
/// `x` = strafe).
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
    /// Vanilla's own input-modifier step applies `speed_multiplier` between
    /// the `0.98` scale and the sneak scale (`modify_input_unit_square`
    /// below); the sprint half of [`UseEffects`] (`can_sprint`) is a
    /// *separate* conjunct on the "may start sprinting" check that the
    /// caller (`lodestone-controller`'s `compute_movement_intent`) must
    /// apply itself before ever constructing this — by the time `sprint`
    /// reaches here it is already gated, matching how the food-level sprint
    /// gate already works.
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
/// Speed/Slowness are deliberately **absent**: they are movement-speed
/// attribute modifiers, so they arrive pre-folded into the effective
/// movement speed via the attribute pipeline (see the crate docs' "attribute
/// seam"), not as a physics flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StatusEffects {
    /// The Levitation effect's amplifier (0-based) if active. During
    /// airborne travel this *replaces* gravity with
    /// `y += (0.05*(amp+1) - y) * 0.2`.
    pub levitation: Option<u32>,
    /// The Slow Falling effect. Reduces the effective gravity to
    /// `min(gravity, 0.01)` **while falling** — which, in fluids, is
    /// precisely what revives the otherwise-dead `-0.003` slow-descent
    /// clamp.
    pub slow_falling: bool,
    /// The Dolphin's Grace effect. Forces the in-water horizontal slow-down
    /// to `0.96F` regardless of sprint state.
    pub dolphins_grace: bool,
    /// The Jump Boost effect's amplifier (0-based) if active. Per the ruling
    /// that Jump Boost is **not** a movement-speed attribute modifier, it
    /// rides its own field: it adds `0.1F * (amp + 1)` to the jump velocity,
    /// *after* the jump-strength/block-jump-factor product.
    pub jump_boost: Option<u32>,
}

/// Mutable player physics state carried across ticks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerState {
    /// World position (feet centre).
    pub position: Vec3d,
    /// Delta movement (velocity).
    pub velocity: Vec3d,
    /// Yaw in degrees; `0` faces `+Z` (south).
    pub yaw: f32,
    /// Pitch in degrees.
    pub pitch: f32,
    /// Whether the player is on the ground, i.e. this tick's move collided
    /// **downward**. This is the flag the client **transmits** to the server
    /// on every movement packet.
    ///
    /// It is a *distinct decision* from the collision result the server re-runs
    /// from our reported position: if the server ever believes we are unsupported
    /// and not descending in open air, it counts consecutive above-ground ticks
    /// and disconnects with `multiplayer.disconnect.flying` once that count
    /// exceeds its maximum (80 ticks at default gravity). Because our position
    /// is bit-exact, the server's own downward collision stays aligned with
    /// this flag, so the two never diverge — but a driver must transmit *this*
    /// value unmodified rather than re-deriving one.
    ///
    /// Vanilla computes it identically in **every** movement mode (walking,
    /// swimming, climbing, falling); there is no bespoke "supported" notion for
    /// swimming or climbing. The **sole override** is vanilla's own per-tick
    /// player update, which forces `onGround = false` while a **spectator or
    /// passenger** (riding a boat/minecart/horse). This engine still has no
    /// riding state — that is a session fact, not a physics one — and the
    /// override now has a real owner: `lodestone_ecs::player::pin_passenger_to_vehicle`
    /// applies it off `lodestone_ecs::session::Riding` in the same system that
    /// snaps the player onto the seat, so `on_ground` keeps exactly one writer
    /// per tick. See the `spectator_or_passenger_note` contract test in
    /// `tests/on_ground.rs` for why it is not a field here, and for the
    /// measured correction that the *server* never kicks a passenger over this
    /// flag (it excludes passengers from that check explicitly) — the
    /// override is for local readers.
    ///
    /// Note: a player starting from rest reports airborne for exactly one settle
    /// tick, because a tick runs `move()` before applying gravity — matching the
    /// server's own first-tick computation.
    pub on_ground: bool,
    /// Whether the player collided horizontally last tick.
    pub horizontal_collision: bool,
    /// Countdown that gates repeated jumps (vanilla's own no-jump-delay timer).
    pub no_jump_delay: i32,
    /// The client Options Auto-Jump toggle, defaulting **on** exactly like
    /// vanilla's own client-side field.
    ///
    /// **This is the only gate on the detector, and it is now really driven.**
    /// The shell's `Options::auto_jump` reaches it once per tick through
    /// `lodestone_ecs::player::AutoJump`: before that the field sat
    /// at its `true` default for the whole session, so the settings toggle was
    /// decorative — the option read OFF and [`update_auto_jump`] still armed
    /// jumps. See that resource's doc for the seam.
    pub auto_jump_enabled: bool,
    /// The one-tick deferral that carries the auto-jump *decision* (made at
    /// the end of this tick's move by [`update_auto_jump`]) across to the
    /// next tick's movement step, where it is spent as a forced jump. `0`
    /// means idle.
    pub auto_jump_time: i32,
    /// Whether the player is currently sprinting (affects movement speed).
    pub sprinting: bool,
    /// Whether the player is gliding with an elytra. When set, [`tick`]
    /// routes to [`tick_elytra`] instead of [`tick_air`] (fluid still takes
    /// precedence, matching vanilla's own travel dispatch order).
    pub fall_flying: bool,
    /// Active physics-affecting status effects.
    pub effects: StatusEffects,
    /// Effective movement-speed attribute value handed in by the entity layer
    /// (`lodestone-entity`'s resolved attribute-value accessor), or `None` to let
    /// physics compute the standalone base+sprint value itself.
    ///
    /// **Reconciled attribute seam.** Vanilla's own player update does, every
    /// tick, casts the resolved movement-speed attribute value to a `float`
    /// and stores it, and movement reads that float back. That attribute
    /// value already folds in the transient **sprint** modifier (a `+30%`
    /// multiply-on-total) *and* any Speed/Slowness/Depth-Strider modifiers,
    /// computed by vanilla's three-stage modifier resolution (flat add →
    /// multiply-on-base → multiply-on-total, applied in that order). Physics
    /// must **not** reimplement that maths or re-apply sprint: when this is
    /// `Some(v)`, [`friction_influenced_speed`] uses `v as f32` directly
    /// (reproducing vanilla's own cast to `float` at the same place) and
    /// ignores the `sprinting` flag, so there is no double-count. Pass the
    /// raw `f64` — never a pre-cast `f32` — so the double→float rounding
    /// stays inside physics.
    pub movement_speed: Option<f64>,
    /// Pending **"stuck in block" speed multiplier**, set last tick by the
    /// block we were inside and consumed at the top of the next move (see
    /// [`CollisionView::stuck_multiplier`]). `ZERO` means "not stuck"; vanilla
    /// treats a squared length `<= 1.0E-7` as unset. Cobweb, powder snow and
    /// sweet berry bush write this; consumption multiplies the tick's
    /// movement component-wise and then zeroes velocity, exactly as vanilla —
    /// the one-tick delay between entering the block and being slowed is
    /// observable and reproduced.
    pub stuck_speed_multiplier: Vec3d,
    /// The player's current [`Pose`], which decides the **collision box** (via
    /// [`Self::dimensions`]) and the [`eye height`](Self::eye_height).
    ///
    /// **An output of [`tick`], not an input to it.** Vanilla's own pose
    /// update runs at the end of its per-tick player update and is
    /// fit-gated — a pose whose box would not fit where the player stands is
    /// vetoed — so it cannot be meaningfully set from outside per tick.
    /// [`Self::with_pose`] exists to seed an initial pose (a test fixture, or
    /// a session resuming mid-swim); after the first [`tick`] the machine
    /// owns it. See [`crate::pose`] for the state machine and for why
    /// skipping its gate would clip a surfacing swimmer into a ceiling with
    /// nothing to catch them.
    ///
    /// The narrower travel entry points ([`tick_air`], [`tick_water`],
    /// [`tick_lava`], [`tick_elytra`]) are vanilla's own travel dispatch, not
    /// its full per-tick player update: they *read* the pose for the box and
    /// never write it.
    pub pose: Pose,
    /// Pose **eye height**, the offset from feet to eye used by
    /// [`crate::compute_fluid_state`] to decide eye-in-fluid. Standing is
    /// `1.62` (vanilla's own default eye height); the swimming/crawling/gliding
    /// pose is `0.4`, crouching `1.27`. Vanilla widens it to `double` and adds
    /// the feet Y to get the eye's world Y, reproduced exactly.
    ///
    /// **Derived from [`Self::pose`], and rewritten by every [`tick`].** In
    /// vanilla the two are one record — refreshing the collision dimensions
    /// sets the eye height in the same step that sets the box — and
    /// splitting them is observable: a `0.6`-high box with a `1.62` eye makes
    /// a fully submerged swimmer read `eye_in_water == false`, because
    /// `compute_fluid_state`'s cell sweep is bounded by the *box* and so
    /// never reaches the eye's cell. That kills the fog, the overlay and the
    /// swim-state entry condition at once.
    ///
    /// It is therefore an **output mirror**, published for the camera and the
    /// fog: nothing inside [`tick`] reads this field. Both places that need
    /// an eye height (`compute_fluid_state`'s eye sweep and the fluid-jump
    /// threshold) call [`Pose::eye_height`] directly, so a caller that
    /// overwrites this field between ticks — as `lodestone-ecs`'s own pose
    /// layer currently does — can mislead the camera but can never
    /// desynchronise the eye from the box inside physics.
    ///
    /// [`Self::with_eye_height`] therefore only usefully models a pose this
    /// crate does not have (sleeping, dying), and only for a driver that does
    /// not call [`tick`].
    pub eye_height: f32,
    /// **Output.** Whether the eye is in water as of the last [`tick`], i.e.
    /// the eye block-column held water spanning the eye Y. Combine with
    /// in-water via [`Self::eye_in_water`] + fluid presence for the
    /// underwater check; the shell reads this for submerged fog, the
    /// underwater overlay, and `ambient.underwater.*`.
    pub eye_in_water: bool,
    /// **Output.** Whether the eye is in lava as of the last [`tick`].
    pub eye_in_lava: bool,
    /// **Output.** Whether the player is sprint-swimming after the last
    /// [`tick`]'s swim-state update: entered when sprinting while submerged
    /// in water and sustained while sprinting in water.
    ///
    /// The server derives this **itself**, from its own sprinting flag and
    /// its own collision — there is no swimming bit anywhere on the wire
    /// (the movement-input packet is seven booleans:
    /// forward/backward/left/right/jump/shift/sprint). What a driver must
    /// transmit is the **sprint** edge, via a dedicated start/stop-sprinting
    /// packet — the movement-input packet's `sprint` flag is stored as the
    /// last-seen client input and does *not* itself flip the server's
    /// sprinting flag. Send only the sprint edge and the server's swim pose
    /// follows; send the input packet alone and it never does.
    pub swimming: bool,
    /// **Output.** The swim-amount ramp after the last [`tick`] — a `0..1`
    /// value ramping toward the swim pose, advanced by a fixed per-tick step
    /// (`0.09F`) and clamped to `[0, 1]`, never snapping the way
    /// [`Self::swimming`] itself does. Vanilla advances this **every tick**,
    /// right after the swim-state update decides this tick's
    /// [`Self::swimming`] and before the rest of its per-tick movement runs,
    /// so [`crate::player::tick`] updates it in that same slot.
    ///
    /// Vanilla uses this to blend the swimming model's body-pitch animation
    /// — **not** the camera eye height, which the camera rig smooths
    /// independently (see `camera_rig.rs`'s `EyeHeightSmoother`). Nothing in
    /// this crate reads this field; it exists so a renderer can consume the
    /// exact per-tick ramp instead of re-deriving one from [`Self::swimming`]
    /// (which would reintroduce the snap this field exists to avoid).
    pub swim_amount: f32,
    /// **Output.** The previous tick's [`Self::swim_amount`], for a
    /// partial-tick interpolated read — vanilla linearly interpolates
    /// between the two by the frame's tick fraction. Because the ramp is
    /// monotonic and clamped (unlike the arm-swing's sawtooth `attack_anim`,
    /// which wraps a negative delta), a plain `lerp(a, swim_amount_o,
    /// swim_amount)` is exactly vanilla's read — no wrap-around correction
    /// needed.
    pub swim_amount_o: f32,
    /// The Depth Strider attribute value, as an **input** from the
    /// equipment layer (like [`Self::movement_speed`]).
    ///
    /// Vanilla's own in-water travel step reads this attribute value, halves
    /// it when airborne, and then uses it to lerp the horizontal slow-down
    /// toward `0.546_000_06` and the input speed toward the effective
    /// movement speed. Depth Strider contributes `0.33` per level via its
    /// enchantment effect, so a level-III boot is `0.99`.
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
    /// three-stage modifier-resolution fold from base + modifiers to an effective
    /// value, which the shell also does not do for the movement-speed attribute
    /// (it recomputes that itself instead).
    ///
    /// The arithmetic lives in [`tick_water`] so that the value is the *only* missing
    /// piece rather than the whole branch, and so nothing can silently substitute a
    /// plausible number for it.
    pub water_movement_efficiency: f32,
    /// Whether **creative flight is currently engaged**.
    ///
    /// This is *server authority*, not a local toggle: the player-abilities
    /// packet carries it, and vanilla's client-side toggle is gated on a
    /// separate may-fly grant (which lives on the driver, not here — physics
    /// never decides whether flight is *allowed*, only what it does). A
    /// driver that lets the player fly without a server grant is a bug, not
    /// a feature: see `docs/creative-flight.md`.
    ///
    /// # What setting this does, in three places and no more
    ///
    /// Flight is **not** a fourth travel mode beside air/water/lava/elytra. It is
    /// a set of modifications to the machinery that already exists, because
    /// vanilla's own player travel step *wraps* the base travel behaviour
    /// rather than replacing it:
    ///
    /// 1. **A dispatch suppressor.** Vanilla's fluid-affected check is
    ///    `!abilities.flying`, and its fluid-travel gate requires it, so a
    ///    flying player **never takes the fluid branch** — flying underwater
    ///    goes through [`tick_air`], not [`tick_water`].
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
    /// mode disables physics entirely, which this crate does not model at all
    /// (see the module docs).
    pub flying: bool,
    /// The **creative-flight** input speed, server-set, default `0.05F`
    /// (vanilla's own default flying speed).
    ///
    /// Consumed by [`player_flying_speed`], which doubles it while sprinting. Note
    /// the vertical fly impulse (the ascend/descend input, times this speed,
    /// times `3.0F`) uses this **raw**, *un*-doubled value even while
    /// sprinting — that asymmetry is the driver's to reproduce, and
    /// `lodestone-ecs`'s `apply_creative_flight_input` does.
    ///
    /// **Not [`PhysicsProfile::flying_speed`]**, which is the unrelated `0.02F`
    /// non-flying airborne constant. They are 2.5x apart and both are called
    /// "flying speed" in vanilla.
    pub flying_speed: f32,
    /// Vanilla's own fall-distance accumulator — a `double`, not a `float`,
    /// since 26.2.
    ///
    /// **Why it is load-bearing.** [`move_entity`] is documented as modelling
    /// only "the parts of vanilla's own move step that affect an entity's
    /// reported position", and fall distance used to be squarely outside
    /// that: it drives fall *damage*, which the server owns. Vanilla's own
    /// edge back-off behaviour changes that — it consults the accumulator to
    /// decide whether the player is "above ground", and the back-off moves
    /// you, so fall distance is now a position input.
    ///
    /// **The "above ground" check is the only reader in *this* crate, not
    /// the only reader in vanilla.** The others are all outside this crate's
    /// scope today, and are listed because a maintained value is what
    /// unblocks them: the critical-hit condition (fall distance above zero),
    /// the fall-damage calculation, and block-landing effects.
    ///
    /// **This crate maintains it.** [`tick`]/[`tick_air`]/[`tick_water`]/
    /// [`tick_lava`]/[`tick_elytra`] reproduce every site vanilla touches it:
    ///
    /// * **Accumulation + grounded reset** — vanilla's own fall-damage
    ///   bookkeeping (reached through the living-entity override, which
    ///   spawns landing particles and then defers to the base behaviour
    ///   unchanged): if not in water and moving downward, subtract the
    ///   downward velocity delta from the accumulator; then, unconditionally,
    ///   if on the ground, reset it to zero. Note the truncation of the
    ///   `double` delta to `float` *before* the subtraction into the `double`
    ///   field — reproduced here as `state.fall_distance -=
    ///   f64::from(ya as f32)`. In vanilla this call sits inside the move
    ///   step itself, gated on a check that is always `true` for the local
    ///   player, which is the only player this crate models. The
    ///   full-block-or-more clip-through reset that also lives inside the
    ///   move step needs a world raycast and is **not** modelled — see
    ///   [`crate::entity::move_entity`]'s own scope note.
    /// * **Water reset** — vanilla's own fluid-interaction update resets the
    ///   accumulator on entering water, called before travel runs each tick.
    ///   This crate's dispatch already computes the same per-tick fluid
    ///   summary before choosing [`tick_water`], so the reset lands at the
    ///   top of that function. **That call is not the only one**: the
    ///   fall-damage check calls the same fluid-interaction update again
    ///   from *inside* the move step, which is why the water-**entry** tick
    ///   diverges by one tick here — see [`accumulate_fall_distance`], which
    ///   documents the bound and why it cannot move the player.
    /// * **Lava halving** — vanilla's own per-tick update halves the
    ///   accumulator while in lava, applied at the top of [`tick_lava`] for
    ///   the same reason.
    /// * **Climbable reset** — vanilla resets the accumulator while on a
    ///   climbable, reached only through its airborne travel step, so only
    ///   [`tick_air`] applies it — matching vanilla, where a climbable never
    ///   resets fall distance while swimming or gliding.
    /// * **Slow Falling / Levitation reset** — vanilla resets the
    ///   accumulator unconditionally, before its travel dispatch, whenever
    ///   either effect is active — applied in [`tick`] before it picks a
    ///   travel path.
    /// * **Elytra accumulation clamp** — vanilla clamps the accumulator to
    ///   `1.0` while gliding, whenever the vertical velocity is above `-0.5`
    ///   and the accumulator already exceeds `1.0`; checked only while
    ///   gliding, before the Slow Falling/Levitation check and before
    ///   travel. Applied in [`tick`] alongside that check, gated on
    ///   [`Self::fall_flying`].
    /// * **Stuck-in-block reset** — vanilla resets the accumulator (and
    ///   stamps the stuck-speed multiplier) every tick its block scan finds
    ///   a stuck-triggering block (cobweb, powder snow, sweet berry bush,
    ///   honey). This crate's `update_stuck_multiplier` already reproduces
    ///   that block scan; the reset rides along whenever it finds one.
    ///
    /// **Not modelled, matching pre-existing gaps elsewhere in this crate.**
    /// The mid-move water re-evaluation is the one gap that is a
    /// *divergence* rather than an absent feature — bounded to the
    /// water-entry tick and unable to affect position, fully documented on
    /// [`accumulate_fall_distance`] and pinned by a test.
    /// Creative flight (vanilla resets the accumulator while flying, unless
    /// a passenger) does not apply — see [`tick_air`]'s own doc on
    /// `!abilities.flying`, "this crate has no creative flight".
    /// Riding/vehicles do not apply — "this engine has no riding state" (see
    /// the `on_ground` doc on [`Self::on_ground`]/`tests/on_ground.rs`'s
    /// `spectator_or_passenger_note`). Bubble columns do not apply — see
    /// [`tick_water`]'s "Not modelled" list. **Teleport is a driver
    /// responsibility**: this crate has no teleport primitive of its own (a
    /// driver sets [`Self::position`] directly), so a caller that snaps the
    /// position (server correction, respawn, an ender pearl or chorus fruit
    /// consume effect, all of which reset the accumulator in vanilla) must
    /// also call [`Self::reset_fall_distance`] itself.
    ///
    /// **Sign, verified against the jar rather than assumed.** A negative
    /// downward-velocity delta (moving down) makes the subtraction an
    /// *increase* — e.g. a delta of `-0.5` gives `fall_distance -= -0.5`,
    /// i.e. `+= 0.5`. This is invisible in any test where the player never
    /// leaves the ground, and it is exactly the input this field exists for.
    pub fall_distance: f64,
    /// Ticks accumulated standing in powder snow (the `TicksFrozen` field on
    /// the wire), `0..=`[`Self::TICKS_REQUIRED_TO_FREEZE`]. Maintained by
    /// [`tick`] (`update_freezing`): `+1` (capped) each tick the
    /// swept movement segment finds `CollisionView::is_powder_snow`, `-2`
    /// (floored at `0`) every other tick — vanilla's own per-tick freezing
    /// update. See [`Self::is_freezing`],
    /// [`Self::is_fully_frozen`], [`Self::percent_frozen`] and
    /// [`Self::should_apply_freeze_damage`] for what a driver reads it through.
    pub frozen_ticks: u32,
    /// The riptide-trident spin-attack countdown, started at
    /// `20` by [`apply_riptide`] (vanilla's own release-using trident logic:
    /// arms a 20-tick spin attack at power `8.0F`) and decremented by
    /// [`tick`] every tick thereafter, unconditionally — no `!flying` gate,
    /// matching vanilla. `> 0` means the spin attack is active
    /// ([`Self::is_auto_spin_attack`]), which [`crate::pose::desired_pose`]
    /// reads to select [`Pose::SpinAttack`].
    ///
    /// **Not modelled**: the attack-damage side (the spin's own damage
    /// value, its held item snapshot, and its entity-hit sweep) — this crate
    /// applies no damage anywhere, matching [`Self::frozen_ticks`]'s scope
    /// note.
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
            // Vanilla's own client-side field defaults to `true` and is read
            // from Options when the player is created.
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
            // Seeded to vanilla's own default flying speed rather than `0.0`
            // so that a driver which sets `flying` but forgets to forward
            // the server's speed flies at vanilla's default rate instead of
            // being unable to move at all — a wrong-but-plausible failure is
            // far easier to see than a silently frozen one.
            flying_speed: 0.05,
            fall_distance: 0.0,
            frozen_ticks: 0,
            auto_spin_attack_ticks: 0,
        }
    }

    /// Returns a copy of this state with creative flight engaged or released,
    /// and the server-reported flying speed it flies at.
    ///
    /// `flying_speed` is ignored while `flying` is `false` — vanilla's own
    /// flying-speed accessor takes its non-flying arm then — but it is stored
    /// regardless so a toggle does not have to re-supply it.
    #[must_use]
    pub fn with_flight(mut self, flying: bool, flying_speed: f32) -> Self {
        self.flying = flying;
        self.flying_speed = flying_speed;
        self
    }

    /// Zeroes [`Self::fall_distance`], matching vanilla's own reset.
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

    /// Vanilla's own base ticks-required-to-freeze value — `140`, and
    /// unconditionally so for a player: vanilla only ever varies it per
    /// living-entity subclass (none of which this crate models).
    pub const TICKS_REQUIRED_TO_FREEZE: u32 = 140;

    /// Matches vanilla's own fully-frozen check: accumulated ticks at or
    /// past the required threshold.
    #[must_use]
    pub fn is_fully_frozen(&self) -> bool {
        self.frozen_ticks >= Self::TICKS_REQUIRED_TO_FREEZE
    }

    /// Matches vanilla's own percent-frozen expression:
    /// `min(ticksFrozen, ticksToFreeze) / ticksToFreeze`, as an `f32` — the
    /// freezing-overlay vignette's `0..1` ramp. The `min` is redundant given
    /// [`Self::frozen_ticks`] is already capped at [`Self::TICKS_REQUIRED_TO_FREEZE`]
    /// by [`tick`], but kept to mirror vanilla's own expression exactly
    /// rather than rely on that invariant holding for every future writer of
    /// the field.
    #[must_use]
    pub fn percent_frozen(&self) -> f32 {
        (self.frozen_ticks.min(Self::TICKS_REQUIRED_TO_FREEZE) as f32)
            / (Self::TICKS_REQUIRED_TO_FREEZE as f32)
    }

    /// Matches vanilla's own per-tick freeze-damage trigger:
    /// `tickCount % 40 == 0 && isFullyFrozen() && canFreeze()`. The
    /// "can freeze" check is unconditionally `true` for a player — see
    /// [`Self::frozen_ticks`]'s "not modelled" note — so this reduces to the
    /// two terms this crate can actually answer, plus the one input it
    /// cannot own: `tick_count` is vanilla's own absolute tick counter
    /// against the world's clock, which this crate has no field for (every
    /// timer it owns is a countdown). Pass the driver's own tick counter;
    /// this function applies no damage itself — see [`Self::frozen_ticks`]'s
    /// doc for why the amount and its application are out of this crate's
    /// scope entirely.
    #[must_use]
    pub fn should_apply_freeze_damage(&self, tick_count: u64) -> bool {
        tick_count % 40 == 0 && self.is_fully_frozen()
    }

    /// Matches vanilla's own auto-spin-attack check: the countdown above
    /// zero.
    #[must_use]
    pub fn is_auto_spin_attack(&self) -> bool {
        self.auto_spin_attack_ticks > 0
    }

    /// Returns a copy of this state with the
    /// [water-movement-efficiency](Self::water_movement_efficiency) attribute
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
    /// [`eye height`](Self::eye_height) to match — the pair that vanilla
    /// always writes together when it refreshes an entity's collision
    /// dimensions.
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

    /// The hitbox dimensions for this state's [`pose`](Self::pose) — matches
    /// vanilla's own per-pose dimensions lookup for a player.
    ///
    /// This is what the collision sweep is handed, and the whole reason the pose
    /// exists: `0.6 × 1.8` standing, `0.6 × 1.5` crouching, `0.6 × 0.6` swimming
    /// or gliding. `step_height` is pose-independent (the step-height
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
    /// movement-speed attribute value injected (see [`Self::movement_speed`]).
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
    /// pose** — matches vanilla's own bounding-box accessor.
    ///
    /// The hitbox is per-entity data ([`EntityDimensions`]), not version data, so
    /// it does not come from the profile. The `profile` parameter is retained (as
    /// `_profile`) purely for source compatibility with existing callers; it is
    /// unused, and a caller may drop the argument once its call sites are updated.
    ///
    /// Since vanilla's own box construction anchors its own minimum Y at the
    /// feet, a pose change moves only the top face (and, for poses this crate does
    /// not model, the width).
    #[must_use]
    pub fn bounding_box(&self, _profile: &PhysicsProfile) -> Aabb {
        self.dimensions().bounding_box(self.position)
    }
}


