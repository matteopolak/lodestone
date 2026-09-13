//! Acceptance gate for closing the "nothing constructs or
//! ticks `ProjectileRegistry`/`ItemEntityRegistry`" island.
//!
//! Both registries (`lodestone_entity::projectile::ProjectileRegistry`,
//! `lodestone_entity::item_entity::ItemEntityRegistry`) were fully implemented
//! and unit-tested inside `lodestone-entity` already — `grep -rn
//! 'ProjectileRegistry\|ItemEntityRegistry' crates | grep -v lodestone-entity`
//! returned nothing before this. A test that drives either registry's own
//! `tick()` directly would pass whether or not anything in production ever
//! called it — that is exactly the closed loop that let this sit unwired, per
//! the driver's own doc comment on `ProjectileRegistry`/`ItemEntityRegistry`.
//!
//! So every test here drives [`MobSim`] instead: the struct
//! `MobHandle::seeded` (`lodestone-server/src/mobs.rs`) constructs once per
//! singleplayer session and `tick::run_tick_loop` (previously
//! `run_mob_tick_loop`) ticks it every 50ms in production
//! (`IntegratedServer::open_in_memory_with_mobs`, the constructor
//! `lodestone-shell`'s `net.rs` uses to start singleplayer). `MobSim::tick`
//! now calls `self.projectiles.tick()` / `self.items.tick()` itself — these
//! tests call `MobSim::spawn_projectile`/`spawn_item` + `MobSim::tick_for`,
//! never `ProjectileRegistry::tick`/`ItemEntityRegistry::tick` directly, so a
//! regression that un-wires `MobSim::tick` from either registry fails here.
//!
//! Expected values are computed independently from the real 26.2 jar
//! (`.cache/mc/26.2/src`), quoted below, never derived from this crate's own
//! encoder — the exact-magnitude discipline CLAUDE.md's evidence standards
//! call for (a directional "it moved" assertion passes for any wrong gravity
//! constant).

use std::str::FromStr;

use lodestone_entity::item_entity::ItemLifecycle;
use lodestone_entity::projectile::Projectile;
use lodestone_model::{ResourceKey, Vec3};
use lodestone_server::{ChunkWorld, MobSim};

fn rk(s: &str) -> ResourceKey {
    ResourceKey::from_str(s).expect("valid resource key")
}

/// Vanilla's own `AbstractArrow.tick`: in air, **move -> drag
/// (`AbstractArrow.getAirDrag` = `0.99`) -> gravity**
/// (`AbstractArrow.getDefaultGravity` = `0.05`). `Projectile::arrow` already encodes this order and
/// these constants (that is `lodestone-entity`'s own, already-verified half);
/// this test's job is only to prove `MobSim` — the thing a real server tick
/// loop owns — actually advances it, by predicting the exact position 10
/// ticks out and comparing against a standalone integrator run the same
/// number of ticks, completely independent of `MobSim`.
///
/// The pre-tick control matters: without it, a `MobSim` that silently never
/// called `self.projectiles.tick()` would report the *unmoved* spawn position
/// and this test's final assertion would already fail loudly — but proving
/// that failure mode is exactly what "run every control and watch it fail"
/// asks for, so it is asserted explicitly rather than assumed.
#[test]
fn mobsim_tick_advances_a_registered_projectile_to_the_exact_predicted_position() {
    let world = ChunkWorld::new(0, 192);
    let mut sim = MobSim::new(&world);

    let start = Vec3::new(0.0, 64.0, 0.0);
    let velocity = Vec3::new(3.0, 0.0, 0.0);
    let id = sim.spawn_projectile(rk("minecraft:arrow"), Projectile::arrow(start, velocity));

    // Control: spawning must not itself move the projectile. If it did, the
    // "after 10 ticks" assertion below could pass by coincidence even with
    // `MobSim::tick` never touching the registry.
    let at_spawn = sim.projectile_position(id).expect("just spawned");
    assert_eq!(at_spawn, start, "spawn must not itself advance motion");

    sim.tick_for(10);

    let mut expected = Projectile::arrow(start, velocity);
    expected.tick_n(10);

    let got = sim
        .projectile_position(id)
        .expect("must still be tracked after 10 ticks (no impact logic exists to remove it)");
    assert!(
        (got.x - expected.position.x).abs() < 1e-9,
        "x: got {} expected {}",
        got.x,
        expected.position.x
    );
    assert!(
        (got.y - expected.position.y).abs() < 1e-9,
        "y: got {} expected {}",
        got.y,
        expected.position.y
    );
    // The arrow must actually have fallen and moved — a projectile frozen at
    // its spawn position would trivially satisfy an unguarded "no panic"
    // test, which is exactly the kind of vacuous gate CLAUDE.md warns about.
    assert!(got.y < start.y, "gravity must have pulled it down");
    assert!(got.x > start.x, "it must have moved forward");
}

