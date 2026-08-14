# Filled-map and advancement wire

## What it is

The four wire gaps that kept filled maps, the advancements screen
and the statistics screen from having any data at all: the v770 decode arms
for `map_item_data` (id 51) and `update_advancements` (id 130), and the two v770
`ServerProtocol` overrides — `encode_update_advancements` and `encode_award_stats`
— that had never existed, so the server's own advancement and statistic tracking
reached the wire as `ServerDirective::None`.

## How it works

### Decode

Both arms live in `crates/protocol/v770/src/adapter/inventory.rs` and emit into session
state, not per-entity state:

| packet | `ClientEvent` | fold | session component |
|---|---|---|---|
| `map_item_data` | `MapItemData` | `lodestone_game::maps::MapStore` | `SessionMaps` |
| `update_advancements` | `AdvancementsUpdated` | `lodestone_game::advancement::AdvancementStore` | `SessionAdvancements` |

A map is keyed on **map id**, not on an entity — one map can be held by several
players and hung in several item frames at once — and the advancement tree is the
local player's. Both are therefore `session` in `lodestone_model::event::route`;
`ingest` would compile, unit-test green and never run.

### The three field orders that are wrong in the obvious reading

Taken from the 26.2 decompile, not from a vendored schema (a `minecraft-data`
1.21.9 `DisplayInfo` schema disagrees with 26.2):

- **`MapPatch` writes width, height, startX, startY** — not the record's own
  declaration order — and spells "absent" as a **zero width byte with no boolean
  tag**. Reading a leading `bool` consumes the width and desynchronises the rest
  of the packet. The patch is a **sub-rectangle**: vanilla sends only the dirty
  columns, so a walking player produces a 1-or-2-column-wide, 128-tall patch, and
  treating `colors` as a full 16 384-byte frame reads garbage. Index it as
  `colors[x + y * width]`, offset by `start_x`/`start_y`.
- **`DisplayInfo`'s flag word is a raw big-endian `int`** (`writeInt`, not a byte),
  and `announceChat` is **not on the wire at all** — vanilla's reader hardcodes
  `false` — so the bits are `1 = background`, `2 = showToast`, `4 = hidden` with
  nothing between.
- **`AdvancementType`'s ordinals are `TASK, CHALLENGE, GOAL`.** Reading them as
  task/goal/challenge swaps the two rarest frames.

`DisplayInfo`'s `x`/`y` are load-bearing rather than cosmetic: 26.2's advancement
JSON on disk carries **no position**, because vanilla computes the tidy-tree layout
server-side in `TreeNodePosition` and only ever writes the result to the wire. The
wire is the only source of vanilla's own layout.

`update_advancements` also carries an `ItemStackTemplate` icon, which is **not** the
same shape as an `ItemStack`: the template writes the item holder *first* and the
count second, where `ItemStack.OPTIONAL_STREAM_CODEC` leads with the count and uses
`<= 0` as the empty sentinel. A template is never empty, so there is no sentinel.

### Encode

`crates/protocol/v770/src/server_protocol.rs`:

- `encode_update_advancements` lowers `lodestone_server::AdvancementUpdate`
  verbatim. The `DisplayInfo` optional is always written **absent**, because
  `lodestone_server::advancements::Advancement` deliberately carries no
  presentation (that crate has no component model). A client with its own
  advancement table — ours has one — keys on the id and draws its own icon, so this
  is complete for our client and partial for vanilla's, whose screen hides a
  display-less advancement.
- `encode_award_stats` resolves each `StatKey` in the registry its stat type
  dispatches on, from `Stats.java`: `mined` in **block**, the five item counters in
  **item**, the two kill counters in **entity_type**, `custom` in **custom_stat**.
  Getting that mapping wrong is invisible — every id resolves to *something* in the
  wrong registry and the client draws a plausible line about the wrong block. A key
  that resolves in none is **skipped**, and the map length is taken after
  resolution so it always matches the entries that follow.

`minecraft:map_decoration_type` and `minecraft:custom_stat` are **built-in**
registries, so their ids come from the jar and the two const tables in these files
are exact rather than provisional — unlike `TRIM_MATERIAL_IDS` beside them, whose
registry is dynamic.

## How to change it

- Adding a `ClientEvent` variant costs one mandatory arm in
  `lodestone_model::event::route` (it will not compile otherwise) plus a fold and a
  session component, or the router drops it silently.
- The shell side of maps — the 143-entry map colour palette, the icon sprites, and
  a `net.rs` `forward` arm if the screen would rather read the stream than the
  `SessionMaps` component — is not built. `SessionMaps` is the seam.
- To grow the advancement encoder's display info, `lodestone-server` needs an item
  and text model. Until then a vanilla client sees a tree with no widgets.

## Configuration

None.

## Dependencies

`lodestone-model` (the event and the `MapDecoration`/`MapPatch`/`AdvancementEntry`
records), `lodestone-game` (the two folds), `lodestone-ecs` (the two session
components and their `NetIngest` systems), `lodestone-data` (item, entity-type and
block registry id tables the encoders resolve against).
