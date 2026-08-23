//! `cargo xtask check-ptr-const` — a syn-based guard for pointer-identity
//! comparisons that target a `const` item.
//!
//! # Why this exists
//!
//! A `const` has no stable address: it is inlined at every use site, so it
//! may occupy as many addresses as it has textual occurrences. LLVM happened
//! to fold repeated occurrences of `ai::roster::FALLBACK` to one address;
//! Cranelift did not, and `is_fallback`'s `std::ptr::eq` search against it
//! matched nothing — a real mob silently read as unclaimed. The same shape
//! then shipped a second time in a different crate, by a different author:
//! `entity_sprite::ENTITY_SPRITES` was a `const`, and recovering a row's
//! index by `std::ptr::eq` against a returned reference matched nothing, so
//! two entity types drew zero pixels. `CLAUDE.md` records both; per its own
//! standard, a rule written in prose after the first incident is not a rule
//! — it has to be checkable. This is that check.
//!
//! # Why `syn` and not a hand-rolled scanner
//!
//! Detecting this needs two things a line-based grep cannot give cheaply: a
//! reliable `const` vs `static` classification for a declaration (which can
//! carry attributes, visibility qualifiers and multi-line generics), and the
//! two arguments of a call that is frequently split across several lines
//! (`crates/lodestone-render/src/entity.rs`'s `std::ptr::eq` call is four
//! lines; `crates/protocol/v770/tests/prototype_shape_seams.rs`'s are two).
//! `xtask::islands` already made this trade for the same reason — three
//! earlier scanners here hand-rolled a Rust lexer and each was wrong about
//! lifetimes, because `&'static str` opens what looks like an unterminated
//! char literal. This scanner parses every file with `syn::parse_file` and
//! walks the real AST with `syn::visit::Visit` instead.
//!
//! # Resolution model — read this before trusting a finding
//!
//! There is no type checker here, and no cross-crate `use` resolution: a
//! pointer-identity comparison's operand is matched to a declaration by its
//! **last path segment only**, against a workspace-wide index of every
//! `const`/`static` item name. This is a deliberate, narrow trade, in the
//! same spirit `xtask::islands` documents for its own name-based resolution:
//!
//! - It only sees a **direct** reference to the item's name inside the
//!   comparison itself — a bare path, optionally behind one `&` and/or one
//!   trailing `.as_ptr()` (or one `as *const _` / `as *mut _` cast for the
//!   binary-operator form). A comparison that goes through a local variable,
//!   a function's return value, or a loop iterator — `std::ptr::eq(ta.as_ptr(),
//!   tb.as_ptr())` where `ta`/`tb` are `let`-bound from a call — is invisible
//!   to it. That is a real blind spot, not an oversight: tracing an
//!   arbitrary call chain back to its source needs a type checker this tool
//!   does not have. Every occurrence of that indirect shape in this
//!   workspace was reviewed by hand (see the audit that added this scanner)
//!   and found to compare two calls that resolve to the exact same source
//!   occurrence of the same `const` — which is safe regardless of
//!   const-inlining, since it is one piece of compiled code executed twice,
//!   not two independent inlined copies.
//! - A name is flagged the moment **any** declaration of it anywhere in the
//!   workspace is a `const` — even if another declaration of the same name
//!   elsewhere is a `static`. This is deliberately the conservative
//!   direction for a gate: a false positive costs a look, a false negative
//!   costs a repeat of the incident this guard exists for.
//!
//! # Scope
//!
//! Catches: `std::ptr::eq(a, b)` / `ptr::eq(a, b)` / `core::ptr::eq(a, b)`,
//! `std::ptr::addr_eq` / `ptr::addr_eq`, and a `==`/`!=` comparison where
//! either side is an `as *const _` / `as *mut _` cast. Does **not** catch
//! `Arc::ptr_eq` / `Rc::ptr_eq` (an `Arc`/`Rc` is a heap allocation from
//! `Arc::new`/`Rc::new` at runtime — never a `const`-inlined address, so the
//! hazard this guard exists for cannot occur there; the census below found
//! sixteen call sites and confirmed none of them is a false negative for
//! that reason).
use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use syn::visit::Visit;
use syn::{
    Expr, ExprBinary, ExprCall, ImplItemConst, ItemConst, ItemStatic, TraitItemConst, Type,
};

