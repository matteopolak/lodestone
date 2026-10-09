//! Copy-on-write section snapshots and their light neighbourhood.
use super::*;

/// Biome registry names in holder-id order.
pub type BiomeNames = Arc<[&'static str]>;

/// Identifies one 16³ section: its column plus the section index within that
/// column (`0` is the lowest section).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SectionKey {
    /// Column X (chunk coordinate).
    pub cx: i32,
    /// Column Z (chunk coordinate).
    pub cz: i32,
    /// Section index within the column.
    pub si: usize,
    /// Lowest world-y of the column (needed to place the section in world space).
    pub min_y: i32,
}

impl SectionKey {
    /// World-space origin (minimum corner) of this section.
    #[must_use]
    pub fn origin(&self) -> [i32; 3] {
        [self.cx * 16, self.min_y + self.si as i32 * 16, self.cz * 16]
    }

    /// This section's coordinate on the 16-block section grid — what the
    /// frustum, distance and occlusion culls are all expressed in
    /// (`lodestone_render::SectionCoord`).
    ///
    /// Derived from [`origin`](Self::origin) with `div_euclid`, not from `si`
    /// directly: `min_y` is negative in the overworld (`-64`), so a plain `/ 16`
    /// truncates toward zero and would put the sections either side of `y == 0`
    /// on the same grid row.
    #[must_use]
    pub fn coord(&self) -> lodestone_render::SectionCoord {
        let [x, y, z] = self.origin();
        (x.div_euclid(16), y.div_euclid(16), z.div_euclid(16))
    }
}

/// Whether the columns a world is meshed from are **all there already** or are
/// still arriving.
///
/// This is the fact that decides what an *absent* horizontal neighbour column
/// means, and nothing downstream of [`snapshot_section_in`] can derive it: an
/// empty slot looks identical either way. Getting it wrong in the `Streaming`
/// direction is the seam-baked-against-air defect (a seam baked against air that never heals); getting
/// it wrong in the `Complete` direction would blank the outer ring of a world
/// whose outer ring is genuinely final.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnSource {
    /// Every column that will ever exist already does — the offline demo world
    /// (`crate::worldgen::generate` emits its whole radius up front) and
    /// hermetic fixtures. An absent neighbour column is the edge of the world,
    /// so air across that seam is the **truth** and meshing against it is
    /// correct and final.
    Complete,
    /// Columns stream in from a server, in an order nothing here controls. An
    /// absent neighbour column has simply not arrived; air across that seam is a
    /// **guess**, and the wrong one often enough to be the whole of the
    /// seam-baked-against-air defect.
    Streaming,
}

/// Why one slot of a 27-section neighbourhood holds no section.
///
/// The two cases must remain distinct. A chunk seam meshed against a
/// not-yet-loaded neighbour cannot share the all-air stand-in used for the edge
/// of the world, or the wrong result is silently treated as final.
#[derive(Debug, Clone)]
pub enum Neighbour {
    /// A real section, held as a clone of the handle
    /// [`lodestone_world::World::section`] already hands back — i.e. a refcount
    /// bump, never a copy of the section's palette data. This used to be an
    /// owned `ChunkSection`, deep-cloning every populated neighbour (its
    /// paletted-container `Vec`s included) on every snapshot regardless of
    /// whether the world ever edits it — which is exactly the cost
    /// `Arc<ChunkSection>` and copy-on-write exist to avoid: see
    /// `docs/chunk-world-resource.md` on "never hold the chunk read lock across
    /// a mesh" for the same rule applied one layer up. An edit to a section this
    /// snapshot still references forks *there*, on write, only if a write
    /// actually happens — not unconditionally, here, on read.
    Present(Arc<ChunkSection>),
    /// No section, and **air is the truth**: above the build ceiling, below the
    /// bedrock floor, an all-air section elided inside a column that *has*
    /// arrived, or any absent column in a [`ColumnSource::Complete`] world.
    /// Meshing against this is correct and needs no revisiting.
    Air,
    /// No section **yet**: the column has not arrived from the server. Air here
    /// is a guess. A snapshot holding any of these is
    /// [`SnapshotOutcome::Deferred`] rather than `Ready`.
    Unloaded,
}

