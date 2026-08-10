# Nether portals and multi-dimension world state

## What it is

The integrated server hosts two dimensions and moves players between them through
nether portals. Three modules and one protocol method:

| piece | where | vanilla |
|---|---|---|
| dimension identity + geometry + 8:1 scaling | `lodestone_server::dimension` | `DimensionType`, `DimensionTypes` |
| frame detection, ignition, destination search, the per-player counter | `lodestone_server::portal` | `PortalShape`, `PortalForcer`, `NetherPortalBlock`, `PortalProcessor` |
| the Nether's terrain source | `lodestone_server::NetherChunkSource` + `lodestone_server::nether_chunk_source` | `ServerLevel` for `Level.NETHER` |
| the wire | `ServerProtocol::encode_dimension_change`, overridden in `lodestone_v770::server_protocol` | `ClientboundRespawnPacket` with `KEEP_ALL_DATA` |

A player can light an obsidian frame with flint and steel, stand in it, and arrive in
the Nether with their inventory, XP and health intact; walking back into the portal
they arrived at returns them to the portal they left.

## How it works

### The dimension seam

`ChunkSource` grew three defaulted methods: `dimension`, `sibling` and
`portal_index`. `dimension::DimensionalSource` is a transparent wrapper that answers
all three, and `crate::integrated`'s `with_nether` wraps the overworld source in one
whose `sibling(Nether)` **builds the Nether on first request** — a generator, a
`ChunkStore` and a `DimensionalSource` of its own.

The connection reaches it without a new parameter anywhere. `server::SourceRef` grew
a third arm, `Dimension(&Arc<dyn ChunkSource>)`, and `serve_play` keeps two locals:

```
travelled: Option<Arc<dyn ChunkSource>>        // where the player is now, if not home
pending_travel: Option<Option<Arc<dyn ...>>>   // a change the current tick discovered
```

At the top of each loop iteration `pending_travel` is promoted into `travelled` and
`source` is **shadowed** — so every arm (chunk streaming, block reads, the fall
sampler, the drowning probe) reads the current dimension without knowing portals
exist. The `?Sized` bounds on this crate's `S: ChunkSource` helpers are there
entirely so `SourceRef::get()` can hand back `&dyn ChunkSource`.

### Ignition

`server::apply_use_item_on` has an arm ahead of the placement branch: holding
`minecraft:flint_and_steel`, it calls `portal::ignite` at `relative(pos, face)` —
the cell the fire would go in, which is where vanilla's `BaseFireBlock.onPlace`
searches from. `portal::find_empty_portal_shape` is vanilla's `PortalShape` search,
X axis first, and requires a valid shape (2–21 wide, 3–21 tall) holding **zero**
portal blocks. Every written cell gets a `block_update` packet and an entry in the
portal index.

### Travel

`portal::PortalTracker` is fed "which portal cell am I standing in" from
`serve_play`'s `vitals_tick` arm, once per server tick, alongside the transition
delay from the game rules (`players_nether_portal_creative_delay` for an invulnerable
player, `players_nether_portal_default_delay` otherwise). It fires when the
post-increment counter reaches the delay, and holds a 10-tick cooldown afterwards.

`server::travel_through_portal` then:

1. resolves the destination through `portal::resolve_destination` (scale, search,
   create) and commits the created portal's blocks;
2. sends a `forget_chunk` for **every** column the view tracker believes is loaded;
3. sends the dimension-change pair (`respawn` + placement teleport);
4. sends the new chunk cache centre;
5. rebuilds the `ViewTracker` and installs a fresh `JoinChunkStream::ringed` over the
   whole square.

## How to change it

* **Adding the End** is a `Dimension::End` variant, a source in `with_nether` (rename
  it), and a `from_key` entry. Do **not** reuse the travel path: an End portal is not
  a coordinate-scaled trip — it lands at a fixed obsidian platform — so the
  destination search would put players in the void. `EndGenerator` already exists in
  `lodestone-worldgen`.
* **A second player in the other dimension** already works for terrain (each
  connection has its own `travelled`), but entity streaming does not: `EntityStreamer`
  and `MobHandle` are world-scoped, so a mob in the overworld is still streamed to a
  player in the Nether. See the gaps below.
* **Persisting which dimension a player is in.** `player_data::PlayerData` already
  round-trips a `Dimension` string field and nothing reads it, so a player always
  joins in the overworld. Acting on it means resolving the source *before*
  `begin_play_at`, which is where the join-time `dimension_type` holder id is chosen.

### Gotchas

