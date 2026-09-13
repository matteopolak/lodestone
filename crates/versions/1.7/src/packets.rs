//! Packet definitions for protocol 5.
//!
//! # How much is shared, and why so little
//!
//! Almost nothing. Measured across every packet definition in both
//! directions and all four states, protocol 5 and its upper neighbour
//! protocol 47 agree on 37 of 112 shapes, and 8 of those 37 are the
//! handshake and status packets that have never changed at all. The
//! definitions this module re-exports from `lodestone-protocol-common` are
//! exactly that measured-identical set; everything else is defined here
//! because it genuinely differs.
//!
//! Three families of difference account for most of it, and each is a
//! whole-module concern rather than a field here and there:
//!
//! - **Numbers are wider and unpacked.** Entity ids are `i32` rather than
//!   varints on all but the four spawn packets; `keep_alive` carries an
//!   `i32`; food, experience level and total experience are `i16`. A varint
//!   decoder pointed at any of them consumes the wrong number of bytes.
//! - **Positions are three separate fields**, in three different width
//!   combinations depending on the packet ([`position`]).
//! - **Item stacks carry gzip-compressed NBT behind an `i16` length**, where
//!   later eras use a bare optional tag ([`slot`]).

pub mod chunk;
pub mod entity;
pub mod game;
pub mod handshake;
pub mod login;
pub mod metadata;
pub mod player_info;
pub mod position;
pub mod settings;
pub mod slot;
pub mod status;
pub mod window;
pub mod world;