impl Neighbour {
    /// The section to mesh against, resolving both absent cases to the shared
    /// all-air stand-in so the mesher sees lit air rather than an unlit void.
    ///
    /// A [`SnapshotOutcome::Ready`] snapshot holds no [`Neighbour::Unloaded`],
    /// so on the render path this only ever resolves [`Neighbour::Air`].
    fn section(&self) -> &ChunkSection {
        match self {
            Neighbour::Present(s) => s.as_ref(),
            Neighbour::Air | Neighbour::Unloaded => air_section_static(),
        }
    }
}

/// An owned, `Send` copy of the 27-section neighbourhood around one section.
///
/// Index `[dx+1][dy+1][dz+1]` for `dx,dy,dz ∈ {-1,0,1}`; the centre is `[1][1][1]`.
/// Missing neighbours are all-air sections so the mesher still sees lit air there
/// rather than an unlit void — but *why* a neighbour is missing is recorded per
/// slot in [`Neighbour`], because the two reasons are not interchangeable.
#[derive(Debug)]
pub struct SectionSnapshot {
    /// Which section this is.
    pub key: SectionKey,
    pub(crate) light_revision: Option<u64>,
    /// One slot per neighbour, `[dx+1][dy+1][dz+1]`.
    pub(crate) sections: Vec<Neighbour>,
    /// Per-neighbour light, indexed identically to `sections`
    /// (`[dx+1][dy+1][dz+1]`). `None` where the neighbour column or light
    /// section is absent (edge of world / below the world). Those slots fall
    /// back to the full-bright bridge in [`mesh_snapshot`]; every present slot
    /// carries the world's real sky/block light.
    pub(crate) lights: Vec<Option<SectionLightData>>,
    /// How to resolve *absent* (`Missing`) sky light, chosen per dimension by
    /// the producer: [`snapshot_section`]'s demo world is always the
    /// overworld ([`SkyDefault::Full`]); [`snapshot_section_live`] resolves
    /// this per the *connected* dimension, defaulting to
    /// [`SkyDefault::None`] outside the overworld so absent sky stays `0`
    /// rather than defaulting up to daylight in the Nether/End.
    pub(crate) sky_default: SkyDefault,
    /// The connection's biome registry names in holder-id order; a biome id
    /// outside the table has no name. Baked into the snapshot because mesh
    /// workers see only their job channel, like [`Self::sky_default`].
    pub(crate) biome_names: BiomeNames,
}

impl SectionSnapshot {
    pub(crate) fn at(&self, dx: i32, dy: i32, dz: i32) -> &ChunkSection {
        let i = ((dx + 1) * 9 + (dy + 1) * 3 + (dz + 1)) as usize;
        self.sections[i].section()
    }

    /// How many of the 27 slots are [`Neighbour::Unloaded`] — i.e. how much of
    /// this neighbourhood is a guess rather than a reading.
    ///
    /// Zero for every snapshot the render path meshes; non-zero is exactly the
    /// [`SnapshotOutcome::Deferred`] condition. Public so a gate can assert the
    /// distinction is really being made rather than take it on trust.
    #[must_use]
    pub fn unloaded_neighbours(&self) -> usize {
        self.sections
            .iter()
            .filter(|n| matches!(n, Neighbour::Unloaded))
            .count()
    }

    /// The number of merged quads this snapshot would emit — a cheap coverage
    /// proxy for gates that need to prove a live neighbourhood produced
    /// non-trivial geometry (an empty world meshes to zero).
    #[must_use]
    pub fn quad_count<C: BlockClassifier>(&self, classifier: &C) -> usize {
        mesh_snapshot(self, classifier).quad_count()
    }