* **`height` is not `logical_height`.** The Nether is `min_y 0, height 256,
  logical_height 128`. Chunks are framed against **256** (16 sections on the wire, and
  what a client that resolved `the_nether`'s registry entry expects), while nothing
  may be *placed* above 127. `NetherChunkSource::WINDOW_HEIGHT` is 256 and
  `ChunkColumn::from_nether` pads the generator's 128 rows up to it; serving the
  generator's own height is a client-side decode failure, not a short world.
* **`ChunkShape` is now derived from the column** (`shape_for_column` in
  `lodestone_v770::server_protocol`), not hardcoded to the overworld. Both
  `encode_chunk` and `compute_column_light` go through it, deliberately — a
  `light_update` with a different section count than the chunk packet before it is the
  failure that shape function's doc exists to prevent.
* **The 8:1 scale is one expression, `from / to`.** Overworld→Nether is `1/8` and the
  return is `8/1` through the same code. A round trip starting at `x = 0` cannot tell
  multiply-by-8, divide-by-8 and doing nothing apart, which is why the gate spawns at
  `x = 1720.5, z = -523.25` (neither eighth an integer).
* **The transition counter decays by 4 per tick outside a portal, it does not reset.**
  And the comparison is post-increment, so a delay of 80 fires on the **81st**
  consecutive tick and a delay of 0 fires on the first.
* **`PortalIndex` is not persisted.** It is this crate's stand-in for vanilla's POI
  manager, and it is what makes a 128-block overworld search affordable (a blind scan
  is 25 M block reads across 289 ungenerated columns). A portal lit in an earlier
  session is not in it, so the first trip after a restart falls back to the bounded
  16-block scan and beyond that will build a second portal beside the first.
* **The links point one way only.** Only the overworld's wrapper carries siblings; the
  way *home* is the source the connection joined with, which `serve_play` still holds
  as `home`. A mutually-referential pair would leak a whole `ChunkStore` per world.

## Configuration

Game rules, all read live from the shared `WorldStateHandle`:

| rule | default | effect |
|---|---|---|
| `allow_entering_nether_using_portals` | `true` | gates travel *into* the Nether only; a player already there can always come home |
| `players_nether_portal_default_delay` | `80` | ticks a survival player must stand in a portal |
| `players_nether_portal_creative_delay` | `0` | the same for an invulnerable (creative) player |

The Nether's seed comes from `worldgen_data::active_world_seed()`, the same static
`natural_spawn` reads. Its terrain data is the bundled
`assets/worldgen/noise_settings/nether.json` plus
`assets/worldgen/biome_parameters/nether.json`, reached through `NetherResolver` — a
delegating newtype over `EmbeddedResolver` whose **only** override is
`biome_parameters`. Handing the Nether generator the overworld table parses fine and
produces overworld biome names in a dimension whose surface rules and carvers do not
exist, which is why the override is the whole difference.

## Not implemented

Honest list, so nothing here reads as finished:

* **The packet sequence is untested.** `tests/nether_portal_round_trip.rs` drives
  `portal::resolve_destination` — production's own function — and covers the scale,
  the search and the index. It does not drive a live connection walking into a portal
  for 81 ticks, so the `forget_chunk` sweep, the respawn framing and the re-stream are
  verified by construction only.
* **Client-side teardown on arrival is the client's gap, worked around here.** The
  shell's `Respawned` handler does not clear the chunk store, drop entities or switch
  the skybox, and `ScreenEffects::portal_intensity` is hardcoded `0.0` so the fully
  implemented portal overlay and warp can never fire. The server's `forget_chunk`
  sweep covers the chunk store; the sky, the leftover entities and the overlay are
  client work.
* **Flint and steel lights portals and nothing else**, and takes no durability damage.
  A plain fire needs `fire::ticks_after_edit` and a live block-tick queue, and an
  inert fire block looks like a working one. This item did nothing at all before.
* **Neither dimension's *other* one ticks.** `tick::run_tick_loop` holds the overworld
  source, so a Nether chunk gets no random ticks, no fluid flow and no scheduled
  ticks, and vice versa once a player leaves the overworld.
* **Entities and mobs are world-scoped, not dimension-scoped.** A player in the Nether
  is still streamed the overworld's entities. Nether biome spawn lists *are* in
  `bundled_biome_spawners` (the five nether biomes are bundled), so natural spawning
  would pick them up for free if it ran over a Nether column — it does not, because
  the spawner runs from the overworld tick loop.
* **Nothing persists per-dimension.** `region_source` and `entity_storage` both
  hardcode `dimensions/minecraft/overworld/`, so the Nether is in-memory for the
  session. `PortalIndex` is not persisted either.
* **A portal is never extinguished.** Vanilla's `NetherPortalBlock.updateShape`
  removes a portal cell whose frame was broken; nothing here does, so mining a frame
  block leaves floating portal blocks.
* **The End.** Its generator exists; nothing reaches it.

## Dependencies

`lodestone-worldgen`'s `nether` module (the generator), `lodestone-data`'s
`block_solidity` census (the `canBeReplaced` proxy in `portal`), and
`lodestone-v770`'s `server_protocol` for the wire. `dimension` and `portal` name no
protocol and no packet id: the mapping from a `Dimension` to a `dimension_type` holder
id lives behind `ServerProtocol::encode_dimension_change`, because the holder id is a
property of the family's own `registry_data` order.
