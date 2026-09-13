use super::*;

mod item_owner_tests;

/// The `follow_range` attribute reaches the controller that bounds target
/// acquisition, including the no-target case.
#[cfg(test)]
mod follow_range_tests;
/// Host-resolved persistent-anger deadline tests.
#[cfg(test)]
mod anger_tests;

/// MobSim seam primitive tests (instant relocation / self-damage / target
/// identity): the host half of the seam primitives in `lodestone-entity`. The
/// gaze feed is not supplied by this seam — see
/// [`PlayerPerception`]'s lack of a view vector.
#[cfg(test)]
mod primitives_tests;

/// Block-identity cues read from generated tag data, and the graze handoff out
/// of an immutably borrowed world.
#[cfg(test)]
mod block_cues_tests;

/// Age-scaled hitbox and baby-only movement modifier, including the
/// `species_shape`/`SimMob::set_age` path that applies `is_baby`.
#[cfg(test)]
mod baby_shape_tests;

/// `MetadataField::Baby`'s producer-side species switch in
/// [`SimMob::snapshot`] — the eligible species must match exactly the union
/// [`baby_dimensions`]/[`baby_speed_multiplier`] already scope "grows a
/// baby" to, and the ineligible species (index 16's other claimants) must
/// never see the field at all.
#[cfg(test)]
mod baby_metadata_tests;

/// Lead attach/detach, the fence-knot re-parent, and the
/// distance-based pull/snap physics.
#[cfg(test)]
mod leash_tests;

/// Entity-spawn slice: the trader plus its leashed llama
/// escort. The spawn-cycle timing/POI search is out of scope here — see
/// `spawn_wandering_trader`'s own doc comment.
#[cfg(test)]
mod wandering_trader_tests;

/// `MobSim`'s periodic idle-vocalisation producer (`roll_ambient_sound`),
/// wired into [`MobSim::tick`], emits ambient sounds during ordinary
/// exploration in addition to hurt and death sounds.
#[cfg(test)]
mod ambient_sound_tests;

/// Gossip, reputation and zombie-villager curing,
/// driven through real production entry points
/// (`MobSim::interact`/`MobSim::tick`/`MobSim::attack_from_player`) rather
/// than calling `villager::gossip`/`villager::reputation`/`villager::conversion`
/// directly — those modules' own test suites already cover the pure
/// arithmetic; what these gates prove is that the wiring actually reaches a
/// live [`SimMob`], the same "reaches pixels, not just a closed loop"
/// standard `ambient_sound_tests` above applies.
#[cfg(test)]
mod villager_gossip_reputation_and_curing_tests;

#[cfg(test)]
mod allay_carrying_tests;

/// Villager hurt or nearby-hostile conditions can summon an iron golem through
/// the integrated mob-simulation path.
#[cfg(test)]
mod golem_summon_tests;

/// Cat gift and parrot shoulder-ride requests are drained and resolved by the
/// real [`MobSim::tick`] loop, covering the production connection between
/// request producers and host consumers.
#[cfg(test)]
mod cat_gift_and_shoulder_tests;

/// The cat block search (`MobSim::tick_cat_block_search`) uses a host-computed
/// candidate position.
#[cfg(test)]
mod cat_block_search_tests;

/// Villager bed claiming runs through `tick_villager_beds` and
/// `occupied_homes_in_range` in the real per-tick [`MobSim`] loop, exercising
/// the integrated path rather than standalone helpers.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod villager_bed_claim_tests;

/// WORK/MEET/REST schedule: proves the chain claimed-POI ->
/// `MobSim::set_day_time` -> `crate::brain::roster::villager_brain`'s
/// schedule -> `WalkToPoi`/`MoveToTargetSink` -> a real position change
/// reaches a real, spawned villager through `MobSim::tick`, the same
/// not-an-island bar `villager_bed_claim_tests` already sets for bed
/// claiming and `vibration_substrate_tests` sets for the warden.
#[cfg(test)]
mod villager_schedule_tests;

/// Vibration events produced by `reap_dead` reach `resolve_vibrations` through
/// the real per-tick [`MobSim`] loop.
#[cfg(test)]
mod vibration_substrate_tests;

/// The elder guardian's mining-fatigue aura,
/// vanilla's own elder-guardian AI step calling
/// its own "add effect to players around" helper.
#[cfg(test)]
mod elder_guardian_mining_fatigue_tests;

/// Goat spawn-finalization's pre-broken-horn roll and the metadata field
/// ([`crate::protocol::MetadataField::GoatHorns`]) that reaches the client.
/// The test wires both through a real [`MobSim::spawn_species`] call.
#[cfg(test)]
mod goat_horn_tests;
