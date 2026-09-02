//! `cargo xtask protocol-dup` — the four duplication measurements from
//! `docs/plans/multi-version-protocol-dedup.md`'s "Duplication, four ways"
//! section, plus its minecraft-data adjacency table, landed as a re-runnable
//! instrument instead of four hand-typed scripts whose numbers rot the
//! moment anyone touches `crates/versions/`.
//!
//! # Why `syn`, not a hand-rolled scanner
//!
//! Three scanners in this repo were once hand-rolled Rust lexers, and all
//! three were wrong about lifetimes: `&'static str` opens what looks like an
//! unterminated char literal, silently disabling whatever came after. This
//! module parses every file with `syn::parse_file` and walks the real AST
//! with `syn::visit::Visit`, the same trade `xtask::islands` and
//! `xtask::ptr_const` already made and for the same reason.
//!
//! # Scope of each measurement — read this before trusting a number
//!
//! - **File similarity** compares every `.rs` file that exists at the same
//!   relative path in both families of an adjacent pair (`src/` and
//!   `tests/`, `generated/` included — this one is about raw text, not
//!   hand-written cost), via a line-level LCS (the same quantity `diff -u`
//!   reports as "unchanged").
//! - **Struct/enum identity** is scoped to `src/packets/` only — the
//!   packet type definitions — with whitespace and attributes (including
//!   doc comments and derives) stripped by tokenizing the item with the
//!   leading attributes cleared, rather than by a text-based comment
//!   strip. `#[cfg(test)]` modules are excluded so a test fixture struct
//!   never inflates a "packet type" count.
//! - **Dispatch arms** are scoped to the `handle_play` method of
//!   `src/adapter.rs` only, in 1.8/1.9/1.14 — 26.2 is a directory module
//!   with a structurally different dispatch (see the plan doc's "n/a" for
//!   that pair), so it is not part of this measurement at all.
//! - **Function identity** is scoped to **free functions only**
//!   (`syn::ItemFn`), deliberately excluding `impl` methods. A by-name
//!   comparison of impl methods would collide every unrelated `fn new`,
//!   `fn encode` and `fn decode` across dozens of unrelated structs into
//!   one bucket — the collision is silent (the map just keeps whichever
//!   occurrence was visited first) and would make the measurement worse
//!   than useless rather than merely incomplete. Free helper functions
//!   (`begin_login`, `chat_kind`, the `player_info` readers, …) do not
//!   have this problem, which is presumably why the plan's own worked
//!   examples are all free functions. `#[cfg(test)]` modules are excluded
//!   here too, for the same reason as the struct scan.
//!
//! Every count this module produces is a measurement of the working tree at
//! the moment it runs, not a citation of the plan document's own numbers —
//! CLAUDE.md's standard applies here just as it does to the plan: re-run
//! before quoting, and report a material disagreement rather than
//! adjusting either side to match.

use anyhow::{Context, Result, anyhow, bail};
use quote::ToTokens;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use syn::spanned::Spanned;
use syn::visit::Visit;

use crate::islands::has_cfg_test;

/// Families this command measures, oldest to newest. Matches
/// `crates/versions/`'s current folder names (era-start Minecraft versions,
/// not protocol numbers), not the plan's proposed post-migration names —
/// this instrument reports what is on disk today.
const FAMILIES: [&str; 4] = ["1.8", "1.9", "1.14", "26.2"];

/// Adjacent-family pairs: the ones a real wire-era migration would merge.
const ADJACENT_PAIRS: [(&str, &str); 3] = [("1.8", "1.9"), ("1.9", "1.14"), ("1.14", "26.2")];

/// The four pairs the packet struct/enum identity table reports.
const STRUCT_PAIRS: [(&str, &str); 4] = [
    ("1.8", "1.9"),
    ("1.9", "1.14"),
    ("1.8", "1.14"),
    ("1.14", "26.2"),
];

/// `handle_play` dispatch-arm comparison only makes sense where the family
/// still has an if-chain `handle_play` in a single `adapter.rs` — 26.2 (the
/// former v770) does not (directory module, data shape differs entirely).
const ARM_PAIRS: [(&str, &str); 3] = [("1.8", "1.9"), ("1.9", "1.14"), ("1.8", "1.14")];

/// The three legacy families the function-identity "identical in all three"
/// bucket is computed over.
const LEGACY_TRIO: [&str; 3] = ["1.8", "1.9", "1.14"];

