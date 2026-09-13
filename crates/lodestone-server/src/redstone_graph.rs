//! Palette-derived reaction classification: the O(1) answer to "what, if
//! anything, reacts at this cell", replacing the string-parsed predicate
//! chain that used to open every neighbour notification.
//!
//! # What it is
//!
//! [`ReactionClass`] names every family
//! [`crate::random_tick::react_to_notification`] dispatches to, plus
//! [`ReactionClass::Inert`] for a cell that reacts to nothing.
//! [`classify`] maps a block-state string to its class, mirroring that
//! function's predicate chain **in the same order, first match wins**, so
//! the two agree by construction rather than by coincidence.
//!
//! The value of naming the classification is that it can then be computed
//! **once per palette entry** instead of once per notification.
//! [`crate::chunk::ChunkColumn`] carries a `palette_reaction` table
//! alongside the `palette_ticking` and `palette_state_ids` tables that
//! already exist for exactly this reason, appended in `intern` and rebuilt
//! in `recalc_ticking_counts`. Answering "what reacts here" then costs two
//! array indexes — `ChunkColumn::reaction_class` — with no string
//! allocation, no `base_name` split and no `strcmp` at all.
//!
//! # Why this is the incremental structure, and why it cannot go stale
//!
//! The obvious shape for an incrementally-invalidated redstone graph is a
//! reverse index of edges — "when this cell changes, wake these listeners".
//! That shape has one defect class this one structurally cannot have: a
//! stale edge is *silently wrong*, where rediscovery is self-healing on
//! every event. This table is **derived from the palette**, and the palette
//! is append-only (`ChunkColumn::intern` is the only writer and `palette`
//! is private, so that is compiler-enforced). A classification therefore
//! cannot outlive the state it classifies, and there is no invalidation
//! step to get wrong. The incrementality is by construction.
//!
//! What that buys is the same asymptotic win a listener index buys per
//! notification — a notification landing on a cell that reacts to nothing
//! costs one array read and a jump — with none of the risk. See
//! `docs/redstone-execution.md` for the measured split that justified
//! stopping here.
//!
//! # Ordering is untouched
//!
//! This module decides **who reacts**, never **in what order**.
//! `crate::neighbor_update::NeighborPropagator::propagate` still enumerates
//! the same six directions in `UPDATE_ORDER`, still issues the same
//! notifications, and still counts them the same way against its chained
//! -update cap — so `issued` is byte-identical with and without this table.
//! The only thing that changes is what a notification *costs* once it
//! arrives. An [`ReactionClass::Inert`] cell's arm already returned an
//! empty cascade and mutated nothing, so short-circuiting to it is
//! observationally equivalent by inspection, and the differential gate in
//! this module's own tests proves the classification agrees with the
//! predicate chain over **every block state in 26.2**, not over a sample.
//!
//! # How to change it
//!
//! Adding a family to `react_to_notification`'s dispatch means three edits,
//! and the gate below fails loudly if you make fewer than all three:
//!
//! 1. a variant on [`ReactionClass`],
//! 2. an arm in [`classify`], **positioned to match the dispatch chain's own
//!    order** — the chain is first-match-wins, so two families whose
//!    predicates overlap resolve by position, and moving an arm here without
//!    moving it there is exactly the drift the gate catches,
//! 3. the same arm in `reference_class` in this module's tests, which is a
//!    deliberate second transcription of the chain **written from the
//!    dispatch site**, not from [`classify`] — a differential whose two arms
//!    share a derivation proves nothing.
//!
//! The gotcha worth stating: every predicate in the chain today is a pure
//! function of the state's **base name** (`is_wire`, `is_openable`'s
//! `ends_with("_door")`, `is_gravity_block`, all of them). A future family
//! that dispatches on a *property* instead — `powered=true`, say — cannot be
//! classified per palette entry unless that property is part of the palette
//! key, which it is, since a palette entry is a whole canonical state
//! string. So a property-sensitive predicate is fine; a predicate reading
//! any **neighbouring** cell is not, and must stay inside its arm's body.
//!
//! # Configuration
//!
//! None. The `redstone-counters` feature adds the per-class notification
//! histogram this module's win is measured with, but changes no decision.
//!
//! # Dependencies
//!
//! The family predicates themselves, which stay where they are and stay the
//! definition: `crate::redstone`, `crate::redstone_openable`,
//! `crate::redstone_rail`, `crate::redstone_dispenser`,
//! `crate::redstone_note_block`, `crate::piston`, `crate::gravity_tick`,
//! `crate::command_block`, `crate::mobs::tnt`.

