//! Redstone notification propagation and placement/removal behavior.
//!
//! This module owns the redstone family dispatch, including deterministic
//! neighbour fan-out and the resident-column view used by cross-chunk cascades.

use super::*;
#[path = "redstone_fanout.rs"]
mod fanout;
use fanout::{wire_update_centres, wire_update_fan_out};


/// Notifies the six neighbours of a just-mutated position `(x, y, z)` via
/// [`NeighborPropagator`] and dispatches every
/// reaction this crate models to a neighbour notification:
///
/// 1. **Gravity** — settles any neighbour that is an unsupported
///    gravity block; a settled block's *old* position is re-notified from
///    directly above so a stacked column collapses one at a time,
///    depth-first.
/// 2. **Redstone dust** — recomputes the neighbour's target power
///    strength (`crate::redstone_wire::calculate_target_strength`); if it
///    changed, writes the new power and re-fans-out through
///    [`wire_update_fan_out`], which is
///    `DefaultRedstoneWireEvaluator.updatePowerStrength`'s **complete**
///    update set (vanilla's own default wire evaluator), both layers.
/// 3. **Redstone torches, repeaters, comparators, and observers**
///    — schedule a delayed recheck into `block_ticks` when the neighbour's
///    steady-state condition disagrees with its current state (torch:
///    `LIT == hasSignal`; diode: `POWERED != shouldTurnOn`; observer: the
///    notification travelled from its watched face and it isn't already
///    outputting) — see each family's own module for the exact per-block
///    citation. No immediate mutation happens here: the flip itself runs
///    when `block_ticks` drains, in `tick::run_tick_loop`.
///
/// Neighbours outside this column's 16×16 footprint are skipped — the same
/// cross-chunk limitation `tick_grass_block`'s own spread already accepts.
/// [`propagate_and_react`], preceded by the reaction owed by the **placed
/// block itself**.
///
/// # Why this is a separate entry point and not a flag on the one above
///
/// `NeighborPropagator::propagate` issues notifications to the origin's six
/// *neighbours* and never to the origin — faithfully, because it models
/// `Level.updateNeighborsAt`, which does exactly that. Every existing caller of
/// [`propagate_and_react`] is a *change* whose origin has already had its say
/// (a drained scheduled tick has just run the block's own callback; a random
/// tick has just mutated it), so the omission is correct there.
///
/// A **placement** is the one case where it is not. Vanilla splits the two
/// halves across different callbacks, and the placed block's own half lives in
/// `BlockBehaviour.setPlacedBy`, called from `BlockItem.place` — nowhere near
/// the neighbour pass. Without it, placing a repeater into an already-powered
/// line does nothing at all: the fan-out notifies the dust either side, neither
/// dust changes power, no cascade reaches the repeater, and the repeater is
/// never asked whether it should turn on.
///
/// # What the jar says, per family
///
/// | family | `setPlacedBy` | modelled here |
/// |---|---|---|
/// | repeater, comparator (`DiodeBlock:160-165`) | `if (shouldTurnOn) scheduleTick(pos, this, 1)` | yes |
/// | redstone torch (`RedstoneTorchBlock`) | none — only `onPlace`'s neighbour notify | nothing to do |
/// | observer (`ObserverBlock`) | none; its `onPlace:115-123` only *cancels* a stale pulse on a block it replaced, which cannot apply to a placement into air | nothing to do |
///
/// **The delay is 1, not `getDelay(state)`, and that is not a slip.** A
/// repeater dropped into a live line lights one game tick later at *every* one
/// of its four delay settings; the `2d` delay governs signal *changes* reaching
/// an already-placed repeater, through `checkTickOnNeighbor`
/// (`DiodeBlock:88-104`), which is a different callback with a different delay.
/// `redstone_placement_gate` measures both and separates them, because reading
/// `2d` here is the single most plausible wrong model of this function.
///
/// Test-only now: `crate::server::propagate_placement`'s only production
/// caller was moved to [`propagate_placement_with_entities`], which calls
/// [`react_at_placement_with_entities`] directly rather than through this
/// `None`-only wrapper. Kept for the oracle gates and unit tests that have no
/// [`BlockEntityHandle`] to hand it.
#[cfg(test)]
pub(crate) fn react_at_placement(
    column: &mut crate::chunk::ChunkColumn,
    min_x: i32,
    min_z: i32,
    x: i32,
    y: i32,
    z: i32,
    block_ticks: &mut ScheduledTickQueue<String>,
    current_tick: u64,
) -> Vec<RandomTickEvent> {
    react_at_placement_with_entities(column, min_x, min_z, &NoNeighbors, x, y, z, block_ticks, current_tick, None)
}
/// [`react_at_placement`], plus a live [`BlockEntityHandle`] threaded into its
/// [`propagate_and_react_with_entities`] fan-out — see that function's own doc
/// for why the parameter exists and who needs it. `None` behaves exactly like
/// [`react_at_placement`] itself.
///
/// Public because the tick loop is not the only consumer: a differential
/// oracle in `crates/lodestone-fuzz` drives a contraption through this exact
/// entry point, which is the only way the placement half of a circuit can be
/// compared against a real server tick for tick.
#[allow(clippy::too_many_arguments)]
pub fn react_at_placement_with_entities<Q: ScheduledTickQueueAccess<ScheduledTickKind> + ?Sized>(
    column: &mut crate::chunk::ChunkColumn,
    min_x: i32,
    min_z: i32,
    world: &dyn ChunkSource,
    x: i32,
    y: i32,
    z: i32,
    block_ticks: &mut Q,
    current_tick: u64,
    block_entities: Option<&BlockEntityHandle>,
) -> Vec<RandomTickEvent> {
    let pos = BlockPos::new(x, y, z);
    let mut own = Vec::new();
    {
        // A placed block's own `setPlacedBy` reactions read and write through
        // the same multi-column view the neighbour-pass fan-out below uses:
        // a hopper placed one cell from a chunk seam is locked by a lever on
        // the far side of it, a placed powered rail reads a signal from
        // across it, and a tripwire run is legal up to
        // `redstone_tripwire::WIRE_DIST_MAX - 1` cells long — two and a half
        // columns — so a scan bounded to one 16x16 footprint cannot find the
        // controlling hook of most real runs. `world` decides how far that
        // reaches: a real [`ChunkSource`] reaches every resident neighbour,
        // [`NoNeighbors`] reaches none.
        let columns = RedstoneColumns::new(column, min_x, min_z, world);
        if columns.reachable(pos) {
            let state = columns.raw_state(pos);
            // Vanilla's own hopper-block on-place hook calls the same
            // `checkPoweredState` its `neighborChanged` does, so a hopper placed
            // into an already-powered cell must come up locked. The
            // neighbour pass cannot do this: it never notifies the origin.
            if redstone::is_hopper(&state) {
                let should_be_on =
                    redstone::best_neighbor_signal(&redstone::make_columns_lookup(&columns), pos, false) == 0;
                if should_be_on != redstone::hopper_enabled(&state) {
                    let new_state = redstone::with_property(&state, "enabled", if should_be_on { "true" } else { "false" });
                    columns.set_block(pos, &new_state);
                    own.push(RandomTickEvent { pos: (x, y, z), from: state.to_string(), to: new_state });
                }
            }
            let placed_kind = if redstone::is_repeater(&state) {
                let facing = redstone::diode_facing(&state);
                redstone_diode::repeater_should_turn_on(&redstone::make_columns_lookup(&columns), pos, facing)
                    .then_some(ScheduledTickKind::Repeater)
            } else if redstone::is_comparator(&state) {
                let facing = redstone::diode_facing(&state);
                let input = redstone::input_signal(&redstone::make_columns_lookup(&columns), pos, facing);
                let side = redstone::alternate_signal(&redstone::make_columns_lookup(&columns), pos, facing, false);
                let subtract = redstone::comparator_mode_subtract(&state);
                redstone_diode::comparator_should_turn_on(input, side, subtract)
                    .then_some(ScheduledTickKind::Comparator)
            } else {
                None
            };
            if let Some(kind) = placed_kind {
                if !block_ticks.has_scheduled((x, y, z), &kind) {
                    // `level.scheduleTick(pos, this, 1)` — the three-argument
                    // overload, so `TickPriority.NORMAL`.
                    block_ticks.schedule((x, y, z), kind, current_tick + 1, TickPriority::Normal);
                }
            }
            // `FireBlock::onPlace` schedules the fire's own first tick, and without
            // it a fire block is inert forever — it neither spreads nor goes out,
            // because every later tick comes from the previous one's reschedule.
            // This is the same "the placed block owes itself a reaction the
            // neighbour pass cannot deliver" case as the hopper above.
            if crate::fire::is_ordinary_fire(&state) {
                for pending in crate::fire::ticks_after_edit(pos) {
                    if !block_ticks.has_scheduled(pending.pos, &pending.kind) {
                        block_ticks.schedule(
                            pending.pos,
                            pending.kind,
                            current_tick + pending.trigger_tick,
                            pending.priority,
                        );
                    }
                }
            }
            // `BaseRailBlock.onPlace` -> `updateState` -> `level.neighborChanged(state,
            // pos, this, ...)` (vanilla's own base-rail on-place chain): a freshly placed
            // powered/activator rail notifies **itself**, the same "placed block
            // owes itself a reaction the neighbour pass cannot deliver" shape as the
            // hopper arm above. `crate::redstone_rail`'s own module doc names why
            // only `POWERED` (not `SHAPE`/connectivity) is modelled.
            if redstone_rail::is_powered_rail_family(&state) {
                let new_state = {
                    let lookup = redstone::make_columns_lookup(&columns);
                    let has_signal = |p: BlockPos| redstone::best_neighbor_signal(&lookup, p, false) > 0;
                    redstone_rail::update_state(&lookup, &has_signal, pos, &state)
                };
                if let Some(new_state) = new_state {
                    columns.set_block(pos, &new_state);
                    own.push(RandomTickEvent { pos: (x, y, z), from: state.to_string(), to: new_state });
                }
            }
            // `TripWireHookBlock.setPlacedBy` (`:104-106`) calls `calculateState`
            // directly on the just-placed hook, with no neighbour notification at
            // all — see `crate::redstone_tripwire`'s own module doc for why this
            // family lives in `react_at_placement` rather than
            // `react_to_notification`.
            if redstone::is_tripwire_hook(&state) {
                let result = {
                    let lookup = redstone::make_columns_lookup(&columns);
                    redstone_tripwire::calculate_state(&lookup, pos, &state, false, None)
                };
                apply_tripwire_result(&columns, &result, &mut own);
            }
            // `TripWireBlock.onPlace` (`:101-105`) calls `updateSource`, which
            // scans south/west for a controlling hook and recalculates *that*
            // hook's state with this wire cell as its `wireSource`.
            if base_name(&state) == redstone_tripwire::TRIPWIRE {
                let found = {
                    let lookup = redstone::make_columns_lookup(&columns);
                    redstone_tripwire::find_controlling_hooks(&lookup, pos, &state)
                };
                for (hook_pos, source) in found {
                    let hook_state = redstone::make_columns_lookup(&columns)(hook_pos);
                    if base_name(&hook_state) != redstone_tripwire::TRIPWIRE_HOOK {
                        continue;
                    }
                    let result = {
                        let lookup = redstone::make_columns_lookup(&columns);
                        redstone_tripwire::calculate_state(&lookup, hook_pos, &hook_state, false, Some(&source))
                    };
                    apply_tripwire_result(&columns, &result, &mut own);
                    if result.reschedule_recheck
                        && !block_ticks.has_scheduled(
                            (hook_pos.x, hook_pos.y, hook_pos.z),
                            &ScheduledTickKind::TripwireRecheck,
                        )
                    {
                        block_ticks.schedule(
                            (hook_pos.x, hook_pos.y, hook_pos.z),
                            ScheduledTickKind::TripwireRecheck,
                            current_tick + u64::from(redstone_tripwire::RECHECK_DELAY),
                            TickPriority::Normal,
                        );
                    }
                }
            }
        }
    }
    own.extend(propagate_and_react_with_entities_across_chunks(
        column, min_x, min_z, world, x, y, z, block_ticks, current_tick, block_entities,
    ));
    own
}

