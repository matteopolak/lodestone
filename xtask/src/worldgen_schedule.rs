//! `cargo xtask check-worldgen-schedule` — source guard for the worldgen seam.
//!
//! The typed schedules are useful only when production orchestration actually
//! consumes them. This deliberately small AST audit checks the boundary that
//! a compiler cannot express: an entrypoint that calls stage-like operations
//! must obtain its ordering cursor from that dimension's central schedule.
//! Stage implementations may contain pass-local loops, but only approved
//! executor files may own typed cursor calls. The audit also checks that the
//! central schedule declares typed option-gate metadata for every dimension.

use anyhow::{Context, Result, bail};
use proc_macro2::LineColumn;
use quote::ToTokens;
use std::fs;
use std::path::{Path, PathBuf};
use syn::visit::Visit;
use syn::{
    Attribute, Expr, ExprCall, ExprForLoop, ExprMethodCall, ImplItemFn, Item, ItemFn, ItemMod, Lit,
    Meta, Pat,
};

const WORLDGEN_ROOT: &str = "crates/lodestone-worldgen/src";
const REQUIRED_FILES: &[&str] = &[
    "crates/lodestone-worldgen/src/stage_schedule.rs",
    "crates/lodestone-worldgen/src/overworld/mod.rs",
    "crates/lodestone-worldgen/src/overworld/fill.rs",
    "crates/lodestone-worldgen/src/nether/mod.rs",
    "crates/lodestone-worldgen/src/end/mod.rs",
];
/// Files which are allowed to own a typed pass sequence.  The dimension
/// modules are executors; `overworld/fill.rs` is the one factored executor for
/// the shared terrain prefix.  Every other production module may implement a
/// pass, but must not decide the order in which passes run.
const APPROVED_STAGE_FILES: &[&str] = &[
    "crates/lodestone-worldgen/src/stage_schedule.rs",
    "crates/lodestone-worldgen/src/overworld/mod.rs",
    "crates/lodestone-worldgen/src/overworld/fill.rs",
    "crates/lodestone-worldgen/src/nether/mod.rs",
    "crates/lodestone-worldgen/src/end/mod.rs",
];
/// Public/batch seams at which a copied source neighbourhood is especially
/// easy to introduce.  This is intentionally a small allowlist of names, not
/// a guess based on every function containing a loop: generation internals
/// quite legitimately scan their own block geometry.
const SOURCE_ENTRYPOINTS: &[&str] = &[
    "column",
    "column_shaped",
    "columns_batch",
    "columns_spatial_batch",
    "columns_spatial_batch_observed",
    "base_world_rectangle",
    "prepare_packet_replay",
    "parity_source_decoration_for_target_with_overrides",
    "parity_source_spills_with_resident",
];
const HORIZON_SETTING_FILES: &[&str] = &[
    "crates/lodestone-shell/src/config.rs",
    "crates/lodestone-shell/src/menu/options.rs",
];

const STAGE_LIKE_METHODS: &[&str] = &[
    "apply_carvers",
    "apply_region",
    "fill_column",
    "materialize_column",
    "surface_stage",
    "structure_place_stage",
    "features_stage",
    "top_layer_stage",
];

/// A source-level finding, named by symbol and line for a useful failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub file: String,
    pub line: usize,
    pub function: String,
    pub reason: String,
}

/// Result of the production worldgen schedule audit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub files_scanned: usize,
    pub entrypoints: usize,
    pub metadata_checks: usize,
    pub settings_checks: usize,
    pub violations: Vec<Violation>,
}

impl Report {
    #[must_use]
    pub fn has_violations(&self) -> bool {
        !self.violations.is_empty()
    }

    #[must_use]
    pub fn render(&self) -> String {
        let mut out = format!(
            "worldgen schedule: {} files, {} entrypoints, {} metadata checks, {} settings checks\n",
            self.files_scanned, self.entrypoints, self.metadata_checks, self.settings_checks
        );
        if self.violations.is_empty() {
            out.push_str("worldgen schedule check passed\n");
        } else {
            for violation in &self.violations {
                out.push_str(&format!(
                    "VIOLATION {}:{} {} — {}\n",
                    violation.file, violation.line, violation.function, violation.reason
                ));
            }
        }
        out
    }
}

