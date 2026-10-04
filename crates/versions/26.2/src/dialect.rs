//! Protocol identity and packet identifiers for the reusable connection codec.

use lodestone_core::{Bound, State};
use lodestone_data::{GameDataVersion, block::Block};
use lodestone_model::{AdapterError, Directive};

use crate::packet_ids;

/// Packet names and identifiers from one direction of a generated packet report.
pub type PacketEntries = &'static [(&'static str, i32)];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixedRegistryKind {
    BlockEntity,
    Entity,
    Particle,
    Sound,
    DataComponent,
    Attribute,
    Menu,
    CustomStat,
    CommandParser,
    MapDecoration,
    PositionSource,
    DebugSubscription,
    VillagerType,
    VillagerProfession,
}

impl FixedRegistryKind {
    pub(crate) fn base_count(self) -> i32 {
        match self {
            Self::BlockEntity => 49,
            Self::Entity => 158,
            Self::Particle => 125,
            Self::Sound => 1968,
            Self::DataComponent => 111,
            Self::Attribute => 40,
            Self::Menu => 25,
            Self::CustomStat => 77,
            Self::CommandParser => 57,
            Self::MapDecoration => 35,
            Self::PositionSource => 2,
            Self::DebugSubscription => 16,
            Self::VillagerType => 7,
            Self::VillagerProfession => 15,
        }
    }
}

#[derive(Debug)]
pub struct FixedRegistryMappings {
    pub decode: fn(FixedRegistryKind, i32) -> Option<i32>,
    pub encode: fn(FixedRegistryKind, i32) -> Option<i32>,
    pub name: fn(FixedRegistryKind, i32) -> Option<&'static str>,
}

/// The two directional identifier spaces of a connection state.
#[derive(Debug, Clone, Copy)]
pub struct StatePackets {
    pub clientbound: PacketEntries,
    pub serverbound: PacketEntries,
}

/// Identifiers remain scoped to both connection state and packet direction.
#[derive(Debug, Clone, Copy)]
pub struct PacketTables {
    pub handshaking: StatePackets,
    pub status: StatePackets,
    pub login: StatePackets,
    pub configuration: StatePackets,
    pub play: StatePackets,
}

impl PacketTables {
    #[must_use]
    pub fn entries(&self, state: State, bound: Bound) -> PacketEntries {
        let packets = match state {
            State::Handshaking => self.handshaking,
            State::Status => self.status,
            State::Login => self.login,
            State::Configuration => self.configuration,
            State::Play => self.play,
        };
        match bound {
            Bound::Client => packets.clientbound,
            Bound::Server => packets.serverbound,
        }
    }

    fn validate(&self) -> Result<(), AdapterError> {
        for state in [
            State::Handshaking,
            State::Status,
            State::Login,
            State::Configuration,
            State::Play,
        ] {
            for bound in [Bound::Client, Bound::Server] {
                let entries = self.entries(state, bound);
                for (index, &(name, id)) in entries.iter().enumerate() {
                    let duplicate = entries[..index]
                        .iter()
                        .any(|&(other, other_id)| other == name || other_id == id);
                    if name.is_empty() || id < 0 || duplicate {
                        return Err(AdapterError::Unsupported(format!(
                            "invalid or duplicate packet entry {name:?}={id} in {state:?}/{bound:?}"
                        )));
                    }
                }
            }
        }
        Ok(())
    }
}

macro_rules! state_packets {
    ($state:ident) => {
        StatePackets {
            clientbound: packet_ids::$state::clientbound::ENTRIES,
            serverbound: packet_ids::$state::serverbound::ENTRIES,
        }
    };
}

/// The generated tables consumed by the 26.2 adapter's internal dispatch.
pub static V26_2_PACKET_TABLES: PacketTables = PacketTables {
    handshaking: state_packets!(handshaking),
    status: state_packets!(status),
    login: state_packets!(login),
    configuration: state_packets!(configuration),
    play: state_packets!(play),
};

