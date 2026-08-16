//! `cargo xtask islands` — a `syn`-based scanner for unwired subsystems.
//!
//! See `docs/island-detection.md` for the full writeup: what each finding
//! category means, why resolution is name-based rather than type-based, and
//! how to run the planted-island control before trusting a report.
//!
//! # Why `syn` and not a hand-rolled scanner
//!
//! Three earlier scanners in this repo hand-rolled a Rust lexer and each was
//! wrong about lifetimes: `&'static str` opened a "char literal" flag that
//! never closed and silently disabled comment detection. This scanner parses
//! every file with `syn::parse_file` and walks the real AST with
//! `syn::visit::Visit`, so it cannot be fooled by a lifetime, a string
//! literal containing `//`, or a raw string containing `"`.
//!
//! # Resolution model — read this before trusting a finding
//!
//! There is no type checker here: a function call is matched to a
//! definition by its **last path segment / method name only**. This is a
//! deliberate, honest trade-off, not an oversight:
//!
//! - It has very few false positives: a name that is genuinely never
//!   written anywhere in the workspace as a call is a strong signal.
//! - It has real false negatives: two unrelated functions sharing a common
//!   name (`new`, `tick`, `run`) hide each other. A distinctively-named
//!   island (`tick_thunder_for_chunk`, `RecipeToastQueue::push`) is exactly
//!   what this method is good at; a generically-named one is invisible to
//!   it. This is the same trade CLAUDE.md's own worked examples make when
//!   they call out that a field-name grep "finds every struct that has
//!   one" — the mitigation there is to read the enclosing symbol, which a
//!   human still has to do for every finding this tool prints.
//!
//! Struct field reads and default-only assignments are resolved the same
//! way, by bare field name, for the same reason and with the same caveat.
use crate::cargo_metadata;
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use syn::visit::Visit;
use syn::{Attribute, Expr, Fields, ImplItem, Item, Lit, Member, Meta};

/// Method names whose *receiver* is a collection being grown, not read.
/// `self.field.push(..)` / `.insert(..)` / `.extend(..)` / `.entry(..)` is a
/// real, non-default mutation of `field` that no `Expr::Assign` or
/// `Expr::Binary` syntax captures -- measured false positive:
/// `Brain::{sensors, behaviors, activity_requirements,
/// activity_any_requirements, activity_memories_to_erase_when_stopped,
/// active_activities}` (`crates/lodestone-entity/src/brain/mod.rs`) are all
/// initialised empty in `Brain::default()`-shaped constructors and grown
/// exclusively this way in `add_sensor`/`add_activity`/`add_activity_any_of`,
/// so without this, every one of them reads as "every production assignment
/// is default-like" despite being populated on every real construction.
const COLLECTION_GROWTH_METHODS: &[&str] = &["push", "insert", "extend", "entry"];

/// Trait names whose `impl` methods are excluded from the dead-function
/// scan. These are reached by the compiler (derive expansion), by `dyn`
/// dispatch through the trait object, or by a framework's own reflection —
/// none of which leaves a textual call site with this method's name. Every
/// one of these is a documented false-positive source, not a guess: flagging
/// `fn fmt` on a `Display` impl as "dead" because nothing calls `.fmt()`
/// directly is exactly the kind of finding CLAUDE.md asks us to explain away
/// rather than report.
const WELL_KNOWN_TRAITS: &[&str] = &[
    "Debug",
    "Display",
    "Default",
    "Drop",
    "Clone",
    "Copy",
    "PartialEq",
    "Eq",
    "PartialOrd",
    "Ord",
    "Hash",
    "From",
    "TryFrom",
    "Into",
    "TryInto",
    "Iterator",
    "IntoIterator",
    "DoubleEndedIterator",
    "ExactSizeIterator",
    "FromIterator",
    "Extend",
    "Deref",
    "DerefMut",
    "Index",
    "IndexMut",
    "Add",
    "AddAssign",
    "Sub",
    "SubAssign",
    "Mul",
    "MulAssign",
    "Div",
    "DivAssign",
    "Rem",
    "Neg",
    "Not",
    "BitAnd",
    "BitOr",
    "BitXor",
    "AsRef",
    "AsMut",
    "Serialize",
    "Deserialize",
    "Visit",
    "VisitMut",
    "Future",
    "Component",
    "Resource",
    "Encode",
    "Decode",
    "Packet",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Realm {
    Prod,
    Test,
}

#[derive(Debug, Clone, Copy, Default)]
struct Counts {
    prod: u32,
    test: u32,
}

fn bump(map: &mut BTreeMap<String, Counts>, key: String, realm: Realm) {
    let entry = map.entry(key).or_default();
    match realm {
        Realm::Prod => entry.prod += 1,
        Realm::Test => entry.test += 1,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FnKind {
    Free,
    Method,
    TraitMethod,
}

#[derive(Debug, Clone)]
struct FnDef {
    crate_name: String,
    file: String,
    name: String,
    kind: FnKind,
    has_allow_dead_code: bool,
}

#[derive(Debug, Clone)]
struct FieldDef {
    crate_name: String,
    file: String,
    struct_name: String,
    field_name: String,
    has_allow_dead_code: bool,
    derive_opaque_reader: bool,
}

/// Derives whose macro-generated code genuinely reads (or fills in) every
/// field, through code this scanner never sees because it never expands
/// macros. `Encode`/`Decode` are the load-bearing case: essentially every
/// struct under `crates/protocol/*/src/packets/` derives one or both of
/// them, because that derive *is* what makes it a wire packet -- the
/// generated `encode` reads every field to serialize it, the generated
/// `decode` writes every field from the wire. A "zero production readers"
/// finding on such a struct is reporting the derive's own read, not an
/// island, so fields of a struct carrying any of these derives are excluded
/// from the dead-field and default-only-field findings entirely (see
/// `docs/island-detection.md`).
const FIELD_OPAQUE_READER_DERIVES: &[&str] = &["Encode", "Decode", "Serialize", "Deserialize"];

fn struct_has_field_opaque_reader_derive(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        if !attr.path().is_ident("derive") {
            return false;
        }
        match &attr.meta {
            Meta::List(list) => FIELD_OPAQUE_READER_DERIVES
                .iter()
                .any(|name| token_stream_has_ident(list.tokens.clone(), name)),
            _ => false,
        }
    })
}

#[derive(Debug, Clone)]
pub struct AllowDeadCodeSite {
    pub crate_name: String,
    pub file: String,
    pub kind: &'static str,
    pub name: String,
}

#[derive(Debug, Clone)]
struct StructLiteralRecord {
    struct_name: String,
    explicit_fields: Vec<(String, bool)>,
    has_rest: bool,
    rest_is_default_like: bool,
    realm: Realm,
}

#[derive(Debug, Clone)]
struct FieldAssignRecord {
    field_name: String,
    is_default_like: bool,
    realm: Realm,
}

#[derive(Default)]
struct Collected {
    fn_defs: Vec<FnDef>,
    excluded_trait_impl_methods: usize,
    excluded_ffi_or_entrypoint: usize,
    field_defs: Vec<FieldDef>,
    allow_dead_code: Vec<AllowDeadCodeSite>,
    call_counts: BTreeMap<String, Counts>,
    field_read_counts: BTreeMap<String, Counts>,
    struct_literals: Vec<StructLiteralRecord>,
    field_assigns: Vec<FieldAssignRecord>,
    parse_errors: Vec<(String, String, String)>,
    files_scanned: usize,
    files_scanned_by_crate: BTreeMap<String, usize>,
}

fn has_path_ident_test(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path()
            .segments
            .last()
            .map(|seg| {
                let ident = seg.ident.to_string();
                ident == "test" || ident.ends_with("_test") || ident.ends_with("Test")
            })
            .unwrap_or(false)
    })
}

fn token_stream_has_test_ident(tokens: proc_macro2::TokenStream) -> bool {
    tokens.into_iter().any(|tree| match tree {
        proc_macro2::TokenTree::Ident(ident) => ident == "test",
        proc_macro2::TokenTree::Group(group) => token_stream_has_test_ident(group.stream()),
        _ => false,
    })
}

fn has_cfg_test(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        if !attr.path().is_ident("cfg") {
            return false;
        }
        match &attr.meta {
            Meta::List(list) => token_stream_has_test_ident(list.tokens.clone()),
            _ => false,
        }
    })
}

fn has_allow_dead_code(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        if !attr.path().is_ident("allow") {
            return false;
        }
        match &attr.meta {
            Meta::List(list) => token_stream_has_ident(list.tokens.clone(), "dead_code"),
            _ => false,
        }
    })
}

fn token_stream_has_ident(tokens: proc_macro2::TokenStream, wanted: &str) -> bool {
    tokens.into_iter().any(|tree| match tree {
        proc_macro2::TokenTree::Ident(ident) => ident == wanted,
        proc_macro2::TokenTree::Group(group) => token_stream_has_ident(group.stream(), wanted),
        _ => false,
    })
}

