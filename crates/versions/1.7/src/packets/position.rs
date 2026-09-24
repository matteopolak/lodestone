//! Block positions on the protocol 5 wire.
//!
//! # Three widths, not one packed long
//!
//! Protocol 47 introduced the packed 64-bit block position (26 bits of x, 12
//! of y, 26 of z) that every later era uses. Before it, a block position is
//! three separate numbers, and the *widths differ per packet* -- so there is
//! no single `Position` type this era can have. Measured from
//! `minecraft-data`'s 1.7 type table and confirmed against the committed join
//! capture:
//!
//! | shape | y width | packets |
//! |---|---|---|
//! | [`PositionIii`] | `i32` | `spawn_position`, `block_break_animation`, `spawn_entity_painting` |
//! | [`PositionIbi`] | `u8` | `block_change`, `world_event`, `bed` |
//! | [`PositionIsi`] | `i16` | `block_action` |
//!
//! Keeping them as three types rather than one type with a width parameter is
//! deliberate: a wrong y width shifts every following field, and the compiler
//! is the cheapest place to catch that.
//!
//! The `u8` y in [`PositionIbi`] is not a signed byte. World height at this
//! protocol is a fixed `0..256`, so an unsigned byte covers it exactly and
//! there is no negative y to represent.

use lodestone_macros::{Decode, Encode};

/// Block position with a full `i32` y.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
pub struct PositionIii {
    /// Block x.
    pub x: i32,
    /// Block y.
    pub y: i32,
    /// Block z.
    pub z: i32,
}

/// Block position with an unsigned-byte y, covering the fixed `0..256` world.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
pub struct PositionIbi {
    /// Block x.
    pub x: i32,
    /// Block y in `0..256`.
    pub y: u8,
    /// Block z.
    pub z: i32,
}

/// Block position with an `i16` y.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
pub struct PositionIsi {
    /// Block x.
    pub x: i32,
    /// Block y.
    pub y: i16,
    /// Block z.
    pub z: i32,
}

impl PositionIii {
    /// This position as the canonical model's block position.
    #[must_use]
    pub const fn to_model(self) -> lodestone_model::BlockPos {
        lodestone_model::BlockPos::new(self.x, self.y, self.z)
    }
}

impl PositionIbi {
    /// This position as the canonical model's block position.
    #[must_use]
    pub const fn to_model(self) -> lodestone_model::BlockPos {
        lodestone_model::BlockPos::new(self.x, self.y as i32, self.z)
    }
}

impl PositionIsi {
    /// This position as the canonical model's block position.
    #[must_use]
    pub const fn to_model(self) -> lodestone_model::BlockPos {
        lodestone_model::BlockPos::new(self.x, self.y as i32, self.z)
    }
}
