//! Configuration-state packets — the phase that has no counterpart in any era
//! below this one.
//!
//! # What the phase is for
//!
//! Login used to end in the play state, with the join packet carrying the
//! dimension registry inline. Here the connection stops in between: the
//! server sends its registries, its enabled feature flags and its tag sets,
//! and only then do the two sides exchange a finish-configuration packet and
//! enter play. The server may also send the client *back* into configuration
//! mid-session (the play-state `start_configuration` packet), which is how a
//! server switches resource packs or transfers a player without a reconnect.
//!
//! # The registry answer that decides how much data arrives
//!
//! Before sending registries the server asks which data packs the client
//! already knows ([`SelectKnownPacks`]). A client that claims the vanilla
//! pack gets registry entries with **no payload** — the server elides them,
//! expecting the client's own copy. This client answers with an **empty**
//! list, so every entry arrives with its NBT: the vertical extent of a
//! dimension is data we read off the wire rather than data we ship, and a
//! data pack may legitimately change it.
//!
//! That answer is not cosmetic. The play-state join packet identifies the
//! dimension by its **index into the `minecraft:dimension_type` registry**,
//! not by name, so a client that never received the registry cannot know how
//! tall the world is, and every chunk it decodes afterwards is the wrong
//! shape.

use lodestone_core::{Ctx, Decode, Encode, Error, Reader, Result, Writer, read_network_nbt};
use lodestone_macros::{Decode, Encode, Packet};

use crate::packets::common::NetworkNbt;

/// Maximum characters in a resource location on the wire (vanilla's own
/// string limit, which it applies to every identifier field).
const MAX_IDENTIFIER_CHARS: usize = 32767;

/// Sanity cap on entries in one `registry_data` packet. The largest registry
/// a vanilla server synchronizes here is the biome registry, in the dozens;
/// this exists only so a hostile length prefix cannot make us pre-allocate.
const MAX_ENTRIES: usize = 65536;

/// One packed registry entry: its resource id, and its contents when the
/// server did not elide them.
#[derive(Debug, Clone, PartialEq)]
pub struct PackedRegistryEntry {
    /// The entry's resource id, e.g. `minecraft:overworld`.
    pub id: String,
    /// The entry's serialized value, or `None` when the server elided it
    /// because the client claimed the data pack it came from.
    pub data: Option<lodestone_core::Nbt>,
}

/// Clientbound `registry_data`: one registry, in registry order.
///
/// Hand-written rather than derived because the per-entry payload is an
/// optional **anonymous** NBT value, and expressing "bool, then NBT if set"
/// through the derive would need a `present_if` on a field the wire does not
/// name.
#[derive(Debug, Clone, PartialEq)]
pub struct RegistryData {
    /// The registry being synchronized, e.g. `minecraft:dimension_type`.
    pub registry: String,
    /// The registry's entries **in registry order** — index `i` is the id the
    /// join and respawn packets use for that entry.
    pub entries: Vec<PackedRegistryEntry>,
}

impl Decode for RegistryData {
    fn decode(r: &mut Reader<'_>, _ctx: Ctx) -> Result<Self> {
        let registry = r.string(MAX_IDENTIFIER_CHARS)?;
        let count = r.var_i32()?;
        let count = usize::try_from(count).map_err(|_| Error::NegativeLength(count))?;
        if count > MAX_ENTRIES {
            return Err(Error::LimitExceeded {
                limit: MAX_ENTRIES,
                actual: count,
            });
        }
        // `min` rather than the raw count: the cap above already rejects
        // absurd values, this keeps a merely-large-but-legal count from
        // pre-allocating.
        let mut entries = Vec::with_capacity(count.min(256));
        for _ in 0..count {
            let id = r.string(MAX_IDENTIFIER_CHARS)?;
            let data = if r.bool()? {
                Some(read_network_nbt(r)?)
            } else {
                None
            };
            entries.push(PackedRegistryEntry { id, data });
        }
        Ok(Self { registry, entries })
    }
}

/// Clientbound `finish_configuration`: the server is done, play may begin.
/// Empty body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Packet)]
#[mc(name = "minecraft:finish_configuration", state = Configuration, bound = Client, protocols = "766..=766")]
pub struct FinishConfiguration;

impl Encode for FinishConfiguration {
    fn encode(&self, _w: &mut Writer, _ctx: Ctx) -> Result<()> {
        Ok(())
    }
}

impl Decode for FinishConfiguration {
    fn decode(_r: &mut Reader<'_>, _ctx: Ctx) -> Result<Self> {
        Ok(Self)
    }
}

/// Serverbound `finish_configuration`: the client agrees, and the connection
/// enters play. Empty body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Packet)]
#[mc(name = "minecraft:finish_configuration", state = Configuration, bound = Server, protocols = "766..=766")]
pub struct AcknowledgeFinishConfiguration;

