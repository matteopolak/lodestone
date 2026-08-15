//! End-to-end: a **real** `lodestone-client`, running the real
//! [`V770Adapter`](lodestone_v770::adapter), attacks a live mob over the real
//! [`V770ServerProtocol`] over the in-memory transport — this crate's own
//! acceptance shape: "a real client against our integrated server," the same
//! choice `block_entities_live.rs` makes and for the identical reason —
//! [`serve_connection`] is driven directly (not through
//! [`IntegratedServer`](lodestone_server::IntegratedServer)) so this test can
//! hold its own clone of the [`MobHandle`] and read the server's own mob
//! state after the connection closes, which `IntegratedServer`'s public
//! constructors build and own internally with no accessor.
//!
//! What this proves, in one run: the whole chain from a real client's
//! left-click (`ClientAction::InteractEntity { interaction:
//! EntityInteraction::Attack, .. }`, exactly what `Sim::attack_entity`
//! already sends in production — see `docs/combat.md`'s "Sending the
//! attack") through the `Attack`/`minecraft:attack` wire packet, this
//! crate's new `server_protocol.rs` decode, `crate::server::apply_attack`,
//! and `MobSim::attack`'s damage+knockback pipeline, back out through the
//! *existing* entity-snapshot stream to the real client's own read model
//! (`ClientHandle::entity`) — with **zero** of that last hop built new for
//! this issue.

use std::time::Duration;

use lodestone_client::{ClientBuilder, LoginProfile, ServerAddress};
use lodestone_model::{ClientAction, EntityInteraction, PlayerInput, ResourceKey, Rotation, Vec3};
use lodestone_net::{Connection, memory_pair};
use lodestone_server::{ChunkColumn, ChunkSource, MobHandle, serve_connection};
use lodestone_v770::{V770ServerProtocol, adapter};

/// A trivial all-air column — this test is about combat, not terrain; no
/// block state is ever read.
struct AirSource;

impl ChunkSource for AirSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(0, 16)
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        // The column-regenerating form (correct, just not cheap); this
        // fixture is small and this path is not hot.
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).block_state(lx, y, lz).to_string()
    }

    // No storage: this fixture serves fresh columns and edits are discarded by
    // design (an edit a test needs to survive goes through a source with real
    // retention). `ChunkSource::set_block` has no default, so this is
    // stated explicitly rather than inherited.
    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
        // No storage; edits are discarded by design.
    }
}

fn profile(name: &str) -> LoginProfile {
    LoginProfile {
        username: name.into(),
        uuid: uuid::Uuid::new_v4(),
    }
}

fn address() -> ServerAddress {
    ServerAddress {
        host: "memory".into(),
        port: 0,
    }
}

