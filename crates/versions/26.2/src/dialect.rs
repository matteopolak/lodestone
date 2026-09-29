//! Protocol identity and packet identifiers for the reusable connection codec.

use lodestone_core::{Bound, State};
use lodestone_model::{AdapterError, Directive};

use crate::packet_ids;

/// Packet names and identifiers from one direction of a generated packet report.
pub type PacketEntries = &'static [(&'static str, i32)];

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

    #[must_use]
    pub fn protocol_version(&self) -> i32 {
        self.protocol
    }

    #[must_use]
    pub fn minecraft_versions(&self) -> &'static [&'static str] {
        self.versions
    }

    pub(crate) fn check_state(&self, state: State) -> Result<(), AdapterError> {
        if !self.canonical && state == State::Play {
            return Err(AdapterError::Unsupported(format!(
                "protocol {} has no reviewed Play codecs or registry mappings",
                self.protocol
            )));
        }
        Ok(())
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
                || id == packet_ids::configuration::clientbound::UPDATE_TAGS)
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
        translate(
            V26_2_PACKET_TABLES.entries(state, Bound::Server),
            self.packets.entries(state, Bound::Server),
            state,
            Bound::Server,
            id,
        )
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