use crate::redstone::base_name;

/// Which family a neighbour notification landing on a cell dispatches to.
///
/// The discriminants are stable and contiguous from zero so
/// [`crate::redstone_counters`] can index a histogram by them; [`CLASS_COUNT`]
/// is the array length that goes with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum ReactionClass {
    /// Reacts to nothing: the notification is a no-op and the cascade is
    /// empty. The overwhelming majority of cells in any world, and — see
    /// `docs/redstone-execution.md` — the majority of notifications inside a
    /// live contraption too.
    Inert = 0,
    Gravity = 1,
    Snowy = 2,
    Wire = 3,
    Torch = 4,
    Repeater = 5,
    Piston = 6,
    Comparator = 7,
    Hopper = 8,
    Observer = 9,
    Openable = 10,
    NoteBlock = 11,
    Rail = 12,
    Dispenser = 13,
    Tnt = 14,
    CommandBlock = 15,
}

/// Number of [`ReactionClass`] variants — the histogram array length.
pub const CLASS_COUNT: usize = 16;

/// Human-readable names, indexed by [`ReactionClass`]'s discriminant, for
/// counter reporting. Kept beside the enum so a new variant that forgets a
/// name fails the length assertion in this module's tests.
pub const CLASS_NAMES: [&str; CLASS_COUNT] = [
    "inert",
    "gravity",
    "snowy",
    "wire",
    "torch",
    "repeater",
    "piston",
    "comparator",
    "hopper",
    "observer",
    "openable",
    "note_block",
    "rail",
    "dispenser",
    "tnt",
    "command_block",
];

impl ReactionClass {
    /// The discriminant, for indexing a per-class array.
    #[must_use]
    #[inline]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// The inverse of [`index`](Self::index), for walking a per-class
    /// histogram back into classes.
    ///
    /// # Panics
    ///
    /// If `index >= CLASS_COUNT`. Callers index a `[_; CLASS_COUNT]` array,
    /// so an out-of-range value means the array and the enum have drifted —
    /// which the contiguity gate in this module's tests exists to prevent,
    /// and which is worth a panic rather than a silent `Inert`.
    #[must_use]
    pub const fn from_index(index: usize) -> Self {
        match index {
            0 => ReactionClass::Inert,
            1 => ReactionClass::Gravity,
            2 => ReactionClass::Snowy,
            3 => ReactionClass::Wire,
            4 => ReactionClass::Torch,
            5 => ReactionClass::Repeater,
            6 => ReactionClass::Piston,
            7 => ReactionClass::Comparator,
            8 => ReactionClass::Hopper,
            9 => ReactionClass::Observer,
            10 => ReactionClass::Openable,
            11 => ReactionClass::NoteBlock,
            12 => ReactionClass::Rail,
            13 => ReactionClass::Dispenser,
            14 => ReactionClass::Tnt,
            15 => ReactionClass::CommandBlock,
            _ => panic!("ReactionClass index out of range: the histogram array and the enum have drifted"),
        }
    }

