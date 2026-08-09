# Server chunk light

## What it is

How the integrated server computes the sky and block light it puts on the wire, and where the
per-block-state light census that feeds it comes from. Issue #517: until this landed, every column
`V770ServerProtocol::encode_chunk` sent carried `ColumnLight::new(section_count)` — all-`Missing`, both
layers, every section — while a full `LightEngine` port sat in `lodestone-world` with the client's
singleplayer worldgen as its only production caller.

This is the *server* half of lighting. Two neighbours it is easy to confuse it with:

| doc | what it covers |
|---|---|
| [`light-ramp.md`](./light-ramp.md) | the per-vertex **lightmap curve** that turns a light *byte* into a brightness |
| [`model-smooth-lighting.md`](./model-smooth-lighting.md) | the mesher's corner blend over already-known light |
| this doc | producing the light bytes in the first place, server-side |

## How it works

Three pieces, in dependency order.

**1. The engine.** `crates/lodestone-world/src/lighting.rs` — a port of
`net/minecraft/world/level/lighting/{LightEngine,BlockLightEngine,SkyLightEngine}`. Both layers share one
descending-level 15-bucket flood; they differ only in their sources (block light seeds emitters, sky light
seeds every cell open to the sky at 15). It takes its per-block data through an injected `LightProperties`
trait so the crate stays registry-free. Two entry points:

- `compute_column_light(volume, props)` — one column in isolation, ~**1.0 ms/column** in release.
- `compute_column_light_with_neighbours(nbh, props)` — a 3×3 neighbourhood, **exact for the centre** (light
  decays at least one level per block and 15 < 16, so no source two chunks away can reach it), ~**9.7
  ms/column**.

**2. The census.** `crates/lodestone-data/src/light_props.rs` — per-block-state `(dampening, emission)` for
26.2, 24 distinct pairs behind a `u8` per-state index, zero heap. Read that module's docs and
`crates/lodestone-data/tests/light_props.rs` before touching it; the provenance argument is the whole point
and it is **not** a JVM dump (neither quantity is reachable without running the jar).

**3. The wiring.** `crates/protocol/v770/src/server_protocol.rs`:

```rust
let world_column = build_world_column(&shape, column);
let light = compute_served_light(&world_column);   // <- issue #517
let payload = encode_column_body(cx, cz, &shape, &world_column, &light);
```

