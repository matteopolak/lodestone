//! Oracle gate for the bundled loot-table subset (issue #337).
//!
//! The tables under `crates/lodestone-server/assets/loot_table/` claim to be
//! verbatim copies of Mojang's own 26.2 datapack data. This gate re-reads the
//! full 1355-table corpus from the decompiled client's data folder and proves
//! two things:
//!
//! 1. **Bundle parity.** Every bundled table is identical to the corpus copy,
//!    modulo the final newline (Mojang's own data files omit it; the checked-in
//!    copies keep a conventional trailing `\n`). A bundled table that drifted
//!    from the game data — or that does not exist in vanilla at all — fails
//!    here.
//! 2. **Whole-corpus parse.** Every one of the 1355 corpus tables parses
//!    without a hard [`LootError`]. A `LootError` on any valid table would mean
//!    the parser misreads a shape the empty-context roller must at least
//!    tolerate. Tables whose *features* the roller does not evaluate still
//!    load; they report them through
//!    [`LootTable::unsupported_features`](lodestone_server::loot::LootTable::unsupported_features).
//!    The printed `supported`/`partial` split (with `--nocapture`) is a
//!    coverage measure for the issue's "table format first" scope, not an
//!    assertion.
//!
//! Like the other oracle gates (`collision_shapes`, `hardness`), this is
//! `#[ignore]`d: it needs `.cache/mc/26.2/client-src/` present. Run it with:
//!
//! ```text
//! cargo test -p lodestone-server --test loot_corpus -- --ignored --nocapture
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use lodestone_model::ResourceKey;
use lodestone_server::loot::{LootTable, LootTableSet};

/// The decompiled client's datapack data, relative to `crates/lodestone-server/`.
///
/// **`../..`, not `../../..`.** `CARGO_MANIFEST_DIR` is
/// `<repo>/crates/lodestone-server`, so two levels up is the repo root and three
/// is its *parent*. This gate carried the three-level version from the day it was
/// written and therefore had **never once run**: both tests aborted on their own
/// `root.is_dir()` precondition with "corpus not found at
/// …/crates/lodestone-server/../../../.cache/…". Because it is `#[ignore]`d, no
/// health check here could see it — the whole class CLAUDE.md calls the
/// precondition species, except that the precondition *failed* rather than
/// silently skipping, and nobody was watching. `just regen-loot-corpus` now exists
/// so there is a named way to run it.
fn corpus_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.cache/mc/26.2/client-src/data/minecraft/loot_table")
}

/// Collects `(id, contents)` for every JSON under `root`, id being the path
/// relative to `root` with the `.json` stripped — the same keying `build.rs`
/// uses for the embedded bundle.
fn collect(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
    let mut children: Vec<_> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .collect();
    children.sort();
    for path in children {
        if path.is_dir() {
            collect(root, &path, out);
        } else if path.extension().is_some_and(|x| x == "json") {
            let rel = path.strip_prefix(root).unwrap().with_extension("");
            let id = rel.to_string_lossy().replace('\\', "/");
            let contents = fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
            out.push((id, contents));
        }
    }
}

#[test]
#[ignore = "needs .cache/mc/26.2/client-src (the decompiled client)"]
fn bundled_tables_match_the_vanilla_corpus() {
    let root = corpus_root();
    assert!(
        root.is_dir(),
        "corpus not found at {} — run the oracle-setup steps that populate .cache/mc/26.2",
        root.display(),
    );

    let mut corpus: Vec<(String, String)> = Vec::new();
    collect(&root, &root, &mut corpus);
    let corpus_by_id: std::collections::HashMap<_, _> = corpus.iter().cloned().collect();
    assert_eq!(
        corpus.len(),
        1355,
        "corpus size changed — the gate's assumptions may be stale"
    );

    let set = LootTableSet::load_bundled();
    let mut checked = 0usize;
    for (id, bundled) in corpus.iter().filter(|(id, _)| set.get(&parse(id)).is_some()) {
        // load_bundled keyed the bundle by `minecraft:{id}`; this filter only
        // sees ids the set actually registered, so `set.get` must succeed.
        checked += 1;
        let vanilla = corpus_by_id
            .get(id)
            .unwrap_or_else(|| panic!("bundled {id} is not in the vanilla corpus"));
        // Mojang's data files end without a newline; the checked-in copies keep
        // a conventional trailing `\n`. That is the only permitted difference.
        assert_eq!(
            bundled.trim_end_matches('\n'),
            vanilla.trim_end_matches('\n'),
            "bundled table {id} drifted from the vanilla corpus",
        );
    }
    assert_eq!(
        checked,
        set.len(),
        "every bundled table must be found in the corpus"
    );
}

