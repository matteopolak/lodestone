# Dimension visuals: sky, fog and the Nether/End

## What it is

How the client's render path is supposed to look different in the Nether and the
End, versus what it actually does today: which parts already work (sky light
defaulting), which parts are stubbed and ready to wire (fog/sky colour), and
one confirmed bug that undermines both — the connected dimension the render
path reads goes stale the moment a player changes dimension without
reconnecting (portal travel, `/execute in`, end-gateway teleport).

This doc also carries the portal traversal diagnosis: what the client sends,
what happens to the dimension-change packet, and where the picture actually
breaks.

## How it works today

### Sky light default (`SkyDefault`) — real, and now dimension-correct

`crates/lodestone-shell/src/mesher.rs::snapshot_section_live` already reads the
connected dimension off `NetClient::shared_handle().get().player().dimension`
and resolves a **missing** (`Missing`, not stored-as-zero) neighbour sky sample
through [`SkyDefault`](../crates/lodestone-render/src/world.rs). That policy used
to be `overworld => Full, everything else => None`. The rule it should follow
is the dimension type's `has_skylight` field
(`.cache/mc/26.2/client-src/data/minecraft/dimension_type/{overworld,the_nether,the_end}.json`):

| dimension | `has_skylight` |
|---|---|
| `minecraft:overworld` | `true` |
| `minecraft:the_nether` | `false` |
| `minecraft:the_end` | **`true`** |

The old rule silently lumped the End in with the Nether. That is invisible in
normal play (a `Missing` neighbour is rare — only at an unresolved chunk edge)
but is exactly backwards for the End: its islands are genuinely lit by real
per-block sky exposure the server computes and sends the same way as the
overworld, so defaulting an absent End sample to `0` would occasionally render
sky-lit End terrain artificially dark — the mirror image of the
too-bright-nether bug this mechanism was built to prevent.

This has been corrected in this pass: `sky_default_for_dimension` in
`mesher.rs` now maps `minecraft:overworld` **and** `minecraft:the_end` to
`SkyDefault::Full`, everything else (including `minecraft:the_nether` and any
unrecognised/datapack dimension) to `SkyDefault::None`. Unit-tested in
`mesher.rs` (`sky_default_is_full_for_overworld_and_end_none_for_nether_and_unknown`)
without a live server. `crates/lodestone-render/src/world.rs`'s `SkyDefault` doc
comment and `crates/lodestone-render/src/mesher.rs`'s module doc carried the
same "no sky light in the nether/end" claim and have been corrected to match.

This client has no dimension-type *registry* decode at all (see below) — the
match is by well-known dimension id, not by an actual `has_skylight` bit read
off the wire.

### Fog and sky colour — stubbed, not wired

There is exactly **one** fog colour/range in the whole client:
`crates/lodestone-shell/src/gpu.rs::SKY_COLOR`, a compile-time overworld sky
blue, used both as the frame's clear colour and as
`sim::fog_for_render_distance`'s fog colour. `Sim::fog_settings()`
(`crates/lodestone-shell/src/sim.rs`) branches **only** on the eye's fluid state
(lava / water / neither) — never on dimension. Standing in the Nether or the
End today renders exactly the overworld's blue sky-fog fade, just with
different terrain underneath.

This pass adds the missing presets to `lodestone-render` (in scope for this
task) so the one remaining step is a call-site change in files this task was
not permitted to touch:

- `lodestone_render::fog::FogSettings::nether(render_distance: u32)` — the
  Nether's fixed `10..96`-block dense fog (not render-distance-relative, per
  `the_nether`'s dimension type `visual/fog_start_distance` /
  `visual/fog_end_distance`), coloured with the `nether_wastes` biome's
  `visual/fog_color` (`#330808`, dark red). Every other Nether biome has its own
  distinct fog colour the shell cannot yet reach (the standing biome is not
  threaded to the mesher/fog call site) — the same documented-fallback shape
  `sim::water_fog` already uses for its one ocean default.
