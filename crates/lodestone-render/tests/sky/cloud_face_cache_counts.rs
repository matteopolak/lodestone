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
//! * the geometry must be **byte-identical** to the uncached build on *every*
//!   frame, including the frames that hit the cache. A cache that changes the
//!   output is a rendering bug, so this is asserted bit-wise against
//!   [`fancy_cloud_geometry`] rather than by checking that the cache "works".
//!
//! The vertex expansion deliberately stays per frame: the sub-cell scroll offset
//! moves every tick, so the frames below move the camera *within* one cell and the
//! output is still expected to change. That is the property that makes this a real
//! test rather than a tautology — see `a_sub_cell_step_changes_the_vertices_but_not_the_faces`.

use lodestone_render::cloud_mesh::{CloudCells, CloudFaceCache, CloudRelativePos};
use lodestone_render::sky::{
    CLOUD_CELL_BLOCKS, CLOUD_HEIGHT, cloud_relative_pos_for_camera_y, fancy_cloud_geometry,
    fancy_cloud_geometry_cached,
};

/// A tint with all four channels distinct, so a colour written to the wrong
/// channel shows up.
const TINT: [f32; 4] = [0.9, 0.8, 0.7, 0.6];
const TIME_OF_DAY: i64 = 6_000;

/// A 16×16 cloud texture with a filled diagonal band, i.e. a pattern with both
/// filled and empty neighbours in every direction.
///
/// **World species**: an all-empty texture makes `extruded_faces` return
/// immediately (`cells.is_empty()`), so every count below would be trivially
/// satisfied and every vertex list trivially equal. `the_fixture_really_produces_faces`
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

/// Bit patterns, not `f32` equality: a rebuilt-but-equal-looking vertex stream
/// with a `-0.0` or a different NaN would pass `==` on some values and is not
/// "identical output".
fn bits(verts: &[([f32; 3], [f32; 4])]) -> Vec<u32> {
    let mut out = Vec::with_capacity(verts.len() * 7);
    for (p, c) in verts {
        out.extend(p.iter().map(|f| f.to_bits()));
        out.extend(c.iter().map(|f| f.to_bits()));
    }
    out
}

/// Assert two vertex streams are bit-identical, reporting **where** rather than
/// dumping both streams.
///
/// The first version of this printed the two `Vec<u32>`s and produced 857 KB of
/// hex on its first real failure, which is a failure report nobody reads. What
/// locates a cloud bug is the vertex index, the component within it and the two
/// values — 7 components per vertex, `(x, y, z, r, g, b, a)`.
fn assert_same_mesh(cached: &[([f32; 3], [f32; 4])], uncached: &[([f32; 3], [f32; 4])], what: &str) {
    let (a, b) = (bits(cached), bits(uncached));
    assert_eq!(
        a.len(),
        b.len(),
        "{what}: {} cached verts vs {} uncached — the face lists are different lengths",
        cached.len(),
        uncached.len()
    );
    let differing = a.iter().zip(&b).filter(|(l, r)| l != r).count();
    if differing == 0 {
        return;
    }
    let first = a.iter().zip(&b).position(|(l, r)| l != r).unwrap();
    const COMPONENT: [&str; 7] = ["x", "y", "z", "r", "g", "b", "a"];
    panic!(
        "{what}: {differing} of {} components differ; first at vertex {} \
         component {} (cached {}, uncached {}); last differing vertex {}",
        a.len(),
        first / 7,
        COMPONENT[first % 7],
        f32::from_bits(a[first]),
        f32::from_bits(b[first]),
        a.iter()
            .zip(&b)
            .rposition(|(l, r)| l != r)
            .unwrap_or(first)
            / 7,
    );
}

/// Camera **below** the layer, which is where a player normally is.
fn below(x: f32, z: f32) -> [f32; 3] {
    [x, CLOUD_HEIGHT - 40.0, z]
}

#[test]
fn the_fixture_really_produces_faces() {
    let cells = cells();
    assert!(!cells.is_empty(), "an empty texture short-circuits everything");
    let verts = fancy_cloud_geometry(&cells, below(0.0, 0.0), TIME_OF_DAY, TINT);
    assert!(
        verts.len() >= 4 * 100,
        "expected a substantial mesh from a mostly-filled texture at radius 16, \
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
        let cached = fancy_cloud_geometry_cached(&mut cache, &cells, camera, TIME_OF_DAY, TINT);
        let uncached = fancy_cloud_geometry(&cells, camera, TIME_OF_DAY, TINT);
        assert_same_mesh(&cached, &uncached, &format!("sub-cell frame at x={step}"));
        let now = bits(&cached);
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
        "and all six frames must still produce *different* vertices — the sub-cell \
         scroll is per frame, so a cache that froze the geometry between crossings \
         would show up here as fewer than 6"
    );
}

/// Crossing a cell boundary must re-enumerate, and the faces must actually change
/// — otherwise the counter above is satisfied by a cache that never invalidates.
#[test]
fn crossing_a_cell_re_enumerates_and_the_faces_really_differ() {
    let cells = cells();
    let mut cache = CloudFaceCache::default();

    let mut previous_faces: Option<Vec<u32>> = None;
    let mut crossings = 0;
    for cell in 0..4 {
        // One camera position per cell, at the *same* in-cell offset each time, so
        // the only thing that changes between these samples is the cell — if the
        // vertices still differ, it is the face list that moved and not the scroll.
        let camera = below(cell as f32 * CLOUD_CELL_BLOCKS, 0.0);
        let cached = fancy_cloud_geometry_cached(&mut cache, &cells, camera, TIME_OF_DAY, TINT);
        let uncached = fancy_cloud_geometry(&cells, camera, TIME_OF_DAY, TINT);
        assert_same_mesh(&cached, &uncached, &format!("cell {cell}"));
        let now = bits(&cached);
        assert!(
            previous_faces.as_ref() != Some(&now),
            "cell {cell} produced the same mesh as the previous cell, so this \
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
        let cached = fancy_cloud_geometry_cached(&mut cache, &cells, camera, TIME_OF_DAY, TINT);
        let uncached = fancy_cloud_geometry(&cells, camera, TIME_OF_DAY, TINT);
        assert_same_mesh(&cached, &uncached, &format!("camera y={y}"));
        meshes.push(cached.len());
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