    /// A copy of this snapshot with **all light stripped** (every neighbour slot
    /// `None`), so [`mesh_snapshot`] falls back to the full-bright
    /// [`UniformLight::pre_light_bridge`] for the whole neighbourhood.
    ///
    /// This is the *control* for lighting gates: it reproduces exactly what the
    /// retired full-bright path rendered, letting a test prove that real light
    /// differs from it (the shadowed-darker-than-open-sky assertion the bridge
    /// cannot satisfy). It carries no meaning on the render path.
    #[must_use]
    pub fn full_bright_control(&self) -> SectionSnapshot {
        SectionSnapshot {
            key: self.key,
            light_revision: None,
            sections: self.sections.clone(),
            lights: (0..self.sections.len()).map(|_| None).collect(),
            sky_default: self.sky_default,
            biome_names: Arc::clone(&self.biome_names),
        }
    }
}

pub(crate) fn air_section() -> ChunkSection {
    ChunkSection::new(
        PaletteKind::block_states(),
        PaletteKind::biomes(),
        id::AIR,
        0,
    )
}

/// A process-wide shared all-air section, handed out for every
/// [`Neighbour::Air`] and [`Neighbour::Unloaded`] slot so none allocates or
/// touches a refcount.
fn air_section_static() -> &'static ChunkSection {
    static AIR: OnceLock<Arc<ChunkSection>> = OnceLock::new();
    AIR.get_or_init(|| Arc::new(air_section()))
}

/// Clone the 27-section neighbourhood around `key` out of the world, or `None`
/// when the centre is absent or entirely air.
///
/// The unbounded-height, overworld-sky, [`ColumnSource::Complete`] form of
/// [`snapshot_section_in`], for hermetic gates and the demo world, where the
/// outcome is never [`SnapshotOutcome::Deferred`].
#[must_use]
pub fn snapshot_section(
    world: &World,
    key: SectionKey,
    biome_names: BiomeNames,
) -> Option<SectionSnapshot> {
    snapshot_section_in(world, key, None, SkyDefault::Full, ColumnSource::Complete, biome_names).ready()
}

/// What [`snapshot_section_in`] found: geometry to mesh now, nothing to mesh, or
/// geometry that must **not** be meshed yet.
///
/// A section whose horizontal neighbourhood is incomplete can be meshed, but
/// every face on the incomplete seam is decided against air the neighbour has
/// not yet contradicted: doubled translucent water sides, wrong ambient
/// occlusion and smooth light, stray faces. Vanilla refuses the same build
/// until all eight horizontal neighbour columns are loaded.
#[derive(Debug)]
pub enum SnapshotOutcome {
    /// The centre holds geometry and the whole neighbourhood is known. Mesh it.
    Ready(SectionSnapshot),
    /// Nothing to draw: the centre section is absent, out of the column's
    /// vertical range, or entirely air. Any geometry already on the GPU for this
    /// key is stale and should be removed.
    Empty,
    /// The centre holds geometry, but at least one of the eight horizontal
    /// neighbour columns has not arrived. The snapshot is carried so a section
    /// already on screen can be rebuilt rather than blink out when a far chunk
    /// unloads.
    Deferred(SectionSnapshot),
}

impl SnapshotOutcome {
    /// The snapshot only when it is safe to mesh now.
    #[must_use]
    pub fn ready(self) -> Option<SectionSnapshot> {
        match self {
            SnapshotOutcome::Ready(snap) => Some(snap),
            SnapshotOutcome::Empty | SnapshotOutcome::Deferred(_) => None,
        }
    }

    /// The snapshot whether or not its neighbourhood is complete — for
    /// diagnostics and gates that want to *measure* the incomplete mesh rather
    /// than render it. Never use this to feed the screen.
    #[must_use]
    pub fn any(self) -> Option<SectionSnapshot> {
        match self {
            SnapshotOutcome::Ready(snap) | SnapshotOutcome::Deferred(snap) => Some(snap),
            SnapshotOutcome::Empty => None,
        }
    }
}

