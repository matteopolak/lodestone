//! Live block interaction as ECS state: the pick target, the two prediction
//! machines, the particle simulation, and the `GameTick` systems that drive them.
//!
//! # What this is
//!
//! Stage 5 of `docs/bevy-migration.md`. Before it, `Sim` held `target`,
//! `mining`, `placement`, `attacking`, `particles` and `version_data` as fields,
//! and drove the first four from a hand-written `drive_interaction()` call
//! *after* the `GameTick` schedule. Stage 2's report recorded why they had not
//! moved: their inputs were "Stage 3/4 residents", so a system would have needed
//! them mirrored into resources.
//!
//! That reasoning turned out to name the wrong blocker. Three of the four inputs
//! (`Sim.target`, `version_data`, the particle emitter) were plain owned values
//! that could have become resources at any point; the live block store stopped
//! being a blocker at Stage 4. What actually kept `drive_mining` out of a system
//! was that it reached the client through `&NetClient`, and `NetClient` holds an
//! `mpsc::Receiver`, which is `Send` but **not `Sync`** — so it can never be a
//! `Resource`. The fix is not to move `NetClient`: every read `drive_mining`
//! needs already goes through [`crate::net::SharedHandle`], which *is*
//! `Send + Sync + 'static`, and every write already has a sanctioned egress in
//! `lodestone_ecs::ActionQueue`. See `docs/sim-dissolution.md`.
//!
//! # The freeze that shipped with Stage 5, and what it cost
//!
//! "Every read goes through `SharedHandle`" was true and **not sufficient**, and
//! this is the correction. A `GameTick` system runs inside the `World` **write**
//! guard, and most of `ClientHandle`'s read-model accessors take a *read* guard on
//! that same `parking_lot::RwLock`. `drive_mining` called one — `player_menu`, for
//! the held item — so the client hard-froze on the first tick of the first dig:
//! no panic, no log line, just a window that stopped.
//!
//! The §4.1(c) audit had narrowed the lock rule to "the *chunk*-backed reads take
//! only the chunk lock", which is **correct** ([`NetHandle::block_at`] is one) and
//! was read as clearing `ClientHandle` generally. It does not: `player_menu`,
//! `open_menu`, `scoreboard`, `tab_list_view`, `boss_bars`, `health`, `player` and
//! the rest read `SharedState.ecs`. The lesson is the one §4.1(c) itself
//! implies — **there is one `World`, so a system should read the component, not
//! call the client** — and [`NetHandle::get`] is private now so the shape cannot
//! come back. `tests/mining_deadlock.rs` is the gate, with a control that
//! observes `player_menu` wedging under the guard.
//!
//! # How it works
//!
//! [`InteractPlugin`] registers five systems in `TickSet::Send`, ordered after
//! `lodestone_controller::ecs::send_player_input` by virtue of being added later
//! into the same set via an explicit `.after()`:
//!
//! 1. [`send_abilities`] — the flight/abilities state the server acks.
//! 2. [`send_sprint_command`] — vanilla's `LocalPlayer.sendIsSprintingIfNeeded`.
//! 3. [`drive_select_slot`] — a plugin's hotbar-selection wish, the same
//!    write-plus-echo `Sim::select_slot` uses.
//! 4. [`drive_mining`] — one tick of the hold-to-mine predictor.
//! 5. [`drive_placement`] — one tick of the placement predictor.
//!
//! **This list said "two systems" and named two while the code registered
//! three, and `drive_placement` was registered in no schedule at all** — found
//! by that fix's island sweep. Prose and code agreed with each other and both were
//! wrong, which is why nothing looked amiss: the only `add_systems` naming
//! `drive_placement` lived in `tests/place_intent.rs`'s hand-built `Schedule`,
//! so a plugin's `PlaceIntent` sat unconsumed forever while `BreakIntent`
//! worked. Human placement was unaffected throughout, going through
//! `Sim::use_item_live` rather than this path — which is what kept it hidden.
//! **If you add a system here, update this list in the same edit.**
//!
//! Both queue into [`ActionQueue`], which the driver drains to the socket once
//! per tick. **That is what preserves wire order**: before Stage 5 these two ran
//! after the queue was already drained, so their packets followed the tick's
//! movement packet; queueing them at the end of `TickSet::Send` puts them in the
//! same place in the same single ordered stream. Sending through
//! `ClientHandle::send_action` directly instead would have been a real
//! regression — that bypasses the net thread's action channel, so a mining
//! packet could overtake the movement packet queued microseconds earlier.
//!
//! # How to change it
//!
//! * **Adding a per-tick interaction:** add a system to `TickSet::Send` here and
//!   queue into `ActionQueue`. Never call `ClientHandle::send_action` from a
//!   system, for the ordering reason above.
//! * **Adding a per-*frame*, input-driven interaction** (a click handler):
//!   `ActionQueue` is drained inside the driver's tick loop, so a frame that runs
//!   no tick does not drain it — an action queued from a click can sit for up to
//!   one tick period. That is what vanilla does (input is handled in the tick),
//!   but it is *not* what this shell did before Stage 5, so
//!   `Sim::{end_attack, use_item_live, send_chat}` deliberately still send
//!   through `NetClient` directly rather than queueing. Changing that is a
//!   latency change, not a refactor.
//! * **The pick target** ([`RayTarget`]) is written once per frame by
//!   `Sim::update_target`, before the tick loop, and read by both systems here.
//!   It is not a `GameTick` product; do not move it into one, because mouse-look
//!   is per-frame (see `Sim::apply_mouse`).
//!
//! # Dependencies
//!
//! `lodestone_game::{mining, placement}` for the two predictors (plain state
//! machines the systems call — §8: verified logic stays a library),
//! `lodestone_ecs` for the sets/resources/components, `crate::particles` for the
//! emitter, `crate::net::SharedHandle` for every read of the client-owned
//! world, and — since issue #596 gave [`drive_mining`] its own local
//! block-edit prediction, the same as [`drive_placement`] already had —
//! `crate::mesher::TerrainMesh` plus [`lodestone_ecs::ChunkWorld`]/
//! [`lodestone_ecs::ChunkWorldWrite`] for the write and the re-mesh it makes
//! visible.

use lodestone_ecs::app::{App, Plugin};
use lodestone_ecs::ecs::prelude::{Commands, Entity, Query, Res, ResMut, With};
use lodestone_ecs::ecs::resource::Resource;
use lodestone_ecs::ecs::schedule::IntoScheduleConfigs;
use lodestone_client::{BlockPos, ClientAction, ClientHandle, Hand, Rotation};
use lodestone_ecs::player::{
    ActionQueue, BreakIntent, BreakOutcome, BreakRejection, BreakStatus, Dead, Egress, Flying,
    LastFlyingSent, LastSprintingSent, LocalPlayer, MovementIntent, PhysicsState, PlaceIntent,
    PlaceOutcome, PlaceRejection, PlaceStatus, Profile, SelectSlotIntent, SelectedSlot, Submersion,
};
use lodestone_ecs::session::{Abilities, ServerEntityId, SessionMenus};
use lodestone_ecs::veto::{ActionVetoes, VerbContext, Verdict};
use lodestone_ecs::{ChunkWorld, ChunkWorldWrite, FrameClock, GameTick, TickSet, VersionData};
use lodestone_game::mining::Mining;
use lodestone_game::placement::{Placement, UseOnContext, UseOnDecision};
use lodestone_model::{BlockFace, PlayerCommand};
use lodestone_physics::Vec3d;

use crate::blocks::id;
use crate::mesher::TerrainMesh;
use crate::net::SharedHandle;
use crate::particles::Particles;
use crate::raycast::{PickBox, RayHit, raycast};
use crate::sim::{
    AudioEngine, HOTBAR_SLOTS, OFFHAND_NATIVE_INDEX, bare_handed_tool_mining,
    block_intersects_player, block_sound_seed, block_states_of, dig_break_inputs, face_from_normal,
    hit_cursor, orientation_for_placement, particle_face, placement_facts, state_for_placement,
    write_predicted_block,
};

/// The block the view ray currently points at, for the outline and every edit.
///
/// Recomputed once per frame by `Sim::update_target` from the *interpolated*
/// camera, so it tracks the mouse at frame rate rather than tick rate — which is
/// vanilla's behaviour too (`MouseHandler.turnPlayer` runs off the render loop).
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct RayTarget(pub Option<RayHit>);