/// Registers `text` (an attribute's string-literal payload) as a reference
/// to whatever production item shares its name, if `text` itself looks like
/// a plausible bare identifier or `path::to::identifier` -- not an arbitrary
/// string that happens to appear in an attribute. See `visit_attribute`.
fn bump_string_literal_reference(text: &str, map: &mut BTreeMap<String, Counts>, realm: Realm) {
    let last = text.rsplit("::").next().unwrap_or(text);
    let is_plausible_ident = !last.is_empty()
        && last
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && last.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if is_plausible_ident {
        bump(map, last.to_string(), realm);
    }
}

/// Lexical, grammar-agnostic call detector: bumps `ident(` and `.ident(`
/// wherever they appear in `tokens`, recursing into every nested group. See
/// `visit_macro` for why this exists (macros like `tokio::select!` whose
/// body is not a flat expression list) and its own limits: it cannot tell a
/// real call from a tuple-struct/enum pattern that happens to look like one
/// (`Some(x)` in a `match` arm inside the macro), which is an acceptable
/// false-positive-toward-"used" in a scanner whose whole design already
/// prefers under-reporting dead code to over-reporting it.
fn scan_call_like_tokens(
    tokens: proc_macro2::TokenStream,
    map: &mut BTreeMap<String, Counts>,
    realm: Realm,
) {
    let items: Vec<proc_macro2::TokenTree> = tokens.into_iter().collect();
    for i in 0..items.len() {
        if let proc_macro2::TokenTree::Group(group) = &items[i] {
            scan_call_like_tokens(group.stream(), map, realm);
        }
        if let proc_macro2::TokenTree::Ident(ident) = &items[i] {
            let is_call = matches!(
                items.get(i + 1),
                Some(proc_macro2::TokenTree::Group(g)) if g.delimiter() == proc_macro2::Delimiter::Parenthesis
            );
            if is_call {
                bump(map, ident.to_string(), realm);
            }
        }
    }
}

fn record_string_literal_references(
    tokens: proc_macro2::TokenStream,
    map: &mut BTreeMap<String, Counts>,
    realm: Realm,
) {
    for tree in tokens {
        match tree {
            proc_macro2::TokenTree::Literal(lit) => {
                if let Ok(Lit::Str(s)) = syn::parse_str::<Lit>(&lit.to_string()) {
                    bump_string_literal_reference(&s.value(), map, realm);
                }
            }
            proc_macro2::TokenTree::Group(group) => {
                record_string_literal_references(group.stream(), map, realm);
            }
            _ => {}
        }
    }
}

/// Best-effort "does this expression look like a default value" check. See
/// the module doc for why this is a heuristic and not a type-checked
/// evaluation: we have no `Default` impl to consult, only syntax.
fn expr_is_default_like(expr: &Expr) -> bool {
    match expr {
        Expr::Lit(lit) => match &lit.lit {
            Lit::Int(i) => i.base10_digits() == "0",
            Lit::Float(f) => f.base10_digits().parse::<f64>().unwrap_or(1.0) == 0.0,
            Lit::Bool(b) => !b.value,
            Lit::Str(s) => s.value().is_empty(),
            _ => false,
        },
        Expr::Path(p) => p
            .path
            .segments
            .last()
            .map(|s| s.ident == "None")
            .unwrap_or(false),
        Expr::Call(c) => expr_call_is_default_like(c),
        Expr::Macro(m) => m.mac.path.is_ident("vec") && m.mac.tokens.is_empty(),
        Expr::Paren(p) => expr_is_default_like(&p.expr),
        _ => false,
    }
}

/// Types whose zero-arg `::new()` is conventionally an empty/inert value,
/// same as `Default::default()` would give them. Deliberately narrow: an
/// arbitrary type's `::new()` is not safe to assume is "the default" just
/// because it takes no arguments -- measured false positive in this
/// workspace, `ChunkBatchSizeCalculator::new()`
/// (`crates/protocol/v770/src/adapter/mod.rs`) is a fully-initialized,
/// meaningful calculator, not a placeholder, and treating any bare `::new()`
/// as default-like flagged `ChunkBatchState::calculator` as "every
/// production assignment is default" when it has exactly one assignment
/// site, full stop -- a "0 non-default of 1" reading that was really "1 of
/// 1, mis-scored".
const EMPTY_BY_CONVENTION_NEW_TYPES: &[&str] = &[
    "Vec",
    "VecDeque",
    "HashMap",
    "HashSet",
    "BTreeMap",
    "BTreeSet",
    "String",
    "Box",
    "Option",
];

fn expr_call_is_default_like(c: &syn::ExprCall) -> bool {
    let Expr::Path(p) = &*c.func else {
        return false;
    };
    let Some(last) = p.path.segments.last() else {
        return false;
    };
    match last.ident.to_string().as_str() {
        "default" => true,
        "new" => {
            c.args.is_empty()
                && p.path.segments.len() >= 2
                && p.path
                    .segments
                    .iter()
                    .nth(p.path.segments.len() - 2)
                    .is_some_and(|seg| EMPTY_BY_CONVENTION_NEW_TYPES.contains(&seg.ident.to_string().as_str()))
        }
        _ => false,
    }
}

/// Whether a struct-update-syntax `..rest` expression looks like a fresh
/// default rather than a copy of an existing value (`..existing_instance`).
/// Only the former licenses treating the fields it fills in as default.
fn rest_is_default_like(expr: &Expr) -> bool {
    match expr {
        Expr::Call(c) => expr_call_is_default_like(c),
        Expr::Paren(p) => rest_is_default_like(&p.expr),
        _ => false,
    }
}

struct Collector<'a> {
    crate_name: &'a str,
    file: &'a str,
    realm: Realm,
    out: &'a mut Collected,
}

impl<'a> Collector<'a> {
    fn record_allow_dead_code(&mut self, kind: &'static str, name: &str, attrs: &[Attribute]) {
        if has_allow_dead_code(attrs) {
            self.out.allow_dead_code.push(AllowDeadCodeSite {
                crate_name: self.crate_name.to_string(),
                file: self.file.to_string(),
                kind,
                name: name.to_string(),
            });
        }
    }

    fn record_fn_def(&mut self, sig: &syn::Signature, attrs: &[Attribute], kind: FnKind) {
        self.record_allow_dead_code("fn", &sig.ident.to_string(), attrs);
        let is_ffi = attrs.iter().any(|a| a.path().is_ident("no_mangle")) || sig.abi.is_some();
        if is_ffi || sig.ident == "main" {
            self.out.excluded_ffi_or_entrypoint += 1;
            return;
        }
        self.out.fn_defs.push(FnDef {
            crate_name: self.crate_name.to_string(),
            file: self.file.to_string(),
            name: sig.ident.to_string(),
            kind,
            has_allow_dead_code: has_allow_dead_code(attrs),
        });
    }
}

