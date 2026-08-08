//! Sign text placement and colour (issue #23's sign scope) — the render-crate
//! half of the chain whose typed NBT parse lives in
//! [`lodestone_world::sign_text`] and whose GPU quads live in
//! `lodestone-shell`'s `gpu/sign_text.rs`.
//!
//! # Why this is not `block_entity.rs`, despite being a block entity
//!
//! Every type in [`crate::block_entity`] exists because vanilla's block
//! *model* is empty and a `BlockEntityRenderer` supplies the missing cuboid
//! geometry. A sign is the opposite case: `assets/minecraft/blockstates/
//! oak_sign.json` maps every `rotation` value to a real `block/
//! oak_sign_rot_N` model, and `StandingSignRenderer` declares **no model at
//! all** — only text transformations
//! (`.cache/mc/26.2/client-src/net/minecraft/client/renderer/blockentity/
//! StandingSignRenderer.java`). So this module has no [`crate::block_entity::
//! BlockEntityMesh`], no [`crate::block_entity::plan_block_entities`] batch,
//! and does not touch [`crate::entity::bake_entity_parts`] at all — the
//! sign's board is already in the terrain mesh the block model produces, and
//! drawing a second one here would double it. What this module owns instead
//! is purely the **text placement matrix** and its **colour resolution**;
//! the actual glyph quads are built in the shell, which already owns a
//! jar-sourced [`RasterFont`](lodestone_assets::font::RasterFont) loader and
//! an ink-run layout walk (`gpu/nametag.rs`) that this reuses rather than
//! reinventing.
//!
//! **Hanging signs are the same story, not a second one.** In 26.2
//! `HangingSignRenderer` is `AbstractSignRenderer` plus a single
//! `textTransformation` and **no model** either — the board, bar and chains
//! come from `block/template_hanging_sign_rot_N` and
//! `block/template_wall_hanging_sign`, real block models the terrain mesher
//! already draws. So both sign families are text-only here and the whole
//! difference between them is the four numbers in [`SignKind`]'s table plus
//! two text metrics. (This was worth measuring rather than inheriting: 1.20's
//! `HangingSignRenderer` *did* own a rig, and
//! `docs/block-entity-renderers.md` recorded the 1.20 shape as if it were
//! 26.2's.)
//!
//! # The placement matrix, ported term for term
//!
//! `StandingSignRenderer.textTransformation`
//! (`StandingSignRenderer.java:53-65`) builds, for one text side:
//!
//! ```text
//! Matrix4f result = new Matrix4f()
//!     .translate(0.5F, 0.5F, 0.5F)
//!     .rotate(Axis.YP.rotationDegrees(-angle));
//! if (attachmentType == WALL) result.translate(0.0F, -0.3125F, -0.4375F);
//! if (!isFrontText) result.rotate(Axis.YP.rotationDegrees(180.0F));
//! result.translate(TEXT_OFFSET);              // (0, 0.33333334, 0.046666667)
//! result.scale(0.010416667F, -0.010416667F, 0.010416667F);
//! ```
//!
//! JOML's `Matrix4f.translate/.rotate/.scale` right-multiply (`this = this *
//! op`), so reading the calls top to bottom gives the composition order
//! exactly — unlike [`crate::block_entity::block_entity_placement_matrix`],
//! which needs a `translate(pivot) · rot · translate(-pivot)` sandwich
//! because its input vertices are pre-authored in absolute `0..1` block
//! space, this local origin is `(0.5, 0.5, 0.5)` reached by translating
//! *before* rotating — the local input here is small font-pixel offsets
//! around zero (see `submitSignText`'s `x1 = -font.width(line)/2`), so no
//! sandwich is needed. [`sign_text_transform`] ports the whole expression
//! directly, with the block's own world translation folded onto the front
//! (matching every other placement matrix in this crate).
//!
//! The **`-Y` scale is the entire y-flip**: font-pixel space is row-index-
//! down (row `0` at the string's top, same convention
//! `gpu/nametag.rs::layout_ink_runs` already returns), and composing that
//! through a negative `Y` scale turns "down in pixel space" into "up in
//! world space" with no separate flip step, unlike
//! `gpu/nametag.rs::quad_vertices`'s billboard path (which has no matrix to
//! carry the flip and negates `ly` by hand). A local point should be fed to
//! [`sign_text_transform`]'s result **unflipped** — flipping it again here
//! would cancel the scale out.
//!
//! `RotationSegment.convertToDegrees(segment)` is `segment * 22.5`
//! (`SegmentedAnglePrecision(4).toDegrees`, measured against
//! [`crate::block_entity::skull_ground_placement_matrix`]'s identical use of
//! the same formula for a floor skull's `rotation` property — segment `0` is
//! **north**, the same non-[`crate::block_entity::horizontal_facing_yaw`]
//! convention skull's doc already records) — [`SignOrientation::Ground`]
//! carries the raw segment rather than a pre-converted angle so that fact
//! stays in one place.
//!
//! # Colour, ported from `AbstractSignRenderer`/`DyeColor`/`ARGB`
//!
//! Non-glowing text is `ARGB.scaleRGB(dye.getTextColor(), 0.4F)`
//! (`AbstractSignRenderer.getDarkColor`, `.../AbstractSignRenderer.java:97-
//! 100`) — **per-channel integer truncation**, `(channel * 0.4) as i32`
//! clamped `0..255`
//! (`.cache/mc/26.2/client-src/net/minecraft/util/ARGB.java:108-114`), not a
//! float multiply carried through — and per `CLAUDE.md`'s rendering
//! constraints this is a **gamma-space** multiply, the same as tint/shade
//! everywhere else in this codebase. Glowing text uses the dye's own
//! `getTextColor()` unscaled. [`DyeColor.java:30-45`] is the source for
//! every constant in [`dye_text_color_rgb`] — transcribed, not derived.
//!
//! **Deferred**: the black-dye-glowing outline
//! (`BLACK_TEXT_OUTLINE_COLOR = -988212`, a second offset glyph pass drawn
//! behind the main one so glowing black text is not literally invisible) and
//! per-pixel world-light modulation for non-glowing text (`state.lightCoords`
//! in vanilla). The second is a deliberate simplification mirroring
//! `gpu/nametag.rs`'s own documented choice to draw "plain full-bright...
//! unconditionally" rather than sample a lightmap per glyph; both gaps are
//! real and both are narrow (one dye combination; one fidelity loss that
//! reads as "sign text is a little brighter in caves than vanilla").

