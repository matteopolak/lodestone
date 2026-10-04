//! Name-derived fixed-registry mappings with a retained 26.2 canonical prefix.
//!
//! Wire IDs keep both their release and registry. Canonical IDs keep their
//! registry and must pass the corresponding data type's checked constructor at
//! the model boundary. Identity mapping does not select a value's body layout.
//! Synchronized registries and metadata serializers use separate boundaries.

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WireVersion {
    V26_2,
    V26_3,
}

impl WireVersion {
    const fn index(self) -> usize {
        match self {
            Self::V26_2 => 0,
            Self::V26_3 => 1,
        }
    }
}

struct Run {
    source: u16,
    target: u16,
    len: u16,
}

impl Run {
    const fn new(source: u16, target: u16, len: u16) -> Self {
        Self { source, target, len }
    }
}

struct VersionTable {
    wire_count: u16,
    from_wire: &'static [Run],
    to_wire: &'static [Run],
}

struct RegistryTable {
    name: &'static str,
    canonical_names: &'static [&'static str],
    name_order: &'static [u16],
    versions: [VersionTable; 2],
}

mod generated {
    use super::{RegistryTable, Run, VersionTable};
    include!("generated/fixed_registries.rs");
}

pub use generated::FixedRegistry;

impl FixedRegistry {
    fn table(self) -> &'static RegistryTable {
        &generated::TABLES[self as usize]
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        self.table().name
    }

    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.binary_search_by(|registry| registry.name().cmp(name))
            .ok().map(|index| Self::ALL[index])
    }

    #[must_use]
    pub fn canonical_count(self) -> u32 {
        self.table().canonical_names.len() as u32
    }

    #[must_use]
    pub fn wire_count(self, version: WireVersion) -> u32 {
        u32::from(self.table().versions[version.index()].wire_count)
    }
}

fn translate(runs: &[Run], source: u32) -> Option<u16> {
    let index = runs.partition_point(|run| u32::from(run.source) <= source);
    let run = runs.get(index.checked_sub(1)?)?;
    let offset = source - u32::from(run.source);
    (offset < u32::from(run.len)).then(|| run.target + offset as u16)
}

/// A name identity in the append-only census of one fixed registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CanonicalFixedId {
    registry: FixedRegistry,
    raw: u16,
}

impl CanonicalFixedId {
    #[must_use]
    pub fn new(registry: FixedRegistry, raw: u32) -> Option<Self> {
        (raw < registry.canonical_count()).then_some(Self { registry, raw: raw as u16 })
    }

    #[must_use]
    pub fn from_name(registry: FixedRegistry, name: &str) -> Option<Self> {
        let table = registry.table();
        let index = table.name_order.binary_search_by(|raw| {
            table.canonical_names[usize::from(*raw)].cmp(name)
        }).ok()?;
        Some(Self { registry, raw: table.name_order[index] })
    }

    #[must_use]
    pub const fn registry(self) -> FixedRegistry {
        self.registry
    }

    #[must_use]
    pub const fn raw(self) -> u32 {
        self.raw as u32
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        self.registry.table().canonical_names[usize::from(self.raw)]
    }

    /// Returns no ID when the selected release does not contain this identity.
    #[must_use]
    pub fn to_wire(self, version: WireVersion) -> Option<WireFixedId> {
        let raw = translate(self.registry.table().versions[version.index()].to_wire, self.raw())?;
        Some(WireFixedId { version, registry: self.registry, raw })
    }
}

/// A validated ID in one release's fixed registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WireFixedId {
    version: WireVersion,
    registry: FixedRegistry,
    raw: u16,
}

impl WireFixedId {
    #[must_use]
    pub fn new(version: WireVersion, registry: FixedRegistry, raw: i32) -> Option<Self> {
        let raw = u32::try_from(raw).ok()?;
        (raw < registry.wire_count(version)).then_some(Self { version, registry, raw: raw as u16 })
    }

    #[must_use]
    pub const fn version(self) -> WireVersion {
        self.version
    }

