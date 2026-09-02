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
//!    build instead of baking it.
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

use std::cell::RefCell;
use std::cmp::Reverse;
use std::collections::{BTreeSet, BinaryHeap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
// The worker pool's plumbing. Native-only: `MeshScheduler`'s browser arm has no
// threads and no channels — it meshes in-frame under a time budget. See that type.
#[cfg(not(target_arch = "wasm32"))]
use std::sync::{
    Mutex,
    mpsc::{self, Receiver},
};
#[cfg(not(target_arch = "wasm32"))]
use std::thread::{self, JoinHandle};

use bevy_ecs::query::With;
use bevy_ecs::resource::Resource;
use bevy_ecs::schedule::IntoScheduleConfigs;
use bevy_ecs::system::{Query, Res, ResMut};
use lodestone_ecs::app::{App, Plugin};
use lodestone_ecs::{ChunkWorld, ChunkWorldWrite, FrameSet, LocalPlayer, PhysicsState, Update};
use lodestone_render::{
    BlockClassifier, BlockModels, ChunkSectionView, FluidCell, FluidKind, FluidMeshes,
    FluidNeighborCell, FluidSectionView, FluidSprites, Mesh, ModelMesh, ModelSectionView,
    SectionLight,
    SectionNeighborhood, SkyDefault, UniformLight, WorldSectionLight, biome_tint_kind_for_slot,
    face_of_direction, mesh_fluids, mesh_models, mesh_simple,
};
use lodestone_render::biome_tint::{
    BLEND_RADIUS, BlendedTintCursor, NamedBiomeTint, rgb_to_bytes,
};
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

/// Per-section instrumentation for the biome-tint path — the answer to "did
/// this mesh tint at all, and against which registry", which no counter in
/// this file could answer before.
///
/// The distinction it exists to draw is the one a screenshot cannot: a quad
/// that resolved a *wrong* biome and a quad that took no tint path at all
/// look like two different colours, but "no tint path" and "block that is
/// simply not tinted" look identical. So every call to
/// [`SnapshotModelView::biome_tint_at`] lands in exactly one bucket below and
/// the buckets are reported together with the registry the snapshot carried.
///
/// Accumulated in a thread-local because a mesh worker owns its section for
/// the whole of [`mesh_one`] and nothing else runs on that thread in between —
/// which is also why the counters can be plain `Cell`s rather than atomics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TintProbe {
    /// Quads offered to the tint path (every quad the model mesher emits).
    pub quads: u32,
    /// The baked quad carries no `tint_index` at all — slot 255. Not a defect:
    /// stone, dirt and most of the game are here.
    pub untinted: u32,
    /// A real palette slot, but not one of the four position-dependent kinds
    /// (`Constant`/`RedstonePower`/…). Takes the frame-shared palette entry.
    pub not_blended: u32,
    /// A biome-blended kind whose tint was **skipped** because
    /// [`BlockModels::colormaps`] is absent. This is the one bucket that is a
    /// silent downgrade: the quad keeps its palette slot and never learns the
    /// biome.
    pub no_colormaps: u32,
    /// The blend itself returned nothing.
    pub unresolved: u32,
    /// A real, position-resolved biome colour reached the vertex.
    pub resolved: u32,
}

thread_local! {
    static TINT_PROBE: std::cell::Cell<TintProbe> = const {
        std::cell::Cell::new(TintProbe {
            quads: 0,
            untinted: 0,
            not_blended: 0,
            no_colormaps: 0,
            unresolved: 0,
            resolved: 0,
        })
    };
}

/// Record one [`SnapshotModelView::biome_tint_at`] outcome on this worker.
fn probe_tint(f: impl FnOnce(&mut TintProbe)) {
    TINT_PROBE.with(|p| {
        let mut v = p.get();
        f(&mut v);
        p.set(v);
    });
}

