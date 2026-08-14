//! **World-save parity against a real vanilla 26.2 server**, in both
//! directions.
//!
//! The owner's specification, verbatim: *"World saving should be 1:1 — we
//! should have a roundtrip test that saves a fresh world, gives it to vanilla,
//! has vanilla save it, then we read it back and it should be identical."*
//!
//! See [`docs/world-save-parity.md`] for the prose. What follows is the part a
//! reader of this file needs in order to trust or change it.
//!
//! # Why this is not a byte compare, and why that is not a narrowing of the ask
//!
//! A byte-for-byte diff of the two directories **cannot** pass, for four
//! reasons that are all correct vanilla behaviour rather than defects — so a
//! gate built that way would fail for reasons that teach nothing:
//!
//! 1. **`level.dat` legitimately moves.** `LastPlayed` is a wall-clock stamp;
//!    `Time`/`DayTime` advance; `ServerBrands`, `DataPacks` and `Version` are
//!    the running server's, not ours.
//! 2. **A `session.lock` appears**, plus `logs/`, `usercache.json`, `ops.json`,
//!    and (because this harness force-loads) `data/chunks.dat`.
//! 3. **Region chunk payloads are recompressed.** zlib level and dictionary
//!    choices are the writer's, so identical NBT yields different `.mca` bytes,
//!    and sector placement follows write order rather than content.
//! 4. **NBT compound field order is not part of the value**, so two writers
//!    agreeing on every field still disagree on bytes.
//!
//! So the meaningful 1:1 is **semantic identity of every field we author,
//! after canonical decode** — [`lodestone_anvil::nbt_diff`] for the tree, plus
//! a *fully decoded* block-state and biome comparison per section, because
//! even canonical NBT is not enough: the `block_states.data` bit width is a
//! function of palette length, so a writer that orders its palette differently
//! packs different `long`s for identical blocks. Comparing packed `long`s would
//! measure the palette's order; comparing decoded cells measures the world.
//!
//! **The allowlist is the load-bearing part of this gate.** Each entry names
//! the vanilla behaviour that justifies it, and an over-broad one makes the
//! whole thing vacuous — CLAUDE.md's *assertion* species. The tolerance is the
//! alarm; widening it is cutting the wire. Anything not justifiable is a
//! finding, not an entry.
//!
//! # What each direction proves, and why one is not enough
//!
//! | direction | test | catches |
//! |---|---|---|
//! | **A**: we write → vanilla loads/saves → we read | [`our_fresh_world_survives_a_vanilla_load_and_save`] | our *writer* emitting something vanilla rejects, silently fixes up, or cannot represent — including a `level.dat`/`world_gen_settings.dat` vanilla refuses, which re-rolls the seed and is invisible in the saved blocks alone |
//! | **B**: vanilla wrote → we load → we write → vanilla loads/saves → we read | [`a_real_vanilla_world_survives_our_load_and_save`] | our *reader* dropping data it cannot model — a destructive-persistence shape where a saved world comes back with its chests emptied |
//!
//! `crates/lodestone-server/tests/world_persistence_round_trip.rs` is **our
//! writer through our reader** and says so in its own header. It is a closed
//! loop: `decode(encode(x)) == x` is satisfied by two symmetric
//! misunderstandings, and it structurally cannot see anything either direction
//! here sees. Neither replaces the other.
//!
//! # The two vacuity traps this gate had to be built around
//!
//! Both were measured against a real 26.2 server while building the harness,
//! and either one alone would have made a green run meaningless:
//!
//! - **A server with no players loads almost nothing.** Handed a real world,
//!   26.2 logged `Loading 0 persistent chunks... / Preparing spawn area: 100%
//!   / Time elapsed: 16 ms` and touched no region chunk. Hence the `forceload
//!   add` in [`FORCELOAD_A`] and direction B's computed equivalent, and hence the settle
//!   period in
//!   `scripts/live-oracles/save-parity.sh`.
//! - **A loaded-but-unmodified chunk is not rewritten.** `save-all flush`
//!   writes what `ChunkMap` holds, and an untouched chunk can be `unsaved =
//!   false`. What actually forces the rewrite here is that we write
//!   `isLightOn = 0`: vanilla relights on load and
//!   `ChunkAccess.setLightCorrect(true)` calls `markUnsaved()`. That is an
//!   inference about vanilla's internals, so it is not trusted — each direction
//!   carries a `vanilla_rewrote` control asserting the region bytes changed
//!   **and** that a field only vanilla writes is now present. Without it, "no
//!   differences" would be indistinguishable from "vanilla never looked".
//!
//! # How to change it
//!
//! - **Do not add an allowlist entry to make a run green.** Add one only with
//!   a citation for the vanilla behaviour, in [`ALLOWED`]. An entry with a
//!   reason like "vanilla changes this" is the failure mode.
//! - The fixture's contents are asserted as a **hard precondition**, not
//!   assumed. A fresh ocean or superflat world contains no interesting palette
//!   and would roundtrip trivially while proving nothing — CLAUDE.md's *world*
//!   species, which cannot be found by reading the test. See
//!   [`assert_fixture_is_worth_testing`].
//! - Every test that needs the container is `#[ignore]`d, and a **missing**
//!   runtime is a named panic, never a skip. `#[ignore]` + skip-on-absence is
//!   the *precondition* species twice over.
//! - The seed is fixed and stated ([`SEED`]). A fresh world that differs run to
//!   run makes every failure ambiguous.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use lodestone_anvil::nbt_diff::{self, Difference, DifferenceKind};
use lodestone_anvil::region::{self, RegionFile};
use lodestone_anvil::{CompressionScheme, level_dat, world_gen_settings};
use lodestone_core::Nbt;
use lodestone_server::chunk_nbt;

// ---------------------------------------------------------------------------
// Fixture constants
// ---------------------------------------------------------------------------

/// The world seed for direction A's fresh world.
///
/// **Chosen by a rule, not by hunting for a seed that passes**: it is the seed
/// of the checked-in real vanilla `tests/support/world_gen_settings_26_2_vanilla.dat`
/// (see `world_gen_settings::tests::reads_the_seed_a_real_vanilla_26_2_server_wrote`,
/// where the value came out of an independent Python parser). Reusing it means
/// the `world_gen_settings.dat` we hand vanilla has its `seed` and its
/// `dimensions` tree from **one** real world rather than a graft of two, and
/// needs no `set_seed` call at all.
const SEED: i64 = -195_764_831;

/// Direction A's chunk block: the 3×3 around the origin.
///
/// Deliberately straddling `0` on both axes, so it spans **four** region files
/// (`r.-1.-1`, `r.-1.0`, `r.0.-1`, `r.0.0`) and exercises
/// [`region::region_and_local`]'s negative-coordinate floor (`>> 5` / `& 31`)
/// on the way through. A 3×3 block inside one region would test one quarter of
/// the addressing.
const CHUNKS_A: std::ops::RangeInclusive<i32> = -1..=1;

/// `forceload add` for [`CHUNKS_A`]. **Block** coordinates, not chunk
/// coordinates — vanilla's `/forceload` takes a block position and derives the
/// chunk, so `0 0 31 31` marks four chunks, not thirty-two. Getting this wrong
/// silently shrinks the compared set to whatever vanilla happened to load.
const FORCELOAD_A: &str = "forceload add -16 -16 31 31";

/// Direction B reads the region file containing the origin from a real vanilla
/// world. Named by a rule — the spawn region of the largest 26.2 oracle world —
/// rather than picked for its contents, and then asserted to actually contain
/// what the gate exists to test.
const VANILLA_WORLD_B: &str = ".cache/mc/survival/world";
/// The one region file direction B copies. `r.0.0` covers chunks 0..31 on both
/// axes, so a local coordinate there *is* an absolute chunk coordinate.
const REGION_B: &str = "r.0.0.mca";

/// Direction B **compares** an `8 x 8` contiguous chunk block, not the whole
/// region: forcing all 1024 chunks of a region into a 1200 MB heap is a
/// different test, and 64 chunks is already ~1,500 sections.
const COMPARED_B: i32 = 8;

/// Direction B **writes and force-loads** a block one chunk wider on every
/// side than it compares, and compares only the interior.
///
/// # Why the margin exists — a real harness artifact, measured
///
/// Handing vanilla a *fragment* of a world means every chunk on the fragment's
/// edge acquires brand-new neighbours, and **vanilla decorates a newly generated
/// chunk into its already-existing neighbours**: `applyBiomeDecoration` places
/// features over a 3x3 chunk region, so an ore vein or a tree rooted in a new
/// chunk writes cells into ours. This repo already tracks the same mechanism from
/// the other side — `crates/lodestone-server/tests/decoration_seam_spill.rs`.
///
/// Without the margin, direction B's first live run reported 99 block-state cell
/// changes that were nothing to do with our save format: 25 `stone ->
/// coal_ore`, 8 `granite -> andesite`, 4 `granite -> diorite`, a
/// `deepslate -> deepslate_diamond_ore`, and a scatter of
/// `birch_leaves[distance=N]` updates from neighbours appearing. All of them are
/// vanilla placing features and recomputing leaf distance across the seam,
/// exactly as it would in a real game.
///
/// A margin is the right fix rather than an allowlist entry, because the
/// difference is real — those cells genuinely changed — and allowlisting
/// `block_states` would gut the gate. Making the compared chunks *interior*
/// removes the cause instead of tolerating the effect.
const WRITTEN_B: i32 = COMPARED_B + 2;

/// **The rule that picks direction B's chunks**: the `BLOCK_B x BLOCK_B`
/// contiguous chunk block inside [`REGION_B`] holding the **most block
/// entities**, computed by scanning the region at test time.
///
/// Stated as a rule rather than hardcoded because the alternative shapes are
/// both worse. Hardcoding a coordinate makes the gate silently vacuous if the
/// oracle world is ever regenerated — the region would still parse, the chunks
/// would still compare, and the block entities the direction exists to test
/// would be gone. Hand-hunting for a block with a chest is the *world* species
/// of vacuous test with extra steps.
///
/// Maximising block-entity count is the right objective because that is
/// precisely what direction B exists to protect (1,608 of 1,613 kinds
/// unmodelled and dropped on save, measured at the time this gate was
/// written). The chosen block and its contents are
/// printed on every run, and asserted against a floor, so a fixture that
/// drifted below usefulness fails loudly instead of passing quietly.
///
/// Measured on the current `.cache/mc/survival` world: the winner is chunk
/// offset `(9, 8)` with **145** block entities across **8** kinds — a trial
/// chamber (vaults, trial spawners, decorated pots, dispensers, chests,
/// barrels, hoppers) plus one beehive. Seven of those eight are kinds this
/// server does not simulate, so they can only survive via the verbatim
/// `BlockEntity::Opaque` passthrough.
/// Returns the offset of the `written`-wide block whose **`compared`-wide
/// interior** holds the most block entities.
///
/// Maximising over the *interior* rather than over the whole written block is
/// the load-bearing detail: the margin chunks are written and force-loaded but
/// never compared, so a block that won on margin density would be selected for
/// content this gate then ignores.
fn densest_chunk_block(region: &RegionFile, written: i32, compared: i32) -> (i32, i32) {
    let margin = (written - compared) / 2;
    let mut counts = [[0usize; 32]; 32];
    for lx in 0..32u8 {
        for lz in 0..32u8 {
            let Ok(Some(bytes)) = region.read_chunk_nbt_bytes(lx, lz) else {
                continue;
            };
            let mut reader = lodestone_core::Reader::new(&bytes);
            let Ok((_, nbt)) = lodestone_core::read_named_nbt(&mut reader) else {
                continue;
            };
            counts[lx as usize][lz as usize] = list_elements(field(&nbt, "block_entities")).len();
        }
    }

    let mut best = (0usize, 0i32, 0i32);
    for ox in 0..=(32 - written) {
        for oz in 0..=(32 - written) {
            let total: usize = (ox + margin..ox + margin + compared)
                .flat_map(|x| (oz + margin..oz + margin + compared).map(move |z| (x, z)))
                .map(|(x, z)| counts[x as usize][z as usize])
                .sum();
            // Strictly greater, so ties resolve to the lowest `(ox, oz)` in
            // scan order and the choice is deterministic.
            if total > best.0 {
                best = (total, ox, oz);
            }
        }
    }
    assert!(
        best.0 > 0,
        "no {written}x{written} block in {REGION_B} has a single block entity in its \
         {compared}x{compared} interior — the fixture world holds no structures at all"
    );
    eprintln!(
        "direction B: {written}x{written} block at offset ({}, {}) has the densest \
         {compared}x{compared} interior: {} block entities",
        best.1, best.2, best.0
    );
    (best.1, best.2)
}

