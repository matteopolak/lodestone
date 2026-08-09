//! The **outside judge** for [`compute_column_light_with_neighbours`]: the sky
//! and block light a real Mojang 26.2 server computed, wrote into a region file,
//! and never showed us.
//!
//! # Why this shape
//!
//! Every other light gate in this repo either compares our engine against itself
//! (`decode(encode(x)) == x`, the closed loop) or needs a live container. This one
//! needs neither. `.cache/mc/survival/world` is a world a real vanilla server
//! generated and lit, and each `minecraft:full` chunk's sections carry vanilla's
//! own `SkyLight`/`BlockLight` byte arrays — computed by vanilla's
//! `SkyLightEngine`/`BlockLightEngine`, stored *independently* of the
//! `block_states` containers we read the terrain from. So the blocks are the input
//! and the light is the expected answer, and neither came from us.
//!
//! # The input has to discriminate, and most chunks do not
//!
//! A flat, fully-lit surface chunk agrees under almost any implementation,
//! including a broken one: everything above ground is 15, everything sealed below
//! is 0, and no propagation happens. Such a chunk measures that the code runs.
//!
//! [`discriminating_chunks`] therefore ranks candidates by their count of
//! **partial** sky cells — vanilla's own value in `1..=14`, which can only exist
//! where light spread sideways or attenuated through something. That is the
//! population where a local-only propagator and a real flood fill give different
//! answers: cave mouths, overhangs, and the vertical shafts under trees that this
//! file exists because of. The chosen chunks' partial counts are printed, and a
//! floor is asserted, so "we accidentally surveyed superflat again" fails instead
//! of passing.
//!
//! # And the answer is reported by location
//!
//! A match percentage cannot tell a uniformly-slightly-wrong volume from one
//! pitch-black shaft, and the shaft is the entire bug this file was written for.
//! [`Survey`] therefore carries a bounding box of every disagreement plus the
//! worst few cells by magnitude, and prints them on failure.
//!
//! # Scope, stated so the next reader does not over-read the result
//!
//! * We compute the **centre** of a loaded 3×3, so cross-chunk propagation is
//!   resolved and the seam residual `compute_column_light`'s isolated form leaves
//!   is not in these numbers.
//! * A light section vanilla **omitted** asserts nothing (vanilla drops an
//!   all-zero `DataLayer`), so it is skipped and counted. `sections_skipped` being
//!   large would silently shrink the survey, which is why it is printed.
//! * Vanilla lights a 3×3 of its own with *its* neighbours loaded, two chunks out.
//!   Ours cannot see two chunks out — but it does not need to: light decays at
//!   least one level per block and 15 < 16, so no source two chunks away reaches
//!   the centre. Where that reasoning is right, exact parity is available; where it
//!   is not, the failure output says where.

#![allow(clippy::items_after_statements)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lodestone_core::{Nbt, Reader, read_named_nbt};
use lodestone_world::{
    BlockVolume, ColumnLight, LightData, LightProperties, NibbleArray, Neighbourhood,
    compute_column_light, compute_column_light_with_neighbours,
};

/// The 26.2 overworld: `yPos = -4` (min section) × 16, 24 sections of 16.
const MIN_Y: i32 = -64;
const SECTION_COUNT: usize = 24;
/// `min_y >> 4` — the signed section index of the bottom *block* section.
const MIN_SECTION: i32 = MIN_Y >> 4;

fn region_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.cache/mc/survival/world/dimensions/minecraft/overworld/region")
}

// ---------------------------------------------------------------------------
// NBT helpers
// ---------------------------------------------------------------------------

fn field<'a>(nbt: &'a Nbt, key: &str) -> Option<&'a Nbt> {
    match nbt {
        Nbt::Compound(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
        _ => None,
    }
}

fn as_str<'a>(nbt: &'a Nbt) -> Option<&'a str> {
    match nbt {
        Nbt::String(s) => Some(s.as_str()),
        _ => None,
    }
}

fn as_i32(nbt: &Nbt) -> Option<i32> {
    match nbt {
        Nbt::Byte(v) => Some(i32::from(*v)),
        Nbt::Short(v) => Some(i32::from(*v)),
        Nbt::Int(v) => Some(*v),
        _ => None,
    }
}

fn as_list<'a>(nbt: &'a Nbt) -> Option<&'a [Nbt]> {
    match nbt {
        Nbt::List { elements, .. } => Some(elements.as_slice()),
        _ => None,
    }
}

