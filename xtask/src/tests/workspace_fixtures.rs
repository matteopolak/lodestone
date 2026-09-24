// Shared workspace fixtures and packet report tests.
    fn fresh_test_workspace(name: &str) -> Result<TestWorkspace> {
        // Keep fixtures on the same filesystem as before (under the crate's
        // `target/`) rather than `$TMPDIR`, but give each run a unique mkdtemp
        // leaf instead of a fixed reused name. Only the shared *parent* is
        // created here (idempotent, race-free); the unique leaf is created
        // atomically by `tempdir_in`, so there is no delete-then-recreate window.
        let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-workspaces");
        std::fs::create_dir_all(&parent)
            .with_context(|| format!("create fixture parent {}", parent.display()))?;
        let dir = tempfile::Builder::new()
            .prefix(&format!("{name}-"))
            .tempdir_in(&parent)
            .with_context(|| format!("create temp workspace for {name}"))?;
        Ok(TestWorkspace { dir })
    }

    fn isolation_fixture(name: &str, crates: &[(&str, &str, &str)]) -> Result<TestWorkspace> {
        let workspace = fresh_test_workspace(name)?;
        let root = workspace.deref();
        let members = crates
            .iter()
            .map(|(path, _, _)| format!("\"{path}\""))
            .collect::<Vec<_>>()
            .join(", ");
        std::fs::write(
            root.join("Cargo.toml"),
            format!("[workspace]\nresolver = \"3\"\nmembers = [{members}]\n"),
        )?;

        for (path, crate_name, extra_manifest) in crates {
            let manifest_dir = root.join(path);
            std::fs::create_dir_all(&manifest_dir)?;
            std::fs::create_dir_all(manifest_dir.join("src"))?;
            std::fs::write(manifest_dir.join("src/lib.rs"), "")?;
            std::fs::write(
                manifest_dir.join("Cargo.toml"),
                format!(
                    "[package]\nname = \"{crate_name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n{extra_manifest}\n"
                ),
            )?;
        }

        Ok(workspace)
    }

    fn connected_fixture(
        name: &str,
        roots: &[(&str, &str, bool, &str)],
        crates: &[(&str, &str, bool, &str)],
        allowlist: &str,
    ) -> Result<TestWorkspace> {
        let workspace = fresh_test_workspace(name)?;
        let all = roots.iter().chain(crates.iter()).collect::<Vec<_>>();
        let members = all
            .iter()
            .map(|(path, _, _, _)| format!("\"{path}\""))
            .collect::<Vec<_>>()
            .join(", ");
        std::fs::write(
            workspace.join("Cargo.toml"),
            format!("[workspace]\nresolver = \"3\"\nmembers = [{members}]\n"),
        )?;

        for (path, crate_name, has_bin, extra_manifest) in all {
            let manifest_dir = workspace.join(path);
            std::fs::create_dir_all(manifest_dir.join("src"))?;
            let target_section = if *has_bin {
                std::fs::write(manifest_dir.join("src/main.rs"), "fn main() {}\n")?;
                ""
            } else {
                std::fs::write(manifest_dir.join("src/lib.rs"), "")?;
                ""
            };
            std::fs::write(
                manifest_dir.join("Cargo.toml"),
                format!(
                    "[package]\nname = \"{crate_name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n{target_section}{extra_manifest}\n"
                ),
            )?;
        }

        if !allowlist.trim().is_empty() {
            let allowlist_path = workspace.join(DEFAULT_CONNECTED_ALLOWLIST);
            std::fs::create_dir_all(allowlist_path.parent().expect("allowlist path has parent"))?;
            std::fs::write(allowlist_path, allowlist)?;
        }
        Ok(workspace)
    }

    fn connectedness_fixture_workspace() -> Result<TestWorkspace> {
        let workspace = fresh_test_workspace("connectedness")?;
        let root = workspace.deref();
        std::fs::create_dir_all(root.join("crates/versions/v9/src/generated"))?;
        std::fs::write(root.join("crates/versions/v9/src/adapter.rs"), "")?;
        std::fs::write(
            root.join("crates/versions/v9/src/generated/packet_ids.rs"),
            "pub mod play { pub mod clientbound { pub const IGNORED: i32 = 0; pub static ENTRIES: &[(&str, i32)] = &[(\"minecraft:ignored\", IGNORED)]; } pub mod serverbound { pub const IGNORED: i32 = 0; pub static ENTRIES: &[(&str, i32)] = &[(\"minecraft:ignored\", IGNORED)]; } }",
        )?;
        let family = root.join("crates/versions/26.2");
        std::fs::create_dir_all(family.join("src/generated"))?;
        std::fs::write(
            family.join("src/generated/packet_ids.rs"),
            r#"
pub mod play {
    pub mod clientbound {
        pub const SYSTEM_CHAT: i32 = 0;
        pub const ADD_ENTITY: i32 = 1;
        pub const BLOCK_UPDATE: i32 = 2;
        pub const SET_OBJECTIVE: i32 = 3;
        pub const MYSTERY: i32 = 4;
        pub static ENTRIES: &[(&str, i32)] = &[
            ("minecraft:system_chat", SYSTEM_CHAT),
            ("minecraft:add_entity", ADD_ENTITY),
            ("minecraft:block_update", BLOCK_UPDATE),
            ("minecraft:set_objective", SET_OBJECTIVE),
            ("minecraft:mystery", MYSTERY),
        ];
    }
    pub mod serverbound {
        pub const CHAT: i32 = 0;
        pub const MOVE: i32 = 1;
        pub static ENTRIES: &[(&str, i32)] = &[
            ("minecraft:chat", CHAT),
            ("minecraft:move", MOVE),
        ];
    }
}
"#,
        )?;
        std::fs::write(
            family.join("src/adapter.rs"),
            r#"
fn handle_add_entity(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    Ok(vec![Directive::Emit(ClientEvent::EntitySpawned { id: 1 })])
}

fn encode_action(action: ClientAction) -> Result<Option<(i32, Vec<u8>)>, AdapterError> {
    Ok(Some((play::serverbound::CHAT, Vec::new())))
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
"#,
        )?;
        Ok(workspace)
    }

    /// A family with a `src/server_protocol.rs` (so `ServerboundDecodeAxis`
    /// is `Measured` rather than `NotApplicable`) plus a minimal
    /// `crates/lodestone-server/src/server.rs` for the second-hop join.
    ///
    /// Deliberately plants one **known** island — `MYSTERY_ACTION` decodes
    /// to a real `ServerBound::MysteryAction` variant, but the only arm
    /// handling that variant in `server.rs` is the empty `=> {}` group it
    /// shares with `Ignored`. This is the control the job's own writeup
    /// demands: a coverage tool that cannot detect a planted island is
    /// worthless, so [`serverbound_decode_axis_detects_a_planted_stranded_variant`]
    /// asserts the exact reported numbers, not just "some islands exist."
    ///
    /// Also plants the naive-scanner failure mode `match_arm_body` exists to
    /// fix: `PING`'s arm is a bare, unbraced expression immediately
    /// followed by `MYSTERY_ACTION`'s braced arm. A `find('{')`-based
    /// scanner would swallow `MYSTERY_ACTION`'s whole body as if it were
    /// `PING`'s.
    fn serverbound_decode_fixture_workspace() -> Result<TestWorkspace> {
        let workspace = fresh_test_workspace("serverbound-decode")?;
        let root = workspace.deref();
        let family = root.join("crates/versions/v999");
        std::fs::create_dir_all(family.join("src/generated"))?;
        std::fs::write(
            family.join("src/generated/packet_ids.rs"),
            r#"
pub mod play {
    pub mod clientbound {
        pub const NOOP: i32 = 0;
        pub static ENTRIES: &[(&str, i32)] = &[("minecraft:noop", NOOP)];
    }
    pub mod serverbound {
        pub const KEEP_ALIVE: i32 = 0;
        pub const PING: i32 = 1;
        pub const MYSTERY_ACTION: i32 = 2;
        pub const WEIRD: i32 = 3;
        pub const UNHANDLED: i32 = 4;
        pub static ENTRIES: &[(&str, i32)] = &[
            ("minecraft:keep_alive", KEEP_ALIVE),
            ("minecraft:ping", PING),
            ("minecraft:mystery_action", MYSTERY_ACTION),
            ("minecraft:weird", WEIRD),
            ("minecraft:unhandled", UNHANDLED),
        ];
    }
}
"#,
        )?;
        std::fs::write(family.join("src/adapter.rs"), "")?;
        std::fs::write(
            family.join("src/server_protocol.rs"),
            r#"
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

fn decode_mystery_action(payload: &[u8]) -> ServerBound {
    match decode_full::<MysteryAction>(payload) {
        Some(m) => ServerBound::MysteryAction { id: m.id },
        None => ServerBound::Ignored,
    }
}
"#,
        )?;

        let server = root.join("crates/lodestone-server/src");
        std::fs::create_dir_all(&server)?;
        std::fs::write(
            server.join("server.rs"),
            r#"
/// Dispatches a decoded [`ServerBound::KeepAlive`] request — this doc
/// comment itself mentions the variant so a non-comment-aware scanner would
/// find this line first and get confused about where the real arm is.
fn dispatch(x: ServerBound) {
    match x {
        ServerBound::KeepAlive { id } => {
            respond(id);
        }
        ServerBound::MysteryAction { .. } | ServerBound::Ignored => {}
    }
}
"#,
        )?;

        Ok(workspace)
    }

    fn new_version_fixture_workspace() -> Result<TestWorkspace> {
        let workspace = fresh_test_workspace("new-version-shape-review")?;
        let root = workspace.deref();
        std::fs::write(
            root.join("Cargo.toml"),
            r#"[workspace]
resolver = "3"
members = ["crates/versions/*", "crates/lodestone-registry"]

[workspace.package]
version = "0.1.0"
edition = "2024"
license = "GPL-3.0-or-later"

[workspace.dependencies]
lodestone-v1 = { path = "crates/versions/v1" }
"#,
        )?;

        let v1 = root.join("crates/versions/v1");
        std::fs::create_dir_all(v1.join("src/generated"))?;
        std::fs::create_dir_all(v1.join("tests"))?;
        std::fs::write(
            v1.join("Cargo.toml"),
            r#"[package]
name = "lodestone-v1"
version.workspace = true
edition.workspace = true
license.workspace = true
"#,
        )?;
        std::fs::write(
            v1.join("src/generated/packet_ids.rs"),
            "pub const PROTOCOL_VERSION: i32 = 1;\npub const MINECRAFT_VERSION: &str = \"source\";\n",
        )?;
        std::fs::write(v1.join("src/adapter.rs"), "pub const PROTOCOL: i32 = 1;\n")?;
        std::fs::write(v1.join("src/lib.rs"), "pub mod adapter;\n")?;
        std::fs::write(
            v1.join("tests/live_chunk.rs"),
            "#[test] fn cloned_live_gate() {}\n",
        )?;

        let registry = root.join("crates/lodestone-registry");
        std::fs::create_dir_all(registry.join("src"))?;
        std::fs::write(
            registry.join("Cargo.toml"),
            r#"[package]
name = "lodestone-registry"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
lodestone-v1 = { workspace = true, optional = true }

[features]
v1 = ["dep:lodestone-v1"]
"#,
        )?;
        std::fs::write(
            registry.join("src/lib.rs"),
            "const FAMILIES: &[&str] = &[\n    #[cfg(feature = \"v1\")]\n    \"v1\",\n];\n",
        )?;

        let pc = root.join("vendor/minecraft-data/data/pc");
        std::fs::create_dir_all(pc.join("source"))?;
        std::fs::create_dir_all(pc.join("target"))?;
        std::fs::write(
            pc.join("source/version.json"),
            r#"{"minecraftVersion":"source","version":1}"#,
        )?;
        std::fs::write(
            pc.join("target/version.json"),
            r#"{"minecraftVersion":"target","version":2}"#,
        )?;
        std::fs::write(
            pc.join("source/protocol.json"),
            minecraft_data_protocol_fixture("old_field"),
        )?;
        std::fs::write(
            pc.join("target/protocol.json"),
            minecraft_data_protocol_fixture("new_field"),
        )?;

        Ok(workspace)
    }

    fn minecraft_data_protocol_fixture(field_name: &str) -> String {
        format!(
            r#"{{
  "play": {{
    "toClient": {{
      "types": {{
        "packet": ["container", [
          {{"name": "name", "type": ["mapper", {{"mappings": {{"0x00": "map_chunk"}}}}]}}
        ]],
        "packet_map_chunk": ["container", [
          {{"name": "{field_name}", "type": "varint"}}
        ]]
      }}
    }}
  }}
}}"#
        )
    }

    #[test]
    fn first_party_manifest_license_control_rejects_non_gpl_declarations() -> Result<()> {
        let workspace = fresh_test_workspace("first-party-license")?;
        std::fs::write(
            workspace.join("Cargo.toml"),
            r#"[workspace.package]
license = "GPL-3.0-or-later"
"#,
        )?;
        let crate_dir = workspace.join("crates/lodestone-control");
        std::fs::create_dir_all(&crate_dir)?;
        std::fs::write(
            crate_dir.join("Cargo.toml"),
            r#"# Third-party prose may say license = "MIT OR Apache-2.0".
[package]
name = "lodestone-control"
license = "MIT OR Apache-2.0"

[dependencies]
license = { package = "third-party", version = "1" }
"#,
        )?;
        let missing_dir = workspace.join("crates/lodestone-missing-license");
        std::fs::create_dir_all(&missing_dir)?;
        std::fs::write(
            missing_dir.join("Cargo.toml"),
            r#"[package]
name = "lodestone-missing-license"
"#,
        )?;

        let violations = first_party_manifest_license_violations(workspace.deref())?;
        assert_eq!(
            violations,
            vec![
                "crates/lodestone-control/Cargo.toml: GPL-3.0-or-later required, found MIT OR Apache-2.0",
                "crates/lodestone-missing-license/Cargo.toml: GPL-3.0-or-later required, found no license declaration",
            ]
        );
        Ok(())
    }

    #[test]
    fn first_party_manifest_licenses_match_the_workspace_license() -> Result<()> {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .canonicalize()
            .context("canonicalize workspace root")?;
        assert!(
            first_party_manifest_license_violations(&workspace_root)?.is_empty(),
            "first-party Cargo.toml license declarations must be GPL-3.0-or-later"
        );
        Ok(())
    }

    #[test]
    fn mojang_sourced_entries_are_their_own_canonical_name() -> Result<()> {
        // Mojang's own report names already are canonical: every entry from
        // this source should self-alias, with no lookup involved.
        let packet_report_json = r#"{
            "configuration": {"clientbound": {}, "serverbound": {}},
            "handshake": {"serverbound": {"minecraft:intention": {"protocol_id": 0}}},
            "login": {"clientbound": {}, "serverbound": {}},
            "play": {
                "clientbound": {"minecraft:set_health": {"protocol_id": 5}},
                "serverbound": {}
            },
            "status": {"clientbound": {}, "serverbound": {}}
        }"#;
        let report = parse_packet_report(packet_report_json, "test", 999)?;
        for entry in report.all_entries() {
            assert_eq!(entry.canonical_name.as_deref(), Some(entry.name.as_str()));
        }
        Ok(())
    }

    #[test]
    fn minecraft_data_sourced_entries_default_to_no_canonical_name() -> Result<()> {
        // MINECRAFT_DATA_CANONICAL_ALIASES is empty today (no fabricated
        // guesses), so a minecraft-data-sourced entry must come back with
        // canonical_name: None rather than inventing a mapping.
        let json = minecraft_data_protocol_fixture("count");
        let report = parse_minecraft_data_report(&json, "1.8.8", 47)?;
        let entry = report
            .entries(PacketState::Play, PacketBound::Clientbound)
            .find(|entry| entry.name == "minecraft:map_chunk")
            .expect("fixture declares minecraft:map_chunk");
        assert_eq!(entry.canonical_name, None);
        Ok(())
    }

    #[test]
    fn resolve_canonical_alias_matches_by_exact_name_only() {
        // Pairwise-distinct entries so a transposition between the "from"
        // and "to" columns, or between two table rows, cannot survive
        // unnoticed.
        let table: &[(&str, &str)] = &[
            ("minecraft:named_entity_spawn", "minecraft:add_entity"),
            ("minecraft:update_health", "minecraft:set_health"),
        ];
        assert_eq!(
            resolve_canonical_alias(table, "minecraft:named_entity_spawn"),
            Some("minecraft:add_entity")
        );
        assert_eq!(
            resolve_canonical_alias(table, "minecraft:update_health"),
            Some("minecraft:set_health")
        );
        assert_eq!(resolve_canonical_alias(table, "minecraft:unmapped"), None);
    }

    #[test]
    fn generate_packet_ids_source_emits_canonical_names_table() -> Result<()> {
        let packet_report_json = r#"{
            "configuration": {"clientbound": {}, "serverbound": {}},
            "handshake": {"serverbound": {"minecraft:intention": {"protocol_id": 0}}},
            "login": {"clientbound": {}, "serverbound": {}},
            "play": {
                "clientbound": {"minecraft:set_health": {"protocol_id": 5}},
                "serverbound": {}
            },
            "status": {"clientbound": {}, "serverbound": {}}
        }"#;
        let report = parse_packet_report(packet_report_json, "test", 999)?;
        let generated = generate_packet_ids_source(&report)?;
        assert!(generated.contains("pub static CANONICAL_NAMES"));
        assert!(generated.contains(r#"("minecraft:set_health", "minecraft:set_health")"#));
        assert!(generated.contains(r#"("minecraft:intention", "minecraft:intention")"#));
        Ok(())
    }