/// Clone the 27-section neighbourhood around `key` out of `world`.
///
/// The one snapshot implementation; the parameters are the per-session facts
/// the store cannot answer:
///
/// * `section_count` — the dimension's column height. `None` means unbounded.
///   Blocks are gated on it; light is not, because the topmost and bottom-most
///   sections sample the boundary light sections one past the build range.
/// * `sky_default` — how an absent sky sample resolves, by the connected
///   dimension's `has_skylight`. See [`sky_default_for_dimension`].
/// * `columns` — whether an absent horizontal neighbour column is the edge of
///   the world or a chunk still in flight. See [`ColumnSource`].
/// * `biome_names` — the registry names that biome ids index.
#[must_use]
pub fn snapshot_section_in(
    world: &World,
    key: SectionKey,
    section_count: Option<usize>,
    sky_default: SkyDefault,
    columns: ColumnSource,
    biome_names: BiomeNames,
) -> SnapshotOutcome {
    // A section index is in range when it is inside the column at all. `None`
    // leaves the top open, which is what the offline world wants: its columns
    // carry their own height and an out-of-range lookup yields nothing anyway.
    let in_range =
        |si: i32| si >= 0 && section_count.is_none_or(|count| (si as usize) < count);

    // Check the centre before the 26 neighbour lookups: scheduling a mesh for a
    // section with no geometry is the work this early return exists to skip.
    if !in_range(key.si as i32) {
        return SnapshotOutcome::Empty;
    }
    let Some(centre) = world.section(ChunkPos::new(key.cx, key.cz), key.si) else {
        return SnapshotOutcome::Empty;
    };
    if is_all_air(&centre) {
        return SnapshotOutcome::Empty;
    }

    // Pre-sized and index-assigned rather than push()ed in `(dx, dy, dz)`
    // order, so the loop below can be reordered to `(dx, dz, dy)` — grouping
    // the three `dy` neighbours that share one `pos` — without disturbing the
    // `[dx+1][dy+1][dz+1]` layout `SectionSnapshot::at`/`light_at` index into.
    // Every slot defaults to `Neighbour::Air` — empty, and empty is the truth
    // (see `air_section_static` for the section it resolves to); a present,
    // in-range section or light overwrites it below, and a slot belonging to a
    // column still in flight is downgraded to `Neighbour::Unloaded`.
    let mut sections: Vec<Neighbour> = vec![Neighbour::Air; 27];
    let mut lights: Vec<Option<SectionLightData>> = vec![None; 27];
    // Set by any column that has not arrived. The centre column is present by
    // construction (checked above), so this can only be raised by one of the
    // eight horizontal neighbours — the same eight vanilla's own
    // section-update tracker's has-all-neighbors check checks.
    let mut awaiting_columns = false;
    for dx in -1..=1 {
        for dz in -1..=1 {
            let pos = ChunkPos::new(key.cx + dx, key.cz + dz);
            // `dy` never changes `pos`, so one `world.get` here serves all
            // three `dy` neighbours below — both the block and the light
            // lookup used to probe `self.chunks` (a `HashMap<ChunkPos, _>`)
            // independently, once each per `dy`, for up to 27 + 27 = 54
            // probes per `snapshot_section_in` call. This is 9.
            let chunk = world.get(pos);
            // Air across this seam is a *guess* only when the column itself is
            // missing **and** columns are still arriving. An elided all-air
            // section inside a column that has arrived is a reading, not a
            // guess: the chunk decoder elides exactly the sections that are
            // genuinely empty.
            let awaiting = chunk.is_none() && columns == ColumnSource::Streaming;
            awaiting_columns |= awaiting;
            for dy in -1..=1 {
                let i = ((dx + 1) * 9 + (dy + 1) * 3 + (dz + 1)) as usize;
                let si = key.si as i32 + dy;

                // `ChunkColumn::section_arc` hands back a clone of the
                // section's `Arc` — a refcount bump, not a copy of its
                // palette data (see `Neighbour::Present`'s docs). An absent or
                // elided neighbour keeps this slot's default: lit air rather
                // than an unlit void, tagged with why it is empty.
                if in_range(si)
                    && let Some(section) = chunk.and_then(|c| c.column.section_arc(si as usize))
                {
                    sections[i] = Neighbour::Present(section);
                } else if awaiting {
                    sections[i] = Neighbour::Unloaded;
                }

                // Light is LIGHT-section indexed: block section `si` reads
                // light section `si + 1` (light section 0 is the boundary
                // *below* the world). This is an off-by-one *by design*, not
                // a bug — do not "correct" it. Deliberately not gated on
                // `in_range`: the two boundary light sections exist precisely
                // so the top and bottom block sections can sample into them.
                // `None` here (absent column, or a genuinely out-of-range
                // light section) keeps the bridge in `mesh_snapshot`.
                lights[i] = if si + 1 < 0 {
                    None
                } else {
                    let li = (si + 1) as usize;
                    chunk.and_then(|c| {
                        (li < c.light.light_section_count()).then(|| c.light.section_light(li))
                    })
                };
            }
        }
    }

    let snapshot = SectionSnapshot {
        key,
        light_revision: None,
        sections,
        lights,
        sky_default,
        biome_names,
    };
    if awaiting_columns {
        SnapshotOutcome::Deferred(snapshot)
    } else {
        SnapshotOutcome::Ready(snapshot)
    }
}

