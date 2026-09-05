//! `MobSim`'s falling-block slice — spawn, per-tick fall/landing, and the
//! query API. Moved out of `mobs/mod.rs` verbatim as part of the `mobs.rs`
//! file split (see `docs/plans/crate-and-file-splits.md`). Zero visibility
//! churn: every method here was already `pub`.

use std::collections::{HashMap, HashSet};

use lodestone_model::{BlockPos, Vec3};
use uuid::Uuid;

use crate::gravity_tick::FallingBlockEffect;

use super::{MobSim, TrackedFallingBlock};

/// The chunk responsible for a falling block at tick start.
///
/// A falling block does not travel horizontally in this simulation, so its
/// tick-start owner remains the owner of both landing effects. Keeping that
/// fact on the returned message prevents a future executor from making
/// landing publication depend on which chunk finishes first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum FallingBlockTickOwner {
    /// The owner of the chunk containing the falling entity.
    Chunk { cx: i32, cz: i32 },
}

impl FallingBlockTickOwner {
    fn for_position(position: Vec3) -> Self {
        Self::Chunk {
            cx: (position.x.floor() as i32).div_euclid(16),
            cz: (position.z.floor() as i32).div_euclid(16),
        }
    }
}

/// One ordered landing effect returned by a chunk owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FallingBlockTickEffect {
    owner: FallingBlockTickOwner,
    serial: usize,
    effect: FallingBlockEffect,
}

/// One chunk owner's completion for a falling-block tick.
///
/// Empty completions are retained. They make a skipped owner observable to the
/// central writer even when none of that owner's blocks happened to land this
/// tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FallingBlockTickEffectBatch {
    owner: FallingBlockTickOwner,
    serial: usize,
    batch_count: usize,
    effects: Vec<FallingBlockTickEffect>,
}

/// Restores the serial falling-block effect sequence at the central writer.
///
/// Owner completion order is deliberately not observable. Every tick-start
/// owner must return exactly one batch, and every returned effect must retain
/// its original serial slot before the world is allowed to apply a landing.
#[must_use]
pub(crate) fn merge_falling_block_tick_effect_batches(
    mut batches: Vec<FallingBlockTickEffectBatch>,
) -> Vec<FallingBlockEffect> {
    let expected_count = batches.first().map_or(0, |batch| batch.batch_count);
    assert_eq!(
        batches.len(),
        expected_count,
        "falling-block owner completion omitted or duplicated a plan batch"
    );
    assert!(
        batches
            .iter()
            .all(|batch| batch.batch_count == expected_count),
        "falling-block owner completions disagree about their tick-start plan"
    );
    let mut owners = HashSet::new();
    assert!(
        batches.iter().all(|batch| owners.insert(batch.owner)),
        "falling-block owner completion has a duplicate owner"
    );
    batches.sort_unstable_by_key(|batch| batch.serial);
    for (serial, batch) in batches.iter().enumerate() {
        assert_eq!(
            batch.serial, serial,
            "falling-block owner completion has a missing or stale plan slot"
        );
    }

    let mut effects = Vec::new();
    for batch in batches {
        for effect in batch.effects {
            assert_eq!(
                effect.owner, batch.owner,
                "a falling-block owner completion contains another owner's effect"
            );
            effects.push(effect);
        }
    }
    effects.sort_unstable_by_key(|effect| effect.serial);
    for (serial, effect) in effects.iter().enumerate() {
        assert_eq!(
            effect.serial, serial,
            "falling-block owner completion has a duplicate, missing, or stale effect slot"
        );
    }
    effects.into_iter().map(|effect| effect.effect).collect()
}

impl<'w> MobSim<'w> {
    // -----------------------------------------------------------------------
    // `FallingBlockEntity` — the falling sand/gravel animation
    // -----------------------------------------------------------------------

