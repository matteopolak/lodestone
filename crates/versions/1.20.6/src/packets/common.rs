//! Types both directions of this era share, and the wire form that separates
//! this era from every one below it: **anonymous** (network-form) NBT.
//!
//! From 1.20.3 on, every text component and every registry payload on the
//! wire is binary NBT written *without* a root name — a tag byte followed
//! immediately by its payload. Below this era the same fields are either JSON
//! strings (text) or named NBT (registry blobs), and the derive's
//! `#[mc(nbt)]` attribute reads the named form, so it cannot express a field
//! at this protocol. [`NetworkNbt`] is the newtype that can: it participates
//! in the derive like any other field type and reads/writes the anonymous
//! form.
//!
//! The keep-alive pair is defined here rather than re-exported from
//! `lodestone-protocol-common`, whose definition is declared `340..=762` and
//! therefore refuses to encode at this protocol even though the shape is
//! identical.

use lodestone_core::{Ctx, Decode, Encode, Nbt, Reader, Result, Writer, read_network_nbt,
    write_network_nbt};
use lodestone_macros::{Decode, Encode, Packet};

/// A binary NBT value in this era's **anonymous** wire form: a tag byte
/// followed immediately by the payload, with no root name.
///
/// Wrapping [`Nbt`] rather than using it directly is what lets a derived
/// struct hold one: the newtype's [`Encode`]/[`Decode`] pair selects the
/// anonymous form, where the derive's own `#[mc(nbt)]` selects the named one.
#[derive(Debug, Clone, PartialEq)]
pub struct NetworkNbt(pub Nbt);

impl Encode for NetworkNbt {
    fn encode(&self, w: &mut Writer, _ctx: Ctx) -> Result<()> {
        write_network_nbt(w, &self.0)
    }
}

impl Decode for NetworkNbt {
    fn decode(r: &mut Reader<'_>, _ctx: Ctx) -> Result<Self> {
        Ok(Self(read_network_nbt(r)?))
    }
}

/// Clientbound `keep_alive` — the play-state liveness probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:keep_alive", state = Play, bound = Client, protocols = "766..=766")]
pub struct KeepAliveRequest {
    /// Opaque id the client must echo back unchanged.
    pub id: i64,
}

/// Serverbound `keep_alive` — the echo of [`KeepAliveRequest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:keep_alive", state = Play, bound = Server, protocols = "766..=766")]
pub struct KeepAliveResponse {
    /// The id from the request this answers.
    pub id: i64,
}
