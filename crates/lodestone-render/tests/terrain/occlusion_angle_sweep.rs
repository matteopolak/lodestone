//! **Does the occlusion graph ever hide something you can see?** — the angle
//! sweep for U3.
//!
//! # Why an angle sweep specifically
//!
//! Every way the occlusion cull can go *wrong* except one draws more than it
//! should: an absent graph entry reads as open, an unwalkable graph degrades to
//! frustum ∩ distance, a stale cached set is a superset. The one direction that
//! loses pixels is the walk over-culling, and that failure is **angle-dependent**
//! — the pre-merge `walk_visible` lost a section reachable through two faces
//! depending on which the BFS reached first, which depends on `Face::ALL`'s order
//! and on where the camera stands. A single-orientation gate passes on it. So
//! every arm below sweeps 24 headings × 5 pitches and asserts on *every*
//! orientation, and the failure message names the orientation.
//!
//! # What is not GPU-tested here, and why that is honest
//!
//! These arms assert on `CullVerdict`, not on pixels. The verdict is the whole of
//! the decision — `gpu/frame.rs` does nothing but `continue` on a non-`Visible`
//! one — and a pixel gate over 120 orientations per arm would cost minutes of GPU
//! readback to re-answer the same question. The pixel half of the culling work is
//! `gpu/pixel_gates.rs`'s existing coverage of the draw path.
//!
//! Every expected value below comes from the *fixture's own geometry*, stated in
//! the comment above it, not from calling the walk.

use std::collections::HashSet;

use glam::Vec3;
use lodestone_render::{
    Camera, CullVerdict, Face, SectionCoord, SectionVisibility, TerrainCull, VisibilityGraph,
    reachable_from_camera,
};

/// Headings swept, in degrees. 24 is 15° apart — finer than the 45° a
/// `Face::ALL`-order bug needs to show up at, and cheap.
const HEADINGS: u32 = 24;

/// Pitches swept. Straight up and straight down are in here because the
/// never-reverse-axis rule and the Y faces are where a transcription flip hides:
/// a surface camera looking *down* is exactly the case U3 exists to make cheap,
/// so it is also the case an over-cull would break.
const PITCHES: [f32; 5] = [-89.0, -45.0, 0.0, 45.0, 89.0];

const RENDER_DISTANCE: u32 = 8;

fn camera_at(position: Vec3, yaw: f32, pitch: f32) -> Camera {
    Camera {
        position,
        yaw,
        pitch,
        aspect: 16.0 / 9.0,
        fov_y_degrees: 70.0,
        near: 0.05,
        far: Camera::far_for_render_distance(RENDER_DISTANCE, 0),
    }
}

/// Every (yaw, pitch) pair in the sweep.
fn orientations() -> impl Iterator<Item = (f32, f32)> {
    (0..HEADINGS).flat_map(|h| {
        PITCHES
            .iter()
            .map(move |p| (h as f32 * (360.0 / HEADINGS as f32), *p))
    })
}

/// The cull as production builds it: this graph's reachable set installed, cull
/// enabled, enforcing.
fn cull_for(graph: &VisibilityGraph, camera: &Camera) -> (TerrainCull, HashSet<SectionCoord>) {
    let reachable = reachable_from_camera(graph, camera.position, RENDER_DISTANCE)
        .expect("a non-empty graph at a non-zero render distance must produce a reachable set");
    let cull = TerrainCull::new(camera, RENDER_DISTANCE)
        .with_reachable(Some(std::sync::Arc::new(reachable.clone())));
    assert!(
        cull.occlusion_active(),
        "the reachable set did not install, so every assertion below would be measuring \
         frustum ∩ distance"
    );
    (cull, reachable)
}

