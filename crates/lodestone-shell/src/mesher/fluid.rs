//! Fluid meshing and visibility derived from an immutable section snapshot.
use super::*;

pub fn snapshot_visibility(
    snapshot: &SectionSnapshot,
    models: &BlockModels,
) -> lodestone_render::SectionVisibility {
    let centre = snapshot.at(0, 0, 0);
    lodestone_render::compute_visibility_from(|x, y, z| {
        StateId::new(centre.get_block(x, y, z))
            .is_some_and(|state| models.occludes(state))
    })
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
    /// same reasoning applies to any cached biome tint view.
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
        let raw = self.snapshot.at(dx, dy, dz).get_block(lx, ly, lz);
        let Some(state) = StateId::new(raw) else {
            return FluidNeighborCell::default();
        };
        FluidNeighborCell {
            fluid: self.models.fluid(state),
            occludes: self.models.occludes(state),
            overlay: self.models.fluid_overlay(state),
        }
    }

    fn fluid_at(&self, x: i32, y: i32, z: i32) -> Option<FluidCell> {
        let (dx, lx) = split16(x);
        let (dy, ly) = split16(y);
        let (dz, lz) = split16(z);
        if !(-1..=1).contains(&dx) || !(-1..=1).contains(&dy) || !(-1..=1).contains(&dz) {
            return None;
        }
        let raw = self.snapshot.at(dx, dy, dz).get_block(lx, ly, lz);
        let state = StateId::new(raw)?;
        self.models.fluid(state)
    }

    fn occludes_at(&self, x: i32, y: i32, z: i32) -> bool {
        let (dx, lx) = split16(x);
        let (dy, ly) = split16(y);
        let (dz, lz) = split16(z);
        if !(-1..=1).contains(&dx) || !(-1..=1).contains(&dy) || !(-1..=1).contains(&dz) {
            return false;
        }
        let raw = self.snapshot.at(dx, dy, dz).get_block(lx, ly, lz);
        let Some(state) = StateId::new(raw) else {
            return false;
        };
        self.models.occludes(state)
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
        let raw = self.snapshot.at(dx, dy, dz).get_block(lx, ly, lz);
        let Some(state) = StateId::new(raw) else {
            return false;
        };
        self.models.fluid_overlay(state)
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
        let state = lodestone_data::block_states::StateId::new(id)?;
        let boxes = lodestone_data::outline_shapes::outline_boxes(state);
        lodestone_assets::fluid::full_footprint_y_range(boxes)
    }

    /// The live half of vanilla's own face-render self test — the block sharing the
    /// fluid's own cell, which for a waterlogged stair is the stair. Same
    /// trait-default-plus-override shape as `overlay_at` and
    /// `partial_occluder_y_range_at` above, and the same failure mode if it is
    /// missing: the rule sits in `lodestone-render` and never reaches terrain.
    ///
    /// # The `RenderLayer::Solid` gate is vanilla's own occlusion-eligibility flag
    ///
    /// Vanilla builds the shape this test reads from a block's real occlusion
    /// shape only when that block is eligible to occlude at all, and falls
    /// back to an empty shape otherwise. Eligibility is a block-properties
    /// flag with no getter, absent from `blocks.json` and from
    /// every table in `lodestone-data`. `BlockModels::layer` stands in for it: a
    /// state whose sprites are fully opaque renders `Solid`, and an
    /// ineligible block is (in 26.2, across every waterloggable block) one
    /// whose textures are not. Without the gate, **waterlogged leaves** — a
    /// full-cube outline shape that vanilla marks ineligible to occlude — would
    /// report all five faces occluded and cull their water away entirely.
    ///
    /// The gate is not a scoping compromise on the *geometry* side:
    /// `face_fully_covered` is exact for any axis-aligned union, so a stair's
    /// two-box solid side is answered correctly where
    /// `partial_occluder_y_range_at`'s single-box reduction would have declined.
    ///
    /// Cheap for ordinary water: `minecraft:water`'s own outline shape is empty
    /// (vanilla's own liquid-block shape getter returns its own empty-shape
    /// sentinel) *and* its layer is
    /// `Translucent`, so an open ocean's cells leave on the first branch.
    fn self_occlusion_at(&self, x: i32, y: i32, z: i32) -> lodestone_assets::fluid::SelfOcclusion {
        use lodestone_assets::fluid::SelfOcclusion;

        let (dx, lx) = split16(x);
        let (dy, ly) = split16(y);
        let (dz, lz) = split16(z);
        if !(-1..=1).contains(&dx) || !(-1..=1).contains(&dy) || !(-1..=1).contains(&dz) {
            return SelfOcclusion::default();
        }
        let raw = self.snapshot.at(dx, dy, dz).get_block(lx, ly, lz);
        let Some(state) = StateId::new(raw) else {
            return SelfOcclusion::default();
        };
        if self.models.layer(state) != lodestone_render::RenderLayer::Solid {
            return SelfOcclusion::default();
        }
        let boxes = lodestone_data::outline_shapes::outline_boxes(state);
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
    /// of the model view's biome tint lookup. `x, y, z` are already
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
/// "there are two meshers" gotcha; the seam behavior is tested in two halves.
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