    /// `FallingBlockEntity.fall`: the block at `origin` becomes a tracked,
    /// broadcast entity that will come to rest at `landing_y`.
    ///
    /// Returns the new entity id and the two effects, **in vanilla's order**:
    /// [`ClearedOrigin`](FallingBlockEffect::ClearedOrigin) then
    /// [`Spawned`](FallingBlockEffect::Spawned). The caller applies them in the
    /// order given — this sim holds `world: &'w ChunkWorld` immutably and cannot
    /// clear the cell itself, exactly as it cannot apply a graze.
    ///
    /// # Why the order is a return value and not a comment
    ///
    /// `fall` is `new FallingBlockEntity(...)`, `level.setBlock(pos, air, 3)`,
    /// *then* `level.addFreshEntity(entity)`. If the entity is broadcast first the
    /// client shows the block **and** the falling copy in the same cell until the
    /// block update arrives. Two statements in a caller cannot be tested for
    /// order; a returned sequence can. See `crate::gravity_tick`'s module doc for
    /// the third ordering (displacement before drag) and for what this crate's
    /// transport does and does not guarantee about the wire.
    ///
    /// `landing_y` comes from `crate::gravity_tick::find_landing_y` against the
    /// live world, which the caller has and this sim does not.
    pub fn spawn_falling_block(
        &mut self,
        state: String,
        origin: BlockPos,
        landing_y: i32,
    ) -> (i32, Vec<FallingBlockEffect>) {
        let id = self.next_id;
        self.next_id += 1;
        self.falling_blocks.insert(
            id,
            TrackedFallingBlock {
                uuid: Uuid::new_v4(),
                state,
                motion: crate::gravity_tick::FallingBlockMotion::fall_from(origin),
                landing_y,
            },
        );
        (
            id,
            vec![
                FallingBlockEffect::ClearedOrigin {
                    pos: origin,
                    entity_id: id,
                },
                FallingBlockEffect::Spawned { entity_id: id },
            ],
        )
    }

    /// One tick of every live falling block — `FallingBlockEntity.tick`'s motion
    /// and landing decision, for all of them.
    ///
    /// Returns the effects of the ticks that *finished*, in vanilla's order per
    /// entity: [`Placed`](FallingBlockEffect::Placed) then
    /// [`Discarded`](FallingBlockEffect::Discarded). An entity still airborne
    /// contributes nothing — its new position rides the ordinary
    /// [`snapshots`](Self::snapshots) diff, so a caller needs no per-tick position
    /// event.
    ///
    /// The reverse order (`discard` then `setBlock`) leaves the client with
    /// *neither* a block nor an entity for as long as the two packets are apart —
    /// the same shape that made the item-pickup animation invisible, where `take`
    /// had to precede `discard`.
    ///
    /// Iterated over a **sorted** id list rather than the map: two blocks landing
    /// on the same tick must produce their placements in a run-to-run stable
    /// order, exactly as [`merge_neighbouring_items`](Self::merge_neighbouring_items)
    /// sorts for the same reason.
    pub fn tick_falling_blocks(&mut self) -> Vec<FallingBlockEffect> {
        let mut ids: Vec<i32> = self.falling_blocks.keys().copied().collect();
        ids.sort_unstable();
        let mut effects = Vec::new();
        for id in ids {
            let Some(tracked) = self.falling_blocks.get_mut(&id) else {
                continue;
            };
            let landing_y = tracked.landing_y;
            match tracked.motion.step(landing_y) {
                crate::gravity_tick::FallingBlockStep::Falling => {}
                crate::gravity_tick::FallingBlockStep::Landed { y } => {
                    let pos = BlockPos::new(
                        // `floor`, not a cast: the entity's `x` is the block
                        // centre (`origin.x + 0.5`) and `x` never changes, so this
                        // recovers `origin.x` for negative coordinates too — where
                        // `as i32` truncates toward zero and would land the block
                        // one cell east of where it fell.
                        tracked.motion.position.x.floor() as i32,
                        y,
                        tracked.motion.position.z.floor() as i32,
                    );
                    let state = tracked.state.clone();
                    effects.push(FallingBlockEffect::Placed {
                        pos,
                        state,
                        entity_id: id,
                    });
                    self.falling_blocks.remove(&id);
                    effects.push(FallingBlockEffect::Discarded { entity_id: id });
                }
                crate::gravity_tick::FallingBlockStep::Expired => {
                    // `FallingBlockEntity.tick`'s `time > 600` branch discards
                    // with no placement. Vanilla also drops the block as an item
                    // when `entityDrops` is on; not modelled, because this branch
                    // is unreachable for a fall resolved by `find_landing_y` (see
                    // `crate::gravity_tick::MAX_FALL_TICKS`) and inventing a drop
                    // for it would be untestable.
                    self.falling_blocks.remove(&id);
                    effects.push(FallingBlockEffect::Discarded { entity_id: id });
                }
            }
        }
        effects
    }

