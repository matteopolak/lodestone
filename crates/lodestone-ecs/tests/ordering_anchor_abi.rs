//! The plugin ABI's ordering anchors, as an **enforceable** gate
//! rather than prose.
//!
//! # What this is for
//!
//! `docs/plugin-api.md`'s "how to change it" section states the policy
//! correctly: *"Adding a new ordering anchor is additive and safe; renaming or
//! removing one is a plugin-breaking change… Treat `TickSet`, `IngestSet`,
//! `FrameSet`, `ExtractSet` variants the way a public API treats enum
//! variants."* Nothing enforced it. A future change could rename a `TickSet`
//! variant in the same PR that adds a feature, with nothing failing except
//! (eventually) a plugin author's build, long after the rename shipped.
//!
//! This is a **drift gate**, in the same shape as `xtask`'s
//! `docs_index_matches_committed` and `lodestone-data`'s
//! `committed_table_matches_dump`: a committed snapshot of the anchor surface,
//! regenerated with `LODESTONE_REGEN=1`, that fails loudly on **any** change.
//!
//! Failing on additions too is deliberate, not over-reach: `docs/plugin-api.md`
//! §"Ordering-anchor changelog" asks that *every* PR touching one of these enums
//! add a changelog entry, additions included. A gate that only caught renames
//! would leave the changelog's own rule unenforced.
//!
//! ```text
//! LODESTONE_REGEN=1 cargo test -p lodestone-ecs --test ordering_anchor_abi
//! ```
//!
//! # What this gate is in scope for, and what it cannot see
//!
//! `CLAUDE.md`: *a gate that compares two things you control cannot tell you
//! that a third thing exists.* This one reads two source files and snapshots two
//! things:
//!
//! 1. The **variant lists** of the five public ordering-anchor enums in
//!    `sets.rs`, in declaration order.
//! 2. The **sequence of anchor mentions** in `plugin.rs`, in source order —
//!    which is where `CorePlugin` actually `chain()`s them, so a *reordering* of
//!    `TickSet::Physics` before `TickSet::Intent` is caught even though no
//!    variant name changed. The chain order is as much a part of the ABI as the
//!    names, and a variants-only snapshot would be blind to it.
//!
//! It is blind to:
//!
//! - **A sixth anchor enum in a new file.** The scanned set is the four
//!   documented anchors plus `EventPriority`, hardcoded below. A new `SystemSet` enum
//!   published from, say, `session.rs` (`SessionSet` already exists and is
//!   deliberately *not* in scope — it is not one of the four documented anchors)
//!   would be invisible. This is the docs-index-gate failure mode exactly: that
//!   gate scanned three directories and not `docs/plans/`. Adding an anchor enum
//!   means adding it to `ANCHOR_ENUMS` here, and nothing forces that.
//! - **Semantic meaning.** Renaming what a variant *does* while keeping its name
//!   passes. Only a name/order change is visible.
//! - **Whether the changelog was actually updated.** The gate makes the reviewer
//!   look; it cannot read `docs/plugin-api.md` and judge prose.
//! - **`#[non_exhaustive]`.** Tracked separately; see
//!   `docs/plugin-api.md`'s note on why its value there is narrower than a
//!   first read suggests.

use std::path::{Path, PathBuf};

/// The enums this gate is in scope for: the four ordering anchors
/// `docs/plugin-api.md` names, plus `EventPriority`, which that document's own
/// changelog calls "a fifth ordering-anchor *type*".
const ANCHOR_ENUMS: &[&str] = &[
    "EventPriority",
    "IngestSet",
    "TickSet",
    "FrameSet",
    "ExtractSet",
];

/// Guards against the parser silently reporting nothing. Each entry is the
/// variant count expected *today*; a mismatch means either a real ABI change
/// (regenerate and add a changelog entry) or a broken scanner.
///
/// This exists because `CLAUDE.md`'s rule is that an audit printing nothing is a
/// failure to run, never a pass. Without a floor, a scanner that stopped
/// matching would produce an empty snapshot, the snapshot would be regenerated
/// empty by the next person, and the gate would pass forever while measuring
/// nothing.
const EXPECTED_COUNTS: &[(&str, usize)] = &[
    ("EventPriority", 6),
    ("IngestSet", 3),
    ("TickSet", 6),
    ("FrameSet", 4),
    ("ExtractSet", 4),
];

