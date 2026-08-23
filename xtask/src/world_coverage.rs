//! `cargo xtask world-coverage` — a census of world content that reaches no
//! draw path.
//!
//! See `docs/world-coverage.md` for the writeup: what each bucket means, what
//! the instrument can and cannot see, and how to run its calibration case
//! before trusting a report.
//!
//! # The question
//!
//! `connectedness` answers "is this clientbound packet reaching anything".
//! This answers a different one, one layer further down the chain: **of the
//! things that can exist in a world, which ones reach no geometry?** Three
//! populations, each taken from the real 26.2 registry rather than from a
//! hand-written list:
//!
//! | population | source of truth | count |
//! |---|---|---|
//! | entity types | `lodestone_data::entity_type::EntityType` | `EntityType::COUNT` |
//! | block-entity types | `lodestone_data::block_entity_types::TYPE_COUNT` | 49 |
//! | particle types | `lodestone_data::particle_types::PARTICLE_TYPE_COUNT` | 125 |
//!
//! Every subject lands in exactly one of three buckets:
//!
//! * **drawn** — something resolves geometry keyed on this subject.
//! * **stranded** — nothing does, *but the subject is named in the client's
//!   own draw surface*. This is the bucket that catches an item frame: a type
//!   with a hitbox entry, a type-path constant, a pose function and a draw
//!   counter, and no renderer of its own. Code that looks like a consumer and
//!   is not reachable from any dispatch that emits geometry.
//! * **absent** — nothing draws it and nothing in the draw surface names it.
//!   Honest missing work, and a much cheaper finding to read than a stranded
//!   one because nobody has half-built it.
//!
//! The reverse direction is reported too, per population: **a renderer no
//! subject routes to**. A rig in the corpus that no registry type resolves to
//! is either a deliberate second-pass mesh or a mesh nothing can reach.
//!
//! # What is mechanical and what is reviewed
//!
//! The populations, the entity rig resolution, the particle dispatch and the
//! whole block-entity analysis are mechanical: they read the real registry
//! through `lodestone-data`, the real corpora through `lodestone-assets`, and
//! the real dispatch tables out of the AST with `syn`.
//!
//! [`RendererClaim`] is the one hand-reviewed surface, and it exists because
//! several renderers here are not tables at all — a dropped item, an
//! experience orb and a primed TNT each reach pixels through a dedicated pass
//! with a bespoke shape. Each entry carries an **anchor**: a file and a symbol
//! that must still be defined, and a rule that must still claim at least one
//! subject. A renderer that is deleted, renamed or moved therefore fails the
//! run rather than silently continuing to vouch for its subjects — the same
//! reason `connectedness`'s `SKIPPED` for a live subject is treated here as a
//! hard failure rather than a quiet zero.
//!
//! Its failure mode is worth stating plainly: an over-broad claim produces a
//! **false negative** (a stranded subject reported as drawn). That is the
//! direction a reviewer cannot see from the output, so keep the claims tight
//! and prefer a mechanical rule to an [`ClaimRule::Explicit`] list.
//!
//! # Why `syn` and not a regex
//!
//! Three hand-rolled scanners in this repo were wrong about lifetimes:
//! `&'static str` opened a "char literal" flag that never closed. Everything
//! here goes through `syn::parse_file` and `syn::visit::Visit`, so a lifetime,
//! a `//` inside a string literal, or a `"` inside a raw string cannot fool
//! it. Macro bodies are the one place `syn` hands back an opaque token
//! stream; those are walked as `proc_macro2` tokens so a `matches!(path,
//! "minecraft:conduit" | ...)` predicate is still seen.
use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use syn::visit::Visit;

// ---------------------------------------------------------------------------
// What gets scanned
// ---------------------------------------------------------------------------

/// The client's **draw surface**: every path whose contents are read as
/// evidence that some code names a subject.
///
/// A mention here is what separates *stranded* from *absent*, so the set is
/// deliberately the client's rendering and render-feeding code and nothing
/// else. A server-side mention of `item_frame` says nothing about whether a
/// frame draws.
///
/// Every entry must exist on disk. A path that has moved makes the scan
/// quietly narrower, which is the failure `connectedness` shipped for a whole
/// session when `adapter.rs` became `adapter/`, so a missing entry is a hard
/// failure rather than a skipped directory.
const DRAW_SURFACE: &[&str] = &[
    "crates/lodestone-render/src",
    "crates/lodestone-assets/src",
    "crates/lodestone-particle/src",
    "crates/lodestone-shell/src/gpu",
    "crates/lodestone-shell/src/gpu.rs",
    "crates/lodestone-shell/src/entities.rs",
    "crates/lodestone-shell/src/display_entities.rs",
    "crates/lodestone-shell/src/block_entities.rs",
    "crates/lodestone-shell/src/particles.rs",
    "crates/lodestone-shell/src/consume.rs",
    "crates/lodestone-shell/src/interact.rs",
    "crates/lodestone-shell/src/sim",
    "crates/lodestone-shell/src/app",
];

/// The subset of [`DRAW_SURFACE`] whose mentions are evidence about an
/// **entity**.
///
/// Narrower than the parse set on purpose. The parse set has to reach
/// `sim/` and `app/` for the render-source wiring diff, but a bare registry
/// path is not a namespace: `firework_rocket` appearing in an elytra-boost
/// gameplay system says nothing about whether a firework draws, and counting
/// it would turn a clean *absent* into a misleading *stranded*.
const ENTITY_MENTION_SURFACE: &[&str] = &[
    "crates/lodestone-render/src",
    "crates/lodestone-assets/src",
    "crates/lodestone-shell/src/gpu",
    "crates/lodestone-shell/src/entities.rs",
    "crates/lodestone-shell/src/display_entities.rs",
];

/// The same, for **particles**.
///
/// The tightest of the three, because particle names collide with entity and
/// block names more than any other population — `lava`, `dolphin`,
/// `nautilus`, `composter` and `elder_guardian` are all particle types *and*
/// something else, and every one of them was a false *stranded* against the
/// full parse set.
const PARTICLE_MENTION_SURFACE: &[&str] = &[
    "crates/lodestone-particle/src",
    "crates/lodestone-shell/src/particles.rs",
    "crates/lodestone-shell/src/consume.rs",
];

/// The subset of [`DRAW_SURFACE`] that actually rasterises: the files a
/// block-entity gather predicate or a per-type pass lives in.
///
/// Used for the block-entity analysis, which asks "does any predicate in the
/// gather layer match a block that owns this type" — a question that would be
/// answered wrongly by a literal sitting in, say, the creative-inventory item
/// list.
const BLOCK_ENTITY_GATHER_SURFACE: &[&str] = &[
    "crates/lodestone-shell/src/block_entities.rs",
    "crates/lodestone-render/src/block_entity.rs",
];

/// String methods whose literal argument is a **rule**, not a name: a
/// predicate written `path.ends_with("_sign")` claims every block whose name
/// ends that way, and a scan that only collected whole literals would report
/// every sign as unreached.
const AFFIX_METHODS: &[&str] = &["ends_with", "starts_with", "strip_suffix", "strip_prefix"];

// ---------------------------------------------------------------------------
// The source index
// ---------------------------------------------------------------------------

/// One place in the scanned source that names something — a file plus the
/// enclosing item path (`gpu/maps.rs` / `ITEM_FRAME_TYPES`).
///
/// Symbols rather than line numbers, deliberately: a line number is wrong the
/// next time anyone inserts a function above it.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Site {
    /// Workspace-relative path.
    pub file: String,
    /// The enclosing item path, `::`-joined, outermost first.
    pub symbol: String,
}

impl std::fmt::Display for Site {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.symbol.is_empty() {
            write!(f, "{}", self.file)
        } else {
            write!(f, "{}::{}", self.file, self.symbol)
        }
    }
}

