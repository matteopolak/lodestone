# Nether portals and multi-dimension world state

## What it is

The integrated server hosts two dimensions and moves players between them through
nether portals. Three modules and one protocol method:

| piece | where | vanilla |
|---|---|---|
| dimension identity + geometry + 8:1 scaling | `lodestone_server::dimension` | `DimensionType`, `DimensionTypes` |
| frame detection, ignition, destination search, the per-player counter | `lodestone_server::portal` | `PortalShape`, `PortalForcer`, `NetherPortalBlock`, `PortalProcessor` |
| the Nether's terrain source | `lodestone_server::NetherChunkSource` + `lodestone_server::nether_chunk_source` | `ServerLevel` for `Level.NETHER` |
| the End's terrain source, reachable via `with_nether`'s sibling factory | `lodestone_server::EndChunkSource` + `lodestone_server::worldgen_data::end_chunk_source` | `ServerLevel` for `Level.END` |
| End-portal-frame ignition, ring detection, the fixed arrival | `lodestone_server::portal::{ignite_end_portal_frame, end_portal_arrival}` | `EnderEyeItem.useOn`, `EndPortalFrameBlock`, `EndPortalBlock.getPortalDestination` |
| the wire | `ServerProtocol::encode_dimension_change`, overridden in `lodestone_v770::server_protocol` | `ClientboundRespawnPacket` with `KEEP_ALL_DATA` |

A player can light an obsidian frame with flint and steel, stand in it, and arrive in
the Nether with their inventory, XP and health intact; walking back into the portal
they arrived at returns them to the portal they left.