fn as_byte_array<'a>(nbt: &'a Nbt) -> Option<&'a [i8]> {
    match nbt {
        Nbt::ByteArray(v) => Some(v.as_slice()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Block properties, from the 26.2 census
// ---------------------------------------------------------------------------

/// [`LightProperties`] over **global block-state ids**, straight through
/// `lodestone_data::light_props` — the same census the integrated server's chunk
/// encoder uses. Nothing in this file invents a dampening or an emission.
struct CensusProps;

impl LightProperties for CensusProps {
    fn opacity(&self, state: u32) -> u8 {
        lodestone_data::light_props::dampening(state)
    }
    fn emission(&self, state: u32) -> u8 {
        lodestone_data::light_props::emission(state)
    }
}

/// The negative control's props: **everything is transparent and nothing emits**.
///
/// This is the detector proof. It is not a plausible implementation — it is a
/// deliberately wrong one whose sky light floods every cell in the world — so the
/// survey must report a large disagreement against it at the very same chunks
/// where `CensusProps` reports parity. Without this, a `0 disagreements` result
/// could just as well mean the comparator never looked at anything.
struct AllTransparentProps;

impl LightProperties for AllTransparentProps {
    fn opacity(&self, _state: u32) -> u8 {
        0
    }
    fn emission(&self, _state: u32) -> u8 {
        0
    }
}

/// Resolves a vanilla NBT palette entry — `{Name, Properties}` — to the global
/// block-state id `lodestone_data::light_props` is keyed by.
///
/// `block_states::state_id` sorts the property list itself, so wire order does not
/// matter. `None` means the census cannot name this state, which would silently
/// darken the survey; the caller counts those and refuses to survey at all if any
/// appear.
fn palette_entry_state_id(entry: &Nbt) -> Option<u32> {
    let name = as_str(field(entry, "Name")?)?;
    let props: Vec<(&str, &str)> = match field(entry, "Properties") {
        Some(Nbt::Compound(fields)) => fields
            .iter()
            .filter_map(|(k, v)| as_str(v).map(|v| (k.as_str(), v)))
            .collect(),
        _ => Vec::new(),
    };
    let canonical = if props.is_empty() {
        name.to_string()
    } else {
        let body: Vec<String> = props.iter().map(|(k, v)| format!("{k}={v}")).collect();
        format!("{name}[{}]", body.join(","))
    };
    lodestone_data::block_states::state_id(&canonical)
}

/// Vanilla's **non-spanning** `SimpleBitStorage` read: `64 / bits` entries per
/// long, no entry straddling a long boundary.
///
/// Getting this wrong is invisible for every palette of 16 or fewer entries,
/// because 4 bits divides 64 evenly — the same trap
/// `lodestone-server/tests/chunk_nbt_vanilla_oracle.rs` builds a dedicated
/// control for.
fn unpack_non_spanning(data: &[i64], count: usize, bits: u32) -> Vec<u32> {
    let per_long = (64 / bits) as usize;
    let mask = (1u64 << bits) - 1;
    (0..count)
        .map(|i| {
            let long = data[i / per_long] as u64;
            let shift = u32::try_from(i % per_long).expect("index fits") * bits;
            u32::try_from((long >> shift) & mask).expect("palette index fits")
        })
        .collect()
}

// ---------------------------------------------------------------------------
// One decoded vanilla chunk
// ---------------------------------------------------------------------------

/// A real chunk: the blocks vanilla generated, and the light vanilla computed for
/// them, kept apart so one can be the input and the other the expected answer.
struct VanillaChunk {
    cx: i32,
    cz: i32,
    /// Global block-state ids, indexed `y_rel * 256 + z * 16 + x` where
    /// `y_rel = world_y - MIN_Y`.
    ///
    /// `u16` rather than `u32` on purpose: 26.2 has ~32k block states, so every id
    /// fits, and this array is 98,304 cells per chunk over hundreds of chunks —
    /// the difference is ~150 MB of resident set on a machine that runs several
    /// agents at once. [`BlockVolume::block`] widens on the way out.
    blocks: Vec<u16>,
    /// Vanilla's own light, in [`ColumnLight`]'s own light-section indexing:
    /// section `s` spans world `y` from `MIN_Y - 16 + s * 16`. Sections vanilla
    /// omitted stay [`LightData::Missing`] and assert nothing.
    vanilla_light: ColumnLight,
}

impl VanillaChunk {
    /// The canonical name of the block state at chunk-local `x`/`z` and world `y`,
    /// or `<outside the column>` for the apron. Used only to make a disagreement
    /// legible: a coordinate says where, this says what.
    fn state_name_at(&self, x: usize, y: i32, z: usize) -> String {
        if y < MIN_Y || y >= MIN_Y + (SECTION_COUNT as i32) * 16 {
            return "<outside the column>".to_string();
        }
        let id = BlockVolume::block(self, x, y, z);
        lodestone_data::block_states::block_name(id)
            .map_or_else(|| format!("<unnamed state {id}>"), ToString::to_string)
    }
}

impl BlockVolume for VanillaChunk {
    fn block(&self, x: usize, y: i32, z: usize) -> u32 {
        // Air outside the built column — the engine reads one section of apron
        // above and below, which is exactly what the sky flood needs to seed.
        if y < MIN_Y || y >= MIN_Y + (SECTION_COUNT as i32) * 16 {
            return u32::from(air_state_id());
        }
        let y_rel = usize::try_from(y - MIN_Y).expect("in range");
        u32::from(self.blocks[y_rel * 256 + z * 16 + x])
    }
    fn min_y(&self) -> i32 {
        MIN_Y
    }
    fn section_count(&self) -> usize {
        SECTION_COUNT
    }
}

/// `minecraft:air`'s global state id, asked of the same census the palette
/// resolves through rather than assumed to be `0`. It happens to be `0` today,
/// and hardcoding that is how the apron above the world quietly becomes a solid
/// block the next time the state table is regenerated.
fn air_state_id() -> u16 {
    u16::try_from(lodestone_data::block_states::air_state_id())
        .expect("26.2 has ~32k block states; every id fits in u16")
}

/// Decodes one chunk's blocks and vanilla light, or `None` if the chunk is not
/// fully generated and lit (`Status != minecraft:full`).
///
/// Returns the count of palette entries the census could not name alongside the
/// chunk, because a shortfall there darkens our side of the comparison and the
/// caller must be able to refuse rather than survey a lie.
fn decode_chunk(nbt: &Nbt) -> Option<(VanillaChunk, usize)> {
    if as_str(field(nbt, "Status")?)? != "minecraft:full" {
        return None;
    }
    let cx = as_i32(field(nbt, "xPos")?)?;
    let cz = as_i32(field(nbt, "zPos")?)?;

    let air = air_state_id();
    let mut blocks = vec![air; SECTION_COUNT * 4096];
    let mut vanilla_light = ColumnLight::new(SECTION_COUNT);
    let mut unresolved = 0usize;

    for section in as_list(field(nbt, "sections")?)? {
        let Some(section_y) = field(section, "Y").and_then(as_i32) else {
            continue;
        };

        // Blocks. Section Y is the absolute (signed) section index.
        if let Some(container) = field(section, "block_states") {
            let index = section_y - MIN_SECTION;
            if (0..SECTION_COUNT as i32).contains(&index) {
                let base = usize::try_from(index).expect("checked") * 4096;
                let palette: Vec<u16> = as_list(field(container, "palette")?)?
                    .iter()
                    .map(|entry| {
                        palette_entry_state_id(entry)
                            .and_then(|id| u16::try_from(id).ok())
                            .unwrap_or_else(|| {
                                unresolved += 1;
                                air
                            })
                    })
                    .collect();
                match field(container, "data") {
                    Some(Nbt::LongArray(data)) => {
                        // `max(4, ceil_log2(len))`, vanilla's own
                        // `PalettedContainer` bit width for a chunk section.
                        let bits = 4.max(u32::BITS - (palette.len() as u32 - 1).leading_zeros());
                        for (i, pi) in unpack_non_spanning(data, 4096, bits).into_iter().enumerate()
                        {
                            blocks[base + i] = palette[pi as usize];
                        }
                    }
                    // No `data` means a single-entry palette filling the section.
                    _ => {
                        let fill = *palette.first()?;
                        for cell in &mut blocks[base..base + 4096] {
                            *cell = fill;
                        }
                    }
                }
            }
        }

        // Light. Light sections run from `MIN_SECTION - 1` to `MIN_SECTION +
        // SECTION_COUNT`, which is `ColumnLight`'s `0..section_count + 2`.
        let light_index = section_y - MIN_SECTION + 1;
        if !(0..(SECTION_COUNT + 2) as i32).contains(&light_index) {
            continue;
        }
        let s = usize::try_from(light_index).expect("checked");
        for (key, layer) in [("SkyLight", true), ("BlockLight", false)] {
            let Some(bytes) = field(section, key).and_then(as_byte_array) else {
                continue;
            };
            if bytes.len() != 2048 {
                continue;
            }
            let target = if layer {
                vanilla_light.sky_mut(s)
            } else {
                vanilla_light.block_mut(s)
            };
            // Vanilla's `DataLayer` nibble order is `y << 8 | z << 4 | x`, the
            // same order `NibbleArray::index` produces, so the byte array copies
            // straight across nibble for nibble.
            for i in 0..NibbleArray::LEN {
                #[expect(
                    clippy::cast_sign_loss,
                    reason = "NBT byte arrays are signed; the nibbles are raw bits"
                )]
                let byte = bytes[i >> 1] as u8;
                let value = if i & 1 == 0 { byte & 0x0F } else { byte >> 4 };
                target.set(i, value);
            }
        }
    }

    Some((
        VanillaChunk {
            cx,
            cz,
            blocks,
            vanilla_light,
        },
        unresolved,
    ))
}

