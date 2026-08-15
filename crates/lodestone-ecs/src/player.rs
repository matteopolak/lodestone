//! The **local player** as components on one entity, plus the `GameTick`
//! systems that advance it — Stage 2 of `docs/bevy-migration.md`.
//!
//! # What lives here and what deliberately does not
//!
//! The state and the *scheduling* live here. The maths does not:
//! [`lodestone_physics`] stays a plain library that [`player_physics`] calls,
//! because it is bit-exact against a JVM oracle with golden traces and a
//! system that re-derived the integration would be re-deriving the oracle from
//! the code under test (`docs/bevy-migration.md` §8).
//!
//! The **input** half is one crate up, in `lodestone_controller::ecs`
//! ([`RawInput`](lodestone_controller::ecs::RawInput) and the
//! [`TickSet::Input`](crate::TickSet::Input) systems that write
//! [`MovementIntent`]). That split is forced, not stylistic:
//! `lodestone-controller` depends on `lodestone-client`, which depends on this
//! crate, so a dependency the other way would be a cycle. It also happens to
//! be the right place — the controller crate's whole purpose is that native and
//! browser share one held-keys → [`MovementInput`] implementation.
//!
//! # The collision borrow, and why this is a `CollisionSource` not a view
//!
//! A `bevy_ecs` `Resource` must be `'static`, and the workspace denies
//! `unsafe_code`, so a `&dyn CollisionView` cannot reach a scheduled system.
//! The obvious fix — `Arc<dyn CollisionView + Send + Sync>` — works for the
//! live path (`Sim::live_collision` already returns an owned snapshot) but
//! *not* for the offline demo world, whose adapter (`WorldCollision`) borrows
//! the world outright.
//!
//! [`CollisionSource`] solves both with one indirection: it hands a
//! `&dyn CollisionView` to a callback rather than returning one, so an
//! implementor may build a borrowed view over state it owns. That is strictly
//! better than an `Arc<dyn CollisionView>` for a second reason — an owned
//! *wrapper* around `WorldCollision` would have to re-delegate all thirteen
//! `CollisionView` methods by hand, and a method added to the trait later would
//! silently fall back to the trait default in the wrapper while
//! `WorldCollision` overrode it. That is exactly the "two adapters, one of them
//! subtly wrong" failure `lodestone_shell::collision`'s module docs warn about.
//!
//! # Ordering
//!
//! ```text
//! GameTick
//!   TickSet::Intent    apply_look_intent
//!                      → (controller) compute_movement_intent → tick_sprint_window
//!   TickSet::Physics   player_physics
//!   TickSet::Send      (controller) send_move_action → send_player_input
//! ```
//!
//! This diagram used to put the controller's two systems in `TickSet::Input`.
//! They are in **`TickSet::Intent`**, alongside [`apply_look_intent`], and the
//! ordering *within* that set is load-bearing rather than incidental:
//! `apply_look_intent` takes `&mut PhysicsState` to commit this tick's rotation
//! while `compute_movement_intent` takes `&PhysicsState`, so the two are a real
//! write/read pair. Left unordered they fail the schedule build under strict
//! ambiguity detection — see `lodestone_controller::ecs`'s
//! `exactly_one_system_writes_movement_intent`, which is the guard that caught it.
//!
//! `Send` last is the point of the stage: a plugin adding a system
//! `.after(TickSet::Physics).before(TickSet::Send)` changes what the server is
//! told this tick.

use std::sync::Arc;

use bevy_app::{App, Plugin};
use bevy_ecs::component::Component;
use bevy_ecs::prelude::{Entity, Query, Res, ResMut, With};
use bevy_ecs::resource::Resource;
use bevy_ecs::schedule::IntoScheduleConfigs;
use bevy_ecs::world::World;
use lodestone_entity::attribute::{
    attribute_value, movement_speed_key, sprinting_modifier_id, water_movement_efficiency_key,
};
use lodestone_model::{ClientAction, PlayerCommand, PlayerInput};
use lodestone_physics::{
    CollisionView, FluidState, MovementInput, NearbyEntity, PhysicsProfile, PlayerState, PushSelf,
    Vec3d, compute_fluid_state, tick_among_entities,
};

use crate::entity::{Attributes, EntityIndex, Leashed, Position};
use crate::schedules::{Extract, GameTick};
use crate::sets::{ExtractSet, TickSet};

/// Eye height of `Pose.SWIMMING` — `EntityDimensions.scalable(0.6F, 0.6F).withEyeHeight(0.4F)`
/// (`Avatar.POSES`, shared with `FALL_FLYING` and `SPIN_ATTACK`).
pub const SWIMMING_EYE_HEIGHT: f32 = 0.4;
/// Eye height of `Pose.CROUCHING` — `1.27F` (`Avatar.POSES`).
pub const CROUCHING_EYE_HEIGHT: f32 = 1.27;
/// Horizontal free-fly speed in blocks per tick (the raw sprint key doubles
/// it). The physics engine models no creative/spectator flight, so free-fly is
/// a driver-side free-cam, not a physics mode.
pub const FLY_SPEED: f64 = 0.45;

// ---------------------------------------------------------------------------
// Components
// ---------------------------------------------------------------------------

/// Marks the entity this client *is*, as opposed to the entities it observes.
///
/// A component and not a resource on purpose: everything on this entity is
/// therefore per-client, which is what keeps a multi-client/swarm driver
/// possible later without a retrofit (azalea's whole design rests on it — see
/// `docs/bevy-migration.md` §2.2).
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LocalPlayer;

/// The bit-exact physics state carried across ticks — position, velocity, view
/// angles, `on_ground`, the swim pose, the pose eye height, status effects.
///
/// Authoritative. There is no second copy: `lodestone_shell::sim::Sim` reads
/// and writes this component through accessors and holds no `PlayerState` of
/// its own.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct PhysicsState(pub PlayerState);

/// This tick's movement intent, written in [`TickSet::Input`] and read by
/// [`TickSet::Physics`] and [`TickSet::Send`].
///
/// **One per tick, not one per frame.** Before Stage 2 this was computed once
/// per *frame*, outside the fixed-timestep loop, so a frame long enough to run
/// several catch-up ticks reused a single decision for all of them. See the
/// crate docs on `lodestone_controller::ecs::compute_movement_intent` for what
/// that changes observably.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct MovementIntent(pub MovementInput);

/// This tick's look target, in degrees, same convention as
/// [`lodestone_physics::PlayerState`]'s `yaw`/`pitch` — **distinct from the
/// camera.**
///
/// [`MovementIntent`] says which way to walk; this says which way to face,
/// and the two are not the same thing for anything that steers the player
/// programmatically. A pathfinder aims at the block it is about to break or
/// place while walking toward a waypoint several blocks past it, and a human
/// player routinely walks backward while looking forward — `MovementInput`'s
/// `forward`/`strafe` are already relative to facing for exactly this reason.
/// The camera is a third, separate thing again: it free-runs ahead of the
/// fixed 20 Hz tick for smoothness (`FrameSet::Camera`, per-frame) while this
/// is read once per tick by [`apply_look_intent`], so a camera
/// mid-interpolation and this tick's committed look direction can differ by
/// design.
///
/// Optional and additive: absent (the default — [`spawn_local_player`] does
/// not insert it), [`apply_look_intent`] is a no-op and whatever already set
/// [`PhysicsState`]'s `yaw`/`pitch` this tick — mouse-look, via the driver's
/// per-frame `apply_mouse` — is left alone. A plugin claims the rotation by
/// inserting this component on the [`LocalPlayer`] entity; there is no
/// "give it back" handshake because insertion and removal already are one
/// (`world.entity_mut(e).remove::<LookIntent>()` hands control straight back
/// to mouse-look next tick).
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct LookIntent {
    /// Degrees, vanilla convention (0 = south, increasing clockwise viewed
    /// from above).
    pub yaw: f32,
    /// Degrees, clamped to `[-90, 90]` by [`apply_look_intent`] — straight
    /// down to straight up, vanilla's own range.
    pub pitch: f32,
}

/// Write this tick's rotation from [`LookIntent`] onto [`PhysicsState`], for
/// every [`LocalPlayer`] that has one.
///
/// Ordered in [`TickSet::Intent`], before [`TickSet::Physics`]: physics reads
/// `yaw` to resolve `MovementInput`'s forward/strafe axes into a world-space
/// direction (vanilla's `getInputVector`), and the egress side reads the same
/// field to report rotation on the wire — see
/// `lodestone_controller::action::move_action`. Writing here, once, before
/// both is what makes "look" and "walk" agree for the same tick regardless of
/// which one a plugin drives.
///
/// **Does not touch `PhysicsState` at all when no [`LookIntent`] is
/// present** — not even to re-write the existing value — so a human session
/// with no plugin installed is bit-identical to before this system existed.
pub fn apply_look_intent(mut players: Query<(&mut PhysicsState, &LookIntent), With<LocalPlayer>>) {
    for (mut state, look) in &mut players {
        state.0.yaw = look.yaw;
        state.0.pitch = look.pitch.clamp(-90.0, 90.0);
    }
}

// ---------------------------------------------------------------------------
// Break intent — a plugin's wish to mine a block
// ---------------------------------------------------------------------------

/// A plugin's wish to mine a block, mirroring [`MovementIntent`]/[`LookIntent`]'s
/// "express a wish, the shell owns the machine" contract exactly.
///
/// # Why this exists
///
/// A plugin can already push a raw [`ClientAction`] onto
/// [`ActionQueue`](crate::player::ActionQueue) (`docs/plugin-api.md`), but it
/// must never push a [`ClientAction::BlockAction`] directly: the
/// block-prediction `sequence` counter, the dig state machine and the
/// post-break cooldown are owned by `lodestone_shell::interact::MiningPredictor`,
/// driven by shell-only resources (`Attacking`, the mouse-driven ray target)
/// that a plugin structurally cannot reach — a plugin depends on
/// `lodestone-ecs` and never on `lodestone-shell` (`docs/plugin-api.md`). A
/// plugin-synthesised sequence number would **fork the counter**, which
/// `docs/baritone-port.md` §3.6 forbids outright ("threaded, never
/// synthesised") — a forked sequence desynchronises block prediction against
/// the server's own acknowledgements.
///
/// So, exactly like [`LookIntent`] claims rotation without ever touching
/// `PhysicsState` itself, this claims *which block to mine* without touching
/// the predictor, the action queue or the counter directly. The shell
/// consumes it (`lodestone_shell::interact::drive_mining`) and remains the
/// **only** writer of the counter, the dig state and the cooldown — a plugin
/// only ever expresses "I would like to be mining this face of this block
/// right now."
///
/// # Two components, not one `InteractIntent`
///
/// Break and place are genuinely different state machines in vanilla: a dig
/// has progress, a cooldown, and a `sequence`-carrying
/// `START`/`STOP`/`ABORT` triple spread over many ticks, while a place is one
/// instantaneous `use_item_on`. A shared enum would make every reader
/// pattern-match a variant just to ask "is a dig in progress" — a question
/// this type's own *presence* already answers. It also mirrors a split the
/// shell already made on its own: `MiningPredictor` and `PlacementPredictor`
/// are two resources with two independent counters, not one, so a plugin's
/// intent vocabulary having the same shape is the less surprising choice, not
/// an arbitrary one.
///
/// # Optional and additive, like [`LookIntent`]
///
/// Absent (the default) changes nothing about human play — mining still runs
/// off the attack button and the mouse ray, resources a plugin cannot reach
/// at all. A plugin claims a dig by inserting this on the [`LocalPlayer`]
/// entity; removing it hands control back with no other handshake, the same
/// "insertion and removal already are one" property [`LookIntent`] documents.
///
/// **While the human attack button is held, the human path takes priority**
/// over this component. A plugin's intent left behind after it stops running
/// must never fight a real player for the same swing, and the human's own
/// input already has a dedicated, always-available seam
/// (`Attacking`/the mouse ray) that this must not shadow.
///
/// # What this cannot express, by construction
///
/// No `sequence`, no [`ClientAction::BlockAction`], no raw `ClientAction` at
/// all — only a target block and the face to approach it from, the same two
/// facts a mouse click's ray hit already carries. Everything the shell would
/// refuse anyway — out of reach, through a wall, an unresolvable block state,
/// a dead player — is rejected by `drive_mining` and reported back through
/// [`BreakOutcome`], never silently absorbed. See that type's docs for why an
/// unreported rejection would be a silent autopilot stall.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct BreakIntent {
    /// The block to mine.
    pub pos: lodestone_model::BlockPos,
    /// Which face to approach it from — decides the outward normal
    /// `ClientAction::BlockAction`'s `face` reports, exactly as a mouse ray
    /// hit would.
    pub face: lodestone_model::BlockFace,
}

/// The shell's answer to this tick's [`BreakIntent`] — the observability half
/// of the contract that component's own docs describe.
///
/// Without this, a plugin whose [`BreakIntent`] the shell silently ignores
/// (target out of reach, through a wall, an unresolvable block state, a dead
/// player) has no way to tell "digging, almost done" from "stalled and will
/// never finish" — exactly the silent-stall failure mode
/// `docs/baritone-port.md` catalogues repeatedly for a committed plan that
/// cannot observe why an edge refuses to progress.
///
/// Always present on [`LocalPlayer`] — unlike the optional, claim-semantics
/// [`BreakIntent`] — so a plugin can poll it on the very first tick without
/// first checking whether the shell has ever run with an intent installed at
/// all. [`spawn_local_player`]/[`reset_local_player`] both insert the
/// [`Default`], [`BreakStatus::Idle`].
///
/// Reflects only the **plugin's own** intent, never the human's: while the
/// human attack button is held, [`BreakIntent`] is not consulted at all (see
/// its own docs on why the human path takes priority), and this reports
/// [`BreakStatus::Idle`] for that tick regardless of how the human's own dig
/// is going — a plugin polling this while a human is playing sees "nothing to
/// report from me," never a fabricated success or failure.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BreakOutcome(pub BreakStatus);

/// See [`BreakOutcome`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BreakStatus {
    /// No [`BreakIntent`] was consulted this tick — either none is installed
    /// on the entity, or the human attack button was driving instead.
    #[default]
    Idle,
    /// The intent was accepted this tick: the dig is running (or a
    /// single-tick block just broke instantly) through the very same
    /// `MiningPredictor` a mouse-driven dig uses.
    Progressing,
    /// The shell would not act on the intent this tick, and why.
    Rejected(BreakRejection),
}

/// Why `drive_mining` would not act on a [`BreakIntent`] this tick.
///
/// Every variant here is something the shell would have refused from a mouse
/// click too — this is not a plugin-specific restriction, it is the same
/// legality the human path is already subject to, just made observable
/// because a plugin has no crosshair and no chat to notice a silent no-op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakRejection {
    /// The local player is dead (on the death screen).
    Dead,
    /// The intended block is outside vanilla's 4.5-block block-interaction
    /// range, or something else stands between the eye and it. The two
    /// collapse to one variant because both are observed the same way: a ray
    /// aimed at the intended face does not land on the intended cell.
    UnreachableOrObstructed,
    /// No live chunk data at that position — outside the loaded world, or no
    /// live connection at all.
    NoWorldData,
    /// The version has no break-time census entry for this exact block
    /// state. The shell refuses to guess a hardness rather than mine at an
    /// invented speed — the same "abort, never guess" contract
    /// `drive_mining`'s own docs already apply to the mouse-driven path.
    UnknownBlockState,
    /// A registered [`crate::veto::ActionVetoes`] predicate denied
    /// [`crate::veto::Verb::BlockBreak`] for this position.
    ///
    /// The one variant here that is **not** "something the shell would have
    /// refused from a mouse click too" — a veto applies to the human path
    /// identically, so this is a *plugin*-imposed refusal rather than a
    /// legality one, and a plugin seeing it should look for another plugin
    /// rather than for a mistake in its own intent. `ActionVetoes::names`
    /// answers "which one".
    Vetoed,
}

// ---------------------------------------------------------------------------
// Place intent — a plugin's wish to place a block, mirroring `BreakIntent`
// ---------------------------------------------------------------------------

/// A plugin's wish to place a block, mirroring [`BreakIntent`]'s "express a
/// wish, the shell owns the machine" contract for placement.
///
/// # Exactly the two facts a mouse ray hit carries
///
/// No sequence, no block-state id, no hand, no cursor sub-position — the
/// block-prediction `sequence` counter is threaded internally by
/// [`lodestone_game::placement::Placement::use_on`] (owned by
/// `lodestone_shell::interact::PlacementPredictor`, exactly as
/// [`BreakIntent`]'s own docs describe for `MiningPredictor`'s counter), the
/// hand is always [`lodestone_model::Hand::Main`] (matching every other send
/// `crate::interact::drive_placement`'s human counterpart,
/// `Sim::use_item_live`, makes), and there is no cursor sub-position for the
/// same reason `BreakIntent` has none: a plugin has no crosshair, so
/// `drive_placement` casts its own ray through `VersionData::block_outline`
/// exactly as `drive_mining` does, and the ray's own resolved hit point
/// supplies the cursor.
///
/// # Placement is instantaneous where a dig is multi-tick
///
/// This is the one real divergence from [`BreakIntent`]'s shape, and it is
/// why [`PlaceOutcome`] carries a [`PlaceOutcome::generation`] that
/// [`BreakOutcome`] has no need for. A dig is a state machine spread over
/// many ticks — `BreakIntent` stays installed for the dig's whole duration,
/// and the plugin removes it itself when it wants to stop. A placement is one
/// `use_item_on` and, at most, one local write: `drive_placement` **removes
/// this component** the moment it finishes resolving an attempt, whatever the
/// result, rather than leaving it for the plugin to clear. One insertion is
/// one attempt, and the removal is itself the acknowledgement that the attempt
/// happened — a plugin never has to guess whether a leftover `PlaceIntent` is
/// "still pending" or "processed ages ago and forgotten."
///
/// # Optional and additive, like [`BreakIntent`]
///
/// Absent (the default) changes nothing about human play. **While the human
/// use button is held, this is not consulted at all** — the human path takes
/// priority, exactly as [`BreakIntent`]'s own docs describe for the attack
/// button, and a plugin's intent left behind must never fight a real
/// right-click for the same placement.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaceIntent {
    /// The block the placement is aimed at — the cell a mouse ray's hit
    /// would report as `clicked`, not necessarily where the new block ends
    /// up (a replaceable `clicked` cell places *into* itself; otherwise the
    /// block lands across `face`, exactly
    /// [`lodestone_game::placement::resolve_target`]'s rule).
    pub pos: lodestone_model::BlockPos,
    /// Which face of `pos` to place against — decides which side of `pos` the
    /// new block lands on, exactly as a mouse ray hit's face does.
    pub face: lodestone_model::BlockFace,
}