#[test]
#[ignore = "needs .cache/mc/26.2/client-src (the decompiled client)"]
fn every_corpus_table_parses_without_a_hard_error() {
    let root = corpus_root();
    assert!(root.is_dir(), "corpus not found at {}", root.display());

    let mut corpus: Vec<(String, String)> = Vec::new();
    collect(&root, &root, &mut corpus);

    let mut failures: Vec<(String, String)> = Vec::new();
    let mut supported = 0usize;
    let mut partial = 0usize;
    for (id, contents) in &corpus {
        let key: ResourceKey = format!("minecraft:{id}").parse().expect("corpus id is a valid key");
        match LootTable::from_json(&key, contents) {
            Ok(table) => {
                if table.unsupported_features().is_empty() {
                    supported += 1;
                } else {
                    partial += 1;
                }
            }
            Err(error) => failures.push((id.clone(), error.to_string())),
        }
    }

    eprintln!("corpus: {supported} fully supported, {partial} partial (use unsupported features)");
    assert!(
        failures.is_empty(),
        "{} corpus tables hard-failed to parse: {failures:?}",
        failures.len(),
    );
}

fn parse(id: &str) -> ResourceKey {
    format!("minecraft:{id}").parse().expect("bundled id is a valid key")
}

/// Where the bundle lives, relative to `crates/lodestone-server/`.
fn bundle_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/loot_table")
}

/// The subset of the corpus this crate is allowed to bundle: every table whose
/// features the roller either fully evaluates or only fails to *decorate*
/// (`lodestone_server::loot::DECORATION_ONLY_UNSUPPORTED` — the enchantment,
/// name, exploration-map and stew-effect functions, whose absence leaves the item
/// and count correct).
///
/// `LootTableSet::load_bundled` debug-asserts exactly this per bundled table, so
/// "clean" is not a preference — it is the bundling precondition. Computed here
/// from the **cache**, never from the bundle, which is what lets the gate below
/// notice a table falling out of scope.
fn clean_corpus() -> Vec<(String, String)> {
    let root = corpus_root();
    assert!(
        root.is_dir(),
        "corpus not found at {} — run the oracle-setup steps that populate .cache/mc/26.2",
        root.display(),
    );
    let mut corpus: Vec<(String, String)> = Vec::new();
    collect(&root, &root, &mut corpus);
    corpus
        .into_iter()
        .filter(|(id, contents)| {
            let key: ResourceKey = format!("minecraft:{id}").parse().expect("corpus id");
            LootTable::from_json(&key, contents).is_ok_and(|table| {
                table.unsupported_features().iter().all(|feature| {
                    lodestone_server::loot::DECORATION_ONLY_UNSUPPORTED.contains(&feature.as_str())
                })
            })
        })
        .collect()
}