/// A surface: solid ground with air above it, which is the shape U3 exists for.
///
/// Rows (section-grid Y): `-1` and below are solid *interior* stone — they mesh
/// to nothing in production and their connectivity connects no faces. Row `0` is
/// the ground surface: stone below, air above, so its `NegY` face is sealed and
/// its `PosY` face is open. Rows `1..=3` are air, and **absent from the graph
/// entirely** — all-air sections are never meshed, which is the trap this whole
/// unit turns on.
fn surface_graph() -> VisibilityGraph {
    let mut graph = VisibilityGraph::new();
    // A surface section: everything except the sealed bottom face connects.
    let surface = SectionVisibility::from_pairs(&[
        (Face::NegX, Face::PosX),
        (Face::NegX, Face::PosY),
        (Face::NegX, Face::NegZ),
        (Face::NegX, Face::PosZ),
        (Face::PosX, Face::PosY),
        (Face::PosX, Face::NegZ),
        (Face::PosX, Face::PosZ),
        (Face::PosY, Face::NegZ),
        (Face::PosY, Face::PosZ),
        (Face::NegZ, Face::PosZ),
    ]);
    for x in -6..=6 {
        for z in -6..=6 {
            for y in -4..=0 {
                let vis = if y == 0 {
                    surface
                } else {
                    // Interior stone: connects nothing. This is the section that
                    // makes the underground free, and it has no geometry at all.
                    SectionVisibility::NONE
                };
                graph.insert((x, y, z), vis);
            }
        }
    }
    graph
}

/// The headline: a camera in the air over solid ground removes the underground at
/// every orientation, and never removes the ground it is standing on.
#[test]
fn surface_camera_keeps_the_ground_and_drops_the_underground_at_every_orientation() {
    let graph = surface_graph();
    // Eye at y = 20, i.e. section row 1 — one row of (unmeshed, absent) air above
    // the surface row 0.
    let position = Vec3::new(8.0, 20.0, 8.0);

    // The fixture's own claim, asserted rather than assumed (the *world* species
    // of vacuous test): the surface row seals downward and the row below it seals
    // in every direction. If either were open the underground would be legitimately
    // reachable and this gate would be measuring nothing.
    assert!(!graph.get((0, 0, 0)).unwrap().connects(Face::NegY, Face::PosY));
    assert!(!graph.get((0, -1, 0)).unwrap().connects(Face::PosY, Face::NegY));
    assert!(graph.get((0, 1, 0)).is_none(), "the air row must be absent");

    // Orientations at which the underground section was removed *by the graph*
    // rather than by the frustum. `classify` reports the first test that fires and
    // the frustum runs first, so looking straight up legitimately reports
    // `Frustum` — asserting `Occlusion` at every orientation would be asserting
    // something false. What must hold everywhere is that it is never `Visible`;
    // what must hold *somewhere* is that the graph is the reason, which is the
    // count below.
    let mut occluded_at = 0usize;
    for (yaw, pitch) in orientations() {
        let camera = camera_at(position, yaw, pitch);
        let (cull, reachable) = cull_for(&graph, &camera);

        // The camera's own section is unconditionally reachable, and the ground
        // directly below it is one face step through open air.
        assert!(reachable.contains(&(0, 1, 0)), "yaw {yaw} pitch {pitch}");
        assert_eq!(
            cull.classify((0, 0, 0)),
            CullVerdict::Visible,
            "the ground under the camera was culled at yaw {yaw} pitch {pitch} — this is the \
             terrain-vanishes bug"
        );
        // Two rows down is behind sealed stone.
        let verdict = cull.classify((0, -2, 0));
        assert_ne!(
            verdict,
            CullVerdict::Visible,
            "a section two rows under sealed stone was drawn at yaw {yaw} pitch {pitch}"
        );
        if verdict == CullVerdict::Occlusion {
            occluded_at += 1;
            // Both hypotheses, and only where they are comparable: at this
            // orientation the frustum keeps the section, so the no-walk cull must
            // draw it. If it did not, the cull being measured would be the
            // frustum's wearing the occlusion counter's name.
            let no_walk = TerrainCull::new(&camera, RENDER_DISTANCE);
            assert_eq!(
                no_walk.classify((0, -2, 0)),
                CullVerdict::Visible,
                "yaw {yaw} pitch {pitch}"
            );
        }
        // Unreachability, on the other hand, is heading-independent and must hold
        // for the *whole* subsurface at every orientation, not just the column
        // under the camera.
        for x in -3..=3 {
            for z in -3..=3 {
                assert!(
                    !reachable.contains(&(x, -2, z)),
                    "({x},-2,{z}) reachable at yaw {yaw} pitch {pitch}"
                );
            }
        }
    }
    assert!(
        occluded_at > 0,
        "the underground was never attributed to occlusion at any of the {} orientations, so \
         the frustum removed it at all of them and this gate proves nothing about the graph",
        HEADINGS as usize * PITCHES.len()
    );
}