/// Applies a [`redstone_tripwire::CalculatedState`]'s write plan against
/// `columns`, skipping any position that is not reachable there — the same
/// "the write cannot happen" limit `react_to_notification`'s piston arm
/// already accepts. A cross-chunk-capable `columns` (see
/// [`propagate_and_react_with_entities_across_chunks`]) reaches an
/// already-loaded neighbour rather than stopping dead at the home column's
/// own edge, since a tripwire run can span far more than one 16×16 column.
fn apply_tripwire_result(
    columns: &RedstoneColumns<'_, '_>,
    result: &redstone_tripwire::CalculatedState,
    own: &mut Vec<RandomTickEvent>,
) {
    let mut writes: Vec<(BlockPos, String)> = Vec::new();
    if let Some(w) = &result.hook_write {
        writes.push(w.clone());
    }
    if let Some(w) = &result.receiver_write {
        writes.push(w.clone());
    }
    writes.extend(result.wire_writes.iter().cloned());

    for (pos, new_state) in writes {
        if !columns.reachable(pos) {
            continue;
        }
        let from = columns.raw_state(pos);
        if from.as_ref() == new_state {
            continue;
        }
        columns.set_block(pos, &new_state);
        own.push(RandomTickEvent {
            pos: (pos.x, pos.y, pos.z),
            from: from.to_string(),
            to: new_state,
        });
    }
}

/// The `redstone_tripwire::TICK_TRIPWIRE_RECHECK` scheduled-tick body —
/// `TripWireHookBlock.tick` (`:196-199`), which re-runs `calculate_state` with
/// no `wire_source`. `pub(crate)` for `crate::tick`'s scheduled-tick drain,
/// the same shape [`settle_gravity_at`] already has for gravity's own
/// specially-handled arm (a multi-position write plan, not a single
/// replacement state, so it cannot go through the ordinary `Option<String>`
/// dispatch chain every diode/torch/observer arm uses).
///
/// `world` extends this cross-chunk, the same way
/// [`propagate_and_react_with_entities_across_chunks`] does — an
/// already-loaded neighbour is reachable rather than truncating the recheck
/// at the home column's own edge.
pub(crate) fn run_tripwire_recheck(
    column: &mut crate::chunk::ChunkColumn,
    min_x: i32,
    min_z: i32,
    world: &dyn ChunkSource,
    pos: BlockPos,
) -> Vec<RandomTickEvent> {
    let columns = RedstoneColumns::new(column, min_x, min_z, world);
    if !columns.reachable(pos) {
        return Vec::new();
    }
    let state = columns.raw_state(pos);
    if base_name(&state) != redstone_tripwire::TRIPWIRE_HOOK {
        return Vec::new();
    }
    let result = {
        let lookup = redstone::make_columns_lookup(&columns);
        redstone_tripwire::calculate_state(&lookup, pos, &state, false, None)
    };
    let mut own = Vec::new();
    apply_tripwire_result(&columns, &result, &mut own);
    own
}

