//! The per-block-state **`SoundType`** census generated into
//! `src/generated/sound_types.rs` and read through
//! [`lodestone_data::sound_types`].
//!
//! Modelled on `shade_brightness.rs` and `hardness.rs`: generate-or-assert with
//! `LODESTONE_REGEN=1`, anchored to a committed JVM dump.
//!
//! # What is being anchored
//!
//! Vanilla's own level-event handler's break case plays
//! `Block.stateById(data).getSoundType().getBreakSound()` at
//! `(soundType.getVolume() + 1.0F) / 2.0F` and `soundType.getPitch() * 0.8F`. The
//! packet carries only the state id, so the sound is a **local lookup** — which
//! is why every block break in lodestone was silent while the event was decoded,
//! routed and handled (`docs/sound-playback.md`).
//!
//! # Data provenance
//!
//! `tests/support/sound_types_jvm.txt` is an authoritative dump produced by
//! booting the real 26.2 server (`oracle-java/SoundTypeOracle.java`) and reading
//! vanilla's own "get sound type" accessor per block state. It carries:
//!
//! * `C <states> <blocks> <distinctValues> <distinctIdentities>` — the two
//!   distinct counts, so [`value_dedup_collapses_nothing`] can *measure* whether
//!   deduplicating by value merges two vanilla statics rather than assuming it
//!   does not.
//! * `N <registryId> <name>` — every sound event any row references, from the
//!   **live registry**. [`dump_sound_event_ids_agree_with_the_registries_json_table`]
//!   crosses it against `src/generated/sound_events.rs`, which was generated from
//!   Mojang's `registries.json` instead; agreement is what licenses this table to
//!   store bare ids and no names.
//! * `T <index> <volumeBits> <pitchBits> <break> <step> <place> <hit> <fall>` —
//!   the deduplicated table, floats as raw bits so no decimal formatting sits
//!   between the JVM and the assertion.
//! * `O <block> <class>` — the per-state sound-type override census by
//!   reflection, which makes "exactly one block is per-state" a measurement
//!   ([`decorated_pot_is_the_only_per_state_sound_type`]).
//! * `B <firstStateId> <block>` and `R <index> <runLength>` — block ranges and a
//!   run-length encoding of the per-state index.
//!
//! # Refreshing after a version bump
//!
//! 1. Re-dump from the server (keep the `#` header when copying over the
//!    committed dump). Byte-reproducible: two runs of the command below produced
//!    identical output (md5 `3f79821b53fcba9d9f01a7d71b7f9e86`).
//!
//! ```text
//! CACHE="$(cd .cache/mc/26.2 && pwd)"
//! HERE="$(cd crates/lodestone-data/oracle-java && pwd)"
//! docker run --rm -v "$CACHE":/mc:ro -v "$HERE":/oracle:ro -w /work eclipse-temurin:25-jdk bash -c '
//!   CP="/mc/versions/26.2/server-26.2.jar:$(find /mc/libraries -name "*.jar" | tr "\n" ":")"
//!   cp /oracle/SoundTypeOracle.java /work/ && javac -cp "$CP" -d /work /work/SoundTypeOracle.java
//!   java -cp "/work:$CP" SoundTypeOracle'
//! ```
//!
//! 2. Regenerate the committed table:
//!
//! ```text
//! LODESTONE_REGEN=1 cargo test -p lodestone-data --test sound_types \
//!     committed_table_matches_dump -- --ignored --nocapture
//! ```
//!
//! As in `shade_brightness.rs`, [`committed_entries_match_the_dump`] is **not**
//! `#[ignore]`d: it compares the committed table's *values* against the dump
//! rather than the generated file's bytes, so a reflow of generated source cannot
//! hide a wrong sound id and an ordinary `cargo test --workspace` still catches
//! drift. The byte-exact comparison lives in the ignored generator test.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::PathBuf;

use lodestone_data::{block_states, hardness, sound_events, sound_types};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn committed_path() -> PathBuf {
    manifest_dir().join("src/generated/sound_types.rs")
}

