//! Packet definitions shared by two or more legacy protocol families.
//!
//! # What lives here, and what does not
//!
//! Every type in [`packets`] was measured byte-identical -- wire shape *and*
//! codec, not just field list -- across the protocol range it declares.
//! `cargo xtask protocol-dup`'s struct-identity scan is the starting point,
//! not the proof: it compares only a struct's own field list (attrs
//! included), so it cannot see a hand-written `Encode`/`Decode` impl that
//! diverges, or a field whose *type* is itself a per-family type despite
//! sharing a name. Two measured instances of exactly that trap are recorded
//! in `src/packets/position.rs` and `src/packets/slot.rs`'s module docs --
//! read those before adding another hand-decoded type here.
//!
//! This crate is version-free by construction and by path: it lives under
//! `crates/`, not `crates/protocol/`, so `xtask`'s isolation lint
//! (`package_is_version_crate`) never classifies it as a version crate. It
//! must never depend on a `crates/protocol/*` family, and no family may
//! depend on another family through it.
//!
//! A macro-derived packet (`#[derive(Packet)]`) declares its measured range
//! with `#[mc(protocols = "a..=b")]`; the derive checks `ctx.version` against
//! it on every encode/decode. A hand-decoded type with no `Packet` derive
//! (`Position`, `Slot`, `PlayerInfo` and its satellites) has no attribute to
//! carry that check, so its module doc states the range instead -- callers
//! must not use it outside that range.

pub mod packets;
