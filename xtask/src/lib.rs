use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use sha1::Digest;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt::Write as _,
    fs::File,
    io::{Read as _, Write as _},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
};

pub mod comment_voice;
pub mod islands;
pub mod no_winit_headless;
pub mod protocol_dup;
pub mod ptr_const;
pub mod string_dispatch;
pub mod world_coverage;

pub const DEFAULT_PACKET_IDS_OUT: &str = "crates/versions/26.2/src/generated/packet_ids.rs";
/// Default output for the minecraft-data-sourced protocol 47 (Minecraft 1.8.x).
pub const DEFAULT_PACKET_IDS_OUT_V47: &str = "crates/versions/1.8/src/generated/packet_ids.rs";
pub const DEFAULT_CONNECTED_ALLOWLIST: &str = "xtask/check-connected.toml";
#[cfg(test)]
const FIRST_PARTY_LICENSE: &str = "GPL-3.0-or-later";
/// Where `gen-registries` reads/writes the `sound_event`/`particle_type`/`menu`/
/// `item`/`data_component_type` registry tables by default, and where
/// `conformance`'s registry step drift-checks them regardless of `--family`.
/// These describe **the game**, not **the protocol** (see
/// `docs/lodestone-data-crate.md`), so unlike `packet_ids.rs` they are not
/// duplicated per protocol family — there is exactly one committed copy, for
/// the one canonical internal version (26.2 / v770).
pub const DEFAULT_REGISTRY_OUT_DIR: &str = "crates/lodestone-data/src/generated";

mod packet_reports;
pub use packet_reports::*;
pub(crate) use packet_reports::{
    format_rust_source, parse_hex_packet_id, resolve_canonical_alias, shape_review_violations,
};
#[cfg(test)]
pub(crate) use packet_reports::first_party_manifest_license_violations;

// `PartialEq` only (not `Eq`): `BenchCompare`'s `tolerance: f64` has no `Eq`
// impl, and assert_eq! in this module's tests only needs `PartialEq + Debug`.
#[derive(Clone, Debug, PartialEq)]
pub enum CliCommand {
    Help,
    GenPacketIds {
        minecraft_version: String,
        protocol_version: i32,
        check: bool,
        out: Option<PathBuf>,
        source: PacketSource,
    },
    FetchAssets {
        minecraft_version: String,
        force: bool,
    },
    FetchSounds {
        minecraft_version: String,
        /// Include background music and jukebox discs (+293 MB).
        all: bool,
        force: bool,
        /// Concurrent downloads; `None` means [`SOUND_FETCH_JOBS`].
        jobs: Option<usize>,
    },
    FetchVersion {
        minecraft_version: String,
        force: bool,
    },
    VersionTable {
        check: bool,
        fetch_missing: bool,
    },
    GenRegistries {
        options: GenRegistriesOptions,
    },
    CheckIsolation,
    CheckConnected {
        allowlist: PathBuf,
    },
    Connectedness,
    CheckDeletable {
        version: String,
    },
    CodegenRatio,
    NewVersion {
        options: NewVersionOptions,
    },
    Conformance {
        options: ConformanceOptions,
    },
    DocsIndex {
        check: bool,
    },
    BenchCompare {
        path: PathBuf,
        metric: String,
        scene: String,
        baseline_sha: Option<String>,
        candidate_sha: Option<String>,
        tolerance: f64,
    },
    /// wasm32 compile + confinement-guard tripwire.
    WasmCheck,
    Islands {
        only_crate: Option<String>,
    },
    /// Census of registry subjects that reach no draw path.
    WorldCoverage,
    /// Pointer-identity comparison / const-vs-static guard (`ptr_const`).
    CheckPtrConst,
    /// Closed-registry string-pattern match census (`string_dispatch`).
    CheckStringDispatch,
    /// `winit`-absence guard for a `--no-default-features` `lodestone-shell`
    /// build (`no_winit_headless`).
    CheckNoWinitHeadless,
    /// Comment-voice / issue-reference guard (`comment_voice`).
    CheckCommentVoice {
        allowlist: PathBuf,
    },
    /// The five `docs/plans/multi-version-protocol-dedup.md` report sections:
    /// four duplication measures (file, struct, legacy dispatch, function)
    /// plus packet-shape adjacency (`protocol_dup`).
    ProtocolDup,

    Planned {
        name: &'static str,
    },
}

#[must_use]
pub const fn root_help() -> &'static str {
    "xtask\n\nUsage:\n    cargo run -p xtask -- <command> [options]\n\nCommands:\n    gen-packet-ids   Generate Rust packet ID tables from a Mojang report or minecraft-data\n    fetch-assets     Download and verify vanilla client.jar, the asset index, and the asset-store objects client.jar stubs, into .cache/mc/<version>/\n    fetch-sounds     Download and verify the vanilla .ogg sound corpus (~80 MB) into .cache/mc/<version>/objects/\n    fetch-version    Download and verify vanilla server.jar into .cache/mc/<version>/\n    version-table    Generate/check the epic-343 16-version protocol/data-version table\n    gen-registries   Generate selected registry id->ResourceKey tables from registries.json\n    check-isolation  Enforce protocol version crate dependency isolation\n    check-connected  Enforce workspace crates are reachable from shipped binary/cdylib roots\n    connectedness    Report 26.2 play packet reachability\n    check-deletable  Simulate deleting a version family's folder and report the fallout\n    codegen-ratio    Report generated-vs-hand-written codec metrics per protocol family\n    new-version      Scaffold a protocol family; registry support is withheld until SHAPE_REVIEW.toml is discharged\n    gen-reports      Not implemented yet\n    conformance      Run packet-id, registry, isolation, deletability, test, and clippy checks for a family\n    docs-index       Generate docs/README.md from every doc's own H1 + `## What it is` summary\n    bench-compare    Ratio + verdict between two recorded bench-results/*.jsonl runs (issue #82)\n    wasm-check       wasm32 compile + confinement-guard tripwire (tested port of scripts/wasm-check.sh)\n    islands          syn-based scan for dead functions/methods, zero-read fields, and default-only fields\n    world-coverage   Census of registry subjects (entity/block-entity/particle types) that reach no draw path\n    check-ptr-const  syn-based guard: fail any std::ptr::eq/addr_eq or raw-pointer == that targets a const\n    check-no-winit-headless  Fail if winit is reachable from a --no-default-features lodestone-shell build\n    check-comment-voice  Fail on issue references and change-voice phrases in .rs/.md/.wgsl comments\n    protocol-dup     Report file/struct/dispatch-arm/function duplication across crates/versions/ plus the minecraft-data adjacency table\n\nOptions for gen-packet-ids:\n    --version <version>   Minecraft version, e.g. 26.2 (Mojang) or 1.8 (minecraft-data dir)\n    --protocol <id>       Protocol version, e.g. 776 or 47\n    --source <source>     Report source: mojang (default) or minecraft-data\n    --out <path>          Output path under crates/versions/*/src/generated/\n    --check               Compare generated output against disk and fail on drift without writing\n\nOptions for gen-registries:\n    --version <version>       Minecraft version, e.g. 26.2\n    --protocol <id>           Protocol version, e.g. 776\n    --out-dir <path>          Output directory (default crates/lodestone-data/src/generated;\n                              crates/versions/*/src/generated also accepted, for a table\n                              that is genuinely per-family translation data)\n    --registries <csv>        Registry keys to generate (default: sound_event,particle_type,menu,item)\n    --check                   Compare generated registry tables against disk without writing\n\nOptions for check-connected:\n    --allowlist <path>    TOML file of explicit exceptions (default: xtask/check-connected.toml)\n\nOptions for connectedness:\n    Parses 26.2's generated packet_ids.rs for play denominators, then classifies adapter dispatch outlets (ClientEvent, Directive, world/sink writes) with explicit UNCLASSIFIED output.\n\nOptions for check-deletable:\n    <version>             Version family to simulate deleting: package name (lodestone-v1-8), folder (1.8), or path\n\nOptions for codegen-ratio:\n    Reports both the optimistic per-struct derive/manual ratio and the more decision-useful absolute hand-written source lines.\n\nOptions for new-version:\n    --protocol <id>       Protocol number for the new family (required)\n    --minecraft <ver>    Minecraft version key for the packet-id oracle (required)\n    --from <family>       Existing family to copy from, e.g. v770 (default) or v47 (legacy tokens; still resolve to their new crates/versions/ folder)\n    --source <source>     Oracle: mojang or minecraft-data (default inferred from --from)\n    --name <vNNN>         Family folder/label (default v<protocol>)\n    --force               Overwrite the target folder if it already exists\n    SHAPE_REVIEW.toml     Generated when packet shapes differ; every entry must be reviewed before registry support may be added\n\nOptions for conformance:\n    --family <vNNN>       Version family package/feature suffix to check, e.g. v1-14\n    --minecraft <ver>     Minecraft version key for packet-id/registry checks\n    --protocol <id>       Protocol number for the family\n    --source <source>     Packet-id oracle: mojang or minecraft-data (default mojang)\n    --skip-cargo          Only run xtask structural checks; skip cargo test/clippy\n\nOptions for fetch-version:\n    --version <version>   Minecraft version, e.g. 1.16.5\n    --force               Re-download even when cached server.jar already matches its SHA-1\n\nOptions for fetch-assets:\n    --version <version>   Minecraft version, e.g. 26.2\n    --force               Re-download even when cached files already match their SHA-1\n    -h, --help            Print help\n  Also fetches asset-store objects, ~3.2 MB in total:\n    - the 8 whose name is in client.jar at a DIFFERENT size, i.e. the stubs the jar ships to be\n      overridden (the 6 panorama faces, panorama_overlay, unifont.json). Nothing at runtime can\n      tell a stub from the real asset, which is why these must be eager.\n    - minecraft/sounds.json (626 KB), which ShellAudio reads eagerly and cannot start without.\n  The 4871 .ogg samples (375 MB) are NOT fetched: a missing sample is one silent sound, resolved\n  lazily per event. Run `fetch-sounds` for the corpus.\n\nOptions for fetch-sounds:\n    --version <version>   Minecraft version, e.g. 26.2 (fetch-assets must have run first)\n    --all                 Also fetch background music and jukebox discs (+293 MB, 92 objects)\n    --jobs <n>            Concurrent downloads (default 12)\n    --force               Re-download every object even when it already matches its SHA-1\n  Derives the corpus from sounds.json, not a file list: every sample any non-music event can\n  select. Measured on 26.2 -- 4751 objects, 80.14 MB, including all six biome ambience loops.\n  Excluded by default: 70 music tracks + 22 jukebox records = 92 objects, 293.23 MB. The 28 index\n  .ogg objects no event references are fetched in neither mode. Every object's SHA-1 is verified\n  against the index, and a re-run of a complete fetch downloads nothing.\n\nOptions for version-table:\n    --check               Compare the generated table against crates/lodestone-registry/src/generated/version_table.rs and fail on drift without writing\n    --fetch-missing       Also run fetch-version for any of the 16 target versions with no cached .cache/mc/<version>/server.jar (network + disk heavy; off by default)\n\nOptions for docs-index:\n    --check               Compare the generated index against docs/README.md and fail on drift without writing\n  Do not hand-edit docs/README.md: add/edit a doc under docs/ (with an H1 and a `## What\n  it is`/`## What this is` summary paragraph) and re-run this command. `cargo test -p xtask`\n  already fails if the committed file drifts from the generator.\n\nOptions for bench-compare:\n    <path>                 A bench-results/<bench>.jsonl file (gitignored local measurement log)\n    --metric <name>        Metric name to compare, e.g. neighbourhood_factor_vs_single\n    --scene <name>          Scene string to compare (must match exactly, including punctuation)\n    --candidate <sha>       Git-sha prefix of the \"after\" run (default: most recent recorded run)\n    --baseline <sha>        Git-sha prefix of the \"before\" run (default: the run immediately\n                            preceding the candidate on the same machine/profile)\n    --tolerance <pct>       Tolerance band as a percentage (default 25, i.e. +/-25%)\n  Never wired into CI by this command -- a manual/local/scheduled check, per\n  docs/roadmap/benchmarks.md's policy. Exits non-zero when the ratio falls outside the\n  tolerance band (useful for a future opt-in script; this alone does not make anything\n  CI-blocking).\n\nOptions for world-coverage:\n    Enumerates all three registries through lodestone-data, resolves each subject against the real\n    rig corpora and dispatch tables (syn), and buckets it as drawn / stranded / absent. \"Stranded\"\n    is the finding class: code that names the subject but emits no geometry for it. Fails hard\n    rather than skipping when a declared draw-surface path or renderer anchor has moved.\n\nOptions for islands:\n    --crate <name>         Only report the named workspace crate (default: every crate)\n  Resolution is name-based (no type checker), so it has few false positives and real false\n  negatives on common names -- see docs/island-detection.md before trusting a finding. Exits\n  non-zero if cargo metadata fails, if any workspace member yields zero .rs files, or if more\n  than 5% of files fail to parse.\n\nOptions for check-ptr-const:\n  syn-parses crates/, xtask/ and web/, indexes every const/static item name, then fails on\n  any std::ptr::eq/addr_eq call or raw-pointer == comparison whose operand directly names a\n  const: a const has no stable address (inlined per use site), so a pointer-identity\n  comparison against one can silently stop matching under a different codegen backend --\n  see CLAUDE.md's const/static rule. Resolution is name-based, like islands; a comparison\n  that goes through a local variable or a function call is out of scope, not asserted safe.\n  Prints the full census (every comparison found, tagged const/static/unresolved) on every\n  run, pass or fail. Exits non-zero if fewer than 500 .rs files are found (a broken walk)\n  or if more than 5% of scanned files fail to parse.\n\nOptions for check-no-winit-headless:\n    No flags. Runs `cargo tree -p lodestone-shell --no-default-features -i winit` and fails\n    if winit is reachable -- a headless build must not link the windowing stack. See\n    docs/runtime-presentation.md's winit-free headless build section.\n\nOptions for check-comment-voice:\n    --allowlist <path>    TOML file of explicit exceptions (default: xtask/check-comment-voice.toml)\n  Scans every .rs/.md/.wgsl comment/doc-comment (.rs) or prose (.md, fenced code excluded) or\n  comment (.wgsl) for #123-shaped issue references and word-bounded, case-insensitive \"this\n  change\"/\"this commit\"/\"this patch\"/\"before this change\"/\"this PR\" phrases. Excludes\n  #[attributes], hex colour literals, and URL fragments from the issue-reference pattern, and\n  requires a trailing word boundary so \"this PR\" never matches \"this process\"/\"this property\".\n  Prints the full census (every hit found, tagged ALLOWED/VIOLATION) on every run, pass or fail.\n  Exits non-zero if fewer than 1500 .rs/.md/.wgsl files are found (a broken walk) or if any hit\n  is not covered by the allowlist.\n\nOptions for protocol-dup:\n  No flags. Re-derives docs/plans/multi-version-protocol-dedup.md's \"Duplication, four ways\"\n  tables from the working tree: whole-file line similarity (src/ + tests/, adjacent family\n  pairs), packet struct/enum body identity under src/packets/, handle_play dispatch-arm\n  token similarity (1.8/1.9/1.14 only -- 26.2 is a directory module, not an if-chain),\n  free-function body identity under src/ (excl. generated/, excl. #[cfg(test)]), and the\n  minecraft-data packet-shape adjacency table across the 15 covered target versions. Every\n  number is a fresh measurement, not a citation -- re-run before quoting, and a material\n  disagreement with the plan document is a finding to report, not a mismatch to paper over.\n"
}

pub fn parse_cli_args<I, S>(args: I) -> Result<CliCommand>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let args: Vec<String> = args
        .into_iter()
        .map(|arg| arg.as_ref().to_owned())
        .collect();
    let Some(command) = args.first().map(String::as_str) else {
        return Ok(CliCommand::Help);
    };

    match command {
        "-h" | "--help" | "help" => Ok(CliCommand::Help),
        "gen-packet-ids" => parse_gen_packet_ids_args(&args[1..]),
        "fetch-assets" => parse_fetch_assets_args(&args[1..]),
        "fetch-sounds" => parse_fetch_sounds_args(&args[1..]),
        "gen-registries" => parse_gen_registries_args(&args[1..]),
        "check-isolation" => Ok(CliCommand::CheckIsolation),
        "check-connected" => parse_check_connected_args(&args[1..]),
        "connectedness" => Ok(CliCommand::Connectedness),
        "check-deletable" => parse_check_deletable_args(&args[1..]),
        "codegen-ratio" => Ok(CliCommand::CodegenRatio),
        "new-version" => parse_new_version_args(&args[1..]),
        "fetch-version" => parse_fetch_version_args(&args[1..]),
        "version-table" => parse_version_table_args(&args[1..]),
        "conformance" => parse_conformance_args(&args[1..]),
        "docs-index" => parse_docs_index_args(&args[1..]),
        "bench-compare" => parse_bench_compare_args(&args[1..]),
        "wasm-check" => Ok(CliCommand::WasmCheck),
        "islands" => parse_islands_args(&args[1..]),
        "world-coverage" => Ok(CliCommand::WorldCoverage),
        "check-ptr-const" => Ok(CliCommand::CheckPtrConst),
        "check-string-dispatch" => Ok(CliCommand::CheckStringDispatch),
        "check-no-winit-headless" => Ok(CliCommand::CheckNoWinitHeadless),
        "check-comment-voice" => parse_check_comment_voice_args(&args[1..]),
        "protocol-dup" => Ok(CliCommand::ProtocolDup),
        "gen-reports" => Ok(CliCommand::Planned {
            name: planned_command_name(command).expect("matched planned command has a name"),
        }),
        unknown => bail!("unknown xtask command {unknown:?}\n\n{}", root_help()),
    }
}

pub fn run_cli_command(command: CliCommand) -> Result<()> {
    match command {
        CliCommand::Help => {
            print!("{}", root_help());
            Ok(())
        }
        CliCommand::GenPacketIds {
            minecraft_version,
            protocol_version,
            check,
            out,
            source,
        } => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            if check {
                let check = check_packet_ids(
                    &workspace_root,
                    &minecraft_version,
                    protocol_version,
                    out.as_deref(),
                    source,
                )?;
                if !check.is_identical() {
                    bail!("{}", check.summary);
                }
                println!("{} is up to date", check.out_path.display());
            } else {
                let path = generate_packet_ids(
                    &workspace_root,
                    &minecraft_version,
                    protocol_version,
                    out.as_deref(),
                    source,
                )?;
                println!("generated {}", path.display());
            }
            Ok(())
        }
        CliCommand::FetchAssets {
            minecraft_version,
            force,
        } => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            let summary = fetch_assets(&workspace_root, &minecraft_version, force)?;
            print!("{}", summary.render());
            Ok(())
        }
        CliCommand::FetchSounds {
            minecraft_version,
            all,
            force,
            jobs,
        } => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            let summary = fetch_sounds(
                &workspace_root,
                &minecraft_version,
                all,
                force,
                jobs.unwrap_or(SOUND_FETCH_JOBS),
            )?;
            print!("{}", summary.render());
            Ok(())
        }
        CliCommand::FetchVersion {
            minecraft_version,
            force,
        } => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            let summary = fetch_version(&workspace_root, &minecraft_version, force)?;
            println!("{}", summary.render());
            Ok(())
        }
        CliCommand::VersionTable {
            check,
            fetch_missing,
        } => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            if check {
                let check = check_version_table(&workspace_root, fetch_missing)?;
                if !check.is_identical() {
                    bail!("{}", check.summary);
                }
                println!("{} is up to date", check.out_path.display());
            } else {
                let path = generate_version_table(&workspace_root, fetch_missing)?;
                println!("generated {}", path.display());
            }
            Ok(())
        }
        CliCommand::GenRegistries { options } => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            if options.check {
                check_registries(&workspace_root, &options)?;
                println!("generated registry tables are up to date");
            } else {
                let paths = generate_registries(&workspace_root, &options)?;
                for path in paths {
                    println!("generated {}", path.display());
                }
            }
            Ok(())
        }
        CliCommand::CheckIsolation => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            let report = check_workspace_isolation(&workspace_root)?;
            if let Some(infos) = report.info_summary() {
                println!("{infos}");
            }
            if let Some(warnings) = report.warning_summary() {
                eprintln!("{warnings}");
            }
            if report.has_violations() {
                bail!("{}", report.violation_summary());
            }
            println!("protocol version crate isolation check passed");
            Ok(())
        }
        CliCommand::CheckConnected { allowlist } => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            let report = check_workspace_connected_with_allowlist(&workspace_root, &allowlist)?;
            if report.has_violations() {
                bail!("{}", report.violation_summary());
            }
            println!("{}", report.success_summary());
            Ok(())
        }
        CliCommand::Connectedness => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            let report = connectedness_report(&workspace_root)?;
            println!("{}", report.render());
            if report.has_unclassified() {
                bail!(
                    "connectedness classification has {} unclassified clientbound arm(s)",
                    report.unclassified_count()
                );
            }
            Ok(())
        }
        CliCommand::CheckDeletable { version } => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            let report = check_workspace_deletable(&workspace_root, &version)?;
            println!("{}", report.render());
            if !report.is_cleanly_deletable() {
                bail!(
                    "{} is not cleanly deletable: {} blocking dependency(ies) would break the build",
                    report.target_crate,
                    report.blockers.len()
                );
            }
            Ok(())
        }
        CliCommand::CodegenRatio => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            println!("{}", codegen_ratio_report(&workspace_root)?.render());
            Ok(())
        }
        CliCommand::NewVersion { options } => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            let report = scaffold_new_version(&workspace_root, &options)?;
            println!("{}", report.render());
            Ok(())
        }
        CliCommand::Conformance { options } => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            let report = run_conformance(&workspace_root, &options)?;
            println!("{}", report.render());
            Ok(())
        }
        CliCommand::DocsIndex { check } => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            if check {
                let check = check_docs_index(&workspace_root)?;
                if !check.is_identical() {
                    bail!("{}", check.summary);
                }
                println!("{} is up to date", check.out_path.display());
            } else {
                let path = write_docs_index(&workspace_root)?;
                println!("generated {}", path.display());
            }
            Ok(())
        }
        CliCommand::BenchCompare {
            path,
            metric,
            scene,
            baseline_sha,
            candidate_sha,
            tolerance,
        } => {
            let records = read_bench_records(&path)?;
            let report = compare_bench_records(
                &records,
                &BenchCompareOptions {
                    metric,
                    scene,
                    baseline_sha,
                    candidate_sha,
                    tolerance,
                },
            )?;
            print!("{}", report.render());
            if !report.within_tolerance() {
                std::process::exit(1);
            }
            Ok(())
        }
        CliCommand::WasmCheck => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            run_wasm_check(&workspace_root)
        }
        CliCommand::Islands { only_crate } => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            let report = islands::islands_report(&workspace_root)?;
            print!(
                "{}",
                islands::format_islands_report(&report, only_crate.as_deref())
            );
            Ok(())
        }
        CliCommand::WorldCoverage => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            let report = world_coverage::world_coverage_report(&workspace_root)?;
            print!("{}", world_coverage::format_world_coverage_report(&report));
            Ok(())
        }
        CliCommand::CheckPtrConst => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            ptr_const::run_check_ptr_const(&workspace_root)
        }
        CliCommand::CheckStringDispatch => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            string_dispatch::run_check(&workspace_root)
        }
        CliCommand::CheckNoWinitHeadless => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            no_winit_headless::run_check_no_winit_headless(&workspace_root)
        }
        CliCommand::CheckCommentVoice { allowlist } => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            comment_voice::run_check_comment_voice(&workspace_root, &allowlist)
        }
        CliCommand::ProtocolDup => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            let report = protocol_dup::protocol_dup_report(&workspace_root)?;
            print!("{}", report.render());
            Ok(())
        }
        CliCommand::Planned { name } => bail!("xtask command {name:?} is not implemented yet"),
    }
}

fn parse_gen_packet_ids_args(args: &[String]) -> Result<CliCommand> {
    let mut minecraft_version = None;
    let mut protocol_version = None;
    let mut check = false;
    let mut out = None;
    let mut source = PacketSource::Mojang;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => return Ok(CliCommand::Help),
            "--check" => {
                check = true;
            }
            "--version" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow!("--version requires a value"))?;
                minecraft_version = Some(value.clone());
            }
            "--protocol" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow!("--protocol requires a value"))?;
                protocol_version = Some(
                    value
                        .parse::<i32>()
                        .with_context(|| format!("parse protocol version {value:?}"))?,
                );
            }
            "--source" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow!("--source requires a value"))?;
                source = match value.as_str() {
                    "mojang" => PacketSource::Mojang,
                    "minecraft-data" => PacketSource::MinecraftData,
                    other => bail!(
                        "unknown packet source {other:?}; expected \"mojang\" or \"minecraft-data\""
                    ),
                };
            }
            "--out" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow!("--out requires a value"))?;
                out = Some(PathBuf::from(value));
            }
            unknown => bail!("unknown gen-packet-ids option {unknown:?}"),
        }
        index += 1;
    }

    Ok(CliCommand::GenPacketIds {
        minecraft_version: minecraft_version.ok_or_else(|| anyhow!("--version is required"))?,
        protocol_version: protocol_version.ok_or_else(|| anyhow!("--protocol is required"))?,
        check,
        out,
        source,
    })
}

fn parse_fetch_assets_args(args: &[String]) -> Result<CliCommand> {
    let mut minecraft_version = None;
    let mut force = false;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => return Ok(CliCommand::Help),
            "--force" => force = true,
            "--version" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow!("--version requires a value"))?;
                minecraft_version = Some(value.clone());
            }
            unknown => bail!("unknown fetch-assets option {unknown:?}"),
        }
        index += 1;
    }

    Ok(CliCommand::FetchAssets {
        minecraft_version: minecraft_version.ok_or_else(|| anyhow!("--version is required"))?,
        force,
    })
}

