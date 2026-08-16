# Island detection (`cargo xtask islands`)

## What it is

A `syn`-based static scanner, `cargo xtask islands` (`xtask/src/islands.rs`),
that reports four things per workspace crate: functions/methods with zero
production call sites, struct fields with zero production readers, struct
fields whose every production assignment is a default-like value, and
`#[allow(dead_code)]` sites. It exists because `cargo xtask connectedness`
answers one narrow question — "is this clientbound packet reaching
anything" — and is structurally blind to everything else: Rust call graphs,
a field nothing reads, a function that is tested but never called from
production. `islands` is the general-purpose instrument; `connectedness`
stays the packet-specific one.

Run it with `just xtask islands` (or `cargo xtask islands`, or `just xtask
islands -- --crate lodestone-v770` to scope to one crate).

## How it works

For every workspace member reported by `cargo metadata --no-deps` (so
`crates/plugins/lodestone-chat-responder-wasm`, which is `exclude`d from the
workspace, is correctly absent — not silently skipped), every `.rs` file
under that crate's directory is parsed with `syn::parse_file` and walked with
a custom `syn::visit::Visit` implementation. **No hand-rolled lexer**: three
earlier scanners in this repo hand-rolled one and each was wrong about
lifetimes (`&'static str` opened a "char literal" flag that never closed and
silently disabled comment detection). Parsing every file as real syntax makes
that whole class of bug impossible.

**Resolution is by bare name, not by type.** A function call is matched to a
definition by its last path segment or method name only — there is no type
checker here. This is a deliberate trade-off:

- Very few false positives: a name genuinely never written anywhere in the
  workspace as a reference is a strong signal.
- Real false negatives on common names: two unrelated items sharing a name
  (`new`, `tick`, `protocol_version`) hide each other. Measured directly: a
  trait method named `protocol_version` collided with an unrelated
  `ServerListPing` struct field of the same name in `lodestone-net`, an
  unrelated local variable in `xtask` itself, and a field in
  `lodestone-assets` — all of which "cover" for the trait method regardless
  of whether anything actually calls it. A distinctively-named island
  (`tick_thunder_for_chunk`, `RecipeToastQueue::push`) is exactly what this
  method is good at; a genuinely-common name is invisible to it.

**"Production" vs "test" is realm-tracked, not just "inside `#[cfg(test)]`"
textually**, because that check alone misses two real shapes:

1. A Cargo target under `tests/`, `benches/`, or `examples/` never carries
   `#[cfg(test)]` itself — that gate is implicit in the target kind. Every
   file under one of those directories is Test realm by path alone.
2. `#[cfg(test)] mod tests;` (an *external* module, body in a sibling file)
   puts the attribute on the *declaration*, not inside the file it names.
   Parsed alone, `src/tests.rs` looks like ordinary production code. The
   scanner runs a first pass over every already-parsed file in a crate to
   collect top-level `mod NAME;` declarations carrying `#[cfg(test)]`, then
   folds that into the realm of any file whose name matches `NAME` (or whose
   parent directory matches `NAME`, for `NAME/mod.rs`). This is what let
   `Family::protocol_version`'s only non-test caller — three call sites
   inside `crates/lodestone-model/src/tests.rs` — register correctly as test
   calls instead of production ones.

A function/method definition is a candidate for the dead-function report
unless it is excluded first:

- **A trait impl of a well-known trait** (`Debug`, `Display`, `Clone`,
  `Encode`, `Decode`, `Serialize`, `Iterator`, ... see
  `WELL_KNOWN_TRAITS`) — reached by the compiler or by `dyn` dispatch, never
  by a textual call to the method name.
- **`#[no_mangle]` / an explicit ABI**, or `fn main` — an entry point, not
  something anything else is expected to call.

References are collected from more shapes than a literal `f(...)` call,
because each of the following was a measured false positive during
development, not a hypothetical one:

