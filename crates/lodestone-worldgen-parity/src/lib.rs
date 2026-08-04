//! Chunk-for-chunk parity harness: the shared, reusable comparator every
//! worldgen phase (epic #404: biomes #405, carvers/aquifer/ores #295,
//! vegetation #406) can point at instead of improvising its own oracle diff.
//!
//! # What this crate is
//!
//! Two halves:
//!
//! 1. **A compact, committed fixture format** (`fixtures/*.txt`) holding a
//!    real vanilla 26.2 JVM's own generated output for a fixed seed and named
//!    chunk coordinates, at two pipeline points (`postsurface` — after
//!    `buildSurface`, before carvers; `postcarve` — after `applyCarvers`, the
//!    full non-structure/non-feature chunk). Produced by
//!    `scripts/worldgen-oracle/ComposedChunkOracle.java`, which runs vanilla's
//!    own `fillFromNoise` + `buildSurface` + `applyCarvers` through the REAL
//!    `MultiNoiseBiomeSource` (the same 7594-row table `BiomeOracle.java`
//!    dumps) — not a biome pinned to a constant, so the biome driving surface
//!    materials and carver selection is whatever vanilla actually assigns.
//! 2. **A diff engine** ([`diff_field`]) that compares one of those fixtures
//!    against [`lodestone_worldgen::overworld::GeneratedColumn`] (or any
//!    `Fn(lx, y, lz) -> String`) and reports *where* it differs — bounding
//!    box, per-section counts, sample mismatches — never just a percentage
//!    (`CLAUDE.md`: "a gate reporting only a fraction cannot tell a
//!    uniform-but-wrong result from a localised blob").
//!
//! See `docs/worldgen-parity.md` for the full write-up: how to add a stage,
//! how to add a seed/coordinate, how to add a second version, and the
//! measured current parity numbers.
//!
//! # Honest scope
//!
//! The fixture's `postcarve` stage is shape + the real aquifer + surface
//! rules + carvers — **not** ore/vegetation features (unbuilt in this repo's
//! Rust; `FeatureOracle.java` already isolates the ore-feature engine
//! separately and is the natural next extension, not attempted here) and
//! **not** structures (unbuilt anywhere in this repo, `#136` says do not
//! start). A diff against `postcarve` therefore over-counts the true gap by
//! exactly "no ores, no vegetation, no structures" on top of whatever
//! carvers/aquifer/biome gap is real — `docs/worldgen-parity.md` breaks this
//! apart per stage so the two are not conflated.

use std::collections::HashMap;
use std::fmt::Write as _;

/// One pipeline stage's full `16 x height x 16` block field for one chunk,
/// indexed by local `(lx, y, lz)` with `lx, lz` in `0..16` and `y` in
/// `min_y..min_y + height`. Missing positions read as `"minecraft:air"`
/// (matching [`lodestone_worldgen::overworld::GeneratedColumn::block_state`]'s
/// out-of-range convention), so a fixture that is honestly all-air still
/// round-trips instead of panicking.
#[derive(Debug, Clone)]
pub struct BlockField {
    pub min_y: i32,
    pub height: i32,
    blocks: HashMap<(i32, i32, i32), String>,
}

impl BlockField {
    #[must_use]
    pub fn new(min_y: i32, height: i32) -> Self {
        Self {
            min_y,
            height,
            blocks: HashMap::new(),
        }
    }

    pub fn set(&mut self, lx: i32, y: i32, lz: i32, state: impl Into<String>) {
        self.blocks.insert((lx, y, lz), state.into());
    }

    #[must_use]
    pub fn get(&self, lx: i32, y: i32, lz: i32) -> &str {
        self.blocks
            .get(&(lx, y, lz))
            .map(String::as_str)
            .unwrap_or("minecraft:air")
    }

    /// Number of non-air entries actually stored (anti-vacuity telemetry —
    /// not the same as `16*16*height`, since air is never explicitly stored).
    #[must_use]
    pub fn non_air_count(&self) -> usize {
        self.blocks
            .values()
            .filter(|s| s.split('[').next() != Some("minecraft:air"))
            .count()
    }

    /// Every stored `(lx, y, lz, state)` triple, for encoding.
    pub fn iter(&self) -> impl Iterator<Item = (i32, i32, i32, &str)> {
        self.blocks
            .iter()
            .map(|(&(lx, y, lz), s)| (lx, y, lz, s.as_str()))
    }
}