/// The control for the arm above, and it must **pass by reaching**, not by
/// culling: punch a vertical shaft through the same surface and the underground
/// under that shaft becomes reachable again at every orientation.
///
/// Without this, "the underground is unreachable" is satisfied by a walk that
/// reaches nothing at all — which is precisely the silent degradation this unit's
/// gotcha is about, only with the sign flipped. Same fixture, same assertions,
/// opposite expected outcome.
#[test]
fn a_shaft_through_the_ground_makes_the_underground_reachable_again() {
    let mut graph = surface_graph();
    // A full-height open shaft at column (0, *, 0): the surface section and every
    // section below it now connect PosY↔NegY.
    let shaft = SectionVisibility::from_pairs(&[(Face::PosY, Face::NegY)]);
    for y in -4..=0 {
        graph.insert((0, y, 0), shaft);
    }
    let position = Vec3::new(8.0, 20.0, 8.0);
    for (yaw, pitch) in orientations() {
        let camera = camera_at(position, yaw, pitch);
        let (_, reachable) = cull_for(&graph, &camera);
        for y in -4..=0 {
            assert!(
                reachable.contains(&(0, y, 0)),
                "the shaft section at row {y} is unreachable at yaw {yaw} pitch {pitch}, so the \
                 sealed arm's result is not evidence about occlusion"
            );
        }
        // And the sealed columns beside the shaft are still removed: the shaft
        // opens a path down, not the whole subsurface.
        assert!(!reachable.contains(&(2, -2, 2)), "yaw {yaw} pitch {pitch}");
    }
}

/// Reachability is heading-independent by construction (the walk takes no
/// frustum), which is the property that makes caching it across frames sound.
/// Stated as a test because it is also the property a "clever" optimisation would
/// break first.
#[test]
fn the_reachable_set_does_not_depend_on_where_the_camera_looks() {
    let graph = surface_graph();
    let position = Vec3::new(8.0, 20.0, 8.0);
    let baseline = reachable_from_camera(&graph, position, RENDER_DISTANCE).unwrap();
    assert!(baseline.len() > 1, "a degenerate walk proves nothing");
    for (yaw, pitch) in orientations() {
        let camera = camera_at(position, yaw, pitch);
        let set = reachable_from_camera(&graph, camera.position, RENDER_DISTANCE).unwrap();
        assert_eq!(set, baseline, "yaw {yaw} pitch {pitch} changed the walk");
    }
}

/// An enclosed space: the camera sealed in a one-section room. The six sections
/// forming the walls are what you can see, and they must never be culled at any
/// orientation that has them on screen; anything past them must always be.
#[test]
fn a_sealed_room_keeps_its_own_walls_and_drops_everything_past_them() {
    let mut graph = VisibilityGraph::new();
    for x in -4..=4 {
        for y in -4..=4 {
            for z in -4..=4 {
                graph.insert((x, y, z), SectionVisibility::NONE);
            }
        }
    }
    // The room itself: the one open section, at the origin.
    graph.insert((0, 0, 0), SectionVisibility::all());
    let position = Vec3::new(8.0, 8.0, 8.0);

    for (yaw, pitch) in orientations() {
        let camera = camera_at(position, yaw, pitch);
        let (cull, reachable) = cull_for(&graph, &camera);
        assert_eq!(
            cull.classify((0, 0, 0)),
            CullVerdict::Visible,
            "the camera's own section was culled at yaw {yaw} pitch {pitch}"
        );
        // The six face neighbours are the walls of the room — reachable at every
        // orientation, because reachability ignores the frustum.
        for wall in [
            (1, 0, 0),
            (-1, 0, 0),
            (0, 1, 0),
            (0, -1, 0),
            (0, 0, 1),
            (0, 0, -1),
        ] {
            assert!(
                reachable.contains(&wall),
                "wall {wall:?} unreachable from inside the room at yaw {yaw} pitch {pitch}"
            );
            // Whichever of them the frustum keeps must be drawn, and the verdict
            // for the rest must be `Frustum` — never `Occlusion`, which would mean
            // the walk removed a wall of the room the player is standing in.
            assert_ne!(
                cull.classify(wall),
                CullVerdict::Occlusion,
                "wall {wall:?} attributed to occlusion at yaw {yaw} pitch {pitch}"
            );
        }
        // Two sections out in any direction is behind a solid wall.
        for far in [(2, 0, 0), (-2, 0, 0), (0, 2, 0), (0, -2, 0), (0, 0, 2), (0, 0, -2)] {
            assert!(
                !reachable.contains(&far),
                "{far:?} reachable through a solid wall at yaw {yaw} pitch {pitch}"
            );
        }
    }
}

