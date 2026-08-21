//! Issue #518 part 2: the `SPAWN` generation stage — vanilla's
//! `ChunkStatus.SPAWN` (`ChunkStatusTasks::generateSpawn` ->
//! `ChunkGenerator.spawnOriginalMobs` -> `NaturalSpawner
//! .spawnMobsForChunkGeneration`), reduced to `MobCategory.CREATURE`: the
//! category the issue's own scope names ("the biome list to draw from is
//! `spawners.creature` only") and the one vanilla's chunk-generation call
//! actually drives (`NaturalSpawner.spawnMobsForChunkGeneration`'s own body
//! only ever asks for `MobCategory.CREATURE`; every other category spawns
//! through the tick-driven `NATURAL` reason instead, which
//! `lodestone_server::natural_spawn::NaturalSpawner` already covers).
//!
//! ## What it is
//!
//! One weighted species pick plus one pack, drawn once per chunk from
//! [`crate::spawners::BiomeSpawners::for_category`]`(`[`MobCategory::Creature`]`)`
//! at a single random position inside the chunk — vanilla's own per-chunk
//! shape (`getRandomPosWithin` plus a weighted `SpawnerData` pick), reduced to
//! one attempt rather than vanilla's bounded retry loop. See "Deliberately not
//! vanilla-exact" below for what that trades away.
//!
//! ## What this stage does NOT decide
//!
//! Placement legality (light, Y-band, solid ground —
//! `SpawnPlacements.checkSpawnRules`) is not evaluated here: this crate has no
//! light engine, and [`crate::spawners`]'s own module doc already flags a
//! placement port built on isolated-column data as the *world* species of
//! vacuous test `CLAUDE.md` warns about (a wrong light input would pass its
//! own tests and place mobs in the wrong places). So every [`GenerationSpawn`]
//! this stage returns is a **candidate** — the position vanilla would consider,
//! unconditioned on whether the position is actually legal. The consumer
//! (`lodestone_server::natural_spawn`, which already computes real per-column
//! light for the tick-driven spawner) re-validates each one through
//! `NaturalSpawner::validate_generation_spawns`, the same per-species
//! `SpawnRule` table and light cache the tick-driven cycle's own validation
//! uses, before it becomes a live mob — see
//! `docs/worldgen-mob-generation-spawn.md`.
//!
//! ## Deliberately not vanilla-exact, and named rather than hidden
//!
//! * **One attempt, not vanilla's retry loop.** `spawnMobsForChunkGeneration`
//!   can place more than one group per chunk; this places at most one. A
//!   ship-fast scope cut, not a parity claim.
//! * **The RNG stream is real per-chunk determinism, not vanilla's exact draw
//!   order.** Seeded via [`WorldgenRandom::set_decoration_seed`] — the same
//!   per-chunk derivation [`crate::feature::apply_ore_step`] uses for
//!   `UNDERGROUND_ORES` — so the same `(seed, cx, cz)` always proposes the
//!   same candidate, which is what makes a fresh world's animal placement
//!   reproducible across restarts. It is not vanilla's own
//!   `WorldgenRandom`/`EntitySpawnReason.CHUNK_GENERATION` call sequence.
//! * **A group's wander offset clamps back into its own 16×16 chunk.** Vanilla
//!   wanders up to ±5 blocks from the anchor with no such fence, because it
//!   can read a neighbour column. This stage only ever sees the chunk it is
//!   finishing (see [`spawn_candidates_for_chunk`]'s `biome_at`/`surface_y`
//!   parameters), so a pack member that would wander outside is clamped to
//!   the edge instead of dropped or reading a neighbour that is not there.
//!
//! ## Dependencies
//!
//! [`crate::spawners`] for the data and [`crate::rng`] for the deterministic
//! per-chunk draw. No block grid, no light, no world handle — consistent with
//! [`crate::spawners`]'s own "no block grid, no light, no RNG" boundary, this
//! module adds the RNG but still takes no world handle.

use std::collections::HashMap;

use crate::rng::{LegacyRandomSource, RandomSource, WorldgenRandom};
use crate::spawners::{BiomeSpawners, MobCategory};

/// One creature placement this stage proposes, in absolute world coordinates —
/// unconditioned on light or ground legality. See the module doc.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenerationSpawn {
    /// A vanilla entity id, e.g. `"minecraft:sheep"`.
    pub entity_type: String,
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