    #[must_use]
    pub const fn registry(self) -> FixedRegistry {
        self.registry
    }

    #[must_use]
    pub const fn raw(self) -> u32 {
        self.raw as u32
    }

    #[must_use]
    pub fn to_canonical(self) -> CanonicalFixedId {
        let raw = translate(self.registry.table().versions[self.version.index()].from_wire, self.raw())
            .expect("each validated fixed wire ID has a generated canonical identity");
        CanonicalFixedId { registry: self.registry, raw }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        self.to_canonical().name()
    }
}

#[cfg(test)]
mod tests {
    use super::{CanonicalFixedId, FixedRegistry, WireFixedId, WireVersion};

    #[test]
    fn literal_report_witnesses_distinguish_shifted_and_new_names() {
        let shifted = WireFixedId::new(WireVersion::V26_3, FixedRegistry::EntityType, 34).unwrap();
        assert_eq!(shifted.to_canonical().raw(), 33);
        assert_eq!(shifted.name(), "minecraft:dark_oak_boat");
        let added = WireFixedId::new(WireVersion::V26_3, FixedRegistry::EntityType, 33).unwrap();
        assert_eq!(added.to_canonical().raw(), 158);
        assert_eq!(added.name(), "minecraft:cushion");
        assert_eq!(added.to_canonical().to_wire(WireVersion::V26_2), None);
    }

    #[test]
    fn same_component_wire_id_names_different_release_identities() {
        let old = WireFixedId::new(WireVersion::V26_2, FixedRegistry::DataComponentType, 40).unwrap();
        let latest = WireFixedId::new(WireVersion::V26_3, FixedRegistry::DataComponentType, 40).unwrap();
        assert_eq!(old.name(), "minecraft:swing_animation");
        assert_eq!(latest.name(), "minecraft:attack_animation");
        assert_eq!(latest.to_canonical().raw(), 111);
        assert_eq!(old.to_canonical().to_wire(WireVersion::V26_3), None);
        assert_ne!(old.to_canonical(), latest.to_canonical());
    }

    #[test]
    fn fixed_block_registry_is_distinct_from_state_registry() {
        let poplar = WireFixedId::new(WireVersion::V26_3, FixedRegistry::Block, 23).unwrap();
        assert_eq!(poplar.name(), "minecraft:poplar_planks");
        assert_eq!(poplar.to_canonical().raw(), 1196);
        let bamboo = CanonicalFixedId::new(FixedRegistry::Block, 23).unwrap();
        assert_eq!(bamboo.name(), "minecraft:bamboo_planks");
        assert_eq!(bamboo.to_wire(WireVersion::V26_3).unwrap().raw(), 24);
    }

    #[test]
    fn bounds_and_absent_release_domains_reject_without_aliasing() {
        assert_eq!(WireFixedId::new(WireVersion::V26_3, FixedRegistry::EntityType, -1), None);
        assert_eq!(WireFixedId::new(WireVersion::V26_3, FixedRegistry::EntityType, 161), None);
        assert_eq!(CanonicalFixedId::new(FixedRegistry::EntityType, 161), None);
        assert_eq!(WireFixedId::new(WireVersion::V26_3, FixedRegistry::DecoratedPotPattern, 0), None);
        assert_eq!(WireFixedId::new(WireVersion::V26_2, FixedRegistry::ContextFloatProviderType, 0), None);
    }

    #[test]
    fn names_resolve_only_with_their_registry() {
        assert_eq!(FixedRegistry::from_name("minecraft:entity_type"), Some(FixedRegistry::EntityType));
        assert_eq!(FixedRegistry::from_name("minecraft:entity_data_serializer"), None);
        let item = CanonicalFixedId::from_name(FixedRegistry::Item, "minecraft:poplar_planks").unwrap();
        assert_eq!(item.raw(), 1537);
        assert_eq!(item.to_wire(WireVersion::V26_3).unwrap().raw(), 72);
        assert_eq!(CanonicalFixedId::from_name(FixedRegistry::EntityType, "minecraft:poplar_planks"), None);
    }
}
