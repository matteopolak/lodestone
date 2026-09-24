//! Sign text placement and colour — the render-crate
//! half of the chain whose typed NBT parse lives in
//! [`lodestone_world::sign_text`] and whose GPU quads live in
//! `lodestone-shell`'s `gpu/sign_text.rs`.
//!
//! # Why this is not `block_entity.rs`, despite being a block entity
//!
//! Every type in [`crate::block_entity`] exists because vanilla's block
//! *model* is empty and a dedicated block-entity renderer type supplies the
//! missing cuboid geometry. A sign is the opposite case: `assets/minecraft/blockstates/
//! oak_sign.json` maps every `rotation` value to a real `block/
//! oak_sign_rot_N` model, and vanilla's standing-sign renderer declares **no model at
//! all** — only text transformations
//! (26.2's decompiled behaviour). So this module has no [`crate::block_entity::
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
//! vanilla's hanging-sign renderer is its base sign-renderer class plus a single
//! text-transformation function override and **no model** either — the board, bar and chains
//! come from `block/template_hanging_sign_rot_N` and
//! `block/template_wall_hanging_sign`, real block models the terrain mesher
//! already draws. So both sign families are text-only here and the whole
//! difference between them is the four numbers in [`SignKind`]'s table plus
//! two text metrics. (This was worth measuring rather than inheriting: 1.20's
//! hanging-sign renderer *did* own a rig, and
//! `docs/block-entity-renderers.md` recorded the 1.20 shape as if it were
//! 26.2's.)
//!
//! # The placement matrix, ported term for term
//!
//! Vanilla's standing-sign renderer's text-transformation function
//! builds, for one text side, an identity matrix composed left-to-right by
//! five steps applied in this exact order: translate by `(0.5, 0.5, 0.5)`;
//! rotate about the world `+Y` axis by the negated placement angle; **only
//! when wall-mounted**, translate by `(0, -0.3125, -0.4375)`; **only for the
//! back-facing side**, rotate about `+Y` by another 180°; unconditionally
//! translate by the text-offset term `(0, 0.33333334, 0.046666667)`; and
//! finally scale by `(0.010416667, -0.010416667, 0.010416667)`.
//!
//! JOML's `Matrix4f.translate/.rotate/.scale` right-multiply (`this = this *
//! op`), so reading the calls top to bottom gives the composition order
//! exactly — unlike [`crate::block_entity::block_entity_placement_matrix`],
//! which needs a `translate(pivot) · rot · translate(-pivot)` sandwich
//! because its input vertices are pre-authored in absolute `0..1` block
//! space, this local origin is `(0.5, 0.5, 0.5)` reached by translating
//! *before* rotating — the local input here is small font-pixel offsets
//! around zero (see vanilla's text-submit function's own halved-negated
//! line-width computation for the starting `x`), so no
//! sandwich is needed. [`sign_text_transform`] ports the whole expression
//! directly, with the block's own world translation folded onto the front
//! (matching every other placement matrix in this crate).
//!
//! The **`-Y` scale is the entire y-flip**: font-pixel space is row-index-
//! down (row `0` at the string's top, same convention
//! `gpu/nametag.rs::layout_styled_ink_runs` already returns), and composing that
//! through a negative `Y` scale turns "down in pixel space" into "up in
//! world space" with no separate flip step, unlike
//! `gpu/nametag.rs::quad_vertices`'s billboard path (which has no matrix to
//! carry the flip and negates `ly` by hand). A local point should be fed to
//! [`sign_text_transform`]'s result **unflipped** — flipping it again here
//! would cancel the scale out.
//!
//! Vanilla's own segment-to-degrees conversion is `segment * 22.5`
//! (a 4-bit segmented-angle precision converted to degrees, measured against
//! [`crate::block_entity::skull_ground_placement_matrix`]'s identical use of
//! the same formula for a floor skull's `rotation` property — segment `0` is
//! **north**, the same non-[`crate::block_entity::horizontal_facing_yaw`]
//! convention skull's doc already records) — [`SignOrientation::Ground`]
//! carries the raw segment rather than a pre-converted angle so that fact
//! stays in one place.
//!
//! # Colour, ported from vanilla's base sign-renderer/dye-colour/packed-colour
//! helpers
//!
//! Non-glowing text is the dye's own text colour, run through vanilla's own
//! packed-colour scale helper at `0.4F`
//! (vanilla's base sign-renderer's dark-colour function) — **per-channel
//! integer truncation**, `(channel * 0.4) as i32`
//! clamped `0..255`
//! , not a
//! float multiply carried through — and per `CLAUDE.md`'s rendering
//! constraints this is a **gamma-space** multiply, the same as tint/shade
//! everywhere else in this codebase. Glowing text uses the dye's own
//! text colour, unscaled. Vanilla's dye-colour registration is the source for
//! every constant in [`dye_text_color_rgb`] — transcribed, not derived.
//!
//! # Glowing text: the outline is the whole visual difference
//!
//! Vanilla's base sign-renderer's text-submit function branches once on
//! whether the sign text has the glowing flag set, and the branch decides
//! three things at once:
//!
//! | | plain | glowing |
//! |---|---|---|
//! | glyph colour | dark-colour function | dye's own text colour, unscaled |
//! | outline colour | none (`0`) | dark-colour function |
//! | light | the block's own light coordinates | `15728880` — full bright |
//!
//! and the outline is gated once more: it draws when the resolved text
//! colour equals black's own text colour, **or** when a separate
//! outline-visible flag on the render state is set — that flag being the
//! camera within a squared distance of 16 blocks
//! of the block centre, or a scoping first-person player. So a **black** glowing sign is
//! outlined at any range (its glyphs are literally colour `0` and the outline
//! is the only thing that makes them legible), and every other glowing sign
//! is outlined within 16 blocks. [`sign_outline_color`] is that whole gate.
//!
//! [`sign_dark_color_rgb`] is that dark-colour function itself, including the
//! substitution this module used to defer: black colour plus the glowing flag
//! yields a named outline-colour constant, `-988212` (`0xFFF0EBCC`, a bone white)
//! rather than the packed-colour scale helper applied to `0` at `0.4`, which is
//! `0`. That substitution is unreachable
//! from [`sign_side_color`]'s non-glowing arm, where the glowing flag is
//! false by construction, so the two functions agree everywhere they overlap.
//!
//! **All three arms are now ported**, the light one last. It was reported as
//! needing no port, on the true observation that the sign-text pass sampled
//! no lightmap at all and so drew every sign full-bright — which made the
//! glow branch's *other* two arms the only visible difference and left the
//! feature not load-bearing in the one situation it exists for: a glowing
//! and a plain sign looked equally bright in the dark. `gpu/sign_text.rs`
//! now folds vanilla's lightmap texel into each side's vertex colours, taking
//! `0xFF` (sky 15, block 15) for a glowing side and this spawn's own
//! [`SignSpawn::light`] for a plain one. That byte was already being
//! resolved — `block_entities::sign_spawns` fills it from
//! `net::entity_light_at` — so the gap was one consumer, not a missing
//! source.
//!
//! Note the full-bright constant is **not** [`ENTITY_FULLBRIGHT`]: that is
//! sky 15 with block 0, and the sky half is scaled by the clock, so a sign
//! carrying it would fade at dusk. Vanilla's `15728880` sets both halves.