/// Vanilla's own tripwire-block affect-neighbors-after-removal routine
/// — the "the string just broke" instantaneous pulse, the block-**removal**
/// twin of the placement arm above (`redstone_tripwire::find_controlling_hooks`
/// called from a placed wire cell). `wire_state_before_removal` is the
/// tripwire's own state *just before* the caller overwrote the cell — the
/// caller must capture it first, the same as every other post-break reaction
/// in `crate::server::destroy_block` already does with its own `broken`
/// binding.
///
/// A no-op for anything that is not `minecraft:tripwire`, so a caller can call
/// this unconditionally on every removed block without a guard of its own —
/// the same shape [`redstone::is_tripwire_hook`]'s placement counterpart is
/// gated on inline rather than by the caller.
pub(crate) fn react_at_removal<Q: ScheduledTickQueueAccess<ScheduledTickKind> + ?Sized>(
    column: &mut crate::chunk::ChunkColumn,
    min_x: i32,
    min_z: i32,
    world: &dyn ChunkSource,
    x: i32,
    y: i32,
    z: i32,
    wire_state_before_removal: &str,
    block_ticks: &mut Q,
    current_tick: u64,
) -> Vec<RandomTickEvent> {
    let mut own = Vec::new();
    if base_name(wire_state_before_removal) != redstone_tripwire::TRIPWIRE {
        return own;
    }
    let pos = BlockPos::new(x, y, z);
    // The hook this scan is looking for sits up to
    // `redstone_tripwire::WIRE_DIST_MAX - 1` cells south or west of the
    // broken cell, so for most legal run lengths it is in a different chunk
    // column than the cell the player just broke — see
    // `react_at_placement_with_entities`'s own note on `world`.
    let columns = RedstoneColumns::new(column, min_x, min_z, world);
    let found = {
        let lookup = redstone::make_columns_lookup(&columns);
        redstone_tripwire::on_wire_removed(&lookup, pos, wire_state_before_removal)
    };
    for (hook_pos, source) in found {
        let hook_state = redstone::make_columns_lookup(&columns)(hook_pos);
        if base_name(&hook_state) != redstone_tripwire::TRIPWIRE_HOOK {
            continue;
        }
        let result = {
            let lookup = redstone::make_columns_lookup(&columns);
            redstone_tripwire::calculate_state(&lookup, hook_pos, &hook_state, false, Some(&source))
        };
        apply_tripwire_result(&columns, &result, &mut own);
        if result.reschedule_recheck
            && !block_ticks.has_scheduled(
                (hook_pos.x, hook_pos.y, hook_pos.z),
                &ScheduledTickKind::TripwireRecheck,
            )
        {
            block_ticks.schedule(
                (hook_pos.x, hook_pos.y, hook_pos.z),
                ScheduledTickKind::TripwireRecheck,
                current_tick + u64::from(redstone_tripwire::RECHECK_DELAY),
                TickPriority::Normal,
            );
        }
    }
    own
}

/// A cascade-scoped, multi-column read/write view over one redstone
/// notification chain (cross-chunk propagation): the home
/// column a caller already holds `&mut`, plus any *already-loaded*
/// neighbouring column a cascade actually reaches.
///
/// # Why the home column stays a plain borrow
///
/// Every existing caller — the production call sites in `crate::tick` and
/// every test in this module — holds `column: &mut ChunkColumn` across the
/// whole call and inspects or reuses it afterward. Moving it into this type
/// and handing it back would need a placeholder `ChunkColumn` for the brief
/// window in between, which this crate has no reason to invent, so `home`
/// stays a wrapped `&mut` reference instead — behind a `RefCell` only so a
/// read closure built from a shared `&RedstoneColumns` (the shape every
/// `redstone::*`/`redstone_wire::*`/… signal-computation function already
/// requires: `F: Fn(BlockPos) -> String`) can still reach it.
///
/// # The residency boundary
///
/// A neighbour chunk is reached only through
/// [`crate::chunk::ChunkSource::is_column_resident`], which answers with no
/// chunk generation at all (see its own doc comment). A position whose
/// chunk is not resident behaves exactly as every out-of-column position
/// did before this type existed: a read answers air and a write is
/// silently dropped. This is deliberate, not a shortcut — the real engine
/// has the same boundary, a circuit does not propagate into a chunk nobody
/// is simulating — drawn at the edge of loaded simulation now, rather than
/// at the arbitrary edge of one 16×16 column.
pub(crate) struct RedstoneColumns<'h, 'w> {
    home_cx: i32,
    home_cz: i32,
    home: RefCell<&'h mut crate::chunk::ChunkColumn>,
    world: &'w dyn ChunkSource,
    neighbors: RefCell<HashMap<(i32, i32), crate::chunk::ChunkColumn>>,
}

impl<'h, 'w> RedstoneColumns<'h, 'w> {
    pub(crate) fn new(
        home: &'h mut crate::chunk::ChunkColumn,
        home_min_x: i32,
        home_min_z: i32,
        world: &'w dyn ChunkSource,
    ) -> Self {
        Self {
            home_cx: home_min_x.div_euclid(16),
            home_cz: home_min_z.div_euclid(16),
            home: RefCell::new(home),
            world,
            neighbors: RefCell::new(HashMap::new()),
        }
    }

    fn key_of(pos: BlockPos) -> (i32, i32) {
        (pos.x.div_euclid(16), pos.z.div_euclid(16))
    }

    fn is_home(&self, key: (i32, i32)) -> bool {
        key == (self.home_cx, self.home_cz)
    }

    /// Ensures the column at `key` is available, fetching a neighbour
    /// (never generating one) on first reach. Returns whether `key` can be
    /// read or written at all.
    fn ensure(&self, key: (i32, i32)) -> bool {
        if self.is_home(key) {
            return true;
        }
        if self.neighbors.borrow().contains_key(&key) {
            return true;
        }
        if !self.world.is_column_resident(key.0, key.1) {
            return false;
        }
        self.neighbors.borrow_mut().insert(key, self.world.column(key.0, key.1));
        true
    }

    /// True if `pos` falls in the home column or an already-loaded,
    /// resident neighbour — the gate every call site here uses in place of
    /// the old single-column bounds check.
    pub(crate) fn reachable(&self, pos: BlockPos) -> bool {
        self.ensure(Self::key_of(pos))
    }

    /// The block-state string at `pos`, or `"minecraft:air"` for a position
    /// outside every reachable column or its build-height range — the same
    /// answer every out-of-column read gave before this type existed. Bumps
    /// `redstone_counters::bump_cell_read` on a real read: the counted half
    /// of the same split [`redstone::make_lookup`]'s own closure and the
    /// direct `ChunkColumn::block_state` calls around it already had —
    /// [`redstone::make_columns_lookup`]'s closure (this cross-chunk-aware
    /// build's replacement for every `redstone::*` signal query) calls this;
    /// a direct re-read this module performs itself (re-reading a cell just
    /// written, say) calls [`Self::raw_state`] instead, uncounted, so the
    /// counters this crate's own harness asserts byte-identical stay
    /// byte-identical for a cascade that never leaves its home column.
    pub(crate) fn state(&self, pos: BlockPos) -> crate::redstone::WorldState {
        self.read(pos, true)
    }

    /// [`Self::state`], without bumping the cell-read counter.
    pub(crate) fn raw_state(&self, pos: BlockPos) -> crate::redstone::WorldState {
        self.read(pos, false)
    }

    /// [`crate::chunk::ChunkColumn::block_state_arc`] through whichever
    /// column `pos` falls in: a clone of an already-interned palette entry
    /// (one atomic increment), never a fresh heap allocation — the
    /// cross-chunk-aware replacement for the single-column
    /// [`crate::redstone::make_lookup`]'s own `Arc` read. See
    /// `palette_arc`'s own doc comment for why this is what makes the
    /// closure cheap to call on every one of a cascade's reads.
    fn read(&self, pos: BlockPos, counted: bool) -> crate::redstone::WorldState {
        let key = Self::key_of(pos);
        if self.is_home(key) {
            let home = self.home.borrow();
            if pos.y < home.min_y || pos.y >= home.min_y + home.height {
                return crate::chunk::air_state_arc();
            }
            if counted {
                crate::redstone_counters::bump_cell_read();
            }
            return home.block_state_arc(pos.x - self.home_cx * 16, pos.y, pos.z - self.home_cz * 16);
        }
        if !self.ensure(key) {
            return crate::chunk::air_state_arc();
        }
        let neighbors = self.neighbors.borrow();
        let col = &neighbors[&key];
        if pos.y < col.min_y || pos.y >= col.min_y + col.height {
            return crate::chunk::air_state_arc();
        }
        if counted {
            crate::redstone_counters::bump_cell_read();
        }
        col.block_state_arc(pos.x - key.0 * 16, pos.y, pos.z - key.1 * 16)
    }

    /// The palette-derived reaction classification at `pos` (see
    /// [`crate::redstone_graph`]) — home and an already-loaded neighbour
    /// both read through their own palette table exactly like
    /// [`crate::chunk::ChunkColumn::reaction_class`] always did. A position
    /// that is not [`reachable`](Self::reachable) answers
    /// [`crate::redstone_graph::ReactionClass::Inert`]; callers that need to
    /// distinguish "inert" from "unloaded" must check `reachable` first, the
    /// same distinction the old bounds check drew before dispatching at
    /// all.
    pub(crate) fn reaction_class(&self, pos: BlockPos) -> crate::redstone_graph::ReactionClass {
        let key = Self::key_of(pos);
        if self.is_home(key) {
            let home = self.home.borrow();
            return home.reaction_class(pos.x - self.home_cx * 16, pos.y, pos.z - self.home_cz * 16);
        }
        if !self.ensure(key) {
            return crate::redstone_graph::ReactionClass::Inert;
        }
        let neighbors = self.neighbors.borrow();
        let col = &neighbors[&key];
        col.reaction_class(pos.x - key.0 * 16, pos.y, pos.z - key.1 * 16)
    }