/// The committed JVM dump — an external anchor, not gitignored.
const DUMP: &str = include_str!("support/sound_types_jvm.txt");

/// One deduplicated `SoundType`: raw float bits plus five sound-event registry
/// ids, exactly as the dump's `T` rows carry them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Entry {
    volume_bits: u32,
    pitch_bits: u32,
    sounds: [u16; 5],
}

struct Dump {
    state_count: usize,
    block_count: usize,
    distinct_values: usize,
    distinct_identities: usize,
    /// `O` rows: block name -> the class declaring its own "get sound type" accessor.
    overrides: BTreeMap<String, String>,
    /// `N` rows: sound-event registry id -> name, from the live registry.
    sound_names: BTreeMap<u16, String>,
    /// `T` rows, in index order.
    entries: Vec<Entry>,
    /// `B` rows: first state id of each block, ascending.
    blocks: Vec<(usize, String)>,
    /// `R` rows flattened: per-state index into [`Dump::entries`].
    state_entry: Vec<usize>,
}

impl Dump {
    /// Block name owning state `id`, from the `B` ranges.
    fn block_of(&self, id: usize) -> &str {
        let index = self
            .blocks
            .partition_point(|(start, _)| *start <= id)
            .saturating_sub(1);
        &self.blocks[index].1
    }

    /// The dump's own answer for state `id`.
    fn entry_of(&self, id: usize) -> Entry {
        self.entries[self.state_entry[id]]
    }

    /// Break-sound name the dump says state `id` has, via the `N` rows.
    fn break_name(&self, id: usize) -> &str {
        &self.sound_names[&self.entry_of(id).sounds[0]]
    }
}

fn parse_dump(text: &str) -> Dump {
    let mut counts: Option<[usize; 4]> = None;
    let mut overrides = BTreeMap::new();
    let mut sound_names = BTreeMap::new();
    let mut entries: Vec<Entry> = Vec::new();
    let mut blocks: Vec<(usize, String)> = Vec::new();
    let mut state_entry: Vec<usize> = Vec::new();

    for line in text.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split(' ');
        let kind = parts.next().expect("non-empty line has a kind");
        match kind {
            "C" => {
                let mut read = || -> usize { parts.next().expect("C field").parse().expect("usize") };
                counts = Some([read(), read(), read(), read()]);
            }
            "O" => {
                let name = parts.next().expect("O name").to_owned();
                let owner = parts.next().expect("O class").to_owned();
                overrides.insert(name, owner);
            }
            "N" => {
                let id: u16 = parts.next().expect("N id").parse().expect("u16");
                let name = parts.next().expect("N name").to_owned();
                sound_names.insert(id, name);
            }
            "T" => {
                let index: usize = parts.next().expect("T index").parse().expect("usize");
                assert_eq!(index, entries.len(), "T rows are in ascending index order");
                let volume_bits =
                    u32::from_str_radix(parts.next().expect("T volume"), 16).expect("hex");
                let pitch_bits =
                    u32::from_str_radix(parts.next().expect("T pitch"), 16).expect("hex");
                let mut sounds = [0u16; 5];
                for slot in &mut sounds {
                    *slot = parts.next().expect("T sound").parse().expect("u16");
                }
                entries.push(Entry {
                    volume_bits,
                    pitch_bits,
                    sounds,
                });
            }
            "B" => {
                let start = parts.next().expect("B id").parse().expect("usize");
                let name = parts.next().expect("B name").to_owned();
                blocks.push((start, name));
            }
            "R" => {
                let index: usize = parts.next().expect("R index").parse().expect("usize");
                let run: usize = parts.next().expect("R length").parse().expect("usize");
                assert!(run > 0, "a run of zero states is meaningless");
                state_entry.extend(std::iter::repeat_n(index, run));
            }
            other => panic!("unknown dump line kind {other}"),
        }
    }

    let [state_count, block_count, distinct_values, distinct_identities] =
        counts.expect("dump carries a C row");
    assert_eq!(
        state_entry.len(),
        state_count,
        "the R run lengths cover every state exactly once"
    );
    assert_eq!(entries.len(), distinct_values, "T rows match the C count");
    assert!(!blocks.is_empty(), "dump carries B rows");

    Dump {
        state_count,
        block_count,
        distinct_values,
        distinct_identities,
        overrides,
        sound_names,
        entries,
        blocks,
        state_entry,
    }
}

