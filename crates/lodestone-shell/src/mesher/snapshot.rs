//! Copy-on-write section snapshots and their light neighbourhood.
use super::*;

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
    /// A snapshot of the live biome registry's ordered entry names
    /// (`net::BiomeNameCell::snapshot`), or empty when none is known (no
    /// connection, no `registry_data` yet, or a version/server that sends
    /// none). Empty is a real, cheap `Arc<[]>` — see [`Self::with_biome_names`].
    ///
    /// Baked into the snapshot itself, rather than threaded into
    /// [`MeshScheduler`]'s workers separately, because that is what already
    /// happens to [`Self::sky_default`]: both are per-connection facts a
    /// worker thread cannot ask a live `Sim`/`NetClient` for (it only ever
    /// sees the jobs on its channel), and both are cheap to carry along —
    /// `Arc::clone`, not a copy of the strings.
    pub(crate) biome_names: Arc<[&'static str]>,
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

    pub(crate) fn light_at(&self, dx: i32, dy: i32, dz: i32) -> Option<&SectionLightData> {
        let i = ((dx + 1) * 9 + (dy + 1) * 3 + (dz + 1)) as usize;
        self.lights[i].as_ref()
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
            sections: self.sections.clone(),
            lights: (0..self.sections.len()).map(|_| None).collect(),
            sky_default: self.sky_default,
            biome_names: Arc::clone(&self.biome_names),
        }
    }

    /// Attach a live biome-registry-names snapshot (follow-up),
    /// overriding the empty default every constructor otherwise leaves in
    /// place. In production the sole caller is [`TerrainMesh::mesh_column`]/
    /// [`TerrainMesh::mesh_section`], which have a `Sim`-derived
    /// `net::SharedBiomeNames` to read; every other caller (every hermetic
    /// test, `crate::gpu`'s gates, the offline demo world) has none and an
    /// empty table correctly falls back to `FALLBACK_BIOME_NAMES` in
    /// [`biome_name_at`] — those callers' existing, unmodified behaviour
    /// depends on that default. `pub`, not `pub(crate)`, so a live gate in
    /// `tests/` (a separate crate) can build a fixture registry order and
    /// prove the live table is genuinely consulted rather than merely
    /// plumbed — see `tests/biome_tint_live_mesh.rs`'s
    /// `live_mesh_snapshot_models_resolves_biome_names_from_the_live_registry_not_the_fallback_table`.
    #[must_use]
    pub fn with_biome_names(mut self, names: Arc<[&'static str]>) -> Self {
        self.biome_names = names;
        self
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

/// A process-wide shared all-air section, for the absent slots of a
/// 27-neighbourhood — what [`Neighbour::section`] hands the mesher for
/// [`Neighbour::Air`] and [`Neighbour::Unloaded`].
///
/// `air_section()` is already cheap to construct (its `PalettedContainer`s are
/// `Storage::Single`, so building one allocates nothing), but a missing neighbour
/// is common — every section at the edge of a loaded 3×3 column footprint has one
/// — and there is no reason for even the small `Arc` box allocation to happen per
/// slot when every slot's content is identical. Borrowing costs nothing at all:
/// since the absent cases became variants rather than a stand-in `Arc`, no
/// refcount is touched either.
fn air_section_static() -> &'static ChunkSection {
    static AIR: OnceLock<Arc<ChunkSection>> = OnceLock::new();
    AIR.get_or_init(|| Arc::new(air_section()))
}

/// Clone the 27-section neighbourhood around `key` out of the world, if the
/// centre section actually holds geometry. Returns `None` when the centre is
/// absent or entirely air (nothing to mesh).
///
/// The unbounded-height, overworld-sky, [`ColumnSource::Complete`] form of
/// [`snapshot_section_in`]. Kept as its own entry point because `crate::gpu`'s
/// hermetic mesh gates and the offline demo world call it with nothing but a
/// world and a key — and for both of those the world really is complete, so the
/// outcome is never [`SnapshotOutcome::Deferred`] and an `Option` says
/// everything there is to say.
#[must_use]
pub fn snapshot_section(world: &World, key: SectionKey) -> Option<SectionSnapshot> {
    snapshot_section_in(world, key, None, SkyDefault::Full, ColumnSource::Complete).ready()
}

/// What [`snapshot_section_in`] found: geometry to mesh now, nothing to mesh, or
/// geometry that must **not** be meshed yet.
///
/// The third arm is the seam-baked-against-air defect. A section whose horizontal neighbourhood is
/// incomplete can be meshed — the code will happily do it — but every face on
/// the incomplete seam is decided against air the neighbour has not had a chance
/// to contradict. For water that is a full-height translucent side quad on each
/// side of the seam, drawn twice with no depth conflict to give it away; for
/// everything else it is wrong ambient occlusion, wrong smooth-light corners and
/// stray uncalled faces. Vanilla refuses the same build for the same reason —
/// vanilla's own level-extractor only compiles a never-compiled section when
/// its own section-update tracker's has-all-neighbors check reports all eight horizontal
/// neighbour columns loaded.
#[derive(Debug)]
pub enum SnapshotOutcome {
    /// The centre holds geometry and the whole neighbourhood is known. Mesh it.
    Ready(SectionSnapshot),
    /// Nothing to draw: the centre section is absent, out of the column's
    /// vertical range, or entirely air. Any geometry already on the GPU for this
    /// key is stale and should be removed.
    Empty,
    /// The centre holds geometry, but at least one of the eight horizontal
    /// neighbour columns has not arrived. The snapshot is carried anyway so a
    /// caller that has *already* put this section on screen can rebuild it
    /// rather than blink it out — vanilla's `sectionMesh != UNCOMPILED` escape
    /// hatch, and the reason a chunk unloading at the far edge of the view does
    /// not punch a hole in the ring beside it.
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

    /// Thread a live biome-registry-names snapshot into whichever
    /// [`SectionSnapshot`] this outcome carries, leaving [`Self::Empty`]
    /// untouched (there is nothing to mesh, so nothing to attach it to). See
    /// [`SectionSnapshot::with_biome_names`].
    #[must_use]
    pub fn with_biome_names(self, names: Arc<[&'static str]>) -> Self {
        match self {
            SnapshotOutcome::Ready(snap) => {
                SnapshotOutcome::Ready(snap.with_biome_names(names))
            }
            SnapshotOutcome::Deferred(snap) => {
                SnapshotOutcome::Deferred(snap.with_biome_names(names))
            }
            SnapshotOutcome::Empty => SnapshotOutcome::Empty,
        }
    }
}

/// Clone the 27-section neighbourhood around `key` out of `world`.
///
/// **The one snapshot implementation**, and that is the point of it: before
/// Stage 4 (`docs/bevy-migration.md` §4.1(d)) there were two — one reading the
/// shell's offline world directly, one reading the live client-owned world
/// through `NetClient::sections_and_light_at` — and they had drifted apart in
/// three ways, only one of which was deliberate. With one
/// [`lodestone_ecs::ChunkWorld`] store there is one world to read, so the two
/// collapse and the remaining parameters are the two things that genuinely are
/// per-session facts rather than per-store ones:
///
/// * `section_count` — the dimension's column height, from
///   [`lodestone_ecs::ChunkWorld::extent`]. `None` means "unbounded": an
///   out-of-range section simply snapshots to nothing. **Blocks** are gated on
///   it; **light** deliberately is not, because vanilla lights one section below
///   and one above the build range and a column's topmost/bottom-most section
///   samples into exactly those (see below).
/// * `sky_default` — how an *absent* sky sample resolves, which depends on the
///   connected dimension's `has_skylight` and cannot be read off the store. See
///   [`sky_default_for_dimension`].
/// * `columns` — whether an absent *horizontal neighbour column* is the edge of
///   the world or a chunk still in flight. See [`ColumnSource`]; this is the
///   third session fact, added for the seam-baked-against-air defect, and it is the one the store
///   provably cannot answer (an absent column looks the same either way).
///
/// # One behaviour change, stated because it is not a refactor
///
/// The live path used to gate light on the same in-range test as blocks, so the
/// two vertical boundary slots (`si == -1` and `si == section_count`) kept the
/// full-bright bridge instead of reading the real boundary light section that
/// [`World::section_light`] serves for exactly this purpose. The offline path
/// never did that. This function follows the offline path — the correct one, per
/// `section_light`'s own docs — which means the *only* observable difference is
/// in a dimension whose absent sky is `0`: the Nether's build ceiling now reads
/// its real sky `0` rather than the bridge's `15`. That direction is a fix, and
/// it is **unverified against a live Nether** (the overworld measures 0 of 192
/// sky sections `Missing`, so no overworld gate can see it either way).
#[must_use]
pub fn snapshot_section_in(
    world: &World,
    key: SectionKey,
    section_count: Option<usize>,
    sky_default: SkyDefault,
    columns: ColumnSource,
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
        sections,
        lights,
        sky_default,
        // Every caller of this function gets the fallback table in
        // `biome_name_at` unless it opts in with `with_biome_names` — see
        // that method's doc for exactly who does.
        biome_names: Arc::from([]),
    };
    if awaiting_columns {
        SnapshotOutcome::Deferred(snapshot)
    } else {
        SnapshotOutcome::Ready(snapshot)
    }
}

/// Build a [`SectionSnapshot`] for `key` from the **live client world**.
///
/// Since Stage 4 this is a thin adapter over [`snapshot_section_in`]: the live
/// world and the shell's world are one [`lodestone_ecs::ChunkWorld`] store, so
/// there is no second gathering loop and no `(pos, block_index, light_index)`
/// request batch — the read lock is taken once, here, by `ChunkWorld::read`, and
/// released before any meshing. Light stays **server-authoritative**: nothing on
/// this path ever recomputes it (recomputing on multiplayer would overwrite the
/// server's seam-complete cross-chunk light with a partial result — a divergence
/// bug).
///
/// `section_count` is the column's block-section count; `key.min_y` must be the
/// dimension's `min_y`. Both come from [`lodestone_ecs::ChunkWorld::extent`] on
/// the shell's own path — this signature survives only because
/// `tests/live_world_mesh.rs` drives the live mesh straight off a `NetClient`,
/// and that file is not this stage's to change.
///
/// Returns [`SnapshotOutcome::Empty`] before login (no client handle published
/// yet) and when the centre section holds no geometry. A live world is
/// [`ColumnSource::Streaming`] by definition, so this *can* return
/// [`SnapshotOutcome::Deferred`] — the caller decides whether an incomplete
/// neighbourhood is good enough for what it is doing.
///
/// The returned snapshot's [`SkyDefault`] follows the **connected dimension** —
/// see [`sky_default_for_dimension`], which carries the End-vs-Nether
/// measurement.
#[must_use]
pub fn snapshot_section_live(
    net: &NetClient,
    key: SectionKey,
    section_count: usize,
) -> SnapshotOutcome {
    let handle = net.shared_handle();
    let Some(handle) = handle.get() else {
        return SnapshotOutcome::Empty;
    };
    // `WorldDimensions` carries only `min_y`/`height`, not dimension identity, so
    // the sky policy reads the connected dimension off the player snapshot — the
    // cheapest place this crate can reach it without growing that struct. The
    // snapshot carries the server's dimension **type**, which is what the
    // policy actually wants; the level id stays as the fallback.
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
    )
}