impl<'a, 'ast> Visit<'ast> for Collector<'a> {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        let prev = self.realm;
        if has_cfg_test(&node.attrs) {
            self.realm = Realm::Test;
        }
        syn::visit::visit_item_mod(self, node);
        self.realm = prev;
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        let prev = self.realm;
        let is_test_fn = has_path_ident_test(&node.attrs) || has_cfg_test(&node.attrs);
        if is_test_fn {
            self.realm = Realm::Test;
        } else if self.realm == Realm::Prod {
            self.record_fn_def(&node.sig, &node.attrs, FnKind::Free);
        }
        syn::visit::visit_item_fn(self, node);
        self.realm = prev;
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        // The definition itself (dead/tested-only classification, exclusion
        // for well-known traits) is recorded by `visit_item_impl` below,
        // which has the enclosing `impl`'s trait in scope. This override
        // only needs to widen the realm for a `#[test]` method's *body* so
        // calls inside it count as test calls, not production ones.
        let prev = self.realm;
        if has_path_ident_test(&node.attrs) || has_cfg_test(&node.attrs) {
            self.realm = Realm::Test;
        }
        syn::visit::visit_impl_item_fn(self, node);
        self.realm = prev;
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        let prev = self.realm;
        if has_cfg_test(&node.attrs) {
            self.realm = Realm::Test;
        }
        let trait_name = node
            .trait_
            .as_ref()
            .and_then(|(path, _)| path.segments.last().map(|s| s.ident.to_string()));
        let is_well_known_trait_impl = trait_name
            .as_deref()
            .map(|n| WELL_KNOWN_TRAITS.contains(&n))
            .unwrap_or(false);
        let is_any_trait_impl = node.trait_.is_some();
        if self.realm == Realm::Prod {
            for item in &node.items {
                if let ImplItem::Fn(m) = item {
                    let is_test_fn = has_path_ident_test(&m.attrs) || has_cfg_test(&m.attrs);
                    if is_test_fn {
                        continue;
                    }
                    if is_well_known_trait_impl {
                        self.out.excluded_trait_impl_methods += 1;
                    } else {
                        let kind = if is_any_trait_impl {
                            FnKind::TraitMethod
                        } else {
                            FnKind::Method
                        };
                        self.record_fn_def(&m.sig, &m.attrs, kind);
                    }
                }
            }
        }
        syn::visit::visit_item_impl(self, node);
        self.realm = prev;
    }

    fn visit_item_struct(&mut self, node: &'ast syn::ItemStruct) {
        if self.realm == Realm::Prod {
            self.record_allow_dead_code("struct", &node.ident.to_string(), &node.attrs);
            let opaque_reader = struct_has_field_opaque_reader_derive(&node.attrs);
            if let Fields::Named(named) = &node.fields {
                for f in &named.named {
                    if let Some(ident) = &f.ident {
                        self.out.field_defs.push(FieldDef {
                            crate_name: self.crate_name.to_string(),
                            file: self.file.to_string(),
                            struct_name: node.ident.to_string(),
                            field_name: ident.to_string(),
                            has_allow_dead_code: has_allow_dead_code(&f.attrs),
                            derive_opaque_reader: opaque_reader,
                        });
                        self.record_allow_dead_code(
                            "field",
                            &format!("{}::{ident}", node.ident),
                            &f.attrs,
                        );
                    }
                }
            }
        }
        syn::visit::visit_item_struct(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        bump(
            &mut self.out.call_counts,
            node.method.to_string(),
            self.realm,
        );
        // See `COLLECTION_GROWTH_METHODS`: a call to one of these on a
        // directly-field-accessed receiver (`self.field.push(x)`,
        // `self.field.entry(k).or_insert(v)` -- the receiver of `.entry` is
        // `self.field`) is a real mutation of that field, not a read.
        // Deliberately only matches a *direct* `Expr::Field` receiver, not
        // an arbitrary expression that might resolve to one -- consistent
        // with this scanner's bare-syntax model elsewhere.
        let method_name = node.method.to_string();
        if COLLECTION_GROWTH_METHODS.contains(&method_name.as_str()) {
            if let Expr::Field(field_expr) = &*node.receiver {
                if let Member::Named(ident) = &field_expr.member {
                    self.out.field_assigns.push(FieldAssignRecord {
                        field_name: ident.to_string(),
                        is_default_like: false,
                        realm: self.realm,
                    });
                }
            }
        }
        syn::visit::visit_expr_method_call(self, node);
    }

    fn visit_expr_path(&mut self, node: &'ast syn::ExprPath) {
        // Counts as a reference whether this path is *called* (`foo()` --
        // `Expr::Call`'s `func` is an `Expr::Path`, and the default
        // traversal walks into it, so this subsumes the old
        // `visit_expr_call` bump) or merely used *as a value*: `.map(f)`,
        // `.and_then(f)`, `.map_err(dec_err)`, a callback stored in a
        // struct field, `Some(f)`. That second shape is not a rare corner
        // case -- it is how half of this codebase passes a decoder or
        // mapper function around, and treating only literal `f(...)` call
        // syntax as "used" produced real false positives on exactly this
        // crate: `map_number_format`, `dec_err`, `tab_game_mode` and
        // `biome_climate` are all passed as bare function values to
        // `.map`/`.and_then`/`.map_err` and would otherwise read as dead.
        //
        // The cost is symmetric with the rest of this scanner's name-based
        // model: a local variable or const that happens to share a name
        // with some unrelated function elsewhere in the workspace will
        // just as wrongly mark that function "used". Accepted for the same
        // reason as the call-name matching above -- see the module doc.
        if let Some(seg) = node.path.segments.last() {
            bump(&mut self.out.call_counts, seg.ident.to_string(), self.realm);
        }
        syn::visit::visit_expr_path(self, node);
    }

    fn visit_attribute(&mut self, node: &'ast Attribute) {
        // A handful of derive macros in this workspace (this crate's own
        // `#[mc(decode_with = "decode_heightmaps")]` among them) name a
        // function by a *string literal* inside an attribute's token tree
        // rather than by an identifier path, which is invisible to every
        // visitor above -- `decode_heightmaps`, `decode_block_entities` and
        // `decode_light` in `crates/protocol/v770/src/packets/chunk.rs` are
        // exactly this shape. Treat any string literal token appearing
        // inside an attribute as a possible reference by its literal text.
        // False-positive risk is low (an unrelated string attribute would
        // have to coincide exactly with a function name) and the failure
        // direction if it fires wrongly is "under-reports a dead function",
        // which is the direction this whole tool already errs toward.
        match &node.meta {
            Meta::List(list) => {
                record_string_literal_references(
                    list.tokens.clone(),
                    &mut self.out.call_counts,
                    self.realm,
                );
            }
            Meta::NameValue(nv) => {
                if let Expr::Lit(syn::ExprLit {
                    lit: Lit::Str(s), ..
                }) = &nv.value
                {
                    bump_string_literal_reference(&s.value(), &mut self.out.call_counts, self.realm);
                }
            }
            Meta::Path(_) => {}
        }
        syn::visit::visit_attribute(self, node);
    }

    fn visit_expr_assign(&mut self, node: &'ast syn::ExprAssign) {
        // A simple `place.field = value` is a *write*, not a read, of
        // `field` — do not let the default traversal count it as one. We
        // still need to visit `place`'s own base (e.g. the `a.b` in
        // `a.b.c = x`, which really is read to locate the place) and the
        // right-hand side.
        if let Expr::Field(field_expr) = &*node.left {
            if let Member::Named(ident) = &field_expr.member {
                self.out.field_assigns.push(FieldAssignRecord {
                    field_name: ident.to_string(),
                    is_default_like: expr_is_default_like(&node.right),
                    realm: self.realm,
                });
            }
            self.visit_expr(&field_expr.base);
            self.visit_expr(&node.right);
            return;
        }
        syn::visit::visit_expr_assign(self, node);
    }

    fn visit_expr_binary(&mut self, node: &'ast syn::ExprBinary) {
        // `syn` folds compound assignment (`+=`, `-=`, ...) into
        // `Expr::Binary` rather than a distinct node, so a counter/
        // accumulator field only ever mutated this way -- exactly
        // `MovementSendState::position_reminder`
        // (`crates/protocol/v770/src/adapter/mod.rs`), reset to `0` at
        // construction and incremented with `state.position_reminder += 1`
        // every call -- had no assignment site in `default_assigns` at all,
        // so its one real `= 0` reset site made it read as "every
        // production assignment is default". A relative mutation is never
        // "the default" regardless of the delta on the right, so record it
        // unconditionally as non-default; the field is already correctly
        // excluded from the *dead* list because the default traversal
        // visits its LHS as a real field read (both a read and a write).
        use syn::BinOp;
        let is_compound_assign = matches!(
            node.op,
            BinOp::AddAssign(_)
                | BinOp::SubAssign(_)
                | BinOp::MulAssign(_)
                | BinOp::DivAssign(_)
                | BinOp::RemAssign(_)
                | BinOp::BitXorAssign(_)
                | BinOp::BitAndAssign(_)
                | BinOp::BitOrAssign(_)
                | BinOp::ShlAssign(_)
                | BinOp::ShrAssign(_)
        );
        if is_compound_assign {
            if let Expr::Field(field_expr) = &*node.left {
                if let Member::Named(ident) = &field_expr.member {
                    self.out.field_assigns.push(FieldAssignRecord {
                        field_name: ident.to_string(),
                        is_default_like: false,
                        realm: self.realm,
                    });
                }
            }
        }
        syn::visit::visit_expr_binary(self, node);
    }

    fn visit_expr_reference(&mut self, node: &'ast syn::ExprReference) {
        // Taking a mutable reference to a field and passing it on
        // (`step_vertical(..., &mut self.fall_speed)`, or
        // `std::mem::take(&mut self.field)`/`std::mem::replace`/
        // `std::mem::swap`, all of which take their target this way) mutates
        // the field through the callee with no `Expr::Assign`/`Expr::Binary`
        // syntax at this call site at all. Measured false positive:
        // `NavigatingMob::fall_speed` (`crates/lodestone-entity/src/ai/
        // navigating_mob.rs`) is written exclusively by passing `&mut
        // self.fall_speed` into `step_vertical`, and `NavigatingMob::
        // {attacks, launches, eaten, self_damage}` are drained the same way
        // via `std::mem::take(&mut self.field)` -- both shapes had no
        // recorded non-default assignment at all before this. Deliberately
        // conservative: this cannot know whether the callee actually writes
        // through the pointer, but that only risks under-flagging a truly
        // dead field, the same bias this whole scanner already has.
        if node.mutability.is_some() {
            if let Expr::Field(field_expr) = &*node.expr {
                if let Member::Named(ident) = &field_expr.member {
                    self.out.field_assigns.push(FieldAssignRecord {
                        field_name: ident.to_string(),
                        is_default_like: false,
                        realm: self.realm,
                    });
                }
            }
        }
        syn::visit::visit_expr_reference(self, node);
    }

    fn visit_expr_field(&mut self, node: &'ast syn::ExprField) {
        if let Member::Named(ident) = &node.member {
            bump(
                &mut self.out.field_read_counts,
                ident.to_string(),
                self.realm,
            );
        }
        syn::visit::visit_expr_field(self, node);
    }

    fn visit_pat_struct(&mut self, node: &'ast syn::PatStruct) {
        // A struct pattern (`let PathParams { max_path_length, .. } =
        // params;`, a `match`/`if let` arm, or a destructured function
        // parameter) *reads* every field it binds, out of the value being
        // matched -- a read this scanner otherwise never sees, because
        // `visit_expr_field` only models `.field` expression access, not
        // pattern binding. Measured false positive:
        // `PathFinder::find_path` (`crates/lodestone-entity/src/
        // pathfinding/search.rs`) destructures `PathParams {
        // max_path_length, reach_range, visited_multiplier }` and uses only
        // the bindings from then on -- no `.field` access anywhere -- so all
        // three read as "zero production readers" without this. Each
        // `FieldPat`'s `member` names the field regardless of whether the
        // pattern is shorthand (`{ field }`) or has its own sub-pattern
        // (`{ field: x }`, `{ field: x @ 1..=5 }`); the `..` rest token is
        // not a `FieldPat` at all and is correctly not counted as a read of
        // anything.
        for field_pat in &node.fields {
            if let Member::Named(ident) = &field_pat.member {
                bump(&mut self.out.field_read_counts, ident.to_string(), self.realm);
            }
        }
        syn::visit::visit_pat_struct(self, node);
    }

    fn visit_expr_struct(&mut self, node: &'ast syn::ExprStruct) {
        if let Some(name) = node.path.segments.last().map(|s| s.ident.to_string()) {
            let explicit: Vec<(String, bool)> = node
                .fields
                .iter()
                .filter_map(|fv| match &fv.member {
                    Member::Named(ident) => {
                        Some((ident.to_string(), expr_is_default_like(&fv.expr)))
                    }
                    Member::Unnamed(_) => None,
                })
                .collect();
            let (has_rest, rest_default) = match &node.rest {
                Some(rest_expr) => (true, rest_is_default_like(rest_expr)),
                None => (false, false),
            };
            self.out.struct_literals.push(StructLiteralRecord {
                struct_name: name,
                explicit_fields: explicit,
                has_rest,
                rest_is_default_like: rest_default,
                realm: self.realm,
            });
        }
        syn::visit::visit_expr_struct(self, node);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        // `syn` does not know the grammar of an arbitrary macro invocation
        // and treats its body as an opaque token stream by default -- so
        // without this override, `assert_eq!(unwired(), 7)` would hide the
        // call to `unwired()` from every count, and virtually all test code
        // (wall-to-wall with `assert!`/`assert_eq!`) would silently vanish
        // from the test-realm call graph. Most of the macros that matter
        // here (assert!, assert_eq!, assert_ne!, println!, format!, write!,
        // writeln!, vec!, panic!, dbg!) happen to take a comma-separated
        // expression list, which is exactly what `Punctuated<Expr, Comma>`
        // parses. When the body is not that shape (`matches!`'s second
        // argument is a *pattern*, not an expression, so this fails), skip
        // silently rather than guess -- a real, documented blind spot, not
        // a crash.
        if let Ok(exprs) = node.parse_body_with(
            syn::punctuated::Punctuated::<Expr, syn::Token![,]>::parse_terminated,
        ) {
            for expr in &exprs {
                self.visit_expr(expr);
            }
        }
        // The Punctuated<Expr, Comma> parse above only succeeds for macros
        // whose body genuinely is a flat expression list. `tokio::select! {
        // pat = future_expr => body, ... }` is not that shape -- each arm is
        // `PATTERN = EXPR => BODY`, not one `Expr` -- so the whole `select!`
        // block, and everything called inside it, was invisible: measured
        // false positives on real production code, `crate::server::
        // travel_through_portal`/`travel_through_end_portal` in
        // `crates/lodestone-server/src/server.rs`'s `serve_play`, both
        // called from inside its `tokio::select! { ... }` loop and both
        // reachable no other way this scanner can see. Rather than try to
        // model `select!`'s (or any other macro's) exact grammar, scan
        // every macro body -- always, not only as a fallback, since this
        // only ever adds references -- for the *lexical* shape `ident (` or
        // `.ident (`, recursing into every nested group regardless of its
        // delimiter. This cannot build a real AST (so it does not feed
        // `self.visit_expr`, and default-value/field-read tracking gets
        // none of this), but a bare "is this name referenced at all"
        // signal is exactly what the dead-function check needs, and it
        // cannot be defeated by a macro grammar we do not know.
        scan_call_like_tokens(node.tokens.clone(), &mut self.out.call_counts, self.realm);
        syn::visit::visit_macro(self, node);
    }

    fn visit_item_enum(&mut self, node: &'ast syn::ItemEnum) {
        if self.realm == Realm::Prod {
            self.record_allow_dead_code("enum", &node.ident.to_string(), &node.attrs);
        }
        syn::visit::visit_item_enum(self, node);
    }

    fn visit_item_const(&mut self, node: &'ast syn::ItemConst) {
        if self.realm == Realm::Prod {
            self.record_allow_dead_code("const", &node.ident.to_string(), &node.attrs);
        }
        syn::visit::visit_item_const(self, node);
    }

    fn visit_item_static(&mut self, node: &'ast syn::ItemStatic) {
        if self.realm == Realm::Prod {
            self.record_allow_dead_code("static", &node.ident.to_string(), &node.attrs);
        }
        syn::visit::visit_item_static(self, node);
    }
}