/// Renders the committed `sound_types.rs` source from the parsed dump.
fn generate(dump: &Dump) -> String {
    // Re-derive the dedup from scratch rather than trusting the dump's `T`
    // indices: if the oracle's first-appearance ordering ever changed, this keeps
    // the committed table self-consistent and the semantic guard below is what
    // notices the values moved.
    let mut index_of: BTreeMap<Entry, usize> = BTreeMap::new();
    let mut distinct: Vec<Entry> = Vec::new();
    let mut state_entry: Vec<usize> = Vec::with_capacity(dump.state_count);
    for id in 0..dump.state_count {
        let entry = dump.entry_of(id);
        let index = *index_of.entry(entry).or_insert_with(|| {
            distinct.push(entry);
            distinct.len() - 1
        });
        state_entry.push(index);
    }
    assert!(
        distinct.len() <= usize::from(u8::MAX) + 1,
        "{} distinct sound types exceeds what a u8 STATE_ENTRY can index; widen \
         src/generated/sound_types.rs to u16 (and lodestone_data::sound_types with it)",
        distinct.len()
    );

    let mut out = String::new();
    out.push_str(
        "// @generated by `cargo test -p lodestone-data --test sound_types -- --ignored`\n\
         // from tests/support/sound_types_jvm.txt (a headless 26.2 server dump of\n\
         // BlockStateBase.getSoundType(), protocol 776 / Minecraft 26.2). DO NOT EDIT BY\n\
         // HAND. Regenerate with LODESTONE_REGEN=1 (see the test module docs).\n",
    );
    out.push_str(
        "//! Generated per-block-state `SoundType` table for protocol 776 (Minecraft\n\
         //! 26.2), indexed by global block-state id. Consumed by\n\
         //! [`crate::sound_types`].\n\n",
    );

    let _ = writeln!(out, "/// Number of block states (ids are `0..STATE_COUNT`).");
    let _ = writeln!(out, "pub const STATE_COUNT: u32 = {};\n", dump.state_count);

    let _ = writeln!(
        out,
        "/// Number of distinct sound-type values, i.e. `ENTRIES.len()`."
    );
    let _ = writeln!(out, "pub const ENTRY_COUNT: u32 = {};\n", distinct.len());

    let _ = writeln!(
        out,
        "/// De-duplicated distinct `SoundType` values ({} of them), as\n\
         /// `(volume, pitch, break, step, place, hit, fall)`. The five sound columns are\n\
         /// `minecraft:sound_event` registry ids — the id space [`crate::sound_events`] is\n\
         /// indexed by.",
        distinct.len()
    );
    let _ = writeln!(
        out,
        "pub static ENTRIES: [(f32, f32, u16, u16, u16, u16, u16); {}] = [",
        distinct.len()
    );
    for entry in &distinct {
        // Round-trip through the exact f32 the game produced. Rust's `{:?}` emits
        // the shortest decimal that parses back to the same f32, so the literal is
        // human-readable *and* bit-exact.
        let volume = f32::from_bits(entry.volume_bits);
        let pitch = f32::from_bits(entry.pitch_bits);
        assert_eq!(volume.to_bits(), entry.volume_bits, "volume round-trips");
        assert_eq!(pitch.to_bits(), entry.pitch_bits, "pitch round-trips");
        let [b, s, p, h, f] = entry.sounds;
        let _ = writeln!(out, "    ({volume:?}, {pitch:?}, {b}, {s}, {p}, {h}, {f}),");
    }
    out.push_str("];\n\n");

    let _ = writeln!(
        out,
        "/// Per-state entry index into [`ENTRIES`], indexed by global block-state id."
    );
    let _ = writeln!(
        out,
        "pub static STATE_ENTRY: [u8; {}] = [",
        dump.state_count
    );
    for chunk in state_entry.chunks(16) {
        out.push_str("    ");
        for index in chunk {
            let _ = write!(out, "{index}, ");
        }
        out.pop();
        out.push('\n');
    }
    out.push_str("];\n");

    out
}

