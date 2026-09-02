//! Neighbour-update propagation: the real engine's fixed
//! direction order plus its depth-first cascade semantics, the piece a piston
//! landing elsewhere in this crate calls "the real update-order quirks."
//!
//! # The direction order, transcribed from the real engine
//!
//! The real fixed neighbor-update direction order is, in order: west, east,
//! down, up, north, south.
//!
//! [`UPDATE_ORDER`] below is that array, verbatim.
//!
//! # The propagation shape is depth-first, not breadth-first — transcribed
//! from the real engine
//!
//! The real per-world state uses a collecting neighbor-updater, constructed
//! once with a cap on chained updates. Its run-updates step drives an explicit
//! stack, and the load-bearing part is its add-and-run step, transcribed as
//! the rule it implements:
//!
//! Check whether an update is already running. If the total-update cap has
//! not been hit: when already running, queue this update onto the current
//! layer; otherwise push it directly onto the stack. Then, only if nothing
//! was already running, drive the run-updates loop.
//!
//! A neighbour notification issued *while* another is already running
//! is queued onto the current layer and, per the run-updates loop's
//! own logic, pushed onto the **top** of the stack before the outer work
//! resumes — so any cascade a single notification triggers is fully drained
//! (including *its own* further cascades) before the direction loop that
//! issued it moves on to the next direction. Concretely: notifying `WEST`
//! and having that block's own state change cascade into notifying three
//! more neighbours means all three of those (and anything *they* cascade
//! into) run to completion **before `EAST` is ever notified** — this is not
//! "notify all six, then resolve cascades level by level" (breadth-first);
//! it is "resolve everything one notification causes before moving to the
//! next" (depth-first). [`NeighborPropagator::propagate`]'s explicit `Vec`
//! stack is that same algorithm, flattened to the one call shape this crate
//! needs (fan-out to up to six neighbours, each of which may report further
//! single-target cascades) rather than the real engine's four update
//! variants (shape/full/simple/multi), none of which this crate has a
//! second consumer for yet.
//!
//! The real chained-update cap caps total notifications per
//! `propagate` call, logging once and discarding the rest, inside
//! the real collecting updater's own add-and-run step — [`NeighborPropagator::propagate`]
//! mirrors this with `max_chained`.
//!
//! # Production status, corrected
//!
//! An earlier version of this doc comment said [`NeighborPropagator`] was
//! "exercised end to end today by `crate::random_tick`'s grass/dirt
//! conversion, which calls it ... with a currently-empty `notify` closure."
//! That was aspirational, not actual: as of the landing this
//! module shipped in, `crate::random_tick` called nothing here at all —
//! `docs/tick-scheduling.md`'s own "what this module does not yet have a
//! real producer for" section had it right, this inline comment did not.
//! Verified directly (`grep -rn "NeighborPropagator" crates/lodestone-server/src/`
//! returned only this module's own definition) before writing this
//! correction, per this repo's own "re-verify before routing around 'X
//! doesn't exist yet'" rule — the mistake here ran the other direction, a
//! claim that something *did* exist when it did not.
//!
//! **Since a later landing, it is true.** `crate::random_tick::RandomTickScheduler
//! ::tick_randomly_ticking_block` calls [`NeighborPropagator::propagate`] on
//! every position any of its four mutation families (grass↔dirt, and
//! crop growth/sapling growth/leaf decay) just changed, mirroring the real
//! "set block and update" always notifying neighbours. The `notify` closure is
//! no longer empty either: `crate::gravity_tick`'s sand/gravel settle check
//! is the one real reaction today. The redstone family is the next
//! consumer of this same call site, inheriting the depth-first ordering
//! contract unchanged — see `crate::gravity_tick`'s own module doc for the
//! full citation and the two named deviations that landing accepts.

use lodestone_model::BlockPos;

/// One of the six axis-aligned neighbour directions — mirrors
/// the real direction enum's six cardinal/vertical values, narrowed
/// to what [`UPDATE_ORDER`] needs (the real direction enum also carries
/// diagonal values used elsewhere, irrelevant here).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    Down,
    Up,
    North,
    South,
    West,
    East,
}