use glam::{Mat4, Vec3};
use lodestone_world::{SignDyeColor, SignSide};

use crate::entity::ENTITY_FULLBRIGHT;

/// Which of vanilla's **two** sign renderers a block uses. Both are
/// text-only — the standing-sign and hanging-sign renderers each declare
/// no model whatsoever (verified against
/// 26.2's decompiled hanging-sign-renderer source, which is the base
/// sign-renderer class plus one
/// text-transformation function) — and the hanging board, its bar and its chains are
/// all real block-model geometry
/// (`assets/minecraft/models/block/oak_hanging_sign_rot_0.json` parents
/// `block/template_hanging_sign_rot_0`, which has genuine elements). **The
/// note in `docs/block-entity-renderers.md` calling hanging signs "a
/// different model set again (chains, a bar)" was reasoning from 1.20, where
/// the hanging-sign renderer really did own a rig; in 26.2 there
/// is no rig to port and the only difference is four numbers.**
///
/// Those four numbers, and nothing else:
///
/// | | plain | hanging |
/// |---|---|---|
/// | base translate `y` | `0.5` | `0.9375` |
/// | pre-offset | `(0, -0.3125, -0.4375)`, **wall only** | `(0, -0.3125, 0)`, **always** |
/// | text-offset term | `(0, 0.33333334, 0.046666667)` | `(0, -0.32, 0.073)` |
/// | render scale | `0.010416667` (`0.6666667 / 64`) | `0.0140625` (`0.9 / 64`) |
///
/// plus the two text metrics ([`SignKind::text_line_height`],
/// [`SignKind::max_text_line_width`]), which live on the **block entity** in
/// vanilla (the plain sign's block entity returns `10`/`90`,
/// the hanging sign's block entity overrides to `9`/`60`) rather than on the
/// renderer. A hanging sign's text is therefore *larger* per glyph and
/// *narrower* per line than a plain sign's, which reads as a mistake and is
/// not one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SignKind {
    /// `oak_sign`/`oak_wall_sign` and every other wood —
    /// vanilla's standing-sign renderer.
    #[default]
    Plain,
    /// `oak_hanging_sign`/`oak_wall_hanging_sign` and every other wood —
    /// vanilla's hanging-sign renderer.
    Hanging,
}