/// Resolves the [`SkyDefault`] a *missing* neighbour sky sample should use for
/// the given connected dimension (`None` when the dimension is not yet known,
/// i.e. pre-login).
///
/// This follows the dimension's `has_skylight`, not a hardcoded
/// "overworld only" assumption: the Nether's dimension type sets
/// `has_skylight: false`, so a `Missing` sky sample there must resolve to `0`,
/// not daylight. Overworld measured 0 of 192 sky sections `Missing`, which is
/// exactly why this was invisible until now — the wrong default never got
/// exercised.
///
/// The End is *not* lumped in with the Nether here, even though both are "not
/// the overworld": the End's own dimension type
/// (`.cache/mc/26.2/client-src/data/minecraft/dimension_type/the_end.json`)
/// carries `"has_skylight": true`, identical to the overworld — its islands
/// really are lit by real per-block sky exposure the server computes and
/// sends the same way. Defaulting a `Missing` End neighbour to `0` would
/// (rarely, at an unresolved chunk edge) render genuinely sky-lit End terrain
/// artificially dark, the same class of bug this function exists to prevent —
/// just aimed the other direction.
///
/// # The registry answers this now, and the name match is the fallback
///
/// `dimension_type` is the server's own `minecraft:dimension_type` entry, decoded
/// off the Configuration `registry_data` packet and carried on
/// `PlayerSnapshot::dimension_type`. When it is present its `has_skylight` **is**
/// the answer, and the level name is not consulted at all — which is what closes
/// the gap where a data pack pointing a level called `mypack:mine` at the vanilla
/// overworld type used to fall through to `SkyDefault::None` and render its
/// terrain dark, and the reverse (a custom 1024-tall type on
/// `minecraft:overworld`) used to be assumed lit.
///
/// The name match survives only for `dimension_type == None`: a server or
/// protocol family that sends no `registry_data`. It is the name-match fallback
/// verbatim, so that path cannot have regressed, and it is deliberately **not**
/// "assume the overworld".
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

