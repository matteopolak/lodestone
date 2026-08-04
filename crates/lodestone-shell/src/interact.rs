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
//! `open_menu`, `scoreboard`, `player_rows`, `boss_bars`, `health`, `player` and
//! the rest read `SharedState.ecs`. The lesson is the one §4.1(c) itself
//! implies — **there is one `World`, so a system should read the component, not
//! call the client** — and [`NetHandle::get`] is private now so the shape cannot
//! come back. `tests/mining_deadlock.rs` is the gate, with a control that
//! observes `player_menu` wedging under the guard.
//!
//! # How it works
//!
//! [`InteractPlugin`] registers two systems in `TickSet::Send`, ordered after
//! `lodestone_controller::ecs::send_player_input` by virtue of being added later
//! into the same set via an explicit `.after()`:
//!
//! 1. [`send_sprint_command`] — vanilla's `LocalPlayer.sendIsSprintingIfNeeded`.
//! 2. [`drive_mining`] — one tick of the hold-to-mine predictor.
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
//! emitter, and `crate::net::SharedHandle` for every read of the client-owned
//! world.

use lodestone_ecs::app::{App, Plugin};
use lodestone_ecs::ecs::prelude::{Query, Res, ResMut, With};
use lodestone_ecs::ecs::resource::Resource;
use lodestone_ecs::ecs::schedule::IntoScheduleConfigs;
use lodestone_client::{BlockPos, ClientAction, ClientHandle};
use lodestone_ecs::player::{
    ActionQueue, BreakIntent, BreakOutcome, BreakRejection, BreakStatus, Dead, Egress, Flying,
    LastFlyingSent, LastSprintingSent, LocalPlayer, PhysicsState, SelectedSlot, Submersion,
};
use lodestone_ecs::session::{Abilities, ServerEntityId, SessionMenus};
use lodestone_ecs::{GameTick, TickSet, VersionData};
use lodestone_game::mining::Mining;
use lodestone_game::placement::Placement;
use lodestone_model::{BlockFace, PlayerCommand};
use lodestone_physics::Vec3d;

use crate::blocks::id;
use crate::net::SharedHandle;
use crate::particles::Particles;
use crate::raycast::{PickBox, RayHit, raycast};
use crate::sim::{bare_handed_tool_mining, dig_break_inputs, face_from_normal, particle_face};

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
/// is `3.0` blocks (`Player.java:134`) versus `DEFAULT_BLOCK_INTERACTION_RANGE`'s
/// `4.5` (`Player.java:133`), and further capped by the block hit distance when
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

/// Whether the attack (left) button is currently held.
///
/// Drives the live hold-to-mine loop. A demo-world break is a one-shot on press
/// instead, so this stays `false` off a live session and [`drive_mining`] is a
/// cheap no-op there.
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct Attacking(pub bool);

/// Whether the use (right) button has been pressed and not yet released.
///
/// The client-side mirror of vanilla's `Minecraft.java:1914`,
/// `this.player.isUsingItem()`, which gates whether releasing `key.use` sends
/// `RELEASE_USE_ITEM` (`:1916`, `gameMode.releaseUsingItem`). Vanilla's own
/// flag comes from a held item's `use()` running identically client- and
/// server-side (a bow's `use()` calls `LivingEntity.startUsingItem` on both),
/// which this client has no local simulation of — there is no item registry
/// here that can say "yes, that bow just started a held use." So this is an
/// **input-state** mirror instead: true from [`crate::sim::Sim::use_item`]'s
/// live press until its release counterpart's release, a superset of
/// vanilla's real gate rather than an exact match.
///
/// That gap is inert, not a wrong state transition: `LivingEntity
/// .releaseUsingItem` (`.cache/mc/26.2/src/…/LivingEntity.java:3602-3613`)
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
    /// (`player_menu`, `open_menu`, `scoreboard`, `player_rows`, `boss_bars`,
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

/// `LocalPlayer.sendIsSprintingIfNeeded` (`LocalPlayer.java:303-312`): put the
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