// ---------------------------------------------------------------------------
// The dump's own self-consistency — what makes the claims below non-vacuous
// ---------------------------------------------------------------------------

/// The distinct count, asserted so a version bump that adds a `SoundType` fails
/// here and names the number instead of being silently folded onto a neighbour by
/// the dedup, and so the `u8` `STATE_ENTRY` index stays provably wide enough.
#[test]
fn there_are_exactly_126_distinct_sound_types() {
    let dump = parse_dump(DUMP);
    assert_eq!(
        dump.distinct_values, 126,
        "the game's distinct SoundType count changed; every block under a new or \
         removed one changes its break, step, place, hit and fall sounds"
    );
    assert_eq!(u32::try_from(dump.distinct_values), Ok(sound_types::ENTRY_COUNT));
    assert!(
        dump.distinct_values <= usize::from(u8::MAX) + 1,
        "STATE_ENTRY is a u8 table"
    );
}

/// Deduplicating by *value* rather than by object identity is only safe if it
/// does not merge two vanilla statics — measured, not assumed. Equal counts mean
/// every distinct `SoundType` object in use has a distinct seven-tuple, so the
/// table is a faithful renaming of the game's own set.
#[test]
fn value_dedup_collapses_nothing() {
    let dump = parse_dump(DUMP);
    assert_eq!(
        (dump.distinct_values, dump.distinct_identities),
        (126, 126),
        "distinct values vs distinct SoundType objects — if these diverge, the \
         value-keyed table merges two of the game's own sound types"
    );
}

/// Vanilla's own sound-type source declares **127** `public static final` sound-type constants
/// and only 126 are reachable from a block state. The dead one is
/// the twisting-vines constant, the only static with `pitch = 0.5F`; twisting vines
/// themselves use the weeping-vines constant. This asserts the
/// consequence — no state anywhere carries pitch `0.5` — which is the exact value
/// a name-matched hand transcription of that file would have shipped.
#[test]
fn no_state_carries_the_dead_twisting_vines_pitch() {
    let dump = parse_dump(DUMP);
    let pitches: BTreeSet<u32> = dump.entries.iter().map(|e| e.pitch_bits).collect();
    let volumes: BTreeSet<u32> = dump.entries.iter().map(|e| e.volume_bits).collect();
    let as_floats = |set: &BTreeSet<u32>| -> Vec<f32> { set.iter().map(|&b| f32::from_bits(b)).collect() };
    assert_eq!(
        as_floats(&pitches),
        vec![1.0f32, 1.5f32],
        "the only pitches in use are 1.0 and METAL's 1.5; 0.5 would mean \
         SoundType.TWISTING_VINES became reachable"
    );
    assert_eq!(
        as_floats(&volumes),
        vec![0.3f32, 1.0f32],
        "the only volumes in use are ANVIL's 0.3 and 1.0"
    );
    // …and the vines really are in the table, so the assertion above is about a
    // populated row rather than an absent block.
    let vine_ids: Vec<usize> = (0..dump.state_count)
        .filter(|&id| dump.block_of(id) == "minecraft:twisting_vines")
        .collect();
    assert!(!vine_ids.is_empty(), "twisting vines are in the dump");
    for id in vine_ids {
        let entry = dump.entry_of(id);
        assert_eq!(f32::from_bits(entry.pitch_bits), 1.0, "twisting vines pitch");
        assert_eq!(
            dump.break_name(id),
            "minecraft:block.weeping_vines.break",
            "twisting vines use SoundType.WEEPING_VINES, not TWISTING_VINES"
        );
    }
}