/// One chunk's fixture: both pipeline stages plus the per-quart biome table,
/// exactly what `ComposedChunkOracle.java` dumps for one `(seed, cx, cz)`.
#[derive(Debug, Clone)]
pub struct ChunkFixture {
    pub seed: i64,
    pub chunk_x: i32,
    pub chunk_z: i32,
    pub min_y: i32,
    pub height: i32,
    pub sea_level: i32,
    /// `(biome_id, sampled_height)` per quart, row-major `qz*4+qx`, matching
    /// [`lodestone_worldgen::overworld::OverworldGenerator::biome_stage`]'s
    /// convention (`crates/lodestone-worldgen/src/overworld.rs`).
    pub biome_quarts: [(String, i32); 16],
    /// Post-`buildSurface`, pre-carve: the stage the currently-composed Rust
    /// pipeline (shape + fluid-approx + biome + surface) should be compared
    /// against.
    pub postsurface: BlockField,
    /// Post-`applyCarvers`: the full non-feature/non-structure vanilla
    /// chunk — the honest "how far are we" target once #295 lands.
    pub postcarve: BlockField,
    /// Post ore-only decoration of the CENTRE chunk (`ComposedChunkOracle
    /// .java`'s `postfeatures` stage) — narrower than `FeatureOracle.java`'s
    /// own isolated ore fixture (single-source, no 3x3 neighbour spill; see
    /// that Java file's `postfeatures` doc comment for the exact scope).
    /// Vegetation features and structures are still entirely absent.
    pub postfeatures: BlockField,
}

// ---------------------------------------------------------------------------
// Raw oracle output (ComposedChunkOracle.java's stdout) -> ChunkFixture list
// ---------------------------------------------------------------------------

/// Parses `ComposedChunkOracle.java`'s raw stdout (one or more chunks,
/// `meta.done cx,cz` terminating each) into a list of [`ChunkFixture`]s.
/// Tolerant of interleaved JVM log lines (`[main/WARN] ...`) — anything not
/// matching a known `key value...` line is skipped, not an error, since
/// `Bootstrap.bootStrap()` logs a couple of harmless warnings to stdout.
///
/// # Panics
/// Panics on a malformed *matched* line (wrong token count, unparseable
/// integer) — a real fixture from a real oracle run should never hit this;
/// tripping it means the oracle or this parser drifted from each other.
#[must_use]
pub fn parse_raw_dump(text: &str) -> Vec<ChunkFixture> {
    let mut out = Vec::new();
    let mut seed = 0i64;
    let mut chunk_x = 0i32;
    let mut chunk_z = 0i32;
    let mut min_y = 0i32;
    let mut height = 0i32;
    let mut sea_level = 0i32;
    let mut biome_quarts: [(String, i32); 16] = std::array::from_fn(|_| (String::new(), 0));
    let mut postsurface = BlockField::new(0, 0);
    let mut postcarve = BlockField::new(0, 0);
    let mut postfeatures = BlockField::new(0, 0);

    for line in text.lines() {
        let Some((key, rest)) = line.split_once(' ') else {
            continue;
        };
        if let Some(coords) = key.strip_prefix("biome.") {
            let (qxs, qzs) = coords.split_once(',').expect("biome.qx,qz");
            let qx: i32 = qxs.parse().expect("qx");
            let qz: i32 = qzs.parse().expect("qz");
            let mut tok = rest.split_whitespace();
            let id = tok.next().expect("biome id").to_string();
            let y: i32 = tok.next().expect("biome sample y").parse().expect("y");
            biome_quarts[(qz * 4 + qx) as usize] = (id, y);
        } else if let Some(coords) = key.strip_prefix("postsurface.") {
            let (lx, y, lz) = parse_xyz(coords);
            postsurface.set(lx, y, lz, rest.to_string());
        } else if let Some(coords) = key.strip_prefix("postfeatures.") {
            let (lx, y, lz) = parse_xyz(coords);
            postfeatures.set(lx, y, lz, rest.to_string());
        } else if let Some(coords) = key.strip_prefix("postcarve.") {
            let (lx, y, lz) = parse_xyz(coords);
            postcarve.set(lx, y, lz, rest.to_string());
        } else {
            match key {
                "meta.seed" => seed = rest.trim().parse().expect("seed"),
                "meta.chunkX" => chunk_x = rest.trim().parse().expect("chunkX"),
                "meta.chunkZ" => chunk_z = rest.trim().parse().expect("chunkZ"),
                "meta.minY" => {
                    min_y = rest.trim().parse().expect("minY");
                    postsurface.min_y = min_y;
                    postcarve.min_y = min_y;
                    postfeatures.min_y = min_y;
                }
                "meta.height" => {
                    height = rest.trim().parse().expect("height");
                    postsurface.height = height;
                    postcarve.height = height;
                    postfeatures.height = height;
                }
                "meta.seaLevel" => sea_level = rest.trim().parse().expect("seaLevel"),
                "meta.done" => {
                    out.push(ChunkFixture {
                        seed,
                        chunk_x,
                        chunk_z,
                        min_y,
                        height,
                        sea_level,
                        biome_quarts: biome_quarts.clone(),
                        postsurface: std::mem::replace(&mut postsurface, BlockField::new(0, 0)),
                        postcarve: std::mem::replace(&mut postcarve, BlockField::new(0, 0)),
                        postfeatures: std::mem::replace(&mut postfeatures, BlockField::new(0, 0)),
                    });
                }
                _ => {} // presurface.*, meta.carveExceptions, meta.carveEx — not needed by the compact fixture
            }
        }
    }
    out
}