impl SignKind {
    /// Vanilla's own text-line-height accessor: the plain sign's block
    /// entity returns `10`, the hanging sign's block entity overrides to
    /// `9`, in font pixels.
    #[must_use]
    pub const fn text_line_height(self) -> f32 {
        match self {
            SignKind::Plain => TEXT_LINE_HEIGHT,
            SignKind::Hanging => HANGING_TEXT_LINE_HEIGHT,
        }
    }

    /// Vanilla's own max-text-line-width accessor: the plain sign's block
    /// entity returns `90`, the hanging sign's block entity overrides to
    /// `60`, in font pixels. Not used
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
    /// the standing-sign renderer's `0.010416667` or the hanging-sign
    /// renderer's
    /// `0.0140625`. Both are a shared render-scale constant divided by `64`, transcribed as the literal
    /// each renderer actually passes rather than recomputed from that
    /// constant's own field, so a float that does not
    /// round-trip cannot drift.
    #[must_use]
    pub const fn render_scale(self) -> f32 {
        match self {
            SignKind::Plain => 0.010_416_667,
            SignKind::Hanging => 0.014_062_5,
        }
    }
}

/// Vanilla's own two-valued attachment property for a standing sign
/// (ground/wall) plus the angle
/// that goes with each, folded into one type the way
/// [`crate::block_entity::SkullOrientation`] folds skull placement — a real
/// sign state carries exactly one of `rotation` (ground) or `facing` (wall),
/// never both.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SignOrientation {
    /// A standing sign's `rotation` property, `0..16` — vanilla's own
    /// segmented-angle type for it. Segment `0` is **north**, not
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
        /// Vanilla's own direction-to-yaw conversion of the `facing` property.
        facing_yaw_deg: f32,
    },
}

/// The world placement matrix for one text side of a sign —
/// vanilla's standing-sign renderer's text-transformation function for [`SignKind::Plain`] and
/// its hanging-sign renderer's text-transformation function for [`SignKind::Hanging`], see
/// the module doc and [`SignKind`] for the term-by-term port and the four
/// numbers that differ. Feed it a local point in **font-pixel space**
/// (`x` right, `y` down from the block of text's own top, `z = 0`); the
/// result is that world position.
///
/// A hanging sign's `is_wall` is **not** a branch in the matrix: a wall
/// hanging sign differs from a ceiling one only in where its `angle` comes
/// from (a wall hanging sign's own `facing` property converted to yaw versus
/// a ceiling hanging sign's own segmented rotation converted to degrees), which is the caller's
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
        // Vanilla's own standing-sign renderer only applies this translate
        // when the attachment is wall-mounted, and it
        // pulls a wall sign's text off the board's own face and onto the
        // wall-mounted board's lower, nearer plane.
        SignKind::Plain if is_wall => {
            m *= Mat4::from_translation(Vec3::new(0.0, -0.3125, -0.4375));
        }
        // Vanilla's own hanging-sign renderer translates `(0, -0.3125, 0)`
        // unconditionally — no attachment branch at all, even though it
        // *does* resolve the attachment separately for the crumbling overlay.
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

