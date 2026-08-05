//! The **external oracle for the write path** (issue #437): a real Mojang 26.2
//! server reading a region file *we* wrote.
//!
//! # Why this exists at all
//!
//! `chunk_nbt_vanilla_oracle.rs` evidences the read direction against files
//! vanilla wrote. `world_persistence_round_trip.rs` evidences the lifecycle,
//! but it is a round trip through our own codec and says so. Neither can
//! establish the claim that actually matters for a save format: **that what we
//! write is loadable by the program that defines it.** `decode(encode(x)) == x`
//! is satisfied by two symmetric misunderstandings, and this repo already paid
//! for that lesson once — hermetic chunk fixtures built with our own encoder
//! passed throughout, and then a live gate produced 49 × "unexpected end of
//! input".
//!
//! So this gate writes a world through the production save path and hands the
//! bytes to Mojang's code. Three separate things are checked by *their* classes,
//! not ours (see `scripts/anvil-oracle/AnvilReadbackOracle.java`):
//!
//! | risk | vanilla class that adjudicates it |
//! |---|---|
//! | sector table, header, compression | `RegionFile` |
//! | `{Name, Properties}` palette entries | `BlockState.CODEC` |
//! | non-spanning bit packing | `SimpleBitStorage` |
//!
//! The third is the one worth having: dense-vs-non-spanning packing is
//! invisible for every palette of 16 or fewer entries, so the probes below
//! deliberately include a chunk with a palette large enough to need 5 bits,
//! where 64 is not divisible by the bit width and the two rules diverge.
//!
//! # The control, run and observed
//!
//! `chunk_nbt::pack_indices` was temporarily changed to pack **densely** (a
//! continuous bit stream across long boundaries) and this gate re-run. Result:
//!
//! ```text
//! 16 of 24 probes disagree — a real Mojang server does not read back what we wrote:
//! (0,64,1): vanilla read "minecraft:deepslate[axis=y]", we wrote "minecraft:gold_ore"
//! (3,67,1): vanilla read "minecraft:air",                we wrote "minecraft:oak_log[axis=x]"
//! ...
//! ```
//!
//! Two things about that output are worth keeping. It fails **loudly**, and it
//! fails *by Mojang's adjudication* rather than ours. And exactly 8 of the 24
//! probes still agreed — the ones in the 1-bit and small-palette regions where
//! the two packing rules coincide — which is the quantitative demonstration
//! that a gate built only from small palettes would have been green. That is
//! why `WIDE_PALETTE` exists.
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

    let world = RegionChunkSource::new(EmptyWorld, &world_dir, MIN_Y, HEIGHT)
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

    // Group probes by the region file that holds them, because the oracle
    // opens one `.mca` per invocation (as `RegionFile` itself does).
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
            // `Bootstrap.bootStrap()` installs a logger that captures
            // `System.out`, so every line arrives wrapped as
            // `[09:12:40] [main/INFO]: [STDOUT]: RESULT ...`. Match the marker
            // anywhere in the line rather than at its start — a prefix match
            // silently found nothing and read as "the oracle returned no
            // results", which is a failure mode worth naming here because it
            // looks exactly like a broken write path.
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