/// The ids in this table and the names in [`sound_events`] come from two
/// different generators — the live sound-event registry here, Mojang's
/// `registries.json` there. Their agreement is the whole licence for storing bare
/// `u16`s with no names, so it is asserted over **every** referenced id rather
/// than spot-checked.
#[test]
fn dump_sound_event_ids_agree_with_the_registries_json_table() {
    let dump = parse_dump(DUMP);
    assert!(
        dump.sound_names.len() > 500,
        "the dump references {} sound events; a near-empty N census would make \
         this test vacuous",
        dump.sound_names.len()
    );
    for (&id, name) in &dump.sound_names {
        assert_eq!(
            sound_events::sound_event_name(i32::from(id)),
            Some(name.as_str()),
            "sound event registry id {id}: the live registry says {name}, \
             src/generated/sound_events.rs disagrees"
        );
    }
    // And every id a table row uses is one the N census named, so no row can
    // reference a sound whose name was never cross-checked above.
    for (index, entry) in dump.entries.iter().enumerate() {
        for slot in entry.sounds {
            assert!(
                dump.sound_names.contains_key(&slot),
                "T row {index} references sound event {slot}, which no N row names"
            );
        }
    }
}

/// `getSoundType(BlockState)` is `protected` and overridden in exactly one class,
/// which is *why* the table is keyed by state and not by block. Measured by
/// reflection in the oracle rather than grepped.
#[test]
fn decorated_pot_is_the_only_per_state_sound_type() {
    let dump = parse_dump(DUMP);
    let by_class: BTreeMap<&str, Vec<&str>> =
        dump.overrides.iter().fold(BTreeMap::new(), |mut acc, (block, class)| {
            acc.entry(class.rsplit('.').next().expect("class has a simple name"))
                .or_default()
                .push(block.as_str());
            acc
        });
    assert_eq!(
        by_class.keys().copied().collect::<Vec<_>>(),
        vec!["DecoratedPotBlock"],
        "the getSoundType override set changed"
    );
    assert_eq!(by_class["DecoratedPotBlock"], vec!["minecraft:decorated_pot"]);

    // The consequence, measured: exactly one block spans more than one entry.
    let mut per_block: BTreeMap<&str, BTreeSet<usize>> = BTreeMap::new();
    for id in 0..dump.state_count {
        per_block
            .entry(dump.block_of(id))
            .or_default()
            .insert(dump.state_entry[id]);
    }
    let multi: Vec<&str> = per_block
        .iter()
        .filter(|(_, set)| set.len() > 1)
        .map(|(name, _)| *name)
        .collect();
    assert_eq!(
        multi,
        vec!["minecraft:decorated_pot"],
        "a block-keyed table would now lose data for these blocks"
    );

    // And the divergence is the shatter sound on the cracked states.
    let mut cracked = 0;
    let mut intact = 0;
    for id in 0..dump.state_count {
        if dump.block_of(id) != "minecraft:decorated_pot" {
            continue;
        }
        let is_cracked = block_states::properties(id as u32)
            .and_then(|props| props.iter().find(|(k, _)| *k == "cracked"))
            .map(|(_, v)| *v == "true")
            .unwrap_or_else(|| panic!("decorated pot state {id} carries a `cracked` property"));
        let expected = if is_cracked {
            cracked += 1;
            "minecraft:block.decorated_pot.shatter"
        } else {
            intact += 1;
            "minecraft:block.decorated_pot.break"
        };
        assert_eq!(dump.break_name(id), expected, "decorated pot state {id}");
    }
    assert_eq!((cracked, intact), (8, 8), "decorated pot state population");
}

// ---------------------------------------------------------------------------
// The committed table against the dump
// ---------------------------------------------------------------------------

