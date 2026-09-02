//! GUI sprite scaling (`gui.scaling`) parsing and geometry tests.
//!
//! Covers the three vanilla scaling modes — `stretch`, `tile`, `nine_slice` —
//! their `.mcmeta` parsing (scalar and object borders, validation), and the
//! renderer-agnostic geometry each produces at various target sizes.

use lodestone_assets::GuiError;
use lodestone_assets::gui::{Border, GuiMeta, GuiQuad, GuiScaling};

fn parse(json: &str) -> Result<GuiMeta, GuiError> {
    GuiMeta::parse(json.as_bytes())
}

/// Total destination area covered by a quad set (used to prove full coverage).
fn dst_area(quads: &[GuiQuad]) -> i64 {
    quads
        .iter()
        .map(|q| q.dst[2] as i64 * q.dst[3] as i64)
        .sum()
}

#[test]
fn missing_scaling_defaults_to_stretch() {
    let meta = parse(r#"{"gui":{}}"#).unwrap();
    assert_eq!(meta.scaling, GuiScaling::Stretch);
    // A completely empty document is also fine (default stretch).
    let meta = parse(r#"{}"#).unwrap();
    assert_eq!(meta.scaling, GuiScaling::Stretch);
}

#[test]
fn parses_tile_scaling() {
    let meta = parse(r#"{"gui":{"scaling":{"type":"tile","width":16,"height":16}}}"#).unwrap();
    assert_eq!(
        meta.scaling,
        GuiScaling::Tile {
            width: 16,
            height: 16
        }
    );
}

#[test]
fn parses_nine_slice_with_scalar_border() {
    let meta =
        parse(r#"{"gui":{"scaling":{"type":"nine_slice","width":200,"height":26,"border":10}}}"#)
            .unwrap();
    assert_eq!(
        meta.scaling,
        GuiScaling::NineSlice {
            width: 200,
            height: 26,
            border: Border {
                left: 10,
                top: 10,
                right: 10,
                bottom: 10
            },
            stretch_inner: false,
        }
    );
}

#[test]
fn parses_nine_slice_with_object_border_and_stretch_inner() {
    let meta = parse(
        r#"{"gui":{"scaling":{"type":"nine_slice","width":20,"height":20,
        "border":{"left":1,"top":2,"right":3,"bottom":4},"stretch_inner":true}}}"#,
    )
    .unwrap();
    assert_eq!(
        meta.scaling,
        GuiScaling::NineSlice {
            width: 20,
            height: 20,
            border: Border {
                left: 1,
                top: 2,
                right: 3,
                bottom: 4
            },
            stretch_inner: true,
        }
    );
}

#[test]
fn unknown_scaling_type_is_rejected() {
    assert!(matches!(
        parse(r#"{"gui":{"scaling":{"type":"warp"}}}"#),
        Err(GuiError::UnknownType(_))
    ));
}

#[test]
fn malformed_json_is_rejected() {
    assert!(matches!(parse("not json"), Err(GuiError::Json(_))));
}

#[test]
fn nine_slice_without_center_slice_is_rejected() {
    // left + right >= width -> no horizontal center.
    assert!(matches!(
        parse(
            r#"{"gui":{"scaling":{"type":"nine_slice","width":10,"height":10,
            "border":{"left":6,"top":1,"right":6,"bottom":1}}}}"#
        ),
        Err(GuiError::NoCenter(_))
    ));
    // top + bottom >= height -> no vertical center.
    assert!(matches!(
        parse(
            r#"{"gui":{"scaling":{"type":"nine_slice","width":10,"height":10,
            "border":{"left":1,"top":6,"right":1,"bottom":6}}}}"#
        ),
        Err(GuiError::NoCenter(_))
    ));
}