/// The living entity the view ray currently points at, for a left-click to
/// attack — vanilla's `Minecraft.hitResult` resolving to `HitResult.Type.ENTITY`
/// rather than `BLOCK`.
///
/// Recomputed alongside [`RayTarget`] by `Sim::update_target`, from the same
/// camera and against a *shorter* range: vanilla's `DEFAULT_ENTITY_INTERACTION_RANGE`
/// is `3.0` blocks (`Player.java`) versus `DEFAULT_BLOCK_INTERACTION_RANGE`'s
/// `4.5` (`Player.java`), and further capped by the block hit distance when
/// a block sits closer than that — an entity behind a wall cannot be targeted
/// through it. Holds the target's [`lodestone_ecs::entity::MinecraftEntityId`]
/// (the wire id `ClientAction::InteractEntity` needs), not a `bevy_ecs::Entity`,
/// so a consumer never has to resolve one through `EntityIndex` just to attack.
///
/// **A `Some` here takes priority over [`RayTarget`]** for `begin_attack`: a
/// closer entity is what vanilla's combined `clip()`/entity-pick would return
/// as the single `hitResult`, and `case ENTITY` never falls through to
/// `case BLOCK`. This resource does not itself suppress mining — `Sim::begin_attack`
/// is the one place that reads both and decides.
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct EntityRayTarget(pub Option<i32>);

/// The **non-living** entity type paths vanilla's pick ray accepts, sorted so
/// the lookup can binary-search. Companion to [`entity_type_can_be_picked`],
/// which reads the living half out of the generated entity census instead.
///
/// Every entry is one of the eight non-living override families named in that
/// function's doc — twenty boats/rafts, seven minecarts, four block-attached
/// decorations, and one each of the five singleton families. The three
/// redirectable projectiles are *not* here: they are resolved by tag in
/// [`entity_type_can_be_picked`], next to the citation for why.
const NON_LIVING_PICKABLE_PATHS: &[&str] = &[
    "acacia_boat",
    "acacia_chest_boat",
    "bamboo_chest_raft",
    "bamboo_raft",
    "birch_boat",
    "birch_chest_boat",
    "cherry_boat",
    "cherry_chest_boat",
    "chest_minecart",
    "command_block_minecart",
    "dark_oak_boat",
    "dark_oak_chest_boat",
    "end_crystal",
    "falling_block",
    "furnace_minecart",
    "glow_item_frame",
    "hopper_minecart",
    "interaction",
    "item_frame",
    "jungle_boat",
    "jungle_chest_boat",
    "leash_knot",
    "mangrove_boat",
    "mangrove_chest_boat",
    "minecart",
    "oak_boat",
    "oak_chest_boat",
    "painting",
    "pale_oak_boat",
    "pale_oak_chest_boat",
    "shulker_bullet",
    "spawner_minecart",
    "spruce_boat",
    "spruce_chest_boat",
    "tnt",
    "tnt_minecart",
];

/// The three `minecraft:redirectable_projectile` members — vanilla's
/// `Projectile.isPickable()` is exactly `is(EntityTypeTags.REDIRECTABLE_PROJECTILE)`,
/// and the tag's data file lists these three and nothing else. Kept apart from
/// [`NON_LIVING_PICKABLE_PATHS`] so the two different vanilla mechanisms stay
/// visibly different: one is a class override, this one is a datapack tag.
const REDIRECTABLE_PROJECTILE_PATHS: &[&str] = &["breeze_wind_charge", "fireball", "wind_charge"];

/// Vanilla's `EntitySelector.CAN_BE_PICKED` — the predicate every entity
/// candidate must pass before the view ray may resolve to it.
///
/// # Why this exists: without it the server kicks us
///
/// `Entity.isPickable()` is **`false`** by default, and `ItemEntity` and
/// `ExperienceOrb` do not override it — so vanilla's ray never resolves to a
/// dropped item or an orb, and vanilla therefore never sends an attack naming
/// one. The server treats that as a protocol violation rather than a no-op:
/// `ServerGamePacketListenerImpl.handleAttack` disconnects with
/// `multiplayer.disconnect.invalid_entity_attacked` ("Attempting to attack an
/// invalid entity") whenever the named target is an `ItemEntity`, an
/// `ExperienceOrb`, the player themselves, or a non-attackable `AbstractArrow`.
///
/// That is the whole reported bug: killing a mob spawns its drops and its
/// experience orbs inside the hitbox the mob just vacated, so the very next
/// left-click resolved to a drop and got the session kicked. Note what it is
/// **not** — a *removed* entity id is harmless, because `handleAttack` looks the
/// id up first and does nothing at all when it misses. The defect is picking a
/// live entity vanilla would never have picked, not picking a dead one.
///
/// # The reduction, and where each arm comes from
///
/// Derived by walking each of the 26.2 entity types' implementation classes (the
/// `impl` column of `lodestone-data`'s committed entity census dump) up to the
/// nearest class declaring `isPickable()`. Ten declaring classes cover all 159
/// types:
///
/// * `LivingEntity` (`!isRemoved()`), 90 types — every mob, plus `Player` and
///   `ArmorStand`, which narrow it further (see below). Read here out of the
///   census's own `is_living` column rather than re-listed.
/// * `AbstractBoat`, `AbstractMinecart`, `FallingBlockEntity`, `PrimedTnt`
///   (all `!isRemoved()`); `BlockAttachedEntity`, `EndCrystal`, `Interaction`,
///   `ShulkerBullet` (all `true`) — the 36 entries of
///   [`NON_LIVING_PICKABLE_PATHS`].
/// * `Projectile` — `is(EntityTypeTags.REDIRECTABLE_PROJECTILE)`, i.e.
///   [`REDIRECTABLE_PROJECTILE_PATHS`]. This is why arrows are excluded:
///   `AbstractArrow.isPickable()` is `super.isPickable() && !isInGround()`, and
///   its `super` is that tag test, which no arrow type is a member of — so
///   `arrow`, `spectral_arrow` and `trident` are **never** pickable and need no
///   in-ground state to decide it.
/// * `EnderDragon` — overrides to `false`; only its `EnderDragonPart`s are
///   pickable, and this client does not model parts.
/// * `Entity` — the `false` default: `item`, `experience_orb`,
///   `area_effect_cloud`, `evoker_fangs`, `eye_of_ender`, `lightning_bolt`,
///   `marker`, `ominous_item_spawner` and the three `Display` variants.
///
/// See `docs/entity-picking.md` for the citations behind each family.
///
/// # Per-instance refinements deliberately not applied
///
/// `Player.isPickable()` also requires `!isSpectator()` and
/// `ArmorStand.isPickable()` also requires `!isMarker()`. Both are entity
/// *state* this client does not track for remote entities, and neither is in
/// the server's rejection list — attacking a spectator or a marker stand is a
/// silent no-op server-side, not a kick — so they are reported as pickable
/// rather than approximated. Same reasoning as `EntityFacts::pushes_players`
/// exposing a type-level maximum and leaving state gates to the consumer.
///
/// # Default-deny, and why that costs nothing
///
/// A non-`minecraft` namespace, or a path this census has never seen, returns
/// `false` — matching vanilla's own `Entity.isPickable()` default. That is not
/// a new restriction: the pick loop already drops any entity
/// `VersionData::entity_facts` cannot size, and that table is the same 26.2
/// census, so an unknown type was unpickable before this predicate existed.
#[must_use]
pub fn entity_type_can_be_picked(kind: &lodestone_model::ResourceKey) -> bool {
    if kind.namespace() != "minecraft" {
        return false;
    }
    let path = kind.path();
    // Checked ahead of the living column: the dragon *is* a `LivingEntity` and
    // overrides `isPickable()` back to `false`.
    if path == "ender_dragon" {
        return false;
    }
    if lodestone_data::entity_types::entity_type_id_parts("minecraft", path)
        .and_then(lodestone_data::entity_census::is_living)
        == Some(true)
    {
        return true;
    }
    NON_LIVING_PICKABLE_PATHS.binary_search(&path).is_ok()
        || REDIRECTABLE_PROJECTILE_PATHS.binary_search(&path).is_ok()
}