impl Direction {
    /// The block position one step off `pos` in this direction — the real
    /// per-direction relative-position query.
    #[must_use]
    pub fn relative(self, pos: BlockPos) -> BlockPos {
        let (dx, dy, dz) = match self {
            Direction::Down => (0, -1, 0),
            Direction::Up => (0, 1, 0),
            Direction::North => (0, 0, -1),
            Direction::South => (0, 0, 1),
            Direction::West => (-1, 0, 0),
            Direction::East => (1, 0, 0),
        };
        BlockPos::new(pos.x + dx, pos.y + dy, pos.z + dz)
    }

    /// The real get-opposite query, backed
    /// by each variant's own opposite-index field —
    /// the real direction enum's own declaration order: down/up, north/south, west/east each pair off.
    /// Needed by the redstone family (e.g. an observer's pulse
    /// travels out the face opposite the one it watches).
    #[must_use]
    pub fn opposite(self) -> Direction {
        match self {
            Direction::Down => Direction::Up,
            Direction::Up => Direction::Down,
            Direction::North => Direction::South,
            Direction::South => Direction::North,
            Direction::West => Direction::East,
            Direction::East => Direction::West,
        }
    }

    /// The real get-clockwise query — the
    /// default (Y-axis) rotation used by every horizontal-directional block,
    /// including repeaters/comparators reading their side inputs
    /// (the real diode's alternate-signal query). Defined only
    /// for the four horizontal directions, matching the real engine's own
    /// exception on `DOWN`/`UP` — callers here only ever pass a
    /// diode's `FACING`, which is always horizontal, so `Down`/`Up` are
    /// unreachable in practice; returning `self` for them (rather than
    /// panicking) keeps this a total function since nothing depends on the
    /// real engine's own defensive exception firing.
    #[must_use]
    pub fn clockwise(self) -> Direction {
        match self {
            Direction::North => Direction::East,
            Direction::East => Direction::South,
            Direction::South => Direction::West,
            Direction::West => Direction::North,
            other => other,
        }
    }

    /// The real get-counter-clockwise query
    /// — see [`clockwise`](Self::clockwise)'s doc comment for the same
    /// horizontal-only scope note.
    #[must_use]
    pub fn counterclockwise(self) -> Direction {
        match self {
            Direction::North => Direction::West,
            Direction::West => Direction::South,
            Direction::South => Direction::East,
            Direction::East => Direction::North,
            other => other,
        }
    }
}

/// The real engine's own fan-out order, verbatim:
/// **west, east, down, up, north, south**. Not alphabetical, not axis-major
/// — this exact sequence is the "real update-order quirk" a piston
/// landing elsewhere in this crate names, and every consumer of [`NeighborPropagator::propagate`] observes
/// neighbours notified in this order (modulo `skip`).
pub const UPDATE_ORDER: [Direction; 6] = [
    Direction::West,
    Direction::East,
    Direction::Down,
    Direction::Up,
    Direction::North,
    Direction::South,
];

/// One pending notification: "tell the block at `pos` that its neighbour
/// changed, as if approached from `from`" — the real neighbor-changed hook
/// carries the *source* block/orientation, not a direction into `pos`, but
/// every caller in this crate reasons in terms of "which of my six sides
/// triggered this," so `from` here is the direction from the *causing*
/// block into `pos` (i.e. `causing_pos.relative(from) == pos`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Notification {
    pub pos: BlockPos,
    pub from: Direction,
}

/// The depth-first neighbour-update propagator described in this module's
/// own doc comment.
#[derive(Debug, Clone, Copy)]
pub struct NeighborPropagator {
    /// Caps total notifications per [`propagate`](Self::propagate) call —
    /// mirrors the real collecting updater's own chained-update-cap field.
    /// `None` means unbounded (only test code should choose this; a live
    /// world always caps it, matching the real engine always constructing its
    /// updater with a finite value).
    pub max_chained: Option<usize>,
}

impl Default for NeighborPropagator {
    fn default() -> Self {
        // Vanilla's own default is large (a gamerule-configurable value in
        // the tens of thousands) — chosen here just to be "large enough
        // never to matter for a legitimate update," not to reproduce a
        // specific vanilla constant this crate has no gamerule plumbing
        // for yet.
        Self { max_chained: Some(1_000_000) }
    }
}