/// The shell's answer to this tick's [`PlaceIntent`] — the observability half
/// of the contract that component's own docs describe, mirroring
/// [`BreakOutcome`].
///
/// Always present on [`LocalPlayer`], for the same reason `BreakOutcome` is:
/// a plugin must be able to poll this on the very first tick, before any
/// placement has ever run, without first checking whether the shell has ever
/// installed one. [`spawn_local_player`]/[`reset_local_player`] both insert
/// the [`Default`], generation `0` and [`PlaceStatus::Idle`].
///
/// # `generation`, and why `BreakOutcome` has nothing like it
///
/// [`BreakOutcome`] is safe to re-derive fresh every tick because a dig is
/// still running the *next* tick too — "stale" is not a concept a continuous
/// state machine needs. A placement is one-shot and its [`PlaceIntent`] is
/// gone the instant `drive_placement` resolves it (see that component's own
/// docs), so a plugin polling on some later, unrelated tick needs a way to
/// tell "this is the result of the attempt I just made" from "this is left
/// over from an attempt several ticks ago that I never read." `generation`
/// increments by exactly one every time `drive_placement` actually resolves
/// an attempt (never on a tick that only reports [`PlaceStatus::Idle`]
/// because there was nothing to attempt), so a plugin that remembers the
/// generation it last observed can tell the two apart without racing the
/// tick that produced either one.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PlaceOutcome {
    /// This tick's (or the last resolved attempt's) status.
    pub status: PlaceStatus,
    /// Bumped by exactly one every time [`Self::status`] is written to
    /// something other than a no-op [`PlaceStatus::Idle`] — see this type's
    /// own docs.
    pub generation: u64,
}

/// See [`PlaceOutcome`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlaceStatus {
    /// No [`PlaceIntent`] was consulted this tick — either none is installed
    /// on the entity, or the human use button was driving instead. Mirrors
    /// [`BreakStatus::Idle`] exactly, including the same caveat: this does
    /// **not** advance [`PlaceOutcome::generation`], so a value written by a
    /// real attempt survives every subsequent idle tick until the next real
    /// attempt overwrites it — see [`PlaceOutcome`]'s own docs on why that is
    /// what makes `generation` meaningful at all.
    #[default]
    Idle,
    /// The census resolved a concrete block state and the shell wrote it
    /// locally — the same optimistic write a human placement makes — while
    /// the `use_item_on` packet is in flight.
    Predicted,
    /// The `use_item_on` packet was sent, but nothing was written locally.
    /// **Required for honesty, not an edge case**: the census legitimately
    /// declines to predict a great many placeable blocks (any property
    /// `lodestone_shell::sim::state_for_placement` cannot resolve), and the
    /// clicked cell being interactable rather than placeable (a chest, a
    /// door) takes this same wire path too — vanilla itself always sends the
    /// packet regardless of whether anything locally changes. A plugin needs
    /// this to tell "will appear after a round trip" from
    /// [`Self::Rejected`]'s "refused, nothing sent at all."
    SentUnpredicted,
    /// The shell would not act on the intent this tick, and why. Unlike
    /// [`Self::SentUnpredicted`], nothing reached the wire.
    Rejected(PlaceRejection),
}

/// Why `drive_placement` would not act on a [`PlaceIntent`] this tick.
///
/// Every variant here is something the shell would have refused (or simply
/// never have attempted) from a real right-click too, made observable for the
/// same reason [`BreakRejection`] is: a plugin has no crosshair and no
/// inventory screen to notice a silent no-op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaceRejection {
    /// The local player is dead (on the death screen).
    Dead,
    /// The intended cell is outside vanilla's 4.5-block block-interaction
    /// range, or something else stands between the eye and it — the two
    /// collapse to one variant for the same reason
    /// [`BreakRejection::UnreachableOrObstructed`] does: a ray aimed at the
    /// intended face does not land on the intended cell.
    UnreachableOrObstructed,
    /// No live chunk data at the clicked position — outside the loaded
    /// world, or no live connection at all.
    NoWorldData,
    /// The main hand is empty, or holds an item the census cannot classify
    /// as a placeable block at all (a tool, food, anything
    /// `orientation_for_placement` declines). Unlike a real right-click —
    /// which vanilla still sends, because a non-placing item might yet
    /// interact the clicked block — a `PlaceIntent` that structurally cannot
    /// place is refused rather than spending a packet on a click the plugin
    /// did not ask for.
    NothingPlaceableHeld,
    /// The block would overlap the player's own bounding box — vanilla's own
    /// placement-legality rule, `lodestone_shell::sim::block_intersects_player`.
    IntersectsPlayer,
    /// A registered [`crate::veto::ActionVetoes`] predicate denied
    /// [`crate::veto::Verb::BlockPlace`] for the clicked position.
    ///
    /// The mirror of [`BreakRejection::Vetoed`], and the same caveat: this is a
    /// *plugin*-imposed refusal, not a legality one, so it is the one variant
    /// here that a real right-click would not also have produced.
    Vetoed,
}

// ---------------------------------------------------------------------------
// Select-slot intent — a plugin's wish to change the selected hotbar slot
// ---------------------------------------------------------------------------

/// A plugin's wish to change the selected hotbar slot, mirroring
/// [`BreakIntent`]/[`PlaceIntent`]'s "express a wish, the shell owns the
/// machine" contract — but for the *selection* rather than a block edit.
///
/// # Why this exists
///
/// [`PlaceIntent`] lets a plugin place whatever is in the **currently
/// selected** slot, and nothing lets a plugin change which slot that is.
/// [`SelectedSlot`] itself is a plain public component a plugin *could* write,
/// but writing it directly moves the shell's local read without ever telling
/// the server — the exact desync `docs/plugin-api.md` forbids, and the state
/// [`PlaceIntent`]'s held-item resolution depends on
/// (`lodestone_shell::sim::Sim::select_slot` writes the component *and* echoes
/// a `ClientAction::SetCarriedItem` precisely so the server's notion of the
/// held item keeps matching the shell's).
///
/// # Optional and additive, like [`LookIntent`]
///
/// Absent (the default — [`spawn_local_player`] does not insert it), nothing
/// changes about human play: slot changes still run off the number keys and
/// scroll wheel through `lodestone_shell::sim::Sim::select_slot`/`cycle_slot`.
/// A plugin claims the selection by inserting this on the [`LocalPlayer`]
/// entity; the shell consumes it in `lodestone_shell::interact::drive_select_slot`
/// (`TickSet::Send`), performs the same write-plus-echo `Sim::select_slot` uses,
/// and removes it — one insertion is one attempt, the same acknowledgement
/// [`PlaceIntent`] documents for its own removal.
///
/// # Why there is no `SelectSlotOutcome`
///
/// [`BreakOutcome`]/[`PlaceOutcome`] exist because a block edit has legality
/// questions the shell must answer (reach, obstruction, a dead player, a veto).
/// Selecting a slot has none: every value `0..=8` is always selectable, and an
/// out-of-range value is ignored exactly as `Sim::select_slot`'s own range gate
/// ignores one — nothing to reject, and therefore nothing to report back.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectSlotIntent(pub usize);

/// The **raw** sprint key, ungated by the forward-only/sneak rules
/// [`MovementIntent`] applies.
///
/// Only free-fly reads it: free-fly is a driver-side debug camera that is not
/// subject to the walking sprint gate, so it cannot use
/// [`MovementIntent`]'s already-gated `sprint` bit.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SprintKeyHeld(pub bool);

/// How the player's box and eye sit in water and lava this tick, from the
/// bit-exact producer (`EntityFluidInteraction.update`).
///
/// Named `Submersion` rather than `FluidState` so the component and
/// [`lodestone_physics::FluidState`] it wraps are not two things with one name.
/// Recomputed against the very view movement collided against, so the summary
/// is consistent with where the tick left the player — the submerged fog, the
/// underwater overlay and the mining `submerged` factor all read this one
/// answer rather than inventing their own boolean.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct Submersion(pub FluidState);

/// Feet position at the **start** of the most recent tick, so a per-frame
/// camera can interpolate across the fixed 20 Hz step.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct PrevPosition(pub Vec3d);

/// Whether the **developer free-fly (noclip) camera** is active instead of
/// physics-walk.
///
/// # Nothing writes this any more
///
/// The only writer was `lodestone-shell`'s `Sim::toggle_fly`, driven by a
/// Lodestone-only `key.lodestone.toggleFly` on `F`. Both were deleted: real
/// creative flight (double-tap space, server-gated, collision on) landed as
/// its own separate work, `/gamemode creative` is the route in, and the
/// binding was squatting on vanilla's `key.swapOffhand`. So this stays
/// `false` for the whole session, and [`fly_step`] is unreachable in
/// practice — deleting both is a follow-up rather than something that
/// creative-flight work covered, since it stopped at the shell boundary.
/// `interact.rs`'s `send_sprint_command` still reads the component. See
/// `docs/creative-flight.md`.
///
/// # This is not creative flight, and the two are deliberately separate
///
/// | | `Flying` (this) | [`Abilities::flying`](crate::session::Abilities::flying) |
/// |---|---|---|
/// | authority | local, and now unreachable — see above | the **server** (`player_abilities`) |
/// | collision | **off** (noclip) | **on** — vanilla creative flight collides |
/// | arithmetic | `position += dir * speed`, no velocity, no drag | vanilla `travelInAir` + the `0.6` Y overwrite |
/// | runs physics | no — [`fly_step`] replaces the whole tick | yes, `lodestone_physics::tick` |
/// | reaches the server | no | yes, echoed as `ClientAction::SetFlying` |
///
/// Creative flight is *not* implemented by flipping this bit, and doing so would
/// be wrong in both directions: it would noclip where vanilla collides, and it
/// would run non-vanilla arithmetic that the server's movement check would
/// eventually correct. Conflating them is specifically the mistake the
/// creative-flight work was written to avoid — it kept this as a distinct
/// developer affordance on purpose; removing the toggle-fly binding then
/// removed the way in, which is a different decision from merging the two.
///
/// The name is kept (rather than renamed to `NoClip`) only because
/// `lodestone-shell`'s `sim.rs` and `interact.rs` both read it and that file is
/// heavily contended; every doc comment on it now says which of the two it is.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Flying(pub bool);

/// `LocalPlayer.jumpTriggerTime`, in `LocalPlayer.aiStep` — the double-tap
/// window for toggling creative flight.
///
/// The first jump *press* while `mayfly` sets this to `7`; a second press while it
/// is still non-zero flips flight. It counts down one per tick
/// (`LocalPlayer.tick`), so the window is seven ticks — a third of a second.
///
/// Vanilla's field is an `int` and the countdown is `if (this.jumpTriggerTime > 0)
/// this.jumpTriggerTime--;`, so it saturates at `0` rather than going negative.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct JumpTriggerTime(pub i32);

/// `LocalPlayer.wasJumping` — the jump key's state *last* tick, so the flight
/// toggle can fire on the **rising edge** rather than every tick the key is held.
///
/// Without it, holding space would flip flight twenty times a second. Vanilla's
/// gate is `!wasJumping && this.input.keyPresses.jump()`.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WasJumping(pub bool);

/// Selected hotbar slot in `0..9`.
///
/// Mostly owned locally: the usual write is the player's own input (number
/// keys, scroll wheel — `lodestone_shell::sim::Sim::select_slot`/`cycle_slot`),
/// which echoes the change to the server rather than waiting for it.
///
/// **But the server can override it too.** `ClientEvent::HeldSlotChanged`
/// (`ClientboundSetCarriedItemPacket`, e.g. `/item`, or creative-mode pickup
/// into a specific slot) is a second, genuinely server-authoritative writer,
/// folded by [`crate::session::apply_local_player_state`] — this component
/// used to say there was "no server-authoritative value to fold" for exactly
/// this event, and that was the island: the fold was real
/// (`lodestone_game::player_state::HudState::select_slot`) and unit-tested,
/// and nothing fed it. The two writers do not race in practice: one runs off
/// local input in `lodestone-shell`, the other off `NetIngest`, and there is
/// no ordering between them because nothing needs one — a real server
/// override always lands as its own tick's event, not concurrently with a
/// keypress the same tick.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SelectedSlot(pub usize);

/// The last [`PlayerInput`] put on the wire, so the edge-triggered
/// player-input packet is only resent on change.
///
/// This is how the server learns we are sneaking — it derives shift from this
/// packet, never from our movement packet — so a sneak-placement against an
/// interactable block only suppresses the interaction if this was sent.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LastPlayerInput(pub Option<PlayerInput>);

/// The last sprint state put on the wire as a
/// `PlayerCommand::{StartSprinting, StopSprinting}`, mirroring vanilla's
/// `wasSprinting` (`LocalPlayer.sendIsSprintingIfNeeded`).
///
/// A **separate packet** from [`LastPlayerInput`] and both are needed:
/// `ServerboundPlayerInputPacket` only stores its `sprint` bit as
/// `ServerPlayer.lastClientInput`, while the thing that actually calls
/// `player.setSprinting(...)` is `handlePlayerCommand`. Without this the
/// server never believes we are sprinting, so its own `updateSwimming` can
/// never put us in the swimming pose.
///
/// Starts `Some(false)`, not `None`: vanilla's `wasSprinting` starts `false`,
/// so a player who joins and does not sprint sends nothing at all rather than
/// a redundant `STOP_SPRINTING` on the first tick.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct LastSprintingSent(pub Option<bool>);

/// The last creative-flight state put on the wire as a
/// [`ClientAction::SetFlying`](lodestone_model::ClientAction::SetFlying), mirroring
/// vanilla's `Player.onUpdateAbilities()` →
/// `ServerboundPlayerAbilitiesPacket`.
///
/// # Why the echo is not optional
///
/// The client toggles `abilities.flying` **locally** and tells the server
/// afterwards. Vanilla sends this on every toggle edge
/// (`LocalPlayer.aiStep` calls `onUpdateAbilities()` at both the engage and the
/// landing-cancel sites). Without it the server still believes we are walking,
/// its own `travel` replay disagrees with the position we claim, and
/// `ServerGamePacketListenerImpl.handleMovePlayer` corrects us — or, once we have
/// been unsupported in open air for `getMaximumFlyingTicks`, disconnects us with
/// `multiplayer.disconnect.flying`. The kick message is not a coincidence: it is
/// literally the anti-cheat for this.
///
/// `ClientAction::SetFlying` was itself an island before this component landed
/// — encoded by four
/// protocol adapters (`v47`, `v340`, `v735`, `v770`) with **zero** producers
/// anywhere outside their own tests. This component and
/// `lodestone_shell::interact::send_abilities` are its first consumer.
///
/// Starts `Some(false)` for the same reason [`LastSprintingSent`] does: a player
/// who joins and never flies sends nothing at all, rather than a redundant
/// "not flying" on the first tick.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct LastFlyingSent(pub Option<bool>);

/// Present while the local player is dead and awaiting the server-confirmed
/// respawn.
///
/// A marker, so "alive" is the absence of a component rather than a `false`
/// nobody has to remember to clear. Death is a transient *state*, not the end
/// of the session — the client answers the death packet with a respawn — but
/// while it holds, the corpse does not walk: [`MovementIntent`] is forced to
/// [`MovementInput::NONE`] and the movement packet is withheld until the
/// post-respawn placement teleport lands.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Dead;

/// Ticks since the local player's last attack — vanilla's `attackStrengthTicker`
/// (declared on `LivingEntity`, incremented in `Player.tick`).
///
/// Counts up from `0`, uncapped: vanilla lets the raw field overshoot the
/// weapon's delay indefinitely once the cooldown is long since full, and
/// [`crate::TickSet::Animate`]'s [`tick_attack_strength`] does the same —
/// callers derive the clamped `0.0..=1.0` fraction (see
/// `lodestone_shell::sim::Sim::attack_strength_scale`, the reader this exists
/// for) rather than the ticker clamping itself.
///
/// **Local-only**, like [`SelectedSlot`]: nothing server-authoritative resets
/// or reports it — the wire `Attack` packet carries only the target entity id
/// (`docs/combat.md`), never a strength scalar — so there is no fold to guard
/// against a second writer the way [`crate::session::Vitals`] has to.
/// [`spawn_local_player`]/[`reset_local_player`] start it at `0`, matching
/// `Player`'s bare (zero-initialised) `int` field; the reset on an actual
/// attack is the caller's job (`Sim::attack_entity`), mirroring vanilla's
/// `MultiPlayerGameMode.attack` calling `player.resetAttackStrengthTicker()`
/// itself rather than `Player.attack` doing it unconditionally.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AttackStrengthTicker(pub u32);

// ---------------------------------------------------------------------------
// The collision seam
// ---------------------------------------------------------------------------

/// Somewhere a `&dyn CollisionView` can be *borrowed from*, rather than a view
/// itself.
///
/// The inversion is what makes collision geometry reachable from a scheduled
/// system at all — see this module's docs. Implementors own whatever the view
/// borrows (a snapshot of the live server terrain, an owned copy of an offline
/// world), which is why the trait is `Send + Sync + 'static`: those are
/// `Resource`'s requirements, not physics's.
///
/// Implementations live in the driver (`lodestone-shell`), because the mapping
/// from block ids to shapes is the driver's business and this crate must not
/// depend on the renderer (`docs/bevy-migration.md` §4.4).
pub trait CollisionSource: std::fmt::Debug + Send + Sync + 'static {
    /// Build a view and hand it to `f`. Called once per physics tick.
    fn with_view(&self, f: &mut dyn FnMut(&dyn CollisionView));
}

/// What this tick's physics collides against. Written by the driver once per
/// tick, before `GameTick` runs.
///
/// The two non-`View` variants are both "hold the player still", and they
/// differ in exactly one thing — whether the pose is still updated. That
/// asymmetry is inherited verbatim from the pre-Stage-2 code and is preserved
/// deliberately rather than tidied, because tidying it would change the eye
/// height (and therefore the camera) on the title screen. It is a latent
/// question, not a settled one.
#[derive(Resource, Debug, Clone, Default)]
pub enum PlayerCollision {
    /// No session **and** no offline terrain: there is nothing to stand on and
    /// nobody to be. Freeze, and do not even update the pose — a driver steps
    /// the sim on every frame including while a menu owns the screen, so
    /// without this the pre-session player free-falls through an empty world
    /// for as long as the title screen is up and then carries that velocity
    /// into the login teleport's first tick.
    #[default]
    NoWorld,
    /// A live session whose player column has not streamed in yet. Freeze —
    /// as vanilla waits for chunks — rather than falling through absent ground
    /// and rubber-banding against the server's corrective teleports. Unlike
    /// [`Self::NoWorld`] the pose *is* updated.
    Pending,
    /// Collide against this.
    View(Arc<dyn CollisionSource>),
}

// ---------------------------------------------------------------------------
// Resources
// ---------------------------------------------------------------------------

/// The physics tuning profile (`PhysicsProfile::mc_1_21()` in practice).
///
/// A resource and not a component: it is a property of the *world's* rules,
/// identical for every player in it, and a per-entity copy would invite two
/// entities in one world to be simulated under different physics.
#[derive(Resource, Debug, Clone)]
pub struct Profile(pub PhysicsProfile);

impl Default for Profile {
    fn default() -> Self {
        Self(PhysicsProfile::mc_1_21())
    }
}