/// Top-level directories this scan walks — the native workspace's members
/// plus the browser's own separate workspace (`web/`), so a hazard anywhere
/// the compiler would ever see it is in scope. `target/` is excluded at
/// every depth by [`collect_scan_files`], not listed here.
const SCAN_ROOTS: &[&str] = &["crates", "xtask", "web"];

/// A sanity floor on how many `.rs` files the walk must find, so a moved
/// directory or a bad root reads as "the walk is broken", never as "nothing
/// to scan". Measured at 1746 files under `crates/`, `xtask/`, `web/`
/// (excluding `target/`) the day this guard was added; set well under that.
const MIN_FILES_SCANNED: usize = 500;

/// Above this fraction of scanned files failing to parse, the run is
/// unreliable rather than merely missing a few files — mirrors
/// `xtask::islands`'s own tolerance, so a shared checkout with one file
/// mid-edit does not fail every agent's run, while a workspace-wide parse
/// regression still does.
const MAX_PARSE_FAILURE_FRACTION: f64 = 0.05;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ItemKind {
    Const,
    Static,
}

/// One resolved `const`/`static` declaration, kept for provenance in a
/// report (which file declared the name, as what kind).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Declaration {
    kind: ItemKind,
    file: String,
}

/// name -> every declaration of that name found anywhere in the scanned
/// tree. A name-based index, not a scoped one — see the module doc.
type NameIndex = BTreeMap<String, BTreeSet<Declaration>>;

/// Whether `name` resolves, across the whole index, to at least one `const`
/// declaration. That is sufficient to flag it: see the module doc for why
/// the conservative direction is chosen for a gate.
fn resolves_to_const(index: &NameIndex, name: &str) -> bool {
    index
        .get(name)
        .is_some_and(|decls| decls.iter().any(|d| d.kind == ItemKind::Const))
}

fn resolves_to_static_only(index: &NameIndex, name: &str) -> bool {
    index
        .get(name)
        .is_some_and(|decls| decls.iter().all(|d| d.kind == ItemKind::Static))
}

/// One operand of a pointer-identity comparison, reduced to a bare
/// identifier if its shape is one this scanner understands — see the module
/// doc's "Resolution model" for exactly which shapes those are.
#[derive(Debug, Clone)]
struct Operand {
    /// Best-effort rendering for a human reader, not a real pretty-printer.
    rendered: String,
    /// The bare identifier this operand reduces to, if its shape matched.
    ident: Option<String>,
}

fn render_shallow(expr: &Expr) -> String {
    match expr {
        Expr::Path(p) => p
            .path
            .segments
            .iter()
            .map(|s| s.ident.to_string())
            .collect::<Vec<_>>()
            .join("::"),
        Expr::Reference(r) => format!("&{}", render_shallow(&r.expr)),
        Expr::MethodCall(mc) => format!(
            "{}.{}({})",
            render_shallow(&mc.receiver),
            mc.method,
            if mc.args.is_empty() { "" } else { ".." }
        ),
        Expr::Cast(c) => format!("{} as _", render_shallow(&c.expr)),
        Expr::Field(f) => match &f.member {
            syn::Member::Named(name) => format!("{}.{}", render_shallow(&f.base), name),
            syn::Member::Unnamed(idx) => format!("{}.{}", render_shallow(&f.base), idx.index),
        },
        Expr::Paren(p) => render_shallow(&p.expr),
        Expr::Group(g) => render_shallow(&g.expr),
        _ => "<expr>".to_string(),
    }
}

/// Reduces `expr` to a bare identifier, stripping any number of `&`,
/// `.as_ptr()` and `as *const _` / `as *mut _` layers — the only shapes a
/// direct reference to a named item can take in the call sites this
/// workspace's real incidents and its current tree both use. See the module
/// doc's "Resolution model" for what this deliberately does not trace
/// through (a local variable, a function call, a loop iterator).
fn base_ident(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Reference(r) => base_ident(&r.expr),
        Expr::Paren(p) => base_ident(&p.expr),
        Expr::Group(g) => base_ident(&g.expr),
        Expr::Cast(c) => base_ident(&c.expr),
        Expr::MethodCall(mc) if mc.method == "as_ptr" && mc.args.is_empty() => {
            base_ident(&mc.receiver)
        }
        Expr::Path(p) if p.qself.is_none() => p.path.segments.last().map(|s| s.ident.to_string()),
        _ => None,
    }
}