enum WorkItem {
    /// Still-iterating fan-out from `origin`, having issued directions
    /// `0..idx` of [`UPDATE_ORDER`] so far.
    FanOut { origin: BlockPos, skip: Option<Direction>, idx: usize },
    /// A single follow-up notification, produced as a cascade of an
    /// already-issued notification.
    Single(Notification),
}

impl NeighborPropagator {
    /// Notifies every neighbour of `origin` except `skip` (pass `None` to
    /// notify all six), in [`UPDATE_ORDER`], with the real engine's depth-first
    /// cascade semantics: `notify` is called once per notification actually
    /// issued, and any [`Notification`]s it returns are fully resolved
    /// (including their own further cascades) before the next direction in
    /// the original fan-out is issued. Mirrors
    /// the real engine's own "update neighbors at, except from facing"
    /// as the entry point, backed by
    /// the real collecting updater's stack — see this module's doc comment
    /// for the full derivation.
    ///
    /// Returns every [`Notification`] actually issued, in the exact order
    /// `notify` was called for them — the "observable behaviour" a caller
    /// or test wants to assert on, since `notify` itself is a `FnMut` and
    /// may have side effects the caller already observed as they happened.
    pub fn propagate<F>(
        &self,
        origin: BlockPos,
        skip: Option<Direction>,
        mut notify: F,
    ) -> Vec<Notification>
    where
        F: FnMut(Notification) -> Vec<Notification>,
    {
        let mut stack = vec![WorkItem::FanOut { origin, skip, idx: 0 }];
        let mut issued = Vec::new();
        let mut count: usize = 0;
        let mut capped = false;

        while let Some(top) = stack.last_mut() {
            match top {
                WorkItem::FanOut { origin, skip, idx } => {
                    if *idx >= UPDATE_ORDER.len() {
                        stack.pop();
                        continue;
                    }
                    let direction = UPDATE_ORDER[*idx];
                    *idx += 1;
                    if Some(direction) == *skip {
                        continue;
                    }
                    let notification = Notification { pos: direction.relative(*origin), from: direction };
                    if capped {
                        continue;
                    }
                    count += 1;
                    if let Some(cap) = self.max_chained {
                        if count > cap {
                            capped = true;
                            tracing::error!(
                                "Too many chained neighbor updates. Skipping the rest. First skipped position: {:?}",
                                notification.pos
                            );
                            continue;
                        }
                    }
                    issued.push(notification);
                    let cascades = notify(notification);
                    for cascade in cascades.into_iter().rev() {
                        stack.push(WorkItem::Single(cascade));
                    }
                }
                WorkItem::Single(notification) => {
                    let notification = *notification;
                    stack.pop();
                    if capped {
                        continue;
                    }
                    count += 1;
                    if let Some(cap) = self.max_chained {
                        if count > cap {
                            capped = true;
                            tracing::error!(
                                "Too many chained neighbor updates. Skipping the rest. First skipped position: {:?}",
                                notification.pos
                            );
                            continue;
                        }
                    }
                    issued.push(notification);
                    let cascades = notify(notification);
                    for cascade in cascades.into_iter().rev() {
                        stack.push(WorkItem::Single(cascade));
                    }
                }
            }
        }

        issued
    }
}

