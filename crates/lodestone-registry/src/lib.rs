//! Version registry: maps a protocol number to a concrete [`VersionAdapter`].
//!
//! This is the single, deliberate aggregation point where Lodestone names
//! concrete protocol version crates. Every other shared crate (notably
//! `lodestone-client`) depends on *this* crate and asks it for an adapter, so no
//! other shared crate ever names a version directly.
//!
//! # Why this crate is allowed to name versions
//!
//! The project's hard rule is that dropping a version means deleting a single
//! `crates/protocol/<version>` folder. To keep that true while still having
//! *somewhere* that knows about concrete versions, this crate:
//!
//! - depends on each version family only through an **optional, feature-gated**
//!   dependency, so the default build compiles no version crate at all and any
//!   family can be removed by deleting its folder plus its one dependency line
//!   and one feature line here; and
//! - is marked with `[package.metadata.lodestone-isolation] role =
//!   "version-registry"` so the isolation lint treats these optional edges as
//!   the intended aggregation rather than a wart — an exemption that, by
//!   construction, can only reclassify non-fatal warnings and never silences a
//!   real (build-breaking) isolation violation.
//!
//! Adding a family is one dependency line, one feature line, and one entry in
//! [`FAMILIES`]. Nothing else in the workspace changes.

#![forbid(unsafe_code)]

use lodestone_model::VersionAdapter;

/// A compiled-in protocol version family.
///
/// `make` constructs a fresh boxed adapter; `supports` reports whether the
/// family handles a given protocol number without allocating.
#[derive(Clone, Copy)]
struct Family {
    /// Human-readable family label, e.g. `"v47"`.
    label: &'static str,
    /// Constructs a boxed adapter for this family.
    make: fn() -> Box<dyn VersionAdapter>,
}

impl std::fmt::Debug for Family {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Family")
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

/// Every version family compiled into this build.
///
/// The list is assembled at compile time from the enabled `vNNN` features; a
/// default build with no family features is empty. Each entry is gated so that
/// deleting a family's folder (and its feature) removes exactly one line here.
const FAMILIES: &[Family] = &[
    #[cfg(feature = "v47")]
    Family {
        label: "v47",
        make: || Box::new(lodestone_v47::adapter()),
    },
    #[cfg(feature = "v770")]
    Family {
        label: "v770",
        make: || Box::new(lodestone_v770::adapter()),
    },
    #[cfg(feature = "v340")]
    Family {
        label: "v340",
        make: || Box::new(lodestone_v340::adapter()),
    },
];

/// Returns a boxed adapter for `protocol`, if a compiled-in family supports it.
///
/// Returns `None` when no enabled family handles that protocol number — for
/// example in a default build with no `vNNN` feature enabled.
#[must_use]
pub fn adapter_for_protocol(protocol: i32) -> Option<Box<dyn VersionAdapter>> {
    FAMILIES.iter().find_map(|family| {
        let adapter = (family.make)();
        adapter.supports(protocol).then_some(adapter)
    })
}

/// Returns the primary protocol number of every compiled-in family.
#[must_use]
pub fn supported_protocols() -> Vec<i32> {
    FAMILIES
        .iter()
        .map(|family| (family.make)().protocol_version())
        .collect()
}

/// Returns the label of every compiled-in family (for diagnostics).
#[must_use]
pub fn compiled_families() -> Vec<&'static str> {
    FAMILIES.iter().map(|family| family.label).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_build_has_no_families() {
        // With no `vNNN` feature enabled the registry is empty, which is what
        // keeps the default build version-free. Feature-enabled behaviour is
        // covered by the client's live tests, which turn a family on.
        if cfg!(not(any(feature = "v47", feature = "v770"))) {
            assert!(compiled_families().is_empty());
            assert!(adapter_for_protocol(47).is_none());
            assert!(supported_protocols().is_empty());
        }
    }

    #[cfg(feature = "v47")]
    #[test]
    fn resolves_v47_when_enabled() {
        let adapter = adapter_for_protocol(47).expect("v47 family compiled in");
        assert!(adapter.supports(47));
        assert!(supported_protocols().contains(&47));
        assert!(compiled_families().contains(&"v47"));
    }

    #[cfg(feature = "v770")]
    #[test]
    fn resolves_v770_when_enabled() {
        let adapter = adapter_for_protocol(776).expect("v770 family compiled in");
        assert!(adapter.supports(776));
        assert!(supported_protocols().contains(&776));
    }

    #[test]
    fn unknown_protocol_resolves_to_none() {
        assert!(adapter_for_protocol(-1).is_none());
    }
}
