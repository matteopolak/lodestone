//! Lava's random-tick ignition behavior.
//!
//! This family owns the behavior RNG draws, local-column probing, and fire
//! state construction. The scheduler delegates here after position selection.

use super::*;

impl RandomTickScheduler {

/// `LavaFluid::randomTick` — lava setting fire to what is near it, and the
/// only thing in a generated world that starts a fire at all.

///
/// Every fire in this crate traces back to here: `crate::fire` owns the
/// spread and the burnout, but a fire block has to exist first, and nothing
/// else creates one (fire has no item, so a player cannot place it, and a
/// creeper's blast carries no fire flag). So this arm is what makes the whole
/// fire family reachable rather than an island.
///
/// # The two branches and their draws
///
/// One `nextInt(3)` decides which branch runs, and the draw counts differ:
///
/// * `passes > 0` (2 of 3 outcomes): walk up to `passes` cells, each step
///   `(nextInt(3) - 1, +1, nextInt(3) - 1)` — **two draws per step** — and stop
///   at the first air cell with a flammable neighbour, lighting it. A cell that
///   blocks motion ends the walk.
/// * `passes == 0`: three independent probes at the lava's own level,
///   `(nextInt(3) - 1, 0, nextInt(3) - 1)` — again two draws each, **six
///   total** — and each probe with air above it and a lava-ignitable block at it
///   lights the cell above. This branch does **not** stop after a success.
///
/// `ignitedByLava` is the flammability test here, and it is a *different set*
/// from fire's own ignite odds — every bed is lava-ignitable with no ignite
/// odds, every small flower the reverse. See `lodestone_data::block_blast`.
///
/// # The one reduction
///
/// Vanilla's `!level.isLoaded(testPos)` returns from the whole method. This
/// runs inside `tick_chunk`, which holds exactly one column, so a probe that
/// lands outside it is treated as unloaded and returns — which is the same
/// branch vanilla takes at a real chunk border, just reached more often. The
/// RNG draws for that probe have already happened either way, so the stream
/// stays aligned.
#[allow(clippy::too_many_arguments)]
    pub(super) fn tick_lava<Q: ScheduledTickQueueAccess<ScheduledTickKind> + ?Sized>(
        &mut self,
        column: &mut crate::chunk::ChunkColumn,
        min_x: i32,
        min_z: i32,
        x: i32,
        y: i32,
        z: i32,
        block_ticks: &mut Q,
        current_tick: u64,
    ) -> Vec<RandomTickEvent> {
        let mut events = Vec::new();
        // `ServerLevel::canSpreadFireAround` is a player-proximity test this
        // function has no players for; the tick loop gates the whole random-tick
        // pass on the tick area already being player-centred, so treating it as
        // true here matches the default 128-block radius.
        let min_y = column.min_y;
        let max_y = column.min_y + column.height;
        // Reads a cell through local coordinates, answering air for anything
        // outside this column or outside build height — the same single-accessor
        // invariant `crate::fire` and `crate::fluid` document.
        let read = |column: &crate::chunk::ChunkColumn, at: BlockPos| -> Option<String> {
            let lx = at.x - min_x;
            let lz = at.z - min_z;
            if !(0..16).contains(&lx) || !(0..16).contains(&lz) || at.y < min_y || at.y >= max_y {
                return None;
            }
            Some(column.block_state(lx, at.y, lz).to_string())
        };
        let flammable = |column: &crate::chunk::ChunkColumn, at: BlockPos| -> bool {
            read(column, at).is_some_and(|state| {
                lodestone_data::block_blast::blast_or_inert(&state).ignited_by_lava
            })
        };
        let mut light = |column: &mut crate::chunk::ChunkColumn,
                         at: BlockPos,
                         support: BlockPos,
                         events: &mut Vec<RandomTickEvent>| {
            let Some(from) = read(column, at) else { return };
            // `BaseFireBlock::getState(level, support)` — soul fire over a soul
            // base, otherwise ordinary fire with its connected faces derived from
            // `support`'s neighbourhood.
            let new_state = fire_state_in_column(column, min_x, min_z, min_y, max_y, support);
            column.set_block(at.x - min_x, at.y, at.z - min_z, &new_state);
            events.push(RandomTickEvent {
                pos: (at.x, at.y, at.z),
                from,
                to: new_state,
            });
            // Without this the fire is inert forever — see `crate::fire`.
            if !block_ticks.has_scheduled((at.x, at.y, at.z), &ScheduledTickKind::Fire) {
                block_ticks.schedule(
                    (at.x, at.y, at.z),
                    ScheduledTickKind::Fire,
                    current_tick + crate::fire::TICK_DELAY_BASE,
                    TickPriority::Normal,
                );
            }
        };

        let passes = self.behavior_rng.next_int(3);
        if passes > 0 {
            let mut test = BlockPos::new(x, y, z);
            for _ in 0..passes {
                let dx = self.behavior_rng.next_int(3) - 1;
                let dz = self.behavior_rng.next_int(3) - 1;
                test = BlockPos::new(test.x + dx, test.y + 1, test.z + dz);
                let Some(state) = read(column, test) else {
                    return events;
                };
                if is_air_variant(&state) {
                    let has_flammable_neighbour = [
                        (0, -1, 0),
                        (0, 1, 0),
                        (0, 0, -1),
                        (0, 0, 1),
                        (-1, 0, 0),
                        (1, 0, 0),
                    ]
                    .iter()
                    .any(|&(nx, ny, nz)| {
                        flammable(column, BlockPos::new(test.x + nx, test.y + ny, test.z + nz))
                    });
                    if has_flammable_neighbour {
                        light(column, test, test, &mut events);
                        return events;
                    }
                } else if lodestone_data::block_states::state_id(&state)
                    .and_then(lodestone_data::block_states::StateId::new)
                    .is_some_and(lodestone_data::block_solidity::blocks_motion)
                {
                    return events;
                }
            }
        } else {
            for _ in 0..3 {
                let dx = self.behavior_rng.next_int(3) - 1;
                let dz = self.behavior_rng.next_int(3) - 1;
                let test = BlockPos::new(x + dx, y, z + dz);
                let above = BlockPos::new(test.x, test.y + 1, test.z);
                if read(column, test).is_none() {
                    return events;
                }
                let above_is_air = read(column, above).is_some_and(|s| is_air_variant(&s));
                if above_is_air && flammable(column, test) {
                    // Vanilla passes `testPos`, not `testPos.above()`, to
                    // `BaseFireBlock::getState` here while writing to
                    // `testPos.above()`. Transcribed as written: the support cell
                    // the state is derived from really is one lower than the cell
                    // being lit.
                    light(column, above, test, &mut events);
                }
            }
        }
        events
    }

}