/// Every `minecraft:full` chunk in every overworld region file, keyed by chunk
/// position, together with the total count of unresolvable palette entries.
fn load_world() -> (HashMap<(i32, i32), VanillaChunk>, usize) {
    let dir = region_dir();
    let mut out = HashMap::new();
    let mut unresolved = 0usize;
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "mca"))
        .collect();
    files.sort();
    assert!(
        !files.is_empty(),
        "no .mca files under {} — the oracle world is missing, not empty",
        dir.display()
    );
    // One region file is 1024 candidate chunks (741 of them `minecraft:full` in
    // `r.0.0.mca`), far more 3x3 neighbourhoods than the survey needs. Reading all
    // 29 costs seconds of decode and gigabytes of resident set for nothing.
    for path in files.iter().take(1) {
        let bytes = std::fs::read(path).expect("read region file");
        let region = lodestone_anvil::region::RegionFile::parse(&bytes).expect("parse region");
        for local_z in 0..32u8 {
            for local_x in 0..32u8 {
                let Ok(Some(raw)) = region.read_chunk_nbt_bytes(local_x, local_z) else {
                    continue;
                };
                let mut reader = Reader::new(&raw);
                let Ok((_, nbt)) = read_named_nbt(&mut reader) else {
                    continue;
                };
                if let Some((chunk, missed)) = decode_chunk(&nbt) {
                    unresolved += missed;
                    out.insert((chunk.cx, chunk.cz), chunk);
                }
            }
        }
    }
    (out, unresolved)
}