/// `ThrowableProjectile.tick`:
/// **gravity (`0.03`) -> drag (`0.99` air) -> move**, the opposite order and
/// different constants from the arrow family above — the two must not
/// converge to the same trajectory from the same start.
#[test]
fn mobsim_tick_advances_a_registered_throwable_with_its_own_family_constants() {
    let world = ChunkWorld::new(0, 192);
    let mut sim = MobSim::new(&world);

    let start = Vec3::new(0.0, 64.0, 0.0);
    let velocity = Vec3::new(1.5, 0.0, 0.0);
    let id = sim.spawn_projectile(rk("minecraft:snowball"), Projectile::snowball(start, velocity));

    sim.tick_for(10);

    let mut expected = Projectile::snowball(start, velocity);
    expected.tick_n(10);

    let got = sim.projectile_position(id).expect("still tracked");
    assert!((got.x - expected.position.x).abs() < 1e-9);
    assert!((got.y - expected.position.y).abs() < 1e-9);

    // The two families must have diverged from identical `tick_for` counts —
    // proving `MobSim` really dispatches per-entry ballistic state (each
    // tracked projectile keeps its own `IntegrationOrder`/constants) rather
    // than, say, applying one hardcoded formula to everything it tracks.
    let mut arrow_from_same_start = Projectile::arrow(start, velocity);
    arrow_from_same_start.tick_n(10);
    assert!(
        (expected.position.y - arrow_from_same_start.position.y).abs() > 1e-6,
        "throwable and arrow families must diverge from an identical start"
    );
}

/// `remove_projectile` must actually stop the entry from advancing — the
/// "despawn/impact" half of the API `MobSim::tick` never calls on its own
/// (hit detection is explicit follow-up, not this issue's scope), but the
/// primitive itself must work.
#[test]
fn remove_projectile_stops_further_ticking_and_drops_wire_metadata() {
    let world = ChunkWorld::new(0, 192);
    let mut sim = MobSim::new(&world);
    let id = sim.spawn_projectile(
        rk("minecraft:arrow"),
        Projectile::arrow(Vec3::new(0.0, 64.0, 0.0), Vec3::new(1.0, 0.0, 0.0)),
    );
    assert_eq!(sim.projectile_count(), 1);

    let removed = sim.remove_projectile(id).expect("was tracked");
    assert_eq!(removed.id, id);
    assert_eq!(sim.projectile_count(), 0);
    assert!(sim.projectile_position(id).is_none());

    // Must not panic on an empty registry, and must not resurrect the entry.
    sim.tick_for(5);
    assert!(sim.projectile_position(id).is_none());
    assert!(
        sim.snapshots().iter().all(|s| s.id != id),
        "a removed projectile must not still be published to the wire"
    );
}

/// `ItemEntity.tick` (pickup delay counts down, stops at the
/// `32767` never-pickup sentinel it is not here; age counts
/// up unless the `-32768` infinite sentinel). `ItemLifecycle::newly_dropped`
/// starts at the vanilla `ItemEntity.setDefaultPickUpDelay` value of `10`,
/// so after exactly 10 ticks pickup delay must
/// read `0` and age must read `10` — an exact prediction, not "it changed".
#[test]
fn mobsim_tick_advances_a_registered_item_lifecycle_to_the_exact_predicted_counters() {
    let world = ChunkWorld::new(0, 192);
    let mut sim = MobSim::new(&world);

    let id = sim.spawn_item(
        rk("minecraft:stick"),
        Vec3::new(10.0, 64.0, 0.0),
        Vec3::new(0.0, 0.0, 0.0),
        ItemLifecycle::newly_dropped(1, 64),
    );

    // Control: at spawn, the delay must still be the un-ticked default (10),
    // not already decremented.
    let at_spawn = sim.item_lifecycle(id).expect("just spawned");
    assert_eq!(at_spawn.pickup_delay, 10);
    assert_eq!(at_spawn.age, 0);
    assert!(!at_spawn.can_be_picked_up());

    sim.tick_for(10);

    let after = sim.item_lifecycle(id).expect("still tracked at age 10 < 6000");
    assert_eq!(after.pickup_delay, 0, "10 ticks must exhaust the default delay");
    assert_eq!(after.age, 10);
    assert!(after.can_be_picked_up());
}

