// Packet CLI and connectedness tests.
    #[test]
    fn cli_parses_gen_packet_ids_command() -> Result<()> {
        let command = parse_cli_args([
            "gen-packet-ids",
            "--version",
            "26.2",
            "--protocol",
            "776",
            "--check",
            "--out",
            "crates/versions/26.2/src/generated/packet_ids.rs",
        ])?;

        assert_eq!(
            command,
            CliCommand::GenPacketIds {
                minecraft_version: "26.2".to_owned(),
                protocol_version: 776,
                check: true,
                out: Some(PathBuf::from(
                    "crates/versions/26.2/src/generated/packet_ids.rs"
                )),
                source: PacketSource::Mojang,
            }
        );
        Ok(())
    }

    #[test]
    fn cli_parses_fetch_assets_command() -> Result<()> {
        let command = parse_cli_args(["fetch-assets", "--version", "26.2", "--force"])?;

        assert_eq!(
            command,
            CliCommand::FetchAssets {
                minecraft_version: "26.2".to_owned(),
                force: true,
            }
        );
        Ok(())
    }

    #[test]
    fn cli_parses_fetch_version_command() -> Result<()> {
        let command = parse_cli_args(["fetch-version", "--version", "1.16.5", "--force"])?;

        assert_eq!(
            command,
            CliCommand::FetchVersion {
                minecraft_version: "1.16.5".to_owned(),
                force: true,
            }
        );
        Ok(())
    }

    #[test]
    fn cli_parses_gen_registries_command() -> Result<()> {
        let command = parse_cli_args([
            "gen-registries",
            "--version",
            "26.2",
            "--protocol",
            "776",
            "--out-dir",
            "crates/versions/26.2/src/generated",
            "--check",
            "--registries",
            "sound_event,particle_type,menu,item",
        ])?;

        assert_eq!(
            command,
            CliCommand::GenRegistries {
                options: GenRegistriesOptions {
                    minecraft_version: "26.2".to_owned(),
                    protocol_version: 776,
                    check: true,
                    out_dir: PathBuf::from("crates/versions/26.2/src/generated"),
                    registries: vec![
                        "minecraft:sound_event".to_owned(),
                        "minecraft:particle_type".to_owned(),
                        "minecraft:menu".to_owned(),
                        "minecraft:item".to_owned(),
                    ],
                }
            }
        );
        Ok(())
    }

    /// `sound_events`/`particle_types`/`menus`/`items`/`data_component_types`
    /// are game data, not protocol data, and the registry extraction moved
    /// their committed tables to `crates/lodestone-data/src/generated`
    /// without anyone updating this default -- `gen-registries` (and
    /// `conformance`'s registry step, which shares this default) kept
    /// pointing at the old `crates/versions/26.2/src/generated` location,
    /// which has not held these tables since. This asserts the default
    /// resolves to where the tables actually live now.
    #[test]
    fn gen_registries_default_out_dir_is_lodestone_data() -> Result<()> {
        let command = parse_cli_args(["gen-registries", "--version", "26.2", "--protocol", "776"])?;

        let CliCommand::GenRegistries { options } = command else {
            panic!("expected GenRegistries, got {command:?}");
        };
        assert_eq!(
            options.out_dir,
            PathBuf::from("crates/lodestone-data/src/generated")
        );
        Ok(())
    }

    #[test]
    fn cli_parses_conformance_command() -> Result<()> {
        let command = parse_cli_args([
            "conformance",
            "--family",
            "v735",
            "--minecraft",
            "1.16.5",
            "--protocol",
            "754",
            "--source",
            "minecraft-data",
        ])?;

        assert_eq!(
            command,
            CliCommand::Conformance {
                options: ConformanceOptions {
                    family: "v735".to_owned(),
                    minecraft_version: "1.16.5".to_owned(),
                    protocol_version: 754,
                    source: PacketSource::MinecraftData,
                    skip_cargo: false,
                }
            }
        );
        Ok(())
    }

    #[test]
    fn cli_parses_check_connected_command() -> Result<()> {
        let command = parse_cli_args([
            "check-connected",
            "--allowlist",
            "xtask/custom-connected.toml",
        ])?;

        assert_eq!(
            command,
            CliCommand::CheckConnected {
                allowlist: PathBuf::from("xtask/custom-connected.toml"),
            }
        );
        Ok(())
    }

    #[test]
    fn cli_parses_connectedness_command() -> Result<()> {
        assert_eq!(
            parse_cli_args(["connectedness"])?,
            CliCommand::Connectedness
        );
        Ok(())
    }

    #[test]
    fn packet_id_play_counts_use_nested_play_modules() -> Result<()> {
        let source = r#"
pub mod login {
    pub mod clientbound {
        pub const LOGIN: i32 = 0;
        pub static ENTRIES: &[(&str, i32)] = &[("minecraft:login", LOGIN)];
    }
}
pub mod play {
    pub mod clientbound {
        pub const CHAT: i32 = 0;
        pub const BLOCK_UPDATE: i32 = 1;
        pub static ENTRIES: &[(&str, i32)] = &[
            ("minecraft:chat", CHAT),
            ("minecraft:block_update", BLOCK_UPDATE),
        ];
    }
    pub mod serverbound {
        pub const CHAT: i32 = 0;
        pub static ENTRIES: &[(&str, i32)] = &[("minecraft:chat", CHAT)];
    }
}
"#;

        let ids = parse_play_packet_id_summary(source)?;
        assert_eq!(ids.clientbound.len(), 2);
        assert_eq!(ids.serverbound.len(), 1);
        assert!(
            ids.clientbound
                .iter()
                .any(|packet| packet.const_name == "CHAT")
        );
        Ok(())
    }

    #[test]
    fn connectedness_classifier_ground_truths_direct_delegated_world_and_stranded() -> Result<()> {
        let adapter = r#"
fn handle_add_entity(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    Ok(vec![Directive::Emit(ClientEvent::EntitySpawned { id: 1 })])
}

fn handle_play(
    &self,
    world: &mut dyn WorldSink,
    packet_id: i32,
    payload: &[u8],
) -> Result<Vec<Directive>, AdapterError> {
    if packet_id == play::clientbound::SYSTEM_CHAT {
        return Ok(vec![Directive::Emit(ClientEvent::Chat { text })]);
    }
    if packet_id == play::clientbound::ADD_ENTITY {
        return handle_add_entity(payload);
    }
    if packet_id == play::clientbound::BLOCK_UPDATE {
        world.set_block(pos, state);
        return Ok(Vec::new());
    }
    if packet_id == play::clientbound::SET_OBJECTIVE {
        decode_and_validate::<SetObjective>(payload)?;
        return Ok(Vec::new());
    }
    if packet_id == play::clientbound::MYSTERY {
        return parse_mystery(payload);
    }
    Ok(Vec::new())
}
"#;

        let functions = extract_functions(adapter)?;
        let arms = classify_clientbound_dispatch(adapter, &functions, "src/adapter.rs", 4)?;
        assert_eq!(
            arms.get("SYSTEM_CHAT").map(|arm| &arm.verdict),
            Some(&ClientboundVerdict::Emits {
                outlet: ConsumerOutlet::ClientEvent,
                via: None,
            })
        );
        assert_eq!(
            arms.get("ADD_ENTITY").map(|arm| &arm.verdict),
            Some(&ClientboundVerdict::Emits {
                outlet: ConsumerOutlet::ClientEvent,
                via: Some("handle_add_entity".to_owned()),
            })
        );
        assert_eq!(
            arms.get("BLOCK_UPDATE").map(|arm| &arm.verdict),
            Some(&ClientboundVerdict::Emits {
                outlet: ConsumerOutlet::WorldSink,
                via: None,
            })
        );
        assert_eq!(
            arms.get("SET_OBJECTIVE").map(|arm| &arm.verdict),
            Some(&ClientboundVerdict::DecodedButStranded)
        );
        assert!(matches!(
            arms.get("MYSTERY").map(|arm| &arm.verdict),
            Some(ClientboundVerdict::Unclassified { .. })
        ));
        assert!(
            arms.len() >= 5,
            "anti-vacuity: classifier saw {}",
            arms.len()
        );
        Ok(())
    }

    #[test]
    fn serverbound_connectedness_recognises_a_pre_dispatch_gate() -> Result<()> {
        let dispatch = r#"
fn dispatch(packet: ServerBound) {
    if let ServerBound::TeleportationAccepted { id } = packet {
        acknowledgements.accepts(id);
        return;
    }
    match packet {
        ServerBound::TeleportationAccepted { .. } | ServerBound::Ignored => {}
    }
}
"#;
        assert!(
            serverbound_variant_is_connected(dispatch, "TeleportationAccepted")?,
            "the acknowledgement's early return mutates the movement gate"
        );
        assert!(
            !serverbound_variant_is_connected(dispatch, "Ignored")?,
            "control: the empty exhaustiveness arm is not a consumer"
        );
        Ok(())
    }

    /// `delegate_function_calls` scans raw source text (comments included --
    /// it has none of `find_outside_comments`/`matching_brace`'s comment
    /// awareness) for `identifier(` calls by walking backward from each `(`
    /// with `str::rfind` to find the start of the identifier. `rfind` hands
    /// back the **byte** index where the matching (non-identifier) character
    /// *starts*, and the old code did `idx + 1` to step past it -- correct
    /// only if that character is one byte (ASCII). A multi-byte character
    /// sitting directly against an identifier, with no space between (the
    /// shape a comment like `note—decode(payload)` takes), makes `idx + 1`
    /// land mid-character, and the subsequent `body[name_start..name_end]`
    /// panics with "byte index N is not a char boundary".
    ///
    /// This is a distinct bug from the lifetime-vs-char-literal one fixed in
    /// `e164d06` (`char_literal_span`): that one was in the three
    /// comment/string scanners and is already repaired. This one is in the
    /// unrelated identifier-boundary arithmetic here, still `idx + 1`, and it
    /// is exactly the class CLAUDE.md's evidence standard requires a test
    /// that visibly fails before the fix for. Em dash (3 bytes), `é` (2
    /// bytes), and `中` (3 bytes) are the minimal non-ASCII set: pure ASCII
    /// input cannot exercise a char-boundary bug at all.
    #[test]
    fn delegate_function_calls_does_not_panic_on_multibyte_characters_before_an_identifier() {
        let mut functions = BTreeMap::new();
        functions.insert("decode".to_owned(), FunctionBody { body: "" });
        for body in [
            "// note—decode(payload)\n",
            "// café—decode(payload)\n",
            "// 中—decode(payload)\n",
        ] {
            // Not just "does not panic": the identifier extraction must
            // still land on the right boundary and find the real call, or a
            // fix that merely avoided the panic (e.g. by giving up on the
            // whole line) would pass a vacuous version of this test.
            let delegates = delegate_function_calls(body, &functions);
            assert_eq!(
                delegates,
                vec!["decode".to_owned()],
                "wrong delegate extracted from {body:?}"
            );
        }
    }

    #[test]
    fn connectedness_report_uses_external_denominators_and_serverbound_encodes() -> Result<()> {
        let workspace = connectedness_fixture_workspace()?;

        let report = connectedness_report(&workspace)?;
        // v9 (the fixture's other family, with an empty adapter.rs and an
        // all-IGNORED packet_ids.rs) is measured too now that the hard
        // `family != "v770"` filter is gone — the whole point of job 1a.
        assert_eq!(
            report.families.iter().map(|f| f.family.as_str()).collect::<Vec<_>>(),
            vec!["v9", "26.2"]
        );
        assert!(
            report.skipped.is_empty(),
            "fixture families both have packet_ids.rs and adapter.rs: {:?}",
            report.skipped
        );
        let v9 = report
            .families
            .iter()
            .find(|family| family.family == "v9")
            .expect("v9 fixture family exists");
        assert_eq!(v9.play_clientbound_total, 1);
        assert_eq!(v9.examined_clientbound_arms, 0);
        assert_eq!(
            v9.serverbound_decode,
            ServerboundDecodeAxis::NotApplicable(
                "no src/server_protocol.rs — family does not implement ServerProtocol, so it \
                 cannot host"
                    .to_owned()
            )
        );

        let family = report
            .families
            .iter()
            .find(|family| family.family == "26.2")
            .expect("26.2 fixture family exists");
        assert_eq!(family.play_clientbound_total, 5);
        assert_eq!(family.play_clientbound_reaches_consumer, 3);
        assert_eq!(family.play_clientbound_decoded, 4);
        assert_eq!(family.play_clientbound_emits, 3);
        assert_eq!(
            family.play_clientbound_stranded_names,
            vec!["SET_OBJECTIVE".to_owned()]
        );
        assert_eq!(family.play_serverbound_total, 2);
        assert_eq!(family.play_serverbound_encoded, 1);
        assert_eq!(family.examined_clientbound_arms, 5);
        assert_eq!(family.unclassified.len(), 1);
        // This fixture's v770 has no src/server_protocol.rs either — the
        // fixture predates job 1b and is intentionally left that way so this
        // test still isolates the clientbound axis. The serverbound-decode
        // axis has its own fixture and tests below.
        assert_eq!(
            family.serverbound_decode,
            ServerboundDecodeAxis::NotApplicable(
                "no src/server_protocol.rs — family does not implement ServerProtocol, so it \
                 cannot host"
                    .to_owned()
            )
        );
        assert!(report.render().contains(
            "26.2  clientbound decoded 4/5; emits 3/5; decoded-but-stranded 1 [SET_OBJECTIVE]"
        ));
        assert!(!report.render().contains("consumed"));
        Ok(())
    }

    #[test]
    fn connectedness_report_names_families_it_could_not_scan_instead_of_dropping_them() -> Result<()>
    {
        let workspace = connectedness_fixture_workspace()?;
        // A third family directory matching the `vNN` naming convention but
        // missing `adapter.rs` — standing in for a family that has
        // bit-rotted past scannability. Before job 1a this test would have
        // been moot (only the flagship family was ever scanned); now every family directory
        // is examined, so a family that can't be measured has to say so.
        std::fs::create_dir_all(workspace.join("crates/versions/v5/src/generated"))?;
        std::fs::write(
            workspace.join("crates/versions/v5/src/generated/packet_ids.rs"),
            "pub mod play { pub mod clientbound { pub const X: i32 = 0; pub static ENTRIES: &[(&str, i32)] = &[(\"minecraft:x\", X)]; } pub mod serverbound { pub const X: i32 = 0; pub static ENTRIES: &[(&str, i32)] = &[(\"minecraft:x\", X)]; } }",
        )?;

        let report = connectedness_report(&workspace)?;
        assert_eq!(
            report
                .families
                .iter()
                .map(|f| f.family.as_str())
                .collect::<Vec<_>>(),
            vec!["v9", "26.2"],
            "v5 has no adapter.rs and must be named as skipped, not silently absent"
        );
        assert_eq!(report.skipped.len(), 1);
        assert_eq!(report.skipped[0].0, "v5");
        assert!(
            report.skipped[0].1.contains("adapter.rs"),
            "skip reason should name the missing file: {}",
            report.skipped[0].1
        );
        assert!(report.render().contains("SKIPPED"));
        assert!(report.render().contains("v5"));
        Ok(())
    }

    /// An adapter that dispatches through an indirection neither arm reader
    /// recognises must fail loudly, not score zero.
    ///
    /// Both readers match on spelling — the if-chain one on the literal
    /// `if packet_id ==`, the table one on a literal `Handler::new(` with the
    /// resource-name literal within the preceding 400 bytes — and neither is
    /// a property of a correct adapter. So an adapter can be entirely correct
    /// and completely unreadable here, and the output for that is
    /// `decoded 0/N`: indistinguishable from a family that decodes nothing,
    /// which is the exact defect this subcommand exists to report. It has
    /// happened twice in this repo, for two different spellings.
    ///
    /// The fixture below is the second of those: entries built through a
    /// helper rather than by writing the call literally.
    #[test]
    fn an_adapter_whose_dispatch_this_scanner_cannot_read_fails_instead_of_scoring_zero()
    -> Result<()> {
        let workspace = connectedness_fixture_workspace()?;
        // Overwrite the flagship family's adapter with a table whose entries
        // never spell `Handler::new(` — the shape that measured 0/122 while
        // being correct.
        std::fs::write(
            workspace.join("crates/versions/26.2/src/adapter.rs"),
            r#"
const fn entry(name: &'static str, id: i32, f: DecodeFn) -> (&'static str, Handler) {
    (name, Handler::new(id..=id, f))
}

fn handle_system_chat(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    Ok(vec![Directive::Emit(ClientEvent::Chat { text: String::new() })])
}

static CLIENTBOUND: &[(&str, Handler)] = &[
    entry("minecraft:system_chat", 0, handle_system_chat),
];

fn table() -> dispatch::Table {
    dispatch::Table::new(CLIENTBOUND)
}
"#,
        )?;

        let error = connectedness_report(&workspace)
            .expect_err("a family with dispatch evidence and no readable arms must fail");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("no dispatch arm could be parsed"),
            "the failure must say the scanner could not read the adapter: {rendered}"
        );
        assert!(
            rendered.contains("26.2"),
            "the failure must name the family so it is actionable: {rendered}"
        );
        Ok(())
    }

    /// The negative half of the control above: an adapter with no dispatch
    /// evidence at all still reports zero rather than failing.
    ///
    /// Without this, the guard could be satisfied by refusing every empty
    /// family, which would make it fire on a family that genuinely has not
    /// been written yet — a false alarm in place of a false zero, no better.
    /// The fixture's own `v9` family is exactly that case: one clientbound id
    /// and a deliberately empty `adapter.rs`.
    #[test]
    fn an_empty_adapter_still_scores_zero_rather_than_failing() -> Result<()> {
        let workspace = connectedness_fixture_workspace()?;
        let report = connectedness_report(&workspace)?;
        let v9 = report
            .families
            .iter()
            .find(|f| f.family == "v9")
            .expect("v9 is scanned");
        assert_eq!(v9.examined_clientbound_arms, 0);
        assert_eq!(v9.play_clientbound_decoded, 0);
        Ok(())
    }

    /// Positive control for the `src/adapter/` directory-module shape
    /// (26.2's actual layout, split across `mod.rs` + submodules such as
    /// `chat.rs`). Before this was handled, `connectedness_report` only ever
    /// looked for a flat `src/adapter.rs`, so any family shaped like this —
    /// 26.2 included, once its adapter grew past one file — was silently
    /// SKIPPED rather than scanned: the tool's own stated purpose
    /// ("Report v770 play packet reachability") was unmet by exactly the
    /// family it exists to check. This fixture also exercises cross-file
    /// delegate-following: `mod.rs`'s dispatch arm calls a helper defined in
    /// a sibling submodule, which only resolves if the functions table is
    /// built across every file in the module rather than one at a time.
    #[test]
    fn connectedness_scans_a_directory_module_adapter_and_follows_cross_file_delegates()
    -> Result<()> {
        let workspace = fresh_test_workspace("connectedness-dir-adapter")?;
        let root = workspace.deref();
        let family = root.join("crates/versions/v771");
        std::fs::create_dir_all(family.join("src/generated"))?;
        std::fs::write(
            family.join("src/generated/packet_ids.rs"),
            r#"
pub mod play {
    pub mod clientbound {
        pub const SYSTEM_CHAT: i32 = 0;
        pub static ENTRIES: &[(&str, i32)] = &[("minecraft:system_chat", SYSTEM_CHAT)];
    }
    pub mod serverbound {
        pub const CHAT: i32 = 0;
        pub static ENTRIES: &[(&str, i32)] = &[("minecraft:chat", CHAT)];
    }
}
"#,
        )?;
        std::fs::create_dir_all(family.join("src/adapter"))?;
        // mod.rs's dispatch arm delegates to a helper it does not itself
        // define -- `handle_system_chat` lives in the sibling `chat.rs`.
        std::fs::write(
            family.join("src/adapter/mod.rs"),
            r#"
mod chat;
use chat::handle_system_chat;

fn handle_play(
    &self,
    world: &mut dyn WorldSink,
    packet_id: i32,
    payload: &[u8],
) -> Result<Vec<Directive>, AdapterError> {
    if packet_id == play::clientbound::SYSTEM_CHAT {
        return handle_system_chat(payload);
    }
    Ok(Vec::new())
}
"#,
        )?;
        std::fs::write(
            family.join("src/adapter/chat.rs"),
            r#"
fn handle_system_chat(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    Ok(vec![Directive::Emit(ClientEvent::Chat { text: String::new() })])
}
"#,
        )?;

        let report = connectedness_report(&workspace)?;
        assert!(
            report.skipped.is_empty(),
            "a directory-module adapter must not be skipped: {:?}",
            report.skipped
        );
        let family_report = report
            .families
            .iter()
            .find(|f| f.family == "v771")
            .expect("v771 must appear in the scanned families, not be silently dropped");
        assert_eq!(
            family_report.play_clientbound_emits, 1,
            "SYSTEM_CHAT's handler lives in a sibling submodule (chat.rs); if the functions \
             table were built per-file instead of across the whole adapter module, the \
             delegate-follow would fail to resolve it and this would be 0"
        );
        assert!(
            family_report.unclassified.is_empty(),
            "unclassified: {:?}",
            family_report.unclassified
        );
        Ok(())
    }

    #[test]
    fn match_arm_body_stops_at_top_level_comma_for_bare_expression_arms() {
        // The naive scanner this replaces (`find('{')` from the clientbound
        // classifier) would, on FOO's bare-expression arm, keep searching
        // and swallow BAR's entire braced body instead.
        let source = "State::Play if packet_id == play::serverbound::FOO => ServerBound::Ignored,\nState::Play if packet_id == play::serverbound::BAR => {\n    ServerBound::Bar { id: 1 }\n}\n";
        let arrow = source.find("=>").expect("first arrow");
        let (start, end) = match_arm_body(source, arrow + 2).expect("body found");
        let body = source[start..end].trim();
        assert_eq!(body, "ServerBound::Ignored");
        assert!(
            !body.contains("Bar"),
            "swallowed the next arm's body: {body:?}"
        );
    }

    #[test]
    fn match_arm_body_handles_braced_arms_and_nested_delimiters_before_the_comma() {
        let source = "=> { ServerBound::Foo { id: vec![1, 2].len() as i64 } },\nnext";
        let (start, end) = match_arm_body(source, 2).expect("body found");
        assert_eq!(
            source[start..end].trim(),
            "ServerBound::Foo { id: vec![1, 2].len() as i64 }"
        );

        let source2 = "=> call(a, (b, c), [d, e]),\nnext";
        let (start2, end2) = match_arm_body(source2, 2).expect("body found");
        assert_eq!(source2[start2..end2].trim(), "call(a, (b, c), [d, e])");
    }

    #[test]
    fn find_outside_comments_skips_matches_inside_comments_and_strings() {
        let source = "// ServerBound::Foo mentioned here should not count\nlet s = \"ServerBound::Foo also should not count\";\nServerBound::Foo { x } => real_body(),\n";
        let found = find_outside_comments(source, 0, "ServerBound::Foo").expect("real occurrence found");
        let snippet = &source[found..(found + 24).min(source.len())];
        assert!(
            snippet.contains("Foo { x }"),
            "found the wrong occurrence: {snippet:?}"
        );
    }

    #[test]
    fn classify_serverbound_decode_ground_truths_emits_always_ignored_and_unclassified()
    -> Result<()> {
        let source = r#"
fn decode_mystery_action(payload: &[u8]) -> ServerBound {
    match decode_full::<MysteryAction>(payload) {
        Some(m) => ServerBound::MysteryAction { id: m.id },
        None => ServerBound::Ignored,
    }
}

impl ServerProtocol for V999ServerProtocol {
    fn decode(&self, state: lodestone_core::State, packet_id: i32, payload: &[u8]) -> ServerBound {
        match state {
            State::Play if packet_id == play::serverbound::KEEP_ALIVE => {
                match decode_full::<KeepAlive>(payload) {
                    Some(k) => ServerBound::KeepAlive { id: k.id },
                    None => ServerBound::Ignored,
                }
            }
            State::Play if packet_id == play::serverbound::PING => ServerBound::Ignored,
            State::Play if packet_id == play::serverbound::MYSTERY_ACTION => {
                decode_mystery_action(payload)
            }
            State::Play if packet_id == play::serverbound::WEIRD => {
                external_helper(payload)
            }
            _ => ServerBound::Ignored,
        }
    }
}
"#;
        let arms = classify_serverbound_decode(source, 4)?;
        assert_eq!(
            arms.get("KEEP_ALIVE").map(|arm| &arm.verdict),
            Some(&ServerboundDecodeVerdict::Emits {
                variants: vec!["KeepAlive".to_owned()],
                via: None,
            })
        );
        assert_eq!(
            arms.get("PING").map(|arm| &arm.verdict),
            Some(&ServerboundDecodeVerdict::AlwaysIgnored)
        );
        assert_eq!(
            arms.get("MYSTERY_ACTION").map(|arm| &arm.verdict),
            Some(&ServerboundDecodeVerdict::Emits {
                variants: vec!["MysteryAction".to_owned()],
                via: Some("decode_mystery_action".to_owned()),
            })
        );
        assert!(matches!(
            arms.get("WEIRD").map(|arm| &arm.verdict),
            Some(ServerboundDecodeVerdict::Unclassified { .. })
        ));
        assert!(
            arms.len() >= 4,
            "anti-vacuity: classifier saw {}",
            arms.len()
        );
        Ok(())
    }

    #[test]
    fn classify_serverbound_decode_reports_depth_limited_when_cap_is_too_low() -> Result<()> {
        let source = r#"
fn inner(payload: &[u8]) -> ServerBound {
    ServerBound::Foo { id: 1 }
}
fn middle(payload: &[u8]) -> ServerBound {
    inner(payload)
}
impl ServerProtocol for V999ServerProtocol {
    fn decode(&self, state: lodestone_core::State, packet_id: i32, payload: &[u8]) -> ServerBound {
        match state {
            State::Play if packet_id == play::serverbound::CHAINED => {
                middle(payload)
            }
            _ => ServerBound::Ignored,
        }
    }
}
"#;
        let arms = classify_serverbound_decode(source, 1)?;
        match arms.get("CHAINED").map(|arm| &arm.verdict) {
            Some(ServerboundDecodeVerdict::Unclassified { depth_limited, .. }) => {
                assert!(*depth_limited, "expected the depth cap to be the reason");
            }
            other => panic!("expected a depth-limited unclassified verdict, got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn serverbound_decode_axis_detects_a_planted_stranded_variant() -> Result<()> {
        let workspace = serverbound_decode_fixture_workspace()?;
        let report = connectedness_report(&workspace)?;
        let family = report
            .families
            .iter()
            .find(|f| f.family == "v999")
            .expect("v999 fixture family exists");
        let ServerboundDecodeAxis::Measured(summary) = &family.serverbound_decode else {
            panic!(
                "expected a measured serverbound-decode axis, got {:?}",
                family.serverbound_decode
            );
        };
        assert_eq!(summary.total, 5);
        assert_eq!(summary.examined_arms, 4);
        assert_eq!(summary.decoded, 3);
        // The control: MYSTERY_ACTION decodes to a real ServerBound variant
        // whose only dispatch arm in server.rs is the empty `=> {}` group it
        // shares with `Ignored` — a planted island. If this assertion ever
        // passes with `connected == 2` (i.e. the planted island stops being
        // reported as stranded), the detector has gone blind.
        assert_eq!(summary.connected, 1);
        assert_eq!(summary.stranded_names, vec!["MYSTERY_ACTION".to_owned()]);
        assert_eq!(summary.always_ignored_names, vec!["PING".to_owned()]);
        assert_eq!(summary.unclassified.len(), 1);
        assert_eq!(summary.unclassified[0].packet, "WEIRD");
        assert!(report.has_unclassified());
        assert_eq!(report.unclassified_count(), 1);
        assert!(report.render().contains("serverbound decoded 3/5, connected 1/5"));
        assert!(report.render().contains("decode-but-stranded 1 [MYSTERY_ACTION]"));
        Ok(())
    }
