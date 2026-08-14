//! Entity tick throughput and interpolation cost.
//!
//! First `benches/` directory in `lodestone-shell`. CPU-only: no GPU adapter, no
//! window, no server, no `client.jar`. Both subjects are the **production**
//! systems — `EntityInterpPlugin`'s real `GameTick` set (`tick_item_physics`,
//! `tick_walk_animation`, `tick_pickup_animations`, `tick_creeper_fuse`) and the
//! real `EntityInterpolator` — not stand-ins, because that fix specifically warns
//! that a simplified stand-in could drift from the `FrameSet::Interpolate`
//! ordering the doc calls load-bearing.
//!
//! # These are durations, and durations here are baselines rather than gates
//!
//! Unlike this harness's count-shaped benches, "how long does a tick take" has
//! no count-shaped equivalent. Per `CLAUDE.md`, a wall-clock number taken while
//! other agents build is a sample, not a measurement — and a *ratio* of two
//! sequential timings is no safer, because the two arms do not see the same
//! load. So every timing below is recorded with a ±25% advisory band and
//! **nothing asserts one**. What *is* asserted is count-shaped and load-immune:
//!
//! * the tick really had N entities in it (`EntityIndex::len() == N`), so a fast
//!   tick can never be fast because the world was empty;
//! * the extract really produced a draw per tracked entity, so interpolation
//!   cost is not being measured over entities that reach nothing.
//!
//! The per-entity normalisation (µs/entity) is the number worth reading across
//! runs: it is far less load-sensitive than the absolute, because both the
//! numerator and the entity count come from the same run.
//!
//! # One axis of that fix is deliberately not implemented, rather than faked
//!
//! That fix asks to use `LockHolds`/`hold_read`/`hold_write` to attribute cost to
//! guard-hold time. This bench drives `world.run_schedule(GameTick)` directly,
//! which involves **no guard at all** — `hold_read`/`hold_write` are what fold
//! into `LockHolds`, and they live at the `EcsHandle` boundary that `Sim` uses,
//! not inside `run_schedule`. So a `LockHolds::snapshot()` taken here would read
//! zero holds, and a gate on it would be vacuous in the most misleading way:
//! green, plausible, and measuring nothing. Wiring that axis honestly means
//! driving the tick through `hold_write(&handle, |w| w.run_schedule(GameTick))`
//! against a real `EcsHandle`, which is a different subject (lock contention)
//! from this one (per-system compute). Left to whoever wants that axis, with the
//! seam named.
//!
//! Run with `cargo bench -p lodestone-shell --bench entity_tick`, or
//! `-- --test` for a correctness-only pass.

mod support;

use std::hint::black_box;
use std::time::Instant;

use bevy_ecs::world::World;
use criterion::{Criterion, criterion_group, criterion_main};
use lodestone::entities::{EntityInterpPlugin, extracted_entity_draws, fold_entities};
use lodestone_ecs::app::App;
use lodestone_ecs::entity::{EntityIndex, EntityKind, HeadYaw, MinecraftEntityId, OnGround, Position, Rotation};
use lodestone_ecs::ingest::{IngestPlugin, IngestQueue};
use lodestone_ecs::{Extract, GameTick, NetIngest};
use lodestone_model::{ClientEvent, Rotation as ModelRotation, Vec3 as ModelVec3};

/// Entity counts the cost-scaling gate names, the last matching the ~5000 order of
/// magnitude the section-profiling bench measured for sections.
const COUNTS: [usize; 4] = [10, 100, 1000, 5000];

/// A mix of entity types, so the tick exercises more than one system: `item`
/// entities are the only ones `tick_item_physics` attaches to (`entities.rs`
/// keys it on `type_path == "item"`), and creepers are what `tick_creeper_fuse`
/// walks. A single-type crowd would leave most of the `GameTick` set idle and
/// report a per-entity cost for a tick that barely ran.
const KINDS: [&str; 3] = ["minecraft:zombie", "minecraft:creeper", "item"];

/// A world carrying the production ingest + interpolation plugins with `n`
/// entities spawned through the **real** path: `ClientEvent::EntitySpawned` into
/// `IngestQueue`, one `NetIngest` run, then `fold_entities`. Same chain
/// `hurt_overlay_pixels.rs`'s `world_with_two_tracked_zombies` uses.
fn world_with_entities(n: usize) -> World {
    let mut app = App::new();
    app.add_plugins((IngestPlugin, EntityInterpPlugin));
    let mut world = std::mem::take(app.world_mut());

    {
        let mut queue = world.resource_mut::<IngestQueue>();
        for i in 0..n {
            queue.push(ClientEvent::EntitySpawned {
                entity_id: i as i32 + 1,
                uuid: None,
                entity_type: KINDS[i % KINDS.len()].parse().expect("valid entity type key"),
                // Spread them out so nothing degenerates to one cell.
                pos: ModelVec3::new(
                    f64::from(i as i32 % 64) * 1.5,
                    64.0,
                    f64::from(i as i32 / 64) * 1.5,
                ),
                rotation: ModelRotation::new(0.0, 0.0),
                velocity: None,
            });
        }
    }
    world.run_schedule(NetIngest);
    fold_entities(&mut world);
    world
}