/// Anchors that are **declared in `sets.rs` but never chained by
/// `CorePlugin::build`**, and are therefore published ordering anchors with no
/// ordering guarantee at all.
///
/// # This list is a bug report, not a design
///
/// Both entries were found by this gate on its first run, which is the whole
/// argument for snapshotting the *chain* and not only the variant names.
/// `docs/plugin-api.md`'s ordering-anchor changelog records `0d82ab4` as adding
/// `TickSet::Intent` "between `Input` and `Physics`" and `ExtractSet::Debug`
/// "between `Entities` and `Hud`". The **variants** landed and the changelog
/// entry landed; two of `CorePlugin::build`'s `configure_sets` calls (for
/// `GameTick` and `Extract`) were never updated to match.
///
/// The consequence is not cosmetic. A plugin writing
/// `.in_set(TickSet::Intent)` — which is exactly what
/// `crates/plugins/lodestone-autopilot` does, and what `TickSet::Intent`'s own
/// doc comment instructs — gets **no ordering relation to `TickSet::Physics`**,
/// so the intent it writes may be read before or after physics integrates,
/// nondeterministically. `ExtractSet::Debug` has the same problem against
/// `ExtractSet::Hud`.
///
/// `src/plugin.rs` is outside this test's file ownership, so the fix is handed
/// over rather than applied here. **When it lands, this list shrinks and
/// `every_declared_anchor_is_chained_or_a_known_gap` fails**, which is the
/// point: the gap is measured and named rather than sitting silently inside a
/// snapshot that looks authoritative.
const KNOWN_UNCHAINED: &[&str] = &[];

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn snapshot_path() -> PathBuf {
    crate_root().join("tests/support/ordering_anchor_abi.txt")
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// Pull `enum <name> { … }`'s variant identifiers out of Rust source.
///
/// Deliberately **not** a Rust lexer. `CLAUDE.md` records that a hand-rolled one
/// will be wrong about lifetimes — `&'static str` opening a char-literal flag
/// that never closed, silently disabling comment detection in three scanners.
/// So this does the least it can: find the `pub enum <name> {` header, then walk
/// forward tracking brace depth, and take a variant only from a line that is a
/// bare identifier followed by a comma at depth 1. Doc comments (`///`),
/// attributes (`#[…]`) and blank lines are skipped explicitly. These five enums
/// are plain C-like enums with no payloads, so nothing more is needed — and if
/// one ever grows a payload, `EXPECTED_COUNTS` fails rather than the scanner
/// quietly under-reporting.
fn enum_variants(source: &str, name: &str) -> Vec<String> {
    let header = format!("pub enum {name} {{");
    let Some(start) = source.find(&header) else {
        return Vec::new();
    };
    let body = &source[start + header.len()..];
    let mut depth = 1usize;
    let mut variants = Vec::new();
    for raw in body.lines() {
        let line = raw.trim();
        depth += line.matches('{').count();
        depth = depth.saturating_sub(line.matches('}').count());
        if depth == 0 {
            break;
        }
        if line.is_empty() || line.starts_with("///") || line.starts_with("//") {
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        let Some(ident) = line.strip_suffix(',') else {
            continue;
        };
        if depth == 1
            && !ident.is_empty()
            && ident
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
            && ident.starts_with(|c: char| c.is_ascii_uppercase())
        {
            variants.push(ident.to_owned());
        }
    }
    variants
}

/// Every `Enum::Variant` mention of an anchor enum in `source`, in source order.
///
/// This is the chain-order half. `CorePlugin::build` is where the anchors are
/// actually `chain()`ed, so the *sequence* of mentions there is the observable
/// ordering a plugin depends on. Substring scanning rather than parsing, because
/// the question is only "in what order are these names named".
fn anchor_mentions(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = source.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let rest = &source[i..];
        let mut matched = None;
        for enum_name in ANCHOR_ENUMS {
            let needle = format!("{enum_name}::");
            if rest.starts_with(&needle) {
                let tail = &rest[needle.len()..];
                let end = tail
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .unwrap_or(tail.len());
                if end > 0 {
                    matched = Some((needle.len() + end, format!("{enum_name}::{}", &tail[..end])));
                }
                break;
            }
        }
        match matched {
            Some((len, mention)) => {
                out.push(mention);
                i += len;
            }
            None => {
                // Advance one *character*, not one byte, so a multi-byte char
                // (these files are full of em dashes) cannot slice mid-scalar.
                i += rest.chars().next().map_or(1, char::len_utf8);
            }
        }
    }
    out
}

/// Render the whole anchor surface as the text that gets committed.
fn generate() -> String {
    let sets = read(&crate_root().join("src/sets.rs"));
    let plugin = read(&crate_root().join("src/plugin.rs"));

    let mut out = String::new();
    out.push_str(
        "# The plugin ABI's ordering anchors. GENERATED -- do not hand-edit.\n\
         #\n\
         # Regenerate with:\n\
         #   LODESTONE_REGEN=1 cargo test -p lodestone-ecs --test ordering_anchor_abi\n\
         #\n\
         # If this file changed, you changed the plugin ABI. Add an entry to\n\
         # docs/plugin-api.md's \"Ordering-anchor changelog\" section in the same\n\
         # commit -- additions included (that section asks for every PR, not only\n\
         # renames). A rename or removal also needs a deprecation window.\n\
         #\n\
         # See crates/lodestone-ecs/tests/ordering_anchor_abi.rs for what this\n\
         # gate cannot see.\n\n",
    );

    out.push_str("[variants] src/sets.rs, declaration order\n");
    for name in ANCHOR_ENUMS {
        let variants = enum_variants(&sets, name);
        out.push_str(&format!("{name} = {}\n", variants.join(", ")));
    }

    out.push_str("\n[chain] src/plugin.rs, source order of anchor mentions\n");
    for (n, mention) in anchor_mentions(&plugin).iter().enumerate() {
        out.push_str(&format!("{n:03} {mention}\n"));
    }
    out
}

/// The gate. Fails when the committed snapshot and the source disagree.
#[test]
fn ordering_anchor_abi_matches_committed() {
    let generated = generate();
    let path = snapshot_path();

    if std::env::var_os("LODESTONE_REGEN").is_some() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("creating tests/support");
        }
        std::fs::write(&path, &generated).expect("writing the snapshot");
        eprintln!("regenerated {}", path.display());
        return;
    }

    let committed = read(&path);
    assert_eq!(
        generated,
        committed,
        "\n\nThe plugin ABI's ordering anchors changed.\n\n\
         If that was intentional: add an entry to docs/plugin-api.md's \
         \"Ordering-anchor changelog\" section, then regenerate with\n  \
         LODESTONE_REGEN=1 cargo test -p lodestone-ecs --test ordering_anchor_abi\n\n\
         Adding a variant is additive and safe. RENAMING OR REMOVING one is a \
         plugin-breaking change and needs a deprecation window \
         (docs/plugin-api.md, \"how to change it\").\n"
    );
}