| shape | example | fixed by |
|---|---|---|
| function passed as a value | `values.map(mapper)`, `.map_err(dec_err)` | `visit_expr_path` counts every bare path reference, not only `ExprCall`'s own `func` |
| function named in an attribute string | `#[mc(decode_with = "decode_heightmaps")]` | `visit_attribute` scans `Meta::List`/`Meta::NameValue` string literals for a plausible identifier |
| call inside a macro whose body is not a flat expression list | `tokio::select! { v = fut => { travel_through_portal(v); } }` | `visit_macro` first tries `Punctuated<Expr, Comma>` (gets full-AST treatment: nested reads, struct literals, ...) and *unconditionally* also runs a grammar-agnostic lexical scan (`scan_call_like_tokens`) for `ident (` / `.ident (`, recursing into every nested token group regardless of delimiter |

The last one mattered the most in practice: `crates/lodestone-server/src/
server.rs`'s `serve_play` calls `travel_through_portal` and
`travel_through_end_portal` only from inside a `tokio::select!` loop, and
`select!`'s arm grammar (`PATTERN = EXPR => BODY`) is not a flat expression
list, so the strict parse failed for the entire block. Fixing this dropped
`lodestone-server`'s dead-function count from 42 to 27 in one pass.

Field reads are tracked the same way — by bare field name — with one
targeted exclusion: a struct carrying `#[derive(Encode)]`, `#[derive(Decode)]`,
`#[derive(Serialize)]`, or `#[derive(Deserialize)]` has every field read or
written by macro-generated code this scanner never expands and therefore
never sees. Essentially every packet under `crates/protocol/*/src/packets/`
derives one of these, so without the exclusion the report was mostly noise:
`lodestone-v770` alone went from 49 falsely-dead fields to 2 real ones the
moment this landed.

A field read also happens through a **struct pattern**, not only a `.field`
expression: `let PathParams { max_path_length, .. } = params;` (equally, a
`match`/`if let` arm or a destructured function parameter) reads
`max_path_length` out of `params` with no `Expr::Field` anywhere in sight.
`visit_pat_struct` records a read for every named `FieldPat` it walks —
`PathFinder::find_path` (`crates/lodestone-entity/src/pathfinding/
search.rs`) destructures all three of `PathParams`'s fields this way and
would otherwise report all three as having zero production readers.

"Default-only" field detection has two mutation shapes that produce no
`Expr::Assign`/`Expr::Binary` at all, so a field genuinely populated only
through one of them looks unpopulated:

- **Growing a collection through a mutating method.** `self.field.push(x)`,
  `.insert(k, v)`, `.extend(iter)`, and `.entry(k)` (whose *receiver* is the
  field, even though the mutation itself happens on the `Entry` the call
  returns) are all real, non-default mutations. `visit_expr_method_call`
  records one whenever the method name matches and the receiver is a direct
  `Expr::Field` — see `COLLECTION_GROWTH_METHODS`. `Brain::sensors` and five
  sibling fields (`crates/lodestone-entity/src/brain/mod.rs`) are grown
  exclusively this way in `add_sensor`/`add_activity`/`add_activity_any_of`.
- **Taking a mutable reference to a field and handing it to a function.**
  `step_vertical(..., &mut self.fall_speed)` mutates `fall_speed` through the
  pointer with nothing at the call site naming it as a write target, and
  `std::mem::take(&mut self.field)`/`replace`/`swap` are the same shape.
  `visit_expr_reference` records a non-default mutation for any `&mut
  self.field` it sees, regardless of what it is passed to — deliberately
  conservative, since it cannot know whether the callee actually writes
  through the pointer. `NavigatingMob::fall_speed` and four sibling `Vec`
  fields drained via `std::mem::take` (`crates/lodestone-entity/src/ai/
  navigating_mob.rs`) are exactly this shape.

"Default-only" field detection tracks every production assignment site (a
struct-literal field, a `..Default::default()`/`..T::default()` rest fill,
or a plain `place.field = value`) and asks whether *any* of them assigns
something other than a default-like value (`0`, `false`, `""`, `None`,
`Vec::new()`-shaped empties, or an explicit `T::default()`/`Default::default()`
call). Two refinements exist because the naive version was wrong in
opposite directions:

