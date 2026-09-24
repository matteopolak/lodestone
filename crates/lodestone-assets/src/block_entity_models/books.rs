use super::{BOOK_SHEET, CubeDef, EntityModelDef, PartDef, PartPose};

/// An open book — vanilla's own book-model body-layer construction
/// (in the decompiled 26.2 client source), sheet 64×32:
///
/// ```text
/// left_lid    texOffs( 0,  0)  addBox(-6, -5, -0.005,  6, 10, 0.005)  pose offset(0, 0, -1)
/// right_lid   texOffs(16,  0)  addBox( 0, -5, -0.005,  6, 10, 0.005)  pose offset(0, 0,  1)
/// seam        texOffs(12,  0)  addBox(-1, -5,  0,      2, 10, 0.005)  pose rotation(0, PI/2, 0)
/// left_pages  texOffs( 0, 10)  addBox( 0, -4, -0.99,   5,  8, 1)      pose ZERO
/// right_pages texOffs(12, 10)  addBox( 0, -4, -0.01,   5,  8, 1)      pose ZERO
/// flip_page1  texOffs(24, 10)  addBox( 0, -4,  0,      5,  8, 0.005)  pose ZERO
/// flip_page2  texOffs(24, 10)  addBox( 0, -4,  0,      5,  8, 0.005)  pose ZERO
/// ```
///
/// Three things in that table are deliberate and would each get "cleaned up"
/// by a reader who assumed a transcription error:
///
/// * **The lids and the flip pages are 0.005 texels thick.** They are paper-thin
///   *boxes*, not quads — `bake_cube` emits all six faces of each, two of which
///   are 0.005 texels wide. A mesher that culled near-degenerate cubes would eat
///   the covers and the turning pages and leave only the two page blocks, which
///   still reads as a book.
/// * **`flip_page1` and `flip_page2` share one `CubeListBuilder`** in the jar, so
///   their UVs are identical by construction. They differ only in the per-frame
///   `yRot` `setupAnim` gives them.
/// * **`seam` is the only part with a rest *rotation*** (`PartPose.rotation`, no
///   offset), and `BookModel.setupAnim` never poses it — so the spine's quarter
///   turn must survive as a rest pose rather than being folded into a caller's
///   override list.
///
/// Shared by two registrations, and the *work* is not shared: a lectern's
/// `BookModel.State` is a compile-time constant (see
/// `lodestone_render::block_entity`'s `LECTERN_BOOK_OPENNESS`), while
/// vanilla's own enchant-table renderer's is a client-simulated animation state machine with
/// its own per-frame `open`/`flip`/`rot` counters. One mesh, two very different
/// consumers.
///
/// Authored **block-space-up** like [`chest_single_model`] and [`bell_model`]:
/// vanilla's own lectern-renderer submit step applies no `scale(-1, -1, 1)` (unlike
/// its own skull-block renderer), so origins and poses add with no sign flip.
#[must_use]
pub fn book_model() -> EntityModelDef {
    // Vanilla builds both flip pages from one `CubeListBuilder`; one `CubeDef`
    // value cloned into both children is the same statement in Rust.
    let flip_page = CubeDef::new([0.0, -4.0, 0.0], [5.0, 8.0, 0.005], [24.0, 10.0]);
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "left_lid",
            PartDef::new(PartPose::offset(0.0, 0.0, -1.0)).with_cube(CubeDef::new(
                [-6.0, -5.0, -0.005],
                [6.0, 10.0, 0.005],
                [0.0, 0.0],
            )),
        )
        .with_child(
            "right_lid",
            PartDef::new(PartPose::offset(0.0, 0.0, 1.0)).with_cube(CubeDef::new(
                [0.0, -5.0, -0.005],
                [6.0, 10.0, 0.005],
                [16.0, 0.0],
            )),
        )
        .with_child(
            "seam",
            PartDef::new(PartPose::rotation(0.0, std::f32::consts::FRAC_PI_2, 0.0)).with_cube(
                CubeDef::new([-1.0, -5.0, 0.0], [2.0, 10.0, 0.005], [12.0, 0.0]),
            ),
        )
        .with_child(
            "left_pages",
            PartDef::new(PartPose::ZERO).with_cube(CubeDef::new(
                [0.0, -4.0, -0.99],
                [5.0, 8.0, 1.0],
                [0.0, 10.0],
            )),
        )
        .with_child(
            "right_pages",
            PartDef::new(PartPose::ZERO).with_cube(CubeDef::new(
                [0.0, -4.0, -0.01],
                [5.0, 8.0, 1.0],
                [12.0, 10.0],
            )),
        )
        .with_child(
            "flip_page1",
            PartDef::new(PartPose::ZERO).with_cube(flip_page.clone()),
        )
        .with_child(
            "flip_page2",
            PartDef::new(PartPose::ZERO).with_cube(flip_page),
        );
    EntityModelDef {
        texture_width: BOOK_SHEET.0,
        texture_height: BOOK_SHEET.1,
        root,
    }
}