/// Whether the attack (left) button is currently held.
///
/// Drives the live hold-to-mine loop. A demo-world break is a one-shot on press
/// instead, so this stays `false` off a live session and [`drive_mining`] is a
/// cheap no-op there.
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct Attacking(pub bool);

/// Whether the use (right) button has been pressed and not yet released.
///
/// The client-side mirror of vanilla's `Minecraft.java`,
/// `this.player.isUsingItem()`, which gates whether releasing `key.use` sends
/// `RELEASE_USE_ITEM` (`:1916`, `gameMode.releaseUsingItem`). Vanilla's own
/// flag comes from a held item's `use()` running identically client- and
/// server-side (a bow's `use()` calls `LivingEntity.startUsingItem` on both),
/// which this client has no local simulation of — there is no item registry
/// here that can say "yes, that bow just started a held use." So this is an
/// **input-state** mirror instead: true from [`crate::sim::Sim::use_item`]'s
/// live press until either its release counterpart or a client-known consumable
/// completion. A held consumable completion immediately re-enters the same use
/// path for the next bite, so this is a superset of vanilla's real gate rather
/// than an exact match.
///
/// That gap is inert, not a wrong state transition: `LivingEntity
/// .releaseUsingItem` (`.cache/mc/26.2/src/…/LivingEntity.java`)
/// already no-ops whenever the server itself has no `useItem` in progress, so
/// a `RELEASE_USE_ITEM` sent while nothing was really being used is a
/// harmless duplicate. Same shape as [`Attacking`] — a plain press/release
/// mirror the click handlers set directly — but this one is consulted by the
/// *release* edge to decide whether to send at all, not by a per-tick system.
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct UsingItem(pub bool);

/// The live block-mining predictor (`START`/`STOP`/`ABORT` + swing), owning its
/// own prediction-sequence counter and post-break cooldown.
#[derive(Resource, Debug, Default)]
pub struct MiningPredictor(pub Mining);

/// The live block-placement predictor, owning its own prediction-sequence
/// counter.
#[derive(Resource, Debug, Default)]
pub struct PlacementPredictor(pub Placement);

/// The vanilla particle simulation.
///
/// A resource rather than a `Sim` field since Stage 5, which is what lets
/// [`drive_mining`] emit the per-tick mining chip from inside a system. Its
/// *tick* is deliberately still driven by the shell rather than being a
/// `TickSet::Animate` system — see `Sim::tick_particles` for the two documented
/// ways its collision decision differs from the player's, which is a behaviour
/// question and not this stage's to settle.
#[derive(Resource, Debug)]
pub struct ParticleSim(pub Particles);

/// The `Send + Sync` half of the live connection: the `Arc<OnceLock<…>>` the net
/// thread publishes its [`lodestone_client::ClientHandle`] into once login
/// completes.
///
/// This is the resource that unblocked Stage 5's interaction systems. `NetClient`
/// itself can never be one — it holds an `mpsc::Receiver`, which is `!Sync` — but
/// every *read* on `NetClient` other than `poll()` is already a delegation to
/// this handle, so a system needs nothing else. `None` before
/// `Sim::attach_net`; `Some` holding an unfilled `OnceLock` between attach and
/// login, which reads exactly like "no data yet" everywhere.
#[derive(Resource, Debug, Default)]
pub struct NetHandle(pub Option<SharedHandle>);

impl NetHandle {
    /// The published client handle, or `None` before login.
    ///
    /// # Deliberately private, and this is the whole bug fix
    ///
    /// A `GameTick` system runs inside `run_schedule(GameTick)`, which the driver
    /// runs inside [`lodestone_ecs::hold_write`] — i.e. under the `World`
    /// **write** guard. Most of [`ClientHandle`]'s read-model accessors
    /// (`player_menu`, `open_menu`, `scoreboard`, `tab_list_view`, `boss_bars`,
    /// `health`, `player`, …) take `ecs.read()` on **that same**
    /// `Arc<parking_lot::RwLock<World>>`, and `parking_lot`'s `RwLock` is not
    /// reentrant. Calling one from a system is an immediate, silent, permanent
    /// deadlock — no panic, no log line, the window simply stops.
    ///
    /// That is exactly what shipped: `drive_mining` resolved the held item with
    /// `net.get().map(ClientHandle::player_menu)`, so the client froze on the
    /// first tick of the first dig. It reproduces hermetically in
    /// `tests/mining_deadlock.rs`.
    ///
    /// So the handle does not leave this type. What the accessors below expose is
    /// exactly the set that is **chunk**-backed — a different lock, taken and
    /// released inside the call, never nested with the `World` guard. Adding one
    /// here is safe only after checking `lodestone_client::state`: if the body
    /// touches `self.ecs`, it must not be reachable from a system, and the right
    /// answer is to read the component out of the `World` the system is already
    /// inside (which is where `SessionMenus` comes from now — there is one
    /// `World`, so the round trip through the client bought nothing anyway).
    fn get(&self) -> Option<&ClientHandle> {
        self.0.as_ref()?.get().map(std::convert::AsRef::as_ref)
    }

    /// The single block state at a world position in the client-owned world, or
    /// `None` when that column/section is not held (before login, or outside the
    /// loaded region).
    ///
    /// **Chunk lock only.** `SharedState::block_at` reads `self.world` (the
    /// `std::sync::RwLock` chunk store), never `self.ecs`, so this is legal from
    /// inside the `World` write guard — the §4.1(c) audit's conclusion on that
    /// point is correct and `tests/mining_deadlock.rs` pins it with a positive
    /// assertion rather than leaving it as prose.
    #[must_use]
    pub fn block_at(&self, pos: BlockPos) -> Option<u32> {
        self.get()?.block_at(pos)
    }
}

/// `LocalPlayer.sendIsSprintingIfNeeded` (`LocalPlayer.java`): put the
/// sprint **edge** on the wire as a `PlayerCommand`.
///
/// The source of truth is [`PhysicsState`]'s `sprinting`, which the physics tick
/// assigns from the movement intent — so what the server hears is what actually
/// drove this tick's movement, not a re-read of the keyboard. This is the packet
/// that makes the server set `isSprinting()`, and therefore the packet that makes
/// its `updateSwimming` agree with ours.
///
/// A dead player is not sprinting, nor is one in the shell's free-fly debug cam
/// (which never runs a physics tick, so `sprinting` would sit stale), and no
/// command is sent before the server has given us an entity id — the packet
/// carries it.
///
/// # `Egress` gates the latch, not just the send
///
/// Same rule as `send_player_input`, and for the same reason: a system that ran
/// while disconnected would record the current value into [`LastSprintingSent`]
/// as "already sent", and the first real change after connecting would then be
/// suppressed as a redundant resend. Before Stage 5 the equivalent gate was the
/// `if phase == Connected && is_live()` around `Sim::drive_interaction`, which is
/// exactly what [`Egress`]'s two bits are.
pub fn send_sprint_command(
    egress: Res<Egress>,
    mut queue: ResMut<ActionQueue>,
    mut players: Query<
        (
            &PhysicsState,
            &Flying,
            Option<&Dead>,
            &ServerEntityId,
            &mut LastSprintingSent,
        ),
        With<LocalPlayer>,
    >,
) {
    if !(egress.in_world && egress.live) {
        return;
    }
    for (state, flying, dead, entity_id, mut last) in &mut players {
        let sprinting = state.0.sprinting && dead.is_none() && !flying.0;
        if last.0 == Some(sprinting) {
            continue;
        }
        let Some(entity_id) = entity_id.0 else {
            continue;
        };
        last.0 = Some(sprinting);
        queue.0.push(ClientAction::PlayerCommand {
            entity_id,
            command: if sprinting {
                PlayerCommand::StartSprinting
            } else {
                PlayerCommand::StopSprinting
            },
        });
    }
}