use glam::{Mat4, Vec3};
use lodestone_world::{SignDyeColor, SignSide};

use crate::entity::ENTITY_FULLBRIGHT;

/// Which of vanilla's **two** sign renderers a block uses. Both are
/// text-only — `StandingSignRenderer` and `HangingSignRenderer` each declare
/// no model whatsoever (verified against
/// `.cache/mc/26.2/client-src/net/minecraft/client/renderer/blockentity/
/// HangingSignRenderer.java`, which is `AbstractSignRenderer` plus one
/// `textTransformation`) — and the hanging board, its bar and its chains are
/// all real block-model geometry
/// (`assets/minecraft/models/block/oak_hanging_sign_rot_0.json` parents
/// `block/template_hanging_sign_rot_0`, which has genuine elements). **The
/// note in `docs/block-entity-renderers.md` calling hanging signs "a
/// different model set again (chains, a bar)" was reasoning from 1.20, where
/// `HangingSignRenderer` really did own a `HangingSignModel`; in 26.2 there
/// is no rig to port and the only difference is four numbers.**
///
/// Those four numbers, and nothing else:
///
/// | | plain | hanging |
/// |---|---|---|
/// | base translate `y` | `0.5` | `0.9375` |
/// | pre-offset | `(0, -0.3125, -0.4375)`, **wall only** | `(0, -0.3125, 0)`, **always** |
/// | `TEXT_OFFSET` | `(0, 0.33333334, 0.046666667)` | `(0, -0.32, 0.073)` |
/// | render scale | `0.010416667` (`0.6666667 / 64`) | `0.0140625` (`0.9 / 64`) |
///
/// plus the two text metrics ([`SignKind::text_line_height`],
/// [`SignKind::max_text_line_width`]), which live on the **block entity** in
/// vanilla (`SignBlockEntity` returns `10`/`90`,
/// `HangingSignBlockEntity` overrides to `9`/`60`) rather than on the
/// renderer. A hanging sign's text is therefore *larger* per glyph and
/// *narrower* per line than a plain sign's, which reads as a mistake and is
/// not one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SignKind {
    /// `oak_sign`/`oak_wall_sign` and every other wood —
    /// `StandingSignRenderer`.
    #[default]
    Plain,
    /// `oak_hanging_sign`/`oak_wall_hanging_sign` and every other wood —
    /// `HangingSignRenderer`.
    Hanging,
}

