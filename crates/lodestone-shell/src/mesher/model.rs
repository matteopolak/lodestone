//! Block-model meshing over a section snapshot.
//!
//! This module owns model-view adaptation, biome tint lookup, and the public
//! model-layer entry points. It deliberately reads only `SectionSnapshot`.
use super::*;

#[derive(Debug, Clone, Copy, Default)]
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
pub(crate) fn take_tint_probe() -> TintProbe {
    TINT_PROBE.with(|p| p.replace(TintProbe::default()))
}

/// Block-model and light lookups over one immutable snapshot.
struct SnapshotModelView<'a> {
    snapshot: &'a SectionSnapshot,
    models: &'a BlockModels,
    light: &'a SnapshotLight<'a>,
    /// Sliding tint samples are valid only for this immutable snapshot.
    tint: RefCell<BlendedTintCursor>,
    /// The live `options.cutoutLeaves` value this snapshot was meshed against —
    /// see [`Self::force_opaque_at`].
    cutout_leaves: bool,
    boundary_only: bool,
}

/// Answers the AO census for a raw id stored in a section snapshot.
///
/// Snapshots preserve the wire's raw global state ids, so this is the one
/// boundary that validates them before entering the total census API. An
/// invalid id is conservatively open, matching the out-of-neighbourhood path.
pub(crate) fn ao_occludes_raw_state(raw: u32) -> bool {
    lodestone_data::block_states::StateId::new(raw)
        .is_some_and(lodestone_data::shade_brightness::occludes_ambient_light)
}

/// Split a signed section coordinate into a neighbour offset (`dx ∈ {-1,0,1}`)
/// and a section-local index (`0..16`). Used to resolve a `cullface` probe that
/// steps one block past a section edge into the adjacent snapshot section.
pub(crate) fn split16(v: i32) -> (i32, usize) {
    (v.div_euclid(16), v.rem_euclid(16) as usize)
}