/// 26.2 overworld vertical extent. Asserted against the fixture's own `yPos`
/// rather than trusted, because a wrong `min_y` silently mis-slices every
/// section (`region_source`'s own documented gotcha).
const OVERWORLD_MIN_Y: i32 = -64;
/// 26.2 overworld height in blocks.
const OVERWORLD_HEIGHT: i32 = 384;

/// Where a 26.2 world keeps its overworld region files. **Not** the pre-1.17
/// flat `region/`.
const OVERWORLD_REGION_DIR: &str = "dimensions/minecraft/overworld/region";

// ---------------------------------------------------------------------------
// The allowlist
// ---------------------------------------------------------------------------

/// Which side of a difference an allowlist entry permits.
///
/// The split exists because "vanilla added a field we deliberately omit" and
/// "vanilla dropped a field we wrote" are the *same NBT path*, and an entry
/// that permitted both would silently license data loss.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Allow {
    /// Vanilla may add this where we wrote nothing. A `Removed` at the same
    /// path is still a failure.
    Added,
    /// Vanilla may change this scalar's value. Neither adding nor removing it
    /// is permitted.
    Changed,
    /// Vanilla may add it *or* change it. Used only where the field's absence
    /// on our side and its presence-with-another-value are the same story.
    AddedOrChanged,
    /// **Anything at all**, because we write zero bytes anywhere under this
    /// path, so neither side of the comparison is ours.
    ///
    /// The strongest permission in this enum and therefore the one that needs
    /// the most care: it is sound only while its premise holds, and the premise
    /// is *checked* rather than asserted in prose. Direction B asserts our
    /// writer emitted no light array in any section before handing the world
    /// over; the moment we start writing light, these entries begin hiding a
    /// real defect and that assertion fails first.
    VanillaOwnsEntirely,
}

/// One allowlist entry: a path pattern, which side it permits, and the vanilla
/// behaviour that justifies it.
///
/// The `reason` is not decoration. It is the thing a future reader checks
/// before believing an entry, and the thing that makes an unjustifiable
/// difference visibly a finding rather than a candidate entry.
struct Allowed {
    /// Index-normalized path pattern; see [`path_matches`].
    pattern: &'static str,
    /// Which side is permitted.
    allow: Allow,
    /// The vanilla behaviour that makes this legitimate.
    reason: &'static str,
}

/// Differences a real 26.2 server is **expected** to introduce, each with the
/// behaviour that justifies it.
///
/// Every entry below is one of exactly two shapes:
///
/// - a field **we deliberately do not write** which vanilla recomputes from
///   data it does have, so the omission is supported input rather than a loss;
/// - a **clock**, whose whole purpose is to advance.
///
/// Nothing else is here. In particular there is no entry touching
/// `block_states`, `biomes`, `Status`, `xPos`/`yPos`/`zPos`, `DataVersion`,
/// `block_entities`, `block_ticks` or `fluid_ticks` — a difference in any of
/// those is the gate firing.
const ALLOWED: &[Allowed] = &[
    Allowed {
        pattern: "Heightmaps",
        allow: Allow::AddedOrChanged,
        reason: "We deliberately omit heightmaps (`chunk_nbt.rs`'s module doc): \
                 `SerializableChunkData` calls `Heightmap.primeHeightmaps` for every type in \
                 `status.heightmapsAfter()` that the file lacks, so an absent heightmap is \
                 recomputed from the blocks while a *wrong* one is trusted and silently \
                 corrupts terrain. Vanilla adding them is the designed outcome. (The wire \
                 half of the same gap is issue #516 and is not about this file.)",
    },
    Allowed {
        pattern: "Heightmaps.**",
        allow: Allow::AddedOrChanged,
        reason: "As `Heightmaps` above — the per-type `long[]`s inside the compound vanilla \
                 primes.",
    },
    Allowed {
        pattern: "isLightOn",
        allow: Allow::Changed,
        reason: "We write `0` because we ship no light arrays, and claiming correct light \
                 would make a real client render our terrain pitch black instead of relighting \
                 it (`chunk_nbt.rs`). Vanilla's light engine relights on load and \
                 `ChunkAccess.setLightCorrect(true)` sets the flag — which is also what marks \
                 the chunk unsaved and so what makes this gate's comparison non-vacuous.",
    },
    Allowed {
        pattern: "sections[Y=*].BlockLight.**",
        allow: Allow::VanillaOwnsEntirely,
        reason: "**We author zero light bytes**, so both sides of any light comparison are \
                 vanilla's own output and nothing here is ours to be right or wrong about. \
                 Vanilla relights on load because we write `isLightOn = 0`, and light propagates \
                 across chunk boundaries — so a fragment of a world handed back with different \
                 neighbours legitimately relights to different values. Measured: direction A \
                 (a freshly written world) sees pure additions, 2 across 9 chunks; direction B \
                 (a fragment of a real world) sees 1,216 changed bytes plus 3 whole arrays \
                 vanilla chose not to re-emit. **Premise, checked not assumed**: direction B \
                 asserts our writer emitted no light array in any section before the handoff. \
                 If we ever start writing light, that assertion fails and this entry must go.",
    },
    Allowed {
        pattern: "sections[Y=*].SkyLight.**",
        allow: Allow::VanillaOwnsEntirely,
        reason: "As `sections[Y=*].BlockLight.**` — the other half of the relight, with the same \
                 checked premise. Vanilla writes a 2048-byte nibble array per lit section; we \
                 author none.",
    },
    Allowed {
        pattern: "PostProcessing",
        allow: Allow::Added,
        reason: "Vanilla writes `ChunkAccess.getPostProcessing()` — a `ShortList[]` with one \
                 sub-list per section (24 for the overworld) — and a chunk loaded with the field \
                 absent gets an empty array, so writing 24 empty sub-lists where we wrote \
                 nothing is the round trip of nothing. **Measured, not assumed**: an independent \
                 stdlib Python NBT parser over all 841 chunks vanilla saved found every one \
                 carrying 24 sub-lists and a total of **0** positions across all of them. If a \
                 future run ever reports a non-empty `PostProcessing`, that is real data and \
                 this entry is wrong.",
    },
    Allowed {
        pattern: "LastUpdate",
        allow: Allow::Changed,
        reason: "A clock: `SerializableChunkData.copyOf` stamps `level.getGameTime()` at save \
                 time. We write 0 because a freshly generated column has no game time to \
                 report.",
    },
    Allowed {
        pattern: "InhabitedTime",
        allow: Allow::Changed,
        reason: "A clock: accumulated per tick while the chunk is loaded within a player's \
                 range (`LevelChunk.setUnsaved`/`ChunkMap`'s inhabited-time bookkeeping). \
                 Force-loading a chunk with nobody online should leave it at 0, but it is a \
                 counter by construction and a gate that failed on it would be asserting \
                 against vanilla's tick scheduler rather than against our save format.",
    },
];

/// Whether `path` — with its numeric and `Y=`-keyed segments normalized —
/// matches `pattern`.
///
/// Two forms only, kept deliberately small so an entry cannot accidentally
/// match a subtree its author did not intend:
///
/// - an exact (normalized) path, e.g. `PostProcessing`;
/// - a `.**` suffix meaning "this path or anything beneath it", e.g.
///   `Heightmaps.**`.
///
/// There is no leading-wildcard or infix form. A pattern like `*.data` would
/// match `block_states.data`, which is the one thing this gate must never
/// allowlist.
///
/// # The premise-false control this had, and what it cost
///
/// The first version of [`normalize_indices`] handled only `[<digits>]`, and
/// this function's control asserted `path_matches("sections[*].SkyLight",
/// "sections[7].SkyLight")` — which passed. But
/// [`compare_sections`] keys sections by `Y` and emits
/// `sections[Y=7].SkyLight`, a shape **the gate never produces in the form the
/// control tested**. So the shipped allowlist matched nothing, and the first
/// live run reported 39 `SkyLight` additions and 2 `BlockLight` additions as
/// unallowlisted failures.
///
/// The control was exemplary to read and tested a path that could not occur —
/// CLAUDE.md's *world* species, where the flaw is the input data. Hence the
/// rule now enforced by
/// [`the_allowlist_matcher_is_tested_against_paths_the_gate_really_emits`]:
/// **every allowlist pattern must be reachable from a path some assertion in
/// this file has actually observed**, not from one its author imagined.
fn path_matches(pattern: &str, path: &str) -> bool {
    let normalized = normalize_indices(path);
    if let Some(prefix) = pattern.strip_suffix(".**") {
        // `prefix` itself, a dotted child (`Heightmaps.MOTION_BLOCKING`), an
        // array element (`SkyLight[*]`), or a space-separated annotation this
        // file appends (` <summary>`). The `[` and ` ` cases are needed because
        // a `LongArray`/`ByteArray` element path has no separator dot, and
        // without them a `.**` entry would permit the array while reporting
        // every differing byte inside it.
        return normalized == prefix
            || normalized.starts_with(&format!("{prefix}."))
            || normalized.starts_with(&format!("{prefix}["))
            || normalized.starts_with(&format!("{prefix} "));
    }
    normalized == pattern
}

/// Normalizes the two index forms this file's comparisons emit:
/// `[123]` (a plain list index, from [`nbt_diff`]) and `[Y=-4]` (a
/// section keyed by its `Y`, from [`compare_sections`]) both become `[*]` and
/// `[Y=*]` respectively.
///
/// A `block_entities[97,-59,199]` segment and a `cell[x=1,y=2,z=3]` segment are
/// deliberately left alone: nothing may be allowlisted at a block-entity
/// position or a block coordinate, so leaving them unnormalized means a pattern
/// would have to name one literally to match it.
fn normalize_indices(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut chars = path.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '[' {
            out.push(c);
            continue;
        }
        let mut inner = String::new();
        for c in chars.by_ref() {
            if c == ']' {
                break;
            }
            inner.push(c);
        }
        let is_index = !inner.is_empty() && inner.chars().all(|c| c.is_ascii_digit());
        let section_y = inner
            .strip_prefix("Y=")
            .is_some_and(|y| !y.is_empty() && y.strip_prefix('-').unwrap_or(y).chars().all(|c| c.is_ascii_digit()));
        if is_index {
            out.push_str("[*]");
        } else if section_y {
            out.push_str("[Y=*]");
        } else {
            out.push('[');
            out.push_str(&inner);
            out.push(']');
        }
    }
    out
}

/// Looks up the allowlist entry that permits `difference`, if any.
fn allowed_for(difference: &Difference) -> Option<&'static Allowed> {
    ALLOWED.iter().find(|entry| {
        if !path_matches(entry.pattern, &difference.path) {
            return false;
        }
        match (entry.allow, &difference.kind) {
            (Allow::VanillaOwnsEntirely, _) => true,
            (Allow::Added | Allow::AddedOrChanged, DifferenceKind::Added { .. }) => true,
            (Allow::Changed | Allow::AddedOrChanged, DifferenceKind::ValueChanged { .. }) => true,
            // A `Heightmaps` compound that we omit entirely arrives as
            // `Added`; one whose *shape* changed arrives as `LengthChanged` or
            // `TypeChanged`. Both are within "vanilla owns this field", but
            // only for an `AddedOrChanged` entry, never for `Added`.
            (
                Allow::AddedOrChanged,
                DifferenceKind::LengthChanged { .. } | DifferenceKind::TypeChanged { .. },
            ) => true,
            _ => false,
        }
    })
}

