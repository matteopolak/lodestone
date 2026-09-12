//! Grass spreading is kept separate from the scheduler's position-selection loop.
//!
//! This module owns the spreading behavior and its state mutation, including the
//! exact twelve-draw attempt pattern. The parent module remains responsible for
//! selecting positions and preserving the independent position LCG.

use super::*;

/// The outcome of one [`grass_random_tick`] call, before any world mutation
/// is applied — kept separate from [`RandomTickEvent`] so the pure decision
/// (this function) stays testable with no `ChunkColumn`/`ChunkSource` in
/// scope at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrassOutcome {
    /// `canStayAlive` was false: convert to dirt. Zero further RNG draws.
    DiesToDirt,
    /// Air-exposed; all four spread attempts (12 draws total) were issued,
    /// but `try_propagate` accepted none of them.
    NoPropagationTargetAccepted,
    /// Air-exposed; the listed relative offsets (a subset of the four
    /// attempted, in attempt order) were accepted by `try_propagate` and
    /// should become grass.
    Spreads(Vec<(i32, i32, i32)>),
}

/// The pure grass ↔ dirt decision — see this module's doc comment for the
/// jar citation and the one check this crate still cannot make.
///
/// `can_stay_alive` is vanilla's `canStayAlive` verdict, which the driver
/// computes with [`grass_can_stay_alive`]. It used to be the parameter
/// `above_is_air`, a proxy that killed grass under *any* non-air block —
/// including `minecraft:short_grass`, which vanilla's own vegetation step
/// places on top of grass blocks. The predicate below preserves that distinction.
///
/// **`can_stay_alive` still doubles as the `getMaxLocalRawBrightness(pos.above())
/// >= 9` gate**, which is a *different* simplification from the one removed
/// and remains: this crate's random-tick driver holds a `ChunkColumn`, not a light
/// map, so the exact brightness is unavailable rather than approximated. The
/// consequence is that a live grass block always attempts a spread regardless of
/// time of day, never that it dies wrongly. The **draw pattern** is exact either
/// way: `0` extra draws when `canStayAlive` is false, exactly `4 * 3 = 12`
/// `next_int` calls otherwise, matching the jar's unconditional `for` loop.
///
/// `try_propagate` is called for each of the four attempts' relative
/// `(dx, dy, dz)` offset (already drawn from `rng` before the call, exactly like
/// vanilla's `pos.offset(random.nextInt(3) - 1, ...)`) and must itself decide
/// whether the target position is a valid spread destination — a `ChunkColumn`
/// lookup this pure function does not perform, so it stays testable with a fake
/// world.
pub fn grass_random_tick(
    can_stay_alive: bool,
    rng: &mut SpawnRng,
    mut try_propagate: impl FnMut(i32, i32, i32) -> bool,
) -> GrassOutcome {
    if !can_stay_alive {
        return GrassOutcome::DiesToDirt;
    }
    let mut spreads = Vec::new();
    for _ in 0..4 {
        let dx = rng.next_int(3) - 1;
        let dy = rng.next_int(5) - 3;
        let dz = rng.next_int(3) - 1;
        if try_propagate(dx, dy, dz) {
            spreads.push((dx, dy, dz));
        }
    }
    if spreads.is_empty() {
        GrassOutcome::NoPropagationTargetAccepted
    } else {
        GrassOutcome::Spreads(spreads)
    }
}

/// `true` iff a dirt block at the target offset can become grass — the real
/// can-propagate check: the target must be able to stay alive as grass, **and**
/// the fluid state directly above the target must not be tagged as water at
/// all.
///
/// So two conditions on the block above the *target*, not one, and the second is
/// not implied by the first: the can-stay-alive check rejects a **full** fluid, while
/// the can-propagate check
/// additionally rejects any water fluid at all — flowing water
/// included. Grass therefore does not spread into a shallow stream it *could*
/// survive under. Plus this crate's own precondition that the target is dirt,
/// which is the real "is the target still the base block" check at the call
/// site.
///
/// The earlier condition was `is_air_variant(above_target_state)`, which
/// collapsed both conditions into the same proxy [`grass_can_stay_alive`]
/// documents.
#[must_use]
pub fn can_propagate_onto(target_state: &str, above_target_state: &str) -> bool {
    base_name(target_state) == DIRT_BLOCK
        && grass_can_stay_alive(above_target_state)
        && base_name(above_target_state) != "minecraft:water"
        && property_of(above_target_state, "waterlogged") != Some("true")
}