/// This tick's entity-push neighbourhood — every nearby entity
/// [`lodestone_physics::push::apply_entity_push`] should test the local
/// player against, refreshed by the driver once per tick before `GameTick`
/// runs. Same pattern as [`PlayerCollision`]: the *decision* (which entities,
/// how their boxes are sized) is the shell's, because it owns the ECS world
/// query and whatever per-type geometry it can resolve, but the snapshot is
/// handed to the ECS as an owned `Vec` so [`player_physics`] can stay a plain
/// scheduled system rather than reaching back into the world itself.
///
/// Empty (the [`Default`]) reproduces prior behaviour exactly: passing an
/// empty slice to [`lodestone_physics::tick_among_entities`] is bit-for-bit
/// [`lodestone_physics::tick`] (`apply_entity_push` returns immediately), so a
/// driver that never populates this — every existing test harness, and
/// `--headless` — sees no behaviour change at all.
#[derive(Resource, Debug, Clone, Default)]
pub struct NearbyEntities {
    /// The neighbourhood itself.
    pub list: Vec<NearbyEntity>,
    /// The local player's own scoreboard-team `CollisionRule` — `Entity.getTeam()`
    /// resolved for *us*, the other half of the team gate
    /// [`lodestone_physics::push::pair_admitted`] applies (each [`NearbyEntity`]
    /// in `list` carries the neighbour's own half already). Threaded into
    /// [`lodestone_physics::push::PushSelf::collision_rule`] at the same call
    /// site `list` is threaded into
    /// [`lodestone_physics::push::apply_entity_push`]'s neighbour slice.
    ///
    /// `Default` is [`lodestone_physics::push::CollisionRule::Always`] — a
    /// team-less player, vanilla's own resolution for `ownTeam == null` — so a
    /// driver that never populates this sees no behaviour change, exactly as
    /// `list`'s own empty default does not.
    pub self_collision_rule: lodestone_physics::push::CollisionRule,
}

/// Vanilla's `Options.autoJump`, pushed down by the driver once per tick and
/// carried into [`PlayerState::auto_jump_enabled`] by [`player_physics`] —
/// **the actual defect behind auto-jump silently staying on**.
///
/// # Why this resource exists at all
///
/// `lodestone_physics`'s [`update_auto_jump`](lodestone_physics) is a complete,
/// exact port of `LocalPlayer.updateAutoJump`, and its one gate is
/// `PlayerState::auto_jump_enabled` — which defaults to `true` (vanilla's own
/// default) and whose only setter, `PlayerState::with_auto_jump`, was called
/// **from tests only**. So the shell's settings toggle read OFF, the shell's own
/// *second*, simplified probe was correctly suppressed by it, and the real
/// detector armed a jump anyway: the option could not turn auto-jump off. The
/// duplicate probe is gone and this is the one seam.
///
/// # Default is `true`, deliberately
///
/// It matches both vanilla's option default and [`PlayerState`]'s own field
/// default, so a harness that adds [`LocalPlayerPlugin`] without pushing the
/// option (`lodestone-controller`'s tests, the offline fixture world, every
/// golden trace) behaves exactly as it did before this resource existed. The
/// *shell's* `Options::auto_jump` defaults to `false`; that is the shell's
/// choice to make, and it now actually reaches here.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoJump(pub bool);

impl Default for AutoJump {
    fn default() -> Self {
        Self(true)
    }
}

/// Whether some equipment slot currently holds a glider (an elytra), pushed
/// down by the driver once per tick — the one conjunct of
/// [`lodestone_physics::can_glide`] that is equipment data rather than physics
/// state.
///
/// Vanilla walks `EquipmentSlot.VALUES` looking for a `DataComponents.GLIDER`
/// component (`LivingEntity.canGlideUsing`); the
/// driver resolves that from whatever inventory model it has and hands the
/// answer here. Default `false` — a harness that never pushes it simply never
/// glides, which is the safe direction.
#[derive(Resource, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GliderEquipped(pub bool);

/// Ticks of firework-rocket boost still owed to a gliding player.
///
/// Set by the driver when the local player uses a firework rocket while
/// gliding; spent one per tick by [`tick_firework_boost`], which calls
/// [`lodestone_physics::apply_firework_boost`] for as long as it is non-zero
/// **and** the player is still fall-flying.
///
/// # Why a countdown and not a tracked entity
///
/// Vanilla's boost is applied by the `FireworkRocketEntity`'s own `tick`
/// for as long as that entity is attached
/// and alive, and the client learns the attachment from the rocket's
/// `DATA_ATTACHED_TO_TARGET` entity data. This client does not decode that
/// field, so the duration is predicted locally instead — see the driver's own
/// doc for the exact lifetime it uses and what part of vanilla's is
/// unpredictable.
#[derive(Resource, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FireworkBoost(pub u32);

/// How many ticks the local player has been holding the use button *with an
/// item in progress*, or `None` when nothing is being used.
///
/// This is `getUseDuration() - getUseItemRemainingTicks()` — vanilla's
/// `timeHeld`, which `TridentItem.releaseUsing` compares against its `10`-tick
/// `THROW_THRESHOLD_TIME`. The driver arms it at the
/// press edge and reads it at the release edge; [`tick_item_use`] advances it.
///
/// A **count of ticks, not a clock**: `Instant::now()` panics on wasm32 under
/// this workspace's `panic = "abort"` browser profile, and every duration in
/// the tick domain here is measured in 20 Hz ticks for that reason.
#[derive(Resource, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ItemUseTicks(pub Option<u32>);

/// Edge tracker for the glide start: last tick's jump input, as
/// `LocalPlayer.aiStep` holds `wasJumping`.
///
/// A **separate** latch from [`WasJumping`], which
/// [`apply_creative_flight_input`] overwrites at the end of its own body — a
/// later system in the same tick reading that one would see this tick's value
/// and never observe an edge at all. Two consumers of the same edge need two
/// latches.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WasJumpingGlide(pub bool);

/// Whether the `START_FALL_FLYING` player command for the current glide has
/// been sent, so [`send_fall_flying_command`] sends exactly one per glide —
/// the same shape as [`LastSprintingSent`].
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FallFlyingSent(pub bool);

/// The one sanctioned egress: actions produced by systems this tick, drained
/// by the driver and handed to the socket.
///
/// A plugin reaches the wire by pushing here from a `GameTick` system, never
/// by touching a connection (`docs/bevy-migration.md` §6). Order is send
/// order, so a system's position in [`TickSet::Send`] is observable on the
/// wire.
#[derive(Resource, Debug, Clone, Default)]
pub struct ActionQueue(pub Vec<ClientAction>);

/// Whether this tick's outbound player packets are meaningful at all.
///
/// A *derived* gate the driver refreshes each frame from its own session state
/// — not a second copy of that state. It exists because the edge-trackers
/// ([`LastPlayerInput`], [`LastSprintingSent`]) must not latch a value that
/// was never actually sent: a system that ran while disconnected would record
/// the current input as "already sent", and the first real change after
/// connecting would then be suppressed as a redundant resend.
///
/// Stage 3 moved session phase onto the local player as
/// [`crate::session::Phase`], and the note that used to sit here predicted this
/// resource would collapse into it. It did not, and the reason is worth keeping:
/// `in_world` *is* now derived from that component, but `live` is
/// `vanilla_atlas.is_some() && net.is_some()` — whether the session is rendering
/// a real server world with vanilla assets — which is an asset/config fact and
/// not a phase. Two bits, two origins, one derived gate.
/// One coloured world-space line segment, for a plugin's debug-geometry
/// channel (see [`DebugLines`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DebugLine {
    pub start: Vec3d,
    pub end: Vec3d,
    /// Linear RGBA, `0.0..=1.0`.
    pub color: [f32; 4],
}

/// World-space debug geometry a plugin wants drawn this frame — a
/// pathfinder's planned route, a reachability probe, anything otherwise
/// invisible and therefore undebuggable (`CLAUDE.md`'s island rule: "nothing
/// is done until something on screen changes").
///
/// A plugin reaches the screen by pushing here from a system ordered
/// `.in_set(ExtractSet::Debug)`, mirroring how [`ActionQueue`] is the one
/// sanctioned way to reach the wire. [`clear_debug_lines`] empties it before
/// that set runs each frame — ordered `.before(ExtractSet::Debug)`, not
/// `.in_set` it, specifically so it can never race a plugin's own writer for
/// a position within the set — so a plugin only ever appends this frame's
/// geometry, never last frame's leftovers.
///
/// This lives on `LocalPlayerPlugin` rather than a set-specific plugin of its
/// own for a build-topology reason, not a conceptual one: it is the plugin
/// already wired into every shipped `App` (`lodestone_shell::sim::Sim`'s
/// `app.add_plugins((CorePlugin, LocalPlayerPlugin, ControllerPlugin, ...))`),
/// so extending it is what reaches a running client without a driver-crate
/// change. The render half — turning this resource into pixels — is
/// `lodestone_shell::gpu`'s `DebugLineRenderer` / `DebugLinesSource`; see its
/// module docs for the one remaining wire (installing the source) that is
/// out of scope for this crate to make.
#[derive(Resource, Debug, Clone, Default)]
pub struct DebugLines(pub Vec<DebugLine>);

/// Empty [`DebugLines`] before this frame's `ExtractSet::Debug` systems run.
/// See that resource's docs for why this is `.before(ExtractSet::Debug)`
/// rather than a member of the set.
pub fn clear_debug_lines(mut lines: ResMut<DebugLines>) {
    lines.0.clear();
}

/// `Extract` / `ExtractSet::Debug`: one [`DebugLine`] per leashed mob, from the
/// mob's own [`Position`] to its holder's — the last hop of the leashing
/// chain, closed through the cheapest channel already wired to the screen
/// every frame.
///
/// # Why `DebugLines` and not a dedicated render pass
///
/// [`DebugLines`] is a generic per-frame world-space geometry channel — its
/// own doc names a pathfinder route and a reachability probe as existing
/// uses — and its render pass (`lodestone_shell::gpu`'s `DebugLineRenderer`)
/// already runs unconditionally, every frame, regardless of any F3 toggle:
/// gating, where it exists (the hitbox/chunk-border overlays), lives in the
/// *systems that choose to populate the channel*, not in the channel or its
/// pipeline. So a system here that always pushes a line for every leashed
/// mob is genuinely always visible, at zero additional GPU-pipeline wiring —
/// no new bind group, shader or `app.rs` install call, all of which sit in
/// choke-point files this pass has no need to touch.
///
/// **This is a disclosed simplification, not vanilla parity.** No catenary
/// sag (`Leashable.tickLeash`'s per-segment curve), no rope texture — a
/// straight, flat-coloured line — and both endpoints are the raw
/// (non-interpolated) tick position, so a leashed mob's rope can lag its
/// eased render position by up to one tick. A future pass that wants
/// vanilla's actual rope needs a real pipeline in `lodestone-render`/
/// `lodestone_shell::gpu`, not this channel; this is what makes the leash
/// **visible** rather than invisible, which was the reported gap.
///
/// # Resolving the holder's position
///
/// [`Leashed`] carries a wire entity id, resolved through [`EntityIndex`]
/// exactly as every other id-addressed ingest fold resolves one. Two shapes,
/// tried in order: `positions` (another leashed-capable mob, or any other
/// entity with a [`Position`]), then `local_player` (`With<LocalPlayer>`,
/// reading [`PhysicsState`] instead) — because the local player's own ingest
/// entity deliberately carries **no** [`Position`] component
/// (`apply_local_player_login`'s own doc explains why: it would be a second
/// copy of `PhysicsState`), which is the common case for "a player holds the
/// mob's lead".
pub fn push_leash_lines(
    index: Res<EntityIndex>,
    leashed: Query<(&Position, &Leashed)>,
    positions: Query<&Position>,
    local_player: Query<&PhysicsState, With<LocalPlayer>>,
    mut lines: ResMut<DebugLines>,
) {
    // A plain leather-brown, in the same ballpark as vanilla's `lead.png` —
    // there is no per-texel sampling to match since this pass has no texture
    // at all, so one flat colour is the whole of "what a lead looks like" here.
    const LEAD_COLOR: [f32; 4] = [0.42, 0.29, 0.16, 1.0];
    for (position, leashed) in &leashed {
        let Some(holder_id) = leashed.0 else {
            continue;
        };
        let Some(holder_entity) = index.get(holder_id) else {
            continue;
        };
        let holder_pos = if let Ok(holder_position) = positions.get(holder_entity) {
            holder_position.0
        } else if let Ok(physics) = local_player.get(holder_entity) {
            let p = physics.0.position;
            lodestone_model::Vec3::new(p.x, p.y, p.z)
        } else {
            // Holder resolved to an `Entity` (it is indexed) but has neither
            // shape of position component yet — e.g. a spawn/login batch
            // whose ordering has not landed this component this tick.
            // Skipping for one frame is honest; a stale guess would not be.
            continue;
        };
        lines.0.push(DebugLine {
            start: Vec3d::new(position.0.x, position.0.y, position.0.z),
            end: Vec3d::new(holder_pos.x, holder_pos.y, holder_pos.z),
            color: LEAD_COLOR,
        });
    }
}

#[derive(Resource, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Egress {
    /// The server has placed us in the world (`SessionPhase::Connected`), so a
    /// movement packet is meaningful.
    pub in_world: bool,
    /// …and this is a real live session, so the interaction/edge packets are
    /// meaningful too.
    pub live: bool,
}

// ---------------------------------------------------------------------------
// Systems
// ---------------------------------------------------------------------------

// `Player.updatePlayerPose` used to be re-implemented here, as an eye-height-only
// approximation, because `lodestone-physics` modelled no pose box. It does now:
// `lodestone_physics::pose::update_player_pose` runs vanilla's real fit gate at
// the tail of `tick`/`tick_among_entities` and commits box and eye height
// together. Deciding the pose a second time in this crate could only *disagree*
// with that gate — specifically by overwriting a fit-forced crouch (shift not
// held, but the ceiling too low to stand) with a standing `1.62` eye. The
// `Pending` arm below still seeds a pose, because it has no `CollisionView` for
// the gate to consult.

/// One free-fly tick: move horizontally relative to yaw, vertically with
/// jump/sneak, ignoring gravity and collision.
///
/// A driver-side camera, not a physics model — the engine has no flight.
fn fly_step(
    player: &mut PlayerState,
    intent: MovementInput,
    sprint_key: bool,
    fluid: &mut FluidState,
) {
    let speed = if sprint_key {
        FLY_SPEED * 2.0
    } else {
        FLY_SPEED
    };
    let yaw = f64::from(player.yaw).to_radians();
    let (sy, cy) = yaw.sin_cos();
    let f = f64::from(intent.forward);
    let s = f64::from(intent.strafe);
    // vanilla `getInputVector` with pitch ignored: horizontal move only.
    let mut dx = s * cy - f * sy;
    let mut dz = f * cy + s * sy;
    let len = (dx * dx + dz * dz).sqrt();
    if len > 1.0 {
        dx /= len;
        dz /= len;
    }
    player.position.x += dx * speed;
    player.position.z += dz * speed;
    if intent.jump {
        player.position.y += speed;
    }
    if intent.sneak {
        player.position.y -= speed;
    }
    player.velocity = Vec3d::ZERO;
    player.on_ground = false;
    // Free-fly is a debug camera, not a physics pose, so it never drives
    // submerged fog — noclipping through an ocean should not tint the whole
    // view. Real submersion resumes the moment physics-walk does.
    *fluid = FluidState::NONE;
    // `Player.updateSwimming` forces `setSwimming(false)` while
    // `abilities.flying`. Free-fly never calls
    // `lodestone_physics::tick`, so nothing would otherwise clear a swim pose
    // entered before taking off — the player would fly around with a 0.4 eye
    // height.
    player.swimming = false;
    // The box half of the same reset. Free-fly never calls `tick`, so nothing
    // else clears a pose entered before taking off — a player who dives, starts
    // swimming and then flies would otherwise keep the `0.6 × 0.6` swimming box
    // for the whole flight, and get it back on landing.
    player.pose = lodestone_physics::Pose::Standing;
    player.eye_height = lodestone_physics::player::DEFAULT_EYE_HEIGHT;
}

/// Vanilla `EntityFluidInteraction.update` for the local player against
/// `view`.
fn player_fluid_state(
    player: &PlayerState,
    profile: &PhysicsProfile,
    view: &dyn CollisionView,
) -> FluidState {
    compute_fluid_state(
        player.bounding_box(profile),
        player.position,
        player.eye_height,
        view,
    )
}