// ---------------------------------------------------------------------------
// Discriminating input selection
// ---------------------------------------------------------------------------

/// How many of vanilla's own sky cells in this chunk are **partial** — value in
/// `1..=14`.
///
/// A useful figure but, on its own, the wrong selection criterion, and finding
/// that out cost a run: the six highest-scoring chunks in `r.0.0.mca` all scored
/// exactly `3584`, which is `14 × 256` — fourteen complete 16×16 layers, each
/// uniform. They are open ocean. Sky light really does attenuate down through
/// water one level per block, so the score was honest, but every cell in such a
/// column is lit from *directly above* and a purely vertical propagator would get
/// the whole chunk right.
fn partial_sky_cells(light: &ColumnLight) -> usize {
    (0..light.light_section_count())
        .map(|s| {
            (0..NibbleArray::LEN)
                .filter(|&i| matches!(light.sky(s).get(i), Some(1..=14)))
                .count()
        })
        .sum()
}

/// How many cells hold a value **different from their `+x` or `+z` neighbour** in
/// vanilla's own light, summed over both layers.
///
/// This is the criterion that actually discriminates, and it is worth stating why
/// in the terms the two hypotheses differ over. Light that fell straight down an
/// open column is equal across a horizontal layer; light that got somewhere by
/// spreading *sideways* is not. So a non-zero count here is a direct census of the
/// cells whose value can only have come from lateral propagation — cave mouths,
/// overhangs, and the shafts under trees — which is exactly the population where a
/// flood fill and a local patch-up give different answers.
///
/// An open-ocean column scores `0` on this and `3584` on [`partial_sky_cells`].
/// Both are printed so the shape of the chosen input is visible rather than
/// assumed.
fn horizontally_varying_cells(light: &ColumnLight) -> usize {
    let mut count = 0usize;
    for s in 0..light.light_section_count() {
        for (data, _) in [(light.sky(s), 0u8), (light.block(s), 1)] {
            if matches!(data, LightData::Missing) {
                continue;
            }
            for y in 0..16 {
                for z in 0..16 {
                    for x in 0..16 {
                        let here = data.get(NibbleArray::index(x, y, z)).unwrap_or(0);
                        let varies = (x + 1 < 16
                            && data.get(NibbleArray::index(x + 1, y, z)).unwrap_or(0) != here)
                            || (z + 1 < 16
                                && data.get(NibbleArray::index(x, y, z + 1)).unwrap_or(0) != here);
                        if varies {
                            count += 1;
                        }
                    }
                }
            }
        }
    }
    count
}

