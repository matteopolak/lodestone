//! Value-predicting gate for the whole pre-Flattening → canonical chain, and
//! the negative control that proves the gate can tell that chain apart from
//! the naive one two families still use.
//!
//! # Why this exists on top of `tests/flattening.rs`
//!
//! `tests/flattening.rs` guards the *table* (committed generated file vs the
//! committed JVM dump). It says nothing about the second half of the journey
//! — [`lodestone_canonical::canonical`]'s bridge from that table's
//! 18w-snapshot-era names to a concrete **26.2 block-state id**, which is the
//! id space `lodestone-world`'s palette consumers (mesher atlas, collision)
//! are actually built from. This file predicts that final number.
//!
//! # Where each expected value comes from
//!
//! Three anchors, joined by the code under test — deliberately not a round
//! trip through our own encoder:
//!
//! 1. **The legacy name/properties** come from `tests/support/
//!    flattening_1_13_2_jvm.txt`, a reflective dump of the real 1.13.2 server
//!    jar's own `DataFixerUpper`. This file re-reads that dump text directly
//!    (not our generated table) and asserts the expectation against it, so a
//!    regeneration that silently changed a slot fails here too.
//! 2. **The canonical 26.2 state id** is a hardcoded literal, checked against
//!    `lodestone_data::block_states` — itself generated from the 26.2 jar
//!    (`lodestone-data`). Hardcoding the number rather than
//!    re-deriving it is the point: a re-derivation would use the very reverse
//!    index `canonical::resolve` uses, and would agree with any two symmetric
//!    misunderstandings.
//! 3. **The naive value** is `(old_block_id << 4) | meta`, the packed legacy
//!    composite that `v47` and `v735` park in the palette raw today. The
//!    control below asserts it names a *different block* in 26.2 for every
//!    pair here — which is what makes the primary assertions non-vacuous.
//!
//! # What would make this vacuous
//!
//! Picking pairs where the packed value coincidentally equals the canonical
//! id, or names the same block. [`naive_packed_ids_name_a_different_block`]
//! is exactly the check that this did not happen, and it prints both sides on
//! failure.

use lodestone_canonical::canonical::{self, CanonicalBlockState};
use lodestone_canonical::flattening::{self, LegacyBlockState};
use lodestone_data::block_states;

/// The committed JVM dump — the same external anchor `tests/flattening.rs`
/// generates from, re-read here independently of the generated table.
const DUMP: &str = include_str!("support/flattening_1_13_2_jvm.txt");

/// One predicted pair. `canonical_id` is a literal read out of
/// `lodestone-data`'s 26.2 census, not re-derived at test time.
struct Predicted {
    old_block_id: u8,
    meta: u8,
    /// Name exactly as vanilla's own 1.13.2 flattening step spells it.
    legacy_name: &'static str,
    /// Properties exactly as that step emits them, sorted by key.
    legacy_properties: &'static [(&'static str, &'static str)],
    /// Predicted canonical 26.2 block-state id.
    canonical_id: u32,
    /// Predicted 26.2 name for `canonical_id` (26.2 may spell it differently
    /// from `legacy_name` — that rename is the bridge's whole job).
    canonical_name: &'static str,
    /// Predicted 26.2 properties for `canonical_id`, including any the bridge
    /// had to add (`waterlogged`, which pre-1.13 cannot express at all).
    canonical_properties: &'static [(&'static str, &'static str)],
}