- **A compound assignment (`+=`, `-=`, ...) is not a constant assignment.**
  `syn` folds these into `Expr::Binary` rather than a distinct node.
  `MovementSendState::position_reminder` (`crates/protocol/v770/src/
  adapter/mod.rs`) is reset to `0` at construction and every 20-tick send,
  but *counted up* with `position_reminder += 1` the rest of the time —
  before `visit_expr_binary` recorded that as a non-default mutation, the
  field's only visible assignment was the `= 0` reset, reading as "100%
  default".
- **An arbitrary type's `::new()` is not assumed to be a default**, only a
  fixed allowlist of collection-shaped types is (`Vec`, `HashMap`, `String`,
  ...). `ChunkBatchSizeCalculator::new()` (same file) is a
  fully-initialized, meaningful value; treating every zero-arg `::new()` as
  "the default" — the same mistake CLAUDE.md's own `creeper_swelling`
  writeup warns about ("every assignment is the same constant" reads a
  *healthy* ratio as broken) — flagged it as default-only from a single,
  perfectly normal constructor call.

## How to change it

- All logic lives in `xtask/src/islands.rs`. The CLI plumbing (`CliCommand::
  Islands`, `parse_islands_args`, the `run_cli_command` arm, and the
  `root_help()` text) is in `xtask/src/lib.rs`, next to every other
  subcommand's — `islands.rs` reaches `cargo_metadata` via `crate::
  cargo_metadata` (private, but visible to a child module by Rust's default
  visibility rules) rather than duplicating the `cargo metadata` invocation.
- To add a new false-positive exclusion, follow the existing shape: extend
  `WELL_KNOWN_TRAITS` for a trait-impl-method exclusion, or
  `FIELD_OPAQUE_READER_DERIVES` for a derive that reads every field. Always
  pair a new exclusion with a test that plants the shape and asserts it is
  *not* flagged — see `encode_decode_derived_struct_fields_are_excluded_
  not_flagged_dead` for the template.
- To add a new reference shape (another way code can "use" something that
  isn't a plain call), add a `visit_*` override and — this is the part that
  is easy to skip and shouldn't be — add a test that reproduces the exact
  false positive it fixes, named after the false positive, not after the
  mechanism. Every fix in the table above has one; that is how the last one
  (`select!`) was caught by its own test suite rather than by a human
  re-reading the whole report by hand.
- **Gotcha: a skip must never look like a clean scan.** If a workspace
  member yields zero `.rs` files, or more than 5% of files fail to parse,
  `islands_report` returns an error rather than a shorter report — mirroring
  the `connectedness` incident where a module-layout change made v770 report
  `SKIPPED` and the tool still exited 0. Keep that failure mode if you touch
  the walking/parsing code.
- **Gotcha: rerun the planted-island control after any change to the
  resolution logic**, not just the unit tests. `xtask/src/islands.rs`'s own
  `#[cfg(test)] mod tests` carries the durable version
  (`planted_island_is_found_and_a_used_one_is_not`, plus one test per
  false-positive fix above); a live, one-off version against the real tree
  looks like:

  ```bash
  # append a deliberately dead pub fn to a file you own, e.g.
  # crates/protocol/v770/src/adapter/mod.rs, then:
  cargo run -p xtask -- islands --crate lodestone-v770 | grep planted_name
  # confirm it is named under "dead functions", then remove the fn and
  # confirm the line disappears on a second run.
  ```

## Configuration

- `--crate <name>`: scope the report to one workspace crate. Omit to scan
  every crate `cargo metadata` reports.
- No environment variables or config files. The tool always scans the whole
  workspace under `std::env::current_dir()`; run it from the repo root.

## Dependencies

