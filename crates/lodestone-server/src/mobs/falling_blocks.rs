//! `MobSim`'s falling-block slice — spawn, per-tick fall/landing, and the
//! query API. Moved out of `mobs/mod.rs` verbatim as part of the `mobs.rs`
//! file split (see `docs/plans/crate-and-file-splits.md`). Zero visibility
//! churn: every method here was already `pub`.

use lodestone_model::{BlockPos, Vec3};
use uuid::Uuid;

use crate::gravity_tick::FallingBlockEffect;

use super::{MobSim, TrackedFallingBlock};

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

    /// The number of live falling blocks.
    #[must_use]
    pub fn falling_block_count(&self) -> usize {
        self.falling_blocks.len()
    }

    /// The current position of a tracked falling block, if any — the entity-space
    /// position (block centre in `x`/`z`), not a block position.
    #[must_use]
    pub fn falling_block_position(&self, id: i32) -> Option<Vec3> {
        self.falling_blocks.get(&id).map(|f| f.motion.position)
    }

    /// The block state a tracked falling block is imitating, if any.
    #[must_use]
    pub fn falling_block_state(&self, id: i32) -> Option<&str> {
        self.falling_blocks.get(&id).map(|f| f.state.as_str())
    }
}
