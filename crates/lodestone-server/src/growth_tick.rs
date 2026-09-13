//! Crop growth, sapling growth, and leaf decay — the random-tick
//! half of the family `crate::random_tick`'s grass ↔ dirt conversion already
//! templates. `crate::random_tick::RandomTickScheduler::tick_chunk` dispatches
//! into this module's functions for any picked position whose block matches
//! one of the predicates below; see that function's own dispatch for how a
//! position that already exists in this crate's `ChunkColumn` reaches this
//! module with zero further plumbing.
//!
//! # Why this module exists separately from `random_tick.rs`
//!
//! `random_tick.rs` owns the *selection* machinery (the position LCG, the
//! per-section eligibility scan, the two-generator split) that is genuinely
//! shared across every randomly-ticking block. This module owns the
//! *per-block-family* decision logic — three families, each transcribed from
//! the real class that implements it — kept apart from the selection
//! machinery the same way the real crop, sapling and leaves blocks are
//! separate classes sharing one random-tick call site.
//!
//! # Crop growth, transcribed from the real crop block
//!
//! The real crop's random tick, transcribed as the rule it implements: if
//! the raw brightness at this position is at least 9, and this crop's age is
//! below its own maximum, draw once with a bound of `(25.0 / growth_speed)
//! as i32 + 1` and, on a hit (`0`), advance the crop to the next age.
//!
//! The light check wraps the **entire** body, including the RNG draw — a
//! crop with insufficient light draws **zero** times, not "draws and always
//! misses." [`crop_random_tick`]'s `above_is_air` proxy stands in for
//! that light check exactly like `random_tick.rs`'s own
//! `is_air_variant` proxy for grass's light check (same named simplification:
//! this crate has no light engine — see that module's doc comment for why
//! the **draw pattern**, not the literal light value, is what is asserted).
//!
//! The real crop's growth-speed derivation reads farmland moisture
//! on the block below and up to eight neighbours,
//! plus same-type crops in the four cardinal/diagonal directions. This crate
//! has no farmland-moisture block-state property and no vegetation in
//! worldgen at all (`crate::chunk`'s module doc: the generator produces no
//! trees/crops/farmland), so [`crop_random_tick`] fixes the real growth-speed multiplier at the
//! real "nothing adjacent helps" baseline of `1.0`, giving a bound of
//! `(25.0 / 1.0) as i32 + 1 == 26` — a named simplification of the speed
//! *multiplier* only; the `nextInt` call shape (one draw, bound 26, hit on
//! `0`) is exact.
//!
//! `wheat`/`carrots`/`potatoes` are plain crop-block subclasses (max age 7,
//! and neither carrot nor potato overrides the random tick or max-age
//! query).
//! `beetroots` is the one crop with its own draw gate — transcribed below.
//!
//! # Beetroot's extra gate, transcribed from the real beetroot block
//!
//! The real beetroot's random tick, transcribed as the rule it implements:
//! draw once with a bound of `3`; if that draw is *not* a hit (`0`), fall
//! through into the shared crop-block body above; otherwise stop.
//!
//! This draw happens **unconditionally, before any light check** — a
//! beetroot with insufficient light still consumes this one draw (it just
//! never reaches the shared crop-block body's own light-gated draw). So the full
//! draw pattern for beetroot is: 1 draw always; if that draw is `0`
//! (1-in-3), stop (0 further draws); otherwise fall into the shared
//! crop-block body above (0 further draws if unlit, 1 further draw if lit).
//! The real beetroot's own max age is `3`.
//!
//! # Sapling growth, transcribed from the real sapling block
//!
//! The real sapling's random tick and its advance-tree step, transcribed as
//! the rules they implement:
//!
//! On the random tick: if the raw brightness at the block above is at least
//! 9 **and** a draw with bound `7` hits (`0`), advance the tree.
//!
//! To advance the tree: if the sapling's own growth stage is `0`, cycle it
//! to the next stage. Otherwise, hand off to the tree-growing feature for
//! this sapling's species.
//!
//! The `&&` short-circuits: the light check is above-block-based (the
//! sapling's own light-proxy target, matching this module's `above_is_air`
//! parameter) and gates the `nextInt(7)` draw entirely — unlit means **zero**
//! draws, lit means **exactly one**, hit on `0` (1-in-7).
//!
//! **The advance-tree step's "hand off to the tree-growing feature" branch
//! (an already-stage-1 sapling growing an
//! actual tree) is a named, uncloseable gap, not an oversight**: that feature
//! calls into a tree *feature* (a multi-block structure placer) this crate
//! has no equivalent of — `lodestone-worldgen` (off-limits to this task;
//! see the vegetation-oracle agent's ownership) generates no trees at all
//! today, so there is no decorator this module could call even if it wanted
//! to guess at a shape. [`sapling_random_tick`] returns
//! [`SaplingOutcome::TreeGrowthNotModeled`] for this case rather than
//! fabricating a placeholder tree: a real, faithfully transcribed stage-0→1
//! cycle is
//! implemented (the one part of the advance-tree step this crate *can* do
//! correctly), and a stage-1 sapling that rolls its 1-in-7 chance again
//! simply stays at stage 1 forever until a future tree feature exists to
//! plug into this exact call site — stated plainly, per this repo's own
//! "nothing is done until something on screen changes" rule, rather than
//! silently no-op'd.
//!
//! # Leaf decay, transcribed from the real leaves block
//!
//! The real leaves block's is-randomly-ticking check, random tick, and
//! decaying predicate, transcribed as the rules they implement: a leaves
//! block is randomly ticking iff its distance-to-log value is `7` and it is
//! not marked persistent — the identical condition its own decaying
//! predicate checks. On the random tick, if decaying, drop its resources and
//! remove the block (without dropping experience).
//!
//! Is-randomly-ticking and decaying are the **identical predicate** —
//! every leaf this crate ever selects for a random tick is, by construction,
//! already decaying, so [`leaves_should_decay`] doubles as both this
//! module's selection gate and its action gate, and the random tick itself
//! draws **zero** RNG values: the check is entirely deterministic once selected.
//! The leaf-decay dispatch in `random_tick.rs` skips
//! the real drop-resources step (item-drop spawning is a separate system this task does
//! not own — see `crate::block_entities`/mob loot for the precedent, out of
//! scope here) and removes the block (sets it to air), which is the
//! visually-observable half of decay.
//!
//! **The `distance`-recompute half of the real leaves block (its own
//! shape-update hook
//! scheduling a tick that walks all six neighbours)
//! is deliberately not implemented here.** That
//! half only matters once something maintains `distance` as logs/leaves are
//! placed or removed near each other — and nothing in this crate places logs
//! or leaves at all (no tree feature, as above), so there is no in-world
//! sequence that could ever exercise a distance recompute today. Building it
//! now would be the exact island this task's own brief warns against:
//! correct in isolation, with no producer that could ever call it. A leaf
//! block's `distance` is therefore fixed for its lifetime in this crate —
//! only ever set by whoever constructs the block-state string (a test, or a
//! future tree feature) — and only the already-transcribed is-randomly-ticking/
//! decaying predicate over that fixed value is modeled.

