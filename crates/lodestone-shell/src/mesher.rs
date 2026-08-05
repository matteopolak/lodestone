//! Off-main-thread section meshing over **copy-on-write snapshots**.
//!
//! The rule from the design plan is absolute: *the world is never locked while
//! meshing*. So the pipeline is split in two:
//!
//! 1. On the owning thread, [`snapshot_section`] clones the 3×3×3 = 27 sections
//!    around a target section into an owned, `Send` [`SectionSnapshot`]. The
//!    neighbourhood is 27, not 6, because ambient occlusion and smooth light
//!    read diagonal neighbours across section edges *and* corners.
//!
//!    Because it is 27 and not 6, **a snapshot taken before its neighbours
//!    arrived is wrong in more ways than a missing face** — see [`Neighbour`] and
//!    [`SnapshotOutcome`] for the typed distinction between "air, and air is the
//!    truth" and "air, and air is a guess", and why the second one defers the
//!    build instead of baking it (issue #389).
//! 2. On worker threads, [`mesh_snapshot`] turns a snapshot into a
//!    [`lodestone_render::Mesh`] with no access to the live world at all.
//!
//! [`MeshScheduler`] is a tiny fixed worker pool wrapping that split.
//!
//! Meshing uses [`lodestone_render::mesh_simple`] (one quad per visible face)
//! rather than the greedy mesher: the shell's atlas packs many sprites into one
//! 2-D texture, and greedy-merged quads tile UVs past a single sprite's cell,
//! which would bleed neighbouring sprites. Per-face quads keep every tile
//! coordinate in `{0,1}`, mapping exactly onto each sprite rect. (A texture-array
//! atlas would let greedy back in — noted in the report.)

use std::collections::{BTreeSet, HashSet};
use std::sync::{
    Arc, Mutex, OnceLock,
    mpsc::{self, Receiver, Sender},
};
use std::thread::{self, JoinHandle};

use bevy_ecs::resource::Resource;
use bevy_ecs::schedule::IntoScheduleConfigs;
use bevy_ecs::system::{Res, ResMut};
use lodestone_ecs::app::{App, Plugin};
use lodestone_ecs::{ChunkWorld, FrameSet, Update};
use lodestone_render::{
    BlockClassifier, BlockModels, ChunkSectionView, FluidCell, FluidKind, FluidMeshes,
    FluidSectionView, FluidSprites, Mesh, ModelMesh, ModelSectionView, SectionLight,
    SectionNeighborhood, SkyDefault, UniformLight, WorldSectionLight, biome_tint_kind_for_slot,
    face_of_direction, mesh_fluids, mesh_models, mesh_simple,
};
use lodestone_render::biome_tint::{BLEND_RADIUS, NamedBiomeTint, resolve_blended_tint, rgb_to_bytes};
use lodestone_assets::{BakedQuad, Direction};
use lodestone_model::BlockPos;
use lodestone_world::{
    ChunkPos, ChunkSection, PaletteKind, SectionLight as SectionLightData, World,
};

use crate::blocks::{ShellClassifier, id};
use crate::net::NetClient;

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
}

/// Whether the columns a world is meshed from are **all there already** or are
/// still arriving.
///
/// This is the fact that decides what an *absent* horizontal neighbour column
/// means, and nothing downstream of [`snapshot_section_in`] can derive it: an
/// empty slot looks identical either way. Getting it wrong in the `Streaming`
/// direction is issue #389 (a seam baked against air that never heals); getting
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
    /// **guess**, and the wrong one often enough to be the whole of issue #389.
    Streaming,
}

/// Why one slot of a 27-section neighbourhood holds no section.
///
/// The two cases are the point of this type. Before it they were one `Option`
/// resolving to the same all-air stand-in, and that conflation is what made
/// #389 invisible: a chunk seam meshed against a not-yet-loaded neighbour is
/// indistinguishable, at the call site, from one meshed against the edge of the
/// world — so the wrong one was silently treated as final.
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
    sections: Vec<Neighbour>,
    /// Per-neighbour light, indexed identically to `sections`
    /// (`[dx+1][dy+1][dz+1]`). `None` where the neighbour column or light
    /// section is absent (edge of world / below the world). Those slots fall
    /// back to the full-bright bridge in [`mesh_snapshot`]; every present slot
    /// carries the world's real sky/block light.
    lights: Vec<Option<SectionLightData>>,
    /// How to resolve *absent* (`Missing`) sky light, chosen per dimension by
    /// the producer: [`snapshot_section`]'s demo world is always the
    /// overworld ([`SkyDefault::Full`]); [`snapshot_section_live`] resolves
    /// this per the *connected* dimension, defaulting to
    /// [`SkyDefault::None`] outside the overworld so absent sky stays `0`
    /// rather than defaulting up to daylight in the Nether/End.
    sky_default: SkyDefault,
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
    biome_names: Arc<[&'static str]>,
}