impl SignKind {
    /// `SignBlockEntity.getTextLineHeight()` (`10`) or
    /// `HangingSignBlockEntity`'s override (`9`), in font pixels.
    #[must_use]
    pub const fn text_line_height(self) -> f32 {
        match self {
            SignKind::Plain => TEXT_LINE_HEIGHT,
            SignKind::Hanging => HANGING_TEXT_LINE_HEIGHT,
        }
    }

    /// `SignBlockEntity.getMaxTextLineWidth()` (`90`) or
    /// `HangingSignBlockEntity`'s override (`60`), in font pixels. Not used
    /// by the placement matrix — this is the width vanilla *splits* a line
    /// at, which this port defers (see the module doc), and the bound a
    /// pixel gate needs for "the widest area this side's text can occupy".
    #[must_use]
    pub const fn max_text_line_width(self) -> f32 {
        match self {
            SignKind::Plain => 90.0,
            SignKind::Hanging => 60.0,
        }
    }

    /// The uniform scale in [`sign_text_transform`]'s last term —
    /// `StandingSignRenderer`'s `0.010416667` or `HangingSignRenderer`'s
    /// `0.0140625`. Both are `RENDER_SCALE / 64`, transcribed as the literal
    /// each renderer actually passes rather than recomputed from the
    /// `RENDER_SCALE`/`TEXT_RENDER_SCALE` field, so a float that does not
    /// round-trip cannot drift.
    #[must_use]
    pub const fn render_scale(self) -> f32 {
        match self {
            SignKind::Plain => 0.010_416_667,
            SignKind::Hanging => 0.014_062_5,
        }
    }
}

/// Vanilla's `PlainSignBlock.Attachment` (`GROUND`/`WALL`) plus the angle
/// that goes with each, folded into one type the way
/// [`crate::block_entity::SkullOrientation`] folds skull placement — a real
/// sign state carries exactly one of `rotation` (ground) or `facing` (wall),
/// never both.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SignOrientation {
    /// A standing sign's `rotation` property, `0..16` — vanilla's
    /// `RotationSegment`. Segment `0` is **north**, not
    /// [`crate::block_entity::horizontal_facing_yaw`]'s south-is-zero
    /// convention (see the module doc).
    Ground {
        /// `0..16`; an out-of-range value still composes a matrix rather
        /// than panicking.
        rotation_segment: u8,
    },
    /// A wall sign's `facing` property, already converted by
    /// [`crate::block_entity::horizontal_facing_yaw`].
    Wall {
        /// `Direction.toYRot()` of the `facing` property.
        facing_yaw_deg: f32,
    },
}

