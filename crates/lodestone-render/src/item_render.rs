//! The **pose** half of the 3-D item-model GUI path: turning a model's
//! `display.gui` transform into the matrices that put a mini-block in a slot.
//!
//! Geometry for a block item is baked once against the block atlas (see
//! [`BlockModels::item_quads`](crate::BlockModels::item_quads)); what makes it
//! *look* like an inventory icon rather than a block sitting at the world origin
//! is entirely the pair of matrices this module builds. There is no GPU here and
//! no state — every function is a pure matrix construction, so the fidelity
//! questions ("is the rotation order right?", "does the winding flip?") are
//! answered by unit tests instead of by squinting at a screenshot.
//!
//! # The three spaces
//!
//! 1. **Model space** — what the baker emits: a `0..1` cube (occasionally poking
//!    outside it), `+X` east, `+Y` up, `+Z` south, exactly as in the world.
//! 2. **GUI pixel space** — `x` right, `y` **down** from the top-left of the
//!    render target, `z` growing *towards the viewer*. This is vanilla's GUI
//!    space: `GuiGraphics.renderItem` does `translate(x + 8, y + 8, 150)` then
//!    `scale(16, -16, 16)`, and vanilla's GUI ortho is set up so a larger `z` is
//!    nearer. [`gui_item_pose`] maps model space here.
//! 3. **Clip space** — `wgpu` NDC with `y` up and depth in `0..1`, `0` nearest.
//!    [`gui_ortho`] maps GUI pixel space here.
//!
//! Feed `gui_ortho(w, h)` to the model pipeline as its `view_proj` and
//! `gui_item_pose(rect, transform)` to
//! [`mesh_item_quads`](crate::mesh_item_quads), and the existing
//! [`ModelPipeline`](crate::ModelPipeline) draws the item with no new pipeline,
//! shader, or atlas.
//!
//! # Handedness: two flips that must cancel
//!
//! [`gui_item_pose`] contains vanilla's `scale(16, -16, 16)` — the `y` flip that
//! reconciles "model `+Y` is up" with "screen `+Y` is down". Its determinant is
//! **negative**, which reverses triangle winding. [`ModelPipeline`] culls back
//! faces with `FrontFace::Ccw`, so on its own that flip would render every block
//! inside-out: you would see the *inside* of the three far faces, which still
//! reads as a plausible-looking isometric cube in a screenshot. [`gui_ortho`]
//! carries the compensating flip (screen `y` down → NDC `y` up), so the composed
//! `gui_ortho * gui_item_pose` has the **same** screen-space orientation as the
//! world path's `Camera::view_projection`, and the same three faces survive
//! culling as are nearest under the depth test. `winding_matches_the_world_camera`
//! in this module's tests is what holds that down.
//!
//! # Why the `/16` and the clamps live here
//!
//! [`DisplayTransform`] stores the *raw JSON numbers*
//! (`lodestone_assets`'s parser is deliberately verbatim, and its tests assert
//! exactly that). Vanilla's `ItemTransform` deserializer multiplies the
//! translation by `1/16` and clamps it to `±5`, and clamps the scale to `±4`.
//! Applying those here keeps the parsed struct honest — a field that silently
//! means "the JSON value ÷ 16" is worse than one that means the JSON value — and
//! keeps every render-time convention in the renderer.

use glam::{Mat4, Vec3};
use lodestone_assets::DisplayTransform;

/// Model-space units per block: a model JSON's `display` translation is in
/// sixteenths of a block, matching the `0..16` element coordinate grid.
pub const UNITS_PER_BLOCK: f32 = 16.0;

/// Vanilla's clamp on a display translation, in **blocks** (i.e. applied after
/// the `/16`). From `ItemTransform`'s deserializer.
pub const TRANSLATION_LIMIT: f32 = 5.0;

/// Vanilla's clamp on a display scale. From `ItemTransform`'s deserializer.
pub const SCALE_LIMIT: f32 = 4.0;

/// Half the depth range [`gui_ortho`] maps into `0..1` clip depth, in GUI
/// pixels. GUI z is tiny in practice (a `0.625`-scaled block spans under ±9 px),
/// so this only has to be comfortably larger than any HUD layering offset;
/// vanilla's GUI ortho spans a similarly generous `1000..21000`.
pub const GUI_DEPTH_HALF_RANGE: f32 = 1000.0;