impl SectionSnapshot {
    fn at(&self, dx: i32, dy: i32, dz: i32) -> &ChunkSection {
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

    fn light_at(&self, dx: i32, dy: i32, dz: i32) -> Option<&SectionLightData> {
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

    /// Attach a live biome-registry-names snapshot (issue #96's follow-up),
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

fn air_section() -> ChunkSection {
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
/// The third arm is issue #389. A section whose horizontal neighbourhood is
/// incomplete can be meshed — the code will happily do it — but every face on
/// the incomplete seam is decided against air the neighbour has not had a chance
/// to contradict. For water that is a full-height translucent side quad on each
/// side of the seam, drawn twice with no depth conflict to give it away; for
/// everything else it is wrong ambient occlusion, wrong smooth-light corners and
/// stray uncalled faces. Vanilla refuses the same build for the same reason —
/// `LevelExtractor` only compiles a never-compiled section when
/// `SectionUpdateTracker.hasAllNeighbors` reports all eight horizontal
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
///   third session fact, added for issue #389, and it is the one the store
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
    // eight horizontal neighbours — the same eight vanilla's
    // `SectionUpdateTracker.hasAllNeighbors` checks.
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
    // cheapest place this crate can reach it without growing that struct. Since
    // #288 the snapshot also carries the server's own dimension **type**, which
    // is what the policy actually wants; the level id stays as the fallback.
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
/// # The registry answers this now (issue #288), and the name match is the fallback
///
/// `dimension_type` is the server's own `minecraft:dimension_type` entry, decoded
/// off the Configuration `registry_data` packet and carried on
/// `PlayerSnapshot::dimension_type`. When it is present its `has_skylight` **is**
/// the answer, and the level name is not consulted at all — which is what closes
/// issue #34: a data pack pointing a level called `mypack:mine` at the vanilla
/// overworld type used to fall through to `SkyDefault::None` and render its
/// terrain dark, and the reverse (a custom 1024-tall type on
/// `minecraft:overworld`) used to be assumed lit.
///
/// The name match survives only for `dimension_type == None`: a server or
/// protocol family that sends no `registry_data`. It is the pre-#288 behaviour
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

/// A section's light source for the mesh pass: either the world's real light
/// (via [`WorldSectionLight`]) or, for a genuinely-absent neighbour (edge of
/// world / below world), the full-bright bridge.
///
/// The bridge lives on **only** in the absent branch: a present section always
/// carries real light. Keeping the two in one enum lets every view share a
/// single concrete `SectionLight` type so the neighbourhood stays monomorphic
/// (no boxing) while still mixing real and fallback light per slot.
enum SnapLight<'a> {
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
struct SnapshotLight<'a> {
    /// One light source per snapshot slot, indexed `[dx+1][dy+1][dz+1]`.
    slots: Vec<SnapLight<'a>>,
}

impl<'a> SnapshotLight<'a> {
    /// Wrap every slot of `snapshot`'s light in a [`SnapLight`].
    ///
    /// Each present neighbour forwards the world's resolved sky/block levels
    /// verbatim (with the dimension's [`SkyDefault`] applied only to
    /// genuinely-absent sky); an absent neighbour — and *only* an absent
    /// neighbour — keeps the full-bright bridge, so air at the edge of the
    /// loaded world stays lit rather than rendering black.
    fn new(snapshot: &'a SectionSnapshot) -> Self {
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
    fn levels_at(&self, x: i32, y: i32, z: i32) -> (u8, u8) {
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
    fn face_light(&self, x: usize, y: usize, z: usize, normal: [i32; 3]) -> u8 {
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
    fn max_light(&self, x: usize, y: usize, z: usize) -> u8 {
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

/// Mesh a snapshot into geometry. Pure and thread-safe: touches only the owned
/// snapshot and a stateless classifier. Generic over the [`BlockClassifier`] so
/// the same code meshes the demo world (via [`crate::blocks::DemoClassifier`])
/// and the live vanilla world (via a [`crate::blocks::ShellClassifier::Vanilla`]
/// atlas) without duplication.
#[must_use]
pub fn mesh_snapshot<C: BlockClassifier>(snapshot: &SectionSnapshot, classifier: &C) -> Mesh {
    // Real per-section light, replacing the retired full-bright bridge. The
    // packed path lights each *cell* and lets `mesh_simple` sample the
    // neighbouring cell per face itself, so it needs the raw per-slot sources
    // rather than `SnapshotLight`'s face rule.
    let srcs = SnapshotLight::new(snapshot).slots;

    // Build a view per neighbour section, then assemble the neighbourhood.
    let mut views: Vec<ChunkSectionView<'_, C, SnapLight<'_>>> = Vec::with_capacity(27);
    let mut i = 0usize;
    for dx in -1..=1 {
        for dy in -1..=1 {
            for dz in -1..=1 {
                views.push(ChunkSectionView::new(
                    snapshot.at(dx, dy, dz),
                    classifier,
                    &srcs[i],
                ));
                i += 1;
            }
        }
    }
    let idx = |dx: i32, dy: i32, dz: i32| ((dx + 1) * 9 + (dy + 1) * 3 + (dz + 1)) as usize;

    let mut hood = SectionNeighborhood::centre_only(&views[idx(0, 0, 0)]);
    for dx in -1..=1 {
        for dy in -1..=1 {
            for dz in -1..=1 {
                if dx == 0 && dy == 0 && dz == 0 {
                    continue;
                }
                hood.set(dx, dy, dz, Some(&views[idx(dx, dy, dz)]));
            }
        }
    }

    mesh_simple(&hood)
}

/// A [`ModelSectionView`] over a [`SectionSnapshot`], driving the model mesh
/// path for the live vanilla world.
///
/// `quads_at`/`occludes_at` read vanilla block-state ids straight out of the
/// snapshot's paletted sections and look up baked geometry/occlusion in
/// [`BlockModels`]; `face_light_at` reads the real sky/block light of the cell
/// each face opens into, across section boundaries (see [`SnapshotLight`]).
/// This is the model-path counterpart to the packed [`ChunkSectionView`].
struct SnapshotModelView<'a> {
    snapshot: &'a SectionSnapshot,
    models: &'a BlockModels,
    light: SnapshotLight<'a>,
}

/// Split a signed section coordinate into a neighbour offset (`dx ∈ {-1,0,1}`)
/// and a section-local index (`0..16`). Used to resolve a `cullface` probe that
/// steps one block past a section edge into the adjacent snapshot section.
fn split16(v: i32) -> (i32, usize) {
    (v.div_euclid(16), v.rem_euclid(16) as usize)
}

/// Biome-id → name, for the [`ChunkSection::biome_at_block`] id space **this
/// client's own server assigns** — `crates/protocol/v770/src/
/// server_protocol.rs`'s `BIOME_NAMES` (alphabetical over the 55 biomes the
/// embedded overworld generator can select; nether/end biomes aren't in the
/// servable set yet, see `docs/worldgen-biomes.md`).
///
/// # This is a known, provisional gap, not an oversight
///
/// The *correct* source for this mapping is per-connection: a real server's
/// `registry_data` sync order, which `crates/protocol/v770/src/packets/
/// registry.rs`'s `ClientRegistries::entry_names(ClientRegistries::BIOME)`
/// already decodes correctly — but nothing between there and here carries it
/// yet (`crates/lodestone-shell/src/net.rs` does not store a
/// `ClientRegistries` on `NetClient` at all today, and threading one through
/// `MeshScheduler`'s worker-thread jobs is real, separately-scoped wiring).
/// This table is only correct **against this codebase's own server** — the
/// only server v770 can host (`CLAUDE.md`), and the default single-player-ish
/// path `cargo run --release` reaches — where it is exactly right by
/// construction, since both sides derive the same alphabetical order from the
/// same fixed biome set. Against a *third-party* vanilla server the mapping
/// would very likely be wrong (any registry reorder, or any biome the real
/// server's data pack adds/removes, shifts every later index), which is why
/// this is a local, `#[expect]`-free fallback rather than treated as the real
/// thing: replace it with a real `ClientRegistries`-backed lookup once that
/// wiring exists, and delete this table's provisional status note when it
/// does — do not treat this list as a substitute for that sync.
///
/// Keep in sync with `crates/protocol/v770/src/server_protocol.rs`'s
/// `BIOME_NAMES` if that table's biome set or order ever changes.
const FALLBACK_BIOME_NAMES: &[&str] = &[
    "minecraft:badlands",
    "minecraft:bamboo_jungle",
    "minecraft:beach",
    "minecraft:birch_forest",
    "minecraft:cherry_grove",
    "minecraft:cold_ocean",
    "minecraft:dark_forest",
    "minecraft:deep_cold_ocean",
    "minecraft:deep_dark",
    "minecraft:deep_frozen_ocean",
    "minecraft:deep_lukewarm_ocean",
    "minecraft:deep_ocean",
    "minecraft:desert",
    "minecraft:dripstone_caves",
    "minecraft:eroded_badlands",
    "minecraft:flower_forest",
    "minecraft:forest",
    "minecraft:frozen_ocean",
    "minecraft:frozen_peaks",
    "minecraft:frozen_river",
    "minecraft:grove",
    "minecraft:ice_spikes",
    "minecraft:jagged_peaks",
    "minecraft:jungle",
    "minecraft:lukewarm_ocean",
    "minecraft:lush_caves",
    "minecraft:mangrove_swamp",
    "minecraft:meadow",
    "minecraft:mushroom_fields",
    "minecraft:ocean",
    "minecraft:old_growth_birch_forest",
    "minecraft:old_growth_pine_taiga",
    "minecraft:old_growth_spruce_taiga",
    "minecraft:pale_garden",
    "minecraft:plains",
    "minecraft:river",
    "minecraft:savanna",
    "minecraft:savanna_plateau",
    "minecraft:snowy_beach",
    "minecraft:snowy_plains",
    "minecraft:snowy_slopes",
    "minecraft:snowy_taiga",
    "minecraft:sparse_jungle",
    "minecraft:stony_peaks",
    "minecraft:stony_shore",
    "minecraft:sulfur_caves",
    "minecraft:sunflower_plains",
    "minecraft:swamp",
    "minecraft:taiga",
    "minecraft:warm_ocean",
    "minecraft:windswept_forest",
    "minecraft:windswept_gravelly_hills",
    "minecraft:windswept_hills",
    "minecraft:windswept_savanna",
    "minecraft:wooded_badlands",
];

/// The biome name at a **signed**, snapshot-relative position (the same
/// coordinate space [`SnapshotModelView::occludes_at`]/[`SnapshotFluidView::
/// occludes_at`] already use), or `None` past the snapshotted 3×3×3
/// neighbourhood. `resolve_blended_tint`'s box blend only ever steps a couple
/// of blocks past the centre section, which is always within that
/// neighbourhood — see `split16`.
fn biome_name_at(snapshot: &SectionSnapshot, pos: BlockPos) -> Option<&'static str> {
    let (dx, lx) = split16(pos.x);
    let (dy, ly) = split16(pos.y);
    let (dz, lz) = split16(pos.z);
    if !(-1..=1).contains(&dx) || !(-1..=1).contains(&dy) || !(-1..=1).contains(&dz) {
        return None;
    }
    let id = snapshot.at(dx, dy, dz).biome_at_block(lx, ly, lz) as usize;
    // The live registry order wins whenever one is known (issue #96's
    // follow-up): `snapshot.biome_names` is only ever non-empty when
    // `TerrainMesh::mesh_column`/`mesh_section` attached a real `Login`-time
    // `registry_data` sync via `with_biome_names` — see that method's doc.
    // Empty (no connection yet, an offline/demo world, a version/server that
    // sends no biome registry, or every test and hermetic gate that builds a
    // `SectionSnapshot` without opting in) falls back to the alphabetical
    // table, which is correct only against this project's own server — see
    // `FALLBACK_BIOME_NAMES`'s own doc for why that is a known, provisional
    // gap rather than an oversight.
    if snapshot.biome_names.is_empty() {
        FALLBACK_BIOME_NAMES.get(id).copied()
    } else {
        snapshot.biome_names.get(id).copied()
    }
}

impl ModelSectionView for SnapshotModelView<'_> {
    fn quads_at(&self, x: usize, y: usize, z: usize) -> &[BakedQuad] {
        let id = self.snapshot.at(0, 0, 0).get_block(x, y, z);
        self.models.quads(id)
    }

    /// Vanilla's `ambientocclusion` model-JSON flag, per state (issue #22).
    ///
    /// The trait default is `true`, which is what preserved behaviour while this
    /// was unwired — so **the flag mechanism was inert in the running game until
    /// this override existed**, exactly the island shape `CLAUDE.md` rule 1
    /// names. Mirrors `quads_at`'s lookup deliberately: same state id, same
    /// `BlockModels`, so a model whose flag says "flat" cannot disagree with the
    /// geometry it was baked alongside.
    ///
    /// Note this is only the model-flag third of
    /// `ModelBlockRenderer.java:65`'s predicate; the `getLightEmission() == 0`
    /// clause has no data source in this codebase yet — see
    /// `docs/model-smooth-lighting.md`.
    fn ambient_occlusion_at(&self, x: usize, y: usize, z: usize) -> bool {
        let id = self.snapshot.at(0, 0, 0).get_block(x, y, z);
        self.models.ambient_occlusion(id)
    }

    fn occludes_at(&self, x: i32, y: i32, z: i32) -> bool {
        let (dx, lx) = split16(x);
        let (dy, ly) = split16(y);
        let (dz, lz) = split16(z);
        // Only the 3×3×3 neighbourhood is snapshotted; a probe further out (never
        // emitted by a single one-block cullface step) reads as non-occluding.
        if !(-1..=1).contains(&dx) || !(-1..=1).contains(&dy) || !(-1..=1).contains(&dz) {
            return false;
        }
        let id = self.snapshot.at(dx, dy, dz).get_block(lx, ly, lz);
        self.models.occludes(id)
    }

    /// Vanilla's ambient-occlusion occluder test, `getShadeBrightness == 0.2F`
    /// — a **collision** predicate, not the `occludes_at` culling one above.
    ///
    /// The trait default forwards to `occludes_at`, which is why this override
    /// is the whole fix: without it, leaves (a full collision cube whose cutout
    /// sprite means it does not occlude for culling) contributed `1.0` to every
    /// AO corner and the underside of a tree canopy stayed full-bright. Same
    /// island shape as `ambient_occlusion_at` above — the default preserved
    /// behaviour, so the mechanism was inert in the running game until the
    /// override existed.
    ///
    /// `SectionSnapshot` stores **vanilla global block-state ids** (see
    /// `quads_at`), which is exactly `lodestone_data::shade_brightness`'s key
    /// space, so this is an O(1) bitset read with no allocation. An id past the
    /// snapshotted 3×3×3 neighbourhood, or past the table, reads as open — the
    /// same conservative answer `occludes_at` gives.
    fn ao_occludes_at(&self, x: i32, y: i32, z: i32) -> bool {
        let (dx, lx) = split16(x);
        let (dy, ly) = split16(y);
        let (dz, lz) = split16(z);
        if !(-1..=1).contains(&dx) || !(-1..=1).contains(&dy) || !(-1..=1).contains(&dz) {
            return false;
        }
        let id = self.snapshot.at(dx, dy, dz).get_block(lx, ly, lz);
        lodestone_data::shade_brightness::occludes_ambient_light(id) == Some(true)
    }

    fn light_at(&self, x: usize, y: usize, z: usize) -> u8 {
        // No facing (cross plants, and any view that ignores `face_light_at`):
        // the brightest cell in the immediate neighbourhood, self included.
        self.light.max_light(x, y, z)
    }

    fn face_light_at(&self, x: usize, y: usize, z: usize, dir: Direction) -> u8 {
        self.light
            .face_light(x, y, z, face_of_direction(dir).normal())
    }

    fn corner_light_at(&self, x: i32, y: i32, z: i32) -> u8 {
        let (sky, block) = self.light.levels_at(x, y, z);
        (sky << 4) | block
    }

    /// The real, position-blended biome colour for a grass/foliage/
    /// dry-foliage/water quad — the live consumer of [`biome_name_at`] +
    /// [`resolve_blended_tint`], and the whole reason issue #171/#174's
    /// `BiomeTint` trait now has an implementor outside a test mock. `slot`
    /// tells us *which* of the four kinds this quad is
    /// ([`biome_tint_kind_for_slot`]); `None` when it's not one of them (no
    /// override needed) or when [`BlockModels::colormaps`] failed to load
    /// (tolerated — falls back to the reserved slot's plains default in the
    /// palette, exactly as before this existed).
    fn biome_tint_at(&self, x: usize, y: usize, z: usize, slot: u8) -> Option<[u8; 3]> {
        let kind = biome_tint_kind_for_slot(slot)?;
        let colormaps = self.models.colormaps()?;
        let biome = NamedBiomeTint::new(|pos| biome_name_at(self.snapshot, pos));
        let rgb = resolve_blended_tint(
            kind,
            colormaps,
            &biome,
            BLEND_RADIUS,
            x as i32,
            y as i32,
            z as i32,
        )?;
        Some(rgb_to_bytes(rgb))
    }
}

/// Mesh a snapshot into wide baked-model geometry — the live vanilla path.
///
/// Every block (full cubes included) is emitted from its baked model quads,
/// face-culled against neighbours' [`BlockModels::occludes`]. This is what lets
/// cross-plants, slabs, stairs and translucent blocks render as their true
/// geometry instead of synthetic full cubes. Pure and thread-safe like
/// [`mesh_snapshot`].
#[must_use]
pub fn mesh_snapshot_models(snapshot: &SectionSnapshot, models: &BlockModels) -> ModelMesh {
    let view = SnapshotModelView {
        snapshot,
        models,
        light: SnapshotLight::new(snapshot),
    };
    mesh_models(&view)
}

/// The mesher's fluid view over a snapshot: resolves each cell's fluid (if any)
/// and occlusion out of the paletted sections, and reads the centre section's
/// light. Fluids need the same signed neighbourhood as the model path (one cell
/// past a section edge) to cull shared faces and slope corners.
struct SnapshotFluidView<'a> {
    snapshot: &'a SectionSnapshot,
    models: &'a BlockModels,
    light: SnapshotLight<'a>,
}

impl FluidSectionView for SnapshotFluidView<'_> {
    fn fluid_at(&self, x: i32, y: i32, z: i32) -> Option<FluidCell> {
        let (dx, lx) = split16(x);
        let (dy, ly) = split16(y);
        let (dz, lz) = split16(z);
        if !(-1..=1).contains(&dx) || !(-1..=1).contains(&dy) || !(-1..=1).contains(&dz) {
            return None;
        }
        let id = self.snapshot.at(dx, dy, dz).get_block(lx, ly, lz);
        self.models.fluid(id)
    }

    fn occludes_at(&self, x: i32, y: i32, z: i32) -> bool {
        let (dx, lx) = split16(x);
        let (dy, ly) = split16(y);
        let (dz, lz) = split16(z);
        if !(-1..=1).contains(&dx) || !(-1..=1).contains(&dy) || !(-1..=1).contains(&dz) {
            return false;
        }
        let id = self.snapshot.at(dx, dy, dz).get_block(lx, ly, lz);
        self.models.occludes(id)
    }

    /// Whether the neighbour at `(x, y, z)` takes water's **overlay** sprite
    /// rather than its still/flow sprite.
    ///
    /// Without this override the trait default answered `false` everywhere, so one
    /// of the five `FluidRenderer` divergences was fixed in `lodestone-render` and
    /// **not live**: the crate had the behaviour and the shell's view never asked
    /// for it. Same shape as `occludes_at` above, keyed on
    /// `BlockModels::fluid_overlay`.
    fn overlay_at(&self, x: i32, y: i32, z: i32) -> bool {
        let (dx, lx) = split16(x);
        let (dy, ly) = split16(y);
        let (dz, lz) = split16(z);
        if !(-1..=1).contains(&dx) || !(-1..=1).contains(&dy) || !(-1..=1).contains(&dz) {
            return false;
        }
        let id = self.snapshot.at(dx, dy, dz).get_block(lx, ly, lz);
        self.models.fluid_overlay(id)
    }

    /// The live half of the partial-occluder cull, and it exists for exactly the
    /// reason `overlay_at` above does: `lodestone-render` grew the behaviour and
    /// the trait default answers `None` everywhere, so without this override the
    /// fix sits in the crate and never reaches a real server's terrain.
    ///
    /// Note this reads **outline** shapes, not collision shapes. Vanilla's
    /// `getOcclusionShape` is the outline getter, and the two tables disagree for
    /// roughly half of 26.2's states — reading `collision_shapes` here would be
    /// wrong for about as many blocks as it was right for.
    fn partial_occluder_y_range_at(&self, x: i32, y: i32, z: i32) -> Option<(f32, f32)> {
        let (dx, lx) = split16(x);
        let (dy, ly) = split16(y);
        let (dz, lz) = split16(z);
        if !(-1..=1).contains(&dx) || !(-1..=1).contains(&dy) || !(-1..=1).contains(&dz) {
            return None;
        }
        let id = self.snapshot.at(dx, dy, dz).get_block(lx, ly, lz);
        let boxes = lodestone_data::outline_shapes::outline_boxes(id)?;
        lodestone_assets::fluid::full_footprint_y_range(boxes)
    }

    fn light_at(&self, x: usize, y: usize, z: usize) -> u8 {
        // A fluid surface has no single facing (its top slopes and its sides are
        // baked together), so it takes the brightest cell of the immediate
        // neighbourhood. Water is not opaque, so its own cell carries real light
        // and dominates; the neighbours matter for the surface layer, whose cell
        // sits under whatever air is above it.
        self.light.max_light(x, y, z)
    }

    fn fluid_sprites(&self, kind: FluidKind) -> FluidSprites {
        self.models.fluid_sprites(kind)
    }

    /// The real, position-blended water colour — the fluid-path counterpart
    /// of [`SnapshotModelView::biome_tint_at`]. `x, y, z` are already
    /// snapshot-relative and signed (matching every other method here), so
    /// [`biome_name_at`] takes them directly with no coordinate conversion.
    fn water_tint_at(&self, x: i32, y: i32, z: i32) -> Option<[u8; 3]> {
        let colormaps = self.models.colormaps()?;
        let biome = NamedBiomeTint::new(|pos| biome_name_at(self.snapshot, pos));
        let rgb = resolve_blended_tint(
            lodestone_assets::tint::TintKind::Water,
            colormaps,
            &biome,
            BLEND_RADIUS,
            x,
            y,
            z,
        )?;
        Some(rgb_to_bytes(rgb))
    }
}

/// Mesh a snapshot's fluid cells into water (translucent) and lava (opaque,
/// full-bright) geometry. Runs alongside [`mesh_snapshot_models`]; the block path
/// emits no quads for fluid cells, so the two never double-render.
///
/// Public so a gate can measure the **live** fluid path rather than
/// `mesh_simple`, which has no fluid path at all — `docs/fluid-rendering.md`'s
/// "there are two meshers" gotcha, and the reason the #389 seam gate exists in
/// two halves.
#[must_use]
pub fn mesh_snapshot_fluids(snapshot: &SectionSnapshot, models: &BlockModels) -> FluidMeshes {
    let view = SnapshotFluidView {
        snapshot,
        models,
        light: SnapshotLight::new(snapshot),
    };
    mesh_fluids(&view)
}

/// The geometry a worker produced for one section.
///
/// The demo world meshes to a packed full-cube [`Mesh`]; the live vanilla world
/// meshes to wide baked-model [`ModelMesh`] geometry. The two never mix within a
/// session (the classifier picks one id space), but both flow through the same
/// [`Meshed`]/upload seam so the GPU side can dispatch on the variant.
#[derive(Debug)]
pub enum SectionGeometry {
    /// Packed full-cube geometry (demo world).
    Packed(Mesh),
    /// Wide baked-model geometry (live vanilla world): opaque terrain (blocks +
    /// lava, drawn on the opaque model pass) plus translucent water (drawn on the
    /// fluid pass after opaque geometry, so the sea floor shows through).
    Model {
        /// Opaque block geometry, with lava merged in.
        opaque: ModelMesh,
        /// Translucent water surface geometry.
        water: ModelMesh,
    },
}

impl SectionGeometry {
    /// The merged quad count, for stats/overlay parity across both paths.
    #[must_use]
    pub fn quad_count(&self) -> usize {
        match self {
            SectionGeometry::Packed(m) => m.quad_count(),
            SectionGeometry::Model { opaque, water } => opaque.quad_count() + water.quad_count(),
        }
    }
}

/// A finished mesh with its key, handed back from a worker.
#[derive(Debug)]
pub struct Meshed {
    /// Which section this mesh is for.
    pub key: SectionKey,
    /// The geometry (packed demo cubes or vanilla baked models).
    pub mesh: SectionGeometry,
}

enum Job {
    Mesh(SectionSnapshot),
    Stop,
}

/// A fixed pool of worker threads that mesh snapshots off the main thread.
///
/// # Why this is a `Resource`, and why that does *not* put meshing on the frame
/// thread
///
/// Stage 4 (`docs/bevy-migration.md`) moves this off `Sim` and into the ECS
/// `World`, so the enqueue and drain steps can be ordinary systems a plugin
/// orders against. What did **not** change is where the work happens: the pool
/// is still `worker_count` OS threads, [`submit`](Self::submit) still only sends
/// down a channel, and [`drain`](Self::drain) still only `try_recv`s. A slow
/// frame therefore delays the *upload* of finished geometry, never the meshing
/// and never the simulation — `docs/frame-pacing.md`'s rule that presentation
/// must not gate simulation is untouched, and it must stay that way: a client the
/// server considers stalled is sent no chunks at all.
///
/// [`drain_blocking`](Self::drain_blocking) is the one method that *does* block
/// the caller. It has exactly two callers, both outside the frame loop — the
/// headless/one-shot render path and `Sim::end_session`'s flush — and it must
/// stay that way.
///
/// `result_rx` is wrapped in a `Mutex` purely to make the type `Sync`, which
/// `bevy_ecs`'s `Resource: Send + Sync + 'static` bound requires: an `mpsc`
/// `Receiver` is `Send` but not `Sync`. The lock is uncontended (only the driver
/// drains) and is never held across a `recv` that could block for long — see
/// `drain_blocking`, which holds it for the whole blocking wait *by design*,
/// since two concurrent drains of one result queue would interleave meshes
/// arbitrarily.
#[derive(Debug, Resource)]
pub struct MeshScheduler {
    job_tx: Sender<Job>,
    result_rx: Mutex<Receiver<Meshed>>,
    workers: Vec<JoinHandle<()>>,
    pending: usize,
    column_source: ColumnSource,
}

impl MeshScheduler {
    /// Spawn `worker_count` (min 1) meshing threads, each meshing with a clone of
    /// `classifier`. The classifier picks the id space: a
    /// [`ShellClassifier::Demo`] pool meshes the offline demo world, a
    /// [`ShellClassifier::Vanilla`] pool meshes the live server's vanilla world.
    /// The atlas behind the vanilla variant is `Arc`-shared, so a per-worker
    /// clone is a refcount bump.
    ///
    /// # The classifier also fixes the [`ColumnSource`]
    ///
    /// The id space and the *provenance* of the columns are the same choice made
    /// twice: `ShellClassifier::is_vanilla`'s own docs state the invariant — "the
    /// session meshes the live world only under this variant and the demo world
    /// only under `Demo`" — and `Sim::build` holds it with a `debug_assert!`.
    /// Deriving it here rather than taking a fourth argument keeps the two from
    /// ever being set inconsistently, which is the failure this would otherwise
    /// invite: a `Streaming` demo world blanks its outer ring forever, a
    /// `Complete` live world is issue #389 unfixed.
    ///
    /// The one non-obvious case is the fallback session — vanilla assets failed
    /// to load, so a live connection meshes under `Demo`. `Complete` is still
    /// right there, because `MeshPolicy::id_spaces_agree` is `false` and
    /// [`TerrainMesh::mesh_column`] meshes nothing at all.
    #[must_use]
    pub fn new(worker_count: usize, classifier: ShellClassifier) -> Self {
        let column_source = if classifier.is_vanilla() {
            ColumnSource::Streaming
        } else {
            ColumnSource::Complete
        };
        let worker_count = worker_count.max(1);
        let (job_tx, job_rx) = mpsc::channel::<Job>();
        let (result_tx, result_rx) = mpsc::channel::<Meshed>();
        let job_rx = Arc::new(Mutex::new(job_rx));

        let mut workers = Vec::with_capacity(worker_count);
        for _ in 0..worker_count {
            let job_rx = Arc::clone(&job_rx);
            let result_tx = result_tx.clone();
            let classifier = classifier.clone();
            workers.push(thread::spawn(move || {
                loop {
                    let job = {
                        let lock = job_rx.lock().expect("mesh job queue poisoned");
                        lock.recv()
                    };
                    match job {
                        Ok(Job::Mesh(snap)) => {
                            // The vanilla classifier carries baked models → mesh
                            // through the model path; the demo classifier has none
                            // → mesh through the packed full-cube path.
                            let mesh = match classifier.models() {
                                Some(models) => {
                                    let mut opaque = mesh_snapshot_models(&snap, models);
                                    let fluids = mesh_snapshot_fluids(&snap, models);
                                    // Lava is opaque and full-bright: fold it into
                                    // the opaque pass. Water is translucent and
                                    // drawn separately.
                                    opaque.merge(&fluids.lava);
                                    SectionGeometry::Model {
                                        opaque,
                                        water: fluids.water,
                                    }
                                }
                                None => SectionGeometry::Packed(mesh_snapshot(&snap, &classifier)),
                            };
                            if result_tx
                                .send(Meshed {
                                    key: snap.key,
                                    mesh,
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                        Ok(Job::Stop) | Err(_) => break,
                    }
                }
            }));
        }

        Self {
            job_tx,
            result_rx: Mutex::new(result_rx),
            workers,
            pending: 0,
            column_source,
        }
    }

    /// Whether the world this pool meshes has all its columns already. See
    /// [`MeshScheduler::new`] for why the classifier decides this.
    #[must_use]
    pub fn column_source(&self) -> ColumnSource {
        self.column_source
    }

    /// Queue a snapshot for meshing.
    pub fn submit(&mut self, snapshot: SectionSnapshot) {
        self.pending += 1;
        // Send failure only happens if all workers died; drop the job then.
        if self.job_tx.send(Job::Mesh(snapshot)).is_err() {
            self.pending -= 1;
        }
    }

    /// Number of submitted jobs not yet drained.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.pending
    }

    /// Collect any finished meshes without blocking.
    pub fn drain(&mut self) -> Vec<Meshed> {
        let mut out = Vec::new();
        let rx = self.result_rx.get_mut().expect("mesh result queue poisoned");
        while let Ok(meshed) = rx.try_recv() {
            out.push(meshed);
        }
        self.pending -= out.len();
        out
    }

    /// Block until at least `n` results are available (or all pending done),
    /// returning everything collected. Used by tests and headless runs.
    pub fn drain_blocking(&mut self, n: usize) -> Vec<Meshed> {
        let mut out = Vec::new();
        let rx = self.result_rx.get_mut().expect("mesh result queue poisoned");
        while out.len() < n && self.pending > out.len() {
            match rx.recv() {
                Ok(meshed) => out.push(meshed),
                Err(_) => break,
            }
        }
        self.pending -= out.len();
        out
    }
}

impl Drop for MeshScheduler {
    fn drop(&mut self) {
        for _ in &self.workers {
            let _ = self.job_tx.send(Job::Stop);
        }
        for w in self.workers.drain(..) {
            let _ = w.join();
        }
    }
}

// ---------------------------------------------------------------------------
// Terrain meshing as ECS state (Stage 4)
// ---------------------------------------------------------------------------

/// Budget for [`heal_dirty_columns`]: how many stale-boundary columns to re-mesh
/// per frame. Bounds the cost of a chunk-load burst — during a spiral load the
/// same column is named by several arrivals and coalesced into one re-mesh, so a
/// small budget is enough to keep seams closed without stalling a frame.
pub const DIRTY_COLUMN_BUDGET: usize = 4;

/// The two facts terrain meshing needs that the [`ChunkWorld`] store cannot
/// answer, because they are properties of the *session* rather than of the
/// chunks.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeshPolicy {
    /// How an **absent** sky sample resolves — the connected dimension's
    /// `has_skylight`. See [`sky_default_for_dimension`].
    pub sky_default: SkyDefault,
    /// Whether the id space the worker pool's classifier was built for is the id
    /// space the store actually holds.
    ///
    /// The demo palette and the vanilla registry are disjoint block-id spaces, so
    /// meshing one with the other's classifier does not fail — it draws garbage,
    /// or nothing. `false` is the "vanilla assets failed to load but we joined a
    /// server anyway" session, which used to `return` silently on a `world.get`
    /// miss and render an empty world with a clean log. It now counts into
    /// [`TerrainMesh::drops`] and warns.
    pub id_spaces_agree: bool,
}

impl Default for MeshPolicy {
    fn default() -> Self {
        Self {
            sky_default: SkyDefault::Full,
            id_spaces_agree: true,
        }
    }
}

/// All terrain-meshing state, as one `Resource`.
///
/// Stage 4 of `docs/bevy-migration.md` moves `Sim`'s `scheduler`,
/// `dirty_columns`, `pending_removals`, `uploaded_sections` and `mesh_drops`
/// here. **One** resource rather than five because they are one subsystem's
/// state and every operation touches several at once: a column that snapshots to
/// nothing pushes a removal *and* may count a drop, and a drained mesh records an
/// upload. Five `ResMut`s would be five borrows of one invariant.
///
/// The worker pool is still a worker pool ([`MeshScheduler`]'s docs say why that
/// matters). Systems here only enqueue and drain.
#[derive(Resource, Debug)]
pub struct TerrainMesh {
    /// The off-thread worker pool.
    pub scheduler: MeshScheduler,
    /// Loaded columns whose *boundary* geometry is stale because a horizontal
    /// neighbour arrived after they were meshed, coalesced.
    ///
    /// A section's mesh depends on its whole 3×3×3 neighbourhood (face culling,
    /// AO, and — most visibly — fluid corner heights and flow faces), so loading
    /// column P invalidates the boundary of P's eight horizontal neighbours too.
    /// Meshing only P leaves every already-meshed neighbour believing there is air
    /// across the seam: **water grows a falling "wall" at each chunk border** and
    /// cross-chunk AO stays wrong. Re-meshing the eight eagerly on every arrival
    /// would be 9× the work, so they coalesce here and drain on a budget.
    pub dirty_columns: BTreeSet<(i32, i32)>,
    /// Columns that must be meshed **even if a horizontal neighbour is missing**,
    /// because a neighbour is missing for a reason that will never resolve: it
    /// left the tracking view.
    ///
    /// Issue #479's second half, and the one that survives any amount of CPU.
    /// [`SnapshotOutcome::Deferred`] means "a neighbour column has not arrived
    /// *yet*", and [`Self::route`] drops a deferred section that has never
    /// reached the GPU, relying on [`Self::mark_neighbours_dirty`] to re-drive it
    /// when the missing column lands. For a column on the **trailing** edge of the
    /// view the missing column never lands — it already came and went — so the
    /// deferral is permanent and nothing ever re-queues it.
    ///
    /// Whether that happens is a race between the heal budget and the player's
    /// speed, which is why it presents as "chunks stop drawing the further you
    /// go" and why it worsens with frame rate: a column that the 4-per-frame
    /// [`DIRTY_COLUMN_BUDGET`] reaches while the column behind it is still loaded
    /// gets uploaded and is thereafter exempt (the `uploaded_sections` clause);
    /// one it reaches later is dropped **forever**. Measured with counts and no
    /// timing at all in `standing_still_drains_the_heal_backlog_and_no_column_is_lost`:
    /// walking twelve steps at the real budget left the whole trailing column
    /// strip missing, and standing still afterwards never brought it back.
    ///
    /// So [`Self::forget_column`] enqueues the departing column's loaded
    /// neighbours here, where "missing" is known *at that moment* to mean gone
    /// rather than pending. Forcing can bake one seam against air if some *other*
    /// neighbour is genuinely still in flight, and that is the right trade: a
    /// wrong seam is corrected by the ordinary arrival signal, whereas invisible
    /// terrain you can still walk into is not corrected by anything.
    ///
    /// Coalesced and budgeted exactly like `dirty_columns` rather than re-meshed
    /// eagerly — an unload sweep names up to eight columns and a single step of
    /// the player unloads a whole strip.
    pub forced_columns: BTreeSet<(i32, i32)>,
    /// Columns known to have **left** the view, as opposed to not having arrived
    /// yet. The two are indistinguishable in the store — both are simply absent —
    /// and telling them apart is what makes [`Self::forced_columns`] safe.
    ///
    /// Without this the force is too broad, and the harness caught it: forcing
    /// every loaded neighbour of a departing column also drags in the
    /// **outermost** ring of the view, whose missing neighbour is missing because
    /// it is beyond the view entirely. That ring is exactly the buffer
    /// singleplayer streams `render_distance + 1` to keep *off* screen
    /// (`app/session.rs`), because a section meshed without its outer neighbour
    /// bakes its seam against air — the "blocky water far away" report. So a
    /// forced column is only really forced when **every** neighbour it still
    /// lacks is in this set.
    ///
    /// Bounded, not a second leak: an entry is dropped as soon as it has no
    /// loaded neighbour left, at which point it cannot affect any decision.
    pub departed: HashSet<(i32, i32)>,
    /// Sections whose geometry vanished (all-air after an edit, or a column that
    /// unloaded) and must be dropped from the GPU. Drained by the app each frame.
    pub pending_removals: Vec<SectionKey>,
    /// Every `SectionKey` this session has handed out for GPU upload and that has
    /// not yet come back out through a removal.
    ///
    /// `RenderState`'s GPU-side section map has no session id in its key, so
    /// without tracking this a quit-to-title followed by a reconnect would leave
    /// the *previous* server's terrain rendered until a new chunk happened to land
    /// on the exact same key.
    pub uploaded_sections: HashSet<SectionKey>,
    /// Count of loaded columns that failed to mesh (id spaces disagreed, or an
    /// all-air centre on a column the server reports loaded). Surfaced in the
    /// debug HUD next to `live_cols` so this defect class is a one-line diagnosis
    /// instead of a play-test archaeology session. Should stay `0` in a healthy
    /// session.
    pub drops: u64,
    /// Whether an absent neighbour column means "edge of the world" or "not here
    /// yet", taken from the worker pool's classifier — see
    /// [`MeshScheduler::new`].
    pub column_source: ColumnSource,
    /// How many times a section's **first** build was held back because a
    /// horizontal neighbour column had not arrived (issue #389).
    ///
    /// Expected to be non-zero and rising during chunk streaming — every column
    /// on the frontier defers until the ring beyond it lands — and to stop rising
    /// once the player stops moving. A count that keeps climbing while nothing
    /// loads means the dirty-propagation half ([`Self::mark_neighbours_dirty`])
    /// has stopped re-driving deferred sections, which would show as terrain
    /// missing rather than terrain wrong.
    pub deferred: u64,
    /// The session facts meshing cannot read off the store.
    pub policy: MeshPolicy,
    /// The live biome registry's ordered entry names (issue #96's follow-up),
    /// refreshed alongside [`Self::policy`] by `Sim::refresh_mesh_policy` and
    /// attached to every section this pool snapshots
    /// ([`SnapshotOutcome::with_biome_names`]) so `mesher::biome_name_at`
    /// resolves against the *real* server registry instead of the
    /// alphabetical `FALLBACK_BIOME_NAMES` table. Empty before any
    /// `registry_data` (no connection, the offline demo world, or a
    /// version/server that sends none) — [`biome_name_at`] treats that as
    /// "use the fallback", never as "holder id 0".
    pub biome_names: Arc<[&'static str]>,
}

impl TerrainMesh {
    /// Build the state around a freshly spawned worker pool.
    #[must_use]
    pub fn new(scheduler: MeshScheduler) -> Self {
        Self {
            column_source: scheduler.column_source(),
            scheduler,
            dirty_columns: BTreeSet::new(),
            forced_columns: BTreeSet::new(),
            departed: HashSet::new(),
            pending_removals: Vec::new(),
            uploaded_sections: HashSet::new(),
            drops: 0,
            deferred: 0,
            policy: MeshPolicy::default(),
            biome_names: Arc::from([]),
        }
    }

    /// Route one section's snapshot outcome: submit it, drop its stale geometry,
    /// or hold it back. Returns whether anything was submitted.
    ///
    /// **Vanilla's rule, and the reason it has two halves.**
    /// `LevelExtractor.extract` compiles a dirty section when
    /// `section.sectionMesh.get() != CompiledSectionMesh.UNCOMPILED ||
    /// sectionUpdateTracker.hasAllNeighbors(level, node)`. The first clause is
    /// what stops the deferral from being a *regression*: a section already on
    /// screen rebuilds unconditionally, so a chunk unloading at the far edge of
    /// the view does not blink out the ring beside it, and a block edit next to
    /// the frontier still shows. Only a section that has never reached the screen
    /// waits — and it cannot wait forever, because [`Self::mark_neighbours_dirty`]
    /// re-drives it the moment the missing column lands.
    /// [`Self::uploaded_sections`] is our `!= UNCOMPILED`.
    ///
    /// `force` is the third way out, added with [`Self::forced_columns`]: the
    /// caller knows a missing neighbour is missing because it *left the view*, so
    /// waiting for it is waiting forever. Vanilla does not need this clause
    /// because its client tracks the view rectangle and can tell "outside the
    /// view" from "inside it and not here yet"; we learn the same fact from the
    /// unload signal instead.
    fn route(&mut self, key: SectionKey, outcome: SnapshotOutcome, force: bool) -> bool {
        match outcome {
            SnapshotOutcome::Ready(snap) => {
                self.scheduler.submit(snap);
                true
            }
            // A single empty section is routine (sky/void sections have no
            // geometry): drop it from the GPU, no alarm.
            SnapshotOutcome::Empty => {
                self.pending_removals.push(key);
                false
            }
            SnapshotOutcome::Deferred(snap) => {
                if force || self.uploaded_sections.contains(&key) {
                    self.scheduler.submit(snap);
                    true
                } else {
                    // Deliberately *not* a removal: there is nothing on the GPU
                    // for this key, and queueing one would make the deferral
                    // look like an unload to the app's drain.
                    self.deferred = self.deferred.saturating_add(1);
                    false
                }
            }
        }
    }

    /// Re-snapshot and re-schedule every section of the column at `(cx, cz)`.
    ///
    /// One implementation for both worlds, which is the point of Stage 4: before
    /// it, this branched on `vanilla_atlas.is_some() && net.is_some() &&
    /// world_dimensions().is_some()` and read one of two `World`s. Now there is
    /// one store, so there is one path.
    ///
    /// An **unloaded** column is a silent no-op: it will be queued for real by its
    /// own arrival, and counting it would drown the drop counter in noise. A
    /// *loaded* column that yields no geometry at all is the "invisible blocks"
    /// defect class and is counted and logged loudly.
    pub fn mesh_column(&mut self, store: &ChunkWorld, cx: i32, cz: i32) {
        self.mesh_column_inner(store, cx, cz, false);
    }

    /// [`Self::mesh_column`], but a section whose neighbourhood is incomplete is
    /// submitted anyway instead of held back.
    ///
    /// For columns drained from [`Self::forced_columns`] — those whose missing
    /// neighbour has *left the view* and is therefore never arriving. See that
    /// field's doc for why waiting on it is waiting forever.
    /// Forces only when [`Self::all_absent_neighbours_departed`] agrees. A column
    /// still genuinely waiting on an arrival falls back to the ordinary path,
    /// which is what keeps the outermost buffer ring off screen.
    pub fn mesh_column_forced(&mut self, store: &ChunkWorld, cx: i32, cz: i32) {
        let force = self.all_absent_neighbours_departed(store, cx, cz);
        self.mesh_column_inner(store, cx, cz, force);
    }

    fn mesh_column_inner(&mut self, store: &ChunkWorld, cx: i32, cz: i32, force: bool) {
        if !self.policy.id_spaces_agree {
            self.drops += 1;
            tracing::warn!(
                cx,
                cz,
                branch = "id-space-mismatch",
                "column skipped: the mesh classifier's block-id space is not the store's \
                 (vanilla assets missing on a live session, or the reverse)"
            );
            return;
        }
        if !store.contains_column(cx, cz) {
            return;
        }
        let Some(extent) = store.extent() else {
            return;
        };

        // One lock for the whole column — the snapshots are owned and `Send`, so
        // the guard is dropped before anything is submitted and the world is
        // never locked while meshing.
        let mut jobs: Vec<(SectionKey, SnapshotOutcome)> =
            Vec::with_capacity(extent.section_count);
        {
            let world = store.read();
            for si in 0..extent.section_count {
                let key = SectionKey {
                    cx,
                    cz,
                    si,
                    min_y: extent.min_y,
                };
                jobs.push((
                    key,
                    snapshot_section_in(
                        &world,
                        key,
                        Some(extent.section_count),
                        self.policy.sky_default,
                        self.column_source,
                    )
                    .with_biome_names(Arc::clone(&self.biome_names)),
                ));
            }
        }

        let mut meshed_any = false;
        let mut deferred_any = false;
        for (key, outcome) in jobs {
            deferred_any |= matches!(outcome, SnapshotOutcome::Deferred(_));
            meshed_any |= self.route(key, outcome, force);
        }
        // A column held back for its neighbourhood is not a drop: it is the
        // frontier of a streaming load doing exactly what it should, and counting
        // it would drown the "invisible blocks" alarm in noise on every join.
        if !meshed_any && !deferred_any {
            self.drops += 1;
            tracing::warn!(
                cx,
                cz,
                branch = "all-air-loaded-column",
                "loaded column produced no geometry despite a dirty signal"
            );
        }
    }

    /// Re-snapshot and re-schedule exactly one section. A section that snapshots
    /// to nothing is queued for GPU removal rather than left showing stale
    /// geometry; one whose neighbourhood is incomplete is handled by
    /// [`Self::route`] (rebuilt if it is already on screen, held back if not).
    pub fn mesh_section(&mut self, store: &ChunkWorld, key: SectionKey, section_count: usize) {
        let outcome = {
            let world = store.read();
            snapshot_section_in(
                &world,
                key,
                Some(section_count),
                self.policy.sky_default,
                self.column_source,
            )
            .with_biome_names(Arc::clone(&self.biome_names))
        };
        self.route(key, outcome, false);
    }

    /// Queue the eight **loaded** horizontal neighbours of `(cx, cz)` for a
    /// boundary re-mesh. The centre is meshed by the caller, immediately, for load
    /// responsiveness; the neighbours coalesce.
    ///
    /// This is the same eight columns vanilla's
    /// `ClientPacketListener.enableChunkLight` dirties on chunk arrival
    /// (`setSectionRangeDirty(x-1, minSectionY, z-1, x+1, maxSectionY, z+1)` —
    /// the 3×3 column footprint over the whole vertical range), and it is *also*
    /// the mechanism that un-defers: a section held back by
    /// [`SnapshotOutcome::Deferred`] is re-snapshotted here the moment the column
    /// it was waiting for lands. Without this the deferral would be permanent
    /// rather than a wait.
    pub fn mark_neighbours_dirty(&mut self, store: &ChunkWorld, cx: i32, cz: i32) {
        for dx in -1..=1 {
            for dz in -1..=1 {
                if dx == 0 && dz == 0 {
                    continue;
                }
                let (nx, nz) = (cx + dx, cz + dz);
                if store.contains_column(nx, nz) {
                    self.dirty_columns.insert((nx, nz));
                }
            }
        }
    }

    /// Drop every GPU section belonging to the column at `(cx, cz)`, which the
    /// server has just told us to forget (issue #479).
    ///
    /// **This is the mesh side of an eviction that only ever had a collision
    /// side.** The adapter answers `forget_level_chunk` by calling
    /// `WorldSink::unload`, so the one [`ChunkWorld`] store loses the column
    /// immediately — and collision, which re-reads the store every tick
    /// (`sim/collide.rs`), tracks that for free. Nothing did the same for the
    /// renderer: `ClientEvent::ChunkUnloaded` had four producers and **zero**
    /// consumers, so [`Self::uploaded_sections`], `RenderState`'s section map
    /// and the fixed-capacity section-origin arena only ever grew, for the
    /// whole session, while the store shrank underneath them. Walking in one
    /// direction therefore accumulated every column ever visited as live draw
    /// calls — `gpu/frame.rs` iterates `model.sections` with no distance or
    /// frustum cull — and ended at an exhausted origin arena, whose failure
    /// mode is `upload_section` returning early: **new terrain stops drawing
    /// while its collision is perfectly present.** That is the reported symptom.
    ///
    /// Derived from [`Self::uploaded_sections`] rather than from the store,
    /// deliberately: by the time this runs the column is *already gone* from
    /// the store (the adapter unloads before it emits), so `store.extent()` and
    /// `contains_column` cannot enumerate what to drop. The uploaded set is the
    /// only record of what this column put on the GPU.
    ///
    /// The loaded neighbours go into [`Self::forced_columns`], **not**
    /// [`Self::dirty_columns`], and that distinction is the whole of #479's second
    /// half. An ordinary dirty signal re-snapshots them and then *drops the result*
    /// unless they already reached the GPU, because a missing neighbour reads as
    /// "not arrived yet". Here we know better: this very call is the neighbour
    /// leaving. A column on the trailing edge of the view that has never been
    /// uploaded would otherwise stay deferred forever, since the column it waits
    /// on already came and went and nothing re-queues it — measured as a whole
    /// missing column strip that standing still never recovers. See
    /// [`Self::forced_columns`].
    pub fn forget_column(&mut self, cx: i32, cz: i32) {
        // A queued heal for a column that has left is budget spent on a
        // `mesh_column` that will early-return anyway.
        self.dirty_columns.remove(&(cx, cz));
        self.forced_columns.remove(&(cx, cz));
        let gone: Vec<SectionKey> = self
            .uploaded_sections
            .iter()
            .filter(|key| key.cx == cx && key.cz == cz)
            .copied()
            .collect();
        for key in gone {
            self.uploaded_sections.remove(&key);
            self.pending_removals.push(key);
        }
    }

    /// Queue the loaded horizontal neighbours of a **departing** column for a
    /// forced re-mesh. Split from [`Self::forget_column`] because it needs the
    /// store and that one deliberately does not take it — see its doc.
    pub fn force_neighbours_of_departed(&mut self, store: &ChunkWorld, cx: i32, cz: i32) {
        self.departed.insert((cx, cz));
        for dx in -1..=1 {
            for dz in -1..=1 {
                if dx == 0 && dz == 0 {
                    continue;
                }
                let (nx, nz) = (cx + dx, cz + dz);
                if store.contains_column(nx, nz) {
                    self.forced_columns.insert((nx, nz));
                }
            }
        }
        // Keep `departed` bounded: an entry with no loaded neighbour can no
        // longer be the reason any column is forced, so it is pure growth.
        self.departed
            .retain(|&(dx, dz)| Self::has_loaded_neighbour(store, dx, dz));
    }

    fn has_loaded_neighbour(store: &ChunkWorld, cx: i32, cz: i32) -> bool {
        (-1..=1).any(|dx| {
            (-1..=1).any(|dz| {
                (dx, dz) != (0, 0) && store.contains_column(cx + dx, cz + dz)
            })
        })
    }

    /// Whether every horizontal neighbour `(cx, cz)` is missing has been
    /// confirmed to have *left*, rather than merely not arrived yet.
    ///
    /// The predicate that keeps the force narrow. `true` means nothing this column
    /// waits for is ever coming, so holding its geometry back is holding it back
    /// forever; `false` means it is an ordinary streaming frontier column and the
    /// existing deferral is right.
    fn all_absent_neighbours_departed(&self, store: &ChunkWorld, cx: i32, cz: i32) -> bool {
        for dx in -1..=1 {
            for dz in -1..=1 {
                if dx == 0 && dz == 0 {
                    continue;
                }
                let n = (cx + dx, cz + dz);
                if !store.contains_column(n.0, n.1) && !self.departed.contains(&n) {
                    return false;
                }
            }
        }
        true
    }

    /// Re-snapshot and re-schedule the section holding `block`, plus any
    /// neighbour section that shares the boundary the block sits on (a face on
    /// a section edge changes the neighbour's mesh via culling/AO). Sections
    /// that became all-air are queued for GPU removal instead — `mesh_section`
    /// already routes that through [`Self::pending_removals`].
    ///
    /// Moved here from `Sim::remesh_around` (`sim/meshing.rs`), which had
    /// reduced to pure `ChunkWorld`/`TerrainMesh` math and no other `Sim`
    /// state — see `docs/plugin-api.md`'s re-mesh-seam note. `Sim::remesh_around`
    /// is now a one-line delegation through `Sim::terrain_and_world`; this is
    /// the version usable from anywhere that already holds a `&ChunkWorld`
    /// and a `&mut TerrainMesh` and nothing else, which is deliberately as far
    /// as this reaches: **not** a `RemeshRequest` resource or event a plugin
    /// could call — re-meshing stays a consequence of a sanctioned world
    /// write, never a plugin-callable verb (`docs/plugin-api.md`'s "what not
    /// to build" note).
    pub fn remesh_around(&mut self, store: &ChunkWorld, block: [i32; 3]) {
        let Some(extent) = store.extent() else {
            return;
        };
        let (min_y, section_count) = (extent.min_y, extent.section_count);
        let cx = block[0].div_euclid(16);
        let cz = block[2].div_euclid(16);
        let lx = block[0].rem_euclid(16);
        let lz = block[2].rem_euclid(16);
        let si = (block[1] - min_y).div_euclid(16);
        let ly = (block[1] - min_y).rem_euclid(16);

        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    if (dx == -1 && lx != 0) || (dx == 1 && lx != 15) {
                        continue;
                    }
                    if (dy == -1 && ly != 0) || (dy == 1 && ly != 15) {
                        continue;
                    }
                    if (dz == -1 && lz != 0) || (dz == 1 && lz != 15) {
                        continue;
                    }
                    let nsi = si + dy;
                    if nsi < 0 || nsi as usize >= section_count {
                        continue;
                    }
                    let key = SectionKey {
                        cx: cx + dx,
                        cz: cz + dz,
                        si: nsi as usize,
                        min_y,
                    };
                    self.mesh_section(store, key, section_count);
                }
            }
        }
    }

    /// Collect finished meshes for the caller to upload, recording each key into
    /// [`Self::uploaded_sections`].
    pub fn drain_meshes(&mut self) -> Vec<Meshed> {
        let meshes = self.scheduler.drain();
        self.uploaded_sections.extend(meshes.iter().map(|m| m.key));
        meshes
    }

    /// Block until every scheduled mesh is ready. Headless runs and tests only —
    /// never the frame loop.
    pub fn drain_all_meshes(&mut self) -> Vec<Meshed> {
        let n = self.scheduler.pending();
        let meshes = self.scheduler.drain_blocking(n);
        self.uploaded_sections.extend(meshes.iter().map(|m| m.key));
        meshes
    }

    /// Sections the app should remove from the GPU.
    pub fn drain_removals(&mut self) -> Vec<SectionKey> {
        let removed = std::mem::take(&mut self.pending_removals);
        for key in &removed {
            self.uploaded_sections.remove(key);
        }
        removed
    }

    /// Session teardown: discard in-flight jobs rather than letting them land in
    /// whatever session comes next, and queue every section this session uploaded
    /// for removal through the app's ordinary drain path.
    pub fn end_session(&mut self) {
        let pending = self.scheduler.pending();
        if pending > 0 {
            let _ = self.scheduler.drain_blocking(pending);
        }
        self.dirty_columns.clear();
        self.forced_columns.clear();
        self.departed.clear();
        self.drops = 0;
        self.deferred = 0;
        self.pending_removals.extend(self.uploaded_sections.drain());
    }
}

/// `Update` / [`FrameSet::Terrain`]: re-mesh up to [`DIRTY_COLUMN_BUDGET`]
/// columns whose boundary went stale.
///
/// This is the coalescing drain — the thing that stops water growing a falling
/// wall at every chunk border. It enqueues snapshots onto the worker pool and
/// returns; it never meshes anything itself.
/// [`TerrainMesh::forced_columns`] is drained **first, and on its own budget**.
/// Those columns are waiting on a neighbour that has already left the view, so
/// unlike an ordinary boundary heal they are not merely stale — they are
/// invisible until this runs, and a shared budget would put them behind whatever
/// backlog the ordinary queue happens to hold. That backlog reached 45 columns in
/// a twelve-step walk, i.e. eleven frames of latency, which is exactly the window
/// in which the old code lost them for good.
pub fn heal_dirty_columns(store: Res<ChunkWorld>, mut terrain: ResMut<TerrainMesh>) {
    for _ in 0..DIRTY_COLUMN_BUDGET {
        let Some((cx, cz)) = terrain.forced_columns.pop_first() else {
            break;
        };
        terrain.mesh_column_forced(&store, cx, cz);
    }
    for _ in 0..DIRTY_COLUMN_BUDGET {
        let Some((cx, cz)) = terrain.dirty_columns.pop_first() else {
            return;
        };
        terrain.mesh_column(&store, cx, cz);
    }
}

/// Registers Stage 4's terrain state and its one `Update` system.
///
/// Deliberately does **not** insert [`TerrainMesh`] itself: the worker pool has
/// to be built with the classifier for whichever id space this session meshes,
/// and that is the session owner's decision — the same rule
/// `lodestone_ecs::CorePlugin` follows for `WorldTime` and
/// `LocalPlayerPlugin` for the local-player entity. It does insert a default
/// [`ChunkWorld`], so a harness that installs only this plugin has a store to
/// read.
#[derive(Debug, Default)]
pub struct TerrainPlugin;

impl Plugin for TerrainPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ChunkWorld>();
        app.add_systems(Update, heal_dirty_columns.in_set(FrameSet::Terrain));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocks::DemoClassifier;

    fn assert_send<T: Send>() {}

    #[test]
    fn snapshot_is_send() {
        // Compile-time proof that snapshots can cross to worker threads.
        assert_send::<SectionSnapshot>();
        assert_send::<Meshed>();
    }

    /// A dimension type as the server would send it, with only `has_skylight`
    /// varied — every other field is irrelevant to this policy and is set to a
    /// value that would be *wrong* for the overworld, so a test that accidentally
    /// read one of them fails.
    fn dim_type(name: &str, has_skylight: bool) -> lodestone_client::DimensionTypeInfo {
        lodestone_client::DimensionTypeInfo {
            name: name.parse().unwrap(),
            has_skylight,
            has_ceiling: true,
            has_fixed_time: true,
            coordinate_scale: 8.0,
            min_y: 0,
            height: 256,
            logical_height: 128,
            ambient_light: 0.1,
        }
    }

    #[test]
    fn sky_default_is_full_for_overworld_and_end_none_for_nether_and_unknown() {
        use lodestone_client::DimensionId;

        let overworld: DimensionId = "minecraft:overworld".parse().unwrap();
        let the_nether: DimensionId = "minecraft:the_nether".parse().unwrap();
        let the_end: DimensionId = "minecraft:the_end".parse().unwrap();
        let custom: DimensionId = "somemod:cave_dimension".parse().unwrap();

        // Every case here passes `None` for the dimension type: this is the
        // pre-#288 name-match fallback, kept verbatim for servers that send no
        // `registry_data`.
        assert_eq!(
            sky_default_for_dimension(None, None),
            SkyDefault::Full,
            "pre-login: keep the full-bright default"
        );
        assert_eq!(
            sky_default_for_dimension(Some(&overworld), None),
            SkyDefault::Full
        );
        // The falsifying case this function exists for: the End has real sky
        // light (`has_skylight: true`) exactly like the overworld, and must
        // not be defaulted to `0` just because it isn't the overworld.
        assert_eq!(
            sky_default_for_dimension(Some(&the_end), None),
            SkyDefault::Full
        );
        assert_eq!(
            sky_default_for_dimension(Some(&the_nether), None),
            SkyDefault::None
        );
        assert_eq!(
            sky_default_for_dimension(Some(&custom), None),
            SkyDefault::None
        );
    }

    #[test]
    fn a_server_declared_dimension_type_overrides_the_level_name_match() {
        use lodestone_client::DimensionId;

        let overworld: DimensionId = "minecraft:overworld".parse().unwrap();
        let custom: DimensionId = "mypack:mine".parse().unwrap();

        // Issue #34, both directions. The name match and the registry disagree
        // in each case, and the registry must win — a test where they agree
        // would pass with the registry lookup deleted.
        assert_eq!(
            sky_default_for_dimension(
                Some(&overworld),
                Some(&dim_type("mypack:dark_overworld", false)),
            ),
            SkyDefault::None,
            "a level called minecraft:overworld with a skylight-less type must be dark"
        );
        assert_eq!(
            sky_default_for_dimension(
                Some(&custom),
                Some(&dim_type("minecraft:overworld", true)),
            ),
            SkyDefault::Full,
            "a datapack level pointing at a skylit type must be lit — this is the \
             case whose name match fell through to None before #288"
        );
    }

    // -----------------------------------------------------------------------
    // #389: a seam meshed against a not-yet-loaded neighbour
    // -----------------------------------------------------------------------

    /// Section count and `min_y` for the seam fixture: two sections, content in
    /// the lower one, so the upper is elided air and the `si == -1` slot is
    /// genuinely out of the world.
    const SEAM_SECTIONS: usize = 2;

    /// One fixture column. `water_over` decides, per `(x, z)`, whether section 0
    /// holds water at every `y` or nothing at all.
    fn seam_column(water_over: &dyn Fn(usize, usize) -> bool) -> lodestone_world::LoadedChunk {
        use lodestone_world::{ChunkColumn, ColumnLight, Heightmaps, LoadedChunk};

        let mut column = ChunkColumn::new(
            0,
            SEAM_SECTIONS,
            PaletteKind::block_states(),
            PaletteKind::biomes(),
            id::AIR,
            0,
        );
        for x in 0..16usize {
            for z in 0..16usize {
                if !water_over(x, z) {
                    continue;
                }
                for y in 0..16i32 {
                    column.set_block(x, y, z, id::WATER);
                }
            }
        }
        LoadedChunk::new(
            column,
            ColumnLight::new(SEAM_SECTIONS),
            Heightmaps::new(),
            Vec::new(),
        )
    }

    /// Which column plays the part of the east neighbour at `(1, 0)`.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum EastNeighbour {
        /// Not in the store at all — the chunk that has not arrived.
        Absent,
        /// Water for `z >= 8`, nothing for `z < 8`. The half-and-half split is
        /// what makes the converged answer a *number* rather than zero: a mesher
        /// that emitted no boundary faces at all would fail the same assertion a
        /// mesher that emitted all of them fails.
        HalfWater,
        /// All air. The **fixture control**: two columns with no shared water
        /// boundary structurally cannot exercise this bug, and this proves the
        /// measurement notices.
        AllAir,
    }