// ---------------------------------------------------------------------------
// Chunk-schema-aware comparison
// ---------------------------------------------------------------------------

/// Compares two chunk NBT trees, returning differences with located paths.
///
/// Schema-aware in exactly three places, each because a purely structural
/// comparison would report a difference where there is none:
///
/// 1. **`sections` is keyed by `Y`, not by list index.** Vanilla writes
///    light-only sections one past each end of the world's vertical extent, so
///    the two lists have different lengths and index-wise comparison would
///    misalign every section — and [`nbt_diff::diff`] would collapse the whole
///    list into one `LengthChanged`, destroying all location information.
///    Paths therefore read `sections[Y=-4]`, which is also a more useful thing
///    to be told.
/// 2. **`block_states` and `biomes` are compared decoded**, cell by cell,
///    rather than as packed `long`s. See this file's header.
/// 3. **`block_entities` is keyed by `(x, y, z)`**, since nothing makes a
///    writer emit them in any particular order.
fn compare_chunk(ours: &Nbt, theirs: &Nbt) -> Vec<Difference> {
    let mut out = Vec::new();

    // Everything except the two order-insensitive lists, structurally.
    let ours_rest = without_fields(ours, &["sections", "block_entities"]);
    let theirs_rest = without_fields(theirs, &["sections", "block_entities"]);
    out.extend(nbt_diff::diff(&ours_rest, &theirs_rest));

    compare_sections(ours, theirs, &mut out);
    compare_block_entities(ours, theirs, &mut out);
    out
}

fn without_fields(nbt: &Nbt, drop: &[&str]) -> Nbt {
    match nbt {
        Nbt::Compound(fields) => Nbt::Compound(
            fields
                .iter()
                .filter(|(name, _)| !drop.contains(&name.as_str()))
                .cloned()
                .collect(),
        ),
        other => other.clone(),
    }
}

fn field<'a>(nbt: &'a Nbt, name: &str) -> Option<&'a Nbt> {
    match nbt {
        Nbt::Compound(fields) => fields
            .iter()
            .find(|(field, _)| field == name)
            .map(|(_, value)| value),
        _ => None,
    }
}

fn list_elements<'a>(nbt: Option<&'a Nbt>) -> &'a [Nbt] {
    match nbt {
        Some(Nbt::List { elements, .. }) => elements,
        _ => &[],
    }
}

fn int_field(nbt: &Nbt, name: &str) -> Option<i64> {
    match field(nbt, name) {
        Some(Nbt::Byte(v)) => Some(i64::from(*v)),
        Some(Nbt::Short(v)) => Some(i64::from(*v)),
        Some(Nbt::Int(v)) => Some(i64::from(*v)),
        Some(Nbt::Long(v)) => Some(*v),
        _ => None,
    }
}

fn compare_sections(ours: &Nbt, theirs: &Nbt, out: &mut Vec<Difference>) {
    let ours_by_y = sections_by_y(ours);
    let theirs_by_y = sections_by_y(theirs);

    let mut ys: Vec<i64> = ours_by_y.keys().chain(theirs_by_y.keys()).copied().collect();
    ys.sort_unstable();
    ys.dedup();

    for y in ys {
        let path = format!("sections[Y={y}]");
        match (ours_by_y.get(&y), theirs_by_y.get(&y)) {
            (Some(ours_section), Some(theirs_section)) => {
                // Structural comparison of everything except the two paletted
                // containers, which are compared decoded below.
                let ours_rest = without_fields(ours_section, &["block_states", "biomes"]);
                let theirs_rest = without_fields(theirs_section, &["block_states", "biomes"]);
                for mut difference in nbt_diff::diff(&ours_rest, &theirs_rest) {
                    difference.path = join_path(&path, &difference.path);
                    out.push(difference);
                }
                compare_paletted(
                    &format!("{path}.block_states"),
                    field(ours_section, "block_states"),
                    field(theirs_section, "block_states"),
                    PalettedKind::BlockStates,
                    y,
                    out,
                );
                compare_paletted(
                    &format!("{path}.biomes"),
                    field(ours_section, "biomes"),
                    field(theirs_section, "biomes"),
                    PalettedKind::Biomes,
                    y,
                    out,
                );
            }
            (Some(only_ours), None) => out.push(Difference {
                path,
                kind: DifferenceKind::Removed {
                    left: nbt_diff::summarize(only_ours),
                },
            }),
            (None, Some(only_theirs)) => out.push(Difference {
                path,
                kind: DifferenceKind::Added {
                    right: nbt_diff::summarize(only_theirs),
                },
            }),
            (None, None) => unreachable!("y came from one of the two maps"),
        }
    }
}

fn join_path(prefix: &str, suffix: &str) -> String {
    if suffix.is_empty() {
        prefix.to_string()
    } else if suffix.starts_with('[') {
        format!("{prefix}{suffix}")
    } else {
        format!("{prefix}.{suffix}")
    }
}

fn sections_by_y(chunk: &Nbt) -> BTreeMap<i64, Nbt> {
    let mut out = BTreeMap::new();
    for section in list_elements(field(chunk, "sections")) {
        if let Some(y) = int_field(section, "Y") {
            out.insert(y, section.clone());
        }
    }
    out
}

fn compare_block_entities(ours: &Nbt, theirs: &Nbt, out: &mut Vec<Difference>) {
    let ours_by_pos = block_entities_by_pos(ours);
    let theirs_by_pos = block_entities_by_pos(theirs);

    let mut positions: Vec<(i64, i64, i64)> = ours_by_pos
        .keys()
        .chain(theirs_by_pos.keys())
        .copied()
        .collect();
    positions.sort_unstable();
    positions.dedup();

    for pos in positions {
        let (x, y, z) = pos;
        let path = format!("block_entities[{x},{y},{z}]");
        match (ours_by_pos.get(&pos), theirs_by_pos.get(&pos)) {
            (Some(l), Some(r)) => {
                for mut difference in nbt_diff::diff(l, r) {
                    difference.path = join_path(&path, &difference.path);
                    out.push(difference);
                }
            }
            (Some(l), None) => out.push(Difference {
                path,
                kind: DifferenceKind::Removed {
                    left: describe_block_entity(l),
                },
            }),
            (None, Some(r)) => out.push(Difference {
                path,
                kind: DifferenceKind::Added {
                    right: describe_block_entity(r),
                },
            }),
            (None, None) => unreachable!("position came from one of the two maps"),
        }
    }
}

fn block_entities_by_pos(chunk: &Nbt) -> BTreeMap<(i64, i64, i64), Nbt> {
    let mut out = BTreeMap::new();
    for entity in list_elements(field(chunk, "block_entities")) {
        let key = (
            int_field(entity, "x").unwrap_or(i64::MIN),
            int_field(entity, "y").unwrap_or(i64::MIN),
            int_field(entity, "z").unwrap_or(i64::MIN),
        );
        out.insert(key, entity.clone());
    }
    out
}

fn describe_block_entity(entity: &Nbt) -> String {
    let id = match field(entity, "id") {
        Some(Nbt::String(id)) => id.as_str(),
        _ => "<no id>",
    };
    format!("{id} {}", nbt_diff::summarize(entity))
}

// ---------------------------------------------------------------------------
// Paletted-container decode
// ---------------------------------------------------------------------------

/// Which of the two paletted containers a section holds, and specifically the
/// two constants that differ between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PalettedKind {
    /// 16×16×16 block states, `bits = max(4, ceil_log2(len))`.
    BlockStates,
    /// 4×4×4 biome quarts, `bits = max(1, ceil_log2(len))` — a **different**
    /// floor, pinned by the real 2-entry biome palette that measured exactly
    /// one `long`.
    Biomes,
}

impl PalettedKind {
    fn cells(self) -> usize {
        match self {
            Self::BlockStates => 4096,
            Self::Biomes => 64,
        }
    }
    fn bits_floor(self) -> u32 {
        match self {
            Self::BlockStates => 4,
            Self::Biomes => 1,
        }
    }
    fn edge(self) -> i64 {
        match self {
            Self::BlockStates => 16,
            Self::Biomes => 4,
        }
    }
}

/// Decodes a `{palette, data?}` compound into one entry name per cell.
///
/// The packing rule is **non-spanning**: `valuesPerLong = 64 / bits` and an
/// entry never straddles two `long`s, so the array is `ceil(cells /
/// valuesPerLong)` longs. That is the one rule which, guessed wrong, silently
/// corrupts everything while every palette of 16 or fewer entries still reads
/// correctly — and it is not a guess here: it was measured against a real 26.2
/// world (a 20-entry palette taking **342** longs, not 320) and independently
/// adjudicated by Mojang's own `SimpleBitStorage` in
/// `crates/lodestone-server/tests/write_path_jvm_oracle.rs`, whose dense-packing
/// control disagreed on 16 of 24 probes.
///
/// An **absent** `data` field means every cell is `palette[0]`, which is how
/// vanilla writes a single-state section. Returning an error for it would fail
/// on every all-air and every all-stone section in the world.
fn decode_paletted(container: &Nbt, kind: PalettedKind) -> Result<Vec<String>, String> {
    let palette = list_elements(field(container, "palette"));
    if palette.is_empty() {
        return Err("palette is empty or absent".to_string());
    }
    let names: Vec<String> = palette.iter().map(canonical_palette_entry).collect();

    let data = match field(container, "data") {
        Some(Nbt::LongArray(data)) => data.as_slice(),
        Some(other) => {
            return Err(format!(
                "`data` is {} rather than LongArray",
                nbt_diff::summarize(other)
            ));
        }
        None => {
            // Single-entry palette, or a writer that elided a uniform
            // container. Either way every cell is entry 0 — but a *multi*-entry
            // palette with no data is a real inconsistency worth reporting
            // rather than flattening.
            if names.len() > 1 {
                return Err(format!(
                    "no `data` array but the palette has {} entries",
                    names.len()
                ));
            }
            return Ok(vec![names[0].clone(); kind.cells()]);
        }
    };

    let bits = ceil_log2(names.len()).max(kind.bits_floor());
    let values_per_long = 64 / bits as usize;
    let expected_longs = kind.cells().div_ceil(values_per_long);
    if data.len() != expected_longs {
        return Err(format!(
            "`data` is {} longs; a {}-entry palette at {bits} bits, {values_per_long} per long, \
             non-spanning, needs {expected_longs}",
            data.len(),
            names.len()
        ));
    }

    let mask = if bits >= 64 { u64::MAX } else { (1u64 << bits) - 1 };
    let mut out = Vec::with_capacity(kind.cells());
    for cell in 0..kind.cells() {
        let word = data[cell / values_per_long] as u64;
        let shift = (cell % values_per_long) * bits as usize;
        let index = ((word >> shift) & mask) as usize;
        match names.get(index) {
            Some(name) => out.push(name.clone()),
            None => {
                return Err(format!(
                    "cell {cell} indexes palette entry {index}, but the palette has {} entries",
                    names.len()
                ));
            }
        }
    }
    Ok(out)
}

/// `ceil(log2(n))` for `n >= 1`, which is what a palette index width is.
fn ceil_log2(n: usize) -> u32 {
    if n <= 1 {
        return 0;
    }
    usize::BITS - (n - 1).leading_zeros()
}

