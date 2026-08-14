# Registry types: generated enums instead of strings

## What it is

The representation this codebase uses to name a registry entry — a block, an
item, an entity type — in memory and in generated tables. The answer is a
**generated enum whose discriminant is the registry id**, with the plugin case
kept in a separate wrapper type so it costs the built-in path nothing.
`lodestone_data::block::Block` is the first and, so far, only registry converted;
this document records the decision, what was rejected, what was measured, and the
order the remaining registries should follow.

## How it works

### `Block`: the shape

`crates/lodestone-data/src/generated/block_enum.rs` is generated from the same
committed headless-26.2-server dump that produces the block registry table
(`tests/support/tool_jvm.txt`, `BuiltInRegistries.BLOCK` in registration order).
It contains one `#[repr(u16)]` enum with 1,196 variants and explicit
discriminants, plus three index tables. `crates/lodestone-data/src/block.rs`
holds the hand-written accessors.

Three properties carry the design, in the order they mattered:

1. **The built-in path is a bare discriminant.** `block as u16` *is* the registry
   id a `Holder<Block>` carries on the wire — no lookup, no branch. Every
   per-block census stays a plain array indexed by it. This is what makes
   `[Option<Block>; 1537]` a legal replacement for `[Option<&str>; 1537]`.
2. **`Block` has no `Custom` variant and is not `#[non_exhaustive]`.** A match
   over it is exhaustive, so a version bump that adds a block fails the compile
   of every incomplete match rather than falling into a wildcard. A terminal
   `_ =>` arm is this repo's named island factory; the type is deliberately
   built so that one is never needed.
3. **The plugin case lives one level out, in `BlockRef`.** A `BlockRef` is a
   single `u32`: below `Block::COUNT` it is a built-in registry id, at or above
   it is `Block::COUNT + custom index`. `BlockRef::kind` is one comparison, paid
   only where a plugin block can actually appear. The custom side carries **no
   storage in this crate** — `CustomBlockId` is an opaque handle into a registry
   the host owns — so an application with no plugins links zero bytes of
   interner.

### Block versus block state

Two id spaces, and conflating them is the mistake that surfaces late.

| | `Block` | `StateId` |
|---|---|---|
| cardinality (26.2) | 1,196 | 32,366 |
| representation | `#[repr(u16)]` enum | validated newtype over `u32` |
| order | registration | name-sorted (from `blocks.json`) |
| wire use | `Holder<Block>` — `block_event`, tool rules | chunk palette, `block_update` |
| can you `match` it? | yes, exhaustively | no, and nothing wants to |

The orders are unrelated permutations: `minecraft:air` is registry id 0 and
`minecraft:stone` is 1, while the state table's block column is alphabetical, so
`air` sits at index 19 and `stone` at 975. Going between them
(`StateId::block`, `Block::default_state`) always goes through the generated
join, never by assuming the indexes coincide — a wrong assumption that has
already shipped once here, silently resolving every id to an unrelated block.

`StateId` is a newtype rather than an enum because 32,366 hand-named variants
buys nothing: states are the cross product of each block's property domains and
no code ever matches on one. Its job is different — it makes the *range*
invariant true by construction, so `StateId::new` is the single fallible step
and `block()`, `properties()` and `is_default()` afterwards return values rather
than `Option`s. That is the general pattern this work is arguing for: move the
`Option` from every call site to one construction site.

### What was rejected

| alternative | why not |
|---|---|
| `enum Block { …, Custom(InternedId) }` | taxes the 99% case: every match needs a wildcard or a `Custom` arm; `Block` stops being a `u16`, so per-block censuses can no longer be arrays indexed by it; and `block as u16 == registry id` — the property everything rests on — no longer holds. |
| `#[non_exhaustive] enum Block` | forces a wildcard in every downstream crate, which is the same defect as the `Custom` arm with none of the functionality. |
| a bare `BlockId(u16)` newtype, no enum | not matchable, so the compiler checks nothing. This is precisely the class of defect a bare integer index causes: a villager-metadata index was recently settled at 19 where a fixture had guessed 17, harmless for decode (which matches by serializer) and wrong for encode. |
| an interned `Symbol`/`&'static str` only | cheap to migrate, but yields no exhaustiveness, no compile-time constants, no array indexing, and does not remove the strings. |
| `phf` or another perfect-hash crate for name lookup | a dependency and a build step to replace an 11-comparison binary search over a permutation that costs 2,392 B. |
| a 1,196-arm `match` for `from_registry_id` | the crate forbids `unsafe`, so the transmute is unavailable; a `[Block; 1196]` table is 2,392 B and one bounds check. Measured: adding a full 1,196-arm match to the probe crate cost +4,817 B raw and +0.24 s. |
| `Custom` resolved through an interner inside `lodestone-data` | makes the plugin case cost the built-in case: global mutable state, and rodata in every build including ones with no plugins. |

