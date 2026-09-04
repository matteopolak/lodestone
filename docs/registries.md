# Registries: synchronized data, canonical block states, and generated tables

## What it is

How Minecraft's data-driven registries reach this client and this server: the
Configuration-phase `registry_data` wire packet and what we keep from it, the
`lodestone-canonical` bridge that maps a legacy protocol family's own block-id space onto
the canonical 26.2 block-state space, the generated-enum representation used for registry
types (`Block`, `Item`, `EntityType`), and the `lodestone-data` crate that owns the ~20
generated game-data censuses (block states, hardness, collision shapes, item prototypes, and
the rest) neither a protocol family nor the game logic should hand-roll.

## How it works

### Registry data ingest (`registry_data`)

During Configuration the server sends one `registry_data` packet per **synchronized
registry** — 29 of them. **The authoritative list is the jar's own
`RegistryDataLoader.SYNCHRONIZED_REGISTRIES`, not `generated/reports/registries.json`**,
which omits `minecraft:dimension_type` and `minecraft:world_clock` entirely because both are
data-pack-loaded registries the report does not enumerate; following the report literally
builds a set missing the registry the client needs most. Each packet is a registry
identifier plus a list of `(entry id, optional NBT)` pairs, and **entry order is the
holder-id space** — `login`/`respawn` and `set_time` reference entries by a bare VarInt
index into that order, so a registry whose entries arrive out of the order you expect (they
are typically alphabetical by resource location, not by any bootstrap class's registration
order — measured directly against several dynamic registries elsewhere in this codebase, and
worth checking again for any new one) silently mis-resolves every later reference. Never
assume a holder id without reading the actual entry order.

Only `minecraft:dimension_type` and `minecraft:world_clock` are parsed into typed values
today (chunk-column height/sky-light defaults and the day-clock selection come from them);
the other 27 keep only their ordered **names**, since retaining raw NBT for registries
nothing reads (enchantments, biomes) would cost real memory per connection for no consumer.
Add a typed arm beside the existing ones when a third registry becomes load-bearing, rather
than growing a generic NBT cache. An elided or unparseable entry keeps its slot (as
`Option<T>`) rather than being dropped, because dropping one would shift every later holder
id; a resent registry (a Configuration re-run) replaces the whole set rather than appending,
for the same reason.

The server side is the mirror: `ServerProtocol::encode_registry_data` emits the same 29-packet
burst (`select_known_packs` with an empty requested-pack list, all 29 registries, then
`update_tags`) so a real vanilla client can join our integrated server.
`dimension_type`/`world_clock` stay hand-built structured tables, since this server resolves
holder ids out of them elsewhere; the other 27 are relayed as opaque bytes captured verbatim
from a real vanilla server, because nothing here needs to parse their contents — a joining
client just needs a self-consistent copy to resolve tag/holder references inside data
components it already decodes.

### `lodestone-canonical`: the shared pre-Flattening bridge

Every pre-1.13 protocol family (`v1-8`, `v1-9`, and any future one below protocol 404) maps
its own wire block representation through this one shared crate rather than each carrying a
private copy of a large generated table. Two modules in series:

- `flattening::lookup(old_block_id, meta)` — the `(id, meta)` → 1.13-era block name and
  properties, dumped reflectively from the real 1.13.2 server jar's own `DataFixerUpper` (the
  same conversion vanilla itself runs upgrading a pre-1.13 world). It distinguishes a
  resolved entry from *no table entry*, from *requires additional context* (flower pots,
  skulls, double-plant upper halves — identity depends on TileEntity data the id/meta pair
  cannot supply), and one structurally out-of-bounds slot.
- `canonical` — bridges that 1.13-era name/properties to a concrete 26.2 block-state id
  (`lodestone_data::block_states`), via a small hand-verified rename table (a few names are
  stale even relative to 1.13.2's own final registry, and a few more relative to later 26.2
  renames) and property fixups for properties 26.2 added that pre-1.13 storage cannot
  express.