/// Cost of one production `GameTick` as entity count grows.
///
/// Reports µs/tick and µs/entity at N = 10/100/1000/5000 and records both. The
/// scaling question that fix asks (linear, or superlinear from a system walking the
/// whole entity set per entity) is answered by reading µs/entity across the
/// sweep: flat means linear. It is **reported, not asserted** — the four arms are
/// sequential timings and cannot be protected against a machine whose load
/// changes between them, which is the exact failure that made a ratio gate go
/// red on committed `main` earlier in this repo's history.
fn bench_entity_tick_scaling(c: &mut Criterion) {
    let mut per_entity = Vec::new();

    for n in COUNTS {
        let mut world = world_with_entities(n);

        // Anti-vacuity, count-shaped and load-immune: the tick must really have
        // N entities in it. A spawn path that silently dropped events would
        // otherwise report a wonderfully flat per-entity cost.
        let tracked = world.resource::<EntityIndex>().len();
        assert_eq!(
            tracked, n,
            "asked for {n} entities but only {tracked} are tracked — the ingest path dropped \
             spawns, so any per-entity cost below is measured over the wrong denominator"
        );

        // Warm: first tick allocates per-system state and archetype caches.
        world.run_schedule(GameTick);

        const TICKS: usize = 20;
        let mut us = Vec::with_capacity(TICKS);
        for _ in 0..TICKS {
            let t = Instant::now();
            world.run_schedule(GameTick);
            us.push(t.elapsed().as_secs_f64() * 1e6);
        }
        us.sort_by(|a, b| a.partial_cmp(b).expect("no NaN timings"));
        let median = us[us.len() / 2];
        per_entity.push((n, median / n as f64));

        println!(
            "entity GameTick: n={n} -> {median:.1}us/tick median of {TICKS} \
             ({:.4} us/entity). PROVISIONAL: wall-clock on a shared machine.",
            median / n as f64
        );
        let scene = format!("n={n} kinds={} plugins=IngestPlugin+EntityInterpPlugin", KINDS.len());
        support::record(support::Record {
            bench: "entity_tick",
            metric: "game_tick_us",
            scene: &scene,
            value: median,
            unit: "us",
        });
        support::record(support::Record {
            bench: "entity_tick",
            metric: "game_tick_us_per_entity",
            scene: &scene,
            value: median / n as f64,
            unit: "us",
        });
        support::record(support::Record {
            bench: "entity_tick",
            metric: "game_tick_tracked_entities",
            scene: &scene,
            value: tracked as f64,
            unit: "entities",
        });
    }

    println!("entity GameTick per-entity cost across the sweep (flat => linear scaling):");
    for (n, ue) in &per_entity {
        println!("  n={n:<5} {ue:.4} us/entity");
    }
    println!(
        "  reported, NOT asserted: these are four sequential wall-clock timings, so a load change \
         between arms moves the ratio. Read it on a quiet machine before concluding anything about \
         scaling."
    );

    let mut world = world_with_entities(1000);
    world.run_schedule(GameTick);
    c.bench_function("entity/game_tick_1000", |b| {
        b.iter(|| {
            world.run_schedule(GameTick);
            black_box(())
        })
    });
}

/// Spawns one entity directly by component, without the ingest queue.
///
/// `EntityInterpolator::new()` installs `CorePlugin + EntityInterpPlugin` and
/// **not** `IngestPlugin`, so its world has no `IngestQueue` to push through —
/// verified in `EntityInterpolator::new`'s own body and `lodestone-ecs/src/plugin.rs`. The private
/// `IngestSnap` test helper does this job inside the crate; a bench cannot reach
/// it, so this is the same insertion open-coded from public components.
fn spawn_direct(world: &mut World, id: i32, kind: &str, x: f32) {
    let entity = world
        .spawn((
            MinecraftEntityId(id),
            EntityKind(kind.parse().expect("valid entity type key")),
            Position(ModelVec3::new(f64::from(x), 64.0, 0.0)),
            Rotation(ModelRotation::new(0.0, 0.0)),
            HeadYaw(0.0),
            OnGround(true),
        ))
        .id();
    world.resource_mut::<EntityIndex>().insert(id, entity);
}