/// A palette entry rendered as one canonical string, `name[a=1,b=2]` with
/// properties **sorted by key**.
///
/// Sorting is not cosmetic: nothing makes two writers emit a state's properties
/// in the same order, and an unsorted rendering would report every multi-property
/// state in the world as changed. A biome palette entry is a bare `String`
/// rather than a compound, and both shapes arrive here.
fn canonical_palette_entry(entry: &Nbt) -> String {
    if let Nbt::String(name) = entry {
        return name.clone();
    }
    let name = match field(entry, "Name") {
        Some(Nbt::String(name)) => name.clone(),
        _ => format!("<malformed palette entry {}>", nbt_diff::summarize(entry)),
    };
    let Some(Nbt::Compound(properties)) = field(entry, "Properties") else {
        return name;
    };
    let mut rendered: Vec<String> = properties
        .iter()
        .map(|(key, value)| {
            let value = match value {
                Nbt::String(v) => v.clone(),
                other => nbt_diff::summarize(other),
            };
            format!("{key}={value}")
        })
        .collect();
    rendered.sort();
    format!("{name}[{}]", rendered.join(","))
}

/// Compares two paletted containers cell by cell, reporting a located
/// difference per differing cell **plus a bounding box**.
///
/// Per CLAUDE.md: measure by location, never by fraction. "99.8% of cells
/// match" cannot distinguish a correct world from one that lost every chest,
/// and a bounding box is what diagnosed two premise-false controls in one step
/// elsewhere in this repo. The per-cell list is capped for readability but the
/// **count and the box are computed over every cell**, so the report can never
/// understate the damage.
fn compare_paletted(
    path: &str,
    ours: Option<&Nbt>,
    theirs: Option<&Nbt>,
    kind: PalettedKind,
    section_y: i64,
    out: &mut Vec<Difference>,
) {
    let (ours, theirs) = match (ours, theirs) {
        (Some(l), Some(r)) => (l, r),
        (Some(l), None) => {
            out.push(Difference {
                path: path.to_string(),
                kind: DifferenceKind::Removed {
                    left: nbt_diff::summarize(l),
                },
            });
            return;
        }
        (None, Some(r)) => {
            out.push(Difference {
                path: path.to_string(),
                kind: DifferenceKind::Added {
                    right: nbt_diff::summarize(r),
                },
            });
            return;
        }
        (None, None) => return,
    };

    let ours_cells = match decode_paletted(ours, kind) {
        Ok(cells) => cells,
        Err(why) => {
            out.push(Difference {
                path: format!("{path} <ours undecodable>"),
                kind: DifferenceKind::ValueChanged {
                    left: why,
                    right: "n/a".to_string(),
                },
            });
            return;
        }
    };
    let theirs_cells = match decode_paletted(theirs, kind) {
        Ok(cells) => cells,
        Err(why) => {
            out.push(Difference {
                path: format!("{path} <vanilla's undecodable>"),
                kind: DifferenceKind::ValueChanged {
                    left: "n/a".to_string(),
                    right: why,
                },
            });
            return;
        }
    };

    let edge = kind.edge();
    let mut mismatches = 0usize;
    let mut box_min = (i64::MAX, i64::MAX, i64::MAX);
    let mut box_max = (i64::MIN, i64::MIN, i64::MIN);
    let mut sample = Vec::new();

    for (cell, (l, r)) in ours_cells.iter().zip(&theirs_cells).enumerate() {
        if l == r {
            continue;
        }
        mismatches += 1;
        // Section-local order is `(y << 8) | (z << 4) | x` for block states
        // and `(y << 4) | (z << 2) | x` for the 4×4×4 biome grid — the same
        // row-major convention at two edge lengths.
        let cell = cell as i64;
        let lx = cell % edge;
        let lz = (cell / edge) % edge;
        let ly = cell / (edge * edge);
        let world_y = section_y * 16 + if kind == PalettedKind::Biomes { ly * 4 } else { ly };
        box_min = (box_min.0.min(lx), box_min.1.min(world_y), box_min.2.min(lz));
        box_max = (box_max.0.max(lx), box_max.1.max(world_y), box_max.2.max(lz));
        if sample.len() < 8 {
            sample.push(Difference {
                path: format!("{path}.cell[x={lx},y={world_y},z={lz}]"),
                kind: DifferenceKind::ValueChanged {
                    left: l.clone(),
                    right: r.clone(),
                },
            });
        }
    }

    if mismatches == 0 {
        return;
    }
    out.extend(sample);
    out.push(Difference {
        path: format!("{path} <summary>"),
        kind: DifferenceKind::ValueChanged {
            left: format!("{mismatches} of {} cells differ", ours_cells.len()),
            right: format!(
                "bounding box (section-local x/z, world y): \
                 x {}..={}, y {}..={}, z {}..={}",
                box_min.0, box_max.0, box_min.1, box_max.1, box_min.2, box_max.2
            ),
        },
    });
}

// ---------------------------------------------------------------------------
// Fixture preconditions
// ---------------------------------------------------------------------------

/// What a fixture actually contains, measured rather than assumed.
#[derive(Debug, Default)]
struct FixtureCensus {
    chunks: usize,
    sections: usize,
    distinct_block_states: std::collections::BTreeSet<String>,
    largest_block_palette: usize,
    block_entities: usize,
    distinct_block_entity_ids: std::collections::BTreeSet<String>,
    block_ticks: usize,
    fluid_ticks: usize,
    min_section_y: i64,
    max_section_y: i64,
    /// Sections whose block-state palette holds **both** spellings of one
    /// fluid state — `minecraft:water` and `minecraft:water[level=0]`, or the
    /// lava pair.
    ///
    /// Not a cosmetic statistic. Two palette entries for one block state cost
    /// a palette slot each, and a palette crossing 16 entries costs a bit of
    /// index width for the whole section — so this is a size regression as well
    /// as a parity one. Direction A's first live run measured this as the sole
    /// cause of all 136 block-state cell differences; see the `sections with
    /// both fluid spellings` line in its report.
    sections_with_both_fluid_spellings: usize,
}

fn census(chunks: &BTreeMap<(i32, i32), Nbt>) -> FixtureCensus {
    let mut out = FixtureCensus {
        min_section_y: i64::MAX,
        max_section_y: i64::MIN,
        ..FixtureCensus::default()
    };
    for chunk in chunks.values() {
        out.chunks += 1;
        for section in list_elements(field(chunk, "sections")) {
            out.sections += 1;
            if let Some(y) = int_field(section, "Y") {
                out.min_section_y = out.min_section_y.min(y);
                out.max_section_y = out.max_section_y.max(y);
            }
            let Some(states) = field(section, "block_states") else {
                continue;
            };
            let palette = list_elements(field(states, "palette"));
            out.largest_block_palette = out.largest_block_palette.max(palette.len());
            let names: std::collections::BTreeSet<String> =
                palette.iter().map(canonical_palette_entry).collect();
            for fluid in ["minecraft:water", "minecraft:lava"] {
                if names.contains(fluid) && names.contains(&format!("{fluid}[level=0]")) {
                    out.sections_with_both_fluid_spellings += 1;
                }
            }
            out.distinct_block_states.extend(names);
        }
        for entity in list_elements(field(chunk, "block_entities")) {
            out.block_entities += 1;
            if let Some(Nbt::String(id)) = field(entity, "id") {
                out.distinct_block_entity_ids.insert(id.clone());
            }
        }
        out.block_ticks += list_elements(field(chunk, "block_ticks")).len();
        out.fluid_ticks += list_elements(field(chunk, "fluid_ticks")).len();
    }
    out
}

/// **The hard precondition.** A fixture that does not contain the structures
/// this gate exists to exercise makes the whole run vacuous in the way
/// CLAUDE.md records as unreadable from the test source: the flaw is in the
/// input data, not in any assertion.
///
/// The specific traps this defends against, both of which have already
/// produced a vacuous gate in this repo: a fresh **ocean** chunk (seed 1234
/// chunk (0,0)) and a worldgen fixture tree carrying only plains and savanna.
/// Either roundtrips trivially.
///
/// `min_palette` is the load-bearing threshold: a section palette of **more
/// than 16 entries** is the only thing that exercises the non-spanning packing
/// rule at all, because every palette of 16 or fewer divides 64 evenly and
/// reads correctly under either rule.
fn assert_fixture_is_worth_testing(
    label: &str,
    census: &FixtureCensus,
    expect_chunks: usize,
    min_distinct_states: usize,
    min_palette: usize,
) {
    assert_eq!(
        census.chunks, expect_chunks,
        "{label}: fixture has {} chunks, expected {expect_chunks} — the compared set is not the \
         one this gate was sized for",
        census.chunks
    );
    assert!(
        census.distinct_block_states.len() >= min_distinct_states,
        "{label}: only {} distinct block states across the fixture (need >= {min_distinct_states}). \
         A fixture this uniform would roundtrip trivially and prove nothing. States seen: {:?}",
        census.distinct_block_states.len(),
        census.distinct_block_states
    );
    assert!(
        census.largest_block_palette > min_palette,
        "{label}: largest section palette is {} entries (need > {min_palette}). Nothing in this \
         fixture exercises the non-spanning bit-packing rule, which is the one rule that \
         silently corrupts everything when guessed wrong — every palette of 16 or fewer \
         divides 64 evenly and reads correctly either way.",
        census.largest_block_palette
    );
    assert!(
        census.distinct_block_states.iter().any(|s| s == "minecraft:air"),
        "{label}: no air anywhere — this is not terrain"
    );
}

// ---------------------------------------------------------------------------
// World assembly and the vanilla handoff
// ---------------------------------------------------------------------------

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repo root is two levels above this crate")
}

/// Per-case scratch directory.
///
/// A **literal nonce**, not a pid: the scratch area is shared between
/// concurrently running agents in this repo, and a pid collision would read as
/// a persistence defect rather than as two runs colliding. Same convention as
/// `write_path_jvm_oracle.rs`'s `lodestone-437-jvm-fixture-4m8k`.
fn case_dir(case: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lodestone-save-parity-b7k3/{case}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the case directory");
    dir
}

/// Writes a set of chunk NBT trees into a 26.2 world directory, grouped into
/// region files, through this crate's own container writer.
fn write_world(world: &Path, chunks: &BTreeMap<(i32, i32), Nbt>) {
    let region_dir = world.join(OVERWORLD_REGION_DIR);
    std::fs::create_dir_all(&region_dir).expect("create the region directory");

    let mut by_region: BTreeMap<(i32, i32), BTreeMap<(i32, i32), Nbt>> = BTreeMap::new();
    for (&(cx, cz), nbt) in chunks {
        let (rx, rz, _, _) = region::region_and_local(cx, cz);
        by_region
            .entry((rx, rz))
            .or_default()
            .insert((cx, cz), nbt.clone());
    }

    for ((rx, rz), region_chunks) in &by_region {
        // Zlib, scheme id 2 — vanilla's `region-file-compression` default, so
        // the file we hand over is the shape a real server expects to find.
        // The timestamp is fixed rather than `SystemTime::now()`: a fresh world
        // that differs run to run makes every failure ambiguous.
        let built = region::build_region_from_nbt(region_chunks, CompressionScheme::Zlib, 1)
            .expect("build the region file");
        std::fs::write(region_dir.join(format!("r.{rx}.{rz}.mca")), &built.bytes)
            .expect("write the region file");
        for (cx, cz, bytes) in &built.external {
            std::fs::write(region_dir.join(format!("c.{cx}.{cz}.mcc")), bytes)
                .expect("write an externalized chunk");
        }
    }
}

