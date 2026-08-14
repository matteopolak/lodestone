//! Hermetic `registry_data` decode, replayed against **captured server bytes**.
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
         field is what `sky_default_for_dimension` matched on a name for"
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
/// a hardcoded overworld — which is precisely the old fallback behaviour this
/// test exists to catch a regression to.
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

// ---------------------------------------------------------------------------
// Biome sky colours
// ---------------------------------------------------------------------------

/// The **holder-id mapping** for biome sky colours, and the elision rule that
/// protects it.
///
/// Scope note, deliberately narrow: the *wire shape* of a biome entry is pinned
/// by `tests/live_registry_data.rs`'s
/// `biome_sky_colours_from_a_real_server_match_mojangs_own_biome_files`, which
/// reads the server's own bytes and checks every entry against Mojang's own
/// `worldgen/biome/*.json`. It cannot be pinned here: the biome payload is tens
/// of kilobytes of deep compounds, too large to check in as reviewable hex, and
/// a compound built with our own `Nbt` writer would confirm our own guess about
/// the shape either way (`CLAUDE.md`, evidence standards).
///
/// What *is* provable hermetically is the part a wrong answer silently ruins:
/// that index `i` is holder id `i` even when entries around it carry no colour.
/// A chunk's biome palette stores bare integers, so an off-by-one here paints
/// every biome with its neighbour's sky and looks entirely plausible.
#[test]
fn biome_sky_colours_resolve_by_holder_id() {
    use lodestone_core::Nbt;
    use lodestone_v770::packets::registry::PackedRegistryEntry;

    /// One biome entry shaped the way the wire carries it: an `attributes`
    /// compound keyed by attribute id, whose value for a plain `override` is the
    /// bare hex string (`EnvironmentAttributeMap.Entry`'s `Codec.either` left
    /// branch).
    fn biome(id: &str, sky: Option<&str>) -> PackedRegistryEntry {
        let mut attributes = vec![(
            "minecraft:gameplay/increased_fire_burnout".to_owned(),
            Nbt::Byte(1),
        )];
        if let Some(sky) = sky {
            attributes.push((
                "minecraft:visual/sky_color".to_owned(),
                Nbt::String(sky.to_owned()),
            ));
        }
        PackedRegistryEntry {
            id: id.to_owned(),
            data: Some(Nbt::Compound(vec![
                ("has_precipitation".to_owned(), Nbt::Byte(1)),
                ("temperature".to_owned(), Nbt::Float(0.8)),
                ("attributes".to_owned(), Nbt::Compound(attributes)),
            ])),
        }
    }

    let mut registries = ClientRegistries::default();
    registries.apply(RegistryData {
        registry: ClientRegistries::BIOME.to_owned(),
        entries: vec![
            // Holder 0: an entry with no colour at all (the Nether/End shape).
            biome("minecraft:nether_wastes", None),
            // Holder 1: the outlier grey.
            biome("minecraft:pale_garden", Some("#b9b9b9")),
            // Holder 2: an entry whose NBT is elided entirely.
            PackedRegistryEntry {
                id: "mypack:elided".to_owned(),
                data: None,
            },
            // Holder 3: a slight neighbour of holder 4, so the table is not
            // separable by "is it the grey one".
            biome("minecraft:desert", Some("#6eb1ff")),
            biome("minecraft:frozen_peaks", Some("#859dff")),
        ],
    });

    let colors = registries.biome_sky_colors();
    assert_eq!(
        colors,
        [
            None,
            Some(0x00b9_b9b9),
            None,
            Some(0x006e_b1ff),
            Some(0x0085_9dff),
        ],
        "index i must be holder id i, with the two colourless entries holding their places"
    );

    // The names still come out of the generic `entry_names` path — lifting one
    // attribute out must not cost the id -> name map every other registry has.
    assert_eq!(
        registries
            .entry_names(ClientRegistries::BIOME)
            .expect("biome names are still recorded")
            .len(),
        5
    );

    // A resent registry replaces rather than appends, exactly as for the other
    // two: `start_configuration` re-sends the whole set, and appending would put
    // the second copy's biomes at holder ids 5.. while the stale ones kept
    // answering 0...
    registries.apply(RegistryData {
        registry: ClientRegistries::BIOME.to_owned(),
        entries: vec![biome("minecraft:plains", Some("#78a7ff"))],
    });
    assert_eq!(registries.biome_sky_colors(), [Some(0x0078_a7ff)]);
}

