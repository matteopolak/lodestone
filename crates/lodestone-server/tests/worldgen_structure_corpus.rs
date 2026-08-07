//! Drift gate for the bundled 26.2 structure corpus, plus the cross-registry
//! closure checks that say whether the corpus is actually *usable*.
//!
//! # What this guards
//!
//! `assets/worldgen/{structure,structure_set,template_pool,processor_list,
//! world_preset,flat_level_generator_preset,tags/worldgen}` and
//! `assets/structure/**.nbt` — 1606 files extracted verbatim from the real 26.2
//! server jar by `scripts/extract-worldgen-structures.py` (issue #484, phase
//! S-data of `docs/plans/worldgen-rewrite.md`).
//!
//! # Why a hash manifest is the anchor
//!
//! `tests/support/worldgen_structure_corpus.txt` lists, for every bundled file,
//! the SHA-256 and byte length **of the jar entry** — not of the bundled copy.
//! Regenerating it requires the jar, which is not repo state, so the manifest is
//! an external anchor in the sense `CLAUDE.md`'s evidence standard means: an
//! asset edited by hand fails [`bundled_corpus_is_byte_identical_to_the_jar_manifest`],
//! and the manifest cannot be re-derived *from the assets* to hide the edit.
//! This is deliberately not `decode(encode(x)) == x`: nothing here compares two
//! things we produced.
//!
//! Committing a second verbatim copy as a dump (the `damage_types_jar.txt`
//! pattern) would mean 4.42 MiB of duplicated payload; hashes cost 130 KB and
//! fail just as loudly.
//!
//! # The three drift directions
//!
//! A gate that only re-hashes listed files cannot see a file that was *added*,
//! which is how a partial or polluted extraction hides. So:
//!
//! * content drift — [`bundled_corpus_is_byte_identical_to_the_jar_manifest`]
//! * files added or removed — [`manifest_covers_the_bundled_tree_exactly`]
//! * the enumeration itself — [`corpus_counts_match_the_jar_enumeration`]
//!
//! # Refreshing after a version bump
//!
//! `just regen-worldgen-structures`, then check
//! [`manifest_matches_a_fresh_jar_extraction`] (`#[ignore]`d — it needs the jar).
//!
//! # Cross-unit boundary
//!
//! `noise_settings/{nether,end}.json` and the nether multi-noise parameter list
//! belong to the concurrent Nether/End unit, not to this phase, which took only
//! `amplified`, `caves`, `floating_islands` and `large_biomes`.
//!
//! [`every_preset_reference_resolves`] originally carried that as a bounded
//! allowance, written to report itself as deletable rather than persist quietly.
//! Those two files have since landed, so the allowance is gone and full closure
//! is required — with a named assertion that both are present, and a control that
//! the presets really do reference both, so their absence fails loudly here
//! instead of being tolerated.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

/// The committed jar-derived manifest — the external anchor.
const MANIFEST: &str = include_str!("support/worldgen_structure_corpus.txt");

/// Counts measured against `.cache/mc/26.2/versions/26.2/server-26.2.jar` on
/// 2026-08-07 by enumerating the jar's own entries, independently of the plan's
/// audit table (which they then confirmed exactly for every registry it names).
///
/// `tags/worldgen/*` is **not** in the plan's table and was found by following
/// what the structure documents actually reference: all 34 state `biomes` as a
/// tag, none inline.
const EXPECTED_COUNTS: &[(&str, usize)] = &[
    ("structure", 1212),
    ("worldgen/flat_level_generator_preset", 9),
    ("worldgen/noise_settings", 4),
    ("worldgen/processor_list", 40),
    ("worldgen/structure", 34),
    ("worldgen/structure_set", 20),
    ("worldgen/tags/worldgen/biome", 68),
    ("worldgen/tags/worldgen/configured_feature", 1),
    ("worldgen/tags/worldgen/flat_level_generator_preset", 1),
    ("worldgen/tags/worldgen/structure", 20),
    ("worldgen/tags/worldgen/world_preset", 2),
    ("worldgen/template_pool", 188),
    ("worldgen/world_preset", 7),
];