## Measurements

Both went partly against expectation. **The type-safety case for this work is
strong; the binary-size case is not, and on the metric the wasm ceiling actually
enforces it is negative.**

### Binary size

Method: four throwaway `cdylib` crates built for `wasm32-unknown-unknown` at
`opt-level="z"`, `lto="fat"`, `codegen-units=1`, `strip`, carrying the *real*
26.2 tables and differing only in the column representation. A `null` arm — a
byte-identical copy of the baseline — is the instrument's own control and
measured a 0 B delta, so the figures below are signal.

| arm | raw B | gzip B |
|---|---|---|
| `str` — `BLOCK_REGISTRY_NAMES` + `BLOCK_FOR_ITEM: [Option<&str>; 1537]` | 52,811 | 11,164 |
| `null` — byte-identical to `str` | 52,811 | 11,164 |
| `enum_col` — the same column as `[Option<Block>; 1537]` | 43,569 | 11,961 |
| `enum_full` — `enum_col` plus the three index tables | 53,200 | 18,707 |

* Migrating one 1,537-row column saves **9,242 B raw** (1,537 × 6 B, exactly the
  arithmetic: `Option<&str>` is 8 B on wasm32, `Option<Block>` is 2 B thanks to
  the enum's 64,340 unused discriminants serving as the `Option` niche) and
  **costs 797 B gzip**. Integer ids are less compressible than the near-monotonic
  pointer table they replace.
* The representation's own index tables (`BLOCKS_BY_REGISTRY_ID`,
  `REGISTRY_IDS_BY_NAME`, `DEFAULT_STATE` — 9,568 B) cost **+9,631 B raw and
  +6,746 B gzip**, which cancels one column's raw saving outright. They are a
  one-off, so the raw ledger turns positive at roughly 1,600 migrated rows; the
  gzip ledger never does.
* Whole-crate ceiling on the prize: 64,756 `&str` slots across all 19 generated
  string columns, costing 518,048 B of `(ptr, len)` pairs plus 127,474 B of
  unique string payload (4,682 distinct strings). If *every* slot became a `u16`
  — impossible, since the canonical name table has to exist — that is **388,536 B
  raw**, against a bundle that is 5,013,669 B gzip and 3.13× over its ceiling.

  So this work cannot fix the wasm ceiling, and should not be justified on those
  grounds. Worth recording where the string bytes actually are: **84% of all
  `&str` slots are `block_states::PROPERTY_SETS`** (27,317 rows × 2), which is
  property keys and values, not registry names. Interning those to `(u16, u16)`
  is worth ~327,804 B raw on its own and is a different piece of work from this
  one.

### Compile time

Method: the same three source bodies in throwaway crates, `cargo build --release
--target wasm32-unknown-unknown` from a removed `target/`, three repetitions
**interleaved** (`str`, `enum_full`, `enum_match`, then again) rather than arm by
arm, because a duration taken on this machine while other agents build gets
attributed to the wrong cause. Every reading is reported, not a mean.

| arm | rep 1 | rep 2 | rep 3 | median |
|---|---|---|---|---|
| `str` (no enum) | 1.98 s | 1.70 s | 1.17 s | 1.70 s |
| `enum_full` (1,196 variants + tables) | 3.58 s | 2.36 s | 2.22 s | 2.36 s |
| `enum_match` (+ a 1,196-arm exhaustive match) | 3.37 s | 2.60 s | 2.41 s | 2.60 s |

The absolute numbers drift downward across reps (page cache), which is why the
within-rep deltas matter more than the medians: the enum costs **+0.66 to
+1.60 s** on a from-clean build of a crate that otherwise takes under two
seconds. The specific worry — that a very large exhaustive match would be the
expensive part — **is not borne out**: adding all 1,196 arms cost only +0.24 s
over the enum alone. The cost is in declaring the enum and its tables, not in
matching on it, and it is paid once per crate rebuild.

## How to change it

* **The enum and its tables are generated. Do not hand-edit
  `src/generated/block_enum.rs`.** It comes from `generate_block_enum` in
  `crates/lodestone-data/tests/tools.rs`; regenerate with
  `LODESTONE_REGEN=1 cargo test -p lodestone-data --test tools
  committed_tables_match_dump -- --ignored`. `committed_tables_match_dump`
  fails when the committed file drifts from the dump.
* **The generator refuses to guess.** It asserts every registry path is
  `minecraft:`-namespaced, is inside `[a-z0-9_]`, does not start with a digit,
  does not camel-case to `Self`, and — the one that matters — that no two paths
  camel-case to the same variant. A collision would otherwise tempt a future
  generator into silently aliasing two blocks. Measured against 26.2: 1,196
  paths, 1,196 distinct variants, zero exceptions. The same holds for the two
  registries not yet converted: 1,537 items and 158 entity types, all clean.
