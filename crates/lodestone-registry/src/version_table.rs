//! The version table backing GitHub epic #343: support the latest patch of
//! every major Minecraft release from 1.7.10 through 26.2 — sixteen versions
//! — via one canonical internal version (26.2) plus a per-version
//! translation layer at the network edge, à la ViaVersion. This module is
//! **not** that translation layer; it is the reference data the layer (and
//! the crates that eventually implement it) will be built against: for each
//! of the sixteen versions, the protocol number, the save-format
//! `DataVersion`, the release date, and — critically — exactly where each
//! figure came from.
//!
//! # Why this lives in `lodestone-registry`
//!
//! This crate is already "the single, deliberate aggregation point where
//! Lodestone names concrete protocol version crates" (see the crate-level
//! docs). A table of every version the project has committed to supporting,
//! independent of which families are compiled in yet, belongs next to that
//! aggregation point rather than duplicated per-family or invented ad hoc
//! when the next family is scaffolded.
//!
//! # Provenance rules (see `CLAUDE.md`, "Data sources, in order")
//!
//! 1. **The jar's own `version.json`** (root of the vanilla server jar) is
//!    authoritative when present. It carries `protocol_version` and
//!    `world_version` (the `DataVersion` used in level/chunk NBT — a
//!    different number from the protocol version, and this table keeps them
//!    in separate columns for exactly that reason).
//! 2. **`vendor/minecraft-data`'s `data/pc/common/protocolVersions.json`** is
//!    used only where the jar predates `version.json` — cross-check-grade,
//!    never authoritative. Empirically, in `EPIC_343_VERSIONS`, that boundary
//!    falls **between 1.13.2 and 1.14.4**: 1.13.2's cached server jar has no
//!    `version.json`, 1.14.4's does (protocol 498 / dataVersion 1976). The
//!    file itself documents `version.json`'s introduction as 18w47b, a 1.14
//!    snapshot, which matches.
//! 3. For every version where *both* sources are available (1.14.4 through
//!    26.2, all nine of them, at the time this table was last generated),
//!    they agree exactly — zero disagreements were found. That is measured,
//!    not assumed: `xtask version-table` hard-errors on any (protocol_version,
//!    data_version) disagreement between the jar and `minecraft-data` rather
//!    than silently preferring one, so an agreeing `cross_checked: true` row
//!    is a real cross-check, not a default.
//!
//! # The weakest row: 1.7.10
//!
//! 1.7.10 predates `minecraft-data`'s own per-version directory structure —
//! there is no `vendor/minecraft-data/data/pc/1.7.10/`, only a generic
//! `data/pc/1.7/` aliased to `minecraftVersion: "1.7.10"`. It does, however,
//! have an explicit entry in `protocolVersions.json` (`version: 5,
//! dataVersion: 18`), so it is not *entirely* uncovered — narrower than "no
//! coverage at all," which turned out to be an inaccurate way to describe it
//! (see `docs/version-table.md` for the fuller account). No vanilla jar was
//! fetched for 1.7.10 in generating this table (see below), so its row rests
//! solely on that one community-maintained cross-reference file with no jar
//! to check it against — the least independently attested entry here.
//!
//! # Not every version's jar was fetched
//!
//! `xtask version-table` only inspects a server jar it can find already
//! cached at `.cache/mc/<version>/server.jar`, plus whatever `--fetch-missing`
//! explicitly adds. At the time this table was last generated, jars for
//! 1.7.10, 1.9.4, 1.10.2, and 1.11.2 were not fetched — all four predate
//! 1.13.2, whose jar was fetched and confirmed to still lack `version.json`,
//! so fetching those older four would not have produced any additional
//! protocol/data-version evidence beyond what `minecraft-data` already gives
//! (see `docs/version-table.md` for the exact reasoning and how to fetch them
//! anyway if that changes).
//!
//! # How to refresh
//!
//! ```text
//! cargo run -p xtask -- version-table                  # regenerate
//! cargo run -p xtask -- version-table --check           # drift guard (CI)
//! cargo run -p xtask -- version-table --fetch-missing   # also fetch every
//!                                                        # uncached target
//!                                                        # version's jar
//!                                                        # first (network +
//!                                                        # disk heavy)
//! ```
//!
//! The generator lives in `xtask` (`version_table_report` and friends) since
//! it is the crate that already owns Mojang-manifest fetching, jar
//! downloading, and jar inspection for `fetch-assets`/`fetch-version`.

pub use crate::generated_version_table::{Entry, Source, VERSIONS};