/// Whether `section` holds nothing but air, i.e. nothing for the mesher to
/// draw.
///
/// This used to be a 4096-cell scan calling `get_block` for every `(x, y, z)`
/// — once per section, i.e. once per `snapshot_section_in` call, i.e.
/// `section_count` times (≈24) per column remesh. `ChunkSection` already
/// maintains `non_air_count` incrementally on every write (see
/// `lodestone-world/src/section.rs`), and every `ChunkSection` in this crate
/// is constructed with `air_id == id::AIR` (`air_section` here,
/// `worldgen::generate_column`'s demo columns, and every version crate's
/// chunk-packet decoder all pass `0`), so `is_air_only` — an `O(1)` field read
/// — answers exactly the same question this scan did.
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
    allocated_sections: usize,
    non_air_sections: usize,
    non_air_blocks: usize,
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

/// The whole 27-section light neighbourhood of a snapshot, plus the rule for
/// resolving the light a *visible face* should carry.
///
/// The rule matters more than it looks. `lodestone-world`'s light engine (and
/// vanilla's, which it matches cell-for-cell) stores `0` inside an opaque block:
/// light propagates *to* a solid cell's neighbours, never into the solid itself.
/// Measured against the live 26.2 oracle, **99.5 % of solid cells store sky
/// light `0`**. So a mesher that lights a block from its own cell renders the
/// entire opaque world at the shader's dark floor — and renders a *just-placed*
/// block full-bright, because its cell still holds the sky light of the air it
/// replaced until the server's relight arrives ~1 tick later. That contrast is
/// the player-visible "blocks I place are super bright".
///
/// [`Self::face_light`] therefore samples the cell the face **opens into**,
/// exactly as vanilla's `ModelBlockRenderer` does. The stale own-cell value is
/// then never read at all, which is also what closes the optimistic-placement
/// window: there is no interval in which a locally-known block is lit by data
/// the server has not yet corrected.
pub(crate) struct SnapshotLight<'a> {
    /// One light source per snapshot slot, indexed `[dx+1][dy+1][dz+1]`.
    pub(crate) slots: Vec<SnapLight<'a>>,
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
        let mut slots: Vec<SnapLight<'a>> = Vec::with_capacity(27);
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    let src = match snapshot.light_at(dx, dy, dz) {
                        Some(world_light) => SnapLight::World(WorldSectionLight::new(
                            world_light,
                            snapshot.sky_default,
                        )),
                        None => SnapLight::Bridge(UniformLight::pre_light_bridge()),
                    };
                    slots.push(src);
                }
            }
        }
        Self { slots }
    }

    /// Resolved `(sky, block)` at a **centre-relative signed** coordinate, which
    /// may step one cell past the centre section into a neighbour. Out of the
    /// 3×3×3 snapshot resolves to unlit `(0, 0)`; a one-step face probe from a
    /// cell inside the centre section can never reach there.
    pub(crate) fn levels_at(&self, x: i32, y: i32, z: i32) -> (u8, u8) {
        let (dx, lx) = split16(x);
        let (dy, ly) = split16(y);
        let (dz, lz) = split16(z);
        if !(-1..=1).contains(&dx) || !(-1..=1).contains(&dy) || !(-1..=1).contains(&dz) {
            return (0, 0);
        }
        let src = &self.slots[((dx + 1) * 9 + (dy + 1) * 3 + (dz + 1)) as usize];
        (
            SectionLight::sky_light(src, lx, ly, lz),
            SectionLight::block_light(src, lx, ly, lz),
        )
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