A player can also place an eye of ender into each of a 12-frame `end_portal_frame`
ring (each frame facing the ring's centre); the twelfth eye fills the ring's 3×3
interior with `end_portal` blocks, and stepping into one of those teleports the
player to the End's fixed obsidian arrival platform. There is no stronghold
generator, so nothing places such a ring naturally yet — reaching one today means
hand-placing the frames (e.g. in creative), which is a legitimate way to play, not a
workaround.

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

**The destination search warms its columns in parallel first.** `portal::create_portal`'s
site search touches every column in a fixed 33 x 33 block square around the scaled
arrival point, unconditionally — vanilla's own `PortalForcer.createPortal` does too (see
that function's doc comment). For a dimension nothing has looked at yet, that footprint
spans several never-generated chunk columns, and `crate::chunk_store`'s own module doc
measures a single fresh column at ~909 ms; touched one `block_state` read at a time, a
first trip could pay that cost several times over before any packet told the client it
had arrived — the join-strip stall's shape (`DESIGN.md` §12.165) applied to a portal
instead of a chunk-boundary crossing. `create_portal` now calls
`crate::chunk::generate_columns_parallel` on whatever the search's footprint is *not*
already `is_column_resident` for, before the search itself runs — a no-op on a warm
dimension (every return trip, and every outbound trip after the first), and a real
fan-out across `std::thread::available_parallelism` threads on a cold one. Native only:
the fan-out is `std::thread::scope`, which panics on `wasm32-unknown-unknown`, so the
prefetch is `#[cfg(not(target_arch = "wasm32"))]` and a browser singleplayer trip pays
the serial cost unchanged — see `portal.rs`'s wasm hazard notes on that call.

### Death away from home

A death with no usable bed respawns at the world spawn in `minecraft:overworld` —
`server::apply_client_command`'s `PERFORM_RESPAWN` arm, and `ServerProtocol::encode_respawn`
always encodes that dimension, matching vanilla's own no-bed/no-anchor default. That
packet alone is not enough when the player died **away from home** (mid-Nether-trip):
the *server's* own dimension tracking — `travelled`, `view`, `join_stream` in
`serve_play`'s loop — stays pointed at the dimension they died in, since nothing else
in the respawn path touches it. The client would be correctly told "you are now in the
overworld at (x, y, z)" and then never sent a single column for that position, because
the join stream never re-centred and the connection kept reading terrain from the
Nether.

`apply_client_command` now takes an `away_from_home: bool` (`dispatch_play_packet`'s
`ClientCommand` arm computes it from `matches!(source, SourceRef::Dimension(_))`) and an
`&mut Option<Vec3>` out-parameter, `dimension_reset`, set to the resolved respawn
position exactly when both are true. The native `serve_play` loop — the only one with
`pending_travel` in scope; `wasm32` never leaves the dimension it joined in, so this is
always `None` there — reads it back right after `dispatch_play_packet` returns and runs
the same forget-chunk/recentre/rebuild-join-stream sequence `travel_through_portal`'s
own tail runs, then parks `pending_travel = Some(None)` so the loop's `source` reverts
to `home` starting the next iteration.

### Arrival, on the client

The server half above puts a player in real Nether terrain. Making the trip *look*
like a trip is the client's half, and it hangs off one event: `ClientEvent::Respawned`
carries the destination dimension, so **the same packet reports a death-respawn and a
portal trip** and the client has to tell them apart.

`Sim::apply_respawn` (`crates/lodestone-shell/src/sim/dimension.rs`) is where that
happens, and the comparison is the whole safety argument — with it dropped, every
death in the game would drop the entity index and throw away every meshed column,
which is a far worse bug than the leftover mobs it exists to fix.

| what the client does on a dimension **change** | what it does on a same-dimension respawn |
|---|---|
| drops every other entity (`reset_ingest_entities` + `reset_entity_tracks`) | nothing |
| forgets every meshed column and flushes in-flight mesh jobs (`TerrainMesh::end_session`) | nothing |
| resets the interpolator accumulator | nothing |
| switches the sky pass off (`Sim::sky_mode`) and the fog to the Nether's red | — (per-frame reads, no edge needed) |

Nothing player-scoped is touched on either path — inventory, XP, health, hunger and
air all cross the portal, and `a_dimension_change_leaves_the_inventory_xp_and_health_alone`
asserts concrete non-default values rather than merely that the components still
exist. Getting that split wrong shows up as an emptied inventory on the first trip.

The dimension travels on `NetUpdate::Respawned`'s own field rather than being read
back off the shared handle at the consumer, and that is not a style choice:
`Driver::emit` folds the read model (`read_model.apply(&event)`) **before** it queues
the event, so by the time the shell drains its channel a frame later
`Sim::dimension()` already reports the *new* dimension — a consumer reading it there
would compare the new dimension against itself and never see a change at all.

The portal overlay and warp are driven by `Sim::portal_effect_intensity`; the curve
and the two-consumer split are in [`screen-overlays.md`](./screen-overlays.md).

### Where the decoded chunk store is cleared, and why not at the shell

The store is cleared by `lodestone_client::Driver::forget_previous_dimension`, called
from `Driver::emit`'s `ClientEvent::Respawned` arm. That site was chosen, not
defaulted to:

- the store is written by the **net thread** as packets decode, through the adapter's
  `WorldSink`;
- the shell's reset runs on the **render thread**, when it next drains
  `NetClient::poll`;
- columns for the *new* dimension can already be in the store by then, and a bulk
  clear there would delete terrain no server will resend — trading leftover geometry
  for a permanent hole.

`Driver::emit` is on the net thread, runs after that packet's world-write guard has
been dropped and before the next `read_packet_timed`, so there is no window in which a
new-dimension column can exist yet.

Dropping the *meshed* columns, which the shell still does, is safe in exactly the way
clearing the store is not: a column still in the store is re-meshed the moment
anything dirties it, so an over-eager mesh drop costs a re-mesh and an over-eager
store clear costs the chunk. That asymmetry is why the two halves of the reset live on
two different threads.

The driver compares the destination against the dimension it last recorded (`Login`,
then each `Respawned`) and clears **only** on a change. A death-respawn reports the
same `DimensionId`; without the comparison every death in the game would empty the
store and force a full terrain reload, which is a worse bug than the one this fixes.
`a_dimension_change_empties_the_chunk_store_and_a_death_respawn_does_not` in
`crates/lodestone-client/tests/driver.rs` gates all four arms — first load,
death-respawn, portal trip, and a second death in the destination — and both neuters
(dropping the comparison; never clearing) were run and observed to fail, in opposite
directions.

### End portal ignition and the fixed arrival

`server::apply_use_item_on` has a second arm, alongside flint-and-steel and ahead of
the placement branch: holding `minecraft:ender_eye` and right-clicking an unfired
(`eye=false`) `end_portal_frame` calls `portal::ignite_end_portal_frame` at the
clicked cell (vanilla clicks the frame itself, not a relative cell — unlike
flint-and-steel). It always returns the frame's own `eye=true` write; when that eye
is the ring's twelfth it also returns the 3×3 interior `end_portal` fill. The call
site writes both, sends a `block_update` per cell, and consumes one eye
(`consume_one`, the same creative-mode no-op every other item-consuming arm uses).

**The ring check is not a port of vanilla's `BlockPattern`.** `EndPortalFrameBlock`
builds a general, reusable multi-block matcher (also used for the iron golem and the
wither) and searches a rotated, translated window for it; porting that generic engine
for one fixed pattern would be infrastructure this crate has no other user for.
`ignite_end_portal_frame`'s own doc comment derives the *result* of applying it to
this one pattern instead: every one of the 12 rim frames must be eyed and must face
the ring's centre (north edge → `facing=south`, south edge → `facing=north`, west
edge → `facing=east`, east edge → `facing=west` — the familiar "arrow points inward"
rule every vanilla stronghold portal room shows). The clicked frame's own facing pins
which edge it can be on, so the search only tries the three lateral offsets along
that edge rather than a full rotation sweep.