/// Value-for-value, the committed table against the dump — **not** `#[ignore]`d,
/// so an ordinary `cargo test --workspace` catches drift. This is the guard that
/// matters: the byte-exact one below can be defeated by a harmless reflow of the
/// generated source.
#[test]
fn committed_entries_match_the_dump() {
    let dump = parse_dump(DUMP);
    assert_eq!(dump.state_count as u32, sound_types::STATE_COUNT);
    assert_eq!(dump.block_count, dump.blocks.len());

    let mut wrong: Vec<(usize, &str)> = Vec::new();
    for id in 0..dump.state_count {
        let expected = dump.entry_of(id);
        let Some(actual) = sound_types::sound_type(id as u32) else {
            wrong.push((id, dump.block_of(id)));
            continue;
        };
        let matches = actual.volume.to_bits() == expected.volume_bits
            && actual.pitch.to_bits() == expected.pitch_bits
            && [
                actual.break_sound,
                actual.step_sound,
                actual.place_sound,
                actual.hit_sound,
                actual.fall_sound,
            ] == expected.sounds;
        if !matches {
            wrong.push((id, dump.block_of(id)));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} of {} states disagree with the JVM dump; first five: {:?} — regenerate \
         src/generated/sound_types.rs with LODESTONE_REGEN=1",
        wrong.len(),
        dump.state_count,
        wrong.iter().take(5).collect::<Vec<_>>()
    );
}

#[test]
#[ignore = "regenerates/verifies the committed table; run explicitly"]
fn committed_table_matches_dump() {
    let dump = parse_dump(DUMP);
    let generated = generate(&dump);

    if std::env::var_os("LODESTONE_REGEN").is_some() {
        std::fs::write(committed_path(), &generated).expect("write committed table");
        eprintln!("regenerated {}", committed_path().display());
        return;
    }

    let committed = std::fs::read_to_string(committed_path()).expect("committed table present");
    assert_eq!(
        generated, committed,
        "src/generated/sound_types.rs is stale vs the JVM dump; regenerate with \
         LODESTONE_REGEN=1"
    );
}

/// The `B` ranges in the dump and the committed block-state table agree on which
/// block owns which id — without this, every by-name assertion here could be
/// reading the right entry of the wrong block.
#[test]
fn dump_block_ranges_agree_with_the_block_state_table() {
    let dump = parse_dump(DUMP);
    for (start, name) in &dump.blocks {
        assert_eq!(
            block_states::block_name(*start as u32),
            Some(name.as_str()),
            "dump says state {start} is the first {name}"
        );
    }
    assert_eq!(sound_types::STATE_COUNT, block_states::STATE_COUNT);
    assert_eq!(sound_types::STATE_COUNT, hardness::STATE_COUNT);
}

// ---------------------------------------------------------------------------
// The API contract the shell depends on
// ---------------------------------------------------------------------------

/// The break sounds a player hears constantly, by name, so a wrong column order
/// in the oracle (`break`/`step`/`place`/`hit`/`fall` are five ids in a row)
/// cannot pass. Every id is looked up by block name so the table survives id
/// renumbering.
#[test]
fn the_common_break_sounds_match_vanilla_by_name() {
    // (block, break sound, step sound) — read off vanilla's own sound-type
    // source's constants
    // and its own block-registration source's `.sound(..)` calls, which is a
    // *different* source from
    // the dump's reflection over the live registry.
    let cases: &[(&str, &str, &str)] = &[
        ("minecraft:stone", "minecraft:block.stone.break", "minecraft:block.stone.step"),
        ("minecraft:dirt", "minecraft:block.gravel.break", "minecraft:block.gravel.step"),
        ("minecraft:grass_block", "minecraft:block.grass.break", "minecraft:block.grass.step"),
        ("minecraft:oak_planks", "minecraft:block.wood.break", "minecraft:block.wood.step"),
        ("minecraft:glass", "minecraft:block.glass.break", "minecraft:block.glass.step"),
        ("minecraft:sand", "minecraft:block.sand.break", "minecraft:block.sand.step"),
        ("minecraft:white_wool", "minecraft:block.wool.break", "minecraft:block.wool.step"),
        // The iron and metal sound-type constants are *different* types and the
        // obvious pairing is wrong: iron blocks are `IRON` (`block.iron.*`, pitch
        // 1.0) while `METAL` (pitch 1.5) belongs to gold, rails and hoppers.
        ("minecraft:iron_block", "minecraft:block.iron.break", "minecraft:block.iron.step"),
        ("minecraft:gold_block", "minecraft:block.metal.break", "minecraft:block.metal.step"),
        ("minecraft:rail", "minecraft:block.metal.break", "minecraft:block.metal.step"),
        ("minecraft:deepslate", "minecraft:block.deepslate.break", "minecraft:block.deepslate.step"),
        ("minecraft:netherrack", "minecraft:block.netherrack.break", "minecraft:block.netherrack.step"),
        // `GLOW_LICHEN` is the mix-and-match case: GRASS's break with VINE's step.
        ("minecraft:glow_lichen", "minecraft:block.grass.break", "minecraft:block.vine.step"),
        // `HARD_CROP` is the other one: WOOD's break with CROP_PLANTED for place.
        ("minecraft:pumpkin_stem", "minecraft:block.wood.break", "minecraft:block.wood.step"),
        // Air has a SoundType too (STONE) — the gotcha a caller must guard.
        ("minecraft:air", "minecraft:block.stone.break", "minecraft:block.stone.step"),
    ];
    for &(name, break_sound, step_sound) in cases {
        let ids: Vec<u32> = (0..block_states::STATE_COUNT)
            .filter(|&id| block_states::block_name(id) == Some(name))
            .collect();
        assert!(!ids.is_empty(), "{name} present in the block-state table");
        for id in ids {
            assert_eq!(sound_types::break_sound_name(id), Some(break_sound), "{name} break");
            assert_eq!(sound_types::step_sound_name(id), Some(step_sound), "{name} step");
        }
    }
}