use crate::mob_spawn::SpawnRng;

/// Strips a `[...]` block-state property suffix — the same convention
/// `random_tick.rs`'s own private `base_name` uses, duplicated here (rather
/// than shared) so this module has no dependency on `random_tick.rs`'s
/// private items; both copies do the identical, trivial thing.
fn base_name(state: &str) -> &str {
    state.split('[').next().unwrap_or(state)
}

/// Parses one `key=value` block-state property as `u32`. Returns `None` if
/// the state has no `[...]` suffix, no property named `key`, or the value
/// does not parse — callers all have a documented vanilla default for the
/// missing case (age `0`, stage `0`), which is *also* Minecraft's own
/// registered default for these properties, so "absent suffix" and
/// "explicitly `=0`" are handled identically on purpose.
fn get_u32_property(state: &str, key: &str) -> Option<u32> {
    let (_, props) = state.split_once('[')?;
    let props = props.strip_suffix(']').unwrap_or(props);
    for pair in props.split(',') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == key {
                return v.parse().ok();
            }
        }
    }
    None
}

/// Parses one `key=value` block-state boolean property. `None` if absent
/// (see [`get_u32_property`]'s doc comment for the same "absent means the
/// vanilla default" handling — `persistent`'s vanilla default is `false`,
/// per the real leaves block's own constructor).
fn get_bool_property(state: &str, key: &str) -> Option<bool> {
    let (_, props) = state.split_once('[')?;
    let props = props.strip_suffix(']').unwrap_or(props);
    for pair in props.split(',') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == key {
                return match v {
                    "true" => Some(true),
                    "false" => Some(false),
                    _ => None,
                };
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Crop growth
// ---------------------------------------------------------------------------

pub const WHEAT: &str = "minecraft:wheat";
pub const CARROTS: &str = "minecraft:carrots";
pub const POTATOES: &str = "minecraft:potatoes";
pub const BEETROOTS: &str = "minecraft:beetroots";

/// `Some(max_age)` for a canonical crop base name, `None` otherwise. `7` for
/// wheat/carrots/potatoes (the real crop block's max age, plain crop-block
/// subclasses
/// with no override); `3` for beetroots (the real beetroot's own max age).
#[must_use]
pub fn crop_max_age(base: &str) -> Option<u32> {
    match base {
        WHEAT | CARROTS | POTATOES => Some(7),
        BEETROOTS => Some(3),
        _ => None,
    }
}

/// `true` iff `block_state` is a crop strictly below its own max age —
/// mirrors the real crop block's is-randomly-ticking query
/// (`!this.isMaxAge(state)`).
#[must_use]
pub fn is_growable_crop(block_state: &str) -> bool {
    let base = base_name(block_state);
    match crop_max_age(base) {
        Some(max) => get_u32_property(block_state, "age").unwrap_or(0) < max,
        None => false,
    }
}

/// The crop's current age (real default `0` — the real crop block's own constructor,
/// `registerDefaultState(... setValue(AGE, 0))`).
#[must_use]
pub fn get_age(block_state: &str) -> u32 {
    get_u32_property(block_state, "age").unwrap_or(0)
}

/// Builds the canonical block-state string for `base` at `age`.
#[must_use]
pub fn set_age(base: &str, age: u32) -> String {
    format!("{base}[age={age}]")
}

/// The outcome of one [`crop_random_tick`] call, before any world mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CropOutcome {
    /// Beetroot's own extra gate (`nextInt(3) == 0`) rejected this tick
    /// before the real crop block's own body ever ran. Only reachable for
    /// [`BEETROOTS`].
    SkippedByOuterGate,
    /// `above_is_air` was `false` (the light-check proxy) — zero further
    /// draws, per the real crop block's random tick's light check wrapping the whole
    /// body.
    NoLight,
    /// Light was sufficient, the growth-chance draw did not hit `0`.
    LightButNoGrowth,
    /// Light was sufficient and the growth-chance draw hit `0`: the crop
    /// advances to this new age.
    Grew(u32),
}

/// The pure crop-growth decision — see this module's doc comment for the jar
/// citation and the growth-speed proxy this crate substitutes. `base` must
/// be one of the four constants above and `age` its current age (callers
/// gate on [`crop_max_age`]/[`is_growable_crop`] first, exactly like
/// `random_tick.rs`'s own dispatch gates on `is_randomly_ticking` before
/// calling any per-block handler).
pub fn crop_random_tick(base: &str, age: u32, above_is_air: bool, rng: &mut SpawnRng) -> CropOutcome {
    if base == BEETROOTS {
        // The real beetroot's random tick: a draw with bound 3 that must be
        // non-zero — unconditional,
        // before any light check.
        if rng.next_int(3) == 0 {
            return CropOutcome::SkippedByOuterGate;
        }
    }
    if !above_is_air {
        return CropOutcome::NoLight;
    }
    // growthSpeed fixed at 1.0 (see module doc comment): bound = (25/1)+1 = 26.
    if rng.next_int(26) == 0 {
        CropOutcome::Grew(age + 1)
    } else {
        CropOutcome::LightButNoGrowth
    }
}

// ---------------------------------------------------------------------------
// Sapling growth
// ---------------------------------------------------------------------------

/// `true` for any of vanilla's suffix-`_sapling` blocks (oak/spruce/birch/
/// jungle/acacia/dark_oak/cherry — mangrove's `mangrove_propagule` is a
/// distinct class with its own age mechanic, not covered here). Vanilla sets
/// the real is-randomly-ticking check unconditionally true for every real
/// sapling
/// instance (no override narrowing it, unlike leaves/crop blocks), so
/// this predicate alone is the full selection gate.
#[must_use]
pub fn is_sapling(block_state: &str) -> bool {
    base_name(block_state).ends_with("_sapling")
}

/// The sapling's current growth stage (vanilla default `0` —
/// the real sapling block's own constructor).
#[must_use]
pub fn get_stage(block_state: &str) -> u32 {
    get_u32_property(block_state, "stage").unwrap_or(0)
}

/// Builds the canonical block-state string for `base` at `stage`.
#[must_use]
pub fn set_stage(base: &str, stage: u32) -> String {
    format!("{base}[stage={stage}]")
}

/// The outcome of one [`sapling_random_tick`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaplingOutcome {
    /// `above_is_air` was `false` — zero draws (the `&&` short-circuit).
    NoLight,
    /// Light was sufficient; the `nextInt(7)` draw did not hit `0`.
    NoRoll,
    /// Light was sufficient, the draw hit `0`, and the sapling was at stage
    /// `0` — advances to stage `1` (the real advance-tree step's `if` branch, a real
    /// mutation this crate can perform exactly).
    AdvancedToStage1,
    /// Light was sufficient, the draw hit `0`, and the sapling was already
    /// at stage `1` — the real engine would hand off to the tree-growing
    /// feature here; this
    /// crate has no tree feature to call (see module doc comment), so
    /// nothing is mutated.
    TreeGrowthNotModeled,
}