#[derive(Default)]
struct FunctionFacts {
    stage_operations: Vec<LineColumn>,
    typed_schedule_calls: Vec<LineColumn>,
    central_cursors: Vec<(String, LineColumn)>,
    level_cutoffs: Vec<LineColumn>,
    descriptors: Vec<LineColumn>,
    worldgen_entry_calls: Vec<LineColumn>,
    source_window_loops: Vec<LineColumn>,
}

struct FunctionVisitor {
    facts: FunctionFacts,
}

impl<'ast> Visit<'ast> for FunctionVisitor {
    fn visit_expr_method_call(&mut self, node: &'ast ExprMethodCall) {
        let method = node.method.to_string();
        if typed_schedule_method(node) {
            let line = node.method.span().start();
            self.facts.typed_schedule_calls.push(line);
            self.facts.stage_operations.push(line);
        }
        if method == "cursor" || method == "cursor_at" {
            let receiver = render_expr(&node.receiver);
            if receiver.contains("stage_schedule") || receiver.contains("StageSchedule") {
                self.facts
                    .central_cursors
                    .push((receiver, node.method.span().start()));
            }
        }
        if method == "stages_for_level" {
            self.facts.level_cutoffs.push(node.method.span().start());
        }
        if method == "descriptor" {
            self.facts.descriptors.push(node.method.span().start());
        }
        if matches!(
            method.as_str(),
            "column"
                | "column_shaped"
                | "columns_batch"
                | "columns_spatial_batch"
                | "columns_spatial_batch_observed"
        ) {
            self.facts
                .worldgen_entry_calls
                .push(node.method.span().start());
        }
        if stage_operation_name(&method) {
            self.facts.stage_operations.push(node.method.span().start());
        }
        syn::visit::visit_expr_method_call(self, node);
    }

    fn visit_expr_call(&mut self, node: &'ast ExprCall) {
        let name = call_name(&node.func);
        if stage_operation_name(&name) {
            self.facts.stage_operations.push(node.paren_token.span.join().start());
        }
        if name == "stages_for_level" {
            self.facts.level_cutoffs.push(node.paren_token.span.join().start());
        }
        if name == "descriptor" {
            self.facts.descriptors.push(node.paren_token.span.join().start());
        }
        if matches!(
            name.as_str(),
            "column"
                | "column_shaped"
                | "columns_batch"
                | "columns_spatial_batch"
                | "columns_spatial_batch_observed"
        ) {
            self.facts
                .worldgen_entry_calls
                .push(node.paren_token.span.join().start());
        }
        syn::visit::visit_expr_call(self, node);
    }

    fn visit_expr_for_loop(&mut self, node: &'ast ExprForLoop) {
        let outer = loop_axis(&node.pat);
        if outer.is_some() && is_offset_or_radius_range(&node.expr) {
            let mut nested = NestedOffsetLoopVisitor {
                outer_axis: outer,
                found: None,
            };
            nested.visit_block(&node.body);
            if let Some(line) = nested.found {
                self.facts.source_window_loops.push(line);
            }
        }
        syn::visit::visit_expr_for_loop(self, node);
    }
}

fn method_receiver_text(node: &ExprMethodCall) -> String {
    render_expr(&node.receiver)
}

fn typed_schedule_method(node: &ExprMethodCall) -> bool {
    let method = node.method.to_string();
    let receiver = method_receiver_text(node);
    if !receiver.contains("schedule")
        && !receiver.contains("cursor")
        && !receiver.contains("StageCursor")
    {
        return false;
    }
    match method.as_str() {
        "enter" | "run" => node.args.to_token_stream().to_string().contains("ColumnStage"),
        "finish_prefix" => true,
        "finish" => true,
        _ => false,
    }
}