/// `TickSet::Send`: echo creative flight to the server as
/// [`ClientAction::SetFlying`], mirroring `Player.onUpdateAbilities()` →
/// `ServerboundPlayerAbilitiesPacket`.
///
/// # Why this exists, and what it closes
///
/// The flight toggle is **client-authoritative in vanilla**: the client flips
/// `abilities.flying` locally (`LocalPlayer.aiStep`) and tells the server after
/// the fact. Without this echo the server keeps simulating a walking player,
/// its `handleMovePlayer` replay diverges from the position we report, and it
/// either teleports us back or eventually disconnects us with
/// `multiplayer.disconnect.flying`.
///
/// `ClientAction::SetFlying` was an **island** before this: four protocol
/// adapters encode it, nothing produced it. This is its first producer.
///
/// # Edge-triggered, and the latch is gated on `Egress`
///
/// Exactly the shape [`send_sprint_command`] uses, for exactly its reasons — a
/// system that ran while disconnected would latch the current value as
/// "already sent" and swallow the first real change after connecting.
///
/// Unlike the sprint command this packet carries **no entity id**, so it does not
/// need [`ServerEntityId`] to be populated; `Egress::in_world` is the whole
/// precondition.
pub fn send_abilities(
    egress: Res<Egress>,
    mut queue: ResMut<ActionQueue>,
    mut players: Query<(&Abilities, &mut LastFlyingSent), With<LocalPlayer>>,
) {
    if !(egress.in_world && egress.live) {
        return;
    }
    for (abilities, mut last) in &mut players {
        if last.0 == Some(abilities.flying) {
            continue;
        }
        last.0 = Some(abilities.flying);
        queue.0.push(ClientAction::SetFlying {
            flying: abilities.flying,
        });
    }
}

/// `TickSet::Send`: consume a plugin's [`SelectSlotIntent`] by performing the
/// same write-plus-echo `Sim::select_slot` uses — write [`SelectedSlot`], queue
/// [`ClientAction::SetCarriedItem`] so the server's notion of the held item
/// stays in sync — then remove the intent: one insertion is one attempt.
///
/// # Why through `ActionQueue`, never `ClientHandle::send_action`
///
/// The same rule [`drive_mining`]/[`drive_placement`] follow (this module's own
/// docs spell out the reason): a system must not send directly, or its packet
/// could overtake the movement packet queued microseconds earlier. Queueing at
/// the end of `TickSet::Send` puts the echo in the same single ordered stream
/// the human's own `select_slot` produces — and, when a plugin inserts both a
/// `SelectSlotIntent` and a [`PlaceIntent`] in one tick, this system running
/// **before** [`drive_placement`] means the `SetCarriedItem` reaches the server
/// first, so the placement is judged against the newly selected item.
///
/// # Not gated on `Egress`, unlike [`send_sprint_command`]/[`send_abilities`]
///
/// Those two latch their edge-trackers and must not run disconnected, or the
/// first real change after connecting would be swallowed as a redundant resend.
/// This system latches nothing, and the local write is meaningful off a live
/// connection too — `Sim::select_slot`'s own docs say exactly that ("No-op off
/// a live connection beyond updating the local selection the HUD draws") — so a
/// plugin that selects a slot before the world loads still changes what the HUD
/// highlights. The echo simply drops: [`ActionQueue`] is only sent to the
/// socket by `Sim::drain_action_queue` when a connection exists.
///
/// # No legality surface to report on
///
/// Unlike a block edit, any slot `0..9` is always selectable, so there is no
/// [`BreakOutcome`]-style rejection a plugin could act on. An out-of-range
/// value and a no-op (the slot already selected) are ignored exactly as
/// `Sim::select_slot`'s own range/same-slot gate ignores them.
pub fn drive_select_slot(
    mut commands: Commands,
    mut queue: ResMut<ActionQueue>,
    mut players: Query<(Entity, &mut SelectedSlot, &SelectSlotIntent), With<LocalPlayer>>,
) {
    for (entity, mut selected, intent) in &mut players {
        // Removed whether or not the slot changed — one insertion is one
        // attempt, the same acknowledgement `SelectSlotIntent`'s own docs (and
        // `PlaceIntent`'s) describe.
        commands.entity(entity).remove::<SelectSlotIntent>();
        let slot = intent.0;
        if slot >= HOTBAR_SLOTS || slot == selected.0 {
            continue;
        }
        selected.0 = slot;
        queue.0.push(ClientAction::SetCarriedItem { slot: slot as i32 });
    }
}

/// [`BlockFace`] to its outward unit normal — the inverse of
/// [`face_from_normal`], needed because a [`BreakIntent`] carries a face
/// (like a mouse ray hit's [`RayHit::normal`]) rather than a direction to cast
/// along.
fn face_to_normal(face: BlockFace) -> [i32; 3] {
    match face {
        BlockFace::Down => [0, -1, 0],
        BlockFace::Up => [0, 1, 0],
        BlockFace::North => [0, 0, -1],
        BlockFace::South => [0, 0, 1],
        BlockFace::West => [-1, 0, 0],
        BlockFace::East => [1, 0, 0],
    }
}

/// Cast a ray from `eye` toward the centre of `face` on `pos`, through the
/// version's own [`VersionData::block_outline`] census, and accept only a hit
/// that lands back on `pos` — the shared core of [`resolve_break_intent`] and
/// [`resolve_place_intent`].
///
/// A plugin has no crosshair, so there is no ray to read for either intent —
/// this casts one of its own, the same way [`crate::raycast::raycast`]'s only
/// other caller (the mouse-driven `Sim::update_target`) does. Accepting the
/// resolved hit **only when it lands on the intended cell** is what makes
/// this a real reach-and-line-of-sight check rather than a rubber stamp: a
/// closer block in the way, or a target beyond vanilla's 4.5-block
/// [`crate::raycast::REACH`], both resolve to a different cell (or no hit at
/// all) and are rejected identically — see [`BreakRejection::UnreachableOrObstructed`]'s
/// own doc on why the two share one variant; [`PlaceRejection::UnreachableOrObstructed`]
/// makes the identical choice for the same reason.
///
/// Cells with no live block data (`NetHandle::block_at` returning `None`) are
/// treated as untargetable rather than solid — the same "not painted, not an
/// obstruction" answer the mouse-driven cast gives for a chunk that has not
/// streamed in, and it is what makes a target beyond the loaded world resolve
/// as unreachable rather than panicking or inventing geometry.
fn resolve_intent_ray(
    pos: BlockPos,
    face: BlockFace,
    eye: Vec3d,
    net: &NetHandle,
    version: &VersionData,
) -> Option<RayHit> {
    let block = [pos.x, pos.y, pos.z];
    let normal = face_to_normal(face);
    let aim_point = RayHit::face_center(block, normal).hit;
    let origin = [eye.x, eye.y, eye.z];
    let dir = [
        aim_point[0] - origin[0],
        aim_point[1] - origin[1],
        aim_point[2] - origin[2],
    ];
    let hit = raycast(origin, dir, crate::raycast::REACH, |x, y, z, out| {
        let Some(state) = net.block_at(BlockPos::new(x, y, z)) else {
            return;
        };
        let Some(boxes) = version.block_outline(state) else {
            return;
        };
        out.extend(boxes.iter().map(|b| PickBox {
            min: [f64::from(b.min[0]), f64::from(b.min[1]), f64::from(b.min[2])],
            max: [f64::from(b.max[0]), f64::from(b.max[1]), f64::from(b.max[2])],
        }));
    });
    match hit {
        Some(resolved) if resolved.block == block => Some(resolved),
        _ => None,
    }
}

/// Resolve a plugin's [`BreakIntent`] into the same [`RayHit`] shape a mouse
/// click's [`RayTarget`] would produce, or reject it. See [`resolve_intent_ray`].
fn resolve_break_intent(
    intent: BreakIntent,
    eye: Vec3d,
    net: &NetHandle,
    version: &VersionData,
) -> Result<RayHit, BreakRejection> {
    resolve_intent_ray(intent.pos, intent.face, eye, net, version)
        .ok_or(BreakRejection::UnreachableOrObstructed)
}

/// Resolve a plugin's [`PlaceIntent`] the same way, into the [`RayHit`] shape
/// `Sim::use_item_live`'s own mouse-driven `clicked`/`face` derivation reads.
/// See [`resolve_intent_ray`].
fn resolve_place_intent(
    intent: PlaceIntent,
    eye: Vec3d,
    net: &NetHandle,
    version: &VersionData,
) -> Result<RayHit, PlaceRejection> {
    resolve_intent_ray(intent.pos, intent.face, eye, net, version)
        .ok_or(PlaceRejection::UnreachableOrObstructed)
}