fn parse_fetch_sounds_args(args: &[String]) -> Result<CliCommand> {
    let mut minecraft_version = None;
    let mut all = false;
    let mut force = false;
    let mut jobs = None;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => return Ok(CliCommand::Help),
            "--all" => all = true,
            "--force" => force = true,
            "--version" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow!("--version requires a value"))?;
                minecraft_version = Some(value.clone());
            }
            "--jobs" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow!("--jobs requires a value"))?;
                let parsed: usize = value
                    .parse()
                    .with_context(|| format!("--jobs expects a positive integer, got {value:?}"))?;
                if parsed == 0 {
                    bail!("--jobs must be at least 1");
                }
                jobs = Some(parsed);
            }
            unknown => bail!("unknown fetch-sounds option {unknown:?}"),
        }
        index += 1;
    }

    Ok(CliCommand::FetchSounds {
        minecraft_version: minecraft_version.ok_or_else(|| anyhow!("--version is required"))?,
        all,
        force,
        jobs,
    })
}

fn parse_fetch_version_args(args: &[String]) -> Result<CliCommand> {
    let mut minecraft_version = None;
    let mut force = false;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => return Ok(CliCommand::Help),
            "--force" => force = true,
            "--version" => {
                index += 1;
                minecraft_version = Some(
                    args.get(index)
                        .ok_or_else(|| anyhow!("--version requires a value"))?
                        .clone(),
                );
            }
            unknown => bail!("unknown fetch-version option {unknown:?}"),
        }
        index += 1;
    }

    Ok(CliCommand::FetchVersion {
        minecraft_version: minecraft_version.ok_or_else(|| anyhow!("--version is required"))?,
        force,
    })
}

fn parse_version_table_args(args: &[String]) -> Result<CliCommand> {
    let mut check = false;
    let mut fetch_missing = false;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => return Ok(CliCommand::Help),
            "--check" => check = true,
            "--fetch-missing" => fetch_missing = true,
            unknown => bail!("unknown version-table option {unknown:?}"),
        }
        index += 1;
    }

    Ok(CliCommand::VersionTable {
        check,
        fetch_missing,
    })
}

fn parse_docs_index_args(args: &[String]) -> Result<CliCommand> {
    let mut check = false;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => return Ok(CliCommand::Help),
            "--check" => check = true,
            unknown => bail!("unknown docs-index option {unknown:?}"),
        }
        index += 1;
    }

    Ok(CliCommand::DocsIndex { check })
}

fn parse_islands_args(args: &[String]) -> Result<CliCommand> {
    let mut only_crate = None;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => return Ok(CliCommand::Help),
            "--crate" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow!("--crate requires a value"))?;
                only_crate = Some(value.clone());
            }
            unknown => bail!("unknown islands option {unknown:?}"),
        }
        index += 1;
    }

    Ok(CliCommand::Islands { only_crate })
}

fn parse_bench_compare_args(args: &[String]) -> Result<CliCommand> {
    let mut path: Option<PathBuf> = None;
    let mut metric: Option<String> = None;
    let mut scene: Option<String> = None;
    let mut baseline_sha: Option<String> = None;
    let mut candidate_sha: Option<String> = None;
    let mut tolerance = 0.25_f64;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => return Ok(CliCommand::Help),
            "--metric" => {
                index += 1;
                metric = Some(
                    args.get(index)
                        .ok_or_else(|| anyhow!("--metric requires a value"))?
                        .clone(),
                );
            }
            "--scene" => {
                index += 1;
                scene = Some(
                    args.get(index)
                        .ok_or_else(|| anyhow!("--scene requires a value"))?
                        .clone(),
                );
            }
            "--baseline" => {
                index += 1;
                baseline_sha = Some(
                    args.get(index)
                        .ok_or_else(|| anyhow!("--baseline requires a git-sha prefix"))?
                        .clone(),
                );
            }
            "--candidate" => {
                index += 1;
                candidate_sha = Some(
                    args.get(index)
                        .ok_or_else(|| anyhow!("--candidate requires a git-sha prefix"))?
                        .clone(),
                );
            }
            "--tolerance" => {
                index += 1;
                let raw = args
                    .get(index)
                    .ok_or_else(|| anyhow!("--tolerance requires a percentage, e.g. 25"))?;
                let pct: f64 = raw
                    .parse()
                    .with_context(|| format!("--tolerance {raw:?} is not a number"))?;
                tolerance = pct / 100.0;
            }
            unknown if unknown.starts_with("--") => bail!("unknown bench-compare option {unknown:?}"),
            positional => {
                if path.is_some() {
                    bail!("bench-compare takes exactly one positional <path>, got a second: {positional:?}");
                }
                path = Some(PathBuf::from(positional));
            }
        }
        index += 1;
    }

    Ok(CliCommand::BenchCompare {
        path: path.ok_or_else(|| anyhow!("bench-compare requires a <path> to a bench-results/*.jsonl file"))?,
        metric: metric.ok_or_else(|| anyhow!("bench-compare requires --metric <name>"))?,
        scene: scene.ok_or_else(|| anyhow!("bench-compare requires --scene <name>"))?,
        baseline_sha,
        candidate_sha,
        tolerance,
    })
}

fn parse_gen_registries_args(args: &[String]) -> Result<CliCommand> {
    let mut minecraft_version = None;
    let mut protocol_version = None;
    let mut check = false;
    let mut out_dir = PathBuf::from(DEFAULT_REGISTRY_OUT_DIR);
    let mut registries: Option<Vec<String>> = None;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => return Ok(CliCommand::Help),
            "--check" => {
                check = true;
            }
            "--version" => {
                index += 1;
                minecraft_version = Some(
                    args.get(index)
                        .ok_or_else(|| anyhow!("--version requires a value"))?
                        .clone(),
                );
            }
            "--protocol" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow!("--protocol requires a value"))?;
                protocol_version = Some(
                    value
                        .parse::<i32>()
                        .with_context(|| format!("parse protocol version {value:?}"))?,
                );
            }
            "--out-dir" => {
                index += 1;
                out_dir = PathBuf::from(
                    args.get(index)
                        .ok_or_else(|| anyhow!("--out-dir requires a value"))?,
                );
            }
            "--registries" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow!("--registries requires a value"))?;
                registries = Some(
                    value
                        .split(',')
                        .map(str::trim)
                        .filter(|part| !part.is_empty())
                        .map(normalize_registry_key)
                        .collect(),
                );
            }
            unknown => bail!("unknown gen-registries option {unknown:?}"),
        }
        index += 1;
    }

    Ok(CliCommand::GenRegistries {
        options: GenRegistriesOptions {
            minecraft_version: minecraft_version.ok_or_else(|| anyhow!("--version is required"))?,
            protocol_version: protocol_version.ok_or_else(|| anyhow!("--protocol is required"))?,
            check,
            out_dir,
            registries: registries.unwrap_or_else(|| {
                default_registry_specs()
                    .iter()
                    .map(|spec| spec.registry_key.to_owned())
                    .collect()
            }),
        },
    })
}

fn normalize_registry_key(key: &str) -> String {
    if key.contains(':') {
        key.to_owned()
    } else {
        format!("minecraft:{key}")
    }
}

fn parse_check_connected_args(args: &[String]) -> Result<CliCommand> {
    let mut allowlist = PathBuf::from(DEFAULT_CONNECTED_ALLOWLIST);
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => return Ok(CliCommand::Help),
            "--allowlist" => {
                index += 1;
                allowlist = PathBuf::from(
                    args.get(index)
                        .ok_or_else(|| anyhow!("--allowlist requires a value"))?,
                );
            }
            unknown => bail!("unknown check-connected option {unknown:?}"),
        }
        index += 1;
    }
    Ok(CliCommand::CheckConnected { allowlist })
}

fn parse_check_comment_voice_args(args: &[String]) -> Result<CliCommand> {
    let mut allowlist = PathBuf::from(comment_voice::DEFAULT_ALLOWLIST);
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => return Ok(CliCommand::Help),
            "--allowlist" => {
                index += 1;
                allowlist = PathBuf::from(
                    args.get(index)
                        .ok_or_else(|| anyhow!("--allowlist requires a value"))?,
                );
            }
            unknown => bail!("unknown check-comment-voice option {unknown:?}"),
        }
        index += 1;
    }
    Ok(CliCommand::CheckCommentVoice { allowlist })
}

fn parse_check_deletable_args(args: &[String]) -> Result<CliCommand> {
    let mut version = None;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => return Ok(CliCommand::Help),
            "--version" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow!("--version requires a value"))?;
                version = Some(value.clone());
            }
            value if !value.starts_with('-') && version.is_none() => {
                version = Some(value.to_owned());
            }
            unknown => bail!("unknown check-deletable option {unknown:?}"),
        }
        index += 1;
    }

    Ok(CliCommand::CheckDeletable {
        version: version.ok_or_else(|| {
            anyhow!("check-deletable requires a version, e.g. `cargo xtask check-deletable v47`")
        })?,
    })
}

fn parse_new_version_args(args: &[String]) -> Result<CliCommand> {
    let mut name = None;
    let mut protocol = None;
    let mut minecraft_version = None;
    let mut source = None;
    let mut from = None;
    let mut force = false;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => return Ok(CliCommand::Help),
            "--force" => force = true,
            "--name" => {
                index += 1;
                name = Some(
                    args.get(index)
                        .ok_or_else(|| anyhow!("--name requires a value"))?
                        .clone(),
                );
            }
            "--protocol" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow!("--protocol requires a value"))?;
                protocol = Some(
                    value
                        .parse::<i32>()
                        .with_context(|| format!("parse protocol version {value:?}"))?,
                );
            }
            "--minecraft" | "--version" => {
                index += 1;
                minecraft_version = Some(
                    args.get(index)
                        .ok_or_else(|| anyhow!("--minecraft requires a value"))?
                        .clone(),
                );
            }
            "--source" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow!("--source requires a value"))?;
                source = Some(match value.as_str() {
                    "mojang" => PacketSource::Mojang,
                    "minecraft-data" => PacketSource::MinecraftData,
                    other => bail!(
                        "unknown packet source {other:?}; expected \"mojang\" or \"minecraft-data\""
                    ),
                });
            }
            "--from" => {
                index += 1;
                from = Some(
                    args.get(index)
                        .ok_or_else(|| anyhow!("--from requires a value"))?
                        .clone(),
                );
            }
            unknown => bail!("unknown new-version option {unknown:?}"),
        }
        index += 1;
    }

    let protocol = protocol.ok_or_else(|| anyhow!("--protocol is required"))?;
    let minecraft_version =
        minecraft_version.ok_or_else(|| anyhow!("--minecraft is required (oracle lookup key)"))?;
    let from = from.unwrap_or_else(|| "v770".to_owned());
    // Default the source from the family we copy: the legacy `v47` family is fed
    // by minecraft-data, everything modern by Mojang's report.
    let source = source.unwrap_or(if from == "v47" {
        PacketSource::MinecraftData
    } else {
        PacketSource::Mojang
    });
    let name = name.unwrap_or_else(|| format!("v{protocol}"));

    Ok(CliCommand::NewVersion {
        options: NewVersionOptions {
            name,
            protocol,
            minecraft_version,
            source,
            from,
            force,
        },
    })
}

fn parse_conformance_args(args: &[String]) -> Result<CliCommand> {
    let mut family = None;
    let mut minecraft_version = None;
    let mut protocol_version = None;
    let mut source = PacketSource::Mojang;
    let mut skip_cargo = false;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => return Ok(CliCommand::Help),
            "--family" => {
                index += 1;
                family = Some(
                    args.get(index)
                        .ok_or_else(|| anyhow!("--family requires a value"))?
                        .clone(),
                );
            }
            "--minecraft" | "--version" => {
                index += 1;
                minecraft_version = Some(
                    args.get(index)
                        .ok_or_else(|| anyhow!("--minecraft requires a value"))?
                        .clone(),
                );
            }
            "--protocol" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow!("--protocol requires a value"))?;
                protocol_version = Some(
                    value
                        .parse::<i32>()
                        .with_context(|| format!("parse protocol version {value:?}"))?,
                );
            }
            "--source" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow!("--source requires a value"))?;
                source = match value.as_str() {
                    "mojang" => PacketSource::Mojang,
                    "minecraft-data" => PacketSource::MinecraftData,
                    other => bail!(
                        "unknown packet source {other:?}; expected \"mojang\" or \"minecraft-data\""
                    ),
                };
            }
            "--skip-cargo" => {
                skip_cargo = true;
            }
            unknown => bail!("unknown conformance option {unknown:?}"),
        }
        index += 1;
    }

    Ok(CliCommand::Conformance {
        options: ConformanceOptions {
            family: family.ok_or_else(|| anyhow!("--family is required"))?,
            minecraft_version: minecraft_version
                .ok_or_else(|| anyhow!("--minecraft is required"))?,
            protocol_version: protocol_version.ok_or_else(|| anyhow!("--protocol is required"))?,
            source,
            skip_cargo,
        },
    })
}

fn planned_command_name(command: &str) -> Option<&'static str> {
    match command {
        "fetch-version" => Some("fetch-version"),
        "gen-reports" => Some("gen-reports"),
        "conformance" => Some("conformance"),
        _ => None,
    }
}

/// Loads and parses a packet report from the configured [`PacketSource`].
mod packet_ids;
pub use packet_ids::*;
pub(crate) use packet_ids::{
    default_out_for_source, load_minecraft_data_protocol_json, load_packet_report,
    minecraft_data_fallback_protocol_dir, minecraft_data_version_info,
};
fn packet_id_diff_summary(path: &Path, expected: &str, actual: &str) -> String {
    let expected_lines: Vec<&str> = expected.lines().collect();
    let actual_lines: Vec<&str> = actual.lines().collect();
    let max_len = expected_lines.len().max(actual_lines.len());

    for index in 0..max_len {
        let expected_line = expected_lines.get(index).copied().unwrap_or("<missing>");
        let actual_line = actual_lines.get(index).copied().unwrap_or("<missing>");
        if expected_line != actual_line {
            return format!(
                "{} is out of date: first difference at line {}\nexpected: {}\nactual:   {}",
                path.display(),
                index + 1,
                expected_line,
                actual_line
            );
        }
    }

    format!(
        "{} is out of date: generated contents differ",
        path.display()
    )
}

mod isolation;
pub use isolation::*;

mod connectedness;
pub use connectedness::*;
// Test fixtures exercise parser/classifier controls directly.
pub(crate) use connectedness::{
    ClientboundArm, ClientboundVerdict, ConsumerOutlet, FunctionBody, PlayPacketIdSummary,
    ServerboundDecodeArm, ServerboundDecodeVerdict, classify_clientbound_dispatch,
    classify_serverbound_decode, delegate_function_calls, extract_functions, find_outside_comments,
    match_arm_body, parse_play_packet_id_summary, serverbound_variant_is_connected,
};

mod deletability;
pub use deletability::*;