/// The `display.gui` (or any other slot's) transform as a model-space matrix.
///
/// ```text
/// T(translation/16) · Rx · Ry · Rz · S(scale) · T(-0.5, -0.5, -0.5)
/// ```
///
/// Read right to left, that is vanilla's order: `ItemRenderer` pushes
/// `translate(-0.5, -0.5, -0.5)` **after** `ItemTransform.apply` has pushed
/// translate → rotate → scale, and a `PoseStack` right-multiplies, so the
/// innermost operation is the centring. The model is centred on the origin
/// first, then scaled, then rotated, then translated — which is why the
/// transformed centre of the unit cube lands exactly on `translation/16`
/// regardless of the rotation.
///
/// The rotation is JOML `Quaternionf.rotationXYZ`, i.e. `Rx · Ry · Rz`.
#[must_use]
pub fn display_matrix(transform: &DisplayTransform) -> Mat4 {
    let translation = (Vec3::from(transform.translation) / UNITS_PER_BLOCK).clamp(
        Vec3::splat(-TRANSLATION_LIMIT),
        Vec3::splat(TRANSLATION_LIMIT),
    );
    let scale =
        Vec3::from(transform.scale).clamp(Vec3::splat(-SCALE_LIMIT), Vec3::splat(SCALE_LIMIT));
    let [rx, ry, rz] = transform.rotation;
    let rotation = Mat4::from_rotation_x(rx.to_radians())
        * Mat4::from_rotation_y(ry.to_radians())
        * Mat4::from_rotation_z(rz.to_radians());

    Mat4::from_translation(translation)
        * rotation
        * Mat4::from_scale(scale)
        * Mat4::from_translation(Vec3::splat(-0.5))
}

/// The full model-space → **GUI pixel space** pose for an item drawn into
/// `rect_px` (`[x, y, w, h]`, top-left origin, in target pixels).
///
/// ```text
/// T(centre of rect) · S(w, -h, min(w, h)) · display_matrix(transform)
/// ```
///
/// For the 16×16 slot every real hotbar/inventory cell uses, `S(w, -h, …)` is
/// vanilla's `scale(16, -16, 16)` exactly. The `y` flip is what turns model-up
/// into screen-up once [`gui_ortho`] flips back; see the module docs on winding.
/// The `z` scale is `min(w, h)` so a non-square rect keeps the block's depth
/// proportional to its smaller on-screen dimension rather than shearing it.
#[must_use]
pub fn gui_item_pose(rect_px: [f32; 4], transform: &DisplayTransform) -> Mat4 {
    let [x, y, w, h] = rect_px;
    let centre = Vec3::new(x + w * 0.5, y + h * 0.5, 0.0);
    Mat4::from_translation(centre)
        * Mat4::from_scale(Vec3::new(w, -h, w.min(h)))
        * display_matrix(transform)
}