/// `EntityInterpolator::update_with_view`'s cost at realistic
/// tracked-entity counts, and separately the cost of entities *entering and
/// leaving* the tracked set, which that fix asks to distinguish because they are
/// different code paths with different scaling risks.
///
/// `fold_entity_snapshots`, which that fix names, **does not exist** — it was deleted
/// and the live replacement is `fold_entities` (the only references left are doc
/// comments calling it "now-deleted"). So the two functions actually benched are
/// `fold_entities` and `extracted_entity_draws`, reached through
/// `EntityInterpolator::update`, which internally runs `Update`, the `GameTick`
/// loop off the real `FrameClock`, `fold_entities` and then `Extract` — i.e. the
/// whole frame, in the production order whose load-bearing-ness that fix flags.
fn bench_interpolation(c: &mut Criterion) {
    use lodestone::entities::EntityInterpolator;

    for n in COUNTS {
        let mut interp = EntityInterpolator::new();
        for i in 0..n {
            spawn_direct(
                interp.world_mut(),
                i as i32 + 1,
                KINDS[i % KINDS.len()],
                (i % 64) as f32 * 1.5,
            );
        }
        fold_entities(interp.world_mut());
        interp.world_mut().run_schedule(Extract);

        // Anti-vacuity: interpolation must actually be producing a draw per
        // tracked entity. If the extract dropped them, a flat per-entity cost
        // would be measuring an empty frame.
        let draws = extracted_entity_draws(interp.world()).len();
        assert_eq!(
            draws, n,
            "{n} entities tracked but {draws} draws extracted — interpolation cost would be \
             measured over entities that reach nothing"
        );

        // Steady state: N tracked, no churn.
        interp.update(1.0 / 60.0);
        const FRAMES: usize = 20;
        let mut us = Vec::with_capacity(FRAMES);
        for _ in 0..FRAMES {
            let t = Instant::now();
            interp.update(1.0 / 60.0);
            us.push(t.elapsed().as_secs_f64() * 1e6);
        }
        us.sort_by(|a, b| a.partial_cmp(b).expect("no NaN timings"));
        let steady = us[us.len() / 2];

        println!(
            "interpolation: n={n} tracked -> {steady:.1}us/frame median of {FRAMES} \
             ({:.4} us/entity), {draws} draws. PROVISIONAL: wall-clock on a shared machine.",
            steady / n as f64
        );
        let scene = format!("n={n} churn=none");
        support::record(support::Record {
            bench: "entity_tick",
            metric: "interp_update_us",
            scene: &scene,
            value: steady,
            unit: "us",
        });
        support::record(support::Record {
            bench: "entity_tick",
            metric: "interp_update_us_per_entity",
            scene: &scene,
            value: steady / n as f64,
            unit: "us",
        });
    }

    // The churn arm that fix asks to separate: a fixed tracked-set size with a slice
    // of it despawned and respawned every frame, so the cost of *entering and
    // leaving* the tracked set is not folded into the steady-state number.
    {
        const N: usize = 1000;
        const CHURN: usize = 50;
        let mut interp = EntityInterpolator::new();
        for i in 0..N {
            spawn_direct(interp.world_mut(), i as i32 + 1, KINDS[i % KINDS.len()], (i % 64) as f32 * 1.5);
        }
        fold_entities(interp.world_mut());
        interp.update(1.0 / 60.0);

        const FRAMES: usize = 20;
        let mut us = Vec::with_capacity(FRAMES);
        let mut next_id = N as i32 + 1;
        for _ in 0..FRAMES {
            // Leave: forget CHURN entities. Enter: spawn CHURN fresh ones.
            let victims: Vec<i32> = interp
                .world()
                .resource::<EntityIndex>()
                .iter()
                .map(|(id, _)| id)
                .take(CHURN)
                .collect();
            let t = Instant::now();
            for id in victims {
                if let Some(entity) = interp.world_mut().resource_mut::<EntityIndex>().remove(id) {
                    interp.world_mut().despawn(entity);
                }
            }
            for _ in 0..CHURN {
                spawn_direct(interp.world_mut(), next_id, KINDS[next_id as usize % KINDS.len()], 8.0);
                next_id += 1;
            }
            fold_entities(interp.world_mut());
            interp.update(1.0 / 60.0);
            us.push(t.elapsed().as_secs_f64() * 1e6);
        }
        us.sort_by(|a, b| a.partial_cmp(b).expect("no NaN timings"));
        let churny = us[us.len() / 2];

        let tracked = interp.world().resource::<EntityIndex>().len();
        assert_eq!(
            tracked, N,
            "churn arm should hold the tracked set at {N}; it drifted to {tracked}, so this \
             measures a growing world rather than churn at a fixed size"
        );

        println!(
            "interpolation churn: n={N} tracked with {CHURN} leaving and {CHURN} entering per \
             frame -> {churny:.1}us/frame median of {FRAMES}. PROVISIONAL. Compare against the \
             n={N} churn=none figure above to separate per-tracked-entity cost from \
             entering/leaving cost — but note both are wall-clock and were taken at different \
             moments, so treat the difference as indicative, not measured."
        );
        support::record(support::Record {
            bench: "entity_tick",
            metric: "interp_update_us",
            scene: &format!("n={N} churn={CHURN}per_frame"),
            value: churny,
            unit: "us",
        });
    }

    let mut interp = EntityInterpolator::new();
    for i in 0..1000usize {
        spawn_direct(interp.world_mut(), i as i32 + 1, KINDS[i % KINDS.len()], (i % 64) as f32 * 1.5);
    }
    fold_entities(interp.world_mut());
    c.bench_function("entity/interp_update_1000", |b| {
        b.iter(|| {
            interp.update(1.0 / 60.0);
            black_box(())
        })
    });
}

criterion_group!(benches, bench_entity_tick_scaling, bench_interpolation);
criterion_main!(benches);