mod codegen_ratio;
pub use codegen_ratio::*;
pub struct NewVersionOptions {
    /// Family label / folder name under `crates/versions/`, e.g. `v340`.
    pub name: String,
    /// Protocol number the new family targets.
    pub protocol: i32,
    /// Minecraft version key used to locate the packet-id oracle.
    pub minecraft_version: String,
    /// Which oracle produces the generated packet ids.
    pub source: PacketSource,
    /// Existing family to copy the module skeleton from, e.g. `v770`.
    pub from: String,
    /// Overwrite the target folder if it already exists.
    pub force: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConformanceOptions {
    pub family: String,
    pub minecraft_version: String,
    pub protocol_version: i32,
    pub source: PacketSource,
    pub skip_cargo: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConformanceReport {
    pub family: String,
    pub steps: Vec<ConformanceStep>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConformanceStep {
    pub name: String,
    pub outcome: ConformanceOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConformanceOutcome {
    Passed,
    Skipped(String),
}

impl ConformanceReport {
    #[must_use]
    pub fn render(&self) -> String {
        let mut rendered = format!("conformance checks for {}:\n", self.family);
        for step in &self.steps {
            match &step.outcome {
                ConformanceOutcome::Passed => {
                    let _ = writeln!(rendered, "  PASS {}", step.name);
                }
                ConformanceOutcome::Skipped(reason) => {
                    let _ = writeln!(rendered, "  SKIP {}: {reason}", step.name);
                }
            }
        }
        rendered
    }
}

/// What `new-version` did, and — crucially — the residue a human must still
/// finish by hand.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewVersionReport {
    /// New family label.
    pub name: String,
    /// New crate package name.
    pub crate_name: String,
    /// Family copied from.
    pub from: String,
    /// Protocol number.
    pub protocol: i32,
    /// Files written into the new crate directory (workspace-relative).
    pub created_files: Vec<String>,
    /// Manifests/sources edited to wire the new family in (workspace-relative).
    pub wired_files: Vec<String>,
    /// Packet shapes that changed according to minecraft-data protocol.json.
    pub shape_changes: Vec<PacketShapeChange>,
    /// Manual follow-up items the scaffold cannot do for you.
    pub residue: Vec<String>,
}

impl NewVersionReport {
    /// A human-readable summary of the scaffold and its residue.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = format!(
            "scaffolded protocol family {} (crate {}, protocol {}) from {}\n",
            self.name, self.crate_name, self.protocol, self.from
        );
        let _ = write!(out, "\ncreated {} file(s):", self.created_files.len());
        for file in &self.created_files {
            let _ = write!(out, "\n  + {file}");
        }
        let _ = write!(
            out,
            "\n\nwired into {} existing file(s):",
            self.wired_files.len()
        );
        for file in &self.wired_files {
            let _ = write!(out, "\n  ~ {file}");
        }
        let _ = write!(
            out,
            "\n\npacket shape changes reported by oracle ({} item(s)):",
            self.shape_changes.len()
        );
        for change in self.shape_changes.iter().take(50) {
            let _ = write!(out, "\n  ? {}", change.render());
        }
        if self.shape_changes.len() > 50 {
            let _ = write!(
                out,
                "\n  ? ... {} more not shown",
                self.shape_changes.len() - 50
            );
        }
        let _ = write!(
            out,
            "\n\nresidue — finish these by hand ({} item(s)):",
            self.residue.len()
        );
        for item in &self.residue {
            let _ = write!(out, "\n  ! {item}");
        }
        out.push('\n');
        out
    }
}

/// Capitalises a family label's leading `v` to match the adapter-type prefix,
/// e.g. `v770` -> `V770`.
fn capitalize_family(token: &str) -> String {
    let mut chars = token.chars();
    match chars.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

/// `(directory name, legacy token)` pairs for the four families whose folder
/// was renamed from a protocol-number token to an era-start Minecraft version
/// while their embedded `V<token>`-style identifiers — and therefore the
/// literal text `--from`/`new-version` still substitutes — did not change.
/// Every other, future family stays symmetric (folder name == token), which
/// is what keeps every existing synthetic fixture in this module's own tests
/// working with no lookup at all.
const RENAMED_FAMILY_DIRS: &[(&str, &str)] =
    &[("1.8", "v47"), ("1.9", "v340"), ("1.14", "v735"), ("26.2", "v770")];

/// Resolves a `--from`/`check-deletable` token to the directory it actually
/// lives in under `crates/versions/`. Falls back to the token itself, which
/// is correct for any family whose folder was never decoupled from its token.
fn family_dir_name(token: &str) -> &str {
    RENAMED_FAMILY_DIRS
        .iter()
        .find(|(_, legacy)| *legacy == token)
        .map_or(token, |(dir, _)| *dir)
}

/// Scaffolds a new protocol version family end to end: copies the skeleton from
/// `--from`, regenerates the packet-id table from the relevant oracle, sets the
/// protocol constant, and wires the family into the workspace and the registry.
pub fn scaffold_new_version(
    workspace_root: &Path,
    options: &NewVersionOptions,
) -> Result<NewVersionReport> {
    let from_token = options.from.as_str();
    let to_token = options.name.as_str();
    if from_token == to_token {
        bail!("--from and --name must differ (both are {to_token:?})");
    }

    let protocol_dir = workspace_root.join("crates/versions");
    let from_dir = protocol_dir.join(family_dir_name(from_token));
    if !from_dir.is_dir() {
        bail!(
            "--from family {from_token:?} not found at {}",
            from_dir.display()
        );
    }
    let target_dir = protocol_dir.join(to_token);
    if target_dir.exists() {
        if options.force {
            std::fs::remove_dir_all(&target_dir).with_context(|| {
                format!("remove existing target directory {}", target_dir.display())
            })?;
        } else {
            bail!(
                "target family {to_token:?} already exists at {} (pass --force to overwrite)",
                target_dir.display()
            );
        }
    }

    let from_cap = capitalize_family(from_token);
    let to_cap = capitalize_family(to_token);
    // Two case-sensitive rewrites carry every crate-identifier form: the lower
    // token covers `lodestone-v26-2`, `lodestone_v26_2`, `mod v770`, doc refs; the
    // capitalised one covers the adapter type prefix `V770Adapter`.
    let substitutions = [
        (from_token.to_owned(), to_token.to_owned()),
        (from_cap, to_cap),
    ];

    let mut created_files = Vec::new();
    copy_tree_with_substitutions(
        &from_dir,
        &target_dir,
        &substitutions,
        workspace_root,
        &mut created_files,
    )?;

    // Regenerate the packet-id table from the correct oracle, replacing the
    // copied-over table (which still described the source family).
    let relative_out = format!("crates/versions/{to_token}/src/generated/packet_ids.rs");
    generate_packet_ids(
        workspace_root,
        &options.minecraft_version,
        options.protocol,
        Some(Path::new(&relative_out)),
        options.source,
    )
    .context("generate packet-id table for the new family")?;
    if !created_files.contains(&relative_out) {
        created_files.push(relative_out.clone());
    }

    // Set the protocol constant in the copied adapter.
    let adapter_path = target_dir.join("src/adapter.rs");
    let mut residue = Vec::new();
    let shape_review = new_version_shape_review(
        workspace_root,
        &from_dir,
        from_token,
        to_token,
        &options.minecraft_version,
        options.protocol,
        options.source,
    )
    .map(Some)
    .unwrap_or_else(|error| {
        residue.push(format!(
            "packet shape diff unavailable for {from_token}->{to_token}: {error}"
        ));
        None
    });
    let shape_changes = shape_review
        .as_ref()
        .map(|review| review.entries.clone())
        .unwrap_or_default();
    let block_registry_for_shape_review = shape_review.is_none() || !shape_changes.is_empty();
    if let Some(review) = &shape_review
        && !review.entries.is_empty()
    {
        write_shape_review_files(&target_dir, review, workspace_root, &mut created_files)?;
        residue.push(format!(
            "registry wiring skipped for {to_token}: {} packet shape review entr{} must be marked reviewed = true in crates/versions/{to_token}/SHAPE_REVIEW.toml first",
            review.entries.len(),
            if review.entries.len() == 1 { "y" } else { "ies" }
        ));
    } else if shape_review.is_none() {
        residue.push(format!(
            "registry wiring skipped for {to_token}: packet shape diff is unavailable, so support cannot be advertised safely"
        ));
    }
    if adapter_path.is_file() {
        set_protocol_constant(&adapter_path, options.protocol)?;
    } else {
        residue.push(format!(
            "no src/adapter.rs found in {from_token}; set `pub const PROTOCOL` and the adapter by hand"
        ));
    }

    // Wire the family into the workspace. Registry support is withheld until
    // field-shape deltas have been explicitly reviewed.
    let mut wired_files = Vec::new();
    wire_workspace_dependency(workspace_root, to_token, &mut wired_files)?;
    if !block_registry_for_shape_review {
        wire_registry(
            workspace_root,
            to_token,
            options.protocol,
            &mut wired_files,
            &mut residue,
        )?;
    }

    created_files.sort();
    created_files.dedup();
    wired_files.sort();
    wired_files.dedup();

    // The unavoidable manual residue: everything semantic the scaffold cannot
    // know. Copying is explicitly acceptable here, so these are edits, not
    // re-abstractions.
    residue.push(format!(
        "review packet structs under crates/versions/{to_token}/src/packets/ — they are {from_token}'s wire shapes; change the ones that differ for protocol {}",
        options.protocol
    ));
    residue.push(format!(
        "update `minecraft_versions()` and crate docs in crates/versions/{to_token} to name {}",
        options.minecraft_version
    ));
    residue.push(format!(
        "update the login/play choreography in crates/versions/{to_token}/src/adapter.rs if it differs from {from_token}"
    ));
    residue.push(format!(
        "run `cargo test -p lodestone-{to_token} && cargo clippy -p lodestone-{to_token} --all-targets` and fix fallout"
    ));

    Ok(NewVersionReport {
        name: to_token.to_owned(),
        crate_name: format!("lodestone-{to_token}"),
        from: from_token.to_owned(),
        protocol: options.protocol,
        created_files,
        wired_files,
        shape_changes,
        residue,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct GeneratedPacketIdsMetadata {
    minecraft_version: String,
    protocol_version: i32,
}

fn new_version_shape_review(
    workspace_root: &Path,
    from_dir: &Path,
    from_family: &str,
    target_family: &str,
    target_minecraft_version: &str,
    target_protocol: i32,
    source: PacketSource,
) -> Result<ShapeReviewManifest> {
    if source != PacketSource::MinecraftData {
        bail!(
            "Mojang packets.json contains packet ids only; use minecraft-data when field-shape diffing is required"
        );
    }
    let source_meta =
        parse_generated_packet_ids_metadata(&from_dir.join("src/generated/packet_ids.rs"))?;
    let source_protocol = load_minecraft_data_protocol_json(
        workspace_root,
        &source_meta.minecraft_version,
        source_meta.protocol_version,
    )?;
    let target_protocol_json = load_minecraft_data_protocol_json(
        workspace_root,
        target_minecraft_version,
        target_protocol,
    )?;
    let entries =
        compare_minecraft_data_packet_shapes(&source_protocol.json, &target_protocol_json.json)?;
    Ok(ShapeReviewManifest {
        source_family: from_family.to_owned(),
        target_family: target_family.to_owned(),
        source_minecraft_version: source_meta.minecraft_version,
        source_protocol_version: source_meta.protocol_version,
        target_minecraft_version: target_protocol_json.minecraft_version,
        target_protocol_version: target_protocol,
        entries,
    })
}

fn write_shape_review_files(
    target_dir: &Path,
    review: &ShapeReviewManifest,
    workspace_root: &Path,
    created_files: &mut Vec<String>,
) -> Result<()> {
    let review_path = target_dir.join("SHAPE_REVIEW.toml");
    std::fs::write(&review_path, render_shape_review_toml(review)?)
        .with_context(|| format!("write {}", review_path.display()))?;
    push_relative(created_files, workspace_root, &review_path);

    let tests_dir = target_dir.join("tests");
    std::fs::create_dir_all(&tests_dir)
        .with_context(|| format!("create {}", tests_dir.display()))?;
    let test_path = tests_dir.join("shape_review.rs");
    std::fs::write(&test_path, shape_review_test_source())
        .with_context(|| format!("write {}", test_path.display()))?;
    push_relative(created_files, workspace_root, &test_path);
    Ok(())
}

fn push_relative(files: &mut Vec<String>, workspace_root: &Path, path: &Path) {
    if let Ok(relative) = path.strip_prefix(workspace_root) {
        files.push(relative.to_string_lossy().into_owned());
    }
}

fn shape_review_test_source() -> &'static str {
    r#"const SHAPE_REVIEW: &str = include_str!("../SHAPE_REVIEW.toml");

#[test]
fn packet_shape_review_is_complete() {
    let mut current_packet = "<unknown packet>";
    for line in SHAPE_REVIEW.lines() {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix("name = ") {
            current_packet = value.trim_matches('"');
        } else if trimmed == "reviewed = false" {
            panic!(
                "packet shape review is incomplete for {current_packet}; audit the codec against this protocol, then set reviewed = true in SHAPE_REVIEW.toml"
            );
        }
    }
}
"#
}

fn parse_generated_packet_ids_metadata(path: &Path) -> Result<GeneratedPacketIdsMetadata> {
    let contents =
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let minecraft_version = contents
        .lines()
        .find_map(|line| line.strip_prefix("pub const MINECRAFT_VERSION: &str = \""))
        .and_then(|rest| rest.strip_suffix("\";"))
        .ok_or_else(|| anyhow!("{} is missing MINECRAFT_VERSION", path.display()))?
        .to_owned();
    let protocol_version = contents
        .lines()
        .find_map(|line| line.strip_prefix("pub const PROTOCOL_VERSION: i32 = "))
        .and_then(|rest| rest.strip_suffix(';'))
        .ok_or_else(|| anyhow!("{} is missing PROTOCOL_VERSION", path.display()))?
        .parse()
        .with_context(|| format!("parse PROTOCOL_VERSION in {}", path.display()))?;
    Ok(GeneratedPacketIdsMetadata {
        minecraft_version,
        protocol_version,
    })
}

/// Recursively copies `from_dir` to `target_dir`, applying textual
/// substitutions to every file's contents and skipping `target/` build output.
fn copy_tree_with_substitutions(
    from_dir: &Path,
    target_dir: &Path,
    substitutions: &[(String, String)],
    workspace_root: &Path,
    created: &mut Vec<String>,
) -> Result<()> {
    std::fs::create_dir_all(target_dir)
        .with_context(|| format!("create directory {}", target_dir.display()))?;
    for entry in std::fs::read_dir(from_dir)
        .with_context(|| format!("read directory {}", from_dir.display()))?
    {
        let entry = entry?;
        let file_name = entry.file_name();
        // Never copy build output.
        if file_name == "target" {
            continue;
        }
        let source = entry.path();
        let destination = target_dir.join(&file_name);
        if entry.file_type()?.is_dir() {
            copy_tree_with_substitutions(
                &source,
                &destination,
                substitutions,
                workspace_root,
                created,
            )?;
        } else {
            if is_live_test_file(&source) {
                continue;
            }
            let contents = std::fs::read_to_string(&source)
                .with_context(|| format!("read {}", source.display()))?;
            let mut rewritten = contents;
            for (needle, replacement) in substitutions {
                rewritten = rewritten.replace(needle, replacement);
            }
            std::fs::write(&destination, rewritten)
                .with_context(|| format!("write {}", destination.display()))?;
            if let Ok(relative) = destination.strip_prefix(workspace_root) {
                created.push(relative.to_string_lossy().into_owned());
            }
        }
    }
    Ok(())
}

fn is_live_test_file(path: &Path) -> bool {
    path.parent()
        .and_then(Path::file_name)
        .is_some_and(|name| name == "tests")
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("live_") && name.ends_with(".rs"))
}

/// Rewrites the `pub const PROTOCOL: i32 = <n>;` line in an adapter file.
fn set_protocol_constant(adapter_path: &Path, protocol: i32) -> Result<()> {
    let contents = std::fs::read_to_string(adapter_path)
        .with_context(|| format!("read {}", adapter_path.display()))?;
    let mut replaced = false;
    let rewritten = contents
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            if !replaced && trimmed.starts_with("pub const PROTOCOL: i32 = ") {
                replaced = true;
                let indent = &line[..line.len() - trimmed.len()];
                format!("{indent}pub const PROTOCOL: i32 = {protocol};")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let rewritten = if contents.ends_with('\n') {
        format!("{rewritten}\n")
    } else {
        rewritten
    };
    if !replaced {
        bail!(
            "could not find `pub const PROTOCOL: i32 = ...;` in {}",
            adapter_path.display()
        );
    }
    std::fs::write(adapter_path, rewritten)
        .with_context(|| format!("write {}", adapter_path.display()))?;
    Ok(())
}

/// Adds the new family's `[workspace.dependencies]` line, after the last
/// existing `lodestone-v* = { path = ... }` entry.
fn wire_workspace_dependency(
    workspace_root: &Path,
    name: &str,
    wired: &mut Vec<String>,
) -> Result<()> {
    let manifest_path = workspace_root.join("Cargo.toml");
    let contents = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let new_line = format!("lodestone-{name} = {{ path = \"crates/versions/{name}\" }}");
    if contents.contains(&new_line) {
        return Ok(());
    }
    let lines: Vec<&str> = contents.lines().collect();
    let insert_at = lines
        .iter()
        .rposition(|line| {
            line.trim_start().starts_with("lodestone-v")
                && line.contains("path = \"crates/versions/")
        })
        .map(|index| index + 1)
        .ok_or_else(|| anyhow!("no existing lodestone-v* workspace dependency to anchor after"))?;
    let mut rebuilt = lines[..insert_at].join("\n");
    rebuilt.push('\n');
    rebuilt.push_str(&new_line);
    rebuilt.push('\n');
    rebuilt.push_str(&lines[insert_at..].join("\n"));
    if contents.ends_with('\n') {
        rebuilt.push('\n');
    }
    std::fs::write(&manifest_path, rebuilt)
        .with_context(|| format!("write {}", manifest_path.display()))?;
    wired.push("Cargo.toml".to_owned());
    Ok(())
}

/// Wires the new family into the registry: an optional dependency, a feature,
/// and a `FAMILIES` entry. Best-effort — records residue if the registry's shape
/// is not what we expect rather than corrupting it.
fn wire_registry(
    workspace_root: &Path,
    name: &str,
    protocol: i32,
    wired: &mut Vec<String>,
    residue: &mut Vec<String>,
) -> Result<()> {
    let manifest_path = workspace_root.join("crates/lodestone-registry/Cargo.toml");
    let lib_path = workspace_root.join("crates/lodestone-registry/src/lib.rs");
    if !manifest_path.is_file() || !lib_path.is_file() {
        residue.push(format!(
            "add lodestone-{name} to the version registry by hand (registry crate not found)"
        ));
        return Ok(());
    }

    // Cargo.toml: optional dependency + feature line.
    let manifest = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let dep_line = format!("lodestone-{name} = {{ workspace = true, optional = true }}");
    let feature_line = format!("{name} = [\"dep:lodestone-{name}\"]");
    let mut manifest_lines: Vec<String> = manifest.lines().map(str::to_owned).collect();
    let mut manifest_changed = false;
    let dep_present = manifest.contains(&dep_line);
    let feature_present = manifest.contains(&feature_line);
    let mut manifest_residue = Vec::new();
    if !dep_present {
        if let Some(index) = manifest_lines.iter().rposition(|line| {
            line.trim_start().starts_with("lodestone-v") && line.contains("optional = true")
        }) {
            manifest_lines.insert(index + 1, dep_line);
            manifest_changed = true;
        } else {
            manifest_residue.push(format!(
                "add lodestone-{name} optional dependency to crates/lodestone-registry/Cargo.toml by hand"
            ));
        }
    }
    if !feature_present {
        if let Some(index) = manifest_lines.iter().rposition(|line| {
            line.trim_start().starts_with("v") && line.contains("= [\"dep:lodestone-v")
        }) {
            manifest_lines.insert(index + 1, feature_line);
            manifest_changed = true;
        } else {
            manifest_residue.push(format!(
                "add `{name}` feature to crates/lodestone-registry/Cargo.toml by hand"
            ));
        }
    }
    if manifest_changed {
        let mut rebuilt = manifest_lines.join("\n");
        if manifest.ends_with('\n') {
            rebuilt.push('\n');
        }
        std::fs::write(&manifest_path, rebuilt)
            .with_context(|| format!("write {}", manifest_path.display()))?;
        wired.push("crates/lodestone-registry/Cargo.toml".to_owned());
    }
    residue.extend(manifest_residue);

    // lib.rs: FAMILIES entry, inserted before the closing `];`.
    let lib = std::fs::read_to_string(&lib_path)
        .with_context(|| format!("read {}", lib_path.display()))?;
    let entry = format!(
        "    #[cfg(feature = \"{name}\")]\n    Family {{\n        label: \"{name}\",\n        make: || Box::new(lodestone_{name}::adapter()),\n    }},\n"
    );
    if lib.contains(&format!("label: \"{name}\"")) {
        // already present
    } else if let Some(marker) = lib.find("const FAMILIES: &[Family] = &[") {
        if let Some(close_rel) = lib[marker..].find("];") {
            let close = marker + close_rel;
            let mut rebuilt = String::with_capacity(lib.len() + entry.len());
            rebuilt.push_str(&lib[..close]);
            rebuilt.push_str(&entry);
            rebuilt.push_str(&lib[close..]);
            std::fs::write(&lib_path, rebuilt)
                .with_context(|| format!("write {}", lib_path.display()))?;
            wired.push("crates/lodestone-registry/src/lib.rs".to_owned());
        } else {
            residue.push(format!(
                "add a FAMILIES entry for {name} (protocol {protocol}) to crates/lodestone-registry/src/lib.rs by hand"
            ));
        }
    } else {
        residue.push(format!(
            "add a FAMILIES entry for {name} (protocol {protocol}) to crates/lodestone-registry/src/lib.rs by hand"
        ));
    }
    Ok(())
}

pub fn run_conformance(
    workspace_root: &Path,
    options: &ConformanceOptions,
) -> Result<ConformanceReport> {
    let mut steps = Vec::new();
    let generated_dir = PathBuf::from(format!(
        "crates/versions/{}/src/generated",
        family_dir_name(&options.family)
    ));
    let packet_ids = generated_dir.join("packet_ids.rs");
    let packet_check = check_packet_ids(
        workspace_root,
        &options.minecraft_version,
        options.protocol_version,
        Some(&packet_ids),
        options.source,
    )?;
    if !packet_check.is_identical() {
        bail!("{}", packet_check.summary);
    }
    steps.push(ConformanceStep {
        name: "gen-packet-ids --check".to_owned(),
        outcome: ConformanceOutcome::Passed,
    });

    let registry_report = workspace_root
        .join(".cache")
        .join("mc")
        .join(&options.minecraft_version)
        .join("generated")
        .join("reports")
        .join("registries.json");
    if registry_report.exists() {
        // Registry tables (sound events, particle types, menus, items, data
        // component types) describe the game, not the wire format for this
        // one family: they live in crates/lodestone-data/src/generated,
        // shared by every family, not under this family's own generated/.
        // Pointing this at `generated_dir` (crates/versions/<family>/src/generated)
        // was the stale-location bug — that path has not held these tables
        // since the lodestone-data extraction, so the check silently read
        // nothing for the one family (v770) that reaches this branch at all.
        let registry_options = GenRegistriesOptions {
            minecraft_version: options.minecraft_version.clone(),
            protocol_version: options.protocol_version,
            check: true,
            out_dir: PathBuf::from(DEFAULT_REGISTRY_OUT_DIR),
            registries: default_registry_specs()
                .iter()
                .map(|spec| spec.registry_key.to_owned())
                .collect(),
        };
        check_registries(workspace_root, &registry_options)?;
        steps.push(ConformanceStep {
            name: "gen-registries --check".to_owned(),
            outcome: ConformanceOutcome::Passed,
        });
    } else {
        steps.push(ConformanceStep {
            name: "gen-registries --check".to_owned(),
            outcome: ConformanceOutcome::Skipped(format!(
                "{} is absent; older server jars such as 1.16.5 do not emit Mojang registry reports",
                registry_report.display()
            )),
        });
    }

    let isolation = check_workspace_isolation(workspace_root)?;
    if isolation.has_violations() {
        bail!("{}", isolation.violation_summary());
    }
    steps.push(ConformanceStep {
        name: "check-isolation".to_owned(),
        outcome: ConformanceOutcome::Passed,
    });

    let deletability = check_workspace_deletable(workspace_root, &options.family)?;
    if !deletability.is_cleanly_deletable() {
        bail!(
            "{} is not cleanly deletable: {} blocking dependency(ies) would break the build",
            deletability.target_crate,
            deletability.blockers.len()
        );
    }
    steps.push(ConformanceStep {
        name: "check-deletable".to_owned(),
        outcome: ConformanceOutcome::Passed,
    });

    check_shape_reviews(workspace_root)?;
    steps.push(ConformanceStep {
        name: "shape-review".to_owned(),
        outcome: ConformanceOutcome::Passed,
    });

    // The BFS itself is unavoidably workspace-wide (reachability can only be
    // judged by walking the whole graph from every shipped root), but the
    // verdict handed to a --family run is scoped to that family's own
    // crates. Without this, `conformance --family v340` fails or passes
    // depending on unrelated crates elsewhere in the workspace -- a
    // per-family tool held hostage to state outside its own subject.
    let connected = check_workspace_connected_for_family(workspace_root, &options.family)?;
    if connected.has_violations() {
        bail!("{}", connected.violation_summary());
    }
    steps.push(ConformanceStep {
        name: "check-connected".to_owned(),
        outcome: ConformanceOutcome::Passed,
    });

    if options.skip_cargo {
        steps.push(ConformanceStep {
            name: "cargo test/clippy".to_owned(),
            outcome: ConformanceOutcome::Skipped("--skip-cargo was provided".to_owned()),
        });
    } else {
        let package = format!("lodestone-{}", options.family);
        run_cargo_command(workspace_root, ["test", "-p", package.as_str()])?;
        steps.push(ConformanceStep {
            name: format!("cargo test -p {package}"),
            outcome: ConformanceOutcome::Passed,
        });
        run_cargo_command(
            workspace_root,
            [
                "clippy",
                "-p",
                package.as_str(),
                "--all-targets",
                "--no-deps",
                "--",
                "-D",
                "warnings",
            ],
        )?;
        steps.push(ConformanceStep {
            name: format!("cargo clippy -p {package} --all-targets --no-deps -- -D warnings"),
            outcome: ConformanceOutcome::Passed,
        });
    }

    Ok(ConformanceReport {
        family: options.family.clone(),
        steps,
    })
}

fn run_cargo_command<'a, I>(workspace_root: &Path, args: I) -> Result<()>
where
    I: IntoIterator<Item = &'a str>,
{
    let args: Vec<&str> = args.into_iter().collect();
    let status = Command::new("cargo")
        .args(&args)
        .current_dir(workspace_root)
        .status()
        .with_context(|| format!("run cargo {}", args.join(" ")))?;
    if !status.success() {
        bail!("cargo {} failed with {status}", args.join(" "));
    }
    Ok(())
}

fn cargo_metadata(workspace_root: &Path) -> Result<Value> {
    let manifest_path = workspace_root.join("Cargo.toml");
    let output = Command::new("cargo")
        .arg("metadata")
        .arg("--format-version")
        .arg("1")
        .arg("--no-deps")
        .arg("--manifest-path")
        .arg(&manifest_path)
        .output()
        .with_context(|| format!("run cargo metadata for {}", manifest_path.display()))?;
    if !output.status.success() {
        bail!(
            "cargo metadata failed for {}: {}",
            manifest_path.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    serde_json::from_slice(&output.stdout).context("parse cargo metadata JSON")
}

fn package_manifest_is_under_protocol(canonical_root: &Path, package: &Value) -> Result<bool> {
    let manifest_path = package
        .get("manifest_path")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("workspace package is missing manifest_path"))?;
    let manifest_path = Path::new(manifest_path);
    let canonical_manifest = manifest_path
        .canonicalize()
        .with_context(|| format!("canonicalize manifest path {}", manifest_path.display()))?;
    let relative = canonical_manifest
        .strip_prefix(canonical_root)
        .with_context(|| {
            format!(
                "manifest path {} is not under workspace root {}",
                canonical_manifest.display(),
                canonical_root.display()
            )
        })?;
    Ok(relative.starts_with("crates/versions"))
}

/// Whether a workspace package has opted in to the version-registry role via
/// `[package.metadata.lodestone-isolation] role = "version-registry"`.
///
/// This is the single, deliberate hook that lets the isolation lint recognise
/// the intended version-aggregation crate. It is safe by construction: the role
/// only ever downgrades an *optional* shared -> version edge (already a
/// non-fatal warning) to an informational note. Every fatal rule — version ->
/// version, and a *required* shared -> version edge even on the registry itself
/// — is unaffected, so stamping this metadata on some other crate can at most
/// silence a warning it was already entitled to have as an optional dependency,
/// never a real, build-breaking violation.
fn package_is_version_registry(package: &Value) -> bool {
    package
        .get("metadata")
        .and_then(|metadata| metadata.get("lodestone-isolation"))
        .and_then(|table| table.get("role"))
        .and_then(Value::as_str)
        == Some("version-registry")
}

fn dependency_table_name(kind: Option<&Value>) -> &'static str {
    match kind.and_then(Value::as_str) {
        Some("dev") => "dev-dependencies",
        Some("build") => "build-dependencies",
        _ => "dependencies",
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GenRegistriesOptions {
    pub minecraft_version: String,
    pub protocol_version: i32,
    pub check: bool,
    pub out_dir: PathBuf,
    pub registries: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RegistryCodegenSpec {
    pub registry_key: &'static str,
    pub file_name: &'static str,
    pub module_stem: &'static str,
    pub count_const: &'static str,
    pub names_const: &'static str,
    pub noun: &'static str,
    pub packet_context: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistryTable {
    pub spec: RegistryCodegenSpec,
    pub names: Vec<String>,
    pub fixed_ranges: Option<Vec<Option<String>>>,
}

#[must_use]
pub fn default_registry_specs() -> Vec<RegistryCodegenSpec> {
    vec![
        RegistryCodegenSpec {
            registry_key: "minecraft:sound_event",
            file_name: "sound_events.rs",
            module_stem: "sound_events",
            count_const: "SOUND_EVENT_COUNT",
            names_const: "SOUND_EVENT_NAMES",
            noun: "sound event",
            packet_context: "sound packets carry Holder<SoundEvent>; direct ids map to this table after subtracting one from positive holder ids",
        },
        RegistryCodegenSpec {
            registry_key: "minecraft:particle_type",
            file_name: "particle_types.rs",
            module_stem: "particle_types",
            count_const: "PARTICLE_TYPE_COUNT",
            names_const: "PARTICLE_TYPE_NAMES",
            noun: "particle type",
            packet_context: "level_particles carries a particle-type registry id before any per-particle payload data",
        },
        RegistryCodegenSpec {
            registry_key: "minecraft:menu",
            file_name: "menus.rs",
            module_stem: "menus",
            count_const: "MENU_COUNT",
            names_const: "MENU_NAMES",
            noun: "menu",
            packet_context: "open_screen carries a menu registry id",
        },
        RegistryCodegenSpec {
            registry_key: "minecraft:item",
            file_name: "items.rs",
            module_stem: "items",
            count_const: "ITEM_COUNT",
            names_const: "ITEM_NAMES",
            noun: "item",
            packet_context: "item stacks carry an item registry id (Holder<Item> via id-mapper) before the data-component patch",
        },
    ]
}

/// Registry specs recognised by `--registries` beyond [`default_registry_specs`].
///
/// The default set is what conformance regenerates and drift-checks for every
/// family; `data_component_type` is opt-in because only 1.20.5+ reports it, so
/// it is generated explicitly for the families that carry item component
/// patches rather than folded into the family-agnostic default sweep.
#[must_use]
pub fn known_registry_specs() -> Vec<RegistryCodegenSpec> {
    let mut specs = default_registry_specs();
    specs.push(RegistryCodegenSpec {
        registry_key: "minecraft:data_component_type",
        file_name: "data_component_types.rs",
        module_stem: "data_component_types",
        count_const: "DATA_COMPONENT_TYPE_COUNT",
        names_const: "DATA_COMPONENT_TYPE_NAMES",
        noun: "data component type",
        packet_context: "an item stack's DataComponentPatch identifies each added or removed component by a data-component-type registry id",
    });
    specs
}

pub fn parse_registry_report(
    json: &str,
    specs: &[RegistryCodegenSpec],
) -> Result<Vec<RegistryTable>> {
    let root: Value = serde_json::from_str(json).context("parse registries.json")?;
    let root = root
        .as_object()
        .ok_or_else(|| anyhow!("registries.json root must be an object"))?;

    let mut tables = Vec::with_capacity(specs.len());
    for spec in specs {
        let registry = root
            .get(spec.registry_key)
            .ok_or_else(|| anyhow!("registries.json is missing {}", spec.registry_key))?;
        let entries = registry
            .get("entries")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow!("registry {} is missing entries object", spec.registry_key))?;
        let mut names_by_id = vec![None; entries.len()];
        let mut fixed_ranges_by_id = if spec.registry_key == "minecraft:sound_event" {
            Some(vec![None; entries.len()])
        } else {
            None
        };

        for (name, entry) in sorted_object_entries(entries) {
            let id = entry
                .get("protocol_id")
                .and_then(Value::as_u64)
                .ok_or_else(|| anyhow!("registry entry {name} is missing integer protocol_id"))?;
            let id = usize::try_from(id)
                .with_context(|| format!("registry entry {name} protocol_id is too large"))?;
            let Some(slot) = names_by_id.get_mut(id) else {
                bail!(
                    "registry {} entry {name} has protocol_id {id}, outside 0..{}",
                    spec.registry_key,
                    entries.len()
                );
            };
            if slot.replace(name.to_owned()).is_some() {
                bail!(
                    "registry {} has duplicate protocol_id {id}",
                    spec.registry_key
                );
            }

            if let Some(fixed_ranges) = &mut fixed_ranges_by_id {
                fixed_ranges[id] = parse_sound_fixed_range(entry)
                    .with_context(|| format!("parse fixed_range for sound event {name}"))?;
            }
        }

        let names = names_by_id
            .into_iter()
            .enumerate()
            .map(|(id, name)| {
                name.ok_or_else(|| {
                    anyhow!(
                        "registry {} is missing contiguous protocol_id {id}",
                        spec.registry_key
                    )
                })
            })
            .collect::<Result<Vec<_>>>()?;
        tables.push(RegistryTable {
            spec: *spec,
            names,
            fixed_ranges: fixed_ranges_by_id,
        });
    }

    Ok(tables)
}

fn parse_sound_fixed_range(entry: &Value) -> Result<Option<String>> {
    let value = entry
        .get("fixed_range")
        .or_else(|| entry.get("fixedRange"))
        .or_else(|| {
            entry
                .get("value")
                .and_then(|value| value.get("fixed_range"))
        })
        .or_else(|| entry.get("value").and_then(|value| value.get("fixedRange")));
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let range = value
        .as_f64()
        .ok_or_else(|| anyhow!("fixed_range must be a JSON number"))?;
    if !range.is_finite() || range > f32::MAX as f64 || range < f32::MIN as f64 {
        bail!("fixed_range {range} is outside finite f32 range");
    }
    Ok(Some(float_literal(range)))
}

fn float_literal(value: f64) -> String {
    let mut literal = value.to_string();
    if !literal.contains(['.', 'e', 'E']) {
        literal.push_str(".0");
    }
    literal
}

pub fn generate_registry_source(
    table: &RegistryTable,
    minecraft_version: &str,
    protocol_version: i32,
) -> Result<String> {
    let count = table.names.len();
    let mut source = String::new();
    writeln!(
        source,
        "// @generated by `cargo xtask gen-registries` from Minecraft {minecraft_version} (protocol {protocol_version}). DO NOT EDIT."
    )?;
    writeln!(
        source,
        "// Source: .cache/mc/{minecraft_version}/generated/reports/registries.json registry {}.",
        table.spec.registry_key
    )?;
    writeln!(
        source,
        "//! Generated {} id->ResourceKey table for protocol {protocol_version} (Minecraft {minecraft_version}).",
        table.spec.noun
    )?;
    source.push_str("//!\n");
    writeln!(source, "//! {}.", table.spec.packet_context)?;
    source.push('\n');
    writeln!(
        source,
        "/// Number of {} entries (network ids are `0..{}`).",
        table.spec.noun, table.spec.count_const
    )?;
    writeln!(
        source,
        "pub const {}: u32 = {count};",
        table.spec.count_const
    )?;
    source.push('\n');
    if let Some(fixed_ranges) = &table.fixed_ranges {
        let ranges_const = table.spec.names_const.replace("_NAMES", "_FIXED_RANGES");
        let present_count = fixed_ranges.iter().filter(|range| range.is_some()).count();
        writeln!(
            source,
            "/// Fixed audible ranges keyed by network registry id, sorted by id."
        )?;
        writeln!(
            source,
            "/// An absent id uses the sound system's volume-derived range."
        )?;
        writeln!(
            source,
            "pub static {ranges_const}: [(u32, f32); {present_count}] = ["
        )?;
        for (id, fixed_range) in fixed_ranges.iter().enumerate() {
            if let Some(range) = fixed_range {
                writeln!(source, "    ({id}, {range}),")?;
            }
        }
        source.push_str("];\n\n");
    }
    writeln!(
        source,
        "/// Canonical {} identifier, indexed by network registry id.",
        table.spec.noun
    )?;
    writeln!(
        source,
        "pub static {}: [&str; {count}] = [",
        table.spec.names_const
    )?;
    for name in &table.names {
        writeln!(source, "    {name:?},")?;
    }
    source.push_str("];\n");

    format_rust_source(&source)
}

pub fn generate_registries(
    workspace_root: &Path,
    options: &GenRegistriesOptions,
) -> Result<Vec<PathBuf>> {
    let (tables, out_dir) = load_registry_tables(workspace_root, options)?;
    std::fs::create_dir_all(&out_dir)
        .with_context(|| format!("create generated registry directory {}", out_dir.display()))?;

    let mut written = Vec::with_capacity(tables.len());
    for table in tables {
        let source =
            generate_registry_source(&table, &options.minecraft_version, options.protocol_version)?;
        let path = out_dir.join(table.spec.file_name);
        std::fs::write(&path, source)
            .with_context(|| format!("write generated registry table {}", path.display()))?;
        written.push(path);
    }
    Ok(written)
}

pub fn check_registries(workspace_root: &Path, options: &GenRegistriesOptions) -> Result<()> {
    let (tables, out_dir) = load_registry_tables(workspace_root, options)?;
    let mut summaries = Vec::new();
    for table in tables {
        let expected =
            generate_registry_source(&table, &options.minecraft_version, options.protocol_version)?;
        let path = out_dir.join(table.spec.file_name);
        let actual = std::fs::read_to_string(&path)
            .with_context(|| format!("read generated registry table {}", path.display()))?;
        if actual != expected {
            summaries.push(packet_id_diff_summary(&path, &expected, &actual));
        }
    }
    if !summaries.is_empty() {
        bail!("{}", summaries.join("\n\n"));
    }
    Ok(())
}

fn load_registry_tables(
    workspace_root: &Path,
    options: &GenRegistriesOptions,
) -> Result<(Vec<RegistryTable>, PathBuf)> {
    let report_path = workspace_root
        .join(".cache")
        .join("mc")
        .join(&options.minecraft_version)
        .join("generated")
        .join("reports")
        .join("registries.json");
    let json = std::fs::read_to_string(&report_path)
        .with_context(|| format!("read registry report at {}", report_path.display()))?;
    let specs = resolve_registry_specs(&options.registries)?;
    let tables = parse_registry_report(&json, &specs)?;
    let out_dir = resolve_generated_dir(workspace_root, &options.out_dir)?;
    Ok((tables, out_dir))
}

fn resolve_registry_specs(registry_keys: &[String]) -> Result<Vec<RegistryCodegenSpec>> {
    let known = known_registry_specs();
    registry_keys
        .iter()
        .map(|key| {
            let normalized = normalize_registry_key(key);
            known
                .iter()
                .copied()
                .find(|spec| spec.registry_key == normalized)
                .ok_or_else(|| {
                    let supported = known
                        .iter()
                        .map(|spec| spec.registry_key.trim_start_matches("minecraft:"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    anyhow!("unsupported registry {normalized:?}; supported registries are {supported}")
                })
        })
        .collect()
}

fn resolve_generated_dir(workspace_root: &Path, requested: &Path) -> Result<PathBuf> {
    let relative = if requested.is_absolute() {
        requested.strip_prefix(workspace_root).with_context(|| {
            format!(
                "output path {} must be inside workspace {}",
                requested.display(),
                workspace_root.display()
            )
        })?
    } else {
        requested
    };

    validate_relative_child_path(relative)?;
    if !path_is_generated_dir(relative) {
        bail!(
            "refusing to write outside crates/lodestone-data/src/generated or crates/versions/*/src/generated; requested {}",
            requested.display()
        );
    }

    Ok(workspace_root.join(relative))
}

/// `crates/versions/*/src/generated` remains legal (a family-scoped override
/// is still a valid `--out-dir`, e.g. for a table this repo later decides is
/// genuinely per-protocol-family translation data rather than shared game
/// data), but `crates/lodestone-data/src/generated` is now the one the
/// default and `conformance` point at: `sound_events`/`particle_types`/
/// `menus`/`items`/`data_component_types` are game data, not protocol data,
/// and live there (see `docs/lodestone-data-crate.md`).
fn path_is_generated_dir(relative: &Path) -> bool {
    let components: Vec<&std::ffi::OsStr> = relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part),
            _ => None,
        })
        .collect();

    if let [crates, protocol, _crate_name, src, generated] = components.as_slice() {
        if *crates == "crates" && *protocol == "versions" && *src == "src" && *generated == "generated"
        {
            return true;
        }
    }

    matches!(
        components.as_slice(),
        [crates, data, src, generated]
            if *crates == "crates"
                && *data == "lodestone-data"
                && *src == "src"
                && *generated == "generated"
    )
}

pub fn registry_table<'a>(
    tables: &'a [RegistryTable],
    registry_key: &str,
) -> Result<&'a RegistryTable> {
    tables
        .iter()
        .find(|table| table.spec.registry_key == registry_key)
        .ok_or_else(|| anyhow!("missing parsed registry table {registry_key}"))
}

const VERSION_MANIFEST_URL: &str =
    "https://launchermeta.mojang.com/mc/game/version_manifest_v2.json";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetDownloads {
    pub client: DownloadSpec,
    pub server: DownloadSpec,
    pub asset_index: AssetIndexSpec,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DownloadSpec {
    pub url: String,
    pub sha1: String,
    pub size: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetIndexSpec {
    pub id: String,
    pub url: String,
    pub sha1: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DownloadDecision {
    Download,
    SkipValid,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JarAssetCounts {
    pub block_textures: usize,
    pub block_models: usize,
    pub blockstates: usize,
}

/// One asset-store object `fetch-assets` ensures is present: either one
/// `client.jar` shadows with a differently-sized stub, or one named in
/// [`REQUIRED_OBJECT_NAMES`]. See [`fetch_shadowed_objects`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FetchedObject {
    /// The asset-index name (no `assets/` prefix).
    pub name: String,
    /// Lowercase hex SHA-1, which is also the object's path.
    pub hash: String,
    /// The index's declared size — the real asset.
    pub size: u64,
    /// The size of the `client.jar` entry of the same name — the stub. `0` when
    /// the jar has no copy at all, which is the case for every
    /// [`REQUIRED_OBJECT_NAMES`] entry.
    pub jar_size: u64,
    /// Whether this run downloaded it (false = already cached and verified).
    pub downloaded: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FetchAssetsSummary {
    pub client_path: PathBuf,
    pub client_size: u64,
    pub client_downloaded: bool,
    pub asset_index_path: PathBuf,
    pub asset_index_size: u64,
    pub asset_index_downloaded: bool,
    pub jar_counts: JarAssetCounts,
    /// The asset-store objects this command ensures are present — the ones the jar
    /// shadows with a differently-sized stub, plus `REQUIRED_OBJECT_NAMES`. All are
    /// on disk and SHA-1 verified by the time this is returned.
    pub fetched_objects: Vec<FetchedObject>,
}

impl FetchAssetsSummary {
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = format!(
            "client.jar: {} ({} bytes, {})\nasset index: {} ({} bytes, {})\njar assets: block textures={}, block models={}, blockstates={}\n",
            self.client_path.display(),
            self.client_size,
            if self.client_downloaded {
                "downloaded"
            } else {
                "cached"
            },
            self.asset_index_path.display(),
            self.asset_index_size,
            if self.asset_index_downloaded {
                "downloaded"
            } else {
                "cached"
            },
            self.jar_counts.block_textures,
            self.jar_counts.block_models,
            self.jar_counts.blockstates
        );
        let fetched = self
            .fetched_objects
            .iter()
            .filter(|o| o.downloaded)
            .count();
        out.push_str(&format!(
            "asset objects: {} ({} downloaded, {} cached)\n",
            self.fetched_objects.len(),
            fetched,
            self.fetched_objects.len() - fetched
        ));
        for object in &self.fetched_objects {
            out.push_str(&format!(
                "  {} {} real={} {}\n",
                object.name,
                if object.jar_size == 0 {
                    "not-in-jar".to_string()
                } else {
                    format!("jar-stub={}", object.jar_size)
                },
                object.size,
                if object.downloaded {
                    "downloaded"
                } else {
                    "cached"
                }
            ));
        }
        out
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FetchVersionSummary {
    pub server_path: PathBuf,
    pub server_size: u64,
    pub server_downloaded: bool,
}

impl FetchVersionSummary {
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "server.jar: {} ({} bytes, {})",
            self.server_path.display(),
            self.server_size,
            if self.server_downloaded {
                "downloaded"
            } else {
                "cached"
            }
        )
    }
}

pub fn parse_version_manifest(json: &str, minecraft_version: &str) -> Result<String> {
    let root: Value = serde_json::from_str(json).context("parse Mojang version manifest")?;
    let versions = root
        .get("versions")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("version manifest is missing versions array"))?;

    for version in versions {
        if version.get("id").and_then(Value::as_str) == Some(minecraft_version) {
            return version
                .get("url")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .ok_or_else(|| anyhow!("version {minecraft_version} is missing url"));
        }
    }

    bail!("Minecraft version {minecraft_version} was not found in Mojang version manifest")
}

pub fn parse_asset_downloads(json: &str) -> Result<AssetDownloads> {
    let root: Value = serde_json::from_str(json).context("parse Mojang per-version JSON")?;
    let client = root
        .get("downloads")
        .and_then(|downloads| downloads.get("client"))
        .ok_or_else(|| anyhow!("version JSON is missing downloads.client"))?;
    let server = root
        .get("downloads")
        .and_then(|downloads| downloads.get("server"))
        .ok_or_else(|| anyhow!("version JSON is missing downloads.server"))?;
    let asset_index = root
        .get("assetIndex")
        .ok_or_else(|| anyhow!("version JSON is missing assetIndex"))?;

    Ok(AssetDownloads {
        client: DownloadSpec {
            url: required_string(client, "downloads.client.url")?,
            sha1: required_string(client, "downloads.client.sha1")?,
            size: client
                .get("size")
                .and_then(Value::as_u64)
                .ok_or_else(|| anyhow!("version JSON is missing downloads.client.size"))?,
        },
        server: DownloadSpec {
            url: required_string(server, "downloads.server.url")?,
            sha1: required_string(server, "downloads.server.sha1")?,
            size: server
                .get("size")
                .and_then(Value::as_u64)
                .ok_or_else(|| anyhow!("version JSON is missing downloads.server.size"))?,
        },
        asset_index: AssetIndexSpec {
            id: required_string(asset_index, "assetIndex.id")?,
            url: required_string(asset_index, "assetIndex.url")?,
            sha1: required_string(asset_index, "assetIndex.sha1")?,
        },
    })
}

fn required_string(object: &Value, path: &str) -> Result<String> {
    let key = path
        .rsplit('.')
        .next()
        .ok_or_else(|| anyhow!("invalid JSON path {path}"))?;
    object
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow!("version JSON is missing {path}"))
}

pub fn verify_sha1(path: &Path, expected_sha1: &str) -> Result<()> {
    let actual_sha1 = file_sha1_hex(path)?;
    if actual_sha1.eq_ignore_ascii_case(expected_sha1) {
        return Ok(());
    }

    bail!(
        "SHA-1 mismatch for {}: expected {}, got {}",
        path.display(),
        expected_sha1,
        actual_sha1
    )
}

pub fn download_decision(
    path: &Path,
    expected_sha1: &str,
    force: bool,
) -> Result<DownloadDecision> {
    if force || !path.exists() {
        return Ok(DownloadDecision::Download);
    }

    match verify_sha1(path, expected_sha1) {
        Ok(()) => Ok(DownloadDecision::SkipValid),
        Err(error) => {
            let message = error.to_string();
            if message.contains("SHA-1 mismatch") {
                Ok(DownloadDecision::Download)
            } else {
                Err(error)
            }
        }
    }
}

pub fn fetch_assets(
    workspace_root: &Path,
    minecraft_version: &str,
    force: bool,
) -> Result<FetchAssetsSummary> {
    let manifest_json = curl_to_string(VERSION_MANIFEST_URL)?;
    let version_json_url = parse_version_manifest(&manifest_json, minecraft_version)?;
    let version_json = curl_to_string(&version_json_url)?;
    let downloads = parse_asset_downloads(&version_json)?;

    let version_cache = workspace_root
        .join(".cache")
        .join("mc")
        .join(minecraft_version);
    std::fs::create_dir_all(&version_cache)
        .with_context(|| format!("create asset cache directory {}", version_cache.display()))?;

    let client_path = version_cache.join("client.jar");
    let client_downloaded = download_verified_file(
        &downloads.client.url,
        &client_path,
        &downloads.client.sha1,
        force,
    )
    .context("download and verify client.jar")?;

    let asset_index_path =
        version_cache.join(format!("asset-index-{}.json", downloads.asset_index.id));
    let asset_index_downloaded = download_verified_file(
        &downloads.asset_index.url,
        &asset_index_path,
        &downloads.asset_index.sha1,
        force,
    )
    .context("download and verify asset index")?;

    let client_size = std::fs::metadata(&client_path)
        .with_context(|| format!("stat {}", client_path.display()))?
        .len();
    if client_size != downloads.client.size {
        bail!(
            "client.jar size mismatch for {}: expected {} bytes, got {} bytes",
            client_path.display(),
            downloads.client.size,
            client_size
        );
    }

    let asset_index_size = std::fs::metadata(&asset_index_path)
        .with_context(|| format!("stat {}", asset_index_path.display()))?
        .len();
    let jar_counts = count_client_jar_assets(&client_path)?;
    if jar_counts.block_textures == 0 || jar_counts.block_models == 0 || jar_counts.blockstates == 0
    {
        bail!(
            "client.jar did not contain the expected vanilla asset layout: block textures={}, block models={}, blockstates={}",
            jar_counts.block_textures,
            jar_counts.block_models,
            jar_counts.blockstates
        );
    }

    let fetched_objects =
        fetch_shadowed_objects(&version_cache, &asset_index_path, &client_path, force)
            .context("download and verify the required asset-store objects")?;

    Ok(FetchAssetsSummary {
        client_path,
        client_size,
        client_downloaded,
        asset_index_path,
        asset_index_size,
        asset_index_downloaded,
        jar_counts,
        fetched_objects,
    })
}

/// Base URL of the launcher's content-addressed asset store.
const RESOURCES_BASE_URL: &str = "https://resources.download.minecraft.net";

/// Logical asset names to fetch **regardless** of whether `client.jar` shadows
/// them, because something refuses to start without them.
///
/// `minecraft/sounds.json` is the whole list today: `ShellAudio::load_from_root`
/// reads it eagerly and returns an error if it is absent, so without it audio does
/// not come up at all. It is one 626 KB file describing 1968 sound events, and it
/// is *not* jar-shadowed — the jar has no copy — so the size-disagreement rule
/// below would never select it.
///
/// `minecraft/font/unifont.zip` is the second, and it is the *data* half of a
/// jar-shadowed pair rather than a shadowed file itself. `font/include/unifont.json`
/// **is** shadowed (29 B stub in the jar, 3993 B real file in the store), so the
/// size-disagreement rule below already selects it — but that file's only content
/// is a `unihex` provider pointing at `font/unifont.zip`, which the jar does not
/// contain at all and so the rule cannot see. Fetching the declaration without the
/// data gives a font that resolves a unihex provider, finds no `hex_file`, and
/// draws the missing-glyph box for all 112,018 codepoints outside the three bitmap
/// sheets — the same symptom as not fetching either. 1.5 MB of GNU Unifont HEX
/// text; the `unifont_jp` and `unifont_pua` variants are behind the `jp` font
/// option and a private-use pack respectively, and are not needed to render.
///
/// The 4871 `.ogg` samples `sounds.json` references are deliberately **not** here.
/// They are 375 MB, and unlike a stub they fail *honestly*: a missing sample is one
/// silent sound, resolved lazily per event, not a wrong asset masquerading as the
/// right one. Someone wanting real audio should fetch them with [`ensure_object`]
/// rather than by growing this list.
const REQUIRED_OBJECT_NAMES: &[&str] =
    &["minecraft/sounds.json", "minecraft/font/unifont.zip"];

/// Ensure one asset-store object is on disk, verifying its SHA-1 against the
/// index.
///
/// This is the general primitive — *given a logical asset name, make the object
/// present and prove it is the right bytes* — that everything else here is built
/// from. `name` is an asset-index name with no `assets/` prefix. Returns whether
/// this call downloaded it (`false` = already cached and verified).
///
/// The hash is both the object's address and its integrity check, which is the
/// only one available: there is no signature and no manifest beyond the index.
/// [`download_verified_file`] does the verify-then-rename, so a failed digest
/// leaves nothing behind.
///
/// # Errors
///
/// Returns an error when `name` is not in the index, its hash is implausible, the
/// download fails, the SHA-1 does not match, or the resulting file's length
/// disagrees with the index.
pub fn ensure_object(
    version_cache: &Path,
    index: &serde_json::Map<String, Value>,
    name: &str,
    force: bool,
) -> Result<bool> {
    let meta = index
        .get(name)
        .ok_or_else(|| anyhow!("asset index has no object named {name:?}"))?;
    let hash = meta
        .get("hash")
        .and_then(|h| h.as_str())
        .ok_or_else(|| anyhow!("asset index entry {name:?} has no hash"))?;
    if hash.len() < 2 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("asset index entry {name:?} has an implausible hash {hash:?}");
    }
    let size = meta
        .get("size")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("asset index entry {name:?} has no size"))?;

    let destination = version_cache
        .join("objects")
        .join(&hash[0..2])
        .join(hash);
    let url = format!("{RESOURCES_BASE_URL}/{}/{hash}", &hash[0..2]);
    let downloaded = download_verified_file(&url, &destination, hash, force)
        .with_context(|| format!("download asset object {name} ({url})"))?;

    let on_disk = std::fs::metadata(&destination)
        .with_context(|| format!("stat {}", destination.display()))?
        .len();
    if on_disk != size {
        bail!(
            "asset object {name} size mismatch: index says {size} bytes, {} has {on_disk}",
            destination.display()
        );
    }
    Ok(downloaded)
}

/// Download every asset-store object whose name is **also** a `client.jar` entry
/// of a *different* size — i.e. every object the jar shadows with a stub — plus
/// [`REQUIRED_OBJECT_NAMES`].
///
/// # Why this exists, and why the boundary is where it is
///
/// `client.jar` ships deliberate stubs for a handful of files the object store
/// overrides, and reading the jar copy silently gives you the stub. Measured on
/// 26.2: of 5057 index objects exactly **8** share a name with a jar entry, and
/// all 8 differ in size —
///
/// | name | jar | real |
/// |---|---|---|
/// | `textures/gui/title/background/panorama_0.png` | 69 | 547,239 |
/// | `panorama_1.png` | 69 | 294,940 |
/// | `panorama_2.png` | 69 | 425,769 |
/// | `panorama_3.png` | 69 | 461,522 |
/// | `panorama_4.png` | 69 | 738,917 |
/// | `panorama_5.png` | 69 | 118,484 |
/// | `font/include/unifont.json` | 29 | 3,993 |
/// | `panorama_overlay.png` | 68 | 86 |
///
/// — about 2.6 MB in total. The title-screen panorama was ported against those
/// stubs and shipped a flat grey sky that looked like working code; this command
/// is what stops that recurring.
///
/// **The shadowed set is derived from the data, not hardcoded.** A name list would
/// rot at the next version bump; "present in both, sizes disagree" cannot. The one
/// hardcoded part is [`REQUIRED_OBJECT_NAMES`], for objects the jar does not
/// shadow but something refuses to start without.
///
/// It also keeps the boundary honest: this deliberately does **not** fetch the
/// remaining index-only objects, 4871 of which are `.ogg` samples totalling
/// 375 MB. A missing sample fails *honestly* — one silent sound, resolved lazily
/// per event — whereas nothing at runtime can tell a stub from the real asset.
/// [`ensure_object`] is the primitive to reach for if you do want the corpus.
///
/// SHA-1 is verified against the index hash after download by
/// [`download_verified_file`] — that is what the index hash is for, and the only
/// integrity check available here.
///
/// # Errors
///
/// Propagates a read/parse failure of the index or the jar, and any download or
/// SHA-1 verification failure.
pub fn fetch_shadowed_objects(
    version_cache: &Path,
    asset_index_path: &Path,
    client_path: &Path,
    force: bool,
) -> Result<Vec<FetchedObject>> {
    let index_bytes = std::fs::read(asset_index_path)
        .with_context(|| format!("read asset index {}", asset_index_path.display()))?;
    let index: serde_json::Value = serde_json::from_slice(&index_bytes)
        .with_context(|| format!("parse asset index {}", asset_index_path.display()))?;
    let objects = index
        .get("objects")
        .and_then(|o| o.as_object())
        .ok_or_else(|| anyhow!("asset index has no \"objects\" map"))?;

    // Jar entry sizes, keyed the way the index names things (no `assets/`).
    let file = File::open(client_path)
        .with_context(|| format!("open client jar {}", client_path.display()))?;
    let mut archive = zip::ZipArchive::new(file)
        .with_context(|| format!("read zip {}", client_path.display()))?;
    let mut jar_sizes: BTreeMap<String, u64> = BTreeMap::new();
    for i in 0..archive.len() {
        let entry = archive
            .by_index(i)
            .with_context(|| format!("read zip entry {i} of {}", client_path.display()))?;
        let name = entry.name();
        if let Some(key) = name.strip_prefix("assets/") {
            jar_sizes.insert(key.to_string(), entry.size());
        }
    }

    let mut wanted: Vec<FetchedObject> = Vec::new();
    for (name, meta) in objects {
        let Some(size) = meta.get("size").and_then(Value::as_u64) else {
            continue;
        };
        let jar_size = jar_sizes.get(name.as_str()).copied();
        let shadowed_stub = jar_size.is_some_and(|jar| jar != size);
        let required = REQUIRED_OBJECT_NAMES.contains(&name.as_str());
        if !shadowed_stub && !required {
            // Either byte-for-byte the same asset in both places (the jar copy is
            // fine), or index-only and nothing refuses to start without it.
            continue;
        }
        let hash = meta
            .get("hash")
            .and_then(|h| h.as_str())
            .ok_or_else(|| anyhow!("asset index entry {name:?} has no hash"))?;
        wanted.push(FetchedObject {
            name: name.clone(),
            hash: hash.to_string(),
            // `jar_size` is 0 for a required-but-unshadowed object, which is what
            // the summary prints and is honest: there is no jar copy.
            jar_size: jar_size.unwrap_or(0),
            size,
            downloaded: false,
        });
    }
    wanted.sort_by(|a, b| a.name.cmp(&b.name));

    // Every name in `REQUIRED_OBJECT_NAMES` must have been found. A typo there
    // would otherwise silently fetch nothing and leave audio dead exactly as
    // before, which is the failure this whole change exists to stop.
    for required in REQUIRED_OBJECT_NAMES {
        if !wanted.iter().any(|o| o.name == *required) {
            bail!(
                "required asset object {required:?} is not in {} — the name is \
                 wrong, or this version's index does not carry it",
                asset_index_path.display()
            );
        }
    }

    for object in &mut wanted {
        let downloaded = ensure_object(version_cache, objects, &object.name, force)?;
        object.downloaded = downloaded;
    }

    Ok(wanted)
}

/// The `.ogg` corpus a `fetch-sounds` run should ensure is present, split out of
/// `sounds.json` by [`plan_sound_corpus`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SoundCorpus {
    /// Index names to fetch, sorted, with their declared sizes.
    pub wanted: Vec<(String, u64)>,
    /// Index names deliberately left out (music and records under the default
    /// policy), sorted, with their declared sizes. Reported, never fetched.
    pub excluded: Vec<(String, u64)>,
    /// `.ogg` objects in the index that **no** `sounds.json` event references.
    /// Neither mode fetches these: nothing can select them.
    pub unreferenced: Vec<(String, u64)>,
    /// Distinct events walked (`sounds.json` top-level keys).
    pub events: usize,
}

impl SoundCorpus {
    /// Total declared bytes of [`Self::wanted`].
    #[must_use]
    pub fn wanted_bytes(&self) -> u64 {
        self.wanted.iter().map(|(_, size)| size).sum()
    }

    /// Total declared bytes of [`Self::excluded`].
    #[must_use]
    pub fn excluded_bytes(&self) -> u64 {
        self.excluded.iter().map(|(_, size)| size).sum()
    }
}

/// True for a `sounds.json` event key that is background music or a music disc.
///
/// This is the **whole** exclusion policy, and it is a two-token prefix over the
/// event namespace rather than a list of files, because a file list rots at the
/// next version bump and a namespace does not. 26.2's music events are `music.*`
/// (biome/menu/credits tracks) and `music_disc.*` (jukebox records); the bare key
/// `music` is accepted defensively in case a version ever ships one.
pub fn is_music_event(event: &str) -> bool {
    event == "music" || event.starts_with("music.") || event.starts_with("music_disc.")
}

/// The asset-index name of a `sounds.json` sound name: `entity.zombie.hurt`'s
/// `"mob/zombie/hurt1"` becomes `minecraft/sounds/mob/zombie/hurt1.ogg`.
///
/// A name may carry its own namespace (`somepack:foo/bar`), in which case that
/// namespace replaces `minecraft`. Measured on 26.2: all 4843 distinct names
/// resolve to a real index entry through this rule, with zero misses — which is
/// what makes the derivation trustworthy rather than approximate.
pub fn sound_object_name(sound_name: &str) -> String {
    let (namespace, path) = match sound_name.split_once(':') {
        Some((namespace, path)) => (namespace, path),
        None => ("minecraft", sound_name),
    };
    format!("{namespace}/sounds/{path}.ogg")
}

/// Decide which `.ogg` objects to fetch, **derived from `sounds.json`** rather
/// than from a hand-written list.
///
/// # Why this is derived and what the derivation is
///
/// The index declares 4871 `.ogg` objects totalling 375 MB, so an unconditional
/// fetch is not acceptable and a curated file list would be stale within one
/// version. Instead this walks every event in `sounds.json` (1968 of them on
/// 26.2), collects each entry's sound name, and resolves it to an index name.
/// `"type": "event"` entries are indirections to another event and contribute no
/// sample of their own, so they are skipped — the event they name is walked in its
/// own right.
///
/// A sample is **excluded** only when *every* event that references it is a music
/// event ([`is_music_event`]). "Every", not "any": a sample shared between a music
/// event and a world event still has to be fetched, and phrasing the rule the
/// other way round would silently drop it.
///
/// # What the default covers, measured on 26.2
///
/// | set | objects | bytes |
/// |---|---|---|
/// | fetched (default) | 4751 | 80.14 MB |
/// | excluded: 70 music tracks + 22 records | 92 | 293.23 MB |
/// | referenced by no event at all | 28 | — |
///
/// So the default is **every sample any non-music event can select**: mobs,
/// blocks, items, entities, steps, digs, liquid, UI, notes, enchanting,
/// fireworks, minecarts, portals — *and* all six biome ambience loops, which is
/// the reason the rule is "music events" and not vanilla's `"stream": true` flag.
/// `stream: true` is the other candidate derivation and it is cheaper to state,
/// but it selects 98 samples including those six nether/underwater loops, so it
/// would silence cave and nether ambience to save 2.9 MB. Measured, not assumed.
///
/// `include_music` (the `--all` flag) folds the excluded set back in: 4843
/// objects, 373.37 MB. The 28 unreferenced objects are never fetched in either
/// mode — no event can select them, so they would be 28 downloads no code path
/// can reach.
///
/// # Errors
///
/// Returns an error when `sounds_json` is not a JSON object, when it is empty, or
/// when a resolved name is not in the asset index — the last of which means the
/// resolution rule is wrong for this version and must be fixed rather than
/// worked around.
pub fn plan_sound_corpus(
    index: &serde_json::Map<String, Value>,
    sounds_json: &[u8],
    include_music: bool,
) -> Result<SoundCorpus> {
    let parsed: Value =
        serde_json::from_slice(sounds_json).context("parse minecraft/sounds.json")?;
    let events = parsed
        .as_object()
        .ok_or_else(|| anyhow!("sounds.json is not a JSON object of event -> definition"))?;
    if events.is_empty() {
        bail!("sounds.json declares no events");
    }

    // name -> whether every referencing event so far is a music event. Starting
    // from `true` and `&&`-ing keeps the "only music references it" semantics.
    let mut only_music: BTreeMap<String, bool> = BTreeMap::new();
    for (event, definition) in events {
        let Some(entries) = definition.get("sounds").and_then(Value::as_array) else {
            continue;
        };
        let music = is_music_event(event);
        for entry in entries {
            let name = match entry {
                // The shorthand form: a bare string is a sound name.
                Value::String(name) => name.as_str(),
                Value::Object(map) => {
                    // `"type": "event"` names another event, not a file.
                    if map.get("type").and_then(Value::as_str).unwrap_or("sound") != "sound" {
                        continue;
                    }
                    match map.get("name").and_then(Value::as_str) {
                        Some(name) => name,
                        None => continue,
                    }
                }
                _ => continue,
            };
            let object = sound_object_name(name);
            only_music
                .entry(object)
                .and_modify(|flag| *flag = *flag && music)
                .or_insert(music);
        }
    }

    let mut corpus = SoundCorpus {
        events: events.len(),
        ..SoundCorpus::default()
    };
    for (name, music_only) in &only_music {
        let size = index
            .get(name)
            .and_then(|meta| meta.get("size"))
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                anyhow!(
                    "sounds.json references {name:?}, which the asset index does not \
                     declare — the sound-name resolution rule is wrong for this version"
                )
            })?;
        if *music_only && !include_music {
            corpus.excluded.push((name.clone(), size));
        } else {
            corpus.wanted.push((name.clone(), size));
        }
    }