    /// The world for the seam gate: a 3×3 of all-water columns around `(0, 0)`,
    /// with `(1, 0)` replaced per `east`.
    ///
    /// All nine columns are populated (bar the absent case) so that **the east
    /// neighbour is the only variable**. With only the subject and its east
    /// neighbour in the store, seven other columns would be missing and every
    /// measurement would be `Deferred` — the variable would not be under test.
    fn seam_world(east: EastNeighbour) -> World {
        let mut world = World::new();
        for dx in -1..=1i32 {
            for dz in -1..=1i32 {
                if (dx, dz) == (1, 0) {
                    match east {
                        EastNeighbour::Absent => continue,
                        EastNeighbour::HalfWater => {
                            world.load(
                                ChunkPos::new(1, 0),
                                seam_column(&|_x, z| z >= 8),
                            );
                        }
                        EastNeighbour::AllAir => {
                            // A column that exists but holds nothing. `World::load`
                            // still makes it *present*, which is the point: the
                            // deferral is about presence, this control is about
                            // content.
                            world.load(ChunkPos::new(1, 0), seam_column(&|_x, _z| false));
                        }
                    }
                    continue;
                }
                world.load(ChunkPos::new(dx, dz), seam_column(&|_x, _z| true));
            }
        }
        world
    }