Travel reuses `portal::PortalTracker` — the same per-tick counter the Nether uses —
because vanilla's own cooldown and counter (`Entity.portalProcess`/`portalCooldown`)
are generic across portal types, not Nether-specific. What differs is the delay:
`Portal.getPortalTransitionTime`'s default is 0, and `EndPortalBlock` does not
override it, so an end portal fires on the very first tick standing in it — the
Nether's gamerule-configurable delay is `NetherPortalBlock`'s own override of that
default, not the shared behaviour. `serve_play`'s `vitals_tick` arm reads which block
type the fired counter's entry cell holds and dispatches to
`travel_through_portal` or the new `travel_through_end_portal` accordingly, so the
two portal types share one counter and one cooldown but two different destination
resolutions.

`server::travel_through_end_portal` is deliberately **not** a generalisation of
`travel_through_portal`: an End portal has no coordinate scale and no linked-position
search. `portal::end_portal_arrival` names the fixed arrival point —
`Dimension::end_spawn_point` (100, 50, 0), with the `ServerPlayer`-only one-block drop
`EndPortalBlock.getPortalDestination` applies (`spawnPos.subtract(0.0, 1.0, 0.0)`), so
the player actually lands standing on the obsidian floor rather than floating above
it — and `portal::ensure_end_platform` builds (or repairs) the platform there before
the packet sequence below runs. The packet sequence itself (forget every loaded
column, the dimension-change pair, the new cache centre, the rebuilt view and join
stream) is identical to the Nether's, because it is the same client-side contract
regardless of which portal type triggered it.

**The return trip (`fromEnd == true` in `EndPortalBlock.getPortalDestination`) is not
implemented.** It needs the stronghold's exit portal and the dragon fight, neither of
which exists in this crate. `serve_play`'s dispatch guards this explicitly: an end
portal reached while already in `Dimension::End` is inert (no travel, no error)
rather than sending the player anywhere — the same "correct degradation"
`travel_through_portal` itself falls back to for a world with no sibling wired.

### Is the `forget_chunk` sweep redundant now?

**No.** The sweep is *protocol*, not client bookkeeping: it is what a real vanilla
client needs in order to unload the old dimension, and it is the only thing that tells
any non-Lodestone client anything at all. Deleting it because our own client now also
clears locally would break every other client against our server, and it would also
falsify step 1 of `travel_through_portal`'s ordering argument.