- `syn` (workspace dependency, with `xtask`'s own `Cargo.toml` adding the
  `visit` feature on top of the `full`/`extra-traits` the workspace default
  already turns on for `lodestone-macros`'s proc-macro use — `islands` is
  the only crate that walks an AST with `syn::visit::Visit` rather than just
  deriving from one).
- `proc-macro2`, for the raw token-stream scanning `visit_macro`'s
  grammar-agnostic fallback and the attribute-string-literal scan need.
- `cargo metadata` (invoked as a subprocess, same as every other `xtask`
  workspace-shape command) for the authoritative list of workspace members
  — never a hand-rolled directory walk, which is exactly the thing that
  would silently include or exclude a crate the workspace manifest itself
  disagrees about.

## Known blind spots

These are not hypothetical — every one below either produced a measured
false positive/negative during this tool's own development, or is a
structural consequence of the name-based design documented above:

- **No type resolution at all.** Two unrelated items sharing a bare name
  cover for each other. This is the dominant source of false negatives and
  is not fixable without adding a real type checker (out of scope: this
  tool is meant to be fast and dependency-light, not `rustc`).
- **`dyn` dispatch, closures stored and called later through a variable, and
  macro-generated call sites this scanner cannot lex** (a custom
  `macro_rules!` whose body does not contain the literal tokens `ident (`
  anywhere, e.g. one that builds a call from separately-interpolated
  fragments) are invisible.
- **Tuple-struct/tuple-variant fields are not tracked at all** — only named
  (`struct Foo { field: T }`) fields are. `struct Foo(T)`'s `.0` is outside
  this scanner's model.
- **The lexical macro-body fallback (`scan_call_like_tokens`) cannot tell a
  real call from a call-shaped pattern**, e.g. `Some(x)` in a `match` arm
  nested inside a macro. This only ever causes something to look *more*
  used than it is — consistent with the tool's overall bias toward fewer
  false positives at the cost of some false negatives.
- **An example under `examples/` or a bench under `benches/` is Test realm**,
  same as `tests/`, on the theory that neither ships in the built binary.
  The consequence: if an example is the *only* real caller of a public
  function, that function will misreport as dead. No case of this was found
  in this workspace, but the shape is worth knowing before trusting a
  finding in a crate with example binaries.
- **A default-only finding is a heuristic about literal syntax, not runtime
  behaviour.** A field assigned a non-default-*looking* expression that
  evaluates to the default at runtime (e.g. `field: some_config_value()`
  where the config happens to always return `0`) will not be flagged.
- **A collection field grown only via `.push`/`.insert`/`.extend`/`.entry`,
  or mutated only by passing `&mut self.field` to a function (including
  `std::mem::take`/`replace`/`swap`), is now handled** — see the two
  mutation shapes in "How it works" above — **but only when the receiver or
  the reference target is a *direct* `Expr::Field`.** A field reached through
  an intermediate binding (`let s = &mut self.field; s.push(x);`) or a
  chained accessor (`self.get_field_mut().push(x)`) is still invisible to
  both fixes, for the same bare-syntax reason a call through a stored
  closure is invisible to the call-count tracker. No case of this narrower
  shape has been found in this workspace yet, but treat a default-only or
  zero-reader finding on a field with a `_mut` accessor as worth a second
  look before trusting it.
- **Not every collection-typed default-only finding is a false positive —
  verify the growth site actually exists before assuming this class covers
  it.** `MobShape::malus_overrides` (`crates/lodestone-entity/src/
  pathfinding/world.rs`) looks identical to the `Brain` fields above (a
  `HashMap` initialised via `HashMap::new()`, read via `.get(&kind)`) but
  has **no** `.insert`/`.entry` call anywhere in the workspace — it is
  read but never populated, so it is correctly, genuinely default-only.
  Confirmed by grep, not assumed from the shape.
- **Findings in a crate this scanner cannot edit (anything outside
  `crates/xtask`/`xtask`, `crates/protocol/*`, `docs/`) still need a human or
  a differently-scoped agent to close them.** The tool reports; it does not
  know which crate boundary is whose to fix.