fn parse_xyz(coords: &str) -> (i32, i32, i32) {
    let mut it = coords.split(',');
    let x: i32 = it.next().expect("x").parse().expect("x int");
    let y: i32 = it.next().expect("y").parse().expect("y int");
    let z: i32 = it.next().expect("z").parse().expect("z int");
    (x, y, z)
}

// ---------------------------------------------------------------------------
// Compact fixture encoding (what's actually committed under fixtures/)
// ---------------------------------------------------------------------------

/// Run-length-encodes a [`ChunkFixture`] list into the compact text format
/// committed under `fixtures/`. A raw oracle dump is ~10.5 MB per chunk
/// (98304 explicit lines x 2 stages); real terrain is mostly vertical runs of
/// one block (stone, then air, then a thin carved/surfaced band), so RLE
/// shrinks this by roughly two orders of magnitude while staying a diffable
/// text format — see `docs/worldgen-parity.md` for the measured size.
#[must_use]
pub fn encode_compact(fixtures: &[ChunkFixture]) -> String {
    let mut out = String::new();
    for f in fixtures {
        writeln!(out, "seed {}", f.seed).unwrap();
        writeln!(out, "chunk {} {}", f.chunk_x, f.chunk_z).unwrap();
        writeln!(out, "minY {}", f.min_y).unwrap();
        writeln!(out, "height {}", f.height).unwrap();
        writeln!(out, "seaLevel {}", f.sea_level).unwrap();
        for (i, (id, y)) in f.biome_quarts.iter().enumerate() {
            writeln!(out, "biome {} {} {} {}", i % 4, i / 4, id, y).unwrap();
        }
        for (label, field) in [
            ("postsurface", &f.postsurface),
            ("postcarve", &f.postcarve),
            ("postfeatures", &f.postfeatures),
        ] {
            writeln!(out, "stage {label}").unwrap();
            for lz in 0..16i32 {
                for lx in 0..16i32 {
                    let mut y = f.min_y;
                    let end = f.min_y + f.height;
                    let mut runs: Vec<(i32, i32, &str)> = Vec::new();
                    while y < end {
                        let state = field.get(lx, y, lz);
                        let mut count = 1;
                        while y + count < end && field.get(lx, y + count, lz) == state {
                            count += 1;
                        }
                        runs.push((y, count, state));
                        y += count;
                    }
                    // Skip all-air columns entirely (common above the
                    // surface) — `col` with zero `run` lines means "all air".
                    if runs.len() == 1 && runs[0].2.split('[').next() == Some("minecraft:air") {
                        continue;
                    }
                    writeln!(out, "col {lx} {lz}").unwrap();
                    for (ry, count, state) in runs {
                        writeln!(out, "{ry} {count} {state}").unwrap();
                    }
                }
            }
        }
    }
    out
}