/// The `has_precipitation`/`temperature`/`downfall` triple lives at the top of
/// the biome compound, a sibling of `attributes` — not nested under it like
/// `sky_color` — per `Biome.ClimateSettings.CODEC`.
/// This is the input `precipitation_for_temperature` and
/// `height_adjusted_temperature` (`lodestone-render`'s `weather.rs`) have had
/// unit tests for but no real caller for, per `docs/weather.md`'s "Snow: the
/// biome lane, precisely".
#[test]
fn biome_climates_resolve_by_holder_id_and_hold_place_for_a_bad_entry() {
    use lodestone_core::Nbt;
    use lodestone_v770::packets::registry::{BiomeClimate, PackedRegistryEntry};

    fn biome(id: &str, has_precipitation: bool, temperature: f32, downfall: f32) -> PackedRegistryEntry {
        PackedRegistryEntry {
            id: id.to_owned(),
            data: Some(Nbt::Compound(vec![
                ("has_precipitation".to_owned(), Nbt::Byte(has_precipitation as i8)),
                ("temperature".to_owned(), Nbt::Float(temperature)),
                ("downfall".to_owned(), Nbt::Float(downfall)),
                ("attributes".to_owned(), Nbt::Compound(Vec::new())),
            ])),
        }
    }

    let mut registries = ClientRegistries::default();
    registries.apply(RegistryData {
        registry: ClientRegistries::BIOME.to_owned(),
        entries: vec![
            // Holder 0: a real biome below the rain/snow threshold.
            biome("minecraft:frozen_peaks", true, 0.1, 0.9),
            // Holder 1: a desert — precipitation entirely off, warm.
            biome("minecraft:desert", false, 2.0, 0.0),
            // Holder 2: an entry whose NBT is elided entirely.
            PackedRegistryEntry {
                id: "mypack:elided".to_owned(),
                data: None,
            },
            // Holder 3: missing `downfall` — vanilla's codec has it required
            // (`fieldOf`, not `optionalFieldOf`), so this must read as `None`,
            // not default to 0.0 and silently answer for a real biome.
            PackedRegistryEntry {
                id: "mypack:malformed".to_owned(),
                data: Some(Nbt::Compound(vec![
                    ("has_precipitation".to_owned(), Nbt::Byte(1)),
                    ("temperature".to_owned(), Nbt::Float(0.8)),
                ])),
            },
            // Holder 4: warm enough to rain, right at the neighbour of holder 0
            // so the table is not separable by "is it the cold one".
            biome("minecraft:plains", true, 0.8, 0.4),
        ],
    });

    let climates = registries.biome_climates();
    assert_eq!(
        climates,
        [
            Some(BiomeClimate {
                has_precipitation: true,
                temperature: 0.1,
                downfall: 0.9,
            }),
            Some(BiomeClimate {
                has_precipitation: false,
                temperature: 2.0,
                downfall: 0.0,
            }),
            None,
            None,
            Some(BiomeClimate {
                has_precipitation: true,
                temperature: 0.8,
                downfall: 0.4,
            }),
        ],
        "index i must be holder id i, with the elided/malformed entries holding their places"
    );

    // A resent registry replaces rather than appends, exactly as
    // `biome_sky_colours_resolve_by_holder_id` proves for the sibling table.
    registries.apply(RegistryData {
        registry: ClientRegistries::BIOME.to_owned(),
        entries: vec![biome("minecraft:swamp", true, 0.8, 0.9)],
    });
    assert_eq!(
        registries.biome_climates(),
        [Some(BiomeClimate {
            has_precipitation: true,
            temperature: 0.8,
            downfall: 0.9,
        })]
    );
}