/// **The floor**, so the gate cannot pass by measuring nothing.
///
/// `CLAUDE.md`: treat an audit that prints nothing as a failure to run, never as
/// a pass. Without this, a scanner that stopped matching would emit an empty
/// snapshot, the next person would regenerate it empty, and the drift gate would
/// pass forever while watching nothing at all.
#[test]
fn every_anchor_enum_is_actually_found_with_the_expected_variant_count() {
    let sets = read(&crate_root().join("src/sets.rs"));
    for (name, expected) in EXPECTED_COUNTS {
        let variants = enum_variants(&sets, name);
        assert_eq!(
            variants.len(),
            *expected,
            "{name} should have {expected} variants, found {} ({variants:?}). \
             Either the ABI changed (regenerate, and add a changelog entry) or \
             the scanner in this file is broken -- and a broken scanner must fail \
             here rather than silently emitting an empty snapshot.",
            variants.len()
        );
    }
}

/// **The scanner's control.** The floor above proves the scanner finds *these*
/// enums; this proves it is discriminating rather than returning whatever it is
/// asked for. It parses a synthetic enum containing exactly the shapes that
/// could fool a naive line scanner — a doc comment mentioning a fake variant, an
/// attribute, a nested braced item, and a lowercase field-like line — and
/// requires the three real variants and nothing else.
#[test]
fn the_variant_scanner_ignores_doc_comments_attributes_and_nested_braces() {
    let source = r#"
/// Not a variant: Decoy,
#[derive(SystemSet)]
pub enum Probe {
    /// Also not a variant: Ghost,
    Alpha,
    #[doc = "Phantom,"]
    Beta,
    // a line comment: Spectre,
    Gamma,
}

pub enum Unrelated {
    Delta,
}
"#;
    assert_eq!(
        enum_variants(source, "Probe"),
        vec!["Alpha", "Beta", "Gamma"],
        "doc comments, attributes and comments must not become variants"
    );
    assert_eq!(
        enum_variants(source, "Unrelated"),
        vec!["Delta"],
        "the scanner must stop at the first enum's closing brace"
    );
    assert!(
        enum_variants(source, "Missing").is_empty(),
        "an absent enum yields nothing -- which is why EXPECTED_COUNTS exists"
    );
}