/// One fixed physics tick for every [`LocalPlayer`], in [`TickSet::Physics`].
///
/// The `MOVEMENT_SPEED` attribute is injected each tick via
/// [`PlayerState::with_movement_speed`] — exercising the attribute seam the
/// physics crate exposes from a *real* caller, not a test. When sprinting we
/// hand in `base·(1 + sprint_modifier)`; the engine then ignores its own
/// sprint speed maths (no double-count) while the sprint flag still drives the
/// sprint jump boost.
///
/// `WATER_MOVEMENT_EFFICIENCY` (Depth Strider) is injected the same way, via
/// [`PlayerState::with_water_movement_efficiency`], folded each tick from the
/// [`Attributes`] component through [`attribute_value`]'s vanilla three-stage
/// `calculateValue` (`docs/swimming.md`). `Attributes` is `Option`al because it
/// is only inserted on `ClientEvent::Login`
/// (`lodestone_ecs::ingest::apply_local_player_login`) — the offline demo
/// world and the pre-login title-screen player carry no attribute snapshot at
/// all, and [`attribute_value`] already reads "no snapshot for this key" as
/// the registry default (`0.0`), so `None` here folds to the same inert value
/// an empty snapshot list would.
///
/// [`PrevPosition`] is captured here rather than by the driver so that a
/// plugin adding a second `GameTick` system cannot desynchronise the camera's
/// interpolation anchor from the tick that actually moved the player.
pub fn player_physics(
    collision: Res<PlayerCollision>,
    profile: Res<Profile>,
    nearby: Res<NearbyEntities>,
    auto_jump: Res<AutoJump>,
    mut players: Query<
        (
            &mut PhysicsState,
            &mut Submersion,
            &mut PrevPosition,
            &MovementIntent,
            &Flying,
            &SprintKeyHeld,
            Option<&Attributes>,
            Option<&crate::session::Abilities>,
        ),
        With<LocalPlayer>,
    >,
) {
    let profile = &profile.0;
    for (mut state, mut fluid, mut prev, intent, flying, sprint_key, attributes, abilities) in
        &mut players
    {
        prev.0 = state.0.position;
        let player = &mut state.0;
        let intent = intent.0;

        // **The server-authoritative half of creative flight.** `Abilities` is folded from
        // `ClientEvent::AbilitiesChanged` by `crate::session::apply_local_player_state`;
        // this is the line that carries it into physics, and without it the whole
        // chain — decode, switch arm, component, fold — reaches zero pixels.
        //
        // `Option` because `spawn_local_player` alone does not insert the session
        // set (`lodestone-controller`'s tests and the offline fixture world call it
        // bare). Absent folds to `Abilities::default()`, i.e. **not flying** — the
        // safe direction, and the same reasoning `Attributes` below uses for its
        // own `None`.
        let abilities = abilities.copied().unwrap_or_default();
        *player = player.with_flight(abilities.flying, abilities.flying_speed);

        // **The auto-jump fix's one real line.** Same shape as `with_flight` above, and
        // for the same reason: the option lives outside physics, physics owns the
        // detector, and this is the seam. Pushed every tick rather than at spawn
        // so toggling Auto-Jump in the settings screen applies on the very next
        // one — and, more importantly, so nothing can leave the field at its
        // `true` default while the option reads OFF, which is exactly what made
        // auto-jump un-disableable. See [`AutoJump`].
        //
        // Note this is applied **before** the `flying` / `NoWorld` early returns
        // below on purpose: those paths do not run the detector at all, so the
        // field's value there is unobservable, but a `continue` above the write
        // would leave a stale value behind for the tick flight ends.
        *player = player.with_auto_jump(auto_jump.0);

        if flying.0 {
            // The *debug* free-fly camera, not creative flight — see [`Flying`].
            fly_step(player, intent, sprint_key.0, &mut fluid.0);
            continue;
        }

        if matches!(*collision, PlayerCollision::NoWorld) {
            player.velocity = Vec3d::ZERO;
            player.on_ground = true;
            fluid.0 = FluidState::NONE;
            continue;
        }

        // The walk speed is the server-reported
        // `minecraft:movement_speed`, which is what makes Speed, Slowness, Soul
        // Speed and boot enchantments reach physics at all: vanilla folds every
        // one of them into this attribute **server-side**
        // (`LivingEntity.onEffectAdded` is `!isClientSide()`-gated) and syncs the
        // result, so this single read covers the lot without a client-side
        // effect-to-modifier translation.
        //
        // **Deliberately not `attribute_value` alone.** Its no-snapshot fallback
        // is `default_def`'s `movement_speed` = `0.7`, which is vanilla's
        // *generic mob* default from `createMobAttributes` — the player's base is
        // `0.1`. Using it would make an offline world, and every frame before the
        // first attributes packet, walk **seven times too fast**. So a missing
        // snapshot is tested for explicitly and answered from the profile.
        let key = movement_speed_key();
        let snapshot = attributes.and_then(|attrs| {
            attrs.0.iter().find(|snapshot| snapshot.attribute == key)
        });
        let base = match snapshot {
            Some(snapshot) => attribute_value(std::slice::from_ref(snapshot), &key),
            None => f64::from(profile.base_movement_speed),
        };
        // Vanilla has no sprint arithmetic in `travel`: `Player.aiStep` reads the
        // folded attribute and `LivingEntity.setSprinting`
        // puts a transient `minecraft:sprinting` (+0.3 `ADD_MULTIPLIED_TOTAL`)
        // modifier on it. Our sprint is client-predicted from the local
        // double-tap, and the server's modifier only arrives a `PlayerCommand`
        // round trip later — so the local multiply covers exactly that window and
        // stops the moment the real modifier shows up in the snapshot. Applying
        // both would compound to ~1.69x instead of 1.3x, which is the trap this
        // branch exists to avoid.
        let sprint_already_folded = snapshot.is_some_and(|snapshot| {
            let sprinting = sprinting_modifier_id();
            snapshot
                .modifiers
                .iter()
                .any(|modifier| modifier.id == sprinting)
        });
        let attr = if intent.sprint && !sprint_already_folded {
            base * (1.0 + f64::from(profile.sprint_speed_modifier))
        } else {
            base
        };
        *player = player.with_movement_speed(attr);

        let efficiency = attributes.map_or(0.0, |attrs| {
            attribute_value(&attrs.0, &water_movement_efficiency_key())
        });
        *player = player.with_water_movement_efficiency(efficiency as f32);

        if let PlayerCollision::View(source) = &*collision {
            source.with_view(&mut |view| {
                // `tick_among_entities` with an empty `nearby` is bit-for-bit
                // `tick` — see [`NearbyEntities`]'s own doc for why that makes
                // this swap provably inert for every caller that does not
                // populate the resource.
                tick_among_entities(
                    player,
                    intent,
                    view,
                    profile,
                    &nearby.list,
                    PushSelf {
                        collision_rule: nearby.self_collision_rule,
                        ..PushSelf::LIVING_PLAYER
                    },
                );
                // The same view movement collided against, so the submerged
                // summary is consistent with where the tick left the player.
                fluid.0 = player_fluid_state(player, profile, view);
            });
        } else {
            // `Pending`: we know nothing about the fluid around the player, so
            // report "dry" rather than stranding a stale submerged fog from
            // before the column went away.
            player.velocity = Vec3d::ZERO;
            player.on_ground = true;
            fluid.0 = FluidState::NONE;
            // No `CollisionView`, so there is nothing to gate the pose against.
            // `with_pose` commits box *and* eye height together — the pair
            // vanilla's `refreshDimensions` always writes at once — so this
            // cannot leave a `0.6` box wearing a `1.62` eye.
            //
            // The `View` arm deliberately does **not** do this: `tick_among_
            // entities` ends in `update_player_pose`, which runs vanilla's fit
            // gate. Re-deciding the pose here from `desired_pose` alone would
            // overwrite a fit-forced crouch with a standing eye height.
            *player = player.with_pose(lodestone_physics::desired_pose(player, intent));
        }
    }
}

/// `TickSet::Physics`, **before** [`player_physics`]: the client half of creative
/// flight — `LocalPlayer.aiStep`'s double-tap toggle and vertical impulse.
///
/// # Order is load-bearing and matches vanilla exactly
///
/// Everything here happens *before* `super.aiStep()` in vanilla, and
/// `super.aiStep()` is what contains the travel dispatch. So the toggle and the
/// vertical impulse both land on the velocity that [`player_physics`] then
/// integrates this same tick. The landing cancel is the mirror image — it is
/// *after* `super.aiStep()` — and therefore lives in a separate system,
/// [`cancel_flight_on_landing`].
///
/// # The toggle
///
/// ```text
/// if (abilities.mayfly && !wasJumping && jump() && !wasAutoJump) {
///    if (jumpTriggerTime == 0) jumpTriggerTime = 7;
///    else if (!isSwimming()) { abilities.flying = !abilities.flying; jumpTriggerTime = 0; }
/// }
/// ```
///
/// **`may_fly` is the whole point.** It is the server's grant, and it is why this
/// cannot be a local toggle: on a survival server `may_fly` is `false`, this
/// system does nothing, and pressing space twice jumps twice.
///
/// # The vertical impulse uses the **raw** ability speed
///
/// `inputYa * abilities.getFlyingSpeed() * 3.0F` — *not* the sprint-doubled value
/// `Player.getFlyingSpeed()` returns. Sprinting doubles horizontal flight and
/// leaves the climb rate alone. Reusing
/// [`lodestone_physics::player_flying_speed`] here would sprint-double it and
/// climb twice as fast as vanilla.
///
/// # Divergences, deliberate
///
/// * **`isControlledCamera()`** is vacuously true — this engine has no camera
///   possession.
/// * **The one-shot hop on engaging flight while standing** (`if (abilities.flying
///   && this.onGround()) this.jumpFromGround();`) is **not** modelled:
///   `jump_from_ground` is private to `lodestone-physics` and needs a
///   `CollisionView` for `getBlockJumpFactor`, which this system does not hold.
///   The cost is a slightly less snappy takeoff from the ground (vanilla gets a
///   one-tick `+0.42` Y); flight itself is unaffected because the impulse below
///   fires on the same tick. Pinned by
///   `takeoff_from_the_ground_does_not_model_vanillas_one_shot_hop`.
/// * **Auto-jump** (`wasAutoJump`) does not exist here, so its `!` is vacuous.
/// * **`!isSwimming()`** *is* modelled, because a sprint-swimmer double-tapping
///   space would otherwise take off mid-stroke.
pub fn apply_creative_flight_input(
    mut players: Query<
        (
            &mut PhysicsState,
            &mut crate::session::Abilities,
            &mut JumpTriggerTime,
            &mut WasJumping,
            &MovementIntent,
            Option<&Dead>,
        ),
        With<LocalPlayer>,
    >,
) {
    for (mut state, mut abilities, mut trigger, mut was_jumping, intent, dead) in &mut players {
        let jump = intent.0.jump;
        // `LocalPlayer.tick`'s countdown, saturating at zero.
        trigger.0 = trigger.0.saturating_sub(1).max(0);

        // A dead player is on the death screen and drives no input; `wasJumping`
        // still latches so the first press after respawn is a genuine rising edge
        // rather than a level that was already held.
        if dead.is_none() && abilities.may_fly && !was_jumping.0 && jump {
            if trigger.0 == 0 {
                trigger.0 = 7;
            } else if !state.0.swimming {
                abilities.flying = !abilities.flying;
                trigger.0 = 0;
            }
        }
        was_jumping.0 = jump;

        if abilities.flying && dead.is_none() {
            let mut input_ya = 0i32;
            if intent.0.sneak {
                input_ya -= 1;
            }
            if jump {
                input_ya += 1;
            }
            if input_ya != 0 {
                // The `f32` product widened to `f64` by `Vec3.add`, exactly as
                // vanilla: `inputYa * abilities.getFlyingSpeed() * 3.0F` is a
                // `float` expression before it reaches the `double` vector.
                let impulse = input_ya as f32 * abilities.flying_speed * 3.0;
                state.0.velocity.y += f64::from(impulse);
            }
        }
    }
}

/// `TickSet::Physics`, **after** [`player_physics`]: landing cancels creative
/// flight (`LocalPlayer.aiStep`'s tail).
///
/// ```text
/// super.aiStep();
/// if (this.onGround() && abilities.flying && !this.minecraft.gameMode.isSpectator()) {
///    abilities.flying = false;
///    this.onUpdateAbilities();
/// }
/// ```
///
/// A **separate system from [`apply_creative_flight_input`]** precisely because
/// vanilla puts it on the other side of `super.aiStep()`: `onGround` is written by
/// the move this tick, so reading it before the move would test *last* tick's
/// landing and cancel flight one tick early — visible as flight cutting out just
/// before you touch down.
///
/// The `!isSpectator()` conjunct is honoured through
/// [`ServerGameMode`](crate::session::ServerGameMode): a spectator stays flying.
/// That is the *only* part of spectator mode this crate models — see
/// `docs/creative-flight.md`'s "Spectator is deferred".
pub fn cancel_flight_on_landing(
    mut players: Query<
        (
            &PhysicsState,
            &mut crate::session::Abilities,
            &crate::session::ServerGameMode,
        ),
        With<LocalPlayer>,
    >,
) {
    for (state, mut abilities, game_mode) in &mut players {
        let spectator = game_mode.0 == Some(lodestone_model::common::GameMode::Spectator);
        if state.0.on_ground && abilities.flying && !spectator {
            abilities.flying = false;
        }
    }
}

/// `TickSet::Physics`, **before** [`player_physics`]: start an elytra glide on
/// the jump-key rising edge, and end one whose preconditions have lapsed.
///
/// Two halves of one vanilla pair, deliberately in one system because they must
/// see the same `on_ground`:
///
/// * the **start** is `LocalPlayer.aiStep`'s `if (input.jump() && !wasJumping &&
///   !onClimbable() && tryToStartFallFlying())`,
///   which is client-authoritative — the client sets the shared flag itself and
///   tells the server afterwards ([`send_fall_flying_command`] is that telling);
/// * the **stop** is `LivingEntity.updateFallFlying`'s `!canGlide()` branch,
///   which vanilla runs server-side and syncs back. This client has no server
///   that tracks glide state at all, so it
///   is predicted here. Without it a landing player keeps `fall_flying` set,
///   [`lodestone_physics::tick`] keeps routing to `tick_elytra`, and they can
///   never walk again.
///
/// # Ordering
///
/// **After [`apply_creative_flight_input`]** so `state.0.flying` already carries
/// this tick's toggle: `can_glide` is `!flying && …`, which is what makes a
/// mayfly player's jump press toggle flight rather than start a glide. Vanilla
/// additionally suppresses the glide attempt on the tick flight is toggled
/// *off* (`justToggledCreativeFlight`); that one-tick creative-only difference
/// is not modelled.
///
/// **Before [`player_physics`]** because vanilla's `aiStep` does both before
/// `travel()`, so the `on_ground` both halves read is the previous tick's move —
/// exactly as here.
///
/// # `onClimbable`
///
/// Not tested here. [`lodestone_physics::tick_elytra`] already ends a glide on a
/// climbable on its very first tick (its `is_climbable` branch clears
/// `fall_flying` and falls through to `tick_air`), so the outcome is the same
/// one tick later, and this system has no `CollisionView` to ask.
pub fn update_fall_flying_state(
    glider: Res<GliderEquipped>,
    mut players: Query<
        (
            &mut PhysicsState,
            &MovementIntent,
            &mut WasJumpingGlide,
            &Submersion,
            Option<&Dead>,
        ),
        With<LocalPlayer>,
    >,
) {
    for (mut state, intent, mut was_jumping, fluid, dead) in &mut players {
        let jump = intent.0.jump;
        if dead.is_none() && jump && !was_jumping.0 {
            lodestone_physics::try_start_fall_flying(
                &mut state.0,
                glider.0,
                fluid.0.in_water(),
            );
        }
        was_jumping.0 = jump;
        lodestone_physics::update_fall_flying(&mut state.0, glider.0);
    }
}

/// `TickSet::Physics`, **before** [`player_physics`]: spend one tick of
/// firework-rocket boost.
///
/// `FireworkRocketEntity.tick`'s attached branch is gated on
/// `attachedToEntity.isFallFlying()` — a
/// rocket attached to a player who stops gliding stops boosting, but keeps
/// ticking down, which is why the countdown is spent whether or not the impulse
/// lands.
///
/// # Why before the move rather than after
///
/// Vanilla pins no order here: the rocket is an ordinary entity ticked in the
/// level's entity-iteration order, which is not defined relative to the
/// player's own tick (see [`lodestone_physics::apply_firework_boost`]'s doc).
/// Applying it before the move integrates the impulse on the same tick it is
/// produced, which is the lower-latency of the two indistinguishable choices.
pub fn tick_firework_boost(
    mut boost: ResMut<FireworkBoost>,
    mut players: Query<&mut PhysicsState, With<LocalPlayer>>,
) {
    if boost.0 == 0 {
        return;
    }
    boost.0 -= 1;
    for mut state in &mut players {
        if state.0.fall_flying {
            lodestone_physics::apply_firework_boost(&mut state.0);
        }
    }
}

/// `TickSet::Physics`: advance vanilla's `timeHeld` for an in-progress item use.
/// See [`ItemUseTicks`].
pub fn tick_item_use(mut ticks: ResMut<ItemUseTicks>) {
    if let Some(held) = &mut ticks.0 {
        *held = held.saturating_add(1);
    }
}

/// Tell the server a glide started, exactly once per glide.
///
/// Registered at the tail of the `TickSet::Physics` chain rather than in
/// `TickSet::Send`, for two reasons the plugin's own comment carries: vanilla
/// sends it from inside `aiStep`, *before* the tick's movement packet, and this
/// crate cannot order against `lodestone_controller`'s `ActionQueue` writers.
///
/// `LocalPlayer.aiStep` sends one `ServerboundPlayerCommandPacket(
/// START_FALL_FLYING)` on the tick `tryToStartFallFlying()` returns true
/// and never resends it — the server owns the
/// shared flag from then on. [`FallFlyingSent`] is that once-per-glide latch,
/// the same shape [`LastSprintingSent`] gives the sprint edge.
///
/// **`ClientAction::PlayerCommand`'s `StartFallFlying` had no producer anywhere
/// in this tree before this system** — four adapters encode it and nothing sent
/// it, the `SetFlying` shape exactly. Without it the server keeps simulating a
/// falling player while we glide, and its own `handleMovePlayer` replay
/// diverges from the position we report.
pub fn send_fall_flying_command(
    egress: Res<Egress>,
    mut queue: ResMut<ActionQueue>,
    mut players: Query<
        (
            &PhysicsState,
            &crate::session::ServerEntityId,
            &mut FallFlyingSent,
        ),
        With<LocalPlayer>,
    >,
) {
    for (state, entity_id, mut sent) in &mut players {
        if !state.0.fall_flying {
            // Rearm for the next glide. Done regardless of `egress` so a glide
            // that ended while disconnected cannot latch the next one shut.
            sent.0 = false;
            continue;
        }
        if sent.0 || !(egress.in_world && egress.live) {
            continue;
        }
        let Some(entity_id) = entity_id.0 else {
            continue;
        };
        sent.0 = true;
        queue.0.push(ClientAction::PlayerCommand {
            entity_id,
            command: PlayerCommand::StartFallFlying,
        });
    }
}