/// The modifier form of an attribute entry, which no vanilla biome uses for
/// `sky_color` and a data pack may.
///
/// `EnvironmentAttributeMap.Entry::createCodec` is
/// `Codec.either(valueCodec, fullCodec)`: a plain `override` collapses to the
/// bare value, anything else serialises as `{ modifier, argument }`. Reading only
/// the bare tag would return `None` here — a silently untinted sky rather than a
/// visible failure, which is the direction this repo keeps getting burned in.
#[test]
fn a_modifier_wrapped_sky_color_is_still_read() {
    use lodestone_core::Nbt;
    use lodestone_v770::packets::registry::PackedRegistryEntry;

    let mut registries = ClientRegistries::default();
    registries.apply(RegistryData {
        registry: ClientRegistries::BIOME.to_owned(),
        entries: vec![PackedRegistryEntry {
            id: "mypack:tinted".to_owned(),
            data: Some(Nbt::Compound(vec![(
                "attributes".to_owned(),
                Nbt::Compound(vec![(
                    "minecraft:visual/sky_color".to_owned(),
                    Nbt::Compound(vec![
                        ("modifier".to_owned(), Nbt::String("override".to_owned())),
                        ("argument".to_owned(), Nbt::String("#123456".to_owned())),
                    ]),
                )]),
            )])),
        }],
    });
    assert_eq!(registries.biome_sky_colors(), [Some(0x0012_3456)]);
}

/// **Control.** A malformed or unexpected `sky_color` must read as `None` — the
/// "the server has not told us" fallback every hop in the biome-registry chain uses — and
/// must not disconnect, default to a plausible blue, or shift the id space.
#[test]
fn an_unusable_sky_color_reads_as_absent_rather_than_as_a_default() {
    use lodestone_core::Nbt;
    use lodestone_v770::packets::registry::PackedRegistryEntry;

    let cases: [(&str, Nbt); 4] = [
        // Missing the leading `#` vanilla's own `hexColor` requires.
        ("no hash", Nbt::String("78a7ff".to_owned())),
        // Wrong digit count (the ARGB 8-digit form, which `RGB_COLOR` is not).
        ("eight digits", Nbt::String("#ff78a7ff".to_owned())),
        ("not hex", Nbt::String("#zzzzzz".to_owned())),
        ("wrong tag", Nbt::Float(1.0)),
    ];
    for (label, value) in cases {
        let mut registries = ClientRegistries::default();
        registries.apply(RegistryData {
            registry: ClientRegistries::BIOME.to_owned(),
            entries: vec![PackedRegistryEntry {
                id: "mypack:broken".to_owned(),
                data: Some(Nbt::Compound(vec![(
                    "attributes".to_owned(),
                    Nbt::Compound(vec![("minecraft:visual/sky_color".to_owned(), value)]),
                )])),
            }],
        });
        assert_eq!(
            registries.biome_sky_colors(),
            [None],
            "{label}: an unusable value must read as absent, keeping its slot"
        );
    }
}

// ---------------------------------------------------------------------------
// Server-side mirror
// ---------------------------------------------------------------------------

