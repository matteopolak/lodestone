# Registry data ingest (`registry_data`, dimension types, world clocks)

## What it is

The client's decode of the Configuration-phase `registry_data` packet, and the
two registries it turns into typed values: `minecraft:dimension_type` and
`minecraft:world_clock`. This is what replaced three hardcoded, level-name-keyed
guesses — chunk column height, sky-light presence, and which world clock is "the"
day clock — with values the server actually declared.

Issue [#288]. It is the root cause the issue names for [#34] (sky light matched on
name) and for the overworld/End clock coincidence in
`crates/protocol/v770/src/packets/time.rs`.

> **Provenance, because `git log` will mislead you here.** The whole change set
> landed inside **`a19e5e4 feat(shell): chests reach pixels`**, an unrelated
> commit by a concurrent agent that harvested this work out of the shared index
> before it could be committed under its own message. `git log` for
> `packets/registry.rs` therefore names a chest renderer. Nothing was lost and
> nothing foreign was shipped, but the commit message is not a description of
> this work — this doc is. See `CLAUDE.md`'s repo hazards for the mechanism and
> the practice that avoids it.

## How it works

### 1. The wire (`crates/protocol/v770/src/packets/registry.rs`)

During Configuration the server sends **one packet per synchronized registry** —
29 of them, measured, matching `RegistryDataLoader.SYNCHRONIZED_REGISTRIES`.
Each is:

```text
registry : Identifier                      -- "minecraft:dimension_type"
entries  : VarInt count, then per entry:
    id   : Identifier                      -- "minecraft:overworld"
    data : bool, then network NBT if true   -- Optional<Tag>
```

`RegistryData` decodes that; `ClientRegistries` folds a stream of them.

Two things about the *set* that a guess gets wrong, both measured on the creative
oracle:

- the biome registry arrives as **`minecraft:worldgen/biome`**, not
  `minecraft:biome`;
- `dimension_type` entries arrive **alphabetically** — `overworld`,
  `overworld_caves`, `the_end`, `the_nether` — so `the_nether` is holder **3**,
  not 1. Never assume a holder id.

**Entry order is the holder-id space.** That is the whole value of this decode:
`login`/`respawn` carry `dimension_type` as a bare VarInt index and `set_time`
keys its clock map by a bare `world_clock` index. Before #288 those integers were
unresolvable, which is exactly why the client routed around them by matching the
*level* name instead.

### 2. What is kept, what is dropped

| registry | kept |
|---|---|
| `minecraft:dimension_type` | typed `DimensionType` per entry, in order |
| `minecraft:world_clock` | ordered names (the values are unit compounds) |
| the other 27 | ordered **names** only; NBT dropped |

Ordered names are the id ↔ name mapping, which is universally useful and costs a
`Vec<String>`. Retaining raw NBT for everything would mean holding
`minecraft:enchantment` (32 KB) and `minecraft:worldgen/biome` (20 KB) per
connection for no reader. When a reader appears — damage types for
`EntityDamaged`, chat types, trim patterns — add a typed arm beside
`DimensionType`; do not grow a generic `Nbt` cache.

### 3. The three consumers

Ingest happens in `V770Adapter::handle_configuration`'s `REGISTRY_DATA` arm, into
`V770Adapter::registries`. `V770Adapter::enter_dimension` is then called from both
the `login` and `respawn` arms with that packet's dimension-type holder id, and it
installs everything downstream:

1. **Chunk column geometry.** `ChunkShape`'s `min_y` / `section_count` /
   `world_height` come from the resolved dimension type instead of
   `ChunkShape::for_dimension`'s name match. This is on the chunk-decode path, so
   it reaches every terrain pixel; a wrong height desynchronises the whole column
   decode rather than degrading gracefully. The palette framing and air/biome ids
   are *not* touched — those are properties of the protocol family, not the
   dimension.
2. **Sky-light default.** `ClientEvent::DimensionTypeChanged` →
   `lodestone_ecs::session::ServerDimensionType` →
   `PlayerSnapshot::dimension_type` →
   `lodestone_shell::mesher::sky_default_for_dimension`, whose `has_skylight`
   branch now decides a missing neighbour sky sample. See the routing note below.
3. **Day clock.** The dimension type's `default_clock` names a `world_clock`
   entry; `ClientRegistries::world_clock_id` turns that name into the holder id,
   and `SetTime::clock_for` picks that clock instead of the lowest one present.

### 4. The routing switch, and why it is called out

`lodestone_client::state::SharedState::apply` forwards a `ClientEvent` to the ECS
only when `lodestone_ecs::ingest::handles_event(e) ||
lodestone_ecs::session::handles_event(e)` says so. `DimensionTypeChanged` is
claimed by **`session::handles_event`**, because `apply_local_player_state` folds
it beside `ServerDimension` off the same packet.