/// Reads back the chunks at `coords` from a world directory.
fn read_world(world: &Path, coords: &[(i32, i32)]) -> BTreeMap<(i32, i32), Nbt> {
    let region_dir = world.join(OVERWORLD_REGION_DIR);
    let mut regions: BTreeMap<(i32, i32), RegionFile> = BTreeMap::new();
    let mut out = BTreeMap::new();

    for &(cx, cz) in coords {
        let (rx, rz, local_x, local_z) = region::region_and_local(cx, cz);
        let region = regions.entry((rx, rz)).or_insert_with(|| {
            let path = region_dir.join(format!("r.{rx}.{rz}.mca"));
            RegionFile::read_from_file(&path).unwrap_or_else(|e| {
                panic!("read {} back after the vanilla round trip: {e}", path.display())
            })
        });
        let Some(bytes) = region
            .read_chunk_nbt_bytes_resolving_external(local_x, local_z, cx, cz, &region_dir)
            .unwrap_or_else(|e| panic!("read chunk ({cx}, {cz}): {e}"))
        else {
            continue;
        };
        let mut reader = lodestone_core::Reader::new(&bytes);
        let (_, nbt) = lodestone_core::read_named_nbt(&mut reader)
            .unwrap_or_else(|e| panic!("decode chunk ({cx}, {cz}) NBT: {e}"));
        out.insert((cx, cz), nbt);
    }
    out
}

/// Runs `scripts/live-oracles/save-parity.sh`, which boots a real vanilla 26.2
/// server on `<server_root>/<level>`, force-loads what `commands` asks for,
/// saves, and stops cleanly.
///
/// **A missing container runtime is a named panic, never a skip.** "Skip when
/// the oracle is absent" is the *precondition* species of vacuous test, and
/// this repo already carries 277 `#[ignore]` attributes whose rot is unbounded;
/// a gate that also degrades silently on a missing runtime is
/// unverified in two independent ways at once.
fn hand_to_vanilla(server_root: &Path, level: &str, commands: &[&str]) {
    let script = repo_root().join("scripts/live-oracles/save-parity.sh");
    assert!(
        script.is_file(),
        "no save-parity harness at {} — this gate has no fallback; it exists to have a real \
         Mojang server adjudicate our bytes",
        script.display()
    );

    let mut command = Command::new("bash");
    command.arg(&script).arg(server_root).arg(level);
    for rcon in commands {
        command.arg(rcon);
    }

    let output = command.output().unwrap_or_else(|e| {
        panic!(
            "could not run {} ({e}). Apple `container` and python3 must both be on PATH; see \
             docs/oracle-runtimes.md",
            script.display()
        )
    });
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    eprintln!("--- save-parity.sh stdout ---\n{stdout}\n--- stderr ---\n{stderr}");
    assert!(
        output.status.success(),
        "the vanilla 26.2 round trip failed (exit {:?}).\nstdout:\n{stdout}\nstderr:\n{stderr}\n\
         If this says the runtime is missing: start Apple `container` (`container system start`) \
         and confirm .cache/mc/26.2 holds the extracted server. This gate must fail rather than \
         skip when the oracle is absent.",
        output.status.code()
    );
}

/// Asserts the boot log shows vanilla accepted the world rather than falling
/// back — the failure mode that is invisible in the saved blocks.
fn assert_vanilla_accepted_the_world(server_root: &Path) {
    let log_path = server_root.join("logs/latest.log");
    let log = std::fs::read_to_string(&log_path)
        .unwrap_or_else(|e| panic!("read {} ({e}) — the harness should have left one", log_path.display()));

    // `LevelStorageSource.readExistingSavedData` failing makes vanilla log
    // this and build `WorldOptions.defaultWithRandomSeed()`, silently replacing
    // the seed. Every block already on disk still loads, so no blocks-only
    // assertion can see it.
    assert!(
        !log.contains("Unable to read or access the world gen settings file"),
        "vanilla REJECTED our world_gen_settings.dat and fell back to a random seed. \
         Every chunk we wrote still loads, so this is invisible in the saved blocks — it is \
         only visible here. Log:\n{log}"
    );
    assert!(
        !log.contains("Failed to load level"),
        "vanilla failed to load the level entirely. Log:\n{log}"
    );
    assert!(
        log.contains("All chunks are saved"),
        "vanilla never completed a save, so the directory read back is not vanilla's own \
         output. Log:\n{log}"
    );
}

/// The **control that makes the comparison non-vacuous**: vanilla must actually
/// have rewritten the region files.
///
/// Two independent signals, because either alone has a false-pass:
///
/// - the region file's bytes changed, which a recompression alone would also
///   cause — necessary but not sufficient;
/// - a field **only vanilla writes** (`Heightmaps`) is now present, which
///   proves the chunk went through `SerializableChunkData` rather than being
///   carried across as opaque bytes.
///
/// Without this, "zero unallowlisted differences" would be indistinguishable
/// from "vanilla never looked at these chunks" — and the harness measured
/// exactly that outcome (`Loading 0 persistent chunks`) before `forceload` was
/// added.
fn assert_vanilla_rewrote(
    label: &str,
    before: &BTreeMap<PathBuf, Vec<u8>>,
    after: &BTreeMap<(i32, i32), Nbt>,
) {
    let mut changed = 0usize;
    for (path, original) in before {
        let now = std::fs::read(path).unwrap_or_else(|e| panic!("re-read {} ({e})", path.display()));
        if now != *original {
            changed += 1;
        }
    }
    assert!(
        changed > 0,
        "{label}: not one of the {} region files changed on disk. Vanilla did not rewrite them, \
         so every comparison below would be this test comparing its own bytes to themselves — \
         a vacuous pass. Check the forceload coordinates and the settle period in \
         scripts/live-oracles/save-parity.sh.",
        before.len()
    );

    let with_heightmaps = after
        .values()
        .filter(|chunk| field(chunk, "Heightmaps").is_some())
        .count();
    assert_eq!(
        with_heightmaps,
        after.len(),
        "{label}: only {with_heightmaps} of {} chunks read back carry `Heightmaps`. We never \
         write that field, so its presence is the proof a chunk went through vanilla's \
         `SerializableChunkData`; a chunk without it was not processed, and comparing it \
         proves nothing.",
        after.len()
    );
}

/// Snapshots every region file's bytes, for [`assert_vanilla_rewrote`].
fn snapshot_regions(world: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let region_dir = world.join(OVERWORLD_REGION_DIR);
    let mut out = BTreeMap::new();
    for entry in std::fs::read_dir(&region_dir).expect("list the region directory") {
        let path = entry.expect("a directory entry").path();
        if path.extension().and_then(|e| e.to_str()) == Some("mca") {
            let bytes = std::fs::read(&path).expect("read a region file");
            out.insert(path, bytes);
        }
    }
    assert!(!out.is_empty(), "no .mca files in {}", region_dir.display());
    out
}

/// Renders a report and fails, or returns quietly.
///
/// The report lists **every** unallowlisted difference — no truncation. A
/// summary that capped the list would turn "we lost 15,000 blocks" into "we
/// lost 20", which is the shape CLAUDE.md records as a control reporting 0
/// where the truth was ~15,000. Allowlisted differences are counted and grouped
/// so a reader can see the allowlist is doing bounded work rather than
/// swallowing the run.
fn assert_parity(label: &str, per_chunk: &BTreeMap<(i32, i32), Vec<Difference>>) {
    let mut failures: Vec<String> = Vec::new();
    let mut allowed_counts: BTreeMap<&'static str, usize> = BTreeMap::new();

    for ((cx, cz), differences) in per_chunk {
        for difference in differences {
            match allowed_for(difference) {
                Some(entry) => *allowed_counts.entry(entry.pattern).or_default() += 1,
                None => failures.push(format!("  chunk ({cx}, {cz})  {difference}")),
            }
        }
    }

    eprintln!("--- {label}: allowlisted differences (expected) ---");
    if allowed_counts.is_empty() {
        eprintln!("  (none — see the vanilla_rewrote control; vanilla should at minimum have \
                   added Heightmaps)");
    }
    for (pattern, count) in &allowed_counts {
        eprintln!("  {count:>6}  {pattern}");
    }

    assert!(
        failures.is_empty(),
        "{label}: {} unallowlisted difference(s) between what we wrote and what a real vanilla \
         26.2 server wrote back.\n\n{}\n\nEach line above is an NBT path. None of these is on \
         the allowlist in ALLOWED, so each is either a defect in our save format or a vanilla \
         behaviour that needs an entry WITH a citation — never an entry added to make this \
         green.",
        failures.len(),
        failures.join("\n")
    );
}

// ---------------------------------------------------------------------------
// Controls that run on every `cargo test` (not #[ignore]d)
// ---------------------------------------------------------------------------

/// Every allowlist pattern must be reachable from a path shape this file's own
/// comparison functions really produce.
///
/// This is the control the first version of this gate did not have, and its
/// absence cost a whole live run: the shipped patterns were written as
/// `sections[*].SkyLight` while [`compare_sections`] emits
/// `sections[Y=-4].SkyLight`, so the allowlist matched nothing and 41 expected
/// differences were reported as failures. The old control passed throughout,
/// because it asserted against a hand-written path rather than an emitted one.
///
/// The fix that generalizes: **derive the paths from the comparison, not from
/// the pattern's author.** Each sample below is produced by running
/// [`compare_chunk`] on a pair of trees whose difference is the one the pattern
/// exists to permit, so a pattern can only be reachable if it matches something
/// the gate can actually say.
#[test]
fn the_allowlist_matcher_is_tested_against_paths_the_gate_really_emits() {
    // A chunk pair differing in exactly the ways a real vanilla round trip
    // differs, built through the same comparison the gate uses.
    let ours = chunk_with_one_section(-4, &["minecraft:air"], 0, 0);
    let mut theirs = ours.clone();
    let Nbt::Compound(fields) = &mut theirs else {
        panic!("compound");
    };
    fields.push((
        "Heightmaps".into(),
        Nbt::Compound(vec![("MOTION_BLOCKING".into(), Nbt::LongArray(vec![0; 37]))]),
    ));
    fields.push((
        "PostProcessing".into(),
        Nbt::List {
            element_type: lodestone_core::NbtTag::List,
            elements: vec![],
        },
    ));
    if let Some((_, Nbt::List { elements, .. })) =
        fields.iter_mut().find(|(name, _)| name == "sections")
    {
        if let Nbt::Compound(section) = &mut elements[0] {
            section.push(("SkyLight".into(), Nbt::ByteArray(vec![0; 2048])));
            section.push(("BlockLight".into(), Nbt::ByteArray(vec![0; 2048])));
        }
    }

    let emitted = compare_chunk(&ours, &theirs);
    assert!(!emitted.is_empty(), "control: this pair really does differ");

    // Every emitted difference must be allowlisted. If a pattern is written in
    // a shape the comparison cannot produce, it fails here rather than on a
    // live run twenty minutes later.
    let unmatched: Vec<String> = emitted
        .iter()
        .filter(|d| allowed_for(d).is_none())
        .map(|d| d.to_string())
        .collect();
    assert!(
        unmatched.is_empty(),
        "these differences are the exact ones the allowlist exists to permit, and no pattern \
         matched them — a pattern is written in a path shape this gate never emits:\n  {}",
        unmatched.join("\n  ")
    );

    // A second pair, for the patterns only reachable when *both* sides carry
    // the field: direction B hands vanilla chunks that already have heightmaps,
    // so `Heightmaps.**` is matched against an inner path rather than against
    // the compound's own addition.
    let mut ours_with_maps = ours.clone();
    let Nbt::Compound(fields) = &mut ours_with_maps else {
        panic!("compound");
    };
    fields.push((
        "Heightmaps".into(),
        Nbt::Compound(vec![("MOTION_BLOCKING".into(), Nbt::LongArray(vec![7; 37]))]),
    ));
    let emitted_inner = compare_chunk(&ours_with_maps, &theirs);
    assert!(
        emitted_inner
            .iter()
            .any(|d| d.path == "Heightmaps.MOTION_BLOCKING[0]"),
        "control: differing heightmap contents must be reported at an inner path: \
         {emitted_inner:#?}"
    );

    // Each pattern had to be the one that did the matching for at least one
    // emitted difference, so a single over-broad pattern cannot stand in for
    // the rest and an unreachable pattern cannot sit in the list unnoticed.
    let all: Vec<&Difference> = emitted.iter().chain(&emitted_inner).collect();
    for pattern in [
        "Heightmaps",
        "Heightmaps.**",
        "PostProcessing",
        "sections[Y=*].SkyLight.**",
        "sections[Y=*].BlockLight.**",
    ] {
        assert!(
            all.iter()
                .any(|d| allowed_for(d).is_some_and(|e| e.pattern == pattern)),
            "no emitted difference was matched by {pattern:?}, so that pattern is unreachable \
             and cannot be trusted to permit anything"
        );
    }

    // `isLightOn` and the two clocks are scalars our writer always emits and
    // vanilla always rewrites, so they are exercised by a value change rather
    // than by an addition.
    for (name, ours_value, theirs_value) in [
        ("isLightOn", Nbt::Byte(0), Nbt::Byte(1)),
        ("LastUpdate", Nbt::Long(0), Nbt::Long(1234)),
        ("InhabitedTime", Nbt::Long(0), Nbt::Long(20)),
    ] {
        let l = Nbt::Compound(vec![(name.to_string(), ours_value)]);
        let r = Nbt::Compound(vec![(name.to_string(), theirs_value)]);
        let differences = compare_chunk(&l, &r);
        assert_eq!(differences.len(), 1, "{differences:#?}");
        assert!(
            allowed_for(&differences[0]).is_some_and(|e| e.pattern == name),
            "{name} must be permitted as a value change: {:?}",
            differences[0]
        );
    }
}