The two are belt and braces against different failures, and the client-side clear
exists for the case the sweep cannot cover: a **vanilla** server, which sends no such
sweep. Note that `travel_through_portal`'s own doc still says the client's `Respawned`
handler does not clear the store; that sentence is now stale and belongs to whoever
next edits `lodestone-server`.

## How to change it

* **The End** (`Dimension::End`) is a real variant, with real geometry transcribed
  from `data/minecraft/dimension_type/the_end.json`, a real generator
  (`worldgen_data::end_generator`/`end_chunk_source`), a real `EndChunkSource`
  (`chunk.rs`, `ChunkColumn::from_end` for the 128→256 pad, mirroring
  `NetherChunkSource`/`from_nether`), and is wired into `crate::integrated`'s
  `with_nether` sibling factory the same way the Nether is — a world can
  `sibling(Dimension::End)` into real terrain. Ignition
  (`portal::ignite_end_portal_frame`), the fixed arrival
  (`portal::end_portal_arrival`, `ensure_end_platform`) and the travel path
  (`server::travel_through_end_portal`, dispatched from the shared `PortalTracker`
  in `vitals_tick`) are all wired — see "End portal ignition and the fixed arrival"
  above for the chain. **What is still missing:**
  * No stronghold generator exists to place a naturally-occurring
    `end_portal_frame` ring anywhere, so the only way to reach one today is
    hand-placing frame blocks — a legitimate creative-mode path, not a stopgap.
  * The return trip (stepping into an `end_portal` block *while already in the
    End*) is unimplemented — it needs the stronghold's exit portal and the dragon
    fight. `serve_play` guards this to a no-op rather than a wrong destination.
  * The dragon fight and the exit-portal/return-gateway mechanism are unbuilt
    entirely; they are separate pieces of work from the entry path this session
    landed.
  See issue #330's tracking comment for the session that landed the generator
  wiring and the session that landed ignition and travel.
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

* **The packet sequence is untested end to end.** `tests/nether_portal_round_trip.rs`
  drives `portal::resolve_destination` — production's own function — and covers the
  scale, the search and the index. It does not drive a live connection walking into a
  portal for 81 ticks, so the `forget_chunk` sweep, the respawn framing and the
  re-stream are verified by construction only. Two of the pieces that sequence depends
  on now have their own narrower, function-level gates instead: `portal::tests::
  the_cooldown_re_arms_while_still_inside_a_portal` (`PortalTracker::tick`'s
  re-arm, so a player who materialises inside the destination portal does not bounce
  straight back), and `server::tests::a_death_away_from_home_asks_for_a_dimension_reset`
  / `a_death_at_home_does_not_ask_for_a_dimension_reset` (the "die away from home"
  dimension-reset signal, with its own control). Neither substitutes for the full
  81-tick live drive; both were run against a deliberate neuter and observed to fail.
* **Client-side teardown on arrival is complete.** The sky pass, the leftover entities,
  the meshed columns, the portal overlay and — since
  `Driver::forget_previous_dimension` — the decoded chunk store are all wired; see
  "Where the decoded chunk store is cleared" above. The `forget_chunk` sweep stays,
  because it is what a vanilla client needs, not a workaround for a client-side gap.
* **Flint and steel lights portals and nothing else**, and takes no durability damage.
  A plain fire needs `fire::ticks_after_edit` and a live block-tick queue, and an
  inert fire block looks like a working one. This item did nothing at all before.
* **A sibling dimension now ticks on its own**, once a player has visited it at least
  once this session. `dimension_tick::spawn_for_dimension` starts a second
  `run_tick_loop_with_weather` the first time `with_nether`'s factory builds a
  Nether/End `ChunkSource`, following the same dimension-tagged `TickAnchors` handle
  every connection already publishes into.
