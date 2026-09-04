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
//! `crates/versions/<version>` folder. To keep that true while still having
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

/// Generated protocol/data-version table for the sixteen target versions. Use
/// the [`version_table`] module for the public API and provenance docs; this
/// holds only the raw generated data.
#[path = "generated/version_table.rs"]
pub(crate) mod generated_version_table;

pub mod version_table;

/// A compiled-in protocol version family.
///
/// # The multi-protocol seam
///
/// `make` takes the **negotiated protocol**, allowing one family to serve every
/// protocol revision listed in its `protocols` slice. Resolution first checks
/// that borrowed coverage list and then constructs exactly one adapter for the
/// selected protocol.
///
/// `protocols` deliberately **points at the family crate's own `PROTOCOLS`
/// const** rather than restating the numbers here — the same reasoning
/// [`ServerFamily::supports`] gives for delegating. A family's coverage has
/// one definition, which its `VersionAdapter::supports` also tests membership
/// in, so this table cannot drift from the adapter it resolves to.
#[derive(Clone, Copy)]
struct Family {
    /// Human-readable family label, e.g. `"v1-8"`.
    label: &'static str,
    /// Every protocol number this family handles. Borrowed from the family
    /// crate's own `PROTOCOLS`; never restated here.
    protocols: &'static [i32],
    /// Constructs a boxed adapter for this family, configured for the
    /// negotiated protocol. Only ever called after [`Self::protocols`] has
    /// confirmed membership.
    make: fn(i32) -> Box<dyn VersionAdapter>,
}

impl std::fmt::Debug for Family {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Family")
            .field("label", &self.label)
            .field("protocols", &self.protocols)
            .finish_non_exhaustive()
    }
}

/// Every version family compiled into this build.
///
/// The list is assembled at compile time from the enabled `vNNN` features; a
/// default build with no family features is empty. Each entry is gated so that
/// deleting a family's folder (and its feature) removes exactly one line here.
const FAMILIES: &[Family] = &[
    #[cfg(feature = "v1-7")]
    Family {
        label: "v1-7",
        protocols: lodestone_v1_7::PROTOCOLS,
        make: |protocol| Box::new(lodestone_v1_7::adapter_for(protocol)),
    },
    #[cfg(feature = "v1-8")]
    Family {
        label: "v1-8",
        protocols: lodestone_v1_8::PROTOCOLS,
        make: |protocol| Box::new(lodestone_v1_8::adapter_for(protocol)),
    },
    #[cfg(feature = "v26-2")]
    Family {
        label: "v26-2",
        // v26-2 is single-protocol (776), so its coverage is spelled from its
        // own `PROTOCOL` const and there is no per-protocol adapter selection.
        // The negotiated number is therefore intentionally discarded by this
        // constructor; the family remains the only `ServerProtocol` provider.
        protocols: &[lodestone_v26_2::PROTOCOL],
        make: |_protocol| Box::new(lodestone_v26_2::adapter()),
    },
    #[cfg(feature = "v1-9")]
    Family {
        label: "v1-9",
        protocols: lodestone_v1_9::PROTOCOLS,
        make: |protocol| Box::new(lodestone_v1_9::adapter_for(protocol)),
    },
    #[cfg(feature = "v1-13")]
    Family {
        label: "v1-13",
        // One protocol, deliberately: 1.13.2 is the flattening boundary and
        // shares under three-quarters of its packet shapes with either
        // neighbour, so it is its own era rather than a member of one.
        protocols: lodestone_v1_13::PROTOCOLS,
        make: |protocol| Box::new(lodestone_v1_13::adapter_for(protocol)),
    },
    #[cfg(feature = "v1-14")]
    Family {
        label: "v1-14",
        // 754, not 735 — the folder name is not the protocol number for this
        // one family. Reading it from the crate rather than typing it is why
        // that cannot be got wrong here.
        protocols: lodestone_v1_14::PROTOCOLS,
        make: |protocol| Box::new(lodestone_v1_14::adapter_for(protocol)),
    },
    #[cfg(feature = "v1-17")]
    Family {
        label: "v1-17",
        // 756 and 758 -- 1.17.1 and 1.18.2. 755 is 1.17, which this era does
        // not serve; both numbers are read off each jar's own metadata and
        // then off the crate, never off the folder name.
        protocols: lodestone_v1_17::PROTOCOLS,
        make: |protocol| Box::new(lodestone_v1_17::adapter_for(protocol)),
    },
    #[cfg(feature = "v1-19")]
    Family {
        label: "v1-19",
        // 762 -- 1.19.4, read off the jar's own metadata and then off the
        // crate. The other 1.19.x releases carry different numbers and a
        // different chat shape; none is served here.
        protocols: lodestone_v1_19::PROTOCOLS,
        make: |protocol| Box::new(lodestone_v1_19::adapter_for(protocol)),
    },
    #[cfg(feature = "v1-20-6")]
    Family {
        label: "v1-20-6",
        // 766 -- Minecraft 1.20.5 and 1.20.6, one wire version for two
        // releases. Read off the jar's own metadata and then off the crate,
        // never off the folder name.
        protocols: lodestone_v1_20_6::PROTOCOLS,
        make: |protocol| Box::new(lodestone_v1_20_6::adapter_for(protocol)),
    },
    #[cfg(feature = "v1-21-11")]
    Family {
        label: "v1-21-11",
        // 774 -- Minecraft 1.21.11 alone. Read off the jar's own metadata and
        // then off the crate, never off the folder name: the neighbouring
        // releases carry their own numbers.
        protocols: lodestone_v1_21_11::PROTOCOLS,
        make: |protocol| Box::new(lodestone_v1_21_11::adapter_for(protocol)),
    },
];

