//! Generator and drift controls for the six-direction face-occlusion table.
//!
//! The external dump is produced by `just oracle-face-occlusion` after a real
//! 26.2 server registry walk. Regenerate the Rust source with:
//!
//! ```text
//! LODESTONE_REGEN=1 cargo test -p lodestone-data --test face_occlusion \
//!     committed_table_matches_dump -- --ignored --nocapture
//! ```
//!
//! The normal tests do not need the dump. They validate the checked-in seam's
//! state-id boundary and full-table shape; the ignored test is the only one
//! that reads the external artifact and can rewrite generated data.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use lodestone_data::{block_states, face_occlusion};

const FACES: [(char, u8); 6] = [('D', 0), ('U', 1), ('N', 2), ('S', 3), ('W', 4), ('E', 5)];

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn committed_path() -> PathBuf {
    manifest_dir().join("src/generated/face_occlusion.rs")
}

/// The committed JVM dump — an external anchor, not generated Rust data.
const DUMP: &str = include_str!("support/face_occlusion_jvm.txt");

#[derive(Debug)]
struct Dump {
    state_count: usize,
    block_count: usize,
    blocks: Vec<(usize, String)>,
    counts: BTreeMap<char, usize>,
    columns: BTreeMap<char, Vec<bool>>,
}

impl Dump {
    fn block_of(&self, id: usize) -> &str {
        let index = self
            .blocks
            .partition_point(|(start, _)| *start <= id)
            .checked_sub(1)
            .expect("dump has a block beginning at state zero");
        &self.blocks[index].1
    }
}

fn parse_dump(text: &str) -> Dump {
    let mut state_count = None;
    let mut block_count = None;
    let mut blocks = Vec::new();
    let mut counts = BTreeMap::new();
    let mut chunks: BTreeMap<char, BTreeMap<usize, String>> = BTreeMap::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        match parts.next().expect("non-empty dump line has a kind") {
            "C" => {
                state_count = Some(
                    parts
                        .next()
                        .expect("C state count")
                        .parse()
                        .expect("state count is usize"),
                );
                block_count = Some(
                    parts
                        .next()
                        .expect("C block count")
                        .parse()
                        .expect("block count is usize"),
                );
                assert!(parts.next().is_none(), "C row has no trailing fields");
            }
            "B" => {
                let start: usize = parts
                    .next()
                    .expect("B state id")
                    .parse()
                    .expect("B state id is usize");
                let name = parts.next().expect("B block name").to_owned();
                assert!(parts.next().is_none(), "B row has no trailing fields");
                if let Some((previous, _)) = blocks.last() {
                    assert!(start > *previous, "B rows are strictly ascending");
                }
                blocks.push((start, name));
            }
            "K" => {
                let face = one_face(parts.next().expect("K face"));
                let count: usize = parts
                    .next()
                    .expect("K count")
                    .parse()
                    .expect("K count is usize");
                assert!(parts.next().is_none(), "K row has no trailing fields");
                assert!(
                    counts.insert(face, count).is_none(),
                    "duplicate K row for {face}"
                );
            }
            "P" => {
                let face = one_face(parts.next().expect("P face"));
                let start: usize = parts
                    .next()
                    .expect("P start")
                    .parse()
                    .expect("P start is usize");
                let bits = parts.next().expect("P bitstring");
                assert!(bits.len() <= 256, "P chunks are at most 256 bits");
                assert!(
                    bits.bytes().all(|byte| byte == b'0' || byte == b'1'),
                    "P contains only 0/1"
                );
                assert!(parts.next().is_none(), "P row has no trailing fields");
                assert!(
                    chunks
                        .entry(face)
                        .or_default()
                        .insert(start, bits.to_owned())
                        .is_none(),
                    "duplicate P row"
                );
            }
            kind => panic!("unknown face-occlusion dump row {kind}"),
        }
    }

    let state_count = state_count.expect("dump carries one C row");
    let block_count = block_count.expect("dump carries one C row");
    assert_eq!(
        blocks.first().map(|(start, _)| *start),
        Some(0),
        "B coverage starts at zero"
    );
    assert_eq!(
        blocks.len(),
        block_count,
        "B rows cover every registered block"
    );
    assert!(
        blocks.iter().all(|(start, _)| *start < state_count),
        "B rows stay inside the state-id space"
    );

    let mut columns = BTreeMap::new();
    for (face, _) in FACES {
        let per_start = chunks
            .remove(&face)
            .unwrap_or_else(|| panic!("dump carries P rows for {face}"));
        let mut flat = Vec::with_capacity(state_count);
        let mut expected_start = 0;
        for (start, bits) in per_start {
            assert_eq!(
                start, expected_start,
                "{face} P chunks are gap-free and ordered"
            );
            flat.extend(bits.bytes().map(|byte| byte == b'1'));
            expected_start += bits.len();
        }
        assert_eq!(
            flat.len(),
            state_count,
            "{face} covers every state exactly once"
        );
        columns.insert(face, flat);
    }
    assert!(chunks.is_empty(), "dump has no unknown face columns");
    for (face, _) in FACES {
        assert!(counts.contains_key(&face), "dump carries K row for {face}");
    }

    Dump {
        state_count,
        block_count,
        blocks,
        counts,
        columns,
    }
}