/// Build a [`SectionSnapshot`] for `key` from the live client world: a thin
/// adapter over [`snapshot_section_in`] that takes the read lock once and
/// releases it before meshing. Light stays server-authoritative; nothing here
/// recomputes it.
///
/// `section_count` is the column's block-section count and `key.min_y` must be
/// the dimension's `min_y`.
///
/// Returns [`SnapshotOutcome::Empty`] before login and when the centre holds no
/// geometry. A live world is [`ColumnSource::Streaming`], so this can return
/// [`SnapshotOutcome::Deferred`]. The sky default follows the connected
/// dimension; see [`sky_default_for_dimension`].
#[must_use]
pub fn snapshot_section_live(
    net: &NetClient,
    key: SectionKey,
    section_count: usize,
    biome_names: BiomeNames,
) -> SnapshotOutcome {
    let handle = net.shared_handle();
    let Some(handle) = handle.get() else {
        return SnapshotOutcome::Empty;
    };
    // `WorldDimensions` has no dimension identity, so the sky policy reads it
    // off the player snapshot.
    let player = handle.player();
    let sky_default =
        sky_default_for_dimension(player.dimension.as_ref(), player.dimension_type.as_ref());
    let store = handle.chunk_world();
    snapshot_section_in(
        &store.read(),
        key,
        Some(section_count),
        sky_default,
        ColumnSource::Streaming,
        biome_names,
    )
}

/// The [`SkyDefault`] for a missing neighbour sky sample in the connected
/// dimension (`None` before login).
///
/// The registry's `has_skylight` decides when the dimension type is known: the
/// Nether has none, so a missing sample there is `0`, while the End has it like
/// the overworld, so defaulting it to `0` would render lit terrain dark. With no
/// registry data (a server or family that sends none) it falls back to matching
/// the level name, and never to "assume the overworld".
#[must_use]
pub fn sky_default_for_dimension(
    dimension: Option<&lodestone_client::DimensionId>,
    dimension_type: Option<&lodestone_client::DimensionTypeInfo>,
) -> SkyDefault {
    if let Some(info) = dimension_type {
        return if info.has_skylight {
            SkyDefault::Full
        } else {
            SkyDefault::None
        };
    }
    match dimension {
        // Dimension not yet known (pre-login): keep the previous default.
        None => SkyDefault::Full,
        Some(dim) if dim.namespace() == "minecraft" && dim.path() == "overworld" => {
            SkyDefault::Full
        }
        Some(dim) if dim.namespace() == "minecraft" && dim.path() == "the_end" => {
            SkyDefault::Full
        }
        Some(_) => SkyDefault::None,
    }
}