/// The fifteen minecraft-data-covered target versions from the plan's wire
/// table, oldest first. 26.2 is deliberately absent: minecraft-data has no
/// entry for it, and the plan records that as "unmeasured" rather than
/// guessing.
const ADJACENCY_VERSIONS: [&str; 15] = [
    "1.7.10", "1.8.9", "1.9.4", "1.10.2", "1.11.2", "1.12.2", "1.13.2", "1.14.4", "1.15.2",
    "1.16.5", "1.17.1", "1.18.2", "1.19.4", "1.20.6", "1.21.11",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileSimilarityRow {
    pub relative_path: String,
    /// `(unchanged_lines, larger_file_lines)` per adjacent pair, in
    /// `ADJACENT_PAIRS` order. `None` when the path does not exist in both
    /// families of that pair.
    pub per_pair: [Option<(usize, usize)>; 3],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StructIdentityRow {
    pub family_a: String,
    pub family_b: String,
    pub same_named: usize,
    pub identical_body: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArmRow {
    pub family_a: String,
    pub family_b: String,
    pub arms_a: usize,
    pub arms_b: usize,
    pub arm_lines_a: usize,
    pub common_names: usize,
    pub identical: (usize, usize),
    pub ge_085: (usize, usize),
    pub ge_060: (usize, usize),
    pub lt_060: (usize, usize),
}

impl ArmRow {
    fn reusable_share(&self) -> Option<f64> {
        if self.arm_lines_a == 0 {
            return None;
        }
        Some(
            (self.identical.1 + self.ge_085.1) as f64 / self.arm_lines_a as f64 * 100.0,
        )
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FunctionIdentityReport {
    pub triple_identical_count: usize,
    pub triple_identical_lines: usize,
    /// `(family_a, family_b, matched_lines, total_lines_a)` for `src/`,
    /// one row per adjacent pair.
    pub src_near_dup: Vec<(String, String, usize, usize)>,
    /// Same shape, for `tests/`.
    pub test_near_dup: Vec<(String, String, usize, usize)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdjacencyRow {
    pub version: String,
    pub packets: usize,
    /// `None` for the first version in the series (nothing to compare
    /// against).
    pub same_as_previous: Option<usize>,
    pub new: usize,
    pub gone: usize,
}

pub struct ProtocolDupReport {
    pub struct_totals: BTreeMap<String, usize>,
    pub file_rows: Vec<FileSimilarityRow>,
    pub struct_rows: Vec<StructIdentityRow>,
    pub arm_rows: Vec<ArmRow>,
    pub function_report: FunctionIdentityReport,
    pub adjacency_rows: Vec<AdjacencyRow>,
    pub adjacency_note: Option<String>,
}

pub fn protocol_dup_report(workspace_root: &Path) -> Result<ProtocolDupReport> {
    let protocol_root = workspace_root.join("crates/versions");

    let struct_totals = struct_totals(&protocol_root)?;
    let file_rows = file_similarity_rows(&protocol_root)?;
    let struct_rows = struct_identity_rows(&protocol_root)?;
    let arm_rows = arm_rows(&protocol_root)?;
    let function_report = function_identity_report(&protocol_root)?;
    let (adjacency_rows, adjacency_note) = minecraft_data_adjacency(workspace_root)?;

    Ok(ProtocolDupReport {
        struct_totals,
        file_rows,
        struct_rows,
        arm_rows,
        function_report,
        adjacency_rows,
        adjacency_note,
    })
}

impl ProtocolDupReport {
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "protocol-dup: duplication across crates/versions/{{1.8,1.9,1.14,26.2}}\n\
             (docs/plans/multi-version-protocol-dedup.md, \"Duplication, four ways\")\n"
        );

        let _ = writeln!(out, "1. whole-file line similarity (unchanged/larger, diff -u sense; src/ + tests/, generated/ included)");
        let _ = writeln!(
            out,
            "{:<40} {:>18} {:>18} {:>18}",
            "path", "1.8->1.9", "1.9->1.14", "1.14->26.2"
        );
        for row in &self.file_rows {
            let cell = |value: &Option<(usize, usize)>| match value {
                Some((unchanged, larger)) => format!("{unchanged}/{larger}"),
                None => "n/a".to_owned(),
            };
            let _ = writeln!(
                out,
                "{:<40} {:>18} {:>18} {:>18}",
                row.relative_path,
                cell(&row.per_pair[0]),
                cell(&row.per_pair[1]),
                cell(&row.per_pair[2]),
            );
        }

        let _ = writeln!(
            out,
            "\n2. packet struct/enum identity under src/packets/ (same name, attrs+whitespace stripped body)"
        );
        let _ = write!(out, "totals:");
        for family in FAMILIES {
            let count = self.struct_totals.get(family).copied().unwrap_or(0);
            let _ = write!(out, " {family}={count}");
        }
        out.push('\n');
        let _ = writeln!(
            out,
            "{:<18} {:>12} {:>16}",
            "pair", "same-named", "identical-body"
        );
        for row in &self.struct_rows {
            let _ = writeln!(
                out,
                "{:<18} {:>12} {:>16}",
                format!("{}/{}", row.family_a, row.family_b),
                row.same_named,
                row.identical_body,
            );
        }

        let _ = writeln!(
            out,
            "\n3. handle_play dispatch-arm token similarity (26.2 excluded: directory module, not an if-chain)"
        );
        let _ = writeln!(
            out,
            "{:<14} {:>10} {:>10} {:>8} {:>14} {:>10} {:>10} {:>10} {:>10}",
            "pair", "arms_a", "arms_b", "common", "identical", ">=0.85", ">=0.60", "<0.60", "reusable"
        );
        for row in &self.arm_rows {
            let _ = writeln!(
                out,
                "{:<14} {:>10} {:>10} {:>8} {:>7}({:>4}) {:>4}({:>4}) {:>4}({:>4}) {:>4}({:>4}) {:>9}",
                format!("{}/{}", row.family_a, row.family_b),
                row.arms_a,
                row.arms_b,
                row.common_names,
                row.identical.0,
                row.identical.1,
                row.ge_085.0,
                row.ge_085.1,
                row.ge_060.0,
                row.ge_060.1,
                row.lt_060.0,
                row.lt_060.1,
                row.reusable_share()
                    .map(|pct| format!("{pct:.0}%"))
                    .unwrap_or_else(|| "n/a".to_owned()),
            );
        }
        let _ = writeln!(
            out,
            "  (\"reusable\" = (identical-lines + >=0.85-lines) / arms_a's own total arm-lines)"
        );

        let _ = writeln!(
            out,
            "\n4. free functions under src/ (excl. generated/, excl. #[cfg(test)]), bodies normalised"
        );
        let _ = writeln!(
            out,
            "  identical across all three legacy families (1.8/1.9/1.14): {} functions / {} lines",
            self.function_report.triple_identical_count, self.function_report.triple_identical_lines
        );
        let _ = writeln!(out, "  near-duplicate (>=0.85) share of function-body lines:");
        let _ = writeln!(out, "    src:");
        for (a, b, matched, total) in &self.function_report.src_near_dup {
            let _ = writeln!(
                out,
                "      {a}/{b}: {}% ({matched} of {total} lines)",
                percent_f(*matched, *total)
            );
        }
        let _ = writeln!(out, "    tests:");
        for (a, b, matched, total) in &self.function_report.test_near_dup {
            let _ = writeln!(
                out,
                "      {a}/{b}: {}% ({matched} of {total} lines)",
                percent_f(*matched, *total)
            );
        }

        let _ = writeln!(
            out,
            "\n5. minecraft-data packet-shape adjacency (types inlined recursively; cycle-guarded)"
        );
        if let Some(note) = &self.adjacency_note {
            let _ = writeln!(out, "  note: {note}");
        }
        let _ = writeln!(
            out,
            "{:<10} {:>8} {:>10} {:>6} {:>6} {:>10}",
            "target", "packets", "same-prev", "new", "gone", "identical"
        );
        for row in &self.adjacency_rows {
            match row.same_as_previous {
                None => {
                    let _ = writeln!(
                        out,
                        "{:<10} {:>8} {:>10} {:>6} {:>6} {:>10}",
                        row.version, row.packets, "-", "-", "-", "-"
                    );
                }
                Some(same) => {
                    let _ = writeln!(
                        out,
                        "{:<10} {:>8} {:>10} {:>6} {:>6} {:>9}%",
                        row.version,
                        row.packets,
                        same,
                        row.new,
                        row.gone,
                        percent_f(same, row.packets)
                    );
                }
            }
        }
        let _ = writeln!(out, "26.2       unmeasured (no minecraft-data entry)");

        out
    }
}

fn percent_f(numerator: usize, denominator: usize) -> String {
    if denominator == 0 {
        "n/a".to_owned()
    } else {
        format!("{:.0}", numerator as f64 / denominator as f64 * 100.0)
    }
}

// ---------------------------------------------------------------------
// Shared: file walking, LCS, token similarity
// ---------------------------------------------------------------------

fn collect_rs_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    if !root.is_dir() {
        return Ok(files);
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).with_context(|| format!("read {}", dir.display()))? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

fn contains_generated_component(relative: &Path) -> bool {
    relative.components().any(|component| {
        matches!(component, std::path::Component::Normal(name) if name == "generated")
    })
}

/// Longest-common-subsequence length between two token/line sequences, via
/// the standard two-row DP (`O(n*m)` time, `O(min(n,m))` space).
fn lcs_len(a: &[&str], b: &[&str]) -> usize {
    let (n, m) = (a.len(), b.len());
    if n == 0 || m == 0 {
        return 0;
    }
    let mut prev = vec![0usize; m + 1];
    let mut curr = vec![0usize; m + 1];
    for i in 1..=n {
        for j in 1..=m {
            curr[j] = if a[i - 1] == b[j - 1] {
                prev[j - 1] + 1
            } else {
                prev[j].max(curr[j - 1])
            };
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[m]
}

/// Ratcliff/Obershelp-style ratio, approximated as `2*LCS/(len_a+len_b)` on
/// whitespace-split tokens of two already-comment-and-attribute-stripped
/// normalised strings.
fn token_similarity_ratio(a: &str, b: &str) -> f64 {
    let tokens_a: Vec<&str> = a.split_whitespace().collect();
    let tokens_b: Vec<&str> = b.split_whitespace().collect();
    if tokens_a.is_empty() && tokens_b.is_empty() {
        return 1.0;
    }
    let lcs = lcs_len(&tokens_a, &tokens_b);
    (2 * lcs) as f64 / (tokens_a.len() + tokens_b.len()) as f64
}

// ---------------------------------------------------------------------
// 1. Whole-file line similarity
// ---------------------------------------------------------------------

fn file_similarity_rows(protocol_root: &Path) -> Result<Vec<FileSimilarityRow>> {
    let mut per_path: BTreeMap<String, [Option<(usize, usize)>; 3]> = BTreeMap::new();

    for (pair_index, (family_a, family_b)) in ADJACENT_PAIRS.iter().enumerate() {
        let root_a = protocol_root.join(family_a);
        let root_b = protocol_root.join(family_b);
        if !root_a.is_dir() || !root_b.is_dir() {
            continue;
        }
        let files_a = relative_rs_files(&root_a)?;
        let files_b: BTreeSet<String> = relative_rs_files(&root_b)?.into_iter().collect();

        for relative in files_a {
            if !files_b.contains(&relative) {
                continue;
            }
            let content_a = std::fs::read_to_string(root_a.join(&relative))
                .with_context(|| format!("read {relative} under {}", root_a.display()))?;
            let content_b = std::fs::read_to_string(root_b.join(&relative))
                .with_context(|| format!("read {relative} under {}", root_b.display()))?;
            let lines_a: Vec<&str> = content_a.lines().collect();
            let lines_b: Vec<&str> = content_b.lines().collect();
            let unchanged = lcs_len(&lines_a, &lines_b);
            let larger = lines_a.len().max(lines_b.len());

            let entry = per_path.entry(relative).or_insert([None, None, None]);
            entry[pair_index] = Some((unchanged, larger));
        }
    }

    Ok(per_path
        .into_iter()
        .map(|(relative_path, per_pair)| FileSimilarityRow {
            relative_path,
            per_pair,
        })
        .collect())
}

/// `.rs` files under `<family_root>/src` and `<family_root>/tests`, keyed by
/// path relative to `family_root` (e.g. `src/adapter.rs`,
/// `tests/join_flow.rs`), so the same key names the same logical file
/// across two families whose crate layout otherwise matches.
fn relative_rs_files(family_root: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for subdir in ["src", "tests"] {
        let root = family_root.join(subdir);
        for path in collect_rs_files(&root)? {
            let relative = path
                .strip_prefix(family_root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            out.push(relative);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------
// 2. Packet struct/enum identity under src/packets/
// ---------------------------------------------------------------------

fn struct_totals(protocol_root: &Path) -> Result<BTreeMap<String, usize>> {
    let mut totals = BTreeMap::new();
    for family in FAMILIES {
        let items = packet_struct_items(protocol_root, family)?;
        totals.insert(family.to_owned(), items.len());
    }
    Ok(totals)
}

fn struct_identity_rows(protocol_root: &Path) -> Result<Vec<StructIdentityRow>> {
    let mut cache: BTreeMap<&str, BTreeMap<String, String>> = BTreeMap::new();
    for family in FAMILIES {
        cache.insert(family, packet_struct_items(protocol_root, family)?);
    }

    let mut rows = Vec::new();
    for (family_a, family_b) in STRUCT_PAIRS {
        let items_a = cache.get(family_a).cloned().unwrap_or_default();
        let items_b = cache.get(family_b).cloned().unwrap_or_default();
        let mut same_named = 0;
        let mut identical_body = 0;
        for (name, body_a) in &items_a {
            if let Some(body_b) = items_b.get(name) {
                same_named += 1;
                if body_a == body_b {
                    identical_body += 1;
                }
            }
        }
        rows.push(StructIdentityRow {
            family_a: family_a.to_owned(),
            family_b: family_b.to_owned(),
            same_named,
            identical_body,
        });
    }
    Ok(rows)
}

fn packet_struct_items(protocol_root: &Path, family: &str) -> Result<BTreeMap<String, String>> {
    let packets_root = protocol_root.join(family).join("src/packets");
    let mut items = BTreeMap::new();
    for path in collect_rs_files(&packets_root)? {
        let content =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let file = syn::parse_file(&content)
            .with_context(|| format!("parse {} as Rust", path.display()))?;
        let mut visitor = StructEnumVisitor::default();
        visitor.visit_file(&file);
        for (name, body) in visitor.items {
            items.entry(name).or_insert(body);
        }
    }
    Ok(items)
}

#[derive(Default)]
struct StructEnumVisitor {
    items: BTreeMap<String, String>,
}

impl<'ast> Visit<'ast> for StructEnumVisitor {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if has_cfg_test(&node.attrs) {
            return;
        }
        syn::visit::visit_item_mod(self, node);
    }

    fn visit_item_struct(&mut self, node: &'ast syn::ItemStruct) {
        let mut clone = node.clone();
        clone.attrs.clear();
        self.items
            .insert(clone.ident.to_string(), clone.to_token_stream().to_string());
    }

    fn visit_item_enum(&mut self, node: &'ast syn::ItemEnum) {
        let mut clone = node.clone();
        clone.attrs.clear();
        self.items
            .insert(clone.ident.to_string(), clone.to_token_stream().to_string());
    }
}

// ---------------------------------------------------------------------
// 3. handle_play dispatch-arm token similarity
// ---------------------------------------------------------------------

struct DispatchArm {
    line_count: usize,
    normalized: String,
}

fn handle_play_arms(protocol_root: &Path, family: &str) -> Result<BTreeMap<String, DispatchArm>> {
    let adapter_path = protocol_root.join(family).join("src/adapter.rs");
    let mut arms = BTreeMap::new();
    if !adapter_path.is_file() {
        return Ok(arms);
    }
    let content = std::fs::read_to_string(&adapter_path)
        .with_context(|| format!("read {}", adapter_path.display()))?;
    let file = syn::parse_file(&content)
        .with_context(|| format!("parse {} as Rust", adapter_path.display()))?;

    let mut finder = HandlePlayFinder::default();
    finder.visit_file(&file);
    let Some(block) = finder.block else {
        return Ok(arms);
    };

    let mut visitor = ArmVisitor::default();
    visitor.visit_block(&block);
    for arm in visitor.arms {
        arms.entry(arm.0).or_insert(DispatchArm {
            line_count: arm.1,
            normalized: arm.2,
        });
    }
    if arms.is_empty() {
        arms = dispatch_table_arms(&file);
    }
    Ok(arms)
}

/// The per-packet handler functions a family's `dispatch::Table` points at,
/// used when `handle_play` has no `if packet_id ==` arms left to find.
///
/// The legacy families replaced their if-chains with data-driven tables, which
/// is what the terminal `_ =>` island factory deserved — but it left this
/// measurement counting zero arms for every converted family, and a duplication
/// report that silently measures nothing is worse than one that is merely
/// wrong. The handler *bodies* are the same code the arms used to hold, so
/// they are the honest successor unit.
///
/// Keyed by the packet name recovered from the handler's own identifier, since
/// the families differ in prefix (`play_map_chunk` versus
/// `handle_play_map_chunk`) and the shared suffix is what makes two families'
/// handlers comparable.
fn dispatch_table_arms(file: &syn::File) -> BTreeMap<String, DispatchArm> {
    let mut visitor = HandlerFnVisitor::default();
    visitor.visit_file(file);
    let mut arms = BTreeMap::new();
    for (name, line_count, normalized) in visitor.handlers {
        arms.entry(name).or_insert(DispatchArm {
            line_count,
            normalized,
        });
    }
    arms
}

#[derive(Default)]
struct HandlerFnVisitor {
    handlers: Vec<(String, usize, String)>,
}

impl<'ast> Visit<'ast> for HandlerFnVisitor {
    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        let ident = node.sig.ident.to_string();
        let packet = ident
            .strip_prefix("handle_play_")
            .or_else(|| ident.strip_prefix("play_"));
        if let Some(packet) = packet {
            let span = node.block.span();
            let line_count = span.end().line.saturating_sub(span.start().line) + 1;
            self.handlers.push((
                packet.to_ascii_uppercase(),
                line_count,
                node.block.to_token_stream().to_string(),
            ));
        }
        syn::visit::visit_impl_item_fn(self, node);
    }
}

#[derive(Default)]
struct HandlePlayFinder {
    block: Option<syn::Block>,
}

impl<'ast> Visit<'ast> for HandlePlayFinder {
    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        if node.sig.ident == "handle_play" {
            self.block = Some(node.block.clone());
        }
        syn::visit::visit_impl_item_fn(self, node);
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        if node.sig.ident == "handle_play" {
            self.block = Some((*node.block).clone());
        }
        syn::visit::visit_item_fn(self, node);
    }
}

#[derive(Default)]
struct ArmVisitor {
    arms: Vec<(String, usize, String)>,
}

impl<'ast> Visit<'ast> for ArmVisitor {
    fn visit_expr_if(&mut self, node: &'ast syn::ExprIf) {
        if let Some(name) = packet_id_eq_name(node.cond.as_ref()) {
            let span = node.then_branch.span();
            let line_count = span.end().line.saturating_sub(span.start().line) + 1;
            let normalized = node.then_branch.to_token_stream().to_string();
            self.arms.push((name, line_count, normalized));
        }
        syn::visit::visit_expr_if(self, node);
    }
}

fn packet_id_eq_name(cond: &syn::Expr) -> Option<String> {
    let syn::Expr::Binary(binary) = cond else {
        return None;
    };
    if !matches!(binary.op, syn::BinOp::Eq(_)) {
        return None;
    }
    if !is_packet_id_path(&binary.left) {
        return None;
    }
    path_last_segment(&binary.right)
}

fn is_packet_id_path(expr: &syn::Expr) -> bool {
    let syn::Expr::Path(path) = expr else {
        return false;
    };
    path
        .path
        .segments
        .last()
        .is_some_and(|segment| segment.ident == "packet_id")
}

fn path_last_segment(expr: &syn::Expr) -> Option<String> {
    let syn::Expr::Path(path) = expr else {
        return None;
    };
    path.path
        .segments
        .last()
        .map(|segment| segment.ident.to_string())
}

fn arm_rows(protocol_root: &Path) -> Result<Vec<ArmRow>> {
    let mut cache: BTreeMap<&str, BTreeMap<String, DispatchArm>> = BTreeMap::new();
    for (family_a, family_b) in ARM_PAIRS {
        for family in [family_a, family_b] {
            if !cache.contains_key(family) {
                cache.insert(family, handle_play_arms(protocol_root, family)?);
            }
        }
    }

    let mut rows = Vec::new();
    for (family_a, family_b) in ARM_PAIRS {
        let arms_a = cache.get(family_a).map(BTreeMap::len).unwrap_or(0);
        let arms_b = cache.get(family_b).map(BTreeMap::len).unwrap_or(0);
        let arm_lines_a = cache
            .get(family_a)
            .map(|arms| arms.values().map(|arm| arm.line_count).sum())
            .unwrap_or(0);

        let mut common_names = 0;
        let mut identical = (0usize, 0usize);
        let mut ge_085 = (0usize, 0usize);
        let mut ge_060 = (0usize, 0usize);
        let mut lt_060 = (0usize, 0usize);

        if let (Some(map_a), Some(map_b)) = (cache.get(family_a), cache.get(family_b)) {
            for (name, arm_a) in map_a {
                let Some(arm_b) = map_b.get(name) else {
                    continue;
                };
                common_names += 1;
                let ratio = token_similarity_ratio(&arm_a.normalized, &arm_b.normalized);
                let bucket = if (ratio - 1.0).abs() < f64::EPSILON {
                    &mut identical
                } else if ratio >= 0.85 {
                    &mut ge_085
                } else if ratio >= 0.6 {
                    &mut ge_060
                } else {
                    &mut lt_060
                };
                bucket.0 += 1;
                bucket.1 += arm_a.line_count;
            }
        }

        rows.push(ArmRow {
            family_a: family_a.to_owned(),
            family_b: family_b.to_owned(),
            arms_a,
            arms_b,
            arm_lines_a,
            common_names,
            identical,
            ge_085,
            ge_060,
            lt_060,
        });
    }
    Ok(rows)
}

// ---------------------------------------------------------------------
// 4. Free-function identity under src/ (excl. generated/)
// ---------------------------------------------------------------------

struct FnInfo {
    normalized: String,
    line_count: usize,
}

fn free_functions(root: &Path, exclude_generated: bool) -> Result<BTreeMap<String, FnInfo>> {
    let mut items = BTreeMap::new();
    for path in collect_rs_files(root)? {
        if exclude_generated {
            let relative = path.strip_prefix(root).unwrap_or(&path);
            if contains_generated_component(relative) {
                continue;
            }
        }
        let content =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let file = syn::parse_file(&content)
            .with_context(|| format!("parse {} as Rust", path.display()))?;
        let mut visitor = FreeFnVisitor::default();
        visitor.visit_file(&file);
        for (name, info) in visitor.items {
            items.entry(name).or_insert(info);
        }
    }
    Ok(items)
}

#[derive(Default)]
struct FreeFnVisitor {
    items: BTreeMap<String, FnInfo>,
}

impl<'ast> Visit<'ast> for FreeFnVisitor {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if has_cfg_test(&node.attrs) {
            return;
        }
        syn::visit::visit_item_mod(self, node);
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        if has_cfg_test(&node.attrs) {
            return;
        }
        let start_line = node.sig.fn_token.span().start().line;
        let end_line = node.block.span().end().line;
        let line_count = end_line.saturating_sub(start_line) + 1;
        let normalized = node.block.to_token_stream().to_string();
        self.items
            .entry(node.sig.ident.to_string())
            .or_insert(FnInfo {
                normalized,
                line_count,
            });
        syn::visit::visit_item_fn(self, node);
    }
}

fn function_identity_report(protocol_root: &Path) -> Result<FunctionIdentityReport> {
    let mut src_fns: BTreeMap<&str, BTreeMap<String, FnInfo>> = BTreeMap::new();
    let mut test_fns: BTreeMap<&str, BTreeMap<String, FnInfo>> = BTreeMap::new();
    for family in FAMILIES {
        src_fns.insert(
            family,
            free_functions(&protocol_root.join(family).join("src"), true)?,
        );
        test_fns.insert(
            family,
            free_functions(&protocol_root.join(family).join("tests"), false)?,
        );
    }

    let mut triple_identical_count = 0;
    let mut triple_identical_lines = 0;
    if let (Some(a), Some(b), Some(c)) = (
        src_fns.get(LEGACY_TRIO[0]),
        src_fns.get(LEGACY_TRIO[1]),
        src_fns.get(LEGACY_TRIO[2]),
    ) {
        for (name, info_a) in a {
            let Some(info_b) = b.get(name) else { continue };
            let Some(info_c) = c.get(name) else { continue };
            if info_a.normalized == info_b.normalized && info_b.normalized == info_c.normalized {
                triple_identical_count += 1;
                triple_identical_lines += info_a.line_count;
            }
        }
    }

    let mut src_near_dup = Vec::new();
    let mut test_near_dup = Vec::new();
    for (family_a, family_b) in ADJACENT_PAIRS {
        let (matched, total) = near_duplicate_share(src_fns.get(family_a), src_fns.get(family_b));
        src_near_dup.push((family_a.to_owned(), family_b.to_owned(), matched, total));
        let (matched, total) =
            near_duplicate_share(test_fns.get(family_a), test_fns.get(family_b));
        test_near_dup.push((family_a.to_owned(), family_b.to_owned(), matched, total));
    }

    Ok(FunctionIdentityReport {
        triple_identical_count,
        triple_identical_lines,
        src_near_dup,
        test_near_dup,
    })
}

/// `(matched_lines, total_lines)` where `total_lines` sums every function's
/// line count in family A (shared or not) and `matched_lines` sums the ones
/// whose same-named counterpart in family B has token-similarity `>= 0.85`.
fn near_duplicate_share(
    family_a: Option<&BTreeMap<String, FnInfo>>,
    family_b: Option<&BTreeMap<String, FnInfo>>,
) -> (usize, usize) {
    let Some(family_a) = family_a else {
        return (0, 0);
    };
    let total: usize = family_a.values().map(|info| info.line_count).sum();
    let Some(family_b) = family_b else {
        return (0, total);
    };
    let matched: usize = family_a
        .iter()
        .filter_map(|(name, info_a)| {
            let info_b = family_b.get(name)?;
            let ratio = token_similarity_ratio(&info_a.normalized, &info_b.normalized);
            (ratio >= 0.85).then_some(info_a.line_count)
        })
        .sum();
    (matched, total)
}

// ---------------------------------------------------------------------
// 5. minecraft-data packet-shape adjacency
// ---------------------------------------------------------------------

fn minecraft_data_adjacency(workspace_root: &Path) -> Result<(Vec<AdjacencyRow>, Option<String>)> {
    let vendor_data = workspace_root.join("vendor/minecraft-data/data");
    if !vendor_data.is_dir() {
        return Ok((
            Vec::new(),
            Some(format!(
                "{} not present; skipped (vendor checkout not available in this environment)",
                vendor_data.display()
            )),
        ));
    }

    let mut rows = Vec::new();
    let mut previous_shapes: Option<BTreeMap<(String, String, String), Value>> = None;
    for version in ADJACENCY_VERSIONS {
        let path = resolve_minecraft_data_protocol_path(&vendor_data, version)?;
        let content =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let root: Value =
            serde_json::from_str(&content).with_context(|| format!("parse {}", path.display()))?;
        let shapes = packet_shapes(&root)?;
        let packets = shapes.len();

        let row = match &previous_shapes {
            None => AdjacencyRow {
                version: version.to_owned(),
                packets,
                same_as_previous: None,
                new: 0,
                gone: 0,
            },
            Some(previous) => {
                let mut same = 0;
                let mut new = 0;
                let mut gone = 0;
                let keys: BTreeSet<&(String, String, String)> =
                    previous.keys().chain(shapes.keys()).collect();
                for key in keys {
                    match (previous.get(key), shapes.get(key)) {
                        (Some(before), Some(after)) => {
                            if before == after {
                                same += 1;
                            }
                        }
                        (None, Some(_)) => new += 1,
                        (Some(_), None) => gone += 1,
                        (None, None) => {}
                    }
                }
                AdjacencyRow {
                    version: version.to_owned(),
                    packets,
                    same_as_previous: Some(same),
                    new,
                    gone,
                }
            }
        };
        rows.push(row);
        previous_shapes = Some(shapes);
    }

    Ok((rows, None))
}

/// Resolves a target Minecraft version to a `protocol.json` path via
/// `dataPaths.json`'s `pc.<version>.protocol` alias, falling back to a
/// `pc/<major.minor>` directory for the two versions
/// (1.7.10, 1.8.9) `dataPaths.json` has no entry for at all — minecraft-data
/// only tracks those by their `major.minor` snapshot.
fn resolve_minecraft_data_protocol_path(vendor_data: &Path, version: &str) -> Result<PathBuf> {
    let data_paths_json = std::fs::read_to_string(vendor_data.join("dataPaths.json"))
        .with_context(|| format!("read {}/dataPaths.json", vendor_data.display()))?;
    let data_paths: Value =
        serde_json::from_str(&data_paths_json).context("parse minecraft-data dataPaths.json")?;

    if let Some(relative) = data_paths
        .get("pc")
        .and_then(|pc| pc.get(version))
        .and_then(|entry| entry.get("protocol"))
        .and_then(Value::as_str)
    {
        let candidate = vendor_data.join(relative).join("protocol.json");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    let mut parts = version.split('.');
    if let (Some(major), Some(minor)) = (parts.next(), parts.next()) {
        let major_minor = format!("{major}.{minor}");
        let candidate = vendor_data
            .join("pc")
            .join(&major_minor)
            .join("protocol.json");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    bail!(
        "no minecraft-data protocol.json found for {version} \
         (checked dataPaths.json's pc.{version}.protocol and a pc/<major.minor> fallback)"
    )
}

/// Every `(state, bound, packet_name) -> shape` entry in a minecraft-data
/// `protocol.json`, with every named type reference recursively inlined —
/// so a change to a shared type like `slot` or `entityMetadata` propagates
/// into every packet that carries one, matching the plan's stated
/// methodology. A `$cycle` guard stops a self-referential type (this
/// workspace has not needed one yet, but nbt-shaped types are exactly the
/// kind that can be) from recursing forever.
fn packet_shapes(root: &Value) -> Result<BTreeMap<(String, String, String), Value>> {
    let root_object = root
        .as_object()
        .ok_or_else(|| anyhow!("protocol.json root must be an object"))?;
    let empty_map = serde_json::Map::new();
    let types_global = root_object
        .get("types")
        .and_then(Value::as_object)
        .unwrap_or(&empty_map);

    let mut shapes = BTreeMap::new();
    for (state_key, state_value) in root_object {
        if state_key == "types" {
            continue;
        }
        let Some(state_object) = state_value.as_object() else {
            continue;
        };
        for bound_key in ["toClient", "toServer"] {
            let Some(bound_value) = state_object.get(bound_key) else {
                continue;
            };
            let Some(types_state) = bound_value.get("types").and_then(Value::as_object) else {
                continue;
            };
            let Some(packet) = types_state.get("packet").and_then(Value::as_array) else {
                continue;
            };
            let Some(fields) = packet.get(1).and_then(Value::as_array) else {
                continue;
            };

            let mut mappings = None;
            let mut switch_fields = None;
            for field in fields {
                let Some(field_object) = field.as_object() else {
                    continue;
                };
                let Some(field_type) = field_object.get("type").and_then(Value::as_array) else {
                    continue;
                };
                match field_type.first().and_then(Value::as_str) {
                    Some("mapper") => {
                        mappings = field_type
                            .get(1)
                            .and_then(Value::as_object)
                            .and_then(|object| object.get("mappings"))
                            .and_then(Value::as_object);
                    }
                    Some("switch") => {
                        switch_fields = field_type
                            .get(1)
                            .and_then(Value::as_object)
                            .and_then(|object| object.get("fields"))
                            .and_then(Value::as_object);
                    }
                    _ => {}
                }
            }
            let (Some(mappings), Some(switch_fields)) = (mappings, switch_fields) else {
                continue;
            };

            for name_value in mappings.values() {
                let Some(name) = name_value.as_str() else {
                    continue;
                };
                let Some(field_type) = switch_fields.get(name) else {
                    continue;
                };
                let resolved = resolve_type(field_type, types_state, types_global, &BTreeSet::new());
                shapes.insert(
                    (state_key.clone(), bound_key.to_owned(), name.to_owned()),
                    resolved,
                );
            }
        }
    }
    Ok(shapes)
}

fn resolve_type(
    value: &Value,
    types_state: &serde_json::Map<String, Value>,
    types_global: &serde_json::Map<String, Value>,
    seen: &BTreeSet<String>,
) -> Value {
    match value {
        Value::String(name) => {
            if seen.contains(name) {
                return serde_json::json!({ "$cycle": name });
            }
            let Some(definition) = types_state.get(name).or_else(|| types_global.get(name)) else {
                return value.clone();
            };
            let mut next_seen = seen.clone();
            next_seen.insert(name.clone());
            resolve_type(definition, types_state, types_global, &next_seen)
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| resolve_type(item, types_state, types_global, seen))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, item)| {
                    (
                        key.clone(),
                        resolve_type(item, types_state, types_global, seen),
                    )
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lcs_len_matches_known_cases() {
        assert_eq!(lcs_len(&[], &["a"]), 0);
        assert_eq!(lcs_len(&["a", "b", "c"], &["a", "b", "c"]), 3);
        assert_eq!(lcs_len(&["a", "b", "c"], &["a", "x", "c"]), 2);
        assert_eq!(lcs_len(&["a", "b"], &["b", "a"]), 1);
    }

    #[test]
    fn token_similarity_ratio_identical_and_partial() {
        assert!((token_similarity_ratio("a b c", "a b c") - 1.0).abs() < f64::EPSILON);
        assert!((token_similarity_ratio("", "") - 1.0).abs() < f64::EPSILON);
        let ratio = token_similarity_ratio("a b c d", "a b x y");
        assert!(ratio > 0.0 && ratio < 1.0, "ratio was {ratio}");
    }

    #[test]
    fn resolve_type_guards_against_a_self_referential_type() {
        let mut types_state = serde_json::Map::new();
        types_state.insert(
            "recursive".to_owned(),
            Value::Array(vec![Value::String("recursive".to_owned())]),
        );
        let types_global = serde_json::Map::new();
        // Must terminate rather than blow the stack.
        let resolved = resolve_type(
            &Value::String("recursive".to_owned()),
            &types_state,
            &types_global,
            &BTreeSet::new(),
        );
        assert!(resolved.to_string().contains("cycle"));
    }

    #[test]
    fn packet_shapes_extracts_a_minimal_fixture() {
        let json = serde_json::json!({
            "types": {},
            "play": {
                "toClient": {
                    "types": {
                        "packet": ["container", [
                            {"name": "name", "type": ["mapper", {"type": "varint", "mappings": {"0x00": "keep_alive"}}]},
                            {"name": "params", "type": ["switch", {"compareTo": "name", "fields": {"keep_alive": "packet_keep_alive"}}]},
                        ]],
                        "packet_keep_alive": ["container", [{"name": "id", "type": "i64"}]],
                    }
                }
            }
        });
        let shapes = packet_shapes(&json).expect("fixture parses");
        assert_eq!(shapes.len(), 1);
        let key = ("play".to_owned(), "toClient".to_owned(), "keep_alive".to_owned());
        assert!(shapes.contains_key(&key));
    }

    #[test]
    fn struct_totals_are_positive_for_every_family_on_the_real_tree() {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let protocol_root = workspace_root.join("crates/versions");
        if !protocol_root.is_dir() {
            return;
        }
        let totals = struct_totals(&protocol_root).expect("struct_totals runs on the real tree");
        for family in FAMILIES {
            let count = totals.get(family).copied().unwrap_or(0);
            assert!(count > 0, "{family} reported zero packet structs/enums");
        }
    }

    #[test]
    fn handle_play_arm_counts_are_positive_on_the_real_tree() {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let protocol_root = workspace_root.join("crates/versions");
        if !protocol_root.is_dir() {
            return;
        }
        for family in LEGACY_TRIO {
            let arms = handle_play_arms(&protocol_root, family)
                .unwrap_or_else(|err| panic!("handle_play_arms({family}) failed: {err}"));
            assert!(!arms.is_empty(), "{family}'s handle_play had no arms");
        }
    }

    #[test]
    fn protocol_dup_report_runs_end_to_end_on_the_real_tree() {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        if !workspace_root.join("crates/versions").is_dir() {
            return;
        }
        let report =
            protocol_dup_report(&workspace_root).expect("protocol_dup_report runs end-to-end");
        // Every measurement should produce *something* to look at; an empty
        // table here would mean a walk silently found nothing, which is the
        // "audit that prints nothing is a failure to run" case CLAUDE.md
        // names, not a clean pass.
        assert!(!report.file_rows.is_empty());
        assert!(!report.struct_rows.is_empty());
        assert!(!report.arm_rows.is_empty());
        let rendered = report.render();
        assert!(rendered.contains("protocol-dup"));
    }
}