/// A player standing at `(0, 64, 0)`, sprinting, attacks a zombie standing 1
/// block away at `(1, 64, 0)` with a bare hand. Asserts, against the
/// **server's own** [`MobHandle`] (not the client's read model, which is
/// covered separately below) two exact, hand-predicted numbers:
///
/// * **Damage**: `Player.createAttributes()`'s bare-hand `ATTACK_DAMAGE =
///   1.0` (`.cache/mc/26.2/src/net/minecraft/world/entity/player/
///   Player.java`) against a real zombie's `ARMOR = 2.0`, no toughness
///   override (`Zombie.java`, `Monster.createMonsterAttributes()`'s
///   base has no `ARMOR_TOUGHNESS`), through
///   `CombatRules.getDamageAfterAbsorb`: `toughness = 2 + 0/4 = 2`,
///   `realArmor = clamp(2 - 1.0/2, 2*0.2, 20) = 1.5`, `frac = 1.5/25 =
///   0.06`, `damage = 1.0 * (1 - 0.06) = 0.94`.
/// * **Knockback**: this is **two independent, chained impulses**, not one —
///   vanilla's `LivingEntity.hurtServer` unconditionally calls
///   `dealDefaultKnockback` (flat `0.4`, gated on nothing but "damage was not
///   `NO_KNOCKBACK`-tagged" — **not** on sprinting) and, separately,
///   `Player.attack` calls `causeExtraKnockback` with `getKnockback(...) +
///   (sprintAttack ? 0.5F : 0.0F)` (`0.5` here: bare hand, no enchant,
///   sprinting). Direction for both, per `dealDefaultKnockback`'s own
///   `source.getSourcePosition().x() - this.getX()` (source = attacker,
///   `this` = target): `dx = attacker_pos.x - target_pos.x = 0 - 1 = -1`,
///   `dz = 0`. `knockback_impulse`'s formula (`LivingEntity.knockback`:
///   `deltaMovement.x/2 - deltaVector.x`) chained twice, hand-derived here
///   and cross-checked against `lodestone-server/tests/mob_attack.rs`'s
///   `positive_knockback_power_produces_the_exact_predicted_velocity`
///   (identical inputs, same two-stage derivation):
///   - Stage 1 (`0.4`, mandatory): `dir = normalize(-1, 0) = (-1, 0)`,
///     `deltaVector = dir * 0.4 = (-0.4, 0)`. `x' = 0/2 - (-0.4) = 0.4`,
///     `y' = min(0/2 + 0.4, 0.4) = 0.4` (grounded cap — a mob's
///     `NavigatingMob` has no ground-contact state and always takes this
///     branch, per `MobSim::attack`'s own doc comment), `z' = 0`. `v1 =
///     (0.4, 0.4, 0.0)`.
///   - Stage 2 (`0.5` sprint bonus, chained onto `v1`): `deltaVector = dir *
///     0.5 = (-0.5, 0)`. `x' = 0.4/2 - (-0.5) = 0.7`, `y' = min(0.4/2 + 0.5,
///     0.4) = 0.4` (capped again), `z' = 0`. `v2 = (0.7, 0.4, 0.0)`.
///
///   `NavigatingMob::apply_knockback` applies this as a direct one-shot
///   position displacement (no drag/decay — see that method's own doc
///   comment), so the target's expected post-hit position is
///   `(1, 64, 0) + (0.7, 0.4, 0.0) = (1.7, 64.4, 0.0)`: pushed *away* from
///   the attacker (positive x, growing past the target's starting `1.0`),
///   not toward it.
///
///   A previous version of this doc comment predicted `(0.5, 64.4, 0.0)` —
///   the target moving *toward* the attacker — from `dx = 1` (attacker→target,
///   i.e. the *wrong* sign for `knockback_impulse`'s convention: vanilla's own
///   `dealDefaultKnockback` uses target→attacker, `attacker.x - target.x`)
///   and only the sprint-bonus stage, silently dropping the mandatory `0.4`.
///   That was the same backwards convention `MobSim::attack`'s own doc
///   comment already documents as a fixed bug (`target_pos - attacker_pos`),
///   reproduced independently in this file's expected value rather than in
///   the implementation — verified against
///   `.cache/mc/26.2/src/net/minecraft/world/entity/LivingEntity.java`'s
///   `hurtServer`/`dealDefaultKnockback`/`knockback` bodies directly, not
///   against either side of the original disagreement.
#[tokio::test]
async fn real_client_attacks_a_live_mob_and_the_server_applies_damage_and_knockback() {
    let view_radius = 0;
    let (client_io, server_io) = memory_pair();

    let mob_handle = MobHandle::new(lodestone_server::ChunkWorld::new(-4, 24));
    let zombie_pos = Vec3::new(1.0, 64.0, 0.0);
    let mob_id = mob_handle.with(|sim| {
        // `MobSim::new`'s default id start (`1`) collides with
        // `V770ServerProtocol::LOCAL_PLAYER_ENTITY_ID` (also `1`) — a real
        // client never spawns "itself" as an `ADD_ENTITY`, so the very
        // first mob a fresh sim spawns would silently never appear (see
        // `MobSim::set_next_id`'s own doc comment; production's
        // `MobHandle::seeded` already does this). Matched here for the
        // identical reason.
        sim.set_next_id(1000);
        let zombie = ResourceKey::new("minecraft", "zombie").expect("valid key");
        sim.spawn_species(zombie, zombie_pos).id()
    });
    let health_before = mob_handle.with(|sim| sim.get(mob_id).expect("just spawned").health());

    let server_mobs = mob_handle.clone();
    let server_task = tokio::spawn(async move {
        let mut conn = Connection::new(server_io);
        // The `MobHandle` doubles as its own `EntitySource` (see that impl's
        // own doc comment) — no separate `LiveMobSource`/tick loop needed for
        // a test that drives one explicit attack rather than continuous AI.
        serve_connection(
            &mut conn,
            &V770ServerProtocol,
            &AirSource,
            &server_mobs,
            view_radius,
            &lodestone_server::BlockEntityHandle::default(),
            &server_mobs,
        )
        .await
    });

    let (mut handle, _events) = ClientBuilder::new(
        address(),
        profile("Puncher"),
        Box::new(adapter()),
    )
    .connect_with(client_io);

    handle
        .wait_for_spawn(Duration::from_secs(30))
        .await
        .expect("client never spawned");
    handle
        .wait_for_chunks(1, Duration::from_secs(30))
        .await
        .expect("initial column never arrived");

    // The mob was already spawned into `mob_handle` before the connection
    // opened, so the join-time entity sync should have delivered it
    // immediately — poll rather than assume, per this project's own "a
    // freshly summoned entity is not selector-visible until the next tick"
    // live-oracle lesson (the client-side analogue: give the wire a moment).
    let spawn_deadline = std::time::Instant::now() + Duration::from_secs(30);
    while handle.entity(mob_id).is_none() {
        assert!(
            std::time::Instant::now() < spawn_deadline,
            "client never observed the pre-spawned zombie within 30s"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let seen_before = handle.entity(mob_id).expect("just polled Some").position;
    assert!(
        (seen_before - zombie_pos).length() < 1e-6,
        "client's initial view of the mob must match the server's real spawn position, got {seen_before:?}"
    );

    // Establish the attacker's tracked position server-side (`player_pos`,
    // `ServerBound::PlayerMoved`) — required for a knockback direction at
    // all; see `apply_attack`'s own doc comment for the "no position yet,
    // don't guess" fallback this sidesteps by sending one.
    handle
        .set_position(Vec3::new(0.0, 64.0, 0.0))
        .expect("client still connected");

    // Sprinting, so this attack's *extra* knockback bonus is the real jar
    // constant `0.5`, not `0.0` — see `SPRINT_ATTACK_KNOCKBACK_POWER`'s own
    // doc comment. This is a magnitude difference, not an on/off one: even a
    // non-sprinting bare-handed hit still gets vanilla's mandatory flat `0.4`
    // default knockback (`LivingEntity.dealDefaultKnockback`, gated on
    // nothing but the damage source not being `NO_KNOCKBACK`-tagged) — see
    // `crates/lodestone-server/tests/mob_attack.rs`'s
    // `a_non_sprinting_hit_still_applies_the_default_knockback`. Sprinting
    // here exercises the second, chained stage so this gate's expected
    // position depends on both terms, not just one.
    handle
        .send_action(ClientAction::SetPlayerInput(PlayerInput {
            forward: false,
            backward: false,
            left: false,
            right: false,
            jump: false,
            shift: false,
            sprint: true,
        }))
        .expect("client still connected");

    handle
        .send_action(ClientAction::InteractEntity {
            entity_id: mob_id,
            interaction: EntityInteraction::Attack,
            sneaking: false,
        })
        .expect("client still connected");

    // Poll the *server's own* `MobHandle` for the exact predicted health —
    // the strongest available observation (real server state), independent
    // of the client's own read model.
    let expected_health = health_before - 0.94;
    let hit_deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let health = mob_handle.with(|sim| sim.get(mob_id).map(|m| m.health()));
        if let Some(h) = health
            && (h - expected_health).abs() < 1e-3
        {
            break;
        }
        assert!(
            std::time::Instant::now() < hit_deadline,
            "server-side health never reached the predicted {expected_health}, last saw {health:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // The **client's own** read model must independently converge on the
    // exact predicted post-knockback position — the proof this reaches a
    // real client over the real wire, not just the server's internal state.
    let expected_pos = Vec3::new(1.7, 64.4, 0.0);
    let move_deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut last_seen = seen_before;
    loop {
        if let Some(view) = handle.entity(mob_id) {
            last_seen = view.position;
            if (last_seen - expected_pos).length() < 1e-3 {
                break;
            }
        }
        assert!(
            std::time::Instant::now() < move_deadline,
            "client never observed the predicted knockback position {expected_pos:?}, last saw {last_seen:?}"
        );
        // Nudge more traffic through so the entity-streaming pass (driven by
        // every inbound packet) has something to react to.
        let _ = handle.chat("poke");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    handle.shutdown();
    let _ = handle.join().await;
    let _ = tokio::time::timeout(Duration::from_secs(10), server_task)
        .await
        .expect("serve_connection task did not finish in time");
}

/// **Control**: a rotation-only nudge (no `InteractEntity`) must never move
/// the mob — proves the position change above is caused by the attack, not
/// by ordinary connection traffic/entity-streaming churn.
#[tokio::test]
async fn no_attack_means_no_movement() {
    let view_radius = 0;
    let (client_io, server_io) = memory_pair();

    let mob_handle = MobHandle::new(lodestone_server::ChunkWorld::new(-4, 24));
    let zombie_pos = Vec3::new(1.0, 64.0, 0.0);
    let mob_id = mob_handle.with(|sim| {
        // `MobSim::new`'s default id start (`1`) collides with
        // `V770ServerProtocol::LOCAL_PLAYER_ENTITY_ID` (also `1`) — a real
        // client never spawns "itself" as an `ADD_ENTITY`, so the very
        // first mob a fresh sim spawns would silently never appear (see
        // `MobSim::set_next_id`'s own doc comment; production's
        // `MobHandle::seeded` already does this). Matched here for the
        // identical reason.
        sim.set_next_id(1000);
        let zombie = ResourceKey::new("minecraft", "zombie").expect("valid key");
        sim.spawn_species(zombie, zombie_pos).id()
    });

    let server_mobs = mob_handle.clone();
    let server_task = tokio::spawn(async move {
        let mut conn = Connection::new(server_io);
        serve_connection(
            &mut conn,
            &V770ServerProtocol,
            &AirSource,
            &server_mobs,
            view_radius,
            &lodestone_server::BlockEntityHandle::default(),
            &server_mobs,
        )
        .await
    });

    let (mut handle, _events) = ClientBuilder::new(
        address(),
        profile("Bystander"),
        Box::new(adapter()),
    )
    .connect_with(client_io);

    handle
        .wait_for_spawn(Duration::from_secs(30))
        .await
        .expect("client never spawned");
    handle
        .wait_for_chunks(1, Duration::from_secs(30))
        .await
        .expect("initial column never arrived");

    let spawn_deadline = std::time::Instant::now() + Duration::from_secs(30);
    while handle.entity(mob_id).is_none() {
        assert!(std::time::Instant::now() < spawn_deadline, "mob never appeared");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    handle
        .set_position(Vec3::new(0.0, 64.0, 0.0))
        .expect("client still connected");
    handle
        .move_to(Vec3::new(0.0, 64.0, 0.0), Rotation { yaw: 90.0, pitch: 0.0 }, true, false)
        .expect("client still connected");
    for _ in 0..5 {
        let _ = handle.chat("poke");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let seen = handle.entity(mob_id).expect("still present").position;
    assert!(
        (seen - zombie_pos).length() < 1e-6,
        "mob must not move without an attack, got {seen:?}"
    );
    let health = mob_handle.with(|sim| sim.get(mob_id).expect("still present").health());
    let max_health = mob_handle.with(|sim| {
        // Re-spawn a control mob fresh to read the untouched default —
        // cheaper than threading the pre-attack health through.
        let zombie = ResourceKey::new("minecraft", "zombie").expect("valid key");
        sim.spawn_species(zombie, Vec3::new(50.0, 64.0, 0.0)).health()
    });
    assert_eq!(health, max_health, "mob must not have taken damage without an attack");

    handle.shutdown();
    let _ = handle.join().await;
    let _ = tokio::time::timeout(Duration::from_secs(10), server_task).await;
}

/// The two tests above put the zombie directly on the attacker's `+x` axis
/// (`dz = 0`), which cannot tell a true `normalize(dx, dz).scale(power)`
/// direction (`knockback_impulse`'s real formula) apart from an incorrect
/// per-axis `(dx * power, dz * power)` scale — with `dz = 0` both give the
/// same unit `x` direction. This is the off-axis arm: the attacker at
/// `(0, 64, 0)` and the zombie at `(3, 64, 4)`, both offsets non-zero and
/// mutually distinct so an x/z transposition would also be visible.
///
/// `dx = attacker.x - target.x = 0 - 3 = -3`, `dz = attacker.z - target.z =
/// 0 - 4 = -4`, magnitude `5`, `dir = normalize(-3, -4) = (-0.6, -0.8)`.
/// Chained exactly as the on-axis case:
///
/// * Stage 1 (`0.4`): `deltaVector = dir * 0.4 = (-0.24, -0.32)`. `x' = 0/2 -
///   (-0.24) = 0.24`, `z' = 0/2 - (-0.32) = 0.32`, `y' = min(0/2 + 0.4, 0.4)
///   = 0.4`. `v1 = (0.24, 0.4, 0.32)`.
/// * Stage 2 (`0.5` sprint bonus): `deltaVector = dir * 0.5 = (-0.3, -0.4)`.
///   `x' = 0.24/2 - (-0.3) = 0.42`, `z' = 0.32/2 - (-0.4) = 0.56`, `y' =
///   min(0.4/2 + 0.5, 0.4) = 0.4`. `v2 = (0.42, 0.4, 0.56)`.
///
/// `apply_knockback` adds this directly to position (one-shot, no drag):
/// `(3, 64, 4) + (0.42, 0.4, 0.56) = (3.42, 64.4, 4.56)`. A per-axis-scale
/// implementation would instead produce `deltaVector = (dx * power, dz *
/// power)` unnormalized — wildly different numbers (stage 1 alone would be
/// `(-1.2, -1.6)` instead of `(-0.24, -0.32)`) — so this input fails loudly
/// under that wrong hypothesis rather than coincidentally agreeing with it.
#[tokio::test]
async fn real_client_attacks_a_live_mob_off_axis_and_knockback_is_normalized_not_per_axis() {
    let view_radius = 0;
    let (client_io, server_io) = memory_pair();

    let mob_handle = MobHandle::new(lodestone_server::ChunkWorld::new(-4, 24));
    let zombie_pos = Vec3::new(3.0, 64.0, 4.0);
    let mob_id = mob_handle.with(|sim| {
        // See the on-axis test above for why `set_next_id(1000)` is needed
        // (avoids colliding with `V770ServerProtocol::LOCAL_PLAYER_ENTITY_ID`).
        sim.set_next_id(1000);
        let zombie = ResourceKey::new("minecraft", "zombie").expect("valid key");
        sim.spawn_species(zombie, zombie_pos).id()
    });
    let health_before = mob_handle.with(|sim| sim.get(mob_id).expect("just spawned").health());

    let server_mobs = mob_handle.clone();
    let server_task = tokio::spawn(async move {
        let mut conn = Connection::new(server_io);
        serve_connection(
            &mut conn,
            &V770ServerProtocol,
            &AirSource,
            &server_mobs,
            view_radius,
            &lodestone_server::BlockEntityHandle::default(),
            &server_mobs,
        )
        .await
    });

    let (mut handle, _events) = ClientBuilder::new(
        address(),
        profile("OffAxisPuncher"),
        Box::new(adapter()),
    )
    .connect_with(client_io);

    handle
        .wait_for_spawn(Duration::from_secs(30))
        .await
        .expect("client never spawned");
    handle
        .wait_for_chunks(1, Duration::from_secs(30))
        .await
        .expect("initial column never arrived");

    let spawn_deadline = std::time::Instant::now() + Duration::from_secs(30);
    while handle.entity(mob_id).is_none() {
        assert!(
            std::time::Instant::now() < spawn_deadline,
            "client never observed the pre-spawned zombie within 30s"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let seen_before = handle.entity(mob_id).expect("just polled Some").position;
    assert!(
        (seen_before - zombie_pos).length() < 1e-6,
        "client's initial view of the mob must match the server's real spawn position, got {seen_before:?}"
    );

    handle
        .set_position(Vec3::new(0.0, 64.0, 0.0))
        .expect("client still connected");
    handle
        .send_action(ClientAction::SetPlayerInput(PlayerInput {
            forward: false,
            backward: false,
            left: false,
            right: false,
            jump: false,
            shift: false,
            sprint: true,
        }))
        .expect("client still connected");
    handle
        .send_action(ClientAction::InteractEntity {
            entity_id: mob_id,
            interaction: EntityInteraction::Attack,
            sneaking: false,
        })
        .expect("client still connected");

    let expected_health = health_before - 0.94;
    let hit_deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let health = mob_handle.with(|sim| sim.get(mob_id).map(|m| m.health()));
        if let Some(h) = health
            && (h - expected_health).abs() < 1e-3
        {
            break;
        }
        assert!(
            std::time::Instant::now() < hit_deadline,
            "server-side health never reached the predicted {expected_health}, last saw {health:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let expected_pos = Vec3::new(3.42, 64.4, 4.56);
    let move_deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut last_seen = seen_before;
    loop {
        if let Some(view) = handle.entity(mob_id) {
            last_seen = view.position;
            if (last_seen - expected_pos).length() < 1e-3 {
                break;
            }
        }
        assert!(
            std::time::Instant::now() < move_deadline,
            "client never observed the predicted off-axis knockback position {expected_pos:?}, last saw {last_seen:?}"
        );
        let _ = handle.chat("poke");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    handle.shutdown();
    let _ = handle.join().await;
    let _ = tokio::time::timeout(Duration::from_secs(10), server_task)
        .await
        .expect("serve_connection task did not finish in time");
}