/// Vanilla's own per-dye text-colour accessor, transcribed from its dye-colour
/// registration table (26.2's decompiled source)
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

/// Vanilla's own packed-colour scale helper, applied at `0.4` — per-channel `(channel * scale) as i32`,
/// clamped `0..255`, **not** a float multiply carried
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

/// One text side's resolved **glyph** draw colour, RGBA in `0..=1` — full dye
/// colour when [`SignSide::glowing`], [`sign_dark_color_rgb`] otherwise.
/// Alpha is always `1.0`; sign text is opaque.
///
/// The glowing arm deliberately does **not** take
/// [`BLACK_TEXT_OUTLINE_RGB`]: vanilla substitutes that into the *outline*
/// colour, never into the glyphs, so a glowing black sign really does draw
/// colour-`0` glyphs and relies on [`sign_outline_color`] to make them
/// legible.
#[must_use]
pub fn sign_side_color(side: &SignSide) -> [f32; 4] {
    let rgb = if side.glowing {
        dye_text_color_rgb(side.color)
    } else {
        // Vanilla's own dark-colour function with the glowing flag false,
        // which can only take
        // the scale arm — routed through the shared function anyway so
        // the two cannot drift apart.
        sign_dark_color_rgb(side)
    };
    [
        ((rgb >> 16) & 0xFF) as f32 / 255.0,
        ((rgb >> 8) & 0xFF) as f32 / 255.0,
        (rgb & 0xFF) as f32 / 255.0,
        1.0,
    ]
}

/// Vanilla's own named outline-colour constant, as 0xRRGGBB — vanilla
/// spells it `-988212`, an ARGB `int` whose alpha byte is `0xFF`, so the
/// colour itself is `0xF0EBCC`. Substituted for the scale-helper result of `0`
/// scaled by `0.4`, which is `0`, when
/// a **black** side has glowing text, which is the one case where the plain
/// formula would make the outline the same colour as the glyphs it exists to
/// separate.
pub const BLACK_TEXT_OUTLINE_RGB: u32 = 0x00F0_EBCC;

/// A squared-16 constant — vanilla's base sign-renderer's outline-render-distance
/// constant, the
/// squared block distance from the camera to the sign's block *centre* within
/// which a glowing (non-black) side draws its outline.
pub const OUTLINE_RENDER_DISTANCE_SQUARED: f32 = 256.0;

/// Vanilla's base sign-renderer's dark-colour function, whole — the dye's text colour scaled
/// by `0.4`, **except** for a black side with glowing text, which yields
/// [`BLACK_TEXT_OUTLINE_RGB`] instead. Packed 0xRRGGBB.
///
/// This is both the glyph colour of a *non*-glowing side and the outline
/// colour of a glowing one; vanilla computes it once, before the branch, and
/// uses it for both. Keeping that single derivation is why [`sign_side_color`]
/// calls this rather than restating the scale.
#[must_use]
pub fn sign_dark_color_rgb(side: &SignSide) -> u32 {
    let rgb = dye_text_color_rgb(side.color);
    if rgb == dye_text_color_rgb(SignDyeColor::Black) && side.glowing {
        BLACK_TEXT_OUTLINE_RGB
    } else {
        scale_rgb(rgb, 0.4)
    }
}

/// The outline colour for one text side, RGBA in `0..=1`, or `None` when this
/// side draws no outline — vanilla's own text-submit function's "draw the
/// dark colour when the outline flag is set, otherwise nothing"
/// with its own gate folded in.
///
/// `distance_squared` is from the camera to the sign block's **centre**
/// (vanilla's own "box centre" accessor), matching vanilla's own
/// outline-visible check. The scoping-spyglass half
/// of that check has no equivalent here and is not modelled; it only ever
/// *adds* an outline at long range.
///
/// A non-glowing side is `None` unconditionally — the outline exists solely to
/// separate full-brightness glyphs from a bright board, and vanilla's plain
/// arm hardcodes the outline flag to false.
#[must_use]
pub fn sign_outline_color(side: &SignSide, distance_squared: f32) -> Option<[f32; 4]> {
    if !side.glowing {
        return None;
    }
    let text_rgb = dye_text_color_rgb(side.color);
    let black = text_rgb == dye_text_color_rgb(SignDyeColor::Black);
    if !black && distance_squared >= OUTLINE_RENDER_DISTANCE_SQUARED {
        return None;
    }
    let rgb = sign_dark_color_rgb(side);
    Some([
        ((rgb >> 16) & 0xFF) as f32 / 255.0,
        ((rgb >> 8) & 0xFF) as f32 / 255.0,
        (rgb & 0xFF) as f32 / 255.0,
        1.0,
    ])
}

