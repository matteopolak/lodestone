//! The 27 Configuration-phase `registry_data` payloads this crate
//! has no typed model for, plus `update_tags` and `select_known_packs`.
//!
//! `server_protocol.rs` already sends `minecraft:dimension_type` and
//! `minecraft:world_clock` from hand-built tables
//! ([`crate::server_protocol`]'s `DIMENSION_TYPE_REGISTRY`/
//! `WORLD_CLOCK_OVERWORLD_NBT`), because this crate resolves both registries'
//! *holder ids* elsewhere (`login`'s dimension type, `set_time`'s clock keys)
//! and needs them as structured per-entry tables, not opaque bytes.
//!
//! Every other synchronized registry (`minecraft:worldgen/biome`,
//! `minecraft:enchantment`, `minecraft:banner_pattern`, …) is sent nowhere
//! else in this crate and nothing here needs to read an entry back out of it
//! — a real vanilla client just needs *a* copy of the registry that agrees
//! with itself, to resolve `Holder`/tag references inside data components it
//! already decodes. So these are relayed **verbatim**: the exact
//! `registry_data` packet payload — including the registry name, entry
//! count, and every entry's NBT — that a real vanilla 26.2 server sent on the
//! flat creative oracle, captured by
//! `tests/live_registry_data_full_set.rs` into
//! `tests/fixtures/registry_data_*.hex` and embedded here via
//! [`include_str!`]. A real client reads bytes it already knows how to
//! parse, rather than a re-encoding of our own understanding of ~20
//! data-driven registries (`decode(encode(x)) == x` is worthless here —
//! `CLAUDE.md`, evidence standards).
//!
//! `minecraft:update_tags` is the same story: `tests/fixtures/
//! update_tags_configuration.hex` is one real server's whole `update_tags`
//! payload (every registry's tag groups, block/item/entity_type/fluid/…),
//! relayed unmodified.
//!
//! # Why this is safe to replay across servers/sessions
//!
//! These are **not** per-connection or per-world data — vanilla's default
//! data pack ships identical `minecraft:enchantment`, `minecraft:biome`, tag
//! definitions etc. regardless of which world or player connects; only a
//! *custom* data pack would change them, and this server ships none. So one
//! capture is a faithful stand-in for what this server's own (nonexistent)
//! synchronize-registries step would produce.
//!
//! # `select_known_packs`
//!
//! [`select_known_packs_directive`] needs no fixture: it is the
//! clientbound "select known packs" packet carrying the server's requested
//! pack list, sent with an empty pack list since this server ships no
//! datapacks (confirmed against the decompiled 26.2 registry-synchronize
//! handshake). Real vanilla then waits for the client's own reply before
//! sending registries, so it can skip contents the client already has (the
//! per-registry pack-sync routine's own "can skip contents" check). This
//! server does not wait: with an **empty** requested-pack list, the
//! handshake handler's own accept/reject logic yields an **empty**
//! negotiated set either way — a client that echoes back the empty list
//! satisfies the "accepted equals requested" branch and gets an empty
//! copied set; a client that replies with anything else falls to the
//! empty-set branch — so the server's notion of "packs the client already
//! knows" is unconditionally empty and every entry is sent in full
//! regardless of what the client says.
//! That is exactly what every captured fixture here contains (full NBT, no
//! elided entries), so firing the whole burst — `select_known_packs` then
//! every `registry_data` then `update_tags` — without reading the client's
//! reply first reaches the same wire content a real negotiating server would,
//! and the reply itself is safely ignored when it arrives (`ServerBound::Ignored`,
//! since no decode arm claims it).

use lodestone_server::ServerDirective;

use crate::packet_ids::configuration;

/// Parses the same hex fixture format `tests/live_registry_data_full_set.rs`
/// writes: `#`-prefixed comment lines are dropped, everything else is
/// whitespace-separated hex byte pairs.
fn parse_hex_fixture(text: &str) -> Vec<u8> {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .flat_map(str::split_whitespace)
        .map(|tok| u8::from_str_radix(tok, 16).expect("registry fixture hex byte"))
        .collect()
}

