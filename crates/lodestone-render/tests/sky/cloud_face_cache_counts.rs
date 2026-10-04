//! How often the FANCY cloud mesh re-enumerates its faces, and that memoising it
//! changes nothing on screen — the face set is a pure function of data that only
//! changes on an event, so recomputing it every frame is pure waste.
//!
//! [`CloudFaceCache`] memoises [`extruded_faces`] on its own four arguments, which
//! were already exactly the cache key. Two things have to hold and they pull in
//! opposite directions:
//!
//! * the enumeration must happen **once per camera cell** (and once per layer
//!   entry/exit), not once per frame — the counter;
//! * the cached list must be **identical** to a fresh enumeration on *every*
//!   frame, including the frames that hit the cache. A cache that changes the
//!   output is a rendering bug, so this is asserted face for face against
//!   [`extruded_faces`] rather than by checking that the cache "works".
//!
//! The in-cell scroll is not part of the cached list: the renderer passes it to
//! the shader every frame. The frames below move the camera *within* one cell and
//! the reference mesh ([`fancy_cloud_geometry`]) is still expected to change —
//! the property that makes this a real test rather than a tautology.

use lodestone_render::cloud_mesh::{CloudCells, CloudFace, CloudFaceCache, CloudRelativePos, extruded_faces};
use lodestone_render::sky::{
    CLOUD_CELL_BLOCKS, CLOUD_FANCY_RADIUS_CELLS, CLOUD_HEIGHT, cloud_cell_and_offset,
    cloud_relative_pos_for_camera_y, fancy_cloud_geometry,
};

/// A tint with all four channels distinct, so a colour written to the wrong
/// channel shows up.
const TINT: [f32; 4] = [0.9, 0.8, 0.7, 0.6];
const GAME_TIME: f64 = 6_000.0;

/// A 16×16 cloud texture with a filled diagonal band, i.e. a pattern with both
/// filled and empty neighbours in every direction.
///
/// **World species**: an all-empty texture makes `extruded_faces` return
/// immediately (`cells.is_empty()`), so every count below would be trivially
/// satisfied and every list trivially equal. `the_fixture_really_produces_faces`
/// is the control that says this one does not.
fn cells() -> CloudCells {
    let (w, h) = (16u32, 16u32);
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let filled = (x + y) % 3 != 0;
            let px = ((y * w + x) * 4) as usize;
            rgba[px] = 255;
            rgba[px + 1] = 255;
            rgba[px + 2] = 255;
            rgba[px + 3] = if filled { 255 } else { 0 };
        }
    }
    CloudCells::from_rgba(w, h, &rgba)
}

/// The faces the cache holds for `camera`, next to a fresh enumeration for the
/// same camera, with the reference mesh's bits for change detection.
fn frame(cache: &mut CloudFaceCache, cells: &CloudCells, camera: [f32; 3]) -> (Vec<CloudFace>, Vec<u32>) {
    let (w, h) = cells.dimensions();
    let (cx, cz, _, _) = cloud_cell_and_offset(camera, GAME_TIME, w, h);
    let pos = cloud_relative_pos_for_camera_y(camera[1]);
    let cached = cache.faces(cells, cx, cz, CLOUD_FANCY_RADIUS_CELLS, pos).to_vec();
    let fresh = extruded_faces(cells, cx, cz, CLOUD_FANCY_RADIUS_CELLS, pos);
    assert!(cached == fresh, "camera {camera:?}: the cached list differs from a fresh enumeration");
    let mesh = fancy_cloud_geometry(cells, camera, GAME_TIME, TINT)
        .iter()
        .flat_map(|(p, c)| p.iter().chain(c).map(|f| f.to_bits()).collect::<Vec<_>>())
        .collect();
    (cached, mesh)
}

/// Camera **below** the layer, which is where a player normally is.
fn below(x: f32, z: f32) -> [f32; 3] {
    [x, CLOUD_HEIGHT - 40.0, z]
}

#[test]
fn the_fixture_really_produces_faces() {
    let cells = cells();
    assert!(!cells.is_empty(), "an empty texture short-circuits everything");
    let verts = fancy_cloud_geometry(&cells, below(0.0, 0.0), GAME_TIME, TINT);
    assert!(
        verts.len() >= 4 * 100,
        "expected a substantial mesh from a mostly-filled texture, \
         got {} verts — a small one means the fixture is not exercising the walk",
        verts.len()
    );
    assert_eq!(verts.len() % 4, 0, "faces are quads");
}

