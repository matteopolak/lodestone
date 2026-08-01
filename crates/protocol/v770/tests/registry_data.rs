//! Hermetic `registry_data` decode, replayed against **captured server bytes**
//! (issue #288).
//!
//! The two fixtures under `tests/fixtures/registry_data_*.hex` are raw payloads
//! a real vanilla 26.2 server authored during Configuration, captured by
//! `tests/live_registry_data.rs`. Nothing in this file was produced by our own
//! encoder — that is the point: `decode(encode(x)) == x` is satisfied by two
//! symmetric misunderstandings, and this repo has already been burned by exactly
//! that (`CLAUDE.md`, evidence standards).
//!
//! The expected *field values* are transcribed from Mojang's own shipped data
//! files, `.cache/mc/26.2/client-src/data/minecraft/dimension_type/*.json`, whose
//! paths are named at each assertion. The live sibling checks the decode against
//! those files **directly** rather than against these literals, so a
//! transcription slip here cannot survive a live run.

use lodestone_core::{Ctx, Decode, Reader};
use lodestone_v770::packets::registry::{ClientRegistries, RegistryData};

const CTX: Ctx = Ctx { version: 776 };

fn fixture(name: &str) -> Vec<u8> {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .flat_map(str::split_whitespace)
        .map(|tok| u8::from_str_radix(tok, 16).expect("fixture hex byte"))
        .collect()
}

fn decode(name: &str) -> RegistryData {
    let bytes = fixture(name);
    let mut reader = Reader::new(&bytes);
    let data = RegistryData::decode(&mut reader, CTX)
        .unwrap_or_else(|err| panic!("{name} must decode: {err}"));
    // The single best detector of a subtly wrong layout: a field read one byte
    // short still decodes, it just leaves a tail.
    reader
        .ensure_empty()
        .unwrap_or_else(|err| panic!("{name} left trailing bytes: {err}"));
    data
}

#[test]
fn real_dimension_type_registry_bytes_decode_with_zero_trailing_bytes() {
    let data = decode("registry_data_dimension_type.hex");
    assert_eq!(data.registry, "minecraft:dimension_type");
    let names: Vec<&str> = data.entries.iter().map(|e| e.id.as_str()).collect();
    // Registry order == holder-id space. `login`/`respawn` send an index into
    // this list, so its *order* is load-bearing, not just its contents.
    assert_eq!(
        names,
        [
            "minecraft:overworld",
            "minecraft:overworld_caves",
            "minecraft:the_end",
            "minecraft:the_nether",
        ],
    );
    // Our join claims no known packs, so the server elides nothing — every entry
    // carries full NBT. See `packets::registry`'s module docs.
    assert!(
        data.entries.iter().all(|entry| entry.data.is_some()),
        "a client that claims no known packs must receive every entry's contents"
    );
}

#[test]
fn real_world_clock_registry_bytes_pin_the_holder_ids_set_time_uses() {
    let data = decode("registry_data_world_clock.hex");
    assert_eq!(data.registry, "minecraft:world_clock");
    let mut registries = ClientRegistries::default();
    registries.apply(data);
    // This pair is the whole reason `SetTime::day_clock`'s "lowest holder id"
    // heuristic looked right: the overworld genuinely is `0`. It is also why the
    // heuristic was wrong in the End, whose own clock is `1`.
    assert_eq!(registries.world_clock_id("minecraft:overworld"), Some(0));
    assert_eq!(registries.world_clock_id("minecraft:the_end"), Some(1));
    assert_eq!(registries.world_clock_id("minecraft:nonexistent"), None);
}

#[test]
fn real_dimension_types_resolve_the_fields_that_were_hardcoded_before_288() {
    let mut registries = ClientRegistries::default();
    registries.apply(decode("registry_data_dimension_type.hex"));

    // Expected values from `.cache/mc/26.2/client-src/data/minecraft/dimension_type/overworld.json`.
    let (name, overworld) = registries
        .dimension_type(0)
        .expect("holder 0 must resolve — it is what `login` reports on a vanilla join");
    assert_eq!(name, "minecraft:overworld");
    assert!(overworld.has_skylight);
    assert!(!overworld.has_ceiling);
    assert!(!overworld.has_fixed_time);
    assert_eq!(overworld.min_y, -64);
    assert_eq!(overworld.height, 384);
    assert_eq!(overworld.section_count(), 24);
    assert_eq!(overworld.logical_height, 384);
    assert!((overworld.coordinate_scale - 1.0).abs() < 1e-9);
    assert!((overworld.ambient_light - 0.0).abs() < 1e-6);
    assert_eq!(overworld.default_clock.as_deref(), Some("minecraft:overworld"));

    // `.../the_nether.json`. The three fields this client used to guess by name,
    // all of which the Nether contradicts: no sky light, a ceiling, `8.0` scale.
    let nether = registries
        .dimension_type_by_name("minecraft:the_nether")
        .expect("the_nether must be present");
    assert!(
        !nether.has_skylight,
        "the Nether is the one vanilla dimension type without sky light — this \
         field is what `sky_default_for_dimension` matched on a name for (#34)"
    );
    assert!(nether.has_ceiling);
    assert!(nether.has_fixed_time);
    assert_eq!(nether.min_y, 0);
    assert_eq!(nether.height, 256);
    assert_eq!(nether.section_count(), 16);
    assert_eq!(
        nether.logical_height, 128,
        "logical_height is half the height in the Nether — a value no name match \
         could have produced"
    );
    assert!((nether.coordinate_scale - 8.0).abs() < 1e-9);
    assert!((nether.ambient_light - 0.1).abs() < 1e-6);
    assert_eq!(
        nether.default_clock, None,
        "the Nether has fixed time and therefore no clock of its own"
    );

    // `.../the_end.json`. The measurement that keeps the End out of the "not the
    // overworld, so no sky light" bucket, plus the clock that made the
    // lowest-holder-id heuristic wrong.
    let end = registries
        .dimension_type_by_name("minecraft:the_end")
        .expect("the_end must be present");
    assert!(
        end.has_skylight,
        "the End has sky light exactly like the overworld — lumping it with the \
         Nether would render sky-lit End terrain dark"
    );
    assert!(end.has_fixed_time);
    assert!(end.has_ender_dragon_fight);
    assert_eq!(end.min_y, 0);
    assert_eq!(end.height, 256);
    assert!((end.ambient_light - 0.25).abs() < 1e-6);
    assert_eq!(end.default_clock.as_deref(), Some("minecraft:the_end"));

    // `.../overworld_caves.json` — same window as the overworld but with a
    // ceiling. It exists to prove `has_ceiling` is not derivable from height.
    let caves = registries
        .dimension_type_by_name("minecraft:overworld_caves")
        .expect("overworld_caves must be present");
    assert!(caves.has_skylight);
    assert!(caves.has_ceiling);
    assert_eq!(caves.min_y, -64);
    assert_eq!(caves.height, 384);
}

/// The control for every assertion above: the same lookups against a store no
/// `registry_data` was folded into must resolve **nothing**.
///
/// Without this, all of the above would also pass if `ClientRegistries` returned
/// a hardcoded overworld — which is precisely the pre-#288 behaviour this issue
/// exists to remove.
#[test]
fn without_registry_data_nothing_resolves() {
    let registries = ClientRegistries::default();
    assert!(registries.is_empty());
    assert!(registries.dimension_type(0).is_none());
    assert!(
        registries
            .dimension_type_by_name("minecraft:overworld")
            .is_none()
    );
    assert!(registries.world_clock_id("minecraft:overworld").is_none());
    assert_eq!(registries.dimension_type_count(), 0);
}