/// Total payload, so a silently-truncated extraction cannot pass by having the
/// right file names.
const EXPECTED_TOTAL_BYTES: u64 = 4_635_950;

/// Directories this phase owns outright: the manifest must describe them
/// **exactly**, with no unlisted file and no missing one.
const EXCLUSIVE_ROOTS: &[&str] = &[
    "worldgen/structure",
    "worldgen/structure_set",
    "worldgen/template_pool",
    "worldgen/processor_list",
    "worldgen/world_preset",
    "worldgen/flat_level_generator_preset",
    "worldgen/tags/worldgen",
    "structure",
];

/// `noise_settings/` is shared. `overworld.json` predates this phase and
/// `nether.json`/`end.json` belong to the Nether/End unit, so exact set equality
/// is the wrong assertion there; see [`noise_settings_directory_holds_only_known_files`].
const SHARED_NOISE_SETTINGS_EXTRAS: &[&str] = &["overworld", "nether", "end"];

/// Vanilla 26.2 ships this reference with no corresponding NBT file.
/// `template_pool/ancient_city/walls/no_corners.json` names
/// `minecraft:ancient_city/walls/intact_horizontal_wall_stairs_5`, and the jar
/// holds only `_1`..`_4`. It is a defect in Mojang's data, not in the
/// extraction: a gate asserting that *every* pool reference resolves fails
/// against the authoritative source, so the exception is carried explicitly and
/// its premise is checked by
/// [`the_known_dangling_template_is_really_absent_and_really_referenced`].
const VANILLA_DANGLING_TEMPLATES: &[&str] =
    &["ancient_city/walls/intact_horizontal_wall_stairs_5"];

fn assets() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets")
}

/// One manifest row: asset-relative path, jar-entry SHA-256, jar-entry length.
#[derive(Debug, Clone)]
struct Row {
    path: String,
    sha256: String,
    bytes: u64,
}

fn rows() -> Vec<Row> {
    let mut out = Vec::new();
    for line in MANIFEST.lines() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut f = line.split(' ');
        let path = f.next().expect("row path").to_owned();
        let sha256 = f.next().unwrap_or_else(|| panic!("row {path}: no digest")).to_owned();
        let bytes = f
            .next()
            .unwrap_or_else(|| panic!("row {path}: no length"))
            .parse()
            .unwrap_or_else(|e| panic!("row {path}: bad length: {e}"));
        assert!(f.next().is_none(), "row {path}: trailing fields");
        assert_eq!(sha256.len(), 64, "row {path}: digest is not a sha256");
        out.push(Row { path, sha256, bytes });
    }
    assert!(
        !out.is_empty(),
        "manifest parsed to zero rows — an audit that measures nothing is a \
         failure to run, not a pass"
    );
    out
}

/// The `# counts <key> <n>` header lines, so the human-readable header cannot
/// disagree with the rows underneath it.
fn header_counts() -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for line in MANIFEST.lines() {
        let Some(rest) = line.strip_prefix("# counts ") else {
            continue;
        };
        let mut f = rest.split(' ');
        let key = f.next().expect("counts key");
        if key == "TOTAL" {
            continue;
        }
        let n: usize = f.next().expect("counts value").parse().expect("counts value");
        out.insert(key.to_owned(), n);
    }
    out
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        write!(s, "{b:02x}").expect("writing to a String cannot fail");
    }
    s
}

fn count_key(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    if parts[0] == "worldgen" && parts.get(1) == Some(&"tags") {
        parts[..4].join("/")
    } else if parts[0] == "worldgen" {
        parts[..2].join("/")
    } else {
        parts[0].to_owned()
    }
}