* **Adding a generated string column now fails a test.**
  `tests/generated_string_columns.rs` scans `src/generated/*.rs` for `pub static`
  arrays whose element type mentions `&str` and requires each to be classified in
  its `ALLOWED` table as `CanonicalNames`, `OpenStringSpace`, `CrossReference` or
  `DuplicateNames`. It diffs both directions, so a stale row fails as loudly as a
  new column, and it fails if it scans nothing — "could not look" must never
  share a verdict with "found nothing". Its three failure modes were each
  demonstrated by planting a violation and observing the failure name the file.
  The four rows still classified as debt are the migration queue.
* **Where the fallible constructor still is.** `lodestone_model::Identifier` is
  two owned `String`s with a fallible constructor, which is why call sites read
  `Identifier::new("minecraft", "armor").expect("valid built-in identifier")`. A
  `Block` needs none of that — `Block::Stone.name()` is a `&'static str` from
  rodata — so every consumer migrated to a registry enum removes an `.expect()`
  rather than merely relocating it.

## Migration plan for the rest

Ordering follows *type → generator → conventions → migration*, with the call-site
sweep done opportunistically as files are touched for other reasons. A mass sweep
is maximally destructive with many agents writing concurrently and nearly free
once the types and generators are right, so it is explicitly **not** scheduled.

**Stage 1 — the two remaining large registries (can run concurrently with each
other).** `Item` (1,537 variants) and `EntityType` (158). Both are the same
shape as `Block` and both were verified derivable: all `minecraft:`, all inside
`[a-z0-9_]`, no digit-leading paths, no camel-case collisions. Each is
additive — a new generated module plus accessors — so neither breaks a held
crate. `Item` should land first: it unblocks `tools::ITEM_TOOLS`, which is keyed
by item name today.

**Stage 2 — the four debt columns, one per registry, each independent.** In
size order: `block_states::BLOCK_NAMES` (a second, name-sorted copy of the block
names — should become a `[u16; 1196]` permutation over `BLOCK_REGISTRY_NAMES`),
`block_blast::BY_NAME` (same shape), `sound_events::SOUND_EVENT_ENTRIES` (the
name half duplicates `SOUND_EVENT_NAMES`), `tools::ITEM_TOOLS` (blocked on
`Item`). Each is a generator edit plus its own drift guard, and each removes one
row from the guard's `ALLOWED` table — the count assertion there is meant to
reach zero.

**Stage 3 — `StateId` adoption.** The newtype exists and is used inside
`lodestone-data`. Threading it outward changes signatures in held crates
(`lodestone-server`, `lodestone-shell`, `lodestone-render`, `crates/protocol/v770`)
and so must be brokered rather than swept. Do it one crate at a time, and keep
the raw-`u32` free functions as the un-migrated entry points while it proceeds —
that is what made the `block_items` migration land without touching a held file.

**Stage 4 — `Identifier` interning.** The largest remaining win and the one with
the widest blast radius, because every crate names the type. It is what makes an
*infallible const* constructor possible at all: a `String`-backed identifier
cannot be built in a `const fn`, so the `.expect("valid built-in identifier")`
at call sites is a symptom of the representation, not of the call sites. Needs
its own allocation measurement before it lands.

**Stage 5 — property-string interning.** Not a registry question at all, but it
is where 84% of the crate's string slots are and the only string change with a
binary-size case behind it.

**Not scheduled: a call-site sweep of the ~8,600 hand-written `"minecraft:`
literals.** Most of them evaporate as the types above reach their consumers.

## Configuration

* `LODESTONE_REGEN=1` — regenerates the committed tables instead of asserting
  against them. Applies to `tests/tools.rs` (block registry, block enum, tools)
  and `tests/block_items.rs`.
* No runtime configuration. Every table is `&'static` rodata.

## Dependencies

* `crates/lodestone-data/tests/support/tool_jvm.txt` — the committed headless
  26.2 server dump that is the external anchor for the registry order and the
  block names. Produced by `crates/lodestone-data/oracle-java/ToolOracle.java`;
  see [`oracle-runtimes.md`](./oracle-runtimes.md) for how the JVM oracles run.
* The committed default-state census (`snow_support`), joined in to give
  `Block::default_state` the server's own answer rather than an inferred one.
* `lodestone_model::Identifier` — still the canonical namespaced type for
  anything that is not one of the converted registries.
* [`lodestone-data-crate.md`](./lodestone-data-crate.md) for the crate as a
  whole, and [`canonical-block-states.md`](./canonical-block-states.md) for the
  block-state table these types sit on top of.