/// One candidate centre chunk and the two figures that describe how hard it is.
#[derive(Clone, Copy)]
struct Candidate {
    cx: i32,
    cz: i32,
    /// [`horizontally_varying_cells`] — the ranking key.
    lateral: usize,
    /// [`partial_sky_cells`] — reported, not ranked on.
    partial: usize,
}

/// The `count` chunks with the most **laterally varying** light whose whole 3×3
/// neighbourhood is loaded, so [`compute_column_light_with_neighbours`] can be
/// exact for them.
fn discriminating_chunks(
    world: &HashMap<(i32, i32), VanillaChunk>,
    count: usize,
) -> Vec<Candidate> {
    let mut ranked: Vec<Candidate> = world
        .values()
        .filter(|c| {
            (-1..=1).all(|dz| (-1..=1).all(|dx| world.contains_key(&(c.cx + dx, c.cz + dz))))
        })
        .map(|c| Candidate {
            cx: c.cx,
            cz: c.cz,
            lateral: horizontally_varying_cells(&c.vanilla_light),
            partial: partial_sky_cells(&c.vanilla_light),
        })
        .collect();
    // Descending by lateral variation, then by position so the choice is stable
    // across runs rather than HashMap-order dependent.
    ranked.sort_by(|a, b| {
        b.lateral
            .cmp(&a.lateral)
            .then(a.cx.cmp(&b.cx))
            .then(a.cz.cmp(&b.cz))
    });
    ranked.truncate(count);
    ranked
}

// ---------------------------------------------------------------------------
// The survey: counts, and where
// ---------------------------------------------------------------------------

/// A disagreement survey reported **by location**. A fraction cannot distinguish
/// a uniformly-slightly-wrong volume from one pitch-black shaft, so this carries
/// the bounding box and the worst cells too.
#[derive(Default)]
struct Survey {
    cells_compared: usize,
    sections_skipped: usize,
    sky_disagreements: usize,
    block_disagreements: usize,
    /// `(min_x, min_y, min_z, max_x, max_y, max_z)` in **world** coordinates over
    /// every disagreeing cell, or `None` when there are none.
    bbox: Option<(i32, i32, i32, i32, i32, i32)>,
    /// Up to eight worst cells by magnitude.
    worst: Vec<WorstCell>,
    /// Disagreeing cells where **ours is brighter** than vanilla's. The census's
    /// own convention is that every gap darkens or occludes, so this population
    /// is supposed to be empty for a different reason than the others.
    ours_brighter: usize,
    /// Disagreeing cells tallied by the **block state that is there**, so a
    /// residual attributable to one missing census row reads as one missing census
    /// row rather than as thousands of unexplained cells.
    ///
    /// This is the field that turned the first real run's "4513 block-light
    /// disagreements" into "`minecraft:glow_lichen` emits 7 in vanilla and 0 in our
    /// census, plus its falloff" — a one-line finding in `lodestone-data`, not a
    /// propagation defect here.
    by_state: HashMap<String, usize>,
}

/// One disagreeing cell, kept with **the block that is actually there**.
///
/// The block name is the load-bearing field and it was added after the first run:
/// 908 block-light cells disagreed, all of them ours-darker, and a list of
/// coordinates cannot distinguish "our flood stopped early" from "the census has
/// no emission for this state". Naming the state answers that in one line.
struct WorstCell {
    delta: u8,
    layer: &'static str,
    wx: i32,
    wy: i32,
    wz: i32,
    ours: u8,
    theirs: u8,
    state: String,
}