/// Decodes [`encode_compact`]'s format back into [`ChunkFixture`]s.
///
/// # Panics
/// Panics on malformed input — this only ever reads this crate's own
/// committed fixtures or output freshly produced by [`encode_compact`].
#[must_use]
pub fn parse_compact(text: &str) -> Vec<ChunkFixture> {
    let mut out = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        let mut tok = line.split_whitespace();
        assert_eq!(tok.next(), Some("seed"), "expected 'seed' line, got {line:?}");
        let seed: i64 = tok.next().expect("seed value").parse().expect("seed int");

        let line = lines.next().expect("chunk line");
        let mut tok = line.split_whitespace();
        assert_eq!(tok.next(), Some("chunk"));
        let chunk_x: i32 = tok.next().expect("cx").parse().expect("cx int");
        let chunk_z: i32 = tok.next().expect("cz").parse().expect("cz int");

        let line = lines.next().expect("minY line");
        let min_y: i32 = line.strip_prefix("minY ").expect("minY prefix").trim().parse().expect("minY int");
        let line = lines.next().expect("height line");
        let height: i32 = line.strip_prefix("height ").expect("height prefix").trim().parse().expect("height int");
        let line = lines.next().expect("seaLevel line");
        let sea_level: i32 = line
            .strip_prefix("seaLevel ")
            .expect("seaLevel prefix")
            .trim()
            .parse()
            .expect("seaLevel int");

        let mut biome_quarts: [(String, i32); 16] = std::array::from_fn(|_| (String::new(), 0));
        for _ in 0..16 {
            let line = lines.next().expect("biome line");
            let mut tok = line.split_whitespace();
            assert_eq!(tok.next(), Some("biome"));
            let qx: i32 = tok.next().expect("qx").parse().expect("qx int");
            let qz: i32 = tok.next().expect("qz").parse().expect("qz int");
            let id = tok.next().expect("biome id").to_string();
            let y: i32 = tok.next().expect("biome y").parse().expect("y int");
            biome_quarts[(qz * 4 + qx) as usize] = (id, y);
        }

        let mut fields = Vec::with_capacity(3);
        for _ in 0..3 {
            let line = lines.next().expect("stage line");
            let _label = line.strip_prefix("stage ").expect("stage prefix");
            let mut field = BlockField::new(min_y, height);
            while let Some(&peeked) = lines.peek() {
                if peeked.starts_with("col ") {
                    lines.next();
                    let mut tok = peeked.split_whitespace();
                    tok.next();
                    let lx: i32 = tok.next().expect("col lx").parse().expect("lx int");
                    let lz: i32 = tok.next().expect("col lz").parse().expect("lz int");
                    let mut y = min_y;
                    let end = min_y + height;
                    while y < end {
                        let Some(&run_line) = lines.peek() else { break };
                        if run_line.starts_with("col ") || run_line.starts_with("stage ") || run_line.starts_with("seed ") {
                            break;
                        }
                        lines.next();
                        let mut rt = run_line.split_whitespace();
                        let ry: i32 = rt.next().expect("run y").parse().expect("run y int");
                        let count: i32 = rt.next().expect("run count").parse().expect("count int");
                        let state = rt.next().expect("run state");
                        assert_eq!(ry, y, "run start must be contiguous (col {lx},{lz})");
                        for dy in 0..count {
                            field.set(lx, y + dy, lz, state.to_string());
                        }
                        y += count;
                    }
                } else {
                    break;
                }
            }
            fields.push(field);
        }
        let postfeatures = fields.pop().expect("postfeatures field");
        let postcarve = fields.pop().expect("postcarve field");
        let postsurface = fields.pop().expect("postsurface field");

        out.push(ChunkFixture {
            seed,
            chunk_x,
            chunk_z,
            min_y,
            height,
            sea_level,
            biome_quarts,
            postsurface,
            postcarve,
            postfeatures,
        });
    }
    out
}

// ---------------------------------------------------------------------------
// Diff engine
// ---------------------------------------------------------------------------

/// One mismatched position.
#[derive(Debug, Clone)]
pub struct Mismatch {
    pub lx: i32,
    pub y: i32,
    pub lz: i32,
    pub expected: String,
    pub got: String,
}

impl Mismatch {
    /// True when `expected`/`got` name the same block (`minecraft:water[level=0]`
    /// vs `minecraft:water`) and differ only in block-state *properties*.
    ///
    /// Measured, not assumed: every property-only mismatch found against the
    /// committed fixtures is `water`/`lava` missing their `level` property —
    /// [`lodestone_worldgen::overworld::OverworldGenerator`]'s fluid fill
    /// writes the bare `default_fluid` string from `noise_settings/overworld.json`
    /// (`"minecraft:water"`, no `Properties`), so it never emits `[level=0]`
    /// even though every vanilla water block-state string always lists its
    /// `level` property. This is a real, if boring, representation gap (the
    /// engine has no concept of partial fluid levels at all) — not a false
    /// positive to silently discard, which is why [`DiffReport`] still counts
    /// it, just in its own bucket instead of blended into "real" mismatches
    /// that mean composition/algorithm differences.
    #[must_use]
    pub fn same_block_id(&self) -> bool {
        self.expected.split('[').next() == self.got.split('[').next()
    }
}