#[test]
fn the_allowlist_matcher_permits_only_what_it_names() {
    // The allowlist is the load-bearing part of the gate, so its matcher gets
    // a control in both directions. An over-broad pattern is how a parity gate
    // becomes vacuous while still looking rigorous.
    assert!(path_matches("sections[Y=*].SkyLight.**", "sections[Y=7].SkyLight"));
    assert!(path_matches("sections[Y=*].SkyLight.**", "sections[Y=-4].SkyLight"));
    assert!(
        path_matches("sections[Y=*].SkyLight.**", "sections[Y=-4].SkyLight[1234]"),
        "a `.**` entry must reach the array's own elements, or the array would be permitted \
         while every differing byte inside it was reported"
    );
    assert!(path_matches("Heightmaps.**", "Heightmaps"));
    assert!(path_matches("Heightmaps.**", "Heightmaps.MOTION_BLOCKING"));

    // The rejections are the point.
    assert!(
        !path_matches("Heightmaps.**", "HeightmapsExtra"),
        "a `.**` suffix must not match a longer sibling name"
    );
    assert!(
        !path_matches("sections[Y=*].SkyLight.**", "sections[Y=7].block_states.data"),
        "an exact pattern must not match a different field"
    );
    assert!(
        !path_matches("sections[Y=*].SkyLight.**", "sections[7].SkyLight"),
        "the plain-index form is not what compare_sections emits, and a pattern must not \
         quietly match both — that ambiguity is what hid the original bug"
    );
    assert!(
        !path_matches("isLightOn", "sections[3].isLightOn"),
        "a root-level pattern must not match a nested field of the same name"
    );

    // And nothing in the shipped allowlist may reach a block-state, biome,
    // block-entity or identity field. This is the assertion that would fail if
    // somebody widened an entry to make a run green.
    for forbidden in [
        "sections[Y=3].block_states.data[0]",
        "sections[Y=3].block_states.palette[7].Name",
        "sections[Y=3].block_states.cell[x=1,y=2,z=3]",
        "sections[Y=3].biomes.palette[0]",
        "sections[Y=3].biomes.cell[x=1,y=2,z=3]",
        "sections[Y=3]",
        "block_entities[97,-59,199].Items[0].id",
        "Status",
        "xPos",
        "yPos",
        "zPos",
        "DataVersion",
        "block_ticks",
        "fluid_ticks",
        "structures.starts",
    ] {
        for entry in ALLOWED {
            assert!(
                !path_matches(entry.pattern, forbidden),
                "allowlist entry {:?} matches {forbidden:?}, which this gate must never \
                 tolerate a difference in",
                entry.pattern
            );
        }
    }
}

#[test]
fn every_allowlist_entry_carries_a_real_reason() {
    // A reason is what a future reader checks before believing an entry, so an
    // empty or perfunctory one is itself a defect. The length floor is crude
    // on purpose: it cannot judge quality, only catch "vanilla changes this".
    for entry in ALLOWED {
        assert!(
            entry.reason.len() > 80,
            "allowlist entry {:?} has no substantive justification: {:?}",
            entry.pattern,
            entry.reason
        );
    }
}

#[test]
fn the_paletted_decoder_reproduces_the_non_spanning_rule() {
    // The one rule that silently corrupts everything when guessed wrong. The
    // expected long counts here come from real 26.2 measurements recorded in
    // `lodestone_server::chunk_nbt`'s module doc (a 20-entry palette taking
    // **342** longs, not 320), not from this decoder.
    //
    // A 20-entry palette needs ceil_log2(20) = 5 bits, 12 values per long,
    // ceil(4096 / 12) = 342 longs. Under the (wrong) dense/spanning rule it
    // would be ceil(4096 * 5 / 64) = 320.
    assert_eq!(ceil_log2(20), 5);
    let values_per_long = 64 / 5;
    assert_eq!(values_per_long, 12);
    assert_eq!(4096usize.div_ceil(values_per_long), 342);
    assert_ne!(
        4096usize.div_ceil(values_per_long),
        (4096 * 5usize).div_ceil(64),
        "control: the non-spanning and dense hypotheses really do disagree at 20 entries — \
         if they agreed, this test would prove nothing about which rule is implemented"
    );

    // And the floors, which differ between the two containers.
    assert_eq!(ceil_log2(6).max(PalettedKind::BlockStates.bits_floor()), 4);
    assert_eq!(ceil_log2(2).max(PalettedKind::Biomes.bits_floor()), 1);
    assert_eq!(4096usize.div_ceil(64 / 4), 256, "a 4-bit section is 256 longs");
    assert_eq!(64usize.div_ceil(64 / 1), 1, "a 1-bit biome grid is one long");
}

#[test]
fn a_single_entry_palette_decodes_without_a_data_array() {
    // How vanilla writes an all-air or all-stone section. Erroring here would
    // fail on most sections in any world.
    let container = Nbt::Compound(vec![(
        "palette".to_string(),
        Nbt::List {
            element_type: lodestone_core::NbtTag::Compound,
            elements: vec![Nbt::Compound(vec![(
                "Name".to_string(),
                Nbt::String("minecraft:air".to_string()),
            )])],
        },
    )]);
    let cells = decode_paletted(&container, PalettedKind::BlockStates).expect("decodes");
    assert_eq!(cells.len(), 4096);
    assert!(cells.iter().all(|c| c == "minecraft:air"));

    // Control: a multi-entry palette with no `data` is a real inconsistency
    // and must NOT be flattened to entry 0 — that would hide a writer that
    // dropped the array.
    let container = Nbt::Compound(vec![(
        "palette".to_string(),
        Nbt::List {
            element_type: lodestone_core::NbtTag::Compound,
            elements: vec![
                Nbt::Compound(vec![("Name".into(), Nbt::String("minecraft:air".into()))]),
                Nbt::Compound(vec![("Name".into(), Nbt::String("minecraft:stone".into()))]),
            ],
        },
    )]);
    assert!(decode_paletted(&container, PalettedKind::BlockStates).is_err());
}

#[test]
fn palette_properties_are_compared_order_insensitively() {
    // Nothing makes two writers emit a state's properties in the same order,
    // and an unsorted rendering would report every multi-property state in the
    // world as changed.
    let forward = Nbt::Compound(vec![
        ("Name".into(), Nbt::String("minecraft:oak_stairs".into())),
        (
            "Properties".into(),
            Nbt::Compound(vec![
                ("facing".into(), Nbt::String("north".into())),
                ("half".into(), Nbt::String("bottom".into())),
            ]),
        ),
    ]);
    let reverse = Nbt::Compound(vec![
        (
            "Properties".into(),
            Nbt::Compound(vec![
                ("half".into(), Nbt::String("bottom".into())),
                ("facing".into(), Nbt::String("north".into())),
            ]),
        ),
        ("Name".into(), Nbt::String("minecraft:oak_stairs".into())),
    ]);
    assert_ne!(forward, reverse, "control: the raw NBT really is different");
    assert_eq!(
        canonical_palette_entry(&forward),
        canonical_palette_entry(&reverse)
    );
    assert_eq!(
        canonical_palette_entry(&forward),
        "minecraft:oak_stairs[facing=north,half=bottom]"
    );
}

#[test]
fn the_section_comparison_locates_a_single_changed_block() {
    // The detector control for the part of the gate that matters most: one
    // block changed in one section must come back as one located difference
    // with a coordinate, not as "chunks differ".
    // Both are all-air except cell 0; in `theirs` that one cell is stone.
    let ours = chunk_with_one_section(0, &["minecraft:air", "minecraft:stone"], 0, 0);
    let theirs = chunk_with_one_section(0, &["minecraft:air", "minecraft:stone"], 0, 1);

    let differences = compare_chunk(&ours, &theirs);
    let located: Vec<&Difference> = differences
        .iter()
        .filter(|d| d.path.contains(".cell["))
        .collect();
    assert_eq!(located.len(), 1, "{differences:#?}");
    assert_eq!(
        located[0].path, "sections[Y=0].block_states.cell[x=0,y=0,z=0]",
        "the path must carry the coordinate"
    );
    assert!(
        differences.iter().any(|d| d.path.ends_with("<summary>")),
        "a bounding-box summary must accompany the located cells: {differences:#?}"
    );
    // And it is not allowlisted — the whole point.
    assert!(
        allowed_for(located[0]).is_none(),
        "a changed block must never be allowlisted"
    );
}

#[test]
fn the_section_comparison_ignores_a_reordered_palette() {
    // The counter-control: the same blocks with the palette written in the
    // opposite order, and therefore different packed `long`s, must compare
    // equal. A gate comparing packed longs would fail here — measuring the
    // palette's order rather than the world.
    // Both sections hold: cell 0 = stone, every other cell = air. The palettes
    // are written in opposite orders, so the packed `long`s differ — which is
    // precisely what a byte or packed-long comparison would report as a
    // difference and what a decoded comparison must not.
    let ours = chunk_with_one_section(0, &["minecraft:air", "minecraft:stone"], 0, 1);
    let theirs = chunk_with_one_section(0, &["minecraft:stone", "minecraft:air"], 1, 0);

    let ours_data = section_data(&ours);
    let theirs_data = section_data(&theirs);
    assert_ne!(
        ours_data, theirs_data,
        "control: the two packed `data` arrays really are different bytes, so the decoded \
         comparison below is doing real work rather than comparing two identical encodings"
    );

    let differences = compare_chunk(&ours, &theirs);
    assert!(
        differences.is_empty(),
        "identical blocks under a reordered palette must compare equal: {differences:#?}"
    );
}

/// The `block_states.data` array of a single-section chunk built by
/// [`chunk_with_one_section`], for the control above.
fn section_data(chunk: &Nbt) -> Vec<i64> {
    let section = &list_elements(field(chunk, "sections"))[0];
    match field(field(section, "block_states").expect("has block_states"), "data") {
        Some(Nbt::LongArray(data)) => data.clone(),
        other => panic!("expected a LongArray `data`, got {other:?}"),
    }
}