/// `HARD_CROP`'s placement sound is `CROP_PLANTED`, not `WOOD_PLACE` — the one
/// column a break/step-only check cannot see.
#[test]
fn hard_crop_places_with_the_crop_sound_not_the_wood_one() {
    let id = (0..block_states::STATE_COUNT)
        .find(|&id| block_states::block_name(id) == Some("minecraft:pumpkin_stem"))
        .expect("pumpkin stem is a block");
    assert_eq!(
        sound_types::place_sound_name(id),
        Some("minecraft:item.crop.plant")
    );
    assert_eq!(
        sound_types::break_sound_name(id),
        Some("minecraft:block.wood.break")
    );
}

/// Vanilla's break/place scaling, predicted from constants outside the code under
/// test: `(volume + 1) / 2` and `pitch * 0.8` (vanilla's own level-event handler,
/// its own block-item place step). Asserted on all three volume/pitch populations, so a
/// transposed multiplier cannot pass by landing on 1.0 everywhere.
#[test]
fn break_and_place_scaling_matches_vanilla_for_every_population() {
    let by_name = |name: &str| -> sound_types::BlockSoundType {
        let id = (0..block_states::STATE_COUNT)
            .find(|&id| block_states::block_name(id) == Some(name))
            .unwrap_or_else(|| panic!("{name} is a block"));
        sound_types::sound_type(id).expect("in range")
    };

    // The 124 ordinary sound types: 1.0 / 1.0 -> 1.0 / 0.8.
    let stone = by_name("minecraft:stone");
    assert_eq!((stone.volume, stone.pitch), (1.0, 1.0));
    assert_eq!(stone.break_or_place_volume(), 1.0);
    assert_eq!(stone.break_or_place_pitch(), 0.8f32);

    // ANVIL, the only volume != 1.0: 0.3 -> 0.65.
    let anvil = by_name("minecraft:anvil");
    assert_eq!(anvil.volume, 0.3f32);
    assert_eq!(anvil.break_or_place_volume(), (0.3f32 + 1.0) / 2.0);
    // Spelled out so a swapped `(v + 1) / 2` vs `v / 2 + 1` is caught: 0.65, not 1.15.
    assert!((anvil.break_or_place_volume() - 0.65).abs() < 1e-6);
    assert_eq!(anvil.break_sound_name(), Some("minecraft:block.anvil.break"));

    // METAL, the only pitch != 1.0: 1.5 -> 1.2. Gold, not iron — see
    // [`the_common_break_sounds_match_vanilla_by_name`].
    let gold = by_name("minecraft:gold_block");
    assert_eq!(gold.pitch, 1.5f32);
    assert_eq!(gold.break_or_place_pitch(), 1.5f32 * 0.8);
    assert!((gold.break_or_place_pitch() - 1.2).abs() < 1e-6);
    assert_eq!(by_name("minecraft:iron_block").pitch, 1.0f32);
}

