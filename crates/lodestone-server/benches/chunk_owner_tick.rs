//! Criterion entry point for the deterministic chunk-owner mixed workload.
//!
//! Run `cargo bench -p lodestone-server --bench chunk_owner_tick -- --quick`
//! for construction and counter verification. Use the matching Samply recipe
//! only for a local call-tree investigation; this bench has no timing gate.

mod support;

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use lodestone_server::chunk_owner_profile::{
    AMBIENT_MOB_COUNT, OWNER_COUNT, SCENE_NAME, ChunkOwnerProfileReport, run,
};
use lodestone_v26_2::V770ServerProtocol;

const PROFILE_TICKS: u64 = 128;

fn assert_report(report: ChunkOwnerProfileReport) {
    assert_eq!(report.stats.tick_count, report.ticks, "the paused runtime must drive exactly the requested ticks");
    assert_eq!(report.stats.mobs_and_items.total_sample_count, report.ticks);
    assert_eq!(report.stats.weather_and_sleep.total_sample_count, report.ticks);
    assert_eq!(report.stats.scheduled_and_physics.total_sample_count, report.ticks);
    assert!(
        report.stats.owner_work.scheduled_block_ticks >= OWNER_COUNT as u64,
        "the scene's one scheduled block tick per owner must reach the central drain"
    );
    assert!(
        report.stats.owner_work.scheduled_fluid_ticks >= OWNER_COUNT as u64,
        "the scene's one scheduled fluid tick per owner must reach the central drain"
    );
    assert!(
        report.stats.owner_work.block_entity_batches >= OWNER_COUNT as u64 * report.ticks,
        "every resident furnace owner must return one batch per driven tick"
    );
    assert!(
        report.stats.owner_work.entity_effect_batches > 0,
        "the deterministic {AMBIENT_MOB_COUNT}-cow scene must emit ambient owner batches"
    );
    assert!(
        report.stats.owner_work.entity_effects >= report.stats.owner_work.entity_effect_batches,
        "every nonempty entity owner batch contains an ambient effect"
    );
}

fn chunk_owner_tick(c: &mut Criterion) {
    let report = run(V770ServerProtocol, PROFILE_TICKS);
    assert_report(report);
    println!(
        "[chunk_owner_tick] scene={SCENE_NAME} ticks={} phases={}/{}/{} \
         owners block_due={} fluid_due={} block_batches={} block_effects={} \
         entity_batches={} entity_effects={}",
        report.ticks,
        report.stats.mobs_and_items.total_sample_count,
        report.stats.weather_and_sleep.total_sample_count,
        report.stats.scheduled_and_physics.total_sample_count,
        report.stats.owner_work.scheduled_block_ticks,
        report.stats.owner_work.scheduled_fluid_ticks,
        report.stats.owner_work.block_entity_batches,
        report.stats.owner_work.block_entity_effects,
        report.stats.owner_work.entity_effect_batches,
        report.stats.owner_work.entity_effects,
    );
    for (metric, value, unit) in [
        ("ticks", report.ticks as f64, "ticks"),
        ("scheduled_block_ticks", report.stats.owner_work.scheduled_block_ticks as f64, "ticks"),
        ("scheduled_fluid_ticks", report.stats.owner_work.scheduled_fluid_ticks as f64, "ticks"),
        ("block_entity_batches", report.stats.owner_work.block_entity_batches as f64, "batches"),
        ("entity_effect_batches", report.stats.owner_work.entity_effect_batches as f64, "batches"),
        ("entity_effects", report.stats.owner_work.entity_effects as f64, "effects"),
    ] {
        support::record(support::Record {
            bench: "chunk_owner_tick",
            metric,
            scene: SCENE_NAME,
            value,
            unit,
        });
    }
    c.bench_function("server/chunk_owner_mixed_8", |b| {
        b.iter(|| black_box(run(V770ServerProtocol, PROFILE_TICKS)));
    });
}

criterion_group!(benches, chunk_owner_tick);
criterion_main!(benches);