fn one_face(value: &str) -> char {
    let mut chars = value.chars();
    let face = chars.next().expect("face is one character");
    assert!(chars.next().is_none(), "face is one character");
    assert!(
        FACES.iter().any(|(candidate, _)| *candidate == face),
        "unknown face {face}"
    );
    face
}

fn generate(dump: &Dump) -> String {
    let mut masks = vec![0u8; dump.state_count];
    for (face, bit) in FACES {
        for (state, &set) in dump.columns[&face].iter().enumerate() {
            if set {
                masks[state] |= 1u8 << bit;
            }
        }
    }

    let mut out = String::new();
    out.push_str(
        "// @generated by `cargo test -p lodestone-data --test face_occlusion -- --ignored`\n\
         // from tests/support/face_occlusion_jvm.txt (a headless 26.2 server dump of\n\
         // effective six-direction face occlusion, protocol 776 / Minecraft 26.2).\n\
         // DO NOT EDIT BY HAND. Regenerate with LODESTONE_REGEN=1 (see the test module docs).\n\n",
    );
    out.push_str(
        "//! Generated effective six-direction face-occlusion masks for protocol\n\
         //! 776 (Minecraft 26.2), indexed by global block-state id.\n\
         //! Bit order is Down, Up, North, South, West, East.\n\n",
    );
    let _ = writeln!(
        out,
        "/// Number of block states (ids are `0..STATE_COUNT`)."
    );
    let _ = writeln!(out, "pub const STATE_COUNT: u32 = {};\n", dump.state_count);
    let _ = writeln!(
        out,
        "/// Per-state mask: one bit for each completely occluded face."
    );
    let _ = writeln!(
        out,
        "pub static FACE_OCCLUSION: [u8; {}] = [",
        dump.state_count
    );
    for chunk in masks.chunks(32) {
        out.push_str("    ");
        for mask in chunk {
            let _ = write!(out, "0x{mask:02x}, ");
        }
        out.pop();
        out.push('\n');
    }
    out.push_str("];\n");
    out
}

/// Checks every committed mask bit against the independently captured dump.
/// Keeping this outside the ignored regeneration test makes an accidental
/// table edit fail during an ordinary workspace test run.
fn assert_committed_bits_match_dump(dump: &Dump) {
    assert_eq!(
        face_occlusion::STATE_COUNT as usize,
        dump.state_count,
        "committed STATE_COUNT disagrees with the oracle dump"
    );
    assert_eq!(
        dump.state_count,
        block_states::STATE_COUNT as usize,
        "oracle and canonical state tables have equal cardinality"
    );
    assert_eq!(dump.blocks.len(), dump.block_count);

    for raw in 0..dump.state_count {
        let expected = block_states::block_name(raw as u32).expect("canonical state has a name");
        assert_eq!(
            dump.block_of(raw),
            expected,
            "oracle B coverage agrees at state {raw}"
        );
    }

    for (face_name, bit) in FACES {
        let face = face_occlusion::Face::ALL[bit as usize];
        let expected = &dump.columns[&face_name];
        let mut count = 0;
        for (raw, &want) in expected.iter().enumerate() {
            let state = block_states::StateId::new(raw as u32).expect("dump id is valid");
            let got = face_occlusion::occludes(state, face);
            assert_eq!(
                got,
                want,
                "{face_name} at state {raw} ({}): committed {got}, dump {want}",
                dump.block_of(raw)
            );
            count += usize::from(got);
        }
        assert_eq!(
            count, dump.counts[&face_name],
            "{face_name} population matches the JVM K row"
        );
    }
}

#[test]
fn count_and_state_id_boundaries_are_total() {
    assert_eq!(face_occlusion::STATE_COUNT, block_states::STATE_COUNT);
    for raw in 0..face_occlusion::STATE_COUNT {
        let id = block_states::StateId::new(raw).expect("every generated id validates");
        let _ = face_occlusion::occlusion_mask(id);
    }
    assert!(block_states::StateId::new(face_occlusion::STATE_COUNT).is_none());
    assert!(block_states::StateId::new(u32::MAX).is_none());
}

#[test]
fn six_direction_columns_are_non_degenerate() {
    for face in face_occlusion::Face::ALL {
        let set = (0..face_occlusion::STATE_COUNT)
            .map(|raw| block_states::StateId::new(raw).expect("generated id validates"))
            .filter(|&id| face_occlusion::occludes(id, face))
            .count();
        assert!(set > 0, "{face:?} is all-zero");
        assert!(
            set < face_occlusion::STATE_COUNT as usize,
            "{face:?} is all-one"
        );
    }
}

#[test]
fn committed_bits_match_dump() {
    let dump = parse_dump(DUMP);
    assert_committed_bits_match_dump(&dump);
}

#[test]
#[ignore = "requires the face-occlusion oracle dump; regenerates and checks the committed table"]
fn committed_table_matches_dump() {
    let dump = parse_dump(DUMP);
    assert_committed_bits_match_dump(&dump);

    let generated = generate(&dump);
    if std::env::var_os("LODESTONE_REGEN").is_some() {
        std::fs::write(committed_path(), generated).expect("write generated face-occlusion table");
        eprintln!("regenerated {}", committed_path().display());
        return;
    }

    let committed = std::fs::read_to_string(committed_path())
        .expect("generated face-occlusion table is present");
    assert_eq!(
        generated, committed,
        "src/generated/face_occlusion.rs is stale; regenerate with LODESTONE_REGEN=1"
    );
}