/// Take and clear this worker's counters.
fn take_tint_probe() -> TintProbe {
    TINT_PROBE.with(|p| p.replace(TintProbe::default()))
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
    /// Vanilla's radius-2 biome blend, shared between adjacent cells of a row —
    /// see [`SnapshotFluidView::tint`] for why this is a `RefCell` and what makes
    /// it safe.
    tint: RefCell<BlendedTintCursor>,
    /// The live `options.cutoutLeaves` value this snapshot was meshed against —
    /// see [`Self::force_opaque_at`].
    cutout_leaves: bool,
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
    // The live registry order wins whenever one is known (a follow-up
    // fix): `snapshot.biome_names` is only ever non-empty when
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

    /// Vanilla's `ambientocclusion` model-JSON flag, per state.
    ///
    /// The trait default is `true`, which is what preserved behaviour while this
    /// was unwired — so **the flag mechanism was inert in the running game until
    /// this override existed**, exactly the island shape `CLAUDE.md` rule 1
    /// names. Mirrors `quads_at`'s lookup deliberately: same state id, same
    /// `BlockModels`, so a model whose flag says "flat" cannot disagree with the
    /// geometry it was baked alongside.
    ///
    /// Note this is only the model-flag third of
    /// vanilla's own model-block-renderer predicate; the `getLightEmission() == 0`
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

    /// Owner report: "the ice texture looks inverted... i can see the four
    /// walls of the ice blocks even when theyre beside other ice so it looks
    /// like a grid". `occludes_at` above is correctly `false` for ice (it is a
    /// vanilla `noOcclusion()` block), so nothing culled its interior faces —
    /// this is the missing second half of vanilla's own should-render-face check,
    /// its own skip-rendering hook. Mirrors `occludes_at`'s split/bounds
    /// logic for the neighbour; the block being meshed (`x, y, z`) is always
    /// section-local, matching every other per-cell lookup on this view.
    fn skips_rendering_against(&self, x: i32, y: i32, z: i32, nx: i32, ny: i32, nz: i32) -> bool {
        let (ndx, nlx) = split16(nx);
        let (ndy, nly) = split16(ny);
        let (ndz, nlz) = split16(nz);
        if !(-1..=1).contains(&ndx) || !(-1..=1).contains(&ndy) || !(-1..=1).contains(&ndz) {
            return false;
        }
        let here_id = self.snapshot.at(0, 0, 0).get_block(x as usize, y as usize, z as usize);
        let neighbour_id = self.snapshot.at(ndx, ndy, ndz).get_block(nlx, nly, nlz);
        self.models.skips_rendering_against(here_id, neighbour_id)
    }

    /// Vanilla's FAST leaves (`options.cutoutLeaves == false`): real per-face
    /// occlusion is untouched — `occludes_at`/`ambient_occlusion_at` above
    /// still answer from the block's *actual*, cutout-textured geometry, so a
    /// leaf still does not cull its neighbours' faces or block ambient
    /// occlusion, matching vanilla (the preset is a render-pass choice, not a
    /// shape change). This is the render-only half:
    /// [`BlockModels::is_leaves`] is vanilla's own `LeavesBlock` list, not a
    /// derivation from [`crate::block_models::RenderLayer`] (see that
    /// method's doc for why the layer alone is the wrong predicate — grass,
    /// panes and a dozen other `Cutout` blocks must **not** go opaque here).
    fn force_opaque_at(&self, x: usize, y: usize, z: usize) -> bool {
        if self.cutout_leaves {
            return false;
        }
        let id = self.snapshot.at(0, 0, 0).get_block(x, y, z);
        self.models.is_leaves(id)
    }

    /// Vanilla's per-**quad** render layer: `SectionCompiler` buckets every
    /// quad on `quad.materialInfo().layer()`, derived from the transparency of
    /// that quad's own sprite. `BakedQuad::sprite` is an index into the same
    /// atlas sprite list `BlockModels::sprite_layer` is keyed on, so this is a
    /// single array read — no UV geometry, no per-state roll-up.
    ///
    /// Two owner reports meet here. "The nether portal swirly block is opaque
    /// when it isn't supposed to be" was the routing half: the classification
    /// existed and nothing read it when choosing a mesh. The pinprick half is
    /// what the per-*state* roll-up cost — a state whose model mixes an opaque
    /// sprite with a cutout one took `Cutout` for every face, so faces vanilla
    /// draws through a pipeline with no alpha test at all were alpha-tested
    /// here, and a mip-filtered alpha at a sprite edge can dip under the
    /// threshold and discard.
    ///
    /// Mirrors `quads_at`'s lookup: same state id, same `BlockModels`.
    fn quad_layer(
        &self,
        x: usize,
        y: usize,
        z: usize,
        quad: &BakedQuad,
    ) -> Option<lodestone_render::RenderLayer> {
        let layer = self.models.sprite_layer(quad.sprite)?;
        if layer != lodestone_render::RenderLayer::Translucent {
            return Some(layer);
        }
        // A cauldron's inset liquid uses a partially-alpha sprite, but the
        // whole cauldron model is one depth-writing unit here: its liquid quad
        // sits *inside* the body rather than in front of it, so blending it
        // without the body's own depth already laid down draws the water
        // through the walls. Demote it to `Cutout` — the alpha-tested opaque
        // pass — which is what this block did before per-quad routing existed.
        // See `BlockModels::is_cauldron`.
        let id = self.snapshot.at(0, 0, 0).get_block(x, y, z);
        if self.models.is_cauldron(id) {
            return Some(lodestone_render::RenderLayer::Cutout);
        }
        Some(layer)
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
    /// [`BlendedTintCursor`], and the whole reason
    /// `BiomeTint` trait now has an implementor outside a test mock. `slot`
    /// tells us *which* of the four kinds this quad is
    /// ([`biome_tint_kind_for_slot`]); `None` when it's not one of them (no
    /// override needed) or when [`BlockModels::colormaps`] failed to load
    /// (tolerated — falls back to the reserved slot's plains default in the
    /// palette, exactly as before this existed).
    fn biome_tint_at(&self, x: usize, y: usize, z: usize, slot: u8) -> Option<[u8; 3]> {
        probe_tint(|p| p.quads += 1);
        let Some(kind) = biome_tint_kind_for_slot(slot) else {
            probe_tint(|p| {
                if slot == 255 {
                    p.untinted += 1;
                } else {
                    p.not_blended += 1;
                }
            });
            return None;
        };
        let Some(colormaps) = self.models.colormaps() else {
            probe_tint(|p| p.no_colormaps += 1);
            return None;
        };
        let biome = NamedBiomeTint::new(|pos| biome_name_at(self.snapshot, pos));
        // `self.tint.resolve` in place of `resolve_blended_tint`: bit-identical,
        // ~5x fewer samples along a row. It keys itself on `(kind, y, z, x)`, and
        // `kind` is per *quad* here rather than per cell (a grass block's own quads
        // are all `Grass`, but a neighbouring foliage quad is not), so a mixed
        // section rebuilds more often than the fluid path does — never worse than
        // the plain call, which is what a rebuild is.
        let Some(rgb) = self.tint.borrow_mut().resolve(
            kind,
            colormaps,
            &biome,
            x as i32,
            y as i32,
            z as i32,
        ) else {
            probe_tint(|p| p.unresolved += 1);
            return None;
        };
        probe_tint(|p| p.resolved += 1);
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
///
/// `cutout_leaves` is vanilla's `options.cutoutLeaves` (`true` = FANCY/
/// FABULOUS's see-through holes, `false` = FAST's solid leaves) — see
/// [`SnapshotModelView::force_opaque_at`].
#[must_use]
pub fn mesh_snapshot_models(
    snapshot: &SectionSnapshot,
    models: &BlockModels,
    cutout_leaves: bool,
) -> ModelMesh {
    mesh_snapshot_models_at(snapshot, models, cutout_leaves, BLEND_RADIUS)
}

/// [`mesh_snapshot_models`] at an explicit biome-blend radius — vanilla's
/// `options.biomeBlendRadius`, an `IntRange(0, 7)` whose displayed value is the
/// window *width* `2r + 1` (`0` is `en_us.json`'s "OFF (Fastest)", i.e. no
/// blending at all).
///
/// The three-argument form above is kept, delegating at
/// [`BLEND_RADIUS`] — vanilla's own default — so the many gates that call it
/// positionally keep compiling and keep measuring the same geometry they always
/// did. Production goes through [`mesh_one`], which takes the live value.
///
/// `BlendedTintCursor::new` clamps to `0..=MAX_BLEND_RADIUS` itself, so an
/// out-of-range radius here is a wider window rather than a panic — see that
/// constructor.
#[must_use]
pub fn mesh_snapshot_models_at(
    snapshot: &SectionSnapshot,
    models: &BlockModels,
    cutout_leaves: bool,
    blend_radius: i32,
) -> ModelMesh {
    let view = SnapshotModelView {
        snapshot,
        models,
        light: SnapshotLight::new(snapshot),
        tint: RefCell::new(BlendedTintCursor::new(blend_radius)),
        cutout_leaves,
    };
    mesh_models(&view)
}

/// Like [`mesh_snapshot_models`], but keeps
/// [`RenderLayer::Translucent`](lodestone_render::RenderLayer::Translucent)
/// blocks (stained glass, ice, the nether portal swirl) in a **second** mesh
/// instead of folding them into the opaque/cutout one — see
/// [`lodestone_render::mesh_models_layers`]'s doc for why the split exists.
/// `mesh_one` is this function's only production caller; the merged
/// [`mesh_snapshot_models`] above stays as-is for existing test/bench callers
/// that only want the combined geometry.
///
/// `pub` because **every** pixel gate in `tests/` was passing
/// `translucent_blocks: ModelMesh::default()` — the whole corpus had never
/// once rendered a translucent block, which is why a display panel deleting
/// the stained glass behind it reached the owner. A gate that wants real
/// translucent terrain must reach the production split, not re-derive one.
#[must_use]
pub fn mesh_snapshot_models_layers(
    snapshot: &SectionSnapshot,
    models: &BlockModels,
    cutout_leaves: bool,
    blend_radius: i32,
) -> (ModelMesh, ModelMesh) {
    let view = SnapshotModelView {
        snapshot,
        models,
        light: SnapshotLight::new(snapshot),
        tint: RefCell::new(BlendedTintCursor::new(blend_radius)),
        cutout_leaves,
    };
    lodestone_render::mesh_models_layers(&view)
}

/// This section's face connectivity for the occlusion graph (U3) — the producer
/// half of `lodestone_render::visibility`.
///
/// Computed here, in the mesh worker, for the reason vanilla computes its
/// `VisGraph` at compile time: the flood is once per remesh and off the render
/// thread, and both of `compute_visibility`'s shortcuts (fewer than 256 opaque
/// cells → fully connected; fully opaque → connects nothing) mean most sections
/// never flood at all.
///
/// Reads the **centre** section only, through the same
/// `BlockModels::occludes` predicate `SnapshotModelView::occludes_at` uses for
/// face culling — vanilla's `isSolidRender` family, which is what feeds its own
/// visibility-graph opaque-flag setter. A block this answers "not opaque" for only ever
/// *connects* more faces, i.e. draws more, which is the safe direction.
#[must_use]
pub fn snapshot_visibility(
    snapshot: &SectionSnapshot,
    models: &BlockModels,
) -> lodestone_render::SectionVisibility {
    let centre = snapshot.at(0, 0, 0);
    lodestone_render::compute_visibility_from(|x, y, z| models.occludes(centre.get_block(x, y, z)))
}

/// The mesher's fluid view over a snapshot: resolves each cell's fluid (if any)
/// and occlusion out of the paletted sections, and reads the centre section's
/// light. Fluids need the same signed neighbourhood as the model path (one cell
/// past a section edge) to cull shared faces and slope corners.
struct SnapshotFluidView<'a> {
    snapshot: &'a SectionSnapshot,
    models: &'a BlockModels,
    light: SnapshotLight<'a>,
    /// Vanilla's radius-2 biome blend is 25 samples per tinted quad, and two
    /// adjacent cells' boxes share 20 of their 25 columns —
    /// [`BlendedTintCursor`] turns that into a sliding sum, bit-identically
    /// (`DESIGN.md` §12.128).
    ///
    /// `RefCell` because [`FluidSectionView::water_tint_at`] takes `&self` and the
    /// cursor is mutable state; `Cell` will not do, since the cursor is ~200 bytes
    /// and not `Copy`. **This makes the view `!Sync`**, which is sound because
    /// [`mesh_snapshot_fluids`] builds one per call and `mesh_fluids` never shares
    /// it — the mesh worker pool parallelises over *sections*, one view each. The
    /// same reasoning `NamedBiomeTint` already relies on since #542.
    ///
    /// The cursor caches sampled colours, so it is only correct because a
    /// [`SectionSnapshot`] is immutable for the life of the view. Do not hoist one
    /// into anything longer-lived.
    tint: RefCell<BlendedTintCursor>,
}

impl FluidSectionView for SnapshotFluidView<'_> {
    /// [`Self::fluid_at`], [`Self::occludes_at`] and [`Self::overlay_at`] in
    /// **one** call, sharing the single expensive part: three `split16`s, three
    /// range checks, one 27-entry snapshot-slot index and one
    /// `PalettedContainer::get` bit-unpack, after which all three answers are
    /// `Vec` lookups on the same state id.
    ///
    /// This is [`lodestone_render::FluidGrid`]'s fill primitive and it runs at
    /// least 4,096 times per section, so the sharing is what makes the grid pay
    /// for itself. Without this override the default composition triples the
    /// fill's coordinate work and a **fluid-free** section costs 2.9× what it
    /// did before the grid existed — measured, not predicted (`DESIGN.md`
    /// §12.124). The out-of-neighbourhood answer is
    /// `FluidNeighborCell::default()`, which is exactly the `None`/`false`/
    /// `false` the three methods below return there.
    fn cell_at(&self, x: i32, y: i32, z: i32) -> FluidNeighborCell {
        let (dx, lx) = split16(x);
        let (dy, ly) = split16(y);
        let (dz, lz) = split16(z);
        if !(-1..=1).contains(&dx) || !(-1..=1).contains(&dy) || !(-1..=1).contains(&dz) {
            return FluidNeighborCell::default();
        }
        let id = self.snapshot.at(dx, dy, dz).get_block(lx, ly, lz);
        FluidNeighborCell {
            fluid: self.models.fluid(id),
            occludes: self.models.occludes(id),
            overlay: self.models.fluid_overlay(id),
        }
    }

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

    /// The live half of `shouldRenderFace`'s *self* test — the block sharing the
    /// fluid's own cell, which for a waterlogged stair is the stair. Same
    /// trait-default-plus-override shape as `overlay_at` and
    /// `partial_occluder_y_range_at` above, and the same failure mode if it is
    /// missing: the rule sits in `lodestone-render` and never reaches terrain.
    ///
    /// # The `RenderLayer::Solid` gate is vanilla's `canOcclude`
    ///
    /// Vanilla builds the shape this test reads as
    /// `canOcclude ? getOcclusionShape(state) : Shapes.empty()`, and `canOcclude`
    /// is a `Properties` flag with no getter, absent from `blocks.json` and from
    /// every table in `lodestone-data`. `BlockModels::layer` stands in for it: a
    /// state whose sprites are fully opaque renders `Solid`, and a
    /// `noOcclusion()` block is (in 26.2, across every waterloggable block) one
    /// whose textures are not. Without the gate, **waterlogged leaves** — a
    /// full-cube outline shape, `noOcclusion()` in vanilla — would report all five
    /// faces occluded and cull their water away entirely.
    ///
    /// The gate is not a scoping compromise on the *geometry* side:
    /// `face_fully_covered` is exact for any axis-aligned union, so a stair's
    /// two-box solid side is answered correctly where
    /// `partial_occluder_y_range_at`'s single-box reduction would have declined.
    ///
    /// Cheap for ordinary water: `minecraft:water`'s own outline shape is empty
    /// (vanilla's own liquid-block shape getter is `Shapes.empty()`) *and* its layer is
    /// `Translucent`, so an open ocean's cells leave on the first branch.
    fn self_occlusion_at(&self, x: i32, y: i32, z: i32) -> lodestone_assets::fluid::SelfOcclusion {
        use lodestone_assets::fluid::SelfOcclusion;

        let (dx, lx) = split16(x);
        let (dy, ly) = split16(y);
        let (dz, lz) = split16(z);
        if !(-1..=1).contains(&dx) || !(-1..=1).contains(&dy) || !(-1..=1).contains(&dz) {
            return SelfOcclusion::default();
        }
        let id = self.snapshot.at(dx, dy, dz).get_block(lx, ly, lz);
        if self.models.layer(id) != lodestone_render::RenderLayer::Solid {
            return SelfOcclusion::default();
        }
        let Some(boxes) = lodestone_data::outline_shapes::outline_boxes(id) else {
            return SelfOcclusion::default();
        };
        lodestone_assets::fluid::self_occlusion(boxes)
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
        // See `SnapshotModelView::biome_tint_at`. `mesh_fluids` iterates
        // `y -> z -> x` with `x` innermost, so consecutive water cells in a row
        // hit the sliding path and pay 5 samples instead of 25.
        let rgb = self.tint.borrow_mut().resolve(
            lodestone_assets::tint::TintKind::Water,
            colormaps,
            &biome,
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
    mesh_snapshot_fluids_at(snapshot, models, BLEND_RADIUS)
}

/// [`mesh_snapshot_fluids`] at an explicit biome-blend radius.
///
/// **Water is tinted per biome exactly as foliage is**, so this has to take the
/// option too — wiring only the block path would leave a lake's colour blending
/// at vanilla's default while the grass around it followed the slider, a
/// mismatch at the shoreline that is more visible than either setting alone.
/// Same delegating shape as [`mesh_snapshot_models_at`], for the same reason.
#[must_use]
pub fn mesh_snapshot_fluids_at(
    snapshot: &SectionSnapshot,
    models: &BlockModels,
    blend_radius: i32,
) -> FluidMeshes {
    let view = SnapshotFluidView {
        snapshot,
        models,
        light: SnapshotLight::new(snapshot),
        tint: RefCell::new(BlendedTintCursor::new(blend_radius)),
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
        /// Translucent **block** geometry — stained glass, ice, the nether
        /// portal swirl and anything else `BlockModels::layer` classifies as
        /// [`RenderLayer::Translucent`](lodestone_render::RenderLayer::Translucent)
        /// from real per-texel alpha. Kept separate from `water` (a different
        /// pipeline: `FLUID_WGSL` tints untinted quads with the water colour
        /// and carries no palette bind group, wrong for a palette-tinted
        /// translucent block) and drawn through its own
        /// `ModelPipeline::for_layer(.., RenderLayer::Translucent)` pass — see
        /// `gpu/frame.rs`.
        translucent_blocks: ModelMesh,
        /// This section's face connectivity for the occlusion graph (U3), from
        /// [`snapshot_visibility`].
        ///
        /// It rides on the *geometry* rather than on [`Meshed`] deliberately, and
        /// that is load-bearing rather than tidy: `RenderState::upload_section`
        /// takes `&SectionGeometry`, so putting it here reaches the graph with
        /// **no** change to the three `upload_section` call sites in
        /// `app/{redraw,runners,lifecycle}.rs`. It also means a section whose
        /// geometry is *empty* still carries its connectivity — which is the
        /// whole point for a fully-enclosed underground section, the very
        /// sections that block the walk and make the underground free.
        visibility: lodestone_render::SectionVisibility,
    },
}

impl SectionGeometry {
    /// The merged quad count, for stats/overlay parity across both paths.
    #[must_use]
    pub fn quad_count(&self) -> usize {
        match self {
            SectionGeometry::Packed(m) => m.quad_count(),
            SectionGeometry::Model {
                opaque,
                water,
                translucent_blocks,
                ..
            } => opaque.quad_count() + water.quad_count() + translucent_blocks.quad_count(),
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

#[cfg(not(target_arch = "wasm32"))]
enum Job {
    /// A snapshot plus the two `Options` values it was submitted under —
    /// `options.cutoutLeaves` and `options.biomeBlendRadius`. See
    /// [`MeshScheduler::submit`]'s doc for why they travel with the job rather
    /// than being read by the worker from shared state. The trailing `u64` is
    /// the generation [`MeshScheduler::submit`] stamped it with, echoed back on
    /// the result channel so a stale completion can be told from the current
    /// one — see [`MeshScheduler::latest_generation`]'s doc.
    Mesh(SectionSnapshot, bool, i32, u64),
}

/// Mesh one snapshot. **The single meshing body, shared by both schedulers.**
///
/// Extracted when the browser arm landed, for the same reason `menu::accounts`'
/// `finish_ms_token` is extracted from its two sign-in flows: the native worker
/// thread and the browser's in-frame drain must not be able to produce *different
/// geometry*. A forked copy is a defect that shows up as a browser world that is
/// subtly wrong rather than as a build failure, and no `cargo check` could see it.
fn mesh_one(
    snap: SectionSnapshot,
    classifier: &ShellClassifier,
    cutout_leaves: bool,
    blend_radius: i32,
) -> Meshed {
    let _span = tracing::info_span!(
        "mesh_section",
        cx = snap.key.cx, cz = snap.key.cz, si = snap.key.si,
    ).entered();
    // The vanilla classifier carries baked models → mesh through the model path;
    // the demo classifier has none → mesh through the packed full-cube path.
    let biome_names_len = snap.biome_names.len();
    let _ = take_tint_probe();
    let mesh = match classifier.models() {
        Some(models) => {
            let (mut opaque, translucent_blocks) =
                mesh_snapshot_models_layers(&snap, models, cutout_leaves, blend_radius);
            let fluids = mesh_snapshot_fluids_at(&snap, models, blend_radius);
            // Lava is opaque and full-bright: fold it into the opaque pass. Water
            // and translucent blocks (glass, ice, the nether portal swirl) are
            // translucent and drawn separately, each through its own pipeline.
            opaque.merge(&fluids.lava);
            SectionGeometry::Model {
                opaque,
                water: fluids.water,
                translucent_blocks,
                visibility: snapshot_visibility(&snap, models),
            }
        }
        None => SectionGeometry::Packed(mesh_snapshot(&snap, classifier)),
    };
    report_tint_probe(
        snap.key,
        biome_names_len,
        mesh.quad_count(),
        matches!(mesh, SectionGeometry::Packed(_)),
        take_tint_probe(),
    );
    Meshed {
        key: snap.key,
        mesh,
    }
}

/// Report one section's [`TintProbe`], so a tint that resolved to *nothing* is
/// distinguishable from a block that simply has no tint.
///
/// `names` is how many biome names the snapshot could see — `0` means
/// `biome_name_at` fell back to `FALLBACK_BIOME_NAMES` rather than the
/// server's own `registry_data` order, which is a *different* answer, not a
/// missing one.
///
/// Two sinks on purpose. `tracing` is the shipped one, and it is a `warn!`
/// only for the bucket that is a silent downgrade (a blended kind whose
/// colormaps were absent); everything else is `debug!`. The stderr line is for
/// a harness that installs no subscriber at all — the screenshot capture is
/// one — and stays off unless `LODESTONE_TINT_PROBE` is set, because this
/// fires once per meshed section and a render-distance-8 join meshes thousands.
fn report_tint_probe(
    key: SectionKey,
    names: usize,
    quads: usize,
    packed: bool,
    probe: TintProbe,
) {
    BIOME_TINT_RESOLVED.fetch_add(u64::from(probe.resolved), Ordering::Relaxed);
    let skipped = probe.no_colormaps + probe.unresolved;
    if skipped > 0 {
        let before = BIOME_TINT_SKIPPED.fetch_add(u64::from(skipped), Ordering::Relaxed);
        if before == 0 {
            tracing::warn!(
                target: "mesh",
                cx = key.cx,
                cz = key.cz,
                si = key.si,
                no_colormaps = probe.no_colormaps,
                unresolved = probe.unresolved,
                "biome tint skipped for a blended quad: it keeps the frame-shared \
                 palette's plains default instead of this position's own biome \
                 colour. Logged once per session; the running total is \
                 mesher::biome_tint_counts"
            );
        }
    }
    tracing::debug!(
        target: "mesh",
        cx = key.cx,
        cz = key.cz,
        si = key.si,
        names,
        quads,
        resolved = probe.resolved,
        unresolved = probe.unresolved,
        untinted = probe.untinted,
        "section meshed"
    );
    static STDERR: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *STDERR.get_or_init(|| std::env::var_os("LODESTONE_TINT_PROBE").is_some()) {
        eprintln!(
            "tint-probe cx={} cz={} si={} path={} names={names} quads={quads} \
offered={} resolved={} unresolved={} no_colormaps={} not_blended={} untinted={}",
            key.cx,
            key.cz,
            key.si,
            if packed { "packed" } else { "model" },
            probe.quads,
            probe.resolved,
            probe.unresolved,
            probe.no_colormaps,
            probe.not_blended,
            probe.untinted,
        );
    }
}

/// Blended-kind quads whose tint was resolved for real, this process.
static BIOME_TINT_RESOLVED: AtomicU64 = AtomicU64::new(0);
/// Blended-kind quads whose tint was **skipped** — absent colormaps, or a
/// blend that returned nothing — and which therefore fell back to the
/// frame-shared palette's plains default.
static BIOME_TINT_SKIPPED: AtomicU64 = AtomicU64::new(0);

/// `(resolved, skipped)` blended-kind quads since process start.
///
/// A running total rather than a per-section one because the mesh workers have
/// no `Sim` to report into, and because the question this answers is a session
/// question: *did any terrain in this session render its biome tint from the
/// palette default rather than from the position?* A neutral-grey ground and a
/// correctly plains-green one are indistinguishable on a plains-only world —
/// the palette default **is** plains — so a screenshot cannot tell them apart
/// and this counter is the only thing that can.
#[must_use]
pub fn biome_tint_counts() -> (u64, u64) {
    (
        BIOME_TINT_RESOLVED.load(Ordering::Relaxed),
        BIOME_TINT_SKIPPED.load(Ordering::Relaxed),
    )
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
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Resource)]
pub struct MeshScheduler {
    /// Lock-free MPMC job channel — crossbeam unbounded. `Receiver` is `Clone`
    /// so each worker gets its own endpoint: no mutex, no round-robin, the
    /// channel distributes by actual work completion. Benchmarked 20.4ms vs
    /// 25.4ms round-robin at 10 workers; dead-even at 4 workers (31.3ms).
    job_tx: crossbeam_channel::Sender<Job>,
    /// Paired with the generation [`Self::submit`] stamped the job with, so
    /// [`Self::drain`]/[`Self::drain_blocking`] can tell a stale completion
    /// from the current one — see [`Self::latest_generation`]'s doc for why
    /// that pairing exists at all.
    result_rx: Mutex<Receiver<(Meshed, u64)>>,
    workers: Vec<JoinHandle<()>>,
    pending: usize,
    column_source: ColumnSource,
    /// The live `options.cutoutLeaves` value, stamped onto each [`Job::Mesh`]
    /// at [`Self::submit`] time (a plain field, not shared state: `submit`
    /// already needs `&mut self`, and a worker thread never reads this field
    /// at all — only the value its own job carried).
    cutout_leaves: bool,
    /// The live `options.biomeBlendRadius` value, stamped onto each
    /// [`Job::Mesh`] beside [`Self::cutout_leaves`] and for the same reason.
    blend_radius: i32,
    /// The generation number [`Self::submit`] most recently stamped a job
    /// for this key with — the defence against a **stale mesh silently
    /// overwriting a fresher one**, which the pool's own doc already admits
    /// is possible: *"the channel distributes by actual work completion"*
    /// and *"two concurrent drains... would interleave meshes arbitrarily"*.
    /// With `worker_count > 1` (the production default), two jobs for the
    /// *same* section — a client-predicted break's own remesh, then the
    /// server's correction moments later when the prediction is denied —
    /// can finish in either order. Without this, a slower worker finishing
    /// the *older* (predicted) job after a faster one finishes the *newer*
    /// (corrected) job hands the caller the stale geometry last, and nothing
    /// ever re-derives it again because no further dirty signal is coming —
    /// the section is uploaded and never marked dirty again, exactly the
    /// "block came back (hitbox and all) but the mesh didn't render" report:
    /// collision reads `ChunkWorld` directly and shows the corrected block,
    /// while the GPU section map is stuck on the superseded snapshot.
    ///
    /// [`Self::drain`]/[`Self::drain_blocking`] drop any completion whose
    /// generation does not match this map's entry for its key — only the
    /// *most recently submitted* job for a section is ever allowed through,
    /// regardless of completion order.
    latest_generation: HashMap<SectionKey, u64>,
    /// Monotonic counter [`Self::submit`] draws from to stamp each job.
    next_generation: u64,
}

#[cfg(not(target_arch = "wasm32"))]
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
    /// `Complete` live world is the seam-baked-against-air defect unfixed.
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
        let (result_tx, result_rx) = mpsc::channel::<(Meshed, u64)>();

        // Lock-free MPMC: one channel, every worker clones the consumer.
        let (job_tx, job_rx) = crossbeam_channel::unbounded::<Job>();

        let mut workers = Vec::with_capacity(worker_count);
        for _ in 0..worker_count {
            let rx = job_rx.clone();
            let result_tx = result_tx.clone();
            let classifier = classifier.clone();
            workers.push(thread::spawn(move || {
                loop {
                    let (snap, cutout_leaves, blend_radius, generation) = match rx.recv() {
                        Ok(Job::Mesh(snap, cutout_leaves, blend_radius, generation)) => {
                            (snap, cutout_leaves, blend_radius, generation)
                        }
                        Err(_) => break,
                    };
                    if result_tx
                        .send((
                            mesh_one(snap, &classifier, cutout_leaves, blend_radius),
                            generation,
                        ))
                        .is_err()
                    {
                        break;
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
            cutout_leaves: true,
            blend_radius: BLEND_RADIUS,
            latest_generation: HashMap::new(),
            next_generation: 0,
        }
    }

    /// Sets the `options.cutoutLeaves` value future [`Self::submit`] calls
    /// stamp onto their jobs. Does **not** itself re-mesh anything already
    /// queued or uploaded — see `Sim::set_cutout_leaves` for the caller that
    /// forces a remesh of every loaded column, vanilla's own
    /// `operateOnLevelExtractor(LevelExtractor::allChanged)`.
    pub fn set_cutout_leaves(&mut self, value: bool) {
        self.cutout_leaves = value;
    }

    /// The `options.cutoutLeaves` value new jobs are currently stamped with.
    #[must_use]
    pub fn cutout_leaves(&self) -> bool {
        self.cutout_leaves
    }

    /// Sets the `options.biomeBlendRadius` value future [`Self::submit`] calls
    /// stamp onto their jobs. Same shape and same caveat as
    /// [`Self::set_cutout_leaves`]: it does not itself re-mesh anything, which
    /// is `TerrainMesh::set_blend_radius`'s job.
    pub fn set_blend_radius(&mut self, value: i32) {
        self.blend_radius = value;
    }

    /// The `options.biomeBlendRadius` value new jobs are currently stamped with.
    #[must_use]
    pub fn blend_radius(&self) -> i32 {
        self.blend_radius
    }

    /// Whether the world this pool meshes has all its columns already. See
    /// [`MeshScheduler::new`] for why the classifier decides this.
    #[must_use]
    pub fn column_source(&self) -> ColumnSource {
        self.column_source
    }

    /// Queue a snapshot for meshing.
    ///
    /// Round-robins across per-worker channels so no two workers contend on a
    /// mutex to dequeue — each worker owns its own `Receiver`, and the sender side
    /// distributes jobs with zero locking.
    pub fn submit(&mut self, snapshot: SectionSnapshot) {
        self.pending += 1;
        // Stamped *before* the send, and recorded as this key's current
        // generation immediately — not when the job completes — so a second
        // `submit` for the same key (the corrected-block case
        // `latest_generation`'s doc describes) always wins the race no matter
        // which of the two workers finishes first.
        self.next_generation += 1;
        let generation = self.next_generation;
        self.latest_generation.insert(snapshot.key, generation);
        // Crossbeam MPMC — lock-free send, workers compete on the shared
        // receiver. No round-robin: the channel distributes by which worker
        // finishes its current job first (true work-stealing).
        if self
            .job_tx
            .send(Job::Mesh(
                snapshot,
                self.cutout_leaves,
                self.blend_radius,
                generation,
            ))
            .is_err()
        {
            self.pending -= 1;
        }
    }

    /// Number of submitted jobs not yet drained.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.pending
    }

    /// Drop `key`'s entry from [`Self::latest_generation`] — a section this
    /// session will never mesh again (its column left the view) has nothing
    /// left to compare a completion against, and there is no reason to keep
    /// growing the map for the life of the session.
    pub fn forget_generation(&mut self, key: &SectionKey) {
        self.latest_generation.remove(key);
    }

    /// Collect any finished meshes without blocking. A completion whose
    /// generation is not this key's *latest* submitted one is a stale mesh a
    /// later `submit` has already superseded — dropped here rather than
    /// handed to the caller, per [`Self::latest_generation`]'s doc.
    pub fn drain(&mut self) -> Vec<Meshed> {
        let mut out = Vec::new();
        let rx = self.result_rx.get_mut().expect("mesh result queue poisoned");
        while let Ok((meshed, generation)) = rx.try_recv() {
            self.pending -= 1;
            if self.latest_generation.get(&meshed.key) == Some(&generation) {
                out.push(meshed);
            }
        }
        out
    }

    /// Block until at least `n` *current* results are available (or every
    /// currently-pending job — stale or not — has completed), returning
    /// everything collected. Used by tests and headless runs.
    ///
    /// A stale completion (superseded by a later `submit` for the same key,
    /// see [`Self::latest_generation`]) still counts against the "every
    /// pending job has completed" bound — it consumed a pool slot and its
    /// raw arrival is what this loop is waiting on — but is not pushed into
    /// `out` and does not count toward `n`, so the caller never receives it.
    pub fn drain_blocking(&mut self, n: usize) -> Vec<Meshed> {
        let mut out = Vec::new();
        let mut received = 0usize;
        let rx = self.result_rx.get_mut().expect("mesh result queue poisoned");
        while out.len() < n && self.pending > received {
            match rx.recv() {
                Ok((meshed, generation)) => {
                    received += 1;
                    if self.latest_generation.get(&meshed.key) == Some(&generation) {
                        out.push(meshed);
                    }
                }
                Err(_) => break,
            }
        }
        self.pending -= received;
        out
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for MeshScheduler {
    fn drop(&mut self) {
        // Drop the sender — closes the channel. Each worker's cloned Receiver
        // gets Err and exits its loop. Crossbeam drops cleanly, unlike
        // std::mpsc which can deadlock if the channel is full.
        drop(self.job_tx.clone());
        self.job_tx = crossbeam_channel::unbounded().0;
        for w in self.workers.drain(..) {
            let _ = w.join();
        }
    }
}

// ---------------------------------------------------------------------------
// The browser scheduler (`wasm32`)
// ---------------------------------------------------------------------------

/// How long [`MeshScheduler::drain`] may spend meshing in one call, in the browser.
///
/// **The whole design rests on this being a deadline rather than a job count**, and
/// on it being well under a frame. A section's mesh cost varies by orders of
/// magnitude — an all-air section is nearly free, a section of foliage with baked
/// models and fluids is not — so "mesh N sections per frame" is a budget in the wrong
/// unit: it is either far too small for air or far too large for leaves, and the
/// backlog after a teleport is thousands of sections either way.
///
/// 4 ms of a 16.7 ms frame leaves room for the render pass and, more importantly, for
/// the event loop to run at all. See [`MeshScheduler`]'s browser docs for why that
/// second point is the load-bearing one.
#[cfg(target_arch = "wasm32")]
const BROWSER_MESH_BUDGET: std::time::Duration = std::time::Duration::from_millis(4);

/// The browser's `MeshScheduler`: **the same interface, meshing in the frame under a
/// time budget.**
///
/// # Why there is no pool, and why that is the right answer rather than a stopgap
///
/// `std::thread::spawn` does not degrade on `wasm32-unknown-unknown` — it **traps**
/// (measured, executed in a wasm VM: `RuntimeError: unreachable`), and with
/// `panic = "abort"` in the browser profile that is the tab dying. So the native pool
/// cannot be ported as-is.
///
/// Threads are *available* — `web/Trunk.toml` already sets COOP/COEP, so the page is
/// cross-origin isolated and `SharedArrayBuffer` works — and this deliberately does
/// **not** use them. `wasm-bindgen-rayon` is a large lift and a large bundle against a
/// 1.6 MB gzip ceiling, and every other thread in the shell turned out to be
/// removable rather than portable. This one is too: meshing is pure compute over an
/// owned [`SectionSnapshot`], with no shared state and no ordering requirement, so
/// spreading it over frames is behaviourally equivalent to spreading it over cores —
/// only slower.
///
/// # What this DOES change, stated plainly
///
/// The native pool's docs make a promise this arm cannot keep: *"a slow frame delays
/// the upload of finished geometry, never the meshing and never the simulation"*, so
/// that presentation never gates simulation. In a browser there is one thread, so
/// **meshing is on the frame thread and that invariant is structurally broken.** It is
/// not papered over; it is bounded. [`BROWSER_MESH_BUDGET`] caps the work per drain,
/// so the event loop keeps turning, keep-alives keep being sent, and the session does
/// not look stalled to the server — which is the actual hazard the native invariant
/// exists to prevent (a client the server considers stalled is sent no chunks at
/// all). The cost is that a large backlog takes more frames to appear, which is
/// visible as terrain filling in progressively rather than as a stall.
///
/// If that ever proves too slow to be pleasant, the next move is a Web Worker holding
/// the classifier and meshing off-thread — a real answer, and a bigger one than a
/// budget. It is not `rayon`.
///
/// # How to change it
///
/// Keep the two `drain` methods the only place work happens. [`submit`](Self::submit)
/// must stay O(1): it is called from the enqueue system, which can submit a whole
/// column's sections in one frame, and meshing there would put the cost back on the
/// caller that the budget exists to protect.
#[cfg(target_arch = "wasm32")]
#[derive(Debug, Resource)]
pub struct MeshScheduler {
    /// Submitted-but-unmeshed snapshots, oldest first.
    ///
    /// A `VecDeque` used strictly FIFO, so submission order is meshing order. That
    /// matters more here than on native: the pool completes jobs in whatever order
    /// its workers finish, but with one thread the queue order *is* the order the
    /// world appears in, and `DirtyColumns` has already sorted its submissions by
    /// ring distance and view cone. Draining LIFO would show the player the far
    /// edge of the backlog first.
    queue: std::collections::VecDeque<SectionSnapshot>,
    /// Meshed but not yet handed to the caller.
    ///
    /// Non-empty only when a `drain` hit the budget mid-queue... which cannot
    /// currently happen, because `drain` returns everything it meshed. It exists so
    /// `drain_blocking(n)` can mesh past `n` without discarding the surplus.
    ready: Vec<Meshed>,
    classifier: ShellClassifier,
    column_source: ColumnSource,
    /// The live `options.cutoutLeaves` value. Read at **mesh** time (inside
    /// [`Self::drain`]/[`Self::drain_blocking`]) rather than stamped at
    /// submit time like the native scheduler's `Job` — there is only one
    /// thread here, so nothing can race a queued snapshot's meshing against a
    /// toggle the way the native pool's workers could.
    cutout_leaves: bool,
    /// The live `options.biomeBlendRadius` value, read at mesh time beside
    /// [`Self::cutout_leaves`] and for the same reason.
    blend_radius: i32,
}

#[cfg(target_arch = "wasm32")]
impl MeshScheduler {
    /// Build the browser scheduler. `worker_count` is accepted and **ignored**.
    ///
    /// Ignored rather than removed from the signature: the caller
    /// (`Sim::build`) derives it from `available_parallelism`, which on wasm32
    /// returns `Err` and so already falls back to 1, and keeping one signature means
    /// no `cfg` at the construction site. The count is logged so a browser session
    /// says out loud that it is meshing in-frame.
    #[must_use]
    pub fn new(worker_count: usize, classifier: ShellClassifier) -> Self {
        let column_source = if classifier.is_vanilla() {
            ColumnSource::Streaming
        } else {
            ColumnSource::Complete
        };
        tracing::info!(
            target: "mesh",
            requested_workers = worker_count,
            budget_ms = BROWSER_MESH_BUDGET.as_millis(),
            "browser mesh scheduler: no worker threads (thread::spawn traps on wasm32);              meshing in-frame under a time budget"
        );
        Self {
            queue: std::collections::VecDeque::new(),
            ready: Vec::new(),
            classifier,
            column_source,
            cutout_leaves: true,
            blend_radius: BLEND_RADIUS,
        }
    }

    /// Whether the world this scheduler meshes has all its columns already.
    #[must_use]
    pub fn column_source(&self) -> ColumnSource {
        self.column_source
    }

    /// See the native scheduler's method of the same name.
    pub fn set_cutout_leaves(&mut self, value: bool) {
        self.cutout_leaves = value;
    }

    /// See the native scheduler's method of the same name.
    #[must_use]
    pub fn cutout_leaves(&self) -> bool {
        self.cutout_leaves
    }

    /// See the native scheduler's method of the same name.
    pub fn set_blend_radius(&mut self, value: i32) {
        self.blend_radius = value;
    }

    /// See the native scheduler's method of the same name.
    #[must_use]
    pub fn blend_radius(&self) -> i32 {
        self.blend_radius
    }

    /// Queue a snapshot for meshing. O(1) — no meshing happens here.
    pub fn submit(&mut self, snapshot: SectionSnapshot) {
        self.queue.push_back(snapshot);
    }

    /// Number of submitted jobs not yet drained.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.queue.len() + self.ready.len()
    }

    /// No-op here: the browser scheduler is a strict FIFO `VecDeque` on one
    /// thread, so a section is always meshed in submission order and never
    /// needs the native pool's staleness map — see
    /// `MeshScheduler::forget_generation`'s native counterpart, and
    /// `Self::drain`'s own doc for why ordering cannot invert in this arm.
    pub fn forget_generation(&mut self, _key: &SectionKey) {}

    /// Mesh for at most [`BROWSER_MESH_BUDGET`] and return what got finished.
    ///
    /// **Meshes at least one section whenever the queue is non-empty, even if the
    /// budget is already spent.** Without that floor a machine slow enough to blow
    /// the budget on a single section would return an empty `Vec` forever and the
    /// world would never appear — a livelock that looks exactly like "meshing is
    /// broken". Checking the deadline *after* each section rather than before is what
    /// gives the floor for free.
    pub fn drain(&mut self) -> Vec<Meshed> {
        let mut out = std::mem::take(&mut self.ready);
        let deadline = crate::platform::Instant::now() + BROWSER_MESH_BUDGET;
        while let Some(snap) = self.queue.pop_front() {
            out.push(mesh_one(
                snap,
                &self.classifier,
                self.cutout_leaves,
                self.blend_radius,
            ));
            if crate::platform::Instant::now() >= deadline {
                break;
            }
        }
        out
    }

    /// Mesh until at least `n` results exist (or the queue empties), ignoring the
    /// budget.
    ///
    /// The budget is deliberately *not* applied: this method's native counterpart
    /// blocks the caller, its two callers are both outside the frame loop (the
    /// headless one-shot path and `Sim::end_session`'s flush), and a caller that
    /// asked for `n` meshes and got fewer because a clock ran out would have no way
    /// to make progress. Honouring the request is the same contract the native arm
    /// has.
    pub fn drain_blocking(&mut self, n: usize) -> Vec<Meshed> {
        while self.ready.len() < n {
            let Some(snap) = self.queue.pop_front() else {
                break;
            };
            let meshed = mesh_one(
                snap,
                &self.classifier,
                self.cutout_leaves,
                self.blend_radius,
            );
            self.ready.push(meshed);
        }
        std::mem::take(&mut self.ready)
    }
}

// ---------------------------------------------------------------------------
// Terrain meshing as ECS state (Stage 4)
// ---------------------------------------------------------------------------

/// Budget for [`heal_dirty_columns`]: max columns to re-mesh per frame.
/// Was 4 before crossbeam MPMC + full-core workers — the old mutex-contended
/// pool couldn't keep up with more. Now each column's sections fan out across
/// all cores via lock-free MPMC, so draining the full backlog every frame is
/// both safe and correct: duplicate dirty signals are coalesced by
/// [`DirtyColumns`], and the worker pool absorbs the burst.
///
/// The budget being finite is exactly why [`DirtyColumns`]' *order* matters:
/// whatever does not fit waits a frame, so the queue decides which part of the
/// world appears first.
pub const DIRTY_COLUMN_BUDGET: usize = 64;

/// Half-angle, in degrees, of the horizontal cone [`DirtyColumns`] treats as
/// "the player is looking at this column".
///
/// The same 60° (120° total) the server's join scheduler picks
/// (`lodestone_server::join_scheduler::FRUSTUM_HALF_ANGLE_DEGREES`), and for the
/// same reason: vanilla's default 70° *vertical* FOV is about 106° horizontal at
/// 16:9, so this is the real view plus a margin — a column about to rotate into
/// view should already have been meshed. **Deliberately a second copy of that
/// constant rather than an import**: the shell must not depend on
/// `lodestone-server` (the version seam, and singleplayer is the only build where
/// both exist), so what is shared is the semantics, stated here and in
/// `docs/section-mesh-invalidation.md`.
const MESH_FRUSTUM_HALF_ANGLE_DEGREES: f32 = 60.0;

/// How finely a yaw is quantised before it counts as "the player turned" — 16
/// sectors of 22.5°, again mirroring the server's `YAW_SECTORS`.
///
/// A *re-sort trigger*, not part of the ordering: the frustum test uses the raw
/// yaw. Quantising is what keeps re-prioritisation off the per-frame path — a
/// player panning smoothly re-keys the queue ~16 times per revolution instead of
/// on every frame.
const MESH_YAW_SECTORS: f32 = 16.0;

/// Chebyshev (chess-king) ring index of `coord` around `centre`.
#[must_use]
fn column_ring_distance(centre: (i32, i32), coord: (i32, i32)) -> i32 {
    (coord.0 - centre.0).abs().max((coord.1 - centre.1).abs())
}

/// Whether `coord` lies in the horizontal cone a player at column `centre`
/// facing `yaw_degrees` can see. Vanilla's yaw convention — 0 looks towards
/// `+Z`, 90 towards `−X`, the same one [`lodestone_physics::PlayerState::yaw`]
/// and `camera_rig`'s `Camera::forward` use.
///
/// The player's own column and its eight neighbours are always in view: the
/// direction to them is degenerate or dominated by where in the column the
/// player is standing, and they are the ground under their feet either way.
#[must_use]
fn column_in_frustum(centre: (i32, i32), yaw_degrees: f32, coord: (i32, i32)) -> bool {
    if column_ring_distance(centre, coord) <= 1 {
        return true;
    }
    if !yaw_degrees.is_finite() {
        return true;
    }
    let yaw = yaw_degrees.to_radians();
    let (fx, fz) = (-yaw.sin(), yaw.cos());
    let (dx, dz) = ((coord.0 - centre.0) as f32, (coord.1 - centre.1) as f32);
    let len = (dx * dx + dz * dz).sqrt();
    if len == 0.0 {
        return true;
    }
    ((fx * dx + fz * dz) / len) >= MESH_FRUSTUM_HALF_ANGLE_DEGREES.to_radians().cos()
}

/// The mesh queue's ordering: **distance first, facing cone second**.
///
/// A sort key rather than a comparator, so the order is a total, deterministic
/// function of integers — `(ring, penalty, cx, cz)`, the shape of the server's
/// `join_scheduler::view_order_key` (a *set* ordering: there is no prior walk to
/// inherit as a tie-break, so the coordinate is one).
///
/// * `ring` — Chebyshev distance from the player's column. **Primary, and that
///   is the anti-starvation property**: a column at distance `d` behind the
///   player (`(d, 1, …)`) still precedes every column at distance `d + 1`,
///   in view or not (`(d + 1, 0, …)`). Pure frustum-first would let a slow spin
///   starve what is behind the player, who then turns round into a hole.
/// * `penalty` — `0` inside the facing cone, `1` outside. The whole of "mesh
///   where the player is looking": it reorders *within* one ring and can never
///   promote a far column over a near one.
/// * `(cx, cz)` — a deterministic tie-break. With `facing: None` this key is
///   `(ring, 0, cx, cz)`, i.e. ring-by-ring and lexicographic inside a ring.
#[must_use]
fn mesh_order_key(
    centre: (i32, i32),
    facing: Option<f32>,
    coord: (i32, i32),
) -> (i32, u8, i32, i32) {
    let penalty = match facing {
        Some(yaw) if column_in_frustum(centre, yaw, coord) => 0,
        Some(_) => 1,
        None => 0,
    };
    (
        column_ring_distance(centre, coord),
        penalty,
        coord.0,
        coord.1,
    )
}

/// The quantised yaw sector a rotation falls in — see [`MESH_YAW_SECTORS`].
#[must_use]
fn mesh_yaw_sector(yaw_degrees: f32) -> i32 {
    if !yaw_degrees.is_finite() {
        return 0;
    }
    let wrapped = yaw_degrees.rem_euclid(360.0);
    (wrapped / (360.0 / MESH_YAW_SECTORS)).floor() as i32
}

/// The set of columns whose boundary geometry is stale, ordered so the
/// [`DIRTY_COLUMN_BUDGET`] is spent **near the player and in front of them
/// first**.
///
/// # Why this is not a `BTreeSet` any more
///
/// It was one, drained with `pop_first()` — i.e. lexicographically, smallest
/// `cx` then smallest `cz`. That is a corner of the world, not a place the
/// player is: a backlog (which `heal_dirty_columns` logs, and which forms on
/// every join) was therefore worked from `−x/−z` outward regardless of where the
/// camera pointed, so the server streaming its columns view-first
/// (`lodestone_server::join_scheduler`) reached no pixels in that order. The
/// visible symptom is chunks appearing behind you while you stare at a hole.
///
/// # Structure, and why this one
///
/// A `BinaryHeap` of [`mesh_order_key`] keys plus a `HashSet` of membership:
///
/// * the set is the **truth**, and it is what keeps "a column enqueued twice is
///   meshed once" — the property the `BTreeSet` gave for free. `insert` only
///   pushes a key when the set did not already hold the coordinate, and
///   [`pop_next`](Self::pop_next) is the only thing that takes one out;
/// * [`remove`](Self::remove) (a column that left the view) drops from the set
///   and leaves the heap entry as a **tombstone**, skipped on pop and compacted
///   when the heap grows past twice the live count. A heap has no cheap
///   arbitrary erase and this is a queue, so paying for one on every unload
///   would be the wrong trade;
/// * re-keying is a **rebuild**, not a per-frame sort, and it only happens when
///   the player's column or quantised yaw sector actually changed
///   ([`reprioritise`](Self::reprioritise)). At a 32-chunk view that is ≤ 4,225
///   integer keys, microseconds, ~16 times per revolution.
///
/// A plain sorted `Vec` was the alternative and loses: dirty columns arrive
/// continuously (every chunk arrival dirties up to eight), and an insert into a
/// sorted `Vec` is `O(n)` where the heap's is `O(log n)`.
#[derive(Debug, Default)]
pub struct DirtyColumns {
    /// The live set — membership, dedup, and [`len`](Self::len).
    queued: HashSet<(i32, i32)>,
    /// Best-first over [`mesh_order_key`]. May hold tombstones for coordinates
    /// no longer in `queued`; see the type doc.
    heap: BinaryHeap<Reverse<(i32, u8, i32, i32)>>,
    /// The player's column, as the keys in `heap` were computed against.
    centre: (i32, i32),
    /// The player's yaw in degrees, or `None` for "no rotation known" — a
    /// distinct state from any particular yaw, under which the ordering is
    /// distance-only and the facing cone is inert.
    facing: Option<f32>,
    /// The quantised sector `facing` was last re-keyed at — see
    /// [`MESH_YAW_SECTORS`].
    sector: Option<i32>,
}

impl DirtyColumns {
    /// Queue `coord` for a boundary re-mesh. Idempotent: a coordinate already
    /// queued is not queued twice, and will be meshed once.
    pub fn insert(&mut self, coord: (i32, i32)) -> bool {
        if !self.queued.insert(coord) {
            return false;
        }
        self.heap
            .push(Reverse(mesh_order_key(self.centre, self.facing, coord)));
        true
    }

    /// Drop `coord` from the queue — the column left the view, so the budget
    /// spent on it would go to a `mesh_column` that early-returns.
    pub fn remove(&mut self, coord: (i32, i32)) -> bool {
        if !self.queued.remove(&coord) {
            return false;
        }
        // Bound the tombstones: an unload sweep names a whole strip, and without
        // this the heap would keep every one of them for the session.
        if self.heap.len() > 2 * self.queued.len().max(32) {
            self.rebuild();
        }
        true
    }

    /// The highest-priority column, or `None` when the queue is empty.
    ///
    /// Skips tombstones left by [`remove`](Self::remove) — a popped key whose
    /// coordinate is no longer in the live set is not a column, it is a hole.
    pub fn pop_next(&mut self) -> Option<(i32, i32)> {
        while let Some(Reverse((_, _, cx, cz))) = self.heap.pop() {
            if self.queued.remove(&(cx, cz)) {
                return Some((cx, cz));
            }
        }
        None
    }

    /// Re-key the queue for a player who has moved to column `centre` or turned
    /// to `facing` (degrees of yaw), returning whether anything was re-ordered.
    ///
    /// **A no-op — and specifically not a rebuild — when neither the centre
    /// column nor the quantised yaw sector changed**, which is the common case on
    /// the frame this is called from. That gate is the whole of "re-prioritisation
    /// must be cheap".
    pub fn reprioritise(&mut self, centre: (i32, i32), facing: Option<f32>) -> bool {
        let sector = facing.map(mesh_yaw_sector);
        if self.centre == centre && self.sector == sector {
            // Keep the *old* yaw: it is what the current keys were computed
            // from, and storing a sub-sector nudge would make them disagree with
            // the ordering they are supposed to describe.
            return false;
        }
        self.centre = centre;
        self.facing = facing;
        self.sector = sector;
        self.rebuild();
        true
    }

    /// How many columns are queued.
    #[must_use]
    pub fn len(&self) -> usize {
        self.queued.len()
    }

    /// Whether anything is queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.queued.is_empty()
    }

    /// Whether `coord` is queued.
    #[must_use]
    pub fn contains(&self, coord: (i32, i32)) -> bool {
        self.queued.contains(&coord)
    }

    /// Forget everything queued (session teardown).
    pub fn clear(&mut self) {
        self.queued.clear();
        self.heap.clear();
    }

    /// Re-derive every heap key from the live set — the only way the heap
    /// changes shape, and it drops tombstones on the way through.
    fn rebuild(&mut self) {
        let (centre, facing) = (self.centre, self.facing);
        self.heap = self
            .queued
            .iter()
            .map(|&coord| Reverse(mesh_order_key(centre, facing, coord)))
            .collect();
    }
}

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

/// Per-frame relighting work reported to the live benchmark dump.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RelightWorkload {
    pub input_blocks: usize,
    pub input_sections: usize,
    pub cells_visited: usize,
    pub cells_changed: usize,
    pub dirty_sections: usize,
    pub remesh_invalidations_enqueued: usize,
    pub remesh_invalidations_coalesced: usize,
    pub remesh_sections_submitted: usize,
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
    ///
    /// Ordered **near the player and in front of them first**, not
    /// lexicographically — see [`DirtyColumns`] for what that fixed and why the
    /// container is no longer a `BTreeSet`.
    pub dirty_columns: DirtyColumns,
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
    /// Sections whose **light** the client's own relight just changed, coalesced
    /// and drained on a budget by [`relight_changed_blocks`].
    ///
    /// Separate from [`Self::dirty_columns`] because the granularity is the point.
    /// A relight around one broken block touches a handful of sections; expressing
    /// that as a column dirty would re-mesh 24 sections each snapshotting a
    /// 27-section neighbourhood, per break, which is the cost the whole bounded-box
    /// design exists to avoid.
    ///
    /// Absolute `(chunk_x, chunk_z, section_y)`, exactly what
    /// [`lodestone_world::Relit::dirty_sections`] reports — converted to a
    /// [`SectionKey`] at drain time, when the store's extent is known.
    pub light_dirty_sections: BTreeSet<(i32, i32, i32)>,
    /// Work completed by [`relight_changed_blocks`] since the app sampled it.
    relight_workload: RelightWorkload,
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
    /// horizontal neighbour column had not arrived.
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
    /// The live biome registry's ordered entry names (follow-up),
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
            dirty_columns: DirtyColumns::default(),
            forced_columns: BTreeSet::new(),
            departed: HashSet::new(),
            light_dirty_sections: BTreeSet::new(),
            relight_workload: RelightWorkload::default(),
            pending_removals: Vec::new(),
            uploaded_sections: HashSet::new(),
            drops: 0,
            deferred: 0,
            policy: MeshPolicy::default(),
            biome_names: Arc::from([]),
        }
    }

    /// Consume relighting work completed since the previous frame sample.
    pub(crate) fn take_relight_workload(&mut self) -> RelightWorkload {
        std::mem::take(&mut self.relight_workload)
    }

    /// Route one section's snapshot outcome: submit it, drop its stale geometry,
    /// or hold it back. Returns whether anything was submitted.
    ///
    /// **Vanilla's rule, and the reason it has two halves.**
    /// Vanilla's own level-extractor extract routine compiles a dirty section when
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

    /// Sets `options.cutoutLeaves` and, only on a real change, re-meshes
    /// every currently-loaded column with the new value — vanilla's own
    /// `operateOnLevelExtractor(LevelExtractor::allChanged)` for this option.
    ///
    /// **The equality guard is load-bearing, not an optimisation.** Called
    /// every frame ([`crate::sim::Sim::set_cutout_leaves`]'s own doc), so
    /// without it every frame would re-mesh every loaded column — the
    /// present-mode poll's exact reasoning, applied to a far more expensive
    /// operation.
    ///
    /// Derives "every currently-loaded column" from
    /// [`Self::uploaded_sections`]'s keys rather than tracking a separate
    /// column set: that field is already every `SectionKey` this session has
    /// handed out for GPU upload and not yet had removed (its own doc), so a
    /// second list would be one more thing that could drift from it.
    pub fn set_cutout_leaves(&mut self, value: bool, store: &ChunkWorld) {
        if self.scheduler.cutout_leaves() == value {
            return;
        }
        self.scheduler.set_cutout_leaves(value);
        self.remesh_every_loaded_column(store);
    }

    /// Sets `options.biomeBlendRadius` and, only on a real change, re-meshes
    /// every currently-loaded column — vanilla's own
    /// `operateOnLevelExtractor(LevelExtractor::allChanged)` for this option
    /// too, and the same equality-guard reasoning as
    /// [`Self::set_cutout_leaves`]: this is polled every frame, so without the
    /// guard every frame would re-mesh the world.
    ///
    /// The blend window is per-*vertex* tint state baked into the mesh, so
    /// unlike a uniform there is no way to apply a new radius to geometry
    /// already uploaded — a remesh is the mechanism, not a heavy-handed
    /// version of one.
    pub fn set_blend_radius(&mut self, value: i32, store: &ChunkWorld) {
        if self.scheduler.blend_radius() == value {
            return;
        }
        self.scheduler.set_blend_radius(value);
        self.remesh_every_loaded_column(store);
    }

    /// Force-remesh every column this session currently has uploaded.
    ///
    /// Extracted because three callers now want exactly this — the two option
    /// setters above and [`Self::reload_classifier`] — and a fourth copy of the
    /// "derive the loaded set from `uploaded_sections`" walk would be one more
    /// thing that could drift from the others. `uploaded_sections` is every
    /// `SectionKey` this session has handed out for GPU upload and not yet had
    /// removed (its own doc), so it is the loaded set rather than a second
    /// list tracking it.
    fn remesh_every_loaded_column(&mut self, store: &ChunkWorld) {
        let columns: std::collections::BTreeSet<(i32, i32)> = self
            .uploaded_sections
            .iter()
            .map(|key| (key.cx, key.cz))
            .collect();
        for (cx, cz) in columns {
            self.mesh_column_forced(store, cx, cz);
        }
    }

    /// Respawns the worker pool against `classifier` — and therefore whatever
    /// atlas it carries — and force-remeshes every currently loaded column
    /// against it. The mesh-side half of a live resource-pack reload
    /// (`crate::sim::Sim::reload_resource_pack_atlas` is the caller); same
    /// "derive the loaded set from `uploaded_sections`, force every column"
    /// shape as [`Self::set_cutout_leaves`] just above, for the same reason:
    /// a fresh atlas moves every sprite's UVs, so re-submitting the *same*
    /// baked geometry without re-meshing would leave the world sampling the
    /// new atlas at the old atlas's coordinates — a visibly wrong texture,
    /// not a missing one.
    ///
    /// Unlike `set_cutout_leaves` there is no cheap equality guard here:
    /// `ShellClassifier` carries an `Arc<BlockAtlas>` with no `PartialEq`, and
    /// the caller already gates this on the pack-selection generation
    /// actually changing (`crate::resources::pack_generation`), so a second
    /// guard here would only ever see `true`.
    ///
    /// The old pool's worker threads are joined by [`MeshScheduler`]'s own
    /// `Drop` the moment the assignment below replaces `self.scheduler` — any
    /// job still queued or in flight for the *old* atlas is simply abandoned;
    /// every section it would have produced is re-submitted below against the
    /// *new* one, so nothing is lost, only redone. `cutout_leaves` is read
    /// and `blend_radius` are read off the outgoing scheduler and carried onto
    /// the new one so a live pack reload cannot silently reset the user's
    /// FAST-leaves or Biome Blend settings back to `MeshScheduler::new`'s
    /// defaults.
    pub fn reload_classifier(
        &mut self,
        store: &ChunkWorld,
        worker_count: usize,
        classifier: ShellClassifier,
    ) {
        let cutout_leaves = self.scheduler.cutout_leaves();
        let blend_radius = self.scheduler.blend_radius();
        let mut scheduler = MeshScheduler::new(worker_count, classifier);
        scheduler.set_cutout_leaves(cutout_leaves);
        scheduler.set_blend_radius(blend_radius);
        self.column_source = scheduler.column_source();
        self.scheduler = scheduler;
        self.remesh_every_loaded_column(store);
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
    /// own enable-chunk-light routine dirties on chunk arrival
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
    /// server has just told us to forget.
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
        self.dirty_columns.remove((cx, cz));
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
            // Otherwise a section this column never returns to leaves a
            // permanent entry in the staleness map — harmless (a generation
            // number that will never be compared against again), but there is
            // no reason to keep it.
            self.scheduler.forget_generation(&key);
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
        self.light_dirty_sections.clear();
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
///
/// # Where the budget is spent
///
/// The queue is re-keyed here, once per frame, from the local player's own pose
/// (`lodestone_physics::PlayerState`'s `position` and `yaw` — the same values
/// mouse-look writes each frame, so this is the *live* facing and not last
/// tick's). [`DirtyColumns::reprioritise`] is a no-op unless the player's column
/// or yaw sector actually moved, so this costs a comparison on a typical frame.
///
/// This is the client half of the server's view-first streaming
/// (`lodestone_server::join_scheduler`): with the queue keyed lexicographically
/// the server's careful ordering reached no pixels, because a backlog was worked
/// from the `−x/−z` corner of the world whatever the camera was pointing at.
#[tracing::instrument(skip_all, fields(dirty = terrain.dirty_columns.len(), forced = terrain.forced_columns.len()))]
pub fn heal_dirty_columns(
    store: Res<ChunkWorld>,
    mut terrain: ResMut<TerrainMesh>,
    view: Query<&PhysicsState, With<LocalPlayer>>,
) {
    // `iter().next()` rather than `single()`: a harness with no local player is a
    // legitimate configuration (`TerrainPlugin` inserts no player entity), and it
    // means exactly "no view known" — under which the ordering falls back to the
    // stored centre and no facing.
    if let Some(state) = view.iter().next() {
        let centre = (
            (state.0.position.x.floor() as i32).div_euclid(16),
            (state.0.position.z.floor() as i32).div_euclid(16),
        );
        terrain
            .dirty_columns
            .reprioritise(centre, Some(state.0.yaw));
    }
    for _ in 0..DIRTY_COLUMN_BUDGET {
        let Some((cx, cz)) = terrain.forced_columns.pop_first() else {
            break;
        };
        terrain.mesh_column_forced(&store, cx, cz);
    }
    for _ in 0..DIRTY_COLUMN_BUDGET {
        let Some((cx, cz)) = terrain.dirty_columns.pop_next() else {
            break;
        };
        terrain.mesh_column(&store, cx, cz);
    }
    // The budget was depleted with work still queued, i.e. this frame's terrain is
    // a *choice* of which columns to mesh — which is what the ordering above is
    // for. Checked after the loop, not inside its `else`: in the `else` the queue
    // is empty by construction, so the old placement could never fire.
    if !terrain.dirty_columns.is_empty() {
        static BACKLOG_FRAME_COUNT: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(0);
        let n = BACKLOG_FRAME_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // Throttle: roughly every two seconds at 120 fps.
        if n % 240 == 0 {
            tracing::warn!(
                "mesh column backlog: {} dirty columns waiting (heal budget is {} forced + {} \
                 dirty), {} backlogged frames",
                terrain.dirty_columns.len(),
                DIRTY_COLUMN_BUDGET,
                DIRTY_COLUMN_BUDGET,
                n,
            );
        }
    }
}

/// Budget for [`relight_changed_blocks`]: max sections to re-mesh per frame after a
/// relight.
///
/// Larger than [`DIRTY_COLUMN_BUDGET`] because the unit is smaller — a *section*, not
/// a whole column — and because the latency matters more: a black hole where a block
/// used to be is the symptom the relight exists to remove, so the sections around the
/// break should land in the frame after the break rather than queue behind a streaming
/// backlog. One break typically reports fewer sections than this, so the budget only
/// engages on a bulk edit.
pub const LIGHT_DIRTY_SECTION_BUDGET: usize = 24;

/// The 26.2 [`lodestone_world::LightProperties`] for a live session: the per-state
/// dampening and emission census, read straight out of rodata.
///
/// A zero-sized adapter rather than a table, so the relight has no per-call setup.
/// See [`lodestone_data::light_props`] for the provenance argument, and in particular
/// that every gap in it darkens rather than brightens — which is why a state we cannot
/// resolve can never fake a bright cell.
#[derive(Debug)]
struct VanillaLightProps;

impl lodestone_world::LightProperties for VanillaLightProps {
    fn opacity(&self, state: u32) -> u8 {
        lodestone_data::light_props::dampening(state)
    }

    fn emission(&self, state: u32) -> u8 {
        lodestone_data::light_props::emission(state)
    }
}

/// `Update` / [`FrameSet::Terrain`]: run the client's own light engine over the block
/// changes applied since last frame, then re-mesh what that changed.
///
/// **This is vanilla's own client-level tick calling its own poll-light-updates and
/// run-light-updates routines**, and without it a block broken on a real
/// vanilla server leaves a pitch-black hole — permanently, because
/// vanilla's own broadcast-changes routine sends its own light-update packet only to
/// `getPlayers(pos, true)`, the players for whom that chunk is on the *outer ring* of
/// their loaded area. The breaker is never on their own chunk's border, so no light
/// packet is coming. See [`lodestone_world::relight`] for the full argument.
///
/// # Why the re-mesh half is not optional
///
/// A relight that changes light and dirties no mesh changes nothing on screen — the
/// dominant defect class in this repo. The block-update path already dirties the 3×3×3
/// around the changed cell, but a relight reaches further (up to
/// [`lodestone_world::relight::AFFECTED_RADIUS`]) and, more importantly, runs a frame
/// *after* that dirty signal was serviced. So the sections the relight itself reports
/// are re-meshed here, budgeted, which is vanilla's own
/// set-section-dirty-with-neighbors routine on the light path.
///
/// # Why `Option<Res<ChunkWorldWrite>>`
///
/// [`TerrainPlugin`] inserts only the read handle, because the write side belongs to
/// the session owner. A harness that installs the plugin and nothing else has no world
/// to write, and that is a legitimate configuration meaning exactly "nothing is
/// applying block changes" — not a panic.
pub fn relight_changed_blocks(
    write: Option<Res<ChunkWorldWrite>>,
    store: Res<ChunkWorld>,
    mut terrain: ResMut<TerrainMesh>,
    mut last_corrections: bevy_ecs::system::Local<(u64, u64)>,
) {
    let Some(write) = write else {
        return;
    };
    // The relight's block-state ids must be the store's. `ColumnSource::Streaming` is
    // the live-vanilla session — the same fact `MeshScheduler::new` derives from
    // `classifier.is_vanilla()` — so it is also the discriminator for which props
    // table applies. Running the 26.2 census against the demo palette would not fail,
    // it would light the demo world from an unrelated table.
    let vanilla_ids = terrain.column_source == ColumnSource::Streaming;
    // The dimension's own `has_skylight`, arrived at the same way the mesher resolves
    // an absent sky sample. The two must agree: the relight reads stored light through
    // this rule and the mesher renders it through the same one.
    let has_skylight = matches!(terrain.policy.sky_default, SkyDefault::Full);

    let (relit, corrections) = {
        // The write guard is held for the relight and dropped before anything
        // reaches for the read handle — `mesh_section` takes a read lock, and the
        // store is one `RwLock`.
        let mut world = write.write();
        let relit = if vanilla_ids {
            world.run_pending_relight(&VanillaLightProps, has_skylight)
        } else {
            world.run_pending_relight(&crate::blocks::DemoLightProps, has_skylight)
        };
        // Read under the same guard as the drain, so the pair describes one moment.
        (relit, world.light_correction_counts())
    };
    let merged = corrections.0.saturating_sub(last_corrections.0);
    let cancelled = corrections.1.saturating_sub(last_corrections.1);
    *last_corrections = corrections;

    if relit.dropped > 0 {
        tracing::warn!(
            target: "light",
            dropped = relit.dropped,
            "client relight dropped a job above its cell ceiling; a shaft that deep \
             stays lit by whatever the server last sent"
        );
    }
    if relit.jobs > 0 {
        tracing::debug!(
            target: "light",
            jobs = relit.jobs,
            cells_visited = relit.cells_visited,
            cells_changed = relit.cells_changed,
            sections = relit.dirty_sections.len(),
            deferred = relit.deferred,
            // Which props table the recompute read. A live session that reports
            // `false` here is lighting 26.2 block-state ids from the offline demo
            // table, which is a whole-world wrong answer rather than a small one.
            vanilla_ids,
            has_skylight,
            // Server light corrections applied since the previous drain, and queued
            // relights they cancelled. Vanilla sends the breaker no light packet for
            // their own break (vanilla's own broadcast-changes routine restricts it to players
            // for whom the chunk is on the outer ring of their loaded area), so
            // `merged = 0` beside a relight is the expected reading — and a non-zero
            // one means the server *did* correct us and the result is still wrong,
            // which is a different defect.
            merged,
            cancelled,
            "client relight"
        );
        // One line per job, because the aggregate above cannot distinguish a
        // recompute that correctly darkened a hole from one that flooded an enclosed
        // room with daylight: `cells_changed` is the same number either way. The
        // signed split and the sky-source provenance are the discriminators.
        for job in &relit.detail {
            tracing::debug!(
                target: "light",
                at = ?job.change,
                changes = job.changes,
                region_min = ?job.region_min,
                region_max = ?job.region_max,
                sky_raised = job.sky_raised,
                sky_lowered = job.sky_lowered,
                max_sky_gain = job.max_sky_gain,
                block_raised = job.block_raised,
                block_lowered = job.block_lowered,
                max_block_gain = job.max_block_gain,
                sky_source_columns = job.sky_source_columns,
                // Sky sources this engine invented out of an absent section rather
                // than reading out of data the server sent. Non-zero underground,
                // beside a large `sky_raised`, is the flood.
                sky_sources_from_missing = job.sky_source_columns_from_missing,
                "client relight job"
            );
        }
    }
    let dirty_sections = relit.dirty_sections.len();
    let remesh_invalidations_enqueued = relit
        .dirty_sections
        .iter()
        .filter(|section| !terrain.light_dirty_sections.contains(section))
        .count();
    let workload = &mut terrain.relight_workload;
    workload.input_blocks += relit.input_blocks;
    workload.input_sections += relit.jobs;
    workload.cells_visited += relit.cells_visited;
    workload.cells_changed += relit.cells_changed;
    workload.dirty_sections += dirty_sections;
    workload.remesh_invalidations_enqueued += remesh_invalidations_enqueued;
    workload.remesh_invalidations_coalesced += dirty_sections - remesh_invalidations_enqueued;
    terrain.light_dirty_sections.extend(relit.dirty_sections);

    if terrain.light_dirty_sections.is_empty() {
        return;
    }
    let Some(extent) = store.extent() else {
        // No extent means no loaded column to mesh; keeping the queue would spend
        // the budget every frame on sections that cannot resolve.
        terrain.light_dirty_sections.clear();
        return;
    };
    let base_si = extent.min_y.div_euclid(16);
    // Re-meshes whose neighbourhood is short a column. `snapshot_section_in` leaves
    // such a slot's light `None` and `mesh_snapshot` then reads it through
    // `UniformLight::pre_light_bridge` — **full sky, no block light** — so every
    // face opening that way is lit at daylight regardless of what the light engine
    // computed. `route` submits it anyway once the section has been on screen, which
    // is vanilla's own rule, so this is not by itself a defect; it is the one way a
    // *correct* relight still reaches bright pixels, and it is invisible from the
    // relight's own counters. Counted here so a "breaking a block made everything
    // bright" report can be attributed to the mesh side or ruled out.
    let mut bridged = 0usize;
    let mut meshed = 0usize;
    for _ in 0..LIGHT_DIRTY_SECTION_BUDGET {
        let Some((cx, cz, sy)) = terrain.light_dirty_sections.pop_first() else {
            break;
        };
        let si = sy - base_si;
        if si < 0 || si as usize >= extent.section_count {
            continue;
        }
        let key = SectionKey {
            cx,
            cz,
            si: si as usize,
            min_y: extent.min_y,
        };
        meshed += 1;
        if (-1..=1).any(|dx| {
            (-1..=1).any(|dz| !store.contains_column(cx + dx, cz + dz))
        }) {
            bridged += 1;
        }
        terrain.mesh_section(&store, key, extent.section_count);
    }
    terrain.relight_workload.remesh_sections_submitted += meshed;
    if bridged > 0 {
        tracing::debug!(
            target: "light",
            bridged,
            meshed,
            "relight re-meshed sections whose neighbourhood is short a column; their \
             absent slots light at full sky"
        );
    }
}

/// Registers Stage 4's terrain state and its `Update` systems.
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
        // The client's own light engine, and the re-mesh that makes its result
        // visible. Registered here rather than in the session builder for the same
        // reason `heal_dirty_columns` is: it reads and writes only the terrain
        // resources this plugin owns.
        app.add_systems(Update, relight_changed_blocks.in_set(FrameSet::Terrain));
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
            // Irrelevant to this fixture's own policy, same rule as the other
            // fields this doc comment calls out.
            ambient_light_color: None,
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

    /// **A section with nothing in it must produce no snapshot at all.**
    ///
    /// The premise — *which* section is empty — is the whole difficulty, and this
    /// test got it wrong twice in the same way. It used to hard-code a section
    /// index; that was fixed to derive one from `surface_height(0, 0)`, which
    /// reads better but is the same mistake: a section is **16×16 columns**, and
    /// `surface_height(0, 0)` is the height of *one* of them. Any column in the
    /// chunk that reaches higher than the origin's puts geometry in the
    /// "guaranteed sky" section, and the feature stage duly did it — a birch tree
    /// at local (3, 13) reaches y82, twelve blocks above the origin's ground, so
    /// section 5 (y80–96) held leaves and the assertion failed with a real
    /// snapshot in hand.
    ///
    /// Deriving one index from one column cannot be repaired by picking a taller
    /// column either: chunk (0,0)'s own canopy reaches y82, so at
    /// `SECTION_COUNT = 6` (window y0–96) that chunk has **no** empty section at
    /// all. Any "the section above the terrain is sky" arithmetic is guessing.
    ///
    /// So the subject is *found* rather than computed — every section of every
    /// loaded chunk is classified all-air or not by direct scan, which makes the
    /// premise true by construction instead of by inference — and then both
    /// directions are asserted over the whole 3×3 world at once: every empty
    /// section must yield `None`, every non-empty one must yield `Some`. The two
    /// counts are asserted non-zero, because either half alone is satisfied by a
    /// `snapshot_section` that answers the same way always, and a world of all
    /// terrain or all sky would make one of them vacuous without saying so.
    #[test]
    fn empty_sky_section_is_skipped() {
        // Radius 1: 3×3 columns, so a chunk whose terrain does not reach the top
        // of the window is available even when the origin's does.
        let world = crate::worldgen::generate(1);
        let min_y = crate::worldgen::MIN_Y;

        let mut empty_sections = 0usize;
        let mut occupied_sections = 0usize;

        for cz in -1..=1 {
            for cx in -1..=1 {
                let loaded = world
                    .get(ChunkPos::new(cx, cz))
                    .expect("generate(1) loads the whole 3×3");
                let column = &loaded.column;
                for si in 0..crate::worldgen::SECTION_COUNT {
                    let base_y = min_y + (si as i32) * 16;
                    let mut occupied = None;
                    'scan: for lx in 0..16usize {
                        for lz in 0..16usize {
                            for ly in 0..16i32 {
                                let block = column.get_block(lx, base_y + ly, lz);
                                if block != crate::blocks::id::AIR {
                                    occupied = Some((lx, base_y + ly, lz, block));
                                    break 'scan;
                                }
                            }
                        }
                    }

                    let key = SectionKey {
                        cx,
                        cz,
                        si,
                        min_y,
                    };
                    let snapshot = snapshot_section(&world, key);
                    match occupied {
                        None => {
                            empty_sections += 1;
                            assert!(
                                snapshot.is_none(),
                                "section ({cx},{cz},{si}) is all air by direct \
                                 scan, so it must produce no snapshot — meshing \
                                 it costs a job and an upload per empty section \
                                 of every column"
                            );
                        }
                        Some((lx, y, lz, block)) => {
                            occupied_sections += 1;
                            assert!(
                                snapshot.is_some(),
                                "section ({cx},{cz},{si}) holds id {block} at \
                                 ({lx},{y},{lz}), so it must produce a snapshot — \
                                 skipping it is the invisible-terrain defect, not \
                                 an optimisation"
                            );
                        }
                    }
                }
            }
        }

        assert!(
            empty_sections > 0,
            "no loaded section was all air, so the skip path was never exercised"
        );
        assert!(
            occupied_sections > 0,
            "control: no loaded section had geometry, so `is_none()` above is \
             satisfied by a function that always returns None"
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

    /// **The bug 1 (grief-protection) reproduction.** Two jobs submitted for
    /// the *same* section — a client-predicted change, then a correction
    /// moments later — must never let the caller see the stale one,
    /// regardless of which order the pool's workers happen to finish them
    /// in. [`MeshScheduler`]'s own `latest_generation` field names the
    /// real-world trigger this guards: a predicted block break gets
    /// remeshed once immediately, then again when the server denies it and
    /// restores the original — and the pool's own doc already admits
    /// completion order is not submission order ("the channel distributes
    /// by actual work completion").
    ///
    /// This does not need to win a real thread race to make the point: a
    /// result superseded by a *later* `submit` for the same key is stale
    /// the moment that second `submit` happens, whether its own completion
    /// arrives before or after — so a single worker (strictly FIFO, no race
    /// needed at all) already exercises the generation check on its own.
    #[test]
    fn a_stale_completion_never_overwrites_a_fresher_one_for_the_same_section() {
        let classifier = ShellClassifier::Demo(DemoClassifier);
        assert_eq!(
            platform_snapshot(None).key,
            platform_snapshot(Some([12, 7, 12])).key,
            "fixture precondition: same section"
        );

        // Ground truth, meshed directly with no scheduler involved, so the
        // assertion below is a prediction rather than a tautology.
        let expected_stale = mesh_one(platform_snapshot(None), &classifier, true, BLEND_RADIUS)
            .mesh
            .quad_count();
        let expected_fresh =
            mesh_one(platform_snapshot(Some([12, 7, 12])), &classifier, true, BLEND_RADIUS)
            .mesh
            .quad_count();
        assert_ne!(
            expected_stale, expected_fresh,
            "fixture precondition: the two snapshots must mesh to different \
             geometry, or a version that always kept the *first* result would \
             pass this test too"
        );

        let mut scheduler = MeshScheduler::new(1, classifier);
        scheduler.submit(platform_snapshot(None));
        scheduler.submit(platform_snapshot(Some([12, 7, 12])));
        let results = scheduler.drain_blocking(2);

        assert_eq!(
            results.len(),
            1,
            "the stale completion must be dropped, not handed to the caller: \
             got {results:?}"
        );
        assert_eq!(
            results[0].mesh.quad_count(),
            expected_fresh,
            "the surviving mesh must be the later submission's geometry \
             ({expected_fresh} quads), not the superseded one ({expected_stale})"
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
                    sprite: 0,
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

    /// Chebyshev radius of the simulated tracking view, in columns, for the
    /// eviction pair. Small on purpose — the defect is about *unbounded growth*,
    /// which a short walk already separates from a bounded window by a factor of
    /// two.
    ///
    /// The heal-backlog test uses [`BACKLOG_RD`] instead, which is larger and
    /// derived; see its doc for why one radius could not serve both, and note
    /// that widening this one would cost every test in this harness real
    /// worldgen time (≈0.15 s per column, and the column count grows as
    /// `(WALK_STEPS + 2·rd + 1)(2·rd + 1)`).
    const WALK_RD: i32 = 3;
    /// How many columns the simulated player advances in `+x`.
    const WALK_STEPS: i32 = 12;

    /// Chebyshev view radius for
    /// [`standing_still_drains_the_heal_backlog_and_no_column_is_lost`], derived
    /// from [`DIRTY_COLUMN_BUDGET`] rather than chosen.
    ///
    /// # Why this cannot be a constant
    ///
    /// A backlog only forms if columns are dirtied faster than one frame's budget
    /// clears them, and at one frame per step there is exactly one moment in the
    /// walk when that can happen: **step 0**, where the whole view loads at once
    /// and every column of it is dirtied by a neighbour's arrival, offering
    /// `(2·rd + 1)²` columns against a single budget. Every later step is a
    /// frontier of `2·(2·rd + 1)` columns — 14 at `WALK_RD` — which no budget
    /// above about 15 can fall behind no matter how many steps are added. So the
    /// scenario size is a function of the budget, and `WALK_STEPS` is not a lever
    /// on it.
    ///
    /// Hard-coding `3` here is what silently voided the test when `17c786e`
    /// raised the budget from 4 to 64: a 49-column window drained inside one
    /// frame, `max_backlog` went from 45 (= 49 − 4, the figure in the #479
    /// record) to **0**, and the vacuity guard the author had written fired
    /// rather than the test passing quietly. This solves for `rd` instead, so
    /// the next budget change scales the scenario instead of breaking it:
    ///
    /// ```text
    /// (2·rd + 1)² > 2 · DIRTY_COLUMN_BUDGET
    /// ```
    ///
    /// One budget to drain in the first frame and more than one still queued
    /// after it, which is exactly what the guard requires. At 64 that gives
    /// `rd = 6` — a 169-column window, 325 columns visited, and a first-frame
    /// backlog of 105. A view distance of 6 is also a real setting a player can
    /// pick, so the scenario is not absurd; if a future budget pushed this past
    /// vanilla's maximum of 32 the honest conclusion would be that no reachable
    /// view distance can back the queue up, and the deferral bug it guards could
    /// no longer occur by that route.
    ///
    /// Floored at `WALK_RD` so a *smaller* budget keeps the original geometry.
    const BACKLOG_RD: i32 = {
        let mut rd = WALK_RD;
        while ((2 * rd + 1) * (2 * rd + 1)) as usize <= 2 * DIRTY_COLUMN_BUDGET {
            rd += 1;
        }
        rd
    };

    /// The columns a tracking view of radius `rd` centred on `(ccx, 0)` holds.
    fn window_rd(ccx: i32, rd: i32) -> BTreeSet<(i32, i32)> {
        let mut out = BTreeSet::new();
        for cx in (ccx - rd)..=(ccx + rd) {
            for cz in -rd..=rd {
                out.insert((cx, cz));
            }
        }
        out
    }

    /// [`window_rd`] at [`WALK_RD`], the eviction pair's radius.
    fn window(ccx: i32) -> BTreeSet<(i32, i32)> {
        window_rd(ccx, WALK_RD)
    }

    /// Every column that at some point during a radius-`rd` walk had all eight of
    /// its horizontal neighbours resident at once — i.e. exactly the columns that
    /// can ever have escaped [`SnapshotOutcome::Deferred`] and reached the GPU.
    ///
    /// Derived from the walk's geometry rather than measured, so it is an
    /// *outside* expectation: the two frontier columns in `x` (the first view's
    /// trailing ring, which is unloaded before the view ever advances past it,
    /// and the last view's leading ring) never qualify, and neither do the
    /// `z = ±rd` rows, since the view never moves in `z`. That is
    /// `(WALK_STEPS + 2·rd − 1) × (2·rd − 1)` columns out of
    /// `(WALK_STEPS + 2·rd + 1) × (2·rd + 1)` visited: 17 × 5 = 85 of 133 at
    /// [`WALK_RD`], 23 × 11 = 253 of 325 at [`BACKLOG_RD`].
    fn ever_interior_rd(rd: i32) -> BTreeSet<(i32, i32)> {
        let mut out = BTreeSet::new();
        for cx in (-rd + 1)..=(WALK_STEPS + rd - 1) {
            for cz in (-rd + 1)..=(rd - 1) {
                out.insert((cx, cz));
            }
        }
        out
    }

    /// [`ever_interior_rd`] at [`WALK_RD`], the eviction pair's radius.
    fn ever_interior() -> BTreeSet<(i32, i32)> {
        ever_interior_rd(WALK_RD)
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
        // Issue #423: the test edits the store, so it holds the write handle and
        // hands the paired read handle to the mesher — the same split production
        // (`drive_placement`) observes.
        let write = ChunkWorldWrite::new(World::new());
        let store = write.read_handle();
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
                write
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
                write.write().unload(ChunkPos::new(cx, cz));
                if evict {
                    terrain.forget_column(cx, cz);
                    terrain.force_neighbours_of_departed(&store, cx, cz);
                }
            }
            live = next;

            // Drain the heal queue completely rather than at
            // `DIRTY_COLUMN_BUDGET`: the subject is eviction, and leaving a
            // backlog would let a *starved* run masquerade as a bounded one.
            while let Some((cx, cz)) = terrain.dirty_columns.pop_next() {
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
    ///
    /// # The scenario is sized from the budget, not chosen
    ///
    /// This test runs at [`BACKLOG_RD`], not [`WALK_RD`], and that indirection is
    /// the whole reason it still means anything: with a hard-coded radius it went
    /// silently vacuous the moment `17c786e` raised [`DIRTY_COLUMN_BUDGET`] from
    /// 4 to 64, because a 49-column window drains inside one frame. Read
    /// `BACKLOG_RD`'s doc before touching either number.
    ///
    /// One consequence is worth stating rather than leaving to be discovered: at
    /// a 64-column budget the backlog is built by the *initial* view load and is
    /// gone within about three of the walk's own frames, so the standing-still
    /// phase now has nothing left to drain and reports **0 frames**. That is the
    /// truth at this budget, not a broken test — the frontier of a moving view is
    /// `2·(2·rd + 1)` columns and only a radius of 16 or more would outpace 64 per
    /// frame, which is a 1,089-column window and roughly four minutes of real
    /// worldgen. The standing loop stays as the bounded safety net that would
    /// catch a genuinely stuck queue, `frames_with_backlog` is what proves the
    /// queue carried work across frames, and `resident == expected` — the
    /// permanent-deferral evidence #479 turned on — is unaffected either way and
    /// now covers 132 columns rather than 30.
    #[test]
    fn standing_still_drains_the_heal_backlog_and_no_column_is_lost() {
        // One frame per step is deliberately the *worst* case the shell can
        // present: a column arrives and the heal system gets a single
        // `DIRTY_COLUMN_BUDGET` before the player has moved again. If the backlog
        // is going to diverge, it diverges here.
        const FRAMES_PER_STEP: usize = 1;

        // Predicted from outside the code under test, before it runs: step 0
        // loads the whole view at once and every column of it is dirtied by a
        // neighbour's arrival, so the first frame is offered `window` columns and
        // clears exactly `DIRTY_COLUMN_BUDGET` of them. Later frontiers are
        // `2·(2·rd+1)` columns, well inside one budget, so the queue only shrinks
        // from here and this is also the maximum over the whole walk.
        let view_columns = window_rd(0, BACKLOG_RD).len();
        // Checked before the subtraction, because an undersized scenario makes
        // that subtraction underflow and "attempt to subtract with overflow" is a
        // useless thing to hand whoever next changes the budget. Observed: with
        // `BACKLOG_RD` forced to `WALK_RD` this fires and names the cause, where
        // the bare subtraction panicked with nothing to act on.
        assert!(
            view_columns > 2 * DIRTY_COLUMN_BUDGET,
            "premise: a {view_columns}-column view cannot leave more than one \
             further {DIRTY_COLUMN_BUDGET}-column budget queued after the first \
             frame, so no backlog forms and this test would measure nothing. \
             `BACKLOG_RD` ({BACKLOG_RD}) must solve \
             (2·rd + 1)² > 2 · DIRTY_COLUMN_BUDGET."
        );
        let predicted_first_frame_backlog = view_columns - DIRTY_COLUMN_BUDGET;

        let write = ChunkWorldWrite::new(World::new());
        let store = write.read_handle();
        let mut terrain = streaming_terrain();
        let mut gpu: HashSet<SectionKey> = HashSet::new();
        let mut max_backlog = 0usize;
        // Frames that ended with work still queued — i.e. frames across which the
        // heal queue actually carried a column. See the assertion below.
        let mut frames_with_backlog = 0usize;

        let mut live = BTreeSet::new();
        for step in 0..=WALK_STEPS {
            let next = window_rd(step, BACKLOG_RD);
            for &(cx, cz) in next.difference(&live) {
                write
                    .write()
                    .load(ChunkPos::new(cx, cz), crate::worldgen::generate_column(cx, cz));
            }
            for &(cx, cz) in next.difference(&live) {
                terrain.mesh_column(&store, cx, cz);
                terrain.mark_neighbours_dirty(&store, cx, cz);
            }
            for &(cx, cz) in live.difference(&next) {
                write.write().unload(ChunkPos::new(cx, cz));
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
                    let Some((cx, cz)) = terrain.dirty_columns.pop_next() else {
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
                if !terrain.dirty_columns.is_empty() {
                    frames_with_backlog += 1;
                }
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
                let Some((cx, cz)) = terrain.dirty_columns.pop_next() else {
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
        let expected: BTreeSet<(i32, i32)> = ever_interior_rd(BACKLOG_RD)
            .intersection(&window_rd(WALK_STEPS, BACKLOG_RD))
            .copied()
            .collect();
        eprintln!(
            "backlog: max {max_backlog} (predicted {predicted_first_frame_backlog} \
             from a {view_columns}-column view against a {DIRTY_COLUMN_BUDGET}-column \
             budget at rd {BACKLOG_RD}), carried across {frames_with_backlog} \
             frames, {backlog_while_walking} on arrival at the last step, drained \
             in {frames_to_drain} standing frames; resident {} of {} expected",
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
        //
        // The *predicted* count, not merely "more than a budget": asserting the
        // sign of the thing would be satisfied by any backlog at all, and would
        // have gone on passing through a change that shrank the real one to a
        // single column. The two competing hypotheses are computed from outside
        // constants and this has to land on one of them — `view_columns −
        // DIRTY_COLUMN_BUDGET` if the first frame is offered the whole view (the
        // mechanism `BACKLOG_RD` is derived from), or `0` if the window drains
        // inside one frame, which is what the 4 → 64 budget change did.
        assert_eq!(
            max_backlog, predicted_first_frame_backlog,
            "the backlog peak must be the whole view minus one frame's budget \
             ({view_columns} − {DIRTY_COLUMN_BUDGET}). A max of 0 means the window \
             drains inside a single frame and this test exercised no queue at all \
             — re-derive `BACKLOG_RD` from `DIRTY_COLUMN_BUDGET` rather than \
             relaxing this."
        );
        assert!(
            max_backlog > DIRTY_COLUMN_BUDGET,
            "the walk never built a backlog past one frame's budget (max \
             {max_backlog}), so this test did not exercise the queue it claims to"
        );
        // And the queue really carried work *across frame boundaries*, which is
        // what "exercised the queue" has to mean — a peak measured inside a single
        // frame would say nothing about deferral surviving a frame. The floor is
        // derived, not picked: a `predicted_first_frame_backlog`-column queue
        // against a `DIRTY_COLUMN_BUDGET`-column budget cannot be cleared in
        // fewer than `ceil(predicted / budget)` further frames even with nothing
        // else arriving.
        let minimum_carrying_frames = predicted_first_frame_backlog.div_ceil(DIRTY_COLUMN_BUDGET);
        assert!(
            frames_with_backlog >= minimum_carrying_frames,
            "the backlog was carried across only {frames_with_backlog} frames, \
             but {predicted_first_frame_backlog} columns against a \
             {DIRTY_COLUMN_BUDGET}-column budget needs at least \
             {minimum_carrying_frames}"
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

    // -----------------------------------------------------------------------
    // The mesh drain's order
    // -----------------------------------------------------------------------

    /// Every column of a Chebyshev-radius-`r` window about the origin, generated
    /// in the **lexicographic** order the old `BTreeSet` drain used — so the
    /// insertion order below is exactly the order this queue has to *not* keep.
    fn window_columns(r: i32) -> Vec<(i32, i32)> {
        let mut out = Vec::new();
        for cx in -r..=r {
            for cz in -r..=r {
                out.push((cx, cz));
            }
        }
        out
    }

    /// **Where the heal budget goes.** Near-and-in-front first, and — the
    /// property that matters more — a near column *behind* the player still beats
    /// a far one in front of them, so no amount of looking one way starves the
    /// other.
    #[test]
    fn the_mesh_drain_prefers_near_and_in_front_but_never_starves_what_is_behind() {
        let radius = 5;
        let mut dirty = DirtyColumns::default();
        for coord in window_columns(radius) {
            dirty.insert(coord);
        }
        // Yaw 0 is due +Z in vanilla's convention, so this player is looking at
        // the columns with positive `cz`.
        assert!(dirty.reprioritise((0, 0), Some(0.0)));

        let mut order = Vec::new();
        while let Some(coord) = dirty.pop_next() {
            order.push(coord);
        }
        assert_eq!(
            order.len(),
            ((2 * radius + 1) * (2 * radius + 1)) as usize,
            "every queued column must come back out exactly once"
        );

        // 1. Distance is the primary key, stated as the property: the ring bands
        //    are contiguous, so nothing at distance `d + 1` can precede anything
        //    at distance `d`. Its concrete form — and the one a pure
        //    frustum-first drain fails — is the pair below.
        let mut previous = 0;
        for &coord in &order {
            let distance = column_ring_distance((0, 0), coord);
            assert!(
                distance >= previous,
                "{coord:?} at distance {distance} follows distance {previous}: the facing \
                 bonus must never promote a far column over a near one, or a player who \
                 turns round finds a hole that was deprioritised for as long as they looked \
                 away"
            );
            previous = distance;
        }
        let behind = order
            .iter()
            .position(|&c| c == (0, -3))
            .expect("the column three behind the player is in the window");
        let ahead = order
            .iter()
            .position(|&c| c == (0, 4))
            .expect("the column four in front of the player is in the window");
        assert!(
            behind < ahead,
            "a near column behind the player must be meshed before a far one in front \
             (behind at {behind}, ahead at {ahead})"
        );

        // 2. …and within one ring the facing cone really does win, or the feature
        //    is inert and assertion 1 would be satisfied by an ordering that
        //    ignores the player's rotation entirely. The whole in-frustum half of
        //    the ring precedes the whole out-of-frustum half.
        let ring: Vec<(i32, i32)> = order
            .iter()
            .copied()
            .filter(|&c| column_ring_distance((0, 0), c) == radius)
            .collect();
        let split = ring
            .iter()
            .position(|&c| !column_in_frustum((0, 0), 0.0, c))
            .expect("a 120° cone cannot contain a whole ring");
        assert!(
            ring[..split]
                .iter()
                .all(|&c| column_in_frustum((0, 0), 0.0, c)),
            "the in-frustum columns of a ring must form its prefix; ring 5 drained as {ring:?}"
        );
        assert_eq!(
            order.first(),
            Some(&(0, 0)),
            "the player's own column is meshed first; the old lexicographic drain started \
             at {:?} instead",
            (-radius, -radius)
        );

        // A column dirtied *after* the queue was keyed is keyed too, rather than
        // appended: this is what makes the ordering hold during streaming, when
        // arrivals and drains interleave every frame.
        dirty.insert((0, -5));
        dirty.insert((0, 2));
        assert_eq!(
            dirty.pop_next(),
            Some((0, 2)),
            "a fresh dirty signal joins the order at its priority, not at the end"
        );
    }

    /// Dedup and unload, the two properties the `BTreeSet` gave for free and a
    /// heap does not: a column dirtied twice is meshed once, and one that left the
    /// view is not meshed at all — its tombstone must not resurface as a phantom
    /// pop.
    #[test]
    fn a_column_dirtied_twice_is_meshed_once_and_an_unloaded_one_not_at_all() {
        let mut dirty = DirtyColumns::default();
        assert!(dirty.insert((2, 3)));
        assert!(!dirty.insert((2, 3)), "a repeat dirty signal must coalesce");
        assert_eq!(dirty.len(), 1);

        assert!(dirty.insert((4, 0)));
        assert!(dirty.remove((4, 0)), "the column left the view");
        assert!(!dirty.remove((4, 0)), "and it is gone only once");
        assert_eq!(dirty.len(), 1);
        assert!(dirty.contains((2, 3)));

        assert_eq!(dirty.pop_next(), Some((2, 3)));
        assert_eq!(
            dirty.pop_next(),
            None,
            "the removed column must not come back out of the heap"
        );
        assert!(dirty.is_empty());
    }

    /// Re-keying runs on every frame, so the common case must not rebuild: it
    /// fires when the player crosses a chunk boundary or turns into a new yaw
    /// sector, and does nothing otherwise.
    #[test]
    fn rekeying_only_fires_when_the_column_or_the_yaw_sector_moves() {
        let mut dirty = DirtyColumns::default();
        for coord in window_columns(3) {
            dirty.insert(coord);
        }
        assert!(
            dirty.reprioritise((0, 0), Some(0.0)),
            "the first known rotation is a change: the default is no facing at all"
        );
        assert!(
            !dirty.reprioritise((0, 0), Some(0.0)),
            "an identical column and yaw must not rebuild"
        );
        assert!(
            !dirty.reprioritise((0, 0), Some(10.0)),
            "a sub-sector nudge (10° of 22.5°) must not rebuild"
        );
        assert!(
            dirty.reprioritise((0, 0), Some(90.0)),
            "a quarter turn is a new sector"
        );
        assert!(
            dirty.reprioritise((1, 0), Some(90.0)),
            "crossing a chunk boundary re-centres the whole ordering"
        );

        // And the new centre is what the order is keyed on afterwards.
        let mut previous = 0;
        while let Some(coord) = dirty.pop_next() {
            let distance = column_ring_distance((1, 0), coord);
            assert!(distance >= previous, "{coord:?} is out of order about (1, 0)");
            previous = distance;
        }
    }
}