/// Drive the live mining predictor one tick from the held attack button and the
/// current target.
///
/// Holding the button keeps the dig active: the predictor emits a `START` on
/// first press, accumulates `getDestroyProgress` every tick thereafter, and emits
/// the `STOP_DESTROY` on the tick its own progress reaches `1.0` — the same tick
/// vanilla's client would, because it is fed the same per-block hardness vanilla
/// reads off the `BlockState`.
///
/// The hardness comes from [`VersionData::block_hardness`] keyed on the *live*
/// state id ([`NetHandle::block_at`]), so it is real version data rather than a
/// shell-side guess. A state the version cannot resolve (or a build with no family
/// compiled in) **aborts the dig** instead of substituting a number: guessing one
/// is precisely how block breaking got too fast the first time, and that defect's
/// signature was a crack overlay pulsing through all ten stages in a quarter
/// second regardless of the block.
///
/// # Why the chip particle is emitted on an OR of before/after
///
/// Vanilla's `ClientLevel.addBreakingBlockEffect` fires from
/// `Minecraft.continueAttack` whenever `MultiPlayerGameMode.continueDestroyBlock`
/// returns `true`, which includes the very tick a fresh dig starts (both
/// `startAttack` and `continueAttack` run off the same `handleKeybinds` pass, so
/// `sameDestroyTarget` is already true by the time `continueDestroyBlock` runs).
/// We have one call where vanilla has two, so the tick-one case has to be read off
/// `Mining::target()` both before and after the call and OR'd. Only
/// "before none, after none" survives, which is the instant-break-from-idle and
/// post-break-cooldown cases — the latter a deliberate, documented divergence
/// matching this port's existing choice not to send a block-action packet during
/// cooldown either.
///
/// # A plugin's [`BreakIntent`], and why it joins here rather than getting its
/// own system
///
/// Consulted only when the human is **not** attacking (see [`BreakIntent`]'s
/// own docs on why the human path takes priority) — resolved by
/// [`resolve_break_intent`] into the identical [`RayHit`] shape a mouse click
/// produces, then handed to the exact same `pos`/`face`/`id_value`/`entry`
/// pipeline below. That reuse is the point: a plugin's dig is not a second,
/// parallel implementation that could drift from the human one, it is the
/// same code with a different source for `hit`. [`BreakOutcome`] is written
/// at every point this function would otherwise silently do nothing with the
/// intent, so a plugin can tell a stalled dig from a progressing one — see
/// that component's own docs for why an unreported rejection is a silent
/// autopilot stall.
#[allow(clippy::too_many_arguments)]
pub fn drive_mining(
    egress: Res<Egress>,
    attacking: Res<Attacking>,
    target: Res<RayTarget>,
    net: Res<NetHandle>,
    version: Res<VersionData>,
    // The chunk store's two halves, for the local block-edit prediction (issue
    // #596) — the same pair [`drive_placement`] already takes, for the same
    // reason: the read handle for the re-mesh, the write handle because only
    // the store's legitimate writers may hold one (see [`ChunkWorldWrite`]'s
    // own docs).
    chunk_world: Res<ChunkWorld>,
    write: Res<ChunkWorldWrite>,
    clock: Res<FrameClock>,
    mut terrain: ResMut<TerrainMesh>,
    mut mining: ResMut<MiningPredictor>,
    mut particles: ResMut<ParticleSim>,
    mut audio: ResMut<AudioEngine>,
    mut queue: ResMut<ActionQueue>,
    // That fix's veto registry. `Option`, so this system is unchanged for a
    // client that installed no plugin.
    vetoes: Option<Res<ActionVetoes>>,
    mut players: Query<
        (
            &PhysicsState,
            &Submersion,
            &SelectedSlot,
            Option<&Dead>,
            Option<&SessionMenus>,
            Option<&BreakIntent>,
            &mut BreakOutcome,
            Option<&Abilities>,
        ),
        With<LocalPlayer>,
    >,
) {
    if !(egress.in_world && egress.live) {
        return;
    }
    let Ok((state, submersion, slot, dead, menus, intent, mut outcome, abilities)) =
        players.single_mut()
    else {
        return;
    };
    // `Abilities.instabuild` — vanilla's own creative-instant-break check,
    // consulted by `Mining::start` ahead of the hardness formula. `Option`
    // because `Abilities` only arrives once the login `PLAYER_ABILITIES`
    // packet lands; `false` (ordinary survival formula) until then, which is
    // also the only sane default for a spectator/session with no server
    // connection at all.
    let creative = abilities.is_some_and(|a| a.instabuild);

    let human_attacking = attacking.0 && dead.is_none();
    // `via_intent` distinguishes "no hit, human idle" from "no hit, a plugin's
    // intent was rejected" — only the latter owes `outcome` a write below, and
    // only the latter is allowed to overwrite an `Idle` a previous branch
    // already set.
    let (hit, via_intent) = if human_attacking {
        (target.0, false)
    } else if let Some(intent) = intent {
        if dead.is_some() {
            outcome.0 = BreakStatus::Rejected(BreakRejection::Dead);
            (None, true)
        } else {
            let eye = Vec3d::new(
                state.0.position.x,
                state.0.position.y + f64::from(state.0.eye_height),
                state.0.position.z,
            );
            match resolve_break_intent(*intent, eye, &net, &version) {
                Ok(resolved) => (Some(resolved), true),
                Err(reason) => {
                    outcome.0 = BreakStatus::Rejected(reason);
                    (None, true)
                }
            }
        }
    } else {
        (None, false)
    };
    if !via_intent {
        // Human-driven or genuinely idle: this tick has nothing to report
        // *from the plugin*, regardless of how the human's own dig is going.
        outcome.0 = BreakStatus::Idle;
    }

    // Not attacking (or no target / dead / rejected intent): abort any live
    // dig. `stop()` is idempotent — one `ABORT` for a live dig, nothing on
    // later ticks.
    let Some(hit) = hit else {
        queue.0.extend(mining.0.stop());
        return;
    };
    let pos = BlockPos::new(hit.block[0], hit.block[1], hit.block[2]);
    let face = face_from_normal(hit.normal);
    // No live state at this position (or no live connection): same "abort, never
    // guess" contract as the unknown-state case below.
    let Some(id_value) = net.block_at(pos) else {
        if via_intent {
            outcome.0 = BreakStatus::Rejected(BreakRejection::NoWorldData);
        }
        queue.0.extend(mining.0.stop());
        return;
    };
    let Some(entry) = version.block_hardness(id_value) else {
        if via_intent {
            outcome.0 = BreakStatus::Rejected(BreakRejection::UnknownBlockState);
        }
        queue.0.extend(mining.0.stop());
        return;
    };
    if via_intent {
        outcome.0 = BreakStatus::Progressing;
    }

    // The held item's contribution (speed, correct-tool-for-drops), resolved
    // through the same version-owned seam as `entry`. Falls back to bare hand —
    // not a guess: it is what an empty main hand *is* — when nothing is held, and,
    // defensively, when the version's tool census has nothing for this state
    // either (which `entry` above already proves should not happen).
    //
    // Read straight off the component, **never** through
    // `ClientHandle::player_menu`. That accessor takes a read guard on the very
    // `World` this system is running inside, which deadlocked the client on the
    // first tick of every dig (see `NetHandle::get`). Since §4.1(c) there is one
    // `World` and `lodestone_ecs::session`'s `NetIngest` fold writes `SessionMenus`
    // into *this* one, so the component and the accessor were already the same
    // bytes — the round trip only added the lock. It is also cheaper: the accessor
    // cloned the whole 46-slot menu per tick to read one stack.
    //
    // `Option<&SessionMenus>` rather than a required term, so a `World` whose local
    // player carries no session components degrades to bare-handed instead of
    // failing `single()` and aborting every dig — and *no* `Menu::player()`
    // fallback is needed, because a fresh player menu is empty, so it would answer
    // `None` for every slot anyway. That is the pre-fix behaviour exactly.
    // `Menus::player_native` rather than `player().player_native(..)`: the one
    // inventory is owned by the *open container's* menu while a screen is up
    // so reading window 0's menu here returned a stale stack and
    // mined at the wrong speed with a tool picked up inside a chest. It also
    // drops the whole-menu clone this comment complains about.
    let held = menus
        .and_then(|menus| menus.0.player_native(slot.0))
        .map(crate::sim::tool_mining_item);
    let tool = version
        .tool_mining(held.as_ref(), id_value)
        .unwrap_or_else(|| bare_handed_tool_mining(entry));
    let inputs = dig_break_inputs(
        entry,
        tool,
        id_value == id::AIR,
        state.0.on_ground,
        // `eye_in_water`, not `under_water()` — see "Trap 2" on `dig_break_inputs`.
        submersion.0.eye_in_water,
        creative,
    );

    // That fix's block-break veto, asked *before* `continue_` advances the dig
    // state machine — a plugin that finds out afterward is too late, which is the
    // whole complaint the issue opens with. A denial aborts any live dig via the
    // same idempotent `stop()` every other early return above uses, so a
    // protection plugin denying mid-hold sends one ABORT rather than stranding
    // the predictor with a dig the server will never see finished.
    //
    // `Option<Res<..>>`: opt-in, so a client with no plugin never has the
    // resource and pays a `None` check.
    if let Some(vetoes) = &vetoes {
        let verdict = vetoes.allows(&VerbContext::BlockBreak {
            pos,
            state_id: Some(id_value),
        });
        if verdict == Verdict::Deny {
            if via_intent {
                outcome.0 = BreakStatus::Rejected(BreakRejection::Vetoed);
            }
            queue.0.extend(mining.0.stop());
            return;
        }
    }

    let was_mining = mining.0.target().is_some();
    // `continue_` delegates to `start` when no dig is live yet, so this one entry
    // point covers first-press, hold, and retarget uniformly.
    let actions = mining.0.continue_(pos, face, &inputs, None);
    let is_mining_now = mining.0.target().is_some();
    if (was_mining || is_mining_now)
        && actions
            .iter()
            .any(|a| matches!(a, ClientAction::SwingArm { .. }))
    {
        // Full-cube shape and untinted white, for the same reason as the
        // destroy-burst debris: the shell does not carry a block's outline shape,
        // so the chip approximates with the unit cube rather than the true model.
        particles
            .0
            .breaking_block(hit.block, id_value, [1.0; 3], particle_face(face));
    }
    // The debris burst at the moment a block actually breaks.
    // Keyed on **destruction**, not on the `StopDestroy` packet.
    //
    // This is the local **prediction** half of vanilla's
    // `MultiPlayerGameMode.destroyBlock` (`MultiPlayerGameMode.java`):
    // it clears the block and throws the destroy-effect debris synchronously
    // on the acting client, without waiting for a server round trip. The
    // effect hangs off that method, not off any packet — `destroyBlock` calls
    // `Block.playerWillDestroy` → `spawnDestroyParticles` →
    // `level.levelEvent(player, 2001, pos, id)`, and on `ClientLevel` that
    // dispatches **locally** into `LevelEventHandler`'s `case 2001`
    // (`addDestroyBlockEffect` + the break sound). The `player` argument is why
    // the server's copy of the same call does not double it: `ServerLevel`
    // broadcasts a `levelEvent` to everyone *except* that player.
    //
    // This originally scanned `actions` for `BlockActionKind::StopDestroy`,
    // which is one of the four ways vanilla reaches `destroyBlock` rather than
    // the funnel itself — and it is the one an **instant break never takes**.
    // `Mining::start`'s `progress_per_tick() >= 1.0` branch emits
    // `StartDestroy` and nothing more, because the block is already gone, so
    // grass, saplings and flowers threw no debris at all while stone did
    // (reported from play). `Mining::take_destroyed` is the funnel:
    // both `start`'s instant-break branch and `continue_`'s progress-reached-1.0
    // branch latch it, so keying on it removes the class instead of
    // special-casing one-shot blocks.
    //
    // Before this, the **only** burst trigger anywhere in the shell was the
    // server-driven `NetUpdate::BlockDestroyed` arm (`Sim::step`'s live-update
    // match, fed by `ClientboundLevelEventPacket` id `2001`) — which
    // structurally **never fires for our own break**, verified against
    // `.cache/mc/26.2/src` rather than assumed:
    // `ServerPlayerGameMode.destroyBlock` (`ServerPlayerGameMode.java`,
    // the server's handler for a player's own break) calls
    // `this.level.removeBlock(pos, false)` — a plain block-state write with no
    // `levelEvent` call anywhere in it. The `2001` particle event instead lives
    // in the *separate* `Level.destroyBlock(pos, drop, breaker, limit)` method
    // (`Level.java`, `this.levelEvent(2001, pos, ...)`), which is what a
    // cascading break (a torch losing support, fire, an explosion) goes through
    // instead — and that call broadcasts to **every** nearby player
    // unconditionally, our own client included, which is exactly the
    // "cascaded breaks already showed particles, my own break never did"
    // asymmetry that was reported. There is no player-exclusion filter to rely
    // on; the two break paths are simply different methods, and only one of
    // them ever touches `levelEvent` at all.
    //
    // No double-burst risk from adding this: our own break structurally cannot
    // reach the `levelEvent`/`2001` path in the first place, so this predicted
    // emit and a `NetUpdate::BlockDestroyed` for the *same* break can never
    // both fire. A **mispredicted** break (the server rejects the dig) is a
    // pre-existing, unrelated gap — nothing currently rolls back a
    // wrongly-predicted client-side block edit either — and is no worse here
    // than it already is for the progressive mining chips a few lines above,
    // which predict exactly as eagerly.
    //
    // # The local block-edit prediction (issue #596)
    //
    // Vanilla's `destroyBlock` does not just spawn debris — it first sets the
    // block to air *locally, synchronously*
    // (`level.setBlock(pos, fluidState.createLegacyBlock(), 11)`,
    // `MultiPlayerGameMode.java`), before any server round trip. This shell
    // used to predict only the particle burst and leave the actual block-state
    // write to the server's `BLOCK_UPDATE` ack: on a laggy connection that
    // showed the normal break animation completing and then the block vanishing
    // only once the ack landed, rather than disappearing on the same tick a
    // real client would. Writing the state here — through the same
    // [`write_predicted_block`] + [`crate::mesher::TerrainMesh::remesh_around`]
    // pair [`drive_placement`] uses for its own predicted edit — closes that
    // gap: the cell reads as air, and the mesh reflects it, on the exact tick
    // [`Mining::take_destroyed`] fires, with no wait for the server.
    //
    // A stray re-latch on the *same* target the very next tick cannot happen
    // regardless of this write: both destroy paths in [`Mining`] arm its 5-tick
    // `delay` immediately (`start`'s instant-break branch and `continue_`'s
    // progress-reached-`1.0` branch both do), and `continue_` checks that
    // cooldown **before** it ever reads the target's block state — see
    // `Mining::continue_`'s own docs. This write's job is narrower: making the
    // *visible* result agree with the server's eventual one immediately,
    // rather than only once the ack round trip completes. A mispredicted break
    // (the server rejects the dig) still has no rollback — the same accepted
    // gap [`drive_placement`]'s predicted write carries, and no worse here.
    if let Some(destroyed) = mining.0.take_destroyed() {
        // `hit.block` rather than the latched `destroyed` position: they are
        // the same cell (`pos` is built from `hit.block` above and is what
        // both predictor entry points were handed) — asserted rather than
        // silently assumed, since a mismatch here would write the wrong cell.
        debug_assert_eq!(
            destroyed,
            pos,
            "Mining::take_destroyed must name the cell drive_mining just aimed \
             at, or this write lands on the wrong block"
        );
        {
            let mut world = write.write();
            write_predicted_block(&mut *world, hit.block, id::AIR);
        }
        terrain.remesh_around(&chunk_world, hit.block);
        // Full-cube shape and untinted white, for the same reason as the
        // mining-chip particle a few lines up: the shell does not carry a
        // block's outline shape, and `destroy_block` itself resolves the
        // real per-state tint (see its own docs) — `[1.0; 3]` is the
        // multiplier, not a placeholder colour.
        //
        // `id_value`, not `id::AIR`: the burst must show the block that *was*
        // there, not the air it just became.
        particles.0.destroy_block(hit.block, id_value, [1.0; 3]);
        // The break sound, predicted locally for the same reason
        // `drive_placement`'s is: `ServerPlayerGameMode.destroyBlock` calls
        // `this.level.removeBlock(pos, false)` with no `levelEvent`/`playSound`
        // anywhere in it (`docs/sound-playback.md`), so a player's own dig never
        // produces a `2001` packet for `NetUpdate::BlockDestroyed` to catch —
        // vanilla's own client makes it audible by having `ClientLevel.levelEvent`
        // dispatch `case 2001` locally, sound and debris together
        // (`MultiPlayerGameMode.destroyBlock` → `Block.spawnDestroyParticles` →
        // `level.levelEvent(player, 2001, …)`). This system could not reach
        // `ShellAudio` before `AudioEngine` became a resource; now it reads it
        // exactly as `drive_placement` already does. `id_value` — the state that
        // *was* there — not `id::AIR`, matching `sim::actions::Sim::break_block`'s
        // offline mirror of the same case.
        if let Some(sound) = lodestone_data::sound_types::sound_type(id_value)
            && let Some(sound_name) = lodestone_data::sound_types::break_sound_name(id_value)
            && let Some(engine) = &mut audio.0
        {
            engine.play_sound(
                sound_name,
                lodestone_model::event::SoundCategory::Block,
                glam::Vec3::new(
                    hit.block[0] as f32 + 0.5,
                    hit.block[1] as f32 + 0.5,
                    hit.block[2] as f32 + 0.5,
                ),
                sound.break_or_place_volume(),
                sound.break_or_place_pitch(),
                block_sound_seed(hit.block, clock.ticks),
            );
        }
    }
    queue.0.extend(actions);
}