    /// The subject: section 0 of column `(0, 0)`.
    fn seam_key() -> SectionKey {
        SectionKey {
            cx: 0,
            cz: 0,
            si: 0,
            min_y: 0,
        }
    }

    /// Quads lying on the section's **east** boundary plane (`x == 16`) — the
    /// faces whose existence is decided by the column at `(1, 0)` — reported as
    /// a count plus the `(z, y)` bounding box they occupy.
    ///
    /// A count alone cannot tell a uniformly-wrong seam from a localised one, and
    /// `CLAUDE.md`'s "measure by location, never by frame average" applies to a
    /// mesh just as much as to a frame. The box is what turns a failure into a
    /// diagnosis.
    fn east_boundary(mesh: &Mesh) -> (usize, String) {
        use lodestone_render::Face;

        let mut count = 0usize;
        let (mut z0, mut z1, mut y0, mut y1) = (u32::MAX, 0u32, u32::MAX, 0u32);
        for quad in mesh.vertices.chunks_exact(4) {
            let fields: Vec<_> = quad.iter().map(|v| v.unpack()).collect();
            if !fields
                .iter()
                .all(|f| f.pos[0] == 16 && f.normal == Face::PosX)
            {
                continue;
            }
            count += 1;
            for f in &fields {
                z0 = z0.min(f.pos[2]);
                z1 = z1.max(f.pos[2]);
                y0 = y0.min(f.pos[1]);
                y1 = y1.max(f.pos[1]);
            }
        }
        let box_ = if count == 0 {
            "none".to_string()
        } else {
            format!("z {z0}..{z1}, y {y0}..{y1}")
        };
        (count, box_)
    }

