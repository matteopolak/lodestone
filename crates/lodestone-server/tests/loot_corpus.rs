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
fn corpus_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../.cache/mc/26.2/client-src/data/minecraft/loot_table")
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