/// The pure sapling-growth decision — see this module's doc comment for the
/// jar citation.
pub fn sapling_random_tick(above_is_air: bool, stage: u32, rng: &mut SpawnRng) -> SaplingOutcome {
    if !above_is_air {
        return SaplingOutcome::NoLight;
    }
    if rng.next_int(7) != 0 {
        return SaplingOutcome::NoRoll;
    }
    if stage == 0 {
        SaplingOutcome::AdvancedToStage1
    } else {
        SaplingOutcome::TreeGrowthNotModeled
    }
}

// ---------------------------------------------------------------------------
// Leaf decay
// ---------------------------------------------------------------------------

/// `true` for any suffix-`_leaves` block (oak/spruce/birch/jungle/acacia/
/// dark_oak/mangrove/cherry/azalea leaves all follow this naming).
#[must_use]
pub fn is_leaves(block_state: &str) -> bool {
    base_name(block_state).ends_with("_leaves")
}

/// `true` iff this leaves block is currently decaying — the single shared
/// predicate for both the real leaves block's is-randomly-ticking query and
/// its decaying query (see this module's doc comment for why those two
/// are the identical check). `distance` defaults to `7` when the property is
/// absent (the real leaves block's own constructor registers this default — a leaf with no
/// `distance` written is, by vanilla's own default, maximally far from any
/// log and therefore eligible), `persistent` defaults to `false`.
#[must_use]
pub fn leaves_should_decay(block_state: &str) -> bool {
    is_leaves(block_state)
        && get_u32_property(block_state, "distance").unwrap_or(7) == 7
        && !get_bool_property(block_state, "persistent").unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    // ---- property parsing ----

    #[test]
    fn get_u32_property_parses_present_key() {
        assert_eq!(get_u32_property("minecraft:wheat[age=3]", "age"), Some(3));
    }

    #[test]
    fn get_u32_property_is_none_for_absent_suffix_or_key() {
        assert_eq!(get_u32_property("minecraft:wheat", "age"), None);
        assert_eq!(get_u32_property("minecraft:wheat[foo=1]", "age"), None);
    }

    #[test]
    fn get_bool_property_parses_both_values() {
        assert_eq!(get_bool_property("minecraft:oak_leaves[persistent=true]", "persistent"), Some(true));
        assert_eq!(get_bool_property("minecraft:oak_leaves[persistent=false]", "persistent"), Some(false));
    }

    // ---- crop eligibility ----

    #[test]
    fn wheat_below_max_age_is_growable() {
        assert!(is_growable_crop("minecraft:wheat[age=6]"));
        assert!(!is_growable_crop("minecraft:wheat[age=7]"));
        assert!(!is_growable_crop("minecraft:wheat[age=8]"), "past max age must also be ineligible, not just exactly-max");
    }

    #[test]
    fn beetroot_max_age_is_three_not_seven() {
        assert!(is_growable_crop("minecraft:beetroots[age=2]"));
        assert!(!is_growable_crop("minecraft:beetroots[age=3]"));
        // Negative control: proves the eligibility check actually reads
        // beetroot's OWN max, not wheat's — age 5 is growable for wheat but
        // must not be for beetroot.
        assert!(is_growable_crop("minecraft:wheat[age=5]"));
        assert!(!is_growable_crop("minecraft:beetroots[age=5]"));
    }

    #[test]
    fn non_crop_block_is_never_growable() {
        assert!(!is_growable_crop("minecraft:stone"));
        assert!(!is_growable_crop("minecraft:stone[age=0]"));
    }

    #[test]
    fn missing_age_property_defaults_to_zero() {
        assert!(is_growable_crop("minecraft:wheat"));
        assert_eq!(get_age("minecraft:wheat"), 0);
    }

    // ---- crop draw pattern: light gate ----

    /// Unlit, non-beetroot: zero draws, proven by RNG-state equality against
    /// an untouched clone — not just "no growth," the stronger claim this
    /// repo's evidence standards ask for.
    #[test]
    fn unlit_wheat_draws_nothing() {
        let mut rng = SpawnRng::new(11);
        let before = format!("{rng:?}");
        let outcome = crop_random_tick(WHEAT, 0, false, &mut rng);
        assert_eq!(outcome, CropOutcome::NoLight);
        assert_eq!(format!("{rng:?}"), before, "an unlit crop must not draw from the behaviour RNG at all");
    }

    /// Negative control for the assertion above: proves the equality check
    /// can actually fail — a real draw must change the RNG's debug state.
    #[test]
    fn a_real_draw_does_change_the_rng_state() {
        let mut rng = SpawnRng::new(11);
        let before = format!("{rng:?}");
        let _ = rng.next_int(26);
        assert_ne!(format!("{rng:?}"), before, "control failed: next_int must actually advance state");
    }

    /// Lit, non-beetroot: exactly one draw (`nextInt(26)`), regardless of
    /// whether it hits — proven by RNG-state equality against an
    /// independent replay, across many seeds so both the hit and miss
    /// branches are actually exercised (a magnitude/coverage check, not
    /// just a single lucky seed).
    #[test]
    fn lit_wheat_draws_exactly_once_regardless_of_hit() {
        let mut hit_seen = false;
        let mut miss_seen = false;
        for seed in 0..500u64 {
            let mut rng = SpawnRng::new(seed);
            let outcome = crop_random_tick(WHEAT, 3, true, &mut rng);
            let mut replay = SpawnRng::new(seed);
            let draw = replay.next_int(26);
            assert_eq!(
                format!("{rng:?}"),
                format!("{replay:?}"),
                "seed {seed}: lit wheat must draw exactly one nextInt(26), no more, no less"
            );
            match outcome {
                CropOutcome::LightButNoGrowth => {
                    assert_ne!(draw, 0);
                    miss_seen = true;
                }
                CropOutcome::Grew(new_age) => {
                    assert_eq!(draw, 0);
                    assert_eq!(new_age, 4, "age 3 -> 4 on a hit");
                    hit_seen = true;
                }
                other => panic!("seed {seed}: unexpected outcome {other:?} for lit wheat"),
            }
        }
        assert!(hit_seen, "growth branch never occurred in this sweep");
        assert!(miss_seen, "no-growth branch never occurred in this sweep");
    }

    /// Beetroot, unlit: draws exactly the outer gate (`nextInt(3)`), never
    /// the inner one — across many seeds, covering both outer outcomes
    /// (skip vs. fall-through-then-blocked-by-light).
    #[test]
    fn unlit_beetroot_draws_only_the_outer_gate() {
        let mut skipped_seen = false;
        let mut no_light_seen = false;
        for seed in 0..500u64 {
            let mut rng = SpawnRng::new(seed);
            let outcome = crop_random_tick(BEETROOTS, 0, false, &mut rng);
            let mut replay = SpawnRng::new(seed);
            let outer = replay.next_int(3);
            assert_eq!(format!("{rng:?}"), format!("{replay:?}"), "seed {seed}: unlit beetroot must draw exactly the outer gate");
            match outcome {
                CropOutcome::SkippedByOuterGate => {
                    assert_eq!(outer, 0);
                    skipped_seen = true;
                }
                CropOutcome::NoLight => {
                    assert_ne!(outer, 0);
                    no_light_seen = true;
                }
                other => panic!("seed {seed}: unexpected outcome {other:?} for unlit beetroot"),
            }
        }
        assert!(skipped_seen, "the outer gate must actually skip for some seed in this sweep");
        assert!(no_light_seen, "the outer gate must actually fall through for some seed in this sweep");
    }

    /// Beetroot, lit: the full three-branch draw pattern (skip / fall-through
    /// -miss / fall-through-hit), each proven by RNG-state equality against
    /// a replay built from the SAME sequence of raw draws the returned
    /// outcome implies.
    #[test]
    fn lit_beetroot_draw_pattern_matches_its_own_outcome() {
        let mut counts: std::collections::HashMap<&str, u32> = std::collections::HashMap::new();
        for seed in 0..2000u64 {
            let mut rng = SpawnRng::new(seed);
            let outcome = crop_random_tick(BEETROOTS, 1, true, &mut rng);
            let mut replay = SpawnRng::new(seed);
            let outer = replay.next_int(3);
            if outer == 0 {
                assert_eq!(outcome, CropOutcome::SkippedByOuterGate);
                *counts.entry("skip").or_default() += 1;
            } else {
                let inner = replay.next_int(26);
                match outcome {
                    CropOutcome::Grew(new_age) => {
                        assert_eq!(inner, 0);
                        assert_eq!(new_age, 2, "age 1 -> 2 on a hit");
                        *counts.entry("grew").or_default() += 1;
                    }
                    CropOutcome::LightButNoGrowth => {
                        assert_ne!(inner, 0);
                        *counts.entry("miss").or_default() += 1;
                    }
                    other => panic!("seed {seed}: unexpected outcome {other:?}"),
                }
            }
            assert_eq!(
                format!("{rng:?}"),
                format!("{replay:?}"),
                "seed {seed}: beetroot draw pattern diverged from its own outcome's implied replay"
            );
        }
        assert!(*counts.get("skip").unwrap_or(&0) > 0, "outer skip branch never occurred in this sweep");
        assert!(*counts.get("miss").unwrap_or(&0) > 0, "fall-through miss branch never occurred in this sweep");
        assert!(*counts.get("grew").unwrap_or(&0) > 0, "fall-through hit branch never occurred in this sweep");
    }

    // ---- sapling ----

    #[test]
    fn unlit_sapling_draws_nothing() {
        let mut rng = SpawnRng::new(3);
        let before = format!("{rng:?}");
        let outcome = sapling_random_tick(false, 0, &mut rng);
        assert_eq!(outcome, SaplingOutcome::NoLight);
        assert_eq!(format!("{rng:?}"), before);
    }

    #[test]
    fn lit_sapling_draws_exactly_once_and_advances_on_a_hit_at_stage_zero() {
        let mut advanced = false;
        let mut no_roll = false;
        for seed in 0..500u64 {
            let mut rng = SpawnRng::new(seed);
            let outcome = sapling_random_tick(true, 0, &mut rng);
            let mut replay = SpawnRng::new(seed);
            let draw = replay.next_int(7);
            assert_eq!(format!("{rng:?}"), format!("{replay:?}"), "seed {seed}: lit sapling must draw exactly one nextInt(7)");
            match outcome {
                SaplingOutcome::AdvancedToStage1 => {
                    assert_eq!(draw, 0);
                    advanced = true;
                }
                SaplingOutcome::NoRoll => {
                    assert_ne!(draw, 0);
                    no_roll = true;
                }
                other => panic!("seed {seed}: unexpected outcome {other:?} for stage-0 sapling"),
            }
        }
        assert!(advanced, "advance branch never occurred in this sweep");
        assert!(no_roll, "no-roll branch never occurred in this sweep");
    }

    #[test]
    fn a_hit_at_stage_one_is_reported_as_not_modeled_rather_than_faking_a_tree() {
        // Search for a seed where the roll hits, at stage 1.
        for seed in 0..500u64 {
            let mut rng = SpawnRng::new(seed);
            let outcome = sapling_random_tick(true, 1, &mut rng);
            if outcome == SaplingOutcome::TreeGrowthNotModeled {
                return;
            }
        }
        panic!("no seed in this sweep produced a stage-1 hit — test setup is broken, not the code under test");
    }

    // ---- leaves ----

    #[test]
    fn distance_seven_non_persistent_leaves_should_decay() {
        assert!(leaves_should_decay("minecraft:oak_leaves[distance=7,persistent=false]"));
    }

    /// Negative control: persistent leaves never decay, however close to
    /// max distance — proves the detector discriminates on `persistent`,
    /// not just `distance`.
    #[test]
    fn persistent_leaves_never_decay_even_at_distance_seven() {
        assert!(!leaves_should_decay("minecraft:oak_leaves[distance=7,persistent=true]"));
    }

    /// Negative control: leaves closer to a log (`distance < 7`) never
    /// decay, however non-persistent — proves the detector discriminates on
    /// `distance` too, not just `persistent`.
    #[test]
    fn leaves_within_range_of_a_log_never_decay() {
        assert!(!leaves_should_decay("minecraft:oak_leaves[distance=6,persistent=false]"));
        assert!(!leaves_should_decay("minecraft:oak_leaves[distance=1,persistent=false]"));
    }

    #[test]
    fn missing_properties_default_to_the_vanilla_registered_defaults() {
        // No suffix at all: distance defaults to 7, persistent defaults to
        // false — LeavesBlock's own registerDefaultState — so a bare
        // "minecraft:oak_leaves" is eligible to decay.
        assert!(leaves_should_decay("minecraft:oak_leaves"));
    }

    #[test]
    fn non_leaves_block_never_decays() {
        assert!(!leaves_should_decay("minecraft:oak_log[distance=7,persistent=false]"));
    }

    #[test]
    fn is_sapling_and_is_leaves_recognize_the_suffix_family() {
        assert!(is_sapling("minecraft:oak_sapling[stage=0]"));
        assert!(is_sapling("minecraft:spruce_sapling"));
        assert!(!is_sapling("minecraft:oak_log"));
        assert!(is_leaves("minecraft:oak_leaves[distance=7,persistent=false]"));
        assert!(!is_leaves("minecraft:oak_sapling"));
    }

    #[test]
    fn set_age_and_set_stage_round_trip_through_the_getters() {
        let s = set_age(WHEAT, 4);
        assert_eq!(get_age(&s), 4);
        let sap = set_stage("minecraft:oak_sapling", 1);
        assert_eq!(get_stage(&sap), 1);
    }

    /// Coverage guard against the fabricated-fixture failure mode this
    /// repo's evidence standards warn about: proves the beetroot sweep test
    /// above is exercising a real distribution, not an accidental constant,
    /// by checking a wide spread of outer-gate draws actually occurred.
    #[test]
    fn beetroot_outer_gate_draws_all_three_residues_across_many_seeds() {
        let mut residues = HashSet::new();
        for seed in 0..200u64 {
            let mut rng = SpawnRng::new(seed);
            residues.insert(rng.next_int(3));
        }
        assert_eq!(residues, HashSet::from([0, 1, 2]), "expected all three residues mod 3 across 200 seeds");
    }
}