    // Index-only `.ogg` objects no event mentions. Reported so the count is
    // visible rather than looking like a shortfall in the plan.
    for (name, meta) in index {
        if !name.ends_with(".ogg") || only_music.contains_key(name) {
            continue;
        }
        let size = meta.get("size").and_then(Value::as_u64).unwrap_or(0);
        corpus.unreferenced.push((name.clone(), size));
    }

    // `wanted` and `excluded` came out of a `BTreeMap`, so they are already sorted;
    // `unreferenced` came out of the index (a `serde_json::Map`, insertion-ordered
    // unless the `preserve_order` feature is off), so sort it explicitly. Stable
    // output matters: the summary is read by a human comparing two runs.
    corpus.unreferenced.sort();
    Ok(corpus)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FetchSoundsSummary {
    pub asset_index_path: PathBuf,
    /// The plan this run executed.
    pub corpus: SoundCorpus,
    /// How many objects this run actually downloaded.
    pub downloaded: usize,
    /// How many were already present and SHA-1 verified.
    pub cached: usize,
    /// Whether music and records were included (`--all`).
    pub included_music: bool,
}

impl FetchSoundsSummary {
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = format!(
            "asset index: {}\nsounds.json: {} events\nsamples: {} ({} downloaded, {} cached), {:.2} MB\n",
            self.asset_index_path.display(),
            self.corpus.events,
            self.corpus.wanted.len(),
            self.downloaded,
            self.cached,
            self.corpus.wanted_bytes() as f64 / 1e6,
        );
        if self.included_music {
            out.push_str("music and records: included (--all)\n");
        } else {
            out.push_str(&format!(
                "music and records: {} objects, {:.2} MB NOT fetched — re-run with --all for \
                 background music and jukebox discs\n",
                self.corpus.excluded.len(),
                self.corpus.excluded_bytes() as f64 / 1e6,
            ));
        }
        if !self.corpus.unreferenced.is_empty() {
            out.push_str(&format!(
                "unreferenced: {} .ogg objects no sounds.json event names (never fetched)\n",
                self.corpus.unreferenced.len()
            ));
        }
        out
    }
}