/// Drive a plugin's [`PlaceIntent`] one tick — the placement mirror of
/// [`drive_mining`], and `docs/plugin-api.md`'s re-mesh-seam note is what
/// makes this possible at all: before it, the local write was reachable from
/// `ChunkWorld` alone, but the re-mesh that makes it *visible* reached into
/// `Sim`'s own mesh-worker pool, which was not a resource.
///
/// # Why placement is a pre-check-then-act shape, unlike `drive_mining`
///
/// A human right-click always sends `use_item_on` and lets
/// [`Placement::use_on`] sort out interact-vs-place-vs-nothing after the
/// fact — vanilla does the same (`Level.playLocalSound`'s packet always goes
/// out). A `PlaceIntent` is narrower: it specifically asks to *place*, so
/// every [`PlaceRejection`] below is checked **before** `use_on` runs and
/// before anything reaches [`ActionQueue`], rather than folded into a generic
/// [`UseOnDecision::Nothing`] the way a human miss would be. That is what
/// lets [`PlaceRejection::NothingPlaceableHeld`] and
/// [`PlaceRejection::IntersectsPlayer`] exist as distinct, checkable reasons
/// instead of one undifferentiated "nothing happened" — see [`PlaceStatus`]'s
/// own docs on why [`PlaceStatus::SentUnpredicted`] is reserved for the cases
/// vanilla itself would still send a packet for (an interactable block, or a
/// placeable item the census cannot resolve a state for).
///
/// # Sources, matching `docs/plugin-api.md`'s wiring list exactly
///
/// Held item/slot from [`SessionMenus`] + [`SelectedSlot`], the same
/// container-screen-aware pattern [`drive_mining`] uses
/// (`Menus::player_native`, never `player().player_native(..)`); the sneak
/// bit from [`MovementIntent`], the same wire the server judges against;
/// reach from [`resolve_place_intent`], [`resolve_break_intent`]'s cast
/// generalised; the decision and the block-prediction sequence from
/// [`PlacementPredictor`], whose [`Placement::use_on`] threads the counter
/// internally — no sequence anywhere on [`PlaceIntent`] itself; the wire via
/// [`ActionQueue`] — never [`ClientHandle::send_action`] from a system, per
/// this module's own docs; the predicted write via [`ChunkWorldWrite::write`] +
/// [`write_predicted_block`], state and block entity together; the re-mesh
/// via [`TerrainMesh::remesh_around`] through the read handle.
///
/// # Human input wins, exactly as `BreakIntent`'s own docs describe
///
/// While [`UsingItem`] is true, the intent is not consulted at all this tick
/// — the human's own right-click already has a dedicated seam
/// (`Sim::use_item_live`) that this must not shadow, and [`PlaceOutcome`] is
/// left completely untouched rather than reset to `Idle` — see that type's
/// own docs for why an idle tick overwriting a real, unread result would make
/// `generation` meaningless.
///
/// # What this does not animate
///
/// Unlike a mouse-driven placement, this never calls `Sim::swing_hand` — that
/// mutates a private `Sim` field (`body_pose`) a system cannot reach, the
/// same reason [`drive_mining`] never calls it for a plugin-driven dig
/// either. The `SwingArm` wire action still goes out, so the server and every
/// other client see the swing; only this client's own first-person arm stays
/// still. A pre-existing, accepted gap this mirrors rather than introduces.
#[allow(clippy::too_many_arguments)]
pub fn drive_placement(
    egress: Res<Egress>,
    using_item: Res<UsingItem>,
    net: Res<NetHandle>,
    version: Res<VersionData>,
    chunk_world: Res<ChunkWorld>,
    // The write side of the split. The read handle above is for the
    // re-mesh; only this one may touch the store — a system that took only
    // `Res<ChunkWorld>` compiles nowhere in `drive_placement`'s role.
    write: Res<ChunkWorldWrite>,
    clock: Res<FrameClock>,
    profile: Res<Profile>,
    mut placement: ResMut<PlacementPredictor>,
    mut terrain: ResMut<TerrainMesh>,
    mut audio: ResMut<AudioEngine>,
    mut queue: ResMut<ActionQueue>,
    // That fix's veto registry -- see `drive_mining`'s own parameter.
    vetoes: Option<Res<ActionVetoes>>,
    mut commands: Commands,
    mut players: Query<
        (
            Entity,
            &PhysicsState,
            &MovementIntent,
            &SelectedSlot,
            Option<&Dead>,
            Option<&SessionMenus>,
            Option<&PlaceIntent>,
            &mut PlaceOutcome,
        ),
        With<LocalPlayer>,
    >,
) {
    if !(egress.in_world && egress.live) {
        return;
    }
    let Ok((entity, state, movement, slot, dead, menus, intent, mut outcome)) =
        players.single_mut()
    else {
        return;
    };
    // Human input wins — see this function's own docs on why the outcome is
    // left untouched rather than reset to `Idle`.
    if using_item.0 {
        return;
    }
    let Some(intent) = intent.copied() else {
        return;
    };
    // From here on this tick *is* an attempt: exactly one generation bump no
    // matter how it resolves, and the intent is removed regardless — one
    // insertion is one attempt, and the removal is the shell's own
    // acknowledgement (see `PlaceIntent`'s docs).
    commands.entity(entity).remove::<PlaceIntent>();
    outcome.generation += 1;

    if dead.is_some() {
        outcome.status = PlaceStatus::Rejected(PlaceRejection::Dead);
        return;
    }

    let eye = Vec3d::new(
        state.0.position.x,
        state.0.position.y + f64::from(state.0.eye_height),
        state.0.position.z,
    );
    let hit = match resolve_place_intent(intent, eye, &net, &version) {
        Ok(hit) => hit,
        Err(reason) => {
            outcome.status = PlaceStatus::Rejected(reason);
            return;
        }
    };
    let clicked = BlockPos::new(hit.block[0], hit.block[1], hit.block[2]);
    let face = face_from_normal(hit.normal);

    // Same "abort, never guess" contract `drive_mining` applies to
    // `NetHandle::block_at` returning `None` — no live chunk data at the
    // clicked cell.
    if net.block_at(clicked).is_none() {
        outcome.status = PlaceStatus::Rejected(PlaceRejection::NoWorldData);
        return;
    }

    // `Menus::player_native`, never `player().player_native(..)`: the one
    // inventory is owned by the *open container's* menu while a screen is up
    // — the exact fix `drive_mining`'s own docs describe for that fix,
    // generalised to placement.
    let main = menus
        .and_then(|menus| menus.0.player_native(slot.0))
        .filter(|stack| !stack.is_empty())
        .map(|stack| stack.item().clone());
    let placeable = main.as_ref().and_then(|item| {
        let name = item.to_string();
        let states = block_states_of(&name)?;
        let orientation = orientation_for_placement(&name, &states)?;
        Some((name, states, orientation))
    });
    let Some((name, states, orientation)) = placeable else {
        // Deliberately refused rather than sent-and-waited, unlike a human
        // click with a non-placing item in hand — see this function's own
        // docs on why a `PlaceIntent` narrows to "place", not "interact".
        outcome.status = PlaceStatus::Rejected(PlaceRejection::NothingPlaceableHeld);
        return;
    };

    let bb = state.0.bounding_box(&profile.0);
    let facts = placement_facts(
        clicked,
        face,
        |pos| net.block_at(pos),
        |pos| block_intersects_player(&bb, [pos.x, pos.y, pos.z]),
    );
    if facts.target_obstructed {
        outcome.status = PlaceStatus::Rejected(PlaceRejection::IntersectsPlayer);
        return;
    }

    let has_item_in_hand = main.is_some()
        || menus
            .and_then(|menus| menus.0.player_native(OFFHAND_NATIVE_INDEX))
            .is_some_and(|stack| !stack.is_empty());
    let ctx = UseOnContext {
        hand: Hand::Main,
        clicked,
        face,
        cursor: hit_cursor(hit),
        inside_block: false,
        rotation: Rotation::new(state.0.yaw, state.0.pitch),
        sneaking: movement.0.sneak,
        has_item_in_hand,
        placing: main.clone(),
        orientation,
    };
    // Read the world facts before taking the predictor's own resource guard,
    // same reason `PlacementFacts` gives — but here there is only ever one
    // guard (`placement`, already held as a system parameter), so the two
    // reads are already disjoint by construction rather than by ordering.
    // That fix's block-place veto, asked *before* `use_on` -- which threads the
    // block-prediction `sequence` counter and so cannot be called speculatively
    // and then discarded (`docs/baritone-port.md` §3.6 forbids forking that
    // counter outright). Denying here leaves the counter untouched.
    if let Some(vetoes) = &vetoes
        && vetoes.allows(&VerbContext::BlockPlace { pos: BlockPos::new(hit.block[0], hit.block[1], hit.block[2]) })
            == Verdict::Deny
    {
        outcome.status = PlaceStatus::Rejected(PlaceRejection::Vetoed);
        return;
    }

    let decision = placement.0.use_on(&ctx, &facts);
    let (UseOnDecision::Interact { action }
    | UseOnDecision::Place { action, .. }
    | UseOnDecision::Nothing { action }) = &decision;
    queue.0.push(action.clone());
    queue.0.push(ClientAction::SwingArm { hand: Hand::Main });

    let UseOnDecision::Place { prediction, .. } = &decision else {
        // `Interact` (clicked cell actuates instead of placing) or `Nothing`
        // (should not normally reach here, since every legality question
        // `use_on` re-asks was already checked above — but the packet is
        // honestly "sent, nothing changed locally" either way).
        outcome.status = PlaceStatus::SentUnpredicted;
        return;
    };
    let Some(state_id) = state_for_placement(&name, &states, orientation, &prediction.state) else {
        // The census legitimately declines many placeable blocks — see
        // `PlaceStatus::SentUnpredicted`'s own docs. The packet already went
        // out above; there is nothing more to do.
        outcome.status = PlaceStatus::SentUnpredicted;
        return;
    };
    let pos = prediction.pos;
    let block = [pos.x, pos.y, pos.z];
    // The write, then the re-mesh that makes it visible — Item 2's whole
    // point. Chunk guard taken and dropped before `remesh_around` reaches for
    // the `TerrainMesh` resource, same rule `Sim::predict_block` follows.
    {
        let mut world = write.write();
        write_predicted_block(&mut *world, block, state_id);
    }
    terrain.remesh_around(&chunk_world, block);
    // The placement sound, predicted locally for the same reason
    // `Sim::use_item_live`'s does — vanilla's own `BlockItem.place` excludes
    // the placing player from the server's broadcast, so our copy has to come
    // from here or not at all. Tied to `state_id` (the predicted state), not
    // the held item, because the sound is the *placed* state's `SoundType`.
    if let Some(sound) = lodestone_data::sound_types::sound_type(state_id)
        && let Some(sound_name) = lodestone_data::sound_types::place_sound_name(state_id)
        && let Some(engine) = &mut audio.0
    {
        engine.play_sound(
            sound_name,
            lodestone_model::event::SoundCategory::Block,
            glam::Vec3::new(
                block[0] as f32 + 0.5,
                block[1] as f32 + 0.5,
                block[2] as f32 + 0.5,
            ),
            sound.break_or_place_volume(),
            sound.break_or_place_pitch(),
            block_sound_seed(block, clock.ticks),
        );
    }
    outcome.status = PlaceStatus::Predicted;
}