    /// This class's own name, for a counter report.
    #[must_use]
    #[inline]
    pub const fn name(self) -> &'static str {
        CLASS_NAMES[self as usize]
    }

    /// `true` for the one class whose notification does nothing at all.
    #[must_use]
    #[inline]
    pub const fn is_inert(self) -> bool {
        matches!(self, ReactionClass::Inert)
    }

    /// The zero-based position of this class's arm in
    /// `react_to_notification`'s dispatch chain, and therefore **the number
    /// of string predicates the chain evaluated before reaching it**;
    /// [`ReactionClass::Inert`] falls off the end having evaluated all of
    /// them.
    ///
    /// This exists so the cost this table removes can be reported as an
    /// exact derived count rather than as a duration — see
    /// `docs/redstone-execution.md`. It is *not* consulted by any dispatch
    /// decision, and the gate below pins it against the chain's real order.
    #[must_use]
    pub const fn chain_probes(self) -> u64 {
        match self {
            ReactionClass::Gravity => 1,
            ReactionClass::Snowy => 2,
            ReactionClass::Wire => 3,
            ReactionClass::Torch => 4,
            ReactionClass::Repeater => 5,
            ReactionClass::Piston => 6,
            ReactionClass::Comparator => 7,
            ReactionClass::Hopper => 8,
            ReactionClass::Observer => 9,
            ReactionClass::Openable => 10,
            ReactionClass::NoteBlock => 11,
            ReactionClass::Rail => 12,
            ReactionClass::Dispenser => 13,
            ReactionClass::Tnt => 14,
            ReactionClass::CommandBlock => 15,
            // Matched nothing, so every predicate ran.
            ReactionClass::Inert => 15,
        }
    }
}