- `lodestone_render::fog::FogSettings::the_end(render_distance: u32,
  start_fraction: f32)` — a flat near-black edge-fade (`#181318`, the End
  dimension type's `visual/fog_color`), reusing the existing
  `for_view_distance` shape rather than vanilla's separate
  `sky_color`/`fog_color` distance blend (`AtmosphericFogEnvironment`'s
  `skyColorMixFactor`), and not attempting the End's actual starfield sky —
  nothing in this renderer draws a sky dome at all today, overworld included
  (the "sky" is only ever the fog colour the frame clears to).
- `lodestone_render::fog::srgb_u8_to_linear` — a CPU-side sRGB→linear helper
  (the same piecewise EOTF the model/entity WGSL shaders already implement),
  so future dimension colours are computed from their real sRGB hex rather than
  hand-typed as a linear literal. `gpu::SKY_COLOR`'s own doc comment records
  that a hand-typed linear value was once silently the *sRGB* value instead —
  this removes that transcription step for every colour added after it.

All hermetically unit-tested in `crates/lodestone-render/src/fog.rs`
(`nether_fog_is_dense_red_and_clamped_to_render_distance`,
`the_end_fog_is_a_flat_near_black_edge_fade`,
`nether_and_end_fog_are_disjoint_from_the_overworld_sky`,
`srgb_to_linear_hits_known_anchors`).

**What is still zero-pixel** — the wiring these presets need, in files this
task could not touch:

1. `Sim::fog_settings()` (`crates/lodestone-shell/src/sim.rs`) needs a
   dimension branch, read the same way `mesher.rs` already does
   (`net.shared_handle().get().and_then(|h| h.player().dimension)`), inserted
   *before* the existing lava/water override so submersion still wins:
   ```rust
   let base = match dimension {
       Some(d) if d.namespace() == "minecraft" && d.path() == "the_nether" => {
           lodestone_render::fog::FogSettings::nether(self.config.render_distance)
       }
       Some(d) if d.namespace() == "minecraft" && d.path() == "the_end" => {
           lodestone_render::fog::FogSettings::the_end(
               self.config.render_distance,
               crate::gpu::FOG_START_FRACTION,
           )
       }
       _ => fog_for_render_distance(self.config.render_distance),
   };
   // then: if under_lava { lava_fog() } else if under_water { water_fog(..) } else { base }
   ```
2. The frame's **background/clear colour** (`RenderState`'s private `clear:
   wgpu::Color` in `crates/lodestone-shell/src/gpu.rs`) is set once at
   construction from `SKY_COLOR` and never updated per frame — unlike fog,
   there is no `set_clear_color`-style setter. The Nether has no sky dome in
   vanilla either (`"skybox": "none"` in its dimension type) — its "sky" *is*
   the fog colour — so without a per-frame clear-colour setter, the edge of
   the loaded Nether world will fade into red fog and then hit a hard blue
   wall at the horizon/above the world. This needs a new setter mirroring
   `set_fog`, called with the same colour as the active `FogSettings` (e.g.
   `render.set_clear_color(fog.color)` right after `render.set_fog(fog)`), and
   touches `gpu.rs`, which this task could not edit.

### Sky darkening (`sky_darken`) — open question, not touched

`crates/lodestone-shell/src/gpu.rs`/`entity_pipeline.rs`/`model_pipeline.rs`
already port a legacy-vanilla `LightTexture.getSkyDarken`-style day/night curve
(`lodestone_render::entity::sky_darken_for_time_of_day`), folded into the fog
uniform's spare lane and applied to the *sky* half of every lit fragment's
lightmap term. It is driven by the single global `world_time()` clock
regardless of dimension.