/// All six directions — the real engine's own full direction-values list
/// (its own enum declaration order: down, up, north, south,
/// west, east). Distinct from [`UPDATE_ORDER`] on purpose: this is the order
/// the real best-neighbor-signal query and a
/// handful of other real-engine loops iterate in — see `crate::redstone`'s own
/// module doc for why that particular order is irrelevant there (every use
/// is a commutative `max`, not a notify cascade), unlike [`UPDATE_ORDER`]
/// where the order *is* the observable behaviour.
pub const ALL_DIRECTIONS: [Direction; 6] = [
    Direction::Down,
    Direction::Up,
    Direction::North,
    Direction::South,
    Direction::West,
    Direction::East,
];

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(x: i32, y: i32, z: i32) -> BlockPos {
        BlockPos::new(x, y, z)
    }

    /// `getOpposite()` pins direct from the real direction enum's own
    /// opposite-index field: down/up, north/south, west/east.
    #[test]
    fn opposite_pairs_match_the_real_field() {
        assert_eq!(Direction::Down.opposite(), Direction::Up);
        assert_eq!(Direction::Up.opposite(), Direction::Down);
        assert_eq!(Direction::North.opposite(), Direction::South);
        assert_eq!(Direction::South.opposite(), Direction::North);
        assert_eq!(Direction::West.opposite(), Direction::East);
        assert_eq!(Direction::East.opposite(), Direction::West);
    }

    /// The real get-clockwise/get-counter-clockwise queries:
    /// a full clockwise loop returns to the start, and going one step
    /// clockwise then one step counter-clockwise is a no-op — a magnitude
    /// check (the whole cycle), not just "it changed".
    #[test]
    fn clockwise_cycles_through_all_four_horizontal_directions_and_back() {
        let mut d = Direction::North;
        let mut seen = vec![d];
        for _ in 0..3 {
            d = d.clockwise();
            seen.push(d);
        }
        assert_eq!(seen, vec![Direction::North, Direction::East, Direction::South, Direction::West]);
        assert_eq!(d.clockwise(), Direction::North, "the cycle must close");
    }

    #[test]
    fn clockwise_and_counterclockwise_are_inverses() {
        for d in [Direction::North, Direction::East, Direction::South, Direction::West] {
            assert_eq!(d.clockwise().counterclockwise(), d);
            assert_eq!(d.counterclockwise().clockwise(), d);
        }
    }

    /// The plain fan-out, no cascades: six calls in exactly `UPDATE_ORDER`.
    #[test]
    fn no_cascades_issues_all_six_in_update_order() {
        let prop = NeighborPropagator::default();
        let issued = prop.propagate(pos(0, 0, 0), None, |_| Vec::new());
        let dirs: Vec<Direction> = issued.iter().map(|n| n.from).collect();
        assert_eq!(
            dirs,
            vec![
                Direction::West,
                Direction::East,
                Direction::Down,
                Direction::Up,
                Direction::North,
                Direction::South,
            ]
        );
    }

    /// `skip` removes exactly one direction and leaves the other five in
    /// order — mirrors the real "update neighbors at, except from facing"
    /// hook's own skip-direction parameter.
    #[test]
    fn skip_direction_is_omitted_but_order_is_otherwise_unchanged() {
        let prop = NeighborPropagator::default();
        let issued = prop.propagate(pos(0, 0, 0), Some(Direction::Down), |_| Vec::new());
        let dirs: Vec<Direction> = issued.iter().map(|n| n.from).collect();
        assert_eq!(
            dirs,
            vec![Direction::West, Direction::East, Direction::Up, Direction::North, Direction::South]
        );
    }

    /// The core ordering claim: a cascade from the FIRST direction (`West`)
    /// must be fully resolved before the SECOND direction (`East`) is even
    /// issued — depth-first, not breadth-first. If this ran breadth-first
    /// instead, the recorded order would have `East, Down, Up, North, South`
    /// all before the cascade position; that wrong hypothesis is asserted
    /// against explicitly below, not just implied.
    #[test]
    fn a_cascade_from_the_first_direction_resolves_before_the_second_direction_is_issued() {
        let prop = NeighborPropagator::default();
        let west_target = Direction::West.relative(pos(0, 0, 0));
        let cascade_target = pos(-99, -99, -99); // a position outside the fan-out entirely

        let issued = prop.propagate(pos(0, 0, 0), None, |n| {
            if n.pos == west_target {
                vec![Notification { pos: cascade_target, from: Direction::North }]
            } else {
                Vec::new()
            }
        });

        let positions: Vec<BlockPos> = issued.iter().map(|n| n.pos).collect();
        let east_target = Direction::East.relative(pos(0, 0, 0));
        let west_idx = positions.iter().position(|&p| p == west_target).unwrap();
        let cascade_idx = positions.iter().position(|&p| p == cascade_target).unwrap();
        let east_idx = positions.iter().position(|&p| p == east_target).unwrap();

        assert!(
            west_idx < cascade_idx && cascade_idx < east_idx,
            "expected depth-first order west -> cascade -> east, got {positions:?}"
        );

        // State the rejected hypothesis explicitly: a breadth-first
        // implementation would place every one of the five other fan-out
        // directions (in particular `east`) before the cascade. Confirm
        // that is NOT what happened, as a real (not merely implied) control.
        let breadth_first_prediction_holds = east_idx < cascade_idx;
        assert!(
            !breadth_first_prediction_holds,
            "control failed: this would also pass under the wrong (breadth-first) ordering"
        );
    }

    /// A cascade that itself cascades (depth 3) must still resolve fully
    /// before the outer fan-out continues — proves the stack recurses, not
    /// just "one level of lookahead."
    #[test]
    fn cascades_of_cascades_all_resolve_before_the_next_fan_out_direction() {
        let prop = NeighborPropagator::default();
        let west_target = Direction::West.relative(pos(0, 0, 0));
        let depth2 = pos(-1, -1, -1);
        let depth3 = pos(-2, -2, -2);

        let issued = prop.propagate(pos(0, 0, 0), None, |n| {
            if n.pos == west_target {
                vec![Notification { pos: depth2, from: Direction::North }]
            } else if n.pos == depth2 {
                vec![Notification { pos: depth3, from: Direction::North }]
            } else {
                Vec::new()
            }
        });

        let positions: Vec<BlockPos> = issued.iter().map(|n| n.pos).collect();
        let east_target = Direction::East.relative(pos(0, 0, 0));
        let idx = |p: BlockPos| positions.iter().position(|&x| x == p).unwrap();
        assert!(idx(west_target) < idx(depth2));
        assert!(idx(depth2) < idx(depth3));
        assert!(idx(depth3) < idx(east_target), "the depth-3 cascade must resolve before `east` is issued");
    }

    /// `max_chained` is a hard cap, proven with a `notify` that cascades
    /// forever (an infinite chain) — without the cap this would hang.
    #[test]
    fn max_chained_stops_an_unbounded_cascade() {
        let prop = NeighborPropagator { max_chained: Some(5) };
        let issued = prop.propagate(pos(0, 0, 0), None, |n| {
            // Always cascade to one more position than we were given.
            vec![Notification { pos: pos(n.pos.x - 1, n.pos.y, n.pos.z), from: Direction::West }]
        });
        assert_eq!(issued.len(), 5, "exactly `max_chained` notifications must be issued, not more");
    }

    /// Negative control for the cap: a chain shorter than `max_chained`
    /// must NOT be truncated — proving the cap is a ceiling, not something
    /// that fires unconditionally.
    #[test]
    fn a_short_cascade_is_not_truncated_by_a_generous_cap() {
        let prop = NeighborPropagator { max_chained: Some(1_000) };
        let west_target = Direction::West.relative(pos(0, 0, 0));
        let issued = prop.propagate(pos(0, 0, 0), None, |n| {
            if n.pos == west_target {
                vec![Notification { pos: pos(-5, -5, -5), from: Direction::North }]
            } else {
                Vec::new()
            }
        });
        // 6 fan-out directions + 1 cascade = 7, nowhere near the cap.
        assert_eq!(issued.len(), 7);
    }

    /// Determinism control: two independently constructed propagators,
    /// given the same cascade script, must issue identical sequences —
    /// built as two separate `NeighborPropagator` values (not the same
    /// instance called twice) per CLAUDE.md's warning that calling one
    /// instance twice can pass by memoisation/pointer-identity rather than
    /// real determinism.
    #[test]
    fn two_independently_built_propagators_issue_identical_sequences() {
        let script = |n: Notification| -> Vec<Notification> {
            if n.from == Direction::West {
                vec![Notification { pos: pos(42, 42, 42), from: Direction::Up }]
            } else {
                Vec::new()
            }
        };
        let prop_a = NeighborPropagator::default();
        let prop_b = NeighborPropagator { max_chained: Some(1_000_000) };
        let issued_a = prop_a.propagate(pos(1, 2, 3), None, script);
        let issued_b = prop_b.propagate(pos(1, 2, 3), None, script);
        assert_eq!(issued_a, issued_b);
    }
}