fn operand_from(expr: &Expr) -> Operand {
    Operand {
        rendered: render_shallow(expr),
        ident: base_ident(expr),
    }
}

/// True for `T as *const _` / `T as *mut _` — the shape the module doc calls
/// the binary-operator form of this hazard. Strips a wrapping `(...)` first:
/// `(x as *const i32) == (&ANCHOR as *const i32)` is `Expr::Paren` around
/// each `Expr::Cast`, not a bare cast, and a real fixture uses exactly that
/// parenthesised form because `as` binds looser than `==`.
fn is_raw_pointer_cast(expr: &Expr) -> bool {
    match expr {
        Expr::Paren(p) => is_raw_pointer_cast(&p.expr),
        Expr::Group(g) => is_raw_pointer_cast(&g.expr),
        Expr::Cast(c) => matches!(c.ty.as_ref(), Type::Ptr(_)),
        _ => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallKind {
    /// `std::ptr::eq` / `ptr::eq` / `core::ptr::eq`.
    PtrEq,
    /// `std::ptr::addr_eq` / `ptr::addr_eq`.
    PtrAddrEq,
    /// A `==`/`!=` comparison with at least one `as *const _`/`as *mut _`
    /// side.
    RawPointerCompare,
}

impl CallKind {
    fn label(self) -> &'static str {
        match self {
            CallKind::PtrEq => "ptr::eq",
            CallKind::PtrAddrEq => "ptr::addr_eq",
            CallKind::RawPointerCompare => "raw pointer ==",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// At least one operand names a `const` item — the hazard this guard
    /// exists for.
    ConstViolation(Vec<String>),
    /// At least one operand names a `static` item and none names a `const`.
    StaticSafe(Vec<String>),
    /// Neither operand reduces to a name in the workspace index (a local
    /// variable, a struct field, a heap allocation, …) — outside this
    /// scanner's resolution model, not asserted either way.
    Unresolved,
}

#[derive(Debug, Clone)]
pub struct PtrConstHit {
    pub file: String,
    pub function: String,
    pub line: usize,
    pub kind: CallKind,
    pub left: String,
    pub right: String,
    pub verdict: Verdict,
}

#[derive(Debug, Default)]
pub struct PtrConstReport {
    pub files_scanned: usize,
    pub files_failed_to_parse: usize,
    pub const_names_indexed: usize,
    pub static_names_indexed: usize,
    pub hits: Vec<PtrConstHit>,
}

impl PtrConstReport {
    #[must_use]
    pub fn violations(&self) -> Vec<&PtrConstHit> {
        self.hits
            .iter()
            .filter(|h| matches!(h.verdict, Verdict::ConstViolation(_)))
            .collect()
    }
}

fn matches_ptr_eq_path(path: &syn::Path) -> Option<CallKind> {
    let segments: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
    if segments.len() < 2 {
        return None;
    }
    let last = segments.last()?.as_str();
    let penultimate = segments.get(segments.len() - 2)?.as_str();
    if penultimate != "ptr" {
        return None;
    }
    match last {
        "eq" => Some(CallKind::PtrEq),
        "addr_eq" => Some(CallKind::PtrAddrEq),
        _ => None,
    }
}

struct NameIndexVisitor<'a> {
    file: &'a str,
    index: &'a mut NameIndex,
}

impl NameIndexVisitor<'_> {
    fn record(&mut self, name: String, kind: ItemKind) {
        self.index.entry(name).or_default().insert(Declaration {
            kind,
            file: self.file.to_string(),
        });
    }
}

impl<'ast> Visit<'ast> for NameIndexVisitor<'_> {
    fn visit_item_const(&mut self, node: &'ast ItemConst) {
        self.record(node.ident.to_string(), ItemKind::Const);
        syn::visit::visit_item_const(self, node);
    }

    fn visit_item_static(&mut self, node: &'ast ItemStatic) {
        self.record(node.ident.to_string(), ItemKind::Static);
        syn::visit::visit_item_static(self, node);
    }

    fn visit_impl_item_const(&mut self, node: &'ast ImplItemConst) {
        self.record(node.ident.to_string(), ItemKind::Const);
        syn::visit::visit_impl_item_const(self, node);
    }

    fn visit_trait_item_const(&mut self, node: &'ast TraitItemConst) {
        self.record(node.ident.to_string(), ItemKind::Const);
        syn::visit::visit_trait_item_const(self, node);
    }
}