Without that one `matches!` arm the decode, the component and the system are all
correct and the whole chain reaches **zero pixels** — the island failure mode that
has hidden working code in this repo three times. `ingest.rs`'s
`handles_event_covers_exactly_the_variants_with_a_system` asserts both halves of
the pair (`!ingest::handles_event` and `session::handles_event`), which is the
check that catches it.

## How to change it

**Adding a field to `DimensionType`.** Add it to the wire struct in
`packets/registry.rs`, to `DimensionTypeInfo` in
`lodestone-model/src/event.rs` if a version-free consumer needs it, and to
`dimension_type_info` in `adapter.rs`. Two field-name traps:

- the codec field is **`has_skylight`**, one word, even though vanilla's accessor
  is `hasSkyLight()`. Code ported from the accessor name silently finds nothing.
- **there is no `bed_works`** and no `respawn_anchor_works` in 26.2. They moved
  into the dimension type's `attributes` map as
  `minecraft:gameplay/bed_rule` and `minecraft:gameplay/respawn_anchor_works`.
  Anything listing `bed_works` as a top-level dimension-type field (including
  issue #288's own scope note) is describing an older game.
- `has_fixed_time` is a **bool** here, not the pre-26.2 `Optional<Long>
  fixed_time`.

**Adding a registry.** Add a `match` arm in `ClientRegistries::apply` and a typed
struct. Nothing else changes: the packet is already decoded and already folded, so
a new registry is a parse, not a plumbing exercise.

**Gotchas.**

- **`data` is `Option`, and for us it is always `Some`.**
  `RegistrySynchronization::packRegistry` elides an entry's contents when the
  entry came from a data pack the client claimed. Our join replies to
  `select_known_packs` with an **empty** list, so nothing is ever elided —
  measured 4 of 4 dimension types and 2 of 2 clocks carrying data. The `Option` is
  still decoded properly: the day we do claim a pack, elision starts immediately,
  and a wrong guess desynchronises the *whole* packet rather than one field.
- **An elided or unparseable entry keeps its slot.** `ClientRegistries` stores
  `Option<DimensionType>` per entry rather than dropping bad ones, because
  dropping one shifts every later holder id by one — far worse than a single
  unresolvable dimension.
- **A resent registry replaces, never appends.** `start_configuration` re-runs
  Configuration and resends the whole set; appending would double every holder id.
- **`None` is not "the overworld".** Every consumer keeps an explicit fallback.
  `sky_default_for_dimension` keeps its pre-#288 name match for exactly this case,
  and an unresolvable holder id **clears** `ServerDimensionType` rather than
  leaving a stale one — a stale `has_skylight` renders a dark dimension lit.
- **A malformed dimension type must not disconnect.** `ClientRegistries::apply`
  cannot fail. A malformed *packet* still errors, because a wrong
  `Optional<Tag>` framing has to be loud.

## Verification

Two files, one live and one hermetic, in the capture-and-replay pattern this repo
uses for wire formats:

```bash
./scripts/live-oracles/creative.sh
cargo test -p lodestone-v770 --features live-registry --test live_registry_data \
    -- --ignored --nocapture          # capture + assert against Mojang's own data
cargo test -p lodestone-v770 --test registry_data   # hermetic replay, always on
```

- `tests/live_registry_data.rs` joins the creative oracle, captures the raw
  `dimension_type` and `world_clock` payloads **the server authored** into
  `tests/fixtures/registry_data_*.hex`, and checks the decoded content against
  `.cache/mc/26.2/client-src/data/minecraft/dimension_type/*.json` — Mojang's own
  shipped data files, parsed at test time rather than transcribed.
- `tests/registry_data.rs` replays those fixtures hermetically with a
  trailing-byte check, and — since issue #275 — asserts the server's own
  `encode_registry_data` payloads are byte-identical to them.

Two notes on the oracle, both of which cost time to discover:

- **`generated/reports/registries.json` does not contain either registry.** Both
  are data-pack registries loaded from JSON, so the report lists them as absent.
  Issue #288 names it as the cross-check; it is the wrong one. The
  `client-src/data/.../dimension_type/*.json` files are the right oracle.
- Nothing in the decode path is validated against bytes our own encoder produced.
  `decode(encode(x)) == x` is satisfied by two symmetric misunderstandings, which
  has already burned this repo (`CLAUDE.md`, evidence standards).

## The server-side mirror (issue #275)

The decode above exists because *vanilla* sends these packets; issue #275 gave
`lodestone-server` the same ability, so a real vanilla client can join our
integrated server. The encoder lives in `crates/protocol/v770/src/server_protocol.rs`:

- `ServerProtocol::encode_registry_data` is a version-free trait method returning
  the Configuration-phase `registry_data` directives. The `V770ServerProtocol`
  override emits exactly two packets — `minecraft:dimension_type` and
  `minecraft:world_clock` — the two this join sequence actually depends on.
- The per-entry NBT bodies are **captured vanilla bytes**, not values rebuilt from
  `lodestone-data`: byte constants copied from the same
  `tests/fixtures/registry_data_*.hex` fixtures this doc's live gate writes, so a
  vanilla client reads its own wire format rather than a re-encoding of our
  understanding. `tests/registry_data.rs` asserts the emitted payloads are
  byte-identical to those fixtures — `decode(encode(fixture)) == fixture` with the
  fixture standing outside both.
- `serve_connection_inner`'s `LoginAcknowledged` arm calls `encode_registry_data`
  **before** `begin_configuration`, so the registries precede
  `FINISH_CONFIGURATION`. That ordering is a version-free invariant in the loop,
  not a per-implementor promise.
- The default method emits nothing, so a hosting family with nothing to declare,
  or a legacy family that does not host at all, sends no packets — the same
  additive seam as every other optional encoder.

**How to change it.** To ship more registries (e.g. `minecraft:worldgen/biome`),
capture the payload from the live oracle (extend `tests/live_registry_data.rs`),
check the fixture in, and add a packet to the override's vec. Our join claims **no
known packs**, so the server must not elide entry contents either — the
`bool(true)` + full-NBT shape is what a vanilla client that knows no packs expects.

Measured values, from the live run:

| dimension type | holder | `has_skylight` | `min_y` | `height` | `logical_height` | `coordinate_scale` | `ambient_light` | `default_clock` |
|---|---|---|---|---|---|---|---|---|
| `overworld` | 0 | true | -64 | 384 | 384 | 1.0 | 0.0 | `minecraft:overworld` |
| `overworld_caves` | 1 | true | -64 | 384 | 384 | 1.0 | 0.0 | `minecraft:overworld` |
| `the_end` | 2 | **true** | 0 | 256 | 256 | 1.0 | 0.25 | **`minecraft:the_end`** |
| `the_nether` | 3 | false | 0 | 256 | **128** | 8.0 | 0.1 | **absent** |

Two rows falsify things the old code assumed:

- the End has sky light **and** its own clock (holder 1). The lowest-holder-id
  pick returned the overworld's clock in the End — not a data-pack edge case, the
  default vanilla behaviour.
- the Nether's `logical_height` is half its `height`, a value no name match
  produced because nothing read it.

## Configuration

None. No feature flag gates the ingest — it is unconditional in the Configuration
handler. The `live-registry` Cargo feature gates only the live capture test, and
`LODESTONE_CAPTURE_FIXTURES=1` rewrites the fixtures from a live run.

## Dependencies

- `lodestone-core` — `Reader`/`Writer`, `Nbt`, `read_network_nbt`.
- `lodestone-model` — `DimensionTypeInfo` and `ClientEvent::DimensionTypeChanged`
  (the version-free carriers).
- `lodestone-ecs` — `session::ServerDimensionType` and the `handles_event` switch.
- `lodestone-client` — `PlayerSnapshot::dimension_type`.
- `lodestone-shell` — `mesher::sky_default_for_dimension`, the pixel consumer.
- Behavioural reference only, never transliterated:
  `.cache/mc/26.2/src/net/minecraft/network/protocol/configuration/ClientboundRegistryDataPacket.java`,
  `core/RegistrySynchronization.java`, `world/level/dimension/DimensionType.java`,
  `world/clock/{WorldClock,WorldClocks}.java`.

## Still open

- The other 27 registries are names-only. `EntityDamaged.damage_type_id` is still
  an unresolved integer; resolving it is now a parse away.
- `has_fixed_time` is decoded and carried but nothing reads it. Vanilla gates
  `Level::isDarkOutside`/`isNight` on it, so a Nether/End session should not have
  a night at all. Until something does, a dimension with no `default_clock` (the
  Nether) deliberately falls back to the lowest-id clock, which is exactly the
  pre-#288 behaviour there and no worse.
- Vanilla's 26.2 `skyDarken` comes from the dimension type's
  `EnvironmentAttributes.SKY_LIGHT_LEVEL`, not directly from a clock. The
  `attributes` map is dropped by this decode, so the sky curve is still our own
  port rather than a data-driven read. See [`dimension-visuals.md`](./dimension-visuals.md).
- The other 27 registries are names-only on the *client* and not emitted at all
  by our *server* — issue #275 shipped only `dimension_type` and `world_clock`,
  the two this join sequence reads. When a third becomes load-bearing (damage
  types, `worldgen/biome`), the capture-and-replay harness above extends to it in
  both directions.

[#288]: https://github.com/matteopolak/lodestone/issues/288
[#34]: https://github.com/matteopolak/lodestone/issues/34