/// A minimal one-section chunk: every cell holds `palette[background]` except
/// cell 0, which holds `palette[cell0]`.
///
/// `background` is separate from `palette[0]` on purpose. An earlier version of
/// this helper always filled with index 0, which made the reordered-palette
/// control above compare a mostly-air section against a mostly-*stone* one and
/// report 4,095 differences — the helper, not the gate, was wrong. The two
/// indices have to be nameable independently for the same *world* to be
/// expressible under two palette orders.
fn chunk_with_one_section(section_y: i8, palette: &[&str], background: u64, cell0: u64) -> Nbt {
    let bits = ceil_log2(palette.len()).max(4) as usize;
    let values_per_long = 64 / bits;
    let mut data = vec![0i64; 4096usize.div_ceil(values_per_long)];
    // Fill every cell with `background`, then overwrite cell 0.
    for cell in 0..4096usize {
        let word = &mut data[cell / values_per_long];
        let shift = (cell % values_per_long) * bits;
        *word |= (background << shift) as i64;
    }
    let mask = ((1u64 << bits) - 1) as i64;
    data[0] = (data[0] & !mask) | (cell0 as i64 & mask);
    Nbt::Compound(vec![
        ("DataVersion".into(), Nbt::Int(4903)),
        (
            "sections".into(),
            Nbt::List {
                element_type: lodestone_core::NbtTag::Compound,
                elements: vec![Nbt::Compound(vec![
                    ("Y".into(), Nbt::Byte(section_y)),
                    (
                        "block_states".into(),
                        Nbt::Compound(vec![
                            (
                                "palette".into(),
                                Nbt::List {
                                    element_type: lodestone_core::NbtTag::Compound,
                                    elements: palette
                                        .iter()
                                        .map(|name| {
                                            Nbt::Compound(vec![(
                                                "Name".into(),
                                                Nbt::String((*name).to_string()),
                                            )])
                                        })
                                        .collect(),
                                },
                            ),
                            ("data".into(), Nbt::LongArray(data.clone())),
                        ]),
                    ),
                ])],
            },
        ),
    ])
}

#[test]
fn a_light_only_section_is_reported_by_its_y_not_swallowed_by_a_length_change() {
    // Vanilla writes light-only sections one past each end of the vertical
    // extent, so the two `sections` lists differ in length. A purely
    // structural differ collapses that into one `LengthChanged` and loses
    // every section's contents; this gate must key by `Y` instead.
    let ours = chunk_with_one_section(0, &["minecraft:air"], 0, 0);
    let mut theirs = ours.clone();
    if let Nbt::Compound(fields) = &mut theirs {
        if let Some((_, Nbt::List { elements, .. })) =
            fields.iter_mut().find(|(name, _)| name == "sections")
        {
            elements.push(Nbt::Compound(vec![
                ("Y".into(), Nbt::Byte(-5)),
                ("SkyLight".into(), Nbt::ByteArray(vec![0; 2048])),
            ]));
        }
    }

    let structural = nbt_diff::diff(&ours, &theirs);
    assert!(
        structural
            .iter()
            .any(|d| matches!(d.kind, DifferenceKind::LengthChanged { .. })),
        "control: a purely structural differ DOES collapse this into a length change, so the \
         keyed comparison below is doing real work: {structural:#?}"
    );

    let differences = compare_chunk(&ours, &theirs);
    assert_eq!(differences.len(), 1, "{differences:#?}");
    assert_eq!(differences[0].path, "sections[Y=-5]");
    assert!(matches!(differences[0].kind, DifferenceKind::Added { .. }));
}

#[test]
fn a_dropped_block_entity_is_located_and_never_allowlisted() {
    // The destructive-persistence shape this gate exists to catch: a chunk
    // that came back with one fewer chest. The report must name the
    // coordinate, and no allowlist entry may permit it.
    let chest = Nbt::Compound(vec![
        ("id".into(), Nbt::String("minecraft:chest".into())),
        ("x".into(), Nbt::Int(97)),
        ("y".into(), Nbt::Int(-59)),
        ("z".into(), Nbt::Int(199)),
    ]);
    let ours = Nbt::Compound(vec![(
        "block_entities".into(),
        Nbt::List {
            element_type: lodestone_core::NbtTag::Compound,
            elements: vec![chest],
        },
    )]);
    let theirs = Nbt::Compound(vec![(
        "block_entities".into(),
        Nbt::List {
            element_type: lodestone_core::NbtTag::End,
            elements: vec![],
        },
    )]);

    let differences = compare_chunk(&ours, &theirs);
    assert_eq!(differences.len(), 1, "{differences:#?}");
    assert_eq!(differences[0].path, "block_entities[97,-59,199]");
    assert!(matches!(differences[0].kind, DifferenceKind::Removed { .. }));
    assert!(allowed_for(&differences[0]).is_none());
}

// ---------------------------------------------------------------------------
// Direction A — we write a fresh world, vanilla loads and saves it
// ---------------------------------------------------------------------------

/// **Direction A, the owner's ask.** Generate a fresh world with our real
/// generator, write it with our real save path, hand it to a real vanilla 26.2
/// server, let vanilla load and save it, read it back, and require semantic
/// identity of everything we authored.
///
/// Fixed seed [`SEED`]; chunk block [`CHUNKS_A`]. Requires Apple `container`
/// and `.cache/mc/26.2`; a missing runtime fails loudly rather than skipping.
#[test]
#[ignore = "boots a real vanilla 26.2 server in a container; run with --ignored --nocapture"]
fn our_fresh_world_survives_a_vanilla_load_and_save() {
    let root = case_dir("direction-a");
    let world = root.join("world");
    std::fs::create_dir_all(&world).expect("create the world directory");

    // --- generate ---------------------------------------------------------
    let source = lodestone_server::overworld_chunk_source(SEED);
    assert_eq!(
        (source.min_y(), source.height()),
        (OVERWORLD_MIN_Y, OVERWORLD_HEIGHT),
        "the generator's vertical extent is not the 26.2 overworld's; a mismatch silently \
         mis-slices every saved section"
    );

    let coords: Vec<(i32, i32)> = CHUNKS_A
        .clone()
        .flat_map(|cx| CHUNKS_A.clone().map(move |cz| (cx, cz)))
        .collect();
    let mut ours: BTreeMap<(i32, i32), Nbt> = BTreeMap::new();
    for &(cx, cz) in &coords {
        let column = lodestone_server::ChunkSource::column(&source, cx, cz);
        ours.insert((cx, cz), chunk_nbt::column_to_nbt(cx, cz, &column));
    }

    // --- the hard precondition -------------------------------------------
    let before_census = census(&ours);
    eprintln!("--- direction A fixture census ---\n{before_census:#?}");
    assert_fixture_is_worth_testing("direction A", &before_census, coords.len(), 12, 16);
    // Measured, and reported rather than asserted-away: our generator produces
    // **no block entities at all** — there is no block-entity layer
    // on the generator path, so even a generated bee nest ships empty. That is
    // why the block-entity half of this gate lives in direction B, where the
    // fixture is a world a real server authored.
    eprintln!(
        "direction A: the generator produced {} block entities across {} chunks (issue #520 \
         predicts 0); the block-entity half of save parity is direction B's job",
        before_census.block_entities, before_census.chunks
    );
    // Reported rather than asserted, so the number is in every run's transcript
    // and shrinks visibly when the worldgen fix lands. Deliberately *not* an
    // assertion: the parity comparison below is what must fail on it, and a
    // second assertion here would just make the first failure harder to read.
    eprintln!(
        "direction A: {} section(s) carry BOTH spellings of one fluid state \
         (`minecraft:water` and `minecraft:water[level=0]`, or the lava pair). Vanilla's own \
         noise settings put `\"Properties\": {{\"level\": \"0\"}}` on `default_fluid`; \
         `crates/lodestone-worldgen/src/overworld/mod.rs` reads only `[\"Name\"]` and drops it, \
         while `crates/lodestone-worldgen/src/carver/mod.rs` uses the canonical form — so one \
         column can hold two palette entries for one block state.",
        before_census.sections_with_both_fluid_spellings
    );

    // --- write ------------------------------------------------------------
    write_world(&world, &ours);
    level_dat::write_to_file(
        &level_dat::LevelDat::for_new_world("world", &level_dat::Spawn::default(), 0),
        &level_dat::path_in(&world),
    )
    .expect("write level.dat");

    // `world_gen_settings.dat` comes from the checked-in **real vanilla** file
    // rather than from `WorldGenSettings::from_seed`, which writes no
    // `dimensions` compound and so produces a file `WorldGenSettings.CODEC`
    // rejects — vanilla then falls back to a random seed. That gap is named in
    // `world_gen_settings`'s own module doc, and reusing the fixture's seed as
    // this test's SEED is what makes the file self-consistent.
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/support/world_gen_settings_26_2_vanilla.dat");
    let settings = world_gen_settings::read_from_file(&fixture)
        .expect("the checked-in vanilla world_gen_settings fixture decodes");
    assert_eq!(
        settings.seed().expect("the fixture has a seed"),
        SEED,
        "SEED must be the checked-in fixture's own seed, so the settings file we hand vanilla \
         has its seed and its dimensions tree from one real world"
    );
    assert!(
        settings.has_dimensions(),
        "control: the fixture really is a full vanilla file, not a stub one of our own writers \
         could have produced"
    );
    world_gen_settings::write_to_file(&settings, &world_gen_settings::path_in(&world))
        .expect("write world_gen_settings.dat");

    let snapshot = snapshot_regions(&world);
    assert_eq!(snapshot.len(), 4, "the 3x3 block around the origin spans four region files");

    // --- hand to vanilla --------------------------------------------------
    hand_to_vanilla(&root, "world", &[FORCELOAD_A]);
    assert_vanilla_accepted_the_world(&root);

    // --- read back and compare -------------------------------------------
    let theirs = read_world(&world, &coords);
    assert_eq!(
        theirs.len(),
        coords.len(),
        "vanilla's save is missing chunks we wrote: read back {} of {}",
        theirs.len(),
        coords.len()
    );
    assert_vanilla_rewrote("direction A", &snapshot, &theirs);

    let after_census = census(&theirs);
    eprintln!("--- direction A post-vanilla census ---\n{after_census:#?}");

    let mut per_chunk = BTreeMap::new();
    for &coord in &coords {
        per_chunk.insert(coord, compare_chunk(&ours[&coord], &theirs[&coord]));
    }
    assert_parity("direction A (we wrote -> vanilla rewrote)", &per_chunk);

    // The seed survived, which is the one thing no blocks-only assertion can
    // check: a rejected world_gen_settings.dat re-rolls it and every chunk we
    // wrote still loads.
    let reloaded = world_gen_settings::read_from_file(&world_gen_settings::path_in(&world))
        .expect("world_gen_settings.dat is still readable after vanilla's save");
    assert_eq!(
        reloaded.seed().expect("still has a seed"),
        SEED,
        "vanilla re-rolled the world seed, so reopening this world would regenerate every \
         unvisited chunk from a different one (issue #468's defect)"
    );
}

// ---------------------------------------------------------------------------
// Direction B — vanilla wrote it, we load and re-write it, vanilla reads again
// ---------------------------------------------------------------------------

