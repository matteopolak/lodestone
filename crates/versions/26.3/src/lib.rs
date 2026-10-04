//! Minecraft 26.3 protocol facts verified against the release jar.
//!
//! This crate records protocol metadata and packet IDs for the 26.3 client
//! dialect. The 26.2 crate is its declared compatibility base: the shared
//! adapter decodes 26.3 by translating packet IDs by name, numeric game-data
//! IDs through the 26.3 tables, and the bodies whose layout changed through
//! release-specific readers.

#![forbid(unsafe_code)]

/// Packet names and IDs generated from the 26.3 server report.
#[path = "generated/packet_ids.rs"]
pub mod packet_ids;

/// Translation between canonical game-data IDs and 26.3 wire IDs.
pub mod id_translation;
pub mod fixed_registries;
pub mod fixed_registry_bridge;

/// Packet bodies whose layout is specific to protocol 777.
pub mod packets;

pub const PROTOCOL: i32 = packet_ids::PROTOCOL_VERSION;
pub const MINECRAFT_VERSION: &str = packet_ids::MINECRAFT_VERSION;
pub const DATA_VERSION: u32 = 5023;
pub const DATA_PACK_VERSION: (u32, u32) = (121, 0);
pub const RESOURCE_PACK_VERSION: (u32, u32) = (97, 1);

/// The 26.3 client dialect: packet IDs translated by canonical name, game-data
/// IDs through the 26.3 tables, Configuration and Play both admitted.
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
        .with_reviewed_registry_data()
        .with_game_data_version(lodestone_data::GameDataVersion::V26_3)
        .with_fixed_registries(&fixed_registry_bridge::V26_3_FIXED_REGISTRIES)
        .with_reviewed_play()
}

/// A client adapter speaking protocol 777.
#[must_use]
pub fn adapter() -> lodestone_v26_2::V770Adapter {
    lodestone_v26_2::V770Adapter::with_connection_dialect(connection_dialect())
}