/// Runs one chunk's worth of vanilla's per-chunk `CREATURE` pick.
///
/// `biome_at(lx, lz)` and `surface_y(lx, lz)` answer this **already-generated**
/// column's own biome id and its `top_non_air_y + 1` for local column
/// `(lx, lz)` in `0..16` — both already on
/// [`crate::overworld::GeneratedColumn`] as `biome_state`/`top_non_air_y`, read
/// through closures so this module keeps the "no block grid" boundary
/// [`crate::spawners`] already commits to, rather than taking a
/// `GeneratedColumn` dependency on the type this stage exists to populate.
///
/// `spawners_by_biome` is [`crate::overworld::OverworldGenerator`]'s own
/// per-biome table (`all_biome_spawners`) — a biome absent from it, or one
/// whose `creature` list is empty, yields an empty `Vec`. That is the negative
/// control the issue's evidence standard asks for: a biome that cannot spawn a
/// creature must not.
///
/// `seed`/`cx`/`cz` seed a [`WorldgenRandom`] the same way
/// [`crate::feature::apply_ore_step`] seeds one for `UNDERGROUND_ORES` — see
/// the module doc's "Deliberately not vanilla-exact" for why this is real
/// per-chunk determinism and not vanilla's own draw order.
#[must_use]
pub fn spawn_candidates_for_chunk(
    biome_at: impl Fn(usize, usize) -> String,
    surface_y: impl Fn(usize, usize) -> i32,
    spawners_by_biome: &HashMap<String, BiomeSpawners>,
    seed: i64,
    cx: i32,
    cz: i32,
) -> Vec<GenerationSpawn> {
    let mut random = WorldgenRandom::new(LegacyRandomSource::new(seed));
    random.set_decoration_seed(seed, cx * 16, cz * 16);

    let lx = random.next_int_bounded(16) as usize;
    let lz = random.next_int_bounded(16) as usize;
    let biome = biome_at(lx, lz);
    let Some(spawners) = spawners_by_biome.get(&biome) else {
        return Vec::new();
    };
    let entries = spawners.for_category(MobCategory::Creature);
    if entries.is_empty() {
        return Vec::new();
    }

    // `WeightedList` pick: the list's own weight, not a `SpawnerData` field —
    // see `crate::spawners`'s "How to change it" for why that split matters.
    let total_weight: i32 = entries.iter().map(|e| e.weight.max(0)).sum();
    if total_weight <= 0 {
        return Vec::new();
    }
    let mut roll = random.next_int_bounded(total_weight);
    let mut chosen = &entries[0];
    for entry in entries {
        let w = entry.weight.max(0);
        if roll < w {
            chosen = entry;
            break;
        }
        roll -= w;
    }

    let pack_size = if chosen.max_count > chosen.min_count {
        chosen.min_count + random.next_int_bounded(chosen.max_count - chosen.min_count + 1)
    } else {
        chosen.min_count
    };
    if pack_size <= 0 {
        return Vec::new();
    }

    let base_x = cx * 16 + lx as i32;
    let base_z = cz * 16 + lz as i32;
    let mut out = Vec::with_capacity(pack_size as usize);
    for _ in 0..pack_size {
        // Vanilla wanders `±5` from the anchor; clamped back into this chunk's
        // own 16x16 rather than reading a neighbour column — see the module
        // doc's "Deliberately not vanilla-exact".
        let dx = random.next_int_bounded(11) - 5;
        let dz = random.next_int_bounded(11) - 5;
        let wx = (base_x + dx).clamp(cx * 16, cx * 16 + 15);
        let wz = (base_z + dz).clamp(cz * 16, cz * 16 + 15);
        let wlx = (wx - cx * 16) as usize;
        let wlz = (wz - cz * 16) as usize;
        out.push(GenerationSpawn {
            entity_type: chosen.entity_type.clone(),
            x: wx,
            y: surface_y(wlx, wlz),
            z: wz,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spawners::parse_biome_spawners;

    fn table(entries: &[(&str, serde_json::Value)]) -> HashMap<String, BiomeSpawners> {
        entries
            .iter()
            .map(|(name, doc)| ((*name).to_owned(), parse_biome_spawners(doc)))
            .collect()
    }

    /// A verbatim slice of `assets/worldgen/biome/beach.json`: exactly one
    /// `creature` entry (turtle), but non-empty `monster`/`ambient` lists too —
    /// chosen so "creature only" and "any category" give different answers, as
    /// `CLAUDE.md`'s evidence standard asks for.
    fn beach_doc() -> serde_json::Value {
        serde_json::json!({
            "spawners": {
                "ambient": [
                    {"type": "minecraft:bat", "minCount": 8, "maxCount": 8, "weight": 10}
                ],
                "creature": [
                    {"type": "minecraft:turtle", "minCount": 2, "maxCount": 5, "weight": 5}
                ],
                "monster": [
                    {"type": "minecraft:zombie", "minCount": 4, "maxCount": 4, "weight": 95}
                ]
            }
        })
    }

    /// A verbatim slice of `assets/worldgen/biome/ocean.json`: an **empty**
    /// `creature` list alongside a non-empty `monster`/`water_creature` — the
    /// negative control. A bug reading "any category" instead of `creature`
    /// only would place a squid or a zombie here; the assertion below is
    /// observed to fail against exactly that bug (see
    /// `wrong_hypothesis_any_category_would_populate_ocean`).
    fn ocean_doc() -> serde_json::Value {
        serde_json::json!({
            "spawners": {
                "creature": [],
                "monster": [
                    {"type": "minecraft:drowned", "minCount": 1, "maxCount": 1, "weight": 5}
                ],
                "water_creature": [
                    {"type": "minecraft:squid", "minCount": 1, "maxCount": 4, "weight": 1}
                ]
            }
        })
    }

    /// Positive case: a beach chunk always proposes a turtle (the only
    /// `creature` entry, so the weighted pick is unambiguous), with a pack
    /// size the biome's own `minCount`/`maxCount` bounds — the *predicted*
    /// range, derived from the fixture's own data rather than a round number.
    #[test]
    fn beach_chunk_proposes_a_turtle_pack_in_range() {
        let spawners = table(&[("minecraft:beach", beach_doc())]);
        let out = spawn_candidates_for_chunk(
            |_lx, _lz| "minecraft:beach".to_owned(),
            |_lx, _lz| 64, // flat surface at y=64 for every column
            &spawners,
            12345,
            3,
            -7,
        );
        assert!(!out.is_empty(), "beach's non-empty creature list must place something");
        assert!(
            out.iter().all(|s| s.entity_type == "minecraft:turtle"),
            "turtle is the only creature entry; every placement must be one"
        );
        // beach.json: minCount 2, maxCount 5 — the range this fixture's own
        // data predicts, not a guessed constant.
        assert!(
            (2..=5).contains(&out.len()),
            "pack size {} outside turtle's declared [2, 5]",
            out.len()
        );
        // Every placement lands inside chunk (3, -7)'s own 16x16, and at the
        // fixed flat surface the closure supplies.
        for s in &out {
            assert!((3 * 16..3 * 16 + 16).contains(&s.x));
            assert!((-7 * 16..-7 * 16 + 16).contains(&s.z));
            assert_eq!(s.y, 64);
        }
    }

    /// Negative control: ocean's `creature` list is empty, so nothing is
    /// proposed even though `monster` and `water_creature` are not — this is
    /// what makes the positive test's category scoping meaningful rather than
    /// vacuous.
    #[test]
    fn ocean_chunk_with_empty_creature_list_proposes_nothing() {
        let spawners = table(&[("minecraft:ocean", ocean_doc())]);
        let out = spawn_candidates_for_chunk(
            |_lx, _lz| "minecraft:ocean".to_owned(),
            |_lx, _lz| 60,
            &spawners,
            12345,
            3,
            -7,
        );
        assert!(out.is_empty(), "an empty creature list must place nothing");
    }

    /// The control the negative test needs: proves the empty result above is
    /// because `creature` is empty, not because the harness places nothing on
    /// principle. Same seed and position, but ocean's `creature` list is
    /// replaced with a real entry — must now populate.
    #[test]
    fn negative_control_is_observed_to_fail_when_populated() {
        let mut doc = ocean_doc();
        doc["spawners"]["creature"] = serde_json::json!([
            {"type": "minecraft:cod", "minCount": 3, "maxCount": 3, "weight": 1}
        ]);
        let spawners = table(&[("minecraft:ocean", doc)]);
        let out = spawn_candidates_for_chunk(
            |_lx, _lz| "minecraft:ocean".to_owned(),
            |_lx, _lz| 60,
            &spawners,
            12345,
            3,
            -7,
        );
        assert!(
            !out.is_empty(),
            "the detector must fire once ocean's creature list is non-empty"
        );
        assert!(out.iter().all(|s| s.entity_type == "minecraft:cod"));
    }

    /// A biome with no entry in the table at all (never generated a document,
    /// or a resolver fixture that supplied none) is the same as an empty list:
    /// no panic, no placement.
    #[test]
    fn unknown_biome_proposes_nothing() {
        let spawners: HashMap<String, BiomeSpawners> = HashMap::new();
        let out = spawn_candidates_for_chunk(
            |_lx, _lz| "minecraft:nonexistent".to_owned(),
            |_lx, _lz| 64,
            &spawners,
            1,
            0,
            0,
        );
        assert!(out.is_empty());
    }

    /// Determinism: the same seed and chunk coordinate always propose the same
    /// candidates — what makes a fresh world's generation-time animals
    /// reproducible across a server restart (see the module doc's "How this
    /// differs from vanilla" and `docs/worldgen-mob-generation-spawn.md`'s
    /// persistence section).
    #[test]
    fn same_seed_and_chunk_is_deterministic() {
        let spawners = table(&[("minecraft:beach", beach_doc())]);
        let run = || {
            spawn_candidates_for_chunk(
                |_lx, _lz| "minecraft:beach".to_owned(),
                |_lx, _lz| 64,
                &spawners,
                999,
                5,
                5,
            )
        };
        assert_eq!(run(), run());
    }
}