/// Registers the live-interaction half of the `GameTick`: [`send_sprint_command`]
/// and [`drive_mining`], both in [`TickSet::Send`].
///
/// # Ordering
///
/// Explicitly `.after(lodestone_controller::ecs::send_player_input)` rather than
/// merely "added later". `add_systems` gives no ordering guarantee from
/// registration order, and the wire order here is load-bearing: the server's
/// sneak state comes from the player-input packet, so a `use_item_on` or a mining
/// `START` that overtook it would be judged against the previous tick's crouch.
///
/// Deliberately **does not** insert `ControllerPlugin` for itself, even though it
/// orders against one of its systems. `add_systems` does not deduplicate — Stage 3
/// shipped a total ingest blackout because two copies of one system ran in
/// sequence and the second cleared what the first filled — so a plugin that
/// unconditionally added another plugin's systems would be the same hazard. The
/// caller composes both; [`InteractPlugin::build`] panics loudly if it was added
/// without one.
#[derive(Debug, Default)]
pub struct InteractPlugin;

impl Plugin for InteractPlugin {
    fn build(&self, app: &mut App) {
        assert!(
            app.is_plugin_added::<lodestone_controller::ControllerPlugin>(),
            "InteractPlugin orders against ControllerPlugin's send_player_input; add \
             ControllerPlugin first rather than letting this plugin add it (add_systems does \
             not deduplicate — see docs/session-components.md)"
        );
        app.init_resource::<RayTarget>();
        app.init_resource::<EntityRayTarget>();
        app.init_resource::<Attacking>();
        app.init_resource::<UsingItem>();
        app.init_resource::<MiningPredictor>();
        app.init_resource::<PlacementPredictor>();
        app.init_resource::<NetHandle>();
        app.init_resource::<VersionData>();
        app.add_systems(
            GameTick,
            // `drive_placement` was defined but registered in **no** schedule
            // until that fix's island sweep found it: its only `add_systems` was a
            // hand-built `Schedule` in `tests/place_intent.rs`. A plugin's
            // `PlaceIntent` therefore sat unconsumed forever while `BreakIntent`
            // worked. Player impact was nil — human placement goes through
            // `Sim::use_item_live` — so nothing looked wrong, and this module's
            // own doc agreed with the code by listing the wrong count.
            //
            // It must stay **inside** the `.chain()`: it shares
            // `ResMut<ActionQueue>` with `drive_mining`, and this app runs with
            // `ambiguity_detection: LogLevel::Error`.
            (
                send_abilities,
                send_sprint_command,
                // Before `drive_mining`/`drive_placement` deliberately: both
                // resolve the held item from `SelectedSlot`, and a plugin that
                // changes the slot and edits in one tick must edit with the
                // *new* selection — and echo the change before the edit reaches
                // the wire, so the server judges it against the same slot.
                drive_select_slot,
                drive_mining,
                drive_placement,
                // The eating/drinking crumbs. **Inside the `.chain()` for the same
                // reason `drive_placement` is**: it shares `ResMut<ParticleSim>`
                // with `drive_mining` and this app runs with
                // `ambiguity_detection: LogLevel::Error`. Its position relative to
                // the others is otherwise arbitrary — it reads the use clock, which
                // no system in this chain writes.
                crate::consume::emit_consume_particles,
            )
                .chain()
                .after(lodestone_controller::ecs::send_player_input)
                .in_set(TickSet::Send),
        );
    }
}