///
/// `crate::fire::state_at` is the [`crate::chunk::ChunkSource`] form and is what
/// the tick loop's fire drain uses; this is the same rule for the one caller that
/// holds a column instead — [`RandomTickScheduler::tick_lava`]. A cell outside the
/// column reads as air, so a fire lit at a chunk border simply has fewer connected
/// faces than vanilla would give it, which is cosmetic.
fn fire_state_in_column(
    column: &crate::chunk::ChunkColumn,
    min_x: i32,
    min_z: i32,
    min_y: i32,
    max_y: i32,
    pos: BlockPos,
) -> String {
    let read = |at: BlockPos| -> String {
        let lx = at.x - min_x;
        let lz = at.z - min_z;
        if !(0..16).contains(&lx) || !(0..16).contains(&lz) || at.y < min_y || at.y >= max_y {
            return crate::chunk::AIR.to_owned();
        }
        column.block_state(lx, at.y, lz).to_string()
    };
    let below = read(BlockPos::new(pos.x, pos.y - 1, pos.z));
    if crate::fire::SOUL_FIRE_BASE.contains(&base_name(&below)) {
        return crate::fire::SOUL_FIRE.to_owned();
    }
    if !crate::fire::can_burn(&below) && !crate::fire::face_sturdy_up(&below) {
        let up = crate::fire::can_burn(&read(BlockPos::new(pos.x, pos.y + 1, pos.z)));
        let north = crate::fire::can_burn(&read(BlockPos::new(pos.x, pos.y, pos.z - 1)));
        let south = crate::fire::can_burn(&read(BlockPos::new(pos.x, pos.y, pos.z + 1)));
        let west = crate::fire::can_burn(&read(BlockPos::new(pos.x - 1, pos.y, pos.z)));
        let east = crate::fire::can_burn(&read(BlockPos::new(pos.x + 1, pos.y, pos.z)));
        return format!(
            "{}[age=0,east={east},north={north},south={south},up={up},west={west}]",
            crate::fire::FIRE
        );
    }
    format!(
        "{}[age=0,east=false,north=false,south=false,up=false,west=false]",
        crate::fire::FIRE
    )
}