    /// Writes `new_state` at `pos`, in the home column or an already-loaded
    /// resident neighbour, mutating whichever cached column `pos` falls in
    /// so a later read in the same cascade sees it. Returns `false` (and
    /// writes nothing) for a position that is not reachable, or outside its
    /// column's build-height range — the same "the write cannot happen"
    /// outcome the old bounds check produced by skipping the write.
    pub(crate) fn set_block(&self, pos: BlockPos, new_state: &str) -> bool {
        let key = Self::key_of(pos);
        if self.is_home(key) {
            let mut home = self.home.borrow_mut();
            if pos.y < home.min_y || pos.y >= home.min_y + home.height {
                return false;
            }
            home.set_block(pos.x - self.home_cx * 16, pos.y, pos.z - self.home_cz * 16, new_state);
            return true;
        }
        if !self.ensure(key) {
            return false;
        }
        let mut neighbors = self.neighbors.borrow_mut();
        let col = neighbors.get_mut(&key).expect("ensure just confirmed this key is present");
        if pos.y < col.min_y || pos.y >= col.min_y + col.height {
            return false;
        }
        col.set_block(pos.x - key.0 * 16, pos.y, pos.z - key.1 * 16, new_state);
        true
    }
}

/// A [`ChunkSource`] that reports every chunk not resident, so a
/// [`RedstoneColumns`] built over it reaches nothing outside its home
/// column.
///
/// **Test-only, and the `#[cfg(test)]` is the guard, not a convenience.**
/// Every production entry point in this module takes a `world: &dyn
/// ChunkSource` and is reached from `crate::server` or `crate::tick`, both
/// of which hold the live world; a single-column cascade is a
/// *fixture-shaped* thing, useful for a rig that has one column and wants
/// its reach bounded to it. Gating the type out of non-test builds makes
/// "no production redstone cascade is bounded to one chunk column" a
/// property the compiler checks rather than one a comment asserts.
///
/// `column`/`block_state`/`biome_state_at`/`set_block` are never called
/// (all four would panic) because [`RedstoneColumns::ensure`]
/// short-circuits on `is_column_resident` before reaching any of them.
#[cfg(test)]
struct NoNeighbors;

#[cfg(test)]
impl ChunkSource for NoNeighbors {
    fn column(&self, _cx: i32, _cz: i32) -> crate::chunk::ChunkColumn {
        unreachable!("NoNeighbors::is_column_resident is always false, so RedstoneColumns never calls this")
    }
    fn block_state(&self, _x: i32, _y: i32, _z: i32) -> String {
        unreachable!("NoNeighbors::is_column_resident is always false, so RedstoneColumns never calls this")
    }
    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        unreachable!("NoNeighbors::is_column_resident is always false, so RedstoneColumns never calls this")
    }
    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
        unreachable!("NoNeighbors::is_column_resident is always false, so RedstoneColumns never calls this")
    }
    fn is_column_resident(&self, _cx: i32, _cz: i32) -> bool {
        false
    }
}

/// [`propagate_and_react_with_entities`] with no block-entity registry.
///
/// Test-only, for the same reason [`NoNeighbors`] is: a rig that holds one
/// column and wants a cascade bounded to it.
#[cfg(test)]
pub(crate) fn propagate_and_react(
    column: &mut crate::chunk::ChunkColumn,
    min_x: i32,
    min_z: i32,
    x: i32,
    y: i32,
    z: i32,
    block_ticks: &mut ScheduledTickQueue<String>,
    current_tick: u64,
) -> Vec<RandomTickEvent> {
    propagate_and_react_with_entities(column, min_x, min_z, x, y, z, block_ticks, current_tick, None)
}

/// [`propagate_and_react_with_entities_across_chunks`] bounded to the home
/// column: the same fan-out and the same dispatch, over a
/// [`RedstoneColumns`] built on [`NoNeighbors`], so nothing outside
/// `column`'s own 16x16 footprint is read, written or notified.
///
/// Test-only. The `block_entities` handle is the registry the one family
/// whose power reaction lives in a block *entity* rather than in a
/// block-state string needs — command blocks, via
/// `crate::command_block::on_power_changed`; `None` skips that family and is
/// what a rig with no registry in scope passes.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn propagate_and_react_with_entities(
    column: &mut crate::chunk::ChunkColumn,
    min_x: i32,
    min_z: i32,
    x: i32,
    y: i32,
    z: i32,
    block_ticks: &mut ScheduledTickQueue<String>,
    current_tick: u64,
    block_entities: Option<&BlockEntityHandle>,
) -> Vec<RandomTickEvent> {
    let no_neighbors = NoNeighbors;
    let columns = RedstoneColumns::new(column, min_x, min_z, &no_neighbors);
    propagate_and_react_over(&columns, x, y, z, block_ticks, current_tick, block_entities)
}

/// The neighbour fan-out every production redstone edge runs through:
/// `world` answers [`ChunkSource::is_column_resident`] for real, so a
/// cascade that steps off the edge of `column`'s own 16x16 footprint keeps
/// going into whichever neighbour is currently resident instead of
/// truncating at an administrative boundary redstone has no concept of.
///
/// Every path a player or the world tick can drive reaches this:
/// `crate::tick`'s scheduled-tick drain (including the torch/repeater/
/// comparator reads that precede a re-propagate), target-block hits,
/// falling-block landings and the random-tick pass, plus
/// [`react_at_placement_with_entities`]/[`react_at_removal`] under
/// `crate::server`'s placement and break handlers. The residency gate is
/// the only remaining boundary, and it is the real one — a circuit does not
/// propagate into a chunk nobody is simulating (see [`RedstoneColumns`]).
#[allow(clippy::too_many_arguments)]
pub(crate) fn propagate_and_react_with_entities_across_chunks<Q: ScheduledTickQueueAccess<ScheduledTickKind> + ?Sized>(
    column: &mut crate::chunk::ChunkColumn,
    min_x: i32,
    min_z: i32,
    world: &dyn ChunkSource,
    x: i32,
    y: i32,
    z: i32,
    block_ticks: &mut Q,
    current_tick: u64,
    block_entities: Option<&BlockEntityHandle>,
) -> Vec<RandomTickEvent> {
    let columns = RedstoneColumns::new(column, min_x, min_z, world);
    propagate_and_react_over(&columns, x, y, z, block_ticks, current_tick, block_entities)
}

/// The shared core both [`propagate_and_react_with_entities`] and
/// [`propagate_and_react_with_entities_across_chunks`] reduce to: fan out
/// from `(x, y, z)` (or dust's own seven centres) through
/// [`NeighborPropagator`], dispatching every notification issued to
/// [`react_to_notification`] against `columns` — home-only or
/// cross-chunk-capable depending only on which [`ChunkSource`] `columns`
/// was built over.
#[allow(clippy::too_many_arguments)]
fn propagate_and_react_over<Q: ScheduledTickQueueAccess<ScheduledTickKind> + ?Sized>(
    columns: &RedstoneColumns<'_, '_>,
    x: i32,
    y: i32,
    z: i32,
    block_ticks: &mut Q,
    current_tick: u64,
    block_entities: Option<&BlockEntityHandle>,
) -> Vec<RandomTickEvent> {
    crate::redstone_counters::begin_drain();
    let mut events = Vec::new();
    let propagator = NeighborPropagator::default();
    let origin = BlockPos::new(x, y, z);

    // The mutated block itself decides how wide the *outermost* fan-out is.
    // Every mutation family except dust mirrors `setBlockAndUpdate`, which is
    // a single `updateNeighborsAt(pos)`; a dust power change instead runs
    // `DefaultRedstoneWireEvaluator.updatePowerStrength`'s seven-centre set —
    // and that applies to the origin exactly as it applies to a wire reached
    // mid-cascade, which an earlier version omitted.
    //
    // `raw_state`, not `state`: this mirrors the direct (uncounted)
    // `ChunkColumn::block_state` read the single-column version of this
    // function always made here — never through `redstone::make_lookup`'s
    // own closure, so never counted. A position with nothing reachable
    // there (origin somehow outside every loaded column) reads
    // `"minecraft:air"`, which is never a wire, so `origin_is_wire` is
    // `false` exactly as the old bounds check produced by construction.
    let origin_is_wire = redstone::is_wire(&columns.raw_state(origin));
    let centres = if origin_is_wire { wire_update_centres(origin) } else { vec![origin] };

    for centre in centres {
        propagator.propagate(centre, None, |n: Notification| -> Vec<Notification> {
            react_to_notification(columns, n, block_ticks, current_tick, &mut events, block_entities)
        });
    }
    crate::redstone_counters::end_drain();
    events
}