fn loop_axis(pattern: &Pat) -> Option<char> {
    let Pat::Ident(ident) = pattern else {
        return None;
    };
    let name = ident.ident.to_string().to_ascii_lowercase();
    if name == "dx" || name.ends_with("_x") || name.ends_with("x_offset") {
        Some('x')
    } else if name == "dz" || name.ends_with("_z") || name.ends_with("z_offset") {
        Some('z')
    } else {
        None
    }
}

fn is_offset_or_radius_range(expr: &Expr) -> bool {
    let text = expr.to_token_stream().to_string().to_ascii_lowercase();
    text.contains("..=")
        && (text.contains("radius")
            || text.contains("wide_radius")
            || text.contains("- 1")
            || text.contains("-1"))
}

#[derive(Default)]
struct NestedOffsetLoopVisitor {
    outer_axis: Option<char>,
    found: Option<LineColumn>,
}

impl<'ast> Visit<'ast> for NestedOffsetLoopVisitor {
    fn visit_expr_for_loop(&mut self, node: &'ast ExprForLoop) {
        if self.found.is_none()
            && loop_axis(&node.pat).is_some_and(|axis| Some(axis) != self.outer_axis)
            && is_offset_or_radius_range(&node.expr)
        {
            self.found = Some(node.for_token.span.start());
        }
        syn::visit::visit_expr_for_loop(self, node);
    }
}

fn call_name(expr: &Expr) -> String {
    match expr {
        Expr::Path(path) => path
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string())
            .unwrap_or_default(),
        Expr::MethodCall(method) => method.method.to_string(),
        Expr::Paren(paren) => call_name(&paren.expr),
        Expr::Group(group) => call_name(&group.expr),
        _ => String::new(),
    }
}

fn render_expr(expr: &Expr) -> String {
    match expr {
        Expr::Path(path) => path
            .path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect::<Vec<_>>()
            .join("::"),
        Expr::MethodCall(method) => format!("{}.{}", render_expr(&method.receiver), method.method),
        Expr::Call(call) => format!("{}()", render_expr(&call.func)),
        Expr::Paren(paren) => render_expr(&paren.expr),
        Expr::Group(group) => render_expr(&group.expr),
        _ => String::new(),
    }
}

fn cfg_test(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attribute| {
        if !attribute.path().is_ident("cfg") {
            return false;
        }
        match &attribute.meta {
            Meta::List(list) => {
                let tokens = list.tokens.to_string();
                tokens
                    .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
                    .any(|token| token == "test")
            }
            _ => false,
        }
    })
}

fn stage_operation_name(name: &str) -> bool {
    // Cached-prefix accessors delegate to cursor-checked implementations and
    // do not choose a pass order themselves. A shaped entrypoint may call one
    // without opening a second cursor.
    if matches!(name, "pre_ore_stage" | "pre_decoration_stage") {
        return false;
    }
    STAGE_LIKE_METHODS.contains(&name) || name.ends_with("_stage")
}

fn far_entrypoint_name(name: &str) -> bool {
    ["far", "distant", "horizon", "lod"]
        .iter()
        .any(|marker| name.split('_').any(|part| part == *marker))
}

fn dimension_for(path: &str) -> Option<&'static str> {
    if path.contains("/overworld/") {
        Some("OVERWORLD")
    } else if path.contains("/nether/") {
        Some("NETHER")
    } else if path.contains("/end/") {
        Some("END")
    } else {
        None
    }
}

fn approved_stage_file(file: &str) -> bool {
    APPROVED_STAGE_FILES.contains(&file)
}

fn source_entrypoint(name: &str) -> bool {
    SOURCE_ENTRYPOINTS.contains(&name)
}

fn stage_implementation(name: &str) -> bool {
    name.ends_with("_stage") || name.contains("_stage_")
}

