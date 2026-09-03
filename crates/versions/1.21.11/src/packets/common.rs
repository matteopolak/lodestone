//! Types both directions of this era share, including the wire form that
//! separates every 1.20.3-and-later era from the ones below it: **anonymous**
//! (network-form) NBT.
//!
//! Every text component and every registry payload on this wire is binary NBT
//! written *without* a root name — a tag byte followed immediately by its
//! payload. The derive's `#[mc(nbt)]` attribute reads the *named* form, so it
//! cannot express a field at this protocol. [`NetworkNbt`] is the newtype that
//! can: it participates in the derive like any other field type and
//! reads/writes the anonymous form.
//!
//! The keep-alive pair is defined here rather than re-exported from
//! `lodestone-protocol-common`, whose definition is declared `340..=762` and
//! therefore refuses to encode at this protocol even though the shape is
//! identical.

use lodestone_core::{
    Ctx, Decode, Encode, Error, Nbt, Reader, Result, Writer, read_network_nbt, write_network_nbt,
};
use lodestone_macros::{Decode, Encode, Packet};

/// A binary NBT value in this era's **anonymous** wire form: a tag byte
/// followed immediately by the payload, with no root name.
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

/// Reads a **registry-entry holder**'s id form, refusing the inline form.
///
/// A holder writes `id + 1` for a reference into a registry the configuration
/// phase already delivered, and `0` to mean "the whole entry follows inline".
/// The inline payload's width is implied by the entry type, so a holder whose
/// entry this crate does not model cannot be skipped past — hence an error
/// naming the field rather than a guess. `what` names the field so the error
/// says which holder was met.
///
/// # Errors
///
/// Returns an error for the inline (`0`) form, and propagates a truncated
/// varint.
pub fn read_registry_holder_id(r: &mut Reader<'_>, what: &'static str) -> Result<i32> {
    let raw = r.var_i32()?;
    if raw == 0 {
        return Err(Error::Custom(format!(
            "{what} arrived as an inline registry entry, which this protocol \
             family does not model; only the registry-id form is supported"
        )));
    }
    Ok(raw - 1)
}

/// Clientbound `minecraft:keep_alive` — the play-state liveness probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:keep_alive", state = Play, bound = Client, protocols = "774..=774")]
pub struct KeepAliveRequest {
    /// Opaque id the client must echo back unchanged.
    pub id: i64,
}

/// Serverbound `minecraft:keep_alive` — the echo of [`KeepAliveRequest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:keep_alive", state = Play, bound = Server, protocols = "774..=774")]
pub struct KeepAliveResponse {
    /// The id from the request this answers.
    pub id: i64,
}
