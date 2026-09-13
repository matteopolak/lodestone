//! The 1.8 (protocol 47) `slot` wire type -- a single inventory item stack.
//!
//! Byte-identical to v1-9's own codec (measured), but genuinely **not** to
//! v1-14's: the 1.13 flattening removed the `(blockId, meta)` split. Shared
//! with v1-9 only; see `lodestone-protocol-common`'s `packets::slot` module
//! docs for the full finding, including which packets embedding `Slot`
//! inherit this same restriction.

pub use lodestone_protocol_common::packets::slot::Slot;