/// The world placement matrix for one text side of a sign —
/// `StandingSignRenderer.textTransformation` for [`SignKind::Plain`] and
/// `HangingSignRenderer.textTransformation` for [`SignKind::Hanging`], see
/// the module doc and [`SignKind`] for the term-by-term port and the four
/// numbers that differ. Feed it a local point in **font-pixel space**
/// (`x` right, `y` down from the block of text's own top, `z = 0`); the
/// result is that world position.
///
/// A hanging sign's `is_wall` is **not** a branch in the matrix: a wall
/// hanging sign differs from a ceiling one only in where its `angle` comes
/// from (`WallHangingSignBlock.FACING.toYRot()` versus
/// `RotationSegment.convertToDegrees(ROTATION)`), which is the caller's
/// [`SignOrientation`] and already resolved by the time this runs.
#[must_use]
pub fn sign_text_transform(
    pos: [i32; 3],
    kind: SignKind,
    orientation: SignOrientation,
    is_front: bool,
) -> Mat4 {
    let (is_wall, angle_deg) = match orientation {
        SignOrientation::Ground { rotation_segment } => {
            (false, f32::from(rotation_segment) * (360.0 / 16.0))
        }
        SignOrientation::Wall { facing_yaw_deg } => (true, facing_yaw_deg),
    };
    let origin = Vec3::new(pos[0] as f32, pos[1] as f32, pos[2] as f32);
    let base_y = match kind {
        SignKind::Plain => 0.5,
        SignKind::Hanging => 0.9375,
    };
    let mut m = Mat4::from_translation(origin)
        * Mat4::from_translation(Vec3::new(0.5, base_y, 0.5))
        * Mat4::from_rotation_y(-angle_deg.to_radians());
    match kind {
        // `if (attachmentType == WALL) result.translate(0, -0.3125, -0.4375)`
        // — pulls a wall sign's text off the board's own face and onto the
        // wall-mounted board's lower, nearer plane.
        SignKind::Plain if is_wall => {
            m *= Mat4::from_translation(Vec3::new(0.0, -0.3125, -0.4375));
        }
        // `HangingSignRenderer` translates `(0, -0.3125, 0)`
        // unconditionally — no attachment branch at all, even though it
        // *computes* `state.attachmentType` for the crumbling overlay.
        SignKind::Hanging => m *= Mat4::from_translation(Vec3::new(0.0, -0.3125, 0.0)),
        SignKind::Plain => {}
    }
    if !is_front {
        m *= Mat4::from_rotation_y(180f32.to_radians());
    }
    let text_offset = match kind {
        SignKind::Plain => Vec3::new(0.0, 0.333_333_34, 0.046_666_667),
        SignKind::Hanging => Vec3::new(0.0, -0.32, 0.073),
    };
    let s = kind.render_scale();
    m * Mat4::from_translation(text_offset) * Mat4::from_scale(Vec3::new(s, -s, s))
}

/// `DyeColor.getTextColor()`, transcribed from
/// `.cache/mc/26.2/client-src/net/minecraft/world/item/DyeColor.java:30-45`
/// (the last constructor argument on each line) — 0xRRGGBB, gamma-space.
#[must_use]
pub const fn dye_text_color_rgb(color: SignDyeColor) -> u32 {
    match color {
        SignDyeColor::White => 16_777_215,
        SignDyeColor::Orange => 16_738_335,
        SignDyeColor::Magenta => 16_711_935,
        SignDyeColor::LightBlue => 10_141_901,
        SignDyeColor::Yellow => 16_776_960,
        SignDyeColor::Lime => 12_582_656,
        SignDyeColor::Pink => 16_738_740,
        SignDyeColor::Gray => 8_421_504,
        SignDyeColor::LightGray => 13_882_323,
        SignDyeColor::Cyan => 65_535,
        SignDyeColor::Purple => 10_494_192,
        SignDyeColor::Blue => 255,
        SignDyeColor::Brown => 9_127_187,
        SignDyeColor::Green => 65_280,
        SignDyeColor::Red => 16_711_680,
        SignDyeColor::Black => 0,
    }
}

/// `ARGB.scaleRGB(rgb, 0.4)` — per-channel `(channel * scale) as i32`,
/// clamped `0..255` (`ARGB.java:108-114`), **not** a float multiply carried
/// through: vanilla truncates in integer space before the result is ever
/// treated as a colour again, so this port truncates too rather than
/// rounding.
#[must_use]
fn scale_rgb(rgb: u32, scale: f32) -> u32 {
    let r = ((rgb >> 16) & 0xFF) as f32;
    let g = ((rgb >> 8) & 0xFF) as f32;
    let b = (rgb & 0xFF) as f32;
    let ch = |c: f32| ((c * scale) as i64).clamp(0, 255) as u32;
    (ch(r) << 16) | (ch(g) << 8) | ch(b)
}