/// Default worker count for [`fetch_sounds`].
///
/// Each object is one `curl` process, and the corpus averages ~17 KB per file, so
/// wall time is dominated by connection setup rather than bandwidth: serial would
/// be ~4751 round trips. Twelve is well within what `resources.download.minecraft.net`
/// serves without throttling and keeps a cold fetch in the low minutes.
pub const SOUND_FETCH_JOBS: usize = 12;

/// Ensure the `.ogg` corpus [`plan_sound_corpus`] selected is on disk, SHA-1
/// verified against the asset index.
///
/// This is deliberately **not** part of `fetch-assets`: 80 MB (or 373 with
/// `--all`) must be an explicit act, and a missing sample degrades honestly — one
/// silent sound — where a `client.jar` stub lies. See
/// [`fetch_shadowed_objects`] for the other half of that boundary.
///
/// Every object goes through [`ensure_object`], which verifies the SHA-1 the
/// index declares; there is no second fetcher here. A re-run of a complete fetch
/// downloads nothing: it re-hashes what is on disk (80 MB of SHA-1, well under a
/// second) and reports every object as cached.
///
/// # Errors
///
/// Returns an error when the version cache has no single `asset-index-*.json`,
/// when `minecraft/sounds.json` is not in the store (run `fetch-assets` first),
/// or on the first download or SHA-1 failure — reported with the object that
/// failed, not as a bare count.
pub fn fetch_sounds(
    workspace_root: &Path,
    minecraft_version: &str,
    include_music: bool,
    force: bool,
    jobs: usize,
) -> Result<FetchSoundsSummary> {
    let version_cache = workspace_root
        .join(".cache")
        .join("mc")
        .join(minecraft_version);
    let asset_index_path = find_cached_asset_index(&version_cache)?;
    let index_bytes = std::fs::read(&asset_index_path)
        .with_context(|| format!("read asset index {}", asset_index_path.display()))?;
    let index_json: Value = serde_json::from_slice(&index_bytes)
        .with_context(|| format!("parse asset index {}", asset_index_path.display()))?;
    let index = index_json
        .get("objects")
        .and_then(|objects| objects.as_object())
        .ok_or_else(|| anyhow!("asset index has no \"objects\" map"))?;

    // sounds.json is the plan's input, so it has to be present first. It is in
    // `REQUIRED_OBJECT_NAMES`, meaning `fetch-assets` already put it there;
    // ensuring it again here makes this command usable on its own.
    ensure_object(&version_cache, index, "minecraft/sounds.json", false)
        .context("ensure minecraft/sounds.json (the corpus is derived from it)")?;
    let sounds_hash = index
        .get("minecraft/sounds.json")
        .and_then(|meta| meta.get("hash"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("asset index has no hash for minecraft/sounds.json"))?;
    let sounds_path = version_cache
        .join("objects")
        .join(&sounds_hash[0..2])
        .join(sounds_hash);
    let sounds_json = std::fs::read(&sounds_path)
        .with_context(|| format!("read {}", sounds_path.display()))?;

    let corpus = plan_sound_corpus(index, &sounds_json, include_music)?;
    let total = corpus.wanted.len();
    println!(
        "fetch-sounds: {total} samples, {:.2} MB declared ({} events in sounds.json)",
        corpus.wanted_bytes() as f64 / 1e6,
        corpus.events
    );
    if !include_music {
        println!(
            "  skipping {} music/record objects ({:.2} MB); --all includes them",
            corpus.excluded.len(),
            corpus.excluded_bytes() as f64 / 1e6
        );
    }

    let jobs = jobs.max(1);
    let cursor = std::sync::atomic::AtomicUsize::new(0);
    let done = std::sync::atomic::AtomicUsize::new(0);
    let downloaded = std::sync::atomic::AtomicUsize::new(0);
    let failure: std::sync::Mutex<Option<anyhow::Error>> = std::sync::Mutex::new(None);

    std::thread::scope(|scope| {
        for _ in 0..jobs {
            scope.spawn(|| {
                loop {
                    // A failure anywhere stops every worker: the first error is
                    // the useful one, and 4750 more retries after a dead CDN are
                    // not.
                    if failure.lock().is_ok_and(|slot| slot.is_some()) {
                        return;
                    }
                    let next = cursor.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let Some((name, _size)) = corpus.wanted.get(next) else {
                        return;
                    };
                    match ensure_object(&version_cache, index, name, force) {
                        Ok(true) => {
                            downloaded.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        }
                        Ok(false) => {}
                        Err(error) => {
                            if let Ok(mut slot) = failure.lock() {
                                slot.get_or_insert(error);
                            }
                            return;
                        }
                    }
                    let finished = done.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    // One line per 250 objects: enough to see it moving, few
                    // enough not to bury the summary.
                    if finished % 250 == 0 || finished == total {
                        println!(
                            "  {finished}/{total} ({} downloaded)",
                            downloaded.load(std::sync::atomic::Ordering::SeqCst)
                        );
                    }
                }
            });
        }
    });

    if let Some(error) = failure.into_inner().ok().flatten() {
        return Err(error);
    }

    let downloaded = downloaded.into_inner();
    Ok(FetchSoundsSummary {
        asset_index_path,
        downloaded,
        cached: total - downloaded,
        corpus,
        included_music: include_music,
    })
}

/// The single `asset-index-*.json` already in a version cache.
///
/// `fetch-assets` learns the index id from Mojang's manifest; this command works
/// off what is on disk instead, so it needs no network before it can plan. Zero
/// or several candidates is an error rather than a guess — several client versions
/// coexist under `.cache/mc` and "first match wins over a shared directory" is a
/// known landmine here.
///
/// # Errors
///
/// Returns an error when the directory is unreadable, holds no
/// `asset-index-*.json`, or holds more than one.
fn find_cached_asset_index(version_cache: &Path) -> Result<PathBuf> {
    let entries = std::fs::read_dir(version_cache).with_context(|| {
        format!(
            "read {} — run: cargo run -p xtask -- fetch-assets --version <version>",
            version_cache.display()
        )
    })?;
    let mut matches: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("asset-index-") && name.ends_with(".json") {
            matches.push(entry.path());
        }
    }
    matches.sort();
    match matches.len() {
        0 => bail!(
            "no asset-index-*.json in {} — run: cargo run -p xtask -- fetch-assets --version <version>",
            version_cache.display()
        ),
        1 => Ok(matches.remove(0)),
        n => bail!(
            "{n} asset-index-*.json files in {}; refusing to guess",
            version_cache.display()
        ),
    }
}

pub fn fetch_version(
    workspace_root: &Path,
    minecraft_version: &str,
    force: bool,
) -> Result<FetchVersionSummary> {
    let manifest_json = curl_to_string(VERSION_MANIFEST_URL)?;
    let version_json_url = parse_version_manifest(&manifest_json, minecraft_version)?;
    let version_json = curl_to_string(&version_json_url)?;
    let downloads = parse_asset_downloads(&version_json)?;

    let version_cache = workspace_root
        .join(".cache")
        .join("mc")
        .join(minecraft_version);
    let server_path = version_cache.join("server.jar");
    let server_downloaded = download_verified_file(
        &downloads.server.url,
        &server_path,
        &downloads.server.sha1,
        force,
    )
    .context("download and verify server.jar")?;
    let server_size = std::fs::metadata(&server_path)
        .with_context(|| format!("stat {}", server_path.display()))?
        .len();
    if server_size != downloads.server.size {
        bail!(
            "server.jar size mismatch for {}: expected {} bytes, got {} bytes",
            server_path.display(),
            downloads.server.size,
            server_size
        );
    }

    Ok(FetchVersionSummary {
        server_path,
        server_size,
        server_downloaded,
    })
}

/// The sixteen versions this workspace tracks: the latest patch of every
/// major Minecraft release from 1.7.10 through the current 26.2.
///
/// This is the one place that list is spelled out; [`version_table_report`]
/// walks it in order, so adding or removing a target version is a one-line
/// change here plus a `cargo run -p xtask -- version-table` regen.
pub const EPIC_343_VERSIONS: &[&str] = &[
    "1.7.10", "1.8.9", "1.9.4", "1.10.2", "1.11.2", "1.12.2", "1.13.2", "1.14.4", "1.15.2",
    "1.16.5", "1.17.1", "1.18.2", "1.19.4", "1.20.6", "1.21.11", "26.2",
];

/// Relative path (from the workspace root) to `vendor/minecraft-data`'s
/// cross-version protocol/data-version index. Covers 1.8 (as `1.7`/`1.7.10`
/// entries in this specific file) through 1.21.11, plus — measured directly,
/// contrary to this repo's usual "no 26.x data" caveat about
/// `vendor/minecraft-data` — a 26.2 entry too. See `docs/version-table.md`
/// for what that means and does not mean.
const MINECRAFT_DATA_PROTOCOL_VERSIONS: &str =
    "vendor/minecraft-data/data/pc/common/protocolVersions.json";

/// Where one field of a [`VersionTableEntry`] was sourced from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VersionSource {
    /// `version.json` at the root of the vanilla server jar. Present since
    /// 18w47b; the oldest version in `EPIC_343_VERSIONS` that ships it is
    /// 1.14.4. Authoritative per `CLAUDE.md`'s "Data sources, in order".
    JarVersionJson,
    /// `vendor/minecraft-data`'s `protocolVersions.json`. Used only for
    /// versions whose jar predates `version.json`; cross-check-grade, never
    /// authoritative, per `CLAUDE.md`.
    MinecraftData,
}

impl VersionSource {
    #[must_use]
    pub const fn as_rust_expr(self) -> &'static str {
        match self {
            Self::JarVersionJson => "Source::JarVersionJson",
            Self::MinecraftData => "Source::MinecraftData",
        }
    }
}

/// One resolved row of the version table.
#[derive(Clone, Debug)]
pub struct VersionTableEntry {
    pub minecraft_version: String,
    pub protocol_version: i32,
    pub data_version: i32,
    /// ISO-8601 `releaseTime` from Mojang's version manifest.
    pub release_date: String,
    pub protocol_source: VersionSource,
    pub data_version_source: VersionSource,
    /// True when both a jar `version.json` and a `minecraft-data` entry were
    /// available and cross-checked in agreement. False when only one source
    /// was available (nothing to cross-check) or the two disagreed — a
    /// disagreement is a hard error in [`resolve_version_table_entry`], not a
    /// row you can see with this flag set, so `false` here just means
    /// single-sourced.
    pub cross_checked: bool,
}

/// Minimal parsed shape of Mojang's per-version JSON, just the pieces
/// `version-table` needs beyond what [`parse_asset_downloads`] already
/// extracts.
fn manifest_release_time(manifest_json: &str, minecraft_version: &str) -> Result<String> {
    let root: Value =
        serde_json::from_str(manifest_json).context("parse Mojang version manifest")?;
    let versions = root
        .get("versions")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("version manifest is missing versions array"))?;

    for version in versions {
        if version.get("id").and_then(Value::as_str) == Some(minecraft_version) {
            return version
                .get("releaseTime")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .ok_or_else(|| anyhow!("version {minecraft_version} is missing releaseTime"));
        }
    }

    bail!("Minecraft version {minecraft_version} was not found in Mojang version manifest")
}

/// A `(protocol_version, data_version)` pair sourced from the vanilla server
/// jar's embedded `version.json`, when present.
struct JarVersionInfo {
    protocol_version: i32,
    data_version: i32,
}

/// Reads `version.json` from the root of a vanilla server jar, if present.
///
/// Returns `Ok(None)` for jars that predate the file (18w47b / 1.14) rather
/// than treating absence as an error — the caller falls back to
/// `minecraft-data` in that case.
fn read_jar_version_json(jar_path: &Path) -> Result<Option<JarVersionInfo>> {
    let file =
        File::open(jar_path).with_context(|| format!("open jar {}", jar_path.display()))?;
    let mut archive = zip::ZipArchive::new(file)
        .with_context(|| format!("read zip {}", jar_path.display()))?;

    let mut entry = match archive.by_name("version.json") {
        Ok(entry) => entry,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| {
                format!("read version.json from {}", jar_path.display())
            });
        }
    };

    let mut contents = String::new();
    entry
        .read_to_string(&mut contents)
        .with_context(|| format!("read version.json contents from {}", jar_path.display()))?;
    drop(entry);

    let root: Value = serde_json::from_str(&contents)
        .with_context(|| format!("parse version.json from {}", jar_path.display()))?;
    let protocol_version = root
        .get("protocol_version")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("{}: version.json missing protocol_version", jar_path.display()))?;
    let data_version = root
        .get("world_version")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("{}: version.json missing world_version", jar_path.display()))?;

    Ok(Some(JarVersionInfo {
        protocol_version: i32::try_from(protocol_version)
            .with_context(|| format!("protocol_version {protocol_version} out of i32 range"))?,
        data_version: i32::try_from(data_version)
            .with_context(|| format!("world_version {data_version} out of i32 range"))?,
    }))
}

/// One row of `vendor/minecraft-data`'s `protocolVersions.json`, filtered to
/// the fields `version-table` needs.
struct MinecraftDataProtocolEntry {
    protocol_version: i32,
    data_version: i32,
}

/// Looks up `minecraft_version` in `vendor/minecraft-data`'s
/// `protocolVersions.json` by exact `minecraftVersion` string match.
///
/// Returns `Ok(None)` when the file has no entry for that exact version
/// string (this repo's cross-check source, never its authority — see
/// `CLAUDE.md`'s "Data sources, in order").
fn minecraft_data_protocol_entry(
    workspace_root: &Path,
    minecraft_version: &str,
) -> Result<Option<MinecraftDataProtocolEntry>> {
    let path = workspace_root.join(MINECRAFT_DATA_PROTOCOL_VERSIONS);
    let json = std::fs::read_to_string(&path)
        .with_context(|| format!("read {}", path.display()))?;
    let entries: Vec<Value> =
        serde_json::from_str(&json).with_context(|| format!("parse {}", path.display()))?;

    for entry in &entries {
        if entry.get("minecraftVersion").and_then(Value::as_str) != Some(minecraft_version) {
            continue;
        }
        let protocol_version = entry
            .get("version")
            .and_then(Value::as_i64)
            .ok_or_else(|| anyhow!("{}: entry for {minecraft_version} missing version", path.display()))?;
        let data_version = entry
            .get("dataVersion")
            .and_then(Value::as_i64)
            .ok_or_else(|| {
                anyhow!("{}: entry for {minecraft_version} missing dataVersion", path.display())
            })?;
        return Ok(Some(MinecraftDataProtocolEntry {
            protocol_version: i32::try_from(protocol_version)
                .with_context(|| format!("protocol_version {protocol_version} out of i32 range"))?,
            data_version: i32::try_from(data_version)
                .with_context(|| format!("dataVersion {data_version} out of i32 range"))?,
        }));
    }

    Ok(None)
}

/// Resolves one [`VersionTableEntry`] for `minecraft_version`.
///
/// Prefers the jar's `version.json` (authoritative) when a server jar is
/// already cached at `.cache/mc/<minecraft_version>/server.jar` — or,
/// with `fetch_missing`, downloads it first via [`fetch_version`]. Falls
/// back to `minecraft-data` when the jar has no `version.json` (every
/// version in `EPIC_343_VERSIONS` at or before 1.13.2, confirmed empirically:
/// see `docs/version-table.md`). When both sources are available, disagreement
/// is a hard error — this table has no silent-drift path.
fn resolve_version_table_entry(
    workspace_root: &Path,
    manifest_json: &str,
    minecraft_version: &str,
    fetch_missing: bool,
) -> Result<VersionTableEntry> {
    let release_date = manifest_release_time(manifest_json, minecraft_version)?;

    let jar_path = workspace_root
        .join(".cache")
        .join("mc")
        .join(minecraft_version)
        .join("server.jar");

    if fetch_missing && !jar_path.exists() {
        fetch_version(workspace_root, minecraft_version, false)
            .with_context(|| format!("fetch server.jar for {minecraft_version}"))?;
    }

    let jar_info = if jar_path.exists() {
        read_jar_version_json(&jar_path)?
    } else {
        None
    };
    let data_entry = minecraft_data_protocol_entry(workspace_root, minecraft_version)?;

    let (protocol_version, protocol_source) = match (&jar_info, &data_entry) {
        (Some(jar), Some(data)) if jar.protocol_version != data.protocol_version => bail!(
            "{minecraft_version}: jar version.json protocol_version {} disagrees with minecraft-data {}",
            jar.protocol_version,
            data.protocol_version
        ),
        (Some(jar), _) => (jar.protocol_version, VersionSource::JarVersionJson),
        (None, Some(data)) => (data.protocol_version, VersionSource::MinecraftData),
        (None, None) => bail!(
            "{minecraft_version}: no protocol_version source available (no cached jar with \
             version.json, and no minecraft-data entry) — re-run with --fetch-missing or add a \
             minecraft-data entry rather than guessing"
        ),
    };

    let (data_version, data_version_source) = match (&jar_info, &data_entry) {
        (Some(jar), Some(data)) if jar.data_version != data.data_version => bail!(
            "{minecraft_version}: jar version.json world_version {} disagrees with minecraft-data \
             dataVersion {}",
            jar.data_version,
            data.data_version
        ),
        (Some(jar), _) => (jar.data_version, VersionSource::JarVersionJson),
        (None, Some(data)) => (data.data_version, VersionSource::MinecraftData),
        (None, None) => bail!(
            "{minecraft_version}: no data_version source available (no cached jar with \
             version.json, and no minecraft-data entry) — re-run with --fetch-missing or add a \
             minecraft-data entry rather than guessing"
        ),
    };

    Ok(VersionTableEntry {
        minecraft_version: minecraft_version.to_owned(),
        protocol_version,
        data_version,
        release_date,
        protocol_source,
        data_version_source,
        cross_checked: jar_info.is_some() && data_entry.is_some(),
    })
}