/// Configuration-phase payloads a server hosting a release replays verbatim:
/// every synchronized `registry_data` packet in wire order, then `update_tags`.
/// Each entry is the hex fixture text of one clientbound payload captured from
/// that release's own server (`#` comment lines allowed).
#[derive(Debug, Clone, Copy)]
pub struct ServerConfigFixtures {
    pub registries: &'static [(&'static str, &'static str)],
    pub update_tags: &'static str,
}

/// Everything a server needs to speak a release other than the built-in 26.2:
/// the wire dialect and the replayed Configuration payloads.
#[derive(Debug)]
pub struct ServerRelease {
    pub dialect: ProtocolDialect,
    pub config: ServerConfigFixtures,
    biome_ids: std::sync::OnceLock<std::collections::HashMap<String, u32>>,
}

impl ServerRelease {
    #[must_use]
    pub fn new(dialect: ProtocolDialect, config: ServerConfigFixtures) -> Self {
        Self { dialect, config, biome_ids: std::sync::OnceLock::new() }
    }

    /// Biome holder ids in this release's own synchronized registry order.
    pub(crate) fn biome_ids(&self) -> &std::collections::HashMap<String, u32> {
        self.biome_ids.get_or_init(|| {
            crate::registry_data_fixtures::biome_names_in(&self.config)
                .into_iter()
                .enumerate()
                .map(|(id, name)| (name, id as u32))
                .collect()
        })
    }
}

/// Selected wire identity, independent of the shared packet-body codecs.
///
/// Custom dialects support connection traffic only unless a reviewed registry
/// body opts into the shared decoder. Tags and Play remain gated separately.
#[derive(Debug, Clone, Copy)]
pub struct ProtocolDialect {
    protocol: i32,
    versions: &'static [&'static str],
    packets: PacketTables,
    canonical: bool,
    registry_data: bool,
    play: bool,
    game_data: GameDataVersion,
    fixed_registries: Option<&'static FixedRegistryMappings>,
}

impl ProtocolDialect {
    /// The complete built-in 26.2 dialect.
    #[must_use]
    pub fn v26_2() -> Self {
        Self {
            protocol: crate::PROTOCOL,
            versions: &["26.2"],
            packets: V26_2_PACKET_TABLES,
            canonical: true,
            registry_data: true,
            play: true,
            game_data: GameDataVersion::V26_2,
            fixed_registries: None,
        }
    }

