// Shared by two test targets — `redstone_contraption_ticks` and
// `differential_live_redstone_contraption` — and each compiles this module
// separately, so whatever one of them does not call reads as dead there. The
// probe alphabet is the live file's; the layout is both files'.
#![allow(dead_code)]
//! The two-seam redstone contraption both the in-process gate and the live
//! differential are laid out from.
//!
//! Shared rather than duplicated because the whole value of the pair is that
//! it is the *same* circuit: the in-process gate pins our side's exact ticks
//! against numbers measured on a real server, and the live comparison is what
//! measured them. Two copies of the layout would let one drift and turn a
//! disagreement about the model into a disagreement about the rig.
//!
//! # The circuit
//!
//! One row along `+x` on a stone floor, in relative coordinates:
//!
//! | cell | block |
//! |---|---|
//! | 0 | `redstone_block` — the source, and the script's only action |
//! | 1..=3 | dust |
//! | 4 | repeater, `delay=1` |
//! | 5..=7 | dust |
//! | 8 | repeater, `delay=4` |
//! | 9..=16 | dust |
//! | 17 | repeater, `delay=2` |
//! | 18..=19 | dust |
//!
//! A repeater's `facing` names the side its **input** comes from, so a signal
//! travelling `+x` (east) runs through repeaters facing west. Placement in
//! game sets it the same way round — the property is the opposite of the
//! placing player's look direction — so one state string drives both sides.
//!
//! # Why the origin's `x` is congruent to 15
//!
//! So that the row crosses a chunk seam **twice**, and crosses the first one
//! on its very first hop: cell 0 is the last column of one chunk, cell 1 the
//! first of the next, and cell 17 the first of a third. A model whose
//! invalidation stops at a column boundary therefore reads zero at *every*
//! probe below rather than at some late one, which is the failure the
//! cross-seam work exists to rule out.
//!
//! # Why three delay settings and not one
//!
//! A repeater's delay is `2 · delay` game ticks. With three different
//! settings the arrival times are 2, 10 and 14 ticks after the source — a
//! model reading the property as `delay` game ticks instead lands on 1, 5 and
//! 7, and one reading the flat one-tick on-place delay lands on 1, 2 and 3.
//! No two of those three timelines share a single arrival, so any probe below
//! separates them. One repeater at one setting would not: `delay=1` gives 2
//! ticks under the right model and 1 under the wrong one, a difference a
//! real-time-aligned oracle cannot resolve.

/// Far from spawn and above the terrain height of every oracle world, and
/// disjoint from the fluid rig's own coordinates so both can run against one
/// live server. `x % 16 == 15` is the load-bearing part — see the module doc.
pub const ORIGIN: (i32, i32, i32) = (5311, 120, 5264);

/// [`ORIGIN`] shifted onto its own `z` lane, so two tests driving one live
/// world do not share a single set of block positions.
///
/// **This is not tidiness, it is a measured requirement.** Run in one process
/// against one world at one origin, three tests that each build the rig,
/// energise it and tear it back down to air interfere: the third measured the
/// far cell arriving 3 game ticks after the source, where the same test alone
/// measures 14 — reproducibly, three trials each way. Tearing a circuit down
/// to air does not retract the block ticks its components had already
/// scheduled at those coordinates, so a rebuild drops fresh components onto a
/// queue that still has stale entries for them and the chain short-circuits.
///
/// Four blocks apart is enough: the rig is one row with a floor a block
/// below, so lanes at `z ± 4` share no position and no neighbour, and
/// redstone reaches one cell.
#[must_use]
pub fn origin_on_lane(lane: i32) -> (i32, i32, i32) {
    (ORIGIN.0, ORIGIN.1, ORIGIN.2 + lane * 4)
}

/// The row's own `y`, relative to [`ORIGIN`]. The floor is one below.
pub const ROW_Y: i32 = 0;
pub const FLOOR_Y: i32 = -1;
pub const FLOOR_STATE: &str = "minecraft:stone";

/// The source, and the differential script's only action.
pub const SOURCE: (i32, i32, i32) = (0, ROW_Y, 0);
pub const SOURCE_STATE: &str = "minecraft:redstone_block";

/// The last cell of the row.
pub const LAST_CELL: i32 = 19;

/// The cells holding a repeater, in the same order [`components`] lays them
/// out. Named because a live rig has to be checked *quiescent* before it is
/// energised, and a repeater's `powered` property is the only thing that
/// reports whether the row is still carrying a pulse.
pub const REPEATER_CELLS: [i32; 3] = [4, 8, 17];

fn dust() -> String {
    "minecraft:redstone_wire[power=0]".to_owned()
}

fn repeater(delay: u32) -> String {
    format!("minecraft:repeater[facing=west,delay={delay},locked=false,powered=false]")
}

/// Every block of the contraption except the source, in relative
/// coordinates. Laid out *before* the script runs, on both sides.
pub fn components() -> Vec<((i32, i32, i32), String)> {
    let mut out = Vec::new();
    for x in 1..=LAST_CELL {
        let state = match x {
            4 => repeater(1),
            8 => repeater(4),
            17 => repeater(2),
            _ => dust(),
        };
        out.push(((x, ROW_Y, 0), state));
    }
    out
}

/// The three probed cells: position, the dust power predicted there once the
/// signal arrives, and the exact game tick after the source is placed on
/// which our side writes it.
///
/// * cell 1 — one hop past the first seam, fed directly by the source
/// * cell 16 — the last cell before the second seam, seven cells of decay
///   past the second repeater's output
/// * cell 18 — one hop past the second seam, fed by the third repeater
pub const PREDICTED: [((i32, i32, i32), u8, u64); 3] = [
    ((1, ROW_Y, 0), 15, 0),
    ((16, ROW_Y, 0), 8, 10),
    ((18, ROW_Y, 0), 15, 14),
];

/// The probe alphabet for one predicted cell: the predicted state, and an
/// unpowered one.
///
/// Two candidates and no more, deliberately. The alphabet **is** the
/// prediction: a side that writes any other power matches neither and answers
/// `None`, which the comparison reports at that position rather than
/// silently accepting as "some power arrived". Enumerating all sixteen powers
/// instead would turn a wrong value into a matched value.
pub fn candidates(power: u8) -> Vec<String> {
    vec![
        format!("minecraft:redstone_wire[power={power}]"),
        "minecraft:redstone_wire[power=0]".to_owned(),
    ]
}

/// The comparison region: the three predicted cells, each with its own
/// two-candidate alphabet.
pub fn region() -> Vec<((i32, i32, i32), Vec<String>)> {
    PREDICTED
        .iter()
        .map(|&(pos, power, _)| (pos, candidates(power)))
        .collect()
}