/// Which realm every item/expression in a whole file starts in, based on its
/// path relative to the crate root. Integration tests (`tests/`), benches
/// (`benches/`) and examples (`examples/`) are Cargo targets that never ship
/// in the built binary/cdylib, so they are Test realm even though nothing in
/// them carries a `#[cfg(test)]` attribute. This mirrors `dev-dependencies`
/// in spirit: code that only exists to exercise the crate, not to be part of
/// it. See the module doc's blind-spot list: an example that is the *only*
/// caller of a public function will misreport that function as dead.
fn classify_file_realm(crate_relative: &Path) -> Realm {
    let top = crate_relative
        .components()
        .next()
        .and_then(|c| c.as_os_str().to_str());
    match top {
        Some("tests") | Some("benches") | Some("examples") => Realm::Test,
        _ => Realm::Prod,
    }
}

fn walk_rs_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let entries =
        fs::read_dir(dir).with_context(|| format!("read directory {}", dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("read entry under {}", dir.display()))?;
        let path = entry.path();
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            if name == "target" || name.starts_with('.') {
                continue;
            }
            walk_rs_files(&path, out)?;
        } else if name.ends_with(".rs") {
            out.push(path);
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct FunctionFinding {
    pub file: String,
    pub name: String,
    pub kind_label: &'static str,
    pub test_call_sites: u32,
    pub has_allow_dead_code: bool,
}

#[derive(Debug, Clone)]
pub struct FieldFinding {
    pub file: String,
    pub struct_name: String,
    pub field_name: String,
    pub secondary: u32,
    pub has_allow_dead_code: bool,
}

#[derive(Debug, Default)]
pub struct CrateFindings {
    pub crate_name: String,
    pub files_scanned: usize,
    pub prod_fn_defs: usize,
    pub dead_functions: Vec<FunctionFinding>,
    pub tested_only_functions: Vec<FunctionFinding>,
    pub prod_field_defs: usize,
    pub excluded_derive_consumed_fields: usize,
    pub dead_fields: Vec<FieldFinding>,
    pub default_only_fields: Vec<FieldFinding>,
    pub allow_dead_code: Vec<AllowDeadCodeSite>,
}

#[derive(Debug)]
pub struct IslandsReport {
    pub crates: Vec<CrateFindings>,
    pub excluded_trait_impl_methods: usize,
    pub excluded_ffi_or_entrypoint: usize,
    pub parse_errors: Vec<(String, String, String)>,
    pub total_files_scanned: usize,
}

fn crate_dir_for_package(package: &Value, workspace_root: &Path) -> Result<(String, PathBuf)> {
    let name = package
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("workspace package missing name"))?
        .to_string();
    let manifest_path = package
        .get("manifest_path")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("workspace package {name} missing manifest_path"))?;
    let dir = Path::new(manifest_path)
        .parent()
        .ok_or_else(|| anyhow::anyhow!("manifest path {manifest_path} has no parent"))?
        .to_path_buf();
    let _ = workspace_root;
    Ok((name, dir))
}