/// One text side's resolved draw colour, RGBA in `0..=1` — full dye colour
/// when [`SignSide::glowing`], `ARGB.scaleRGB(dye, 0.4)` otherwise
/// (`AbstractSignRenderer.getDarkColor`, minus the black-glowing outline
/// substitution — see the module doc's Deferred section). Alpha is always
/// `1.0`; sign text is opaque.
#[must_use]
pub fn sign_side_color(side: &SignSide) -> [f32; 4] {
    let rgb = dye_text_color_rgb(side.color);
    let rgb = if side.glowing { rgb } else { scale_rgb(rgb, 0.4) };
    [
        ((rgb >> 16) & 0xFF) as f32 / 255.0,
        ((rgb >> 8) & 0xFF) as f32 / 255.0,
        (rgb & 0xFF) as f32 / 255.0,
        1.0,
    ]
}

/// A plain sign's text-line height in font pixels
/// (`SignBlockEntity.getTextLineHeight()`, `10`). **This used to be
/// documented as "always `10` — the per-instance method never varies in the
/// real jar", which was wrong**: `HangingSignBlockEntity` overrides it to
/// `9`. Prefer [`SignKind::text_line_height`], which cannot be reached for
/// the wrong kind.
pub const TEXT_LINE_HEIGHT: f32 = 10.0;

/// A hanging sign's text-line height in font pixels
/// (`HangingSignBlockEntity.getTextLineHeight()`, `9`).
pub const HANGING_TEXT_LINE_HEIGHT: f32 = 9.0;

/// The version-free description of one sign to draw this frame. The caller
/// owns every field: block state → `pos`/`orientation`, block-entity NBT →
/// `front`/`back` (via [`lodestone_world::SignText::parse`]), world light →
/// `light`. Same "caller resolves everything, this crate names no protocol
/// version" contract [`crate::block_entity::ChestSpawn`] documents.
#[derive(Debug, Clone, PartialEq)]
pub struct SignSpawn {
    /// Block position (the block's minimum corner).
    pub pos: [i32; 3],
    /// Which vanilla renderer's transform and text metrics apply — see
    /// [`SignKind`]. Independent of [`SignSpawn::orientation`]: all four
    /// combinations are real blocks.
    pub kind: SignKind,
    /// Ground or wall placement.
    pub orientation: SignOrientation,
    /// The side read facing the sign from in front.
    pub front: SignSide,
    /// The side read facing the sign from behind.
    pub back: SignSide,
    /// Packed sky/block light. Not currently sampled per-glyph (see the
    /// module doc's Deferred section) — carried on the spawn anyway so a
    /// future lightmap pass has it without widening this struct again.
    pub light: u8,
}

impl SignSpawn {
    /// A ground-placed, `rotation_segment = 0`, full-bright sign at `pos`
    /// with no text on either side — the minimum a hermetic gate needs
    /// before filling in `front`/`back`.
    #[must_use]
    pub fn at(pos: [i32; 3]) -> Self {
        SignSpawn {
            pos,
            kind: SignKind::Plain,
            orientation: SignOrientation::Ground { rotation_segment: 0 },
            front: SignSide::default(),
            back: SignSide::default(),
            light: ENTITY_FULLBRIGHT,
        }
    }