    /// Ticks falling blocks and returns one completion per tick-start chunk
    /// owner for the central world writer.
    ///
    /// Simulation remains serial through [`Self::tick_falling_blocks`]. This
    /// method only makes the existing write hand-off explicit: it snapshots
    /// every live entity's owner before advancing, retains empty owner results,
    /// and tags each produced effect with the sequence the former serial path
    /// gave it. The caller must use
    /// [`merge_falling_block_tick_effect_batches`] before applying effects.
    pub(crate) fn tick_falling_block_owner_batches(
        &mut self,
    ) -> Vec<FallingBlockTickEffectBatch> {
        let mut ids: Vec<i32> = self.falling_blocks.keys().copied().collect();
        ids.sort_unstable();

        let mut owner_by_entity = HashMap::new();
        let mut batches = Vec::new();
        for id in ids {
            let tracked = self
                .falling_blocks
                .get(&id)
                .expect("a tick-start falling-block id must remain present before its tick");
            let owner = FallingBlockTickOwner::for_position(tracked.motion.position);
            owner_by_entity.insert(id, owner);
            if !batches
                .iter()
                .any(|batch: &FallingBlockTickEffectBatch| batch.owner == owner)
            {
                batches.push(FallingBlockTickEffectBatch {
                    owner,
                    serial: batches.len(),
                    batch_count: 0,
                    effects: Vec::new(),
                });
            }
        }
        let batch_count = batches.len();
        for batch in &mut batches {
            batch.batch_count = batch_count;
        }

        for (serial, effect) in self.tick_falling_blocks().into_iter().enumerate() {
            let entity_id = match &effect {
                FallingBlockEffect::ClearedOrigin { entity_id, .. }
                | FallingBlockEffect::Spawned { entity_id }
                | FallingBlockEffect::Placed { entity_id, .. }
                | FallingBlockEffect::Discarded { entity_id } => *entity_id,
            };
            let owner = *owner_by_entity
                .get(&entity_id)
                .expect("a falling-block tick effect must belong to its tick-start owner");
            batches
                .iter_mut()
                .find(|batch| batch.owner == owner)
                .expect("a tick-start owner must have an effect completion")
                .effects
                .push(FallingBlockTickEffect {
                    owner,
                    serial,
                    effect,
                });
        }
        batches
    }

    /// The number of live falling blocks.
    #[must_use]
    pub fn falling_block_count(&self) -> usize {
        self.falling_blocks.len()
    }

}

#[cfg(test)]
mod falling_block_tests {
    use super::*;
    use super::super::ChunkWorld;
    use crate::gravity_tick::{FALLING_BLOCK_ENTITY_TYPE, FallingBlockEffect};
    use lodestone_data::block_states;