impl RandomTickScheduler {
    /// Applies [`grass_random_tick`] at world position `(x, y, z)` against
    /// `column`, mutating it and returning every resulting event: at most
    /// one self-conversion (dies-to-dirt) OR up to four spread events (one
    /// per accepted propagation target).
    pub(super) fn tick_grass_block(
        &mut self,
        column: &mut crate::chunk::ChunkColumn,
        min_x: i32,
        min_z: i32,
        x: i32,
        y: i32,
        z: i32,
        current_state: &str,
    ) -> Vec<RandomTickEvent> {
        let lx = x - min_x;
        let lz = z - min_z;
        let above = column.block_state(lx, y + 1, lz).to_string();
        // The light-dampening predicate, rather than the old
        // `is_air_variant` proxy. The proxy killed grass under *any* non-air
        // block, and vanilla's own vegetation step puts `minecraft:short_grass`
        // on top of grass blocks — so every decorated patch turned to dirt on
        // its first random tick.
        let can_stay_alive = grass_can_stay_alive(&above);

        // `try_propagate` only reads `column` (via the immutable reborrow
        // below) — no mutation happens until after `grass_random_tick`
        // returns and this borrow ends, so the two phases (decide, then
        // apply) never overlap.
        let outcome = {
            let column_ref: &crate::chunk::ChunkColumn = column;
            grass_random_tick(can_stay_alive, &mut self.behavior_rng, |dx, dy, dz| {
                let tx = x + dx;
                let tz = z + dz;
                let tlx = tx - min_x;
                let tlz = tz - min_z;
                if !(0..16).contains(&tlx) || !(0..16).contains(&tlz) {
                    // Cross-chunk propagation target: this crate has no
                    // neighbour-column access from inside `tick_chunk` (only
                    // the one column being ticked is in scope) — treated as
                    // "not a valid target," matching vanilla's own
                    // `canPropagate` returning false for anything that fails
                    // its checks. The RNG draw for this attempt still
                    // happened (see `grass_random_tick`), only the mutation
                    // is skipped.
                    return false;
                }
                let ty = y + dy;
                // `block_state` takes LOCAL x/z: passing the
                // absolute `tz` here tripped `ChunkColumn::index`'s
                // `debug_assert` on every singleplayer session, and in
                // release silently aliased onto a different cell — the
                // index is `((y_local * 16 + z) * 16 + x)`, so an absolute
                // `z` of `min_z + tlz` reads local z `tlz` at a y-level
                // `min_z / 16` sections higher. Invisible at chunk (0, 0),
                // where the two coordinates coincide.
                let target_state = column_ref.block_state(tlx, ty, tlz);
                let above_target = column_ref.block_state(tlx, ty + 1, tlz);
                can_propagate_onto(target_state, above_target)
            })
        };

        let mut events = Vec::new();
        match outcome {
            GrassOutcome::DiesToDirt => {
                column.set_block(lx, y, lz, DIRT_BLOCK);
                events.push(RandomTickEvent {
                    pos: (x, y, z),
                    from: current_state.to_string(),
                    to: DIRT_BLOCK.to_string(),
                });
            }
            GrassOutcome::NoPropagationTargetAccepted => {}
            GrassOutcome::Spreads(offsets) => {
                for (dx, dy, dz) in offsets {
                    let tx = x + dx;
                    let ty = y + dy;
                    let tz = z + dz;
                    let tlx = tx - min_x;
                    let tlz = tz - min_z;
                    let above_target = column.block_state(tlx, ty + 1, tlz).to_string();
                    let spread_state = spreading_snowy_state(GRASS_BLOCK, &above_target);
                    column.set_block(tlx, ty, tlz, spread_state);
                    events.push(RandomTickEvent {
                        pos: (tx, ty, tz),
                        from: DIRT_BLOCK.to_string(),
                        to: spread_state.to_string(),
                    });
                }
            }
        }
        events
    }
}
