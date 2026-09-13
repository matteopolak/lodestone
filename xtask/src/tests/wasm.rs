// Wasm build and confinement tests.
    #[test]
    fn captured_builds_scrub_inherited_no_color_before_spawning() {
        let mut command = Command::new("trunk");
        command.env("NO_COLOR", "1");

        configure_captured_build(&mut command);

        let no_color = command
            .get_envs()
            .find(|(key, _)| *key == "NO_COLOR")
            .expect("the command must explicitly remove NO_COLOR from its child environment");
        assert!(no_color.1.is_none());
        assert_eq!(
            command
                .get_envs()
                .find(|(key, _)| *key == "CARGO_TERM_COLOR")
                .and_then(|(_, value)| value),
            Some(std::ffi::OsStr::new("never"))
        );
    }

    fn load_real_report() -> Result<Option<PacketReport>> {
        let path = Path::new(REAL_REPORT);
        if !path.exists() {
            eprintln!("skipping packet report tests: {REAL_REPORT} is absent");
            return Ok(None);
        }

        let json = std::fs::read_to_string(path)?;
        Ok(Some(parse_packet_report(&json, "26.2", 776)?))
    }

    #[test]
    fn cli_help_lists_supported_and_planned_commands() {
        let help = root_help();
        assert!(help.contains("gen-packet-ids"));
        assert!(help.contains("fetch-assets"));
        assert!(help.contains("fetch-version"));
        assert!(help.contains("version-table"));
        assert!(help.contains("gen-reports"));
        assert!(help.contains("gen-registries"));
        assert!(help.contains("codegen-ratio"));
        assert!(help.contains("new-version"));
        assert!(help.contains("conformance"));
        assert!(help.contains("wasm-check"));
        assert!(help.contains("check-ptr-const"));
        assert!(help.contains("check-string-dispatch"));
        assert!(help.contains("check-comment-voice"));
    }

    #[test]
    fn cli_parses_check_ptr_const_command() -> Result<()> {
        assert_eq!(
            parse_cli_args(["check-ptr-const"])?,
            CliCommand::CheckPtrConst
        );
        Ok(())
    }

    #[test]
    fn cli_parses_check_string_dispatch_command() -> Result<()> {
        assert_eq!(
            parse_cli_args(["check-string-dispatch"])?,
            CliCommand::CheckStringDispatch
        );
        Ok(())
    }

    #[test]
    fn cli_parses_check_comment_voice_command_with_default_allowlist() -> Result<()> {
        assert_eq!(
            parse_cli_args(["check-comment-voice"])?,
            CliCommand::CheckCommentVoice {
                allowlist: PathBuf::from(comment_voice::DEFAULT_ALLOWLIST)
            }
        );
        Ok(())
    }

    #[test]
    fn cli_parses_check_comment_voice_command_with_explicit_allowlist() -> Result<()> {
        assert_eq!(
            parse_cli_args(["check-comment-voice", "--allowlist", "custom.toml"])?,
            CliCommand::CheckCommentVoice {
                allowlist: PathBuf::from("custom.toml")
            }
        );
        Ok(())
    }

    #[test]
    fn cli_rejects_unknown_check_comment_voice_option() {
        assert!(parse_cli_args(["check-comment-voice", "--nope"]).is_err());
    }

    #[test]
    fn cli_parses_codegen_ratio_command() -> Result<()> {
        assert_eq!(parse_cli_args(["codegen-ratio"])?, CliCommand::CodegenRatio);
        Ok(())
    }

    // --- wasm-check --------------------------------------------------------

    fn write_fixture(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    fn demo_rule() -> ConfinementRule {
        ConfinementRule {
            label: "demo fs-confinement",
            src_dir: "crates/demo/src",
            banned: "std::fs::",
            allowlist: &[],
        }
    }

    #[test]
    fn cli_parses_wasm_check_command() -> Result<()> {
        // Flagless, like codegen-ratio / connectedness: trailing args are
        // ignored by the parser (the wasm-check run itself does the work).
        assert_eq!(parse_cli_args(["wasm-check"])?, CliCommand::WasmCheck);
        Ok(())
    }

    #[test]
    fn confinement_scanner_reports_path_line_and_content() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        write_fixture(
            tmp.path(),
            "crates/demo/src/lib.rs",
            "line one\nlet x = std::fs::read(\"a\");\nline three",
        );
        let leaks = scan_confinement(tmp.path(), &demo_rule())?;
        assert_eq!(leaks.len(), 1);
        assert_eq!(leaks[0].path, PathBuf::from("crates/demo/src/lib.rs"));
        assert_eq!(leaks[0].line, 2);
        assert_eq!(leaks[0].content, "let x = std::fs::read(\"a\");");
        Ok(())
    }

    #[test]
    fn confinement_scanner_honors_allowlist_by_basename() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        write_fixture(tmp.path(), "crates/demo/src/native.rs", "std::fs::read_allowed");
        write_fixture(tmp.path(), "crates/demo/src/lib.rs", "std::fs::read_banned");
        let rule = ConfinementRule {
            src_dir: "crates/demo/src",
            allowlist: &["native.rs"],
            ..demo_rule()
        };
        let leaks = scan_confinement(tmp.path(), &rule)?;
        assert_eq!(leaks.len(), 1);
        assert_eq!(leaks[0].path, PathBuf::from("crates/demo/src/lib.rs"));
        Ok(())
    }

    #[test]
    fn confinement_scanner_empty_allowlist_reports_every_file() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        write_fixture(tmp.path(), "crates/demo/src/a.rs", "Instant::now()");
        write_fixture(tmp.path(), "crates/demo/src/sub/b.rs", "Instant::now()");
        let rule = ConfinementRule {
            label: "demo time-confinement",
            banned: "Instant::now(",
            ..demo_rule()
        };
        let leaks = scan_confinement(tmp.path(), &rule)?;
        assert_eq!(leaks.len(), 2);
        assert_eq!(leaks[0].path, PathBuf::from("crates/demo/src/a.rs"));
        assert_eq!(leaks[1].path, PathBuf::from("crates/demo/src/sub/b.rs"));
        Ok(())
    }

    #[test]
    fn confinement_scanner_sorts_by_path_then_line() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        write_fixture(tmp.path(), "crates/demo/src/b.rs", "std::fs::\nstd::fs::\nstd::fs::");
        write_fixture(tmp.path(), "crates/demo/src/a.rs", "std::fs::");
        let leaks = scan_confinement(tmp.path(), &demo_rule())?;
        assert_eq!(leaks.len(), 4);
        assert_eq!(leaks[0].path, PathBuf::from("crates/demo/src/a.rs"));
        assert_eq!(leaks[1].path, PathBuf::from("crates/demo/src/b.rs"));
        assert_eq!(leaks[1].line, 1);
        assert_eq!(leaks[2].line, 2);
        assert_eq!(leaks[3].line, 3);
        Ok(())
    }

    #[test]
    fn confinement_scanner_missing_dir_is_an_error_not_a_pass() {
        let tmp = tempfile::tempdir().unwrap();
        let rule = ConfinementRule {
            src_dir: "crates/does-not-exist/src",
            ..demo_rule()
        };
        assert!(scan_confinement(tmp.path(), &rule).is_err());
    }

    /// `trunk` 0.21.14's real failure output, captured VERBATIM from a run that
    /// reproduced the CI failure (a detached worktree, which has no gitignored
    /// `.cache/`, exactly the runner's condition). Only the absolute path was
    /// shortened.
    ///
    /// This is the outside source the assertions below need: it is what the
    /// producer actually writes, not what we assume it writes. Note the shape
    /// that broke the old filter — every line carries an RFC-3339 timestamp and a
    /// level, so NO line starts with `error`.
    const TRUNK_FAILURE_SAMPLE: &str = concat!(
        "2026-08-09T22:13:07.017492Z  INFO 🚀 Starting trunk 0.21.14\n",
        "2026-08-09T22:13:07.018088Z  INFO 📦 starting build\n",
        "2026-08-09T22:13:07.343401Z ERROR ❌ error\n",
        "error from build pipeline\n",
        "\n",
        "Caused by:\n",
        "    0: error getting canonical path for \"/repo/web/../.cache/mc/26.2/client.jar\"\n",
        "    1: No such file or directory (os error 2)\n",
        "2026-08-09T22:13:07.343622Z ERROR error from build pipeline\n",
    );

    /// The regression this whole mechanism exists for. The previous filter was
    /// `line.starts_with(\"error\") || line.contains(\"error from\") || …`, capped
    /// at 8 lines, and against the sample above it selected exactly TWO lines,
    /// neither of which named a file or a cause — which is what CI printed.
    ///
    /// The control is the second half: the same anchored predicate is evaluated
    /// here and required to MISS the `Caused by:` chain, so this test fails if
    /// someone reintroduces an anchor and it happens to work by accident.
    #[test]
    fn diagnostic_selection_survives_ansi_and_keeps_the_caused_by_chain() {
        // Uncoloured output must survive the strip untouched; the coloured case
        // is covered by `diagnostic_selection_keeps_rustc_location_frames`.
        let stripped = strip_ansi(TRUNK_FAILURE_SAMPLE);
        assert_eq!(
            stripped, TRUNK_FAILURE_SAMPLE,
            "strip_ansi must be the identity on text with no escape sequences"
        );

        let selected = select_diagnostic_lines(&stripped, WASM_DIAGNOSTIC_MARKERS, 40);
        let joined = selected.join("\n");
        // The three things a reader needs, none of which reached the CI log.
        assert!(
            joined.contains("client.jar"),
            "the summary must name the missing file; got:\n{joined}"
        );
        assert!(
            joined.contains("No such file or directory"),
            "the summary must name the cause; got:\n{joined}"
        );
        assert!(
            joined.contains("Caused by:"),
            "the summary must keep the Caused by: header; got:\n{joined}"
        );

        // The control: the anchored predicate this replaced, run on the same
        // bytes. If it can see the cause, this test is not measuring anything.
        let anchored: Vec<&str> = TRUNK_FAILURE_SAMPLE
            .lines()
            .filter(|line| line.starts_with("error") || line.contains("error from"))
            .collect();
        assert!(
            !anchored.iter().any(|line| line.contains("client.jar")),
            "the anchored filter was supposed to MISS the cause, so this control \
             proves nothing; it selected: {anchored:?}"
        );
    }

    /// A filter that can return nothing must never *print* nothing: an empty
    /// summary reads as "no error found", which is the one thing a failing build
    /// cannot mean. Output nobody's markers match still has to be shown.
    #[test]
    fn diagnostic_selection_is_empty_only_when_the_tail_fallback_takes_over() {
        let opaque = "linker invoked\nsegmentation fault\n";
        let selected = select_diagnostic_lines(opaque, WASM_DIAGNOSTIC_MARKERS, 40);
        assert!(
            selected.is_empty(),
            "sample was meant to match no marker, so the fallback arm is what \
             `report_build_failure` would take; it selected: {selected:?}"
        );
        // The fallback prints the non-blank tail, so it is non-empty exactly when
        // the captured output is.
        let tail: Vec<&str> = opaque.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(tail, ["linker invoked", "segmentation fault"]);
    }

    /// A coloured rustc diagnostic must keep its `-->` location line, which is
    /// indented and therefore invisible to any per-line filter.
    #[test]
    fn diagnostic_selection_keeps_rustc_location_frames() {
        let sample = concat!(
            "\u{1b}[1m\u{1b}[31merror[E0433]\u{1b}[0m\u{1b}[1m: failed to resolve\u{1b}[0m\n",
            "  \u{1b}[1m\u{1b}[34m-->\u{1b}[0m crates/lodestone-server/src/lib.rs:12:5\n",
            "   \u{1b}[1m\u{1b}[34m|\u{1b}[0m\n",
        );
        let selected = select_diagnostic_lines(&strip_ansi(sample), WASM_DIAGNOSTIC_MARKERS, 40);
        let joined = selected.join("\n");
        assert!(
            joined.contains("--> crates/lodestone-server/src/lib.rs:12:5"),
            "the summary must keep rustc's location frame; got:\n{joined}"
        );
    }

    /// Serialises the two tests that walk the REAL crate directories: the positive
    /// control plants a probe file inside them, and
    /// `confinement_rules_hold_across_the_real_workspace` walks the same trees.
    ///
    /// Measured, not hypothesised: without this the control's probe was removed
    /// while the other test was mid-read and the walk died with ENOENT — a red test
    /// that says nothing about the code under guard.
    static REAL_WORKSPACE_SCAN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Workspace root, from the manifest dir rather than cwd so tests work from
    /// anywhere.
    fn wasm_test_workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .canonicalize()
            .expect("canonicalize workspace root")
    }

    /// Reads the reference script's text.
    fn reference_script_text() -> String {
        let path = wasm_test_workspace_root()
            .join("scripts")
            .join("wasm-check.sh");
        std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
    }

    /// Pulls the rows out of a `NAME=( … )` bash array of double-quoted strings,
    /// skipping the comment lines interleaved through it.
    ///
    /// The parity gates below PARSE the other implementation's table rather than
    /// restate it. That distinction is load-bearing: the previous version of
    /// `confinement_rules_match_the_reference_script_table` hard-coded a list of
    /// nine labels, so when the script grew to seventeen rules the test kept
    /// passing and `cargo xtask wasm-check` — the implementation CI runs — silently
    /// enforced eight fewer rules than the script it claimed parity with. A gate
    /// that compares one table against a copy of itself cannot tell you a third
    /// table exists.
    fn parse_script_array(script: &str, name: &str) -> Vec<String> {
        let opener = format!("{name}=(");
        let mut rows = Vec::new();
        let mut inside = false;
        for line in script.lines() {
            if !inside {
                if line.starts_with(&opener) {
                    inside = true;
                }
                continue;
            }
            if line.starts_with(')') {
                return rows;
            }
            let trimmed = line.trim();
            if let Some(body) = trimmed.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
                rows.push(body.to_string());
            }
        }
        panic!("{name}=( … ) array not found (or unterminated) in the reference script");
    }

    #[test]
    fn wasm_crates_match_the_reference_script_subset() {
        let script = reference_script_text();
        let rows = parse_script_array(&script, "CRATES");
        assert!(
            rows.len() > 10,
            "parsed only {} CRATES rows — the parser, not the table, is what broke",
            rows.len()
        );

        let expected: Vec<(String, Vec<String>)> = rows
            .iter()
            .map(|row| match row.split_once('|') {
                None => (row.clone(), Vec::new()),
                Some((pkg, extra)) => (
                    pkg.to_string(),
                    extra.split_whitespace().map(str::to_string).collect(),
                ),
            })
            .collect();

        let actual: Vec<(String, Vec<String>)> = wasm_crates()
            .iter()
            .map(|c| {
                (
                    c.name.to_string(),
                    c.extra_args.iter().map(|a| a.to_string()).collect(),
                )
            })
            .collect();

        assert_eq!(
            actual, expected,
            "wasm compile subset drifted from scripts/wasm-check.sh's CRATES table"
        );

        let names: Vec<&str> = wasm_crates().iter().map(|c| c.name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len(), "duplicate crate in wasm subset");
    }

    #[test]
    fn confinement_rules_match_the_reference_script_table() {
        let script = reference_script_text();
        let rows = parse_script_array(&script, "CONFINEMENT_RULES");
        assert!(
            rows.len() > 10,
            "parsed only {} CONFINEMENT_RULES rows — the parser, not the table, is what broke",
            rows.len()
        );

        // Every row must split into EXACTLY four `|` fields. This is the mechanical
        // catch for the defect that made five rules vacuous: they spelled a BRE
        // alternation `\(Instant\|SystemTime\)`, whose `\|` is the script's own field
        // separator, so `IFS='|' read` truncated the pattern to `std::time::\(Instant\`,
        // grep exited 2, and a swallowed error printed PASS. Under this assertion that
        // row is a red test instead.
        let malformed: Vec<&String> = rows
            .iter()
            .filter(|row| row.split('|').count() != 4)
            .collect();
        assert!(
            malformed.is_empty(),
            "confinement rule rows must have exactly 4 '|'-separated fields; a '|' \
             inside a pattern truncates it. Offending rows:\n{malformed:#?}"
        );

        let expected: Vec<(String, String, String, Vec<String>)> = rows
            .iter()
            .map(|row| {
                let fields: Vec<&str> = row.split('|').collect();
                (
                    fields[0].to_string(),
                    fields[1].to_string(),
                    fields[2].to_string(),
                    fields[3]
                        .split(',')
                        .filter(|f| !f.is_empty())
                        .map(str::to_string)
                        .collect(),
                )
            })
            .collect();

        let actual: Vec<(String, String, String, Vec<String>)> = confinement_rules()
            .iter()
            .map(|r| {
                (
                    r.label.to_string(),
                    r.src_dir.to_string(),
                    r.banned.to_string(),
                    r.allowlist.iter().map(|f| f.to_string()).collect(),
                )
            })
            .collect();

        assert_eq!(
            actual, expected,
            "confinement rules drifted from scripts/wasm-check.sh's CONFINEMENT_RULES table"
        );

        for rule in confinement_rules() {
            assert!(!rule.src_dir.is_empty(), "{} has empty src_dir", rule.label);
            assert!(!rule.banned.is_empty(), "{} has empty banned", rule.label);
            // A pattern is matched here with `str::contains` and in the script with
            // grep, so it must be a literal substring in both. `|` additionally
            // truncates the script's row; the rest would silently mean something
            // different on one side.
            for meta in ['|', '(', ')', '[', ']', '*', '+', '?', '\\'] {
                if meta == '(' && rule.banned.ends_with('(') {
                    // A trailing `(` is literal in grep's BRE and in `contains`, and
                    // `Instant::now(` deliberately relies on it to distinguish the
                    // call from the type.
                    continue;
                }
                assert!(
                    !rule.banned.contains(meta),
                    "{}: banned pattern {:?} contains regex metacharacter {meta:?}; \
                     patterns must be literal substrings in both implementations",
                    rule.label,
                    rule.banned
                );
            }
        }
    }

    #[test]
    fn every_confinement_rule_fires_under_a_planted_violation() -> Result<()> {
        // THE POSITIVE CONTROL. A confinement rule that has never been observed
        // failing is a rule you hope works — and five of them did not, for their
        // whole life, while printing PASS.
        //
        // For each rule: create ONE new file in the directory that rule scans,
        // carrying the rule's banned pattern on a non-comment line, and require the
        // scan to report it by path. No existing file is touched, so there is
        // nothing to restore; the probe uses a non-`.rs` extension so cargo never
        // considers it, and a basename no allowlist names.
        //
        // Mismatches are COLLECTED and asserted on the collection. An `assert!`
        // inside the loop would prove exactly one arm and leave the rest arguments
        // rather than observations.
        let _serial = REAL_WORKSPACE_SCAN_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let workspace_root = wasm_test_workspace_root();
        let rules = confinement_rules();
        assert!(rules.len() > 10, "suspiciously few rules to control");

        let mut fired: Vec<&str> = Vec::new();
        let mut silent: Vec<String> = Vec::new();

        for (index, rule) in rules.iter().enumerate() {
            // Per-rule filename: two rules scan the same dir, and a shared name
            // would let one rule's probe satisfy another's assertion.
            let probe_name = format!("zz_wasm_guard_control_{index}.probe");
            let probe = workspace_root.join(rule.src_dir).join(&probe_name);
            // Not a comment: the scanner drops lines opening with `//`, `*` or `#`.
            let planted = format!("let _positive_control = {}PLANTED;\n", rule.banned);
            std::fs::write(&probe, &planted)
                .with_context(|| format!("plant probe at {}", probe.display()))?;

            let outcome = scan_confinement(&workspace_root, rule);
            // Remove the probe before judging, so a failed assertion cannot leave it
            // behind and turn every later run red.
            let _ = std::fs::remove_file(&probe);

            match outcome {
                Ok(leaks) => {
                    let named = leaks
                        .iter()
                        .any(|leak| leak.path.ends_with(Path::new(&probe_name)));
                    if named {
                        fired.push(rule.label);
                    } else {
                        silent.push(format!(
                            "{}: planted {:?} in {} and the scan did not report it ({} other leak(s))",
                            rule.label,
                            rule.banned,
                            rule.src_dir,
                            leaks.len()
                        ));
                    }
                }
                Err(err) => silent.push(format!("{}: scanner errored: {err:#}", rule.label)),
            }
            assert!(
                !probe.exists(),
                "probe {} survived cleanup — remove it before re-running",
                probe.display()
            );
        }

        assert!(
            silent.is_empty(),
            "{} of {} confinement rules did NOT fire under a planted violation:\n{}",
            silent.len(),
            rules.len(),
            silent.join("\n")
        );
        assert_eq!(
            fired.len(),
            rules.len(),
            "every rule must be observed failing; fired: {fired:?}"
        );
        // Printed so `-- --nocapture` reports the count rather than only the verdict.
        println!(
            "confinement rules observed FAILING under a planted violation: {}/{}",
            fired.len(),
            rules.len()
        );
        Ok(())
    }

    #[test]
    fn confinement_scan_ignores_comment_lines_and_allowlisted_files() -> Result<()> {
        // The two ways a rule is allowed NOT to fire, each given an arm that fails
        // if the mechanism inverts. Without this, comment-stripping could silently
        // widen to "starts with any punctuation" and nothing would notice.
        let dir = tempfile::tempdir()?;
        let root = dir.path();
        let src = root.join("crates/probe/src");
        std::fs::create_dir_all(src.join("nested"))?;
        std::fs::write(
            src.join("confined.rs"),
            "use std::time::Instant;\n", // allowlisted file: must be ignored
        )?;
        std::fs::write(
            src.join("prose.rs"),
            "// use std::time::Instant; -- traps on wasm32, use crate::platform\n\
             /// `std::time::Instant` is the trapping one\n\
             //! std::time::Instant\n\
             * std::time::Instant\n\
             # std::time::Instant\n",
        )?;
        std::fs::write(
            src.join("nested/leaky.rs"),
            "fn f() {\n    let t = std::time::Instant::now();\n}\n",
        )?;

        let rule = ConfinementRule {
            label: "probe instant-ban",
            src_dir: "crates/probe/src",
            banned: "std::time::Instant",
            allowlist: &["confined.rs"],
        };
        let leaks = scan_confinement(root, &rule)?;
        let reported: Vec<String> = leaks
            .iter()
            .map(|l| format!("{}:{}", l.path.display(), l.line))
            .collect();
        assert_eq!(
            reported,
            vec!["crates/probe/src/nested/leaky.rs:2"],
            "exactly the one executable line, found recursively, must be reported"
        );

        // Control: the same tree with the allowlist emptied must report the
        // allowlisted file too — proving the previous arm's silence came from the
        // allowlist and not from the scanner failing to read the file at all.
        let unguarded = ConfinementRule {
            allowlist: &[],
            ..rule.clone()
        };
        let leaks = scan_confinement(root, &unguarded)?;
        assert_eq!(
            leaks.len(),
            2,
            "with an empty allowlist both executable lines must appear; got {leaks:#?}"
        );
        Ok(())
    }

    #[test]
    fn confinement_rules_hold_across_the_real_workspace() -> Result<()> {
        // The guard as a test: every configured rule must scan clean against
        // the real crates, so `cargo test -p xtask` (and thus `just health`)
        // trips on a leaked wasm hazard instead of waiting for a manual script
        // run. Env-var manifest dir, not cwd, so it works from any cwd.
        //
        // Shares a lock with the positive control, which plants probe files in these
        // same directories.
        let _serial = REAL_WORKSPACE_SCAN_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let workspace_root = wasm_test_workspace_root();
        let mut failures = Vec::new();
        for rule in confinement_rules() {
            let leaks = scan_confinement(&workspace_root, &rule)?;
            if !leaks.is_empty() {
                failures.push(format!("{}: {} leak(s)", rule.label, leaks.len()));
                for leak in &leaks {
                    failures.push(format!(
                        "  {}:{}:{}",
                        leak.path.display(),
                        leak.line,
                        leak.content
                    ));
                }
            }
        }
        assert!(
            failures.is_empty(),
            "wasm confinement guards leaked:\n{}",
            failures.join("\n")
        );
        Ok(())
    }