    /// A sim over a world with a solid floor at `y = -1` and nothing else. The
    /// world's contents are irrelevant here: `MobSim` never resolves a falling
    /// block's landing itself (`crate::random_tick::settle_gravity_at` does, from
    /// the live column the tick loop holds), so `landing_y` is supplied per test.
    fn sim() -> MobSim<'static> {
        let world: &'static ChunkWorld = Box::leak(Box::new(ChunkWorld::new(-64, 384)));
        MobSim::new(world)
    }

    /// **The spawn ordering, as an ordering fact.** `FallingBlockEntity.fall`
    /// clears the origin cell *before* `addFreshEntity`, so the client is never
    /// told about the entity while the block it came from is still there.
    ///
    /// Both wrong orderings the brief for this work named are constructed
    /// explicitly and required to differ from what the code produced, rather than
    /// described in prose. Mismatches are collected and asserted on afterwards: an
    /// `assert!` per candidate would abort at the first one, so a neuter would
    /// demonstrate one arm and leave the rest as arguments.
    #[test]
    fn a_spawn_clears_the_origin_cell_before_it_broadcasts_the_entity() {
        let mut sim = sim();
        let origin = BlockPos::new(3, 70, -8);
        let (id, effects) = sim.spawn_falling_block("minecraft:sand".to_string(), origin, 64);

        let expected = vec![
            FallingBlockEffect::ClearedOrigin {
                pos: origin,
                entity_id: id,
            },
            FallingBlockEffect::Spawned { entity_id: id },
        ];
        assert_eq!(effects, expected, "`fall` is setBlock(air) then addFreshEntity");

        // The two rejected orderings, each named by what a player would see.
        let mut reversed = expected.clone();
        reversed.reverse();
        let wrong: Vec<(&str, Vec<FallingBlockEffect>)> = vec![
            (
                "entity broadcast before the cell is cleared: the block and its \
                 falling copy are both visible until the block update lands",
                reversed,
            ),
            (
                "the cell cleared and no entity at all: the block simply vanishes",
                vec![FallingBlockEffect::ClearedOrigin {
                    pos: origin,
                    entity_id: id,
                }],
            ),
        ];
        let coincidences: Vec<&str> = wrong
            .iter()
            .filter(|(_, candidate)| *candidate == effects)
            .map(|(why, _)| *why)
            .collect();
        assert!(
            coincidences.is_empty(),
            "the produced sequence matches a rejected ordering: {coincidences:?}"
        );
    }

    /// **The landing ordering, as an ordering fact.** The landing branch is
    /// `setBlock(pos, blockState, 3)`, the block-update broadcast, *then*
    /// `discard()`. The reverse leaves the client with neither a block nor an
    /// entity — the shape that made the item-pickup animation invisible, where
    /// `take` had to precede `discard`.
    #[test]
    fn a_landing_places_the_block_before_it_discards_the_entity() {
        let mut sim = sim();
        let origin = BlockPos::new(3, 70, -8);
        let (id, _) = sim.spawn_falling_block("minecraft:gravel".to_string(), origin, 64);

        // Step until the landing. 18 ticks is the predicted count for a 6-block
        // drop (see `crate::gravity_tick`'s own gate, which derives it from the
        // closed form); the bound here is generous because this test is about the
        // *order* of the landing's effects, not about when it happens.
        let mut landing: Option<Vec<FallingBlockEffect>> = None;
        for _ in 0..40 {
            let effects = sim.tick_falling_blocks();
            if !effects.is_empty() {
                landing = Some(effects);
                break;
            }
        }
        let effects = landing.expect("the fall must finish inside 40 ticks");

        let expected = vec![
            FallingBlockEffect::Placed {
                pos: BlockPos::new(3, 64, -8),
                state: "minecraft:gravel".to_string(),
                entity_id: id,
            },
            FallingBlockEffect::Discarded { entity_id: id },
        ];
        assert_eq!(effects, expected);

        let mut reversed = expected.clone();
        reversed.reverse();
        let wrong: Vec<(&str, Vec<FallingBlockEffect>)> = vec![
            (
                "discarded before the block is placed: the client has neither for \
                 as long as the two packets are apart",
                reversed,
            ),
            (
                "discarded with no placement at all: the block is destroyed by \
                 landing",
                vec![FallingBlockEffect::Discarded { entity_id: id }],
            ),
            (
                "placed with no discard: the entity keeps falling through its own \
                 landed block, and streams forever",
                vec![FallingBlockEffect::Placed {
                    pos: BlockPos::new(3, 64, -8),
                    state: "minecraft:gravel".to_string(),
                    entity_id: id,
                }],
            ),
        ];
        let coincidences: Vec<&str> = wrong
            .iter()
            .filter(|(_, candidate)| *candidate == effects)
            .map(|(why, _)| *why)
            .collect();
        assert!(
            coincidences.is_empty(),
            "the produced sequence matches a rejected ordering: {coincidences:?}"
        );
        assert_eq!(
            sim.falling_block_count(),
            0,
            "the discard must really remove the entity, or it streams forever"
        );
    }

    fn owner_batch_fixture() -> MobSim<'static> {
        let mut sim = sim();
        // Entity ids give the old serial order A, B, A. Reversing owner
        // completion therefore differs from that sequence instead of passing
        // vacuously because each owner has only one landing.
        sim.spawn_falling_block(
            "minecraft:sand".to_string(),
            BlockPos::new(-1, 70, -1),
            64,
        );
        sim.spawn_falling_block(
            "minecraft:gravel".to_string(),
            BlockPos::new(16, 70, 0),
            64,
        );
        sim.spawn_falling_block(
            "minecraft:red_sand".to_string(),
            BlockPos::new(-2, 70, -2),
            64,
        );
        sim
    }

    fn first_serial_landing(sim: &mut MobSim<'static>) -> Vec<FallingBlockEffect> {
        for _ in 0..40 {
            let effects = sim.tick_falling_blocks();
            if !effects.is_empty() {
                return effects;
            }
        }
        panic!("all fixture blocks must land inside 40 ticks");
    }

    fn first_owner_batches(sim: &mut MobSim<'static>) -> Vec<FallingBlockTickEffectBatch> {
        for _ in 0..40 {
            let batches = sim.tick_falling_block_owner_batches();
            if batches.iter().any(|batch| !batch.effects.is_empty()) {
                return batches;
            }
        }
        panic!("all fixture blocks must land inside 40 ticks");
    }

    /// Owner completion is not the old entity-id sequence. The serial
    /// reference runs a separate simulation through the former direct path;
    /// the owner path then reverses the same completions before the central
    /// merge consumes them.
    #[test]
    fn falling_block_owner_batches_restore_serial_effect_order_after_reversed_completion() {
        let serial = first_serial_landing(&mut owner_batch_fixture());
        let mut batches = first_owner_batches(&mut owner_batch_fixture());
        assert_eq!(
            batches.iter().map(|batch| batch.owner).collect::<Vec<_>>(),
            [
                FallingBlockTickOwner::Chunk { cx: -1, cz: -1 },
                FallingBlockTickOwner::Chunk { cx: 1, cz: 0 },
            ],
            "owners enter the completion plan at their first serial entity"
        );
        batches.reverse();
        let completion_order = batches
            .iter()
            .flat_map(|batch| batch.effects.iter())
            .map(|effect| effect.effect.clone())
            .collect::<Vec<_>>();
        assert_ne!(
            completion_order, serial,
            "control requires reversed owner completion to change raw effect order"
        );
        assert_eq!(
            merge_falling_block_tick_effect_batches(batches),
            serial,
            "the central merge must restore every serial landing effect"
        );
    }

    #[test]
    #[should_panic(expected = "omitted or duplicated a plan batch")]
    fn falling_block_owner_batch_merge_rejects_a_missing_owner() {
        let mut batches = first_owner_batches(&mut owner_batch_fixture());
        batches.pop();
        let _ = merge_falling_block_tick_effect_batches(batches);
    }

    #[test]
    #[should_panic(expected = "duplicate owner")]
    fn falling_block_owner_batch_merge_rejects_a_duplicate_owner() {
        let mut batches = first_owner_batches(&mut owner_batch_fixture());
        batches[1] = batches[0].clone();
        let _ = merge_falling_block_tick_effect_batches(batches);
    }

    /// The block a landing places is the one that left, at the cell it landed in —
    /// including for a **negative** `x`/`z`, which is the discriminating input.
    ///
    /// The entity's `x` is `origin.x + 0.5`, so recovering `origin.x` needs
    /// `floor`. `as i32` truncates toward zero, which is identical to `floor` for
    /// positive coordinates and one cell off for negative ones: at `x = -8` the
    /// entity sits at `-7.5` and `as i32` gives `-7`. A test at positive
    /// coordinates alone passes under both readings, so it measures nothing.
    #[test]
    fn a_landing_at_negative_coordinates_lands_in_the_cell_it_fell_from() {
        let mut sim = sim();
        let origin = BlockPos::new(-8, 70, -3);
        // Both readings, evaluated: `floor` is the correct one.
        assert_eq!((-7.5_f64).floor() as i32, -8);
        assert_eq!(-7.5_f64 as i32, -7, "the wrong reading, stated so it is excluded");

        sim.spawn_falling_block("minecraft:red_sand".to_string(), origin, 64);
        let mut placed = None;
        for _ in 0..40 {
            for effect in sim.tick_falling_blocks() {
                if let FallingBlockEffect::Placed { pos, state, .. } = effect {
                    placed = Some((pos, state));
                }
            }
            if placed.is_some() {
                break;
            }
        }
        assert_eq!(
            placed,
            Some((BlockPos::new(-8, 64, -3), "minecraft:red_sand".to_string()))
        );
    }

    /// A live falling block is in [`MobSim::snapshots`] with the falling-block
    /// entity type, the block state in its **Object Data**, and its real velocity.
    ///
    /// The object-data assertion is the one that matters: it is the only channel a
    /// client learns which block is falling
    /// (`FallingBlockEntity.defineSynchedData` registers `DATA_START_POS` alone),
    /// so a `0` here draws whatever state id `0` happens to be with nothing logged
    /// anywhere. Compared against `lodestone_data::block_states::state_id`, which
    /// is generated from the real 26.2 `Block.BLOCK_STATE_REGISTRY` — an outside
    /// source, not a restatement of the producer.
    #[test]
    fn a_live_falling_block_streams_with_its_block_state_as_object_data() {
        let mut sim = sim();
        let (id, _) = sim.spawn_falling_block(
            "minecraft:sand".to_string(),
            BlockPos::new(2, 70, 2),
            64,
        );
        sim.tick_falling_blocks();

        let snaps = sim.snapshots();
        let snap = snaps
            .iter()
            .find(|s| s.id == id)
            .expect("a live falling block must be streamed, or it reaches zero pixels");
        assert_eq!(snap.entity_type.to_string(), FALLING_BLOCK_ENTITY_TYPE);
        let expected = block_states::state_id("minecraft:sand")
            .expect("`minecraft:sand` is in the generated 26.2 state table")
            as i32;
        assert_eq!(
            snap.object_data, expected,
            "the imitated block state must ride the Object Data field"
        );
        assert_ne!(
            snap.object_data, 0,
            "control: `sand` must not resolve to state id 0, or the assertion \
             above is satisfied by the field never being written"
        );
        // One tick of gravity has run, so the velocity is the post-drag value the
        // *next* tick starts from: `0.98 * -0.04`.
        assert!(
            (snap.velocity.y - (-0.98 * 0.04)).abs() < 1e-12,
            "velocity {} is not the dragged carry after one tick",
            snap.velocity.y
        );
        assert_eq!(snap.velocity.x, 0.0, "a falling block never drifts horizontally");
        assert_eq!(snap.velocity.z, 0.0);
        assert!(
            snap.metadata.is_empty(),
            "`FallingBlockEntity` synchs no metadata a client needs"
        );
    }

    /// Two blocks landing on the same tick produce their effects in a stable
    /// order, and each pairs its own placement with its own discard.
    ///
    /// Interleaving (`Placed(a)`, `Placed(b)`, `Discarded(a)`, `Discarded(b)`)
    /// would still satisfy "place before discard" globally while breaking it per
    /// entity, which is the version that shows a hole for one of the two.
    #[test]
    fn simultaneous_landings_keep_each_entitys_place_before_its_own_discard() {
        let mut sim = sim();
        let (a, _) = sim.spawn_falling_block("minecraft:sand".to_string(), BlockPos::new(0, 70, 0), 64);
        let (b, _) = sim.spawn_falling_block("minecraft:gravel".to_string(), BlockPos::new(1, 70, 0), 64);
        assert!(a < b, "ids are assigned in spawn order");

        let mut effects = Vec::new();
        for _ in 0..40 {
            effects = sim.tick_falling_blocks();
            if !effects.is_empty() {
                break;
            }
        }
        let order: Vec<(&str, i32)> = effects
            .iter()
            .map(|e| match e {
                FallingBlockEffect::Placed { entity_id, .. } => ("placed", *entity_id),
                FallingBlockEffect::Discarded { entity_id } => ("discarded", *entity_id),
                FallingBlockEffect::ClearedOrigin { entity_id, .. } => ("cleared", *entity_id),
                FallingBlockEffect::Spawned { entity_id } => ("spawned", *entity_id),
            })
            .collect();
        assert_eq!(
            order,
            vec![("placed", a), ("discarded", a), ("placed", b), ("discarded", b)],
            "each entity's placement must be immediately followed by its own discard, \
             in ascending id order"
        );
    }

    /// A falling block leaves the snapshot set the moment it is discarded, which
    /// is what makes the entity streamer emit its `REMOVE_ENTITIES`.
    ///
    /// The control is the *before* reading: without it, an assertion that the
    /// entity is absent afterwards is satisfied by it never having been there.
    #[test]
    fn a_discarded_falling_block_leaves_the_snapshot_set() {
        let mut sim = sim();
        let (id, _) = sim.spawn_falling_block("minecraft:sand".to_string(), BlockPos::new(0, 66, 0), 64);
        assert!(
            sim.snapshots().iter().any(|s| s.id == id),
            "control: the entity must be streamed before this test can show it stops"
        );
        for _ in 0..40 {
            if !sim.tick_falling_blocks().is_empty() {
                break;
            }
        }
        assert!(
            !sim.snapshots().iter().any(|s| s.id == id),
            "a landed falling block must stop being streamed"
        );
    }
}