/// The result of diffing a generated field against a vanilla fixture over a
/// whole `16 x height x 16` chunk. Always records *where* it differs, never
/// only a fraction — `CLAUDE.md`'s "measure by location, never by frame
/// average" applies to worldgen exactly as it does to pixels.
#[derive(Debug)]
pub struct DiffReport {
    pub min_y: i32,
    pub height: i32,
    pub total: usize,
    pub mismatches: Vec<Mismatch>,
}

impl DiffReport {
    #[must_use]
    pub fn match_count(&self) -> usize {
        self.total - self.mismatches.len()
    }

    #[must_use]
    pub fn match_fraction(&self) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        self.match_count() as f64 / self.total as f64
    }

    /// Mismatches where the block id itself differs (a real composition/
    /// algorithm gap — different terrain), as opposed to
    /// [`Self::representation_only_mismatches`] (same block, different
    /// property string). See [`Mismatch::same_block_id`].
    #[must_use]
    pub fn real_mismatches(&self) -> Vec<&Mismatch> {
        self.mismatches.iter().filter(|m| !m.same_block_id()).collect()
    }

    /// Mismatches where both sides name the same block id — currently always
    /// the fluid-`level`-property gap (see [`Mismatch::same_block_id`]).
    #[must_use]
    pub fn representation_only_mismatches(&self) -> Vec<&Mismatch> {
        self.mismatches.iter().filter(|m| m.same_block_id()).collect()
    }

    /// Inclusive `(min, max)` local `(lx, y, lz)` bounding box of every
    /// mismatch, or `None` if there are none.
    #[must_use]
    pub fn bounding_box(&self) -> Option<((i32, i32, i32), (i32, i32, i32))> {
        let mut it = self.mismatches.iter();
        let first = it.next()?;
        let mut min = (first.lx, first.y, first.lz);
        let mut max = min;
        for m in it {
            min.0 = min.0.min(m.lx);
            min.1 = min.1.min(m.y);
            min.2 = min.2.min(m.lz);
            max.0 = max.0.max(m.lx);
            max.1 = max.1.max(m.y);
            max.2 = max.2.max(m.lz);
        }
        Some((min, max))
    }

    /// `(mismatch_count, total_count)` per 16-tall Y section (`y >> 4`),
    /// sorted by section index — lets a reader see "this is all one carved
    /// tunnel at y=32..48" instead of a single opaque percentage.
    #[must_use]
    pub fn by_section(&self) -> std::collections::BTreeMap<i32, (usize, usize)> {
        let mut map = std::collections::BTreeMap::new();
        for ly in 0..self.height {
            let y = self.min_y + ly;
            map.entry(y.div_euclid(16)).or_insert((0usize, 0usize)).1 += 16 * 16;
        }
        for m in &self.mismatches {
            map.entry(m.y.div_euclid(16)).or_insert((0, 0)).0 += 1;
        }
        map
    }

    /// Human-readable summary: match fraction, bounding box, per-section
    /// breakdown (sections with >0 mismatches only), and up to `sample` example
    /// mismatches.
    #[must_use]
    pub fn summary(&self, sample: usize) -> String {
        let mut s = String::new();
        let real = self.real_mismatches().len();
        let repr = self.representation_only_mismatches().len();
        writeln!(
            s,
            "{}/{} match ({:.4}%), {} mismatches ({real} real block-id differences, {repr} same-block property-only)",
            self.match_count(),
            self.total,
            self.match_fraction() * 100.0,
            self.mismatches.len()
        )
        .unwrap();
        if let Some((min, max)) = self.bounding_box() {
            writeln!(s, "bounding box: {min:?} .. {max:?}").unwrap();
        }
        let sections = self.by_section();
        let dirty: Vec<_> = sections.iter().filter(|(_, (bad, _))| *bad > 0).collect();
        if !dirty.is_empty() {
            writeln!(s, "sections with mismatches ({} of {}):", dirty.len(), sections.len()).unwrap();
            for (sec, (bad, total)) in dirty {
                writeln!(s, "  y[{}..{}): {bad}/{total}", sec * 16, sec * 16 + 16).unwrap();
            }
        }
        for m in self.mismatches.iter().take(sample) {
            writeln!(s, "  ({}, {}, {}): expected {:?}, got {:?}", m.lx, m.y, m.lz, m.expected, m.got).unwrap();
        }
        if self.mismatches.len() > sample {
            writeln!(s, "  ... and {} more", self.mismatches.len() - sample).unwrap();
        }
        s
    }
}