/// `ItemEntity.tick`: `this.age >= 6000` discards the entity. Driven
/// exclusively through `MobSim::tick`/`item_lifecycle`/`item_count` — never
/// through `ItemEntityRegistry::tick` directly — so a regression that
/// un-wires `MobSim::tick` from `self.items.tick()` fails this, not just the
/// registry's own (already-passing) unit test.
#[test]
fn mobsim_tick_actually_despawns_an_item_at_the_exact_vanilla_age() {
    let world = ChunkWorld::new(0, 192);
    let mut sim = MobSim::new(&world);

    let despawns_at_5999 = sim.spawn_item(
        rk("minecraft:stick"),
        Vec3::new(0.0, 64.0, 0.0),
        Vec3::new(0.0, 0.0, 0.0),
        ItemLifecycle {
            age: 5999,
            pickup_delay: 0,
            count: 1,
            max_stack_size: 64,
        },
    );
    // Control: a young sibling that must survive the same ticks the old one
    // does not, proving despawn is per-entry, not a blanket sweep that would
    // remove everything after N ticks regardless of age.
    let survives = sim.spawn_item(
        rk("minecraft:stick"),
        Vec3::new(0.0, 64.0, 0.0),
        Vec3::new(0.0, 0.0, 0.0),
        ItemLifecycle::newly_dropped(1, 64),
    );

    assert_eq!(sim.item_count(), 2);
    sim.tick_for(1);
    assert!(
        sim.item_lifecycle(despawns_at_5999).is_none(),
        "age 5999 -> 6000 this tick must despawn"
    );
    assert!(
        sim.item_lifecycle(survives).is_some(),
        "a freshly dropped sibling must not be swept along with it"
    );
    assert_eq!(sim.item_count(), 1);
    assert!(
        sim.snapshots().iter().all(|s| s.id != despawns_at_5999),
        "a despawned item must not still be published to the wire"
    );
}

/// The wire-facing seam: `MobSim::snapshots()` must actually include
/// projectiles and dropped items, not just mobs — `run_mob_tick_loop`
/// publishes exactly this to `LiveMobSource`, so a snapshot list that only
/// ever covered mobs would mean a correctly-ticking registry still reached
/// zero pixels (CLAUDE.md's "island" failure mode, one hop further along
/// than "nothing constructs it").
#[test]
fn snapshots_include_projectiles_and_items_with_their_own_identity_and_motion() {
    let world = ChunkWorld::new(0, 192);
    let mut sim = MobSim::new(&world);

    let arrow_pos = Vec3::new(0.0, 64.0, 0.0);
    let arrow_vel = Vec3::new(2.0, 0.0, 0.0);
    let arrow_id =
        sim.spawn_projectile(rk("minecraft:arrow"), Projectile::arrow(arrow_pos, arrow_vel));

    let item_pos = Vec3::new(5.0, 64.0, 0.0);
    let item_id = sim.spawn_item(
        rk("minecraft:diamond"),
        item_pos,
        Vec3::new(0.0, 0.0, 0.0),
        ItemLifecycle::newly_dropped(3, 64),
    );

    let snaps = sim.snapshots();
    assert_eq!(snaps.len(), 2, "no mobs were spawned, just these two entities");

    let arrow_snap = snaps
        .iter()
        .find(|s| s.id == arrow_id)
        .expect("arrow snapshot present");
    assert_eq!(arrow_snap.entity_type, rk("minecraft:arrow"));
    assert_eq!(arrow_snap.position, arrow_pos);
    assert_eq!(arrow_snap.velocity, arrow_vel);

    let item_snap = snaps
        .iter()
        .find(|s| s.id == item_id)
        .expect("item snapshot present");
    // **This assertion used to read `rk("minecraft:diamond")`, and that was the
    // bug, not the fix.**
    //
    // `EntitySnapshot::entity_type` is an *entity* type key, and a dropped item's
    // is `minecraft:item` — the stack's own identity travels as entity metadata
    // (`ItemEntity.DATA_ITEM`), not in this field. Setting it to the item key
    // meant `v770`'s `encode_add_entity_body` called
    // `entity_type_id("minecraft:diamond")`, which misses because that is not an
    // entity type, and its `.unwrap_or(0)` resolved the miss to network entity
    // type **`0` = `minecraft:acacia_boat`**. Every dropped item this server has
    // ever spawned arrived at a client as a boat, silently.
    //
    // The test name ("with their own identity") and this line agreed with each
    // other, which is why it survived review: it was self-consistent and wrong.
    // Nothing in `cargo xtask connectedness` could see it either — the wire was
    // fully connected and carrying a wrong value, the same shape as the
    // wall-clock-instead-of-tick-counter `SET_TIME` regression.
    assert_eq!(
        item_snap.entity_type,
        rk("minecraft:item"),
        "a dropped item streams as entity type `minecraft:item`; the item's own \
         key here resolves to `minecraft:acacia_boat` on the wire"
    );
    assert_eq!(item_snap.position, item_pos);

    // Distinct uuids: two different entities must not collide on wire
    // identity even though both were "just spawned" in the same tick.
    assert_ne!(arrow_snap.uuid, item_snap.uuid);
}