Neither module collapses a failure into air itself — the decision to substitute air belongs
to the **consuming family**, made in its own chunk decoder and counted on a `FallbackTally`
so it stays visible rather than silent. One table serves every pre-1.13 version because the
dumped table upgrades *1.12.2-space* ids and older versions' ids are a strict subset (ids
were only ever added), so the per-version difference is only which slots are populated. This
crate is shared game data, not a protocol family, and names none in
`lodestone-registry` — see `docs/multi-protocol-seam.md` for how `v1-8`/`v1-9`/`v1-14` each use
their own canonicalisation path (`v1-14` is post-Flattening and needs a different,
per-family baked table instead, since 1.16.5 already speaks a flat state-id space).

### Registry types: generated enums instead of strings

`lodestone_data::block::Block`, `item::Item`, and `entity_type::EntityType` are each a
generated `#[repr(u16)]`/`#[repr(u8)]` enum whose discriminant **is** the registry id a
`Holder<T>` carries on the wire — no lookup, no branch, and a per-entry census is a plain
array indexed by the enum. None carries a `Custom` variant and none is
`#[non_exhaustive]`, so a match over one is exhaustive and a version bump that adds an entry
fails every incomplete match at compile time rather than falling through a wildcard — the
enum is built specifically so that a terminal `_ =>` arm, this repo's named island factory,
is never needed. The plugin/custom case lives one level out, in a `*Ref` wrapper (`BlockRef`,
etc.): a `u32` where a value below the built-in count is a registry id and at or above it
indexes an opaque host-owned interner, so an application with no plugins links zero bytes of
interner code.

**`Block` and `StateId` are two different id spaces and conflating them is the mistake that
surfaces late.** `Block` has 1,196 values in **registration** order (wire use: `Holder<Block>`
in `block_event`, tool rules); `StateId` has 32,366 values in **name-sorted** order (wire use:
chunk palettes, `block_update`) and is a validated newtype rather than an enum, because
32,366 hand-named variants buys nothing when no code ever matches on one. The orders are
unrelated permutations — going between them always goes through the generated join
(`StateId::block`, `Block::default_state`), never by assuming the indexes coincide.
`block_states::state_id` is the reverse map (a canonical state string → its global state id)
and is deliberately **derived at first use from the already-committed tables**, behind a
`OnceLock`, rather than itself generated — generating it would add a second drift surface
that must stay in lockstep with the tables it derives from, and a stale one fails in the
worst possible way (a plausible-looking wrong id). Its resolver has three tiers — exact
match, default-plus-named-overrides, default alone — and the default is deliberately not
simply "the lowest id"; do not hand-roll a copy of this fallback, which has silently drifted
from the real one before.

The sound-event registry keeps one canonical id-indexed name column rather than duplicating
those names beside entry metadata. Optional fixed audible ranges are a sparse `(u32, f32)`
table sorted by registry id; absence means the ordinary volume-derived range. The 26.2 report
has 1,968 names and zero fixed-range rows, while the generator still emits any future rows
present in the report. `sound_events::sound_event` bounds-checks the name table first and then
joins the sparse metadata, preserving the same `(name, Option<range>)` API without storing a
second set of 1,968 string pointers.

### `lodestone-data`: the crate these censuses live in

Owns roughly twenty generated 26.2 game-data tables — block states, hardness, collision
shapes, block solidity, item prototypes, entity census/dimensions, tools, sound events,
particle types, menus, data component types, and more — split from the protocol crate
because they describe **the game**, not the wire format (`packet_ids` is the one table that
stayed behind, in `v26-2`, for exactly that reason). Each table has three parts: a generated
`src/generated/*.rs` raw rodata file (never hand-edited), a hand-written `src/*.rs` lookup API
returning `lodestone-model` types, and a dump program under `oracle-java/` that produces the
data it is regenerated from. Two provenance shapes: **registry-report tables**
(`attribute_types`, `entity_types`, `block_states`, `sound_events`, `particle_types`, `menus`,
`items`, `data_component_types`) parse Mojang's own `registries.json`/`blocks.json` reports
directly; **JVM-walked tables** (`hardness`, `collision_shapes`, `block_solidity`,
`entity_census`, `entity_dimensions`, `item_prototypes`, `outline_shapes`, `path_types`,
`snow_support`, `tools`, `block_entity_types`) need a real headless 26.2 server booted and
walked, because the fact in question has no getter and is absent from the reports (block
entity coverage, for instance, is recovered from vanilla's own per-type state-validity check
rather than
by constructing a live `BlockEntity`).