/// Diffs `generated(lx, y, lz)` against `vanilla(lx, y, lz)` over the whole
/// `16 x height x 16` chunk starting at `min_y`. Visits every cell (never a
/// short-circuit), so `report.total` is always exactly `16*16*height` —
/// asserting that in the caller is the standing anti-vacuity check ("did the
/// loop actually visit everything") this codebase's other parity tests use.
pub fn diff_field(
    min_y: i32,
    height: i32,
    generated: impl Fn(i32, i32, i32) -> String,
    vanilla: impl Fn(i32, i32, i32) -> String,
) -> DiffReport {
    let mut mismatches = Vec::new();
    let mut total = 0usize;
    for lz in 0..16i32 {
        for lx in 0..16i32 {
            for ly in 0..height {
                let y = min_y + ly;
                total += 1;
                let want = vanilla(lx, y, lz);
                let got = generated(lx, y, lz);
                if want != got {
                    mismatches.push(Mismatch {
                        lx,
                        y,
                        lz,
                        expected: want,
                        got,
                    });
                }
            }
        }
    }
    DiffReport {
        min_y,
        height,
        total,
        mismatches,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_round_trips_through_raw_parse() {
        let raw = "\
meta.seed 42
meta.chunkX 0
meta.chunkZ 0
meta.minY -64
meta.height 8
meta.seaLevel 63
biome.0,0 minecraft:plains 5
biome.1,0 minecraft:plains 5
biome.2,0 minecraft:plains 5
biome.3,0 minecraft:plains 5
biome.0,1 minecraft:plains 5
biome.1,1 minecraft:plains 5
biome.2,1 minecraft:plains 5
biome.3,1 minecraft:plains 5
biome.0,2 minecraft:plains 5
biome.1,2 minecraft:plains 5
biome.2,2 minecraft:plains 5
biome.3,2 minecraft:plains 5
biome.0,3 minecraft:plains 5
biome.1,3 minecraft:plains 5
biome.2,3 minecraft:plains 5
biome.3,3 minecraft:plains 5
postsurface.0,-64,0 minecraft:stone
postsurface.0,-63,0 minecraft:stone
postsurface.0,-62,0 minecraft:air
postcarve.0,-64,0 minecraft:stone
postcarve.0,-63,0 minecraft:air
postcarve.0,-62,0 minecraft:air
postfeatures.0,-64,0 minecraft:stone
postfeatures.0,-63,0 minecraft:air
postfeatures.0,-62,0 minecraft:iron_ore
meta.done 0,0
";
        let fixtures = parse_raw_dump(raw);
        assert_eq!(fixtures.len(), 1);
        let compact = encode_compact(&fixtures);
        let round = parse_compact(&compact);
        assert_eq!(round.len(), 1);
        let f = &round[0];
        assert_eq!(f.seed, 42);
        assert_eq!(f.postsurface.get(0, -64, 0), "minecraft:stone");
        assert_eq!(f.postsurface.get(0, -63, 0), "minecraft:stone");
        assert_eq!(f.postsurface.get(0, -62, 0), "minecraft:air");
        assert_eq!(f.postcarve.get(0, -64, 0), "minecraft:stone");
        assert_eq!(f.postcarve.get(0, -63, 0), "minecraft:air");
        assert_eq!(f.postfeatures.get(0, -64, 0), "minecraft:stone");
        assert_eq!(f.postfeatures.get(0, -62, 0), "minecraft:iron_ore");
        // Untouched position not in the raw dump reads as air on both sides.
        assert_eq!(f.postsurface.get(5, 5, 5), "minecraft:air");
        assert_eq!(f.postfeatures.get(5, 5, 5), "minecraft:air");
    }

    #[test]
    fn diff_field_visits_every_cell_and_localises_a_single_change() {
        let min_y = 0;
        let height = 4;
        let report = diff_field(
            min_y,
            height,
            |lx, y, lz| {
                if lx == 3 && y == 2 && lz == 7 {
                    "minecraft:diamond_block".to_string()
                } else {
                    "minecraft:stone".to_string()
                }
            },
            |_lx, _y, _lz| "minecraft:stone".to_string(),
        );
        assert_eq!(report.total, 16 * 16 * height as usize);
        assert_eq!(report.mismatches.len(), 1);
        let (min, max) = report.bounding_box().expect("bbox");
        assert_eq!(min, (3, 2, 7));
        assert_eq!(max, (3, 2, 7));
    }
}
