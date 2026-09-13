//! Property: a truncated prefix of a valid packet must error cleanly, never
//! panic. This is the exact real failure mode `CLAUDE.md` names: hermetic
//! chunk fixtures round-tripped fine against our own encoder for months,
//! then a live server produced 49 × "unexpected end of input" — a clean
//! `Err`, not a crash, but only because the reader primitives happened to be
//! bounds-checked. This test makes that property explicit and checks it
//! against real bytes, not just our own encoder's idea of a valid packet.
//!
//! ## Corpus provenance (read this before trusting a green run)
//!
//! Per CLAUDE.md's evidence rule ("an expected value must originate outside
//! the code under test"), corpus entries are ranked:
//!
//! - **Strong**: `crates/versions/26.2/tests/fixtures/*.hex` — real bytes
//!   captured from a live vanilla 26.2 server, already checked in and used
//!   by that crate's own hermetic tests (`registry_data`, `item_entity_metadata`,
//!   `item_components`). Six fixtures, covering `registry_data`,
//!   `set_entity_data`, and `container_set_slot`.
//! - **Weak, explicitly marked as such**: for every family (including v26-2's
//!   own remaining packet types with no captured fixture), a payload built
//!   by that family's *own* `V*Adapter::begin_login`. This is self-encoded —
//!   it proves nothing about whether we understood the protocol correctly,
//!   only that truncating *something structurally packet-shaped* doesn't
//!   panic. `v1-8`/`v1-9`/`v1-14` have no captured-fixture corpus at all today
//!   (JVM-oracle capture was never added for the legacy families);
//!   that gap is real and is called out in `docs/fuzz-harness.md`.

// The corpus is built from `lodestone_v26_2`'s own packet-id tables, so this file
// exists only in a build that compiles that family in. On by default; see the
// crate manifest's `[features]`.
#![cfg(feature = "v26-2")]

use lodestone_fuzz::{Family, catch};
use lodestone_model::ConnectionState;

struct CorpusEntry {
    family: Family,
    state: ConnectionState,
    packet_id: i32,
    bytes: Vec<u8>,
    source: &'static str,
}

fn strong_corpus() -> Vec<CorpusEntry> {
    let fixtures: &[(&str, i32)] = &[
        (
            "registry_data_dimension_type.hex",
            lodestone_v26_2::packet_ids::configuration::clientbound::REGISTRY_DATA,
        ),
        (
            "registry_data_world_clock.hex",
            lodestone_v26_2::packet_ids::configuration::clientbound::REGISTRY_DATA,
        ),
        (
            "item_entity_metadata_diamond.hex",
            lodestone_v26_2::packet_ids::play::clientbound::SET_ENTITY_DATA,
        ),
        (
            "item_entity_metadata_unmodeled_component.hex",
            lodestone_v26_2::packet_ids::play::clientbound::SET_ENTITY_DATA,
        ),
        (
            "tool_component_absent_plain_pickaxe.hex",
            lodestone_v26_2::packet_ids::play::clientbound::CONTAINER_SET_SLOT,
        ),
        (
            "tool_component_explicit.hex",
            lodestone_v26_2::packet_ids::play::clientbound::CONTAINER_SET_SLOT,
        ),
    ];

    fixtures
        .iter()
        .map(|(name, packet_id)| {
            let path = lodestone_fuzz::v26_2_fixture_path(name);
            let bytes = lodestone_fuzz::read_hex_fixture(&path);
            let state = if name.starts_with("registry_data") {
                ConnectionState::Configuration
            } else {
                ConnectionState::Play
            };
            CorpusEntry {
                family: Family::V770,
                state,
                packet_id: *packet_id,
                bytes,
                source: "captured vanilla 26.2 server bytes (tests/fixtures/*.hex)",
            }
        })
        .collect()
}

/// Weak fallback corpus: the serverbound handshake/login bytes each
/// adapter's own `begin_login` emits. **Self-encoded — this is the weaker
/// source CLAUDE.md's evidence rule warns about**, kept only so every family
/// has at least one structurally-real packet to truncate when no captured
/// fixture exists for it. `begin_login` directives are serverbound (what our
/// client sends), so this exercises truncation of *our own* encoder's output
/// rather than a captured clientbound packet — even weaker than a normal
/// self-round-trip, and named as such deliberately.
fn weak_self_encoded_corpus() -> Vec<CorpusEntry> {
    use lodestone_model::{Directive, LoginProfile, ServerAddress};

    let profile = LoginProfile {
        username: "FuzzUser".to_owned(),
        uuid: uuid::Uuid::from_u128(0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10),
    };
    let server = ServerAddress {
        host: "localhost".to_owned(),
        port: 25565,
    };

    let mut out = Vec::new();
    for &family in Family::ALL {
        let adapter = family.adapter();
        let directives = match adapter.begin_login(&profile, &server) {
            Ok(d) => d,
            Err(_) => continue,
        };
        for directive in directives {
            if let Directive::Send { packet_id, payload } = directive {
                out.push(CorpusEntry {
                    family,
                    // begin_login's packets span Handshaking/Login; Play is a
                    // deliberately-wrong state for some of them, which is
                    // fine — decoding our own login bytes as a Play packet
                    // must still not panic, just error.
                    state: ConnectionState::Login,
                    packet_id,
                    bytes: payload,
                    source: "self-encoded via begin_login (weak: not external evidence)",
                });
            }
        }
    }
    out
}

#[test]
fn truncating_every_corpus_entry_never_panics() {
    let mut entries = strong_corpus();
    entries.extend(weak_self_encoded_corpus());
    assert!(!entries.is_empty(), "corpus must not be empty, or this test proves nothing");

    let mut prefixes_checked = 0usize;
    for entry in &entries {
        for len in 0..=entry.bytes.len() {
            let prefix = &entry.bytes[..len];
            let result = catch(|| {
                lodestone_fuzz::decode_clientbound(entry.family, entry.state, entry.packet_id, prefix)
            });
            assert!(
                result.is_ok(),
                "{}: truncating source {} (packet_id {}) to {len}/{} bytes panicked: {}",
                entry.family.name(),
                entry.source,
                entry.packet_id,
                entry.bytes.len(),
                result.unwrap_err(),
            );
            prefixes_checked += 1;
        }
    }

    assert!(
        prefixes_checked > 100,
        "expected well over 100 truncation prefixes across the corpus, got {prefixes_checked}"
    );
}