/// Resolves `protocol` against an arbitrary family table.
///
/// [`adapter_for_protocol`] is this applied to [`FAMILIES`]. It is factored out
/// so the tests can drive **the production resolution path** with a table of
/// their own — including a genuinely multi-protocol family, which no compiled-in
/// family is yet. Testing dispatch against a table the test supplies is the
/// only way to gate the seam before the first grouped family exists; asserting
/// against `FAMILIES` alone could not distinguish "carries the negotiated
/// protocol" from "ignores it", because every real family has exactly one.
fn resolve_adapter(families: &[Family], protocol: i32) -> Option<Box<dyn VersionAdapter>> {
    families
        .iter()
        .find(|family| family.protocols.contains(&protocol))
        .map(|family| (family.make)(protocol))
}

/// Returns a boxed adapter for `protocol`, if a compiled-in family supports it.
///
/// The adapter is **constructed for that protocol** (see [`Family`]), which is
/// what lets one family crate serve a whole wire era rather than one revision.
///
/// Returns `None` when no enabled family handles that protocol number — for
/// example in a default build with no `vNNN` feature enabled.
#[must_use]
pub fn adapter_for_protocol(protocol: i32) -> Option<Box<dyn VersionAdapter>> {
    resolve_adapter(FAMILIES, protocol)
}

/// One compiled-in family's entry in the protocol → [`lodestone_physics::PhysicsProfile`]
/// mapping. See [`physics_profile_for_protocol`] for the family → profile
/// table and the fidelity limits of the available profiles.
#[derive(Clone, Copy)]
struct PhysicsFamily {
    /// Same list [`Family::protocols`] uses for this family — borrowed from the
    /// family crate's own `PROTOCOLS`, never restated, so this table cannot
    /// disagree with [`adapter_for_protocol`] about which protocol numbers
    /// belong to which family.
    protocols: &'static [i32],
    /// Which profile this family gets. A `fn` pointer rather than a stored
    /// value because [`lodestone_physics::PhysicsProfile`] is not `const`-
    /// constructible from a `static` initializer in a `&[PhysicsFamily]` (its
    /// constructors are inherent `const fn`s, not a `const` item), and a `fn`
    /// pointer keeps this table exactly the same shape as [`Family::make`].
    profile: fn() -> lodestone_physics::PhysicsProfile,
}