/// Eight pairs spanning every shape the chain has to get right: a bare block,
/// a meta-selected variant, a meta-selected *material*, a colour, an axis
/// property, a multi-property stair that gains `waterlogged` on the way, and
/// two pairs whose old ids sit far apart in the legacy id space.
const PREDICTED: &[Predicted] = &[
    // Baseline: meta 0 of the lowest interesting id. Naive packed id 16.
    Predicted {
        old_block_id: 1,
        meta: 0,
        legacy_name: "minecraft:stone",
        legacy_properties: &[],
        canonical_id: 1,
        canonical_name: "minecraft:stone",
        canonical_properties: &[],
    },
    // Same old id, different meta -> a *different block*, not a property.
    // This is the case a formula cannot express and a table must. Naive 21.
    Predicted {
        old_block_id: 1,
        meta: 5,
        legacy_name: "minecraft:andesite",
        legacy_properties: &[],
        canonical_id: 6,
        canonical_name: "minecraft:andesite",
        canonical_properties: &[],
    },
    // Dirt variants: meta selects the material. Naive 49.
    Predicted {
        old_block_id: 3,
        meta: 1,
        legacy_name: "minecraft:coarse_dirt",
        legacy_properties: &[],
        canonical_id: 11,
        canonical_name: "minecraft:coarse_dirt",
        canonical_properties: &[],
    },
    // Planks: six woods behind one old id. Naive 83.
    Predicted {
        old_block_id: 5,
        meta: 3,
        legacy_name: "minecraft:jungle_planks",
        legacy_properties: &[],
        canonical_id: 18,
        canonical_name: "minecraft:jungle_planks",
        canonical_properties: &[],
    },
    // A log: meta carries both species *and* axis. Naive 273.
    Predicted {
        old_block_id: 17,
        meta: 1,
        legacy_name: "minecraft:spruce_log",
        legacy_properties: &[("axis", "y")],
        canonical_id: 140,
        canonical_name: "minecraft:spruce_log",
        canonical_properties: &[("axis", "y")],
    },
    // Wool: sixteen colours behind one old id, and the canonical id is nearly
    // four times the naive one. Naive 574.
    Predicted {
        old_block_id: 35,
        meta: 14,
        legacy_name: "minecraft:red_wool",
        legacy_properties: &[],
        canonical_id: 2307,
        canonical_name: "minecraft:red_wool",
        canonical_properties: &[],
    },
    // Stairs: meta is orientation, and 26.2 adds `waterlogged`, which pre-1.13
    // storage cannot carry at all. The bridge must append it. Naive 850.
    Predicted {
        old_block_id: 53,
        meta: 2,
        legacy_name: "minecraft:oak_stairs",
        legacy_properties: &[("facing", "south"), ("half", "bottom"), ("shape", "straight")],
        canonical_id: 3938,
        canonical_name: "minecraft:oak_stairs",
        canonical_properties: &[
            ("facing", "south"),
            ("half", "bottom"),
            ("shape", "straight"),
            ("waterlogged", "false"),
        ],
    },
    // A high old id (1.7-era "log2"), to reach a different region of the
    // legacy space. Naive 2593.
    Predicted {
        old_block_id: 162,
        meta: 1,
        legacy_name: "minecraft:dark_oak_log",
        legacy_properties: &[("axis", "y")],
        canonical_id: 155,
        canonical_name: "minecraft:dark_oak_log",
        canonical_properties: &[("axis", "y")],
    },
];

/// The packed legacy composite id, exactly as it appears on a pre-1.13 wire
/// and exactly what `v47`/`v735` currently store in the palette unchanged.
fn naive_packed(old_block_id: u8, meta: u8) -> u32 {
    (u32::from(old_block_id) << 4) | u32::from(meta)
}

/// Reads one slot straight out of the committed JVM dump text, bypassing the
/// generated table entirely. Returns `(name, sorted properties)`.
fn dump_slot(old_block_id: u8, meta: u8) -> Option<(String, Vec<(String, String)>)> {
    let index = usize::from(old_block_id) * 16 + usize::from(meta);
    let wanted = index.to_string();
    for line in DUMP.lines() {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let mut fields = line.split('\t');
        let n = fields.next()?;
        if n != wanted {
            continue;
        }
        let name = fields.next()?.to_owned();
        let properties = match fields.next() {
            None | Some("") => Vec::new(),
            Some(raw) => raw
                .split(',')
                .filter_map(|pair| {
                    let (key, value) = pair.split_once('=')?;
                    Some((key.to_owned(), value.to_owned()))
                })
                .collect(),
        };
        return Some((name, properties));
    }
    None
}

