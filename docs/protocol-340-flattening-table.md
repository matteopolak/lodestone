# The `id:meta` → block-state table for protocol 340 (1.12.2)

## What it is

Epic #343 (support 1.7.10 → 26.2 via one canonical internal version plus a per-version
translation layer) asks for one pre-Flattening version to be built early, specifically to
force the `id:meta` → block-state mapping problem while the cost of changing course is
still low. `v340` (protocol 340, Minecraft 1.12.2) is that version: pre-Flattening,
already depends only on version-free crates, already live-verified against a real 1.12.2
server.

Below 1.13, a block is a numeric `(old_block_id, meta)` pair — the exact
`(old_block_id << 4) | meta` value `crates/protocol/v340/src/packets/chunk.rs` already
extracts per paletted-chunk-section entry. From 1.13 on it is a namespaced block state,
and this project's entire data layer downstream of `lodestone-world` (the 32,366-state
collision census, hardness, block physics constants, `blocks_motion`, the outline census,
the mesher's palettes) is state-shaped. This is the table that translates the former into
the latter for 1.12.2, **built and verified against the real 1.13.2 server jar's own
`DataFixerUpper` flattening fix** — the same conversion vanilla itself runs to upgrade a
pre-1.13 world — rather than written by hand or trusted blindly from a community dataset.

The table lives in `crates/lodestone-canonical/src/generated/flattening.rs` (generated, do not
hand-edit) with a hand-written public API in `crates/lodestone-canonical/src/flattening.rs`
(`flattening::lookup(old_block_id, meta) -> LegacyBlockState`). Full module docs live in
both files; this doc is the narrative version plus every ambiguous case found along the
way.

**Scope of this work: the table only.** `v340`'s adapter is not rewired to use it, and no
attempt was made to make a 1.12.2 client join and render — see "What wiring `v340` would
need" below for what that follow-up requires.

**Update: the adapter wiring described below has since landed.** See
[`protocol-340-canonical-bridge.md`](./protocol-340-canonical-bridge.md) for what
`crates/lodestone-canonical/src/canonical.rs` does with each of the four outcomes below, the
additional renames/property fixups that direction needed beyond this table's own scope, and the
live-server evidence gathered for it. This document is left as originally written (the table's
own construction and verification); only this note and the "What wiring `v340` would need"
section below were touched.

## Where the mapping actually lives (and where it does not)

**It is not in the 1.12.2 jar, and it is not in `minecraft-data`.** The 1.12.2 jar only
ever needs to encode blocks in its own format; it has no reason to know what 1.13
namespaced states look like. The authoritative conversion lives inside the **1.13.2**
jar's `DataFixerUpper` — Mojang's own world-upgrade machinery, the same code vanilla runs
when it opens a pre-1.13 world.

### Finding it inside an obfuscated jar