/// The **GUI pixel space → clip space** projection for a `width_px × height_px`
/// render target.
///
/// * `x`: `0..width` → `-1..1`.
/// * `y`: `0..height` → `1..-1` (top-left origin; NDC `y` is up).
/// * `z`: [`GUI_DEPTH_HALF_RANGE`]`..-`[`GUI_DEPTH_HALF_RANGE`] → `0..1`, so a
///   **larger** GUI `z` is **nearer** — vanilla's convention, and the one
///   [`gui_item_pose`]'s positive `z` scale needs for the faces that survive
///   back-face culling to also be the faces nearest under `CompareFunction::Less`.
///
/// The `y` flip here is the counterpart to [`gui_item_pose`]'s: the two cancel,
/// so triangle winding is preserved. See the module docs.
#[must_use]
pub fn gui_ortho(width_px: u32, height_px: u32) -> Mat4 {
    let w = width_px.max(1) as f32;
    let h = height_px.max(1) as f32;
    Mat4::from_translation(Vec3::new(-1.0, 1.0, 0.5))
        * Mat4::from_scale(Vec3::new(2.0 / w, -2.0 / h, -0.5 / GUI_DEPTH_HALF_RANGE))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::Camera;
    use lodestone_assets::Direction;

    /// Vanilla's `block/block` `display.gui` transform: the isometric pose every
    /// block item is drawn with.
    fn vanilla_block_gui() -> DisplayTransform {
        DisplayTransform {
            rotation: [30.0, 225.0, 0.0],
            translation: [0.0, 0.0, 0.0],
            scale: [0.625, 0.625, 0.625],
        }
    }

    // --- display_matrix: the multiplication order ------------------------

    #[test]
    fn centring_is_innermost_so_the_cube_centre_lands_on_the_translation() {
        // The sharpest statement of "T(-0.5) is applied FIRST": whatever the
        // rotation and scale, the model's centre is the rotation pivot, so it
        // maps to exactly translation/16. Any other order (rotating before
        // centring, or translating before rotating) breaks this.
        let t = DisplayTransform {
            rotation: [30.0, 225.0, 15.0],
            translation: [8.0, -16.0, 4.0], // → (0.5, -1.0, 0.25) blocks
            scale: [0.625, 0.5, 2.0],
        };
        let centre = display_matrix(&t).transform_point3(Vec3::splat(0.5));
        assert!(
            (centre - Vec3::new(0.5, -1.0, 0.25)).length() < 1e-5,
            "cube centre must map to translation/16, got {centre}"
        );
    }

    #[test]
    fn rotation_then_translation_maps_a_known_point_exactly() {
        // A hand-computable case. rotation Y=90°, translation (16,0,0) → 1 block.
        // Model point (1, 0.5, 0.5) centres to (0.5, 0, 0); Ry(90) sends
        // +X to -Z, so it becomes (0, 0, -0.5); the translation lands it at
        // (1, 0, -0.5).
        let t = DisplayTransform {
            rotation: [0.0, 90.0, 0.0],
            translation: [16.0, 0.0, 0.0],
            scale: [1.0, 1.0, 1.0],
        };
        let p = display_matrix(&t).transform_point3(Vec3::new(1.0, 0.5, 0.5));
        assert!(
            (p - Vec3::new(1.0, 0.0, -0.5)).length() < 1e-5,
            "expected (1, 0, -0.5), got {p}"
        );

        // The order actually matters: translating *before* rotating would send
        // the same point to Ry(90)·(1.5, 0, 0) = (0, 0, -1.5) instead.
        let wrong = Mat4::from_rotation_y(90f32.to_radians())
            * Mat4::from_translation(Vec3::new(1.0, 0.0, 0.0))
            * Mat4::from_translation(Vec3::splat(-0.5));
        let wrong_p = wrong.transform_point3(Vec3::new(1.0, 0.5, 0.5));
        assert!(
            (wrong_p - p).length() > 0.5,
            "the wrong order must be distinguishable, got {wrong_p} vs {p}"
        );
    }

    #[test]
    fn rotation_is_x_then_y_then_z_in_that_multiplication_order() {
        // Rx·Ry·Rz and Rz·Ry·Rx disagree unless two of the angles are zero, so a
        // single triple pins the JOML `rotationXYZ` order down.
        let t = DisplayTransform {
            rotation: [90.0, 90.0, 0.0],
            translation: [0.0; 3],
            scale: [1.0; 3],
        };
        // Rx(90)·Ry(90) applied to the centred corner offset (0.5, 0, 0):
        // Ry(90): +X → -Z → (0, 0, -0.5); Rx(90): -Z → +Y → (0, 0.5, 0).
        let p = display_matrix(&t).transform_point3(Vec3::new(1.0, 0.5, 0.5));
        assert!(
            (p - Vec3::new(0.0, 0.5, 0.0)).length() < 1e-5,
            "expected Rx·Ry ordering to give (0, 0.5, 0), got {p}"
        );
        // The reversed order Rz·Ry·Rx would give Ry(90)·Rx(90)·(0.5,0,0) =
        // Ry(90)·(0.5,0,0) = (0,0,-0.5) — a different point.
        assert!((p - Vec3::new(0.0, 0.0, -0.5)).length() > 0.4);
    }

    // --- display_matrix: the /16 and the two clamps ----------------------

    #[test]
    fn translation_is_divided_by_sixteen_and_clamped_to_five_blocks() {
        let at_limit = DisplayTransform {
            translation: [80.0, -80.0, 0.0], // exactly ±5 blocks
            ..DisplayTransform::default()
        };
        let o = display_matrix(&at_limit).transform_point3(Vec3::splat(0.5));
        assert!((o - Vec3::new(5.0, -5.0, 0.0)).length() < 1e-5, "got {o}");

        let past_limit = DisplayTransform {
            translation: [1600.0, -1600.0, 0.0], // 100 blocks before clamping
            ..DisplayTransform::default()
        };
        let o = display_matrix(&past_limit).transform_point3(Vec3::splat(0.5));
        assert!(
            (o - Vec3::new(5.0, -5.0, 0.0)).length() < 1e-5,
            "translation must clamp to ±5 blocks, got {o}"
        );
    }

    #[test]
    fn scale_is_clamped_to_four() {
        let t = DisplayTransform {
            scale: [10.0, -10.0, 1.0],
            ..DisplayTransform::default()
        };
        // The +X corner offset of 0.5 scales to 0.5 * 4 = 2.0, and the clamped
        // negative scale to -2.0 — not 5.0 / -5.0.
        let p = display_matrix(&t).transform_point3(Vec3::new(1.0, 1.0, 0.5));
        assert!(
            (p - Vec3::new(2.0, -2.0, 0.0)).length() < 1e-5,
            "scale must clamp to ±4, got {p}"
        );
    }

    #[test]
    fn the_identity_transform_only_centres() {
        let m = display_matrix(&DisplayTransform::default());
        assert!(
            (m.transform_point3(Vec3::splat(0.5)) - Vec3::ZERO).length() < 1e-6,
            "a default transform still centres the model on the origin"
        );
    }

    // --- The vanilla block pose: silhouette ------------------------------

    /// Axis-aligned half-extents of the transformed unit cube.
    fn cube_half_extents(m: Mat4) -> Vec3 {
        let mut lo = Vec3::splat(f32::MAX);
        let mut hi = Vec3::splat(f32::MIN);
        for i in 0..8u32 {
            let c = Vec3::new((i & 1) as f32, ((i >> 1) & 1) as f32, ((i >> 2) & 1) as f32);
            let p = m.transform_point3(c);
            lo = lo.min(p);
            hi = hi.max(p);
        }
        (hi - lo) * 0.5
    }

    #[test]
    fn vanilla_block_pose_nearly_fills_a_sixteen_pixel_slot() {
        // For a cube of half-extent h rotated by R, the bbox half-extent on axis
        // i is h * sum_j |R_ij|. With h = 0.625/2 and R = Rx(30)·Ry(225):
        //   x: 0.3125 * (0.70711 + 0 + 0.70711)       = 0.44194
        //   y: 0.3125 * (0.35355 + 0.86603 + 0.35355) = 0.49160
        //   z: 0.3125 * (0.61237 + 0.5     + 0.61237) = 0.53898
        let e = cube_half_extents(display_matrix(&vanilla_block_gui()));
        assert!((e.x - 0.44194).abs() < 1e-4, "x half-extent {}", e.x);
        assert!((e.y - 0.49160).abs() < 1e-4, "y half-extent {}", e.y);
        assert!((e.z - 0.53898).abs() < 1e-4, "z half-extent {}", e.z);

        // In a 16 px slot that is 14.14 px wide by 15.73 px tall: the icon fills
        // the slot without overflowing it, which is the visible signature of the
        // vanilla pose being right.
        let e = cube_half_extents(gui_item_pose([0.0, 0.0, 16.0, 16.0], &vanilla_block_gui()));
        assert!((e.x * 2.0 - 14.142).abs() < 1e-2, "width px {}", e.x * 2.0);
        assert!((e.y * 2.0 - 15.731).abs() < 1e-2, "height px {}", e.y * 2.0);
        assert!(
            e.x < 8.0 && e.y < 8.0,
            "the posed block must stay inside its 16 px slot"
        );
    }

    #[test]
    fn the_item_is_centred_on_its_slot_rect() {
        let m = gui_item_pose([40.0, 20.0, 16.0, 16.0], &vanilla_block_gui());
        let c = m.transform_point3(Vec3::splat(0.5));
        assert!(
            (c - Vec3::new(48.0, 28.0, 0.0)).length() < 1e-4,
            "the model centre must land on the rect centre, got {c}"
        );
    }

    #[test]
    fn model_up_becomes_screen_up() {
        // Model +Y through the pose *and* the ortho must end up with a larger NDC
        // y (up). Checking only the pose would read the flipped intermediate.
        let m = gui_ortho(256, 256) * gui_item_pose([0.0, 0.0, 16.0, 16.0], &vanilla_block_gui());
        let top = m.transform_point3(Vec3::new(0.5, 1.0, 0.5));
        let bottom = m.transform_point3(Vec3::new(0.5, 0.0, 0.5));
        assert!(
            top.y > bottom.y,
            "the model's top must render above its bottom: {top} vs {bottom}"
        );
    }

    // --- gui_ortho -------------------------------------------------------

    #[test]
    fn gui_ortho_maps_the_pixel_rect_onto_ndc_with_a_top_left_origin() {
        let m = gui_ortho(320, 240);
        let tl = m.transform_point3(Vec3::new(0.0, 0.0, 0.0));
        let br = m.transform_point3(Vec3::new(320.0, 240.0, 0.0));
        assert!((tl - Vec3::new(-1.0, 1.0, 0.5)).length() < 1e-6, "{tl}");
        assert!((br - Vec3::new(1.0, -1.0, 0.5)).length() < 1e-6, "{br}");
    }

    #[test]
    fn larger_gui_z_is_nearer() {
        // The depth convention `gui_item_pose`'s positive z scale depends on:
        // wgpu depth 0 is nearest and the pipeline compares with `Less`.
        let m = gui_ortho(320, 240);
        let near = m.transform_point3(Vec3::new(0.0, 0.0, 100.0)).z;
        let far = m.transform_point3(Vec3::new(0.0, 0.0, -100.0)).z;
        assert!(near < far, "larger GUI z must be nearer: {near} vs {far}");
        assert!((0.0..=1.0).contains(&near) && (0.0..=1.0).contains(&far));
    }

    #[test]
    fn a_degenerate_target_size_does_not_produce_nans() {
        let m = gui_ortho(0, 0);
        assert!(m.to_cols_array().iter().all(|v| v.is_finite()));
    }

    // --- Winding: the bug class that survives visual review ---------------

    /// A unit-cube face with vanilla's outward winding: `cross(v1-v0, v2-v0)`
    /// equals the outward normal (counter-clockwise seen from outside), which is
    /// the invariant [`crate::face_winding_is_outward`] asserts for the packed
    /// path and the baker holds to for model quads.
    fn outward_face(dir: Direction) -> [Vec3; 4] {
        let n = match dir {
            Direction::East => Vec3::X,
            Direction::West => -Vec3::X,
            Direction::Up => Vec3::Y,
            Direction::Down => -Vec3::Y,
            Direction::South => Vec3::Z,
            Direction::North => -Vec3::Z,
        };
        // Any tangent u perpendicular to n, with v = n × u, gives u × v = n.
        let u = if n.x.abs() < 0.5 { Vec3::X } else { Vec3::Y };
        let v = n.cross(u);
        let centre = Vec3::splat(0.5) + n * 0.5;
        [
            centre - u * 0.5 - v * 0.5,
            centre + u * 0.5 - v * 0.5,
            centre + u * 0.5 + v * 0.5,
            centre - u * 0.5 + v * 0.5,
        ]
    }

    const ALL_FACES: [Direction; 6] = [
        Direction::East,
        Direction::West,
        Direction::Up,
        Direction::Down,
        Direction::South,
        Direction::North,
    ];

    #[test]
    fn the_test_cube_really_is_wound_outward() {
        // Guard the guard: if this helper's winding were inward, every winding
        // assertion below would be measuring the wrong thing.
        for dir in ALL_FACES {
            let q = outward_face(dir);
            let n = (q[1] - q[0]).cross(q[2] - q[0]).normalize();
            let expect = q.iter().fold(Vec3::ZERO, |a, p| a + *p) / 4.0 - Vec3::splat(0.5);
            assert!(
                n.dot(expect.normalize()) > 0.99,
                "{dir:?} face must be wound counter-clockwise from outside"
            );
        }
    }

    /// The signed screen area of a quad's first triangle after `m`. Its **sign**
    /// is what `FrontFace::Ccw` + `cull_mode: Back` acts on; positions come from
    /// `mesh_item_quads`' `[0, 1, 2]` triangle.
    fn screen_area(m: Mat4, q: [Vec3; 4]) -> f32 {
        let p: Vec<Vec3> = q.iter().map(|v| m.project_point3(*v)).collect();
        let a = p[1] - p[0];
        let b = p[2] - p[0];
        a.x * b.y - a.y * b.x
    }

    /// The mean clip-space depth of a quad after `m`.
    fn mean_depth(m: Mat4, q: [Vec3; 4]) -> f32 {
        q.iter().map(|v| m.project_point3(*v).z).sum::<f32>() / 4.0
    }

    #[test]
    fn winding_matches_the_world_camera() {
        // Ground truth: the world path renders correctly today through the same
        // `ModelPipeline` (`FrontFace::Ccw`, cull `Back`) with the same
        // outward-wound quads. So whatever screen-area sign a *known visible*
        // face has under `Camera::view_projection` is the front-facing sign, and
        // the GUI matrix must reproduce it. Deriving the reference instead of
        // hardcoding "positive" means this test cannot be fooled by a wgpu/glam
        // convention we misremembered.
        let camera = Camera {
            position: Vec3::new(0.5, 0.5, 4.0),
            yaw: 180.0, // forward = (0, 0, -1): looking at the cube's +Z face
            pitch: 0.0,
            ..Camera::default()
        };
        let world = camera.view_projection();
        let front_sign = screen_area(world, outward_face(Direction::South)).signum();
        // Sanity: the face pointing away from the camera has the other sign.
        assert_eq!(
            screen_area(world, outward_face(Direction::North)).signum(),
            -front_sign,
            "the reference camera must disagree about the far face"
        );

        // The GUI matrix, composed exactly as the draw path composes it.
        let gui = gui_ortho(256, 256) * gui_item_pose([0.0, 0.0, 16.0, 16.0], &vanilla_block_gui());

        // With rotation [30, 225, 0] the three faces turned towards the viewer
        // are up, east and north — which is why a furnace item shows its front
        // (a north face on `block/orientable`) in the inventory.
        let visible = [Direction::Up, Direction::East, Direction::North];
        let hidden = [Direction::Down, Direction::West, Direction::South];

        for dir in visible {
            assert_eq!(
                screen_area(gui, outward_face(dir)).signum(),
                front_sign,
                "{dir:?} must survive back-face culling in the GUI pose"
            );
        }
        for dir in hidden {
            assert_eq!(
                screen_area(gui, outward_face(dir)).signum(),
                -front_sign,
                "{dir:?} must be culled in the GUI pose"
            );
        }

        // ...and the surviving faces must also be the *nearest* ones, or the
        // depth test would hide exactly what culling kept. This is the half of
        // the inside-out bug that a still screenshot cannot show.
        let nearest_hidden = hidden
            .iter()
            .map(|d| mean_depth(gui, outward_face(*d)))
            .fold(f32::MAX, f32::min);
        for dir in visible {
            let d = mean_depth(gui, outward_face(dir));
            assert!(
                d < nearest_hidden,
                "{dir:?} (depth {d}) must be in front of every culled face ({nearest_hidden})"
            );
        }
    }

    #[test]
    fn the_two_y_flips_cancel_against_the_world_convention() {
        // The determinant restates the winding result compactly. Note the sign
        // is *negative*, matching `Camera::view_projection` — the invariant is
        // "same sign as the world path", not "positive": glam's
        // `rh::proj::directx::perspective` itself has a negative determinant.
        let camera = Camera::default();
        let world = camera.view_projection().determinant();
        let gui = (gui_ortho(256, 256)
            * gui_item_pose([0.0, 0.0, 16.0, 16.0], &vanilla_block_gui()))
        .determinant();
        assert_eq!(
            world.signum(),
            gui.signum(),
            "GUI and world matrices must agree on handedness (world {world}, gui {gui})"
        );

        // And the flips are genuinely two: each half is orientation-reversing on
        // its own, so dropping either one would invert the composition.
        assert!(gui_ortho(256, 256).determinant() > 0.0);
        assert!(
            gui_item_pose([0.0, 0.0, 16.0, 16.0], &vanilla_block_gui()).determinant() < 0.0,
            "the pose's scale(w, -h, d) is orientation-reversing"
        );
    }
}