impl Survey {
    #[allow(clippy::too_many_arguments)]
    fn note(
        &mut self,
        layer: &'static str,
        cx: i32,
        cz: i32,
        s: usize,
        x: usize,
        y_local: usize,
        z: usize,
        ours: u8,
        theirs: u8,
        state: String,
    ) {
        let wx = cx * 16 + i32::try_from(x).expect("fits");
        let wz = cz * 16 + i32::try_from(z).expect("fits");
        let wy = MIN_Y - 16
            + i32::try_from(s * 16 + y_local).expect("fits");
        let (min_x, min_y, min_z, max_x, max_y, max_z) =
            self.bbox.unwrap_or((wx, wy, wz, wx, wy, wz));
        self.bbox = Some((
            min_x.min(wx),
            min_y.min(wy),
            min_z.min(wz),
            max_x.max(wx),
            max_y.max(wy),
            max_z.max(wz),
        ));
        if ours > theirs {
            self.ours_brighter += 1;
        }
        *self.by_state.entry(state.clone()).or_default() += 1;
        self.worst.push(WorstCell {
            delta: ours.abs_diff(theirs),
            layer,
            wx,
            wy,
            wz,
            ours,
            theirs,
            state,
        });
        self.worst.sort_by(|a, b| b.delta.cmp(&a.delta));
        self.worst.truncate(8);
    }

    fn disagreements(&self) -> usize {
        self.sky_disagreements + self.block_disagreements
    }

    /// The failure output the evidence rules ask for: *where*, not *what*.
    fn report(&self, label: &str) -> String {
        use std::fmt::Write as _;
        let mut s = String::new();
        let _ = write!(
            s,
            "{label}: {} disagreements ({} sky, {} block) over {} cells compared, \
             {} light sections skipped as absent from the oracle, {} of them ours-brighter",
            self.disagreements(),
            self.sky_disagreements,
            self.block_disagreements,
            self.cells_compared,
            self.sections_skipped,
            self.ours_brighter,
        );
        if let Some((x0, y0, z0, x1, y1, z1)) = self.bbox {
            let _ = write!(
                s,
                "\n  bounding box of disagreement: x {x0}..={x1}, y {y0}..={y1}, z {z0}..={z1}"
            );
        }
        let mut tally: Vec<(&String, &usize)> = self.by_state.iter().collect();
        tally.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        for (state, count) in tally.iter().take(6) {
            let _ = write!(s, "\n  {count} disagreeing cells in {state}");
        }
        for w in &self.worst {
            let _ = write!(
                s,
                "\n  worst {} @ ({}, {}, {}): ours {}, vanilla {} (delta {}) in {}",
                w.layer, w.wx, w.wy, w.wz, w.ours, w.theirs, w.delta, w.state,
            );
        }
        s
    }
}

/// Diffs one computed [`ColumnLight`] against vanilla's, accumulating into
/// `survey`. Light sections vanilla omitted are counted and skipped: an absent
/// `DataLayer` is not an assertion of zero.
fn survey_column(survey: &mut Survey, centre: &VanillaChunk, ours: &ColumnLight) {
    let (cx, cz) = (centre.cx, centre.cz);
    let theirs = &centre.vanilla_light;
    for s in 0..theirs.light_section_count().min(ours.light_section_count()) {
        for (layer, our_data, their_data) in [
            ("sky", ours.sky(s), theirs.sky(s)),
            ("block", ours.block(s), theirs.block(s)),
        ] {
            if matches!(their_data, LightData::Missing) {
                survey.sections_skipped += 1;
                continue;
            }
            for y_local in 0..16 {
                for z in 0..16 {
                    for x in 0..16 {
                        let idx = NibbleArray::index(x, y_local, z);
                        let Some(t) = their_data.get(idx) else {
                            continue;
                        };
                        let o = our_data.get(idx).unwrap_or(0);
                        survey.cells_compared += 1;
                        if o != t {
                            if layer == "sky" {
                                survey.sky_disagreements += 1;
                            } else {
                                survey.block_disagreements += 1;
                            }
                            // World y of this light cell: light section `s`
                            // starts at `MIN_Y - 16 + s * 16`.
                            let wy = MIN_Y - 16
                                + i32::try_from(s * 16 + y_local).expect("fits");
                            let state = centre.state_name_at(x, wy, z);
                            survey.note(layer, cx, cz, s, x, y_local, z, o, t, state);
                        }
                    }
                }
            }
        }
    }
}