    /// Mesh the subject section out of `world`, under `columns`, returning the
    /// outcome discriminant, the east-boundary count and its bounding box.
    fn seam_measure(world: &World, columns: ColumnSource) -> (&'static str, usize, String) {
        let outcome =
            snapshot_section_in(world, seam_key(), Some(SEAM_SECTIONS), SkyDefault::Full, columns);
        let label = match &outcome {
            SnapshotOutcome::Ready(_) => "Ready",
            SnapshotOutcome::Empty => "Empty",
            SnapshotOutcome::Deferred(_) => "Deferred",
        };
        let snap = outcome.any().expect("the subject section holds water");
        let (count, box_) = east_boundary(&mesh_snapshot(&snap, &DemoClassifier));
        (label, count, box_)
    }

    /// **Anti-vacuity, and the `CLAUDE.md` *world* species specifically.** The
    /// flaw this gate hunts lives in the input data, so the input data is
    /// asserted: the subject's east-most cells and the neighbour's west-most
    /// cells must both be water over the same `(y, z)` range, or nothing below
    /// can distinguish a fix from a fixture with no seam in it.
    #[test]
    fn the_seam_fixture_really_has_water_on_both_sides() {
        let world = seam_world(EastNeighbour::HalfWater);
        let subject = world
            .section(ChunkPos::new(0, 0), 0)
            .expect("subject section present");
        let east = world
            .section(ChunkPos::new(1, 0), 0)
            .expect("east neighbour present");

        let mut shared = 0usize;
        let mut subject_only = 0usize;
        for y in 0..16usize {
            for z in 0..16usize {
                let a = subject.get_block(15, y, z) == id::WATER;
                let b = east.get_block(0, y, z) == id::WATER;
                assert!(a, "the subject must be water across its whole east face");
                if b {
                    shared += 1;
                } else {
                    subject_only += 1;
                }
            }
        }
        assert_eq!(
            shared, 128,
            "half the seam must be water-against-water (the faces that should be culled)"
        );
        assert_eq!(
            subject_only, 128,
            "the other half must be water-against-air (the faces that should survive)"
        );
    }

    /// **The convergence gate.** Both halves, and the second one is the
    /// load-bearing one.
    ///
    /// 1. Meshing the subject with its east neighbour **absent** emits more
    ///    boundary faces than meshing it once the neighbour has arrived — the
    ///    count *drops*.
    /// 2. And the count after the neighbour arrives **equals** the count from
    ///    meshing with the neighbour present from the start. Without this, a
    ///    mesher that converged on some other wrong answer would pass: "it
    ///    changed" is not "it is right".
    #[test]
    fn a_seam_meshed_without_its_neighbour_converges_on_the_neighbour_present_answer() {
        // Meshed while the east column is still in flight. `ColumnSource::Complete`
        // is used here on purpose: it reproduces the pre-#389 code exactly, which
        // had no other option — this is the *stale* mesh, measured, not described.
        let (stale_label, stale, stale_box) =
            seam_measure(&seam_world(EastNeighbour::Absent), ColumnSource::Complete);
        // The same section re-meshed after the column landed — what
        // `mark_neighbours_dirty` → `heal_dirty_columns` re-drives.
        let (healed_label, healed, healed_box) =
            seam_measure(&seam_world(EastNeighbour::HalfWater), ColumnSource::Streaming);
        // And the same section meshed once, with the neighbour there all along.
        let (fresh_label, fresh, fresh_box) =
            seam_measure(&seam_world(EastNeighbour::HalfWater), ColumnSource::Complete);

        assert_eq!(
            stale_label, "Ready",
            "the pre-fix policy must mesh the incomplete neighbourhood — otherwise this \
             is not the stale case"
        );
        assert_eq!(healed_label, "Ready", "a complete neighbourhood meshes");
        assert_eq!(fresh_label, "Ready", "a complete neighbourhood meshes");

        assert!(
            healed < stale,
            "half 1: the boundary face count must DROP once the neighbour arrives — \
             stale {stale} ({stale_box}) vs healed {healed} ({healed_box})"
        );
        assert_eq!(
            healed, fresh,
            "half 2 (load-bearing): re-meshing after the neighbour arrives must land on \
             exactly the from-the-start answer — healed {healed} ({healed_box}) vs fresh \
             {fresh} ({fresh_box})"
        );
        assert_eq!(
            (stale, healed),
            (256, 128),
            "the fixture's arithmetic: 16×16 boundary faces against air, half of them \
             culled by the neighbour's water — stale box {stale_box}, healed box {healed_box}"
        );
        // The neighbour holds water at `z >= 8`, so *those* seam faces are the
        // ones culled and the survivors are `z < 8` (a quad at cell `z = 7` spans
        // `z 7..8`, hence the exclusive upper bound). Asserting the box and not
        // just the count is what makes "128 faces survived" mean "the right 128":
        // the first version of this assertion had the halves the wrong way round
        // and the printed box is what said so.
        assert_eq!(
            healed_box, "z 0..8, y 0..16",
            "the surviving faces must be exactly the half with no water across the seam; \
             a count that matched with the wrong faces surviving would be a different bug"
        );
    }