/// One `(registry key, embedded fixture text)` pair. The registry key is
/// carried for documentation/debugging only — the fixture's own bytes already
/// encode it as the packet's first field, so nothing here re-derives it.
macro_rules! fixture {
    ($registry:literal, $file:literal) => {
        ($registry, include_str!($file))
    };
}

/// The 27 synchronized registries (of 29 —
/// `minecraft:dimension_type`/`minecraft:world_clock` are the structured
/// exceptions above) this server relays as opaque captured bytes, in
/// the vanilla registry loader's own synchronized-registries list order
/// (not load-bearing —
/// each `registry_data` packet is independently framed — but matching a real
/// server's own emission order costs nothing and helps a packet capture diff
/// cleanly against vanilla's).
const PASSTHROUGH_REGISTRY_FIXTURES: &[(&str, &str)] = &[
    fixture!(
        "minecraft:worldgen/biome",
        "../tests/fixtures/registry_data_worldgen_biome.hex"
    ),
    fixture!(
        "minecraft:chat_type",
        "../tests/fixtures/registry_data_chat_type.hex"
    ),
    fixture!(
        "minecraft:trim_pattern",
        "../tests/fixtures/registry_data_trim_pattern.hex"
    ),
    fixture!(
        "minecraft:trim_material",
        "../tests/fixtures/registry_data_trim_material.hex"
    ),
    fixture!(
        "minecraft:wolf_variant",
        "../tests/fixtures/registry_data_wolf_variant.hex"
    ),
    fixture!(
        "minecraft:wolf_sound_variant",
        "../tests/fixtures/registry_data_wolf_sound_variant.hex"
    ),
    fixture!(
        "minecraft:pig_variant",
        "../tests/fixtures/registry_data_pig_variant.hex"
    ),
    fixture!(
        "minecraft:pig_sound_variant",
        "../tests/fixtures/registry_data_pig_sound_variant.hex"
    ),
    fixture!(
        "minecraft:frog_variant",
        "../tests/fixtures/registry_data_frog_variant.hex"
    ),
    fixture!(
        "minecraft:cat_variant",
        "../tests/fixtures/registry_data_cat_variant.hex"
    ),
    fixture!(
        "minecraft:cat_sound_variant",
        "../tests/fixtures/registry_data_cat_sound_variant.hex"
    ),
    fixture!(
        "minecraft:cow_sound_variant",
        "../tests/fixtures/registry_data_cow_sound_variant.hex"
    ),
    fixture!(
        "minecraft:cow_variant",
        "../tests/fixtures/registry_data_cow_variant.hex"
    ),
    fixture!(
        "minecraft:chicken_sound_variant",
        "../tests/fixtures/registry_data_chicken_sound_variant.hex"
    ),
    fixture!(
        "minecraft:chicken_variant",
        "../tests/fixtures/registry_data_chicken_variant.hex"
    ),
    fixture!(
        "minecraft:zombie_nautilus_variant",
        "../tests/fixtures/registry_data_zombie_nautilus_variant.hex"
    ),
    fixture!(
        "minecraft:painting_variant",
        "../tests/fixtures/registry_data_painting_variant.hex"
    ),
    fixture!(
        "minecraft:sulfur_cube_archetype",
        "../tests/fixtures/registry_data_sulfur_cube_archetype.hex"
    ),
    fixture!(
        "minecraft:damage_type",
        "../tests/fixtures/registry_data_damage_type.hex"
    ),
    fixture!(
        "minecraft:banner_pattern",
        "../tests/fixtures/registry_data_banner_pattern.hex"
    ),
    fixture!(
        "minecraft:enchantment",
        "../tests/fixtures/registry_data_enchantment.hex"
    ),
    fixture!(
        "minecraft:jukebox_song",
        "../tests/fixtures/registry_data_jukebox_song.hex"
    ),
    fixture!(
        "minecraft:instrument",
        "../tests/fixtures/registry_data_instrument.hex"
    ),
    fixture!(
        "minecraft:test_environment",
        "../tests/fixtures/registry_data_test_environment.hex"
    ),
    fixture!(
        "minecraft:test_instance",
        "../tests/fixtures/registry_data_test_instance.hex"
    ),
    fixture!(
        "minecraft:dialog",
        "../tests/fixtures/registry_data_dialog.hex"
    ),
    fixture!(
        "minecraft:timeline",
        "../tests/fixtures/registry_data_timeline.hex"
    ),
];