/// Resolves every row of the epic-343 version table, in `EPIC_343_VERSIONS`
/// order.
///
/// Fetches Mojang's version manifest once. With `fetch_missing`, also
/// downloads (and SHA-1-verifies, via [`fetch_version`]) any target
/// version's server jar not already cached under `.cache/mc/<version>/` —
/// this is the only network-heavy path and is off by default specifically so
/// routine `--check` runs do not silently pull a dozen jars.
pub fn version_table_report(
    workspace_root: &Path,
    fetch_missing: bool,
) -> Result<Vec<VersionTableEntry>> {
    let manifest_json = curl_to_string(VERSION_MANIFEST_URL)?;
    EPIC_343_VERSIONS
        .iter()
        .map(|minecraft_version| {
            resolve_version_table_entry(
                workspace_root,
                &manifest_json,
                minecraft_version,
                fetch_missing,
            )
        })
        .collect()
}

const VERSION_TABLE_OUT: &str = "crates/lodestone-registry/src/generated/version_table.rs";

fn version_table_out_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join(VERSION_TABLE_OUT)
}

/// Renders [`VersionTableEntry`] rows as the checked-in generated Rust
/// source, matching the `generated_*.rs` convention used throughout
/// `crates/versions/*/src/generated/`.
pub fn render_version_table_source(entries: &[VersionTableEntry]) -> Result<String> {
    let mut source = String::new();
    writeln!(
        source,
        "// @generated by `cargo run -p xtask -- version-table`. DO NOT EDIT BY HAND.\n\
         // Regenerate with `cargo run -p xtask -- version-table [--fetch-missing]` (see\n\
         // `crates/lodestone-registry/src/version_table.rs` module docs and\n\
         // `docs/version-table.md` for provenance and how to refresh).\n\
         //! Generated version table: the latest patch of every major Minecraft\n\
         //! release this workspace tracks, 1.7.10 through 26.2.\n\
         //! See [`crate::version_table`] for the public API and full provenance notes.\n"
    )?;
    writeln!(
        source,
        "/// Where one field of an [`Entry`] was sourced from."
    )?;
    writeln!(
        source,
        "#[derive(Clone, Copy, Debug, Eq, PartialEq)]\npub enum Source {{"
    )?;
    writeln!(
        source,
        "    /// `version.json` embedded in the vanilla server jar (present since 18w47b / 1.14)."
    )?;
    writeln!(source, "    JarVersionJson,")?;
    writeln!(
        source,
        "    /// `vendor/minecraft-data`'s `protocolVersions.json`; used only where the jar\n    /// predates `version.json`."
    )?;
    writeln!(source, "    MinecraftData,")?;
    writeln!(source, "}}\n")?;

    writeln!(source, "/// One row of the version table.")?;
    writeln!(source, "#[derive(Clone, Copy, Debug)]\npub struct Entry {{")?;
    writeln!(source, "    pub minecraft_version: &'static str,")?;
    writeln!(source, "    pub protocol_version: i32,")?;
    writeln!(source, "    pub data_version: i32,")?;
    writeln!(
        source,
        "    /// ISO-8601 `releaseTime` from Mojang's version_manifest_v2.json.\n    pub release_date: &'static str,"
    )?;
    writeln!(source, "    pub protocol_source: Source,")?;
    writeln!(source, "    pub data_version_source: Source,")?;
    writeln!(
        source,
        "    /// Whether jar and minecraft-data agreed (both present and equal). False means\n    /// only one source was available; the two never disagree in a committed row.\n    pub cross_checked: bool,"
    )?;
    writeln!(source, "}}\n")?;

    writeln!(
        source,
        "pub static VERSIONS: [Entry; {}] = [",
        entries.len()
    )?;
    for entry in entries {
        writeln!(
            source,
            "    Entry {{ minecraft_version: {:?}, protocol_version: {}, data_version: {}, release_date: {:?}, protocol_source: {}, data_version_source: {}, cross_checked: {} }},",
            entry.minecraft_version,
            entry.protocol_version,
            entry.data_version,
            entry.release_date,
            entry.protocol_source.as_rust_expr(),
            entry.data_version_source.as_rust_expr(),
            entry.cross_checked,
        )?;
    }
    writeln!(source, "];")?;

    format_rust_source(&source)
}

/// Regenerates `crates/lodestone-registry/src/generated/version_table.rs`
/// from the network + any cached jars, and writes it.
pub fn generate_version_table(workspace_root: &Path, fetch_missing: bool) -> Result<PathBuf> {
    let entries = version_table_report(workspace_root, fetch_missing)?;
    let source = render_version_table_source(&entries)?;
    let out_path = version_table_out_path(workspace_root);
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create output directory {}", parent.display()))?;
    }
    std::fs::write(&out_path, source)
        .with_context(|| format!("write generated version table to {}", out_path.display()))?;
    Ok(out_path)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VersionTableCheck {
    pub out_path: PathBuf,
    pub summary: String,
    identical: bool,
}

impl VersionTableCheck {
    #[must_use]
    pub const fn is_identical(&self) -> bool {
        self.identical
    }
}

/// Recomputes the version table and compares it against the checked-in file
/// without writing, for use as a drift-guard CI check.
pub fn check_version_table(workspace_root: &Path, fetch_missing: bool) -> Result<VersionTableCheck> {
    let entries = version_table_report(workspace_root, fetch_missing)?;
    let expected = render_version_table_source(&entries)?;
    let out_path = version_table_out_path(workspace_root);
    let actual = std::fs::read_to_string(&out_path)
        .with_context(|| format!("read generated version table at {}", out_path.display()))?;

    if actual == expected {
        return Ok(VersionTableCheck {
            out_path,
            summary: "version_table.rs is up to date".to_owned(),
            identical: true,
        });
    }

    Ok(VersionTableCheck {
        summary: packet_id_diff_summary(&out_path, &expected, &actual),
        out_path,
        identical: false,
    })
}

// ---------------------------------------------------------------------------
// docs-index: `docs/README.md` generated from every doc's own H1 + summary
// ---------------------------------------------------------------------------
//
// `docs/README.md` was the single most contended file in this repo -- every
// agent that lands a doc needs one index line in it, so it was hand-edited
// under a shared index and hand-edited files under a shared index are exactly
// what CLAUDE.md's repo-hazards section warns about (a stale staged blob of
// this file has, historically, deleted another agent's index bullet). The fix
// is to stop hand-editing it: `CLAUDE.md`'s own docs convention already
// requires every doc to open with a "what it is" summary, so the index is a
// pure function of the doc tree, not a thing anyone should ever type by hand
// again.
//
// A doc's summary is the first paragraph under a `## What it is` or
// `## What this is` heading (the two spellings actually used across this
// repo's 123 existing docs, checked before picking them -- 113 use one of the
// two, and no doc uses a spelling other than these two). Docs written before
// that heading convention existed fall back to the first paragraph directly
// under the H1. A doc with neither -- so `extract_doc_summary` cannot find
// any prose to quote -- fails loudly naming the file, per the acceptance
// criterion: no doc gets a blank index line.

/// `docs/*.md` files that are not part of the generated index at all --
/// companion docs `CLAUDE.md` and `docs/README.md`'s own prose already link
/// to directly, structurally different from a per-subsystem doc (an ordered
/// work queue, not the record of a landed feature). Kept as an explicit,
/// documented exception rather than an inferred one.
const DOCS_INDEX_SKIP: &[&str] = &["backlog.md"];

/// Top-level `docs/*.md` files that belong in the "Plans and research" group
/// (phased plans and read-only diagnoses) rather than the main per-subsystem
/// list, mirroring the hand-curated split the pre-generator `docs/README.md`
/// used. Everything under `docs/research/` joins them automatically.
const DOCS_INDEX_PLANS_AND_RESEARCH: &[&str] = &["worldgen-plan.md", "worldgen-parity.md"];

struct DocIndexEntry {
    /// Repo-relative link target, e.g. `./accounts.md` or `./roadmap/protocol.md`.
    link: String,
    title: String,
    summary: String,
}

/// True for a real ATX heading line (`#` through `######`, followed by a
/// space or end of line, per CommonMark) -- deliberately **not** just
/// `starts_with('#')`. A prose line beginning with an issue reference like
/// `#12/#72/#98/#121)` also starts with `#`, and treating that as a heading
/// silently truncated `docs/research/combat-scope.md`'s summary mid-sentence
/// the first time this ran -- caught by eye in the generated
/// `docs/README.md`, not by any test, which is why this got its own name
/// instead of staying an inline check.
fn is_atx_heading(trimmed_line: &str) -> bool {
    let hashes = trimmed_line.chars().take_while(|&c| c == '#').count();
    (1..=6).contains(&hashes) && matches!(trimmed_line.as_bytes().get(hashes), None | Some(b' '))
}

/// Extracts a doc's H1 title and a one-paragraph summary. See the module note
/// above for the extraction rule and why these two heading spellings.
fn extract_doc_summary(text: &str, rel_path: &str) -> Result<(String, String)> {
    let lines: Vec<&str> = text.lines().collect();

    let h1_idx = lines
        .iter()
        .position(|l| l.starts_with("# "))
        .ok_or_else(|| anyhow!("{rel_path}: no H1 (`# Title`) heading found"))?;
    let title = lines[h1_idx][2..].trim().to_string();
    if title.is_empty() {
        bail!("{rel_path}: H1 heading has no title text");
    }

    let is_summary_heading = |l: &str| {
        let t = l.trim();
        t.eq_ignore_ascii_case("## what it is") || t.eq_ignore_ascii_case("## what this is")
    };

    // Search the whole doc for the heading (not just immediately after the
    // H1): several docs carry a long preamble -- issue status, corrections --
    // before reaching it.
    let body_start = lines[h1_idx + 1..]
        .iter()
        .position(|l| is_summary_heading(l))
        .map(|i| h1_idx + 1 + i + 1);
    let scan_from = body_start.unwrap_or(h1_idx + 1);

    let mut para: Vec<&str> = Vec::new();
    for &line in &lines[scan_from..] {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if para.is_empty() {
                continue; // skip leading blank lines
            }
            break; // paragraph ended
        }
        if is_atx_heading(trimmed) {
            break; // hit a heading before (or right after) any prose
        }
        para.push(trimmed);
    }

    if para.is_empty() {
        bail!(
            "{rel_path}: no usable summary paragraph found (no prose under a \
             `## What it is`/`## What this is` heading, and none directly under the H1) -- \
             add one instead of leaving this doc out of the index"
        );
    }

    Ok((title, para.join(" ")))
}

/// Word-wraps one index bullet at a fixed width, matching the hand-authored
/// file's rough line length so the generated output stays readable as plain
/// text, not just as rendered markdown.
fn write_docs_index_entry(out: &mut String, entry: &DocIndexEntry) {
    const WIDTH: usize = 86;
    let mut line = format!("- [{}]({}) —", entry.title, entry.link);
    for word in entry.summary.split_whitespace() {
        if line.len() + 1 + word.len() > WIDTH {
            out.push_str(&line);
            out.push('\n');
            line = format!("  {word}");
        } else {
            line.push(' ');
            line.push_str(word);
        }
    }
    out.push_str(&line);
    out.push('\n');
}

/// [`read_md_dir_sorted`] for a directory that is allowed not to exist.
///
/// Only `NotFound` is tolerated; every other error still propagates. The
/// distinction matters: a directory that is absent has nothing to omit, whereas
/// a directory that fails to read for any other reason would be silently
/// skipped, which is the failure mode the `docs/plans/` comment below records —
/// the drift gate proves the index is *consistent* with the docs, never that it
/// *covers* them.
fn read_md_dir_sorted_optional(dir: &Path) -> Result<Vec<PathBuf>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    read_md_dir_sorted(dir)
}

fn read_md_dir_sorted(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("md"))
        .collect();
    files.sort();
    Ok(files)
}

/// Builds `docs/README.md`'s full contents from the doc tree. Deterministic:
/// a pure function of what is on disk under `docs/`, so two runs against the
/// same tree always produce byte-identical output -- the property the
/// drift-guard test below depends on.
pub fn generate_docs_index(workspace_root: &Path) -> Result<String> {
    let docs_dir = workspace_root.join("docs");

    let mut main: Vec<DocIndexEntry> = Vec::new();
    let mut plans: Vec<DocIndexEntry> = Vec::new();

    for path in read_md_dir_sorted(&docs_dir)? {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        if name == "README.md" || DOCS_INDEX_SKIP.contains(&name.as_str()) {
            continue;
        }
        let rel = format!("./{name}");
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let (title, summary) = extract_doc_summary(&text, &rel)?;
        let entry = DocIndexEntry { link: rel, title, summary };
        if DOCS_INDEX_PLANS_AND_RESEARCH.contains(&name.as_str()) {
            plans.push(entry);
        } else {
            main.push(entry);
        }
    }

    let mut roadmap: Vec<DocIndexEntry> = Vec::new();
    for path in read_md_dir_sorted(&docs_dir.join("roadmap"))? {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        let rel = format!("./roadmap/{name}");
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let (title, summary) = extract_doc_summary(&text, &rel)?;
        roadmap.push(DocIndexEntry { link: rel, title, summary });
    }

    // `docs/research/` was deleted wholesale during the documentation
    // reduction, so this scan is optional rather than required. It stays
    // because the group still exists and a research doc added later must be
    // indexed rather than silently dropped.
    for path in read_md_dir_sorted_optional(&docs_dir.join("research"))? {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        let rel = format!("./research/{name}");
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let (title, summary) = extract_doc_summary(&text, &rel)?;
        plans.push(DocIndexEntry { link: rel, title, summary });
    }

    // `docs/plans/` joins the same group, for the same reason `docs/research/`
    // does. It was NOT scanned until 2026-08-04, and the omission was silent in
    // the worst way: six plan documents, including the server-ECS migration
    // plan, landed invisible to the index, each one having
    // been written to satisfy the H1 + `## What it is` contract that only
    // matters *because* the generator reads it. Nothing failed -- the drift test
    // compares the generator against `docs/README.md`, and both agreed the
    // directory did not exist. A generated index cannot drift from the docs, but
    // it can silently omit a whole directory, which is a distinct failure mode
    // worth remembering: the gate proves consistency, not coverage.
    for path in read_md_dir_sorted(&docs_dir.join("plans"))? {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        let rel = format!("./plans/{name}");
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let (title, summary) = extract_doc_summary(&text, &rel)?;
        plans.push(DocIndexEntry { link: rel, title, summary });
    }

    let mut out = String::new();
    out.push_str("# Lodestone docs\n\n");
    out.push_str(
        "<!-- Generated by `cargo xtask docs-index` from every doc's own H1 and its \
`## What it is`/`## What this is` summary paragraph. Do not hand-edit: edit the doc\n\
     itself and regenerate (`cargo xtask docs-index`), or run `LODESTONE_REGEN=1 cargo\n\
     test -p xtask docs_index_matches_committed`. `cargo test -p xtask` fails loudly if\n\
     this file drifts from the generator's output. -->\n\n",
    );
    out.push_str(
        "Subsystem documentation. See also [`architecture.md`](./architecture.md)\n\
(the crate graph and the cross-cutting constraints) and\n\
[`meta/handoff.md`](./meta/handoff.md) (for an agent orchestrating this repo).\n\n",
    );

    for entry in &main {
        write_docs_index_entry(&mut out, entry);
    }

    out.push_str("\n---\n\n## Roadmap\n\n");
    out.push_str(
        "Per-track roadmap documents for the plan to 1:1 parity (epic decompositions, one per\n\
track) -- see the first entry below for how the whole set is organised and what invariants\n\
every issue under it inherits.\n\n",
    );
    for entry in &roadmap {
        write_docs_index_entry(&mut out, entry);
    }

    out.push_str("\n---\n\n## Plans and research\n\n");
    out.push_str(
        "Longer-form artifacts that are not per-subsystem docs: phased plans (everything under\n\
`docs/plans/`, written to be dispatchable before the work starts), and read-only\n\
diagnoses produced before the corresponding fix was written. They live here because a\n\
diagnosis is worth keeping *after* the fix lands -- CLAUDE.md's standing claim is that the\n\
record of confidently-held false beliefs is the most valuable thing in this repo, and several\n\
of these caught the *brief* being wrong rather than the code.\n\n",
    );
    for entry in &plans {
        write_docs_index_entry(&mut out, entry);
    }

    Ok(out)
}

fn docs_index_out_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join("docs/README.md")
}

/// Writes the generated index to `docs/README.md`.
pub fn write_docs_index(workspace_root: &Path) -> Result<PathBuf> {
    let generated = generate_docs_index(workspace_root)?;
    let out_path = docs_index_out_path(workspace_root);
    std::fs::write(&out_path, generated)
        .with_context(|| format!("write generated docs index to {}", out_path.display()))?;
    Ok(out_path)
}

/// The result of a docs-index drift check, shaped like
/// [`VersionTableCheck`] (same idea, different generated file) but kept as
/// its own type rather than reused -- `VersionTableCheck` naming a
/// `docs/README.md` result would be its own small staleness trap.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocsIndexCheck {
    pub out_path: PathBuf,
    pub summary: String,
    identical: bool,
}

impl DocsIndexCheck {
    #[must_use]
    pub const fn is_identical(&self) -> bool {
        self.identical
    }
}

/// Recomputes the docs index and compares it against the checked-in file
/// without writing, for use as a drift-guard check (mirrors
/// [`check_version_table`]'s shape).
pub fn check_docs_index(workspace_root: &Path) -> Result<DocsIndexCheck> {
    let expected = generate_docs_index(workspace_root)?;
    let out_path = docs_index_out_path(workspace_root);
    let actual = std::fs::read_to_string(&out_path)
        .with_context(|| format!("read {}", out_path.display()))?;

    if actual == expected {
        return Ok(DocsIndexCheck {
            out_path,
            summary: "docs/README.md is up to date".to_owned(),
            identical: true,
        });
    }

    Ok(DocsIndexCheck {
        summary: packet_id_diff_summary(&out_path, &expected, &actual),
        out_path,
        identical: false,
    })
}

// ---------------------------------------------------------------------------
// bench-compare: ratio-against-a-stored-baseline tool
// ---------------------------------------------------------------------------
//
// `docs/roadmap/benchmarks.md` already states the policy this tool
// implements (ratio against a same-machine baseline, ±25% tolerance band,
// never a CI-blocking gate); this command is the one piece that policy left
// still missing: "a small comparison script/tool ... that reads two
// bench-results/*.jsonl records and reports a ratio + verdict against a
// stated tolerance." `benches/support.rs`'s `record()` already does this
// automatically for "this run vs the immediately preceding run" every time a
// bench executes; this command is the standalone form that needs no bench
// re-run at all -- point it at an existing `bench-results/<bench>.jsonl` and
// ask it to compare two *specific* recorded commits (e.g. "before my change"
// vs "after my change") without regenerating anything.
//
// Per CLAUDE.md's evidence standard, this never asserts a verdict like
// "regression" -- a metric can be either direction-is-better depending on
// its unit (lower is better for a `_ms` timing, higher is better for a
// `_throughput` count), and this schema does not carry that annotation. It
// reports the ratio and whether it falls inside the tolerance band, and lets
// the caller -- who knows what the metric means -- read the direction.

/// One recorded line from a `bench-results/<bench>.jsonl` file, matching
/// `benches/support.rs`'s `record()` schema exactly (kept as its own
/// deserialization target, independent of that file, since `xtask` cannot
/// depend on any one crate's `benches/support.rs` — it is intentionally
/// duplicated per crate, not a shared module).
#[derive(Clone, Debug)]
pub struct BenchRecord {
    pub timestamp: u64,
    pub git_sha: String,
    pub machine: String,
    pub profile: String,
    pub scene: String,
    pub metric: String,
    pub value: f64,
    pub unit: String,
}

/// Parses every line of a `bench-results/*.jsonl` file into [`BenchRecord`]s,
/// in file order (which is chronological -- the format is append-only).
/// Malformed lines are skipped with a `None` filtered out rather than
/// failing the whole read, matching `support.rs`'s own tolerant parsing (a
/// hand-edited or partially-written line should not make every other
/// recorded run unreadable).
pub fn read_bench_records(path: &Path) -> Result<Vec<BenchRecord>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let records = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|v| {
            Some(BenchRecord {
                timestamp: v.get("timestamp")?.as_u64()?,
                git_sha: v.get("git_sha")?.as_str()?.to_string(),
                machine: v.get("machine")?.as_str()?.to_string(),
                profile: v.get("profile")?.as_str()?.to_string(),
                scene: v.get("scene")?.as_str()?.to_string(),
                metric: v.get("metric")?.as_str()?.to_string(),
                value: v.get("value")?.as_f64()?,
                unit: v.get("unit")?.as_str()?.to_string(),
            })
        })
        .collect();
    Ok(records)
}

/// Inputs to [`compare_bench_records`].
#[derive(Clone, Debug)]
pub struct BenchCompareOptions {
    pub metric: String,
    pub scene: String,
    /// Git-sha prefix to select the candidate ("after") run. `None` means
    /// "the most recent recorded run matching `metric`/`scene`".
    pub candidate_sha: Option<String>,
    /// Git-sha prefix to select the baseline ("before") run. `None` means
    /// "the run immediately preceding the candidate, on the same machine and
    /// build profile" -- the same pairing `support.rs::record` compares
    /// against automatically.
    pub baseline_sha: Option<String>,
    /// Tolerance band as a fraction (e.g. `0.25` for ±25%), matching
    /// `docs/roadmap/benchmarks.md`'s stated policy and `support.rs`'s own
    /// literal.
    pub tolerance: f64,
}

/// The result of comparing two [`BenchRecord`]s.
#[derive(Clone, Debug)]
pub struct BenchCompareReport {
    pub baseline: BenchRecord,
    pub candidate: BenchRecord,
    pub ratio: f64,
    pub tolerance: f64,
}

impl BenchCompareReport {
    #[must_use]
    pub fn within_tolerance(&self) -> bool {
        (1.0 - self.tolerance..=1.0 + self.tolerance).contains(&self.ratio)
    }

    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "metric={} scene={:?}",
            self.candidate.metric, self.candidate.scene
        );
        let _ = writeln!(
            out,
            "  baseline  {:>12.4}{} @ {} ({}, {})",
            self.baseline.value, self.baseline.unit, self.baseline.git_sha, self.baseline.machine, self.baseline.profile
        );
        let _ = writeln!(
            out,
            "  candidate {:>12.4}{} @ {} ({}, {})",
            self.candidate.value, self.candidate.unit, self.candidate.git_sha, self.candidate.machine, self.candidate.profile
        );
        let band_pct = self.tolerance * 100.0;
        if self.within_tolerance() {
            let _ = writeln!(
                out,
                "  ratio {:.3} -- within +/-{band_pct:.2}% band -> OK",
                self.ratio
            );
        } else {
            let _ = writeln!(
                out,
                "  ratio {:.3} -- OUTSIDE +/-{band_pct:.2}% band -> FLAGGED (direction depends on whether \
                 {:?} is lower- or higher-is-better; this tool does not know)",
                self.ratio, self.candidate.metric
            );
        }
        out
    }
}

/// Finds the baseline/candidate pair `opts` describes among `records`
/// (already filtered to one `metric`/`scene`... no -- takes the *unfiltered*
/// list and does the metric/scene filtering itself, so callers just hand it
/// [`read_bench_records`]'s output) and reports their ratio.
pub fn compare_bench_records(records: &[BenchRecord], opts: &BenchCompareOptions) -> Result<BenchCompareReport> {
    let filtered: Vec<&BenchRecord> = records
        .iter()
        .filter(|r| r.metric == opts.metric && r.scene == opts.scene)
        .collect();
    if filtered.is_empty() {
        bail!(
            "no records match metric {:?} scene {:?}",
            opts.metric,
            opts.scene
        );
    }

    let candidate_index = match &opts.candidate_sha {
        Some(prefix) => filtered
            .iter()
            .rposition(|r| r.git_sha.starts_with(prefix.as_str()))
            .ok_or_else(|| anyhow!("no record matching metric/scene has git_sha prefix {prefix:?} (candidate)"))?,
        None => filtered.len() - 1,
    };
    let candidate = filtered[candidate_index];

    let baseline_index = match &opts.baseline_sha {
        Some(prefix) => filtered[..candidate_index]
            .iter()
            .rposition(|r| r.git_sha.starts_with(prefix.as_str()))
            .ok_or_else(|| {
                anyhow!("no record before the candidate has git_sha prefix {prefix:?} (baseline)")
            })?,
        None => filtered[..candidate_index]
            .iter()
            .rposition(|r| r.machine == candidate.machine && r.profile == candidate.profile)
            .ok_or_else(|| {
                anyhow!(
                    "no prior record on machine {:?} profile {:?} to use as an implicit baseline -- \
                     pass --baseline <sha>",
                    candidate.machine,
                    candidate.profile
                )
            })?,
    };
    let baseline = filtered[baseline_index];

    if baseline.machine != candidate.machine || baseline.profile != candidate.profile {
        bail!(
            "baseline ({}, {}) and candidate ({}, {}) are not the same machine/profile -- \
             not a valid comparison per the evidence standard (a number is not comparable across machines)",
            baseline.machine,
            baseline.profile,
            candidate.machine,
            candidate.profile
        );
    }

    Ok(BenchCompareReport {
        baseline: baseline.clone(),
        candidate: candidate.clone(),
        ratio: candidate.value / baseline.value,
        tolerance: opts.tolerance,
    })
}

fn file_sha1_hex(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = sha1::Sha1::new();
    let mut buffer = [0_u8; 16 * 1024];

    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("read {}", path.display()))?;
        if read == 0 {
            break;
        }
        sha1::Digest::update(&mut hasher, &buffer[..read]);
    }

    Ok(bytes_to_lower_hex(&sha1::Digest::finalize(hasher)))
}