`lodestone-v26-2`'s adapter delegates every data-shaped `VersionAdapter` trait method
(`block_hardness`, `block_collision`, `item_prototype`, `entity_dimensions`, and similar)
straight into this crate, one line each — the seam `lodestone-shell`/`lodestone-physics`
already used before the split and still use unchanged. A version crate other than `v26-2`
needing one of these tables is a different question from this crate becoming version-generic:
per the canonical-internal-version design, 26.2 is the one canonical version and these are
that version's data; `v1-8`/`v1-9`/`v1-14` keep their own *translation* tables for their own
protocol, which is not a second copy of the canonical census.

## How to change it, and the gotchas

- **Every generated file in this cluster is generated — never hand-edit one.** Regenerate
  with `LODESTONE_REGEN=1 cargo test -p <crate> --test <name> <fn> -- --ignored --nocapture`;
  each test file's own header carries the exact invocation.
- **A generated census keyed by a built-in registry must reuse that registry's canonical
  names.** For example, the blast/fire facts use a `Block` registry-id → fact-index
  mapping; they do not repeat block names beside the facts. Its generator checks that
  dump ids are the exact `0..BLOCK_COUNT` permutation and that each dump name joins to the
  same `Block` id before emitting the table.
- **Registry-report tables** use
  `cargo xtask gen-registries --version 26.2 --protocol 776`; run
  `cargo xtask gen-registries --version 26.2 --protocol 776 --check` to detect drift without
  writing. The sound-event generator derives the sparse fixed-range keys from each entry's
  protocol id, so adding a range requires no parallel hand-maintained table.
- **Adding a field to a typed registry struct** (e.g. `DimensionType`): add it to the wire
  struct, to the version-free carrier in `lodestone-model` if a version-free consumer needs
  it, and to the adapter that builds the carrier. Watch for a field that moved into a generic
  `attributes`/component map in 26.2 versus where an older doc or issue describes it as a
  top-level field — a stale description reads as a real gap and is not one.
- **Adding a new `lodestone-data` census**: a dump program or registry-report parser, a
  generated raw table, and a lookup-API file, wired into `lib.rs`'s module declarations.
  `tests/generated_string_columns.rs` fails if a new `&'static str` column is not classified
  in its `ALLOWED` table, so a genuinely new string column needs that entry, not a workaround.
- **Adding a rename or property fixup to the canonical bridge is hand-written work** and
  needs its own justification checked against the decompiled 26.2 source, not merely "the
  registry has a plausible-shaped entry."
- **A dynamic (datapack) registry's entry order is not its bootstrap class's registration
  order** — it is typically alphabetical by resource location. Assuming the bootstrap order
  has silently mis-mapped entries before; settle order against a captured `registry_data`
  fixture or the registry report, never against the class that constructs the entries.
- **`cargo xtask connectedness` cannot see a canonicalisation defect** — it is blind to what
  *value* flows through an already-connected wire, only whether the wire is connected at all.
  A jar-derived oracle (a captured real-server section, or a live server via RCON) is what
  actually verifies a decoded block id.

## Configuration

- `LODESTONE_REGEN=1` switches every generator above from assert to write.
- `live-registry` (Cargo feature) gates the live registry-capture test; `LODESTONE_CAPTURE_
  FIXTURES=1` rewrites captured passthrough-registry fixtures from a live run.
- No other environment variables or feature flags gate any of this; every table is
  `&'static` rodata read unconditionally.

## Dependencies

- `lodestone-model` for every public type these lookup APIs return (`BlockAabb`, `PathType`,
  `DimensionTypeInfo`, `Identifier`, and the rest); `lodestone-data` depends on nothing else.
- `lodestone-canonical` depends only on `lodestone-data`, consumed by `v1-8`/`v1-9` today.
- `lodestone-ecs`/`lodestone-client`/`lodestone-shell` as the consumers of the typed
  `dimension_type`/`world_clock` registry data (chunk geometry, sky-light default, day clock).
- `.cache/mc/26.2/{generated/reports,client-src}` and the 1.13.2 server jar (gitignored,
  fetched per this repo's oracle conventions) as the outside sources every generator dumps
  from; see `docs/oracles-and-benchmarks.md` for how the JVM oracles actually run.
