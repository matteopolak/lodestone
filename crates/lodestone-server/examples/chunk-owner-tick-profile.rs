//! Finite, Samply-ready entry point for the chunk-owner mixed workload.

use lodestone_server::chunk_owner_profile::{
    AMBIENT_MOB_COUNT, DEFAULT_PROFILE_TICKS, OWNER_COUNT, SCENE_NAME, run, validate_profile_ticks,
};
use lodestone_v26_2::V770ServerProtocol;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ticks = std::env::args()
        .nth(1)
        .map(|value| value.parse::<u64>())
        .transpose()?
        .unwrap_or(DEFAULT_PROFILE_TICKS);
    let ticks = validate_profile_ticks(ticks).map_err(|error| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, error)
    })?;
    let report = run(V770ServerProtocol, ticks);
    println!(
        "scene={SCENE_NAME} ticks={} owners={OWNER_COUNT} ambient_mobs={AMBIENT_MOB_COUNT} phases={}/{}/{} scheduled_block_ticks={} \
         scheduled_fluid_ticks={} block_entity_batches={} block_entity_effects={} \
         entity_effect_batches={} entity_effects={}",
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
    Ok(())
}