Mojang started shipping official (Mojang-mapped) obfuscation maps at 1.14.4 — the same
boundary `docs/version-table.md` established empirically for `version.json`
(`docs/version-table.md` §"The jar-`version.json` boundary is empirically 1.13.2 →
1.14.4"). The 1.13.2 server jar predates that: every game class is obfuscated to a short,
meaningless, **jar-build-specific** name, except `com.mojang.datafixers` itself, which is
a separate open-source library and ships with real names.

Method used (full detail and citations in `oracle-java/FlatteningOracle.java`'s class
doc):

1. Extract every `*.class` from `.cache/mc/1.13.2/server.jar` (SHA-256
   `ffd3aa2c25c5ba68a706b59f2abdc69ac1748e115ca9d3b47941e197736f088e`).
2. Constant-pool strings are never obfuscated, only identifiers are. `grep` the raw class
   bytes for distinctive pre-Flattening block names that can only appear inside the
   flattening table itself — `minecraft:log2`, `minecraft:double_plant` — and intersect
   the hits across all matching classes.
3. Decompile the survivors (CFR 0.152, fetched from Maven Central — see "What was
   downloaded" below) and read the `static` initializer.

This surfaced two classes, both single/double-letter obfuscated names **specific to this
one jar build**:

- **`yp`** (`com/mojang/datafixers/...`-adjacent, obfuscated top-level class) — the table
  itself. A private `static final Dynamic<?>[] b` of length **4095**, indexed by
  `(old_block_id << 4) | meta`, each populated by a call
  `yp.a(n, "<canonical modern state>", "<old-form 1>", "<old-form 2>", ...)` inside one
  giant `static {}` block (CFR: "Opcode count of 30439 triggered aggressive code
  reduction" — a decompiler display warning, not evidence of missing data; the calls
  themselves are intact and were counted mechanically, see "How verified" below).
- **`yw`** ("EntityBlockStateFix") — a smaller, incidental find: a `Map<String, Integer>`
  that is exactly 1.12.2's own numeric block-id registry (`minecraft:mob_spawner → 52`,
  ..., 254 entries, ids 0–255 with gaps). Not the flattening table itself, but the
  authoritative cross-check for "which ids are real 1.12.2 blocks at all" used throughout
  this doc.
- **`aah`** ("ItemInstanceTheFlatteningFix") — a related, smaller table for **ItemStack
  Damage values** (302 `"old_item.damage" → "new_item"` entries, no properties, since
  inventory items have no block-state properties). This is the item-side counterpart to
  the block-side table and was **not built** here (out of scope — the task is the
  block-in-world mapping that feeds the state-shaped censuses); its existence and location
  are recorded for whoever picks up item flattening next.

### How verified: executed live, not just parsed from decompiled source

The static-source parse (regex over CFR's output) was cross-checked against **actually
running vanilla's own code**: `FlatteningOracle.java` reflectively reads the private
`Dynamic<?>[] b` field after class-load (`yp`'s static initializer runs automatically,
no `Bootstrap`/`SharedConstants` call needed — this is not a live-server oracle in the
`v770` sense, just a JVM executing one class) and dumps `dyn.get("Name")`/
`dyn.get("Properties").getMapValues()` (both real, non-obfuscated `com.mojang.datafixers`
API) for every one of the 4095 slots, sorting property keys for determinism.

Result: **zero mismatches** between the static-source parse and the live-executed dump
across all 4095 slots. This is the version committed as
`crates/lodestone-canonical/tests/support/flattening_1_13_2_jvm.txt`.

### What was downloaded

- CFR 0.152 decompiler jar (2.16 MB), from Maven Central
  (`https://repo1.maven.org/maven2/org/benf/cfr/0.152/cfr-0.152.jar`). Used only as a
  local tool during this investigation (kept in the session scratchpad, not committed to
  the repo — it is not needed to regenerate the table, since regeneration only needs a
  JDK and `FlatteningOracle.java` reflecting directly into the already-loaded class).
- No jars were downloaded. `.cache/mc/1.12.2/server.jar` and `.cache/mc/1.13.2/server.jar`
  were both already cached, per the briefing.

## The table's shape: four outcomes, not one

`flattening::lookup(old_block_id, meta) -> LegacyBlockState` returns one of four
variants, on purpose — a table that silently resolves the ambiguous cases below to a
single answer would produce plausible wrong terrain nobody could trace:

| variant | meaning | count (of 4095 valid slots) |
|---|---|---|
| `Resolved` | vanilla's own table names one modern state | 1663 (1594 distinct states) |
| `NoTableEntry` | this exact pair was never assigned a target | 2400 |
| `RequiresAdditionalContext` | identity needs TileEntity/neighbor data `id:meta` cannot supply | 32 |
| `OutOfBounds` | `old_block_id == 255 && meta == 15`, past the end of vanilla's own array | 1 |

`1663 + 2400 + 32 = 4095`; `OutOfBounds` is the 4096th theoretically-possible slot
(`256 * 16`) and is handled outside the table entirely (see below).

## Enumeration of ambiguous cases

### 1. Old `id:meta` with no distinct modern state — 2400 of 4096 slots

Vanilla's own array only ever populates 1695 of its 4095 usable slots (1663 once the 32
`RequiresAdditionalContext` slots are carved out — see the table above). The remaining
2400 fall into two structurally different buckets, both real and both worth telling
apart:

- **209 block ids are partially defined** — a real, registered 1.12.2 block whose used
  metadata range is narrower than 16. Example: `minecraft:stone` (id 1) defines metas
  0–6 (stone/granite/polished_granite/diorite/polished_diorite/andesite/
  polished_andesite) and leaves 7–15 undefined; `minecraft:torch` (id 50) defines metas
  1–5 (the five facings) and leaves meta 0 itself undefined (no vanilla torch is ever
  placed with meta 0). These are metadata values that never occur on wire from an
  unmodified server, but nothing stops a modified or malicious one from sending them.
- **2 block ids are wholly undefined** — ids 253 and 254. Cross-checked against `yw`'s
  own 1.12.2 block-id registry (254 entries): neither 253 nor 254 was ever assigned to any
  real block. These are not "missing data", they are ids that were never blocks at all.

**Live vanilla's own accessor (`yp.b(int)`, the public method) silently substitutes slot 0
(air) for every one of these 2400 slots when called at runtime** — confirmed directly:
calling `yp.b(4090)` (past the end of the defined range) returns `{Name:"minecraft:air"}`
in the executed dump. This project's table refuses to make that substitution: `lookup`
returns `NoTableEntry`, not `Resolved` with `minecraft:air`. A caller that needs an actual
fallback (e.g. to keep rendering something) must choose one explicitly and say so at the
call site — this table will not choose "air" for you.

### 2. Modern states with no old representation — 171 of 593 (per `minecraft-data`'s 1.13 `blocks.json`)

Comparing the set of distinct block names reachable through this table (432 names) against
`vendor/minecraft-data/data/pc/1.13/blocks.json`'s full 1.13 block list (593 names) leaves
171 names with zero legacy representation. Almost all fall into recognizable groups that
make sense once you see them:

- **The whole Update Aquatic set** (1.13's headline feature): kelp, seagrass, coral
  (5 colours × {plant, block, fan, wall_fan, dead-of-each} = the majority of the 171),
  sea pickle, turtle egg, conduit, dried kelp block, bubble column, blue ice, cave/void
  air. None of these existed before 1.13; there is no old id to map from.
- **Wood variants introduced alongside Flattening itself**: stripped logs (10 species ×
  log/wood), `*_wood` (the un-stripped bark-all-sides variant — see the naming note
  below), acacia/birch/dark_oak/jungle/spruce buttons/pressure_plates/trapdoors (only oak
  and stone had these pre-1.13; the other four wood types are new state variants of a
  block family, not a new family).
- **Potted-plant blocks and single-colour wall variants**: `potted_*` (16 of them),
  `*_wall_banner` per colour, `*_wall_skull`/`*_wall_head`. These exist post-Flattening as
  separate block IDs; pre-1.13 they were represented through TileEntity fields on a single
  shared block id (140 for pots, 144 for skulls) — see case 4 below, the same TileEntity
  dependency that makes the *forward* direction ambiguous also explains why there is no
  *old* representation for each individual potted/wall variant.
- A few one-off renames not yet resolved by this table alone: `spawner`, `melon`,
  `nether_portal`, `flower_pot`, `shulker_box` (base, unqualified) — see "Not authoritative
  for" below.

### 3. Old metadata that encoded orientation/state, not identity — 141 pure, 15 mixed (of 254 defined ids)

Classifying every one of the 254 block ids with at least one table entry, by whether its
metadata values keep the same modern block name (pure orientation/state) or switch to a
different name (identity split), or both (the remaining 75 ids have exactly one defined
meta each and so are trivially neither — single-state blocks like `gold_ore`, `obsidian`,
`bookshelf`):

- **141 ids are pure orientation/state**: every defined meta keeps the same modern block
  name and differs only in properties. Stairs (`facing`+`half`), buttons/levers
  (`facing`+`face`+`powered`), rails (`shape`), tripwire (7 boolean connection flags) are
  the largest examples.
- **23 ids are pure identity split**: every defined meta switches to an entirely different
  block name, with no further per-name property variance. Wool, carpet, stained glass
  (and pane), concrete, concrete powder, terracotta, glazed terracotta — the 16-colour
  families.
- **15 ids are mixed** — metadata sometimes switches identity *and* sometimes switches
  properties within one of those identities. This is the case most likely to surprise a
  naive per-block-family assumption:
  - `log`/`log2` (ids 17/162): low 2 bits select species (oak/spruce/birch/jungle for id
    17, acacia/dark_oak for id 162 — note the *split id*, itself an identity choice, not
    just a property), high 2 bits select axis (x/y/z/none-i.e.-all-bark). Meta 12–15 on
    each id are the "bark on every face" variant, which post-Flattening becomes a
    *different block name* (`oak_bark`/... in this table's intermediate spelling — see
    below) rather than an axis property value on the log block.
  - `leaves`/`leaves2` (ids 18/161): species selects identity (4/2 species), while the two
    remaining bits (`decayable`, `check_decay`) are orientation-style boolean properties
    on top.
  - `sapling` (id 6): species selects identity (6 species), `stage` (0/1) is a property.
  - `torch`/`redstone_torch`/`unlit_redstone_torch` (ids 50/75/76): metas 1–4 are
    `wall_torch` (or `redstone_wall_torch`) with a `facing` property (east/west/south/
    north) — pure orientation. Meta 5 switches identity entirely, to the free-standing
    `torch`/`redstone_torch` (no `facing` property at all, since a standing torch has only
    one orientation). So one id genuinely mixes an orientation property (wall-mounted
    facings) with an identity split (wall-mounted vs. free-standing) depending on the meta
    value, rather than doing only one or the other.
  - `stone_slab`/`wooden_slab` (ids 44/126), `anvil` (145), `quartz_block` (155),
    mushroom blocks (99/100), `double_plant` (175): each combines a family-identity split
    (slab material / anvil damage stage / quartz cut / mushroom face-cap pattern / plant
    species) with an orientation-style property (slab `type`, mushroom face booleans,
    plant `half`) on top of it.

  The full 15-id list, machine-derived and reproducible, is in this doc's companion
  analysis; the block names are: `sapling`, `log`, `leaves`, `stone_slab`, `torch`,
  `unlit_redstone_torch`, `redstone_torch`, `brown_mushroom_block`, `red_mushroom_block`,
  `wooden_slab`, `anvil`, `quartz_block`, `leaves2`, `log2`, `double_plant`.

### 4. Old metadata that cannot resolve identity at all without additional data — 32 slots, 3 confirmed families

This is the case CLAUDE.md's briefing specifically asked to be surfaced rather than
silently resolved, and it is real: three families where `id:meta` is **structurally
insufficient**, confirmed directly in vanilla's own table rather than inferred:

- **Flower pots (old id 140, all 16 metas)** — the contained plant is a TileEntity field
  (`Item`/`Data` NBT on the pot's block entity), not part of the block's own metadata at
  all (id 140 only ever had **one** real block-level meta value in 1.12.2). Vanilla's own
  table gives every single one of the 16 slots the *same* placeholder answer,
  `minecraft:potted_cactus` — a real block name, but wrong for 15 of the 16 slots, and a
  dead giveaway that this direction of the table cannot actually answer the question.
  `flattening::lookup` reports `RequiresAdditionalContext` for all 16, not
  `potted_cactus`.
- **Skulls (old id 144, the 12 defined metas)** — type and rotation are likewise
  TileEntity fields (`SkullType`, `Rot`), not block metadata. Vanilla's own table leaves
  its own internal placeholder string, the literal text `"%%FILTER_ME%%"`, in the `Name`
  field for every skull meta — Mojang's own code marking "cannot resolve this from meta
  alone" mechanically detectably, unlike the flower-pot case above. This table detects it
  by string match and reports `RequiresAdditionalContext`, never forwarding the literal
  `%%FILTER_ME%%` to a caller.
- **Double plants, upper half only (old id 175, metas 8–11)** — the *lower* half's meta
  directly encodes species (sunflower/lilac/tall_grass/large_fern/rose_bush/peony, metas
  0–5) and resolves cleanly. The *upper* half's meta only ever encoded a single bit
  (facing, historically) — species was read from the **paired lower-half block** at
  conversion time, never stored in the upper half's own state. Vanilla's own table
  returns a fixed, plausible-looking but wrong species (`peony`) for all four upper-half
  metas, with **no mechanical sentinel** distinguishing it from a real answer — this is
  the one family in the enumeration that could not be auto-detected by matching a marker
  string, and is hardcoded by id/meta-range in `tests/flattening.rs` from this verified
  reading, cited there for exactly that reason. Metas 12–15 for the same id are simply
  undefined (`NoTableEntry`) on top of this.

No other family surfaced a placeholder/sentinel pattern during this pass; there may be
more (beds' "which half needs the neighbour's colour" question below is a candidate that
was not fully chased down), but these three are the ones with direct, checked evidence.

### 5. The array-bound quirk — 1 slot, `old_block_id == 255 && meta == 15`

`yp.b(int)`'s backing array is declared `new Dynamic[4095]`, one short of the naive
`256 * 16 = 4096` id:meta space. `structure_block` (id 255) is the last real block and
only ever uses metas 0–3 (its four `mode` values), so no real block needs index 4094 or
4095 — but index 4095 (`old_block_id=255, meta=15`) is not merely undefined, it is
**structurally unreachable**: `yp.b(int n)`'s own bounds check is `n < b.length` (strict),
so this exact combination cannot even be looked up through vanilla's own accessor without
falling into its fallback branch. This project's `flattening::lookup` reports it as a
distinct `OutOfBounds` variant rather than folding it into `NoTableEntry`, because the
*reason* is different (an off-by-one in vanilla's own array size, not an unassigned
value) and a caller debugging "why is this undefined" should see that.

## Cross-check against `minecraft-data`

Per `CLAUDE.md`'s data-source ordering, `vendor/minecraft-data` is cross-check-grade here,
not authoritative — but 1.12.2 is frozen and will never change, so unlike the 26.2
cross-checks in `docs/version-table.md`, disagreements here are not "which source is
stale" but "which source models a different point in the pipeline". `vendor/minecraft-data
/data/pc/common/legacy.json`'s `blocks` map (1682 entries, `"id:meta" → "name[props]"`) was
compared against this table on identical keys.

- **1674 keys in common. 1537 (91.8%) agree exactly** (name and every property).
- **137 keys (8.2%) disagree.** Every one was inspected; none is a case of "our jar-derived
  table is simply wrong" — see the next section.
- **21 keys only in this table** (`minecraft-data` has no entry) — mostly the upper halves
  of double doors (oak/iron door metas 12–15) and a few mushroom-block/tripwire
  combinations `minecraft-data` omits.
- **8 keys only in `minecraft-data`** (this table has no entry) — `red_bed` "foot" states
  (26:4–7 in `minecraft-data`'s numbering), `quartz_pillar` axis states 155:6/155:10, and
  `rose_bush`/`peony` upper-half entries (175:12–13) that this table's own source array
  simply never assigned (a real gap in vanilla's own table, not something this port lost).
  **Not resolved here** — reported per `CLAUDE.md`'s "report every disagreement... rather
  than picking a winner silently", since establishing definitively who is right requires
  chasing the bed encoding further than this task's time budget allowed (see the
  mixed-id list above; beds were not among the 15 hand-inspected).

### The leaves case: decisive, in this table's favour

The most informative disagreement: `minecraft-data` gives leaves properties
`persistent`/`distance`; this table gives `decayable`/`check_decay`. Resolved with direct
evidence rather than argued from memory: **the literal strings `"persistent"` and
`"distance"` do not appear anywhere in the 1.13.2 server jar's bytes at all** (`grep -c`
across every extracted `.class` file: 0 hits for both), while `"decayable"`/`"check_decay"`
do (2 and 1 hits respectively). Whatever version `minecraft-data`'s `legacy.json` was
generated against, it was not genuine 1.13.2 for this property — `persistent`/`distance`
is a later (1.14+) rename. This table's answer is the one actually present in the pinned
jar.

## Not authoritative for: this is step one of a longer pipeline

This is important enough to restate outside the enumeration above. `yp` is vanilla's own
**first** flattening step (`DataFixerUpper` schema ~V100, applied once when upgrading a
world from below `DataVersion` 1631). It reliably resolves block **identity**, but a
handful of names/property keys it produces are the intermediate 18w-snapshot-era spelling,
later renamed by separate, unrelated fixes chained further down the same
`DataFixerUpper` pipeline (a pipeline this task did not walk in full — that is real
follow-up work, not done here). Confirmed disagreements between this table's output and
1.13.2's own final block registry (cross-checked against `minecraft-data`'s 1.13
`blocks.json`, which lists final names):

| this table gives | final 1.13.2 name |
|---|---|
| `minecraft:mob_spawner` | `minecraft:spawner` |
| `minecraft:melon_block` | `minecraft:melon` |
| `minecraft:portal` | `minecraft:nether_portal` |
| `minecraft:oak_bark` / `spruce_bark` / `birch_bark` / `jungle_bark` / `acacia_bark` / `dark_oak_bark` | `minecraft:oak_wood` / `spruce_wood` / ... |
| `decayable`/`check_decay` (leaves properties) | `persistent`/`distance` |

**Consequence for anyone wiring this table up: `ResolvedState.name` must not be assumed to
already match `lodestone-v770`'s (26.2) naming.** It needs to pass through whatever rename
layer accounts for the table above before being used as a lookup key into the canonical
censuses — see the next section.

## What wiring `v340` would need (status as of this table's original writing)

1. **A rename pass** for the small set of known-stale names/properties above (and any
   others not surfaced by this task's spot-checks — the full rename chain inside
   `DataFixerUpper` was not walked). Small in count, not yet enumerated exhaustively.
   **Done** — `canonical.rs`'s `bridge_name`, see `protocol-340-canonical-bridge.md`. It
   found a few more stale names this table's own spot-checks did not surface
   (`sign`/`wall_sign`/`grass`/`grass_path`), verified by registry existence + shape rather
   than by tracing the later jar (that remains real, unstarted follow-up work).
2. **A decision for `NoTableEntry`** at each of `v340`'s two consumption points
   (`packets/chunk.rs`'s palette decode, and any block-update/interaction packet carrying
   a raw legacy id): whether to reject the packet, substitute a real "unknown block"
   sentinel state, or something else. This table deliberately does not make that choice.
   **Done for `packets/chunk.rs`** — substituted with air, counted and logged per column,
   never silent. Block-update/interaction packets (`v340` currently ignores everything in
   play besides the packets `adapter.rs` explicitly lists) remain unaddressed; nothing in
   this crate today produces a raw legacy id outside chunk decode.
3. **A decision for `RequiresAdditionalContext`** at the same points: flower pots and
   skulls need their TileEntity to be decoded and consulted *alongside* the block
   metadata, not instead of it — this table's job ends at flagging that the block-only
   answer isn't one. Double-plant upper halves need the neighbour block, which
   `packets/chunk.rs`'s per-section decode does not currently expose during palette
   resolution (it would need to defer resolution to a second pass with the full section in
   hand, or resolve lower-before-upper within a column).
   **Partially done**: substituted with air, counted and logged, same as `NoTableEntry` —
   this crate still does not decode block entities at all (see `packets/chunk.rs`'s own
   module docs on why they are consumed and discarded), so there is no TileEntity data to
   resolve these from even now. Decoding block entities is real, unstarted follow-up work.
4. **A decision for `OutOfBounds`** — realistically the same handling as `NoTableEntry`,
   since no real client ever sends `old_block_id=255, meta=15` and the distinction mostly
   matters for debugging, not runtime behaviour. **Done** — same air substitution.
5. **The item-side table** (`aah`, 302 entries) is a separate, smaller piece of work for
   inventory/creative-menu item resolution — not needed for `packets/chunk.rs`, but needed
   before a 1.12.2 client's window/inventory packets can be translated. **Still not done** —
   out of scope for chunk decode; unchanged by this follow-up.

See `protocol-340-canonical-bridge.md` for the full account of what was actually built,
including the additional property fixups (`waterlogged`, leaves' `persistent`/`distance`,
`*_wood`'s `axis`, trapdoor `powered`, note block's full property set, and the cauldron/wall
identity splits) that turned out to be necessary beyond the rename pass alone, and the live
1.12.2 server evidence gathered for it.

## How to change it

- Regenerate the dump (pure JDK, no Docker/live server — `yp`'s static initializer runs on
  class-load): see the command block at the top of
  `crates/lodestone-canonical/tests/support/flattening_1_13_2_jvm.txt` and
  `crates/lodestone-canonical/tests/flattening.rs`'s module docs.
- Regenerate the committed table:
  `LODESTONE_REGEN=1 cargo test -p lodestone-canonical --test flattening committed_table_matches_dump -- --ignored --nocapture`.
- **If the source jar ever changes, `yp`/`yw`/`aah` will almost certainly not be the class
  names any more** — obfuscated names are jar-build-specific. Rediscover them with the
  grep-then-decompile method in `FlatteningOracle.java`'s class doc before touching
  anything else; do not assume the names carry over.
- The generator hard-codes exactly two ambiguous-case detections by id (flower pots,
  double-plant upper halves) alongside one mechanical one (the `%%FILTER_ME%%` sentinel,
  detected by string match, no hardcoding). If a jar update surfaces a new
  TileEntity-dependent family without a sentinel, it will silently show up as a plausible
  but wrong `Resolved` entry rather than `RequiresAdditionalContext` — this is a known gap
  in the *detection*, not the underlying data; watch for it the way beds' foot-half gap
  (see the cross-check section) was watched for and reported rather than resolved.

## Configuration

No environment variables beyond `LODESTONE_REGEN=1` for the drift-guard regeneration
test. No CLI flags — this table's generator lives in the crate's own `tests/flattening.rs`
(the `hardness.rs`/`collision_shapes.rs` pattern), not in `xtask`, since — like those two —
it needs no cross-version orchestration, just one dump file and one generator.

## Dependencies

- `.cache/mc/1.13.2/server.jar` (gitignored; SHA-256
  `ffd3aa2c25c5ba68a706b59f2abdc69ac1748e115ca9d3b47941e197736f088e`), read-only, for
  regeneration only — not needed to build or test the committed table.
- A JDK on `PATH` for regeneration only (this session used Homebrew's `openjdk@26` at
  `/opt/homebrew/opt/openjdk/bin/java`; no minimum version requirement identified — the
  reflective read only touches `com.mojang.datafixers` public API and one private array).
- `vendor/minecraft-data/data/pc/common/legacy.json` and
  `vendor/minecraft-data/data/pc/1.13/blocks.json`, cross-check only, per `CLAUDE.md`.