/// **The chain scanner's control**, and the reason this gate looks at
/// `plugin.rs` at all: a *reordering* with no renames must be visible.
///
/// Two synthetic chains differing only in order must produce different
/// snapshots. Without this, the chain half could be emitting a sorted or
/// deduplicated list and a swap of `TickSet::Physics` and `TickSet::Intent` —
/// which moves the sprint window by a tick, per `TickSet::Intent`'s own doc —
/// would slip through.
#[test]
fn the_chain_scanner_distinguishes_a_reordering_from_the_original() {
    let before = "(TickSet::Input, TickSet::Intent, TickSet::Physics).chain()";
    let after = "(TickSet::Input, TickSet::Physics, TickSet::Intent).chain()";
    let a = anchor_mentions(before);
    let b = anchor_mentions(after);
    assert_eq!(a, vec!["TickSet::Input", "TickSet::Intent", "TickSet::Physics"]);
    assert_ne!(a, b, "a reordering must change the snapshot, or the gate is blind to it");
}

/// Every declared anchor is either chained by `CorePlugin` or a **named** gap.
///
/// This is the assertion the variant-only version of this gate could not make,
/// and it found two real defects on its first run — see [`KNOWN_UNCHAINED`].
///
/// It is written as "chained, or on the known list" rather than "chained" so
/// that `main` stays green while the fix is brokered, and so that **fixing it
/// fails this test** with an instruction to shrink the list. A silent snapshot of
/// the broken state would have looked authoritative and taught nobody anything.
#[test]
fn every_declared_anchor_is_chained_or_a_known_gap() {
    let sets = read(&crate_root().join("src/sets.rs"));
    let plugin = read(&crate_root().join("src/plugin.rs"));
    let chained = anchor_mentions(&plugin);

    let mut unchained = Vec::new();
    for name in ANCHOR_ENUMS {
        for variant in enum_variants(&sets, name) {
            let path = format!("{name}::{variant}");
            if !chained.contains(&path) {
                unchained.push(path);
            }
        }
    }

    let mut expected: Vec<String> = KNOWN_UNCHAINED.iter().map(|s| (*s).to_owned()).collect();
    expected.sort();
    unchained.sort();
    assert_eq!(
        unchained, expected,
        "\n\nThe set of declared-but-unchained ordering anchors changed.\n\n\
         If an anchor is now chained in CorePlugin (thank you), remove it from \
         KNOWN_UNCHAINED in this file.\n\
         If a NEW anchor is unchained, that is the defect this list documents: a \
         published anchor with no ordering guarantee. Chain it in \
         CorePlugin::build's configure_sets call for its schedule.\n"
    );
}

/// The real `plugin.rs` genuinely contains a chain — the *world*-species check.
/// If `CorePlugin` stopped configuring the anchors (moved to another file, say),
/// the chain half would silently snapshot an empty list and keep passing.
#[test]
fn the_real_core_plugin_still_configures_every_anchor_enum() {
    let plugin = read(&crate_root().join("src/plugin.rs"));
    let mentions = anchor_mentions(&plugin);
    assert!(
        mentions.len() >= 20,
        "expected CorePlugin to name many anchors, found {} ({mentions:?})",
        mentions.len()
    );
    for name in ANCHOR_ENUMS {
        assert!(
            mentions.iter().any(|m| m.starts_with(&format!("{name}::"))),
            "{name} is an ordering anchor but CorePlugin never names it -- either it \
             moved (point this gate at the new file) or it is no longer configured"
        );
    }
}