/// Everything the AST walk collected, keyed for the queries the three
/// populations ask of it.
#[derive(Debug, Default)]
pub struct SourceIndex {
    /// Every string literal → the sites it appears at.
    literals: BTreeMap<String, BTreeSet<Site>>,
    /// String literals appearing inside a `match` arm **pattern** (or inside a
    /// macro body, which is where `matches!` predicates live). This is the
    /// dispatch-table query: `spawn_one`'s arms, not the strings its bodies
    /// happen to mention.
    arm_literals: BTreeMap<String, BTreeSet<Site>>,
    /// `Enum::Variant` paths appearing inside a `match` arm pattern, keyed by
    /// `(enum, variant)`.
    arm_variants: BTreeMap<(String, String), BTreeSet<Site>>,
    /// Literal arguments to [`AFFIX_METHODS`] — suffix/prefix rules.
    affixes: BTreeMap<String, BTreeSet<Site>>,
    /// Every item this scan defines, so a manifest anchor can be checked.
    defined: BTreeSet<Site>,
    /// Named struct fields, for the render-source wiring diff.
    fields: BTreeMap<String, BTreeSet<Site>>,
    /// Method-call names, for the same.
    calls: BTreeMap<String, BTreeSet<Site>>,
    /// How many files parsed, and how many refused to.
    files_scanned: usize,
    parse_failures: Vec<String>,
}

impl SourceIndex {
    /// Sites naming `literal` anywhere in the scanned surface.
    fn literal_sites(&self, literal: &str) -> Vec<Site> {
        self.literals
            .get(literal)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Is `symbol` defined in `file`?
    ///
    /// Matches on the **last** component of the item path so that an anchor
    /// may name a method without also naming the `impl` block it sits in —
    /// an `impl` block's identity is a type, and a method can move between
    /// two `impl`s of the same type without changing anything a reader cares
    /// about.
    fn defines(&self, file: &str, symbol: &str) -> bool {
        self.defined.iter().any(|site| {
            site.file == file && site.symbol.rsplit("::").next().is_some_and(|s| s == symbol)
        })
    }

    /// Every string literal lexically inside `file`'s `symbol`.
    fn literals_in(&self, file: &str, symbol: &str) -> BTreeSet<String> {
        self.select(&self.literals, file, symbol)
    }

    /// Every string literal in a `match`-arm pattern inside `file`'s `symbol`.
    fn arm_literals_in(&self, file: &str, symbol: &str) -> BTreeSet<String> {
        self.select(&self.arm_literals, file, symbol)
    }

    /// Every `enum_name::Variant` in a `match`-arm pattern inside `file`'s
    /// `symbol`.
    fn arm_variants_in(&self, file: &str, symbol: &str, enum_name: &str) -> BTreeSet<String> {
        self.arm_variants
            .iter()
            .filter(|((e, _), sites)| {
                e == enum_name && sites.iter().any(|s| site_within(s, file, symbol))
            })
            .map(|((_, variant), _)| variant.clone())
            .collect()
    }

    fn select(
        &self,
        map: &BTreeMap<String, BTreeSet<Site>>,
        file: &str,
        symbol: &str,
    ) -> BTreeSet<String> {
        map.iter()
            .filter(|(_, sites)| sites.iter().any(|s| site_within(s, file, symbol)))
            .map(|(literal, _)| literal.clone())
            .collect()
    }
}

/// Is `site` inside `file`'s `symbol`? True for the symbol itself and for
/// anything nested in it (a `const TABLE` declared inside a function body is
/// inside that function).
fn site_within(site: &Site, file: &str, symbol: &str) -> bool {
    site.file == file && site.symbol.split("::").any(|s| s == symbol)
}

// ---------------------------------------------------------------------------
// The AST walk
// ---------------------------------------------------------------------------

struct Indexer<'a> {
    file: String,
    stack: Vec<String>,
    index: &'a mut SourceIndex,
    /// Non-zero while inside a `match` arm's pattern.
    in_pattern: usize,
    /// Non-zero while inside an [`AFFIX_METHODS`] call's arguments.
    in_affix: usize,
}

impl Indexer<'_> {
    fn site(&self) -> Site {
        Site {
            file: self.file.clone(),
            symbol: self.stack.join("::"),
        }
    }

    fn record_definition(&mut self) {
        let site = self.site();
        self.index.defined.insert(site);
    }

    fn scoped<T>(&mut self, name: String, f: impl FnOnce(&mut Self) -> T) {
        self.stack.push(name);
        self.record_definition();
        f(self);
        self.stack.pop();
    }

    fn record_literal(&mut self, value: String) {
        let site = self.site();
        self.index
            .literals
            .entry(value.clone())
            .or_default()
            .insert(site.clone());
        if self.in_pattern > 0 {
            self.index
                .arm_literals
                .entry(value.clone())
                .or_default()
                .insert(site.clone());
        }
        if self.in_affix > 0 {
            self.index.affixes.entry(value).or_default().insert(site);
        }
    }
}

/// Does this item carry `#[cfg(test)]`?
///
/// A mention that exists only in a test is not evidence that anything draws:
/// the whole point of the *stranded* bucket is production code that looks
/// like a consumer, and a fixture constructing an `EntityDraw` for a giant
/// would otherwise read as one.
fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        if !attr.path().is_ident("cfg") {
            return false;
        }
        let mut found = false;
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("test") {
                found = true;
            }
            Ok(())
        });
        found
    })
}

impl<'ast> Visit<'ast> for Indexer<'_> {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if is_cfg_test(&node.attrs) || node.ident == "tests" {
            return;
        }
        let name = node.ident.to_string();
        self.scoped(name, |me| syn::visit::visit_item_mod(me, node));
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        if is_cfg_test(&node.attrs) {
            return;
        }
        let name = node.sig.ident.to_string();
        self.scoped(name, |me| syn::visit::visit_item_fn(me, node));
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        if is_cfg_test(&node.attrs) {
            return;
        }
        let name = node.sig.ident.to_string();
        self.scoped(name, |me| syn::visit::visit_impl_item_fn(me, node));
    }

    fn visit_item_const(&mut self, node: &'ast syn::ItemConst) {
        if is_cfg_test(&node.attrs) {
            return;
        }
        let name = node.ident.to_string();
        self.scoped(name, |me| syn::visit::visit_item_const(me, node));
    }

    fn visit_item_static(&mut self, node: &'ast syn::ItemStatic) {
        if is_cfg_test(&node.attrs) {
            return;
        }
        let name = node.ident.to_string();
        self.scoped(name, |me| syn::visit::visit_item_static(me, node));
    }

    fn visit_item_struct(&mut self, node: &'ast syn::ItemStruct) {
        if is_cfg_test(&node.attrs) {
            return;
        }
        let name = node.ident.to_string();
        self.scoped(name, |me| syn::visit::visit_item_struct(me, node));
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        if is_cfg_test(&node.attrs) {
            return;
        }
        let name = impl_type_name(&node.self_ty);
        self.scoped(name, |me| syn::visit::visit_item_impl(me, node));
    }

    fn visit_field(&mut self, node: &'ast syn::Field) {
        if let Some(ident) = &node.ident {
            let site = self.site();
            self.index
                .fields
                .entry(ident.to_string())
                .or_default()
                .insert(site);
        }
        syn::visit::visit_field(self, node);
    }

    fn visit_arm(&mut self, node: &'ast syn::Arm) {
        // Only the pattern is `in_pattern`: an arm's *body* mentioning a name
        // is not that arm dispatching on it, and conflating the two is what
        // would make `spawn_one`'s bodies look like dispatch keys.
        self.in_pattern += 1;
        self.visit_pat(&node.pat);
        self.in_pattern -= 1;
        self.visit_expr(&node.body);
    }

    fn visit_expr_path(&mut self, node: &'ast syn::ExprPath) {
        // `syn` models a path *pattern* (`EntityType::Player`) as an
        // `ExprPath`, so this one override serves both positions; the
        // `in_pattern` gate inside `record_path_variant` is what keeps an
        // ordinary expression path out of the arm-variant index.
        self.record_path_variant(&node.path);
        syn::visit::visit_expr_path(self, node);
    }

    fn visit_pat_tuple_struct(&mut self, node: &'ast syn::PatTupleStruct) {
        self.record_path_variant(&node.path);
        syn::visit::visit_pat_tuple_struct(self, node);
    }

    fn visit_pat_struct(&mut self, node: &'ast syn::PatStruct) {
        self.record_path_variant(&node.path);
        syn::visit::visit_pat_struct(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        let method = node.method.to_string();
        let site = self.site();
        self.index
            .calls
            .entry(method.clone())
            .or_default()
            .insert(site);
        self.visit_expr(&node.receiver);
        let affix = AFFIX_METHODS.contains(&method.as_str());
        if affix {
            self.in_affix += 1;
        }
        for arg in &node.args {
            self.visit_expr(arg);
        }
        if affix {
            self.in_affix -= 1;
        }
    }

    fn visit_lit_str(&mut self, node: &'ast syn::LitStr) {
        self.record_literal(node.value());
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        // A macro body is an opaque token stream to `syn`, and one of the
        // block-entity gather predicates is a `matches!(path, "a" | "b")`.
        // Walking the raw tokens is the only way to see those literals, and
        // they are recorded as arm literals because that is what a
        // `matches!` arm is.
        self.in_pattern += 1;
        record_macro_literals(self, node.tokens.clone());
        self.in_pattern -= 1;
    }
}