/// The server's Configuration-phase registry burst is byte-identical to the
/// captured vanilla fixtures, for every one of the 29 synchronized
/// registries, and carries `select_known_packs` first and `update_tags` last.
///
/// A later fix made the server send the **full** synchronized-registry set
/// (previously just `dimension_type`/`world_clock`) plus `select_known_packs`
/// and `update_tags`.
/// [`V770ServerProtocol::encode_registry_data`](lodestone_v770::V770ServerProtocol)'s
/// bodies are copied verbatim from fixtures this crate's `live_registry_data*`
/// gates captured from a real vanilla 26.2 server, so the proof is a
/// round-trip through the public seam: the directives it emits, compared
/// against every byte a real server authored. The fixture stands outside
/// both encoder and decoder, so two symmetric misunderstandings cannot
/// satisfy it (`CLAUDE.md`, evidence standards).
#[test]
fn server_registry_data_payloads_match_the_captured_vanilla_fixtures() {
    use lodestone_server::{ServerDirective, ServerProtocol};
    use lodestone_v770::packets::registry::RegistryData;
    use lodestone_v770::{
        V770ServerProtocol,
        packet_ids::configuration::clientbound::{
            REGISTRY_DATA, SELECT_KNOWN_PACKS, UPDATE_TAGS,
        },
    };

    // The 29 entries of `RegistryDataLoader.SYNCHRONIZED_REGISTRIES`
    // (`.cache/mc/26.2/src/net/minecraft/resources/RegistryDataLoader.java`),
    // not `generated/reports/registries.json` — that file is authoritative
    // about registry *contents*, not which registries are synchronized, and
    // it omits `dimension_type`/`world_clock` entirely (`CLAUDE.md`).
    const SYNCHRONIZED_REGISTRIES: &[&str] = &[
        "minecraft:worldgen/biome",
        "minecraft:chat_type",
        "minecraft:trim_pattern",
        "minecraft:trim_material",
        "minecraft:wolf_variant",
        "minecraft:wolf_sound_variant",
        "minecraft:pig_variant",
        "minecraft:pig_sound_variant",
        "minecraft:frog_variant",
        "minecraft:cat_variant",
        "minecraft:cat_sound_variant",
        "minecraft:cow_sound_variant",
        "minecraft:cow_variant",
        "minecraft:chicken_sound_variant",
        "minecraft:chicken_variant",
        "minecraft:zombie_nautilus_variant",
        "minecraft:painting_variant",
        "minecraft:sulfur_cube_archetype",
        "minecraft:dimension_type",
        "minecraft:damage_type",
        "minecraft:banner_pattern",
        "minecraft:enchantment",
        "minecraft:jukebox_song",
        "minecraft:instrument",
        "minecraft:test_environment",
        "minecraft:test_instance",
        "minecraft:dialog",
        "minecraft:world_clock",
        "minecraft:timeline",
    ];

    let directives = V770ServerProtocol.encode_registry_data();
    assert_eq!(
        directives.len(),
        31,
        "select_known_packs + 29 registries + update_tags"
    );

    let sends: Vec<(i32, &[u8])> = directives
        .iter()
        .map(|directive| match directive {
            ServerDirective::Send { packet_id, payload } => (*packet_id, payload.as_slice()),
            other => panic!("expected every configuration directive to be a Send, got {other:?}"),
        })
        .collect();

    // --- Order: select_known_packs first, update_tags last ----------------
    assert_eq!(
        sends.first().map(|(id, _)| *id),
        Some(SELECT_KNOWN_PACKS),
        "select_known_packs must precede every registry_data packet, matching \
         SynchronizeRegistriesTask's own wire order"
    );
    assert_eq!(
        sends.first().map(|(_, payload)| *payload),
        Some([0u8].as_slice()),
        "requesting zero known packs — this server ships no datapacks"
    );
    assert_eq!(
        sends.last().map(|(id, _)| *id),
        Some(UPDATE_TAGS),
        "update_tags must follow every registry_data packet"
    );
    assert_eq!(
        sends.last().map(|(_, payload)| *payload),
        Some(fixture("update_tags_configuration.hex").as_slice()),
        "update_tags payload must match vanilla's captured bytes byte-for-byte"
    );

    // --- The 29 registries in between --------------------------------------
    let registry_sends = &sends[1..sends.len() - 1];
    assert_eq!(registry_sends.len(), 29);

    let mut by_name: std::collections::BTreeMap<String, &[u8]> = std::collections::BTreeMap::new();
    for &(packet_id, payload) in registry_sends {
        assert_eq!(packet_id, REGISTRY_DATA, "every middle payload is registry_data");
        let mut reader = lodestone_core::Reader::new(payload);
        let decoded = RegistryData::decode(&mut reader, CTX).expect("must decode");
        reader.ensure_empty().expect("no trailing bytes");
        assert!(
            by_name.insert(decoded.registry, payload).is_none(),
            "a registry must not be sent twice"
        );
    }

    let sent: Vec<&str> = by_name.keys().map(String::as_str).collect();
    let mut expected: Vec<&str> = SYNCHRONIZED_REGISTRIES.to_vec();
    expected.sort_unstable();
    assert_eq!(
        sent, expected,
        "the server must send exactly the 29 synchronized registries, no more, no fewer"
    );

    // --- Spot-check byte-identity against the captured fixtures ------------
    // The two structured registries (resolved elsewhere by holder id) plus a
    // sample of the opaque pass-through ones, so this is a content check, not
    // just a count.
    assert_eq!(
        by_name["minecraft:dimension_type"],
        fixture("registry_data_dimension_type.hex"),
        "dimension_type payload must match vanilla's captured bytes byte-for-byte"
    );
    assert_eq!(
        by_name["minecraft:world_clock"],
        fixture("registry_data_world_clock.hex"),
        "world_clock payload must match vanilla's captured bytes byte-for-byte"
    );
    assert_eq!(
        by_name["minecraft:worldgen/biome"],
        fixture("registry_data_worldgen_biome.hex"),
        "worldgen/biome payload must match vanilla's captured bytes byte-for-byte"
    );
    assert_eq!(
        by_name["minecraft:enchantment"],
        fixture("registry_data_enchantment.hex"),
        "enchantment payload must match vanilla's captured bytes byte-for-byte"
    );
    assert_eq!(
        by_name["minecraft:test_environment"],
        fixture("registry_data_test_environment.hex"),
        "test_environment payload must match vanilla's captured bytes byte-for-byte \
         (the smallest registry — an empty-or-near-empty entry list)"
    );
}