    /// [`SignSpawn::at`]'s hanging counterpart — a ceiling hanging sign at
    /// `pos`, `rotation_segment = 0`, no text.
    #[must_use]
    pub fn hanging_at(pos: [i32; 3]) -> Self {
        SignSpawn {
            kind: SignKind::Hanging,
            ..SignSpawn::at(pos)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-computed from `textTransformation`'s own expression (angle `0`,
    /// so the rotation is identity and every step is a plain translate):
    /// `TEXT_OFFSET (0, 0.33333334, 0.046666667)` then the wall offset
    /// `(0, -0.3125, -0.4375)` then the block-centring `(0.5, 0.5, 0.5)` —
    /// `y = 0.5 + 0.33333334 - 0.3125 = 0.52083334`,
    /// `z = 0.5 + 0.046666667 - 0.4375 = 0.109166667`. Pinned to that exact
    /// value (not a loose sanity range) so a swapped operand or a dropped
    /// wall-offset term fails here rather than only looking "plausible".
    #[test]
    fn wall_transform_origin_matches_the_hand_computed_expression() {
        let m = sign_text_transform(
            [0, 0, 0],
            SignKind::Plain,
            SignOrientation::Wall { facing_yaw_deg: 0.0 },
            true,
        );
        let origin = m.transform_point3(Vec3::ZERO);
        assert!((origin.x - 0.5).abs() < 1e-4, "x {}", origin.x);
        assert!((origin.y - 0.520_833_34).abs() < 1e-4, "y {}", origin.y);
        assert!((origin.z - 0.109_166_67).abs() < 1e-4, "z {}", origin.z);
    }

    /// A ground sign has none of the wall offset, so its origin sits at the
    /// block's own vertical/horizontal centre (plus the small `TEXT_OFFSET`
    /// nudge) rather than pulled toward one face.
    #[test]
    fn ground_transform_has_no_wall_offset() {
        let m = sign_text_transform(
            [0, 0, 0],
            SignKind::Plain,
            SignOrientation::Ground { rotation_segment: 0 },
            true,
        );
        let origin = m.transform_point3(Vec3::ZERO);
        assert!((origin.x - 0.5).abs() < 1e-4, "x {}", origin.x);
        assert!((origin.y - (0.5 + 0.333_333_34)).abs() < 1e-3, "y {}", origin.y);
    }

    /// The back side is the front side rotated 180° about the sign's own
    /// vertical axis, but the rotation happens **before** `TEXT_OFFSET` is
    /// applied (`if (!isFrontText) result.rotate(...)` precedes
    /// `result.translate(TEXT_OFFSET)` in `textTransformation`), so the
    /// offset's own `z` component flips sign along with everything drawn
    /// through the matrix. That is not a bug to normalise away: it is what
    /// puts the front and back text on the two opposite faces of the
    /// board's thin depth rather than both writing to the same plane.
    /// Hand-computed: front origin `z = 0.5 + 0.046666667 = 0.546666667`,
    /// back origin `z = 0.5 - 0.046666667 = 0.453333333`.
    #[test]
    fn back_text_sits_behind_front_text_on_the_boards_two_faces() {
        let orientation = SignOrientation::Ground { rotation_segment: 0 };
        let front = sign_text_transform([0, 0, 0], SignKind::Plain, orientation, true);
        let back = sign_text_transform([0, 0, 0], SignKind::Plain, orientation, false);
        let front_origin = front.transform_point3(Vec3::ZERO);
        let back_origin = back.transform_point3(Vec3::ZERO);
        assert!((front_origin.x - 0.5).abs() < 1e-4);
        assert!((front_origin.z - 0.546_666_67).abs() < 1e-4, "{front_origin:?}");
        assert!((back_origin.z - 0.453_333_33).abs() < 1e-4, "{back_origin:?}");
        // A point offset on local +X ends up on opposite sides of the block's
        // centre X once placed in the world (the 180° turn), confirming the
        // rotation itself is real and not just the offset's sign.
        let probe = Vec3::new(10.0, 0.0, 0.0);
        let front_delta = front.transform_point3(probe) - front_origin;
        let back_delta = back.transform_point3(probe) - back_origin;
        assert!(
            front_delta.x * back_delta.x < 0.0,
            "front {front_delta:?} back {back_delta:?} should point opposite ways on x"
        );
    }

    /// `RotationSegment.convertToDegrees` is `segment * 22.5`, the identical
    /// formula `skull_ground_placement_matrix` already uses for the same
    /// property — this pins that the two have not drifted apart, since
    /// nothing in the type system would catch it if they had.
    #[test]
    fn ground_rotation_segments_span_a_full_turn_in_sixteen_steps() {
        let at = |segment: u8| {
            sign_text_transform(
                [0, 0, 0],
                SignKind::Plain,
                SignOrientation::Ground { rotation_segment: segment },
                true,
            )
        };
        // Segment 0 and segment 8 (half the circle) must place a probe point
        // on opposite sides of the origin.
        let probe = Vec3::new(10.0, 0.0, 0.0);
        let d0 = at(0).transform_point3(probe) - at(0).transform_point3(Vec3::ZERO);
        let d8 = at(8).transform_point3(probe) - at(8).transform_point3(Vec3::ZERO);
        assert!(d0.dot(d8) < 0.0, "segment 0 vs 8: {d0:?} / {d8:?}");
    }

    /// `getDarkColor`'s truncation, not rounding: white (255,255,255) scaled
    /// by 0.4 must be 102 per channel (`(255*0.4) as i32 == 102`), not 102.5
    /// rounded to 103.
    #[test]
    fn dark_scaling_truncates_like_the_real_jar() {
        assert_eq!(scale_rgb(0xFFFFFF, 0.4), 0x666666);
        assert_eq!(0x66, 102);
    }

    /// Glowing draws the dye's own full colour; non-glowing draws the
    /// darkened one. Measured against the real `DyeColor.RED` constant
    /// (`16_711_680 = 0xFF0000`) rather than restated.
    #[test]
    fn glowing_uses_full_colour_and_non_glowing_uses_the_dark_one() {
        let side = SignSide {
            lines: Default::default(),
            glowing: false,
            color: SignDyeColor::Red,
        };
        let dark = sign_side_color(&side);
        assert!((dark[0] - 0x66 as f32 / 255.0).abs() < 1e-4, "{dark:?}");
        assert_eq!(dark[1], 0.0);
        assert_eq!(dark[2], 0.0);

        let glow = SignSide { glowing: true, ..side };
        let bright = sign_side_color(&glow);
        assert!((bright[0] - 1.0).abs() < 1e-4, "{bright:?}");
    }

    /// Hand-computed from `HangingSignRenderer.textTransformation`'s own
    /// expression at angle `0`, where every step is a plain translate:
    /// `y = 0.9375 - 0.3125 - 0.32 = 0.305`, `z = 0.5 + 0.073 = 0.573`.
    /// Pinned exactly, and **the plain hypothesis is computed alongside and
    /// required to disagree** — a hanging sign fed through the plain branch
    /// would land at `y = 0.83333`, so an accidentally-shared branch fails
    /// here rather than looking plausible.
    #[test]
    fn hanging_transform_origin_matches_the_hand_computed_expression() {
        let orientation = SignOrientation::Ground { rotation_segment: 0 };
        let hanging = sign_text_transform([0, 0, 0], SignKind::Hanging, orientation, true)
            .transform_point3(Vec3::ZERO);
        assert!((hanging.x - 0.5).abs() < 1e-4, "x {}", hanging.x);
        assert!((hanging.y - 0.305).abs() < 1e-4, "y {}", hanging.y);
        assert!((hanging.z - 0.573).abs() < 1e-4, "z {}", hanging.z);

        let plain = sign_text_transform([0, 0, 0], SignKind::Plain, orientation, true)
            .transform_point3(Vec3::ZERO);
        assert!(
            (plain.y - hanging.y).abs() > 0.5,
            "the plain and hanging text planes must be far apart vertically, \
             got plain {plain:?} hanging {hanging:?}"
        );
    }

    /// The back side of a hanging sign sits on the board's opposite face,
    /// the same `rotate-before-TEXT_OFFSET` consequence the plain sign has —
    /// hand-computed `z = 0.5 - 0.073 = 0.427` against the front's `0.573`.
    #[test]
    fn hanging_back_text_sits_on_the_other_face() {
        let orientation = SignOrientation::Ground { rotation_segment: 0 };
        let back = sign_text_transform([0, 0, 0], SignKind::Hanging, orientation, false)
            .transform_point3(Vec3::ZERO);
        assert!((back.z - 0.427).abs() < 1e-4, "{back:?}");
        // Same height as the front: the 180° turn is about Y, so it cannot
        // move the text up or down.
        assert!((back.y - 0.305).abs() < 1e-4, "{back:?}");
    }

    /// The render scales are the *magnitude* assertion, not a sign check: a
    /// 64-font-pixel run spans `0.9` blocks on a hanging sign and
    /// `0.6666667` on a plain one (`RENDER_SCALE * 64 / 64`, i.e. the
    /// `RENDER_SCALE` field itself). Both hypotheses are computed and the
    /// measurement must land on the right one — swapping the two constants
    /// still produces a plausible-looking sign and fails here.
    #[test]
    fn the_two_render_scales_span_their_own_vanilla_render_scale() {
        let orientation = SignOrientation::Ground { rotation_segment: 0 };
        let span = |kind: SignKind| {
            let m = sign_text_transform([0, 0, 0], kind, orientation, true);
            (m.transform_point3(Vec3::new(64.0, 0.0, 0.0)) - m.transform_point3(Vec3::ZERO)).length()
        };
        assert!((span(SignKind::Hanging) - 0.9).abs() < 1e-3, "{}", span(SignKind::Hanging));
        assert!(
            (span(SignKind::Plain) - 0.666_666_7).abs() < 1e-3,
            "{}",
            span(SignKind::Plain)
        );
    }

    /// The two text metrics really are per-kind, and in *opposite*
    /// directions from the scale — a hanging sign draws bigger glyphs over a
    /// narrower line. Transcribed from `HangingSignBlockEntity`'s two
    /// overrides.
    #[test]
    fn hanging_text_metrics_override_the_plain_ones() {
        assert_eq!(SignKind::Plain.text_line_height(), 10.0);
        assert_eq!(SignKind::Hanging.text_line_height(), 9.0);
        assert_eq!(SignKind::Plain.max_text_line_width(), 90.0);
        assert_eq!(SignKind::Hanging.max_text_line_width(), 60.0);
        assert!(SignKind::Hanging.render_scale() > SignKind::Plain.render_scale());
    }

    /// A wall hanging sign takes its angle from `facing` and a ceiling one
    /// from `rotation`, but neither adds the plain sign's wall offset — the
    /// hanging branch is unconditional, so the two differ **only** by the
    /// rotation. Ceiling segment `4` is `90°`, the same angle
    /// `Direction.toYRot()` gives `WEST`.
    #[test]
    fn a_wall_hanging_sign_differs_from_a_ceiling_one_only_by_its_angle() {
        let ceiling = sign_text_transform(
            [0, 0, 0],
            SignKind::Hanging,
            SignOrientation::Ground { rotation_segment: 4 },
            true,
        );
        let wall = sign_text_transform(
            [0, 0, 0],
            SignKind::Hanging,
            SignOrientation::Wall { facing_yaw_deg: 90.0 },
            true,
        );
        let probe = Vec3::new(10.0, 3.0, 0.0);
        let a = ceiling.transform_point3(probe);
        let b = wall.transform_point3(probe);
        assert!((a - b).length() < 1e-5, "ceiling {a:?} vs wall {b:?}");
        // The control: a *plain* wall sign at the same angle does not agree
        // with either, because it carries the `-0.4375` face offset.
        let plain_wall = sign_text_transform(
            [0, 0, 0],
            SignKind::Plain,
            SignOrientation::Wall { facing_yaw_deg: 90.0 },
            true,
        )
        .transform_point3(probe);
        assert!((plain_wall - b).length() > 0.2, "plain {plain_wall:?} vs hanging {b:?}");
    }

    #[test]
    fn black_dye_resolves_to_zero_in_both_modes() {
        assert_eq!(dye_text_color_rgb(SignDyeColor::Black), 0);
        let side = SignSide {
            lines: Default::default(),
            glowing: false,
            color: SignDyeColor::Black,
        };
        assert_eq!(sign_side_color(&side), [0.0, 0.0, 0.0, 1.0]);
    }
}