- **Nether**: harmless as-is. `has_skylight: false` means every sky sample is
  already `0` (via the `SkyDefault` fix above and the server's own light data),
  and `0 * anything == 0`, so the darken factor never has anything to act on.
- **The End**: `the_end.json`'s dimension type sets `"has_fixed_time": true`
  and, in the attribute list, `"minecraft:visual/sky_light_factor": 0.0`. 26.2
  has replaced the old `LightTexture` entirely with a new
  `EnvironmentAttributeMap`/`LightmapRenderStateExtractor` system
  (`.cache/mc/26.2/client-src/net/minecraft/client/renderer/
  LightmapRenderStateExtractor.java`, `Lightmap.java`) where `skyFactor` is a
  *dimension/biome attribute*, not a function of time of day at all — and
  reasoning from `model_pipeline.rs`'s own `light_term = 0.2 + 0.8 *
  max(sky * sky_darken(), block)` formula, a `sky_light_factor` of `0.0` would
  mean the End's sky exposure should contribute **nothing**, leaving every
  sky-only-lit block at the shader's `0.2` ambient floor regardless of the
  overworld's current time of day — the *opposite* of "pin to full daylight."

  This is flagged rather than fixed: it would require either decoding the
  `dimension_type` registry entry (not done anywhere in this client — see
  below) or a second special-cased id match, and the correct pinned value is
  genuinely uncertain without a live-server screenshot comparison (getting the
  sign of this backwards is exactly the class of mistake `CLAUDE.md`'s
  validation log warns about). **Do not wire a guess here** — verify against
  a live End first.

### No dimension-type registry decode at all

Nowhere in `crates/protocol/v770` does the client decode a `dimension_type`
registry entry's fields (`has_skylight`, `ambient_light`, `skybox`, or any of
the new `visual/*` `EnvironmentAttribute`s). The `Respawn`/`Login` packets
*do* carry a `dimension_type` registry holder id
(`crates/protocol/v770/src/packets/game.rs`), but it is only ever used to
select a hardcoded `ChunkShape` by dimension *name*
(`crates/protocol/v770/src/packets/chunk.rs::ChunkShape::for_dimension`) —
never resolved to its actual registry payload. Every dimension-conditioned
choice in this codebase (`SkyDefault`, and the new fog presets above) is
therefore necessarily a **name-based special case** for the three built-in
dimensions, not a data-driven read of the real registry entry, and a custom
datapack dimension falls back to overworld-shaped chunk framing and
`SkyDefault::None`. Decoding the registry for real is a `protocol/v770`
change, out of scope for this task's file permissions.

## Portal diagnosis

**Lighting a portal**: no special-case code exists anywhere in
`lodestone-shell`/`lodestone-render`/`lodestone-physics` for
`minecraft:nether_portal` or `minecraft:end_portal` (confirmed by grep — zero
hits). Lighting one with flint and steel is an ordinary right-click
block-interaction (`use_item_on`, already generically implemented in
`sim.rs`), and the server alone validates the frame and ignites it; there is
nothing dimension- or portal-specific for the client to get wrong here beyond
whatever generic interaction bugs would affect any other block.

**Standing in a portal / the dimension change itself** is entirely
server-initiated — the client sends nothing to trigger it. What happens on
the wire and in this client, traced end to end:

1. Server sends `RESPAWN` (`play::clientbound::RESPAWN`). `adapter.rs` fully
   decodes it (`Respawn::decode`, including the conditional
   last-death-location field, guarded by the trailing `ensure_empty` misparse
   detector), calls `self.set_dimension(&respawn.dimension)` — which resets
   the per-connection `ChunkShape` so the *next* `level_chunk_with_light`
   packet decodes against the destination dimension's build-height window —
   and emits `ClientEvent::Respawned { dimension, game_mode,
   previous_game_mode, last_death_location }`. **This part works.**
2. `lodestone-client`'s `driver.rs` re-arms `awaiting_player_load` on
   `Respawned` (every non-death dimension change re-seeds the server's
   load-timeout timer the same way a death-respawn does), so the next
   placement teleport is recognised and a `PlayerLoaded` action is sent
   automatically. **This part works.**
3. `lodestone-shell`'s `net.rs`/`sim.rs` map `Respawned` to
   `NetUpdate::Respawned`, which un-marks the player dead, bumps a diagnostic
   respawn counter and updates the status line; the placement teleport that
   immediately follows snaps `position`/`prev_position` together so the frame
   interpolator does not smear the camera across the world. **This part
   works** (shares the tested death/respawn path, `live_death_respawn.rs`).
4. **This is where it breaks.** `lodestone-client/src/state.rs`'s
   `Inner::apply` — the fold every non-entity `ClientEvent` goes through —
   has **no arm for `ClientEvent::Respawned` at all**. Grepped the whole
   crate: `self.player.dimension = ...` is assigned exactly once, in the
   `ClientEvent::Login` arm, never again. `lodestone-ecs`'s
   `ingest::handles_event` (the other place an event could be routed instead)
   only claims entity-family events — `Respawned` is not among them, so it is
   not "handled one layer up" either; it is simply dropped on the floor after
   `set_dead(false)`/counters/status. `NetClient::shared_handle().player()
   .dimension` therefore keeps reporting whatever dimension the player first
   logged into, forever, across any number of subsequent portal trips.

**Consequence**: `mesher.rs::snapshot_section_live`'s `SkyDefault` selection —
the very mechanism this task opened by pointing at as "already fixed" — reads
exactly this stale field. It is correctly wired and correctly reasoned; the
data source underneath it is wrong. A player who logs in on the Overworld and
walks through a portal into the Nether will keep getting `SkyDefault::Full`,
i.e. the original too-bright-nether bug, reintroduced specifically by
*traversal* rather than by fresh login. Any future dimension-conditioned fog
color pick (the wiring spec above) that follows the same
`h.player().dimension` read inherits the identical staleness.

Terrain geometry (build-height window) and camera placement are **not**
affected by this bug — those are driven by `ChunkShape`/the placement
teleport respectively, both of which update correctly and independently of
`player.dimension`.

**The fix** is a one-arm addition to `Inner::apply` in
`crates/lodestone-client/src/state.rs`, mirroring the existing `Login` arm:

```rust
ClientEvent::Respawned { dimension, .. } => {
    self.player.dimension = Some(dimension.clone());
}
```

`lodestone-client` was not in this task's editable file set, so this is
reported rather than fixed. Flagged as a background-task suggestion.

## How to change it

- New dimension colour → add an sRGB hex constant plus a `FogSettings`
  constructor in `crates/lodestone-render/src/fog.rs`, converted through
  `srgb_u8_to_linear`, never hand-typed as linear (see that function's doc
  comment for why).
- New dimension-conditioned render decision → resolve the dimension the same
  way `mesher.rs::sky_default_for_dimension` does (well-known id match on
  `net.shared_handle().get().and_then(|h| h.player().dimension)`), until the
  registry decode above exists.
- Gotcha: any such decision is only as fresh as `player.dimension`, which is
  stale after a portal trip until the `state.rs` fix above lands — verify a
  live gate through an actual dimension change, not just a fresh login into
  the target dimension, or the same "invisible until traversal" trap will
  repeat.

## Configuration

None. All colours/distances here are constants derived from the decompiled
26.2 data files (`.cache/mc/26.2/client-src/data/minecraft/{dimension_type,
worldgen/biome}/*.json`), not runtime-configurable.

## Dependencies

- `lodestone-render::fog` — the presets and the sRGB helper.
- `lodestone-render::world::SkyDefault` — the sky-light-default policy.
- `lodestone-client::DimensionId`/`ClientHandle::shared_handle` — the (stale,
  see above) dimension identity every consumer here reads.