#[test]
fn zero_dimension_is_rejected() {
    assert!(matches!(
        parse(r#"{"gui":{"scaling":{"type":"tile","width":0,"height":16}}}"#),
        Err(GuiError::InvalidField(_))
    ));
}

#[test]
fn stretch_geometry_is_one_quad_spanning_the_target() {
    let quads = GuiScaling::Stretch.geometry(16, 16, 64, 48);
    assert_eq!(quads.len(), 1);
    assert_eq!(quads[0].dst, [0, 0, 64, 48]);
    assert_eq!(quads[0].src, [0.0, 0.0, 16.0, 16.0]);
}

#[test]
fn tile_geometry_repeats_at_native_size_and_crops_edges() {
    // 16x16 sprite tiled into a 40x16 target -> 3 columns: 16, 16, 8 (cropped).
    let scaling = GuiScaling::Tile {
        width: 16,
        height: 16,
    };
    let quads = scaling.geometry(16, 16, 40, 16);
    assert_eq!(quads.len(), 3);
    assert_eq!(quads[0].dst, [0, 0, 16, 16]);
    assert_eq!(quads[1].dst, [16, 0, 16, 16]);
    // Cropped final tile: 8px wide, sampling only the left half of the sprite.
    assert_eq!(quads[2].dst, [32, 0, 8, 16]);
    assert_eq!(quads[2].src, [0.0, 0.0, 8.0, 16.0]);
    // Full coverage, no overlap.
    assert_eq!(dst_area(&quads), 40 * 16);
}

#[test]
fn nine_slice_at_native_size_maps_one_to_one() {
    // At the native size the nine regions tile the sprite exactly.
    let scaling = GuiScaling::NineSlice {
        width: 20,
        height: 20,
        border: Border {
            left: 4,
            top: 4,
            right: 4,
            bottom: 4,
        },
        stretch_inner: true,
    };
    let quads = scaling.geometry(20, 20, 20, 20);
    assert_eq!(dst_area(&quads), 20 * 20);
    // A corner is present at the origin, border-sized, sampling the sprite corner.
    let tl = quads
        .iter()
        .find(|q| q.dst[0] == 0 && q.dst[1] == 0)
        .unwrap();
    assert_eq!(tl.dst, [0, 0, 4, 4]);
    assert_eq!(tl.src, [0.0, 0.0, 4.0, 4.0]);
}

#[test]
fn nine_slice_corners_stay_fixed_when_target_grows() {
    let scaling = GuiScaling::NineSlice {
        width: 20,
        height: 20,
        border: Border {
            left: 4,
            top: 4,
            right: 4,
            bottom: 4,
        },
        stretch_inner: true,
    };
    let quads = scaling.geometry(20, 20, 100, 60);
    // Corners keep their 4x4 size regardless of target growth.
    let tl = quads
        .iter()
        .find(|q| q.dst[0] == 0 && q.dst[1] == 0)
        .unwrap();
    assert_eq!(tl.dst, [0, 0, 4, 4]);
    let br = quads
        .iter()
        .find(|q| q.dst[0] == 96 && q.dst[1] == 56)
        .unwrap();
    assert_eq!(br.dst, [96, 56, 4, 4]);
    assert_eq!(br.src, [16.0, 16.0, 4.0, 4.0]);
    // Full coverage of the 100x60 target.
    assert_eq!(dst_area(&quads), 100 * 60);
}

#[test]
fn nine_slice_stretch_inner_center_is_a_single_quad() {
    let scaling = GuiScaling::NineSlice {
        width: 20,
        height: 20,
        border: Border {
            left: 4,
            top: 4,
            right: 4,
            bottom: 4,
        },
        stretch_inner: true,
    };
    let quads = scaling.geometry(20, 20, 100, 60);
    // Exactly one quad occupies the center region (offset by the border).
    let center: Vec<_> = quads
        .iter()
        .filter(|q| q.dst[0] == 4 && q.dst[1] == 4)
        .collect();
    assert_eq!(center.len(), 1);
    assert_eq!(center[0].dst, [4, 4, 92, 52]);
    assert_eq!(center[0].src, [4.0, 4.0, 12.0, 12.0]);
}

#[test]
fn nine_slice_tiled_inner_repeats_the_center() {
    let scaling = GuiScaling::NineSlice {
        width: 20,
        height: 20,
        border: Border {
            left: 4,
            top: 4,
            right: 4,
            bottom: 4,
        },
        stretch_inner: false,
    };
    // Center native size is 12x12; a 28-wide center dst -> 3 tiles (12,12,4).
    let quads = scaling.geometry(20, 20, 36, 20);
    let center: Vec<_> = quads
        .iter()
        .filter(|q| q.dst[1] == 4 && q.dst[0] >= 4 && q.dst[0] + q.dst[2] <= 32)
        .collect();
    assert!(center.len() >= 3, "center should be tiled, got {center:?}");
    assert_eq!(dst_area(&quads), 36 * 20);
}

#[test]
fn nine_slice_border_is_clamped_at_small_target() {
    // Target smaller than 2*border: borders clamp to half so regions never overlap.
    let scaling = GuiScaling::NineSlice {
        width: 20,
        height: 20,
        border: Border {
            left: 8,
            top: 8,
            right: 8,
            bottom: 8,
        },
        stretch_inner: true,
    };
    let quads = scaling.geometry(20, 20, 10, 10);
    // No quad extends beyond the 10x10 target.
    for q in &quads {
        assert!(q.dst[0] + q.dst[2] <= 10, "quad {q:?} overflows width");
        assert!(q.dst[1] + q.dst[3] <= 10, "quad {q:?} overflows height");
    }
    assert_eq!(dst_area(&quads), 10 * 10);
}
