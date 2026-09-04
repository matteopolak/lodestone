//! The **external oracle for the write path**: a reference 26.2 server reads a
//! region file written through the production save path.
//!
//! # Why this exists at all
//!
//! `chunk_nbt_vanilla_oracle.rs` checks the read direction against files written
//! by the reference server. `world_persistence_round_trip.rs` checks the
//! lifecycle, but its round trip uses our own codec on both sides. Neither
//! establishes the independent claim that matters for a save format: **that
//! bytes written here are loadable by the defining program.** `decode(encode(x))
//! == x` can hold when both operations share the same misunderstanding.
//!
//! This gate writes a world through the production save path and hands the
//! bytes to an independent reader. Three separate concerns are checked by the
//! external harness rather than by our codec (see
//! `scripts/anvil-oracle/AnvilReadbackOracle.java`):
//!
//! | risk | independent reader concern |
//! |---|---|
//! | sector table, header, compression | region-container reader |
//! | `{Name, Properties}` palette entries | block-state codec |
//! | non-spanning bit packing | packed-section storage reader |
//!
//! The packing probe is essential because dense-vs-non-spanning packing is
//! invisible for every palette of 16 or fewer entries. The probes therefore
//! include a chunk with a palette large enough to need 5 bits, where 64 is not
//! divisible by the bit width and the two rules diverge.
//!
//! # The discriminating probe
//!
//! Dense bit-stream packing and the required non-spanning packing produce the
//! same result for palettes of 16 or fewer entries. `WIDE_PALETTE` therefore
//! supplies 20 distinct states: its 5-bit indices make `64 % 5 != 0`, so a
//! reader using the wrong rule must disagree at some probe positions. The
//! external reader is the adjudicator for those positions.
//!
//! # Ignored by default
//!
//! It needs `.cache/mc/26.2` and the `container` runtime, neither of which is
//! repo state. Run it with:
//!
//! ```text
//! cargo test -p lodestone-server --test write_path_jvm_oracle -- --ignored --nocapture
//! ```

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use lodestone_server::dimension::Dimension;
use lodestone_server::region_source::RegionChunkSource;
use lodestone_server::{ChunkColumn, ChunkSource};

const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;

/// Deliberately more than 16 distinct states in one section, so the packed
/// index width is 5 bits — where `64 % 5 != 0` and the non-spanning rule
/// diverges from a dense bit stream. With a smaller palette this oracle would
/// pass under either rule and prove nothing about packing.
const WIDE_PALETTE: [&str; 20] = [
    "minecraft:stone",
    "minecraft:granite",
    "minecraft:diorite",
    "minecraft:andesite",
    "minecraft:deepslate[axis=y]",
    "minecraft:tuff",
    "minecraft:calcite",
    "minecraft:dripstone_block",
    "minecraft:gravel",
    "minecraft:dirt",
    "minecraft:coarse_dirt",
    "minecraft:clay",
    "minecraft:sand",
    "minecraft:sandstone",
    "minecraft:iron_ore",
    "minecraft:copper_ore",
    "minecraft:gold_ore",
    "minecraft:redstone_ore[lit=false]",
    "minecraft:diamond_block",
    "minecraft:oak_log[axis=x]",
];

/// An empty world; everything interesting is written by `set_block` so the
/// fixture's contents are exactly what this file states, with no generator
/// between the assertion and the bytes.
#[derive(Debug)]
struct EmptyWorld;

impl ChunkSource for EmptyWorld {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(MIN_Y, HEIGHT)
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        // The column-regenerating form (correct, just not cheap); this
        // fixture is all air and this path is not hot.
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).block_state(lx, y, lz).to_string()
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        // The column-regenerating form (correct, just not cheap); this
        // fixture is all air and this path is not hot.
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).biome_state_at(lx, y, lz).to_string()
    }

    // No storage: this fixture serves fresh columns and edits are discarded by
    // design (an edit a test needs to survive goes through a source with real
    // retention). The no-op is explicit so the fixture's retention behavior is
    // clear at the implementation boundary.
    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
        // No storage; edits are discarded by design.
    }
}

fn fixture_dir() -> PathBuf {
    // A literal nonce, not a pid: the scratchpad is shared between agents and
    // a collision here would read as a persistence defect.
    std::env::temp_dir().join("lodestone-437-jvm-fixture-4m8k")
}