/// Only two [`lodestone_physics::PhysicsProfile`]s exist
/// (`mc_1_8`/`mc_1_21`) for the **ten** client families. This table is the one
/// place that says which profile each family gets. `v1-8`, `v1-21-11`, and
/// `v26-2` are exact matches; the other seven families use the closer
/// available profile with explicitly documented fidelity limits:
///
/// - **`v1-8` (1.8.9) → `mc_1_8`.** Exact family match for the movement rules
///   represented by this profile.
/// - **`v1-7` (1.7.6-1.7.10) → `mc_1_8`.** Not an exact match, but the
///   structurally right half of the choice: protocol 5 pre-dates the 1.9
///   input-pipeline rewrite, so `mc_1_8`'s input model is the algorithm this
///   era actually ran, where `mc_1_21`'s would be the wrong one on every tick.
///   Its constants are 1.8's rather than 1.7's and are not validated for this
///   era, so it is the pre-1.9 profile with explicitly limited fidelity.
/// - **`v26-2` (26.2) → `mc_1_21`.** Exact family match. This is the profile
///   used by the current production construction sites for this family.
/// - **`v1-21-11` (1.21.11) → `mc_1_21`.** Exact family match.
/// - **`v1-9` (1.9.4-1.12.2), `v1-13` (1.13.2), `v1-14` (1.14.4-1.16.5) and
///   `v1-17` (1.17.1-1.18.2), `v1-19` (1.19.4), `v1-20-6` (1.20.5-1.20.6) →
///   `mc_1_21`, as an approximation, not a validated fit. None of these six
///   later families is a clean match for either profile: all post-date the 1.9
///   input-pipeline rewrite ([`InputModel::UnitSquareProjection`], which
///   `mc_1_21` selects and `mc_1_8` does not), so `mc_1_8` would run the
///   *wrong* structural input algorithm for any of them, not merely an
///   imprecise one. Their fluid behavior is not validated against
///   [`FluidModel::Modern`]'s exact constants. `mc_1_21` is still the nearer pick:
///   the input model
///   is live on *every* tick a player takes, while the fluid model only
///   diverges while actually in a fluid, so getting the input pipeline
///   structurally right dominates. Treat movement through
///   v1-9/v1-13/v1-14/v1-17/v1-19/v1-20-6 as "the modern profile, unvalidated
///   for this era" — not as bit-exact parity. An era-specific profile would be
///   required for bit-exact fidelity.
const PHYSICS_FAMILIES: &[PhysicsFamily] = &[
    #[cfg(feature = "v1-7")]
    PhysicsFamily {
        protocols: lodestone_v1_7::PROTOCOLS,
        profile: lodestone_physics::PhysicsProfile::mc_1_8,
    },
    #[cfg(feature = "v1-8")]
    PhysicsFamily {
        protocols: lodestone_v1_8::PROTOCOLS,
        profile: lodestone_physics::PhysicsProfile::mc_1_8,
    },
    #[cfg(feature = "v26-2")]
    PhysicsFamily {
        protocols: &[lodestone_v26_2::PROTOCOL],
        profile: lodestone_physics::PhysicsProfile::mc_1_21,
    },
    #[cfg(feature = "v1-9")]
    PhysicsFamily {
        protocols: lodestone_v1_9::PROTOCOLS,
        profile: lodestone_physics::PhysicsProfile::mc_1_21,
    },
    #[cfg(feature = "v1-13")]
    PhysicsFamily {
        protocols: lodestone_v1_13::PROTOCOLS,
        profile: lodestone_physics::PhysicsProfile::mc_1_21,
    },
    #[cfg(feature = "v1-14")]
    PhysicsFamily {
        protocols: lodestone_v1_14::PROTOCOLS,
        profile: lodestone_physics::PhysicsProfile::mc_1_21,
    },
    #[cfg(feature = "v1-17")]
    PhysicsFamily {
        protocols: lodestone_v1_17::PROTOCOLS,
        profile: lodestone_physics::PhysicsProfile::mc_1_21,
    },
    #[cfg(feature = "v1-19")]
    PhysicsFamily {
        protocols: lodestone_v1_19::PROTOCOLS,
        profile: lodestone_physics::PhysicsProfile::mc_1_21,
    },
    #[cfg(feature = "v1-20-6")]
    PhysicsFamily {
        protocols: lodestone_v1_20_6::PROTOCOLS,
        profile: lodestone_physics::PhysicsProfile::mc_1_21,
    },
    #[cfg(feature = "v1-21-11")]
    PhysicsFamily {
        protocols: lodestone_v1_21_11::PROTOCOLS,
        profile: lodestone_physics::PhysicsProfile::mc_1_21,
    },
];