/// Every regular file under `dir`, as paths relative to `assets()`.
fn walk(dir: &Path, out: &mut BTreeSet<String>) {
    let base = assets();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap_or_else(|e| panic!("reading {}: {e}", d.display())) {
            let p = e.expect("dir entry").path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.insert(
                    p.strip_prefix(&base)
                        .expect("under assets/")
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Drift direction 1: content
// ---------------------------------------------------------------------------

#[test]
fn bundled_corpus_is_byte_identical_to_the_jar_manifest() {
    let assets = assets();
    let mut checked = 0usize;
    let mut total = 0u64;
    let mut bad: Vec<String> = Vec::new();

    for row in rows() {
        let p = assets.join(&row.path);
        let Ok(body) = std::fs::read(&p) else {
            bad.push(format!("{}: absent from the bundle", row.path));
            continue;
        };
        if body.len() as u64 != row.bytes {
            bad.push(format!(
                "{}: {} bytes, manifest says {}",
                row.path,
                body.len(),
                row.bytes
            ));
            continue;
        }
        let digest = hex(&Sha256::digest(&body));
        if digest != row.sha256 {
            bad.push(format!(
                "{}: sha256 {} != jar {}",
                row.path, digest, row.sha256
            ));
            continue;
        }
        checked += 1;
        total += row.bytes;
    }

    assert!(
        bad.is_empty(),
        "{} of {} bundled files drifted from the jar. These assets are verbatim \
         jar bytes — a mismatch means somebody hand-edited vanilla data, which \
         is exactly the failure this gate exists for. Re-extract with \
         `just regen-worldgen-structures`.\nFirst offenders:\n  {}",
        bad.len(),
        checked + bad.len(),
        bad[..bad.len().min(12)].join("\n  "),
    );
    assert_eq!(
        total, EXPECTED_TOTAL_BYTES,
        "corpus payload changed; a truncated extraction can have every filename \
         right and still be short"
    );
}

// ---------------------------------------------------------------------------
// Drift direction 2: files added or removed
// ---------------------------------------------------------------------------

#[test]
fn manifest_covers_the_bundled_tree_exactly() {
    let assets = assets();
    let listed: BTreeSet<String> = rows().into_iter().map(|r| r.path).collect();

    let mut on_disk = BTreeSet::new();
    for root in EXCLUSIVE_ROOTS {
        let d = assets.join(root);
        assert!(d.is_dir(), "corpus root missing: {}", d.display());
        walk(&d, &mut on_disk);
    }
    // `noise_settings/` is shared, so restrict the comparison there to the four
    // files this phase owns.
    let mut ns = BTreeSet::new();
    walk(&assets.join("worldgen/noise_settings"), &mut ns);
    let shared: BTreeSet<String> = SHARED_NOISE_SETTINGS_EXTRAS
        .iter()
        .map(|s| format!("worldgen/noise_settings/{s}.json"))
        .collect();
    on_disk.extend(ns.difference(&shared).cloned());

    let unlisted: Vec<&String> = on_disk.difference(&listed).collect();
    let missing: Vec<&String> = listed.difference(&on_disk).collect();
    assert!(
        unlisted.is_empty() && missing.is_empty(),
        "bundled corpus and manifest disagree: {} unlisted on disk, {} listed \
         but absent. A gate that only re-hashes listed files cannot see an \
         addition, which is how a polluted extraction hides.\n  unlisted: \
         {:?}\n  missing: {:?}",
        unlisted.len(),
        missing.len(),
        &unlisted[..unlisted.len().min(8)],
        &missing[..missing.len().min(8)],
    );
}

#[test]
fn noise_settings_directory_holds_only_known_files() {
    let mut ns = BTreeSet::new();
    walk(&assets().join("worldgen/noise_settings"), &mut ns);
    let stems: BTreeSet<String> = ns
        .iter()
        .map(|p| {
            p.rsplit('/')
                .next()
                .expect("file name")
                .trim_end_matches(".json")
                .to_owned()
        })
        .collect();
    // This phase's four, plus whatever of the shared three has landed. Anything
    // else means a seventh dimension appeared and nobody claimed it.
    let known: BTreeSet<String> = ["amplified", "caves", "floating_islands", "large_biomes"]
        .into_iter()
        .chain(SHARED_NOISE_SETTINGS_EXTRAS.iter().copied())
        .map(str::to_owned)
        .collect();
    let unexpected: Vec<&String> = stems.difference(&known).collect();
    assert!(
        unexpected.is_empty(),
        "unrecognised noise_settings: {unexpected:?} (jar has exactly 7: \
         amplified, caves, end, floating_islands, large_biomes, nether, overworld)"
    );
    for own in ["amplified", "caves", "floating_islands", "large_biomes"] {
        assert!(stems.contains(own), "this phase's noise_settings/{own}.json is missing");
    }
}

// ---------------------------------------------------------------------------
// Drift direction 3: the enumeration
// ---------------------------------------------------------------------------

#[test]
fn corpus_counts_match_the_jar_enumeration() {
    let rows = rows();
    let mut got: BTreeMap<String, usize> = BTreeMap::new();
    for r in &rows {
        *got.entry(count_key(&r.path)).or_default() += 1;
    }
    let want: BTreeMap<String, usize> = EXPECTED_COUNTS
        .iter()
        .map(|(k, v)| ((*k).to_owned(), *v))
        .collect();
    assert_eq!(
        got, want,
        "per-registry counts differ from the jar enumeration measured on \
         2026-08-07"
    );
    assert_eq!(
        rows.len(),
        EXPECTED_COUNTS.iter().map(|(_, n)| n).sum::<usize>(),
        "row total"
    );
    assert_eq!(
        header_counts(),
        want,
        "the manifest's own `# counts` header disagrees with its rows"
    );
}

// ---------------------------------------------------------------------------
// Closure: is the corpus actually usable?
// ---------------------------------------------------------------------------

/// Every `*.json` under `assets/worldgen/<registry>/`, keyed by id without the
/// extension (`village/plains/town_centers`).
fn ids(registry: &str) -> BTreeSet<String> {
    let root = assets().join("worldgen").join(registry);
    let mut files = BTreeSet::new();
    walk(&root, &mut files);
    let prefix = format!("worldgen/{registry}/");
    files
        .iter()
        .filter_map(|p| p.strip_prefix(&prefix))
        .map(|p| p.trim_end_matches(".json").to_owned())
        .collect()
}

fn nbt_ids() -> BTreeSet<String> {
    rows()
        .into_iter()
        .filter_map(|r| r.path.strip_prefix("structure/").map(str::to_owned))
        .filter_map(|p| p.strip_suffix(".nbt").map(str::to_owned))
        .collect()
}

fn json(path: &Path) -> Value {
    let body = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&body).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Every string value in `doc` reached under one of `keys`.
fn refs_under(doc: &Value, keys: &[&str]) -> BTreeSet<String> {
    fn go(n: &Value, key: Option<&str>, keys: &[&str], out: &mut BTreeSet<String>) {
        match n {
            Value::Object(m) => {
                for (k, v) in m {
                    go(v, Some(k), keys, out);
                }
            }
            Value::Array(a) => {
                for v in a {
                    go(v, key, keys, out);
                }
            }
            Value::String(s) => {
                if key.is_some_and(|k| keys.contains(&k)) {
                    out.insert(s.clone());
                }
            }
            _ => {}
        }
    }
    let mut out = BTreeSet::new();
    go(doc, None, keys, &mut out);
    out
}

/// Like [`refs_under`] but keeps every *occurrence*, not the distinct set.
///
/// Load-bearing for the pool counts: `bastion/treasure/extensions/{large,small}_pool.json`
/// list `bastion/treasure/extensions/empty` 3 and 2 times respectively, each with
/// its own weight, so a pool holds 1134 element references across 1131 distinct
/// values. Deduplicating would silently weaken the resolution check and make the
/// predicted magnitude a different quantity than the jar census measured.
fn all_refs_under(doc: &Value, keys: &[&str]) -> Vec<String> {
    fn go(n: &Value, key: Option<&str>, keys: &[&str], out: &mut Vec<String>) {
        match n {
            Value::Object(m) => {
                for (k, v) in m {
                    go(v, Some(k), keys, out);
                }
            }
            Value::Array(a) => {
                for v in a {
                    go(v, key, keys, out);
                }
            }
            Value::String(s) => {
                if key.is_some_and(|k| keys.contains(&k)) {
                    out.push(s.clone());
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    go(doc, None, keys, &mut out);
    out
}

fn strip(id: &str) -> &str {
    id.strip_prefix("minecraft:").unwrap_or(id)
}

/// Absolute paths of every `*.json` in a bundled registry.
fn files_of(registry: &str) -> Vec<PathBuf> {
    let mut files = BTreeSet::new();
    walk(&assets().join("worldgen").join(registry), &mut files);
    files.into_iter().map(|p| assets().join(p)).collect()
}

#[test]
fn every_structure_set_reference_resolves_and_the_sets_cover_all_34_structures() {
    let structures = ids("structure");
    assert_eq!(structures.len(), 34, "bundled structures");
    let mut referenced = BTreeSet::new();
    let mut sets = 0usize;
    for p in files_of("structure_set") {
        sets += 1;
        for r in refs_under(&json(&p), &["structure"]) {
            referenced.insert(strip(&r).to_owned());
        }
    }
    assert_eq!(sets, 20, "bundled structure sets");
    let unresolved: Vec<&String> = referenced.difference(&structures).collect();
    assert!(unresolved.is_empty(), "structure_set references nothing bundled: {unresolved:?}");
    // The other direction: a structure no set places can never generate, so a
    // partial extraction of `structure_set/` shows up here rather than as a
    // silently absent structure at runtime.
    let unplaced: Vec<&String> = structures.difference(&referenced).collect();
    assert!(
        unplaced.is_empty(),
        "{} bundled structures are named by no structure_set: {unplaced:?}",
        unplaced.len()
    );
}

#[test]
fn every_structure_biome_filter_resolves_to_a_bundled_worldgen_tag() {
    let biome_tags = ids("tags/worldgen/biome");
    let mut checked = 0usize;
    for p in files_of("structure") {
        let doc = json(&p);
        let b = doc.get("biomes").unwrap_or_else(|| panic!("{}: no biomes", p.display()));
        let refs: Vec<String> = match b {
            Value::String(s) => vec![s.clone()],
            Value::Array(a) => a.iter().map(|v| v.as_str().expect("biome ref").to_owned()).collect(),
            other => panic!("{}: biomes is {other:?}", p.display()),
        };
        for r in refs {
            // All 34 are tag references in 26.2 — zero inline biome lists. That
            // is exactly why `tags/worldgen/` had to be bundled: without it no
            // structure's biome filter resolves and placement stays blocked.
            let tag = r
                .strip_prefix('#')
                .unwrap_or_else(|| panic!("{}: biomes {r} is not a tag", p.display()));
            assert!(
                biome_tags.contains(strip(tag)),
                "{}: biome filter {r} resolves to no bundled worldgen biome tag",
                p.display()
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 34, "one biome-tag reference per structure");
}

#[test]
fn every_template_pool_reference_resolves() {
    let pools = ids("template_pool");
    let processors = ids("processor_list");
    let templates = nbt_ids();
    assert_eq!(pools.len(), 188, "bundled template pools");
    assert_eq!(processors.len(), 40, "bundled processor lists");
    assert_eq!(templates.len(), 1212, "bundled NBT templates");

    let dangling: BTreeSet<&str> = VANILLA_DANGLING_TEMPLATES.iter().copied().collect();
    let mut bad = Vec::new();
    let mut n_loc = 0usize;
    let mut n_proc = 0usize;
    let mut n_fallback = 0usize;

    for p in files_of("template_pool") {
        let doc = json(&p);
        for r in all_refs_under(&doc, &["location"]) {
            n_loc += 1;
            let id = strip(&r);
            if !templates.contains(id) && !dangling.contains(id) {
                bad.push(format!("{}: location {r} -> no bundled NBT", p.display()));
            }
        }
        for r in all_refs_under(&doc, &["processors"]) {
            n_proc += 1;
            if !processors.contains(strip(&r)) {
                bad.push(format!("{}: processors {r} -> no bundled processor_list", p.display()));
            }
        }
        for r in all_refs_under(&doc, &["fallback"]) {
            n_fallback += 1;
            let id = strip(&r);
            // `minecraft:empty` is the terminal fallback and is a real pool file.
            if !pools.contains(id) {
                bad.push(format!("{}: fallback {r} -> no bundled template_pool", p.display()));
            }
        }
    }

    assert!(bad.is_empty(), "{} unresolved pool references:\n  {}", bad.len(), bad[..bad.len().min(12)].join("\n  "));
    // Magnitude, not sign: predict the reference counts so a pool tree that
    // parsed but yielded almost nothing cannot pass as "all resolved".
    assert_eq!(n_loc, 1134, "location references across all 188 pools");
    assert_eq!(n_proc, 757, "processors references");
    assert_eq!(n_fallback, 188, "fallback references (one per pool)");
}

#[test]
fn the_known_dangling_template_is_really_absent_and_really_referenced() {
    // The control for the whitelist above. A whitelist whose premise is false
    // is worse than no whitelist: it silently excuses a real gap.
    assert_eq!(VANILLA_DANGLING_TEMPLATES.len(), 1, "exactly one known vanilla dangling ref");
    let id = VANILLA_DANGLING_TEMPLATES[0];
    assert!(
        !nbt_ids().contains(id),
        "{id} now HAS an NBT file — drop it from VANILLA_DANGLING_TEMPLATES, the \
         whitelist is now excusing nothing"
    );
    // ... and it is genuinely referenced, so the entry is not dead weight.
    let referrer = assets().join("worldgen/template_pool/ancient_city/walls/no_corners.json");
    let refs = refs_under(&json(&referrer), &["location"]);
    assert!(
        refs.contains(&format!("minecraft:{id}")),
        "{} no longer references {id}", referrer.display()
    );
    // And the siblings that made this look like an off-by-one really do exist.
    let have = nbt_ids();
    for n in 1..=4 {
        let sib = format!("ancient_city/walls/intact_horizontal_wall_stairs_{n}");
        assert!(have.contains(&sib), "{sib} missing");
    }
}

#[test]
fn every_jigsaw_structure_start_pool_resolves() {
    let pools = ids("template_pool");
    let mut n = 0usize;
    for p in files_of("structure") {
        let doc = json(&p);
        for r in refs_under(&doc, &["start_pool"]) {
            assert!(
                pools.contains(strip(&r)),
                "{}: start_pool {r} -> no bundled template_pool",
                p.display()
            );
            n += 1;
        }
    }
    // 10 of the 34 are jigsaw-rooted (5 villages, ancient city, bastion,
    // trial chambers, trail ruins, pillager outpost).
    assert_eq!(n, 10, "jigsaw start_pool references");
}

#[test]
fn every_preset_reference_resolves() {
    let noise_settings = ids("noise_settings");
    let flat = ids("flat_level_generator_preset");
    let structure_set_tags = ids("tags/worldgen/structure");
    let structure_sets = ids("structure_set");
    assert_eq!(flat.len(), 9, "bundled flat presets");
    assert_eq!(structure_set_tags.len(), 20, "bundled structure_set tags");
    assert_eq!(structure_sets.len(), 20, "bundled structure sets");

    // `nether` and `end` belong to the concurrent Nether/End unit, not to this
    // phase, and this assertion was originally a bounded allowance for them. They
    // have since landed, so the allowance is gone and closure is required
    // outright — which is strictly stronger, and the reason the allowance was
    // written to report itself as deletable rather than to persist quietly.
    const NE_OWNED: [&str; 2] = ["nether", "end"];
    for id in NE_OWNED {
        assert!(
            noise_settings.contains(id),
            "noise_settings/{id}.json is absent. It is the Nether/End unit's file, \
             not this phase's, and the presets cannot close without it — see \
             docs/worldgen-structure-corpus.md's cross-unit boundary section."
        );
    }

    let mut n_settings = 0usize;
    let mut n_overrides = 0usize;
    for p in files_of("world_preset").into_iter().chain(files_of("flat_level_generator_preset")) {
        let doc = json(&p);
        for r in refs_under(&doc, &["settings"]) {
            n_settings += 1;
            let id = strip(&r);
            assert!(
                noise_settings.contains(id),
                "{}: settings {r} -> no bundled noise_settings",
                p.display()
            );
        }
        for r in all_refs_under(&doc, &["structure_overrides"]) {
            n_overrides += 1;
            // Measured, not assumed: all 20 override references in 26.2 name a
            // structure **set** directly (`minecraft:villages`) — none is a tag.
            // Resolving them against `tags/worldgen/structure/` looked right and
            // failed on the first file. The `#` arm is kept because the codec
            // accepts either form.
            if let Some(tag) = r.strip_prefix('#') {
                assert!(
                    structure_set_tags.contains(strip(tag)),
                    "{}: structure_overrides {r} -> no bundled structure tag",
                    p.display()
                );
            } else {
                assert!(
                    structure_sets.contains(strip(&r)),
                    "{}: structure_overrides {r} -> no bundled structure_set",
                    p.display()
                );
            }
        }
    }
    assert_eq!(n_settings, 16, "noise_settings references across the 16 presets");
    assert_eq!(n_overrides, 20, "structure_overrides references across the 16 presets");
    // The control that the NE files above are load-bearing here rather than a
    // vacuous precondition: the presets really do reference both, so removing
    // either one fails this test rather than passing unexercised.
    let referenced: BTreeSet<String> = files_of("world_preset")
        .into_iter()
        .chain(files_of("flat_level_generator_preset"))
        .flat_map(|p| refs_under(&json(&p), &["settings"]))
        .map(|r| strip(&r).to_owned())
        .collect();
    for id in NE_OWNED {
        assert!(referenced.contains(id), "no preset references noise_settings/{id}");
    }
}

// ---------------------------------------------------------------------------
// Regeneration gate (needs the jar, hence #[ignore]d)
// ---------------------------------------------------------------------------

/// Proves the committed manifest still traces to the jar, rather than to
/// whatever is currently in `assets/`.
///
/// `LODESTONE_REGEN=1 just regen-worldgen-structures` is the refresh path; this
/// test is the read-only check that no refresh is needed.
#[test]
#[ignore = "needs .cache/mc/26.2/versions/26.2/server-26.2.jar"]
fn manifest_matches_a_fresh_jar_extraction() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let jar = repo.join(".cache/mc/26.2/versions/26.2/server-26.2.jar");
    assert!(
        jar.is_file(),
        "jar not found at {} — the OUTER .cache/mc/26.2/server.jar is a bundler \
         and holds none of these paths",
        jar.display()
    );
    let scratch = std::env::temp_dir().join("lodestone-worldgen-structure-corpus-check");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).expect("scratch dir");
    let fresh_manifest = scratch.join("manifest.txt");

    let status = std::process::Command::new("python3")
        .arg(repo.join("scripts/extract-worldgen-structures.py"))
        .arg(&jar)
        .arg(scratch.join("assets"))
        .arg(&fresh_manifest)
        .status()
        .expect("running the extractor");
    assert!(status.success(), "extractor failed: {status}");

    let fresh = std::fs::read_to_string(&fresh_manifest).expect("fresh manifest");
    if fresh != MANIFEST {
        let committed = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/support/worldgen_structure_corpus.txt");
        assert!(
            std::env::var_os("LODESTONE_REGEN").is_none(),
            "LODESTONE_REGEN is set but this check does not write — run \
             `just regen-worldgen-structures`, which re-extracts assets and \
             manifest together"
        );
        panic!(
            "committed manifest ({}) differs from a fresh extraction of {}. \
             Run `just regen-worldgen-structures`.",
            committed.display(),
            jar.display()
        );
    }
    let _ = std::fs::remove_dir_all(&scratch);
}