`ColumnLight::encode` already spoke the exact `ClientboundLightUpdatePacketData` shape (four bitsets —
present-sky, present-block, empty-sky, empty-block — then the present arrays per layer, ascending), so no
wire-format work was needed. `LightData::Uniform(0)` maps to the *empty* mask and `Uniform(1..=15)`
materialises a filled 2048-byte array, matching vanilla's `DataLayer::isEmpty` (`data == null &&
defaultValue == 0`) and `getData` (which fills from a non-zero default).

### `Missing` is full daylight, not darkness

Worth knowing before debugging anything here. A light section in neither mask is not "dark" — the client
resolves an absent overworld sky section to its dimension default, and
`lodestone_render::mesher::sky_default_for` resolves the overworld to `SkyDefault::Full` (15). Vanilla's own
client behaves the same way: `ClientPacketListener::readSectionList` queues nothing for a section in neither
mask, and `SkyLightSectionStorage` answers 15 above the topmost known section. So the pre-#517 symptom was a
uniformly **bright** world — lit caves, lit sealed rooms, no night.

## How to change it, and the gotchas

**The cross-chunk seam is the open item, and it is measured rather than described.**
`ServerProtocol::encode_chunk` is handed one column and has no access to the `ChunkSource` its neighbours
live in, so `compute_served_light` runs the **isolated** compute. A cache inside `V770ServerProtocol` would
not fix it: at join `join_scheduler::ColumnPipeline` yields columns in spiral **ring** order, so a column's
outward neighbours do not exist yet when it is encoded, and lighting against only the neighbours already seen
biases every column's outward edge dark — a directional bias is *more* visible than a symmetric residual, not
less.

Measured residual, isolated versus the exact 3×3 compute, over ten served chunks (two seeds × five chunks):

| statistic | value |
|---|---|
| chunks with **zero** differing cells | 7 of 10 |
| worst case | 121 of 212,992 cells (0.057%) |
| direction | **never brighter** (0 cells) at every chunk |
| worst cell | sky 6 against 11 (Δ5) |
| bounding box of the worst chunk | `x 0..15`, `y 75..82`, `z 0..15` — surface relief across a column border |

`crates/protocol/v770/tests/server_light.rs` gates that bound, asserts the never-brighter direction as a hard
claim, prints a bounding box on failure, and carries a **control proving the detector fires**: a glowstone
placed in the west neighbour at its local `x = 15` reads 14 in the exact compute and 0 in the isolated one at
the centre's `x = 0`.

**To close it — the brokered `lodestone-server` patch.** Compute light in the chunk source, where the
neighbourhood is already resident, and carry it on `ChunkColumn`:

1. `crates/lodestone-server/Cargo.toml` — promote `lodestone-world` from dev-dependency to dependency.
2. `crates/lodestone-server/src/chunk.rs` — add `light: Option<ColumnLight>` to `ChunkColumn` plus a
   `light()` accessor; implement `lodestone_world::BlockVolume` for `ChunkColumn` keyed by **palette index**
   (no registry needed), and a `LightProperties` built per neighbourhood from
   `lodestone_data::light_props` via each palette entry's state string.
3. `OverworldChunkSource::column` / `region_source.rs` — after the column is materialised, build a
   `Neighbourhood` from the staged store (`crates/lodestone-worldgen/src/overworld/store.rs` already retains
   it) and fill `light`. This runs on the blocking pool alongside generation, where 9.7 ms against
   generation's 61 ms is a 16% add — affordable. It is *not* affordable on the net task, where 361 columns ×
   9.7 ms is 3.5 s of join latency.
4. `compute_served_light` in `server_protocol.rs` — prefer `column.light()` when present, keeping the
   isolated compute as the fallback for sources that do not supply one.
5. Set `SEAM_RESIDUAL_CEILING` in `tests/server_light.rs` to `0.0`.

**Every gap in the props census darkens or occludes; never brighten one.**
`crates/protocol/v770/tests/live_terrain_light.rs` judges the engine against a real vanilla server by
asserting we never produce *more* light than it does, and that claim is sound **only** because a props
shortfall cannot fake it. Guessing an emission upward — or dropping the `lit=false` correction — leaves that
gate green while destroying its argument.

**Do not run `compute_column_light` on live client-side columns.** `lodestone-shell/src/net.rs`'s contract is
unchanged: *MP consumes server light; SP computes it.* This change makes the server hold up its end; it does
not move the client.

**A per-block-change relight does exist now, and this paragraph used to say the opposite.** It said light was
computed per column at serve time only, so a block edit was reflected the next time that column was sent. That
is no longer true in either half: `lodestone_server::light::should_relight` decides, and
`resend_column_for_light` recomputes the edited column from zero and sends a `light_update`.

**Its predicate compares emission *and* dampening, and the dampening half is the whole of a reported bug.** It
compared emission only, which meant an edit that changed what a cell *occludes* rather than what it emits
moved nothing the predicate looked at. Dirt and logs emit `0`, and so does the air that replaces them, so
breaking a tree trunk and the dirt under it sent no light packet at all — and the client, which deliberately
never recomputes light on the live path, kept serving the pre-break value, which under a tree is `0`. The hole
was pitch black.

Nothing was wrong with the propagation, and that is the part worth remembering: `compute_column_light` floods
from zero and seeds every cell open to the sky at `15`, so it gets a freshly opened shaft right on the first
try. The gap was one predicate away from a flood that already worked, and the reason it stayed open is that
the cost of firing on every placement was treated as a correctness argument. It is ~1.0 ms per edit against a
player edit rate of a handful per second. Incremental re-propagation from the changed cell — which vanilla has
and this crate does not — is the optimisation, not the fix.

Two consequences to keep in mind when changing it:

- **Both directions must fire.** Placing a solid has to darken what is under it, exactly as removing a torch
  has to darken what was around it. A predicate keyed on `dampening(new) != 0` rather than on a *change*
  answers water-for-leaves and leaves-for-water inconsistently, and both answers are wrong.
- **A decorative edit must still cost nothing.** `axis`, `facing`, a stone-for-dirt swap: comparing two
  resolved *values* rather than the state strings is what keeps a rotation from paying for a flood and a
  packet.

**The relight is the isolated compute, so the seam above still applies to it.** An edit within 15 blocks of a
column border under-reports by the neighbour's contribution, and the neighbours' own light changes too and is
not re-sent. Answering one edit exactly means nine 3×3 floods and nine packets, which is why it is not done on
the connection task; the shape that makes it affordable is the chunk-source plan above.

**`LIGHT_UPDATE` (packet 48) now has an encoder**, which it did not when the paragraph above was written:
`ServerProtocol::encode_light_update` plus `ServerProtocol::compute_column_light`, with the v770 overrides
beside `encode_chunk`, and `lodestone-server`'s `resend_column_for_light` as the consumer. `light_update.rs`'s
`encode_light_update_matches_the_golden_wire_body` pins it byte-for-byte against the hand-written golden body
the decode arm was already gated on — the only kind of expectation that can catch the real trap, which is that
the wire order is sky / block / empty-sky / empty-block masks *then* the two array lists and is **not**
`LightPatch::from_light_masks`' argument order. A transposition is a well-formed packet the client mis-merges
in silence, so a round-trip through our own decoder cannot see it.

**But the encoder was not what the two open items were blocked on, and building it did not close either.**
Both this doc's Δ5 seam and `lodestone_server::light`'s "light does not cross a chunk border" are properties of
the *computation*; `light_update` is a cheaper carrier for the same values. `compute_column_light` is still the
isolated compute. What closes them is what §12.117 already names — compute light in the chunk source, where
the 3×3 neighbourhood is resident, and carry it on the column — and that carries a trap worth restating: **if
`ChunkColumn` gains precomputed light, `ChunkColumn::set_block` *and* `ChunkStore::set_block` must invalidate
it.** Both write blocks into a retained column without touching anything derived from them, so stale light
would produce a correct-looking wire, a re-meshed client, and no change on screen.

**Fixture warning, because it is unreadable from a test's source.** Seed 1234, chunk (0, 0) — the fixture the
neighbouring `encode_chunk_*` tests use — is a **vacuous world for light**: ocean, so its light is sky 15
above the water and a purely *vertical* decay through it, with zero horizontal sky gradient and zero
block-lit cells. A gate on it exercises neither horizontal propagation nor emission. `server_light.rs` uses
seed 42, chunk (−9, 4) and places its own emitter for exactly this reason. If you add a light gate, count the
axis you care about — "cells with an intermediate level" is satisfied by vertical decay alone.

**That warning has now been paid for twice, and the second time gives you the number to rank by.**
`crates/lodestone-world/tests/vanilla_light_oracle.rs` first selected its survey chunks by counting cells whose
vanilla sky value is *partial* (`1..=14`). The six top-scoring chunks in `r.0.0.mca` all scored exactly
**3584**, which is `14 × 256` — fourteen complete, individually uniform 16×16 layers. Open ocean again, and the
score was honest: sky light really does attenuate one level per block through water. Every one of those cells
is lit from directly above, so a purely vertical propagator would have got the whole chunk right.

Rank by **lateral** variation instead — cells whose value differs from their `+x` or `+z` neighbour — which is
a direct census of light that can only have arrived by spreading sideways. An ocean column scores `0` on it.
The chunks it picks score 2969–5375 and are real cave systems, and the survey over them is what licenses the
numbers below.

### The engine judged against a real 26.2 server's own stored light

`.cache/mc/survival/world` is a world a vanilla server generated **and lit**, and its `minecraft:full` chunks
carry vanilla's own `SkyLight`/`BlockLight` arrays — computed by vanilla, stored independently of the
`block_states` containers we read the terrain from. So the blocks are the input and the light is the expected
answer, neither came from us, and it needs **no container and no JVM**.

Measured over the six most laterally-varying chunks of `r.0.0.mca`, centre-of-a-loaded-3×3:

| statistic | value |
|---|---|
| sky-light disagreements | **0** of 200,704 cells compared |
| block-light disagreements | 4,513 |
| cells where **ours is brighter** | **0** |
| attributable to | 38 `minecraft:glow_lichen` source cells plus their falloff |
| isolated vs 3×3, same vanilla answer | 5,565 vs 4,513 disagreements |

The block-light residual is **not** an engine defect: it is the upstream `blocks.json` `emitLight=0` gap for
`glow_lichen` (and `cave_vines[berries=true]`) that `crates/lodestone-data/tests/light_props.rs` already
records as a known residual. The survey's own per-state tally is what turns "4,513 unexplained cells" into
"one missing census row", which is why that tally exists.

Two scope facts, because the result is easy to over-read:

- **Vanilla materialises a `DataLayer` only where light is non-trivial** — measured over `r.0.0.mca`, 2 to 7
  `SkyLight` sections and 0 to 10 `BlockLight` sections per full chunk, out of 26 possible. The survey
  therefore covers vanilla's own transition band, not the whole column, and its cell-count floor is derived
  from that rather than from the column height. A section vanilla omitted is skipped and counted, because an
  absent layer is not an assertion of zero.
- **Both controls are executed.** An all-transparent props arm floods every sealed cell and produces 24,379
  ours-brighter cells; and the survey refuses to run at all if the census cannot name a palette entry, since a
  naming gap darkens our side and would be measured as agreement.

## Configuration

None. No feature flags, no env vars: light is computed unconditionally for every served column. The census is
static rodata, so there is nothing to load or configure at runtime.

Regenerating the census after a version bump:

```bash
LODESTONE_REGEN=1 cargo test -p lodestone-data --test light_props \
    committed_table_matches_source -- --ignored --nocapture
```

Running the gates:

```bash
cargo test -p lodestone-v770 --test server_light -- --nocapture
cargo test -p lodestone-data --test light_props
# and the outside oracle for the engine itself (needs Docker + scripts/live-oracles/terrain.sh):
cargo test -p lodestone-v770 --features live-terrain-light --test live_terrain_light \
    -- --ignored --nocapture
```

## Dependencies

- `lodestone-world` — `lighting.rs` (the engine) and `light.rs` (`ColumnLight`/`LightData`/`NibbleArray`
  storage plus the wire codec).
- `lodestone-data` — `light_props` (the 26.2 census) and `block_states` (the id → name/properties census the
  census is keyed through).
- `lodestone-server` — `ChunkSource`/`ChunkColumn`, the terrain the light is computed over. The seam fix
  above is the only change it needs.
- `vendor/minecraft-data/data/pc/1.21.11/blocks.json` and `.cache/mc/26.2/src` — the two committed sources
  behind the census. The second is a cache, not repo state; see `light_props.rs`'s docs for what was read
  from it.