/// Resolves `protocol` to the [`lodestone_physics::PhysicsProfile`] its family
/// gets, per [`PHYSICS_FAMILIES`]'s table and doc comment.
///
/// Unlike [`adapter_for_protocol`] this never returns `None`: a session always
/// needs *some* physics profile — the offline demo world included, which has
/// no live adapter at all but still simulates a player — so an unrecognised or
/// unresolvable protocol number (including every number in a
/// `--no-default-features` build, where [`PHYSICS_FAMILIES`] is empty) falls
/// back to `mc_1_21`, the profile used by the current production fallback.
/// Threading a real protocol number through this lookup preserves that fallback
/// for unknown protocols while selecting a family-specific profile when one is
/// available.
#[must_use]
pub fn physics_profile_for_protocol(protocol: i32) -> lodestone_physics::PhysicsProfile {
    PHYSICS_FAMILIES
        .iter()
        .find(|family| family.protocols.contains(&protocol))
        .map_or_else(lodestone_physics::PhysicsProfile::mc_1_21, |family| {
            (family.profile)()
        })
}

/// A compiled-in family's **server** side: the thing that lets the integrated
/// server speak this family's wire format.
///
/// Separate from [`Family`] rather than a field on it, because the two sets are
/// not the same: a family can have a `VersionAdapter` (so the client can *join*
/// that version) and no `ServerProtocol` (so we cannot *host* it). A fused table
/// would carry an `Option` whose `None` means "this family cannot be hosted".
/// The independent tables make that join/host distinction explicit.
#[derive(Clone, Copy)]
struct ServerFamily {
    /// Human-readable family label, e.g. `"v26-2"`. Same value as the matching
    /// [`Family::label`].
    label: &'static str,
    /// Whether this host implements a given protocol number. A family may host
    /// fewer revisions than its joining adapter supports.
    supports: fn(i32) -> bool,
    /// Constructs a boxed server protocol for the negotiated protocol.
    make: fn(i32) -> Box<dyn lodestone_server::ServerProtocol>,
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
    #[cfg(feature = "v1-7")]
    ServerFamily {
        label: "v1-7",
        supports: |protocol| protocol == lodestone_v1_7::PROTOCOL,
        make: |_| Box::new(lodestone_v1_7::V5ServerProtocol),
    },
    #[cfg(feature = "v1-8")]
    ServerFamily {
        label: "v1-8",
        supports: |protocol| protocol == lodestone_v1_8::PROTOCOL,
        make: |_| Box::new(lodestone_v1_8::V47ServerProtocol),
    },
    #[cfg(feature = "v26-2")]
    ServerFamily {
        label: "v26-2",
        supports: |protocol| lodestone_v26_2::adapter().supports(protocol),
        make: |_| Box::new(lodestone_v26_2::V770ServerProtocol),
    },
    #[cfg(feature = "v1-9")]
    ServerFamily {
        label: "v1-9",
        // The two host implementations have distinct packet-id tables. The
        // family adapter covers earlier revisions too, but their server packet
        // layouts have not been implemented.
        supports: |protocol| protocol == lodestone_v1_9::PROTOCOL || protocol == 316,
        make: |protocol| match protocol {
            316 => Box::new(lodestone_v1_9::V316ServerProtocol),
            _ => Box::new(lodestone_v1_9::V340ServerProtocol),
        },
    },
    #[cfg(feature = "v1-13")]
    ServerFamily {
        label: "v1-13",
        supports: |protocol| protocol == lodestone_v1_13::PROTOCOL,
        make: |_| Box::new(lodestone_v1_13::V404ServerProtocol),
    },
    #[cfg(feature = "v1-14")]
    ServerFamily {
        label: "v1-14",
        // Hosting has separate implementations for all three packet layouts;
        // each selector owns its packet ids, join shape and chunk framing.
        supports: |protocol| {
            protocol == lodestone_v1_14::PROTOCOL_1_14_4
                || protocol == lodestone_v1_14::PROTOCOL_1_15_2
                || protocol == lodestone_v1_14::PROTOCOL_1_16_5
        },
        make: |protocol| match protocol {
            lodestone_v1_14::PROTOCOL_1_14_4 => Box::new(lodestone_v1_14::V498ServerProtocol),
            lodestone_v1_14::PROTOCOL_1_15_2 => Box::new(lodestone_v1_14::V578ServerProtocol),
            lodestone_v1_14::PROTOCOL_1_16_5 => Box::new(lodestone_v1_14::V754ServerProtocol),
            _ => unreachable!("server family checked protocol before construction"),
        },
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
        .map(|family| (family.make)(protocol))
}

/// Returns the label of every compiled-in family that can be served in-process
/// (for diagnostics — e.g. naming what a build *could* host when a launch is
/// refused).
#[must_use]
pub fn compiled_server_families() -> Vec<&'static str> {
    SERVER_FAMILIES.iter().map(|family| family.label).collect()
}