/// Whether `section` holds nothing but air; an `O(1)` read of the section's
/// maintained non-air count.
fn is_all_air(section: &ChunkSection) -> bool {
    section.is_air_only()
}

/// Cheap, column-wide content facts used to interpret a mesh result.
///
/// `ChunkColumn` elides an all-air section, but it can still retain an allocated
/// section whose biome differs from the column default. That section is valid
/// storage and still has no block geometry. Counting the section's maintained
/// non-air total rather than using `allocated_sections()` keeps those cases
/// distinct from a loaded column whose block data disappeared before meshing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ColumnBlockSummary {
    pub(crate) allocated_sections: usize,
    pub(crate) non_air_sections: usize,
    pub(crate) non_air_blocks: usize,
}

impl ColumnBlockSummary {
    /// Summarise block content without walking every cell in every section.
    #[must_use]
    pub(crate) fn from_column(column: &ChunkColumn) -> Self {
        let mut summary = Self::default();
        for section_index in 0..column.section_count() {
            let Some(section) = column.section(section_index) else {
                continue;
            };
            summary.allocated_sections += 1;
            let non_air_blocks = usize::from(section.non_air_count());
            summary.non_air_blocks += non_air_blocks;
            if non_air_blocks > 0 {
                summary.non_air_sections += 1;
            }
        }
        summary
    }

    /// Whether every retained section is air-only (or the column is fully
    /// elided). Biome-only sections intentionally classify as empty geometry.
    #[must_use]
    pub(crate) const fn is_all_air(self) -> bool {
        self.non_air_blocks == 0
    }
}

/// A no-snapshot result is actionable only when the loaded block storage says
/// that some non-air data should have reached the mesher. Streaming deferrals
/// are handled separately and intentional all-air columns are routine.
#[must_use]
pub(crate) fn should_report_empty_column(
    summary: ColumnBlockSummary,
    meshed_any: bool,
    deferred_any: bool,
) -> bool {
    !meshed_any && !deferred_any && !summary.is_all_air()
}

/// A section's light source for the mesh pass: either the world's real light
/// (via [`WorldSectionLight`]) or, for a genuinely-absent neighbour (edge of
/// world / below world), the full-bright bridge.
///
/// The bridge lives on **only** in the absent branch: a present section always
/// carries real light. Keeping the two in one enum lets every view share a
/// single concrete `SectionLight` type so the neighbourhood stays monomorphic
/// (no boxing) while still mixing real and fallback light per slot.
pub(crate) enum SnapLight<'a> {
    World(WorldSectionLight<'a>),
    /// Full-bright fallback for an absent neighbour section only.
    Bridge(UniformLight),
}

impl SectionLight for SnapLight<'_> {
    fn block_light(&self, x: usize, y: usize, z: usize) -> u8 {
        match self {
            SnapLight::World(w) => w.block_light(x, y, z),
            SnapLight::Bridge(b) => b.block_light(x, y, z),
        }
    }

    fn sky_light(&self, x: usize, y: usize, z: usize) -> u8 {
        match self {
            SnapLight::World(w) => w.sky_light(x, y, z),
            SnapLight::Bridge(b) => b.sky_light(x, y, z),
        }
    }
}

/// Borrowed light neighbourhood shared by a snapshot's model and fluid views.
/// Faces sample the cell they open into, not the opaque cell that owns them.
pub(crate) struct SnapshotLight<'a> {
    /// One light source per snapshot slot, indexed `[dx+1][dy+1][dz+1]`.
    pub(crate) slots: [SnapLight<'a>; 27],
    read_probe: Option<&'a super::light_reads::LightReadProbe>,
}

