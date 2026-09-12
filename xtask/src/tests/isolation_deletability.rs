// Isolation, deletability, and codegen tests.
    #[test]
    fn version_crate_may_depend_on_any_shared_crate() -> Result<()> {
        // The lint is expressed in terms of deletability, not an allowlist of
        // "blessed" shared crates. A version crate may depend on ANY version-free
        // shared crate (core/model/macros, but also world, net, ...) because
        // deleting the version folder never breaks a shared crate.
        let workspace = isolation_fixture(
            "version-depends-on-shared",
            &[
                ("crates/lodestone-core", "lodestone-core", ""),
                ("crates/lodestone-model", "lodestone-model", ""),
                ("crates/lodestone-macros", "lodestone-macros", ""),
                ("crates/lodestone-world", "lodestone-world", ""),
                ("crates/lodestone-net", "lodestone-net", ""),
                (
                    "crates/versions/v1",
                    "lodestone-v1",
                    r#"
[dependencies]
lodestone-core = { path = "../../lodestone-core" }
lodestone-model = { path = "../../lodestone-model" }
lodestone-macros = { path = "../../lodestone-macros" }
lodestone-world = { path = "../../lodestone-world" }
lodestone-net = { path = "../../lodestone-net" }
"#,
                ),
            ],
        )?;

        let report = check_workspace_isolation(&workspace)?;
        assert_eq!(report.findings, Vec::new());
        assert!(!report.has_violations());
        Ok(())
    }

    #[test]
    fn version_to_version_dependency_is_a_violation() -> Result<()> {
        let workspace = isolation_fixture(
            "version-crate-dependency",
            &[
                ("crates/versions/v1", "lodestone-v1", ""),
                (
                    "crates/versions/v2",
                    "lodestone-v2",
                    r#"
[dependencies]
lodestone-v1 = { path = "../v1" }
"#,
                ),
            ],
        )?;

        let report = check_workspace_isolation(&workspace)?;
        assert_eq!(
            report.findings,
            vec![IsolationFinding {
                crate_name: "lodestone-v2".to_owned(),
                dependency_name: "lodestone-v1".to_owned(),
                dependency_table: "dependencies",
                optional: false,
                rule: IsolationRule::VersionDependsOnVersion,
                severity: Severity::Violation,
                detail: None,
            }]
        );
        assert!(report.has_violations());
        Ok(())
    }

    #[test]
    fn required_shared_to_version_dependency_is_a_violation() -> Result<()> {
        // A shared crate with a *required* dependency on a version crate makes
        // that version undeletable, so it is fatal.
        let workspace = isolation_fixture(
            "required-shared-to-version",
            &[
                ("crates/versions/v1", "lodestone-v1", ""),
                (
                    "crates/lodestone-client",
                    "lodestone-client",
                    r#"
[dependencies]
lodestone-v1 = { path = "../versions/v1" }
"#,
                ),
            ],
        )?;

        let report = check_workspace_isolation(&workspace)?;
        assert_eq!(
            report.findings,
            vec![IsolationFinding {
                crate_name: "lodestone-client".to_owned(),
                dependency_name: "lodestone-v1".to_owned(),
                dependency_table: "dependencies",
                optional: false,
                rule: IsolationRule::SharedDependsOnVersion,
                severity: Severity::Violation,
                detail: None,
            }]
        );
        assert!(report.has_violations());
        Ok(())
    }

    #[test]
    fn optional_shared_to_version_dependency_is_a_surfaced_warning() -> Result<()> {
        // This models the real, deliberate wart: lodestone-client names a
        // concrete version crate through an OPTIONAL, feature-gated dependency
        // for its live-join test. The version is still deletable (drop the
        // folder plus the feature line), so this is surfaced, not fatal.
        let workspace = isolation_fixture(
            "optional-shared-to-version",
            &[
                ("crates/versions/v1", "lodestone-v1", ""),
                (
                    "crates/lodestone-client",
                    "lodestone-client",
                    r#"
[dependencies]
lodestone-v1 = { path = "../versions/v1", optional = true }

[features]
live-v1 = ["dep:lodestone-v1"]
"#,
                ),
            ],
        )?;

        let report = check_workspace_isolation(&workspace)?;
        assert_eq!(
            report.findings,
            vec![IsolationFinding {
                crate_name: "lodestone-client".to_owned(),
                dependency_name: "lodestone-v1".to_owned(),
                dependency_table: "dependencies",
                optional: true,
                rule: IsolationRule::SharedDependsOnVersion,
                severity: Severity::Warning,
                detail: None,
            }]
        );
        assert!(!report.has_violations());
        assert!(report.warning_summary().is_some());
        Ok(())
    }

    #[test]
    fn dev_shared_to_version_dependency_is_a_surfaced_warning() -> Result<()> {
        // A dev-only shared -> version edge only breaks the shared crate's
        // tests, not its build, so it is surfaced rather than fatal.
        let workspace = isolation_fixture(
            "dev-shared-to-version",
            &[
                ("crates/versions/v1", "lodestone-v1", ""),
                (
                    "crates/lodestone-client",
                    "lodestone-client",
                    r#"
[dev-dependencies]
lodestone-v1 = { path = "../versions/v1" }
"#,
                ),
            ],
        )?;

        let report = check_workspace_isolation(&workspace)?;
        assert_eq!(
            report.findings,
            vec![IsolationFinding {
                crate_name: "lodestone-client".to_owned(),
                dependency_name: "lodestone-v1".to_owned(),
                dependency_table: "dev-dependencies",
                optional: false,
                rule: IsolationRule::SharedDependsOnVersion,
                severity: Severity::Warning,
                detail: None,
            }]
        );
        assert!(!report.has_violations());
        Ok(())
    }

    #[test]
    fn check_isolation_allows_third_party_dependencies() -> Result<()> {
        let workspace = isolation_fixture(
            "third-party-dependencies",
            &[(
                "crates/versions/v1",
                "lodestone-v1",
                r#"
[dependencies]
serde_json = "1"

[build-dependencies]
anyhow = "1"
"#,
            )],
        )?;

        let report = check_workspace_isolation(&workspace)?;
        assert_eq!(report.findings, Vec::new());
        Ok(())
    }

    #[test]
    fn new_version_parser_defaults_from_v770_and_mojang() -> Result<()> {
        let command =
            parse_cli_args(["new-version", "--protocol", "340", "--minecraft", "1.12.2"])?;
        let CliCommand::NewVersion { options } = command else {
            panic!("expected NewVersion command");
        };
        assert_eq!(options.protocol, 340);
        assert_eq!(options.minecraft_version, "1.12.2");
        assert_eq!(options.from, "v770");
        assert_eq!(options.source, PacketSource::Mojang);
        assert_eq!(options.name, "v340");
        assert!(!options.force);
        Ok(())
    }

    #[test]
    fn new_version_parser_infers_minecraft_data_from_v47() -> Result<()> {
        let command = parse_cli_args([
            "new-version",
            "--protocol",
            "107",
            "--minecraft",
            "1.9",
            "--from",
            "v47",
        ])?;
        let CliCommand::NewVersion { options } = command else {
            panic!("expected NewVersion command");
        };
        // Copying from the legacy family selects the minecraft-data oracle.
        assert_eq!(options.source, PacketSource::MinecraftData);
        assert_eq!(options.from, "v47");
        Ok(())
    }

    #[test]
    fn new_version_parser_honours_explicit_name_and_source() -> Result<()> {
        let command = parse_cli_args([
            "new-version",
            "--protocol",
            "47",
            "--minecraft",
            "1.8",
            "--from",
            "v47",
            "--name",
            "v18",
            "--source",
            "mojang",
            "--force",
        ])?;
        let CliCommand::NewVersion { options } = command else {
            panic!("expected NewVersion command");
        };
        assert_eq!(options.name, "v18");
        assert_eq!(options.source, PacketSource::Mojang);
        assert!(options.force);
        Ok(())
    }

    #[test]
    fn capitalize_family_uppercases_leading_v() {
        assert_eq!(capitalize_family("v47"), "V47");
        assert_eq!(capitalize_family(""), "");
    }

    #[test]
    fn set_protocol_constant_rewrites_only_the_constant_line() -> Result<()> {
        let root = fresh_test_workspace("set-protocol-constant")?;
        let path = root.join("adapter.rs");
        std::fs::write(
            &path,
            "pub const PROTOCOL: i32 = 776;\nfn supports(p: i32) -> bool { p == PROTOCOL }\n",
        )?;
        set_protocol_constant(&path, 340)?;
        let rewritten = std::fs::read_to_string(&path)?;
        assert!(rewritten.contains("pub const PROTOCOL: i32 = 340;"));
        // The reference to PROTOCOL elsewhere is untouched.
        assert!(rewritten.contains("p == PROTOCOL"));
        assert!(!rewritten.contains("776"));
        Ok(())
    }

    #[test]
    fn shape_review_toml_records_specific_unreviewed_packet_deltas() -> Result<()> {
        let toml = render_shape_review_toml(&ShapeReviewManifest {
            source_family: "v340".to_owned(),
            target_family: "v735".to_owned(),
            source_minecraft_version: "1.12.2".to_owned(),
            source_protocol_version: 340,
            target_minecraft_version: "1.16.5".to_owned(),
            target_protocol_version: 754,
            entries: vec![PacketShapeChange {
                state: PacketState::Play,
                bound: PacketBound::Clientbound,
                packet_name: "minecraft:map_chunk".to_owned(),
                kind: PacketShapeChangeKind::Changed,
            }],
        })?;

        assert!(toml.contains("source_family = \"v340\""));
        assert!(toml.contains("target_family = \"v735\""));
        assert!(toml.contains("[[packet]]"));
        assert!(toml.contains("state = \"play\""));
        assert!(toml.contains("bound = \"clientbound\""));
        assert!(toml.contains("name = \"minecraft:map_chunk\""));
        assert!(toml.contains("change = \"changed\""));
        assert!(toml.contains("reviewed = false"));
        Ok(())
    }

    #[test]
    fn shape_review_guard_rejects_unreviewed_entries() -> Result<()> {
        let workspace = fresh_test_workspace("shape-review-guard")?;
        let family = workspace.join("crates/versions/v999");
        std::fs::create_dir_all(&family)?;
        std::fs::write(
            family.join("SHAPE_REVIEW.toml"),
            r#"source_family = "v1"
target_family = "v999"

[[packet]]
state = "play"
bound = "clientbound"
name = "minecraft:map_chunk"
change = "changed"
reviewed = false
"#,
        )?;

        let error = check_shape_reviews(&workspace).unwrap_err().to_string();
        assert!(error.contains("v999"), "{error}");
        assert!(error.contains("minecraft:map_chunk"), "{error}");
        assert!(error.contains("reviewed = true"), "{error}");
        Ok(())
    }

    #[test]
    fn copy_tree_refuses_to_clone_live_tests() -> Result<()> {
        let workspace = fresh_test_workspace("skip-live-tests")?;
        let source = workspace.join("from");
        let target = workspace.join("to");
        std::fs::create_dir_all(source.join("tests"))?;
        std::fs::create_dir_all(source.join("src"))?;
        std::fs::write(
            source.join("tests/live_chunk.rs"),
            "panic!(\"wrong server\");",
        )?;
        std::fs::write(source.join("tests/chunk.rs"), "#[test] fn hermetic() {}")?;
        std::fs::write(source.join("src/lib.rs"), "pub struct V1;")?;

        let mut created = Vec::new();
        copy_tree_with_substitutions(
            &source,
            &target,
            &[("V1".to_owned(), "V2".to_owned())],
            &workspace,
            &mut created,
        )?;

        assert!(!target.join("tests/live_chunk.rs").exists());
        assert!(target.join("tests/chunk.rs").exists());
        assert_eq!(
            std::fs::read_to_string(target.join("src/lib.rs"))?,
            "pub struct V2;"
        );
        Ok(())
    }

    #[test]
    fn new_version_writes_shape_review_and_skips_registry_until_reviewed() -> Result<()> {
        let workspace = new_version_fixture_workspace()?;

        let report = scaffold_new_version(
            &workspace,
            &NewVersionOptions {
                name: "v2".to_owned(),
                protocol: 2,
                minecraft_version: "target".to_owned(),
                source: PacketSource::MinecraftData,
                from: "v1".to_owned(),
                force: false,
            },
        )?;

        assert_eq!(report.shape_changes.len(), 1);
        let review =
            std::fs::read_to_string(workspace.join("crates/versions/v2/SHAPE_REVIEW.toml"))?;
        assert!(review.contains("name = \"minecraft:map_chunk\""));
        assert!(review.contains("reviewed = false"));
        assert!(
            workspace
                .join("crates/versions/v2/tests/shape_review.rs")
                .exists()
        );
        assert!(
            !workspace
                .join("crates/versions/v2/tests/live_chunk.rs")
                .exists()
        );

        let registry_manifest =
            std::fs::read_to_string(workspace.join("crates/lodestone-registry/Cargo.toml"))?;
        let registry_lib =
            std::fs::read_to_string(workspace.join("crates/lodestone-registry/src/lib.rs"))?;
        assert!(!registry_manifest.contains("lodestone-v2"));
        assert!(!registry_manifest.contains("v2 = [\"dep:lodestone-v2\"]"));
        assert!(!registry_lib.contains("label: \"v2\""));
        assert!(
            report
                .residue
                .iter()
                .any(|item| item.contains("registry wiring skipped"))
        );
        Ok(())
    }

    #[test]
    fn new_version_skips_registry_when_shape_diff_is_unavailable() -> Result<()> {
        let workspace = new_version_fixture_workspace()?;
        std::fs::write(
            workspace.join("crates/versions/v1/src/generated/packet_ids.rs"),
            "pub const PROTOCOL_VERSION: i32 = 1;\n",
        )?;

        let report = scaffold_new_version(
            &workspace,
            &NewVersionOptions {
                name: "v2".to_owned(),
                protocol: 2,
                minecraft_version: "target".to_owned(),
                source: PacketSource::MinecraftData,
                from: "v1".to_owned(),
                force: false,
            },
        )?;

        let registry_manifest =
            std::fs::read_to_string(workspace.join("crates/lodestone-registry/Cargo.toml"))?;
        assert!(!registry_manifest.contains("lodestone-v2"));
        assert!(
            report
                .residue
                .iter()
                .any(|item| item.contains("shape diff unavailable"))
        );
        assert!(
            report
                .residue
                .iter()
                .any(|item| item.contains("registry wiring skipped"))
        );
        Ok(())
    }

    #[test]
    fn codegen_ratio_counts_version_families_structurally() -> Result<()> {
        let workspace = fresh_test_workspace("codegen-ratio")?;
        let v1_src = workspace.join("crates/versions/v1/src");
        let v2_src = workspace.join("crates/versions/v2/src");
        std::fs::create_dir_all(v1_src.join("generated"))?;
        std::fs::create_dir_all(&v2_src)?;
        std::fs::write(
            v1_src.join("lib.rs"),
            "#[derive(Encode, Decode)]\nstruct GeneratedShape;\n\nimpl Encode for Manual {}\nimpl Decode for Manual {}\n",
        )?;
        std::fs::write(
            v1_src.join("generated/packet_ids.rs"),
            "pub const A: i32 = 1;\n",
        )?;
        std::fs::write(
            v2_src.join("lib.rs"),
            "#[derive(\n    Debug,\n    Decode,\n)]\nstruct MultiLine;\n",
        )?;

        let report = codegen_ratio_report(&workspace)?;
        assert_eq!(report.families.len(), 2);
        assert_eq!(
            report.families[0],
            CodegenRatioFamily {
                family: "v1".to_owned(),
                derive_blocks: 1,
                manual_impls: 2,
                generated_lines: 1,
                hand_written_lines: 5,
            }
        );
        assert_eq!(report.families[1].family, "v2");
        assert_eq!(report.families[1].derive_blocks, 1);
        assert_eq!(report.families[1].manual_impls, 0);
        assert_eq!(report.families[1].generated_lines, 0);
        assert_eq!(report.families[1].hand_written_lines, 5);

        let rendered = report.render();
        assert!(rendered.contains("per-struct ratio is optimistic"));
        assert!(rendered.contains("v1"));
        assert!(rendered.contains("hand-written"));
        Ok(())
    }

    #[test]
    fn registry_optional_version_dependency_is_by_design_aggregation() -> Result<()> {
        // The version registry opts in via metadata and names versions only
        // through optional, feature-gated edges. That is the intended
        // aggregation point, so it is reported as informational, never a warning
        // or a violation.
        let workspace = isolation_fixture(
            "registry-optional-version",
            &[
                ("crates/versions/v1", "lodestone-v1", ""),
                (
                    "crates/lodestone-registry",
                    "lodestone-registry",
                    r#"
[package.metadata.lodestone-isolation]
role = "version-registry"

[dependencies]
lodestone-v1 = { path = "../versions/v1", optional = true }

[features]
v1 = ["dep:lodestone-v1"]
"#,
                ),
            ],
        )?;

        let report = check_workspace_isolation(&workspace)?;
        assert_eq!(
            report.findings,
            vec![IsolationFinding {
                crate_name: "lodestone-registry".to_owned(),
                dependency_name: "lodestone-v1".to_owned(),
                dependency_table: "dependencies",
                optional: true,
                rule: IsolationRule::RegistryAggregatesVersion,
                severity: Severity::Info,
                detail: None,
            }]
        );
        assert!(!report.has_violations());
        assert!(report.warning_summary().is_none());
        assert!(report.info_summary().is_some());
        Ok(())
    }

    #[test]
    fn registry_cannot_aggregate_unreviewed_shape_family() -> Result<()> {
        let workspace = isolation_fixture(
            "registry-unreviewed-shapes",
            &[
                ("crates/versions/v2", "lodestone-v2", ""),
                (
                    "crates/lodestone-registry",
                    "lodestone-registry",
                    r#"
[package.metadata.lodestone-isolation]
role = "version-registry"

[dependencies]
lodestone-v2 = { path = "../versions/v2", optional = true }
"#,
                ),
            ],
        )?;
        std::fs::write(
            workspace.join("crates/versions/v2/SHAPE_REVIEW.toml"),
            r#"source_family = "v1"
target_family = "v2"

[[packet]]
state = "play"
bound = "clientbound"
name = "minecraft:map_chunk"
change = "changed"
reviewed = false
"#,
        )?;

        let report = check_workspace_isolation(&workspace)?;
        assert_eq!(report.findings.len(), 1);
        assert_eq!(
            report.findings[0].rule,
            IsolationRule::RegistryAggregatesUnreviewedVersion
        );
        assert_eq!(report.findings[0].severity, Severity::Violation);
        assert!(report.violation_summary().contains("minecraft:map_chunk"));
        Ok(())
    }

    #[test]
    fn check_connected_reports_orphan_chain_and_ignores_dev_dependencies() -> Result<()> {
        let workspace = connected_fixture(
            "connected-orphans",
            &[(
                "apps/lodestone",
                "lodestone-shell",
                true,
                r#"
[dev-dependencies]
lodestone-entity = { path = "../../crates/lodestone-entity" }
"#,
            )],
            &[
                (
                    "crates/lodestone-server",
                    "lodestone-server",
                    false,
                    r#"
[dependencies]
lodestone-worldgen = { path = "../lodestone-worldgen" }
"#,
                ),
                ("crates/lodestone-worldgen", "lodestone-worldgen", false, ""),
                ("crates/lodestone-entity", "lodestone-entity", false, ""),
            ],
            "",
        )?;

        let report = check_workspace_connected(&workspace)?;
        assert!(report.has_violations());
        let rendered = report.violation_summary();
        assert!(
            rendered.contains("lodestone-server is unreachable"),
            "{rendered}"
        );
        assert!(
            rendered.contains("lodestone-worldgen is unreachable; its non-dev workspace dependent lodestone-server is also unreachable"),
            "{rendered}"
        );
        assert!(
            rendered
                .contains("lodestone-entity is unreachable; it is only used by dev-dependencies"),
            "{rendered}"
        );
        Ok(())
    }

    #[test]
    fn check_connected_counts_optional_non_dev_dependencies_as_reachable() -> Result<()> {
        let workspace = connected_fixture(
            "connected-optional",
            &[(
                "apps/lodestone",
                "lodestone-shell",
                true,
                r#"
[dependencies]
lodestone-registry = { path = "../../crates/lodestone-registry" }
"#,
            )],
            &[
                (
                    "crates/lodestone-registry",
                    "lodestone-registry",
                    false,
                    r#"
[dependencies]
lodestone-v26-2 = { path = "../versions/26.2", optional = true }
"#,
                ),
                ("crates/versions/26.2", "lodestone-v26-2", false, ""),
            ],
            "",
        )?;

        let report = check_workspace_connected(&workspace)?;
        assert!(
            !report
                .violations()
                .any(|finding| finding.crate_name == "lodestone-v26-2"),
            "{}",
            report.violation_summary()
        );
        Ok(())
    }

    #[test]
    fn check_connected_requires_allowlist_reason_and_owner() -> Result<()> {
        let workspace = connected_fixture(
            "connected-allowlist",
            &[("apps/lodestone", "lodestone-shell", true, "")],
            &[("xtask", "xtask", true, "")],
            r#"
[[allow]]
crate = "xtask"
owner = "impl-xtask"
reason = "build tool, not shipped runtime artifact"
"#,
        )?;

        let report = check_workspace_connected(&workspace)?;
        assert!(!report.has_violations(), "{}", report.violation_summary());

        std::fs::write(
            workspace.join(DEFAULT_CONNECTED_ALLOWLIST),
            r#"
[[allow]]
crate = "xtask"
reason = ""
"#,
        )?;
        let error = check_workspace_connected(&workspace)
            .unwrap_err()
            .to_string();
        assert!(error.contains("owner"), "{error}");
        assert!(error.contains("reason"), "{error}");
        Ok(())
    }

    /// `conformance --family v340` used to run `check-connected` workspace-
    /// wide and unconditionally: an orphan crate belonging to an unrelated
    /// family (or anything else in the workspace) failed v340's conformance
    /// run even though v340 itself was perfectly fine -- a per-family tool
    /// held hostage to state outside its own subject, the mirror image of
    /// the docs-index gate that scanned three directories and not a fourth.
    ///
    /// Builds two protocol families, only one wired into the shipped root:
    /// `lodestone-v999` is reachable, `lodestone-v888` is a genuine orphan.
    /// Two things must both be true, or this is a skip path rather than a
    /// scope: family-scoped v999 must NOT see v888's violation (that is the
    /// fix), and family-scoped v888 must still see its OWN violation (so a
    /// subject that exists cannot come back "no findings" -- an errored or
    /// vacuous detector is the failure mode this whole audit exists to catch).
    #[test]
    fn check_connected_for_family_scopes_violations_to_the_named_family() -> Result<()> {
        let workspace = connected_fixture(
            "connected-family-scope",
            &[(
                "apps/lodestone",
                "lodestone-shell",
                true,
                r#"
[dependencies]
lodestone-v999 = { path = "../../crates/versions/v999" }
"#,
            )],
            &[
                ("crates/versions/v999", "lodestone-v999", false, ""),
                ("crates/versions/v888", "lodestone-v888", false, ""),
            ],
            "",
        )?;

        // Sanity precondition: the global, unscoped check must actually see
        // both crates' status, or the scoped assertions below prove nothing.
        let global = check_workspace_connected(&workspace)?;
        assert!(
            global
                .violations()
                .any(|finding| finding.crate_name == "lodestone-v888"),
            "{}",
            global.violation_summary()
        );
        assert!(
            !global
                .violations()
                .any(|finding| finding.crate_name == "lodestone-v999"),
            "{}",
            global.violation_summary()
        );

        // The fix: v999's own conformance run must not see v888's orphan.
        let scoped_to_v999 = check_workspace_connected_for_family(&workspace, "v999")?;
        assert!(
            !scoped_to_v999.has_violations(),
            "v999 must not be held hostage by v888's unrelated violation: {}",
            scoped_to_v999.violation_summary()
        );

        // Not a skip path: v888's own conformance run must still catch its
        // own real violation.
        let scoped_to_v888 = check_workspace_connected_for_family(&workspace, "v888")?;
        assert!(scoped_to_v888.has_violations());
        assert!(
            scoped_to_v888
                .violations()
                .any(|finding| finding.crate_name == "lodestone-v888"),
            "{}",
            scoped_to_v888.violation_summary()
        );
        Ok(())
    }

    #[test]
    fn registry_required_version_dependency_is_still_a_violation() -> Result<()> {
        // Safety valve: the registry role only downgrades OPTIONAL edges. A
        // *required* version dependency — even on the designated registry — would
        // make that version undeletable, so it stays fatal. This is what stops
        // the metadata marker from being abused to hide a real violation.
        let workspace = isolation_fixture(
            "registry-required-version",
            &[
                ("crates/versions/v1", "lodestone-v1", ""),
                (
                    "crates/lodestone-registry",
                    "lodestone-registry",
                    r#"
[package.metadata.lodestone-isolation]
role = "version-registry"

[dependencies]
lodestone-v1 = { path = "../versions/v1" }
"#,
                ),
            ],
        )?;

        let report = check_workspace_isolation(&workspace)?;
        assert_eq!(
            report.findings,
            vec![IsolationFinding {
                crate_name: "lodestone-registry".to_owned(),
                dependency_name: "lodestone-v1".to_owned(),
                dependency_table: "dependencies",
                optional: false,
                rule: IsolationRule::SharedDependsOnVersion,
                severity: Severity::Violation,
                detail: None,
            }]
        );
        assert!(report.has_violations());
        Ok(())
    }

    #[test]
    fn registry_role_does_not_exempt_other_crates() -> Result<()> {
        // The exemption is scoped to the crate that carries the metadata role. A
        // different shared crate with the same optional edge still gets a
        // surfaced warning, so stamping one crate as the registry cannot quiet
        // another crate's coupling.
        let workspace = isolation_fixture(
            "registry-scoped-exemption",
            &[
                ("crates/versions/v1", "lodestone-v1", ""),
                (
                    "crates/lodestone-registry",
                    "lodestone-registry",
                    r#"
[package.metadata.lodestone-isolation]
role = "version-registry"

[dependencies]
lodestone-v1 = { path = "../versions/v1", optional = true }

[features]
v1 = ["dep:lodestone-v1"]
"#,
                ),
                (
                    "crates/lodestone-client",
                    "lodestone-client",
                    r#"
[dependencies]
lodestone-v1 = { path = "../versions/v1", optional = true }

[features]
live-v1 = ["dep:lodestone-v1"]
"#,
                ),
            ],
        )?;

        let report = check_workspace_isolation(&workspace)?;
        // Registry edge is info; client edge is still a warning.
        assert!(
            report
                .infos()
                .any(|finding| finding.crate_name == "lodestone-registry")
        );
        let client_findings: Vec<_> = report
            .findings
            .iter()
            .filter(|finding| finding.crate_name == "lodestone-client")
            .collect();
        assert_eq!(client_findings.len(), 1);
        assert_eq!(client_findings[0].severity, Severity::Warning);
        assert_eq!(
            client_findings[0].rule,
            IsolationRule::SharedDependsOnVersion
        );
        assert!(!report.has_violations());
        assert!(report.warning_summary().is_some());
        Ok(())
    }

    #[test]
    fn check_deletable_treats_optional_dependent_as_clean() -> Result<()> {
        let workspace = isolation_fixture(
            "deletable-optional-dependent",
            &[
                ("crates/versions/v1", "lodestone-v1", ""),
                (
                    "crates/lodestone-client",
                    "lodestone-client",
                    r#"
[dependencies]
lodestone-v1 = { path = "../versions/v1", optional = true }

[features]
live-v1 = ["dep:lodestone-v1"]
"#,
                ),
            ],
        )?;

        let report = check_workspace_deletable(&workspace, "v1")?;
        assert_eq!(report.target_crate, "lodestone-v1");
        assert_eq!(report.target_dir, "crates/versions/v1");
        assert!(report.is_cleanly_deletable());
        assert!(report.blockers.is_empty());
        assert_eq!(report.manual_edits.len(), 1);
        assert_eq!(report.manual_edits[0].crate_name, "lodestone-client");
        assert!(report.manual_edits[0].optional);
        // The workspace root manifest + the client manifest reference it, but the
        // v1 crate's own manifest is excluded.
        assert!(!report.manifest_lines.is_empty());
        assert!(
            report
                .manifest_lines
                .iter()
                .all(|line| !line.path.starts_with("crates/versions/v1"))
        );
        Ok(())
    }

    #[test]
    fn check_deletable_flags_required_dependent_as_blocker() -> Result<()> {
        let workspace = isolation_fixture(
            "deletable-required-dependent",
            &[
                ("crates/versions/v1", "lodestone-v1", ""),
                (
                    "crates/lodestone-client",
                    "lodestone-client",
                    r#"
[dependencies]
lodestone-v1 = { path = "../versions/v1" }
"#,
                ),
            ],
        )?;

        let report = check_workspace_deletable(&workspace, "lodestone-v1")?;
        assert!(!report.is_cleanly_deletable());
        assert_eq!(report.blockers.len(), 1);
        assert_eq!(report.blockers[0].crate_name, "lodestone-client");
        assert!(!report.blockers[0].optional);
        Ok(())
    }

    #[test]
    fn check_deletable_flags_version_to_version_dependent_as_blocker() -> Result<()> {
        let workspace = isolation_fixture(
            "deletable-version-dependent",
            &[
                ("crates/versions/v1", "lodestone-v1", ""),
                (
                    "crates/versions/v2",
                    "lodestone-v2",
                    r#"
[dependencies]
lodestone-v1 = { path = "../v1", optional = true }

[features]
compat = ["dep:lodestone-v1"]
"#,
                ),
            ],
        )?;

        // Even though the edge is optional, a version->version dependency is a
        // hard break: v2 could not be built against a deleted v1 in its compat
        // configuration, and it is an isolation violation regardless.
        let report = check_workspace_deletable(&workspace, "v1")?;
        assert!(!report.is_cleanly_deletable());
        assert_eq!(report.blockers.len(), 1);
        assert!(report.blockers[0].dependent_is_version_crate);
        Ok(())
    }

    #[test]
    fn check_deletable_rejects_unknown_version() -> Result<()> {
        let workspace = isolation_fixture(
            "deletable-unknown",
            &[("crates/versions/v1", "lodestone-v1", "")],
        )?;

        let error = check_workspace_deletable(&workspace, "v999").unwrap_err();
        assert!(error.to_string().contains("no version crate matched"));
        Ok(())
    }

    #[test]
    fn check_deletable_flags_feature_forward_reference() -> Result<()> {
        // This mirrors the real client wart: the client depends on the *registry*
        // (not on the version), and only forwards a Cargo feature to it via
        // `live-v1 = ["lodestone-registry/v1"]`. There is no dependency-graph edge
        // from the client to lodestone-v1, so a naive graph-only check misses it —
        // but Cargo validates the feature string at resolve time, so a dangling
        // forward breaks even the default build. The manifest scan must catch it.
        let workspace = isolation_fixture(
            "deletable-feature-forward",
            &[
                ("crates/versions/v1", "lodestone-v1", ""),
                (
                    "crates/lodestone-registry",
                    "lodestone-registry",
                    r#"
[dependencies]
lodestone-v1 = { path = "../versions/v1", optional = true }

[features]
v1 = ["dep:lodestone-v1"]
"#,
                ),
                (
                    "crates/lodestone-client",
                    "lodestone-client",
                    r#"
[dependencies]
lodestone-registry = { path = "../lodestone-registry" }

[features]
live-v1 = ["lodestone-registry/v1"]
"#,
                ),
            ],
        )?;

        let report = check_workspace_deletable(&workspace, "v1")?;
        // No graph edge from the client, but its feature-forward line must be
        // surfaced as a required manifest edit.
        let client_line = report
            .manifest_lines
            .iter()
            .find(|line| line.path == "crates/lodestone-client/Cargo.toml");
        let client_line = client_line.expect("client feature-forward line should be surfaced");
        assert!(
            client_line.text.contains("lodestone-registry/v1"),
            "expected the forwarded feature line, got {:?}",
            client_line.text
        );
        Ok(())
    }

    #[test]
    fn feature_forward_detection_respects_token_boundaries() {
        assert!(line_forwards_to_family_feature(
            r#"live-v1 = ["lodestone-registry/v1"]"#,
            "v1"
        ));
        assert!(line_forwards_to_family_feature(
            r#"x = ["lodestone-registry/v47", "other"]"#,
            "v47"
        ));
        // ...but not a longer token that merely starts with it.
        assert!(!line_forwards_to_family_feature(
            r#"live-v470 = ["lodestone-registry/v470"]"#,
            "v47"
        ));
        // A feature name that merely embeds the token (no `/token` path segment)
        // is not a forward and must not match.
        assert!(!line_forwards_to_family_feature(
            r#"compat = ["v47-shim"]"#,
            "v47"
        ));
    }

    #[test]
    fn check_deletable_surfaces_registry_source_cfg_entries() -> Result<()> {
        // The registry's FAMILIES table gates each family behind
        // `#[cfg(feature = "v1")]`. Those lines never break the build (a dead cfg
        // just compiles out), but they emit `unexpected_cfgs` warnings once the
        // feature is gone, and the workspace standard is zero warnings — so the
        // drill must surface them as required source edits.
        let workspace = isolation_fixture(
            "deletable-registry-source",
            &[
                ("crates/versions/v1", "lodestone-v1", ""),
                (
                    "crates/lodestone-registry",
                    "lodestone-registry",
                    "[package.metadata.lodestone-isolation]\nrole = \"version-registry\"\n\n[dependencies]\nlodestone-v1 = { path = \"../versions/v1\", optional = true }\n\n[features]\nv1 = [\"dep:lodestone-v1\"]\n",
                ),
            ],
        )?;
        std::fs::write(
            workspace.join("crates/lodestone-registry/src/lib.rs"),
            "pub const FAMILIES: &[Family] = &[\n    #[cfg(feature = \"v1\")]\n    Family { make: || Box::new(lodestone_v1::adapter()) },\n];\n",
        )?;

        let report = check_workspace_deletable(&workspace, "v1")?;
        assert_eq!(
            report.registry_source_lines.len(),
            2,
            "expected the cfg gate and the crate-path line, got {:?}",
            report.registry_source_lines
        );
        assert!(
            report
                .registry_source_lines
                .iter()
                .any(|line| line.text.contains("cfg(feature = \"v1\")"))
        );
        assert!(
            report
                .registry_source_lines
                .iter()
                .any(|line| line.text.contains("lodestone_v1::adapter"))
        );
        Ok(())
    }

    #[test]
    fn parses_real_packet_report_counts() -> Result<()> {
        let Some(report) = load_real_report()? else {
            return Ok(());
        };

        assert_eq!(
            report.count(PacketState::Play, PacketBound::Clientbound),
            141
        );
        assert_eq!(
            report.count(PacketState::Play, PacketBound::Serverbound),
            69
        );
        assert_eq!(
            report.count(PacketState::Configuration, PacketBound::Clientbound),
            20
        );
        assert_eq!(
            report.count(PacketState::Configuration, PacketBound::Serverbound),
            10
        );
        assert_eq!(
            report.count(PacketState::Login, PacketBound::Clientbound),
            6
        );
        assert_eq!(
            report.count(PacketState::Login, PacketBound::Serverbound),
            5
        );
        assert_eq!(
            report.count(PacketState::Status, PacketBound::Clientbound),
            2
        );
        assert_eq!(
            report.count(PacketState::Status, PacketBound::Serverbound),
            2
        );
        assert_eq!(
            report.count(PacketState::Handshaking, PacketBound::Clientbound),
            0
        );
        assert_eq!(
            report.count(PacketState::Handshaking, PacketBound::Serverbound),
            1
        );
        Ok(())
    }

    #[test]
    fn parses_real_minecraft_data_report_for_protocol_47() -> Result<()> {
        let path = Path::new("vendor/minecraft-data/data/pc/1.8/protocol.json");
        if !path.exists() {
            eprintln!("skipping minecraft-data test: {} is absent", path.display());
            return Ok(());
        }

        let json = std::fs::read_to_string(path)?;
        let report = parse_minecraft_data_report(&json, "1.8.8", 47)?;

        assert_eq!(report.protocol_version, 47);
        assert_eq!(report.minecraft_version, "1.8.8");

        // Spot-check a handful of well-known 1.8 ids across states/bounds.
        assert_eq!(
            report.id_for(
                PacketState::Handshaking,
                PacketBound::Serverbound,
                "minecraft:set_protocol"
            ),
            Some(0x00)
        );
        assert_eq!(
            report.id_for(
                PacketState::Login,
                PacketBound::Serverbound,
                "minecraft:login_start"
            ),
            Some(0x00)
        );
        assert_eq!(
            report.id_for(
                PacketState::Login,
                PacketBound::Clientbound,
                "minecraft:success"
            ),
            Some(0x02)
        );
        assert_eq!(
            report.id_for(
                PacketState::Login,
                PacketBound::Clientbound,
                "minecraft:compress"
            ),
            Some(0x03)
        );
        assert_eq!(
            report.id_for(
                PacketState::Play,
                PacketBound::Clientbound,
                "minecraft:keep_alive"
            ),
            Some(0x00)
        );
        assert_eq!(
            report.id_for(
                PacketState::Play,
                PacketBound::Clientbound,
                "minecraft:login"
            ),
            Some(0x01)
        );
        assert_eq!(
            report.name_for(PacketState::Play, PacketBound::Clientbound, 0x00),
            Some("minecraft:keep_alive")
        );

        // 1.8 has no configuration state.
        assert_eq!(
            report.count(PacketState::Configuration, PacketBound::Clientbound),
            0
        );
        Ok(())
    }

    #[test]
    fn parse_hex_packet_id_handles_prefixes() -> Result<()> {
        assert_eq!(parse_hex_packet_id("0x00")?, 0);
        assert_eq!(parse_hex_packet_id("0x1a")?, 26);
        assert_eq!(parse_hex_packet_id("0xfe")?, 254);
        assert!(parse_hex_packet_id("0xzz").is_err());
        Ok(())
    }

    #[test]
    fn generated_identifiers_are_unique_per_state_and_bound() -> Result<()> {
        let Some(report) = load_real_report()? else {
            return Ok(());
        };

        for state in PacketState::ALL {
            for bound in PacketBound::ALL {
                let mut identifiers = BTreeSet::new();
                for entry in report.entries(state, bound) {
                    assert!(
                        identifiers.insert(entry.const_ident.as_str()),
                        "duplicate identifier {} in {:?}/{:?}",
                        entry.const_ident,
                        state,
                        bound
                    );
                }
            }
        }
        Ok(())
    }

    #[test]
    fn generated_lookup_helpers_round_trip() -> Result<()> {
        let Some(report) = load_real_report()? else {
            return Ok(());
        };

        let source = generate_packet_ids_source(&report)?;
        let test_dir = Path::new("xtask/target/generated-packet-id-tests");
        std::fs::create_dir_all(test_dir)?;
        let test_source = test_dir.join("packet_ids_roundtrip.rs");
        let test_binary = test_dir.join("packet_ids_roundtrip");

        let mut source_with_tests = source;
        source_with_tests.push_str("\n#[cfg(test)]\nmod generated_round_trip_tests {\n    use super::*;\n\n    #[test]\n    fn all_packet_entries_round_trip() {\n");
        for entry in report.all_entries() {
            source_with_tests.push_str(&format!(
                "        assert_eq!(id_for({}, {}, {:?}), Some({}));\n        assert_eq!(name_for({}, {}, {}), Some({:?}));\n",
                entry.state.code_const(),
                entry.bound.code_const(),
                entry.name,
                entry.protocol_id,
                entry.state.code_const(),
                entry.bound.code_const(),
                entry.protocol_id,
                entry.name
            ));
        }
        source_with_tests.push_str("    }\n}\n");
        std::fs::write(&test_source, source_with_tests)?;

        let status = Command::new("rustc")
            .arg("--edition=2024")
            .arg("--test")
            .arg(&test_source)
            .arg("-o")
            .arg(&test_binary)
            .status()?;
        assert!(
            status.success(),
            "generated source test harness failed to compile"
        );

        let status = Command::new(&test_binary).status()?;
        assert!(
            status.success(),
            "generated id_for/name_for round-trip tests failed"
        );
        Ok(())
    }

    #[test]
    fn codegen_is_deterministic() -> Result<()> {
        let Some(report) = load_real_report()? else {
            return Ok(());
        };

        let first = generate_packet_ids_source(&report)?;
        let second = generate_packet_ids_source(&report)?;
        assert_eq!(first.as_bytes(), second.as_bytes());
        Ok(())
    }

    /// A throwaway workspace rooted in a unique temp directory.
    ///
    /// Fixtures used to live at a *fixed* reused path
    /// (`xtask/target/test-workspaces/<name>`) that each test deleted and
    /// recreated. Under the concurrent filesystem load of `cargo test
    /// --workspace`, that delete-then-recreate is not atomic: `remove_dir_all`
    /// intermittently failed with `ENOTEMPTY` and the follow-up `write` with
    /// `ENOENT` (confirmed by backtrace to originate in the fixture helper, not
    /// in any spawned `cargo`/`rustc` child). A unique `mkdtemp` directory per
    /// run removes the reuse window entirely, and cleanup happens on drop with
    /// errors ignored — so teardown can never fail a test. The guard is held by
    /// the test's binding for the duration of the test; `Deref<Target = Path>`
    /// lets call sites keep using `&workspace` and `workspace.join(..)`.
    struct TestWorkspace {
        dir: tempfile::TempDir,
    }

    impl Deref for TestWorkspace {
        type Target = Path;

        fn deref(&self) -> &Path {
            self.dir.path()
        }
    }