fn bytes_to_lower_hex(bytes: &[u8]) -> String {
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

fn download_verified_file(
    url: &str,
    destination: &Path,
    expected_sha1: &str,
    force: bool,
) -> Result<bool> {
    if download_decision(destination, expected_sha1, force)? == DownloadDecision::SkipValid {
        return Ok(false);
    }

    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create download directory {}", parent.display()))?;
    }
    let partial = destination.with_extension("download");
    if partial.exists() {
        std::fs::remove_file(&partial)
            .with_context(|| format!("remove stale partial download {}", partial.display()))?;
    }

    if let Err(error) = curl_to_file(url, &partial) {
        let _ = std::fs::remove_file(&partial);
        return Err(error);
    }

    if let Err(error) = verify_sha1(&partial, expected_sha1) {
        let _ = std::fs::remove_file(&partial);
        return Err(error);
    }

    std::fs::rename(&partial, destination).with_context(|| {
        format!(
            "rename verified download {} to {}",
            partial.display(),
            destination.display()
        )
    })?;
    Ok(true)
}

fn curl_to_string(url: &str) -> Result<String> {
    let output = Command::new("curl")
        .arg("--fail")
        .arg("--location")
        .arg("--silent")
        .arg("--show-error")
        .arg(url)
        .output()
        .with_context(|| format!("run curl for {url}"))?;
    if !output.status.success() {
        bail!(
            "curl failed for {url}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    String::from_utf8(output.stdout).with_context(|| format!("curl output for {url} was not UTF-8"))
}

fn curl_to_file(url: &str, destination: &Path) -> Result<()> {
    let output = Command::new("curl")
        .arg("--fail")
        .arg("--location")
        .arg("--silent")
        .arg("--show-error")
        .arg("--output")
        .arg(destination)
        .arg(url)
        .output()
        .with_context(|| format!("run curl for {url}"))?;
    if !output.status.success() {
        bail!(
            "curl failed for {url}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

pub fn count_client_jar_assets(path: &Path) -> Result<JarAssetCounts> {
    let file = File::open(path).with_context(|| format!("open client jar {}", path.display()))?;
    let archive =
        zip::ZipArchive::new(file).with_context(|| format!("read zip {}", path.display()))?;
    let mut counts = JarAssetCounts {
        block_textures: 0,
        block_models: 0,
        blockstates: 0,
    };

    for name in archive.file_names() {
        if name.starts_with("assets/minecraft/textures/block/") && !name.ends_with('/') {
            counts.block_textures += 1;
        } else if name.starts_with("assets/minecraft/models/block/") && !name.ends_with('/') {
            counts.block_models += 1;
        } else if name.starts_with("assets/minecraft/blockstates/") && !name.ends_with('/') {
            counts.blockstates += 1;
        }
    }

    Ok(counts)
}

fn sorted_object_entries(object: &serde_json::Map<String, Value>) -> BTreeMap<&String, &Value> {
    object.iter().collect()
}

fn ensure_unique_generated_identifiers(report: &PacketReport) -> Result<()> {
    for state in PacketState::ALL {
        for bound in PacketBound::ALL {
            let mut seen = BTreeSet::new();
            for entry in report.entries(state, bound) {
                if !seen.insert(entry.const_ident.as_str()) {
                    bail!(
                        "duplicate generated identifier {} in {:?}/{:?}",
                        entry.const_ident,
                        state,
                        bound
                    );
                }
            }
        }
    }
    Ok(())
}

fn resolve_output_path(
    workspace_root: &Path,
    out: Option<&Path>,
    default: &str,
) -> Result<PathBuf> {
    let requested = out.unwrap_or_else(|| Path::new(default));
    let relative = if requested.is_absolute() {
        requested.strip_prefix(workspace_root).with_context(|| {
            format!(
                "output path {} must be inside workspace {}",
                requested.display(),
                workspace_root.display()
            )
        })?
    } else {
        requested
    };

    validate_relative_child_path(relative)?;
    if !path_is_generated_packet_ids(relative) {
        bail!(
            "refusing to write outside crates/versions/*/src/generated; requested {}",
            requested.display()
        );
    }

    Ok(workspace_root.join(relative))
}

/// Returns whether `relative` names `crates/versions/<crate>/src/generated/<file>`.
///
/// This keeps generated packet id tables confined to a version crate's
/// `generated` directory regardless of which version crate is targeted.
fn path_is_generated_packet_ids(relative: &Path) -> bool {
    let components: Vec<&std::ffi::OsStr> = relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part),
            _ => None,
        })
        .collect();

    matches!(
        components.as_slice(),
        [crates, protocol, _crate_name, src, generated, _file]
            if *crates == "crates"
                && *protocol == "versions"
                && *src == "src"
                && *generated == "generated"
    )
}

fn validate_relative_child_path(path: &Path) -> Result<()> {
    for component in path.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!(
                    "output path must be a normal child path: {}",
                    path.display()
                );
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// wasm-check — wasm32 compile + confinement-guard tripwire.
//
// Tested port of scripts/wasm-check.sh. The script is a shell pipeline with no
// gate (it relies on manual review of its output); this command runs the same
// three phases — compile the wasm crate subset, grep-based CONFINEMENT guards,
// trunk build of web/ — but reports every failure through Result so a leak is a
// non-zero exit rather than something a `| grep | tail` can swallow. The
// scanners are unit-tested below; the shell original has none.
//
// Read scripts/wasm-check.sh's header for the WHY this exists: "compiles to
// wasm" and "works on wasm" are different, and std::fs / Instant::now /
// std::thread::spawn / tokio::time all COMPILE for wasm32 and only die at
// runtime. The compile pass is structurally blind to them; the confinement
// guards are the tripwire that actually catches a leaked hazard.
// ---------------------------------------------------------------------------

/// Target triple the wasm crate subset is compiled for.
pub const WASM_TARGET: &str = "wasm32-unknown-unknown";

/// One crate in the wasm compile subset, plus any extra `cargo build` args the
/// browser configuration requires (the script's `"pkg|extra"` rows).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WasmCrate {
    pub name: &'static str,
    pub extra_args: &'static [&'static str],
}

/// The wasm compile subset, in build order — parity with scripts/wasm-check.sh.
/// The two non-obvious rows' whys are kept from the script: lodestone-net needs
/// the `ws-web` feature for browser websockets; every other crate builds
/// default.
pub fn wasm_crates() -> Vec<WasmCrate> {
    vec![
        // The portable clock seam: nearly every crate below depends on this
        // one, so a regression here would otherwise surface only
        // transitively, attributed to whichever dependent happened to fail
        // first. Listed first for the same reason lodestone-data is listed
        // separately from v770.
        WasmCrate {
            name: "lodestone-time",
            extra_args: &[],
        },
        WasmCrate {
            name: "lodestone-core",
            extra_args: &[],
        },
        WasmCrate {
            name: "lodestone-model",
            extra_args: &[],
        },
        WasmCrate {
            name: "lodestone-world",
            extra_args: &[],
        },
        WasmCrate {
            name: "lodestone-physics",
            extra_args: &[],
        },
        WasmCrate {
            name: "lodestone-assets",
            extra_args: &[],
        },
        WasmCrate {
            name: "lodestone-registry",
            extra_args: &[],
        },
        WasmCrate {
            name: "lodestone-render",
            extra_args: &[],
        },
        WasmCrate {
            name: "lodestone-audio",
            extra_args: &[],
        },
        // Event→sound bridge. Its default build is device-free and version-free
        // (lodestone-audio, lodestone-assets, lodestone-model, glam, thiserror
        // — all wasm-safe); the live gate's client/tokio/registry deps are
        // gated behind the off-by-default `live-v770` feature.
        WasmCrate {
            name: "lodestone-sound",
            extra_args: &[],
        },
        // Canonical 26.2 game-data censuses; depends on nothing
        // but lodestone-model, listed separately so a regression is
        // unambiguous rather than only surfacing via v770.
        WasmCrate {
            name: "lodestone-data",
            extra_args: &[],
        },
        WasmCrate {
            name: "lodestone-v26-2",
            extra_args: &[],
        },
        WasmCrate {
            name: "lodestone-v1-8",
            extra_args: &[],
        },
        WasmCrate {
            name: "lodestone-net",
            extra_args: &["--features", "ws-web"],
        },
        // bevy_ecs must be wasm32-clean or the bevy migration stops here.
        WasmCrate {
            name: "lodestone-ecs",
            extra_args: &[],
        },
        WasmCrate {
            name: "lodestone-client",
            extra_args: &[],
        },
        WasmCrate {
            name: "lodestone-controller",
            extra_args: &[],
        },
        // Integrated server runs in the browser under the `spawn_local` seam;
        // browser singleplayer depends on it.
        WasmCrate {
            name: "lodestone-server",
            extra_args: &[],
        },
        WasmCrate {
            name: "lodestone-worldgen",
            extra_args: &[],
        },
        // The playable game shell — the menu, `Sim`, the renderer, all of it. The
        // browser consumes this crate's LIB target, and it is the crate most likely
        // to regress because almost nobody working in it builds for wasm. Placed
        // last because it sits on top of everything above, so a failure here is
        // unambiguous rather than transitive.
        //
        // It was missing from this table while the reference script listed it, so
        // `cargo xtask wasm-check` — the implementation CI runs — never named it and
        // only reached it transitively through the trunk build of `web/`.
        WasmCrate {
            name: "lodestone-shell",
            extra_args: &[],
        },
    ]
}

/// One confinement guard rule — parity with scripts/wasm-check.sh's
/// CONFINEMENT_RULES table, asserted by a test that PARSES that table rather than
/// restating it.
///
/// `banned` is matched as a literal substring, and that is a requirement rather
/// than an observation: the same string is handed to grep by the script, so a
/// regex metacharacter would mean two different things on the two sides.
/// `confinement_rules_match_the_reference_script_table` enforces it. A `|` is the
/// worst case — it is the script's own field separator, and a `\|` alternation is
/// what made five rules print PASS with their grep never executing. Split a
/// two-hazard rule into one rule per hazard instead.
#[derive(Debug, Clone)]
pub struct ConfinementRule {
    /// Report label, e.g. "lodestone-assets fs-confinement".
    pub label: &'static str,
    /// Directory under the workspace root to scan, e.g. "crates/lodestone-assets/src".
    pub src_dir: &'static str,
    /// Banned symbol, matched as a literal substring.
    pub banned: &'static str,
    /// File basenames allowed to contain the banned symbol — the
    /// cfg(not(target_arch = "wasm32"))-gated files that confine the hazard.
    pub allowlist: &'static [&'static str],
}

/// The confinement rules in effect — parity with scripts/wasm-check.sh. Add a
/// row only after the crate actually confines the hazard to an allowlisted,
/// cfg(not(wasm32))-gated file; a rule for ungated code goes red for everyone.
pub fn confinement_rules() -> Vec<ConfinementRule> {
    vec![
        // lodestone-audio has NO time source at all (sample-driven clock), so
        // Instant::now() is banned across the whole crate with an empty
        // allowlist — "audio never touches wall-clock time" is a checked
        // invariant, not a promise.
        ConfinementRule {
            label: "lodestone-assets fs-confinement",
            src_dir: "crates/lodestone-assets/src",
            banned: "std::fs::",
            allowlist: &["source_native.rs"],
        },
        ConfinementRule {
            label: "lodestone-audio device-confinement",
            src_dir: "crates/lodestone-audio/src",
            banned: "cpal::",
            allowlist: &["sink.rs"],
        },
        ConfinementRule {
            label: "lodestone-audio time-confinement",
            src_dir: "crates/lodestone-audio/src",
            banned: "Instant::now(",
            allowlist: &[],
        },
        ConfinementRule {
            label: "lodestone-sound time-confinement",
            src_dir: "crates/lodestone-sound/src",
            banned: "Instant::now(",
            allowlist: &[],
        },
        // lodestone-client confines tokio::time to native_time.rs and bans the
        // whole Instant/std::fs/std::thread family across the crate (the driver
        // is event-driven and never reads a wall clock); tokio::spawn is
        // confined to the spawn.rs seam, whose wasm arm uses
        // wasm_bindgen_futures::spawn_local.
        ConfinementRule {
            label: "lodestone-client time-confinement",
            src_dir: "crates/lodestone-client/src",
            banned: "tokio::time::",
            allowlist: &["native_time.rs"],
        },
        ConfinementRule {
            label: "lodestone-client instant-ban",
            src_dir: "crates/lodestone-client/src",
            banned: "Instant::now(",
            allowlist: &[],
        },
        ConfinementRule {
            label: "lodestone-client fs-ban",
            src_dir: "crates/lodestone-client/src",
            banned: "std::fs::",
            allowlist: &[],
        },
        ConfinementRule {
            label: "lodestone-client thread-ban",
            src_dir: "crates/lodestone-client/src",
            banned: "std::thread",
            allowlist: &[],
        },
        ConfinementRule {
            label: "lodestone-client spawn-confinement",
            src_dir: "crates/lodestone-client/src",
            banned: "tokio::spawn",
            allowlist: &["spawn.rs"],
        },
        // --- lodestone-shell ---
        // The shell confines both trapping clocks to `crate::platform`, which
        // re-exports `web_time` (std's own types on native, `performance.now()` /
        // `Date.now()` in a browser). These rules ban the `std::time::` PATHS rather
        // than the bare `Instant::now(` spelling, deliberately: the shell's call
        // sites read `crate::platform::Instant::now()`, so an `Instant::now(`
        // pattern would match all of them and the rule could never go green. The
        // path is what distinguishes a trapping call from a portable one.
        //
        // `platform.rs` alone is allowlisted — the strongest form. Both rules found
        // LIVE TRAPS when first added, on a tree whose wasm32 build was already
        // exit 0, which is the entire argument for having them.
        ConfinementRule {
            label: "lodestone-shell instant-confinement",
            src_dir: "crates/lodestone-shell/src",
            banned: "std::time::Instant",
            allowlist: &["platform.rs"],
        },
        ConfinementRule {
            label: "lodestone-shell systemtime-confinement",
            src_dir: "crates/lodestone-shell/src",
            banned: "std::time::SystemTime::now",
            allowlist: &["platform.rs"],
        },
        // `thread::spawn` TRAPS on wasm32; the three thread entry points do NOT
        // behave alike, which is why this names one of them and not the family:
        //
        //     std::thread::spawn                 TRAPS
        //     std::thread::sleep                 TRAPS
        //     std::thread::Builder::new().spawn  Err(Unsupported) — degrades
        //     std::thread::available_parallelism Err              — degrades
        //
        // Allowlist = the files that confine it behind
        // cfg(not(target_arch = "wasm32")) with a browser arm beside it. SCOPE
        // LIMIT: this does NOT cover `thread::sleep`, whose remaining sites are
        // inside `#[cfg(test)] mod tests` in files whose production halves must stay
        // covered — a scanner cannot tell a test module from a production one, so
        // allowlisting those files would buy one hazard and blind two files to it.
        //
        // `app/runners.rs` joined this list for `run_headless_session`:
        // its stdin control thread is
        // `cfg(all(not(target_arch = "wasm32"), feature = "runtime-presentation"))`
        // — the whole function is native-only, like `run_connect`/`run_headless`
        // right beside it in the same file, and has no browser arm because
        // `Mode::HeadlessSession` itself is refused on wasm32 (`app.rs`'s `run`).
        // `terminal.rs` is declared only by `lib.rs`'s native target arm. Its
        // blocking stdin reader has no browser caller or compiled browser code.
        ConfinementRule {
            label: "lodestone-shell thread-spawn-confinement",
            src_dir: "crates/lodestone-shell/src",
            banned: "thread::spawn",
            allowlist: &["mesher.rs", "accounts.rs", "status.rs", "runners.rs", "terminal.rs"],
        },
        // --- the clock, in every other crate the browser reaches ---
        //
        // These exist because lodestone-shell's three rules were not enough. The
        // browser build reached exit 0 with all three PASSing and still died twice:
        // once in lodestone-particle (`from_entropy` → `SystemTime::now()`, three
        // crates below the shell) and once in lodestone-server/lodestone-worldgen on
        // the way into a world. A confinement guard only covers the crate it names,
        // and the browser reaches about fifteen.
        //
        // lodestone-server is the sharpest case: `collect_nearby_items` already
        // carried a comment stating the rule — "this crate must not call
        // std::time::Instant::now() anywhere in lodestone-server, because the crate
        // links into a wasm32 bundle where that compiles and then panics at runtime"
        // — and four sites violated it anyway. The rule was right and it was prose.
        //
        // ONE RULE PER HAZARD, not one `(Instant|SystemTime)` rule per crate. In the
        // reference script the alternation form spelled `\|`, which IS that table's
        // field separator, so all five of those rules had their pattern truncated,
        // grep exited 2, and a swallowed error printed PASS. Keeping every pattern a
        // literal substring is what lets both implementations share one table.
        //
        // Empty allowlists: these crates have no business reading a wall clock
        // through `std`. Each uses `web_time`, whose non-wasm arm is
        // `pub use std::time::*`, so native is byte-identical.
        ConfinementRule {
            label: "lodestone-server instant-ban",
            src_dir: "crates/lodestone-server/src",
            banned: "std::time::Instant",
            allowlist: &[],
        },
        ConfinementRule {
            label: "lodestone-server systemtime-ban",
            src_dir: "crates/lodestone-server/src",
            banned: "std::time::SystemTime",
            allowlist: &[],
        },
        // `tokio::time::Instant::now()` is a different literal than
        // `std::time::Instant`, so the rule above cannot see it, and it traps
        // identically — `server.rs`'s own `JoinStopwatch` doc says so ("it
        // bottoms out in std::time::Instant::now() ... and panics identically"),
        // which did not stop `serve_play`'s keep-alive/time-sync/vitals/
        // container-sync interval setup from shipping six unguarded calls anyway.
        // Measured live in the browser build: joining a singleplayer world
        // panics at `library/std/src/sys/time/unsupported.rs:13:9` the instant
        // the client reaches Play, and "Joining world..." spins forever because
        // the connection task that died was the one about to send the rest of
        // the view. `tick.rs` also names this symbol, but only inside
        // `run_tick_loop`, which wasm32's `open_in_memory` deliberately never
        // spawns (see `net.rs`'s own comment on that constructor) — a real,
        // documented gap rather than a live trap, so it is allowlisted rather
        // than making this rule impossible to turn green.
        ConfinementRule {
            label: "lodestone-server tokio-instant-ban",
            src_dir: "crates/lodestone-server/src",
            banned: "tokio::time::Instant",
            allowlist: &["tick.rs"],
        },
        // This crate's clock now goes through `lodestone_time::` rather than a
        // direct `web_time::` dependency — `Cargo.toml` no longer lists
        // `web-time` at all, so a reintroduced bare `web_time::` call would not
        // even compile. That is not a reason to skip a rule for it: the whole
        // point of a confinement guard, per the two rules above, is to catch a
        // regression by name before anyone waits on a build to find it. Empty
        // allowlist: every legitimate call site in this crate (including
        // `browser_timer.rs`, migrated alongside the rest — its `BrowserInstant`
        // alias is `lodestone_time::Instant`, the identical type on every
        // target) reads `lodestone_time::`, which this qualified `web_time::`
        // pattern does not match.
        ConfinementRule {
            label: "lodestone-server web-time-ban",
            src_dir: "crates/lodestone-server/src",
            banned: "web_time::",
            allowlist: &[],
        },
        // Rayon is the native batch dispatcher only. The browser path uses the
        // same source algorithm through its yielding serial loop, so a new
        // Rayon call outside the target-confined chunk seam is a wasm trap risk.
        ConfinementRule {
            label: "lodestone-server rayon-confinement",
            src_dir: "crates/lodestone-server/src",
            banned: "rayon::",
            allowlist: &["chunk.rs", "worldgen_dispatch.rs"],
        },
        ConfinementRule {
            label: "lodestone-worldgen instant-ban",
            src_dir: "crates/lodestone-worldgen/src",
            banned: "std::time::Instant",
            allowlist: &[],
        },
        ConfinementRule {
            label: "lodestone-worldgen systemtime-ban",
            src_dir: "crates/lodestone-worldgen/src",
            banned: "std::time::SystemTime",
            allowlist: &[],
        },
        ConfinementRule {
            label: "lodestone-particle instant-ban",
            src_dir: "crates/lodestone-particle/src",
            banned: "std::time::Instant",
            allowlist: &[],
        },
        ConfinementRule {
            label: "lodestone-particle systemtime-ban",
            src_dir: "crates/lodestone-particle/src",
            banned: "std::time::SystemTime",
            allowlist: &[],
        },
        ConfinementRule {
            label: "lodestone-net instant-ban",
            src_dir: "crates/lodestone-net/src",
            banned: "std::time::Instant",
            allowlist: &[],
        },
        ConfinementRule {
            label: "lodestone-net systemtime-ban",
            src_dir: "crates/lodestone-net/src",
            banned: "std::time::SystemTime",
            allowlist: &[],
        },
        // `async_task.rs`'s only clock hits are inside a `#[cfg(test)] mod`, which
        // never reaches a browser; a scanner cannot tell a test module from a
        // production one, so it is named.
        ConfinementRule {
            label: "lodestone-ecs instant-ban",
            src_dir: "crates/lodestone-ecs/src",
            banned: "std::time::Instant",
            allowlist: &["async_task.rs"],
        },
        ConfinementRule {
            label: "lodestone-ecs systemtime-ban",
            src_dir: "crates/lodestone-ecs/src",
            banned: "std::time::SystemTime",
            allowlist: &["async_task.rs"],
        },
        // `lodestone-auth` joined this qualified-pattern bucket once `flow.rs`
        // (which now compiles and runs on wasm32 — see that module's doc)
        // took a `lodestone-time` dependency for a real wall-clock deadline
        // (`PendingLogin::is_expired`). That makes this crate's own
        // `lodestone-auth systemtime-ban` rule below (still bare-pattern) an
        // exception rather than the rule for this crate now: `lodestone-time`
        // re-exports `Instant` but not `SystemTime` (see `lodestone-time`'s own
        // `src/lib.rs`), so there is no legitimate qualified
        // `lodestone_time::SystemTime::now()` spelling to protect — only
        // `Instant` needed to move buckets. An empty allowlist, matching
        // `lodestone-server`/`-worldgen`/`-particle`/`-net` above: neither
        // `browser_login.rs` nor `migrate.rs` (both still native-only, both
        // still allowlisted on the *systemtime* rule below) spells the type
        // out fully qualified — both reach it through a bare `use
        // std::time::{..., Instant}` import, which this qualified substring
        // does not match at all, so there is nothing here to allowlist.
        ConfinementRule {
            label: "lodestone-auth instant-ban",
            src_dir: "crates/lodestone-auth/src",
            banned: "std::time::Instant",
            allowlist: &[],
        },
        // --- crates outside the wasm build, tightened toward "no crate but
        // lodestone-time may name std::time's clocks" ---
        //
        // None of these three crates appears in wasm_crates() above: testsupport
        // is mostly dev-dependency-only, while its optional normal edges from
        // sound/fuzz are off by default; its new bench-record feature is
        // additionally native-only. The other two are a native-only bin nothing
        // depends on (lodestone-allocbench, already excluded from the
        // workspace-wide --all-features sweep for its allocator mutual-exclusion),
        // or reaches wasm only via a #[cfg(test)] module that never enters a
        // --lib build either way (lodestone-world's
        // fill_region_lock_hold_time_on_a_large_synthetic_fill test).
        //
        // A rule here still earns its keep: it turns "this file structurally
        // cannot reach wasm" from a claim into something re-checked on every run,
        // and it is what stops a NEW file in one of these crates from growing an
        // ungated clock call unnoticed.
        //
        // PATTERN CHOICE: none of these three crates depends on lodestone-time, so
        // there is no legitimate lodestone_time::Instant::now() call anywhere in
        // them to avoid catching — unlike lodestone-server/worldgen/particle/net/
        // ecs/auth above, which must use the qualified std::time:: path
        // specifically so a legitimate lodestone_time::Instant::now() elsewhere in
        // the same crate does not false-positive. These three instead use the bare
        // Instant::now(/SystemTime::now( method-call spelling (as
        // lodestone-audio/lodestone-sound do, for the same "no legitimate caller
        // exists" reason) because their actual call sites mix qualified and
        // unqualified spellings and the bare form catches both.
        //
        // `lodestone-auth systemtime-ban` stays here (bare pattern) rather than
        // moving with `lodestone-auth instant-ban` above: `lodestone-time` has no
        // `SystemTime` re-export at all (see its own `src/lib.rs`), so unlike
        // `Instant` there is still no legitimate qualified
        // `lodestone_time::SystemTime::now()` spelling in this crate for a
        // qualified pattern to protect — the bare form remains the tightest
        // correct rule for this one hazard in this one crate.
        ConfinementRule {
            label: "lodestone-auth systemtime-ban",
            src_dir: "crates/lodestone-auth/src",
            banned: "SystemTime::now(",
            allowlist: &["browser_login.rs", "migrate.rs"],
        },
        ConfinementRule {
            label: "lodestone-world instant-ban",
            src_dir: "crates/lodestone-world/src",
            banned: "Instant::now(",
            allowlist: &["world.rs"],
        },
        ConfinementRule {
            label: "lodestone-world systemtime-ban",
            src_dir: "crates/lodestone-world/src",
            banned: "SystemTime::now(",
            allowlist: &["world.rs"],
        },
        ConfinementRule {
            label: "lodestone-testsupport instant-ban",
            src_dir: "crates/lodestone-testsupport/src",
            banned: "Instant::now(",
            allowlist: &["lib.rs"],
        },
        // `bench_record.rs` is gated at the module declaration by both the
        // `bench-record` feature and not-wasm32. The scanner is lexical, so the
        // native-only file must still be named here.
        ConfinementRule {
            label: "lodestone-testsupport systemtime-ban",
            src_dir: "crates/lodestone-testsupport/src",
            banned: "SystemTime::now(",
            allowlist: &["lib.rs", "bench_record.rs"],
        },
        ConfinementRule {
            label: "lodestone-allocbench instant-ban",
            src_dir: "crates/lodestone-allocbench/src",
            banned: "Instant::now(",
            allowlist: &["main.rs"],
        },
        ConfinementRule {
            label: "lodestone-allocbench systemtime-ban",
            src_dir: "crates/lodestone-allocbench/src",
            banned: "SystemTime::now(",
            allowlist: &["main.rs"],
        },
        // lodestone-time itself: the ONE place allowed to depend on
        // `web-time`, so every other crate's rule above can ban
        // `std::time::{Instant,SystemTime}` with an empty allowlist. This
        // crate is held to the identical rule, with an EMPTY allowlist too —
        // it has no special exemption to spell `std::time` directly, because
        // everything it re-exports comes from `web_time`, whose own non-wasm
        // arm is `pub use std::time::*` — that happens inside the `web-time`
        // dependency, not in this crate's own source.
        ConfinementRule {
            label: "lodestone-time instant-ban",
            src_dir: "crates/lodestone-time/src",
            banned: "std::time::Instant",
            allowlist: &[],
        },
        ConfinementRule {
            label: "lodestone-time systemtime-ban",
            src_dir: "crates/lodestone-time/src",
            banned: "std::time::SystemTime",
            allowlist: &[],
        },
        // The dedicated server Worker is a separate wasm crate, not a root
        // workspace member. Keep its entry point free of crash-class host calls
        // even though its shell/server dependencies have their own crate-level
        // guards.
        ConfinementRule {
            label: "lodestone-server-worker thread-ban",
            src_dir: "web/worker/src",
            banned: "std::thread::spawn",
            allowlist: &[],
        },
        ConfinementRule {
            label: "lodestone-server-worker instant-ban",
            src_dir: "web/worker/src",
            banned: "std::time::Instant",
            allowlist: &[],
        },
        ConfinementRule {
            label: "lodestone-server-worker systemtime-ban",
            src_dir: "web/worker/src",
            banned: "std::time::SystemTime",
            allowlist: &[],
        },
    ]
}