/// Returns every protocol number any compiled-in family handles.
///
/// Returns the union of each compiled family's protocol coverage. The values
/// are read straight from [`Family::protocols`], so this requires no adapter
/// construction and cannot disagree with what [`adapter_for_protocol`] resolves.
#[must_use]
pub fn supported_protocols() -> Vec<i32> {
    FAMILIES
        .iter()
        .flat_map(|family| family.protocols.iter().copied())
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
            feature = "v1-7",
            feature = "v1-8",
            feature = "v26-2",
            feature = "v1-9",
            feature = "v1-13",
            feature = "v1-14",
            feature = "v1-17",
            feature = "v1-19",
            feature = "v1-20-6",
            feature = "v1-21-11"
        ))) {
            assert!(compiled_families().is_empty());
            assert!(adapter_for_protocol(47).is_none());
            assert!(supported_protocols().is_empty());
            // The serverbound twin has the same property, and it is the one the
            // shell's `--no-default-features` build depends on: singleplayer must
            // resolve to `None` and be *reported*, not fail to compile.
            assert!(compiled_server_families().is_empty());
            assert!(server_protocol_for_protocol(776).is_none());
            // No family compiled means `physics_profile_for_protocol` has
            // nothing to match, so it must fall back to `mc_1_21` — never
            // `None`, and never a panic. This is also what
            // `cargo check -p lodestone-shell --no-default-features` depends
            // on staying true.
            assert_eq!(
                physics_profile_for_protocol(776),
                lodestone_physics::PhysicsProfile::mc_1_21()
            );
        }
    }

    #[cfg(feature = "v1-8")]
    #[test]
    fn resolves_v47_when_enabled() {
        let adapter = adapter_for_protocol(47).expect("v1-8 family compiled in");
        assert!(adapter.supports(47));
        assert!(supported_protocols().contains(&47));
        assert!(compiled_families().contains(&"v1-8"));
    }

    #[cfg(feature = "v1-7")]
    #[test]
    fn resolves_only_protocol_5_for_the_legacy_server_family() {
        assert!(server_protocol_for_protocol(5).is_some());
        assert!(compiled_server_families().contains(&"v1-7"));
        assert!(server_protocol_for_protocol(4).is_none());
        assert!(server_protocol_for_protocol(6).is_none());
    }

    #[cfg(feature = "v1-8")]
    #[test]
    fn resolves_only_protocol_47_for_the_legacy_server_family() {
        assert!(server_protocol_for_protocol(47).is_some());
        assert!(compiled_server_families().contains(&"v1-8"));
        assert!(server_protocol_for_protocol(46).is_none());
        assert!(server_protocol_for_protocol(48).is_none());
    }

    /// `v1-8` is 1.8.9, the exact-match family for [`PhysicsProfile`] `mc_1_8`.
    /// `v1-7` also maps there because its pre-1.9 input model is structurally
    /// closer; `v1-21-11` and `v26-2` are exact modern-profile families, while
    /// the remaining later families use that profile as an approximation.
    #[cfg(feature = "v1-8")]
    #[test]
    fn v47_maps_to_the_1_8_physics_profile() {
        assert_eq!(
            physics_profile_for_protocol(47),
            lodestone_physics::PhysicsProfile::mc_1_8()
        );
    }

    #[cfg(feature = "v26-2")]
    #[test]
    fn resolves_v770_when_enabled() {
        let adapter = adapter_for_protocol(776).expect("v26-2 family compiled in");
        assert!(adapter.supports(776));
        assert!(supported_protocols().contains(&776));
    }

    /// `v26-2` (26.2) is an exact-match family using the modern profile.
    #[cfg(feature = "v26-2")]
    #[test]
    fn v770_maps_to_the_modern_physics_profile() {
        assert_eq!(
            physics_profile_for_protocol(776),
            lodestone_physics::PhysicsProfile::mc_1_21()
        );
    }

    /// The serverbound twin resolves the currently hostable family for the
    /// same protocol that the clientbound table accepts. The clientbound
    /// [`FAMILIES`] table also contains join-only families with no server
    /// implementation, so the two tables intentionally need not agree for
    /// every accepted client protocol.
    ///
    /// The hostable entry delegates `supports` to its `VersionAdapter`, so its
    /// client and server protocol coverage cannot drift. This assertion checks
    /// that shared coverage while also checking that unsupported numbers remain
    /// rejected.
    ///
    /// That a *joined session* comes out the other end is
    /// `crates/versions/26.2/tests/singleplayer_seam.rs`, which drives this
    /// function's real return value into a real `IntegratedServer`. Nothing here
    /// can see that, because a registry test cannot tell a working protocol from
    /// a boxed one whose methods all fall through to the trait defaults.
    #[cfg(feature = "v26-2")]
    #[test]
    fn resolves_the_v770_server_protocol_when_enabled() {
        assert!(server_protocol_for_protocol(776).is_some());
        assert!(compiled_server_families().contains(&"v26-2"));
        // The same protocol the client adapter claims, and nothing else: a
        // number no family supports must be `None` even with v26-2 compiled in,
        // or `find` is matching unconditionally.
        assert!(server_protocol_for_protocol(776 + 1).is_none());
    }

    #[cfg(feature = "v1-9")]
    #[test]
    fn resolves_the_hosted_1_9_family_protocols() {
        assert!(server_protocol_for_protocol(340).is_some());
        assert!(compiled_server_families().contains(&"v1-9"));
        assert!(server_protocol_for_protocol(316).is_some());
        assert!(server_protocol_for_protocol(315).is_none());
        assert!(server_protocol_for_protocol(341).is_none());
    }

    #[cfg(feature = "v1-13")]
    #[test]
    fn resolves_only_protocol_404_for_the_flattened_server_family() {
        assert!(server_protocol_for_protocol(404).is_some());
        assert!(compiled_server_families().contains(&"v1-13"));
        assert!(server_protocol_for_protocol(403).is_none());
        assert!(server_protocol_for_protocol(405).is_none());
    }

    #[cfg(feature = "v1-14")]
    #[test]
    fn resolves_v735_when_enabled() {
        let adapter = adapter_for_protocol(754).expect("v1-14 family compiled in");
        assert!(adapter.supports(754));
        assert!(supported_protocols().contains(&754));
        assert!(compiled_families().contains(&"v1-14"));
    }

    #[cfg(feature = "v1-14")]
    #[test]
    fn resolves_all_three_hosted_protocols_for_the_1_14_family() {
        assert!(server_protocol_for_protocol(498).is_some());
        assert!(server_protocol_for_protocol(578).is_some());
        assert!(server_protocol_for_protocol(754).is_some());
        assert!(compiled_server_families().contains(&"v1-14"));
        assert!(server_protocol_for_protocol(497).is_none());
    }

    /// `v1-14` speaks protocol 754 (1.16.5) — the folder name is not the
    /// protocol number, so this resolves through the real adapter's number,
    /// never the crate's. 1.16.5 gets `mc_1_21` as the nearer of the two
    /// available profiles, **not** as a validated fit — see
    /// [`PHYSICS_FAMILIES`]'s doc comment for what is and is not actually
    /// checked here.
    #[cfg(feature = "v1-14")]
    #[test]
    fn v735_maps_to_the_modern_physics_profile_as_an_approximation() {
        assert_eq!(
            physics_profile_for_protocol(754),
            lodestone_physics::PhysicsProfile::mc_1_21()
        );
    }

    /// `v1-9` (1.12.2) gets the same approximate mapping as `v1-14`, for the
    /// same reason: post-1.9 input model, pre-Update-Aquatic fluids, and
    /// neither of the two available profiles is a validated fit — see
    /// [`PHYSICS_FAMILIES`]'s doc comment.
    #[cfg(feature = "v1-9")]
    #[test]
    fn v340_maps_to_the_modern_physics_profile_as_an_approximation() {
        let adapter = adapter_for_protocol(340).expect("v1-9 family compiled in");
        assert!(adapter.supports(340));
        assert_eq!(
            physics_profile_for_protocol(340),
            lodestone_physics::PhysicsProfile::mc_1_21()
        );
    }

    #[test]
    fn unknown_protocol_resolves_to_none() {
        assert!(adapter_for_protocol(-1).is_none());
    }

    /// An unrecognised protocol number must still return a usable profile —
    /// never `None`, never a panic — because a session (including the
    /// offline demo world, which has no live adapter at all) always needs
    /// *some* [`PhysicsProfile`], and the safe fallback is the same constant
    /// used by the production fallback.
    #[test]
    fn unknown_protocol_falls_back_to_the_modern_physics_profile() {
        assert_eq!(
            physics_profile_for_protocol(-1),
            lodestone_physics::PhysicsProfile::mc_1_21()
        );
    }

    /// Every compiled-in family's registry entry agrees with the family's own
    /// `VersionAdapter::supports`, in both directions.
    ///
    /// This is the drift guard that makes [`Family::protocols`] safe to consult
    /// *instead of* constructing an adapter and asking it. Without it the
    /// registry would hold a second, independent protocol list — exactly the
    /// duplication [`ServerFamily::supports`] delegates to avoid.
    ///
    /// The negative half is load-bearing: a family must answer `false` for
    /// `protocol + 1`. A `supports` that returned `true` unconditionally would
    /// pass the positive half alone.
    #[test]
    fn every_family_entry_agrees_with_its_own_adapter() {
        for family in FAMILIES {
            assert!(
                !family.protocols.is_empty(),
                "{} declares no protocols at all, so it can never be resolved",
                family.label
            );
            for &protocol in family.protocols {
                let adapter = (family.make)(protocol);
                assert!(
                    adapter.supports(protocol),
                    "{} is registered for protocol {protocol} but its own adapter \
                     denies supporting it",
                    family.label
                );
                if !family.protocols.contains(&(protocol + 1)) {
                    assert!(
                        !adapter.supports(protocol + 1),
                        "{}'s adapter claims to support {}, which is not in its \
                         PROTOCOLS — `supports` is matching too broadly",
                        family.label,
                        protocol + 1
                    );
                }
            }
        }
    }

    /// A family that really does cover two protocols, used to gate the seam
    /// itself. No compiled-in family is multi-protocol yet, so nothing else in
    /// this crate can tell an adapter that *carries* the negotiated protocol
    /// from one that ignores it.
    ///
    /// It is a fake **adapter**, but the dispatch under test is the real
    /// [`resolve_adapter`] — the same function [`adapter_for_protocol`] is.
    #[derive(Debug)]
    struct TwoProtocolFake {
        negotiated: i32,
    }

    /// The lower protocol of the fake's era.
    const FAKE_A: i32 = 900;
    /// The upper protocol of the fake's era.
    const FAKE_B: i32 = 901;
    /// `SendChat`'s serverbound packet id in protocol [`FAKE_A`]'s table.
    const FAKE_A_CHAT_ID: i32 = 0x03;
    /// `SendChat`'s serverbound packet id in protocol [`FAKE_B`]'s table —
    /// renumbered, which is the whole reason a grouped family needs to know
    /// which protocol it was built for.
    const FAKE_B_CHAT_ID: i32 = 0x0F;

    impl TwoProtocolFake {
        /// Stands in for a grouped family's per-protocol `packet_ids` module
        /// selection.
        fn chat_packet_id(&self) -> i32 {
            match self.negotiated {
                FAKE_A => FAKE_A_CHAT_ID,
                FAKE_B => FAKE_B_CHAT_ID,
                other => panic!("built for unsupported protocol {other}"),
            }
        }
    }

    impl VersionAdapter for TwoProtocolFake {
        fn protocol_version(&self) -> i32 {
            self.negotiated
        }

        fn minecraft_versions(&self) -> &'static [&'static str] {
            &["fake-a", "fake-b"]
        }

        fn supports(&self, protocol: i32) -> bool {
            protocol == FAKE_A || protocol == FAKE_B
        }

        fn begin_login(
            &self,
            _profile: &lodestone_model::LoginProfile,
            _server: &lodestone_model::ServerAddress,
        ) -> Result<Vec<lodestone_model::Directive>, lodestone_model::AdapterError> {
            Ok(Vec::new())
        }

        fn handle_packet(
            &self,
            _world: &mut dyn lodestone_model::WorldSink,
            _state: lodestone_model::ConnectionState,
            _packet_id: i32,
            _payload: &[u8],
        ) -> Result<Vec<lodestone_model::Directive>, lodestone_model::AdapterError> {
            Ok(Vec::new())
        }

        fn encode_action(
            &self,
            _state: lodestone_model::ConnectionState,
            _action: &lodestone_model::ClientAction,
        ) -> Result<Option<(i32, Vec<u8>)>, lodestone_model::AdapterError> {
            Ok(Some((self.chat_packet_id(), Vec::new())))
        }
    }

    const FAKE_FAMILIES: &[Family] = &[Family {
        label: "fake-two-protocol",
        protocols: &[FAKE_A, FAKE_B],
        make: |protocol| Box::new(TwoProtocolFake { negotiated: protocol }),
    }];

    /// Asks the resolved adapter which packet id it would use for chat — the
    /// observable that distinguishes one protocol's table from the other's.
    fn resolved_chat_id(protocol: i32) -> Option<i32> {
        let adapter = resolve_adapter(FAKE_FAMILIES, protocol)?;
        let action = lodestone_model::ClientAction::SendChat { text: String::new() };
        match adapter.encode_action(lodestone_model::ConnectionState::Play, &action) {
            Ok(Some((packet_id, _))) => Some(packet_id),
            other => panic!("fake adapter did not encode chat: {other:?}"),
        }
    }

    /// **The seam's gate.** A family constructed for protocol A must select A's
    /// table even though B is also in its set, and vice versa.
    #[test]
    fn a_grouped_family_selects_the_table_of_the_protocol_it_was_built_for() {
        // The control that stops this being vacuous: if the two protocols'
        // tables agreed on this packet, the assertions below would pass for a
        // family that ignored the negotiated protocol entirely, and the pair
        // would have to be replaced with one that actually renumbers.
        assert_ne!(
            FAKE_A_CHAT_ID, FAKE_B_CHAT_ID,
            "the two protocols agree on this packet id, so it cannot separate \
             the two tables and this gate proves nothing"
        );

        assert_eq!(resolved_chat_id(FAKE_A), Some(FAKE_A_CHAT_ID));
        assert_eq!(resolved_chat_id(FAKE_B), Some(FAKE_B_CHAT_ID));

        // And the negotiated number itself reaches the adapter, not just the
        // table derived from it.
        let adapter = resolve_adapter(FAKE_FAMILIES, FAKE_B).expect("fake family resolves");
        assert_eq!(adapter.protocol_version(), FAKE_B);
    }

    /// The negative control for the resolution half: a protocol adjacent to the
    /// fake's set must resolve to nothing. Without this, a `find` that matched
    /// unconditionally would satisfy every assertion above.
    #[test]
    fn a_protocol_outside_a_grouped_family_resolves_to_none() {
        assert!(resolve_adapter(FAKE_FAMILIES, FAKE_A - 1).is_none());
        assert!(resolve_adapter(FAKE_FAMILIES, FAKE_B + 1).is_none());
        assert!(resolve_adapter(&[], FAKE_A).is_none());
    }
}