fn inspect_function(
    file: &str,
    name: &str,
    body: &syn::Block,
    dimension: Option<&str>,
    approved: bool,
    violations: &mut Vec<Violation>,
) -> bool {
    // Do not maintain a second list of entrypoint names: any non-stage helper
    // that directly sequences a stage is an orchestration owner and must use
    // the central cursor. This catches a newly named production entrypoint as
    // soon as it is added. Stage implementations remain the explicit escape
    // hatch because they are the operations the cursor surrounds.
    let mut visitor = FunctionVisitor {
        facts: FunctionFacts::default(),
    };
    visitor.visit_block(body);

    if !approved && !visitor.facts.typed_schedule_calls.is_empty() {
        let line = visitor.facts.typed_schedule_calls[0].line.max(1);
        violations.push(Violation {
            file: file.to_owned(),
            line,
            function: name.to_owned(),
            reason: "manual stage sequencing must live in an approved dimension executor or stage-prefix implementation".to_owned(),
        });
    }

    if !approved && source_entrypoint(name) && !visitor.facts.source_window_loops.is_empty() {
        let line = visitor.facts.source_window_loops[0].line.max(1);
        violations.push(Violation {
            file: file.to_owned(),
            line,
            function: name.to_owned(),
            reason: "source entrypoint duplicates an offset/radius neighbourhood loop; use the central source schedule".to_owned(),
        });
    }

    // A helper whose name identifies one pass may contain several internal
    // calls while implementing that pass. It is not an ordering owner, but a
    // rogue typed cursor in it was still reported above before this exemption.
    if stage_implementation(name) {
        return false;
    }

    // A distant/far worldgen entrypoint must be a request against the same
    // schedule, not a second implementation of a reduced pipeline. Requiring
    // both calls in the owner keeps the requested cutoff and its descriptor
    // adjacent in source, while the runtime schedule remains the authority for
    // the actual prefix and retained products.
    if far_entrypoint_name(name)
        && (!visitor.facts.worldgen_entry_calls.is_empty()
            || !visitor.facts.stage_operations.is_empty())
        && (visitor.facts.level_cutoffs.is_empty() || visitor.facts.descriptors.is_empty())
    {
        violations.push(Violation {
            file: file.to_owned(),
            line: body.brace_token.span.open().start().line.max(1),
            function: name.to_owned(),
            reason: "far-worldgen entrypoint must use the central descriptor with a requested GenerationLevel cutoff".to_owned(),
        });
    }
    // A helper that delegates one operation (for example a public accessor
    // for structure starts) is not an ordering owner. Two or more stage
    // operations, or an explicit cursor sequence, are the mechanical signal
    // that a function is choosing a pipeline order.
    if visitor.facts.stage_operations.len() < 2 {
        return false;
    }
    if visitor.facts.central_cursors.is_empty() {
        violations.push(Violation {
            file: file.to_owned(),
            line: body.brace_token.span.open().start().line.max(1),
            function: name.to_owned(),
            reason: "entrypoint sequences stage work without a cursor from its central dimension schedule".to_owned(),
        });
    } else if let Some(dimension) = dimension {
        if visitor.facts.central_cursors.iter().all(|(receiver, _)| {
            !receiver.contains(dimension) && !receiver.contains("Self::stage_schedule")
        }) {
            let line = visitor.facts.central_cursors[0].1.line.max(1);
            violations.push(Violation {
                file: file.to_owned(),
                line,
                function: name.to_owned(),
                reason: format!("cursor must come from {dimension} or Self::stage_schedule"),
            });
        }
    }
    true
}

struct SourceVisitor<'a> {
    file: &'a str,
    dimension: Option<&'static str>,
    violations: &'a mut Vec<Violation>,
    entrypoints: &'a mut usize,
    approved: bool,
}