/// The captured `update_tags` payload sent once, after every `registry_data`
/// packet and before `FINISH_CONFIGURATION` — matching the vanilla
/// registry-synchronize handshake's own send order (registries, then one
/// tag-update packet).
const UPDATE_TAGS_FIXTURE: &str = include_str!("../tests/fixtures/update_tags_configuration.hex");

/// Builds the 27 opaque `registry_data` sends (see module docs for why
/// `dimension_type`/`world_clock` are not among them).
pub(crate) fn passthrough_registry_directives() -> Vec<ServerDirective> {
    PASSTHROUGH_REGISTRY_FIXTURES
        .iter()
        .map(|(_registry, text)| ServerDirective::Send {
            packet_id: configuration::clientbound::REGISTRY_DATA,
            payload: parse_hex_fixture(text),
        })
        .collect()
}

/// Builds the single `update_tags` send.
pub(crate) fn update_tags_directive() -> ServerDirective {
    ServerDirective::Send {
        packet_id: configuration::clientbound::UPDATE_TAGS,
        payload: parse_hex_fixture(UPDATE_TAGS_FIXTURE),
    }
}

/// Builds `select_known_packs`, requesting zero packs (see module docs for
/// why an empty request needs no negotiation wait). Wire layout: a single
/// VarInt `0` — the vanilla packet's own collection-writer with nothing to
/// write.
pub(crate) fn select_known_packs_directive() -> ServerDirective {
    let mut w = lodestone_core::Writer::default();
    w.var_i32(0);
    ServerDirective::Send {
        packet_id: configuration::clientbound::SELECT_KNOWN_PACKS,
        payload: w.into_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_passthrough_fixture_parses_to_a_nonempty_payload_naming_its_own_registry() {
        for (registry, text) in PASSTHROUGH_REGISTRY_FIXTURES {
            let bytes = parse_hex_fixture(text);
            assert!(!bytes.is_empty(), "{registry}: fixture must not be empty");
            // The payload's own first field is the registry name string
            // (`VarInt length` then UTF-8 bytes) — assert the captured bytes
            // actually carry *this* registry's identifier, catching a
            // copy-paste of the wrong fixture into the wrong table row.
            let mut r = lodestone_core::Reader::new(&bytes);
            let decoded_name = r
                .string(32767)
                .unwrap_or_else(|err| panic!("{registry}: {err}"));
            assert_eq!(&decoded_name, registry);
        }
    }

    #[test]
    fn passthrough_directives_cover_27_of_the_29_synchronized_registries() {
        let directives = passthrough_registry_directives();
        assert_eq!(directives.len(), 27);
        for directive in &directives {
            match directive {
                ServerDirective::Send { packet_id, payload } => {
                    assert_eq!(*packet_id, configuration::clientbound::REGISTRY_DATA);
                    assert!(!payload.is_empty());
                }
                other => panic!("expected a Send directive, got {other:?}"),
            }
        }
    }

    #[test]
    fn update_tags_directive_is_nonempty_and_uses_the_configuration_packet_id() {
        match update_tags_directive() {
            ServerDirective::Send { packet_id, payload } => {
                assert_eq!(packet_id, configuration::clientbound::UPDATE_TAGS);
                assert!(!payload.is_empty());
            }
            other => panic!("expected a Send directive, got {other:?}"),
        }
    }

    #[test]
    fn select_known_packs_directive_requests_zero_packs() {
        match select_known_packs_directive() {
            ServerDirective::Send { packet_id, payload } => {
                assert_eq!(packet_id, configuration::clientbound::SELECT_KNOWN_PACKS);
                // A VarInt `0` is one byte: `0x00`.
                assert_eq!(payload, vec![0u8]);
            }
            other => panic!("expected a Send directive, got {other:?}"),
        }
    }
}
