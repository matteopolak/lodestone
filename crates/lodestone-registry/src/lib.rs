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
//!
//! # Two directions, one registry
//!
//! [`adapter_for_protocol`] resolves the **clientbound** half (a
//! `VersionAdapter`, for joining a server). [`server_protocol_for_protocol`] is
//! its **serverbound** twin: a `lodestone_server::ServerProtocol`, for *being*
//! the server — which is what singleplayer is (an integrated server on an
//! in-memory duplex) and, over the identical loop, open-to-LAN.
//!
//! Both exist here for the same reason: the shell must not name a version, so
//! the only thing it can hold is a protocol number and the only thing it can get
//! back is a trait object. A version family that has a client adapter but no
//! server protocol simply has no [`SERVER_FAMILIES`] entry, and
//! [`server_protocol_for_protocol`] answers `None` for it.

#![forbid(unsafe_code)]

use lodestone_model::VersionAdapter;

/// Generated protocol/data-version table for GitHub epic #343's sixteen
/// target versions. Use the [`version_table`] module for the public API and
/// provenance docs; this holds only the raw generated data.
#[path = "generated/version_table.rs"]
pub(crate) mod generated_version_table;

pub mod version_table;

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
    #[cfg(feature = "v735")]
    Family {
        label: "v735",
        make: || Box::new(lodestone_v735::adapter()),
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

/// A compiled-in family's **server** side: the thing that lets the integrated
/// server speak this family's wire format.
///
/// Separate from [`Family`] rather than a field on it, because the two sets are
/// not the same: a family can have a `VersionAdapter` (so the client can *join*
/// that version) and no `ServerProtocol` (so we cannot *host* it). Today only
/// `v770` implements the server side, and a fused table would have had to carry
/// an `Option` that is `None` for three of four entries and mean "this family
/// cannot be hosted" — which reads as an oversight rather than a fact.
#[derive(Clone, Copy)]
struct ServerFamily {
    /// Human-readable family label, e.g. `"v770"`. Same value as the matching
    /// [`Family::label`].
    label: &'static str,
    /// Whether this family handles a given protocol number. Delegates to the
    /// family's own `VersionAdapter::supports` rather than restating a protocol
    /// number, so the two directions can never disagree about which versions a
    /// family covers.
    supports: fn(i32) -> bool,
    /// Constructs a boxed server protocol for this family.
    make: fn() -> Box<dyn lodestone_server::ServerProtocol>,
}

impl std::fmt::Debug for ServerFamily {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServerFamily")
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

/// Every version family compiled into this build that can be **served** by
/// `lodestone-server` (singleplayer, and open-to-LAN over the identical loop).
///
/// Gated exactly as [`FAMILIES`] is: deleting a family's folder removes one line
/// here too.
const SERVER_FAMILIES: &[ServerFamily] = &[
    #[cfg(feature = "v770")]
    ServerFamily {
        label: "v770",
        supports: |protocol| lodestone_v770::adapter().supports(protocol),
        make: || Box::new(lodestone_v770::V770ServerProtocol),
    },
];

/// Returns a boxed **server** protocol for `protocol`, if a compiled-in family
/// can be hosted in-process.
///
/// The serverbound twin of [`adapter_for_protocol`], and the seam that makes
/// singleplayer possible without any consumer naming a version: the shell asks
/// for a protocol *number*, gets a trait object, and hands it straight to
/// `lodestone_server::IntegratedServer::open_in_memory`. (`Box<dyn ServerProtocol>`
/// is servable because `lodestone-server` forwards the trait through `Box` — see
/// the impl beside the trait.)
///
/// Returns `None` when no enabled family can be served — a default build with no
/// `vNNN` feature, or a family whose client adapter exists but whose
/// `ServerProtocol` does not. Callers must treat that as "singleplayer is
/// unavailable in this build" and say so, never as an error to route around.
#[must_use]
pub fn server_protocol_for_protocol(
    protocol: i32,
) -> Option<Box<dyn lodestone_server::ServerProtocol>> {
    SERVER_FAMILIES
        .iter()
        .find(|family| (family.supports)(protocol))
        .map(|family| (family.make)())
}

/// Returns the label of every compiled-in family that can be served in-process
/// (for diagnostics — e.g. naming what a build *could* host when a launch is
/// refused).
#[must_use]
pub fn compiled_server_families() -> Vec<&'static str> {
    SERVER_FAMILIES.iter().map(|family| family.label).collect()
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
        if cfg!(not(any(
            feature = "v47",
            feature = "v770",
            feature = "v340",
            feature = "v735"
        ))) {
            assert!(compiled_families().is_empty());
            assert!(adapter_for_protocol(47).is_none());
            assert!(supported_protocols().is_empty());
            // The serverbound twin has the same property, and it is the one the
            // shell's `--no-default-features` build depends on: singleplayer must
            // resolve to `None` and be *reported*, not fail to compile.
            assert!(compiled_server_families().is_empty());
            assert!(server_protocol_for_protocol(776).is_none());
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

    /// The serverbound twin resolves the same family the clientbound one does.
    ///
    /// Asserting both directions agree is the point: `supports` is delegated to
    /// the family's `VersionAdapter`, so a family that can be joined can be
    /// hosted at exactly the same protocol numbers — there is no second, hand
    /// written protocol list to drift.
    ///
    /// That a *joined session* comes out the other end is
    /// `crates/protocol/v770/tests/singleplayer_seam.rs`, which drives this
    /// function's real return value into a real `IntegratedServer`. Nothing here
    /// can see that, because a registry test cannot tell a working protocol from
    /// a boxed one whose methods all fall through to the trait defaults.
    #[cfg(feature = "v770")]
    #[test]
    fn resolves_the_v770_server_protocol_when_enabled() {
        assert!(server_protocol_for_protocol(776).is_some());
        assert!(compiled_server_families().contains(&"v770"));
        // The same protocol the client adapter claims, and nothing else: a
        // number no family supports must be `None` even with v770 compiled in,
        // or `find` is matching unconditionally.
        assert!(server_protocol_for_protocol(776 + 1).is_none());
    }

    #[cfg(feature = "v735")]
    #[test]
    fn resolves_v735_when_enabled() {
        let adapter = adapter_for_protocol(754).expect("v735 family compiled in");
        assert!(adapter.supports(754));
        assert!(supported_protocols().contains(&754));
        assert!(compiled_families().contains(&"v735"));
    }

    #[test]
    fn unknown_protocol_resolves_to_none() {
        assert!(adapter_for_protocol(-1).is_none());
    }
}