    /// **The fix, at the seam that used to bake it silently.** With the world
    /// declared `Streaming`, the same absent neighbour that produced a `Ready`
    /// mesh above produces `Deferred` — and the snapshot says, in the type, how
    /// much of its neighbourhood is a guess.
    #[test]
    fn an_absent_neighbour_column_defers_the_build_and_is_typed_as_unloaded() {
        let absent = snapshot_section_in(
            &seam_world(EastNeighbour::Absent),
            seam_key(),
            Some(SEAM_SECTIONS),
            SkyDefault::Full,
            ColumnSource::Streaming,
        );
        assert!(
            matches!(absent, SnapshotOutcome::Deferred(_)),
            "a missing neighbour column must defer, not mesh"
        );
        assert!(
            absent.ready().is_none(),
            "`ready()` must refuse a deferred snapshot — this is what keeps the wrong \
             geometry off the screen"
        );

        let present = snapshot_section_in(
            &seam_world(EastNeighbour::HalfWater),
            seam_key(),
            Some(SEAM_SECTIONS),
            SkyDefault::Full,
            ColumnSource::Streaming,
        );
        let snap = match present {
            SnapshotOutcome::Ready(snap) => snap,
            other => panic!("a complete neighbourhood must be Ready, got {other:?}"),
        };
        assert_eq!(
            snap.unloaded_neighbours(),
            0,
            "a Ready snapshot must contain no guessed slot"
        );

        // The vertical boundary is *not* a guess: `si == -1` is below the world
        // and section 1 is an elided all-air section inside a column that has
        // arrived. Both are `Neighbour::Air` — air is the truth there — which is
        // why a column at the bottom of the world does not defer forever.
        let deferred = snapshot_section_in(
            &seam_world(EastNeighbour::Absent),
            seam_key(),
            Some(SEAM_SECTIONS),
            SkyDefault::Full,
            ColumnSource::Streaming,
        )
        .any()
        .expect("subject holds water");
        assert_eq!(
            deferred.unloaded_neighbours(),
            3,
            "exactly the three dy slots of the one absent column are guesses — not the \
             six vertical/elided ones, which are genuinely air"
        );
    }

    /// **Control 1, executed: a `Complete` world must NOT defer.** The offline
    /// demo world's outer ring has no neighbours and never will; deferring it
    /// would trade #389's fake seam for a permanent hole. If this ever starts
    /// deferring, `Sim::build`'s demo terrain loses its rim.
    #[test]
    fn control_a_complete_world_never_defers_an_absent_neighbour() {
        let outcome = snapshot_section_in(
            &seam_world(EastNeighbour::Absent),
            seam_key(),
            Some(SEAM_SECTIONS),
            SkyDefault::Full,
            ColumnSource::Complete,
        );
        assert!(
            matches!(outcome, SnapshotOutcome::Ready(_)),
            "a complete world's edge is the edge: air across it is the truth"
        );
        let snap = outcome.ready().expect("Ready");
        assert_eq!(
            snap.unloaded_neighbours(),
            0,
            "nothing in a complete world is 'not loaded yet'"
        );
    }

    /// **Control 2, executed: a fixture with no water across the seam cannot see
    /// this bug at all.** Same three measurements, same code, an east neighbour
    /// that is present but empty — and the count does *not* drop. This is the
    /// `CLAUDE.md` *world* species made to fire: had the real fixture been built
    /// this way, every assertion above would have passed with the fix reverted.
    #[test]
    fn control_a_seamless_fixture_shows_no_convergence() {
        let (_, stale, stale_box) =
            seam_measure(&seam_world(EastNeighbour::Absent), ColumnSource::Complete);
        let (_, healed, healed_box) =
            seam_measure(&seam_world(EastNeighbour::AllAir), ColumnSource::Streaming);
        assert_eq!(
            (stale, healed),
            (256, 256),
            "control: an all-air neighbour culls nothing, so arrival changes nothing — \
             stale {stale_box}, healed {healed_box}. A gate built on this fixture would be \
             blind to #389."
        );
    }

    #[test]
    fn split16_maps_signed_probes_to_neighbour_and_local() {
        // In-section coordinates stay in the centre neighbour (offset 0).
        assert_eq!(split16(0), (0, 0));
        assert_eq!(split16(15), (0, 15));
        // One block below/west of the section wraps into the -1 neighbour at
        // local index 15 — exactly the cullface probe across a section edge.
        assert_eq!(split16(-1), (-1, 15));
        // One block past the far edge wraps into the +1 neighbour at local 0.
        assert_eq!(split16(16), (1, 0));
        assert_eq!(split16(17), (1, 1));
    }

    #[test]
    fn snapshot_and_mesh_a_ground_section() {
        let world = crate::worldgen::generate(0);
        // Section 2 straddles sea level / surface, so it has terrain.
        let key = SectionKey {
            cx: 0,
            cz: 0,
            si: 2,
            min_y: crate::worldgen::MIN_Y,
        };
        let snap = snapshot_section(&world, key).expect("centre section has geometry");
        let mesh = mesh_snapshot(&snap, &DemoClassifier);
        assert!(mesh.quad_count() > 0, "ground section should emit faces");
    }

    #[test]
    fn empty_sky_section_is_skipped() {
        let world = crate::worldgen::generate(0);
        // Pick the first section that starts strictly above the generated
        // surface at the origin, so it is guaranteed sky. Deriving the index
        // from `surface_height` keeps this honest as the terrain generator
        // changes underneath us (real vanilla terrain lifted the origin surface
        // to ~y71, which used to be hard-coded sky).
        let surface = crate::worldgen::surface_height(0, 0);
        let si = ((surface - crate::worldgen::MIN_Y) / 16 + 1) as usize;
        assert!(
            si < crate::worldgen::SECTION_COUNT,
            "surface {surface} leaves no sky section in the window"
        );
        let key = SectionKey {
            cx: 0,
            cz: 0,
            si,
            min_y: crate::worldgen::MIN_Y,
        };
        assert!(
            snapshot_section(&world, key).is_none(),
            "all-air section produces no snapshot"
        );
    }

    #[test]
    fn scheduler_meshes_many_sections() {
        let world = crate::worldgen::generate(1);
        let mut scheduler = MeshScheduler::new(3, ShellClassifier::Demo(DemoClassifier));
        let mut submitted = 0;
        for cz in -1..=1 {
            for cx in -1..=1 {
                for si in 0..crate::worldgen::SECTION_COUNT {
                    let key = SectionKey {
                        cx,
                        cz,
                        si,
                        min_y: crate::worldgen::MIN_Y,
                    };
                    if let Some(snap) = snapshot_section(&world, key) {
                        scheduler.submit(snap);
                        submitted += 1;
                    }
                }
            }
        }
        assert!(submitted > 0, "should have scheduled some sections");
        let results = scheduler.drain_blocking(submitted);
        assert_eq!(results.len(), submitted, "every job returns a mesh");
        assert!(
            results.iter().any(|m| m.mesh.quad_count() > 0),
            "at least one section has geometry"
        );
    }