/// Looks up a table row by its exact Mojang version id (e.g. `"1.16.5"`).
#[must_use]
pub fn entry(minecraft_version: &str) -> Option<&'static Entry> {
    VERSIONS
        .iter()
        .find(|entry| entry.minecraft_version == minecraft_version)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact sixteen versions epic #343 named, in release order. Kept
    /// here (independent of `xtask::EPIC_343_VERSIONS`) so a hermetic,
    /// network-free `cargo test -p lodestone-registry` still catches the
    /// table silently losing or reordering a target version.
    const EXPECTED_VERSIONS: [&str; 16] = [
        "1.7.10", "1.8.9", "1.9.4", "1.10.2", "1.11.2", "1.12.2", "1.13.2", "1.14.4", "1.15.2",
        "1.16.5", "1.17.1", "1.18.2", "1.19.4", "1.20.6", "1.21.11", "26.2",
    ];

    #[test]
    fn has_exactly_the_sixteen_epic_versions_in_release_order() {
        let ids: Vec<&str> = VERSIONS.iter().map(|entry| entry.minecraft_version).collect();
        assert_eq!(ids, EXPECTED_VERSIONS);
    }

    #[test]
    fn protocol_and_data_versions_are_strictly_increasing() {
        // Every later release in this list has a strictly higher protocol
        // number and a strictly higher data version than the one before it —
        // true for every Minecraft release to date. A violation here means
        // either a transcription error or two rows swapped.
        for pair in VERSIONS.windows(2) {
            let [previous, next] = pair else {
                unreachable!("windows(2) always yields two-element slices")
            };
            assert!(
                previous.protocol_version < next.protocol_version,
                "{} (protocol {}) should precede {} (protocol {})",
                previous.minecraft_version,
                previous.protocol_version,
                next.minecraft_version,
                next.protocol_version
            );
            assert!(
                previous.data_version < next.data_version,
                "{} (data version {}) should precede {} (data version {})",
                previous.minecraft_version,
                previous.data_version,
                next.minecraft_version,
                next.data_version
            );
        }
    }

    #[test]
    fn jar_sourced_rows_are_exactly_1_14_4_and_later() {
        // Empirically-established boundary (see module docs): 1.13.2's
        // cached jar has no version.json, 1.14.4's does. Everything at or
        // after 1.14.4 in this table was jar-sourced when this table was
        // last generated; everything before was minecraft-data-only. This
        // pins that boundary so a future regen silently losing jar coverage
        // (e.g. a jar going missing from the cache) is visible as a row
        // moving from `JarVersionJson` back to `MinecraftData`.
        let index_of_1_14_4 = VERSIONS
            .iter()
            .position(|entry| entry.minecraft_version == "1.14.4")
            .expect("1.14.4 is in the table");

        for (index, entry) in VERSIONS.iter().enumerate() {
            let expected = if index < index_of_1_14_4 {
                Source::MinecraftData
            } else {
                Source::JarVersionJson
            };
            assert_eq!(
                entry.protocol_source, expected,
                "{}: protocol_source",
                entry.minecraft_version
            );
            assert_eq!(
                entry.data_version_source, expected,
                "{}: data_version_source",
                entry.minecraft_version
            );
        }
    }

    #[test]
    fn cross_checked_iff_jar_sourced() {
        // `cross_checked` should track jar-sourced-ness exactly given the
        // current fetch set: a row can only have been cross-checked against
        // minecraft-data if the jar was consulted at all.
        for entry in &VERSIONS {
            let jar_sourced = entry.protocol_source == Source::JarVersionJson;
            assert_eq!(
                entry.cross_checked, jar_sourced,
                "{}: cross_checked should equal jar-sourced-ness",
                entry.minecraft_version
            );
        }
    }

    #[test]
    fn known_anchor_values_match_what_this_module_documents() {
        // A handful of values transcribed into the module docs above, kept
        // as an executable check so the prose can't drift from the table.
        let v1_7_10 = entry("1.7.10").expect("1.7.10 present");
        assert_eq!(v1_7_10.protocol_version, 5);
        assert_eq!(v1_7_10.data_version, 18);
        assert_eq!(v1_7_10.protocol_source, Source::MinecraftData);

        let v1_14_4 = entry("1.14.4").expect("1.14.4 present");
        assert_eq!(v1_14_4.protocol_version, 498);
        assert_eq!(v1_14_4.data_version, 1976);
        assert_eq!(v1_14_4.protocol_source, Source::JarVersionJson);

        let v26_2 = entry("26.2").expect("26.2 present");
        assert_eq!(v26_2.protocol_version, 776);
        assert_eq!(v26_2.data_version, 4903);
    }

    #[test]
    fn entry_returns_none_for_unknown_version() {
        assert!(entry("1.6.4").is_none());
        assert!(entry("nonsense").is_none());
    }
}