struct HitVisitor<'a> {
    file: &'a str,
    index: &'a NameIndex,
    /// Nearest enclosing fn/method name, for the census's "enclosing
    /// symbol" column. `<module scope>` when a hit is outside any function
    /// (a `const fn`-free item initialiser, unusual but not impossible).
    scope: Vec<String>,
    hits: Vec<PtrConstHit>,
}

impl HitVisitor<'_> {
    fn current_scope(&self) -> String {
        self.scope
            .last()
            .cloned()
            .unwrap_or_else(|| "<module scope>".to_string())
    }

    fn line_of(span: proc_macro2::Span) -> usize {
        span.start().line
    }

    fn verdict_for(&self, left: &Operand, right: &Operand) -> Verdict {
        let mut const_hits = Vec::new();
        let mut static_hits = Vec::new();
        for operand in [left, right] {
            let Some(ident) = &operand.ident else {
                continue;
            };
            if resolves_to_const(self.index, ident) {
                const_hits.push(ident.clone());
            } else if resolves_to_static_only(self.index, ident) {
                static_hits.push(ident.clone());
            }
        }
        if !const_hits.is_empty() {
            Verdict::ConstViolation(const_hits)
        } else if !static_hits.is_empty() {
            Verdict::StaticSafe(static_hits)
        } else {
            Verdict::Unresolved
        }
    }

    fn record_call(&mut self, kind: CallKind, call: &ExprCall, span: proc_macro2::Span) {
        let args: Vec<&Expr> = call.args.iter().collect();
        let (Some(a), Some(b)) = (args.first(), args.get(1)) else {
            return;
        };
        let left = operand_from(a);
        let right = operand_from(b);
        let verdict = self.verdict_for(&left, &right);
        self.hits.push(PtrConstHit {
            file: self.file.to_string(),
            function: self.current_scope(),
            line: Self::line_of(span),
            kind,
            left: left.rendered,
            right: right.rendered,
            verdict,
        });
    }

    fn record_binary(&mut self, bin: &ExprBinary, span: proc_macro2::Span) {
        let left = operand_from(&bin.left);
        let right = operand_from(&bin.right);
        let verdict = self.verdict_for(&left, &right);
        self.hits.push(PtrConstHit {
            file: self.file.to_string(),
            function: self.current_scope(),
            line: Self::line_of(span),
            kind: CallKind::RawPointerCompare,
            left: left.rendered,
            right: right.rendered,
            verdict,
        });
    }
}

impl<'ast> Visit<'ast> for HitVisitor<'_> {
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        self.scope.push(node.sig.ident.to_string());
        syn::visit::visit_item_fn(self, node);
        self.scope.pop();
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        self.scope.push(node.sig.ident.to_string());
        syn::visit::visit_impl_item_fn(self, node);
        self.scope.pop();
    }

    fn visit_trait_item_fn(&mut self, node: &'ast syn::TraitItemFn) {
        self.scope.push(node.sig.ident.to_string());
        syn::visit::visit_trait_item_fn(self, node);
        self.scope.pop();
    }

    fn visit_expr_call(&mut self, node: &'ast ExprCall) {
        if let Expr::Path(p) = node.func.as_ref() {
            if let Some(kind) = matches_ptr_eq_path(&p.path) {
                self.record_call(kind, node, node.func.span_for_diagnostics());
            }
        }
        syn::visit::visit_expr_call(self, node);
    }

    fn visit_expr_binary(&mut self, node: &'ast ExprBinary) {
        let is_eq_like = matches!(
            node.op,
            syn::BinOp::Eq(_) | syn::BinOp::Ne(_)
        );
        if is_eq_like && (is_raw_pointer_cast(&node.left) || is_raw_pointer_cast(&node.right)) {
            self.record_binary(node, node.op.span_for_diagnostics());
        }
        syn::visit::visit_expr_binary(self, node);
    }

    /// **Load-bearing.** `syn::visit::Visit`'s default handling of a macro
    /// invocation is opaque — it visits the macro's *path* and stops, never
    /// looking inside the token stream, because a macro can define its own
    /// grammar. Without this override, every `std::ptr::eq` call written
    /// inside an `assert!`/`assert_eq!`/`assert_ne!`/`debug_assert*!` — which
    /// is most of them: measured on this workspace's own roster tests, 8 of
    /// the 12 real `std::ptr::eq` call sites are wrapped this way — would be
    /// invisible to this scanner, silently, with no parse error and no
    /// warning. That is exactly the failure shape CLAUDE.md records for the
    /// reference wasm-check script's swallowed grep exit code: a detector
    /// that structurally cannot see most of its own subject and reports a
    /// clean census anyway.
    ///
    /// The fix: best-effort re-parse the macro's body as a comma-separated
    /// expression list and re-enter each expression through this same
    /// visitor. That grammar is what `assert!`/`assert_eq!`/`assert_ne!`/the
    /// `debug_assert*!` family actually use (condition, then optional
    /// format args, all valid `Expr`s), so it recovers exactly the macros
    /// this hazard idiomatically lives in. A macro with a genuinely
    /// different grammar (`vec!`, `matches!`, `select!`, a custom macro)
    /// fails to parse this way and is silently skipped — the same
    /// "cannot model an arbitrary macro grammar in general" trade
    /// `xtask::islands` documents for its own macro handling, narrowed here
    /// to the one shape that matters for this hazard rather than attempted
    /// for every macro that exists.
    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        if let Ok(exprs) = node.parse_body_with(
            syn::punctuated::Punctuated::<Expr, syn::Token![,]>::parse_terminated,
        ) {
            for expr in &exprs {
                self.visit_expr(expr);
            }
        }
        syn::visit::visit_macro(self, node);
    }
}