/// Runs the whole survey over the chosen chunks with the given props, returning
/// the survey and the total partial-cell count of the input (its discriminating
/// power).
fn run_survey(props: &impl LightProperties, chunk_count: usize) -> (Survey, usize, usize) {
    let (world, unresolved) = load_world();
    assert_eq!(
        unresolved, 0,
        "the 26.2 census could not name some palette entries; every gap darkens our \
         side of this comparison, so surveying would measure the gap, not the engine"
    );
    let chosen = discriminating_chunks(&world, chunk_count);
    assert_eq!(
        chosen.len(),
        chunk_count,
        "fewer than {chunk_count} chunks have a fully loaded 3x3 neighbourhood"
    );

    let mut survey = Survey::default();
    let mut lateral_total = 0usize;
    for &Candidate { cx, cz, lateral, .. } in &chosen {
        lateral_total += lateral;
        let centre = &world[&(cx, cz)];
        let mut hood = Neighbourhood::new(centre);
        for dz in -1..=1 {
            for dx in -1..=1 {
                if (dx, dz) != (0, 0) {
                    hood = hood.with(dx, dz, &world[&(cx + dx, cz + dz)]);
                }
            }
        }
        let ours = compute_column_light_with_neighbours(&hood, props);
        survey_column(&mut survey, centre, &ours);
    }
    eprintln!(
        "chosen chunks (lateral-varying cells / partial sky cells): {:?}",
        chosen
            .iter()
            .map(|c| format!("({},{}) {}/{}", c.cx, c.cz, c.lateral, c.partial))
            .collect::<Vec<_>>()
    );
    (survey, lateral_total, chosen.len())
}

/// Chunks surveyed. Small on purpose: each centre runs a 48x48x416 flood, and a
/// handful of genuinely cave-riddled chunks is worth more than a hundred flat
/// ones.
const SURVEY_CHUNKS: usize = 6;

// ---------------------------------------------------------------------------
// Gates
// ---------------------------------------------------------------------------

/// The measurement: our engine against vanilla's stored answer, over chunks
/// chosen for the propagation they actually contain, reported by location.
///
/// The thresholds here are **not** the point and should not be relaxed to keep
/// this green. Read the printed bounding box: a residual spread thinly across a
/// cave system is a different finding from a solid dark block, and only the second
/// is the class of bug this file was written for.
#[test]
#[ignore = "requires .cache/mc/survival/world, a real 26.2 world this repo did not write"]
fn computed_light_matches_the_light_a_real_26_2_server_wrote() {
    let (survey, lateral_total, chunks) = run_survey(&CensusProps, SURVEY_CHUNKS);
    eprintln!("{}", survey.report("census props"));

    // Input floor. A survey over flat or open-ocean chunks measures that the code
    // runs; this says the input contains light that got where it is by spreading
    // sideways, which is the only thing the two hypotheses disagree about. One
    // whole 16x16 layer per chunk would be 256 cells, so a per-chunk average of
    // 1000 is a real cave system rather than a rounding artefact.
    assert!(
        lateral_total > chunks * 1000,
        "the chosen input does not exercise lateral propagation: only {lateral_total} \
         laterally-varying cells across {chunks} chunks. A purely vertical propagator \
         would agree with vanilla on such input, so this survey would prove nothing."
    );
    // Vanilla materialises a `DataLayer` only where light is non-trivial — measured
    // over `r.0.0.mca`: 2 to 7 `SkyLight` sections and 0 to 10 `BlockLight`
    // sections per `minecraft:full` chunk, out of 26 possible. So this survey's
    // scope is vanilla's own transition band, not the whole column, and the floor
    // has to be derived from that rather than from the column height: two sky
    // sections per chunk is the measured minimum.
    let floor = chunks * 2 * NibbleArray::LEN;
    assert!(
        survey.cells_compared > floor,
        "only {} cells compared against a floor of {floor} — agreement over too few \
         cells is the vacuous pass",
        survey.cells_compared
    );

    // The claim the census's own convention licenses, and the one that must hold
    // exactly: we never invent light vanilla does not have. A shortfall in the
    // dampening/emission table can only darken, so a single ours-brighter cell is
    // a propagation defect rather than a data gap.
    assert_eq!(
        survey.ours_brighter,
        0,
        "our engine produced MORE light than vanilla somewhere — the one direction \
         a census gap cannot explain.\n{}",
        survey.report("census props")
    );
}