/// **Control.** A protocol family that does not host (no `ServerProtocol`
/// impl, or a family that hosts but has nothing to declare) emits **no**
/// registry data at all — the trait default is empty. The `Numbered` test
/// double in `lodestone-server` overrides this, so this proves the empty
/// answer comes from the default, not from some latent "send nothing"
/// convention in the seam.
#[test]
fn the_server_protocol_default_emits_no_registry_data() {
    use lodestone_server::ServerProtocol;

    struct EmitsNothing;
    impl ServerProtocol for EmitsNothing {
        fn decode(
            &self,
            _state: lodestone_core::State,
            _packet_id: i32,
            _payload: &[u8],
        ) -> lodestone_server::ServerBound {
            lodestone_server::ServerBound::Ignored
        }

        // The join and chunk-streaming methods are **required**, with no trait
        // default, and that is deliberate: a
        // defaulted `ServerProtocol` method is a trap, because a family that
        // silently inherits a no-op looks implemented. This double exists only
        // to read `encode_registry_data`'s default, so every one of them is
        // unreachable here — and says so loudly rather than returning an empty
        // vec that a future reader could mistake for a real answer.
        fn login_success(
            &self,
            _name: &str,
            _uuid: uuid::Uuid,
        ) -> Vec<lodestone_server::ServerDirective> {
            unreachable!("this control never drives a join")
        }

        fn begin_configuration(&self) -> Vec<lodestone_server::ServerDirective> {
            unreachable!("this control never drives a join")
        }

        fn begin_play(&self, _entity_id: i32) -> Vec<lodestone_server::ServerDirective> {
            unreachable!("this control never drives a join")
        }

        fn begin_chunk_batch(&self) -> lodestone_server::ServerDirective {
            unreachable!("this control never streams chunks")
        }

        fn encode_chunk(
            &self,
            _cx: i32,
            _cz: i32,
            _column: &lodestone_server::ChunkColumn,
        ) -> lodestone_server::ServerDirective {
            unreachable!("this control never streams chunks")
        }

        fn end_chunk_batch(&self, _count: i32) -> lodestone_server::ServerDirective {
            unreachable!("this control never streams chunks")
        }
    }

    assert!(
        EmitsNothing.encode_registry_data().is_empty(),
        "a hosting family that sends no registries must be representable — \
         the seam defaults to an empty stream"
    );
}

/// **Control.** `minecraft:biome` — the name this lookup is *tempting* to use —
/// must yield nothing, because the registry actually arrives as
/// `minecraft:worldgen/biome`. If a future refactor matched the short name, the
/// affirmative tests above would keep passing off their own literal and the live
/// path would silently tint nothing.
#[test]
fn the_short_registry_name_is_not_the_biome_registry() {
    use lodestone_core::Nbt;
    use lodestone_v770::packets::registry::PackedRegistryEntry;

    assert_eq!(ClientRegistries::BIOME, "minecraft:worldgen/biome");
    let mut registries = ClientRegistries::default();
    registries.apply(RegistryData {
        registry: "minecraft:biome".to_owned(),
        entries: vec![PackedRegistryEntry {
            id: "minecraft:plains".to_owned(),
            data: Some(Nbt::Compound(vec![(
                "attributes".to_owned(),
                Nbt::Compound(vec![(
                    "minecraft:visual/sky_color".to_owned(),
                    Nbt::String("#78a7ff".to_owned()),
                )]),
            )])),
        }],
    });
    assert!(
        registries.biome_sky_colors().is_empty(),
        "a registry called minecraft:biome is not the biome registry in 26.2"
    );
}