/// Biome-id → name, for the [`ChunkSection::biome_at_block`] id space **this
/// client's own server assigns** — `crates/protocol/v770/src/
/// server_protocol.rs`'s `BIOME_NAMES` (alphabetical over the 55 biomes the
/// embedded overworld generator can select; nether/end biomes aren't in the
/// servable set yet).
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
/// coordinate space used by the model and fluid views, or `None` past the
/// snapshotted 3×3×3
/// neighbourhood. `resolve_blended_tint`'s box blend only ever steps a couple
/// of blocks past the centre section, which is always within that
/// neighbourhood — see `split16`.
pub(crate) fn biome_name_at(snapshot: &SectionSnapshot, pos: BlockPos) -> Option<&'static str> {
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
    fn interior_quads_are_culled(&self) -> bool {
        self.boundary_only
    }

    fn quads_at(&self, x: usize, y: usize, z: usize) -> &[BakedQuad] {
        let raw = self.snapshot.at(0, 0, 0).get_block(x, y, z);
        let Some(state) = StateId::new(raw) else {
            return &[];
        };
        self.models.quads(state)
    }

    /// The complete per-state smooth-lighting gate: model JSON's
    /// `ambientocclusion` flag plus zero state emission.
    ///
    /// The trait default is `true`, which is what preserved behaviour while this
    /// was unwired — so **the flag mechanism was inert in the running game until
    /// this override existed**, exactly the island shape `CLAUDE.md` rule 1
    /// names. Mirrors `quads_at`'s lookup deliberately: same state id, same
    /// `BlockModels`, so a model whose flag says "flat" cannot disagree with the
    /// geometry it was baked alongside.
    ///
    /// The state id is validated at the snapshot boundary, then the renderer's
    /// model table combines the model flag with its canonical emission census.
    fn ambient_occlusion_at(&self, x: usize, y: usize, z: usize) -> bool {
        let raw = self.snapshot.at(0, 0, 0).get_block(x, y, z);
        let Some(state) = StateId::new(raw) else {
            return true;
        };
        self.models.ambient_occlusion(state)
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
        let raw = self.snapshot.at(dx, dy, dz).get_block(lx, ly, lz);
        let Some(state) = StateId::new(raw) else {
            return false;
        };
        self.models.occludes(state)
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
        let here_raw = self.snapshot.at(0, 0, 0).get_block(x as usize, y as usize, z as usize);
        let neighbour_raw = self.snapshot.at(ndx, ndy, ndz).get_block(nlx, nly, nlz);
        let (Some(here), Some(neighbour)) =
            (StateId::new(here_raw), StateId::new(neighbour_raw))
        else {
            return false;
        };
        self.models.skips_rendering_against(here, neighbour)
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
        let raw = self.snapshot.at(0, 0, 0).get_block(x, y, z);
        let Some(state) = StateId::new(raw) else {
            return false;
        };
        self.models.is_leaves(state)
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
        let raw = self.snapshot.at(0, 0, 0).get_block(x, y, z);
        if StateId::new(raw).is_some_and(|state| self.models.is_cauldron(state)) {
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
    /// `SectionSnapshot` stores raw global block-state ids (see `quads_at`).
    /// They are validated at this boundary before the O(1), allocation-free AO
    /// census lookup. An id past the snapshotted 3×3×3 neighbourhood or outside
    /// the state census reads as open — the same conservative answer
    /// `occludes_at` gives.
    fn ao_occludes_at(&self, x: i32, y: i32, z: i32) -> bool {
        let (dx, lx) = split16(x);
        let (dy, ly) = split16(y);
        let (dz, lz) = split16(z);
        if !(-1..=1).contains(&dx) || !(-1..=1).contains(&dy) || !(-1..=1).contains(&dz) {
            return false;
        }
        let id = self.snapshot.at(dx, dy, dz).get_block(lx, ly, lz);
        ao_occludes_raw_state(id)
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
    if center_is_quadless(snapshot, models) {
        return ModelMesh::default();
    }
    let light = SnapshotLight::new(snapshot);
    let view = SnapshotModelView {
        snapshot,
        models,
        light: &light,
        tint: RefCell::new(BlendedTintCursor::new(blend_radius)),
        cutout_leaves,
        boundary_only: center_interior_is_culled(snapshot, models),
    };
    mesh_models(&view)
}

/// Like [`mesh_snapshot_models`], but keeps
/// [`RenderLayer::Translucent`](lodestone_render::RenderLayer::Translucent)
/// blocks in a second mesh instead of folding them into the opaque/cutout one.
#[must_use]
pub fn mesh_snapshot_models_layers(
    snapshot: &SectionSnapshot,
    models: &BlockModels,
    cutout_leaves: bool,
    blend_radius: i32,
) -> (ModelMesh, ModelMesh) {
    let light = SnapshotLight::new(snapshot);
    mesh_snapshot_models_layers_with_light(snapshot, models, cutout_leaves, blend_radius, &light)
}

fn center_is_quadless(snapshot: &SectionSnapshot, models: &BlockModels) -> bool {
    snapshot.at(0, 0, 0).block_states().palette_values().is_some_and(|palette| {
        palette.iter().all(|&raw| {
            StateId::new(raw).is_some_and(|state| models.quads(state).is_empty())
        })
    })
}

fn center_interior_is_culled(snapshot: &SectionSnapshot, models: &BlockModels) -> bool {
    snapshot.at(0, 0, 0).block_states().palette_values().is_some_and(|palette| {
        palette.iter().all(|&raw| {
            StateId::new(raw).is_some_and(|state| {
                models.occludes(state) && models.quads(state).iter().all(|quad| quad.cullface.is_some())
            })
        })
    })
}

pub(super) fn mesh_snapshot_models_layers_with_light(
    snapshot: &SectionSnapshot,
    models: &BlockModels,
    cutout_leaves: bool,
    blend_radius: i32,
    light: &SnapshotLight<'_>,
) -> (ModelMesh, ModelMesh) {
    if center_is_quadless(snapshot, models) {
        return (ModelMesh::default(), ModelMesh::default());
    }
    let view = SnapshotModelView {
        snapshot,
        models,
        light,
        tint: RefCell::new(BlendedTintCursor::new(blend_radius)),
        cutout_leaves,
        boundary_only: center_interior_is_culled(snapshot, models),
    };
    lodestone_render::mesh_models_layers(&view)
}

#[cfg(test)]
mod boundary_tests {
    use super::*;
    use lodestone_assets::{MemorySource, ResourceManager};
    use lodestone_render::BlocksJsonRegistry;
    use lodestone_world::PalettedContainer;

    fn models() -> &'static BlockModels {
        static MODELS: OnceLock<BlockModels> = OnceLock::new();
        MODELS.get_or_init(|| {
            let mut source = MemorySource::new("opaque-centre-test");
            for (name, pixels) in [("solid", [255; 8]), ("cutout", [255, 255, 255, 255, 255, 255, 255, 0])] {
                let mut texture = Vec::new();
                {
                    let mut encoder = png::Encoder::new(&mut texture, 2, 1);
                    encoder.set_color(png::ColorType::Rgba);
                    encoder.set_depth(png::BitDepth::Eight);
                    encoder.write_header().unwrap().write_image_data(&pixels).unwrap();
                }
                source.insert(format!("assets/minecraft/textures/block/test_{name}.png"), texture);
            }
            for name in ["test_cube", "test_unculled", "test_partial", "oak_leaves"] {
                let faces: serde_json::Map<_, _> = ["east", "west", "up", "down", "north", "south"]
                    .into_iter().map(|face| (face.into(), serde_json::json!({"texture": "#all", "cullface": face})))
                    .collect();
                let mut elements = vec![serde_json::json!({"from": [0, 0, 0],
                    "to": [16, if name == "test_partial" { 8 } else { 16 }, 16], "faces": faces})];
                if name == "test_unculled" {
                    elements.push(serde_json::json!({"from": [4, 4, 4], "to": [12, 12, 12],
                        "faces": {"east": {"texture": "#all"}}}));
                }
                source.insert(format!("assets/minecraft/blockstates/{name}.json"), serde_json::to_vec(
                    &serde_json::json!({"variants": {"": {"model": format!("minecraft:block/{name}")}}}),
                ).unwrap());
                source.insert(format!("assets/minecraft/models/block/{name}.json"), serde_json::to_vec(
                    &serde_json::json!({"textures": {"all": format!("minecraft:block/test_{}",
                        if name == "oak_leaves" { "cutout" } else { "solid" })}, "elements": elements}),
                ).unwrap());
            }
            let report = serde_json::json!({
                "minecraft:air": {"states": [{"id": 0, "default": true}]},
                "minecraft:test_cube": {"states": [{"id": 1, "default": true}]},
                "minecraft:test_unculled": {"states": [{"id": 2, "default": true}]},
                "minecraft:test_partial": {"states": [{"id": 3, "default": true}]},
                "minecraft:oak_leaves": {"states": [{"id": 4, "default": true}]}
            });
            let registry = BlocksJsonRegistry::from_slice(&serde_json::to_vec(&report).unwrap()).unwrap();
            BlockModels::build_with_mip_levels(&ResourceManager::new(vec![Box::new(source)]), &registry, 0).unwrap()
        })
    }

    fn section(blocks: PalettedContainer) -> Arc<ChunkSection> {
        Arc::new(ChunkSection::from_containers(blocks, PalettedContainer::new(PaletteKind::biomes(), 0), 0))
    }

    fn snapshot(center: PalettedContainer, neighbor: u32) -> SectionSnapshot {
        let neighbor = section(PalettedContainer::new(PaletteKind::block_states(), neighbor));
        let mut sections = vec![Neighbour::Present(neighbor); 27];
        sections[13] = Neighbour::Present(section(center));
        SectionSnapshot { key: SectionKey { cx: 0, cz: 0, si: 1, min_y: -64 },
            sections, lights: vec![None; 27], sky_default: SkyDefault::Full, biome_names: Arc::from([]),
            light_revision: None }
    }

    fn mesh_with_proof(snapshot: &SectionSnapshot, boundary_only: bool, cutout_leaves: bool) -> (ModelMesh, ModelMesh) {
        let light = SnapshotLight::new(snapshot);
        lodestone_render::mesh_models_layers(&SnapshotModelView { snapshot, models: models(), light: &light,
            tint: RefCell::new(BlendedTintCursor::new(0)), cutout_leaves, boundary_only })
    }

    fn same_geometry(actual: &(ModelMesh, ModelMesh), expected: &(ModelMesh, ModelMesh)) {
        for (actual, expected) in [(&actual.0, &expected.0), (&actual.1, &expected.1)] {
            assert_eq!(bytemuck::cast_slice::<_, u8>(&actual.vertices), bytemuck::cast_slice::<_, u8>(&expected.vertices));
            assert_eq!(actual.indices, expected.indices);
        }
    }

    #[test]
    fn light_inputs_track_real_model_sampling_without_changing_geometry() {
        use super::super::light_reads::LightReadProbe;
        use lodestone_world::{LightData, NibbleArray};
        let kind = PaletteKind::block_states();
        let mut center = PalettedContainer::new(kind, 0);
        center.set(kind.index(8, 8, 8), 1);
        let mut snapshot = snapshot(center, 0);
        snapshot.lights.fill(Some(SectionLightData {
            sky: LightData::Uniform(3), block: LightData::Uniform(0),
        }));
        let control = mesh_snapshot_models_layers(&snapshot, models(), true, 0);
        assert_eq!(control.0.quad_count(), 6);
        assert!(control.0.vertices.iter().all(|vertex| vertex.light == 0x30));
        let probe = LightReadProbe::new();
        let light = SnapshotLight::new(&snapshot).with_read_probe(Some(&probe));
        let observed = mesh_snapshot_models_layers_with_light(&snapshot, models(), true, 0, &light);
        same_geometry(&observed, &control);
        let current = SnapshotLight::new(&snapshot);
        let inputs = probe.finish(|[x, y, z]| {
            let (sky, block) = current.levels_at(x, y, z);
            sky << 4 | block
        }).unwrap();
        assert_eq!(inputs.unchanged([0; 3], [0; 3], |_| panic!("unread cell")), (true, 0));
        let mut sky = NibbleArray::filled(3);
        sky.set(8 * 256 + 8 * 16 + 9, 11);
        snapshot.lights[13].as_mut().unwrap().sky = LightData::Values(sky);
        let current = SnapshotLight::new(&snapshot);
        assert_eq!(inputs.unchanged([9, 8, 8], [9, 8, 8], |[x, y, z]| {
            let (sky, block) = current.levels_at(x, y, z);
            sky << 4 | block
        }), (false, 1));
        let changed = mesh_snapshot_models_layers(&snapshot, models(), true, 0);
        assert_eq!(changed.0.indices, control.0.indices);
        assert!(changed.0.vertices.iter().any(|vertex| vertex.light == 0x50),
            "the 3,3,3,11 corner control must round to sky level 5");
    }

    #[test]
    fn opaque_palette_boundary_traversal_matches_full_geometry() {
        let kind = PaletteKind::block_states();
        let air_halo = snapshot(PalettedContainer::new(kind, 1), 0);
        let sealed = snapshot(PalettedContainer::new(kind, 1), 1);
        let mut gap = snapshot(PalettedContainer::new(kind, 1), 1);
        let mut neighbor = PalettedContainer::new(kind, 1);
        neighbor.set(kind.index(0, 7, 11), 0);
        gap.sections[22] = Neighbour::Present(section(neighbor));
        for (snapshot, quads) in [(&air_halo, 1536), (&sealed, 0), (&gap, 1)] {
            assert!(center_interior_is_culled(snapshot, models()));
            let actual = mesh_snapshot_models_layers(snapshot, models(), true, 0);
            assert_eq!(actual.0.quad_count() + actual.1.quad_count(), quads);
            same_geometry(&actual, &mesh_with_proof(snapshot, false, true));
            same_geometry(&(mesh_snapshot_models_at(snapshot, models(), true, 0), ModelMesh::default()), &actual);
        }
    }

    #[test]
    fn unculled_cutout_partial_invalid_and_direct_centres_reject_boundary_proof() {
        let kind = PaletteKind::block_states();
        assert!(models().occludes(StateId::new(2).unwrap()));
        assert!(models().quads(StateId::new(2).unwrap()).iter().any(|quad| quad.cullface.is_none()));
        assert!(models().is_leaves(StateId::new(4).unwrap()));
        for state in [0, 2, 3, 4, u32::MAX] {
            let snapshot = snapshot(PalettedContainer::new(kind, state), 1);
            assert!(!center_interior_is_culled(&snapshot, models()), "state {state}");
            for cutout_leaves in [false, true] {
                let actual = mesh_snapshot_models_layers(&snapshot, models(), cutout_leaves, 0);
                same_geometry(&actual, &mesh_with_proof(&snapshot, false, cutout_leaves));
                if state == 2 {
                    assert_eq!(actual.0.quad_count(), 4096);
                    let invalid_shortcut = mesh_with_proof(&snapshot, true, cutout_leaves);
                    assert_eq!(invalid_shortcut.0.quad_count(), 1352);
                    assert_ne!(invalid_shortcut.0.indices.len(), actual.0.indices.len());
                }
            }
        }
        let mut unused_cutout = PalettedContainer::new(kind, 1);
        unused_cutout.set(100, 4);
        unused_cutout.set(100, 1);
        assert!(!center_interior_is_culled(&snapshot(unused_cutout, 1), models()));
        let values: Vec<_> = (0..kind.entry_count()).map(|index| index as u32 + 100).collect();
        let direct = snapshot(PalettedContainer::from_values(kind, &values), 1);
        assert_eq!(direct.at(0, 0, 0).block_states().palette_values(), None);
        assert!(!center_interior_is_culled(&direct, models()));
    }
}