#[test]
#[ignore = "requires .cache/mc/26.2 and the `container` runtime; see this file's docs"]
fn a_real_mojang_server_can_read_the_region_file_we_wrote() {
    let world_dir = fixture_dir();
    let _ = std::fs::remove_dir_all(&world_dir);
    std::fs::create_dir_all(&world_dir).expect("create fixture world dir");

    let world = RegionChunkSource::new(EmptyWorld, &world_dir, Dimension::Overworld, MIN_Y, HEIGHT)
        .expect("open persistent world");

    // Every probe: what we place, and where. Chunk (0,0) and chunk (1,2) both
    // live in region (0,0); chunk (-1,-1) is in region (-1,-1), so more than
    // one region file is exercised.
    let mut expected: BTreeMap<(i32, i32, i32), String> = BTreeMap::new();
    for (i, state) in WIDE_PALETTE.iter().enumerate() {
        // All inside section Y=4 of chunk (0,0), so they share one palette and
        // force it to 20 entries → 5 bits.
        let x = i as i32 % 16;
        let z = (i as i32 / 16) % 16;
        let y = 64 + i as i32 % 4;
        world.set_block(x, y, z, state);
        expected.insert((x, y, z), (*state).to_owned());
    }
    for (x, y, z, state) in [
        (20, 70, 35, "minecraft:oak_log[axis=x]"),
        (21, -60, 36, "minecraft:deepslate[axis=y]"),
        (-5, 100, -9, "minecraft:redstone_ore[lit=false]"),
        (-16, 0, -16, "minecraft:diamond_block"),
    ] {
        world.set_block(x, y, z, state);
        expected.insert((x, y, z), state.to_owned());
    }

    let written = world.save_handle().save().expect("save the fixture world");
    assert!(written > 0, "the fixture save wrote nothing");

    let region_dir = world_dir
        .join("dimensions")
        .join("minecraft")
        .join("overworld")
        .join("region");

    // Group probes by the region file that holds them, because the oracle opens
    // one `.mca` per invocation.
    let mut by_region: BTreeMap<(i32, i32), Vec<(i32, i32, i32)>> = BTreeMap::new();
    for &(x, y, z) in expected.keys() {
        let rx = x.div_euclid(16).div_euclid(32);
        let rz = z.div_euclid(16).div_euclid(32);
        by_region.entry((rx, rz)).or_default().push((x, y, z));
    }

    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/anvil-oracle/run.sh");
    let mut checked = 0usize;
    let mut mismatches = Vec::new();

    for ((rx, rz), probes) in &by_region {
        let mut args: Vec<String> = vec![
            region_dir.display().to_string(),
            rx.to_string(),
            rz.to_string(),
        ];
        args.extend(probes.iter().map(|(x, y, z)| format!("{x},{y},{z}")));

        let output = Command::new("bash")
            .arg(&script)
            .args(&args)
            .output()
            .expect("run the JVM readback oracle");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "the JVM oracle failed for region ({rx},{rz}).\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );

        let mut seen = 0usize;
        for line in stdout.lines() {
            // The oracle launcher prefixes captured output with a timestamp and
            // logger marker such as `[09:12:40] [main/INFO]: [STDOUT]: RESULT`.
            // Match the marker anywhere in the line rather than at its start;
            // otherwise a harmless wrapper prefix would make the parser find
            // no results and obscure a write-path failure.
            let Some(body) = line.split_once("RESULT ").map(|(_, rest)| rest) else {
                continue;
            };
            let Some((coords, actual)) = body.split_once('=') else {
                continue;
            };
            let nums: Vec<i32> = coords
                .split(',')
                .map(|n| n.parse().expect("probe coordinate"))
                .collect();
            let key = (nums[0], nums[1], nums[2]);
            let want = expected.get(&key).expect("probe we asked for");
            seen += 1;
            checked += 1;
            if actual != want {
                mismatches.push(format!(
                    "({},{},{}): vanilla read {actual:?}, we wrote {want:?}",
                    key.0, key.1, key.2
                ));
            }
        }
        assert_eq!(
            seen,
            probes.len(),
            "region ({rx},{rz}): the oracle returned {seen} results for {} probes.\nstdout:\n{stdout}",
            probes.len()
        );
    }

    assert!(
        mismatches.is_empty(),
        "{} of {checked} probes disagree — a real Mojang server does not read back what we \
         wrote:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
    assert_eq!(
        checked,
        expected.len(),
        "expected every probe to be adjudicated by the JVM"
    );
    println!("JVM oracle: {checked} probes read back correctly by Mojang's own RegionFile, \
              BlockState.CODEC and SimpleBitStorage");
}