/// One neighbour notification's worth of reaction dispatch — the body of
/// [`propagate_and_react`]'s `notify` closure, named so the seven centres a
/// dust change fans out from can share it.
fn react_to_notification<Q: ScheduledTickQueueAccess<ScheduledTickKind> + ?Sized>(
    columns: &RedstoneColumns<'_, '_>,
    n: Notification,
    block_ticks: &mut Q,
    current_tick: u64,
    events: &mut Vec<RandomTickEvent>,
    block_entities: Option<&BlockEntityHandle>,
) -> Vec<Notification> {
    {
        // Reachability is not a single-column bounds check: `n.pos` may be in
        // the home column or an
        // already-loaded resident neighbour — which is [`RedstoneColumns`]'s
        // own boundary; see its doc comment for why an unloaded neighbour
        // still truncates the cascade exactly as before.
        if !columns.reachable(n.pos) {
            return Vec::new();
        }

        crate::redstone_counters::bump_notification();

        // **Which family — if any — reacts here, in two array indexes.**
        // Read from the reacting column's palette-derived classification
        // table (`crate::redstone_graph`), not by parsing the state string:
        // the classification happened once when this palette entry was
        // interned, and the palette is append-only, so it cannot be stale.
        //
        // This replaces the chain of fifteen `base_name`-plus-`strcmp`
        // family predicates each arm below used to open with. Every guard is
        // now `class == ReactionClass::X` instead of `family::is_x(&state)`;
        // the arms' bodies are untouched, and `redstone_graph`'s own gate
        // proves the two agree over **every** block state in 26.2, not over
        // a sample.
        //
        // **Ordering is unaffected.** `NeighborPropagator::propagate` still
        // enumerates and counts the same notifications in the same
        // `UPDATE_ORDER`; only what one costs on arrival — and now, whether
        // the reacting cell is in the home column or a loaded neighbour —
        // changes.
        let class = columns.reaction_class(n.pos);
        crate::redstone_counters::bump_notification_class(class);

        // The early out that carries the win: a cell reacting to nothing
        // already fell through every arm below to an empty cascade, having
        // first cloned its state string and evaluated all fifteen
        // predicates. It now costs the table read above and this branch.
        // Observationally identical by inspection — no arm ran, nothing was
        // written, the cascade was empty — which is why this is a
        // short-circuit and not a new decision.
        if class.is_inert() {
            return Vec::new();
        }

        let state = columns.raw_state(n.pos);

        // 1. Gravity — first, matching the existing precedent.
        //
        // **`FallingBlock.updateShape`, which is `scheduleTick(pos, this,
        // getDelayAfterPlace())` and nothing else.** No `isFree(below)` test here
        // and no fall: the eligibility check belongs to `FallingBlock.tick`, which
        // the scheduled tick dispatches to (`crate::tick`'s drain →
        // `settle_gravity_at`).
        //
        // This arm used to *settle inline*, which was a second fall path that
        // skipped both the 2-tick delay and — once the entity existed — the entity
        // itself. Sand whose support was removed by a neighbour mutation
        // teleported while sand placed in mid-air fell properly, from the same
        // module, for no reason a reader could see. There is now exactly one place
        // a block ever leaves the world for a fall.
        //
        // No further fan-out (empty return): `updateShape` returns
        // `super.updateShape(...)` unchanged, so nothing about the world moved and
        // there is nothing to notify.
        if class == crate::redstone_graph::ReactionClass::Gravity {
            block_ticks.schedule(
                (n.pos.x, n.pos.y, n.pos.z),
                ScheduledTickKind::Gravity,
                current_tick + gravity_tick::DELAY_AFTER_PLACE,
                TickPriority::Normal,
            );
            return Vec::new();
        }

        // 1b. `snowy` upkeep. Recompute the property when the block above changes.
        // recomputes `snowy` from the block above
        // whenever that neighbour changes, so placing or breaking snow on grass
        // flips it in both directions. Nothing did this before, so `snowy` was
        // whatever it was written as and never moved.
        //
        // Recomputed unconditionally rather than only for a from-above
        // notification: the value depends solely on the block above, so the two
        // agree, and this needs no assumption about `Notification::from`'s
        // orientation. Flag 2 in vanilla's own `setBlock` — clients told, no
        // further fan-out, hence the empty return.
        if class == crate::redstone_graph::ReactionClass::Snowy {
            let above = columns.raw_state(Direction::Up.relative(n.pos));
            let want_snowy = is_snowy_setting(&above);
            // Compared on the *value*, not on the whole string: a property-less
            // `minecraft:grass_block` already means the default, `snowy=false`,
            // so this rewrites only when the value really has to flip.
            if (property_of(&state, "snowy") == Some("true")) != want_snowy {
                let want = spreading_snowy_state(base_name(&state), &above);
                columns.set_block(n.pos, want);
                events.push(RandomTickEvent {
                    pos: (n.pos.x, n.pos.y, n.pos.z),
                    from: state.to_string(),
                    to: want.to_string(),
                });
            }
            return Vec::new();
        }

        // 2. Redstone dust.
        if class == crate::redstone_graph::ReactionClass::Wire {
            crate::redstone_counters::bump_reaction(crate::redstone_counters::ReactionKind::Dust);
            crate::redstone_counters::bump_wire_recompute();
            let new_power = redstone_wire::calculate_target_strength(&redstone::make_columns_lookup(columns), n.pos);
            let old_power = redstone::wire_power(&state);
            if new_power != old_power {
                let new_state = redstone_wire::set_power(new_power);
                columns.set_block(n.pos, &new_state);
                events.push(RandomTickEvent { pos: (n.pos.x, n.pos.y, n.pos.z), from: state.to_string(), to: new_state });
                return wire_update_fan_out(n.pos);
            }
            return Vec::new();
        }

        // 3a. Redstone torches.
        if class == crate::redstone_graph::ReactionClass::Torch {
            crate::redstone_counters::bump_reaction(crate::redstone_counters::ReactionKind::Torch);
            let has_signal = redstone_torch::has_neighbor_signal(&redstone::make_columns_lookup(columns), n.pos, &state);
            if redstone_torch::should_schedule_check(&state, has_signal) {
                if block_ticks.has_scheduled((n.pos.x, n.pos.y, n.pos.z), &ScheduledTickKind::Torch) {
                    crate::redstone_counters::bump_schedule_deduped();
                } else {
                    crate::redstone_counters::bump_schedule_requested();
                    block_ticks.schedule(
                        (n.pos.x, n.pos.y, n.pos.z),
                        ScheduledTickKind::Torch,
                        current_tick + 2,
                        TickPriority::Normal,
                    );
                }
            }
            return Vec::new();
        }

        // 3b. Repeaters.
        if class == crate::redstone_graph::ReactionClass::Repeater {
            crate::redstone_counters::bump_reaction(crate::redstone_counters::ReactionKind::Repeater);
            let facing = redstone::diode_facing(&state);
            let recomputed_lock = redstone_diode::recompute_locked(&redstone::make_columns_lookup(columns), n.pos, &state);
            if let Some(new_state) = recomputed_lock {
                columns.set_block(n.pos, &new_state);
                events.push(RandomTickEvent { pos: (n.pos.x, n.pos.y, n.pos.z), from: state.to_string(), to: new_state });
            }
            let state_now = columns.raw_state(n.pos);
            let should_on = redstone_diode::repeater_should_turn_on(&redstone::make_columns_lookup(columns), n.pos, facing);
            if redstone_diode::should_schedule_repeater_check(&state_now, should_on) {
                if block_ticks.has_scheduled((n.pos.x, n.pos.y, n.pos.z), &ScheduledTickKind::Repeater) {
                    crate::redstone_counters::bump_schedule_deduped();
                } else {
                    crate::redstone_counters::bump_schedule_requested();
                    let priority = redstone_diode::repeater_schedule_priority(
                        &redstone::make_columns_lookup(columns),
                        n.pos,
                        facing,
                        redstone::diode_powered(&state_now),
                    );
                    let delay = redstone_diode::repeater_delay(&state_now);
                    block_ticks.schedule(
                        (n.pos.x, n.pos.y, n.pos.z),
                        ScheduledTickKind::Repeater,
                        current_tick + u64::from(delay),
                        priority,
                    );
                }
            }
            return Vec::new();
        }

        // 3b-bis. Pistons. The neighbour-update dispatch calls
        // `checkIfExtend`, which is **immediate** — it fires a block event rather
        // than scheduling a tick, so the move happens in the same neighbour pass
        // that noticed the signal. That is why this arm mutates here and returns a
        // fan-out rather than scheduling.
        //
        // The signal test is `piston::has_extend_signal`, which includes
        // **quasi-connectivity** — see its own doc comment for why that is not a
        // bug to be fixed.
        //
        // **The move is two-phase.** `crate::piston::begin_move` splits
        // `apply_move`'s one-step writes into the cells that empty now and the cells
        // that hold a `moving_piston` for `PISTON_MOVE_DELAY` ticks, and each of the
        // latter schedules its own commit carrying the state it will write
        // (`piston::finish_kind`). `crate::tick`'s scheduled-tick drain runs that
        // commit, so the world two ticks from now is what the one-step path used to
        // produce immediately — and in between, a client has a `moving_piston` cell
        // and a block entity to animate.
        //
        // A pending commit can still run to completion instead of being
        // interrupted by a zero-tick pulse, and entity shoving is not modeled.
        // `crate::piston` documents both limitations.
        if class == crate::redstone_graph::ReactionClass::Piston {
            let facing = crate::piston::piston_facing(&state);
            let extended = crate::piston::piston_extended(&state);
            let want_extended =
                crate::piston::has_extend_signal(&redstone::make_columns_lookup(columns), n.pos, facing);
            if want_extended != extended {
                let sticky = crate::piston::is_sticky_piston(&state);

                // Finish a matching pending commit before applying a retraction.
                // `PistonBaseBlock.triggerEvent`'s retract branch always looks at
                // the piston's *own arm cell* (`pos.relative(direction)`, never a
                // cell further out a run may have carried a block to) for a still
                // -pending commit and forces it to finish immediately
                // (`PistonMovingBlockEntity.finalTick`) before doing anything else.
                // A `source` entity there (an extension's not-yet-placed head, or a
                // retraction's not-yet-restored base) evaporates to air instead of
                // materialising — see `crate::piston::interrupt`'s own doc comment,
                // live-verified against the 26.2 oracle in
                // `redstone_piston_order_oracle_gate.rs`. This is what a piston
                // caught mid-extend and immediately retracted needs to never show a
                // head, the specific "update-order quirk" is named for.
                //
                // `take_matching` both finds *and removes* the pending commit, so
                // the ordinary drain can never also fire it later against a cell
                // this write has already rewritten.
                let mut interrupt_fan_out = Vec::new();
                if !want_extended {
                    let arm_pos = facing.relative(n.pos);
                    if let Some(pending) = block_ticks.take_matching(
                        (arm_pos.x, arm_pos.y, arm_pos.z),
                        |kind: &ScheduledTickKind| crate::piston::is_finish_kind(kind),
                    ) {
                        if let Some(entity) = crate::piston::parse_finish_kind(&pending.kind) {
                            let write = crate::piston::interrupt(arm_pos, &entity);
                            if columns.reachable(write.pos) {
                                let from = columns.raw_state(write.pos);
                                if from.as_ref() != write.to {
                                    columns.set_block(write.pos, &write.to);
                                    events.push(RandomTickEvent {
                                        pos: (write.pos.x, write.pos.y, write.pos.z),
                                        from: from.to_string(),
                                        to: write.to.clone(),
                                    });
                                    interrupt_fan_out.push(Notification { pos: write.pos, from: Direction::Down });
                                }
                            }
                        }
                    }
                }

                // Second interrupt — sticky retraction only, and a *different*
                // cell from the arm above. `PistonBaseBlock.triggerEvent`'s
                // `isSticky` branch also inspects `pos.offset(direction * 2)`
                // (`piston::relative_n(.., 2)`, the cell this piston's own pull
                // would grab from) and, if it holds a still-**extending**
                // `moving_piston` entity travelling the *same* direction this
                // piston is retracting along, finalTicks that too — one
                // piston's retraction interrupting a different piston's
                // extension two cells away. Vanilla's own `if (!pistonPiece)`
                // guard then skips the sticky-pull decision entirely for this
                // event rather than grabbing whatever the interrupt left
                // behind, reproduced below by forcing the resolution's
                // `to_push` empty — the same reduction the plain (non-sticky)
                // retract path already uses.
                let mut sticky_pull_intercepted = false;
                if !want_extended && sticky {
                    let two_pos = crate::piston::relative_n(n.pos, facing, 2);
                    if let Some(pending) = block_ticks.take_matching(
                        (two_pos.x, two_pos.y, two_pos.z),
                        |kind: &ScheduledTickKind| {
                            crate::piston::parse_finish_kind(kind)
                                .is_some_and(|e| e.direction == facing && e.extending)
                        },
                    ) {
                        let entity = crate::piston::parse_finish_kind(&pending.kind)
                            .expect("take_matching's predicate already parsed this kind");
                        let write = crate::piston::interrupt(two_pos, &entity);
                        if columns.reachable(write.pos) {
                            let from = columns.raw_state(write.pos);
                            if from.as_ref() != write.to {
                                columns.set_block(write.pos, &write.to);
                                events.push(RandomTickEvent {
                                    pos: (write.pos.x, write.pos.y, write.pos.z),
                                    from: from.to_string(),
                                    to: write.to.clone(),
                                });
                                interrupt_fan_out.push(Notification { pos: write.pos, from: Direction::Down });
                            }
                        }
                        sticky_pull_intercepted = true;
                    }
                }

                let resolution = crate::piston::resolve(
                    &redstone::make_columns_lookup(columns),
                    n.pos,
                    facing,
                    want_extended,
                );
                // A retraction always happens (the head comes back even with
                // nothing to pull); an extension only happens if the run resolves.
                // That asymmetry is vanilla's: `checkIfExtend` gates the *extend*
                // event on `resolve()` and the contract event on nothing.
                let resolution = match (want_extended, resolution) {
                    (true, None) => return Vec::new(),
                    (_, Some(resolution)) => resolution,
                    (false, None) => crate::piston::Resolution {
                        to_push: Vec::new(),
                        to_destroy: Vec::new(),
                        push_direction: facing.opposite(),
                    },
                };
                // A sticky piston pulls; a normal one only drops its head. A
                // sticky piston whose pull target was just interrupted above
                // pulls nothing this event either — vanilla's own
                // `!pistonPiece` guard (see the interrupt's own comment).
                let resolution = if want_extended || (sticky && !sticky_pull_intercepted) {
                    resolution
                } else {
                    crate::piston::Resolution { to_push: Vec::new(), ..resolution }
                };
                let writes = crate::piston::apply_move(
                    &redstone::make_columns_lookup(columns),
                    &resolution,
                    n.pos,
                    facing,
                    want_extended,
                    sticky,
                );
                let start = crate::piston::begin_move(&writes, &state, n.pos, facing, want_extended);

                // Destinations first, then the cells the run vacated, then the
                // base's own immediate write — `apply_move`'s own order, kept
                // because it is the order that never overwrites a block still
                // waiting to move. Each entry carries the block entity to schedule,
                // or `None` for a plain write.
                let mut plan: Vec<(BlockPos, String, Option<crate::piston::MovingBlockEntity>)> =
                    Vec::with_capacity(start.moving.len() + start.cleared.len() + 1);
                for (pos, moving_state, entity) in &start.moving {
                    plan.push((*pos, moving_state.clone(), Some(entity.clone())));
                }
                for pos in &start.cleared {
                    plan.push((*pos, "minecraft:air".to_string(), None));
                }
                if let Some(base_now) = &start.base_now {
                    plan.push((n.pos, base_now.clone(), None));
                }

                // The interrupt's own notification precedes the new move's, matching
                // vanilla's order: `finalTick`'s `neighborChanged` call happens before
                // `triggerEvent` goes on to build the new move at all.
                let mut fan_out = interrupt_fan_out;
                for (pos, to, entity) in plan {
                    if !columns.reachable(pos) {
                        // Not the home column and not an already-loaded
                        // neighbour, so the `moving_piston` write cannot
                        // happen — and the commit must not be scheduled either, or a
                        // cell that never animated would still be rewritten two ticks
                        // late. Same border limit the module doc already records for
                        // every redstone reaction has: the boundary moves with
                        // the loaded columns, but remains enforced.
                        continue;
                    }
                    // A pending commit is scheduled even when the state write is a
                    // no-op: the write is idempotent (a cell already holding this
                    // exact `moving_piston` state) but the commit is what actually
                    // moves the block, so skipping it would strand the cell.
                    if let Some(entity) = &entity {
                        block_ticks.schedule(
                            (pos.x, pos.y, pos.z),
                            ScheduledTickKind::from_name(crate::piston::finish_kind(entity)),
                            current_tick + crate::piston::PISTON_MOVE_DELAY,
                            TickPriority::Normal,
                        );
                    }
                    let from = columns.raw_state(pos);
                    if from.as_ref() == to {
                        continue;
                    }
                    columns.set_block(pos, &to);
                    events.push(RandomTickEvent {
                        pos: (pos.x, pos.y, pos.z),
                        from: from.to_string(),
                        to,
                    });
                    fan_out.push(Notification { pos, from: Direction::Down });
                }
                return fan_out;
            }
            return Vec::new();
        }

        // 3c. Comparators.
        if class == crate::redstone_graph::ReactionClass::Comparator {
            crate::redstone_counters::bump_reaction(crate::redstone_counters::ReactionKind::Comparator);
            let facing = redstone::diode_facing(&state);
            let input = redstone::input_signal(&redstone::make_columns_lookup(columns), n.pos, facing);
            let side = redstone::alternate_signal(&redstone::make_columns_lookup(columns), n.pos, facing, false);
            if redstone_diode::should_schedule_comparator_check(&state, input, side) {
                if block_ticks.has_scheduled((n.pos.x, n.pos.y, n.pos.z), &ScheduledTickKind::Comparator) {
                    crate::redstone_counters::bump_schedule_deduped();
                } else {
                    crate::redstone_counters::bump_schedule_requested();
                    let priority = redstone_diode::comparator_schedule_priority(&redstone::make_columns_lookup(columns), n.pos, facing);
                    block_ticks.schedule(
                        (n.pos.x, n.pos.y, n.pos.z),
                        ScheduledTickKind::Comparator,
                        current_tick + 2,
                        priority,
                    );
                }
            }
            return Vec::new();
        }

        // 3c-bis. Hoppers. Recompute their powered state when a neighbour changes.
        // reached from `neighborChanged`
        // and `onPlace`:
        //
        //     boolean shouldBeOn = !level.hasNeighborSignal(pos);
        //     if (shouldBeOn != state.getValue(ENABLED)) {
        //        level.setBlock(pos, state.setValue(ENABLED, shouldBeOn), 2);
        //     }
        //
        // Unlike every other family in this function, a hopper's reaction is
        // **immediate, not scheduled** — vanilla writes the new state right here
        // and there is no `scheduleTick` in that method. Flag 2 is
        // `UPDATE_CLIENTS` without `UPDATE_NEIGHBORS`, so the write does not
        // fan out further; returning an empty notification list is that.
        //
        // The `enabled` property is what `BlockEntityRegistry::tick_all` reads to
        // decide whether the hopper transfers, so this is the whole lock: the
        // block state is the single source of truth, exactly as in vanilla, and
        // it is a real property of `minecraft:hopper` so the client is told
        // precisely (see `redstone::with_property`).
        if class == crate::redstone_graph::ReactionClass::Hopper {
            let should_be_on =
                redstone::best_neighbor_signal(&redstone::make_columns_lookup(columns), n.pos, false) == 0;
            if should_be_on != redstone::hopper_enabled(&state) {
                let new_state = redstone::with_property(&state, "enabled", if should_be_on { "true" } else { "false" });
                columns.set_block(n.pos, &new_state);
                events.push(RandomTickEvent { pos: (n.pos.x, n.pos.y, n.pos.z), from: state.to_string(), to: new_state });
            }
            return Vec::new();
        }

        // 3d. Observers.
        if class == crate::redstone_graph::ReactionClass::Observer {
            crate::redstone_counters::bump_reaction(crate::redstone_counters::ReactionKind::Observer);
            let watch = redstone_observer::watch_direction(&state);
            if n.from == watch && redstone_observer::should_start_signal(&state) {
                if block_ticks.has_scheduled((n.pos.x, n.pos.y, n.pos.z), &ScheduledTickKind::Observer) {
                    crate::redstone_counters::bump_schedule_deduped();
                } else {
                    crate::redstone_counters::bump_schedule_requested();
                    block_ticks.schedule(
                        (n.pos.x, n.pos.y, n.pos.z),
                        ScheduledTickKind::Observer,
                        current_tick + 2,
                        TickPriority::Normal,
                    );
                }
            }
            return Vec::new();
        }

        // 3e. Redstone-openable blocks: doors, trapdoors and fence
        // gates. `DoorBlock.neighborChanged` / `TrapDoorBlock.neighborChanged` /
        // `FenceGateBlock.neighborChanged` read whether the block is
        // redstone-powered and, when that differs from the stored `powered`,
        // write both `open` and `powered` to the new value — **immediately**,
        // with a flag-2 `setBlock` (no `scheduleTick`, no neighbour fan-out),
        // exactly like the hopper arm above rather than the delayed torch/
        // diode/observer families. See `crate::redstone_openable`'s module doc
        // for the full citation and for why the door's two-high half is synced
        // here (this crate has no `updateShape` pass for vanilla's to live in).
        if class == crate::redstone_graph::ReactionClass::Openable {
            let has_signal = redstone_openable::has_neighbor_signal(
                &redstone::make_columns_lookup(columns),
                n.pos,
                &state,
            );
            if let Some(new_state) = redstone_openable::react(&state, has_signal) {
                // Resolve the other door half before `state` is moved into
                // the event below (this function has no `updateShape`, so the
                // half-sync vanilla performs there is done right here).
                let other_half = redstone_openable::other_door_half_pos(n.pos, &state);
                columns.set_block(n.pos, &new_state);
                events.push(RandomTickEvent {
                    pos: (n.pos.x, n.pos.y, n.pos.z),
                    from: state.to_string(),
                    to: new_state,
                });
                // A door occupies two cells; both halves must flip together.
                // Vanilla keeps them in sync through `DoorBlock.updateShape`;
                // this crate has no such pass, so the same `signal` is applied
                // to the other half here. The other half is not re-notified
                // (empty cascade below), matching flag 2's no-fan-out.
                if let Some(other) = other_half {
                    if columns.reachable(other) {
                        let other_state = columns.raw_state(other);
                        if redstone_openable::is_door(&other_state) {
                            if let Some(other_new) = redstone_openable::react(&other_state, has_signal) {
                                columns.set_block(other, &other_new);
                                events.push(RandomTickEvent {
                                    pos: (other.x, other.y, other.z),
                                    from: other_state.to_string(),
                                    to: other_new,
                                });
                            }
                        }
                    }
                }
            }
            return Vec::new();
        }

        // 3f. Note blocks. Their neighbour update
        // neighbor-changed hook — immediate, like the hopper/openable arms
        // above, not scheduled. See `crate::redstone_note_block`'s own module
        // doc for the client-visible "pulse" half this crate cannot transport
        // yet (`reaction.play_pulse` is computed correctly but not consumed
        // here — there is nowhere in this event type to put it).
        if class == crate::redstone_graph::ReactionClass::NoteBlock {
            let (has_signal, above_is_air) = {
                let lookup = redstone::make_columns_lookup(columns);
                let has_signal = redstone::best_neighbor_signal(&lookup, n.pos, false) > 0;
                let above_state = lookup(Direction::Up.relative(n.pos));
                (has_signal, is_air_variant(&above_state))
            };
            if let Some(reaction) = redstone_note_block::on_neighbor_changed(&state, has_signal, above_is_air) {
                columns.set_block(n.pos, &reaction.new_state);
                events.push(RandomTickEvent {
                    pos: (n.pos.x, n.pos.y, n.pos.z),
                    from: state.to_string(),
                    to: reaction.new_state.to_string(),
                });
            }
            return Vec::new();
        }

        // 3g. Powered/activator rails (rail half — detector
        // rail's own producer is still unbuilt, see `crate::redstone_rail`'s
        // module doc). `PoweredRailBlock.updateState`, reached through
        // `BaseRailBlock.neighborChanged` (`:80-92`) since neither block
        // overrides `neighborChanged` itself.
        if class == crate::redstone_graph::ReactionClass::Rail {
            let new_state = {
                let lookup = redstone::make_columns_lookup(columns);
                let has_signal = |p: BlockPos| redstone::best_neighbor_signal(&lookup, p, false) > 0;
                redstone_rail::update_state(&lookup, &has_signal, n.pos, &state)
            };
            if let Some(new_state) = new_state {
                columns.set_block(n.pos, &new_state);
                let shape = redstone_rail::shape_of(&new_state);
                events.push(RandomTickEvent {
                    pos: (n.pos.x, n.pos.y, n.pos.z),
                    from: state.to_string(),
                    to: new_state,
                });
                if let Some(shape) = shape {
                    return redstone_rail::extra_notifications(n.pos, shape);
                }
            }
            return Vec::new();
        }

        // 3h. Dispensers/droppers — the `TRIGGERED` state
        // machine only. Vanilla's own dispenser-block neighbor-changed hook;
        // see `crate::redstone_dispenser`'s
        // own module doc for exactly why the actual fire (the scheduled tick
        // this arm schedules) has nothing to consume yet.
        if class == crate::redstone_graph::ReactionClass::Dispenser {
            let should_trigger = {
                let lookup = redstone::make_columns_lookup(columns);
                redstone::best_neighbor_signal(&lookup, n.pos, false) > 0
                    || redstone::best_neighbor_signal(&lookup, Direction::Up.relative(n.pos), false) > 0
            };
            if let Some(reaction) = redstone_dispenser::on_neighbor_changed(&state, should_trigger) {
                columns.set_block(n.pos, &reaction.new_state);
                events.push(RandomTickEvent {
                    pos: (n.pos.x, n.pos.y, n.pos.z),
                    from: state.to_string(),
                    to: reaction.new_state.to_string(),
                });
                if reaction.schedule_fire
                    && !block_ticks.has_scheduled(
                        (n.pos.x, n.pos.y, n.pos.z),
                        &ScheduledTickKind::DispenserFire,
                    )
                {
                    block_ticks.schedule(
                        (n.pos.x, n.pos.y, n.pos.z),
                        ScheduledTickKind::DispenserFire,
                        current_tick + u64::from(redstone_dispenser::TRIGGER_DURATION),
                        TickPriority::Normal,
                    );
                }
            }
            return Vec::new();
        }

        // 3i. TNT — redstone-signal ignition (vanilla's own `TntBlock::onPlace`/
        // `neighborChanged`). Unlike the dispenser
        // above there is no `TRIGGERED` state machine: vanilla primes and
        // removes the block in the same call, unconditionally, whenever
        // `hasNeighborSignal(pos)` is true. This dispatcher has no `MobSim` to
        // spawn a `PrimedTnt` into, so it schedules
        // `crate::mobs::tnt::TICK_TNT_PRIME` instead — see that constant's
        // own doc for the handoff and its one-tick cost.
        if class == crate::redstone_graph::ReactionClass::Tnt {
            let has_signal = {
                let lookup = redstone::make_columns_lookup(columns);
                redstone::best_neighbor_signal(&lookup, n.pos, false) > 0
            };
            if has_signal
                && !block_ticks.has_scheduled(
                    (n.pos.x, n.pos.y, n.pos.z),
                    &ScheduledTickKind::TntPrime,
                )
            {
                block_ticks.schedule(
                    (n.pos.x, n.pos.y, n.pos.z),
                    ScheduledTickKind::TntPrime,
                    current_tick,
                    TickPriority::Normal,
                );
            }
            return Vec::new();
        }

        // 3j. Command blocks — the redstone-edge half of the command-block
        // remainder (vanilla's own `CommandBlock.neighborChanged` →
        // `CommandBlock.setPoweredAndUpdate`). `crate::command_block::on_power_changed` was
        // written and unit-tested with no production caller until this arm;
        // see that module's own doc for the other two hops (wire decode,
        // tick-loop scheduling) that were already wired.
        //
        // Gated on `block_entities` being `Some`: `powered`/`auto`/
        // `condition_met` live on the block *entity*, not in the block-state
        // string every other arm here mutates, and most callers of
        // [`propagate_and_react`] (every oracle gate, every unit test in this
        // crate) have no registry to hand it. Those callers take the `None`
        // branch and this arm is a no-op for them.
        if let Some(block_entities) = block_entities {
            if class == crate::redstone_graph::ReactionClass::CommandBlock {
                // `hasNeighborSignal`, not `getBestOwnOrNeighbourSignal`: a
                // command block is not itself a signal source
                // (vanilla's own signal-getter default-method pair), so there is
                // no "own signal" term to fold in — just the same six-direction
                // scan the dispenser arm above already uses.
                let is_powered = {
                    let lookup = redstone::make_columns_lookup(columns);
                    redstone::best_neighbor_signal(&lookup, n.pos, false) > 0
                };
                let mode = crate::command_block::mode_for_block(&state);
                let snapshot = block_entities.with(|reg| match reg.get(n.pos) {
                    Some(crate::block_entities::BlockEntity::CommandBlock(d)) => Some(d.clone()),
                    _ => None,
                });
                if let Some(mut data) = snapshot {
                    if let Some(reaction) =
                        crate::command_block::on_power_changed(mode, data.powered, is_powered, data.auto)
                    {
                        data.powered = reaction.new_powered;
                        if reaction.schedule_execution {
                            // `markConditionMet()` — computed at the edge, not
                            // deferred to the scheduled tick: `CommandBlock.tick`'s
                            // own `REDSTONE` arm reads `wasConditionMet` as-is with
                            // no recompute, matching `CommandBlockEntity`'s own
                            // split between `markConditionMet` (called from
                            // `setPoweredAndUpdate`) and `wasConditionMet` (read
                            // later by `tick`).
                            let conditional = crate::command_block::is_conditional(&state);
                            // The predecessor read reaches an already-loaded
                            // neighbour column exactly like every other read
                            // in this function: a conditional
                            // command block whose predecessor sits in a
                            // resident neighbour column now sees it. Only a
                            // predecessor in a chunk that is not currently
                            // resident degrades to "no predecessor found"
                            // (`None`), the same boundary
                            // `apply_tripwire_result` draws for a cross-column
                            // write. Every *unconditional* command block
                            // (`is_conditional` false, the common case) never
                            // reaches this branch at all: `mark_condition_met`
                            // ignores `predecessor_succeeded` unless
                            // `conditional` is true.
                            let predecessor_succeeded = conditional.then(|| {
                                let behind = crate::command_block::facing(&state).opposite().relative(n.pos);
                                if columns.reachable(behind) {
                                    let behind_state = columns.raw_state(behind);
                                    crate::command_block::is_command_block_family(&behind_state)
                                        && block_entities.with(|reg| {
                                            matches!(
                                                reg.get(behind),
                                                Some(crate::block_entities::BlockEntity::CommandBlock(d))
                                                    if d.success_count > 0
                                            )
                                        })
                                } else {
                                    false
                                }
                            });
                            data.condition_met = crate::command_block::mark_condition_met(conditional, predecessor_succeeded);
                            if !block_ticks
                                .has_scheduled(
                                    (n.pos.x, n.pos.y, n.pos.z),
                                    &ScheduledTickKind::CommandBlock,
                                )
                            {
                                block_ticks.schedule(
                                    (n.pos.x, n.pos.y, n.pos.z),
                                    ScheduledTickKind::CommandBlock,
                                    current_tick + 1,
                                    TickPriority::Normal,
                                );
                            }
                        }
                        block_entities.with(|reg| {
                            if let Some(crate::block_entities::BlockEntity::CommandBlock(d)) = reg.get_mut(n.pos) {
                                *d = data.clone();
                            }
                        });
                    }
                }
            }
            return Vec::new();
        }

        Vec::new()
    }
}
