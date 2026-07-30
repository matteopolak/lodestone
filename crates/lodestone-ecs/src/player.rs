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
use lodestone_entity::attribute::{attribute_value, water_movement_efficiency_key};
use lodestone_model::{ClientAction, PlayerInput};
use lodestone_physics::{
    CollisionView, FluidState, MovementInput, NearbyEntity, PhysicsProfile, PlayerState, PushSelf,
    Vec3d, compute_fluid_state, tick_among_entities,
};

use crate::entity::Attributes;
use crate::schedules::{Extract, GameTick};
use crate::sets::{ExtractSet, TickSet};

/// Eye height of `Pose.SWIMMING` — `EntityDimensions.scalable(0.6F, 0.6F).withEyeHeight(0.4F)`
/// (`Avatar.java:28`, shared with `FALL_FLYING` and `SPIN_ATTACK`).
pub const SWIMMING_EYE_HEIGHT: f32 = 0.4;
/// Eye height of `Pose.CROUCHING` — `1.27F` (`Avatar.java:33`).
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

/// Whether free-fly (noclip) is active instead of physics-walk.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Flying(pub bool);

/// Selected hotbar slot in `0..9`.
///
/// Owned locally: the selection is an input the player drives (number keys,
/// scroll wheel) and merely *echoed* to the server, so unlike health or
/// experience there is no server-authoritative value to fold.
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
/// `wasSprinting` (`LocalPlayer.sendIsSprintingIfNeeded`,
/// `LocalPlayer.java:303-312`).
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
pub struct NearbyEntities(pub Vec<NearbyEntity>);

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
    // `abilities.flying` (`Player.java:1433-1439`). Free-fly never calls
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
    mut players: Query<
        (
            &mut PhysicsState,
            &mut Submersion,
            &mut PrevPosition,
            &MovementIntent,
            &Flying,
            &SprintKeyHeld,
            Option<&Attributes>,
        ),
        With<LocalPlayer>,
    >,
) {
    let profile = &profile.0;
    for (mut state, mut fluid, mut prev, intent, flying, sprint_key, attributes) in &mut players {
        prev.0 = state.0.position;
        let player = &mut state.0;
        let intent = intent.0;

        if flying.0 {
            fly_step(player, intent, sprint_key.0, &mut fluid.0);
            continue;
        }

        if matches!(*collision, PlayerCollision::NoWorld) {
            player.velocity = Vec3d::ZERO;
            player.on_ground = true;
            fluid.0 = FluidState::NONE;
            continue;
        }

        let base = f64::from(profile.base_movement_speed);
        let attr = if intent.sprint {
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
                    &nearby.0,
                    PushSelf::LIVING_PLAYER,
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
    ));
    entity.remove::<Dead>();
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
        app.init_resource::<ActionQueue>();
        app.init_resource::<Egress>();
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

        app.add_systems(GameTick, player_physics.in_set(TickSet::Physics));
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
}