/// A cave mouth: a corridor of open sections leading out of a sealed room. Every
/// section of the corridor stays reachable at every orientation — this is the arm
/// that would have failed under the pre-merge single-entry-face walk, since the
/// corridor turns a corner and is therefore reachable through two faces.
#[test]
fn a_corridor_that_turns_a_corner_stays_reachable_at_every_orientation() {
    let mut graph = VisibilityGraph::new();
    for x in -4..=4 {
        for y in -4..=4 {
            for z in -4..=4 {
                graph.insert((x, y, z), SectionVisibility::NONE);
            }
        }
    }
    // Room at the origin, then +X, then a corner, then +Z: the corner section is
    // entered through NegX and exits through PosZ, so it must connect that pair.
    graph.insert((0, 0, 0), SectionVisibility::all());
    graph.insert((1, 0, 0), SectionVisibility::from_pairs(&[(Face::NegX, Face::PosX)]));
    graph.insert((2, 0, 0), SectionVisibility::from_pairs(&[(Face::NegX, Face::PosZ)]));
    graph.insert((2, 0, 1), SectionVisibility::from_pairs(&[(Face::NegZ, Face::PosZ)]));
    graph.insert((2, 0, 2), SectionVisibility::all());
    let corridor = [(1, 0, 0), (2, 0, 0), (2, 0, 1), (2, 0, 2)];
    let position = Vec3::new(8.0, 8.0, 8.0);

    for (yaw, pitch) in orientations() {
        let camera = camera_at(position, yaw, pitch);
        let (_, reachable) = cull_for(&graph, &camera);
        for section in corridor {
            assert!(
                reachable.contains(&section),
                "corridor section {section:?} unreachable at yaw {yaw} pitch {pitch}"
            );
        }
        // The control that the corridor is a corridor and not a hole in the
        // fixture. `(2,0,3)` is the *wall* at the corridor's end and is
        // legitimately reachable — an adjacent blocker is drawn, since it is the
        // surface you are looking at; `(2,0,4)` is behind it, and `(1,0,1)` is
        // beside the corner with no connected face into it.
        assert!(!reachable.contains(&(2, 0, 4)), "yaw {yaw} pitch {pitch}");
        assert!(!reachable.contains(&(1, 0, 1)), "yaw {yaw} pitch {pitch}");
    }
}

/// `render_distance_chunks == 0` must not produce a set — the walk's horizontal
/// bound *is* the view circle, and a zero-radius circle is empty. The failure this
/// pins is a blank world on a default-constructed render state.
#[test]
fn zero_render_distance_produces_no_reachable_set() {
    let graph = surface_graph();
    assert!(reachable_from_camera(&graph, Vec3::new(8.0, 20.0, 8.0), 0).is_none());
    // And the control: the same graph and position at a real render distance does.
    assert!(reachable_from_camera(&graph, Vec3::new(8.0, 20.0, 8.0), 8).is_some());
}

/// An empty graph produces no set, which is what makes the whole unit inert until
/// something populates it (the packed demo path never does).
#[test]
fn an_empty_graph_produces_no_reachable_set() {
    let graph = VisibilityGraph::new();
    assert!(reachable_from_camera(&graph, Vec3::ZERO, 8).is_none());
}