/// A plain sign's text-line height in font pixels
/// (vanilla's own text-line-height accessor, `10`). **This used to be
/// documented as "always `10` — the per-instance method never varies in the
/// real jar", which was wrong**: the hanging sign's block entity overrides it to
/// `9`. Prefer [`SignKind::text_line_height`], which cannot be reached for
/// the wrong kind.
pub const TEXT_LINE_HEIGHT: f32 = 10.0;

/// A hanging sign's text-line height in font pixels
/// (the hanging sign's block entity's own text-line-height override, `9`).
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
    /// Packed sky/block light (`sky << 4 | block`) at the sign's own block,
    /// vanilla's own light-coordinates field on the render state.
    ///
    /// `gpu/sign_text.rs` multiplies the lightmap texel for this byte into
    /// every glyph and outline vertex of a **non-glowing** side; a glowing
    /// side substitutes full bright and ignores it, per
    /// vanilla's base sign-renderer's text-submit function. Filled by
    /// `block_entities::sign_spawns` from `net::entity_light_at`.
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

}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-computed from vanilla's own text-transformation function's expression (angle `0`,
    /// so the rotation is identity and every step is a plain translate):
    /// the text-offset term `(0, 0.33333334, 0.046666667)` then the wall offset
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
    /// block's own vertical/horizontal centre (plus the small text-offset
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
    /// vertical axis, but the rotation happens **before** the text-offset term is
    /// applied (vanilla's own text-transformation function rotates for the
    /// back side, then translates by the text-offset term, in that order), so the
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

    /// Vanilla's own segment-to-degrees conversion is `segment * 22.5`, the identical
    /// formula [`crate::block_entity::skull_ground_placement_matrix`] already uses for the same
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

    /// Vanilla's own dark-colour function truncates, and does not round: white (255,255,255) scaled
    /// by 0.4 must be 102 per channel (`(255*0.4) as i32 == 102`), not 102.5
    /// rounded to 103.
    #[test]
    fn dark_scaling_truncates_like_the_real_jar() {
        assert_eq!(scale_rgb(0xFFFFFF, 0.4), 0x666666);
        assert_eq!(0x66, 102);
    }

    /// Glowing draws the dye's own full colour; non-glowing draws the
    /// darkened one. Measured against vanilla's own red dye-colour constant
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

    /// Hand-computed from vanilla's hanging-sign renderer's text-transformation function's own
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
    /// narrower line. Transcribed from the hanging sign's block entity's two
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
    /// vanilla's own direction-to-yaw conversion gives for due west.
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

    /// Vanilla's own dark-colour function's one substitution, and the discriminating input is
    /// **black plus glowing** — every other combination takes the scale
    /// arm, so a gate on red or on non-glowing black cannot tell the two
    /// hypotheses apart. Both are computed here and required to differ:
    /// the scale helper applied to `0` at `0.4` is `0`, the substitution is `0xF0EBCC`.
    #[test]
    fn the_dark_colour_substitutes_bone_white_only_for_black_glowing_text() {
        let side = |color, glowing| SignSide {
            lines: Default::default(),
            glowing,
            color,
        };
        assert_eq!(
            sign_dark_color_rgb(&side(SignDyeColor::Black, true)),
            BLACK_TEXT_OUTLINE_RGB
        );
        assert_eq!(BLACK_TEXT_OUTLINE_RGB, 0x00F0_EBCC);
        // The wrong hypothesis, computed rather than restated.
        assert_eq!(scale_rgb(dye_text_color_rgb(SignDyeColor::Black), 0.4), 0);
        assert_eq!(sign_dark_color_rgb(&side(SignDyeColor::Black, false)), 0);
        // A non-black glowing side is untouched by the substitution.
        assert_eq!(sign_dark_color_rgb(&side(SignDyeColor::Red, true)), 0x66_0000);
        assert_eq!(sign_dark_color_rgb(&side(SignDyeColor::Red, false)), 0x66_0000);
    }

    /// The outline gate, all three of its arms. Vanilla's own squared-16
    /// distance constant is `256`, so a
    /// sign at 15 blocks (225) is inside and one at 17 (289) is outside —
    /// inputs chosen either side of the boundary rather than at it, since the
    /// comparison is strict and a fixture *on* 256 cannot separate `<` from
    /// `<=`.
    #[test]
    fn only_glowing_text_is_outlined_and_only_black_is_outlined_at_any_range() {
        let side = |color, glowing| SignSide {
            lines: Default::default(),
            glowing,
            color,
        };
        let near = 15.0f32 * 15.0;
        let far = 17.0f32 * 17.0;
        assert!(near < OUTLINE_RENDER_DISTANCE_SQUARED);
        assert!(far > OUTLINE_RENDER_DISTANCE_SQUARED);

        // Not glowing: no outline at any range at all.
        assert_eq!(sign_outline_color(&side(SignDyeColor::Lime, false), near), None);
        assert_eq!(sign_outline_color(&side(SignDyeColor::Black, false), near), None);

        // Glowing and not black: outlined near, bare far.
        let lime = sign_outline_color(&side(SignDyeColor::Lime, true), near)
            .expect("a glowing sign within 16 blocks is outlined");
        // The scale helper applied to `0xBFFF00` at `0.4` is `(76, 102, 0)` — the dark colour, not
        // the glyph colour, and computed here from the constant rather than
        // read back off the function under test.
        assert!((lime[0] - 76.0 / 255.0).abs() < 1e-4, "{lime:?}");
        assert!((lime[1] - 102.0 / 255.0).abs() < 1e-4, "{lime:?}");
        assert_eq!(lime[2], 0.0);
        assert_eq!(sign_outline_color(&side(SignDyeColor::Lime, true), far), None);

        // Glowing and black: outlined at *any* range, in bone white.
        let black_far = sign_outline_color(&side(SignDyeColor::Black, true), far)
            .expect("black glowing text is outlined regardless of distance");
        assert!((black_far[0] - 240.0 / 255.0).abs() < 1e-4, "{black_far:?}");
        assert!((black_far[1] - 235.0 / 255.0).abs() < 1e-4, "{black_far:?}");
        assert!((black_far[2] - 204.0 / 255.0).abs() < 1e-4, "{black_far:?}");
    }

    /// Lime's *text* colour is a yellow-green and that is not a bug: vanilla's
    /// own lime dye-colour registration carries a diffuse block colour of `8439583` (a green) and
    /// a text colour of `12582656`, which is `0xBFFF00` — 191 red against 255
    /// green. A glowing lime sign therefore reads yellowish next to the dyed
    /// *block*, in vanilla exactly as here, and "the dye is not reaching the
    /// glyph" is the wrong diagnosis for it. Pinned against both the packed
    /// constant and its channels so a future transcription slip fails here.
    #[test]
    fn limes_text_colour_is_a_yellow_green_in_the_jar_not_a_dropped_dye() {
        let rgb = dye_text_color_rgb(SignDyeColor::Lime);
        assert_eq!(rgb, 12_582_656);
        assert_eq!(rgb, 0x00BF_FF00);
        assert_eq!((rgb >> 16) & 0xFF, 191);
        assert_eq!((rgb >> 8) & 0xFF, 255);
        assert_eq!(rgb & 0xFF, 0);
        // And the glowing glyph colour is that value unscaled, so a glowing
        // lime sign draws the full `0xBFFF00` rather than the dark `0x4C6600`.
        let glowing = SignSide {
            lines: Default::default(),
            glowing: true,
            color: SignDyeColor::Lime,
        };
        let c = sign_side_color(&glowing);
        assert!((c[0] - 191.0 / 255.0).abs() < 1e-4, "{c:?}");
        assert!((c[1] - 1.0).abs() < 1e-4, "{c:?}");
        assert_eq!(c[2], 0.0);
    }
}