impl<'a> SnapshotLight<'a> {
    /// Wrap every slot of `snapshot`'s light in a [`SnapLight`].
    ///
    /// Each present neighbour forwards the world's resolved sky/block levels
    /// verbatim (with the dimension's [`SkyDefault`] applied only to
    /// genuinely-absent sky); an absent neighbour — and *only* an absent
    /// neighbour — keeps the full-bright bridge, so air at the edge of the
    /// loaded world stays lit rather than rendering black.
    pub(crate) fn new(snapshot: &'a SectionSnapshot) -> Self {
        let slots = std::array::from_fn(|i| match snapshot.lights[i].as_ref() {
            Some(light) => SnapLight::World(WorldSectionLight::new(light, snapshot.sky_default)),
            None => SnapLight::Bridge(UniformLight::pre_light_bridge()),
        });
        Self { slots, read_probe: None }
    }

    pub(super) fn with_read_probe(mut self, probe: Option<&'a super::light_reads::LightReadProbe>) -> Self {
        self.read_probe = probe;
        self
    }

    /// Resolves signed centre-relative samples; outside the snapshot is unlit.
    pub(crate) fn levels_at(&self, x: i32, y: i32, z: i32) -> (u8, u8) {
        let (dx, lx) = split16(x);
        let (dy, ly) = split16(y);
        let (dz, lz) = split16(z);
        if !(-1..=1).contains(&dx) || !(-1..=1).contains(&dy) || !(-1..=1).contains(&dz) {
            if let Some(probe) = self.read_probe { probe.observe(x, y, z, 0, 0); }
            return (0, 0);
        }
        let src = &self.slots[((dx + 1) * 9 + (dy + 1) * 3 + (dz + 1)) as usize];
        let levels = (
            SectionLight::sky_light(src, lx, ly, lz),
            SectionLight::block_light(src, lx, ly, lz),
        );
        if let Some(probe) = self.read_probe { probe.observe(x, y, z, levels.0, levels.1); }
        levels
    }

    /// Packed `sky << 4 | block` for a face of the centre-section cell
    /// `(x, y, z)` pointing along `normal` — the light of the neighbouring cell,
    /// read across the section boundary when the face sits on one.
    pub(crate) fn face_light(&self, x: usize, y: usize, z: usize, normal: [i32; 3]) -> u8 {
        let (sky, block) = self.levels_at(
            x as i32 + normal[0],
            y as i32 + normal[1],
            z as i32 + normal[2],
        );
        (sky << 4) | block
    }

    /// Packed `sky << 4 | block` for geometry with no single facing (fluid
    /// surfaces, cross-shaped models): the brightest of the cell itself and its
    /// six orthogonal neighbours.
    ///
    /// Self is included deliberately — a non-opaque cell (water, glass, an
    /// emitter) carries real light of its own, and including it cannot
    /// manufacture a bright outlier: in a diffusive light field a cell's level
    /// exceeds its brightest neighbour's by at most one, so a stale own-cell
    /// value is bounded to ±1 rather than the 15-vs-0 contrast own-cell-only
    /// sampling produces.
    pub(crate) fn max_light(&self, x: usize, y: usize, z: usize) -> u8 {
        const NEIGHBOURS: [[i32; 3]; 7] = [
            [0, 0, 0],
            [-1, 0, 0],
            [1, 0, 0],
            [0, -1, 0],
            [0, 1, 0],
            [0, 0, -1],
            [0, 0, 1],
        ];
        let (mut sky, mut block) = (0u8, 0u8);
        for n in NEIGHBOURS {
            let (s, b) = self.levels_at(x as i32 + n[0], y as i32 + n[1], z as i32 + n[2]);
            sky = sky.max(s);
            block = block.max(b);
        }
        (sky << 4) | block
    }
}