impl<'ast> Visit<'ast> for SourceVisitor<'_> {
    fn visit_item_mod(&mut self, node: &'ast ItemMod) {
        if cfg_test(&node.attrs) {
            return;
        }
        syn::visit::visit_item_mod(self, node);
    }

    fn visit_item_fn(&mut self, node: &'ast ItemFn) {
        if cfg_test(&node.attrs) {
            return;
        }
        if inspect_function(
            self.file,
            &node.sig.ident.to_string(),
            &node.block,
            self.dimension,
            self.approved,
            self.violations,
        ) {
            if self.dimension.is_some() {
                *self.entrypoints += 1;
            }
        }
        syn::visit::visit_item_fn(self, node);
    }

    fn visit_impl_item_fn(&mut self, node: &'ast ImplItemFn) {
        if cfg_test(&node.attrs) {
            return;
        }
        if inspect_function(
            self.file,
            &node.sig.ident.to_string(),
            &node.block,
            self.dimension,
            self.approved,
            self.violations,
        ) {
            if self.dimension.is_some() {
                *self.entrypoints += 1;
            }
        }
        syn::visit::visit_impl_item_fn(self, node);
    }
}

fn metadata_violations(file: &str, source: &str) -> Vec<Violation> {
    let required = [
        "StageOption",
        "StageGate",
        "GenerationLevel",
        "stages_for_level",
        "StageDescriptor",
        "descriptor",
        "StageSchedule::with_gates",
        "OVERWORLD_GATES",
        "NETHER_GATES",
        "END_GATES",
    ];
    required
        .iter()
        .filter(|needle| !source.contains(*needle))
        .map(|needle| Violation {
            file: file.to_owned(),
            line: 1,
            function: "schedule metadata".to_owned(),
            reason: format!("missing typed option-gate declaration {needle}"),
        })
        .collect()
}