/// `TickSet::Physics`, **last in the chain**: while the local player is a
/// passenger, snap them onto their seat and clear the state a walking player
/// would have written — `Entity.rideTick` + `Entity.positionRider`
/// and `Player.tick`'s passenger override.
///
/// # This is what makes riding reach pixels
///
/// The camera is *not* separately taught about vehicles, and 26.2's own client
/// does not teach it either: `Camera.alignWithEntity` has **no
/// `isPassenger()` branch** other than a lerp fix-up for new-behaviour minecarts,
/// and riding changes neither the player's pose nor its eye height
/// (`Player.updatePlayerPose` has no riding case, and
/// there is no `SITTING` pose — a mounted player keeps
/// `Avatar.DEFAULT_EYE_HEIGHT = 1.62`). So moving the *feet*
/// here moves the eye, the block-target ray origin and the audio listener
/// together, all three through `lodestone_shell::sim::Sim::camera`'s existing
/// read of [`PhysicsState`]. Nothing downstream needed a new seam.
///
/// # Why this runs *after* `player_physics` rather than replacing it
///
/// Vanilla runs the passenger's full tick — travel included, with the same
/// `xxa`/`zza` the vehicle reads — and only then overwrites the position:
/// `rideTick()` is `setDeltaMovement(ZERO); this.tick(); vehicle.positionRider(this)`,
/// and `LivingEntity.aiStep` still reaches
/// `travel(input)` for a passenger because `canSimulateMovement()` — which
/// `Player.canSimulateMovement` overrides as `!isClientSide() ||
/// isLocalPlayer()` — is true for the local player either way. So a walking player's one tick of drift out of the seat
/// really does happen upstream and really is thrown away here. Suppressing
/// `player_physics` instead would be a *different* engine, and it would also
/// throw away the fluid-state computation the pose and fog read.
///
/// The one divergence: vanilla zeroes velocity at the **top** of `rideTick`, this
/// zeroes it at the bottom. For every tick after the first the two are identical
/// (travel reads the velocity the previous tick left), so the difference is one
/// tick of pre-mount momentum, which the same tick's position snap discards
/// anyway. Measured in ticks it is not observable; stated because the ordering
/// looks arbitrary otherwise.
///
/// # `on_ground` is forced false — but *not* to avoid the flying kick
///
/// `Player.tick` has `if (isSpectator() || isPassenger()) setOnGround(false);`
/// — unconditional, before anything else in `tick()`. This closes the
/// `spectator_or_passenger_note` contract test in
/// `lodestone-physics/tests/on_ground.rs`, which existed precisely because the
/// pure engine has no riding state and the override had to be a driver's.
///
/// **The obvious reason to do it is wrong, and the check was worth running.**
/// `PlayerState::on_ground`'s own docs frame the flag as a wire contract guarded
/// by the server's `aboveGroundTickCount` / `multiplayer.disconnect.flying`
/// counter, which would make this a kick-avoidance necessity. It is not: the
/// server's float check is explicitly `&& !this.player.isPassenger()`
/// (`ServerGamePacketListenerImpl`'s floating check), and its move handler
/// **discards a passenger's reported position outright**, keeping only the
/// rotation (`ServerGamePacketListenerImpl`'s move handler:
/// `absSnapTo(getX(), getY(), getZ(), targetYRot, targetXRot)`). So neither the
/// position nor the flag we send while mounted can desync us.
///
/// What the override is actually for is every **local** consumer of the flag:
/// the pose machine, the view bob's `pre_on_ground`, the jump gate and
/// `cancel_flight_on_landing`, all of which would otherwise read "standing on
/// something" for a player sitting in a boat. That is a smaller claim than
/// "prevents a disconnect", and it is the true one.
///
/// # Every reason this can decline to pin, and why each is the safe direction
///
/// A missing input leaves the player where `player_physics` put them rather than
/// snapping them somewhere invented:
///
/// * [`Riding`](crate::session::Riding) absent or `None` — not riding.
/// * the vehicle id is not in [`EntityIndex`](crate::entity::EntityIndex) — the
///   seat's vehicle has not spawned client-side yet. Self-healing:
///   `SET_PASSENGERS` is re-sent on any seat change, and the vehicle's own
///   `AddEntity` normally precedes it.
/// * the vehicle has no [`Position`](crate::entity::Position) /
///   [`Rotation`](crate::entity::Rotation) /
///   [`EntityKind`](crate::entity::EntityKind) — the same case one step later.
/// * [`VersionData`](crate::VersionData) holds no adapter, or the adapter does not
///   know the type. The seat height comes from the vehicle's real generated box
///   height, so an unknown type has no height to fall back on and **must not** be
///   guessed — [`crate::riding`]'s fallback is `(0, height, 0)` and a fabricated
///   height would put the rider at a plausible wrong altitude, which is worse
///   than not moving them. Same default-deny rule [`VersionData`](crate::VersionData)
///   documents.
pub fn pin_passenger_to_vehicle(
    index: Res<crate::entity::EntityIndex>,
    version: Option<Res<crate::VersionData>>,
    vehicles: Query<(
        &crate::entity::Position,
        &crate::entity::Rotation,
        &crate::entity::EntityKind,
        Option<&crate::entity::Passengers>,
    )>,
    mut players: Query<
        (
            &mut PhysicsState,
            &crate::session::Riding,
            &crate::session::ServerEntityId,
        ),
        With<LocalPlayer>,
    >,
) {
    let Some(version) = version else {
        return;
    };
    for (mut state, riding, own_id) in &mut players {
        let Some(vehicle_id) = riding.0 else {
            continue;
        };
        // `Player.tick`'s spectator-or-passenger guard, and it applies the moment we know we are a
        // passenger — before, and independently of, whether the seat itself can be
        // resolved. A tick that cannot find the vehicle still must not tell the
        // server we are standing on the boat's roof.
        state.0.on_ground = false;
        state.0.velocity = Vec3d::ZERO;

        let Some(vehicle) = index.get(vehicle_id) else {
            continue;
        };
        let Ok((position, rotation, kind, passengers)) = vehicles.get(vehicle) else {
            continue;
        };
        let Some(facts) = version.entity_facts(&kind.0) else {
            continue;
        };
        // `Entity.getDefaultPassengerAttachmentPoint`: `vehicle.getPassengers().indexOf(passenger)`. A
        // vehicle with no `Passengers` component yet, or a list that does not
        // mention us, reads as seat 0 — which is what `indexOf` returning `-1`
        // then feeding `Mth.clamp(index, 0, size - 1)` gives in vanilla
        // (`EntityAttachments::getClamped`), so the degenerate case agrees rather
        // than merely being harmless.
        let seat_index = own_id
            .0
            .and_then(|own| {
                passengers.and_then(|list| list.0.iter().position(|id| *id == own))
            })
            .unwrap_or(0);
        state.0.position = crate::riding::player_seat_position(
            Vec3d::new(position.0.x, position.0.y, position.0.z),
            rotation.0.yaw,
            kind.0.path(),
            facts.dimensions.height,
            seat_index,
        );
    }
}

/// `TickSet::Animate`: advance every [`LocalPlayer`]'s [`AttackStrengthTicker`]
/// one tick, mirroring `Player.tick()`'s unconditional `this
/// .attackStrengthTicker++`. Same rate and same
/// "runs regardless of anything else this tick" contract as
/// [`crate::ingest::tick_hurt_time`]/[`crate::ingest::tick_entity_swing`],
/// which this is the local-player counterpart of — those age a *remote*
/// entity's hurt/swing state; this ages our own attack cooldown.
///
/// Registered here rather than in `crate::ingest` because the ticker is a
/// [`LocalPlayer`]-only concept with no server event feeding it at all — see
/// [`AttackStrengthTicker`]'s docs — so it belongs with this crate's other
/// local-player-only tick systems ([`player_physics`]) rather than beside the
/// net-ingest-driven ones.
pub fn tick_attack_strength(mut players: Query<&mut AttackStrengthTicker, With<LocalPlayer>>) {
    for mut ticker in &mut players {
        ticker.0 = ticker.0.saturating_add(1);
    }
}

// ---------------------------------------------------------------------------
// Spawning
// ---------------------------------------------------------------------------

/// Spawn the [`LocalPlayer`] entity with every Stage-2 component present.
///
/// Every component is inserted eagerly here, unlike the *observed*-entity set
/// in [`crate::entity`] where absence encodes "the server has never mentioned
/// this". Nothing about the local player is server-reported in that sense —
/// it is all locally owned — so there is no three-state encoding to preserve
/// and a system may rely on the whole set existing. The one exception is
/// [`Dead`], which is a marker precisely so that alive is the default.
pub fn spawn_local_player(world: &mut World, state: PlayerState) -> Entity {
    world
        .spawn((
            LocalPlayer,
            PhysicsState(state),
            PrevPosition(state.position),
            Submersion(FluidState::NONE),
            MovementIntent(MovementInput::NONE),
            SprintKeyHeld(false),
            Flying(false),
            SelectedSlot(0),
            LastPlayerInput(None),
            LastSprintingSent(Some(false)),
            LastFlyingSent(Some(false)),
            AttackStrengthTicker(0),
            // Always present — see `BreakOutcome`'s own docs on why the
            // observability half of the `BreakIntent` contract must not be
            // opt-in the way the intent itself is.
            BreakOutcome::default(),
            // Nested: bevy's tuple `Bundle` impl tops out at 15 elements and
            // this spawn was already there, so `PlaceOutcome` (same
            // always-present reasoning as `BreakOutcome`, one line up) joins
            // a sub-tuple rather than the top level. A tuple of `Bundle`s is
            // itself a `Bundle`, so this changes nothing about what gets
            // inserted.
            (
                // Creative-flight client state. Both start cleared:
                // `jumpTriggerTime` at `0` means the *next* jump press opens a
                // fresh double-tap window rather than immediately completing
                // one, and `wasJumping` at `false` makes a jump key already
                // held at spawn read as a rising edge — which is vanilla's own
                // initial state for both fields.
                JumpTriggerTime(0),
                WasJumping(false),
                PlaceOutcome::default(),
                // `WasJumpingGlide` starts cleared for the same
                // reason `WasJumping` does; `FallFlyingSent` starts cleared
                // because no glide is in progress to have announced.
                WasJumpingGlide(false),
                FallFlyingSent(false),
                // The horse jump-charge ramp. Cleared for the same reason
                // `WasJumping` is: a jump key already held at spawn must read as a
                // rising edge, not as a charge already in progress.
                crate::vehicle::RidingJumpCharge::default(),
            ),
        ))
        .id()
}