/// A `Span` accessor that works the same whether the expression carries a
/// single token or several — every call site above needs only "some span
/// inside this node", not the exact extent.
trait SpanForDiagnostics {
    fn span_for_diagnostics(&self) -> proc_macro2::Span;
}

impl SpanForDiagnostics for Expr {
    fn span_for_diagnostics(&self) -> proc_macro2::Span {
        use syn::spanned::Spanned;
        self.span()
    }
}

impl SpanForDiagnostics for syn::BinOp {
    fn span_for_diagnostics(&self) -> proc_macro2::Span {
        use syn::spanned::Spanned;
        self.span()
    }
}

fn collect_scan_files(workspace_root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for root in SCAN_ROOTS {
        let dir = workspace_root.join(root);
        if dir.is_dir() {
            walk_rs_files(&dir, &mut files)?;
        }
    }
    files.sort();
    Ok(files)
}

fn walk_rs_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        // A shared checkout: a directory can vanish between the parent's
        // listing and this recursive call landing on it (another agent's
        // `git worktree remove`, a rename mid-flight). Nothing to scan there
        // is not a defect in the scan.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err).with_context(|| format!("read dir {}", dir.display())),
    };
    for entry in entries {
        let entry = entry.with_context(|| format!("read dir entry under {}", dir.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("stat {}", path.display()))?;
        if file_type.is_dir() {
            if path.file_name().and_then(|n| n.to_str()) == Some("target") {
                continue;
            }
            walk_rs_files(&path, out)?;
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
    Ok(())
}

/// Parses every scanned file once, keeping the `(relative path, AST)` pairs
/// so the two passes below (index, then detect) never re-parse. A file that
/// disappears between the walk and the read is tolerated the same way
/// `scan_confinement_dir` tolerates it; a genuine parse error is counted and
/// the file skipped, subject to [`MAX_PARSE_FAILURE_FRACTION`].
fn parse_all(files: &[PathBuf], workspace_root: &Path) -> Result<(Vec<(String, syn::File)>, usize)> {
    let mut parsed = Vec::with_capacity(files.len());
    let mut failed = 0usize;
    for path in files {
        let rel = path
            .strip_prefix(workspace_root)
            .unwrap_or(path)
            .display()
            .to_string();
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => return Err(err).with_context(|| format!("read {}", path.display())),
        };
        match syn::parse_file(&text) {
            Ok(ast) => parsed.push((rel, ast)),
            Err(_) => failed += 1,
        }
    }
    Ok((parsed, failed))
}

/// Runs the full scan over a workspace tree, with the file-count floor that
/// tells a broken walk apart from a legitimately tiny tree. Production and
/// the CLI always go through this. The unit tests below exercise
/// [`scan_paths`] directly against small fixture trees that are honestly
/// below the floor by construction, and exercise the floor itself
/// separately, so the two concerns are never conflated in one assertion.
pub fn scan_workspace(workspace_root: &Path) -> Result<PtrConstReport> {
    let files = collect_scan_files(workspace_root)?;
    if files.len() < MIN_FILES_SCANNED {
        bail!(
            "ptr-const scan found only {} .rs files under {:?} (floor: {MIN_FILES_SCANNED}) — \
             the walk is broken, not the tree; this must FAIL, not report a clean pass over \
             nothing",
            files.len(),
            SCAN_ROOTS
        );
    }
    scan_paths(&files, workspace_root)
}

/// The scan itself, given an already-collected file list: parse, index every
/// `const`/`static` name, then find every pointer-identity comparison and
/// resolve it against that index. This is both the census (every hit is in
/// the returned report, tagged with its verdict) and the raw material for
/// the gate in [`run_check_ptr_const`], which is the same data with a
/// pass/fail read off it. Split out from [`scan_workspace`] so a unit test
/// can drive a small fixture tree without tripping the file-count floor,
/// which exists to catch a *broken walk*, not a small input.
fn scan_paths(files: &[PathBuf], workspace_root: &Path) -> Result<PtrConstReport> {
    let (parsed, failed) = parse_all(files, workspace_root)?;
    let failure_fraction = failed as f64 / files.len() as f64;
    if failure_fraction > MAX_PARSE_FAILURE_FRACTION {
        bail!(
            "ptr-const scan: {failed}/{} files failed to parse ({:.1}%), over the \
             {:.0}% tolerance — a detector that could not look at most of the tree has \
             measured nothing, not passed",
            files.len(),
            failure_fraction * 100.0,
            MAX_PARSE_FAILURE_FRACTION * 100.0
        );
    }

    let mut index: NameIndex = BTreeMap::new();
    for (file, ast) in &parsed {
        let mut visitor = NameIndexVisitor {
            file,
            index: &mut index,
        };
        visitor.visit_file(ast);
    }

    let const_names_indexed = index
        .values()
        .filter(|decls| decls.iter().any(|d| d.kind == ItemKind::Const))
        .count();
    let static_names_indexed = index
        .values()
        .filter(|decls| decls.iter().all(|d| d.kind == ItemKind::Static))
        .count();

    let mut hits = Vec::new();
    for (file, ast) in &parsed {
        let mut visitor = HitVisitor {
            file,
            index: &index,
            scope: Vec::new(),
            hits: Vec::new(),
        };
        visitor.visit_file(ast);
        hits.append(&mut visitor.hits);
    }
    hits.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)));

    Ok(PtrConstReport {
        files_scanned: parsed.len(),
        files_failed_to_parse: failed,
        const_names_indexed,
        static_names_indexed,
        hits,
    })
}