/// The counter, with both hypotheses named: **1** enumeration for a whole run of
/// frames inside one cell, against **one per frame** (the pre-cache
/// implementation).
#[test]
fn a_sub_cell_step_changes_the_vertices_but_not_the_faces() {
    let cells = cells();
    let mut cache = CloudFaceCache::default();

    // Six frames, each moving less than a cell (12 blocks), so all six share the
    // camera's cell. The first frame's position is deliberately not on a cell
    // boundary.
    let steps: [f32; 6] = [0.3, 1.1, 2.0, 3.7, 5.2, 9.9];
    let mut previous: Option<Vec<u32>> = None;
    let mut distinct = 0;
    for step in steps {
        let camera = below(step, 0.0);
        let (_, now) = frame(&mut cache, &cells, camera);
        if previous.as_ref() != Some(&now) {
            distinct += 1;
        }
        previous = Some(now);
    }

    assert_eq!(
        cache.rebuilds(),
        1,
        "expected one face enumeration for six frames in one cell; 6 is the \
         pre-cache implementation (one per frame)"
    );
    assert_eq!(
        distinct, 6,
        "and all six frames must still produce *different* reference meshes — the \
         sub-cell scroll is per frame, so fewer than 6 means the fixture cannot see \
         a renderer that froze the scroll between crossings"
    );
}

/// Crossing a cell boundary must re-enumerate, and the faces must actually change
/// — otherwise the counter above is satisfied by a cache that never invalidates.
#[test]
fn crossing_a_cell_re_enumerates_and_the_faces_really_differ() {
    let cells = cells();
    let mut cache = CloudFaceCache::default();

    let mut previous_faces: Option<Vec<CloudFace>> = None;
    let mut crossings = 0;
    for cell in 0..4 {
        // One camera position per cell, at the *same* in-cell offset each time, so
        // the only thing that changes between these samples is the cell — if the
        // faces still differ, it is the face list that moved and not the scroll.
        let camera = below(cell as f32 * CLOUD_CELL_BLOCKS, 0.0);
        let (now, _) = frame(&mut cache, &cells, camera);
        assert!(
            previous_faces.as_ref() != Some(&now),
            "cell {cell} produced the same faces as the previous cell, so this \
             fixture cannot see a cache that fails to invalidate"
        );
        previous_faces = Some(now);
        crossings += 1;
    }
    assert_eq!(crossings, 4);
    assert_eq!(
        cache.rebuilds(),
        4,
        "one enumeration per cell entered, no more and no fewer"
    );
}

/// `relative_pos` is the key component a reader drops, because it changes with the
/// camera's **y** at an unchanged cell. This is the control that it is really in
/// the key: the same cell, three vertical positions, three enumerations, three
/// different meshes.
#[test]
fn crossing_the_cloud_layer_re_enumerates_at_an_unchanged_cell() {
    let cells = cells();
    let mut cache = CloudFaceCache::default();

    let ys = [CLOUD_HEIGHT - 40.0, CLOUD_HEIGHT + 2.0, CLOUD_HEIGHT + 40.0];
    // The premise, from the same function the production path calls: these three
    // really are the three distinct `CloudRelativePos` values. If they were not,
    // the assertions below would pass for the wrong reason.
    let positions: Vec<CloudRelativePos> = ys.iter().map(|y| cloud_relative_pos_for_camera_y(*y)).collect();
    assert_eq!(
        positions,
        vec![
            CloudRelativePos::BelowClouds,
            CloudRelativePos::InsideClouds,
            CloudRelativePos::AboveClouds
        ],
        "the fixture must span all three relative positions"
    );

    let mut meshes = Vec::new();
    for y in ys {
        let camera = [0.3_f32, y, 0.0];
        let (faces, _) = frame(&mut cache, &cells, camera);
        meshes.push(faces.len());
    }

    assert_eq!(
        cache.rebuilds(),
        3,
        "the camera never left its cell, so a cache keyed on the cell alone would \
         report 1 here and render the layer from the wrong side"
    );
    assert!(
        meshes[0] != meshes[1] || meshes[1] != meshes[2],
        "the three relative positions must produce different face counts \
         ({meshes:?}), or this control proves nothing about the key"
    );
}