    /// Selects identifiers for independently reviewed, unchanged connection bodies.
    /// This does not establish gameplay or registry compatibility.
    pub fn connection_only(
        protocol: i32,
        versions: &'static [&'static str],
        packets: PacketTables,
    ) -> Result<Self, AdapterError> {
        if protocol < 0 || versions.is_empty() || versions.iter().any(|version| version.is_empty()) {
            return Err(AdapterError::Unsupported(
                "invalid dialect identity".to_owned(),
            ));
        }
        packets.validate()?;
        Ok(Self {
            protocol,
            versions,
            packets,
            canonical: false,
            registry_data: false,
            play: false,
            game_data: GameDataVersion::V26_2,
            fixed_registries: None,
        })
    }

    /// Allows Configuration registry bodies through the shared decoder after
    /// their framing has been checked against independent wire bytes. This does
    /// not permit tags or Play, whose numeric game-data IDs need separate work.
    #[must_use]
    pub fn with_reviewed_registry_data(mut self) -> Self {
        self.registry_data = true;
        self
    }

    /// Opens Play for a dialect whose Play bodies and game-data translations
    /// have been reviewed against its release. Packet IDs are still
    /// translated by name into the compatibility core's tables.
    #[must_use]
    pub fn with_reviewed_play(mut self) -> Self {
        self.play = true;
        self
    }

    #[must_use]
    pub fn with_game_data_version(mut self, version: GameDataVersion) -> Self {
        self.game_data = version;
        self
    }

    #[must_use]
    pub fn with_fixed_registries(mut self, mappings: &'static FixedRegistryMappings) -> Self {
        self.fixed_registries = Some(mappings);
        self
    }

    pub fn canonical_fixed_id(self, kind: FixedRegistryKind, raw: i32) -> Result<i32, AdapterError> {
        let value = match self.fixed_registries {
            Some(mappings) => (mappings.decode)(kind, raw),
            None => (raw >= 0 && raw < kind.base_count()).then_some(raw),
        };
        value.ok_or_else(|| AdapterError::Unsupported(format!(
            "invalid {kind:?} wire ID {raw} for protocol {}", self.protocol
        )))
    }

    pub fn wire_fixed_id(self, kind: FixedRegistryKind, canonical: i32) -> Result<i32, AdapterError> {
        let value = match self.fixed_registries {
            Some(mappings) => (mappings.encode)(kind, canonical),
            None => (canonical >= 0 && canonical < kind.base_count()).then_some(canonical),
        };
        value.ok_or_else(|| AdapterError::Unsupported(format!(
            "unsupported {kind:?} canonical ID {canonical} for protocol {}", self.protocol
        )))
    }

    #[must_use]
    pub fn fixed_registry_name(self, kind: FixedRegistryKind, canonical: i32) -> Option<&'static str> {
        if let Some(mappings) = self.fixed_registries {
            return (mappings.name)(kind, canonical);
        }
        if canonical < 0 || canonical >= kind.base_count() {
            return None;
        }
        match kind {
            FixedRegistryKind::Particle => lodestone_data::particle_types::ParticleTypeId::new(canonical)
                .map(lodestone_data::particle_types::particle_type_name),
            FixedRegistryKind::DataComponent => lodestone_data::data_component_types::DataComponentTypeId::new(canonical)
                .map(lodestone_data::data_component_types::component_type_name),
            FixedRegistryKind::Entity => lodestone_data::entity_types::entity_type_name(canonical),
            FixedRegistryKind::Sound => lodestone_data::sound_events::SoundEventId::new(canonical)
                .map(lodestone_data::sound_events::sound_event_name),
            _ => None,
        }
    }

    #[must_use]
    pub const fn game_data_version(self) -> GameDataVersion {
        self.game_data
    }

    #[must_use]
    pub fn block_from_wire(self, raw: u32) -> Option<Block> {
        self.game_data.block_from_wire(raw)
    }

    #[must_use]
    pub fn protocol_version(&self) -> i32 {
        self.protocol
    }

    #[must_use]
    pub fn minecraft_versions(&self) -> &'static [&'static str] {
        self.versions
    }

    pub(crate) fn check_state(&self, state: State) -> Result<(), AdapterError> {
        if !self.canonical && !self.play && state == State::Play {
            return Err(AdapterError::Unsupported(format!(
                "protocol {} has no reviewed Play codecs or registry mappings",
                self.protocol
            )));
        }
        Ok(())
    }

    pub(crate) fn clientbound_name(self, state: State, id: i32) -> Option<&'static str> {
        self.packets.entries(state, Bound::Client).iter()
            .find_map(|&(name, raw)| (raw == id).then_some(name))
    }

    pub(crate) fn inbound(&self, state: State, id: i32) -> Result<i32, AdapterError> {
        self.check_state(state)?;
        if self.canonical {
            return Ok(id);
        }
        let id = translate(
            self.packets.entries(state, Bound::Client),
            V26_2_PACKET_TABLES.entries(state, Bound::Client),
            state,
            Bound::Client,
            id,
        )?;
        if state == State::Configuration
            && ((id == packet_ids::configuration::clientbound::REGISTRY_DATA
                && !self.registry_data)
                || (id == packet_ids::configuration::clientbound::UPDATE_TAGS && !self.play))
        {
            return Err(AdapterError::Unsupported(format!(
                "protocol {} has no reviewed registry mappings",
                self.protocol
            )));
        }
        Ok(id)
    }

    pub(crate) fn outbound(&self, state: State, id: i32) -> Result<i32, AdapterError> {
        self.check_state(state)?;
        if self.canonical {
            return Ok(id);
        }
        if state == State::Play
            && id == packet_ids::play::serverbound::SWING
            && self.game_data == GameDataVersion::V26_3
        {
            return self.packets.play.serverbound.iter()
                .find_map(|&(name, id)| (name == "minecraft:punch").then_some(id))
                .ok_or_else(|| AdapterError::Unsupported("dialect has no punch packet".to_owned()));
        }
        translate(
            V26_2_PACKET_TABLES.entries(state, Bound::Server),
            self.packets.entries(state, Bound::Server),
            state,
            Bound::Server,
            id,
        )
    }

    /// Rewrites a clientbound packet id from this crate's 26.2 numbering into
    /// the dialect's own, by packet name. A server hosting the dialect writes
    /// every outgoing packet through this.
    pub fn host_clientbound_id(&self, state: State, base_id: i32) -> Result<i32, AdapterError> {
        if self.canonical {
            return Ok(base_id);
        }
        translate(
            V26_2_PACKET_TABLES.entries(state, Bound::Client),
            self.packets.entries(state, Bound::Client),
            state,
            Bound::Client,
            base_id,
        )
    }

    /// The dialect's own id for a clientbound packet by name, or `None` when
    /// the release has no such packet.
    #[must_use]
    pub fn clientbound_id_named(&self, state: State, name: &str) -> Option<i32> {
        self.packets
            .entries(state, Bound::Client)
            .iter()
            .find_map(|&(entry, id)| (entry == name).then_some(id))
    }

    /// Rewrites a serverbound packet id received under the dialect into this
    /// crate's 26.2 numbering, by packet name. `None` when the dialect names no
    /// such packet or the 26.2 tables have no counterpart.
    #[must_use]
    pub fn host_serverbound_base_id(&self, state: State, wire_id: i32) -> Option<i32> {
        if self.canonical {
            return Some(wire_id);
        }
        translate(
            self.packets.entries(state, Bound::Server),
            V26_2_PACKET_TABLES.entries(state, Bound::Server),
            state,
            Bound::Server,
            wire_id,
        )
        .ok()
    }

    /// The name the dialect gives a serverbound packet id.
    #[must_use]
    pub fn serverbound_name(&self, state: State, wire_id: i32) -> Option<&'static str> {
        self.packets
            .entries(state, Bound::Server)
            .iter()
            .find_map(|&(name, raw)| (raw == wire_id).then_some(name))
    }

    pub(crate) fn directives(
        &self,
        mut state: State,
        mut directives: Vec<Directive>,
    ) -> Result<Vec<Directive>, AdapterError> {
        for directive in &mut directives {
            match directive {
                Directive::Send { packet_id, .. } => *packet_id = self.outbound(state, *packet_id)?,
                Directive::SetState(next) => {
                    self.check_state(*next)?;
                    state = *next;
                }
                _ => {}
            }
        }
        Ok(directives)
    }
}

