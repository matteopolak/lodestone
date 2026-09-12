// Documentation, benchmark, and version-table tests.
    #[test]
    fn cli_parses_version_table_command() -> Result<()> {
        assert_eq!(
            parse_cli_args(["version-table"])?,
            CliCommand::VersionTable {
                check: false,
                fetch_missing: false,
            }
        );
        assert_eq!(
            parse_cli_args(["version-table", "--check", "--fetch-missing"])?,
            CliCommand::VersionTable {
                check: true,
                fetch_missing: true,
            }
        );
        Ok(())
    }

    #[test]
    fn cli_parses_docs_index_command() -> Result<()> {
        assert_eq!(
            parse_cli_args(["docs-index"])?,
            CliCommand::DocsIndex { check: false }
        );
        assert_eq!(
            parse_cli_args(["docs-index", "--check"])?,
            CliCommand::DocsIndex { check: true }
        );
        assert!(parse_cli_args(["docs-index", "--nope"]).is_err());
        assert!(root_help().contains("docs-index"));
        Ok(())
    }

    #[test]
    fn cli_parses_islands_command() -> Result<()> {
        assert_eq!(
            parse_cli_args(["islands"])?,
            CliCommand::Islands { only_crate: None }
        );
        assert_eq!(
            parse_cli_args(["islands", "--crate", "lodestone-entity"])?,
            CliCommand::Islands {
                only_crate: Some("lodestone-entity".to_string())
            }
        );
        assert!(parse_cli_args(["islands", "--nope"]).is_err());
        assert!(root_help().contains("islands"));
        Ok(())
    }

    #[test]
    fn extract_doc_summary_prefers_what_it_is_section() -> Result<()> {
        let text = "# Example doc\n\n**Status:** a long preamble that should be skipped\nentirely because a real section follows.\n\n## What it is\n\nThe real summary paragraph,\nwrapped across two source lines.\n\n## How it works\n\nThis part must never be quoted.\n";
        let (title, summary) = extract_doc_summary(text, "example.md")?;
        assert_eq!(title, "Example doc");
        assert_eq!(
            summary,
            "The real summary paragraph, wrapped across two source lines."
        );
        Ok(())
    }

    #[test]
    fn extract_doc_summary_accepts_what_this_is_spelling() -> Result<()> {
        let text = "# Example\n\n## What this is\n\nA summary using the second spelling.\n";
        let (_, summary) = extract_doc_summary(text, "example.md")?;
        assert_eq!(summary, "A summary using the second spelling.");
        Ok(())
    }

    /// Regression for the real bug this generator's first draft shipped:
    /// `docs/research/combat-scope.md`'s summary paragraph contains
    /// `#12/#72/#98/#121)` (an issue-reference list), and a naive
    /// `starts_with('#')` heading check truncated the summary right before
    /// it, mid-sentence. A real ATX heading needs a space (or EOL) after the
    /// `#`s.
    #[test]
    fn extract_doc_summary_does_not_treat_issue_references_as_headings() -> Result<()> {
        let text = "# Scoping doc\n\n## What it is\n\nSee issues landed under\n#12/#72/#98/#121), which continue the sentence.\n\n## Next heading\n\nUnreachable.\n";
        let (_, summary) = extract_doc_summary(text, "scoping.md")?;
        assert_eq!(
            summary,
            "See issues landed under #12/#72/#98/#121), which continue the sentence."
        );
        Ok(())
    }

    #[test]
    fn extract_doc_summary_falls_back_to_paragraph_under_h1() -> Result<()> {
        let text = "# Legacy doc\n\nNo `What it is` heading exists in this one, so the first\nparagraph under the H1 is the summary.\n\n## Some other heading\n\nNot this.\n";
        let (title, summary) = extract_doc_summary(text, "legacy.md")?;
        assert_eq!(title, "Legacy doc");
        assert_eq!(
            summary,
            "No `What it is` heading exists in this one, so the first paragraph under the H1 is the summary."
        );
        Ok(())
    }

    /// Anti-vacuity control: a doc with no prose anywhere (no `What it is`
    /// section, and nothing but headings right after the H1) must fail
    /// loudly and name the file -- never emit a blank summary. Run and
    /// watched fail per `CLAUDE.md`'s evidence standard for a negative
    /// assertion.
    #[test]
    fn extract_doc_summary_fails_loudly_with_no_usable_prose() {
        let text = "# Heading-only doc\n\n## Immediately another heading\n\n### And another\n";
        let error = extract_doc_summary(text, "heading-only.md").unwrap_err();
        assert!(
            error.to_string().contains("heading-only.md"),
            "error must name the offending file: {error}"
        );
    }

    #[test]
    fn extract_doc_summary_fails_loudly_with_no_h1() {
        let text = "## What it is\n\nNo H1 above this.\n";
        let error = extract_doc_summary(text, "no-h1.md").unwrap_err();
        assert!(error.to_string().contains("no-h1.md"));
    }

    /// Every real doc under `docs/` (minus the explicit skip list) must
    /// produce a usable title and summary -- this is the check that would
    /// fail loudly, naming the file, the moment a new doc lands without a
    /// `## What it is` section and no prose under its H1 either.
    #[test]
    fn generate_docs_index_succeeds_over_the_real_doc_tree() -> Result<()> {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let generated = generate_docs_index(&workspace_root)?;
        assert!(generated.contains("# Lodestone docs"));
        assert!(generated.contains("## Roadmap"));
        assert!(generated.contains("## Plans and research"));
        // Every doc that exists on disk must appear as a link target
        // somewhere in the output, so nothing was silently dropped.
        for path in read_md_dir_sorted(&workspace_root.join("docs"))? {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            if name == "README.md" || DOCS_INDEX_SKIP.contains(&name) {
                continue;
            }
            assert!(
                generated.contains(name),
                "docs/{name} is missing from the generated index"
            );
        }
        Ok(())
    }

    /// The drift guard: `docs/README.md` must be exactly what the generator
    /// produces from the current doc tree. Regenerate with
    /// `LODESTONE_REGEN=1 cargo test -p xtask docs_index_matches_committed`
    /// (same pattern as `crates/lodestone-data/tests/hardness.rs`'s
    /// `committed_table_matches_dump`) or `cargo xtask docs-index`.
    #[test]
    fn docs_index_matches_committed() -> Result<()> {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let generated = generate_docs_index(&workspace_root)?;

        if std::env::var_os("LODESTONE_REGEN").is_some() {
            let out_path = docs_index_out_path(&workspace_root);
            std::fs::write(&out_path, &generated)?;
            eprintln!("regenerated {}", out_path.display());
            return Ok(());
        }

        let committed = std::fs::read_to_string(docs_index_out_path(&workspace_root))?;
        assert_eq!(
            generated, committed,
            "docs/README.md is stale vs the doc tree -- regenerate with `cargo xtask docs-index` \
             or `LODESTONE_REGEN=1 cargo test -p xtask docs_index_matches_committed`"
        );
        Ok(())
    }

    #[test]
    fn cli_parses_bench_compare_command() -> Result<()> {
        assert_eq!(
            parse_cli_args([
                "bench-compare",
                "bench-results/foo.jsonl",
                "--metric",
                "m",
                "--scene",
                "s",
            ])?,
            CliCommand::BenchCompare {
                path: PathBuf::from("bench-results/foo.jsonl"),
                metric: "m".to_string(),
                scene: "s".to_string(),
                baseline_sha: None,
                candidate_sha: None,
                tolerance: 0.25,
            }
        );
        assert_eq!(
            parse_cli_args([
                "bench-compare",
                "bench-results/foo.jsonl",
                "--metric",
                "m",
                "--scene",
                "s",
                "--baseline",
                "abc123",
                "--candidate",
                "def456",
                "--tolerance",
                "10",
            ])?,
            CliCommand::BenchCompare {
                path: PathBuf::from("bench-results/foo.jsonl"),
                metric: "m".to_string(),
                scene: "s".to_string(),
                baseline_sha: Some("abc123".to_string()),
                candidate_sha: Some("def456".to_string()),
                tolerance: 0.10,
            }
        );
        assert!(parse_cli_args(["bench-compare", "p.jsonl", "--metric", "m"]).is_err());
        assert!(root_help().contains("bench-compare"));
        Ok(())
    }

    fn bench_record(sha: &str, ts: u64, value: f64) -> BenchRecord {
        BenchRecord {
            timestamp: ts,
            git_sha: sha.to_string(),
            machine: "macbook.local".to_string(),
            profile: "release".to_string(),
            scene: "test scene".to_string(),
            metric: "test_metric".to_string(),
            value,
            unit: "ms".to_string(),
        }
    }

    #[test]
    fn compare_bench_records_defaults_to_latest_vs_immediately_preceding() -> Result<()> {
        let records = vec![
            bench_record("aaa000000000", 1, 10.0),
            bench_record("bbb000000000", 2, 11.0),
            bench_record("ccc000000000", 3, 20.0),
        ];
        let opts = BenchCompareOptions {
            metric: "test_metric".to_string(),
            scene: "test scene".to_string(),
            candidate_sha: None,
            baseline_sha: None,
            tolerance: 0.25,
        };
        let report = compare_bench_records(&records, &opts)?;
        assert_eq!(report.baseline.git_sha, "bbb000000000");
        assert_eq!(report.candidate.git_sha, "ccc000000000");
        assert!((report.ratio - (20.0 / 11.0)).abs() < 1e-9);
        assert!(!report.within_tolerance(), "20/11 ~= 1.818, well outside +/-25%");
        Ok(())
    }

    #[test]
    fn compare_bench_records_selects_by_explicit_sha_prefix() -> Result<()> {
        let records = vec![
            bench_record("aaa000000000", 1, 10.0),
            bench_record("bbb000000000", 2, 11.0),
            bench_record("ccc000000000", 3, 20.0),
        ];
        let opts = BenchCompareOptions {
            metric: "test_metric".to_string(),
            scene: "test scene".to_string(),
            candidate_sha: Some("ccc".to_string()),
            baseline_sha: Some("aaa".to_string()),
            tolerance: 0.25,
        };
        let report = compare_bench_records(&records, &opts)?;
        assert_eq!(report.baseline.value, 10.0);
        assert_eq!(report.candidate.value, 20.0);
        assert!((report.ratio - 2.0).abs() < 1e-9);
        Ok(())
    }

    /// Anti-vacuity control for the tolerance check itself: a ratio of
    /// exactly 1.0 (identical value) must read as within tolerance, run and
    /// watched to actually assert `true`, not merely constructed.
    #[test]
    fn compare_bench_records_within_tolerance_reports_ok_for_identical_values() -> Result<()> {
        let records = vec![bench_record("aaa000000000", 1, 5.0), bench_record("bbb000000000", 2, 5.0)];
        let opts = BenchCompareOptions {
            metric: "test_metric".to_string(),
            scene: "test scene".to_string(),
            candidate_sha: None,
            baseline_sha: None,
            tolerance: 0.25,
        };
        let report = compare_bench_records(&records, &opts)?;
        assert!(report.within_tolerance());
        assert!(report.render().contains("-> OK"));
        Ok(())
    }

    /// The negative control for the above: run and watched to actually
    /// fail the same `within_tolerance` predicate, not merely assumed to.
    #[test]
    fn compare_bench_records_outside_tolerance_reports_flagged() -> Result<()> {
        let records = vec![bench_record("aaa000000000", 1, 5.0), bench_record("bbb000000000", 2, 50.0)];
        let opts = BenchCompareOptions {
            metric: "test_metric".to_string(),
            scene: "test scene".to_string(),
            candidate_sha: None,
            baseline_sha: None,
            tolerance: 0.25,
        };
        let report = compare_bench_records(&records, &opts)?;
        assert!(!report.within_tolerance());
        assert!(report.render().contains("FLAGGED"));
        Ok(())
    }

    #[test]
    fn compare_bench_records_rejects_cross_machine_comparison() {
        let mut older = bench_record("aaa000000000", 1, 5.0);
        older.machine = "other-machine".to_string();
        let records = vec![older, bench_record("bbb000000000", 2, 5.0)];
        let opts = BenchCompareOptions {
            metric: "test_metric".to_string(),
            scene: "test scene".to_string(),
            candidate_sha: None,
            baseline_sha: Some("aaa".to_string()),
            tolerance: 0.25,
        };
        let error = compare_bench_records(&records, &opts).unwrap_err();
        assert!(error.to_string().contains("not the same machine"));
    }

    #[test]
    fn compare_bench_records_errors_when_no_records_match() {
        let records = vec![bench_record("aaa000000000", 1, 5.0)];
        let opts = BenchCompareOptions {
            metric: "nonexistent".to_string(),
            scene: "test scene".to_string(),
            candidate_sha: None,
            baseline_sha: None,
            tolerance: 0.25,
        };
        assert!(compare_bench_records(&records, &opts).is_err());
    }

    /// Demonstration against this repo's own real recorded data, satisfying
    /// the requirement that this tool be "used by at least one sibling
    /// benchmark as a demonstration". `#[ignore]`d because it depends on the *contents* of
    /// a gitignored, machine-local file that keeps growing every time anyone
    /// runs the bench -- not hermetic, but valuable to run by hand.
    #[test]
    #[ignore = "depends on the local, gitignored bench-results/light_propagation.jsonl history"]
    fn bench_compare_against_real_light_propagation_history() -> Result<()> {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let path = workspace_root.join("bench-results/light_propagation.jsonl");
        let records = read_bench_records(&path)?;
        let report = compare_bench_records(
            &records,
            &BenchCompareOptions {
                metric: "neighbourhood_factor_vs_single".to_string(),
                scene: "3x3 realistic terrain neighbourhood".to_string(),
                candidate_sha: None,
                baseline_sha: None,
                tolerance: 0.25,
            },
        )?;
        println!("{}", report.render());
        assert!(
            report.within_tolerance(),
            "issue #80's fixture consolidation should not have changed this bench's numbers"
        );
        Ok(())
    }

    #[test]
    fn epic_343_versions_lists_exactly_sixteen_versions_in_release_order() {
        assert_eq!(EPIC_343_VERSIONS.len(), 16);
        assert_eq!(EPIC_343_VERSIONS[0], "1.7.10");
        assert_eq!(EPIC_343_VERSIONS[EPIC_343_VERSIONS.len() - 1], "26.2");
        // No duplicates.
        let unique: BTreeSet<&str> = EPIC_343_VERSIONS.iter().copied().collect();
        assert_eq!(unique.len(), EPIC_343_VERSIONS.len());
    }

    #[test]
    fn render_version_table_source_is_deterministic_and_parses_expected_rows() -> Result<()> {
        let entries = vec![
            VersionTableEntry {
                minecraft_version: "1.7.10".to_owned(),
                protocol_version: 5,
                data_version: 18,
                release_date: "2014-05-14T17:29:23+00:00".to_owned(),
                protocol_source: VersionSource::MinecraftData,
                data_version_source: VersionSource::MinecraftData,
                cross_checked: false,
            },
            VersionTableEntry {
                minecraft_version: "26.2".to_owned(),
                protocol_version: 776,
                data_version: 4903,
                release_date: "2026-06-16T12:03:33+00:00".to_owned(),
                protocol_source: VersionSource::JarVersionJson,
                data_version_source: VersionSource::JarVersionJson,
                cross_checked: true,
            },
        ];

        let first = render_version_table_source(&entries)?;
        let second = render_version_table_source(&entries)?;
        assert_eq!(first, second, "rendering must be deterministic");
        assert!(first.contains("\"1.7.10\""));
        assert!(first.contains("\"26.2\""));
        assert!(first.contains("protocol_version: 5"));
        assert!(first.contains("protocol_version: 776"));
        assert!(first.contains("Source::MinecraftData"));
        assert!(first.contains("Source::JarVersionJson"));
        assert!(first.contains("@generated by `cargo run -p xtask -- version-table`"));
        Ok(())
    }
