//! Tests for `builtin/generated` item-model extrusion.

use lodestone_assets::Direction;
use lodestone_assets::Image;
use lodestone_assets::item::{ItemQuad, bake_item_generated};

/// A sprite where `mask[y][x] == true` means opaque.
fn sprite(mask: &[&[bool]]) -> Image {
    let h = mask.len() as u32;
    let w = mask[0].len() as u32;
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for row in mask {
        for &opaque in *row {
            let a = if opaque { 255 } else { 0 };
            rgba.extend_from_slice(&[200, 100, 50, a]);
        }
    }
    Image {
        width: w,
        height: h,
        rgba,
    }
}

fn front_back(quads: &[ItemQuad]) -> (usize, usize) {
    (
        quads
            .iter()
            .filter(|q| q.direction == Direction::South)
            .count(),
        quads
            .iter()
            .filter(|q| q.direction == Direction::North)
            .count(),
    )
}

#[test]
fn always_emits_front_and_back_even_when_fully_transparent() {
    let img = sprite(&[&[false, false], &[false, false]]);
    let quads = bake_item_generated(&[&img]);
    let (f, b) = front_back(&quads);
    assert_eq!((f, b), (1, 1), "front+back always emitted");
    // No opaque pixels → no side walls.
    assert_eq!(quads.len(), 2);
}

#[test]
fn front_face_covers_full_sprite_uv_at_max_z() {
    let img = sprite(&[&[true]]);
    let quads = bake_item_generated(&[&img]);
    let front = quads
        .iter()
        .find(|q| q.direction == Direction::South)
        .unwrap();
    // Front sits at z = 8.5/16.
    for p in front.positions {
        assert!((p[2] - 8.5 / 16.0).abs() < 1e-6);
    }
    // Spans the full [0,1] quad in x and y.
    let xs: Vec<f32> = front.positions.iter().map(|p| p[0]).collect();
    let ys: Vec<f32> = front.positions.iter().map(|p| p[1]).collect();
    assert!(xs.iter().cloned().fold(f32::MAX, f32::min).abs() < 1e-6);
    assert!((xs.iter().cloned().fold(f32::MIN, f32::max) - 1.0).abs() < 1e-6);
    assert!(ys.iter().cloned().fold(f32::MAX, f32::min).abs() < 1e-6);
    assert!((ys.iter().cloned().fold(f32::MIN, f32::max) - 1.0).abs() < 1e-6);
    // UV covers the whole sprite.
    let umin = front.uvs.iter().map(|u| u[0]).fold(f32::MAX, f32::min);
    let umax = front.uvs.iter().map(|u| u[0]).fold(f32::MIN, f32::max);
    assert!(umin.abs() < 1e-6 && (umax - 1.0).abs() < 1e-6);
}

#[test]
fn back_face_is_at_min_z() {
    let img = sprite(&[&[true]]);
    let quads = bake_item_generated(&[&img]);
    let back = quads
        .iter()
        .find(|q| q.direction == Direction::North)
        .unwrap();
    for p in back.positions {
        assert!((p[2] - 7.5 / 16.0).abs() < 1e-6);
    }
}

#[test]
fn single_opaque_pixel_has_four_side_walls() {
    let img = sprite(&[&[true]]);
    let quads = bake_item_generated(&[&img]);
    // front + back + 4 perimeter walls.
    assert_eq!(quads.len(), 6);
    let sides = quads.len() - 2;
    assert_eq!(sides, 4);
    // Every side wall spans the extrusion depth 7.5..8.5.
    for q in quads
        .iter()
        .filter(|q| !matches!(q.direction, Direction::North | Direction::South))
    {
        let zmin = q.positions.iter().map(|p| p[2]).fold(f32::MAX, f32::min);
        let zmax = q.positions.iter().map(|p| p[2]).fold(f32::MIN, f32::max);
        assert!((zmin - 7.5 / 16.0).abs() < 1e-6);
        assert!((zmax - 8.5 / 16.0).abs() < 1e-6);
    }
}

#[test]
fn fully_opaque_2x2_has_perimeter_walls_only() {
    let img = sprite(&[&[true, true], &[true, true]]);
    let quads = bake_item_generated(&[&img]);
    // Perimeter of a 2x2 block = 2*w + 2*h = 8 walls; interior edges are shared
    // between two opaque pixels and produce none.
    let sides = quads.len() - 2;
    assert_eq!(sides, 8, "only the outer boundary produces walls");
}

#[test]
fn interior_hole_adds_walls() {
    // A 3x3 ring (hole in the middle) adds 4 inner walls on top of the 12 outer.
    let t = true;
    let f = false;
    let img = sprite(&[&[t, t, t], &[t, f, t], &[t, t, t]]);
    let quads = bake_item_generated(&[&img]);
    let sides = quads.len() - 2;
    assert_eq!(sides, 3 * 4 + 4, "outer perimeter (12) plus the hole (4)");
}

#[test]
fn multiple_layers_each_contribute_geometry() {
    let a = sprite(&[&[true]]);
    let b = sprite(&[&[true]]);
    let quads = bake_item_generated(&[&a, &b]);
    // Two layers → two front faces, tagged by layer.
    assert_eq!(
        quads
            .iter()
            .filter(|q| q.direction == Direction::South)
            .count(),
        2
    );
    assert!(quads.iter().any(|q| q.layer == 0));
    assert!(quads.iter().any(|q| q.layer == 1));
}

#[test]
fn empty_layers_produce_nothing() {
    let quads = bake_item_generated(&[]);
    assert!(quads.is_empty());
}