* **Live placement routing now follows the player across a portal trip, too.**
  `server.rs`'s connection loop used to thread one fixed `BlockEntityHandle`/
  `BlockTickFeed` pair through the whole connection, taken from the join dimension, and
  never swapped it on a portal trip — so a furnace lit or a lever flipped while standing
  in the Nether landed in the overworld's registry, and could even collide with an
  overworld block entity at the same `BlockPos`. The fix needed no new `serve_play`
  parameter: `crate::dimension::DimensionalSource::alone_with_dimension_handles` stores
  each sibling's own registry/scheduled-tick queue/tick feed directly on the source
  `ChunkSource::sibling` returns, reachable through `ChunkSource::world_registries`/
  `ChunkSource::block_tick_feed`; `server::dimension_scoped_handles` reads them back and
  the connection loop shadows its local `block_entities`/`block_ticks` bindings the same
  way `source` itself is already shadowed on travel. This closes the registry-collision
  risk too, as a side effect rather than by widening the `BlockPos` key: each dimension
  now routes through a physically separate registry instance. Random ticks (crop
  growth, fire, leaf decay) and any scheduled tick already restored from a dimension's
  own saved region file were never affected by the old gap, since neither went through
  the connection's fixed handle. See `dimension_tick.rs`'s own module doc for the full
  account and its own tests, plus `server::tests::dimension_scoped_handles_*` and
  `integrated::tests::a_nether_sibling_answers_its_own_registry_and_tick_feed`, for a
  playerless-dimension gate with controls.
* **Entities and mobs are world-scoped, not dimension-scoped.** A player in the Nether
  is still streamed the overworld's entities. Nether biome spawn lists *are* in
  `bundled_biome_spawners` (the five nether biomes are bundled), so natural spawning
  would pick them up for free if it ran over a Nether column — it does not, because
  the spawner runs from the overworld tick loop.
* **Terrain now persists per dimension.** `RegionChunkSource::new` takes the
  `Dimension` it is rooting, so `IntegratedServer::open_persistent_with_mobs` roots a
  Nether/End sibling's own `RegionChunkSource` under
  `<world>/dimensions/minecraft/the_nether|the_end/region` (verified against
  `.cache/mc/survival/world`'s own layout) the first time that sibling is built,
  rather than staying in-memory for the session. **Still not persisted**:
  `entity_storage` (mobs/dropped items) is still overworld-only — extending it needs
  the entity/mob dimension-scoping item above first, since an entity has no dimension
  tag of its own yet. `PortalIndex` is still not persisted either.
* **A portal is now extinguished when its frame breaks.** `portal::should_extinguish`
  ports `NetherPortalBlock.updateShape`'s three-clause condition (wrong-axis skip,
  portal-neighbour skip, frame re-scan), and `portal::extinguish_broken_frames` runs
  it outward from a changed cell as a cascade — mining one frame block clears the
  whole interior, not just the cell nearest the break, matching vanilla's own
  `setBlock`-triggers-`updateShape`-on-its-neighbours chain. Wired into
  `server::destroy_block` alongside `collapse_unsupported`, on the same broken cell.
* **The End is reachable from a running server**, entry-only: `Dimension::End`, its
  generator, `EndChunkSource`, the obsidian platform's geometry
  (`portal::ensure_end_platform`), frame ignition
  (`portal::ignite_end_portal_frame`) and travel (`server::travel_through_end_portal`)
  all exist and are wired. A creative player can build a 12-frame ring by hand, place
  the final eye, and step through to real End terrain. **What remains**: no
  stronghold generator places a ring naturally (see "How to change it" above), and
  the return trip — stepping into an `end_portal` block from *inside* the End —
  is a deliberate no-op pending the stronghold's exit portal and the dragon fight,
  neither of which this crate builds.

## Dependencies

`lodestone-worldgen`'s `nether` module (the generator), `lodestone-data`'s
`block_solidity` census (the `canBeReplaced` proxy in `portal`), and
`lodestone-v770`'s `server_protocol` for the wire. `dimension` and `portal` name no
protocol and no packet id: the mapping from a `Dimension` to a `dimension_type` holder
id lives behind `ServerProtocol::encode_dimension_change`, because the holder id is a
property of the family's own `registry_data` order.