/// **Direction B, the reverse.** Take a region file a real 26.2 server wrote,
/// load it through our reader, write it back out through our writer, hand *that*
/// to a real vanilla server, and require the chunks vanilla saves to still match
/// the ones it originally wrote.
///
/// This catches what direction A structurally cannot: our **reader** dropping
/// data we do not model — persistence turning a missing feature into a
/// destructive one, where a world opened here comes back with its chests
/// emptied. That destructive-persistence bug is fixed (the `BlockEntity::Opaque`
/// passthrough landed), so this direction is also the end-to-end evidence that
/// the passthrough survives a real JVM rather than only our own re-read.
#[test]
#[ignore = "boots a real vanilla 26.2 server in a container; run with --ignored --nocapture"]
fn a_real_vanilla_world_survives_our_load_and_save() {
    let source_world = repo_root().join(VANILLA_WORLD_B);
    let source_region = source_world.join(OVERWORLD_REGION_DIR).join(REGION_B);
    let region = RegionFile::read_from_file(&source_region).unwrap_or_else(|e| {
        panic!(
            "no real vanilla region file at {} ({e}); .cache/mc/<world> is not checked in and \
             must be regenerated by that version's live oracle first \
             (scripts/live-oracles/survival.sh). This gate must fail rather than skip when the \
             fixture is absent.",
            source_region.display()
        )
    });

    // The written block is `WRITTEN_B` wide; the compared block is the
    // `COMPARED_B` interior of it. See [`WRITTEN_B`] for the measured reason.
    let (block_x, block_z) = densest_chunk_block(&region, WRITTEN_B, COMPARED_B);
    let written: Vec<(i32, i32)> = (block_x..block_x + WRITTEN_B)
        .flat_map(|cx| (block_z..block_z + WRITTEN_B).map(move |cz| (cx, cz)))
        .collect();
    let coords: Vec<(i32, i32)> = (block_x + 1..block_x + 1 + COMPARED_B)
        .flat_map(|cx| (block_z + 1..block_z + 1 + COMPARED_B).map(move |cz| (cx, cz)))
        .collect();
    assert_eq!(written.len(), (WRITTEN_B * WRITTEN_B) as usize);
    assert_eq!(coords.len(), (COMPARED_B * COMPARED_B) as usize);

    // `forceload` takes **block** coordinates and derives the chunk, so a
    // chunk-coordinate rectangle here would force a 16th of the intended area
    // and silently shrink the compared set to whatever vanilla happened to load.
    // The forced area is the **written** block, margin included: an unloaded
    // margin chunk is not a neighbour vanilla can decorate *from*, which would
    // defeat the margin's purpose.
    let forceload = format!(
        "forceload add {} {} {} {}",
        block_x * 16,
        block_z * 16,
        (block_x + WRITTEN_B) * 16 - 1,
        (block_z + WRITTEN_B) * 16 - 1
    );

    // --- what vanilla originally wrote -----------------------------------
    // Read the whole written block: the margin has to be written out too, or
    // the seam simply moves inward by one chunk.
    let mut vanilla_original: BTreeMap<(i32, i32), Nbt> = BTreeMap::new();
    for &(cx, cz) in &written {
        let (_, _, local_x, local_z) = region::region_and_local(cx, cz);
        let bytes = region
            .read_chunk_nbt_bytes(local_x, local_z)
            .unwrap_or_else(|e| panic!("read chunk ({cx}, {cz}) from the real world: {e}"))
            .unwrap_or_else(|| {
                panic!(
                    "chunk ({cx}, {cz}) is absent from {}; this gate's compared set must be \
                     fully populated in the fixture",
                    source_region.display()
                )
            });
        let mut reader = lodestone_core::Reader::new(&bytes);
        let (_, nbt) = lodestone_core::read_named_nbt(&mut reader)
            .unwrap_or_else(|e| panic!("decode vanilla chunk ({cx}, {cz}): {e}"));
        vanilla_original.insert((cx, cz), nbt);
    }

    // --- the hard precondition -------------------------------------------
    // Measured over the **compared** subset, not the written one: a precondition
    // asserting content that lives only in the uncompared margin would be the
    // *world* species of vacuous test wearing a precondition's clothes.
    let compared_only: BTreeMap<(i32, i32), Nbt> = coords
        .iter()
        .map(|coord| (*coord, vanilla_original[coord].clone()))
        .collect();
    let source_census = census(&compared_only);
    eprintln!("--- direction B fixture census (compared interior only) ---\n{source_census:#?}");
    assert_fixture_is_worth_testing("direction B", &source_census, coords.len(), 30, 16);
    assert_eq!(
        source_census.min_section_y,
        i64::from(OVERWORLD_MIN_Y / 16),
        "the fixture's lowest section is not the 26.2 overworld's, so the min_y this test \
         reconstructs columns with would mis-slice every section"
    );
    // The whole reason direction B exists: unmodelled block entities. Without
    // them the run is the *world* species of vacuous test — it would pass on
    // an empty list either way.
    // The floor is 100, not 1. `densest_chunk_block` selects for this, and the
    // current oracle world yields 145 across 8 kinds — so a fixture that
    // collapsed to a handful means the oracle world was regenerated into
    // something without a structure in it, and the direction would be testing
    // terrain preservation under a name that promises block entities.
    //
    // An earlier version of this test compared the fixed chunk block `0..=7`
    // and this assertion fired at **6** block entities, which is exactly what
    // it is for: that run would otherwise have reported a green
    // block-entity-preservation result off six containers.
    assert!(
        source_census.block_entities >= 100,
        "direction B: the fixture holds only {} block entities (need >= 100). This direction \
         exists to catch our reader dropping the ones it cannot model (#477), and a thin \
         fixture passes either way. `densest_chunk_block` selects for density, so this means \
         the oracle world at {VANILLA_WORLD_B} no longer contains a structure — regenerate it \
         with scripts/live-oracles/survival.sh and explore.",
        source_census.block_entities
    );
    assert!(
        source_census.distinct_block_entity_ids.len() >= 6,
        "direction B: only {} distinct block-entity id(s) (need >= 6): {:?}. #477's finding was \
         that 1,608 of 1,613 kinds are unmodelled, so a fixture carrying one or two kinds \
         barely exercises the passthrough. The current oracle world yields 8.",
        source_census.distinct_block_entity_ids.len(),
        source_census.distinct_block_entity_ids
    );
    // At least one kind our server does **not** simulate, i.e. one that can
    // only survive via the verbatim `BlockEntity::Opaque` passthrough.
    const MODELLED: &[&str] = &[
        "minecraft:furnace",
        "minecraft:smoker",
        "minecraft:blast_furnace",
        "minecraft:hopper",
        "minecraft:brewing_stand",
        "lodestone:composter",
    ];
    let unmodelled: Vec<&String> = source_census
        .distinct_block_entity_ids
        .iter()
        .filter(|id| !MODELLED.contains(&id.as_str()))
        .collect();
    assert!(
        !unmodelled.is_empty(),
        "direction B: every block-entity kind in the fixture is one we simulate ({:?}), so \
         nothing here exercises the verbatim passthrough that #477 added",
        source_census.distinct_block_entity_ids
    );
    eprintln!("direction B: unmodelled block-entity kinds in the fixture: {unmodelled:?}");

    // --- our reader, then our writer -------------------------------------
    let mut ours: BTreeMap<(i32, i32), Nbt> = BTreeMap::new();
    for &(cx, cz) in &written {
        let original = &vanilla_original[&(cx, cz)];
        let column = chunk_nbt::column_from_nbt(original, OVERWORLD_MIN_Y, OVERWORLD_HEIGHT)
            .unwrap_or_else(|e| panic!("our reader could not decode vanilla chunk ({cx}, {cz}): {e}"));
        let extras = chunk_nbt::extras_from_nbt(original);
        ours.insert(
            (cx, cz),
            chunk_nbt::column_to_nbt_with(cx, cz, &column, &extras),
        );
    }
    let rewritten_interior: BTreeMap<(i32, i32), Nbt> = coords
        .iter()
        .map(|coord| (*coord, ours[coord].clone()))
        .collect();
    let rewritten_census = census(&rewritten_interior);
    eprintln!("--- direction B after our load/save (interior) ---\n{rewritten_census:#?}");

    // **The hermetic half, asserted before vanilla is involved at all.**
    //
    // This separates two questions the post-vanilla comparison cannot: whether
    // *our* reader/writer preserved the counts, and whether *vanilla* then
    // changed them by simulating. It is also what licenses the clock entries in
    // ALLOWED — a difference in a block-entity tick counter after the round trip
    // is only attributable to vanilla's tick loop if our own rewrite is known to
    // have preserved the counter exactly, and that is checked here rather than
    // assumed.
    assert_eq!(
        rewritten_census.block_entities, source_census.block_entities,
        "our reader/writer changed the block-entity count before vanilla was involved: {} in, \
         {} out. This is the #477 shape and it is ours, not vanilla's.",
        source_census.block_entities, rewritten_census.block_entities
    );
    assert_eq!(
        rewritten_census.distinct_block_entity_ids, source_census.distinct_block_entity_ids,
        "our reader/writer changed which block-entity kinds exist before vanilla was involved"
    );
    assert_eq!(
        (rewritten_census.block_ticks, rewritten_census.fluid_ticks),
        (source_census.block_ticks, source_census.fluid_ticks),
        "our reader/writer changed the pending block/fluid tick counts before vanilla was \
         involved: {:?} in, {:?} out (issue #468's other half)",
        (source_census.block_ticks, source_census.fluid_ticks),
        (rewritten_census.block_ticks, rewritten_census.fluid_ticks)
    );

    // The premise of the light allowlist entries: we must author **no light at
    // all**, or `Allow::VanillaOwnsEntirely` on those paths would hide a wrong
    // light array we did write. Checked, not assumed.
    let ours_light_arrays: usize = ours
        .values()
        .flat_map(|chunk| list_elements(field(chunk, "sections")))
        .filter(|section| {
            field(section, "BlockLight").is_some() || field(section, "SkyLight").is_some()
        })
        .count();
    assert_eq!(
        ours_light_arrays, 0,
        "our writer emitted light arrays in {ours_light_arrays} section(s). The \
         `sections[Y=*].BlockLight`/`SkyLight` allowlist entries are justified *only* by our \
         authoring none of it; the moment we do, those entries hide a real defect and must be \
         removed."
    );

    // --- assemble a world vanilla can open -------------------------------
    let root = case_dir("direction-b");
    let world = root.join("world");
    std::fs::create_dir_all(&world).expect("create the world directory");
    write_world(&world, &ours); // the whole written block, margin included
    // `level.dat` and the `data/` tree come from the real world verbatim: this
    // direction's subject is the chunk schema, and re-deriving the metadata
    // would mix a second variable into the result.
    std::fs::copy(
        level_dat::path_in(&source_world),
        level_dat::path_in(&world),
    )
    .expect("copy the real world's level.dat");
    let settings_src = world_gen_settings::path_in(&source_world);
    let settings_dst = world_gen_settings::path_in(&world);
    std::fs::create_dir_all(settings_dst.parent().expect("has a parent"))
        .expect("create data/minecraft");
    std::fs::copy(&settings_src, &settings_dst)
        .unwrap_or_else(|e| panic!("copy {} ({e})", settings_src.display()));

    let snapshot = snapshot_regions(&world);

    // --- hand to vanilla --------------------------------------------------
    hand_to_vanilla(&root, "world", &[forceload.as_str()]);
    assert_vanilla_accepted_the_world(&root);

    let theirs = read_world(&world, &coords);
    assert_eq!(theirs.len(), coords.len(), "vanilla's save is missing chunks we wrote");
    assert_vanilla_rewrote("direction B", &snapshot, &theirs);

    // --- compare against what vanilla ORIGINALLY wrote --------------------
    // Not against our rewrite: the question is whether a round trip through
    // our reader and writer preserved the world a real server authored, so the
    // reference is that server's own output.
    let mut per_chunk = BTreeMap::new();
    for &coord in &coords {
        per_chunk.insert(
            coord,
            compare_chunk(&vanilla_original[&coord], &theirs[&coord]),
        );
    }
    assert_parity(
        "direction B (vanilla wrote -> we rewrote -> vanilla rewrote)",
        &per_chunk,
    );

    // Counted separately from the path comparison, because a count is what
    // this gate's requirement asks for and a per-path report can be read as
    // "a few small differences" when it is really every container in the
    // region.
    let after_census = census(&theirs);
    assert_eq!(
        after_census.block_entities, source_census.block_entities,
        "block-entity count changed across the round trip: {} originally, {} after. #477's \
         requirement is the count, not the presence of one chest.",
        source_census.block_entities, after_census.block_entities
    );
}
