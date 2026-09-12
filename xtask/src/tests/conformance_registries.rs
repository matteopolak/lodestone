// Conformance, registry, and asset tests.
    #[test]
    fn conformance_skip_cargo_checks_packet_ids_and_skips_absent_registry_report() -> Result<()> {
        let workspace = isolation_fixture(
            "conformance",
            &[("crates/versions/v999", "lodestone-v999", "")],
        )?;
        let packet_report_json = r#"{
            "configuration": {"clientbound": {}, "serverbound": {}},
            "handshake": {"serverbound": {"minecraft:intention": {"protocol_id": 0}}},
            "login": {"clientbound": {}, "serverbound": {}},
            "play": {"clientbound": {}, "serverbound": {}},
            "status": {"clientbound": {}, "serverbound": {}}
        }"#;
        let cache_dir = workspace.join(".cache/mc/test/generated/reports");
        std::fs::create_dir_all(&cache_dir)?;
        std::fs::write(cache_dir.join("packets.json"), packet_report_json)?;
        std::fs::create_dir_all(workspace.join("xtask"))?;
        std::fs::write(
            workspace.join(DEFAULT_CONNECTED_ALLOWLIST),
            r#"
[[allow]]
crate = "lodestone-v999"
owner = "xtask-test"
reason = "fixture has no shipped binary root"
"#,
        )?;

        let report = parse_packet_report(packet_report_json, "test", 999)?;
        let generated_dir = workspace.join("crates/versions/v999/src/generated");
        std::fs::create_dir_all(&generated_dir)?;
        std::fs::write(
            generated_dir.join("packet_ids.rs"),
            generate_packet_ids_source(&report)?,
        )?;

        let conformance = run_conformance(
            &workspace,
            &ConformanceOptions {
                family: "v999".to_owned(),
                minecraft_version: "test".to_owned(),
                protocol_version: 999,
                source: PacketSource::Mojang,
                skip_cargo: true,
            },
        )?;

        assert_eq!(
            conformance.steps,
            vec![
                ConformanceStep {
                    name: "gen-packet-ids --check".to_owned(),
                    outcome: ConformanceOutcome::Passed,
                },
                ConformanceStep {
                    name: "gen-registries --check".to_owned(),
                    outcome: ConformanceOutcome::Skipped(format!(
                        "{} is absent; older server jars such as 1.16.5 do not emit Mojang registry reports",
                        workspace
                            .join(".cache/mc/test/generated/reports/registries.json")
                            .display()
                    ),),
                },
                ConformanceStep {
                    name: "check-isolation".to_owned(),
                    outcome: ConformanceOutcome::Passed,
                },
                ConformanceStep {
                    name: "check-deletable".to_owned(),
                    outcome: ConformanceOutcome::Passed,
                },
                ConformanceStep {
                    name: "shape-review".to_owned(),
                    outcome: ConformanceOutcome::Passed,
                },
                ConformanceStep {
                    name: "check-connected".to_owned(),
                    outcome: ConformanceOutcome::Passed,
                },
                ConformanceStep {
                    name: "cargo test/clippy".to_owned(),
                    outcome: ConformanceOutcome::Skipped("--skip-cargo was provided".to_owned()),
                },
            ]
        );
        Ok(())
    }

    /// Reproduces the stale-location bug directly: `conformance`'s registry
    /// step used to point at `crates/versions/<family>/src/generated`, but
    /// the four registries it drift-checks (`sound_events`, `particle_types`,
    /// `menus`, `items`) have lived in `crates/lodestone-data/src/generated`
    /// since the `lodestone-data` extraction. Legacy families skip this step
    /// entirely (no `registries.json`), so the stale path was unreachable
    /// for three of four families and, before `check-connected` was fixed,
    /// unreachable for the fourth too -- two independent guards masking one
    /// bug. This plants a `registries.json` (making the family the one path
    /// that reaches the step) and pre-generates the committed tables at the
    /// *correct*, family-independent location, then asserts the step passes.
    /// Before the fix this failed with "No such file or directory" against
    /// `crates/versions/v999/src/generated/sound_events.rs`, which never
    /// existed.
    #[test]
    fn conformance_registry_check_reads_lodestone_data_not_the_family_generated_dir() -> Result<()>
    {
        let workspace = isolation_fixture(
            "conformance-registry-redirect",
            &[("crates/versions/v999", "lodestone-v999", "")],
        )?;
        let packet_report_json = r#"{
            "configuration": {"clientbound": {}, "serverbound": {}},
            "handshake": {"serverbound": {"minecraft:intention": {"protocol_id": 0}}},
            "login": {"clientbound": {}, "serverbound": {}},
            "play": {"clientbound": {}, "serverbound": {}},
            "status": {"clientbound": {}, "serverbound": {}}
        }"#;
        let cache_dir = workspace.join(".cache/mc/test/generated/reports");
        std::fs::create_dir_all(&cache_dir)?;
        std::fs::write(cache_dir.join("packets.json"), packet_report_json)?;
        // Pairwise-distinct entries per registry so a transposition between
        // registries (all four go through the same generator) cannot survive
        // unnoticed.
        std::fs::write(
            cache_dir.join("registries.json"),
            r#"{
                "minecraft:sound_event": {"entries": {"minecraft:test_sound": {"protocol_id": 0}}},
                "minecraft:particle_type": {"entries": {"minecraft:test_particle": {"protocol_id": 0}}},
                "minecraft:menu": {"entries": {"minecraft:test_menu": {"protocol_id": 0}}},
                "minecraft:item": {"entries": {"minecraft:test_item": {"protocol_id": 0}}}
            }"#,
        )?;
        std::fs::create_dir_all(workspace.join("xtask"))?;
        std::fs::write(
            workspace.join(DEFAULT_CONNECTED_ALLOWLIST),
            r#"
[[allow]]
crate = "lodestone-v999"
owner = "xtask-test"
reason = "fixture has no shipped binary root"
"#,
        )?;

        let report = parse_packet_report(packet_report_json, "test", 999)?;
        let family_generated_dir = workspace.join("crates/versions/v999/src/generated");
        std::fs::create_dir_all(&family_generated_dir)?;
        std::fs::write(
            family_generated_dir.join("packet_ids.rs"),
            generate_packet_ids_source(&report)?,
        )?;

        // Pre-generate the committed registry tables at the real location --
        // crates/lodestone-data/src/generated, not the family's own
        // generated/ -- exactly as they are actually committed in this repo.
        let registry_options = GenRegistriesOptions {
            minecraft_version: "test".to_owned(),
            protocol_version: 999,
            check: false,
            out_dir: PathBuf::from(DEFAULT_REGISTRY_OUT_DIR),
            registries: default_registry_specs()
                .iter()
                .map(|spec| spec.registry_key.to_owned())
                .collect(),
        };
        let written = generate_registries(&workspace, &registry_options)?;
        assert_eq!(written.len(), 4);
        // Positive control: the family's own (stale) location must stay
        // empty, or this test would not distinguish the fix from the bug.
        assert!(!family_generated_dir.join("sound_events.rs").exists());

        let conformance = run_conformance(
            &workspace,
            &ConformanceOptions {
                family: "v999".to_owned(),
                minecraft_version: "test".to_owned(),
                protocol_version: 999,
                source: PacketSource::Mojang,
                skip_cargo: true,
            },
        )?;
        let registry_step = conformance
            .steps
            .iter()
            .find(|step| step.name == "gen-registries --check")
            .expect("conformance always reports a gen-registries --check step");
        assert_eq!(registry_step.outcome, ConformanceOutcome::Passed);

        // Negative control: corrupt the committed table at the real location
        // and confirm conformance's registry step actually reads it (rather
        // than, say, vacuously passing because the file it checks does not
        // exist and some earlier bug swallowed the read error).
        let items_path = workspace.join(DEFAULT_REGISTRY_OUT_DIR).join("items.rs");
        let pristine = std::fs::read_to_string(&items_path)?;
        std::fs::write(&items_path, pristine.replace("minecraft:test_item", "minecraft:corrupted"))?;
        let error = run_conformance(
            &workspace,
            &ConformanceOptions {
                family: "v999".to_owned(),
                minecraft_version: "test".to_owned(),
                protocol_version: 999,
                source: PacketSource::Mojang,
                skip_cargo: true,
            },
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("items.rs is out of date"), "{error}");
        assert!(
            error.contains(
                workspace
                    .join(DEFAULT_REGISTRY_OUT_DIR)
                    .join("items.rs")
                    .to_str()
                    .expect("workspace path is valid UTF-8")
            ),
            "expected the error to name the lodestone-data path, got: {error}"
        );
        Ok(())
    }

    #[test]
    fn planned_commands_return_not_implemented_errors() {
        for command in ["fetch-version", "gen-reports", "new-version"] {
            let error = run_cli_command(CliCommand::Planned { name: command }).unwrap_err();
            assert!(
                error.to_string().contains("not implemented yet"),
                "unexpected error for {command}: {error}"
            );
        }
    }

    #[test]
    fn parses_asset_manifest_and_version_json() -> Result<()> {
        let manifest = r#"{
                "versions": [
                    {"id": "1.21.11", "url": "https://example.invalid/old.json"},
                    {"id": "26.2", "url": "https://example.invalid/26.2.json"}
                ]
            }"#;
        let version_url = parse_version_manifest(manifest, "26.2")?;
        assert_eq!(version_url, "https://example.invalid/26.2.json");

        let version_json = r#"{
                "assetIndex": {
                    "id": "26",
                    "url": "https://example.invalid/assets/26.json",
                    "sha1": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                },
                "downloads": {
                    "client": {
                        "url": "https://example.invalid/client.jar",
                        "sha1": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                        "size": 123456
                    },
                    "server": {
                        "url": "https://example.invalid/server.jar",
                        "sha1": "cccccccccccccccccccccccccccccccccccccccc",
                        "size": 654321
                    }
                }
            }"#;
        let downloads = parse_asset_downloads(version_json)?;
        assert_eq!(downloads.client.url, "https://example.invalid/client.jar");
        assert_eq!(
            downloads.client.sha1,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert_eq!(downloads.client.size, 123456);
        assert_eq!(downloads.server.url, "https://example.invalid/server.jar");
        assert_eq!(
            downloads.server.sha1,
            "cccccccccccccccccccccccccccccccccccccccc"
        );
        assert_eq!(downloads.server.size, 654321);
        assert_eq!(downloads.asset_index.id, "26");
        assert_eq!(
            downloads.asset_index.url,
            "https://example.invalid/assets/26.json"
        );
        assert_eq!(
            downloads.asset_index.sha1,
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        );
        Ok(())
    }

    #[test]
    fn packet_shape_diff_reports_changed_added_and_removed_shapes() -> Result<()> {
        let source = r#"{
            "play": {
                "toClient": {
                    "types": {
                        "packet": ["container", [
                            {"name": "name", "type": ["mapper", {"mappings": {
                                "0x00": "keep_alive",
                                "0x01": "entity_destroy",
                                "0x02": "old_only"
                            }}]}
                        ]],
                        "packet_keep_alive": ["container", [{"name": "id", "type": "i64"}]],
                        "packet_entity_destroy": ["container", [{"name": "count", "type": "i8"}]],
                        "packet_old_only": ["container", [{"name": "id", "type": "varint"}]]
                    }
                }
            }
        }"#;
        let target = r#"{
            "play": {
                "toClient": {
                    "types": {
                        "packet": ["container", [
                            {"name": "name", "type": ["mapper", {"mappings": {
                                "0x00": "keep_alive",
                                "0x01": "entity_destroy",
                                "0x03": "new_only"
                            }}]}
                        ]],
                        "packet_keep_alive": ["container", [{"name": "id", "type": "i64"}]],
                        "packet_entity_destroy": ["container", [{"name": "ids", "type": ["array", {"type": "varint"}]}]],
                        "packet_new_only": ["container", [{"name": "flag", "type": "bool"}]]
                    }
                }
            }
        }"#;

        let changes = compare_minecraft_data_packet_shapes(source, target)?;
        assert_eq!(
            changes,
            vec![
                PacketShapeChange {
                    state: PacketState::Play,
                    bound: PacketBound::Clientbound,
                    packet_name: "minecraft:entity_destroy".to_owned(),
                    kind: PacketShapeChangeKind::Changed,
                },
                PacketShapeChange {
                    state: PacketState::Play,
                    bound: PacketBound::Clientbound,
                    packet_name: "minecraft:new_only".to_owned(),
                    kind: PacketShapeChangeKind::Added,
                },
                PacketShapeChange {
                    state: PacketState::Play,
                    bound: PacketBound::Clientbound,
                    packet_name: "minecraft:old_only".to_owned(),
                    kind: PacketShapeChangeKind::Removed,
                },
            ]
        );
        Ok(())
    }

    #[test]
    fn minecraft_data_protocol_json_falls_back_to_latest_same_major_shape() -> Result<()> {
        let workspace = fresh_test_workspace("minecraft-data-fallback")?;
        let pc = workspace.join("vendor/minecraft-data/data/pc");
        std::fs::create_dir_all(pc.join("1.16.2"))?;
        std::fs::create_dir_all(pc.join("1.16.5"))?;
        std::fs::write(
            pc.join("1.16.2/version.json"),
            r#"{"minecraftVersion":"1.16.2","version":751,"majorVersion":"1.16"}"#,
        )?;
        std::fs::write(pc.join("1.16.2/protocol.json"), r#"{"play":{}}"#)?;
        std::fs::write(
            pc.join("1.16.5/version.json"),
            r#"{"minecraftVersion":"1.16.5","version":754,"majorVersion":"1.16"}"#,
        )?;

        let protocol = load_minecraft_data_protocol_json(&workspace, "1.16.5", 754)?;
        assert_eq!(protocol.minecraft_version, "1.16.5");
        assert_eq!(protocol.protocol_data_version, "1.16.2");
        assert_eq!(protocol.json, r#"{"play":{}}"#);
        Ok(())
    }

    #[test]
    fn parses_registry_report_fixture_and_generates_table_source() -> Result<()> {
        let report = r#"{
            "minecraft:sound_event": {
                "entries": {
                    "minecraft:block.note_block.bell": {"protocol_id": 1, "fixed_range": 16.0},
                    "minecraft:entity.allay.ambient_with_item": {"protocol_id": 0}
                }
            },
            "minecraft:particle_type": {
                "entries": {
                    "minecraft:block": {"protocol_id": 1},
                    "minecraft:angry_villager": {"protocol_id": 0}
                }
            },
            "minecraft:menu": {
                "entries": {
                    "minecraft:generic_9x2": {"protocol_id": 1},
                    "minecraft:generic_9x1": {"protocol_id": 0}
                }
            },
            "minecraft:item": {
                "entries": {
                    "minecraft:air": {"protocol_id": 0},
                    "minecraft:stone": {"protocol_id": 1}
                }
            }
        }"#;

        let tables = parse_registry_report(report, &default_registry_specs())?;
        assert_eq!(tables.len(), 4);
        assert_eq!(
            tables[0].names,
            vec![
                "minecraft:entity.allay.ambient_with_item".to_owned(),
                "minecraft:block.note_block.bell".to_owned(),
            ]
        );
        assert_eq!(
            tables[0].fixed_ranges,
            Some(vec![None, Some("16.0".to_owned())])
        );

        let source = generate_registry_source(&tables[0], "26.2", 776)?;
        assert!(source.contains("pub const SOUND_EVENT_COUNT: u32 = 2;"));
        assert!(source.contains("pub static SOUND_EVENT_FIXED_RANGES: [(u32, f32); 1]"));
        assert!(source.contains("pub static SOUND_EVENT_NAMES: [&str; 2]"));
        assert!(source.contains("(1, 16.0)"));
        assert!(!source.contains("SOUND_EVENT_ENTRIES"));
        assert!(source.contains("\"minecraft:entity.allay.ambient_with_item\""));
        let item_source =
            generate_registry_source(registry_table(&tables, "minecraft:item")?, "26.2", 776)?;
        assert!(item_source.contains("pub const ITEM_COUNT: u32 = 2;"));
        assert!(item_source.contains("pub static ITEM_NAMES: [&str; 2]"));
        assert!(item_source.contains("\"minecraft:stone\""));
        Ok(())
    }

    #[test]
    fn sound_registry_metadata_is_sparse_and_keeps_registry_ids() -> Result<()> {
        let report = r#"{
            "minecraft:sound_event": {
                "entries": {
                    "minecraft:block.note_block.bell": {"protocol_id": 3, "fixed_range": 16.0},
                    "minecraft:entity.allay.ambient_with_item": {"protocol_id": 0},
                    "minecraft:entity.allay.ambient_without_item": {"protocol_id": 1},
                    "minecraft:entity.allay.death": {"protocol_id": 2}
                }
            }
        }"#;
        let spec = default_registry_specs()
            .into_iter()
            .find(|spec| spec.registry_key == "minecraft:sound_event")
            .expect("sound-event registry spec");
        let tables = parse_registry_report(report, &[spec])?;

        let source = generate_registry_source(&tables[0], "26.2", 776)?;

        assert!(source.contains("pub static SOUND_EVENT_FIXED_RANGES: [(u32, f32); 1]"));
        assert!(source.contains("(3, 16.0)"));
        assert!(!source.contains("SOUND_EVENT_ENTRIES"));
        assert_eq!(source.matches("minecraft:block.note_block.bell").count(), 1);
        assert_eq!(
            source
                .matches("minecraft:entity.allay.ambient_with_item")
                .count(),
            1
        );
        Ok(())
    }

    #[test]
    fn parses_real_registry_report_counts_for_dispatch_blockers() -> Result<()> {
        let path = Path::new(".cache/mc/26.2/generated/reports/registries.json");
        if !path.exists() {
            eprintln!(
                "skipping registry report codegen test: {} is absent",
                path.display()
            );
            return Ok(());
        }

        let json = std::fs::read_to_string(path)?;
        let tables = parse_registry_report(&json, &default_registry_specs())?;
        assert_eq!(
            registry_table(&tables, "minecraft:sound_event")?
                .names
                .len(),
            1968
        );
        assert_eq!(
            registry_table(&tables, "minecraft:particle_type")?
                .names
                .len(),
            125
        );
        assert_eq!(registry_table(&tables, "minecraft:menu")?.names.len(), 25);
        assert_eq!(registry_table(&tables, "minecraft:item")?.names.len(), 1537);
        Ok(())
    }

    #[test]
    fn registry_codegen_is_deterministic_and_standalone_rust() -> Result<()> {
        let path = Path::new(".cache/mc/26.2/generated/reports/registries.json");
        if !path.exists() {
            eprintln!(
                "skipping registry report codegen test: {} is absent",
                path.display()
            );
            return Ok(());
        }

        let json = std::fs::read_to_string(path)?;
        let tables = parse_registry_report(&json, &default_registry_specs())?;
        let workspace = fresh_test_workspace("registry-codegen")?;

        for table in &tables {
            let first = generate_registry_source(table, "26.2", 776)?;
            let second = generate_registry_source(table, "26.2", 776)?;
            assert_eq!(first, second);

            let source_path = workspace.join(table.spec.file_name);
            let output_path = workspace.join(format!("{}.rmeta", table.spec.module_stem));
            std::fs::write(&source_path, first)?;
            let status = Command::new("rustc")
                .arg("--edition=2024")
                .arg("--crate-type=lib")
                .arg(&source_path)
                .arg("--emit=metadata")
                .arg("-o")
                .arg(&output_path)
                .status()?;
            assert!(
                status.success(),
                "generated registry source failed to compile: {}",
                source_path.display()
            );
        }
        Ok(())
    }

    #[test]
    fn gen_registries_check_detects_drift_without_writing() -> Result<()> {
        let workspace = fresh_test_workspace("gen-registries-check")?;
        let report_dir = workspace.join(".cache/mc/26.2/generated/reports");
        let out_dir = workspace.join("crates/versions/26.2/src/generated");
        std::fs::create_dir_all(&report_dir)?;
        let report = r#"{
            "minecraft:sound_event": {"entries": {"minecraft:a": {"protocol_id": 0}}},
            "minecraft:particle_type": {"entries": {"minecraft:p": {"protocol_id": 0}}},
            "minecraft:menu": {"entries": {"minecraft:m": {"protocol_id": 0}}},
            "minecraft:item": {"entries": {"minecraft:air": {"protocol_id": 0}}}
        }"#;
        std::fs::write(report_dir.join("registries.json"), report)?;
        let options = GenRegistriesOptions {
            minecraft_version: "26.2".to_owned(),
            protocol_version: 776,
            check: true,
            out_dir: PathBuf::from("crates/versions/26.2/src/generated"),
            registries: default_registry_specs()
                .iter()
                .map(|spec| spec.registry_key.to_owned())
                .collect(),
        };

        let written = generate_registries(&workspace, &options)?;
        assert_eq!(written.len(), 4);
        check_registries(&workspace, &options)?;

        let item_path = out_dir.join("items.rs");
        let pristine = std::fs::read_to_string(&item_path)?;
        std::fs::write(
            &item_path,
            pristine.replace("minecraft:air", "minecraft:dirt"),
        )?;
        let error = check_registries(&workspace, &options).unwrap_err();
        let error = error.to_string();
        assert!(error.contains("items.rs is out of date"), "{error}");
        assert!(error.contains("expected:"), "{error}");
        assert!(error.contains("actual:"), "{error}");
        assert!(std::fs::read_to_string(&item_path)?.contains("minecraft:dirt"));
        Ok(())
    }

    /// A hand-written `sounds.json` exercising every entry shape vanilla uses,
    /// with an index that declares one size per sample. The expected partition is
    /// stated by hand from the fixture, not read back out of the planner.
    const SOUNDS_FIXTURE: &[u8] = br#"{
        "block.stone.break":      { "sounds": ["dig/stone1", "dig/stone2"] },
        "entity.zombie.hurt":     { "sounds": [{ "name": "mob/zombie/hurt1" }] },
        "entity.player.hurt":     { "sounds": [{ "type": "event", "name": "entity.generic.hurt" }] },
        "ambient.cave":           { "sounds": [{ "name": "ambient/cave/cave1", "stream": true }] },
        "music.creative":         { "sounds": [{ "name": "music/game/creative/creative1", "stream": true }] },
        "music_disc.cat":         { "sounds": [{ "name": "records/cat", "stream": true }] },
        "jukebox.play":           { "sounds": ["records/cat"] }
    }"#;

    fn sounds_fixture_index() -> serde_json::Map<String, Value> {
        let mut index = serde_json::Map::new();
        // Sizes are arbitrary but distinct, so a byte total cannot come out right
        // by coincidence.
        for (name, size) in [
            ("minecraft/sounds/dig/stone1.ogg", 100),
            ("minecraft/sounds/dig/stone2.ogg", 200),
            ("minecraft/sounds/mob/zombie/hurt1.ogg", 400),
            ("minecraft/sounds/ambient/cave/cave1.ogg", 800),
            ("minecraft/sounds/music/game/creative/creative1.ogg", 1600),
            ("minecraft/sounds/records/cat.ogg", 3200),
            // An index-only sample no event names.
            ("minecraft/sounds/orphan.ogg", 6400),
            // A non-ogg object, which must not land in `unreferenced`.
            ("minecraft/sounds.json", 12800),
        ] {
            index.insert(
                name.to_string(),
                serde_json::json!({ "hash": "abcdef0123456789abcdef0123456789abcdef01", "size": size }),
            );
        }
        index
    }

    #[test]
    fn the_sound_corpus_excludes_music_only_samples_and_keeps_everything_else() -> Result<()> {
        let index = sounds_fixture_index();
        let corpus = plan_sound_corpus(&index, SOUNDS_FIXTURE, false)?;

        assert_eq!(corpus.events, 7, "one entry per top-level sounds.json key");
        let wanted: Vec<&str> = corpus.wanted.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            wanted,
            vec![
                // The cave loop is `stream: true` and still fetched: the policy is
                // "music events", not vanilla's streaming flag, precisely so
                // ambience survives.
                "minecraft/sounds/ambient/cave/cave1.ogg",
                "minecraft/sounds/dig/stone1.ogg",
                "minecraft/sounds/dig/stone2.ogg",
                "minecraft/sounds/mob/zombie/hurt1.ogg",
                // `records/cat` is referenced by `music_disc.cat` *and* by
                // `jukebox.play`, so "every referencing event is music" is false
                // and it must be fetched. This is the case an "any music event"
                // rule would silently drop.
                "minecraft/sounds/records/cat.ogg",
            ],
            "the wanted set is wrong"
        );
        assert_eq!(corpus.wanted_bytes(), 100 + 200 + 400 + 800 + 3200);

        assert_eq!(
            corpus
                .excluded
                .iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>(),
            vec!["minecraft/sounds/music/game/creative/creative1.ogg"],
            "only the sample referenced exclusively by a music event is excluded"
        );
        assert_eq!(corpus.excluded_bytes(), 1600);

        // The `type: event` indirection contributes no file of its own, so
        // `entity.generic.hurt` (which this fixture does not even define) must not
        // appear as a sample.
        assert!(
            !wanted
                .iter()
                .any(|name| name.contains("generic") || name.contains("entity.")),
            "a type:event entry must not be resolved as a file: {wanted:?}"
        );

        // Only `.ogg` objects count as unreferenced; `sounds.json` itself must not.
        assert_eq!(
            corpus
                .unreferenced
                .iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>(),
            vec!["minecraft/sounds/orphan.ogg"]
        );
        Ok(())
    }

    #[test]
    fn all_folds_music_back_in_and_leaves_nothing_excluded() -> Result<()> {
        let index = sounds_fixture_index();
        let default = plan_sound_corpus(&index, SOUNDS_FIXTURE, false)?;
        let all = plan_sound_corpus(&index, SOUNDS_FIXTURE, true)?;

        assert!(all.excluded.is_empty(), "--all excludes nothing");
        assert_eq!(all.wanted.len(), default.wanted.len() + 1);
        assert_eq!(
            all.wanted_bytes(),
            default.wanted_bytes() + default.excluded_bytes(),
            "the two modes must partition the same total"
        );
        // The orphan is fetched by neither mode: no event can select it.
        assert!(
            !all.wanted
                .iter()
                .any(|(name, _)| name == "minecraft/sounds/orphan.ogg")
        );
        Ok(())
    }

    #[test]
    fn a_sound_name_the_index_does_not_declare_is_an_error_not_a_skip() {
        // The resolution rule being wrong for a version must fail loudly: silently
        // dropping the name would fetch a short corpus and read as success.
        let mut index = sounds_fixture_index();
        index.remove("minecraft/sounds/mob/zombie/hurt1.ogg");
        let error = plan_sound_corpus(&index, SOUNDS_FIXTURE, false)
            .expect_err("a name missing from the index must fail");
        let error = error.to_string();
        assert!(error.contains("mob/zombie/hurt1.ogg"), "{error}");
        assert!(error.contains("resolution rule"), "{error}");

        // Control: with the entry restored the same input plans cleanly, so the
        // failure above was the missing declaration and not the fixture.
        assert!(plan_sound_corpus(&sounds_fixture_index(), SOUNDS_FIXTURE, false).is_ok());
    }

    #[test]
    fn a_namespaced_sound_name_resolves_under_its_own_namespace() {
        assert_eq!(
            sound_object_name("mob/zombie/hurt1"),
            "minecraft/sounds/mob/zombie/hurt1.ogg"
        );
        assert_eq!(
            sound_object_name("somepack:foo/bar"),
            "somepack/sounds/foo/bar.ogg"
        );
    }

    #[test]
    fn music_event_keys_are_recognised_and_world_events_are_not() {
        assert!(is_music_event("music.creative"));
        assert!(is_music_event("music_disc.cat"));
        assert!(is_music_event("music"));
        // The near-misses that a substring test would get wrong.
        assert!(!is_music_event("ambient.cave"));
        assert!(!is_music_event("block.note_block.harp"));
        assert!(!is_music_event("item.goat_horn.sound.0"));
        assert!(!is_music_event("musical"));
    }

    #[test]
    fn fetch_sounds_args_parse_and_reject_nonsense() -> Result<()> {
        assert_eq!(
            parse_cli_args(["fetch-sounds", "--version", "26.2"])?,
            CliCommand::FetchSounds {
                minecraft_version: "26.2".to_string(),
                all: false,
                force: false,
                jobs: None,
            }
        );
        assert_eq!(
            parse_cli_args([
                "fetch-sounds",
                "--version",
                "26.2",
                "--all",
                "--force",
                "--jobs",
                "4",
            ])?,
            CliCommand::FetchSounds {
                minecraft_version: "26.2".to_string(),
                all: true,
                force: true,
                jobs: Some(4),
            }
        );
        assert!(parse_cli_args(["fetch-sounds"]).is_err(), "--version is required");
        assert!(parse_cli_args(["fetch-sounds", "--version", "26.2", "--jobs", "0"]).is_err());
        assert!(parse_cli_args(["fetch-sounds", "--version", "26.2", "--jobs", "x"]).is_err());
        assert!(parse_cli_args(["fetch-sounds", "--nope"]).is_err());
        assert!(root_help().contains("fetch-sounds"));
        Ok(())
    }

    /// The real 26.2 corpus plan, against numbers derived by an **independent**
    /// walk of the same two files (a throwaway Python pass over
    /// `asset-index-32.json` and `sounds.json`) rather than by running this code
    /// and writing down what it said.
    ///
    /// `#[ignore]`d because it needs a populated `.cache/mc/26.2`; an opted-in run
    /// with no cache is a failure with a named fix, never a silent pass.
    #[test]
    #[ignore = "requires .cache/mc/26.2 (cargo run -p xtask -- fetch-assets --version 26.2)"]
    fn the_real_26_2_corpus_matches_an_independently_derived_partition() -> Result<()> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let cache = root.join(".cache/mc/26.2");
        let index_path = find_cached_asset_index(&cache)?;
        let index_json: Value = serde_json::from_slice(&std::fs::read(&index_path)?)?;
        let index = index_json
            .get("objects")
            .and_then(|o| o.as_object())
            .ok_or_else(|| anyhow!("no objects map"))?;
        let hash = index["minecraft/sounds.json"]["hash"]
            .as_str()
            .ok_or_else(|| anyhow!("no hash"))?;
        let sounds =
            std::fs::read(cache.join("objects").join(&hash[0..2]).join(hash)).with_context(|| {
                "minecraft/sounds.json is not in the store; run: cargo run -p xtask -- \
                 fetch-assets --version 26.2"
            })?;

        let corpus = plan_sound_corpus(index, &sounds, false)?;
        assert_eq!(corpus.events, 1968);
        assert_eq!(corpus.wanted.len(), 4751);
        assert_eq!(corpus.wanted_bytes(), 80_139_855);
        assert_eq!(corpus.excluded.len(), 92);
        assert_eq!(corpus.excluded_bytes(), 293_228_876);
        assert_eq!(corpus.unreferenced.len(), 28);
        // Every excluded object is under music/ or records/ — the derivation is by
        // *event key*, so agreement with the path layout is a real cross-check
        // rather than a restatement.
        for (name, _) in &corpus.excluded {
            assert!(
                name.starts_with("minecraft/sounds/music/")
                    || name.starts_with("minecraft/sounds/records/"),
                "excluded by event key but not a music/records path: {name}"
            );
        }
        // And the six streamed ambience loops are on the *fetched* side, which is
        // the whole reason the policy is not vanilla's `stream: true` flag.
        assert!(
            corpus
                .wanted
                .iter()
                .any(|(name, _)| name == "minecraft/sounds/ambient/underwater/underwater_ambience.ogg")
        );

        let all = plan_sound_corpus(index, &sounds, true)?;
        assert_eq!(all.wanted.len(), 4843);
        assert_eq!(all.wanted_bytes(), 373_368_731);
        Ok(())
    }

    #[test]
    fn sha1_verification_accepts_match_and_rejects_mismatch() -> Result<()> {
        let workspace = fresh_test_workspace("sha1-verification")?;
        let path = workspace.join("hello.txt");
        std::fs::write(&path, b"hello")?;

        verify_sha1(&path, "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d")?;
        let error = verify_sha1(&path, "0000000000000000000000000000000000000000").unwrap_err();
        assert!(error.to_string().contains("SHA-1 mismatch"));
        assert!(error.to_string().contains("hello.txt"));
        Ok(())
    }

    #[test]
    fn valid_existing_asset_file_is_skipped_unless_forced() -> Result<()> {
        let workspace = fresh_test_workspace("asset-skip")?;
        let path = workspace.join("client.jar");
        std::fs::write(&path, b"hello")?;

        assert_eq!(
            download_decision(&path, "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d", false)?,
            DownloadDecision::SkipValid
        );
        assert_eq!(
            download_decision(&path, "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d", true)?,
            DownloadDecision::Download
        );
        assert_eq!(
            download_decision(&path, "0000000000000000000000000000000000000000", false)?,
            DownloadDecision::Download
        );
        Ok(())
    }

    #[test]
    fn packet_id_check_accepts_pristine_and_rejects_corrupted_file() -> Result<()> {
        let Some(report) = load_real_report()? else {
            return Ok(());
        };

        let workspace_root = fresh_test_workspace("packet-id-check")?;
        let report_dir = workspace_root.join(".cache/mc/26.2/generated/reports");
        std::fs::create_dir_all(&report_dir)?;
        std::fs::write(
            report_dir.join("packets.json"),
            std::fs::read_to_string(REAL_REPORT)?,
        )?;

        let generated_path = workspace_root.join(DEFAULT_PACKET_IDS_OUT);
        std::fs::create_dir_all(generated_path.parent().expect("generated path has parent"))?;
        std::fs::write(&generated_path, generate_packet_ids_source(&report)?)?;

        let pristine = check_packet_ids(
            &workspace_root,
            "26.2",
            776,
            Some(Path::new(DEFAULT_PACKET_IDS_OUT)),
            PacketSource::Mojang,
        )?;
        assert!(pristine.is_identical());

        let corrupted = std::fs::read_to_string(&generated_path)?.replace(
            "pub const PROTOCOL_VERSION: i32 = 776;",
            "pub const PROTOCOL_VERSION: i32 = 777;",
        );
        std::fs::write(&generated_path, corrupted)?;

        let drift = check_packet_ids(
            &workspace_root,
            "26.2",
            776,
            Some(Path::new(DEFAULT_PACKET_IDS_OUT)),
            PacketSource::Mojang,
        )?;
        assert!(!drift.is_identical());
        assert!(drift.summary.contains("packet_ids.rs is out of date"));
        assert!(drift.summary.contains("line 3"));
        assert!(drift.summary.contains("PROTOCOL_VERSION"));
        assert!(std::fs::read_to_string(&generated_path)?.contains("PROTOCOL_VERSION: i32 = 777"));
        Ok(())
    }