/// Return `entity` to its just-spawned state around `state`, for a
/// quit-to-title that must behave exactly like a first connection rather than
/// starting with the previous session's leftovers.
///
/// Deliberately not `despawn` + [`spawn_local_player`]: the `Entity` id is
/// held by the driver (and, later, by plugins), so it has to survive a session
/// teardown.
pub fn reset_local_player(world: &mut World, entity: Entity, state: PlayerState) {
    let Ok(mut entity) = world.get_entity_mut(entity) else {
        return;
    };
    entity.insert((
        PhysicsState(state),
        PrevPosition(state.position),
        Submersion(FluidState::NONE),
        MovementIntent(MovementInput::NONE),
        SprintKeyHeld(false),
        Flying(false),
        SelectedSlot(0),
        LastPlayerInput(None),
        LastSprintingSent(Some(false)),
        LastFlyingSent(Some(false)),
        AttackStrengthTicker(0),
        BreakOutcome::default(),
        // Nested for the same "bevy's tuple `Bundle` impl tops out at 15"
        // reason `spawn_local_player`'s own comment gives — this insert was
        // already at 14 before `PlaceOutcome` joined it.
        (
            // A quit-to-title must not leave a half-open double-tap window
            // behind. `Abilities` itself is reset by
            // `insert_session_components`, which the driver calls alongside
            // this — so a new session starts with no flight grant until the
            // server sends one.
            JumpTriggerTime(0),
            WasJumping(false),
            PlaceOutcome::default(),
            // A quit-to-title must not leave a glide announced
            // (the next session's server has never heard of it) or a stale
            // jump edge behind.
            WasJumpingGlide(false),
            FallFlyingSent(false),
            // A quit-to-title must not leave a half-charged horse jump behind
            // either — the next session's server has never heard of it.
            crate::vehicle::RidingJumpCharge::default(),
        ),
    ));
    entity.remove::<Dead>();
    // A quit-to-title must hand rotation back to mouse-look and drop any
    // plugin's claimed dig — a stale `BreakIntent` surviving into the next
    // session would resume mining as soon as a plugin re-adds itself, with no
    // human ever having pressed anything this session.
    entity.remove::<BreakIntent>();
    // Same reasoning, for placement: a leftover `PlaceIntent` from a session
    // that ended mid-attempt must not resolve into a placement the plugin
    // never re-confirmed under the new session.
    entity.remove::<PlaceIntent>();
    // Same reasoning, for slot selection: a leftover `SelectSlotIntent` from a
    // session that ended mid-change must not re-select a slot the plugin never
    // re-confirmed under the new session.
    entity.remove::<SelectSlotIntent>();
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

/// Registers the local player's resources and its [`TickSet::Physics`] system.
///
/// Does **not** spawn the entity: which `World` the local player lives in, and
/// with what initial pose, is the driver's decision (see
/// [`spawn_local_player`]).
///
/// Pairs with `lodestone_controller::ecs::ControllerPlugin`, which owns the
/// `Input` and `Send` halves of the same tick. Adding this one alone gives a
/// player that is simulated but neither driven nor reported — useful for a
/// headless physics harness, and the reason the two are separate plugins.
#[derive(Debug, Default)]
pub struct LocalPlayerPlugin;

impl Plugin for LocalPlayerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PlayerCollision>();
        app.init_resource::<Profile>();
        app.init_resource::<NearbyEntities>();
        // All four default to "as before this plugin
        // gained them": auto-jump on (vanilla's default, and `PlayerState`'s),
        // no glider, no boost owed, nothing being used. A driver that pushes
        // none of them is bit-identical to one built before they existed.
        app.init_resource::<AutoJump>();
        app.init_resource::<GliderEquipped>();
        app.init_resource::<FireworkBoost>();
        app.init_resource::<ItemUseTicks>();
        // [`pin_passenger_to_vehicle`] resolves the ride's vehicle id through it,
        // and this plugin is usable **without** `crate::ingest::IngestPlugin` — a
        // headless physics harness, `lodestone-controller`'s own tests, and the
        // offline fixture world all add this plugin alone, where a `Res<EntityIndex>`
        // would panic on first `GameTick`. `init_resource` and not
        // `insert_resource` precisely so that installing both plugins (which the
        // shell does) leaves `IngestPlugin`'s populated index untouched whichever
        // order they are added in; the default is an empty index, which reads as
        // "nothing tracked" and correctly declines to pin.
        //
        // `VersionData` gets the `Option<Res<…>>` treatment in that system instead
        // of being defaulted here, because a default `VersionData(None)` inserted
        // by *this* plugin could shadow a real adapter the driver inserts later —
        // `Sim::build` does exactly that, after `add_plugins`.
        app.init_resource::<crate::entity::EntityIndex>();
        app.init_resource::<ActionQueue>();
        app.init_resource::<Egress>();
        // `ControlledVehicle`, shared with `crate::ingest::IngestPlugin`'s
        // `apply_vehicle_moved`; either plugin can be installed without the other,
        // so both init it. `init_resource` and **not** a nested plugin: adding a
        // plugin from inside another plugin's `build` is a Bevy hazard this crate
        // has no reason to take on for one resource.
        app.init_resource::<crate::vehicle::ControlledVehicle>();
        // `TickSet::Intent` before `TickSet::Physics`: the master chain in
        // `CorePlugin` (`Input, Physics, Predict, Animate, Send`) predates
        // this variant and is out of scope for this crate's edit list, so
        // the constraint is added here instead — `configure_sets` is
        // additive, so declaring the same edge from more than one plugin
        // (see `lodestone_controller::ecs::ControllerPlugin`, which needs it
        // for `MovementIntent`) is redundant, not contradictory.
        app.configure_sets(
            GameTick,
            TickSet::Intent
                .after(TickSet::Input)
                .before(TickSet::Physics),
        );
        app.add_systems(GameTick, apply_look_intent.in_set(TickSet::Intent));

        app.init_resource::<DebugLines>();
        // Same reasoning as the `TickSet::Intent` edge above: `CorePlugin`'s
        // `Extract` chain is `Terrain, Entities, Hud` and predates
        // `ExtractSet::Debug`, so this plugin adds the missing edges.
        app.configure_sets(
            Extract,
            ExtractSet::Debug
                .after(ExtractSet::Entities)
                .before(ExtractSet::Hud),
        );
        app.add_systems(Extract, clear_debug_lines.before(ExtractSet::Debug));
        // The leash-rope substitute described on `push_leash_lines`'s
        // own doc. `.in_set(ExtractSet::Debug)`, not merely `.after` the clear —
        // same requirement every writer into `DebugLines` has, per that set's own
        // doc, so this system's push cannot land before the clear by luck of
        // registration order.
        app.add_systems(Extract, push_leash_lines.in_set(ExtractSet::Debug));

        // `.chain()` reproduces `LocalPlayer.aiStep`'s three-part ordering around
        // `super.aiStep()`, and the order is observable rather than cosmetic:
        // the toggle and the vertical impulse must land on the velocity this
        // tick's travel integrates, while the landing cancel must read the
        // `on_ground` that same travel just wrote. Registered as one chain so a
        // plugin cannot reorder them independently.
        //
        // `pin_passenger_to_vehicle` is **last**, and the position is what makes
        // that load-bearing: it is `Entity.positionRider`, which vanilla runs
        // after the passenger's whole `tick()`, and it is also the writer of the
        // transmitted `on_ground` for a passenger — so it must land after
        // `player_physics` (which computes the walking answer) and after
        // `cancel_flight_on_landing` (which reads it). See that system's docs.
        app.add_systems(
            GameTick,
            (
                apply_creative_flight_input,
                // Both **before** `player_physics` and in this
                // order: the glide decision is `aiStep`'s (pre-`travel`) and
                // the rocket impulse has to be on the velocity this tick's
                // travel integrates. `tick_item_use` joins the
                // chain rather than floating so the use-duration a release
                // edge reads is deterministic relative to the move.
                update_fall_flying_state,
                tick_firework_boost,
                tick_item_use,
                player_physics,
                cancel_flight_on_landing,
                // The client-authoritative vehicle tick, and it sits **before**
                // the seat pin on purpose: the pin reads the vehicle's
                // `Position`, so moving the vehicle afterwards would carry the
                // camera one tick behind the boat it is sitting in.
                //
                // `charge_riding_jump` is first of the three because it is
                // `LocalPlayer.aiStep`'s own jump block, which vanilla runs before
                // `travel` — the charge released this tick has to reach this
                // tick's impulse.
                crate::vehicle::charge_riding_jump,
                crate::vehicle::tick_controlled_vehicle,
                pin_passenger_to_vehicle,
                // The glide report's outbound half, and **`TickSet::Physics` is where
                // vanilla puts it**: `LocalPlayer.aiStep` sends
                // START_FALL_FLYING inline, and
                // `sendPosition()` runs afterwards from `LocalPlayer.tick` — so
                // the command precedes the tick's movement packet on the wire,
                // which queueing it here reproduces and queueing it in
                // `TickSet::Send` would not.
                //
                // It also has to be in *this* chain rather than `TickSet::Send`
                // for a mechanical reason: it writes `ResMut<ActionQueue>`,
                // which `lodestone_controller`'s two `Send` systems also write,
                // and this crate cannot name them to order against (the
                // controller depends on this crate, not the reverse). An
                // unordered second writer in `Send` fails
                // `exactly_one_system_writes_movement_intent`'s ambiguity build
                // — which is how this was caught.
                send_fall_flying_command,
                // `ClientAction::MoveVehicle` / `PaddleBoat`, in this chain and
                // not in `TickSet::Send` for exactly the reason
                // `send_fall_flying_command` above gives: it writes
                // `ResMut<ActionQueue>`, which `lodestone_controller`'s two `Send`
                // systems also write, and this crate cannot name them to order
                // against. An unordered second writer there fails the schedule's
                // own ambiguity check.
                crate::vehicle::send_vehicle_actions,
            )
                .chain()
                .in_set(TickSet::Physics),
        );
        // `TickSet::Animate` is already chained after `Physics`/`Predict` by
        // `CorePlugin` (see `plugin.rs`'s `GameTick` `configure_sets`), so no
        // extra ordering edge is needed here — same reasoning `crate::ingest`
        // relies on for `tick_hurt_time`/`tick_entity_swing`.
        app.add_systems(GameTick, tick_attack_strength.in_set(TickSet::Animate));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_physics::Aabb;

    /// A floor at `y = 0` and nothing else, as an owned [`CollisionSource`].
    #[derive(Debug)]
    struct Floor;

    impl CollisionView for Floor {
        fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
            if y == 0 {
                out.push(Aabb {
                    min_x: f64::from(x),
                    min_y: f64::from(y),
                    min_z: f64::from(z),
                    max_x: f64::from(x) + 1.0,
                    max_y: f64::from(y) + 1.0,
                    max_z: f64::from(z) + 1.0,
                });
            }
        }
    }

    impl CollisionSource for Floor {
        fn with_view(&self, f: &mut dyn FnMut(&dyn CollisionView)) {
            f(self);
        }
    }

    fn app_with_player(collision: PlayerCollision) -> (App, Entity) {
        let mut app = App::new();
        app.add_plugins((crate::CorePlugin, LocalPlayerPlugin));
        app.insert_resource(collision);
        let state = PlayerState::at(Vec3d::new(0.5, 4.0, 0.5), 0.0);
        let entity = spawn_local_player(app.world_mut(), state);
        (app, entity)
    }

    fn run_tick(app: &mut App) {
        app.world_mut().run_schedule(GameTick);
    }

    /// An app with the session component set too, so `Abilities` exists — the
    /// shell's real shape (`Sim::build` calls `spawn_local_player` then
    /// `insert_session_components` on **one** entity).
    fn app_with_flightworthy_player(collision: PlayerCollision) -> (App, Entity) {
        let (mut app, entity) = app_with_player(collision);
        crate::session::insert_session_components(app.world_mut(), entity);
        (app, entity)
    }

    fn set_input(app: &mut App, entity: Entity, input: MovementInput) {
        app.world_mut().get_mut::<MovementIntent>(entity).unwrap().0 = input;
    }

    fn grant_flight(app: &mut App, entity: Entity, may_fly: bool) {
        let mut abilities = app
            .world_mut()
            .get_mut::<crate::session::Abilities>(entity)
            .unwrap();
        abilities.may_fly = may_fly;
    }

    fn flying(app: &App, entity: Entity) -> bool {
        app.world()
            .get::<crate::session::Abilities>(entity)
            .unwrap()
            .flying
    }

    fn feet_y(app: &App, entity: Entity) -> f64 {
        app.world().get::<PhysicsState>(entity).unwrap().0.position.y
    }

    /// A rising edge on jump: the toggle is `!wasJumping && jump()`, so a held key
    /// must be released and pressed again to count.
    fn tap_jump(app: &mut App, entity: Entity) {
        set_input(app, entity, MovementInput::NONE);
        run_tick(app);
        set_input(
            app,
            entity,
            MovementInput {
                jump: true,
                ..MovementInput::NONE
            },
        );
        run_tick(app);
    }

    /// **Creative flight's end-to-end gate.** Not "the system works" — *the whole chain from
    /// the server's grant to the player leaving the ground*, driven through the real
    /// `GameTick` schedule. A hermetic call to either flight system passes whether
    /// or not it is registered, which is the island this repo has hit fourteen times.
    #[test]
    fn a_granted_double_tap_lifts_the_player_off_the_ground() {
        let (mut app, entity) =
            app_with_flightworthy_player(PlayerCollision::View(Arc::new(Floor)));
        // Settle onto the floor first, so `on_ground` is genuinely true and the
        // landing-cancel system has something to cancel.
        set_input(&mut app, entity, MovementInput::NONE);
        for _ in 0..40 {
            run_tick(&mut app);
        }
        let resting = feet_y(&app, entity);
        assert!(
            app.world().get::<PhysicsState>(entity).unwrap().0.on_ground,
            "precondition: the player must be standing before takeoff is meaningful"
        );

        grant_flight(&mut app, entity, true);
        tap_jump(&mut app, entity); // opens the 7-tick window
        assert!(!flying(&app, entity), "one tap must not fly");
        tap_jump(&mut app, entity); // completes it
        assert!(
            flying(&app, entity),
            "a granted double-tap must engage flight"
        );

        // Hold jump: the +flyingSpeed*3 impulse must actually lift them.
        set_input(
            &mut app,
            entity,
            MovementInput {
                jump: true,
                ..MovementInput::NONE
            },
        );
        for _ in 0..10 {
            run_tick(&mut app);
        }
        assert!(
            feet_y(&app, entity) > resting + 0.5,
            "flight must lift the player off the floor (resting {resting}, now {})",
            feet_y(&app, entity)
        );
        assert!(flying(&app, entity), "still airborne, so still flying");
    }

    /// **The negative control, and it is the bug that actually shipped.** With no
    /// server grant the identical input sequence must leave the player walking.
    ///
    /// The input here is genuinely negative — `may_fly` is `false` — because a
    /// gate test whose fixture always grants flight is the *world* species of
    /// vacuous test: green, and measuring nothing about the gate.
    #[test]
    fn an_ungranted_double_tap_does_not_fly_on_a_survival_server() {
        let (mut app, entity) =
            app_with_flightworthy_player(PlayerCollision::View(Arc::new(Floor)));
        set_input(&mut app, entity, MovementInput::NONE);
        for _ in 0..40 {
            run_tick(&mut app);
        }
        let resting = feet_y(&app, entity);

        // PRECONDITION asserted, not assumed: a fresh session has no grant.
        assert!(
            !app.world()
                .get::<crate::session::Abilities>(entity)
                .unwrap()
                .may_fly,
            "a fresh session must not believe it may fly"
        );

        tap_jump(&mut app, entity);
        tap_jump(&mut app, entity);
        assert!(
            !flying(&app, entity),
            "flight without a server grant is exactly the bug #191 closed"
        );

        set_input(
            &mut app,
            entity,
            MovementInput {
                jump: true,
                ..MovementInput::NONE
            },
        );
        for _ in 0..10 {
            run_tick(&mut app);
        }
        // Jumping is allowed to move them, but they must come back down rather
        // than climb away: bounded by well under the flier's gain above.
        assert!(
            feet_y(&app, entity) < resting + 0.5,
            "an ungranted player must not climb (resting {resting}, now {})",
            feet_y(&app, entity)
        );
    }

    /// Landing releases flight, and it must read the `on_ground` written by *this*
    /// tick's move — hence a system ordered after `player_physics`.
    #[test]
    fn landing_cancels_flight() {
        let (mut app, entity) =
            app_with_flightworthy_player(PlayerCollision::View(Arc::new(Floor)));
        grant_flight(&mut app, entity, true);
        {
            let mut abilities = app
                .world_mut()
                .get_mut::<crate::session::Abilities>(entity)
                .unwrap();
            abilities.flying = true;
        }
        // Descend onto the floor while holding shift.
        set_input(
            &mut app,
            entity,
            MovementInput {
                sneak: true,
                ..MovementInput::NONE
            },
        );
        for _ in 0..60 {
            run_tick(&mut app);
            if !flying(&app, entity) {
                break;
            }
        }
        assert!(
            !flying(&app, entity),
            "touching the ground must release flight"
        );
        assert!(
            app.world().get::<PhysicsState>(entity).unwrap().0.on_ground,
            "…and the reason must be that they landed"
        );
    }

    /// The server's grant must reach physics, not just the component. This is the
    /// line that makes the whole fold worth having.
    #[test]
    fn abilities_flying_reaches_physics_state() {
        let (mut app, entity) =
            app_with_flightworthy_player(PlayerCollision::View(Arc::new(Floor)));
        run_tick(&mut app);
        // NEGATIVE CONTROL: not flying by default.
        assert!(!app.world().get::<PhysicsState>(entity).unwrap().0.flying);

        {
            let mut abilities = app
                .world_mut()
                .get_mut::<crate::session::Abilities>(entity)
                .unwrap();
            abilities.may_fly = true;
            abilities.flying = true;
            abilities.flying_speed = 0.0625;
        }
        run_tick(&mut app);
        let state = app.world().get::<PhysicsState>(entity).unwrap().0;
        assert!(state.flying, "PlayerState::flying must track the abilities bit");
        assert_eq!(
            state.flying_speed, 0.0625,
            "a server-set flying speed must reach physics, not the 0.05 default"
        );
    }

    /// The physics system must actually be reachable *through the schedule* —
    /// a directly-called function would pass a unit test while the schedule
    /// registration was missing, which is the island this migration's Stage 1
    /// found nine times.
    #[test]
    fn a_game_tick_run_falls_the_player_toward_the_floor() {
        let (mut app, entity) = app_with_player(PlayerCollision::View(Arc::new(Floor)));
        let before = app
            .world()
            .get::<PhysicsState>(entity)
            .unwrap()
            .0
            .position
            .y;
        // Two ticks, not one: a player starting from rest does not move on the
        // first tick, because `tick` runs `move()` *before* applying gravity
        // (see `PlayerState::on_ground`'s docs on the one settle tick). One
        // tick here asserts nothing.
        run_tick(&mut app);
        run_tick(&mut app);
        let after = app
            .world()
            .get::<PhysicsState>(entity)
            .unwrap()
            .0
            .position
            .y;
        assert!(
            after < before,
            "gravity should have moved the player down: {before} → {after}"
        );
    }

    /// The negative control for the above: with no collision source the same
    /// schedule run must leave the player exactly where it was. Without this,
    /// "the player moved" could be satisfied by any writer at all.
    #[test]
    fn no_world_freezes_the_player_instead_of_dropping_it() {
        let (mut app, entity) = app_with_player(PlayerCollision::NoWorld);
        let before = app.world().get::<PhysicsState>(entity).unwrap().0.position;
        for _ in 0..40 {
            run_tick(&mut app);
        }
        let state = app.world().get::<PhysicsState>(entity).unwrap().0;
        assert_eq!(state.position, before, "a worldless player must not fall");
        assert!(state.on_ground, "…and must report standing, not airborne");
        assert_eq!(state.velocity, Vec3d::ZERO);
    }

    /// Enough ticks on a real floor must settle the player *on* it, which is
    /// what proves the view reached the integrator rather than merely being
    /// consulted.
    #[test]
    fn a_collision_source_actually_stops_the_fall_at_the_floor() {
        let (mut app, entity) = app_with_player(PlayerCollision::View(Arc::new(Floor)));
        for _ in 0..60 {
            run_tick(&mut app);
        }
        let state = app.world().get::<PhysicsState>(entity).unwrap().0;
        assert!(
            (state.position.y - 1.0).abs() < 1e-6,
            "expected to settle on the y=0 floor's top face, got {}",
            state.position.y
        );
        assert!(state.on_ground);
    }

    /// [`PrevPosition`] is the camera's interpolation anchor. It must be the
    /// position at the *start* of the tick that just ran — not the end, and
    /// not two ticks ago.
    #[test]
    fn prev_position_anchors_to_the_start_of_the_tick_that_just_ran() {
        let (mut app, entity) = app_with_player(PlayerCollision::View(Arc::new(Floor)));
        // Burn the settle tick (see `a_game_tick_run_falls_the_player_toward_the_floor`)
        // so the tick under test genuinely moves the player — otherwise
        // "prev == start of tick" is satisfied trivially by "nothing moved".
        run_tick(&mut app);
        let before = app.world().get::<PhysicsState>(entity).unwrap().0.position;
        run_tick(&mut app);
        let prev = app.world().get::<PrevPosition>(entity).unwrap().0;
        let now = app.world().get::<PhysicsState>(entity).unwrap().0.position;
        assert_eq!(prev, before);
        assert_ne!(prev, now, "the tick has to have moved the player at all");
    }

    /// Free-fly is a driver camera: it ignores collision entirely and holds
    /// the standing eye height even where physics-walk would be submerged.
    #[test]
    fn flying_ignores_the_floor_and_the_sneak_pose() {
        let (mut app, entity) = app_with_player(PlayerCollision::View(Arc::new(Floor)));
        app.world_mut().entity_mut(entity).insert((
            Flying(true),
            MovementIntent(MovementInput {
                sneak: true,
                ..MovementInput::NONE
            }),
        ));
        run_tick(&mut app);
        let state = app.world().get::<PhysicsState>(entity).unwrap().0;
        assert!(
            (state.position.y - (4.0 - FLY_SPEED)).abs() < 1e-9,
            "sneak should descend at exactly the fly speed, got {}",
            state.position.y
        );
        assert_eq!(
            state.eye_height,
            lodestone_physics::player::DEFAULT_EYE_HEIGHT,
            "free-fly must not adopt the crouch eye height"
        );
    }

    /// The sneak pose *is* adopted on the physics-walk path — the control for
    /// the assertion above.
    #[test]
    fn walking_while_sneaking_adopts_the_crouch_eye_height() {
        let (mut app, entity) = app_with_player(PlayerCollision::View(Arc::new(Floor)));
        app.world_mut()
            .entity_mut(entity)
            .insert(MovementIntent(MovementInput {
                sneak: true,
                ..MovementInput::NONE
            }));
        run_tick(&mut app);
        let state = app.world().get::<PhysicsState>(entity).unwrap().0;
        assert_eq!(state.eye_height, CROUCHING_EYE_HEIGHT);
    }

    /// `Pending` and `NoWorld` both freeze, and differ only in the pose — the
    /// one asymmetry [`PlayerCollision`]'s docs call out. Pinned so a future
    /// tidy-up is a deliberate decision rather than an accident.
    #[test]
    fn pending_updates_the_pose_while_no_world_does_not() {
        let sneaking = MovementIntent(MovementInput {
            sneak: true,
            ..MovementInput::NONE
        });

        let (mut app, entity) = app_with_player(PlayerCollision::Pending);
        app.world_mut().entity_mut(entity).insert(sneaking);
        run_tick(&mut app);
        assert_eq!(
            app.world()
                .get::<PhysicsState>(entity)
                .unwrap()
                .0
                .eye_height,
            CROUCHING_EYE_HEIGHT
        );

        let (mut app, entity) = app_with_player(PlayerCollision::NoWorld);
        app.world_mut().entity_mut(entity).insert(sneaking);
        run_tick(&mut app);
        assert_eq!(
            app.world()
                .get::<PhysicsState>(entity)
                .unwrap()
                .0
                .eye_height,
            lodestone_physics::player::DEFAULT_EYE_HEIGHT
        );
    }

    /// A session teardown must return the player to a first-connection state
    /// while keeping its `Entity` id, which the driver and any plugin hold.
    #[test]
    fn reset_keeps_the_entity_id_and_clears_the_session_state() {
        let (mut app, entity) = app_with_player(PlayerCollision::NoWorld);
        app.world_mut()
            .entity_mut(entity)
            .insert((Dead, SelectedSlot(7), Flying(true)));
        let spawn = PlayerState::at(Vec3d::new(0.5, 71.0, 0.5), 180.0);
        reset_local_player(app.world_mut(), entity, spawn);

        assert_eq!(
            app.world().get::<PhysicsState>(entity).unwrap().0.position,
            spawn.position
        );
        assert_eq!(app.world().get::<SelectedSlot>(entity).unwrap().0, 0);
        assert!(!app.world().get::<Flying>(entity).unwrap().0);
        assert!(app.world().get::<Dead>(entity).is_none());
    }

    /// [`spawn_local_player`] must insert [`BreakOutcome`] unconditionally
    /// (unlike the opt-in [`BreakIntent`]) and it must start at
    /// [`BreakStatus::Idle`] — a plugin polling on the very first tick, before
    /// any dig has ever run, must see "nothing to report" rather than a
    /// missing component or a fabricated success.
    #[test]
    fn spawn_local_player_starts_with_idle_break_outcome_and_no_intent() {
        let (app, entity) = app_with_player(PlayerCollision::NoWorld);
        assert_eq!(
            app.world().get::<BreakOutcome>(entity).unwrap().0,
            BreakStatus::Idle
        );
        assert!(
            app.world().get::<BreakIntent>(entity).is_none(),
            "a fresh session must not start with a claimed dig — nothing has \
             claimed it yet"
        );
    }

    /// A quit-to-title must hand the dig back, the same way it hands rotation
    /// back by never having claimed it in the first place. A `BreakIntent`
    /// left over from the previous session must not resume mining under the
    /// next one.
    #[test]
    fn reset_local_player_drops_a_claimed_break_intent() {
        let (mut app, entity) = app_with_player(PlayerCollision::NoWorld);
        app.world_mut().entity_mut(entity).insert(BreakIntent {
            pos: lodestone_model::BlockPos::new(3, 4, 5),
            face: lodestone_model::BlockFace::Up,
        });
        assert!(app.world().get::<BreakIntent>(entity).is_some());

        let spawn = PlayerState::at(Vec3d::new(0.5, 71.0, 0.5), 0.0);
        reset_local_player(app.world_mut(), entity, spawn);

        assert!(
            app.world().get::<BreakIntent>(entity).is_none(),
            "reset_local_player must clear a claimed dig like every other \
             session-scoped claim"
        );
        assert_eq!(
            app.world().get::<BreakOutcome>(entity).unwrap().0,
            BreakStatus::Idle,
            "the outcome must reset alongside the intent, not report a stale \
             Progressing/Rejected from a session that no longer exists"
        );
    }

    /// [`spawn_local_player`] must insert [`PlaceOutcome`] unconditionally
    /// (unlike the opt-in [`PlaceIntent`]), starting at [`PlaceStatus::Idle`]
    /// and generation `0` — mirrors
    /// `spawn_local_player_starts_with_idle_break_outcome_and_no_intent`
    /// exactly, for the placement half of the contract.
    #[test]
    fn spawn_local_player_starts_with_idle_place_outcome_and_no_intent() {
        let (app, entity) = app_with_player(PlayerCollision::NoWorld);
        let outcome = app.world().get::<PlaceOutcome>(entity).unwrap();
        assert_eq!(outcome.status, PlaceStatus::Idle);
        assert_eq!(outcome.generation, 0);
        assert!(
            app.world().get::<PlaceIntent>(entity).is_none(),
            "a fresh session must not start with a claimed placement — \
             nothing has claimed it yet"
        );
    }

    /// A quit-to-title must hand a queued placement back too, mirroring
    /// `reset_local_player_drops_a_claimed_break_intent`. A `PlaceIntent`
    /// left over from the previous session must not resolve into a placement
    /// under the next one.
    #[test]
    fn reset_local_player_drops_a_claimed_place_intent() {
        let (mut app, entity) = app_with_player(PlayerCollision::NoWorld);
        app.world_mut().entity_mut(entity).insert(PlaceIntent {
            pos: lodestone_model::BlockPos::new(3, 4, 5),
            face: lodestone_model::BlockFace::Up,
        });
        assert!(app.world().get::<PlaceIntent>(entity).is_some());

        let spawn = PlayerState::at(Vec3d::new(0.5, 71.0, 0.5), 0.0);
        reset_local_player(app.world_mut(), entity, spawn);

        assert!(
            app.world().get::<PlaceIntent>(entity).is_none(),
            "reset_local_player must clear a claimed placement like every \
             other session-scoped claim"
        );
        let outcome = app.world().get::<PlaceOutcome>(entity).unwrap();
        assert_eq!(
            outcome.status,
            PlaceStatus::Idle,
            "the outcome must reset alongside the intent, not report a stale \
             result from a session that no longer exists"
        );
        assert_eq!(
            outcome.generation, 0,
            "the generation counter must reset too, or a plugin from the new \
             session could mistake a leftover generation for one of its own \
             attempts"
        );
    }

    /// The whole point of [`LookIntent`]: inserting it changes the tick's
    /// rotation, distinctly from [`MovementIntent`], through the schedule —
    /// not just through a directly-called function.
    #[test]
    fn a_look_intent_writes_the_ticks_rotation() {
        let (mut app, entity) = app_with_player(PlayerCollision::NoWorld);
        app.world_mut().entity_mut(entity).insert(LookIntent {
            yaw: 123.0,
            pitch: -45.0,
        });
        run_tick(&mut app);
        let state = app.world().get::<PhysicsState>(entity).unwrap().0;
        assert_eq!(state.yaw, 123.0);
        assert_eq!(state.pitch, -45.0);
    }

    /// The negative control: with no [`LookIntent`] present, a tick must not
    /// perturb the rotation at all — without this, the assertion above could
    /// pass against a system that unconditionally zeroed rotation and
    /// happened to be fed zero.
    #[test]
    fn no_look_intent_leaves_rotation_untouched() {
        let mut app = App::new();
        app.add_plugins((crate::CorePlugin, LocalPlayerPlugin));
        app.insert_resource(PlayerCollision::NoWorld);
        let mut state = PlayerState::at(Vec3d::new(0.5, 4.0, 0.5), 77.0);
        state.pitch = 12.0;
        let entity = spawn_local_player(app.world_mut(), state);
        run_tick(&mut app);
        let after = app.world().get::<PhysicsState>(entity).unwrap().0;
        assert_eq!(after.yaw, 77.0);
        assert_eq!(after.pitch, 12.0);
    }

    /// [`apply_look_intent`]'s clamp: vanilla's own pitch range is
    /// `[-90, 90]`, and a plugin computing a raw aim vector should not have
    /// to re-derive that clamp itself.
    #[test]
    fn look_intent_pitch_is_clamped_to_vanilla_range() {
        let (mut app, entity) = app_with_player(PlayerCollision::NoWorld);
        app.world_mut().entity_mut(entity).insert(LookIntent {
            yaw: 0.0,
            pitch: 400.0,
        });
        run_tick(&mut app);
        assert_eq!(
            app.world().get::<PhysicsState>(entity).unwrap().0.pitch,
            90.0
        );
    }

    /// [`DebugLines`] is the plugin-writable half of the world-space debug
    /// channel; [`clear_debug_lines`] is the driver-owned half that must run
    /// first each `Extract`, or a plugin that stops drawing would leave its
    /// last frame's geometry on screen forever.
    #[test]
    fn clear_debug_lines_empties_the_resource_through_the_schedule() {
        let mut app = App::new();
        app.add_plugins((crate::CorePlugin, LocalPlayerPlugin));
        app.world_mut()
            .resource_mut::<DebugLines>()
            .0
            .push(DebugLine {
                start: Vec3d::ZERO,
                end: Vec3d::new(1.0, 0.0, 0.0),
                color: [1.0, 0.0, 0.0, 1.0],
            });
        app.world_mut().run_schedule(crate::Extract);
        assert!(app.world().resource::<DebugLines>().0.is_empty());
    }

    /// The negative control for the above: without running `Extract` at all,
    /// the same push must still be sitting there — otherwise the assertion
    /// above could be trivially satisfied by a `DebugLines` that starts empty
    /// and nothing ever populates.
    #[test]
    fn debug_lines_survive_until_extract_actually_runs() {
        let mut app = App::new();
        app.add_plugins((crate::CorePlugin, LocalPlayerPlugin));
        app.world_mut()
            .resource_mut::<DebugLines>()
            .0
            .push(DebugLine {
                start: Vec3d::ZERO,
                end: Vec3d::new(1.0, 0.0, 0.0),
                color: [1.0, 0.0, 0.0, 1.0],
            });
        assert_eq!(app.world().resource::<DebugLines>().0.len(), 1);
    }

    /// A plugin's own system ordered `.in_set(ExtractSet::Debug)` must run
    /// *after* the clear, so it is this frame's geometry that survives, not
    /// last frame's push landing after the clear by luck of registration
    /// order.
    #[test]
    fn a_plugin_writer_in_extract_debug_survives_the_clear() {
        fn push_a_line(mut lines: ResMut<DebugLines>) {
            lines.0.push(DebugLine {
                start: Vec3d::ZERO,
                end: Vec3d::new(2.0, 0.0, 0.0),
                color: [0.0, 1.0, 0.0, 1.0],
            });
        }

        let mut app = App::new();
        app.add_plugins((crate::CorePlugin, LocalPlayerPlugin));
        app.add_systems(Extract, push_a_line.in_set(ExtractSet::Debug));
        // Push a stale line directly, simulating "last frame's leftovers",
        // before running `Extract` at all.
        app.world_mut()
            .resource_mut::<DebugLines>()
            .0
            .push(DebugLine {
                start: Vec3d::ZERO,
                end: Vec3d::ZERO,
                color: [1.0, 1.0, 1.0, 1.0],
            });
        app.world_mut().run_schedule(Extract);
        let lines = &app.world().resource::<DebugLines>().0;
        assert_eq!(
            lines.len(),
            1,
            "the clear must have run before the plugin's write"
        );
        assert_eq!(lines[0].end, Vec3d::new(2.0, 0.0, 0.0));
    }

    /// [`push_leash_lines`]: a mob leashed to another indexed
    /// entity (a mob-to-mob lead, or any entity carrying [`Position`]) draws
    /// exactly one line, from the leashed mob's own position to the holder's.
    #[test]
    fn push_leash_lines_draws_a_line_to_an_indexed_holder() {
        let mut app = App::new();
        app.add_plugins((crate::CorePlugin, LocalPlayerPlugin));
        let mob = app
            .world_mut()
            .spawn((
                Position(lodestone_model::Vec3::new(1.0, 2.0, 3.0)),
                Leashed(Some(99)),
            ))
            .id();
        let holder = app
            .world_mut()
            .spawn(Position(lodestone_model::Vec3::new(4.0, 5.0, 6.0)))
            .id();
        app.world_mut().resource_mut::<EntityIndex>().insert(99, holder);

        app.world_mut().run_schedule(Extract);

        let lines = &app.world().resource::<DebugLines>().0;
        assert_eq!(lines.len(), 1, "expected exactly one line for one leashed mob: {lines:?}");
        assert_eq!(lines[0].start, Vec3d::new(1.0, 2.0, 3.0));
        assert_eq!(lines[0].end, Vec3d::new(4.0, 5.0, 6.0));
        let _ = mob;
    }

    /// The common case in practice: the holder is the local player, whose
    /// ingest entity carries no [`Position`] at all — the fallback
    /// [`push_leash_lines`]'s own doc names.
    #[test]
    fn push_leash_lines_resolves_a_local_player_holder_through_physics_state() {
        let mut app = App::new();
        app.add_plugins((crate::CorePlugin, LocalPlayerPlugin));
        app.world_mut().spawn((
            Position(lodestone_model::Vec3::new(1.0, 2.0, 3.0)),
            Leashed(Some(1)),
        ));
        let local = spawn_local_player(app.world_mut(), PlayerState::at(Vec3d::new(10.0, 11.0, 12.0), 0.0));
        app.world_mut().resource_mut::<EntityIndex>().insert(1, local);

        app.world_mut().run_schedule(Extract);

        let lines = &app.world().resource::<DebugLines>().0;
        assert_eq!(lines.len(), 1, "expected one line to the local-player holder: {lines:?}");
        assert_eq!(lines[0].end, Vec3d::new(10.0, 11.0, 12.0));
    }

    /// Negative control: an entity with no [`Leashed`] component at all, and
    /// one explicitly `Leashed(None)`, must both draw nothing — proving the
    /// system is a real filter and not "one line per entity that has ever had
    /// a position".
    #[test]
    fn push_leash_lines_emits_nothing_for_an_unleashed_mob() {
        let mut app = App::new();
        app.add_plugins((crate::CorePlugin, LocalPlayerPlugin));
        app.world_mut()
            .spawn(Position(lodestone_model::Vec3::new(1.0, 2.0, 3.0)));
        app.world_mut().spawn((
            Position(lodestone_model::Vec3::new(4.0, 5.0, 6.0)),
            Leashed(None),
        ));

        app.world_mut().run_schedule(Extract);

        assert!(
            app.world().resource::<DebugLines>().0.is_empty(),
            "no leash means no line: {:?}",
            app.world().resource::<DebugLines>().0
        );
    }

    /// **Depth Strider, the routing gate.** `docs/swimming.md` tracked this as
    /// "still open, and it is one line: nothing consumes the value" — the fold
    /// itself (`lodestone_entity::attribute`) and the read side
    /// (`ClientHandle::local_player_attributes`) already existed, but no
    /// scheduled system ever called them. This pins that a
    /// `water_movement_efficiency` snapshot on the [`Attributes`] component
    /// actually reaches [`PlayerState::water_movement_efficiency`] through a
    /// real `GameTick` run, not merely through a hand-called function — the
    /// same island class `CLAUDE.md` rule 1 is about, one layer downstream of
    /// the `EntityIndex` fix.
    ///
    /// The modifier shape (`AddValue` `+0.99`) mirrors the worked example in
    /// `lodestone_entity::attribute`'s own tests; the exact number is
    /// arbitrary, chosen only to be recognisably non-default and non-1.0 so a
    /// build that clamped or rounded would be caught.
    /// **The server-reported walk speed, and the two traps that make it more than a one-line read.**
    ///
    /// Pins three things a naive `attribute_value(&attrs.0, &movement_speed_key())`
    /// would each get wrong:
    ///
    /// 1. **No snapshot must fall back to the *player* base (`0.1`), not
    ///    `default_def`'s `0.7`.** That default is vanilla's generic-mob value
    ///    from `createMobAttributes`; using it would make an offline world walk
    ///    seven times too fast. This is the case an online-only test cannot see.
    /// 2. **A real snapshot must reach physics**, so Speed/Slowness — which
    ///    vanilla folds into this attribute server-side — actually change the
    ///    walk speed.
    /// 3. **The local sprint multiply must stop once the server's own
    ///    `minecraft:sprinting` modifier is in the snapshot**, or the two
    ///    compound to ~1.69x instead of 1.3x.
    #[test]
    fn movement_speed_prefers_the_snapshot_but_never_the_generic_mob_default() {
        use lodestone_model::{EntityAttributeModifier, EntityAttributeSnapshot};

        // (1) No `Attributes` component at all: the profile base, not 0.7.
        let (mut app, entity) = app_with_player(PlayerCollision::View(Arc::new(Floor)));
        run_tick(&mut app);
        let bare = app.world().get::<PhysicsState>(entity).unwrap().0;
        let speed = bare.movement_speed.expect("player_physics always sets it");
        // `f64::from(0.1_f32)` rather than the literal `0.1`: the profile field is
        // an `f32`, so widening it is `0.10000000149011612`. Written as the
        // conversion instead of a loosened tolerance so the f32 origin stays
        // visible — a reader who "tidies" this to `0.1` will see it fail.
        assert!(
            (speed - f64::from(0.1_f32)).abs() < 1e-12,
            "with no snapshot the player base (0.1) must be used, not default_def's \
             generic-mob 0.7 — got {speed}"
        );

        // (2) A snapshot reaches physics. `0.26` is recognisably neither the
        // player base nor the mob default, so a build that ignored the snapshot
        // or fell back would be caught either way.
        let (mut app, entity) = app_with_player(PlayerCollision::View(Arc::new(Floor)));
        app.world_mut().entity_mut(entity).insert(Attributes(vec![EntityAttributeSnapshot {
            attribute: movement_speed_key(),
            base: 0.26,
            modifiers: Vec::new(),
        }]));
        run_tick(&mut app);
        let got = app.world().get::<PhysicsState>(entity).unwrap().0.movement_speed.unwrap();
        assert!(
            (got - 0.26).abs() < 1e-9,
            "a movement_speed snapshot must reach PlayerState, got {got}"
        );

        // (3) The sprint multiply is suppressed once the server's own modifier is
        // present. Same base, sprinting intent, with and without the modifier.
        let with_sprint_modifier = vec![EntityAttributeSnapshot {
            attribute: movement_speed_key(),
            base: 0.1,
            modifiers: vec![EntityAttributeModifier {
                id: sprinting_modifier_id(),
                amount: 0.3,
                operation: 2, // ADD_MULTIPLIED_TOTAL
            }],
        }];
        let folded = attribute_value(&with_sprint_modifier, &movement_speed_key());

        let (mut app, entity) = app_with_player(PlayerCollision::View(Arc::new(Floor)));
        app.world_mut().entity_mut(entity).insert(Attributes(with_sprint_modifier));
        app.world_mut().entity_mut(entity).insert(MovementIntent(MovementInput {
            forward: 0.0,
            strafe: 0.0,
            jump: false,
            sneak: false,
            sprint: true,
        }));
        run_tick(&mut app);
        let sprinting = app.world().get::<PhysicsState>(entity).unwrap().0.movement_speed.unwrap();
        assert!(
            (sprinting - folded).abs() < 1e-9,
            "with the server's sprinting modifier already folded in ({folded}), the local \
             multiply must not compound on top — got {sprinting}"
        );
    }

    #[test]
    fn depth_strider_attribute_reaches_the_physics_state_each_tick() {
        use lodestone_model::{EntityAttributeModifier, EntityAttributeSnapshot, Identifier};
        use std::str::FromStr;

        let (mut app, entity) = app_with_player(PlayerCollision::View(Arc::new(Floor)));
        app.world_mut().entity_mut(entity).insert(Attributes(vec![EntityAttributeSnapshot {
            attribute: water_movement_efficiency_key(),
            base: 0.0,
            modifiers: vec![EntityAttributeModifier {
                id: Identifier::from_str("minecraft:enchantment/depth_strider").unwrap(),
                amount: 0.99,
                operation: 0, // AddValue
            }],
        }]));

        run_tick(&mut app);

        let state = app.world().get::<PhysicsState>(entity).unwrap().0;
        assert!(
            (state.water_movement_efficiency - 0.99).abs() < 1e-6,
            "Depth Strider's folded attribute must reach PlayerState each tick, got {}",
            state.water_movement_efficiency
        );
    }

    /// The control for the gate above: with no [`Attributes`] component at all
    /// — the offline demo world and the pre-login title-screen player, per
    /// [`player_physics`]'s own docs — the fold must read the registry default
    /// rather than inventing a value or panicking on the missing component.
    /// Without this, a system that always wrote a hard-coded constant would
    /// pass the positive test above just as well.
    #[test]
    fn no_attributes_component_folds_to_the_registry_default() {
        let (mut app, entity) = app_with_player(PlayerCollision::View(Arc::new(Floor)));

        run_tick(&mut app);

        let state = app.world().get::<PhysicsState>(entity).unwrap().0;
        assert_eq!(
            state.water_movement_efficiency, 0.0,
            "control: no attribute snapshot at all must fold to the default, not a stale \
             or hard-coded value"
        );
    }

    /// [`AttackStrengthTicker`] must actually advance through a real
    /// `GameTick` run, not merely be advanceable by a hand-called
    /// [`tick_attack_strength`] — the same island class `CLAUDE.md` rule 1
    /// warns about, and the same shape `depth_strider_attribute_reaches_the_
    /// physics_state_each_tick` above already guards for `PhysicsState`.
    #[test]
    fn attack_strength_ticker_advances_one_per_game_tick_through_the_schedule() {
        let (mut app, entity) = app_with_player(PlayerCollision::View(Arc::new(Floor)));
        assert_eq!(
            app.world().get::<AttackStrengthTicker>(entity).unwrap().0,
            0,
            "spawn_local_player must start the ticker at 0, matching Player's bare int field"
        );

        for expected in 1..=5u32 {
            run_tick(&mut app);
            assert_eq!(
                app.world().get::<AttackStrengthTicker>(entity).unwrap().0,
                expected,
                "the ticker must advance by exactly one per GameTick run"
            );
        }
    }

    /// [`reset_local_player`] must put the ticker back to `0`, matching every
    /// other locally-owned field it resets — a session that quits to title
    /// mid-swing must not carry the old cooldown into the next one.
    #[test]
    fn reset_local_player_zeroes_the_attack_strength_ticker() {
        let (mut app, entity) = app_with_player(PlayerCollision::View(Arc::new(Floor)));
        for _ in 0..3 {
            run_tick(&mut app);
        }
        assert!(app.world().get::<AttackStrengthTicker>(entity).unwrap().0 > 0);

        let state = PlayerState::at(Vec3d::new(0.5, 4.0, 0.5), 0.0);
        reset_local_player(app.world_mut(), entity, state);
        assert_eq!(
            app.world().get::<AttackStrengthTicker>(entity).unwrap().0,
            0,
            "reset_local_player must zero the ticker like every other local-only field"
        );
    }

    // -----------------------------------------------------------------------
    // Riding (Tier 1 item 8)
    // -----------------------------------------------------------------------

    /// A minimal [`lodestone_model::VersionAdapter`] that answers exactly one
    /// question — the vehicle's base box height — so [`pin_passenger_to_vehicle`]
    /// can be exercised in a crate that (deliberately) depends on no protocol
    /// family.
    ///
    /// The three required methods are stubs; every other seam keeps its trait
    /// default, which is what makes this a *narrow* double rather than a second
    /// implementation of the adapter. The heights fed to it below come from
    /// `.cache/mc/26.2`'s `EntityTypes.java`, not from `lodestone-data`, so the
    /// expected seat positions do not originate inside the code under test.
    #[derive(Debug)]
    struct HeightOnlyAdapter {
        height: f32,
    }

    impl lodestone_model::VersionAdapter for HeightOnlyAdapter {
        fn protocol_version(&self) -> i32 {
            0
        }

        fn minecraft_versions(&self) -> &'static [&'static str] {
            &[]
        }

        fn supports(&self, _protocol: i32) -> bool {
            false
        }

        fn entity_facts(
            &self,
            _entity_type: &lodestone_model::ResourceKey,
        ) -> Option<lodestone_model::EntityFacts> {
            Some(lodestone_model::EntityFacts {
                dimensions: lodestone_model::EntityBaseDimensions {
                    // Width is not read by the passenger attachment rule at all
                    // (`EntityAttachment.Fallback.AT_HEIGHT` ignores it), so a
                    // deliberate zero here would be a silent lie if it ever
                    // started mattering. The real value is passed instead.
                    width: 0.98,
                    height: self.height,
                },
                pushes_players: false,
            })
        }

        // The three wire methods. This fixture exists only to answer
        // `entity_facts` for the passenger-attachment rule — it never sees a
        // socket, and `VersionData` is read for facts alone in these tests.
        //
        // `unreachable!` rather than an empty `Ok(vec![])`: a silent success here
        // would let a future test wire this adapter to something that really does
        // expect protocol behaviour and get *nothing*, which is the fail-open
        // shape this repo has been bitten by. If one of these ever fires, the
        // fixture is being used for something it cannot do, and the panic says so.
        fn begin_login(
            &self,
            _profile: &lodestone_model::LoginProfile,
            _server: &lodestone_model::ServerAddress,
        ) -> Result<Vec<lodestone_model::Directive>, lodestone_model::AdapterError> {
            unreachable!("HeightOnlyAdapter answers entity_facts only; it has no wire")
        }

        fn handle_packet(
            &self,
            _world: &mut dyn lodestone_model::WorldSink,
            _state: lodestone_model::ConnectionState,
            _packet_id: i32,
            _payload: &[u8],
        ) -> Result<Vec<lodestone_model::Directive>, lodestone_model::AdapterError> {
            unreachable!("HeightOnlyAdapter answers entity_facts only; it has no wire")
        }

        fn encode_action(
            &self,
            _state: lodestone_model::ConnectionState,
            _action: &lodestone_model::ClientAction,
        ) -> Result<Option<(i32, Vec<u8>)>, lodestone_model::AdapterError> {
            unreachable!("HeightOnlyAdapter answers entity_facts only; it has no wire")
        }
    }

    /// A world with the full local-player component set, a tracked vehicle at
    /// `vehicle_feet` with yaw `vehicle_yaw`, and the local player seated in it.
    ///
    /// Deliberately built through the same `spawn_local_player` +
    /// `insert_session_components` pair `Sim::build` uses, and the vehicle is
    /// registered in [`crate::entity::EntityIndex`] the way
    /// `crate::ingest::apply_entity_spawn` would — so the only thing this fixture
    /// short-circuits is the wire, never the wiring.
    fn app_with_mounted_player(
        entity_type: &str,
        vehicle_height: f32,
        vehicle_feet: Vec3d,
        vehicle_yaw: f32,
    ) -> (App, Entity) {
        let (mut app, player) =
            app_with_flightworthy_player(PlayerCollision::View(Arc::new(Floor)));
        app.insert_resource(crate::VersionData(Some(Box::new(HeightOnlyAdapter {
            height: vehicle_height,
        }))));
        const OWN_ID: i32 = 7;
        const VEHICLE_ID: i32 = 42;
        let vehicle = app
            .world_mut()
            .spawn((
                crate::entity::MinecraftEntityId(VEHICLE_ID),
                crate::entity::EntityKind(
                    entity_type.parse().expect("valid entity type key"),
                ),
                crate::entity::Position(lodestone_model::Vec3::new(
                    vehicle_feet.x,
                    vehicle_feet.y,
                    vehicle_feet.z,
                )),
                crate::entity::Rotation(lodestone_model::Rotation::new(vehicle_yaw, 0.0)),
                crate::entity::Passengers(vec![OWN_ID]),
            ))
            .id();
        app.world_mut()
            .resource_mut::<crate::entity::EntityIndex>()
            .insert(VEHICLE_ID, vehicle);
        {
            let mut entity = app.world_mut().entity_mut(player);
            entity.get_mut::<crate::session::ServerEntityId>().unwrap().0 = Some(OWN_ID);
            entity.get_mut::<crate::session::Riding>().unwrap().0 = Some(VEHICLE_ID);
        }
        (app, player)
    }

    /// **The end-to-end seat pin, with the value predicted from vanilla's
    /// constants rather than from our own arithmetic.**
    ///
    /// A minecart is `sized(0.98F, 0.7F)` with `passengerAttachments(0.1875F)`
    /// (`EntityTypes.MINECART`), and the player's own `VEHICLE` attachment is
    /// `0.6` (`Avatar.DEFAULT_VEHICLE_ATTACHMENT`). So a rider's feet sit at
    /// `cart.y + 0.1875 - 0.6 = cart.y - 0.4125`, i.e. **below** the cart's
    /// origin, and the camera then sits 1.62 above that.
    ///
    /// Predicting the number is the point: "the player moved" is satisfied by any
    /// pin at all, including one that used the `AT_HEIGHT` fallback (which would
    /// give `+0.1` here — a 0.5125 error, and the wrong side of the cart origin).
    /// Both hypotheses are asserted against.
    #[test]
    fn a_mounted_player_is_pinned_to_the_vehicles_seat() {
        let cart = Vec3d::new(10.5, 64.0, -3.5);
        let (mut app, player) = app_with_mounted_player("minecraft:minecart", 0.7, cart, 0.0);
        run_tick(&mut app);
        let state = app.world().get::<PhysicsState>(player).unwrap().0;
        let expected_y = cart.y + 0.1875 - 0.6;
        assert!(
            (state.position.y - expected_y).abs() < 1e-9,
            "expected the seat at y={expected_y}, got {}",
            state.position.y
        );
        // The horizontal axes are the cart's exactly: a minecart's attachment has
        // no X or Z component, so any drift here would be leftover walking.
        assert!(
            (state.position.x - cart.x).abs() < 1e-9 && (state.position.z - cart.z).abs() < 1e-9,
            "a rider must sit on the vehicle's own column, got ({}, {})",
            state.position.x,
            state.position.z
        );
        // The wrong-but-plausible hypothesis: ignoring the declared attachment and
        // using vanilla's `AT_HEIGHT` fallback gives `0.7 - 0.6 = +0.1`.
        let fallback_y = cart.y + 0.7 - 0.6;
        assert!(
            (state.position.y - fallback_y).abs() > 0.5,
            "the declared minecart attachment must be used, not the AT_HEIGHT \
             fallback (which would give {fallback_y})"
        );
        // And the other wrong-but-plausible one: forgetting the player's own
        // `VEHICLE` attachment leaves the rider 0.6 too high.
        assert!(
            (state.position.y - (cart.y + 0.1875)).abs() > 0.5,
            "the player's own 0.6 VEHICLE attachment must be subtracted"
        );
    }

    /// The control for the test above: **with the ride state cleared, the same
    /// fixture must fail the same assertion.** Without this, the pin could be
    /// passing because the player happened to fall to that height on the `Floor`.
    #[test]
    fn an_unmounted_player_in_the_same_fixture_is_not_at_the_seat() {
        let cart = Vec3d::new(10.5, 64.0, -3.5);
        let (mut app, player) = app_with_mounted_player("minecraft:minecart", 0.7, cart, 0.0);
        // The only change from the passing case.
        app.world_mut()
            .get_mut::<crate::session::Riding>(player)
            .unwrap()
            .0 = None;
        run_tick(&mut app);
        let state = app.world().get::<PhysicsState>(player).unwrap().0;
        let seat_y = cart.y + 0.1875 - 0.6;
        assert!(
            (state.position.y - seat_y).abs() > 1.0,
            "an unmounted player must not land on the seat by coincidence — the \
             pin's evidence depends on this failing. Got y={}, seat={seat_y}",
            state.position.y
        );
    }

    /// `Player.tick`'s `if (isSpectator() || isPassenger())
    /// setOnGround(false);`. This is the `spectator_or_passenger_note` contract in
    /// `lodestone-physics/tests/on_ground.rs`, made executable.
    ///
    /// The seat is deliberately placed a fraction of a block **above the floor the
    /// fixture provides**, so a naive implementation that simply reported the
    /// collision result would say `true` here within a tick or two — the assertion
    /// is not vacuous against a floor-less world, which is the shape of control
    /// premise `CLAUDE.md` records as having been false twice.
    #[test]
    fn a_passenger_transmits_on_ground_false_while_sitting_just_above_a_block() {
        // `Floor` is a solid cube filling y in [0, 1]. A cart at y=1.6 puts its
        // seat at 1.6 + 0.1875 - 0.6 = 1.1875 — 0.1875 above the floor's top face.
        let (mut app, player) =
            app_with_mounted_player("minecraft:minecart", 0.7, Vec3d::new(0.5, 1.6, 0.5), 0.0);
        for _ in 0..3 {
            run_tick(&mut app);
        }
        let state = app.world().get::<PhysicsState>(player).unwrap().0;
        assert!(
            (state.position.y - 1.1875).abs() < 1e-9,
            "precondition: the pin must be holding the seat, got y={}",
            state.position.y
        );
        assert!(
            !state.on_ground,
            "a passenger must report on_ground=false regardless of collision"
        );
        // The control: the identical fixture minus the ride state falls the 0.1875
        // onto that floor and reports grounded, so the assertion above measures the
        // passenger override rather than a flag that is always false here.
        app.world_mut()
            .get_mut::<crate::session::Riding>(player)
            .unwrap()
            .0 = None;
        for _ in 0..4 {
            run_tick(&mut app);
        }
        assert!(
            app.world().get::<PhysicsState>(player).unwrap().0.on_ground,
            "the same player, dismounted onto the same floor, must report grounded \
             — otherwise the passenger assertion above proves nothing"
        );
    }

    /// A ride whose vehicle the client has not spawned must not invent a
    /// position, but must still apply the `on_ground` override — the two halves
    /// of the system are deliberately separated for exactly this case.
    #[test]
    fn an_unresolvable_vehicle_still_forces_on_ground_false_without_moving_us() {
        let (mut app, player) =
            app_with_mounted_player("minecraft:minecart", 0.7, Vec3d::new(0.5, 1.0, 0.5), 0.0);
        // Forget the vehicle, as if `SET_PASSENGERS` had arrived before the
        // vehicle's own `AddEntity`.
        app.world_mut()
            .resource_mut::<crate::entity::EntityIndex>()
            .remove(42);
        run_tick(&mut app);
        let state = app.world().get::<PhysicsState>(player).unwrap().0;
        assert!(!state.on_ground);
        assert_eq!(
            state.velocity, Vec3d::ZERO,
            "a passenger's velocity is zeroed whether or not the seat resolves"
        );
        // And it did not teleport to a fabricated seat: the player is still near
        // where the fixture spawned them (y=4.0), not at the cart.
        assert!(
            state.position.y > 0.5,
            "an unresolvable vehicle must leave the player where physics put them, \
             got y={}",
            state.position.y
        );
    }

    // -----------------------------------------------------------------------
    // The Auto-Jump option actually reaching the detector
    // -----------------------------------------------------------------------

    /// A floor at `y = 0` plus a full-height step at `z = 1`, `y = 1` — the
    /// same 1.0 rise `lodestone-physics`' own auto-jump gate uses, which is
    /// above the 0.6 auto-step and inside the 1.2 jump ceiling.
    #[derive(Debug)]
    struct FloorWithStep;

    impl CollisionView for FloorWithStep {
        fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
            let solid = y == 0 || (y == 1 && z == 1);
            if solid {
                out.push(Aabb {
                    min_x: f64::from(x),
                    min_y: f64::from(y),
                    min_z: f64::from(z),
                    max_x: f64::from(x) + 1.0,
                    max_y: f64::from(y) + 1.0,
                    max_z: f64::from(z) + 1.0,
                });
            }
        }
    }

    impl CollisionSource for FloorWithStep {
        fn with_view(&self, f: &mut dyn FnMut(&dyn CollisionView)) {
            f(self);
        }
    }

    /// Walks forward into the step for 30 ticks and reports the highest feet `y`.
    fn peak_y_walking_into_a_step(auto_jump: bool) -> f64 {
        let mut app = App::new();
        app.add_plugins((crate::CorePlugin, LocalPlayerPlugin));
        app.insert_resource(PlayerCollision::View(Arc::new(FloorWithStep)));
        app.insert_resource(AutoJump(auto_jump));
        let mut state = PlayerState::at(Vec3d::new(0.5, 1.0, 0.5), 0.0);
        state.on_ground = true;
        let entity = spawn_local_player(app.world_mut(), state);
        set_input(
            &mut app,
            entity,
            MovementInput {
                forward: 1.0,
                ..MovementInput::NONE
            },
        );
        let mut peak = 1.0f64;
        for _ in 0..30 {
            // `MovementIntent` is not recomputed here (no controller plugin), so
            // the input set once above persists across ticks.
            run_tick(&mut app);
            peak = peak.max(feet_y(&app, entity));
        }
        peak
    }

    /// **The auto-jump-un-disableable defect, at the layer that had it.** The option is the
    /// shell's; the detector is physics'; this resource is the only thing
    /// joining them, and before it existed the field sat at its `true` default
    /// for the whole session no matter what the settings screen said.
    #[test]
    fn the_auto_jump_option_off_really_stops_the_detector() {
        let on = peak_y_walking_into_a_step(true);
        assert!(
            on > 1.9,
            "control: with the option ON the player must clear the 1.0 step. If \
             this fails the scenario is wrong, not the option; peak y = {on}"
        );
        let off = peak_y_walking_into_a_step(false);
        assert!(
            off < 1.05,
            "with the option OFF the player must stop at the step. This is the \
             assertion that was impossible to satisfy before AutoJump existed; \
             peak y = {off}"
        );
    }

    #[test]
    fn auto_jump_defaults_on_so_an_unpushed_harness_is_unchanged() {
        // `LocalPlayerPlugin` alone, nothing inserted: the golden traces and the
        // offline fixture world both look like this, and they were written
        // against `PlayerState`'s own `true` default.
        let (app, _) = app_with_player(PlayerCollision::NoWorld);
        assert!(app.world().resource::<AutoJump>().0);
    }

    // -----------------------------------------------------------------------
    // Glide state and the firework boost
    // -----------------------------------------------------------------------

    /// An airborne player with no floor at all, so `on_ground` stays false and
    /// `canGlide`'s first conjunct holds.
    fn app_with_airborne_player() -> (App, Entity) {
        let mut app = App::new();
        app.add_plugins((crate::CorePlugin, LocalPlayerPlugin));
        app.insert_resource(PlayerCollision::View(Arc::new(Void)));
        let mut state = PlayerState::at(Vec3d::new(0.5, 200.0, 0.5), 0.0);
        state.on_ground = false;
        let entity = spawn_local_player(app.world_mut(), state);
        crate::session::insert_session_components(app.world_mut(), entity);
        (app, entity)
    }

    /// Nothing solid anywhere.
    #[derive(Debug)]
    struct Void;

    impl CollisionView for Void {
        fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<Aabb>) {}
    }

    impl CollisionSource for Void {
        fn with_view(&self, f: &mut dyn FnMut(&dyn CollisionView)) {
            f(self);
        }
    }

    fn gliding(app: &App, entity: Entity) -> bool {
        app.world().get::<PhysicsState>(entity).unwrap().0.fall_flying
    }

    #[test]
    fn a_jump_edge_with_an_elytra_starts_a_glide_and_queues_the_command() {
        let (mut app, entity) = app_with_airborne_player();
        app.insert_resource(GliderEquipped(true));
        app.insert_resource(Egress {
            in_world: true,
            live: true,
        });
        app.world_mut()
            .get_mut::<crate::session::ServerEntityId>(entity)
            .unwrap()
            .0 = Some(7);
        set_input(
            &mut app,
            entity,
            MovementInput {
                jump: true,
                ..MovementInput::NONE
            },
        );
        run_tick(&mut app);
        assert!(gliding(&app, entity), "the glide must start on the jump edge");
        // The outbound half: `PlayerCommand::StartFallFlying` had no producer
        // anywhere in this tree before this system, the `SetFlying` shape
        // exactly. Without it the server keeps simulating a falling player.
        let queued = std::mem::take(&mut app.world_mut().resource_mut::<ActionQueue>().0);
        assert!(
            queued.contains(&ClientAction::PlayerCommand {
                entity_id: 7,
                command: PlayerCommand::StartFallFlying,
            }),
            "START_FALL_FLYING must be queued exactly once per glide; queue was {queued:?}"
        );
        // Held, not tapped: a second tick must not re-announce it.
        run_tick(&mut app);
        let queued = std::mem::take(&mut app.world_mut().resource_mut::<ActionQueue>().0);
        assert!(
            !queued.iter().any(|action| matches!(
                action,
                ClientAction::PlayerCommand {
                    command: PlayerCommand::StartFallFlying,
                    ..
                }
            )),
            "one command per glide, not one per tick"
        );
    }

    #[test]
    fn no_elytra_means_no_glide() {
        // The control that makes the test above about the *elytra* rather than
        // about pressing jump. `GliderEquipped` defaults to `false`, so this is
        // also the shape every harness that never pushes it sees.
        let (mut app, entity) = app_with_airborne_player();
        set_input(
            &mut app,
            entity,
            MovementInput {
                jump: true,
                ..MovementInput::NONE
            },
        );
        run_tick(&mut app);
        assert!(!gliding(&app, entity));
    }

    /// The boost's magnitude, predicted from `FireworkRocketEntity.tick`'s own
    /// line rather than asserted as "faster".
    ///
    /// From rest, looking straight down the `+Z` axis (yaw 0, pitch 0), the look
    /// vector is exactly `(0, 0, 1)` and the impulse is
    /// `movement.add(look * 0.1 + (look * 1.5 - movement) * 0.5)`, i.e.
    /// `0 + (0.1 + (1.5 - 0) * 0.5) = 0.85` on Z alone. `tick_elytra` then
    /// applies `updateFallFlyingMovement`'s `0.99` horizontal drag plus the lift
    /// terms, so the *post-tick* velocity is not 0.85 — but it must be far above
    /// the unboosted arm, and the unboosted arm from rest is essentially zero on
    /// Z. Both arms are measured, so the difference is attributable to the boost
    /// term and nothing else.
    #[test]
    fn a_firework_boost_accelerates_a_gliding_player_along_the_look_vector() {
        let z_after = |boost: u32| {
            let (mut app, entity) = app_with_airborne_player();
            app.world_mut()
                .get_mut::<PhysicsState>(entity)
                .unwrap()
                .0
                .fall_flying = true;
            app.insert_resource(GliderEquipped(true));
            app.insert_resource(FireworkBoost(boost));
            run_tick(&mut app);
            app.world().get::<PhysicsState>(entity).unwrap().0.velocity.z
        };
        let boosted = z_after(20);
        let unboosted = z_after(0);
        assert!(
            unboosted.abs() < 0.01,
            "control: an unboosted glide from rest has no forward speed to \
             explain the boosted arm away, got {unboosted}"
        );
        // 0.85 impulse × the glide's own 0.99 horizontal drag and 0.1 look-lerp
        // is comfortably above 0.8; a boost applied *after* the move, or one
        // scaled by the wrong power constant, would not land here.
        assert!(
            boosted > 0.8,
            "the boost must add ~0.85 along +Z before drag, got {boosted}"
        );
    }

    #[test]
    fn a_firework_boost_is_spent_and_does_nothing_when_not_gliding() {
        let (mut app, entity) = app_with_airborne_player();
        app.insert_resource(FireworkBoost(3));
        // Not gliding: `FireworkRocketEntity.tick`'s attached branch is gated on
        // `attachedToEntity.isFallFlying()`, so the rocket keeps ticking down
        // while boosting nothing.
        for _ in 0..3 {
            run_tick(&mut app);
        }
        assert_eq!(app.world().resource::<FireworkBoost>().0, 0);
        let velocity = app.world().get::<PhysicsState>(entity).unwrap().0.velocity;
        assert!(
            velocity.z.abs() < 1e-9,
            "a non-gliding player must get no impulse, got z={}",
            velocity.z
        );
    }

    // -----------------------------------------------------------------------
    // The use-duration counter riptide's release edge reads
    // -----------------------------------------------------------------------

    #[test]
    fn item_use_ticks_counts_ticks_and_only_while_armed() {
        let (mut app, _) = app_with_player(PlayerCollision::NoWorld);
        // Unarmed: nothing counts, so a release with no press cannot ever reach
        // the 10-tick threshold.
        run_tick(&mut app);
        assert_eq!(app.world().resource::<ItemUseTicks>().0, None);
        // Armed at the press edge, as `Sim::use_item_live` does.
        app.world_mut().resource_mut::<ItemUseTicks>().0 = Some(0);
        for expected in 1..=10 {
            run_tick(&mut app);
            assert_eq!(
                app.world().resource::<ItemUseTicks>().0,
                Some(expected),
                "the count is in 20 Hz ticks, so ten ticks is exactly \
                 TridentItem's THROW_THRESHOLD_TIME"
            );
        }
    }
}