#[must_use]
pub fn format_report(report: &PtrConstReport) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "== Lodestone pointer-identity / const guard ==");
    let _ = writeln!(
        out,
        "files scanned: {} (parse failures: {})",
        report.files_scanned, report.files_failed_to_parse
    );
    let _ = writeln!(
        out,
        "workspace names indexed: {} const, {} static",
        report.const_names_indexed, report.static_names_indexed
    );
    let _ = writeln!(out, "pointer-identity comparisons found: {}", report.hits.len());
    let _ = writeln!(out);
    for hit in &report.hits {
        let verdict = match &hit.verdict {
            Verdict::ConstViolation(names) => format!("CONST VIOLATION ({})", names.join(", ")),
            Verdict::StaticSafe(names) => format!("safe (static: {})", names.join(", ")),
            Verdict::Unresolved => "safe (unresolved operand)".to_string(),
        };
        let _ = writeln!(
            out,
            "  {}:{} in {} — {}({}, {}) — {verdict}",
            hit.file, hit.line, hit.function, hit.kind.label(), hit.left, hit.right
        );
    }
    let _ = writeln!(out);
    let violations = report.violations();
    if violations.is_empty() {
        let _ = writeln!(
            out,
            "RESULT: PASS — no pointer-identity comparison targets a const item."
        );
    } else {
        let _ = writeln!(
            out,
            "RESULT: FAIL — {} pointer-identity comparison(s) target a const item:",
            violations.len()
        );
        for hit in &violations {
            let _ = writeln!(out, "  - {}:{} in {}", hit.file, hit.line, hit.function);
        }
    }
    out
}