/// The detector proof, and it is not decoration.
///
/// The gate above asserts an **absence** (no cell where we are brighter than
/// vanilla). That is worth exactly as much as the evidence the comparator would
/// have noticed one. So run the same survey with a deliberately wrong
/// implementation — every block transparent, so sky light floods the entire
/// column including sealed bedrock — and require it to fire, loudly, at the same
/// chunks.
#[test]
#[ignore = "requires .cache/mc/survival/world, a real 26.2 world this repo did not write"]
fn the_survey_detects_a_deliberately_wrong_light_engine() {
    let (survey, _, _) = run_survey(&AllTransparentProps, SURVEY_CHUNKS);
    eprintln!("{}", survey.report("all-transparent control"));
    // Derived from the survey's own scope rather than guessed: the control makes
    // every cell in every compared section a full-strength sky source, so the cells
    // it must light up are all the ones vanilla has dark. A tenth of the compared
    // population is a floor no partially-working comparator clears by accident. The
    // first version of this assertion asked for 100_000 against a population of
    // 126_976 -- a plausible round number, and wrong, exactly the way `CLAUDE.md`
    // says round numbers fail.
    let floor = survey.cells_compared / 10;
    assert!(
        survey.ours_brighter > floor,
        "the all-transparent control produced only {} ours-brighter cells against a \
         floor of {floor}. It floods every sealed cell with sky 15, so a comparator \
         that cannot see that is not measuring anything.\n{}",
        survey.ours_brighter,
        survey.report("all-transparent control")
    );
}

/// The seam residual, measured rather than asserted away.
///
/// [`compute_column_light`] treats a chunk border as opaque; the 3x3 form pulls
/// the neighbour's light in. Both are compared against the *same* vanilla answer,
/// so the difference between their disagreement counts is the cross-chunk
/// contribution — a number, on real terrain, rather than the caveat both
/// functions' docs carry.
#[test]
#[ignore = "requires .cache/mc/survival/world, a real 26.2 world this repo did not write"]
fn the_isolated_compute_is_measurably_worse_at_the_seam_than_the_3x3() {
    let (world, unresolved) = load_world();
    assert_eq!(unresolved, 0, "census gap; see the main survey");
    let chosen = discriminating_chunks(&world, SURVEY_CHUNKS);

    let mut iso = Survey::default();
    let mut hood_survey = Survey::default();
    for &Candidate { cx, cz, .. } in &chosen {
        let centre = &world[&(cx, cz)];
        survey_column(
            &mut iso,
            centre,
            &compute_column_light(centre, &CensusProps),
        );
        let mut hood = Neighbourhood::new(centre);
        for dz in -1..=1 {
            for dx in -1..=1 {
                if (dx, dz) != (0, 0) {
                    hood = hood.with(dx, dz, &world[&(cx + dx, cz + dz)]);
                }
            }
        }
        survey_column(
            &mut hood_survey,
            centre,
            &compute_column_light_with_neighbours(&hood, &CensusProps),
        );
    }
    eprintln!("{}", iso.report("isolated"));
    eprintln!("{}", hood_survey.report("3x3"));
    assert!(
        hood_survey.disagreements() < iso.disagreements(),
        "the 3x3 compute must beat the isolated one on real terrain, or the \
         neighbour plumbing is not reaching the flood.\n{}\n{}",
        iso.report("isolated"),
        hood_survey.report("3x3"),
    );
}

/// Guards the two facts the survey silently depends on, so a change to either
/// fails here rather than turning the survey vacuous.
#[test]
#[ignore = "requires .cache/mc/survival/world, a real 26.2 world this repo did not write"]
fn the_oracle_world_really_carries_vanilla_computed_light() {
    let (world, _) = load_world();
    let with_light = world
        .values()
        .filter(|c| {
            (0..c.vanilla_light.light_section_count())
                .any(|s| !matches!(c.vanilla_light.sky(s), LightData::Missing))
        })
        .count();
    assert!(
        with_light > 500,
        "only {with_light} of {} full chunks carry a SkyLight array; without vanilla's \
         own light there is no outside expectation here at all",
        world.len()
    );
    // The population `discriminating_chunks` draws from, measured on the criterion
    // it actually ranks by. `partial_sky_cells` is deliberately *not* the filter
    // here: open ocean scores 3584 on it while varying laterally nowhere at all.
    let lateral = world
        .values()
        .filter(|c| horizontally_varying_cells(&c.vanilla_light) > 1000)
        .count();
    assert!(
        lateral > 20,
        "only {lateral} chunks hold more than 1000 laterally-varying light cells — \
         the population this survey draws its discriminating input from"
    );
}