/// A single banned-symbol hit outside the allowlisted file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfinementLeak {
    /// Path relative to the workspace root.
    pub path: PathBuf,
    /// 1-based line number.
    pub line: usize,
    /// Full line content.
    pub content: String,
}

/// Scans one rule's src dir for the banned symbol, skipping allowlisted file
/// basenames and comment lines. A missing src dir is an ERROR, not a silent pass —
/// a rule pointing at a typo'd path would otherwise report green forever.
///
/// Every rule has a positive control:
/// `every_confinement_rule_fires_under_a_planted_violation` plants a violating line
/// in the crate each rule names and requires the scan to report it by path.
pub fn scan_confinement(
    workspace_root: &Path,
    rule: &ConfinementRule,
) -> Result<Vec<ConfinementLeak>> {
    let root = workspace_root.join(rule.src_dir);
    if !root.is_dir() {
        bail!(
            "confinement rule {:?} scans a missing dir: {}",
            rule.label,
            root.display()
        );
    }
    let mut leaks = Vec::new();
    scan_confinement_dir(&root, workspace_root, rule, &mut leaks)?;
    leaks.sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));
    Ok(leaks)
}

/// True for a line whose first non-whitespace characters make it a comment, so a
/// banned symbol inside it cannot execute.
///
/// Parity with the reference script, which drops the same three openers. Every one
/// of these confinements is worth a sentence at its call site saying "use
/// `crate::platform::Instant`, not `std::time::Instant`, because the latter traps",
/// and a guard that fires on its own documentation trains people to delete the
/// documentation. Same reasoning that made a `"` legal inside a `.wgsl` comment.
///
/// SCOPE LIMIT, stated because a filter you trust further than it reaches is worse
/// than none: this is a line-opener test, not a lexer. It does not see a `/* … */`
/// block whose first line does not start with `*`, and it does not see a trailing
/// `// …` comment after code — which is the safe direction, since such a line has
/// executable content anyway. A hand-rolled Rust lexer would be wrong about
/// lifetimes; three scanners in this repo already were.
fn is_comment_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("//") || trimmed.starts_with('*') || trimmed.starts_with('#')
}

fn scan_confinement_dir(
    dir: &Path,
    workspace_root: &Path,
    rule: &ConfinementRule,
    leaks: &mut Vec<ConfinementLeak>,
) -> Result<()> {
    let entries = std::fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("read dir entry under {}", dir.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("stat {}", path.display()))?;
        if file_type.is_dir() {
            scan_confinement_dir(&path, workspace_root, rule, leaks)?;
        } else if file_type.is_file() {
            if rule
                .allowlist
                .contains(&entry.file_name().to_string_lossy().as_ref())
            {
                continue;
            }
            // Lossy read (not read_to_string) so a non-UTF-8 file is still
            // scanned rather than silently skipped, matching grep's behaviour.
            //
            // A file that disappeared between `read_dir` and here is skipped rather
            // than fatal: this is a shared checkout where another agent may delete a
            // file mid-walk, and a file that no longer exists cannot carry a hazard.
            // Only NotFound is tolerated — a permissions error still fails loudly,
            // because that one CAN hide a leak.
            let bytes = match std::fs::read(&path) {
                Ok(bytes) => bytes,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => {
                    return Err(err).with_context(|| format!("read {}", path.display()));
                }
            };
            let text = String::from_utf8_lossy(&bytes);
            for (index, line) in text.lines().enumerate() {
                if line.contains(rule.banned) && !is_comment_line(line) {
                    let rel = path
                        .strip_prefix(workspace_root)
                        .unwrap_or(&path)
                        .to_path_buf();
                    leaks.push(ConfinementLeak {
                        path: rel,
                        line: index + 1,
                        content: line.to_string(),
                    });
                }
            }
        }
    }
    Ok(())
}

/// Runs the full wasm-check tripwire: prereqs, per-crate compile, confinement
/// guards, then the trunk build of web/. Returns Err (non-zero exit) on any
/// failure — `cargo xtask wasm-check`, the tested replacement for
/// scripts/wasm-check.sh.
pub fn run_wasm_check(workspace_root: &Path) -> Result<()> {
    ensure_wasm_prereqs()?;

    println!("== Lodestone wasm32 compile guard ==");
    println!("target: {WASM_TARGET}");
    println!();

    let mut failures: Vec<String> = Vec::new();

    for wasm_crate in wasm_crates() {
        let display = if wasm_crate.extra_args.is_empty() {
            wasm_crate.name.to_string()
        } else {
            format!("{} {}", wasm_crate.name, wasm_crate.extra_args.join(" "))
        };
        print!("  {display:<34} ");
        match compile_crate_for_wasm(workspace_root, &wasm_crate) {
            Ok(()) => println!("PASS"),
            Err(failure) => {
                println!("FAIL");
                failures.push(format!(
                    "{} {}",
                    wasm_crate.name,
                    wasm_crate.extra_args.join(" ")
                ));
                report_build_failure(&failure);
                println!(
                    "      └─ two common causes: (a) a dependency pulled '{}' onto native-only",
                    wasm_crate.name
                );
                println!("         code (threads / std::fs / OS sockets / OS audio like cpal) — fix by gating");
                println!("         that dep or call behind cfg(not(target_arch = \"wasm32\")) or an");
                println!("         off-by-default feature; or (b) a plain compile error in '{}' or a crate", wasm_crate.name);
                println!("         it depends on — which, in this shared workspace, is often a sibling crate");
                println!("         mid-edit (see the named crate in the error above): wait and re-run.");
                println!(
                    "         Reproduce: cargo build -p {} --target {WASM_TARGET} {}",
                    wasm_crate.name,
                    wasm_crate.extra_args.join(" ")
                );
            }
        }
    }

    // The authoritative singleplayer server is a second browser wasm artifact,
    // built from its own `web/` workspace.  Trunk also builds it while staging
    // the final page, but waiting for that hook obscures a worker-only failure
    // behind the page build.  Keep the worker compile as an explicit gate so a
    // green page crate cannot be mistaken for a green singleplayer path.
    print!("  {:<34} ", "lodestone-server-worker");
    match compile_server_worker_for_wasm(workspace_root) {
        Ok(()) => println!("PASS"),
        Err(failure) => {
            println!("FAIL");
            failures.push("lodestone-server-worker".to_string());
            report_build_failure(&failure);
            println!(
                "      └─ reproduce: cargo build --manifest-path web/worker/Cargo.toml \
                 --target {WASM_TARGET}"
            );
        }
    }

    print!("  {:<34} ", "server-worker control test");
    match run_worker_bootstrap_tests(workspace_root) {
        Ok(()) => println!("PASS"),
        Err(failure) => {
            println!("FAIL");
            failures.push("server-worker control test".to_string());
            report_build_failure(&failure);
            println!("      └─ reproduce: node --test web/worker/worker_bootstrap.test.mjs");
        }
    }

    // A count with a verdict that depends on the count, printed unconditionally. A
    // confinement rule that reported neither clean nor leaked has measured nothing,
    // and the whole reason these guards exist is that five of them did exactly that
    // in the reference script for their entire life.
    let mut rules_scanned = 0usize;
    let rule_total = confinement_rules().len();
    for rule in confinement_rules() {
        print!("  {:<34} ", rule.label);
        match scan_confinement(workspace_root, &rule) {
            Ok(leaks) if leaks.is_empty() => {
                rules_scanned += 1;
                println!("PASS");
            }
            Ok(leaks) => {
                rules_scanned += 1;
                println!("FAIL");
                for leak in &leaks {
                    println!(
                        "      {}:{}:{}",
                        leak.path.display(),
                        leak.line,
                        leak.content
                    );
                }
                failures.push(format!(
                    "{}: '{}' used outside {{{}}}",
                    rule.label,
                    rule.banned,
                    rule.allowlist.join(",")
                ));
            }
            Err(err) => {
                println!("FAIL");
                println!("      {err:#}");
                failures.push(format!("{}: scanner error: {err:#}", rule.label));
            }
        }
    }
    println!();
    println!("  confinement rules that actually ran: {rules_scanned}/{rule_total}");
    if rules_scanned != rule_total {
        failures.push(format!(
            "only {rules_scanned} of {rule_total} confinement rules ran"
        ));
    }
    println!();

    // The browser app is its own workspace (outside the crates/ glob), built
    // through trunk so a wasm-bindgen-level break is caught, not just a rustc
    // one. Cheap because the crate graph above is already warm in the shared
    // target dir.
    if workspace_root.join("web").join("Cargo.toml").is_file() {
        print!("  {:<34} ", "lodestone-web (trunk build)");
        match build_web_with_trunk(workspace_root) {
            Ok(()) => println!("PASS"),
            Err(failure) => {
                println!("FAIL");
                failures.push("lodestone-web (trunk build)".to_string());
                report_build_failure(&failure);
                println!("      └─ the browser app failed to build. If the per-crate rows above are all");
                println!("         PASS, this is a wasm-bindgen/trunk-level break in web/ itself.");
                println!("         Reproduce: (cd web && trunk build)");
            }
        }
    }

    println!();
    if !failures.is_empty() {
        bail!(
            "RESULT: FAIL — {} item(s) failed the wasm check:\n  - {}",
            failures.len(),
            failures.join("\n  - ")
        );
    }

    println!("RESULT: PASS — all listed crates COMPILE to {WASM_TARGET}.");
    println!();
    println!("NOTE: the COMPILE pass proves compilation, NOT runtime, and is blind to the");
    println!("      'compiles on wasm, panics at runtime' family: std::fs, Instant::now,");
    println!("      std::thread::spawn, tokio::time all build green here. cfg(target_arch)");
    println!("      does NOT turn a fresh ungated call into a compile error (it only removes");
    println!("      existing native entry points), and a Cargo feature is weaker still");
    println!("      (unification re-enables it). The CONFINEMENT guards above are what");
    println!("      actually catch a leaked hazard, by reporting it back to file:line.");
    Ok(())
}

/// A check that cannot run must FAIL, not pass quietly (the script's own
/// philosophy, kept here): a missing wasm32 target or trunk is an error with
/// the install command, never a silent green.
fn ensure_wasm_prereqs() -> Result<()> {
    let installed = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).to_string())
        .unwrap_or_default();
    if !installed.contains(WASM_TARGET) {
        bail!(
            "error: rust target '{WASM_TARGET}' is not installed.\n       \
             this check CANNOT RUN without it — failing rather than passing quietly.\n       \
             run: rustup target add {WASM_TARGET}"
        );
    }

    let trunk_present = Command::new("trunk")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);
    if !trunk_present {
        bail!(
            "error: 'trunk' is not installed (required to build/serve the browser app).\n       \
             this check CANNOT RUN without it — failing rather than passing quietly.\n       \
             run: cargo install trunk --version 0.21.14\n       \
             or (prebuilt, faster): curl -sSL \\\n       \
             https://github.com/trunk-rs/trunk/releases/download/v0.21.14/trunk-$(uname -m)-apple-darwin.tar.gz \\\n       \
             | tar xz -C ~/.cargo/bin trunk"
        );
    }
    let node_present = Command::new("node")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);
    if !node_present {
        bail!(
            "error: 'node' is not installed (required for the browser worker control-plane test).\n       \
             this check CANNOT RUN without it — failing rather than passing quietly."
        );
    }
    Ok(())
}

/// Where wasm-check writes the full output of each failed build, so the console
/// summary is never the only copy.
pub const WASM_CHECK_LOG_DIR: &str = "target/wasm-check";

/// How many summary lines a failed build may print before being cut off. Chosen
/// to fit a `Caused by:` chain or one rustc diagnostic with its `-->` frame,
/// which the previous 6/8-line caps could not.
const WASM_DIAGNOSTIC_MAX_LINES: usize = 40;

/// Substrings (matched case-insensitively, against ANSI-stripped text) that mark
/// a line worth showing from a failed build.
///
/// Deliberately **not anchored**. The anchored form (`line.starts_with("error")`)
/// is what destroyed the evidence in the only CI failure this check has ever
/// caught: `trunk` prefixes every line with an RFC-3339 timestamp and a level, so
/// nothing it writes starts with `error`, and `cargo` under
/// `CARGO_TERM_COLOR=always` (which CI sets globally) starts its error lines with
/// an escape sequence rather than a letter. An anchor is only as good as the
/// assumption that the producer writes bare, uncoloured, unprefixed lines, and
/// neither producer here does.
const WASM_DIAGNOSTIC_MARKERS: &[&str] = &[
    "error",
    "caused by",
    "could not compile",
    "is not supported",
    "unresolved import",
    "cannot find",
    "wasm-bindgen",
];

/// A captured failed build: the full combined stdout+stderr, plus the path the
/// whole thing was written to.
///
/// This type exists because of a measured failure of what it replaces. The
/// previous shape returned a bare `String` that the caller pushed through an
/// anchored `starts_with("error")` filter capped at 8 lines. The one CI failure
/// this check has ever caught therefore reported exactly two useless lines --
/// `error from build pipeline` and `trunk`'s own timestamped echo of it -- while
/// the `Caused by:` chain naming the actual missing file never reached the log at
/// all, and the failure had to be re-diagnosed from scratch. The filter that
/// makes output readable is also the filter that can invent a silence, so the
/// full output now always goes to a file and the console view is explicitly a
/// summary *of that file*.
#[derive(Debug)]
pub struct CapturedBuild {
    /// Combined stdout+stderr of the failed command, prefixed with the command
    /// line and its real exit status.
    pub output: String,
    /// Where the full output was written. `None` only when the log could not be
    /// written — which must never itself replace the build error.
    pub log_path: Option<PathBuf>,
}

/// Runs `command`, returning `Ok(())` on a zero exit and a [`CapturedBuild`]
/// otherwise.
///
/// The verdict comes from the process's own exit status, never from what its
/// output looks like: a build that prints the word `error` and exits 0 is a
/// warning, and a build that prints nothing and exits 1 is still a failure.
fn run_captured_build(
    command: &mut Command,
    workspace_root: &Path,
    log_name: &str,
) -> Result<(), CapturedBuild> {
    // Ask cargo not to colour output we are about to machine-match, which also
    // covers the cargo `trunk` shells out to. Belt-and-braces with `strip_ansi`
    // rather than a substitute for it: this keeps the *log file* readable, and the
    // strip keeps the *matching* correct for any producer that colours anyway.
    //
    configure_captured_build(command);
    let description = format!("{command:?}");
    let output = match command.output() {
        Ok(output) => output,
        // A spawn failure is a failure like any other and is reported through
        // the same path, so it can never be mistaken for a green.
        Err(err) => {
            let text = format!("failed to spawn {description}: {err}\n");
            let log_path = write_wasm_check_log(workspace_root, log_name, &text);
            return Err(CapturedBuild {
                output: text,
                log_path,
            });
        }
    };
    if output.status.success() {
        return Ok(());
    }
    let combined = format!(
        "$ {description}\nexit status: {}\n\n{}{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let log_path = write_wasm_check_log(workspace_root, log_name, &combined);
    Err(CapturedBuild {
        output: combined,
        log_path,
    })
}

/// Makes captured builds independent of presentation variables inherited from
/// the caller. Trunk maps `NO_COLOR` to a clap boolean, so the conventional
/// `NO_COLOR=1` value aborts before the browser build begins.
fn configure_captured_build(command: &mut Command) {
    command.env_remove("NO_COLOR");
    command.env("CARGO_TERM_COLOR", "never");
}

/// Writes `contents` to `target/wasm-check/<log_name>.log`, returning its path.
///
/// A write failure is reported inline and swallowed on purpose: losing the log
/// file must degrade the diagnosis, never replace the build error with a
/// filesystem error.
fn write_wasm_check_log(
    workspace_root: &Path,
    log_name: &str,
    contents: &str,
) -> Option<PathBuf> {
    let sanitised: String = log_name
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect();
    let dir = workspace_root.join(WASM_CHECK_LOG_DIR);
    if let Err(err) = std::fs::create_dir_all(&dir) {
        println!("      │ (could not create {}: {err})", dir.display());
        return None;
    }
    let path = dir.join(format!("{sanitised}.log"));
    match std::fs::write(&path, contents) {
        Ok(()) => Some(path),
        Err(err) => {
            println!("      │ (could not write {}: {err})", path.display());
            None
        }
    }
}

/// Strips ANSI escape sequences from captured output.
///
/// Load-bearing for every match in [`select_diagnostic_lines`], not cosmetic.
/// With `CARGO_TERM_COLOR=always` set — which this repo's CI sets for every job —
/// a line that reads `error: …` on a terminal is really
/// `ESC[1mESC[31merror ESC[0m: …` in the captured bytes, and any anchored match
/// against it silently fails. Handles the CSI (`ESC [` … final byte in
/// `0x40..=0x7E`) and OSC (`ESC ]` … BEL or `ESC \`) forms; any other byte after
/// an ESC drops the ESC alone, which cannot turn a matching line into a
/// non-matching one.
#[must_use]
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch != '\x1b' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for next in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&next) {
                        break;
                    }
                }
            }
            Some(']') => {
                let mut prev_was_esc = false;
                for next in chars.by_ref() {
                    if next == '\x07' || (prev_was_esc && next == '\\') {
                        break;
                    }
                    prev_was_esc = next == '\x1b';
                }
            }
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// Selects the lines of `output` worth showing: every line containing one of
/// `markers`, plus the indented continuation lines that follow it.
///
/// The continuation rule is the half the previous filter lacked, and it is where
/// the whole diagnosis lives: `Caused by:`'s numbered causes and rustc's
/// `--> file:line` / `|` frames all arrive *indented, on the lines after* the one
/// that matched, so a per-line filter drops precisely the payload and keeps only
/// the headline.
#[must_use]
pub fn select_diagnostic_lines(output: &str, markers: &[&str], max_lines: usize) -> Vec<String> {
    let mut selected = Vec::new();
    let mut in_continuation = false;
    for line in output.lines() {
        if selected.len() >= max_lines {
            break;
        }
        let lower = line.to_ascii_lowercase();
        if markers.iter().any(|marker| lower.contains(marker)) {
            selected.push(line.trim_end().to_owned());
            in_continuation = true;
            continue;
        }
        let blank = line.trim().is_empty();
        if in_continuation && !blank && (line.starts_with(' ') || line.starts_with('\t')) {
            selected.push(line.trim_end().to_owned());
            continue;
        }
        if !blank {
            in_continuation = false;
        }
    }
    selected
}

/// Prints a diagnosable summary of a failed build, and says where the full
/// output is.
///
/// Three properties, each of which the anchored-grep-and-truncate version it
/// replaces lacked: matching happens on ANSI-**stripped** text; a matched line
/// brings its continuation lines with it; and when nothing matches, the tail is
/// printed **verbatim** rather than nothing at all. That last one is the
/// mechanism fix — a filter that can yield an empty summary turns a failing build
/// into a silent one, and CLAUDE.md's rule is that output which prints nothing
/// must be read as a failure to run, never as an absence of findings.
fn report_build_failure(failure: &CapturedBuild) {
    if let Some(path) = &failure.log_path {
        println!("      │ full output: {}", path.display());
    }
    let stripped = strip_ansi(&failure.output);
    let selected = select_diagnostic_lines(
        &stripped,
        WASM_DIAGNOSTIC_MARKERS,
        WASM_DIAGNOSTIC_MAX_LINES,
    );
    if !selected.is_empty() {
        for line in selected {
            println!("      │ {line}");
        }
        return;
    }
    println!(
        "      │ (no line matched the diagnostic markers — last {WASM_DIAGNOSTIC_MAX_LINES} \
         non-blank lines verbatim)"
    );
    let tail: Vec<&str> = stripped
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let start = tail.len().saturating_sub(WASM_DIAGNOSTIC_MAX_LINES);
    for line in &tail[start..] {
        println!("      │ {line}");
    }
}

/// Overrides `[profile.dev].codegen-backend = "cranelift"` (`.cargo/config.toml`)
/// back to LLVM for the one target that setting does not apply to.
///
/// `rustc_codegen_cranelift` has no wasm32 backend at all: "error: can't compile
/// for wasm32-unknown-unknown: Support for this target has not been implemented
/// yet". Cargo profiles are not target-scoped, so `[profile.dev]` reaches every
/// `--target wasm32-unknown-unknown` build the same as a native one — landing
/// Cranelift as the workspace default (see docs/compile-times.md) silently took
/// every row of this check from PASS to FAIL, compile error rather than a
/// runtime hazard, and nothing native-side could have shown it. `.cargo/config.toml`
/// cannot express "cranelift except for this target" (profile tables are not
/// conditional on target triple), so the override has to live at the call site
/// instead — the same reasoning `docs/compile-times.md` already gives for
/// `RUSTFLAGS` clobbering `build.rustflags`, just one level further out.
const WASM_CODEGEN_BACKEND_ENV: (&str, &str) = ("CARGO_PROFILE_DEV_CODEGEN_BACKEND", "llvm");

/// Runs `cargo build -p <name> --target wasm32-unknown-unknown [extra]` from
/// the workspace root, capturing the build to a log file on failure. The native
/// xtask binary's own `--target-dir` is deliberately NOT forwarded: the wasm
/// build shares the default target/ dir, exactly as the script did.
fn compile_crate_for_wasm(
    workspace_root: &Path,
    wasm_crate: &WasmCrate,
) -> Result<(), CapturedBuild> {
    let mut command = Command::new("cargo");
    command
        .arg("build")
        .arg("-p")
        .arg(wasm_crate.name)
        .arg("--target")
        .arg(WASM_TARGET)
        .args(wasm_crate.extra_args)
        .env(WASM_CODEGEN_BACKEND_ENV.0, WASM_CODEGEN_BACKEND_ENV.1)
        .current_dir(workspace_root);
    run_captured_build(&mut command, workspace_root, wasm_crate.name)
}

/// Builds the dedicated browser singleplayer server, whose manifest belongs to
/// the separate `web/` workspace rather than the root workspace package set.
/// Keeping this separate from [`wasm_crates`] preserves the reference script's
/// package table while making the worker failure visible before `trunk` runs its
/// staging hook.
fn compile_server_worker_for_wasm(workspace_root: &Path) -> Result<(), CapturedBuild> {
    let manifest = workspace_root.join("web/worker/Cargo.toml");
    let mut command = Command::new("cargo");
    command
        .arg("build")
        .arg("--manifest-path")
        .arg(&manifest)
        .arg("--target")
        .arg(WASM_TARGET)
        .env(WASM_CODEGEN_BACKEND_ENV.0, WASM_CODEGEN_BACKEND_ENV.1)
        .current_dir(workspace_root);
    run_captured_build(&mut command, workspace_root, "lodestone-server-worker")
}

/// Runs `trunk build` inside web/, capturing the build to a log file on failure.
fn build_web_with_trunk(workspace_root: &Path) -> Result<(), CapturedBuild> {
    let mut command = Command::new("trunk");
    command
        .arg("build")
        .env(WASM_CODEGEN_BACKEND_ENV.0, WASM_CODEGEN_BACKEND_ENV.1)
        .current_dir(workspace_root.join("web"));
    run_captured_build(&mut command, workspace_root, "lodestone-web-trunk")
}

/// Runs the browser-worker control-plane test without needing a browser. The
/// test supplies a synthetic Worker event and port, then proves startup never
/// reports `ready` before the supplied wasm entry point accepts that port.
fn run_worker_bootstrap_tests(workspace_root: &Path) -> Result<(), CapturedBuild> {
    let mut command = Command::new("node");
    command
        .args(["--test", "web/worker/worker_bootstrap.test.mjs"])
        .current_dir(workspace_root);
    run_captured_build(&mut command, workspace_root, "server-worker-control")
}

#[cfg(test)]
mod tests;