/// The gate: scan, print the full census (so a passing run still shows what
/// it looked at, per the "no findings must never share a value with 'could
/// not look'" rule), and fail loudly on any const-targeted comparison.
pub fn run_check_ptr_const(workspace_root: &Path) -> Result<()> {
    let report = scan_workspace(workspace_root)?;
    print!("{}", format_report(&report));
    let violations = report.violations();
    if !violations.is_empty() {
        bail!(
            "RESULT: FAIL — {} pointer-identity comparison(s) target a const item; see the \
             census above for file:line and the operand that resolved to one. Fix: change the \
             `const` to a `static` (a `static` has exactly one address for its whole \
             `'static` lifetime; a `const` is inlined at every use site and may not).",
            violations.len()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Workspace {
        dir: tempfile::TempDir,
    }

    impl Workspace {
        fn new() -> Result<Self> {
            Ok(Self {
                dir: tempfile::tempdir()?,
            })
        }

        fn write(&self, relative: &str, contents: &str) -> Result<()> {
            let path = self.dir.path().join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(path, contents)?;
            Ok(())
        }

        fn root(&self) -> &Path {
            self.dir.path()
        }
    }

    /// Runs the scan against a fixture tree, deliberately bypassing
    /// [`scan_workspace`]'s file-count floor: every fixture below is a
    /// handful of files by design, and the floor exists to catch a *broken
    /// walk* over the real workspace, not to reject a small, honest input.
    /// [`a_workspace_below_the_file_floor_is_a_hard_failure`] exercises the
    /// floor itself, through `scan_workspace`, so the two concerns stay
    /// separate.
    fn scan_fixture(root: &Path) -> Result<PtrConstReport> {
        let files = collect_scan_files(root)?;
        scan_paths(&files, root)
    }

    /// The direct-reference shape the two real incidents both had: a
    /// pointer-identity comparison naming a `const` item by name inside the
    /// call. Must be flagged.
    #[test]
    fn planted_const_violation_is_found_and_named() -> Result<()> {
        let ws = Workspace::new()?;
        ws.write(
            "crates/fixture/src/lib.rs",
            r#"
                pub const TABLE: &[i32] = &[1, 2, 3];

                pub fn is_default(candidate: &i32) -> bool {
                    std::ptr::eq(candidate, &TABLE[0])
                }

                pub fn same_table(a: &[i32], b: &[i32]) -> bool {
                    std::ptr::eq(a.as_ptr(), TABLE.as_ptr())
                }
            "#,
        )?;
        let report = scan_fixture(ws.root())?;
        let violations = report.violations();
        assert_eq!(
            violations.len(),
            1,
            "expected exactly the TABLE.as_ptr() call to be flagged, got: {:#?}",
            report.hits
        );
        assert_eq!(violations[0].function, "same_table");
        assert!(matches!(&violations[0].verdict, Verdict::ConstViolation(names) if names == &["TABLE".to_string()]));
        Ok(())
    }

    /// The exact fix CLAUDE.md records: turning the `const` into a `static`
    /// must clear the finding. This is the "restore and re-run" half of the
    /// control.
    #[test]
    fn the_same_shape_with_static_is_not_flagged() -> Result<()> {
        let ws = Workspace::new()?;
        ws.write(
            "crates/fixture/src/lib.rs",
            r#"
                pub static TABLE: &[i32] = &[1, 2, 3];

                pub fn same_table(a: &[i32], b: &[i32]) -> bool {
                    std::ptr::eq(a.as_ptr(), TABLE.as_ptr())
                }
            "#,
        )?;
        let report = scan_fixture(ws.root())?;
        assert!(
            report.violations().is_empty(),
            "a static target must never be flagged: {:#?}",
            report.violations()
        );
        let hit = report
            .hits
            .iter()
            .find(|h| h.function == "same_table")
            .expect("the ptr::eq call was scanned");
        assert!(matches!(&hit.verdict, Verdict::StaticSafe(names) if names == &["TABLE".to_string()]));
        Ok(())
    }

    /// A comparison entirely between local variables — the shape every
    /// currently-safe roster test in the real workspace uses — must not be
    /// flagged, because this scanner does not (and honestly cannot, without
    /// a type checker) trace a local binding back to its source. This is
    /// the documented blind spot, asserted so a future change to the
    /// resolver cannot silently start guessing here.
    #[test]
    fn a_comparison_between_local_variables_is_unresolved_not_flagged() -> Result<()> {
        let ws = Workspace::new()?;
        ws.write(
            "crates/fixture/src/lib.rs",
            r#"
                pub const ZOMBIE: &[i32] = &[1];

                fn lookup(species: &str) -> &'static [i32] {
                    match species {
                        "zombie" | "husk" => ZOMBIE,
                        _ => &[],
                    }
                }

                pub fn same(a: &str, b: &str) -> bool {
                    let (ta, tb) = (lookup(a), lookup(b));
                    std::ptr::eq(ta.as_ptr(), tb.as_ptr())
                }
            "#,
        )?;
        let report = scan_fixture(ws.root())?;
        assert!(
            report.violations().is_empty(),
            "a local-variable comparison must read as Unresolved, not a violation: {:#?}",
            report.violations()
        );
        let hit = report
            .hits
            .iter()
            .find(|h| h.function == "same")
            .expect("the ptr::eq call was scanned");
        assert_eq!(hit.verdict, Verdict::Unresolved);
        Ok(())
    }

    /// `Arc::ptr_eq` compares heap allocations, never a `const`-inlined
    /// address — it must not even be matched as a candidate, regardless of
    /// what it is compared against.
    #[test]
    fn arc_ptr_eq_is_out_of_scope_even_against_a_const_named_field() -> Result<()> {
        let ws = Workspace::new()?;
        ws.write(
            "crates/fixture/src/lib.rs",
            r#"
                use std::sync::Arc;

                pub fn same(a: &Arc<i32>, b: &Arc<i32>) -> bool {
                    Arc::ptr_eq(a, b)
                }
            "#,
        )?;
        let report = scan_fixture(ws.root())?;
        assert!(
            report.hits.is_empty(),
            "Arc::ptr_eq must not be treated as a ptr::eq call at all: {:#?}",
            report.hits
        );
        Ok(())
    }

    /// The raw-pointer-cast `==` form, the module doc's second shape.
    #[test]
    fn raw_pointer_cast_equality_against_a_const_is_found() -> Result<()> {
        let ws = Workspace::new()?;
        ws.write(
            "crates/fixture/src/lib.rs",
            r#"
                pub const ANCHOR: i32 = 0;

                pub fn same(x: &i32) -> bool {
                    (x as *const i32) == (&ANCHOR as *const i32)
                }
            "#,
        )?;
        let report = scan_fixture(ws.root())?;
        assert_eq!(
            report.violations().len(),
            1,
            "expected the raw-pointer cast comparison to be flagged: {:#?}",
            report.hits
        );
        Ok(())
    }

    /// A name that is `const` in one file and `static` in an unrelated one
    /// is still flagged when the comparison's operand matches that name —
    /// documents the conservative-direction choice in the module doc rather
    /// than leaving it implicit.
    #[test]
    fn an_ambiguous_name_is_flagged_conservatively() -> Result<()> {
        let ws = Workspace::new()?;
        ws.write(
            "crates/fixture/src/a.rs",
            r#"
                pub const SHARED: i32 = 0;
            "#,
        )?;
        ws.write(
            "crates/fixture/src/b.rs",
            r#"
                pub static SHARED: i32 = 1;

                pub fn same(x: &i32) -> bool {
                    std::ptr::eq(x, &SHARED)
                }
            "#,
        )?;
        let report = scan_fixture(ws.root())?;
        assert_eq!(
            report.violations().len(),
            1,
            "an ambiguous name must resolve to the conservative (const) reading: {:#?}",
            report.hits
        );
        Ok(())
    }

    /// The floor exists so a broken walk (wrong root, everything renamed)
    /// fails loudly instead of reporting a clean pass over zero files —
    /// the exact "no findings shares a value with could not look" trap
    /// CLAUDE.md records for the reference wasm-check script.
    #[test]
    fn a_workspace_below_the_file_floor_is_a_hard_failure() -> Result<()> {
        let ws = Workspace::new()?;
        ws.write("crates/fixture/src/lib.rs", "pub const X: i32 = 0;")?;
        let err = scan_workspace(ws.root()).expect_err("one file must be under the floor");
        assert!(
            err.to_string().contains("floor"),
            "error should name the floor it tripped: {err}"
        );
        Ok(())
    }
}
