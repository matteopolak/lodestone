//! Minecraft 26.3 protocol facts verified against the release jar.
//!
//! This crate records protocol metadata and packet IDs for the next client
//! dialect. The 26.2 crate is its declared compatibility base, but packet IDs
//! alone do not establish compatible packet bodies, registries, or game data.
//! No client adapter or server protocol is exported until those differences
//! have independent wire evidence.

#![forbid(unsafe_code)]

/// Packet names and IDs generated from the 26.3 server report.
#[path = "generated/packet_ids.rs"]
pub mod packet_ids;

pub const PROTOCOL: i32 = packet_ids::PROTOCOL_VERSION;
pub const MINECRAFT_VERSION: &str = packet_ids::MINECRAFT_VERSION;
pub const DATA_VERSION: u32 = 5023;
pub const DATA_PACK_VERSION: (u32, u32) = (121, 0);
pub const RESOURCE_PACK_VERSION: (u32, u32) = (97, 1);

/// The reviewed connection boundary of the 26.3 dialect.
///
/// The shared core translates packet IDs by canonical names. It rejects Play
/// and configuration registry/tag payloads because their bodies and registry
/// mappings have not yet been reviewed for 26.3.
#[must_use]
pub fn connection_dialect() -> lodestone_v26_2::dialect::ProtocolDialect {
    use lodestone_v26_2::dialect::{PacketTables, ProtocolDialect, StatePackets};

    macro_rules! packets {
        ($state:ident) => {
            StatePackets {
                clientbound: packet_ids::$state::clientbound::ENTRIES,
                serverbound: packet_ids::$state::serverbound::ENTRIES,
            }
        };
    }

    let tables = PacketTables {
        handshaking: packets!(handshaking),
        status: packets!(status),
        login: packets!(login),
        configuration: packets!(configuration),
        play: packets!(play),
    };
    ProtocolDialect::connection_only(PROTOCOL, &[MINECRAFT_VERSION], tables)
        .expect("generated 26.3 packet IDs must be unique within each state and direction")
}