fn translate(
    from: PacketEntries,
    to: PacketEntries,
    state: State,
    bound: Bound,
    id: i32,
) -> Result<i32, AdapterError> {
    let name = from
        .iter()
        .find_map(|&(name, candidate)| (candidate == id).then_some(name));
    name.and_then(|name| {
        to.iter().find_map(|&(candidate, id)| (candidate == name).then_some(id))
    }).ok_or_else(|| {
        AdapterError::Unsupported(format!("unmapped packet {id} in {state:?}/{bound:?}"))
    })
}

#[cfg(test)]
mod tests {
    use super::{FixedRegistryKind, ProtocolDialect};

    #[test]
    fn base_registry_bounds_do_not_expand_with_canonical_tables() {
        let dialect = ProtocolDialect::v26_2();
        for (kind, last, first_absent) in [
            (FixedRegistryKind::DataComponent, 110, 111),
            (FixedRegistryKind::Particle, 124, 125),
            (FixedRegistryKind::Entity, 157, 158),
            (FixedRegistryKind::Sound, 1967, 1968),
        ] {
            assert_eq!(dialect.canonical_fixed_id(kind, last).unwrap(), last);
            assert_eq!(dialect.wire_fixed_id(kind, last).unwrap(), last);
            for raw in [-1, first_absent] {
                assert!(dialect.canonical_fixed_id(kind, raw).is_err());
                assert!(dialect.wire_fixed_id(kind, raw).is_err());
            }
        }
    }
}
