//! This era's **low-precision velocity** vector, the packed variable-length
//! form that replaced the three fixed `i16` components.
//!
//! # Why it is not three shorts any more
//!
//! Up to and including the 1.20.6 era, every entity velocity on the wire is
//! three big-endian `i16`s in units of `1/8000` block/tick — six bytes,
//! always. Here it is one leading byte for the zero vector and six bytes
//! otherwise, carrying three 15-bit components plus a shared magnitude.
//!
//! Two packets are affected, [`AddEntity`](super::entity::AddEntity) and
//! [`SetEntityMotion`](super::entity::SetEntityMotion), and the failure mode is
//! not a decode error: a stationary entity's velocity is *one* byte here where
//! the old form is *six*, so a decoder inherited from the era below consumes
//! five bytes that belong to the fields after it and reports angles and
//! type-specific data that were never sent.
//!
//! # The layout
//!
//! A leading `0` byte is the zero vector and the field ends there. Otherwise
//! that byte, a second byte and a big-endian `u32` assemble — least-significant
//! byte first — into a 48-bit word laid out as
//!
//! ```text
//!  bits  0..2   magnitude, low two bits, plus a continuation flag at bit 2
//!  bits  3..18  x, 15 bits
//!  bits 18..33  y, 15 bits
//!  bits 33..48  z, 15 bits
//! ```
//!
//! which accounts for all 48 bits exactly. When bit 2 is set, a trailing
//! `varint` carries the magnitude's bits above the low two. Each 15-bit field
//! is a quantisation of `[-1, 1]` over `0..=32766` — so `q * 2 / 32766 - 1` —
//! scaled by the magnitude. The quantisation step is therefore
//! `2 / 32766 ≈ 6.1e-5` block/tick at magnitude 1.
//!
//! # Where that layout comes from
//!
//! Not from a sibling family, and not from a round trip against this module's
//! own reader. `tests/captures/join_1_21_11.txt` holds four of these vectors as
//! a real server sent them, and all four decode to a `y` of `-0.078374`. The
//! outside arithmetic that pins them is vanilla's own falling-entity
//! integration, unchanged since 1.8 and independently implemented in
//! `lodestone-physics`: one tick of gravity followed by vertical air drag is
//! `-0.08 * 0.98 = -0.0784` block/tick. The decoded value differs from that by
//! `2.6e-5`, which is inside a single quantisation step and outside the next
//! one — so the layout, the field offsets and the dequantisation are all
//! confirmed together, by a number this module cannot produce on its own. See
//! `tests/lp_vec3.rs`.

use lodestone_core::{Ctx, Decode, Reader, Result};

/// Number of quantisation steps a 15-bit component is divided into.
///
/// `32767` values are representable in 15 bits; the top one is clamped away so
/// the range is symmetric about zero, leaving `32766` steps and making `0` and
/// `32766` map to exactly `-1` and `+1`.
const STEPS: u64 = 32766;

/// Mask selecting one packed component.
const COMPONENT_MASK: u64 = 0x7fff;

/// Bit offset of the `x` component in the packed word; the header below it
/// holds the magnitude's low two bits and the continuation flag.
const X_SHIFT: u32 = 3;
/// Bit offset of the `y` component.
const Y_SHIFT: u32 = 18;
/// Bit offset of the `z` component.
const Z_SHIFT: u32 = 33;

/// Mask selecting the magnitude's low two bits from the packed word.
const MAGNITUDE_MASK: u64 = 0b11;
/// The continuation flag: a magnitude too large for two bits sets this and
/// appends a `varint` carrying the bits above them.
const MAGNITUDE_CONTINUES: u64 = 0b100;

/// A velocity in blocks per tick, as this era packs it.
///
/// Held as `f64` rather than as the raw packed word: the quantisation is lossy
/// in one direction only, so keeping the wire form would leave every consumer
/// to repeat the same dequantisation.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LpVec3 {
    /// X component, blocks per tick.
    pub x: f64,
    /// Y component, blocks per tick.
    pub y: f64,
    /// Z component, blocks per tick.
    pub z: f64,
}

impl LpVec3 {
    /// The zero vector, which the wire spells as a single `0` byte.
    pub const ZERO: Self = Self {
        x: 0.0,
        y: 0.0,
        z: 0.0,
    };

    /// Whether every component is exactly zero.
    #[must_use]
    pub fn is_zero(&self) -> bool {
        self.x == 0.0 && self.y == 0.0 && self.z == 0.0
    }
}

/// Dequantises one packed 15-bit field to `[-1, 1]`.
fn dequantise(packed: u64) -> f64 {
    #[expect(
        clippy::cast_precision_loss,
        reason = "a value bounded by 32766 is exact in f64"
    )]
    let steps = (packed & COMPONENT_MASK).min(STEPS) as f64;
    #[expect(
        clippy::cast_precision_loss,
        reason = "STEPS is a small constant and exact in f64"
    )]
    let range = STEPS as f64;
    steps * 2.0 / range - 1.0
}

impl Decode for LpVec3 {
    fn decode(r: &mut Reader<'_>, _ctx: Ctx) -> Result<Self> {
        let low = u64::from(r.u8()?);
        if low == 0 {
            return Ok(Self::ZERO);
        }
        let mid = u64::from(r.u8()?);
        let high = u64::from(r.u32()?);
        let word = (high << 16) | (mid << 8) | low;

        let mut magnitude = low & MAGNITUDE_MASK;
        if low & MAGNITUDE_CONTINUES == MAGNITUDE_CONTINUES {
            #[expect(
                clippy::cast_sign_loss,
                reason = "the magnitude's high bits are an unsigned bit pattern"
            )]
            let extra = u64::from(r.var_i32()? as u32);
            magnitude |= extra << 2;
        }

        #[expect(
            clippy::cast_precision_loss,
            reason = "a magnitude beyond f64's exact integer range would already \
                      exceed every velocity the game can express"
        )]
        let magnitude = magnitude as f64;
        Ok(Self {
            x: dequantise(word >> X_SHIFT) * magnitude,
            y: dequantise(word >> Y_SHIFT) * magnitude,
            z: dequantise(word >> Z_SHIFT) * magnitude,
        })
    }
}