#[test]
fn predicted_legacy_names_match_the_committed_jvm_dump() {
    for case in PREDICTED {
        let (name, properties) = dump_slot(case.old_block_id, case.meta).unwrap_or_else(|| {
            panic!(
                "({}, {}) has no resolved line in the committed 1.13.2 JVM dump",
                case.old_block_id, case.meta
            )
        });
        assert_eq!(
            name, case.legacy_name,
            "({}, {}): the committed JVM dump disagrees with this file's prediction",
            case.old_block_id, case.meta
        );
        let expected: Vec<(String, String)> = case
            .legacy_properties
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        assert_eq!(
            properties, expected,
            "({}, {}): dump properties disagree with this file's prediction",
            case.old_block_id, case.meta
        );
    }
}

#[test]
fn flattening_lookup_matches_the_predicted_legacy_state() {
    for case in PREDICTED {
        let LegacyBlockState::Resolved(state) =
            flattening::lookup(case.old_block_id, case.meta)
        else {
            panic!(
                "({}, {}) did not resolve; the table has drifted from the dump",
                case.old_block_id, case.meta
            );
        };
        assert_eq!(state.name, case.legacy_name);
        assert_eq!(state.properties, case.legacy_properties);
    }
}

#[test]
fn canonical_resolve_matches_the_predicted_26_2_state_id() {
    for case in PREDICTED {
        let outcome = canonical::resolve(case.old_block_id, case.meta);
        let CanonicalBlockState::Resolved(state_id) = outcome else {
            panic!(
                "({}, {}) -> {outcome:?}; expected canonical state {}",
                case.old_block_id, case.meta, case.canonical_id
            );
        };
        assert_eq!(
            state_id, case.canonical_id,
            "({}, {}) resolved to 26.2 state {state_id} ({:?}), predicted {} ({})",
            case.old_block_id,
            case.meta,
            block_states::block_name(state_id),
            case.canonical_id,
            case.canonical_name,
        );
        // The literal is only meaningful if 26.2 agrees it is that block.
        assert_eq!(
            block_states::block_name(case.canonical_id),
            Some(case.canonical_name),
            "the hardcoded id {} is not {} in lodestone-data's 26.2 census; \
             re-read the census before touching this file",
            case.canonical_id,
            case.canonical_name,
        );
        assert_eq!(
            block_states::properties(case.canonical_id),
            Some(case.canonical_properties),
            "26.2 properties for state {} disagree with this file's prediction",
            case.canonical_id,
        );
    }
}

/// **The negative control.** Every assertion above is worthless if the naive
/// packed composite id happened to land on the same 26.2 state — the failure
/// mode `v47` and `v735` are in today would then be invisible. This asserts
/// the two answers are not merely different numbers but name *different
/// blocks*, and prints both sides so a future failure is diagnosable.
#[test]
fn naive_packed_ids_name_a_different_block() {
    for case in PREDICTED {
        let packed = naive_packed(case.old_block_id, case.meta);
        assert_ne!(
            packed, case.canonical_id,
            "({}, {}): packed composite equals the canonical id, so this pair \
             cannot separate the two mappings and must be replaced",
            case.old_block_id, case.meta
        );
        let naive_name = block_states::block_name(packed);
        assert_ne!(
            naive_name,
            Some(case.canonical_name),
            "({}, {}): packed id {packed} also names {} in 26.2, so this pair \
             is a coincidence and the control is vacuous",
            case.old_block_id,
            case.meta,
            case.canonical_name,
        );
        // Not an assertion — the record of what the wrong mapping actually
        // produces, so the magnitude of the defect is visible, not implied.
        println!(
            "({}, {}) canonical {} = {} | naive packed {packed} = {:?}",
            case.old_block_id,
            case.meta,
            case.canonical_id,
            case.canonical_name,
            naive_name,
        );
    }
}

/// The control's own control: the detector above compares block *names*, so it
/// would be blind if `block_name` answered `None` for every naive id. Prove it
/// does not — each naive id must resolve to a real 26.2 block.
#[test]
fn the_naive_ids_are_real_states_so_the_control_is_not_comparing_none_to_some() {
    for case in PREDICTED {
        let packed = naive_packed(case.old_block_id, case.meta);
        assert!(
            block_states::block_name(packed).is_some(),
            "({}, {}): naive packed id {packed} is not a valid 26.2 state at all, \
             so `naive_packed_ids_name_a_different_block` would pass for the wrong \
             reason on this pair",
            case.old_block_id,
            case.meta,
        );
    }
}