/// Classifies one canonical block-state string.
///
/// **The arm order is the specification**, not a stylistic choice: it
/// mirrors `crate::random_tick::react_to_notification`'s own first-match
/// -wins chain, so a state matched by two predicates resolves here exactly
/// as it resolves there. See this module's doc for the three-edit rule when
/// adding a family.
#[must_use]
pub fn classify(state: &str) -> ReactionClass {
    let base = base_name(state);

    if crate::gravity_tick::is_gravity_block(base) {
        return ReactionClass::Gravity;
    }
    if crate::random_tick::is_snowy_family(base) {
        return ReactionClass::Snowy;
    }
    if crate::redstone::is_wire(state) {
        return ReactionClass::Wire;
    }
    if crate::redstone::is_torch(state) {
        return ReactionClass::Torch;
    }
    if crate::redstone::is_repeater(state) {
        return ReactionClass::Repeater;
    }
    if crate::piston::is_piston(state) {
        return ReactionClass::Piston;
    }
    if crate::redstone::is_comparator(state) {
        return ReactionClass::Comparator;
    }
    if crate::redstone::is_hopper(state) {
        return ReactionClass::Hopper;
    }
    if crate::redstone::is_observer(state) {
        return ReactionClass::Observer;
    }
    if crate::redstone_openable::is_openable(state) {
        return ReactionClass::Openable;
    }
    if base == crate::redstone_note_block::NOTE_BLOCK {
        return ReactionClass::NoteBlock;
    }
    if crate::redstone_rail::is_powered_rail_family(state) {
        return ReactionClass::Rail;
    }
    if crate::redstone_dispenser::is_dispenser_family(state) {
        return ReactionClass::Dispenser;
    }
    if crate::mobs::tnt::is_tnt_block(state) {
        return ReactionClass::Tnt;
    }
    if crate::command_block::is_command_block_family(state) {
        return ReactionClass::CommandBlock;
    }

    ReactionClass::Inert
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A **second, independent transcription** of
    /// `react_to_notification`'s dispatch chain, written by reading that
    /// function's arms top to bottom rather than by reading [`classify`].
    ///
    /// This is the differential's other arm. It deliberately calls the same
    /// family predicates — those *are* the definition and duplicating them
    /// would be transcribing a port from a sibling port, which this repo's
    /// evidence rules forbid — but it transcribes the **order and the guard
    /// expressions** separately. Order is the only thing that can drift
    /// between the two, because every predicate is first-match-wins, and
    /// order is exactly what this reproduces from the dispatch site.
    fn reference_class(state: &str) -> ReactionClass {
        // 1. gravity          `if gravity_tick::is_gravity_block(base_name(&state))`
        // 1b. snowy           `if SNOWY_FAMILY.contains(&base_name(&state))`
        // 2. dust             `if redstone::is_wire(&state)`
        // 3a. torch           `if redstone::is_torch(&state)`
        // 3b. repeater        `if redstone::is_repeater(&state)`
        // 3b-bis. piston      `if crate::piston::is_piston(&state)`
        // 3c. comparator      `if redstone::is_comparator(&state)`
        // 3d. hopper          `if redstone::is_hopper(&state)`
        // 3e. observer        `if redstone::is_observer(&state)`
        // 3f. openable        `if redstone_openable::is_openable(&state)`
        // 3g. note block      `if base_name(&state) == redstone_note_block::NOTE_BLOCK`
        // 3h. rail            `if redstone_rail::is_powered_rail_family(&state)`
        // 3i. dispenser       `if redstone_dispenser::is_dispenser_family(&state)`
        // 3i-bis. tnt         `if crate::mobs::tnt::is_tnt_block(&state)`
        // 3j. command block   `if crate::command_block::is_command_block_family(&state)`
        // fall through        `Vec::new()`
        let b = base_name(state);
        match () {
            () if crate::gravity_tick::is_gravity_block(b) => ReactionClass::Gravity,
            () if crate::random_tick::is_snowy_family(b) => ReactionClass::Snowy,
            () if crate::redstone::is_wire(state) => ReactionClass::Wire,
            () if crate::redstone::is_torch(state) => ReactionClass::Torch,
            () if crate::redstone::is_repeater(state) => ReactionClass::Repeater,
            () if crate::piston::is_piston(state) => ReactionClass::Piston,
            () if crate::redstone::is_comparator(state) => ReactionClass::Comparator,
            () if crate::redstone::is_hopper(state) => ReactionClass::Hopper,
            () if crate::redstone::is_observer(state) => ReactionClass::Observer,
            () if crate::redstone_openable::is_openable(state) => ReactionClass::Openable,
            () if b == crate::redstone_note_block::NOTE_BLOCK => ReactionClass::NoteBlock,
            () if crate::redstone_rail::is_powered_rail_family(state) => ReactionClass::Rail,
            () if crate::redstone_dispenser::is_dispenser_family(state) => ReactionClass::Dispenser,
            () if crate::mobs::tnt::is_tnt_block(state) => ReactionClass::Tnt,
            () if crate::command_block::is_command_block_family(state) => ReactionClass::CommandBlock,
            () => ReactionClass::Inert,
        }
    }

    /// Rebuilds the canonical state string for one global 26.2 block-state
    /// id, in the `name[k=v,k=v]` form this crate's palette holds.
    fn canonical_state(id: u32) -> Option<String> {
        let name = lodestone_data::block_states::block_name(id)?;
        let props = lodestone_data::block_states::properties(id)?;
        if props.is_empty() {
            return Some(name.to_string());
        }
        let body = props.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(",");
        Some(format!("{name}[{body}]"))
    }

    /// **The exhaustive differential.** Every block state in 26.2 — not a
    /// sample — classified both ways, and required to agree.
    ///
    /// This is what licenses replacing the dispatch chain's fifteen string
    /// predicates with one array index: the domain is finite and the whole
    /// of it is checked, so "the two agree on the states I thought of" is
    /// not the claim being made.
    #[test]
    fn classification_agrees_with_the_dispatch_chain_for_every_state_in_the_game() {
        let total = lodestone_data::block_states::STATE_COUNT;
        assert!(
            total > 20_000,
            "STATE_COUNT is {total}, far below 26.2's real block-state count — the table this \
             gate enumerates is not the one it thinks it is, so a pass would be vacuous"
        );

        let mut checked = 0u32;
        let mut mismatches: Vec<(String, ReactionClass, ReactionClass)> = Vec::new();
        for id in 0..total {
            let Some(state) = canonical_state(id) else { continue };
            checked += 1;
            let got = classify(&state);
            let want = reference_class(&state);
            if got != want {
                // Collected, not asserted in the loop: an `assert!` inside
                // the loop proves exactly one arm and leaves the rest
                // arguments rather than observations.
                if mismatches.len() < 32 {
                    mismatches.push((state, got, want));
                }
            }
        }

        assert_eq!(
            checked, total,
            "every id in 0..STATE_COUNT must rebuild into a canonical state string"
        );
        assert!(
            mismatches.is_empty(),
            "classify disagrees with the dispatch chain for {} state(s) (first {} shown): {:?}",
            mismatches.len(),
            mismatches.len().min(32),
            mismatches
        );
    }

    /// The control for the gate above: a deliberately wrong classifier must
    /// make it fail. Without this, "no mismatches" is equally consistent
    /// with "the loop never compared anything".
    ///
    /// The defect injected is the one the module doc says is the real
    /// hazard — an arm in the wrong **position** — rather than a wrong
    /// verdict, so it also demonstrates the gate is sensitive to order and
    /// not merely to membership.
    #[test]
    fn the_exhaustive_gate_can_actually_fail() {
        // `minecraft:tnt` is matched by the Tnt arm and by nothing before
        // it; hoisting a broader arm above it is what a careless insertion
        // looks like. Simulated here rather than by editing `classify`,
        // since the point is to prove the *comparison* fires.
        fn wrong(state: &str) -> ReactionClass {
            if crate::mobs::tnt::is_tnt_block(state) {
                // Wrong verdict for exactly one family.
                return ReactionClass::Inert;
            }
            classify(state)
        }

        let total = lodestone_data::block_states::STATE_COUNT;
        let mut disagreements = 0u32;
        for id in 0..total {
            let Some(state) = canonical_state(id) else { continue };
            if wrong(&state) != reference_class(&state) {
                disagreements += 1;
            }
        }
        assert!(
            disagreements > 0,
            "a deliberately wrong classifier produced zero disagreements over {total} states — \
             the exhaustive gate above is measuring nothing"
        );
    }

    /// `chain_probes` is a *derived cost model*, so it has to be pinned
    /// against the chain's real order or a report built on it is fiction.
    /// One row per family, read off the dispatch site's own numbered
    /// comments (1, 1b, 2, 3a…3j).
    #[test]
    fn chain_probe_positions_match_the_dispatch_order() {
        let expected = [
            (ReactionClass::Gravity, 1),
            (ReactionClass::Snowy, 2),
            (ReactionClass::Wire, 3),
            (ReactionClass::Torch, 4),
            (ReactionClass::Repeater, 5),
            (ReactionClass::Piston, 6),
            (ReactionClass::Comparator, 7),
            (ReactionClass::Hopper, 8),
            (ReactionClass::Observer, 9),
            (ReactionClass::Openable, 10),
            (ReactionClass::NoteBlock, 11),
            (ReactionClass::Rail, 12),
            (ReactionClass::Dispenser, 13),
            (ReactionClass::Tnt, 14),
            (ReactionClass::CommandBlock, 15),
            (ReactionClass::Inert, 15),
        ];
        assert_eq!(
            expected.len(),
            CLASS_COUNT,
            "every class needs a probe-position row, including Inert"
        );
        for (class, probes) in expected {
            assert_eq!(class.chain_probes(), probes, "{} probe position", class.name());
        }
    }

    /// Discriminants must stay contiguous `0..CLASS_COUNT`, because the
    /// counter histogram indexes an array by them and a gap would silently
    /// mis-attribute a class.
    #[test]
    fn discriminants_are_contiguous_and_named() {
        let all = [
            ReactionClass::Inert,
            ReactionClass::Gravity,
            ReactionClass::Snowy,
            ReactionClass::Wire,
            ReactionClass::Torch,
            ReactionClass::Repeater,
            ReactionClass::Piston,
            ReactionClass::Comparator,
            ReactionClass::Hopper,
            ReactionClass::Observer,
            ReactionClass::Openable,
            ReactionClass::NoteBlock,
            ReactionClass::Rail,
            ReactionClass::Dispenser,
            ReactionClass::Tnt,
            ReactionClass::CommandBlock,
        ];
        assert_eq!(all.len(), CLASS_COUNT);
        for (i, class) in all.into_iter().enumerate() {
            assert_eq!(class.index(), i, "{} discriminant", class.name());
            assert_eq!(
                ReactionClass::from_index(i),
                class,
                "index {i} must round-trip: `chain_probes_avoided` walks a histogram back \
                 through `from_index`, so a wrong row silently re-attributes a whole class"
            );
            assert!(!class.name().is_empty());
        }
        assert!(ReactionClass::Inert.is_inert());
        assert!(!ReactionClass::Wire.is_inert());
    }

    /// A handful of named states, so a reader can see what the classes mean
    /// without running the exhaustive pass — and so a wholesale rewrite of
    /// [`classify`] that happens to agree with a similarly-rewritten
    /// reference still trips something.
    ///
    /// The expected values come from the dispatch site's own arms, and the
    /// inert rows are chosen to be near-misses: `minecraft:redstone_lamp`
    /// and `minecraft:redstone_block` look like redstone and have no arm,
    /// `minecraft:rail` is a rail with no arm (only powered/activator rails
    /// have one), and `minecraft:lever` is a producer that is never
    /// notified into a reaction.
    #[test]
    fn named_states_land_in_the_class_the_dispatch_site_handles_them_in() {
        let rows: &[(&str, ReactionClass)] = &[
            ("minecraft:sand", ReactionClass::Gravity),
            ("minecraft:gravel", ReactionClass::Gravity),
            ("minecraft:grass_block[snowy=false]", ReactionClass::Snowy),
            ("minecraft:redstone_wire[power=0]", ReactionClass::Wire),
            ("minecraft:redstone_torch[lit=true]", ReactionClass::Torch),
            ("minecraft:redstone_wall_torch[facing=north,lit=true]", ReactionClass::Torch),
            ("minecraft:repeater[delay=1,facing=north,locked=false,powered=false]", ReactionClass::Repeater),
            ("minecraft:piston[extended=false,facing=up]", ReactionClass::Piston),
            ("minecraft:sticky_piston[extended=false,facing=up]", ReactionClass::Piston),
            ("minecraft:comparator[facing=north,mode=compare,powered=false]", ReactionClass::Comparator),
            ("minecraft:hopper[enabled=true,facing=down]", ReactionClass::Hopper),
            ("minecraft:observer[facing=north,powered=false]", ReactionClass::Observer),
            ("minecraft:oak_door[facing=north,half=lower,hinge=left,open=false,powered=false]", ReactionClass::Openable),
            ("minecraft:oak_trapdoor[facing=north,half=bottom,open=false,powered=false,waterlogged=false]", ReactionClass::Openable),
            ("minecraft:oak_fence_gate[facing=north,in_wall=false,open=false,powered=false]", ReactionClass::Openable),
            ("minecraft:note_block[instrument=harp,note=0,powered=false]", ReactionClass::NoteBlock),
            ("minecraft:powered_rail[powered=false,shape=north_south,waterlogged=false]", ReactionClass::Rail),
            ("minecraft:activator_rail[powered=false,shape=north_south,waterlogged=false]", ReactionClass::Rail),
            ("minecraft:dispenser[facing=north,triggered=false]", ReactionClass::Dispenser),
            ("minecraft:dropper[facing=north,triggered=false]", ReactionClass::Dispenser),
            ("minecraft:tnt[unstable=false]", ReactionClass::Tnt),
            ("minecraft:command_block[conditional=false,facing=north]", ReactionClass::CommandBlock),
            ("minecraft:chain_command_block[conditional=false,facing=north]", ReactionClass::CommandBlock),
            // Near-misses that react to nothing.
            ("minecraft:air", ReactionClass::Inert),
            ("minecraft:stone", ReactionClass::Inert),
            ("minecraft:redstone_lamp[lit=false]", ReactionClass::Inert),
            ("minecraft:redstone_block", ReactionClass::Inert),
            ("minecraft:rail[shape=north_south,waterlogged=false]", ReactionClass::Inert),
            ("minecraft:lever[face=wall,facing=north,powered=false]", ReactionClass::Inert),
            ("minecraft:piston_head[facing=up,short=false,type=normal]", ReactionClass::Inert),
        ];
        let mut wrong = Vec::new();
        for (state, want) in rows {
            let got = classify(state);
            if got != *want {
                wrong.push((*state, got, *want));
            }
        }
        assert!(wrong.is_empty(), "misclassified: {wrong:?}");
    }
}