/// Resolve a plugin's [`BreakIntent`] into the same [`RayHit`] shape a mouse
/// click's [`RayTarget`] would produce, or reject it.
///
/// A plugin has no crosshair, so there is no ray to read — this casts one of
/// its own, from the eye toward the centre of the face the intent names,
/// using the version's own [`VersionData::block_outline`] census for the
/// per-cell geometry the same way [`crate::raycast::raycast`]'s only other
/// caller (the mouse-driven `Sim::update_target`) does. Accepting the
/// resolved hit **only when it lands on the intended cell** is what makes
/// this a real reach-and-line-of-sight check rather than a rubber stamp: a
/// closer block in the way, or a target beyond vanilla's 4.5-block
/// [`crate::raycast::REACH`], both resolve to a different cell (or no hit at
/// all) and are rejected identically, matching
/// [`BreakRejection::UnreachableOrObstructed`]'s own doc on why the two share
/// one variant.
///
/// Cells with no live block data (`NetHandle::block_at` returning `None`) are
/// treated as untargetable rather than solid — the same "not painted, not an
/// obstruction" answer the mouse-driven cast gives for a chunk that has not
/// streamed in, and it is what makes a target beyond the loaded world resolve
/// as unreachable rather than panicking or inventing geometry.
fn resolve_break_intent(
    intent: BreakIntent,
    eye: Vec3d,
    net: &NetHandle,
    version: &VersionData,
) -> Result<RayHit, BreakRejection> {
    let block = [intent.pos.x, intent.pos.y, intent.pos.z];
    let normal = face_to_normal(intent.face);
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
        Some(resolved) if resolved.block == block => Ok(resolved),
        _ => Err(BreakRejection::UnreachableOrObstructed),
    }
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
    mut mining: ResMut<MiningPredictor>,
    mut particles: ResMut<ParticleSim>,
    mut queue: ResMut<ActionQueue>,
    mut players: Query<
        (
            &PhysicsState,
            &Submersion,
            &SelectedSlot,
            Option<&Dead>,
            Option<&SessionMenus>,
            Option<&BreakIntent>,
            &mut BreakOutcome,
        ),
        With<LocalPlayer>,
    >,
) {
    if !(egress.in_world && egress.live) {
        return;
    }
    let Ok((state, submersion, slot, dead, menus, intent, mut outcome)) = players.single_mut()
    else {
        return;
    };

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
    // (issue #373), so reading window 0's menu here returned a stale stack and
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
    );

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
    // Issue #360: the debris burst at the moment a block actually breaks.
    // Issue #387: keyed on **destruction**, not on the `StopDestroy` packet.
    //
    // This is the local **prediction** half of vanilla's
    // `MultiPlayerGameMode.destroyBlock` (`MultiPlayerGameMode.java:113-145`):
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
    // (issue #387, reported from play). `Mining::take_destroyed` is the funnel:
    // both `start`'s instant-break branch and `continue_`'s progress-reached-1.0
    // branch latch it, so keying on it removes the class instead of
    // special-casing one-shot blocks.
    //
    // Before this, the **only** burst trigger anywhere in the shell was the
    // server-driven `NetUpdate::BlockDestroyed` arm (`Sim::step`'s live-update
    // match, fed by `ClientboundLevelEventPacket` id `2001`) — which
    // structurally **never fires for our own break**, verified against
    // `.cache/mc/26.2/src` rather than assumed:
    // `ServerPlayerGameMode.destroyBlock` (`ServerPlayerGameMode.java:262-298`,
    // the server's handler for a player's own break) calls
    // `this.level.removeBlock(pos, false)` — a plain block-state write with no
    // `levelEvent` call anywhere in it. The `2001` particle event instead lives
    // in the *separate* `Level.destroyBlock(pos, drop, breaker, limit)` method
    // (`Level.java:280-289`, `this.levelEvent(2001, pos, ...)`), which is what a
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
    // # Known divergence: a one-shot break can burst more than once
    //
    // Vanilla's guard against re-breaking the same cell is the *local* block
    // edit `destroyBlock` performs (`level.setBlock(pos, .., 11)`), after which
    // its own `oldState.isAir()` early-return fires. This shell predicts no
    // block edit, so between the `StartDestroy` going out and the server's
    // `BLOCK_UPDATE` coming back, `net.block_at(pos)` still reports the plant
    // and a held button re-latches a destruction each tick. The progressive
    // path is shielded from this by `Mining`'s 5-tick `destroyDelay`; vanilla
    // deliberately sets no such delay on the survival instant-break branch,
    // relying on the local edit instead. Bounded by the round trip (one or two
    // ticks) and self-limiting — `Sim::update_target` stops reporting a hit once
    // the cell is air, which aborts the dig — so it reads as a slightly heavier
    // puff rather than a stream. Predicting the block edit is the real fix and
    // is the same missing piece the mispredicted-break gap above names.
    if mining.0.take_destroyed().is_some() {
        // Full-cube shape and untinted white, for the same reason as the
        // mining-chip particle a few lines up: the shell does not carry a
        // block's outline shape, and `destroy_block` itself resolves the
        // real per-state tint (see its own docs) — `[1.0; 3]` is the
        // multiplier, not a placeholder colour.
        //
        // `hit.block`/`id_value` rather than the latched position: they are the
        // same cell (`pos` is built from `hit.block` above and is what both
        // predictor entry points were handed), and taking both the position and
        // the state id from the one `net.block_at` lookup keeps them from ever
        // describing different blocks.
        particles.0.destroy_block(hit.block, id_value, [1.0; 3]);
    }
    queue.0.extend(actions);
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
            (send_abilities, send_sprint_command, drive_mining)
                .chain()
                .after(lodestone_controller::ecs::send_player_input)
                .in_set(TickSet::Send),
        );
    }
}