impl Encode for AcknowledgeFinishConfiguration {
    fn encode(&self, _w: &mut Writer, _ctx: Ctx) -> Result<()> {
        Ok(())
    }
}

impl Decode for AcknowledgeFinishConfiguration {
    fn decode(_r: &mut Reader<'_>, _ctx: Ctx) -> Result<Self> {
        Ok(Self)
    }
}

/// One data pack the server or client claims to know.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct KnownPack {
    /// Pack namespace, e.g. `minecraft`.
    pub namespace: String,
    /// Pack id, e.g. `core`.
    pub id: String,
    /// Pack version string.
    pub version: String,
}

/// Clientbound `select_known_packs`: the packs the server would elide
/// registry payloads for.
///
/// `minecraft-data` carries the id for this packet but no field list at all,
/// so the shape here comes from a real join capture: three strings per entry
/// behind a varint count, which is what `tests/capture_join.rs` replays.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:select_known_packs", state = Configuration, bound = Client, protocols = "766..=766")]
pub struct SelectKnownPacks {
    /// The packs the server has, in its own order.
    pub packs: Vec<KnownPack>,
}

/// Serverbound `select_known_packs`: the packs this client claims.
///
/// Always sent empty — see the module docs for why claiming none is what
/// makes the dimension registry arrive with its payloads.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:select_known_packs", state = Configuration, bound = Server, protocols = "766..=766")]
pub struct SelectKnownPacksResponse {
    /// The packs this client claims to already have.
    pub packs: Vec<KnownPack>,
}

/// Clientbound `feature_flags`: the experimental feature set this server has
/// enabled.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:feature_flags", state = Configuration, bound = Client, protocols = "766..=766")]
pub struct FeatureFlags {
    /// Enabled feature identifiers.
    pub features: Vec<String>,
}

/// Clientbound `disconnect` during configuration. Its reason is anonymous
/// NBT, unlike the login-state disconnect's JSON string.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:disconnect", state = Configuration, bound = Client, protocols = "766..=766")]
pub struct ConfigurationDisconnect {
    /// Chat component describing the refusal.
    pub reason: NetworkNbt,
}

/// Clientbound `keep_alive` during configuration — the same liveness probe
/// the play state runs, in a state that can now last arbitrarily long.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:keep_alive", state = Configuration, bound = Client, protocols = "766..=766")]
pub struct ConfigurationKeepAliveRequest {
    /// Opaque id the client must echo back unchanged.
    pub id: i64,
}

/// Serverbound `keep_alive` during configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:keep_alive", state = Configuration, bound = Server, protocols = "766..=766")]
pub struct ConfigurationKeepAliveResponse {
    /// The id from the request this answers.
    pub id: i64,
}

/// Clientbound `ping` during configuration: a round-trip probe the client
/// answers with the same id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:ping", state = Configuration, bound = Client, protocols = "766..=766")]
pub struct ConfigurationPing {
    /// Opaque id to echo.
    pub id: i32,
}

/// Serverbound `pong` during configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:pong", state = Configuration, bound = Server, protocols = "766..=766")]
pub struct ConfigurationPong {
    /// The id from the ping this answers.
    pub id: i32,
}

/// Serverbound `custom_payload` during configuration — how this client
/// announces its brand before play begins.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:custom_payload", state = Configuration, bound = Server, protocols = "766..=766")]
pub struct ConfigurationBrandPayload {
    /// Plugin-message channel; `minecraft:brand` for this use.
    #[mc(max = 32767)]
    pub channel: String,
    /// Client brand string.
    #[mc(max = 32767)]
    pub brand: String,
}

/// Serverbound `settings` (client information) during configuration.
///
/// The same eight fields the play-state packet carries — see
/// [`crate::packets::settings::Settings`] for the field-by-field notes. It
/// exists in both states from this era on because the server needs the
/// client's locale and view distance *before* play begins, and
/// `minecraft-data` has no field list for either state at 766 (it does at
/// 764 and 765, where the shape is the same), so the check on this shape is
/// the join capture: a server with strict error handling on closes the
/// connection on a malformed configuration packet rather than ignoring it.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:settings", state = Configuration, bound = Server, protocols = "766..=766")]
pub struct ConfigurationSettings {
    /// Client locale, such as `en_us`.
    #[mc(max = 16)]
    pub locale: String,
    /// Requested render distance in chunks.
    pub view_distance: i8,
    /// Chat visibility: `0` full, `1` commands only, `2` hidden.
    #[mc(varint)]
    pub chat_flags: i32,
    /// Whether chat colours are enabled.
    pub chat_colors: bool,
    /// Displayed skin-part bitmask.
    pub skin_parts: u8,
    /// Dominant hand: `0` left, `1` right.
    #[mc(varint)]
    pub main_hand: i32,
    /// The text-filtering flag.
    pub text_filtering: bool,
    /// Whether the player may appear in the server's public player sample.
    pub allow_server_listing: bool,
}