/// The `intentionally_empty` sentinel is a real registry entry, so a bare id
/// lookup succeeds where "there is a sound" is false. The `*_name` helpers must
/// report `None` for it; the struct accessor must still resolve it.
#[test]
fn the_empty_sound_sentinel_is_reported_as_absent() {
    let id = (0..block_states::STATE_COUNT)
        .find(|&id| block_states::block_name(id) == Some("minecraft:cactus_flower"))
        .expect("cactus flower is a block");
    let sound = sound_types::sound_type(id).expect("in range");
    // `CACTUS_FLOWER` is `(1.0, 1.0, CACTUS_FLOWER_BREAK, EMPTY, CACTUS_FLOWER_PLACE, EMPTY, EMPTY)`.
    assert_eq!(sound.step_sound_name(), Some(sound_types::EMPTY_SOUND));
    assert_eq!(sound_types::step_sound_name(id), None, "no step sound to play");
    assert_eq!(
        sound_types::break_sound_name(id),
        Some("minecraft:block.cactus_flower.break"),
        "the break sound is real, so the None above is about the sentinel and \
         not about an unresolvable block"
    );
    assert!(sound_types::BlockSoundType::is_empty_sound(
        sound_types::EMPTY_SOUND
    ));
    assert!(!sound_types::BlockSoundType::is_empty_sound(
        "minecraft:block.stone.break"
    ));
}

/// Out-of-range ids report `None` rather than a plausible-looking stone break.
#[test]
fn unknown_ids_are_none() {
    assert_eq!(sound_types::sound_type(sound_types::STATE_COUNT), None);
    assert_eq!(sound_types::break_sound_name(sound_types::STATE_COUNT), None);
    assert_eq!(sound_types::place_sound_name(sound_types::STATE_COUNT), None);
    assert_eq!(sound_types::step_sound_name(sound_types::STATE_COUNT), None);
    assert_eq!(sound_types::sound_type(u32::MAX), None);
}

/// Every state resolves — no hole anywhere in the 32,366, and every referenced
/// sound event resolves to a name. Without this the by-name tests above could all
/// pass while most of the table was garbage.
#[test]
fn every_state_resolves_to_a_named_sound_type() {
    let mut unresolved_state = 0usize;
    let mut unresolved_sound = 0usize;
    let mut empty_break = Vec::new();
    for id in 0..sound_types::STATE_COUNT {
        let Some(sound) = sound_types::sound_type(id) else {
            unresolved_state += 1;
            continue;
        };
        for name in [
            sound.break_sound_name(),
            sound.step_sound_name(),
            sound.place_sound_name(),
            sound.hit_sound_name(),
            sound.fall_sound_name(),
        ] {
            if name.is_none() {
                unresolved_sound += 1;
            }
        }
        if sound_types::break_sound_name(id).is_none() {
            empty_break.push(block_states::block_name(id).unwrap_or("?"));
        }
    }
    assert_eq!(unresolved_state, 0, "states with no sound type");
    assert_eq!(unresolved_sound, 0, "sound ids that resolve to no name");
    // Measured: exactly three blocks carry the empty sound-type constant, and all three are
    // fluids, which no `LEVEL_EVENT` 2001 can name. Asserted by name so a version
    // bump that empties a *solid* surface's break slot is visible rather than
    // absorbed by a threshold.
    let empty: BTreeSet<&str> = empty_break.into_iter().collect();
    assert_eq!(
        empty,
        BTreeSet::from([
            "minecraft:bubble_column",
            "minecraft:lava",
            "minecraft:water",
        ]),
        "the set of blocks with no break sound changed — if a solid block joined \
         it, the 2001 arm in lodestone-shell just went quiet for it"
    );
}