    /// A hand-built snapshot: a full stone floor in the centre section with air
    /// above and around it, so every exposed face samples a neighbouring air
    /// cell for its light. `sky` is the uniform sky level fed to the whole
    /// neighbourhood; `lights_present` toggles between the real light field and
    /// the absent-neighbour bridge (all `None`).
    fn floor_snapshot(sky: u8, lights_present: bool) -> SectionSnapshot {
        use lodestone_world::LightData;

        let mut sections = Vec::with_capacity(27);
        let mut lights = Vec::with_capacity(27);
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    let mut sec = air_section();
                    if dx == 0 && dy == 0 && dz == 0 {
                        for x in 0..16 {
                            for z in 0..16 {
                                sec.set_block(x, 0, z, id::STONE);
                            }
                        }
                    }
                    sections.push(Neighbour::Present(Arc::new(sec)));
                    lights.push(if lights_present {
                        Some(SectionLightData {
                            sky: LightData::Uniform(sky),
                            block: LightData::Uniform(0),
                        })
                    } else {
                        None
                    });
                }
            }
        }
        SectionSnapshot {
            key: SectionKey {
                cx: 0,
                cz: 0,
                si: 1,
                min_y: 0,
            },
            sections,
            lights,
            sky_default: SkyDefault::Full,
            biome_names: Arc::from([]),
        }
    }

    fn max_vertex_sky(mesh: &Mesh) -> u8 {
        mesh.vertices
            .iter()
            .map(|v| v.unpack().sky_light)
            .max()
            .unwrap_or(0)
    }

    /// The load-bearing lighting proof: a shadowed neighbourhood (stored sky
    /// `0`) must mesh **measurably darker** than an open-sky one (sky `15`), and
    /// the retired full-bright bridge must be **unable to tell them apart** — the
    /// exact assertion the old `UniformLight::default()` path fails.
    ///
    /// "It still draws" proves nothing here: full-bright and correct lighting
    /// both emit the same geometry. This asserts on the *vertex light bytes*, so
    /// it fails if the mesher ever silently reverts to a constant light field.
    #[test]
    fn shadowed_meshes_darker_than_open_sky_and_the_bridge_cannot_tell() {
        let open = mesh_snapshot(&floor_snapshot(15, true), &DemoClassifier);
        let shadow = mesh_snapshot(&floor_snapshot(0, true), &DemoClassifier);

        let open_sky = max_vertex_sky(&open);
        let shadow_sky = max_vertex_sky(&shadow);

        assert!(open.quad_count() > 0 && shadow.quad_count() > 0, "geometry");
        assert!(
            shadow_sky < open_sky,
            "shadowed sky light ({shadow_sky}) must be darker than open sky ({open_sky}); \
             a constant/full-bright light field would make these equal"
        );
        assert_eq!(open_sky, 255, "open sky should reach full brightness");
        assert_eq!(shadow_sky, 0, "stored sky 0 must stay dark, not default up");

        // Control: with the absent-neighbour bridge (lights all `None`) the mesher
        // falls back to full-bright, so the SAME two inputs become
        // indistinguishable — this is precisely the assertion the pre-light-bridge
        // path fails, demonstrating the swap is what put real light on screen.
        let bridge_open = mesh_snapshot(&floor_snapshot(15, false), &DemoClassifier);
        let bridge_shadow = mesh_snapshot(&floor_snapshot(0, false), &DemoClassifier);
        assert_eq!(
            max_vertex_sky(&bridge_open),
            max_vertex_sky(&bridge_shadow),
            "the full-bright bridge cannot distinguish shadow from open sky"
        );
        assert_eq!(
            max_vertex_sky(&bridge_shadow),
            255,
            "the bridge renders everything full-bright"
        );
    }

    /// End-to-end producer check: worldgen now computes real column light, so a
    /// snapshot pulled from the generated world carries a **non-uniform** sky
    /// field — a cave/underground cell is darker than an exposed one. Guards
    /// against a regression where generation reverts to all-`Missing` light,
    /// which would render the whole world flat full-bright again.
    #[test]
    fn generated_world_snapshot_has_real_light_gradient() {
        let world = crate::worldgen::generate(1);
        // Walk sections at the origin column from the bottom up; the first that
        // meshes holds terrain with sky above and rock below — a genuine
        // gradient across its faces.
        let mut saw_dark = false;
        let mut saw_bright = false;
        for si in 0..crate::worldgen::SECTION_COUNT {
            let key = SectionKey {
                cx: 0,
                cz: 0,
                si,
                min_y: crate::worldgen::MIN_Y,
            };
            if let Some(snap) = snapshot_section(&world, key) {
                let mesh = mesh_snapshot(&snap, &DemoClassifier);
                for v in &mesh.vertices {
                    let s = v.unpack().sky_light;
                    if s == 0 {
                        saw_dark = true;
                    }
                    if s > 200 {
                        saw_bright = true;
                    }
                }
            }
        }
        assert!(
            saw_dark && saw_bright,
            "generated terrain should mesh both fully-shadowed (0) and sky-lit \
             (>200) faces; saw_dark={saw_dark} saw_bright={saw_bright} — an \
             all-Missing (flat full-bright) world would have no dark faces"
        );
    }

    // -----------------------------------------------------------------------
    // The model path's face-light rule, and the placement defect it caused
    // -----------------------------------------------------------------------

    /// A unit cube's six baked quads: one per direction, each culled by its own
    /// facing, positioned on that face of the block. Enough geometry to carry a
    /// light byte through [`mesh_models`] and be identified again by centroid.
    fn cube_quads() -> Vec<BakedQuad> {
        const DIRS: [Direction; 6] = [
            Direction::Down,
            Direction::Up,
            Direction::North,
            Direction::South,
            Direction::West,
            Direction::East,
        ];
        DIRS.iter()
            .map(|&d| {
                let n = face_of_direction(d).normal();
                // The face plane: the fixed axis sits at 0 or 1, the other two
                // sweep the unit square.
                let (axis, plane) = match d {
                    Direction::West => (0usize, 0.0f32),
                    Direction::East => (0, 1.0),
                    Direction::Down => (1, 0.0),
                    Direction::Up => (1, 1.0),
                    Direction::North => (2, 0.0),
                    Direction::South => (2, 1.0),
                };
                let (a, b) = match axis {
                    0 => (1usize, 2usize),
                    1 => (0, 2),
                    _ => (0, 1),
                };
                let corners = [[0.0f32, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]];
                let mut positions = [[0.0f32; 3]; 4];
                for (i, c) in corners.iter().enumerate() {
                    positions[i][axis] = plane;
                    positions[i][a] = c[0];
                    positions[i][b] = c[1];
                }
                let _ = n;
                BakedQuad {
                    positions,
                    uvs: [[0.0, 0.0]; 4],
                    direction: d,
                    cullface: Some(d),
                    tint_index: None,
                    shade: true,
                    layer: 0,
                    anim: 0,
                }
            })
            .collect()
    }

    /// Which light rule a [`ProbeView`] applies — the shipped face rule, or the
    /// **pre-fix** own-cell rule kept verbatim as the negative control.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum LightRule {
        /// `SnapshotModelView`'s rule: the cell the face opens into.
        FaceNeighbour,
        /// The rule this test exists to retire: `(sky << 4) | block` read at the
        /// block's **own** cell, which is `0` inside every opaque block.
        OwnCell,
    }

    /// A [`ModelSectionView`] over a snapshot that emits a full cube for every
    /// non-air cell and resolves light through the real [`SnapshotLight`] — so
    /// the assertions below run the shipped resolver, not a copy of it.
    struct ProbeView<'a> {
        snapshot: &'a SectionSnapshot,
        light: SnapshotLight<'a>,
        quads: Vec<BakedQuad>,
        empty: Vec<BakedQuad>,
        rule: LightRule,
    }

    impl ModelSectionView for ProbeView<'_> {
        fn quads_at(&self, x: usize, y: usize, z: usize) -> &[BakedQuad] {
            if self.snapshot.at(0, 0, 0).get_block(x, y, z) == id::AIR {
                &self.empty
            } else {
                &self.quads
            }
        }

        fn occludes_at(&self, x: i32, y: i32, z: i32) -> bool {
            let (dx, lx) = split16(x);
            let (dy, ly) = split16(y);
            let (dz, lz) = split16(z);
            if !(-1..=1).contains(&dx) || !(-1..=1).contains(&dy) || !(-1..=1).contains(&dz) {
                return false;
            }
            self.snapshot.at(dx, dy, dz).get_block(lx, ly, lz) != id::AIR
        }

        fn light_at(&self, x: usize, y: usize, z: usize) -> u8 {
            self.light.max_light(x, y, z)
        }

        fn face_light_at(&self, x: usize, y: usize, z: usize, dir: Direction) -> u8 {
            match self.rule {
                LightRule::FaceNeighbour => self
                    .light
                    .face_light(x, y, z, face_of_direction(dir).normal()),
                LightRule::OwnCell => {
                    let (sky, block) = self.light.levels_at(x as i32, y as i32, z as i32);
                    (sky << 4) | block
                }
            }
        }

        /// **Do not delete this, and do not let it drift from
        /// [`SnapshotModelView::corner_light_at`].**
        ///
        /// `mesh_models` grew per-vertex smooth lighting in `1b8e46b`, which added
        /// a *fourth* light hook to [`ModelSectionView`] —
        /// [`ModelSectionView::corner_light_at`], sampling the two edge-adjacent
        /// cells and the diagonal around each quad corner. Its trait default is
        /// **full-bright `0xF0`**, for views that model no neighbourhood at all
        /// (GUI items). The shipped view implements it; this probe did not, so
        /// every AO corner read `0xF0` and each measurement below came out as
        /// `round((centre + 15 + 15 + 15) / 4)` — 11 where 0 was expected, which is
        /// exactly the `176` these three tests reported for four commits.
        ///
        /// That is the **world** species of vacuous test from `CLAUDE.md`: the
        /// assertions were exemplary and the fixture had stopped containing the
        /// structure the code under test needs. A `ProbeView` that omits a light
        /// hook does not measure the shipped resolver, it measures a trait default.
        fn corner_light_at(&self, x: i32, y: i32, z: i32) -> u8 {
            let (sky, block) = self.light.levels_at(x, y, z);
            (sky << 4) | block
        }
    }

    /// The packed light byte carried by the quad of block `b` facing `dir`,
    /// located by its centroid in the emitted mesh. `None` when that face was
    /// culled (so "the face is missing" can never read as "the face is dark").
    fn quad_light(mesh: &ModelMesh, b: [usize; 3], dir: Direction) -> Option<u8> {
        let n = face_of_direction(dir).normal();
        let want = [
            b[0] as f32 + 0.5 + 0.5 * n[0] as f32,
            b[1] as f32 + 0.5 + 0.5 * n[1] as f32,
            b[2] as f32 + 0.5 + 0.5 * n[2] as f32,
        ];
        for quad in mesh.vertices.chunks_exact(4) {
            let mut c = [0.0f32; 3];
            for v in quad {
                for a in 0..3 {
                    c[a] += v.position[a] / 4.0;
                }
            }
            if (0..3).all(|a| (c[a] - want[a]).abs() < 1e-4) {
                let light = quad[0].light;
                assert!(
                    quad.iter().all(|v| v.light == light),
                    "a flat-lit quad must carry one light on all four vertices"
                );
                return Some(light);
            }
        }
        None
    }

    /// The fixture: a one-block-thick stone platform at `y = 6` with a dark cave
    /// beneath it, a stone roof over the `x < 8, z < 8` quadrant at `y = 12`,
    /// and open sky everywhere else.
    ///
    /// Three *different* light populations, which is what stops this gate being
    /// the "fully sunlit flat world" species of vacuous test:
    ///
    /// | region | sky | block |
    /// |---|---|---|
    /// | open air (`y >= 7`, outside the roofed quadrant) | 15 | 0 |
    /// | roofed pocket (`y 7..=11`, `x < 8 && z < 8`) | 0 | 11 |
    /// | cave under the platform (`y <= 5`) | 0 | 0 |
    /// | any solid cell | 0 | 0 |
    ///
    /// `placed` optionally turns one air cell to stone **without touching the
    /// light field** — precisely the optimistic-placement window, where the
    /// block is known and the server's relight has not arrived.
    fn platform_snapshot(placed: Option<[usize; 3]>) -> SectionSnapshot {
        use lodestone_world::{LightData, NibbleArray};

        let roofed = |x: usize, z: usize| x < 8 && z < 8;
        let solid = |x: usize, y: usize, z: usize| y == 6 || (y == 12 && roofed(x, z));

        let mut centre = air_section();
        for x in 0..16 {
            for y in 0..16 {
                for z in 0..16 {
                    if solid(x, y, z) {
                        centre.set_block(x, y, z, id::STONE);
                    }
                }
            }
        }
        // The light field describes the world *before* the placement.
        let mut sky = LightData::Uniform(0);
        let mut block = LightData::Uniform(0);
        for x in 0..16 {
            for y in 0..16 {
                for z in 0..16 {
                    if solid(x, y, z) || y <= 5 {
                        continue;
                    }
                    let i = NibbleArray::index(x, y, z);
                    if roofed(x, z) && y <= 11 {
                        block.set(i, 11);
                    } else {
                        sky.set(i, 15);
                    }
                }
            }
        }
        if let Some([x, y, z]) = placed {
            assert_eq!(
                centre.get_block(x, y, z),
                id::AIR,
                "the fixture must place into an air cell"
            );
            centre.set_block(x, y, z, id::STONE);
        }

        let light = SectionLightData { sky, block };
        let mut sections = Vec::with_capacity(27);
        let mut lights = Vec::with_capacity(27);
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    sections.push(if (dx, dy, dz) == (0, 0, 0) {
                        Neighbour::Present(Arc::new(centre.clone()))
                    } else {
                        // `Air`, not `Unloaded`: this fixture's neighbourhood is
                        // deliberately empty, not deliberately unknown.
                        Neighbour::Air
                    });
                    // Every slot carries real light, so the absent-neighbour
                    // bridge cannot leak full-bright into this measurement.
                    lights.push(Some(light.clone()));
                }
            }
        }
        SectionSnapshot {
            key: SectionKey {
                cx: 0,
                cz: 0,
                si: 1,
                min_y: 0,
            },
            sections,
            lights,
            sky_default: SkyDefault::Full,
            biome_names: Arc::from([]),
        }
    }

    fn probe(snapshot: &SectionSnapshot, rule: LightRule) -> ModelMesh {
        let view = ProbeView {
            snapshot,
            light: SnapshotLight::new(snapshot),
            quads: cube_quads(),
            empty: Vec::new(),
            rule,
        };
        mesh_models(&view)
    }

    /// Anti-vacuity: the fixture really does hold a lit/shadowed distinction, and
    /// the shipped rule resolves *the same block's* two faces differently.
    ///
    /// Without this, every assertion below could be satisfied by a constant.
    #[test]
    fn face_light_distinguishes_sunlit_shadowed_and_torchlit_faces() {
        let snap = platform_snapshot(None);
        let mesh = probe(&snap, LightRule::FaceNeighbour);

        // The platform block under open sky: bright on top (opens into sky-15
        // air), dark underneath (opens into the unlit cave). One block, two
        // values — a constant light field cannot produce this.
        let open_top = quad_light(&mesh, [12, 6, 12], Direction::Up).expect("open top face");
        let open_bottom = quad_light(&mesh, [12, 6, 12], Direction::Down).expect("open bottom");
        assert_eq!(open_top, 0xF0, "a sunlit top face carries sky 15");
        assert_eq!(open_bottom, 0x00, "the cave-side face carries no light");

        // The platform under the roof: its top opens into the torchlit pocket,
        // so it is neither 15 nor 0 — a third, independently sourced population.
        let roofed_top = quad_light(&mesh, [2, 6, 2], Direction::Up).expect("roofed top face");
        assert_eq!(
            roofed_top, 0x0B,
            "a roofed top face carries the pocket's block light 11 and sky 0"
        );
    }

    /// **The defect.** A block placed into open sky must mesh with the same light
    /// as the terrain beside it. Before the fix it did not: the model path lit
    /// every block from its own cell, which the light engine stores as `0` for a
    /// solid, while the just-placed block's cell still held the sky-15 of the air
    /// it replaced. The new block rendered at the shader's maximum against
    /// neighbours at its minimum — the player-reported "super bright".
    ///
    /// Asserted as a *relationship* (placed == its neighbours), not an absolute,
    /// so it cannot be satisfied by clamping everything to one value: the
    /// shadowed half of the same fixture is checked in the same test.
    #[test]
    fn a_placed_block_meshes_with_its_neighbours_light_not_full_bright() {
        // Before: bare platform. After: one stone dropped on top of it, with the
        // pre-placement light field still in force (the optimistic window).
        let before = platform_snapshot(None);
        let after = platform_snapshot(Some([12, 7, 12]));

        let neighbour_before = quad_light(
            &probe(&before, LightRule::FaceNeighbour),
            [11, 6, 12],
            Direction::Up,
        )
        .expect("neighbouring platform top");

        let after_mesh = probe(&after, LightRule::FaceNeighbour);
        let placed_top =
            quad_light(&after_mesh, [12, 7, 12], Direction::Up).expect("placed block top");
        let placed_side =
            quad_light(&after_mesh, [12, 7, 12], Direction::East).expect("placed block side");
        let neighbour_after = quad_light(&after_mesh, [11, 6, 12], Direction::Up)
            .expect("neighbouring platform top, after");

        assert_eq!(
            neighbour_before, neighbour_after,
            "placing a block must not change the light of the terrain beside it"
        );
        assert_eq!(
            placed_top, neighbour_after,
            "the placed block's top must match the sunlit terrain beside it \
             ({placed_top:#04x} vs {neighbour_after:#04x})"
        );
        assert_eq!(
            placed_side, neighbour_after,
            "the placed block's side must match too ({placed_side:#04x})"
        );

        // And the same placement in shadow must land *dark*, so "matches its
        // neighbours" cannot be met by returning full-bright everywhere.
        let shadowed = platform_snapshot(Some([2, 7, 2]));
        let shadow_mesh = probe(&shadowed, LightRule::FaceNeighbour);
        let shadow_top =
            quad_light(&shadow_mesh, [2, 7, 2], Direction::Up).expect("shadowed placed top");
        assert_eq!(
            shadow_top, 0x0B,
            "a block placed in the torchlit, roofed pocket takes the pocket's \
             light (sky 0, block 11) — not sky 15"
        );
        assert!(
            shadow_top < placed_top,
            "the shadowed placement must be measurably darker than the sunlit one"
        );
    }

    /// The negative control, run rather than described: with the pre-fix
    /// own-cell rule restored **and nothing else changed**, the same placement
    /// renders brighter than the terrain it sits on. If this ever stops failing
    /// the way it does here, the assertion above has gone vacuous.
    ///
    /// # Smooth lighting diluted this control, and the numbers say by how much
    ///
    /// When this test was written `mesh_models` was flat-lit, so an own-cell face
    /// carried its cell's stored light outright: a solid cell reads `0`, and the
    /// defect was a 15-vs-0 contrast. `1b8e46b` added per-vertex smooth lighting,
    /// which averages the face cell with three corner neighbours — so the centre
    /// contributes only **a quarter** of the result and an opaque cell's `0` is
    /// pulled up by whatever surrounds it. Measured here:
    ///
    /// | face | own-cell rule | shipped rule |
    /// |---|---|---|
    /// | just-placed block, open sky | `0xF0` | `0xF0` |
    /// | established terrain beside it | `0xB0` (`round(45/4) = 11`) | `0xF0` |
    /// | sunlit platform top, no placement | `0xB0` | `0xF0` |
    /// | roofed platform top | `0x08` (`round(33/4) = 8`) | `0x0B` |
    ///
    /// So the defect is still there and still visible — a seam between a placed
    /// block and its neighbours — but it is sky 11 vs 15, not 0 vs 15. Two claims
    /// this control used to make are now simply **false** and are replaced rather
    /// than patched: "own-cell reads 0 inside every opaque block" (it reads the
    /// smoothed average) and "own-cell cannot tell a sunlit face from a roofed
    /// one" (the corner samples leak that distinction back in). What is asserted
    /// instead is the *relationship between the two rules* at the same faces,
    /// which cannot be satisfied by a constant and cannot be satisfied by the
    /// shipped rule.
    #[test]
    fn control_own_cell_light_makes_the_placed_block_brighter_than_its_neighbours() {
        let after = platform_snapshot(Some([12, 7, 12]));
        let mesh = probe(&after, LightRule::OwnCell);

        let placed_top = quad_light(&mesh, [12, 7, 12], Direction::Up).expect("placed block top");
        let neighbour = quad_light(&mesh, [11, 6, 12], Direction::Up).expect("neighbour top");

        assert_eq!(
            placed_top, 0xF0,
            "control: own-cell sampling reads the stale air light of the cell the \
             block replaced — full bright"
        );
        assert_eq!(
            neighbour, 0xB0,
            "control: own-cell sampling reads 0 inside the opaque neighbour, \
             smoothed against its three sky-15 corners"
        );
        assert!(
            placed_top > neighbour,
            "control must reproduce the reported defect: a just-placed block \
             brighter than the terrain it sits on"
        );

        // The whole world, not just the placement: under the old rule *every*
        // solid face is darker than it should be, sunlit and roofed alike, because
        // its own unlit cell is a quarter of every corner average. Measured on the
        // bare platform, where the sunlit top face is not covered by the placement,
        // and compared against the shipped rule at the same two faces — a
        // rule-versus-rule assertion the shipped rule cannot satisfy.
        let bare_own = probe(&platform_snapshot(None), LightRule::OwnCell);
        let bare_face = probe(&platform_snapshot(None), LightRule::FaceNeighbour);
        for (block, label) in [([12usize, 6, 12], "sunlit"), ([2, 6, 2], "roofed")] {
            let own = quad_light(&bare_own, block, Direction::Up)
                .expect("control: the platform top face must exist");
            let shipped = quad_light(&bare_face, block, Direction::Up)
                .expect("the platform top face must exist");
            assert!(
                own < shipped,
                "control: the {label} top face must read darker under own-cell \
                 sampling ({own:#04x}) than under the shipped face-neighbour rule \
                 ({shipped:#04x})"
            );
        }
        assert_eq!(
            quad_light(&bare_own, [12, 6, 12], Direction::Up),
            Some(0xB0),
            "control: sunlit top face, own cell 0 smoothed against three sky-15 corners"
        );
        assert_eq!(
            quad_light(&bare_own, [2, 6, 2], Direction::Up),
            Some(0x08),
            "control: roofed top face, own cell 0 smoothed against three block-11 corners"
        );
    }

    // -----------------------------------------------------------------------
    // Issue #479: the walk harness
    // -----------------------------------------------------------------------

    /// Chebyshev radius of the simulated tracking view, in columns. Small on
    /// purpose — the defect is about *unbounded growth*, which a short walk
    /// already separates from a bounded window by a factor of two.
    const WALK_RD: i32 = 3;
    /// How many columns the simulated player advances in `+x`.
    const WALK_STEPS: i32 = 12;

    /// The columns a tracking view centred on `(ccx, 0)` holds.
    fn window(ccx: i32) -> BTreeSet<(i32, i32)> {
        let mut out = BTreeSet::new();
        for cx in (ccx - WALK_RD)..=(ccx + WALK_RD) {
            for cz in -WALK_RD..=WALK_RD {
                out.insert((cx, cz));
            }
        }
        out
    }

    /// Every column that at some point during the walk had all eight of its
    /// horizontal neighbours resident at once — i.e. exactly the columns that can
    /// ever have escaped [`SnapshotOutcome::Deferred`] and reached the GPU.
    ///
    /// Derived from the walk's geometry rather than measured, so it is an
    /// *outside* expectation: the two frontier columns in `x` (the first view's
    /// trailing ring, which is unloaded before the view ever advances past it,
    /// and the last view's leading ring) never qualify, and neither do the
    /// `z = ±WALK_RD` rows, since the view never moves in `z`. At the constants
    /// above that is 17 × 5 = 85 columns out of 133 visited.
    fn ever_interior() -> BTreeSet<(i32, i32)> {
        let mut out = BTreeSet::new();
        for cx in (-WALK_RD + 1)..=(WALK_STEPS + WALK_RD - 1) {
            for cz in (-WALK_RD + 1)..=(WALK_RD - 1) {
                out.insert((cx, cz));
            }
        }
        out
    }

    /// A `TerrainMesh` whose deferral rule is the **live** one.
    ///
    /// `ColumnSource` is derived from the classifier, and the demo classifier
    /// yields `Complete` — under which nothing ever defers and the frontier
    /// behaviour this harness exists to model does not exist. Overriding the
    /// field is how a hermetic test reaches the `Streaming` rule without the
    /// vanilla atlas; it is the one production fact the demo classifier cannot
    /// supply. The eviction path under test (`forget_column` →
    /// `pending_removals` → `drain_removals`) is id-space agnostic.
    fn streaming_terrain() -> TerrainMesh {
        let mut terrain =
            TerrainMesh::new(MeshScheduler::new(2, ShellClassifier::Demo(DemoClassifier)));
        terrain.column_source = ColumnSource::Streaming;
        terrain
    }

    /// Walk `+x` across `WALK_STEPS` columns behind a moving tracking view,
    /// returning the columns still resident on the (modelled) GPU at the end.
    ///
    /// `evict` is the switch this test and its control share: `true` is the
    /// fixed client, `false` reproduces the pre-#479 client exactly — the
    /// arrival half wired and the unload half absent. Everything else is
    /// identical, so the two runs differ only in the thing under test.
    fn walk(evict: bool) -> (BTreeSet<(i32, i32)>, BTreeSet<(i32, i32)>) {
        let store = ChunkWorld::new(World::new());
        let mut terrain = streaming_terrain();
        // The modelled GPU: what `RenderState`'s section map would hold, driven
        // by the same two drains `app/redraw.rs` calls, in the same order.
        let mut gpu: HashSet<SectionKey> = HashSet::new();
        let mut visited: BTreeSet<(i32, i32)> = BTreeSet::new();

        let mut live = BTreeSet::new();
        for step in 0..=WALK_STEPS {
            let next = window(step);
            // Arrivals: the adapter writes the column, then the shell is told.
            for &(cx, cz) in next.difference(&live) {
                store
                    .write()
                    .load(ChunkPos::new(cx, cz), crate::worldgen::generate_column(cx, cz));
                visited.insert((cx, cz));
            }
            for &(cx, cz) in next.difference(&live) {
                terrain.mesh_column(&store, cx, cz);
                terrain.mark_neighbours_dirty(&store, cx, cz);
            }
            // Departures: the adapter unloads the column *before* it emits, so
            // the store has already lost it when `forget_column` runs. Modelling
            // that order is the point — a `forget_column` that tried to read the
            // store would enumerate nothing.
            for &(cx, cz) in live.difference(&next) {
                store.write().unload(ChunkPos::new(cx, cz));
                if evict {
                    terrain.forget_column(cx, cz);
                    terrain.force_neighbours_of_departed(&store, cx, cz);
                }
            }
            live = next;

            // Drain the heal queue completely rather than at
            // `DIRTY_COLUMN_BUDGET`: the subject is eviction, and leaving a
            // backlog would let a *starved* run masquerade as a bounded one.
            while let Some((cx, cz)) = terrain.dirty_columns.pop_first() {
                terrain.mesh_column(&store, cx, cz);
            }
            for key in terrain.drain_removals() {
                gpu.remove(&key);
            }
            for meshed in terrain.drain_all_meshes() {
                gpu.insert(meshed.key);
            }
        }

        let resident: BTreeSet<(i32, i32)> = gpu.iter().map(|k| (k.cx, k.cz)).collect();
        (resident, visited)
    }

    /// **The invariant gate.** Nothing may stay on the GPU for a column the
    /// client no longer has.
    ///
    /// Stated as a *predicted magnitude*, not a direction: the two hypotheses
    /// are computed from the walk's own geometry and the measurement has to land
    /// on one of them. Bounded (correct) is at most `window()`'s 49 columns;
    /// unbounded (pre-#479) is every column the walk ever visited, 126 at these
    /// constants. "Fewer than it visited" would be satisfied by both, so it is
    /// not what this asserts.
    #[test]
    fn walking_away_evicts_meshes_for_columns_the_client_dropped() {
        let (resident, visited) = walk(true);
        let live = window(WALK_STEPS);

        // Not vacuous: the walk has to have drawn something, and to have moved
        // far enough that the two hypotheses are actually distinguishable.
        assert!(
            visited.len() >= live.len() * 2,
            "harness must outrun its own window for the bound to mean anything: \
             visited {} vs window {}",
            visited.len(),
            live.len()
        );
        // Both hypotheses, computed from outside the code under test, so the
        // measurement has to land on one of them. Correct: the columns that ever
        // escaped deferral *and* are still in view. Pre-#479: every column that
        // ever escaped deferral, in view or not.
        let correct: BTreeSet<(i32, i32)> =
            ever_interior().intersection(&live).copied().collect();
        let leaked = ever_interior();
        assert!(
            correct.len() * 2 < leaked.len(),
            "the two hypotheses must be far apart or this asserts nothing: \
             {} vs {}",
            correct.len(),
            leaked.len()
        );
        assert!(
            !correct.is_empty(),
            "the interior of the final view must be drawn, else this gate would \
             pass on a client that renders nothing at all"
        );

        let stale: Vec<(i32, i32)> = resident.difference(&live).copied().collect();
        assert!(
            stale.is_empty(),
            "{} column(s) still hold GPU geometry after the client dropped them; \
             x range {:?}..={:?} — the renderer is drawing blocks the store no \
             longer has, and its section-origin arena never gets those slots back. \
             resident={} live-window={} ever-visited={}",
            stale.len(),
            stale.iter().map(|c| c.0).min(),
            stale.iter().map(|c| c.0).max(),
            resident.len(),
            live.len(),
            visited.len()
        );
        assert_eq!(
            resident, correct,
            "residency must be exactly the in-view columns that escaped deferral \
             ({} of them); the leak hypothesis is {} columns",
            correct.len(),
            leaked.len()
        );
    }

    /// **The control for the gate above, and it must fail that gate's
    /// assertion.** Without the eviction call the identical walk leaves every
    /// column it ever visited resident — so the assertion is answering the
    /// question it claims to, rather than passing because the harness never
    /// evicts anything or never draws at all.
    #[test]
    fn control_without_eviction_the_walk_leaks_every_column_it_visited() {
        let (resident, visited) = walk(false);
        let live = window(WALK_STEPS);

        let stale: Vec<(i32, i32)> = resident.difference(&live).copied().collect();
        assert!(
            !stale.is_empty(),
            "premise check: with eviction suppressed the walk MUST leak, or the \
             gate above proves nothing — resident {} vs window {}",
            resident.len(),
            live.len()
        );
        // The leak is unbounded, not merely present: residency tracks the whole
        // walk rather than the view. This is the number that turns into an
        // exhausted origin arena and a silently dropped section.
        assert!(
            resident.len() > live.len(),
            "the pre-#479 leak grows past the view: resident {} vs window {}",
            resident.len(),
            live.len()
        );
        // And it leaks by exactly the predicted amount: residency tracks the
        // *walk*, not the view. 85 columns at these constants, against a 49-column
        // view — after twelve steps. This is the growth that ends at an exhausted
        // section-origin arena, whose failure mode is `upload_section` returning
        // early and new terrain never drawing.
        assert_eq!(
            resident,
            ever_interior(),
            "the pre-#479 leak is every column that ever escaped deferral: \
             resident {} vs view {} vs visited {}",
            resident.len(),
            live.len(),
            visited.len()
        );
    }

    /// **Is the mesh *scheduling* path lossy, or only slow?** Walks at the real
    /// [`DIRTY_COLUMN_BUDGET`] instead of draining the heal queue, then stands
    /// still.
    ///
    /// This is the discriminator between the two explanations that look identical
    /// from outside the process — a queue that never drains, and workers that are
    /// merely starved. It answers it with **counts of frames and columns**, never
    /// a duration: a millisecond figure taken on a shared machine gets attributed
    /// to the wrong cause, and this repo's record has a 585× instance of exactly
    /// that.
    ///
    /// The isolation is deliberate and is what makes the result mean something:
    /// mesh workers are drained to completion every frame, i.e. modelled as
    /// **infinitely fast**. So worker throughput cannot influence the outcome, and
    /// what is left under test is purely the enqueue/defer/heal *scheduling*. A
    /// pass therefore says something quite specific: **no column is lost or
    /// permanently deferred, so the mesh path is not lossy and any real-world
    /// shortfall is throughput or latency** — which is a worldgen/CPU question,
    /// not a client scheduling bug. A failure would have said the opposite.
    #[test]
    fn standing_still_drains_the_heal_backlog_and_no_column_is_lost() {
        // One frame per step is deliberately the *worst* case the shell can
        // present: a column arrives and the heal system gets a single 4-column
        // budget before the player has moved again. If the backlog is going to
        // diverge, it diverges here.
        const FRAMES_PER_STEP: usize = 1;

        let store = ChunkWorld::new(World::new());
        let mut terrain = streaming_terrain();
        let mut gpu: HashSet<SectionKey> = HashSet::new();
        let mut max_backlog = 0usize;

        let mut live = BTreeSet::new();
        for step in 0..=WALK_STEPS {
            let next = window(step);
            for &(cx, cz) in next.difference(&live) {
                store
                    .write()
                    .load(ChunkPos::new(cx, cz), crate::worldgen::generate_column(cx, cz));
            }
            for &(cx, cz) in next.difference(&live) {
                terrain.mesh_column(&store, cx, cz);
                terrain.mark_neighbours_dirty(&store, cx, cz);
            }
            for &(cx, cz) in live.difference(&next) {
                store.write().unload(ChunkPos::new(cx, cz));
                terrain.forget_column(cx, cz);
                terrain.force_neighbours_of_departed(&store, cx, cz);
            }
            live = next;

            for _ in 0..FRAMES_PER_STEP {
                // `heal_dirty_columns`, by hand at its real budget.
                for _ in 0..DIRTY_COLUMN_BUDGET {
                    let Some((cx, cz)) = terrain.forced_columns.pop_first() else {
                        break;
                    };
                    terrain.mesh_column_forced(&store, cx, cz);
                }
                for _ in 0..DIRTY_COLUMN_BUDGET {
                    let Some((cx, cz)) = terrain.dirty_columns.pop_first() else {
                        break;
                    };
                    terrain.mesh_column(&store, cx, cz);
                }
                for key in terrain.drain_removals() {
                    gpu.remove(&key);
                }
                for meshed in terrain.drain_all_meshes() {
                    gpu.insert(meshed.key);
                }
                max_backlog = max_backlog.max(terrain.dirty_columns.len());
            }
        }
        let backlog_while_walking = terrain.dirty_columns.len();

        // Now stand still. Nothing arrives and nothing unloads; only the heal
        // budget runs. Bounded so a genuinely stuck queue fails instead of
        // looping forever.
        let mut frames_to_drain = 0usize;
        for frame in 1..=512 {
            if terrain.dirty_columns.is_empty() && terrain.forced_columns.is_empty() {
                frames_to_drain = frame - 1;
                break;
            }
            for _ in 0..DIRTY_COLUMN_BUDGET {
                let Some((cx, cz)) = terrain.forced_columns.pop_first() else {
                    break;
                };
                terrain.mesh_column_forced(&store, cx, cz);
            }
            for _ in 0..DIRTY_COLUMN_BUDGET {
                let Some((cx, cz)) = terrain.dirty_columns.pop_first() else {
                    break;
                };
                terrain.mesh_column(&store, cx, cz);
            }
            for key in terrain.drain_removals() {
                gpu.remove(&key);
            }
            for meshed in terrain.drain_all_meshes() {
                gpu.insert(meshed.key);
            }
            frames_to_drain = frame;
        }

        let resident: BTreeSet<(i32, i32)> = gpu.iter().map(|k| (k.cx, k.cz)).collect();
        let expected: BTreeSet<(i32, i32)> = ever_interior()
            .intersection(&window(WALK_STEPS))
            .copied()
            .collect();
        eprintln!(
            "backlog: max {max_backlog}, {backlog_while_walking} on arrival at the \
             last step, drained in {frames_to_drain} standing frames; resident {} \
             of {} expected",
            resident.len(),
            expected.len()
        );

        assert!(
            terrain.dirty_columns.is_empty() && terrain.forced_columns.is_empty(),
            "the heal queue never drained while standing still ({} dirty + {} forced \
             columns left) — that is a genuine queue bug, not starvation",
            terrain.dirty_columns.len(),
            terrain.forced_columns.len()
        );
        // Not vacuous: there has to have *been* a backlog for draining it to be
        // evidence of anything.
        assert!(
            max_backlog > DIRTY_COLUMN_BUDGET,
            "the walk never built a backlog past one frame's budget (max \
             {max_backlog}), so this test did not exercise the queue it claims to"
        );
        assert_eq!(
            resident, expected,
            "a column was lost or permanently deferred: {} resident vs {} expected. \
             With mesh workers modelled as infinitely fast, that could only be the \
             scheduling path itself.",
            resident.len(),
            expected.len()
        );
    }
}