/// Run the full workspace scan. Exits with an error (never a silent skip)
/// if `cargo metadata` fails, if a workspace member yields zero `.rs`
/// files, or if parse failures are widespread enough that the report
/// cannot be trusted — see the module doc.
pub fn islands_report(workspace_root: &Path) -> Result<IslandsReport> {
    let metadata = cargo_metadata(workspace_root)?;
    let packages = metadata
        .get("packages")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("cargo metadata output missing packages array"))?;

    let mut collected = Collected::default();
    let mut crate_order: Vec<String> = Vec::new();

    for package in packages {
        let (name, crate_dir) = crate_dir_for_package(package, workspace_root)?;
        let mut files = Vec::new();
        walk_rs_files(&crate_dir, &mut files)
            .with_context(|| format!("walk .rs files for crate {name} under {crate_dir:?}"))?;
        if files.is_empty() {
            bail!(
                "islands: crate {name} yielded zero .rs files under {}; treating this as a scan \
                 failure rather than a silent skip -- a workspace member with no source is not a \
                 crate that legitimately has nothing to scan",
                crate_dir.display()
            );
        }
        // Parse every file up front (rather than visiting as we go) so we
        // can resolve `#[cfg(test)] mod tests;` -- a *declaration* naming
        // an external file -- before deciding that file's own realm. A
        // file parsed in isolation has no way to see an attribute that
        // lives in a sibling file: `crates/lodestone-model/src/tests.rs`
        // carries no `#[cfg(test)]` itself, only `lib.rs`'s `mod tests;`
        // declaration does. Without this pass, every call made from such a
        // file -- and there is often exactly one such file per crate --
        // reads as a production call, silently hiding real islands (a
        // trait method with its only caller inside one of these files
        // looked "used" during development of this scanner before this
        // pass existed).
        let mut parsed_files: Vec<(PathBuf, String, syn::File)> = Vec::new();
        for file in &files {
            let rel = file.strip_prefix(workspace_root).unwrap_or(file);
            let content = fs::read_to_string(file)
                .with_context(|| format!("read {}", file.display()))?;
            match syn::parse_file(&content) {
                Ok(parsed) => parsed_files.push((file.clone(), rel.display().to_string(), parsed)),
                Err(err) => {
                    collected.parse_errors.push((
                        name.clone(),
                        rel.display().to_string(),
                        err.to_string(),
                    ));
                }
            }
        }

        // Top-level-only: a `mod tests;` nested inside some other inline
        // module is not resolved here, so this pass covers the common
        // per-crate `mod tests;` pattern and nothing deeper. Documented
        // blind spot, not a silent gap.
        let mut test_mod_names: BTreeSet<String> = BTreeSet::new();
        for (_, _, parsed) in &parsed_files {
            for item in &parsed.items {
                if let Item::Mod(m) = item {
                    if m.content.is_none() && has_cfg_test(&m.attrs) {
                        test_mod_names.insert(m.ident.to_string());
                    }
                }
            }
        }

        for (file, rel_label, parsed) in &parsed_files {
            let crate_relative = file.strip_prefix(&crate_dir).unwrap_or(file);
            let mut file_realm = classify_file_realm(crate_relative);
            if file_realm == Realm::Prod {
                let match_key = if file.file_name().and_then(|n| n.to_str()) == Some("mod.rs") {
                    file.parent()
                        .and_then(|p| p.file_name())
                        .and_then(|n| n.to_str())
                        .map(str::to_string)
                } else {
                    file.file_stem().and_then(|s| s.to_str()).map(str::to_string)
                };
                if match_key.is_some_and(|key| test_mod_names.contains(&key)) {
                    file_realm = Realm::Test;
                }
            }
            let mut collector = Collector {
                crate_name: &name,
                file: rel_label,
                realm: file_realm,
                out: &mut collected,
            };
            for item in &parsed.items {
                collector.visit_item(item);
            }
            collected.files_scanned += 1;
            *collected.files_scanned_by_crate.entry(name.clone()).or_insert(0) += 1;
        }
        crate_order.push(name);
    }

    let total_files: usize = collected.files_scanned + collected.parse_errors.len();
    if total_files > 0 && collected.parse_errors.len() * 20 > total_files {
        // More than 5% of files failed to parse. A `syn`-based scanner that
        // cannot parse most of the workspace is not a scanner that found
        // few islands -- it is one that did not look. Fail loudly instead
        // of reporting a suspiciously clean result. (The exact threshold is
        // a judgement call; the point is that *some* threshold exists so
        // this can never quietly degrade to "SKIPPED" like the
        // `connectedness` v770-folder incident.)
        bail!(
            "islands: {} of {} files failed to parse (>5%) -- refusing to report a partial \
             scan as if it were complete. First failures:\n{}",
            collected.parse_errors.len(),
            total_files,
            collected
                .parse_errors
                .iter()
                .take(5)
                .map(|(c, f, e)| format!("  {c}/{f}: {e}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    // --- Field default-assignment resolution (needs the full field-def
    // table before struct-literal `..rest` expansion can know which fields
    // were omitted) ---
    let mut struct_fields_by_name: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for fd in &collected.field_defs {
        struct_fields_by_name
            .entry(fd.struct_name.clone())
            .or_default()
            .insert(fd.field_name.clone());
    }

    let mut default_assigns: BTreeMap<String, Vec<(Realm, bool)>> = BTreeMap::new();
    for rec in &collected.field_assigns {
        default_assigns
            .entry(rec.field_name.clone())
            .or_default()
            .push((rec.realm, rec.is_default_like));
    }
    for lit in &collected.struct_literals {
        let explicit_names: BTreeSet<&str> =
            lit.explicit_fields.iter().map(|(n, _)| n.as_str()).collect();
        for (name, is_default) in &lit.explicit_fields {
            default_assigns
                .entry(name.clone())
                .or_default()
                .push((lit.realm, *is_default));
        }
        if lit.has_rest {
            if let Some(all_fields) = struct_fields_by_name.get(&lit.struct_name) {
                for f in all_fields {
                    if !explicit_names.contains(f.as_str()) {
                        default_assigns
                            .entry(f.clone())
                            .or_default()
                            .push((lit.realm, lit.rest_is_default_like));
                    }
                }
            }
        }
    }

    // --- Assemble per-crate findings ---
    let mut by_crate: BTreeMap<String, CrateFindings> = BTreeMap::new();
    for name in &crate_order {
        by_crate.entry(name.clone()).or_insert_with(|| CrateFindings {
            crate_name: name.clone(),
            ..Default::default()
        });
    }

    for fd in &collected.fn_defs {
        let entry = by_crate.entry(fd.crate_name.clone()).or_insert_with(|| {
            CrateFindings {
                crate_name: fd.crate_name.clone(),
                ..Default::default()
            }
        });
        entry.prod_fn_defs += 1;
        let counts = collected.call_counts.get(&fd.name).copied().unwrap_or_default();
        if counts.prod == 0 {
            let kind_label = match fd.kind {
                FnKind::Free => "fn",
                FnKind::Method => "method",
                FnKind::TraitMethod => "trait-impl method",
            };
            let finding = FunctionFinding {
                file: fd.file.clone(),
                name: fd.name.clone(),
                kind_label,
                test_call_sites: counts.test,
                has_allow_dead_code: fd.has_allow_dead_code,
            };
            if counts.test > 0 {
                entry.tested_only_functions.push(finding);
            } else {
                entry.dead_functions.push(finding);
            }
        }
    }

    for field in &collected.field_defs {
        let entry = by_crate
            .entry(field.crate_name.clone())
            .or_insert_with(|| CrateFindings {
                crate_name: field.crate_name.clone(),
                ..Default::default()
            });
        entry.prod_field_defs += 1;
        if field.derive_opaque_reader {
            // See `FIELD_OPAQUE_READER_DERIVES`: an `Encode`/`Decode`/
            // `Serialize`/`Deserialize` derive reads or writes every field
            // through generated code we never see, so this scanner cannot
            // tell a genuine island from the derive's own consumption.
            // Excluded from both findings rather than reported as a
            // suspicious positive.
            entry.excluded_derive_consumed_fields += 1;
            continue;
        }
        let read_counts = collected
            .field_read_counts
            .get(&field.field_name)
            .copied()
            .unwrap_or_default();
        if read_counts.prod == 0 {
            entry.dead_fields.push(FieldFinding {
                file: field.file.clone(),
                struct_name: field.struct_name.clone(),
                field_name: field.field_name.clone(),
                secondary: read_counts.test,
                has_allow_dead_code: field.has_allow_dead_code,
            });
        }
        if let Some(assigns) = default_assigns.get(&field.field_name) {
            let prod: Vec<bool> = assigns
                .iter()
                .filter(|(r, _)| *r == Realm::Prod)
                .map(|(_, d)| *d)
                .collect();
            if !prod.is_empty() && prod.iter().all(|d| *d) {
                entry.default_only_fields.push(FieldFinding {
                    file: field.file.clone(),
                    struct_name: field.struct_name.clone(),
                    field_name: field.field_name.clone(),
                    secondary: prod.len() as u32,
                    has_allow_dead_code: field.has_allow_dead_code,
                });
            }
        }
    }

    for site in &collected.allow_dead_code {
        let entry = by_crate
            .entry(site.crate_name.clone())
            .or_insert_with(|| CrateFindings {
                crate_name: site.crate_name.clone(),
                ..Default::default()
            });
        entry.allow_dead_code.push(site.clone());
    }

    for (name, count) in &collected.files_scanned_by_crate {
        if let Some(entry) = by_crate.get_mut(name) {
            entry.files_scanned = *count;
        }
    }

    let crates: Vec<CrateFindings> = by_crate.into_values().collect();

    Ok(IslandsReport {
        crates,
        excluded_trait_impl_methods: collected.excluded_trait_impl_methods,
        excluded_ffi_or_entrypoint: collected.excluded_ffi_or_entrypoint,
        parse_errors: collected.parse_errors,
        total_files_scanned: collected.files_scanned,
    })
}

pub fn format_islands_report(report: &IslandsReport, only_crate: Option<&str>) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "islands: scanned {} files across {} crates ({} parse errors, {} trait-impl methods \
         excluded, {} FFI/entrypoint fns excluded)",
        report.total_files_scanned,
        report.crates.len(),
        report.parse_errors.len(),
        report.excluded_trait_impl_methods,
        report.excluded_ffi_or_entrypoint,
    );
    if !report.parse_errors.is_empty() {
        let _ = writeln!(out, "parse errors (excluded from the scan, not silently dropped):");
        for (c, f, e) in &report.parse_errors {
            let _ = writeln!(out, "  {c}/{f}: {e}");
        }
    }
    let mut crates: Vec<&CrateFindings> = report
        .crates
        .iter()
        .filter(|c| only_crate.is_none_or(|only| c.crate_name == only))
        .collect();
    crates.sort_by(|a, b| a.crate_name.cmp(&b.crate_name));
    for c in crates {
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "== {} == ({} files, {} production fn/method defs, {} production named-struct \
             fields, {} fields excluded as Encode/Decode/Serialize/Deserialize-consumed)",
            c.crate_name,
            c.files_scanned,
            c.prod_fn_defs,
            c.prod_field_defs,
            c.excluded_derive_consumed_fields,
        );
        let _ = writeln!(
            out,
            "  dead functions (0 prod, 0 test call sites): {}",
            c.dead_functions.len()
        );
        for f in &c.dead_functions {
            let _ = writeln!(
                out,
                "    {} {} ({}){}",
                f.kind_label,
                f.name,
                f.file,
                if f.has_allow_dead_code { " [#[allow(dead_code)]]" } else { "" }
            );
        }
        let _ = writeln!(
            out,
            "  tested-but-unwired functions (0 prod, >0 test call sites): {}",
            c.tested_only_functions.len()
        );
        for f in &c.tested_only_functions {
            let _ = writeln!(
                out,
                "    {} {} ({}) -- {} test call site(s)",
                f.kind_label, f.name, f.file, f.test_call_sites
            );
        }
        let _ = writeln!(
            out,
            "  fields with zero production readers: {}",
            c.dead_fields.len()
        );
        for f in &c.dead_fields {
            let _ = writeln!(
                out,
                "    {}::{} ({}) -- {} test read(s){}",
                f.struct_name,
                f.field_name,
                f.file,
                f.secondary,
                if f.has_allow_dead_code { " [#[allow(dead_code)]]" } else { "" }
            );
        }
        let _ = writeln!(
            out,
            "  fields whose every production assignment is default-like: {}",
            c.default_only_fields.len()
        );
        for f in &c.default_only_fields {
            let _ = writeln!(
                out,
                "    {}::{} ({}) -- {} production assignment(s), 0 non-default",
                f.struct_name, f.field_name, f.file, f.secondary
            );
        }
        let _ = writeln!(
            out,
            "  #[allow(dead_code)] sites: {}",
            c.allow_dead_code.len()
        );
        for s in &c.allow_dead_code {
            let _ = writeln!(out, "    {} {} ({})", s.kind, s.name, s.file);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ops::Deref;

    /// A throwaway single-crate workspace for exercising `islands_report`
    /// without touching the real repo. Mirrors `fresh_test_workspace` in
    /// `crate::tests`, kept local because `islands` is its own module and
    /// that helper is private to `crate::tests`.
    struct Workspace {
        dir: tempfile::TempDir,
    }

    impl Deref for Workspace {
        type Target = Path;

        fn deref(&self) -> &Path {
            self.dir.path()
        }
    }

    fn workspace_with_lib(name: &str, lib_rs: &str) -> Result<Workspace> {
        let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/islands-test-workspaces");
        std::fs::create_dir_all(&parent)?;
        let dir = tempfile::Builder::new()
            .prefix(&format!("{name}-"))
            .tempdir_in(&parent)?;
        let root = dir.path();
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nresolver = \"3\"\nmembers = [\"one\"]\n",
        )?;
        let crate_dir = root.join("one");
        std::fs::create_dir_all(crate_dir.join("src"))?;
        std::fs::write(
            crate_dir.join("Cargo.toml"),
            "[package]\nname = \"one\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )?;
        std::fs::write(crate_dir.join("src/lib.rs"), lib_rs)?;
        Ok(Workspace { dir })
    }

    /// The tool's own control, made durable rather than a one-off manual
    /// check: a deliberately dead function and a deliberately never-read
    /// field must both be named; a used function and an always-different
    /// field assignment must both be absent from the findings. Per
    /// CLAUDE.md, a control that is only ever run by hand is the same class
    /// of hazard as an instrument nobody re-validates.
    #[test]
    fn planted_island_is_found_and_a_used_one_is_not() -> Result<()> {
        let workspace = workspace_with_lib(
            "planted",
            r#"
                pub struct Widget {
                    pub used_field: u32,
                    pub dead_field: u32,
                }

                pub fn used_fn(w: &Widget) -> u32 {
                    w.used_field
                }

                pub fn dead_fn() -> u32 {
                    42
                }

                pub fn caller() -> u32 {
                    used_fn(&Widget { used_field: 1, dead_field: 2 })
                }
            "#,
        )?;
        let report = islands_report(&workspace)?;
        let one = report
            .crates
            .iter()
            .find(|c| c.crate_name == "one")
            .expect("crate one scanned");

        assert!(
            one.dead_functions.iter().any(|f| f.name == "dead_fn"),
            "planted dead_fn was not found: {:?}",
            one.dead_functions
        );
        assert!(
            !one.dead_functions.iter().any(|f| f.name == "used_fn"),
            "used_fn was wrongly flagged as dead"
        );
        assert!(
            one.dead_fields
                .iter()
                .any(|f| f.field_name == "dead_field"),
            "planted dead_field was not found: {:?}",
            one.dead_fields
        );
        assert!(
            !one.dead_fields.iter().any(|f| f.field_name == "used_field"),
            "used_field was wrongly flagged as dead"
        );
        Ok(())
    }

    #[test]
    fn tested_but_unwired_is_distinguished_from_dead() -> Result<()> {
        let workspace = workspace_with_lib(
            "tested-unwired",
            r#"
                pub fn unwired() -> u32 { 7 }
                pub fn truly_dead() -> u32 { 8 }

                #[cfg(test)]
                mod tests {
                    use super::*;

                    #[test]
                    fn exercises_unwired() {
                        assert_eq!(unwired(), 7);
                    }
                }
            "#,
        )?;
        let report = islands_report(&workspace)?;
        let one = report.crates.iter().find(|c| c.crate_name == "one").unwrap();

        assert!(one.tested_only_functions.iter().any(|f| f.name == "unwired"));
        assert!(!one.dead_functions.iter().any(|f| f.name == "unwired"));
        assert!(one.dead_functions.iter().any(|f| f.name == "truly_dead"));
        Ok(())
    }

    #[test]
    fn default_only_field_is_distinguished_from_a_real_one() -> Result<()> {
        let workspace = workspace_with_lib(
            "default-only",
            r#"
                #[derive(Default)]
                pub struct Config {
                    pub always_zero: u32,
                    pub sometimes_nonzero: u32,
                }

                pub fn make_a() -> Config {
                    Config { always_zero: 0, sometimes_nonzero: 5 }
                }

                pub fn make_b() -> Config {
                    Config { always_zero: 0, sometimes_nonzero: 0 }
                }
            "#,
        )?;
        let report = islands_report(&workspace)?;
        let one = report.crates.iter().find(|c| c.crate_name == "one").unwrap();

        assert!(
            one.default_only_fields
                .iter()
                .any(|f| f.field_name == "always_zero"),
        );
        assert!(
            !one
                .default_only_fields
                .iter()
                .any(|f| f.field_name == "sometimes_nonzero"),
        );
        Ok(())
    }

    /// The other real false positive found in `crates/protocol/v770/src/
    /// adapter/mod.rs`: `MovementSendState::position_reminder` is reset to
    /// `0` at construction and every 20-tick send, but *counted up* to that
    /// point with `position_reminder += 1` -- a compound assignment, which
    /// `syn` represents as `Expr::Binary` rather than `Expr::Assign`. Before
    /// `visit_expr_binary` handled this, the `+=` mutation was invisible to
    /// the default-assignment tracker entirely, so the field's only
    /// *recorded* assignment was the `= 0` reset -- reading as "100% default
    /// assignments" for a field that is very much not always zero at
    /// runtime.
    #[test]
    fn compound_assignment_counts_as_a_non_default_mutation() -> Result<()> {
        let workspace = workspace_with_lib(
            "compound-assign",
            r#"
                pub struct Counter {
                    pub reminder: u32,
                }

                pub fn new_counter() -> Counter {
                    Counter { reminder: 0 }
                }

                pub fn tick(state: &mut Counter) {
                    state.reminder += 1;
                    if state.reminder >= 20 {
                        state.reminder = 0;
                    }
                }
            "#,
        )?;
        let report = islands_report(&workspace)?;
        let one = report.crates.iter().find(|c| c.crate_name == "one").unwrap();

        assert!(
            !one.default_only_fields.iter().any(|f| f.field_name == "reminder"),
            "a field only ever mutated by += was wrongly called default-only: {:?}",
            one.default_only_fields
        );
        Ok(())
    }

    /// `ChunkBatchSizeCalculator::new()` in `crates/protocol/v770/src/
    /// adapter/mod.rs` is a fully-initialized, meaningful value, not a
    /// placeholder -- treating every zero-arg `T::new()` as "the default"
    /// (matching `Vec::new()`, `String::new()`, ...) flagged it as
    /// default-only despite having exactly one, real, non-trivial
    /// assignment site.
    #[test]
    fn arbitrary_types_new_is_not_assumed_to_be_default() -> Result<()> {
        let workspace = workspace_with_lib(
            "custom-new",
            r#"
                pub struct Calculator {
                    pub rate: u32,
                }

                impl Calculator {
                    pub fn new() -> Self {
                        Self { rate: 42 }
                    }
                }

                pub struct Holder {
                    pub calculator: Calculator,
                }

                pub fn make() -> Holder {
                    Holder { calculator: Calculator::new() }
                }
            "#,
        )?;
        let report = islands_report(&workspace)?;
        let one = report.crates.iter().find(|c| c.crate_name == "one").unwrap();

        assert!(
            !one
                .default_only_fields
                .iter()
                .any(|f| f.field_name == "calculator"),
            "Calculator::new() was wrongly assumed to be a default/placeholder value: {:?}",
            one.default_only_fields
        );
        Ok(())
    }

    #[test]
    fn trait_impl_of_display_is_excluded_not_flagged_dead() -> Result<()> {
        let workspace = workspace_with_lib(
            "trait-impl",
            r#"
                use std::fmt;

                pub struct Thing;

                impl fmt::Display for Thing {
                    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                        write!(f, "thing")
                    }
                }
            "#,
        )?;
        let report = islands_report(&workspace)?;
        let one = report.crates.iter().find(|c| c.crate_name == "one").unwrap();

        assert!(!one.dead_functions.iter().any(|f| f.name == "fmt"));
        assert_eq!(report.excluded_trait_impl_methods, 1);
        Ok(())
    }

    /// The false positive this repo actually has: every wire packet under
    /// `crates/protocol/*/src/packets/` derives `Encode`/`Decode`, and the
    /// generated `encode`/`decode` genuinely reads/writes every field
    /// through code this scanner never expands. Without the exclusion this
    /// guards, a struct's fields would show as "zero production readers"
    /// purely because the derive's own consumption is invisible to us.
    #[test]
    fn encode_decode_derived_struct_fields_are_excluded_not_flagged_dead() -> Result<()> {
        let workspace = workspace_with_lib(
            "encode-decode-derive",
            r#"
                pub trait Encode {}
                pub trait Decode {}

                #[derive(Encode, Decode)]
                pub struct Packet {
                    pub never_textually_read: u32,
                }
            "#,
        )?;
        let report = islands_report(&workspace)?;
        let one = report.crates.iter().find(|c| c.crate_name == "one").unwrap();

        assert!(
            !one
                .dead_fields
                .iter()
                .any(|f| f.field_name == "never_textually_read"),
            "Encode/Decode-derived field was wrongly flagged dead: {:?}",
            one.dead_fields
        );
        assert_eq!(one.excluded_derive_consumed_fields, 1);
        Ok(())
    }

    /// The other real false positive this repo has, found the same way as
    /// the `Encode`/`Decode` one above: `crates/protocol/v770/src/adapter/
    /// scoreboard.rs` passes `map_number_format` to `.map(...)` and
    /// `dec_err` to `.map_err(...)` -- a bare function *value*, never a
    /// literal `f(...)` call. Before `visit_expr_path` this scanner
    /// flagged both as dead.
    #[test]
    fn function_passed_as_a_bare_value_is_not_flagged_dead() -> Result<()> {
        let workspace = workspace_with_lib(
            "fn-as-value",
            r#"
                pub fn mapper(x: u32) -> u32 { x + 1 }
                pub fn truly_dead() -> u32 { 0 }

                pub fn caller(values: Vec<u32>) -> Vec<u32> {
                    values.into_iter().map(mapper).collect()
                }
            "#,
        )?;
        let report = islands_report(&workspace)?;
        let one = report.crates.iter().find(|c| c.crate_name == "one").unwrap();

        assert!(!one.dead_functions.iter().any(|f| f.name == "mapper"));
        assert!(one.dead_functions.iter().any(|f| f.name == "truly_dead"));
        Ok(())
    }

    /// `crates/protocol/v770/src/packets/chunk.rs` names its decoder
    /// functions by string inside `#[mc(decode_with = "decode_heightmaps")]`
    /// -- a shape no identifier-based visitor sees at all, since the
    /// reference is a string literal, not a path.
    #[test]
    fn function_named_in_an_attribute_string_is_not_flagged_dead() -> Result<()> {
        let workspace = workspace_with_lib(
            "attr-string-ref",
            r#"
                pub struct Marker;

                pub struct Packet {
                    #[custom(decode_with = "custom_decoder")]
                    pub field: u32,
                }

                fn custom_decoder(_input: &[u8]) -> u32 { 0 }
                fn truly_dead() -> u32 { 0 }
            "#,
        )?;
        let report = islands_report(&workspace)?;
        let one = report.crates.iter().find(|c| c.crate_name == "one").unwrap();

        assert!(!one.dead_functions.iter().any(|f| f.name == "custom_decoder"));
        assert!(one.dead_functions.iter().any(|f| f.name == "truly_dead"));
        Ok(())
    }

    /// The real, measured false positive: `crates/lodestone-server/src/
    /// server.rs`'s `serve_play` calls `travel_through_portal` and
    /// `travel_through_end_portal` only from inside its `tokio::select! {
    /// ... }` loop. `select!`'s arms (`PATTERN = EXPR => BODY`) are not a
    /// flat expression list, so the `Punctuated<Expr, Comma>` parse in
    /// `visit_macro` fails for the whole block and everything called inside
    /// it -- real, production, gameplay-critical calls -- read as dead
    /// without the lexical fallback this test guards.
    #[test]
    fn call_inside_a_non_expr_list_macro_is_not_flagged_dead() -> Result<()> {
        let workspace = workspace_with_lib(
            "select-macro",
            r#"
                pub fn travel_through_portal(x: u32) -> u32 { x }
                pub fn truly_dead() -> u32 { 0 }

                pub async fn serve(future_a: impl std::future::Future<Output = u32>) {
                    fake_select! {
                        v = future_a => {
                            travel_through_portal(v);
                        }
                    }
                }

                // A stand-in for `tokio::select!`: this test only needs a
                // macro whose body is not a valid `Punctuated<Expr, Comma>`
                // (arm syntax `pat = expr => body` is not one `Expr`), which
                // is exactly what defeats the strict parse.
                macro_rules! fake_select {
                    ($($pat:pat = $fut:expr => $body:block)*) => {};
                }
            "#,
        )?;
        let report = islands_report(&workspace)?;
        let one = report.crates.iter().find(|c| c.crate_name == "one").unwrap();

        assert!(
            !one
                .dead_functions
                .iter()
                .any(|f| f.name == "travel_through_portal"),
            "a call inside a select!-shaped macro arm was wrongly flagged dead: {:?}",
            one.dead_functions
        );
        assert!(one.dead_functions.iter().any(|f| f.name == "truly_dead"));
        Ok(())
    }

    #[test]
    fn zero_rs_files_is_a_hard_failure_not_a_skip() -> Result<()> {
        let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/islands-test-workspaces");
        std::fs::create_dir_all(&parent)?;
        let dir = tempfile::Builder::new()
            .prefix("empty-crate-")
            .tempdir_in(&parent)?;
        let root = dir.path();
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nresolver = \"3\"\nmembers = [\"one\"]\n",
        )?;
        let crate_dir = root.join("one");
        std::fs::create_dir_all(crate_dir.join("src"))?;
        std::fs::write(
            crate_dir.join("Cargo.toml"),
            "[package]\nname = \"one\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )?;
        // Note: no src/lib.rs written at all -- `cargo metadata` will still
        // resolve the package (it does not require the target to exist to
        // parse the manifest), but the walk must find no files and bail.
        let result = islands_report(root);
        assert!(result.is_err(), "expected a hard failure for a crate with no .rs files");
        Ok(())
    }

    #[test]
    fn integration_test_file_is_test_realm_by_path_alone() -> Result<()> {
        let workspace = workspace_with_lib(
            "integration-realm",
            r#"
                pub fn only_called_from_integration_test() -> u32 { 1 }
            "#,
        )?;
        let root = workspace.deref();
        let tests_dir = root.join("one/tests");
        std::fs::create_dir_all(&tests_dir)?;
        std::fs::write(
            tests_dir.join("it.rs"),
            r#"
                #[test]
                fn calls_it() {
                    assert_eq!(one::only_called_from_integration_test(), 1);
                }
            "#,
        )?;
        let report = islands_report(root)?;
        let one = report.crates.iter().find(|c| c.crate_name == "one").unwrap();
        assert!(
            one
                .tested_only_functions
                .iter()
                .any(|f| f.name == "only_called_from_integration_test"),
            "a tests/ file should count as Test realm even with no #[cfg(test)]: {:?}",
            one.tested_only_functions
        );
        Ok(())
    }

    /// The bug this scanner actually had during development: a crate that
    /// splits its test module into its own file (`#[cfg(test)] mod tests;`
    /// in `lib.rs`, body in `src/tests.rs` -- exactly
    /// `crates/lodestone-model/src/lib.rs`'s shape) carries the `#[cfg(test)]`
    /// on the *declaration*, not inside the file it names. Parsed alone,
    /// `tests.rs` looks like ordinary production code, so a call made from
    /// it used to read as a production call site and hide a real island
    /// (`Family::protocol_version` had its only non-test caller inside
    /// exactly this shape). Confirmed to still hide it in `crate::islands`
    /// (this module has no `#[cfg(test)] mod tests;` of its own to exercise
    /// this against for real, so the workspace-level finding stood in as
    /// the control before this fix).
    #[test]
    fn external_test_module_file_is_test_realm_even_without_its_own_cfg_test() -> Result<()> {
        let workspace = workspace_with_lib(
            "external-test-mod",
            r#"
                #[cfg(test)]
                mod tests;

                pub fn only_called_from_split_out_tests() -> u32 { 1 }
                pub fn truly_dead() -> u32 { 2 }
            "#,
        )?;
        let root = workspace.deref();
        std::fs::write(
            root.join("one/src/tests.rs"),
            r#"
                use super::*;

                #[test]
                fn calls_it() {
                    assert_eq!(only_called_from_split_out_tests(), 1);
                }
            "#,
        )?;
        let report = islands_report(root)?;
        let one = report.crates.iter().find(|c| c.crate_name == "one").unwrap();
        assert!(
            one.tested_only_functions
                .iter()
                .any(|f| f.name == "only_called_from_split_out_tests"),
            "a call from an externally-declared #[cfg(test)] mod file must count as a test \
             call, not a production one: {:?} / dead: {:?}",
            one.tested_only_functions,
            one.dead_functions
        );
        assert!(one.dead_functions.iter().any(|f| f.name == "truly_dead"));
        Ok(())
    }

    /// The real, measured false positive: `PathFinder::find_path`
    /// (`crates/lodestone-entity/src/pathfinding/search.rs`) destructures
    /// `PathParams { max_path_length, reach_range, visited_multiplier }`
    /// with a `let PathParams { .. } = params;` pattern and only ever uses
    /// the bindings afterward -- no `.field` expression access anywhere --
    /// so before `visit_pat_struct`, all three fields read as "zero
    /// production readers".
    #[test]
    fn struct_pattern_destructuring_counts_as_a_field_read() -> Result<()> {
        let workspace = workspace_with_lib(
            "struct-pattern-read",
            r#"
                pub struct Params {
                    pub max_path_length: f32,
                    pub reach_range: i32,
                }

                pub fn find_path(params: Params) -> f32 {
                    let Params { max_path_length, .. } = params;
                    max_path_length
                }
            "#,
        )?;
        let report = islands_report(&workspace)?;
        let one = report.crates.iter().find(|c| c.crate_name == "one").unwrap();

        assert!(
            !one
                .dead_fields
                .iter()
                .any(|f| f.field_name == "max_path_length"),
            "a field bound by a struct-pattern destructure was wrongly flagged dead: {:?}",
            one.dead_fields
        );
        Ok(())
    }

    /// The real, measured false positive: `Brain::sensors`
    /// (`crates/lodestone-entity/src/brain/mod.rs`) is initialised empty and
    /// grown exclusively via `self.sensors.push(sensor)` in `add_sensor` --
    /// a real, meaningful mutation with no `Expr::Assign`/`Expr::Binary`
    /// shape, so before this fix its only recorded assignment was the
    /// `Vec::new()` at construction, reading as "every production
    /// assignment is default-like".
    #[test]
    fn collection_growth_via_push_counts_as_a_non_default_mutation() -> Result<()> {
        let workspace = workspace_with_lib(
            "collection-growth",
            r#"
                pub struct Brain {
                    pub sensors: Vec<u32>,
                }

                pub fn new_brain() -> Brain {
                    Brain { sensors: Vec::new() }
                }

                impl Brain {
                    pub fn add_sensor(&mut self, sensor: u32) {
                        self.sensors.push(sensor);
                    }
                }
            "#,
        )?;
        let report = islands_report(&workspace)?;
        let one = report.crates.iter().find(|c| c.crate_name == "one").unwrap();

        assert!(
            !one
                .default_only_fields
                .iter()
                .any(|f| f.field_name == "sensors"),
            "a Vec field grown only via .push was wrongly called default-only: {:?}",
            one.default_only_fields
        );
        Ok(())
    }

    /// The real, measured false positive: `NavigatingMob::fall_speed`
    /// (`crates/lodestone-entity/src/ai/navigating_mob.rs`) is mutated
    /// exclusively by passing `&mut self.fall_speed` into `step_vertical`,
    /// which writes through the pointer -- no `Expr::Assign`/`Expr::Binary`
    /// at the call site names the field at all, so before this fix its only
    /// recorded assignment was the `0.0` literal at construction.
    #[test]
    fn passing_mut_reference_to_a_field_counts_as_a_non_default_mutation() -> Result<()> {
        let workspace = workspace_with_lib(
            "mut-ref-field",
            r#"
                pub struct Mob {
                    pub fall_speed: f64,
                }

                fn step_vertical(fall_speed: &mut f64) {
                    *fall_speed += 1.0;
                }

                pub fn new_mob() -> Mob {
                    Mob { fall_speed: 0.0 }
                }

                impl Mob {
                    pub fn tick(&mut self) {
                        step_vertical(&mut self.fall_speed);
                    }
                }
            "#,
        )?;
        let report = islands_report(&workspace)?;
        let one = report.crates.iter().find(|c| c.crate_name == "one").unwrap();

        assert!(
            !one
                .default_only_fields
                .iter()
                .any(|f| f.field_name == "fall_speed"),
            "a field mutated only through &mut passed to a function was wrongly called \
             default-only: {:?}",
            one.default_only_fields
        );
        Ok(())
    }

    /// The real, measured false positive: `NavigatingMob::attacks`
    /// (`crates/lodestone-entity/src/ai/navigating_mob.rs`) is drained via
    /// `std::mem::take(&mut self.attacks)` in `take_new_attacks` -- the same
    /// `&mut self.field` shape as `step_vertical` above, isolated here with
    /// no `.push` in the fixture so this test cannot pass on the collection-
    /// growth fix alone; it must be the `&mut`-reference fix that catches it.
    #[test]
    fn mem_take_drain_counts_as_a_non_default_mutation() -> Result<()> {
        let workspace = workspace_with_lib(
            "mem-take-drain",
            r#"
                pub struct Queue {
                    pub attacks: Vec<u32>,
                }

                pub fn new_queue() -> Queue {
                    Queue { attacks: Vec::new() }
                }

                impl Queue {
                    pub fn take_new_attacks(&mut self) -> Vec<u32> {
                        std::mem::take(&mut self.attacks)
                    }
                }
            "#,
        )?;
        let report = islands_report(&workspace)?;
        let one = report.crates.iter().find(|c| c.crate_name == "one").unwrap();

        assert!(
            !one
                .default_only_fields
                .iter()
                .any(|f| f.field_name == "attacks"),
            "a field drained via std::mem::take(&mut self.field) was wrongly called \
             default-only: {:?}",
            one.default_only_fields
        );
        Ok(())
    }
}
