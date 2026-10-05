//! The release a hosted connection speaks, and the translations from the
//! canonical game data the server works in to that release's wire ids.
//!
//! The built-in release is 26.2, whose wire ids are the canonical ones. Another
//! release supplies a [`ServerRelease`]; every id written into a packet body
//! goes through [`Wire`] so a 26.3 client is never handed a 26.2 number.

use lodestone_data::{GameDataVersion, block_states::StateId, item::Item};
use lodestone_world::{BitSetWire, PaletteKind};

use crate::dialect::{FixedRegistryKind, ProtocolDialect, ServerRelease};
use crate::packets::chunk::ChunkShape;

/// The wire a connection speaks. `Copy`, so detached encoders carry it.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Wire {
    release: Option<&'static ServerRelease>,
}

impl Wire {
    pub(crate) const BASE: Self = Self { release: None };

    pub(crate) const fn for_release(release: &'static ServerRelease) -> Self {
        Self { release: Some(release) }
    }

    pub(crate) fn release(self) -> Option<&'static ServerRelease> {
        self.release
    }

    /// The holder id `key` has in the variant registry `registry` as this
    /// release's server sends it, or `None` when the registry does not list it.
    pub(crate) fn holder_id(self, registry: &str, key: &str) -> Option<i32> {
        let names = match self.release {
            Some(release) => release.holder_names(),
            None => crate::registry_data_fixtures::base_holder_names(),
        };
        names.get(registry)?.iter().position(|name| name == key).map(|id| id as i32)
    }

    /// The wire id of the enchantment whose canonical (26.2 registry) id is
    /// `canonical`, or `None` when this release's registry lacks it.
    pub(crate) fn enchantment(self, canonical: i32) -> Option<i32> {
        let base = crate::registry_data_fixtures::base_holder_names().get("minecraft:enchantment")?;
        let name = base.get(usize::try_from(canonical).ok()?)?;
        self.holder_id("minecraft:enchantment", name)
    }

    pub(crate) fn dialect(self) -> Option<&'static ProtocolDialect> {
        self.release.map(|release| &release.dialect)
    }

    pub(crate) fn game_data(self) -> GameDataVersion {
        self.release.map_or(GameDataVersion::V26_2, |release| release.dialect.game_data_version())
    }

    /// Whether this is the 26.3 release, whose packet bodies differ from 26.2's.
    pub(crate) fn is_latest(self) -> bool {
        self.game_data() == GameDataVersion::V26_3
    }

    pub(crate) fn protocol(self) -> i32 {
        self.release.map_or(crate::PROTOCOL, |release| release.dialect.protocol_version())
    }

    /// The wire id for a canonical block state. A state the release does not
    /// have degrades to air rather than writing another block's id.
    pub(crate) fn state(self, state: StateId) -> u32 {
        match self.release {
            None => state.raw(),
            Some(release) => release
                .dialect
                .game_data_version()
                .state_to_wire(state)
                .unwrap_or(0),
        }
    }

    /// The canonical state a wire id names, for reading back packed values.
    pub(crate) fn state_from_wire(self, raw: u32) -> Option<StateId> {
        match self.release {
            None => StateId::new(raw),
            Some(release) => release.dialect.game_data_version().state_from_wire(raw),
        }
    }

    /// Bits per entry of the direct (global) block-state palette.
    pub(crate) fn state_bits(self) -> u32 {
        self.game_data().block_state_wire_bits()
    }

    /// The wire id of a built-in item, or `None` when the release lacks it.
    pub(crate) fn item(self, item: Item) -> Option<i32> {
        match self.release {
            None => Some(i32::from(item.registry_id())),
            Some(release) => release
                .dialect
                .game_data_version()
                .item_to_wire(item)
                .and_then(|raw| i32::try_from(raw).ok()),
        }
    }

    /// The wire id of a built-in item key, or `None` for a custom or unknown key.
    pub(crate) fn item_by_name(self, name: &str) -> Option<i32> {
        self.item(Item::from_name(name)?)
    }

    /// The wire id of a block in the `minecraft:block` registry.
    pub(crate) fn block_by_name(self, name: &str) -> Option<i32> {
        let block = lodestone_data::block::Block::all().find(|block| block.name() == name)?;
        match self.release {
            None => Some(i32::from(block.registry_id())),
            Some(release) => {
                let version = release.dialect.game_data_version();
                (0..version.block_count())
                    .find(|&raw| version.block_from_wire(raw) == Some(block))
                    .and_then(|raw| i32::try_from(raw).ok())
            }
        }
    }

    /// The wire id of an entity type.
    pub(crate) fn entity_type(self, entity: lodestone_data::entity_type::EntityType) -> Option<i32> {
        self.fixed(FixedRegistryKind::Entity, i32::from(entity.registry_id()))
    }

    /// The canonical item a wire id names.
    pub(crate) fn item_from_wire(self, raw: i32) -> Option<Item> {
        let raw = u32::try_from(raw).ok()?;
        match self.release {
            None => u16::try_from(raw).ok().and_then(Item::from_registry_id),
            Some(release) => release.dialect.game_data_version().item_from_wire(raw),
        }
    }

    /// The wire id of a canonical fixed-registry entry, or `None` when the
    /// release has no such entry.
    pub(crate) fn fixed(self, kind: FixedRegistryKind, canonical: i32) -> Option<i32> {
        match self.release {
            None => (0..kind.base_count()).contains(&canonical).then_some(canonical),
            Some(release) => release.dialect.wire_fixed_id(kind, canonical).ok(),
        }
    }

    pub(crate) fn bit_set_wire(self) -> BitSetWire {
        crate::packets::chunk::bit_set_wire(self.game_data())
    }

    /// The chunk framing for a column with this window under this release.
    pub(crate) fn shape(self, mut shape: ChunkShape) -> ChunkShape {
        if let Some(release) = self.release {
            // A direct biome palette is as wide as the biome registry's index
            // space, which is the ceiling of its base-two logarithm.
            let biome_bits = u32::BITS - (release.biome_ids().len() as u32 - 1).leading_zeros();
            shape.biome_kind = PaletteKind::biomes_with_direct_bits(biome_bits)
                .with_framing(lodestone_world::LongArrayFraming::FixedSize);
            shape.block_kind = PaletteKind::block_states_with_direct_bits(self.state_bits())
                .with_framing(lodestone_world::LongArrayFraming::FixedSize);
            shape.air_id = self.state(lodestone_data::block_states::air_state());
        }
        shape
    }

    /// The biome holder id for a biome name, falling back to plains.
    pub(crate) fn biome(self, name: &str) -> u32 {
        match self.release {
            None => super::biome_registry_id(name),
            Some(release) => {
                let ids = release.biome_ids();
                ids.get(name).or_else(|| ids.get("minecraft:plains")).copied()
                    .expect("biome registry missing minecraft:plains")
            }
        }
    }
}
