//! Canonical particle identities and generated payload classifications.

pub use crate::generated_particle_types::PARTICLE_TYPE_COUNT;
use crate::generated_particle_types::{PARTICLE_TYPE_IS_SIMPLE, PARTICLE_TYPE_NAMES};

/// A validated entry in the canonical particle-type census.
///
/// Decode or import a raw registry id with [`Self::new`] before consulting the
/// census. A custom, future, or malformed value deliberately remains outside
/// this type: it may have a payload whose shape this built-in table cannot
/// describe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ParticleTypeId(i32);

impl ParticleTypeId {
    /// Validates a raw particle-type registry id at a wire or import boundary.
    #[must_use]
    pub const fn new(raw: i32) -> Option<Self> {
        if raw >= 0 && raw < PARTICLE_TYPE_COUNT as i32 {
            Some(Self(raw))
        } else {
            None
        }
    }

    /// The registry id emitted by the version-specific wire codec.
    #[must_use]
    pub const fn raw(self) -> i32 {
        self.0
    }
}

/// Resolves a network particle-type id to its canonical `minecraft:*`
/// identifier.
///
/// Raw values enter through [`ParticleTypeId::new`], so this lookup is total.
#[must_use]
pub fn particle_type_name(id: ParticleTypeId) -> &'static str {
    PARTICLE_TYPE_NAMES[id.raw() as usize]
}

/// Whether the canonical particle has no option payload.
/// Generated registration-shape flags do not replace packet framing checks.
#[must_use]
pub fn is_simple_particle_type(id: ParticleTypeId) -> bool {
    PARTICLE_TYPE_IS_SIMPLE[id.raw() as usize]
}


#[cfg(test)]
mod tests {
    use super::*;

    /// Every id this test names is cross-checked against the pinned 26.2
    /// decompile's own particle-type registration source at the time this table was built —
    /// see [`PARTICLE_TYPE_IS_SIMPLE`]'s own doc for the exact derivation.
    /// `gust_emitter_small` (id 34) is the one that matters operationally:
    /// vanilla's own wind-charge explode step sends it as its own
    /// explosion-particle field, and
    /// it is a simple particle type exactly like `explosion_emitter`/
    /// `explosion`, so a decoder that only allowlists the latter two rejects
    /// a perfectly byte-skippable id.
    #[test]
    fn known_ids_classify_correctly() {
        let id = |raw| ParticleTypeId::new(raw).expect("known particle id validates");
        assert!(is_simple_particle_type(id(29)), "explosion_emitter");
        assert!(is_simple_particle_type(id(30)), "explosion");
        assert!(is_simple_particle_type(id(33)), "gust_emitter_large");
        assert!(is_simple_particle_type(id(34)), "gust_emitter_small");
        assert!(!is_simple_particle_type(id(1)), "block carries a BlockParticleOption");
        assert!(!is_simple_particle_type(id(21)), "dust carries a DustParticleOptions");
        assert!(!is_simple_particle_type(id(54)), "item carries an ItemParticleOption");
        assert_eq!(ParticleTypeId::new(-1), None);
        assert_eq!(ParticleTypeId::new(PARTICLE_TYPE_COUNT as i32), None);
    }

    /// The table's length must track the registry, or a version bump silently
    /// truncates classification for every id past the old length.
    #[test]
    fn table_length_matches_registry_count() {
        assert_eq!(PARTICLE_TYPE_IS_SIMPLE.len(), PARTICLE_TYPE_COUNT as usize);
    }
}