/// **Issue #538: the bundle *is* the clean subset of the vanilla corpus, and this
/// generates it or asserts it.**
///
/// `LODESTONE_REGEN=1` rewrites `assets/loot_table/` from the cache; without it,
/// the test asserts the two agree exactly — the generate-or-assert shape
/// `crates/lodestone-data/tests/{collision_shapes,hardness}.rs` established. Run
/// it with `just regen-loot-corpus`.
///
/// # Why the comparison has to be three-sided
///
/// `CLAUDE.md`: a gate that compares two things you control cannot tell you a
/// third thing exists. So this asserts all three directions, and the middle one
/// is the one a bundle-only drift check structurally cannot make:
///
/// 1. every bundled table is **byte-identical** to Mojang's copy (modulo the
///    trailing newline their generator omits);
/// 2. every **clean corpus** table is bundled — a table that newly becomes clean
///    because the roller learned a feature fails here until it is added, and a
///    bundled table that stops being clean fails the `load_bundled` assertion;
/// 3. nothing is bundled that is **not** in the clean subset — which catches both
///    an invented table and one whose features regressed.
#[test]
#[ignore = "needs .cache/mc/26.2/client-src (the decompiled client)"]
fn the_bundle_is_exactly_the_clean_subset_of_the_vanilla_corpus() {
    let clean = clean_corpus();
    let bundle = bundle_root();
    let regen = std::env::var_os("LODESTONE_REGEN").is_some();

    let total_bytes: usize = clean.iter().map(|(_, text)| text.len() + 1).sum();
    eprintln!(
        "clean subset: {} tables, {total_bytes} bytes ({:.1} KB) — see docs/loot-tables.md",
        clean.len(),
        total_bytes as f64 / 1024.0,
    );

    if regen {
        // Remove the whole tree first, so a table that stopped being clean is
        // *deleted* rather than left behind to trip `load_bundled`.
        if bundle.is_dir() {
            fs::remove_dir_all(&bundle).expect("clearing the bundle");
        }
        for (id, contents) in &clean {
            let path = bundle.join(format!("{id}.json"));
            fs::create_dir_all(path.parent().expect("id has a parent")).expect("mkdir");
            // Mojang's files end without a newline; the checked-in copies keep a
            // conventional trailing one, which is the sole permitted difference
            // (and what `bundled_tables_match_the_vanilla_corpus` allows for).
            fs::write(&path, format!("{}\n", contents.trim_end_matches('\n'))).expect("write");
        }
        eprintln!("regenerated {} into {}", clean.len(), bundle.display());
        return;
    }

    let mut on_disk: Vec<(String, String)> = Vec::new();
    collect(&bundle, &bundle, &mut on_disk);

    let clean_ids: std::collections::BTreeSet<&str> =
        clean.iter().map(|(id, _)| id.as_str()).collect();
    let bundled_ids: std::collections::BTreeSet<&str> =
        on_disk.iter().map(|(id, _)| id.as_str()).collect();

    let missing: Vec<&&str> = clean_ids.difference(&bundled_ids).collect();
    assert!(
        missing.is_empty(),
        "{} clean vanilla tables are not bundled (run `just regen-loot-corpus`): {:?}",
        missing.len(),
        &missing[..missing.len().min(20)],
    );
    let extra: Vec<&&str> = bundled_ids.difference(&clean_ids).collect();
    assert!(
        extra.is_empty(),
        "{} bundled tables are not in the clean vanilla subset — either invented, \
         or their features stopped being supported: {:?}",
        extra.len(),
        &extra[..extra.len().min(20)],
    );

    let clean_by_id: std::collections::HashMap<&str, &str> =
        clean.iter().map(|(id, t)| (id.as_str(), t.as_str())).collect();
    let mut compared = 0usize;
    for (id, bundled) in &on_disk {
        let vanilla = clean_by_id[id.as_str()];
        assert_eq!(
            bundled.trim_end_matches('\n'),
            vanilla.trim_end_matches('\n'),
            "bundled table {id} drifted from the vanilla corpus",
        );
        compared += 1;
    }
    assert_eq!(
        compared,
        clean.len(),
        "every clean table must have been compared, not merely enumerated"
    );

    // And the embedded copy `build.rs` produced must agree with the files, which
    // is a different question from the files agreeing with the cache: a stale
    // `OUT_DIR` would pass every assertion above.
    assert_eq!(
        LootTableSet::load_bundled().len(),
        clean.len(),
        "the embedded bundle and the checked-in files disagree — `build.rs`'s \
         rerun-if-changed did not fire, or a file is not valid JSON"
    );
}