impl Indexer<'_> {
    fn record_path_variant(&mut self, path: &syn::Path) {
        if self.in_pattern == 0 || path.segments.len() < 2 {
            return;
        }
        let n = path.segments.len();
        let enum_name = path.segments[n - 2].ident.to_string();
        let variant = path.segments[n - 1].ident.to_string();
        let site = self.site();
        self.index
            .arm_variants
            .entry((enum_name, variant))
            .or_default()
            .insert(site);
    }
}

fn record_macro_literals(indexer: &mut Indexer<'_>, tokens: proc_macro2::TokenStream) {
    for token in tokens {
        match token {
            proc_macro2::TokenTree::Literal(lit) => {
                if let Ok(lit) = syn::parse_str::<syn::LitStr>(&lit.to_string()) {
                    indexer.record_literal(lit.value());
                }
            }
            proc_macro2::TokenTree::Group(group) => {
                record_macro_literals(indexer, group.stream());
            }
            _ => {}
        }
    }
}

/// A readable name for an `impl` block's self type, so a method's site is
/// `Particles::spawn_one` rather than `spawn_one`.
fn impl_type_name(ty: &syn::Type) -> String {
    match ty {
        syn::Type::Path(path) => path
            .path
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .unwrap_or_else(|| "impl".to_owned()),
        _ => "impl".to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Building the index
// ---------------------------------------------------------------------------

/// Every `.rs` file under `entry`, which may itself be a file.
fn rust_files(root: &Path, entry: &str) -> Result<Vec<PathBuf>> {
    let path = root.join(entry);
    if !path.exists() {
        bail!(
            "world-coverage: declared draw-surface path {entry:?} does not exist.\n  \
             This is a hard failure and not a skip: a moved module makes the scan silently \
             narrower, which is exactly how `connectedness` reported v770 as SKIPPED for a \
             whole session. Update DRAW_SURFACE in xtask/src/world_coverage.rs."
        );
    }
    if path.is_file() {
        return Ok(vec![path]);
    }
    let mut out = Vec::new();
    let mut stack = vec![path];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).with_context(|| format!("read dir {}", dir.display()))? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// The files reached only through a `#[cfg(test)] mod name;` **declaration**,
/// which live in their own file and therefore carry no `#[cfg(test)]` of
/// their own.
///
/// Skipping these is what stops a fixture from voting. `gpu/pixel_gates.rs`
/// and `gpu/tests.rs` construct an `EntityDraw` for mobs this client has no
/// rig for; counting those as production mentions would report an unported
/// mob as half-built rather than unported, which is a worse answer than
/// either truth.
fn test_only_module_files(root: &Path, files: &[PathBuf]) -> Result<BTreeSet<PathBuf>> {
    let mut skip = BTreeSet::new();
    for path in files {
        let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        let Ok(parsed) = syn::parse_file(&text) else {
            continue;
        };
        // `foo.rs` declares its children in `foo/`; `lib.rs`/`mod.rs` declare
        // theirs beside themselves.
        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned());
        let dir = match stem.as_deref() {
            Some("lib" | "main" | "mod") | None => path.parent().map(Path::to_path_buf),
            Some(stem) => path.parent().map(|p| p.join(stem)),
        };
        let Some(dir) = dir else { continue };
        for item in &parsed.items {
            let syn::Item::Mod(module) = item else { continue };
            if module.content.is_some() || !is_cfg_test(&module.attrs) {
                continue;
            }
            let name = module.ident.to_string();
            skip.insert(dir.join(format!("{name}.rs")));
            skip.insert(dir.join(&name).join("mod.rs"));
        }
    }
    let _ = root;
    Ok(skip)
}

/// Parse every file in [`DRAW_SURFACE`] and build the index.
pub fn build_source_index(root: &Path) -> Result<SourceIndex> {
    let mut index = SourceIndex::default();
    let mut all_files = Vec::new();
    for entry in DRAW_SURFACE {
        all_files.extend(rust_files(root, entry)?);
    }
    all_files.sort();
    all_files.dedup();
    let skip = test_only_module_files(root, &all_files)?;
    {
        for path in all_files {
            if skip.contains(&path) {
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let text = fs::read_to_string(&path)
                .with_context(|| format!("read {}", path.display()))?;
            match syn::parse_file(&text) {
                Ok(file) => {
                    index.files_scanned += 1;
                    let mut indexer = Indexer {
                        file: relative,
                        stack: Vec::new(),
                        index: &mut index,
                        in_pattern: 0,
                        in_affix: 0,
                    };
                    indexer.visit_file(&file);
                }
                Err(err) => index.parse_failures.push(format!("{relative}: {err}")),
            }
        }
    }
    if index.files_scanned == 0 {
        bail!("world-coverage: scanned zero files — the draw surface resolved to nothing");
    }
    if !index.parse_failures.is_empty() {
        bail!(
            "world-coverage: {} file(s) in the draw surface failed to parse, so the scan is \
             incomplete and its \"no findings\" would be a lie:\n  {}",
            index.parse_failures.len(),
            index.parse_failures.join("\n  ")
        );
    }
    Ok(index)
}

// ---------------------------------------------------------------------------
// The renderer manifest
// ---------------------------------------------------------------------------

/// How a [`RendererClaim`] works out which subjects it covers.
#[derive(Clone, Copy, Debug)]
pub enum ClaimRule {
    /// Every string literal lexically inside the anchor symbol that names a
    /// subject. Right for a small keyed table like `thrown_item_for`'s.
    LiteralsInSymbol,
    /// Every string literal in a `match`-arm **pattern** inside the anchor
    /// symbol. Right for a dispatch `match` whose bodies mention other
    /// strings, like `Particles::spawn_one`.
    ArmLiteralsInSymbol,
    /// Every `Enum::Variant` in a `match`-arm pattern inside the anchor
    /// symbol, mapped back to a subject by the variant's registry path.
    ArmVariantsInSymbol(&'static str),
    /// Every literal in the anchor symbol that begins with `_`, read as an id
    /// **suffix** — `boat_model_name`'s `ends_with("_chest_boat")` ladder.
    SuffixLiteralsInSymbol,
    /// A reviewed list. Use only where the pass has no table to read: a
    /// dedicated per-type pass such as `prepare_orbs`.
    Explicit(&'static [&'static str]),
}

/// One thing that turns a subject into geometry.
#[derive(Debug)]
pub struct RendererClaim {
    /// What it draws, for the report.
    pub name: &'static str,
    /// Workspace-relative anchor file. Must exist and must define `symbol`.
    pub file: &'static str,
    /// Anchor symbol. Its disappearance is a hard failure, not a silent zero.
    pub symbol: &'static str,
    /// How the claimed ids are worked out.
    pub rule: ClaimRule,
}

/// Entity types that reach geometry through something other than the
/// `entity_models` rig corpus.
///
/// The corpus itself is not in this table — it is resolved mechanically from
/// `lodestone_assets::entity_models()` plus the two rules in
/// `lodestone-render`'s `entity` module, and covers the large majority of the
/// registry. What is left is the handful of types vanilla draws with a
/// dedicated renderer rather than a `ModelPart` rig.
const ENTITY_RENDERERS: &[RendererClaim] = &[
    // `ThrownItemRenderer`: an item billboard, not a rig. The table inside the
    // function is the complete 26.2 registration list.
    RendererClaim {
        name: "thrown item billboard",
        file: "crates/lodestone-render/src/entity.rs",
        symbol: "thrown_item_for",
        rule: ClaimRule::LiteralsInSymbol,
    },
    // Types vanilla draws with another mob's model class, matched on the enum
    // discriminant rather than the path string.
    RendererClaim {
        name: "rig alias",
        file: "crates/lodestone-render/src/entity.rs",
        symbol: "canonical_model_name_for_type",
        rule: ClaimRule::ArmVariantsInSymbol("EntityType"),
    },
    // The boat/raft family: one rig per shape, selected by path suffix.
    RendererClaim {
        name: "boat/raft suffix rule",
        file: "crates/lodestone-render/src/entity.rs",
        symbol: "boat_model_name",
        rule: ClaimRule::SuffixLiteralsInSymbol,
    },
    // `ItemEntityRenderer`: the dropped stack's own baked item model, bob and
    // spin included. Keyed off `EntityDraw::item`, not off a rig.
    RendererClaim {
        name: "dropped item model",
        file: "crates/lodestone-shell/src/gpu/world_items.rs",
        symbol: "prepare_item_geometry",
        rule: ClaimRule::Explicit(&["item"]),
    },
    // `ExperienceOrbRenderer`: one camera-facing quad off a standalone sheet.
    RendererClaim {
        name: "experience orb sprite",
        file: "crates/lodestone-shell/src/gpu/entity_passes.rs",
        symbol: "prepare_orbs",
        rule: ClaimRule::Explicit(&["experience_orb"]),
    },
    // `FallingBlockRenderer`/`TntRenderer`: a block model at the entity's pose.
    RendererClaim {
        name: "moving block model",
        file: "crates/lodestone-shell/src/gpu/moving_blocks.rs",
        symbol: "prepare_moving_blocks",
        rule: ClaimRule::Explicit(&["falling_block", "tnt"]),
    },
    // `DisplayRenderer.TextDisplayRenderer`.
    RendererClaim {
        name: "text display glyphs",
        file: "crates/lodestone-shell/src/gpu/display_text.rs",
        symbol: "push_text_display_quads",
        rule: ClaimRule::Explicit(&["text_display"]),
    },
    // `DisplayRenderer.BlockDisplayRenderer`: the imitated block state's own
    // baked quads, posed by the display's billboard + `Transformation`. Its own
    // claim rather than a second id on the moving-block one above, because the
    // anchor check is the point: `merge_block_displays` disappearing must fail
    // this scan, and it would not if `block_display` rode on
    // `prepare_moving_blocks`' anchor.
    RendererClaim {
        name: "block display model",
        file: "crates/lodestone-shell/src/gpu/moving_blocks.rs",
        symbol: "merge_block_displays",
        rule: ClaimRule::Explicit(&["block_display"]),
    },
    // `DisplayRenderer.ItemDisplayRenderer`: the stack's own item model, posed
    // the same way. Separate from the dropped-item claim for the same
    // anchor-check reason as the block one.
    RendererClaim {
        name: "item display model",
        file: "crates/lodestone-shell/src/gpu/world_items.rs",
        symbol: "merge_item_displays",
        rule: ClaimRule::Explicit(&["item_display"]),
    },
    // `PaintingRenderer`: a flat slab of `width x height` blocks, its front
    // face the variant's own sprite and its back and edges a shared tile.
    // Neither a rig nor a billboard, so it has its own pass rather than a
    // corpus entry.
    RendererClaim {
        name: "painting slab",
        file: "crates/lodestone-shell/src/gpu/entity_passes.rs",
        symbol: "prepare_paintings",
        rule: ClaimRule::Explicit(&["painting"]),
    },
];

/// Particle types that reach geometry through something other than the
/// wire-driven `Particles::spawn_one` dispatch.
const PARTICLE_RENDERERS: &[RendererClaim] = &[
    // The one real dispatch: a `match` on the namespace-stripped path. Its
    // catch-all is a hard drop with no fallback sprite, so an arm here is the
    // whole of "this type draws".
    RendererClaim {
        name: "wire dispatch",
        file: "crates/lodestone-shell/src/particles.rs",
        symbol: "spawn_one",
        rule: ClaimRule::ArmLiteralsInSymbol,
    },
    // Client-predicted block-break debris. Semantically `minecraft:block`,
    // but emitted locally off a block state and never through a registry id.
    RendererClaim {
        name: "local block-break debris",
        file: "crates/lodestone-shell/src/particles.rs",
        symbol: "destroy_block",
        rule: ClaimRule::Explicit(&["block"]),
    },
    // Client-predicted eating/drinking crumbs, likewise local and
    // item-sprite-sourced rather than registry-driven.
    RendererClaim {
        name: "local item crumbs",
        file: "crates/lodestone-shell/src/consume.rs",
        symbol: "emit_consume_particles",
        rule: ClaimRule::Explicit(&["item"]),
    },
];

/// Resolve one claim against the index, failing loudly if its anchor has gone
/// or if it now claims nothing.
fn resolve_claim(
    index: &SourceIndex,
    claim: &RendererClaim,
    population: &BTreeSet<String>,
) -> Result<BTreeSet<String>> {
    if !index.defines(claim.file, claim.symbol) {
        bail!(
            "world-coverage: renderer {:?} anchors on {}::{}, which the draw-surface scan does \
             not define.\n  Either the symbol was renamed or moved, or its file left \
             DRAW_SURFACE. A claim whose anchor is gone must not keep vouching for its \
             subjects, so this is a hard failure — fix ENTITY_RENDERERS/PARTICLE_RENDERERS in \
             xtask/src/world_coverage.rs.",
            claim.name,
            claim.file,
            claim.symbol
        );
    }
    let claimed: BTreeSet<String> = match claim.rule {
        ClaimRule::LiteralsInSymbol => index
            .literals_in(claim.file, claim.symbol)
            .into_iter()
            .filter(|lit| population.contains(lit))
            .collect(),
        ClaimRule::ArmLiteralsInSymbol => index
            .arm_literals_in(claim.file, claim.symbol)
            .into_iter()
            .filter(|lit| population.contains(lit))
            .collect(),
        ClaimRule::ArmVariantsInSymbol(enum_name) => {
            let variants = index.arm_variants_in(claim.file, claim.symbol, enum_name);
            population
                .iter()
                .filter(|id| variants.contains(&pascal_case(id)))
                .cloned()
                .collect()
        }
        ClaimRule::SuffixLiteralsInSymbol => {
            let suffixes: Vec<String> = index
                .literals_in(claim.file, claim.symbol)
                .into_iter()
                .filter(|lit| lit.starts_with('_'))
                .collect();
            if suffixes.is_empty() {
                bail!(
                    "world-coverage: renderer {:?} reads suffix rules out of {}::{} and found \
                     none. The function still exists but no longer spells its rules as `_`-\
                     prefixed literals, so the rule is measuring nothing.",
                    claim.name,
                    claim.file,
                    claim.symbol
                );
            }
            population
                .iter()
                .filter(|id| suffixes.iter().any(|suffix| id.ends_with(suffix)))
                .cloned()
                .collect()
        }
        ClaimRule::Explicit(ids) => {
            let unknown: Vec<&str> = ids
                .iter()
                .copied()
                .filter(|id| !population.contains(*id))
                .collect();
            if !unknown.is_empty() {
                bail!(
                    "world-coverage: renderer {:?} explicitly claims {unknown:?}, which is not \
                     in the registry population. A claim on a subject that does not exist is a \
                     typo or a version drift, not a coverage fact.",
                    claim.name
                );
            }
            ids.iter().map(|id| (*id).to_owned()).collect()
        }
    };
    if claimed.is_empty() {
        bail!(
            "world-coverage: renderer {:?} ({}::{}) claims zero subjects. A rule that matches \
             nothing has stopped working — an empty claim must never read the same as a \
             renderer that legitimately covers nothing.",
            claim.name,
            claim.file,
            claim.symbol
        );
    }
    Ok(claimed)
}

/// `chest_minecart` → `ChestMinecart`, so a registry path can be compared
/// against an enum variant a `match` arm names.
fn pascal_case(path: &str) -> String {
    path.split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The report
// ---------------------------------------------------------------------------

/// Which bucket a subject fell into.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Verdict {
    /// Something resolves geometry keyed on this subject.
    Drawn(String),
    /// Nothing does, but the client draw surface names it — half-built.
    Stranded,
    /// Nothing draws it and nothing names it.
    Absent,
    /// Nothing draws it here **and nothing draws it in vanilla either** — a
    /// marker, an interaction box, a hopper's block entity. Not a finding.
    /// Only ever assigned when the vanilla oracle was actually readable.
    NoVanillaRenderer,
}

/// One registry entry and what became of it.
#[derive(Debug)]
pub struct Subject {
    pub id: String,
    pub verdict: Verdict,
    /// Where the draw surface names it, for a stranded subject.
    pub mentions: Vec<Site>,
}

/// One population's census.
#[derive(Debug)]
pub struct PopulationReport {
    pub name: &'static str,
    /// How many registry entries were examined.
    pub examined: usize,
    /// How many the registry says there are. A run where these differ has not
    /// looked at everything and must not read as clean.
    pub expected: usize,
    /// How many claim rules resolved, and how many were declared.
    pub detectors_ran: usize,
    pub detectors_declared: usize,
    pub subjects: Vec<Subject>,
    /// Renderers/rigs no subject routes to, with a note where one is known.
    pub unrouted: Vec<(String, &'static str)>,
}

impl PopulationReport {
    /// The two buckets that are findings.
    ///
    /// Deliberately **not** "everything that is not drawn": a subject vanilla
    /// itself has no renderer for is not a hole, and folding it in here is how
    /// a census turns into a number nobody acts on.
    #[must_use]
    pub fn findings(&self) -> usize {
        self.subjects
            .iter()
            .filter(|s| matches!(s.verdict, Verdict::Stranded | Verdict::Absent))
            .count()
    }
}

/// A render source declared on the GPU state but never installed.
#[derive(Debug)]
pub struct SourceWiring {
    pub declared: Vec<String>,
    pub installed: Vec<String>,
    pub never_installed: Vec<String>,
}

#[derive(Debug)]
pub struct WorldCoverageReport {
    pub populations: Vec<PopulationReport>,
    pub wiring: SourceWiring,
    pub oracle: VanillaOracle,
    pub files_scanned: usize,
}

// ---------------------------------------------------------------------------
// The vanilla oracle
// ---------------------------------------------------------------------------

/// Where the pinned 26.2 decompile keeps its two renderer registries.
///
/// These are **inputs**, in the same sense `registries.json` is: a subject
/// nothing draws here is only a finding if something draws it there. Without
/// them the census cannot tell "we have not built this" from "there is
/// nothing to build", and 23 of the 49 block-entity types would read as holes
/// when every one of them is correct.
const VANILLA_ENTITY_RENDERERS: &str =
    ".cache/mc/26.2/client-src/net/minecraft/client/renderer/entity/EntityRenderers.java";
const VANILLA_BLOCK_ENTITY_RENDERERS: &str =
    ".cache/mc/26.2/client-src/net/minecraft/client/renderer/blockentity/BlockEntityRenderers.java";

/// What vanilla itself draws.
#[derive(Debug, Default)]
pub struct VanillaOracle {
    /// Entity paths registered against a renderer that draws nothing.
    ///
    /// Only the **positive** signal is used. The registration list is not one
    /// call per type — a dozen types are registered through shared helpers and
    /// loops rather than by name — so "absent from this file" is not evidence
    /// of anything, while "named here against a no-op renderer" is.
    entity_no_render: BTreeSet<String>,
    /// Block-entity paths with any renderer at all. This list *is* one call
    /// per type, so absence from it is conclusive.
    block_entity_rendered: Option<BTreeSet<String>>,
    /// Why the oracle is degraded, if it is. Printed rather than swallowed:
    /// "could not look" must never share a value with "no findings".
    pub unavailable: Vec<String>,
}

/// Pull `register(<Owner>.<CONSTANT>, <Renderer>` out of a Java source file.
///
/// A literal scan rather than a parser, and deliberately so: the pattern is
/// fixed, and the lifetime hazard that broke this repo's hand-rolled *Rust*
/// scanners has no analogue in Java. The count floors below are what stop a
/// changed spelling from reading as "vanilla draws nothing".
fn java_registrations(text: &str, owner: &str) -> Vec<(String, String)> {
    let needle = format!("register({owner}.");
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find(&needle) {
        rest = &rest[at + needle.len()..];
        let Some(comma) = rest.find(',') else { break };
        let constant = rest[..comma].trim();
        if constant.is_empty()
            || !constant
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        {
            continue;
        }
        let tail = rest[comma + 1..].trim_start();
        let renderer: String = tail
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        out.push((constant.to_ascii_lowercase(), renderer));
    }
    out
}

/// Read both registries, or record precisely why not.
fn vanilla_oracle(root: &Path) -> Result<VanillaOracle> {
    let mut oracle = VanillaOracle::default();

    let entity_path = root.join(VANILLA_ENTITY_RENDERERS);
    match fs::read_to_string(&entity_path) {
        Ok(text) => {
            let regs = java_registrations(&text, "EntityTypes");
            if regs.len() < 100 {
                bail!(
                    "world-coverage: {VANILLA_ENTITY_RENDERERS} yielded {} registrations, well                      under the floor. The file is present but the scan did not understand it,                      which is a detector error and must not be reported as \"vanilla draws                      nothing\".",
                    regs.len()
                );
            }
            oracle.entity_no_render = regs
                .iter()
                .filter(|(_, renderer)| renderer == "NoopRenderer")
                .map(|(name, _)| name.clone())
                .collect();
            if oracle.entity_no_render.is_empty() {
                bail!(
                    "world-coverage: {VANILLA_ENTITY_RENDERERS} parsed but named no draw-nothing                      renderer. That set has never been empty; treat this as the scan breaking,                      not as the game changing."
                );
            }
        }
        Err(err) => oracle
            .unavailable
            .push(format!("{VANILLA_ENTITY_RENDERERS}: {err}")),
    }

    let block_entity_path = root.join(VANILLA_BLOCK_ENTITY_RENDERERS);
    match fs::read_to_string(&block_entity_path) {
        Ok(text) => {
            let regs = java_registrations(&text, "BlockEntityTypes");
            if regs.len() < 20 {
                bail!(
                    "world-coverage: {VANILLA_BLOCK_ENTITY_RENDERERS} yielded {} registrations,                      under the floor — a detector error, not a coverage fact.",
                    regs.len()
                );
            }
            oracle.block_entity_rendered =
                Some(regs.into_iter().map(|(name, _)| name).collect());
        }
        Err(err) => oracle
            .unavailable
            .push(format!("{VANILLA_BLOCK_ENTITY_RENDERERS}: {err}")),
    }

    Ok(oracle)
}

// ---------------------------------------------------------------------------
// Populations
// ---------------------------------------------------------------------------

/// Every mention of `id` in the draw surface — the bare path and the
/// namespaced form both count, since the tree uses both conventions and which
/// one a given layer uses is not something a coverage question should depend
/// on.
fn mentions_of(index: &SourceIndex, id: &str, surface: &[&str]) -> Vec<Site> {
    let mut sites = index.literal_sites(id);
    sites.extend(index.literal_sites(&format!("minecraft:{id}")));
    sites.retain(|site| in_surface(&site.file, surface));
    sites.sort();
    sites.dedup();
    sites
}

/// Is `file` inside one of `surface`'s roots? A root may be a file or a
/// directory; a directory match requires the separator so that
/// `.../gpu.rs` does not read as being inside `.../gpu`.
fn in_surface(file: &str, surface: &[&str]) -> bool {
    surface
        .iter()
        .any(|root| file == *root || file.starts_with(&format!("{root}/")))
}

fn entity_population(index: &SourceIndex, oracle: &VanillaOracle) -> Result<PopulationReport> {
    use lodestone_data::entity_type::EntityType;

    let expected = usize::from(EntityType::COUNT);
    let mut ids = Vec::with_capacity(expected);
    for id in 0..EntityType::COUNT {
        let entity_type = EntityType::from_registry_id(id).with_context(|| {
            format!("world-coverage: entity registry id {id} has no type — table truncated")
        })?;
        ids.push(entity_type.path().to_owned());
    }
    if ids.len() != expected {
        bail!("world-coverage: entity registry yielded {} of {expected}", ids.len());
    }
    let population: BTreeSet<String> = ids.iter().cloned().collect();

    // The rig corpus, taken from the real corpus rather than a transcription
    // of it: a type whose registry path *is* a corpus entry name resolves to
    // that entry, which is how `canonical_model_name_for_type` works and why a
    // newly ported mesh makes its mob drawable with no table edit.
    let rigs: BTreeSet<&'static str> = lodestone_assets::entity_models::entity_models()
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    if rigs.is_empty() {
        bail!("world-coverage: the entity model corpus is empty — the detector cannot have run");
    }

    let mut drawn: BTreeMap<String, String> = BTreeMap::new();
    let mut routed_rigs: BTreeSet<String> = BTreeSet::new();
    for id in &ids {
        if rigs.contains(id.as_str()) {
            drawn.insert(id.clone(), "entity model corpus".to_owned());
            routed_rigs.insert(id.clone());
        }
    }

    let mut detectors_ran = 0;
    for claim in ENTITY_RENDERERS {
        let claimed = resolve_claim(index, claim, &population)?;
        detectors_ran += 1;
        for id in claimed {
            drawn.entry(id).or_insert_with(|| claim.name.to_owned());
        }
    }

    // The reverse direction: a rig in the corpus no registry type resolves to.
    // The alias and suffix rules retarget a type onto another rig's name, so
    // read those targets back out of the same two symbols rather than
    // re-deriving them.
    let alias_targets = index.literals_in(
        "crates/lodestone-render/src/entity.rs",
        "canonical_model_name_for_type",
    );
    let boat_targets =
        index.literals_in("crates/lodestone-render/src/entity.rs", "boat_model_name");
    for target in alias_targets.into_iter().chain(boat_targets) {
        if rigs.contains(target.as_str()) {
            routed_rigs.insert(target);
        }
    }
    let unrouted: Vec<(String, &'static str)> = rigs
        .iter()
        .filter(|rig| !routed_rigs.contains(**rig))
        .map(|rig| ((*rig).to_owned(), ""))
        .collect();

    Ok(finish_population(
        "entity types",
        index,
        ENTITY_MENTION_SURFACE,
        &oracle.entity_no_render,
        ids,
        drawn,
        detectors_ran,
        ENTITY_RENDERERS.len(),
        expected,
        unrouted,
    ))
}

fn particle_population(index: &SourceIndex) -> Result<PopulationReport> {
    use lodestone_data::particle_types::{PARTICLE_TYPE_COUNT, particle_type_name};

    let expected = PARTICLE_TYPE_COUNT as usize;
    let mut ids = Vec::with_capacity(expected);
    for id in 0..PARTICLE_TYPE_COUNT {
        let name = particle_type_name(id as i32).with_context(|| {
            format!("world-coverage: particle registry id {id} has no name — table truncated")
        })?;
        ids.push(name.strip_prefix("minecraft:").unwrap_or(name).to_owned());
    }
    let population: BTreeSet<String> = ids.iter().cloned().collect();

    let mut drawn: BTreeMap<String, String> = BTreeMap::new();
    let mut detectors_ran = 0;
    for claim in PARTICLE_RENDERERS {
        let claimed = resolve_claim(index, claim, &population)?;
        detectors_ran += 1;
        for id in claimed {
            drawn.entry(id).or_insert_with(|| claim.name.to_owned());
        }
    }

    Ok(finish_population(
        "particle types",
        index,
        PARTICLE_MENTION_SURFACE,
        &BTreeSet::new(),
        ids,
        drawn,
        detectors_ran,
        PARTICLE_RENDERERS.len(),
        expected,
        Vec::new(),
    ))
}

/// Block entities are the one population with no dispatch table to read.
///
/// Nothing on the render path consults a block entity's `type_id`: the
/// block-entity list from a chunk supplies candidate *positions*, and every
/// gather predicate in `lodestone-shell`'s `block_entities` module then keys
/// on the **block state's own name**. So the census cannot ask "is this type
/// in a table"; it asks the question that path actually answers — *does any
/// predicate in the gather surface match a block that owns this type?* — by
/// inverting the per-state table and testing each owning block name against
/// every literal and suffix rule the gather surface spells.
///
/// The suffix half is load-bearing rather than a nicety: signs are claimed by
/// `ends_with("_sign")` and shelves by `ends_with("_shelf")`, so a scan that
/// collected only whole literals would report both families as unreached.
fn block_entity_population(index: &SourceIndex, oracle: &VanillaOracle) -> Result<PopulationReport> {
    use lodestone_data::block_entity_types::{TYPE_COUNT, block_entity_type, block_entity_type_name};
    use lodestone_data::block_states::{STATE_COUNT, block_name};

    let expected = TYPE_COUNT as usize;
    let mut ids = Vec::with_capacity(expected);
    for id in 0..TYPE_COUNT {
        let name = block_entity_type_name(id).with_context(|| {
            format!("world-coverage: block-entity registry id {id} has no name — table truncated")
        })?;
        ids.push(name.strip_prefix("minecraft:").unwrap_or(name).to_owned());
    }

    // Invert the per-block-state table: which block names own each type.
    let mut blocks_for_type: BTreeMap<u32, BTreeSet<String>> = BTreeMap::new();
    for state in 0..STATE_COUNT {
        let Some(type_id) = block_entity_type(state) else {
            continue;
        };
        let Some(name) = block_name(state) else {
            continue;
        };
        blocks_for_type
            .entry(type_id)
            .or_default()
            .insert(name.strip_prefix("minecraft:").unwrap_or(name).to_owned());
    }
    if blocks_for_type.is_empty() {
        bail!(
            "world-coverage: no block state claims a block-entity type — the per-state table \
             read as empty, so the detector examined nothing"
        );
    }

    // The gather-surface predicate corpus: every literal, plus every affix
    // rule, that a file in the gather surface spells.
    let mut literals: BTreeSet<String> = BTreeSet::new();
    let mut suffixes: BTreeSet<String> = BTreeSet::new();
    for (literal, sites) in &index.literals {
        if sites.iter().any(|s| in_gather_surface(&s.file)) {
            literals.insert(literal.strip_prefix("minecraft:").unwrap_or(literal).to_owned());
        }
    }
    for (affix, sites) in &index.affixes {
        if sites.iter().any(|s| in_gather_surface(&s.file)) {
            suffixes.insert(affix.clone());
        }
    }
    if literals.is_empty() || suffixes.is_empty() {
        bail!(
            "world-coverage: the block-entity gather surface yielded {} literals and {} affix \
             rules. Either count reading zero means the predicate scan did not run.",
            literals.len(),
            suffixes.len()
        );
    }

    let mut drawn: BTreeMap<String, String> = BTreeMap::new();
    for (type_id, id) in ids.iter().enumerate() {
        let Some(blocks) = blocks_for_type.get(&(type_id as u32)) else {
            continue;
        };
        for block in blocks {
            if literals.contains(block) {
                drawn.insert(id.clone(), format!("gather predicate on block {block:?}"));
                break;
            }
            if let Some(suffix) = suffixes
                .iter()
                .find(|suffix| suffix.len() > 1 && block.ends_with(suffix.as_str()))
            {
                drawn.insert(id.clone(), format!("gather suffix rule {suffix:?}"));
                break;
            }
        }
    }

    // Vanilla registers exactly one renderer call per block-entity type, so
    // absence from that list is conclusive here in a way it is not for
    // entities: 23 of these 49 have no renderer in the game at all.
    let no_vanilla_renderer: BTreeSet<String> = match &oracle.block_entity_rendered {
        Some(rendered) => ids
            .iter()
            .filter(|id| !rendered.contains(*id))
            .cloned()
            .collect(),
        None => BTreeSet::new(),
    };

    Ok(finish_population(
        "block-entity types",
        index,
        BLOCK_ENTITY_GATHER_SURFACE,
        &no_vanilla_renderer,
        ids,
        drawn,
        1,
        1,
        expected,
        Vec::new(),
    ))
}

fn in_gather_surface(file: &str) -> bool {
    in_surface(file, BLOCK_ENTITY_GATHER_SURFACE)
}

#[allow(clippy::too_many_arguments)]
fn finish_population(
    name: &'static str,
    index: &SourceIndex,
    mention_surface: &[&str],
    no_vanilla_renderer: &BTreeSet<String>,
    ids: Vec<String>,
    drawn: BTreeMap<String, String>,
    detectors_ran: usize,
    detectors_declared: usize,
    expected: usize,
    unrouted: Vec<(String, &'static str)>,
) -> PopulationReport {
    let subjects = ids
        .into_iter()
        .map(|id| {
            if let Some(via) = drawn.get(&id) {
                return Subject {
                    id,
                    verdict: Verdict::Drawn(via.clone()),
                    mentions: Vec::new(),
                };
            }
            let mentions = mentions_of(index, &id, mention_surface);
            // The vanilla check comes first: a subject nothing draws *there*
            // is not a hole here, however much of our own code names it.
            let verdict = if no_vanilla_renderer.contains(&id) {
                Verdict::NoVanillaRenderer
            } else if mentions.is_empty() {
                Verdict::Absent
            } else {
                Verdict::Stranded
            };
            Subject {
                id,
                verdict,
                mentions,
            }
        })
        .collect::<Vec<_>>();
    PopulationReport {
        name,
        examined: subjects.len(),
        expected,
        detectors_ran,
        detectors_declared,
        subjects,
        unrouted,
    }
}

// ---------------------------------------------------------------------------
// Render-source wiring
// ---------------------------------------------------------------------------

/// A fourth check, orthogonal to the three populations and cheap to run
/// alongside them: the GPU state declares a `*_source` field per per-type
/// pass, and each has to be re-installed through the matching `set_*_source`
/// every frame. A field with no installer is a renderer that can never draw,
/// whatever its own tests say.
fn source_wiring(index: &SourceIndex) -> Result<SourceWiring> {
    let declared: Vec<String> = index
        .fields
        .iter()
        .filter(|(name, sites)| {
            name.ends_with("_source") && sites.iter().any(|s| s.file == "crates/lodestone-shell/src/gpu.rs")
        })
        .map(|(name, _)| name.clone())
        .collect();
    let installed: Vec<String> = index
        .calls
        .keys()
        .filter(|name| name.starts_with("set_") && name.ends_with("_source"))
        .cloned()
        .collect();
    if declared.is_empty() {
        bail!(
            "world-coverage: found no `*_source` field on crates/lodestone-shell/src/gpu.rs. \
             The wiring check cannot have run — the struct moved, or the file left DRAW_SURFACE."
        );
    }
    if installed.is_empty() {
        bail!("world-coverage: found no `set_*_source` call anywhere in the draw surface");
    }
    let never_installed = declared
        .iter()
        .filter(|field| !installed.contains(&format!("set_{field}")))
        .cloned()
        .collect();
    Ok(SourceWiring {
        declared,
        installed,
        never_installed,
    })
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Run the whole census.
pub fn world_coverage_report(root: &Path) -> Result<WorldCoverageReport> {
    let index = build_source_index(root)?;
    let oracle = vanilla_oracle(root)?;
    let populations = vec![
        entity_population(&index, &oracle)?,
        block_entity_population(&index, &oracle)?,
        particle_population(&index)?,
    ];
    for population in &populations {
        if population.examined != population.expected {
            bail!(
                "world-coverage: {} examined {}/{} subjects — a partial scan must not report a \
                 clean bill",
                population.name,
                population.examined,
                population.expected
            );
        }
        if population.detectors_ran != population.detectors_declared {
            bail!(
                "world-coverage: {} ran {}/{} detectors",
                population.name,
                population.detectors_ran,
                population.detectors_declared
            );
        }
    }
    let wiring = source_wiring(&index)?;
    Ok(WorldCoverageReport {
        populations,
        wiring,
        oracle,
        files_scanned: index.files_scanned,
    })
}

/// Render the report.
///
/// Every count is printed as `N/M` with the verdict depending on the pair, so
/// a scan that examined nothing cannot be read as a clean one.
#[must_use]
pub fn format_world_coverage_report(report: &WorldCoverageReport) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "world coverage — registry subjects that reach no draw path\n\
         (scanned {} files across the client draw surface)\n",
        report.files_scanned
    );
    if report.oracle.unavailable.is_empty() {
        let _ = writeln!(
            out,
            "vanilla renderer oracle: READ — a subject vanilla draws nothing for is \
             separated out rather than counted as a hole\n"
        );
    } else {
        let _ = writeln!(
            out,
            "vanilla renderer oracle: UNAVAILABLE — every count below therefore mixes \"we have \
             not built this\" with \"there is nothing to build\", and is an over-count rather \
             than a clean bill:\n  {}\n  Run `cargo xtask fetch-version --version 26.2` and \
             extract the decompile to restore it.\n",
            report.oracle.unavailable.join("\n  ")
        );
    }

    let mut total_examined = 0;
    let mut total_findings = 0;
    for population in &report.populations {
        let drawn = population
            .subjects
            .iter()
            .filter(|s| matches!(s.verdict, Verdict::Drawn(_)))
            .count();
        let stranded: Vec<&Subject> = population
            .subjects
            .iter()
            .filter(|s| s.verdict == Verdict::Stranded)
            .collect();
        let absent: Vec<&Subject> = population
            .subjects
            .iter()
            .filter(|s| s.verdict == Verdict::Absent)
            .collect();
        let no_vanilla = population
            .subjects
            .iter()
            .filter(|s| s.verdict == Verdict::NoVanillaRenderer)
            .count();
        total_examined += population.examined;
        total_findings += stranded.len() + absent.len();

        let _ = writeln!(
            out,
            "## {} — examined {}/{}, detectors {}/{}",
            population.name,
            population.examined,
            population.expected,
            population.detectors_ran,
            population.detectors_declared
        );
        let _ = writeln!(
            out,
            "   drawn            {drawn:>4}\n   stranded         {:>4}   (named in draw code, no \
             geometry — the finding class)\n   absent           {:>4}   (no geometry, nothing \
             names it)\n   no vanilla rig   {no_vanilla:>4}   (vanilla draws nothing for it \
             either — not a finding)",
            stranded.len(),
            absent.len()
        );

        if !stranded.is_empty() {
            let _ = writeln!(out, "\n   STRANDED — code exists, nothing draws it:");
            for subject in &stranded {
                let shown: Vec<String> = subject
                    .mentions
                    .iter()
                    .take(4)
                    .map(ToString::to_string)
                    .collect();
                let more = subject.mentions.len().saturating_sub(shown.len());
                let suffix = if more > 0 {
                    format!(" (+{more} more)")
                } else {
                    String::new()
                };
                let _ = writeln!(out, "     {:<28} {}{suffix}", subject.id, shown.join(", "));
            }
        }
        if !absent.is_empty() {
            let names: Vec<&str> = absent.iter().map(|s| s.id.as_str()).collect();
            let _ = writeln!(
                out,
                "\n   ABSENT — nothing draws it and nothing names it:\n     {}",
                names.join(", ")
            );
        }
        if !population.unrouted.is_empty() {
            let names: Vec<&str> = population
                .unrouted
                .iter()
                .map(|(name, _)| name.as_str())
                .collect();
            let _ = writeln!(
                out,
                "\n   renderers no subject routes to: {}",
                names.join(", ")
            );
        }
        out.push('\n');
    }

    let _ = writeln!(
        out,
        "## render sources — declared {}, installed {}",
        report.wiring.declared.len(),
        report.wiring.installed.len()
    );
    if report.wiring.never_installed.is_empty() {
        let _ = writeln!(
            out,
            "   every declared source has a matching set_* installer\n"
        );
    } else {
        let _ = writeln!(
            out,
            "   NEVER INSTALLED (a renderer that cannot draw): {}\n",
            report.wiring.never_installed.join(", ")
        );
    }

    let _ = writeln!(
        out,
        "subjects examined: {total_examined}/{total_examined}; findings: {total_findings}"
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask/ has a parent")
            .to_path_buf()
    }

    /// The calibration case, and the reason this tool exists.
    ///
    /// An item frame has a hitbox entry, a two-element type-path constant, a
    /// ported pose matrix and its own `special_item_frames_drawn` counter, and
    /// draws **nothing**: `entity_models` deliberately holds no `item_frame`
    /// rig (vanilla resolves it through a block-model JSON, not a
    /// `ModelPart`), and the two passes that key on the type draw the *item*
    /// hanging in the frame rather than the frame. A census that reports it as
    /// covered is measuring the wrong thing, so this test is the control that
    /// says the instrument works at all.
    #[test]
    fn the_census_reports_item_frames_as_stranded() {
        let report = world_coverage_report(&root()).expect("census runs");
        let entities = report
            .populations
            .iter()
            .find(|p| p.name == "entity types")
            .expect("entity population present");
        for id in ["item_frame", "glow_item_frame"] {
            let subject = entities
                .subjects
                .iter()
                .find(|s| s.id == id)
                .unwrap_or_else(|| panic!("{id} is in the registry population"));
            assert_eq!(
                subject.verdict,
                Verdict::Stranded,
                "{id} must land in the stranded bucket — it is named all over the draw surface \
                 and reaches no geometry. Found {:?} instead.",
                subject.verdict
            );
            assert!(
                !subject.mentions.is_empty(),
                "{id} is stranded, so the report must be able to say where the half-built code \
                 lives"
            );
        }
    }

    /// The other half of the control: a type that genuinely does draw must not
    /// be reported, or the instrument is just a list of the registry.
    #[test]
    fn a_ported_mob_and_a_sprite_only_type_both_read_as_drawn() {
        let report = world_coverage_report(&root()).expect("census runs");
        let entities = report
            .populations
            .iter()
            .find(|p| p.name == "entity types")
            .expect("entity population present");
        for id in ["zombie", "experience_orb", "oak_boat", "snowball"] {
            let subject = entities
                .subjects
                .iter()
                .find(|s| s.id == id)
                .unwrap_or_else(|| panic!("{id} is in the registry population"));
            assert!(
                matches!(subject.verdict, Verdict::Drawn(_)),
                "{id} reaches real geometry (rig corpus, sprite pass, suffix rule and thrown-item \
                 table respectively) and must read as drawn; found {:?}",
                subject.verdict
            );
        }
    }

    /// Every population must have examined its whole registry. This is the
    /// count-with-a-verdict-on-the-count rule: a scan that looked at nothing
    /// must not be able to print a clean report.
    #[test]
    fn every_population_examines_its_whole_registry() {
        let report = world_coverage_report(&root()).expect("census runs");
        assert_eq!(report.populations.len(), 3);
        for population in &report.populations {
            assert_eq!(
                population.examined, population.expected,
                "{} examined {}/{}",
                population.name, population.examined, population.expected
            );
            assert!(
                population.examined > 0,
                "{} examined nothing",
                population.name
            );
        }
    }

    /// The block-entity half has no dispatch table to read — it evaluates the
    /// gather layer's own predicates against the blocks that own each type —
    /// so it needs its own control that the evaluation actually resolves
    /// something. `sign` is the interesting one: it is claimed by an
    /// `ends_with("_sign")` suffix rule and by no whole literal at all, so a
    /// scan that collected only complete strings would report every sign in
    /// the game as unreached.
    #[test]
    fn the_block_entity_scan_resolves_both_a_literal_and_a_suffix_rule() {
        let report = world_coverage_report(&root()).expect("census runs");
        let population = report
            .populations
            .iter()
            .find(|p| p.name == "block-entity types")
            .expect("block-entity population present");
        for id in ["chest", "sign", "hanging_sign", "bell"] {
            let subject = population
                .subjects
                .iter()
                .find(|s| s.id == id)
                .unwrap_or_else(|| panic!("{id} is in the registry population"));
            assert!(
                matches!(subject.verdict, Verdict::Drawn(_)),
                "{id} has a live gather predicate and must read as drawn; found {:?}",
                subject.verdict
            );
        }
    }

    /// The oracle is an optional input, and a missing one must be *loud*.
    /// A detector that could not look has to be distinguishable from one that
    /// looked and found nothing, so this pins that the two states differ in
    /// the report rather than only in a comment.
    #[test]
    fn a_missing_vanilla_oracle_is_reported_rather_than_assumed() {
        let empty = tempfile::tempdir().expect("tempdir");
        let oracle = vanilla_oracle(empty.path()).expect("a missing oracle is not an error");
        assert_eq!(
            oracle.unavailable.len(),
            2,
            "both registry files must be reported missing by name"
        );
        assert!(oracle.entity_no_render.is_empty());
        assert!(oracle.block_entity_rendered.is_none());
    }

    /// The Java scan has count floors precisely so that a changed spelling
    /// cannot present as "vanilla draws nothing". This is the control that
    /// the extractor works at all on the real file shape.
    #[test]
    fn the_java_registration_scan_reads_the_registration_shape() {
        let text = "register(EntityTypes.MARKER, NoopRenderer::new);\n                    register(EntityTypes.ZOMBIE, ZombieRenderer::new);\n                    // register(EntityTypes.notAConstant, X);\n";
        let regs = java_registrations(text, "EntityTypes");
        assert_eq!(
            regs,
            vec![
                ("marker".to_owned(), "NoopRenderer".to_owned()),
                ("zombie".to_owned(), "ZombieRenderer".to_owned()),
            ],
            "the scan must read the constant and its renderer, and ignore a non-constant"
        );
    }

    /// A renderer claim whose anchor symbol has gone must fail the run rather
    /// than quietly keep vouching for its subjects — the `SKIPPED`-is-a-false-
    /// negative rule, applied to this tool's own manifest.
    #[test]
    fn a_claim_with_a_missing_anchor_is_a_hard_failure() {
        let index = build_source_index(&root()).expect("index builds");
        let claim = RendererClaim {
            name: "test",
            file: "crates/lodestone-render/src/entity.rs",
            symbol: "a_symbol_that_does_not_exist",
            rule: ClaimRule::Explicit(&["zombie"]),
        };
        let population = BTreeSet::from(["zombie".to_owned()]);
        let err = resolve_claim(&index, &claim, &population)
            .expect_err("a missing anchor must be an error");
        assert!(
            err.to_string().contains("does not define"),
            "the error must name the missing anchor: {err}"
        );
    }

    /// The scan must not count a mention that exists only in a test fixture:
    /// a gate constructing an `EntityDraw` for an unported mob would otherwise
    /// make that mob read as half-built production code.
    #[test]
    fn cfg_test_items_are_not_part_of_the_draw_surface() {
        let file: syn::File = syn::parse_str(
            r#"
            #[cfg(test)]
            mod tests {
                const X: &str = "a_literal_only_a_test_names";
            }
            fn production() { let _ = "a_literal_production_names"; }
            "#,
        )
        .expect("fixture parses");
        let mut index = SourceIndex::default();
        let mut indexer = Indexer {
            file: "fixture.rs".to_owned(),
            stack: Vec::new(),
            index: &mut index,
            in_pattern: 0,
            in_affix: 0,
        };
        indexer.visit_file(&file);
        assert!(
            index.literals.contains_key("a_literal_production_names"),
            "a production literal must be indexed"
        );
        assert!(
            !index.literals.contains_key("a_literal_only_a_test_names"),
            "a literal inside #[cfg(test)] must not count as a draw-surface mention"
        );
    }

    /// A lifetime is what broke three earlier hand-rolled scanners here. The
    /// AST cannot be fooled by one, and this pins that rather than assuming it.
    #[test]
    fn a_lifetime_does_not_derail_the_literal_scan() {
        let file: syn::File = syn::parse_str(
            r#"
            fn f() -> &'static str { let _c = 'a'; "after_the_lifetime" }
            "#,
        )
        .expect("fixture parses");
        let mut index = SourceIndex::default();
        let mut indexer = Indexer {
            file: "fixture.rs".to_owned(),
            stack: Vec::new(),
            index: &mut index,
            in_pattern: 0,
            in_affix: 0,
        };
        indexer.visit_file(&file);
        assert!(index.literals.contains_key("after_the_lifetime"));
    }
}