fn collect_rust_files(directory: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(directory).with_context(|| format!("walk {}", directory.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_rust_files(&path, files)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
    Ok(())
}

fn scan_paths(root: &Path, paths: &[PathBuf]) -> Result<Report> {
    let mut violations = Vec::new();
    let mut entrypoints = 0;
    let mut metadata_checks = 0;
    for path in paths {
        let relative = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let source = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        if relative.ends_with("stage_schedule.rs") {
            violations.extend(metadata_violations(&relative, &source));
            metadata_checks += 1;
            continue;
        }
        let syntax = syn::parse_file(&source)
            .with_context(|| format!("parse production worldgen source {}", path.display()))?;
        let mut visitor = SourceVisitor {
            file: &relative,
            dimension: dimension_for(&relative),
            violations: &mut violations,
            entrypoints: &mut entrypoints,
            approved: approved_stage_file(&relative),
        };
        visitor.visit_file(&syntax);
    }
    Ok(Report {
        files_scanned: paths.len(),
        entrypoints,
        metadata_checks,
        settings_checks: 0,
        violations,
    })
}

fn horizon_setting_violations(root: &Path) -> Result<Vec<Violation>> {
    let config_path = root.join(HORIZON_SETTING_FILES[0]);
    let options_path = root.join(HORIZON_SETTING_FILES[1]);
    let config_source = fs::read_to_string(&config_path)
        .with_context(|| format!("read {}", config_path.display()))?;
    let options_source = fs::read_to_string(&options_path)
        .with_context(|| format!("read {}", options_path.display()))?;
    let config_file = syn::parse_file(&config_source)
        .with_context(|| format!("parse {}", config_path.display()))?;
    let has_exact_max = config_file.items.iter().any(|item| {
        let Item::Const(item) = item else { return false };
        item.ident == "MAX_HORIZON_DISTANCE_CHUNKS"
            && matches!(&*item.expr, Expr::Lit(expr) if matches!(&expr.lit, Lit::Int(value) if value.base10_digits() == "256"))
    });
    let mut violations = Vec::new();
    if !has_exact_max {
        violations.push(Violation {
            file: HORIZON_SETTING_FILES[0].to_owned(),
            line: 1,
            function: "MAX_HORIZON_DISTANCE_CHUNKS".to_owned(),
            reason: "Distant Horizon storage/slider maximum must remain exactly 256 chunks".to_owned(),
        });
    }
    if !options_source.contains("DistantHorizon")
        || !options_source.contains("MAX_HORIZON_DISTANCE_CHUNKS")
        || !options_source.contains("horizon_distance_slider_fraction")
    {
        violations.push(Violation {
            file: HORIZON_SETTING_FILES[1].to_owned(),
            line: 1,
            function: "DistantHorizon".to_owned(),
            reason: "Distant Horizon slider must use the shared 256-chunk maximum".to_owned(),
        });
    }
    Ok(violations)
}

/// Audit production worldgen orchestration and central schedule metadata.
pub fn check_worldgen_schedule(root: &Path) -> Result<Report> {
    for relative in REQUIRED_FILES {
        let path = root.join(relative);
        if !path.is_file() {
            bail!("worldgen schedule guard cannot find required source {}", path.display());
        }
    }
    let mut paths = Vec::new();
    collect_rust_files(&root.join(WORLDGEN_ROOT), &mut paths)?;
    paths.sort();
    let mut report = scan_paths(root, &paths)?;
    report.violations.extend(horizon_setting_violations(root)?);
    report.settings_checks = 1;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(root: &Path, relative: &str, source: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().expect("fixture parent")).unwrap();
        fs::write(path, source).unwrap();
    }

    #[test]
    fn real_worldgen_schedule_has_no_orchestration_violations() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let report = check_worldgen_schedule(&root).unwrap();
        assert!(!report.has_violations(), "{}", report.render());
        assert!(report.entrypoints >= 5);
        assert_eq!(report.metadata_checks, 1);
        assert_eq!(report.settings_checks, 1);
    }

    #[test]
    fn independent_entrypoint_ordering_is_rejected_but_stage_impl_is_allowed() {
        let tmp = tempfile::tempdir().unwrap();
        fixture(
            tmp.path(),
            "crates/lodestone-worldgen/src/stage_schedule.rs",
            "enum StageOption {} struct StageGate {} enum GenerationLevel {} struct StageDescriptor {} impl StageSchedule { fn with_gates() {} fn stages_for_level() {} fn descriptor() {} } const OVERWORLD_GATES: () = (); const NETHER_GATES: () = (); const END_GATES: () = ();",
        );
        fixture(
            tmp.path(),
            "crates/lodestone-worldgen/src/overworld/mod.rs",
            "impl Generator { fn column(&self) { self.fill_stage(); self.surface_stage(); } fn fill_stage(&self) {} }",
        );
        fixture(
            tmp.path(),
            "crates/lodestone-worldgen/src/nether/mod.rs",
            "impl Generator { fn column(&self) { let mut schedule = crate::stage_schedule::NETHER.cursor(); schedule.enter(Stage::Fill); } fn fill_stage(&self) {} }",
        );
        fixture(
            tmp.path(),
            "crates/lodestone-worldgen/src/end/mod.rs",
            "impl Generator { fn column(&self) {} fn fill_stage(&self) {} }",
        );
        fixture(
            tmp.path(),
            "crates/lodestone-worldgen/src/overworld/fill.rs",
            "",
        );
        let paths = REQUIRED_FILES.iter().map(|path| tmp.path().join(path)).collect::<Vec<_>>();
        let report = scan_paths(tmp.path(), &paths).unwrap();
        assert!(report.violations.iter().any(|violation| {
            violation.function == "column"
                && violation.reason.contains("without a cursor")
        }));
        assert!(!report.violations.iter().any(|violation| violation.function == "fill_stage"));
    }

    #[test]
    fn missing_gate_metadata_is_not_a_pass() {
        let tmp = tempfile::tempdir().unwrap();
        fixture(
            tmp.path(),
            "crates/lodestone-worldgen/src/stage_schedule.rs",
            "const OVERWORLD_GATES: () = ();",
        );
        let path = tmp.path().join(REQUIRED_FILES[0]);
        let violations = metadata_violations(REQUIRED_FILES[0], &fs::read_to_string(path).unwrap());
        assert!(violations.iter().any(|violation| violation.reason.contains("StageOption")));
        assert!(violations.iter().any(|violation| violation.reason.contains("END_GATES")));
    }

    #[test]
    fn far_entrypoint_requires_requested_level_and_descriptor() {
        let tmp = tempfile::tempdir().unwrap();
        fixture(
            tmp.path(),
            "crates/lodestone-worldgen/src/stage_schedule.rs",
            "enum StageOption {} struct StageGate {} struct GenerationLevel {} struct StageDescriptor {} impl StageSchedule { fn with_gates() {} fn stages_for_level() {} fn descriptor() {} } const OVERWORLD_GATES: () = (); const NETHER_GATES: () = (); const END_GATES: () = ();",
        );
        fixture(
            tmp.path(),
            "crates/lodestone-worldgen/src/overworld/mod.rs",
            "impl Generator { fn far_column(&self) { self.column_shaped(0, 0); } fn far_level(&self, level: GenerationLevel) { Self::stage_schedule().stages_for_level(level); Self::stage_schedule().descriptor(Stage::Fill); } }",
        );
        let paths = [REQUIRED_FILES[0], REQUIRED_FILES[1]]
            .iter()
            .map(|path| tmp.path().join(path))
            .collect::<Vec<_>>();
        let report = scan_paths(tmp.path(), &paths).unwrap();
        assert!(report.violations.iter().any(|violation| {
            violation.function == "far_column"
                && violation.reason.contains("GenerationLevel cutoff")
        }));
        assert!(!report.violations.iter().any(|violation| {
            violation.function == "far_level"
        }));
    }

    #[test]
    fn direct_source_window_at_an_unapproved_entrypoint_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        fixture(
            tmp.path(),
            "crates/lodestone-worldgen/src/stage_schedule.rs",
            "enum StageOption {} struct StageGate {} enum GenerationLevel {} struct StageDescriptor {} impl StageSchedule { fn with_gates() {} fn stages_for_level() {} fn descriptor() {} } const OVERWORLD_GATES: () = (); const NETHER_GATES: () = (); const END_GATES: () = ();",
        );
        fixture(
            tmp.path(),
            "crates/lodestone-worldgen/src/feature/rogue.rs",
            "pub fn columns_batch(radius: i32) { for dx in -radius..=radius { for dz in -radius..=radius { let _ = (dx, dz); } } }",
        );
        let paths = [REQUIRED_FILES[0], "crates/lodestone-worldgen/src/feature/rogue.rs"]
            .iter()
            .map(|path| tmp.path().join(path))
            .collect::<Vec<_>>();
        let report = scan_paths(tmp.path(), &paths).unwrap();
        assert!(report.violations.iter().any(|violation| {
            violation.function == "columns_batch"
                && violation.reason.contains("offset/radius neighbourhood loop")
        }));
    }

    #[test]
    fn tests_and_generation_internals_are_not_source_window_entrypoints() {
        let tmp = tempfile::tempdir().unwrap();
        fixture(
            tmp.path(),
            "crates/lodestone-worldgen/src/stage_schedule.rs",
            "enum StageOption {} struct StageGate {} enum GenerationLevel {} struct StageDescriptor {} impl StageSchedule { fn with_gates() {} fn stages_for_level() {} fn descriptor() {} } const OVERWORLD_GATES: () = (); const NETHER_GATES: () = (); const END_GATES: () = ();",
        );
        fixture(
            tmp.path(),
            "crates/lodestone-worldgen/src/feature/internals.rs",
            "pub fn fill_geometry(radius: i32) { for dx in -radius..=radius { for dz in -radius..=radius { let _ = (dx, dz); } } } #[cfg(any(test, feature = \"fixture\"))] mod tests { pub fn columns_batch(radius: i32) { for dx in -radius..=radius { for dz in -radius..=radius { let _ = (dx, dz); } } } }",
        );
        let paths = [REQUIRED_FILES[0], "crates/lodestone-worldgen/src/feature/internals.rs"]
            .iter()
            .map(|path| tmp.path().join(path))
            .collect::<Vec<_>>();
        let report = scan_paths(tmp.path(), &paths).unwrap();
        assert!(!report.violations.iter().any(|violation| {
            violation.function == "fill_geometry" || violation.function == "columns_batch"
        }));
    }
}
