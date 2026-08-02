# Dimension visuals: sky, fog and the Nether/End

## What it is

How the client's render path is supposed to look different in the Nether and the
End, versus what it actually does today: which parts already work (sky light
defaulting, and now fog colour and the frame clear colour), the one dimension
attribute deliberately left unwired (the End's sky-darken factor, pending a
live-server check), and one bug that used to undermine the rest — the connected
dimension the render path read went stale the moment a player changed dimension
without reconnecting (portal travel, `/execute in`, end-gateway teleport). **That
one is fixed**; the diagnosis is kept below because it is the best record of how it
failed and of the trap that hid it.

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

**Superseded by issue #288, which landed the registry decode.**
`sky_default_for_dimension` now takes a second argument — the server's own
`minecraft:dimension_type` entry, carried on `PlayerSnapshot::dimension_type` —
and when it is present its `has_skylight` **is** the answer; the level name is not
consulted at all. That closes issue #34 properly: a data pack pointing a level called
`mypack:mine` at the vanilla overworld type used to fall through to
`SkyDefault::None` and render its terrain dark, and a custom skylight-less type on
`minecraft:overworld` used to be assumed lit. Both directions are now asserted in
`mesher.rs`'s `a_server_declared_dimension_type_overrides_the_level_name_match`.

The name match above survives only as the fallback for a server that sends no
`registry_data`. See [`registry-data-ingest.md`](./registry-data-ingest.md).

> **Scope split.** This doc owns fog *colour* — which dimension and biome picks
> which colour, and how it stays in step with the frame clear. The fog
> **distance** math (where the ramp starts, how wide it is, which metric measures
> it) moved to [`fog.md`](./fog.md) with issue #388. If you are here because fog
> looked too thick rather than the wrong hue, that is the other doc.

### Fog colour and clear colour — both wired now

`Sim::fog_settings()` (`crates/lodestone-shell/src/sim.rs`) now branches on the
connected dimension, read the same way `mesher.rs` does
(`net.shared_handle().get().and_then(|h| h.player().dimension)`), *before*
falling through to the lava/water override so submersion still wins over the
dimension fog (the priority order this doc originally specified):
`minecraft:the_nether` selects `FogSettings::nether`,
`minecraft:the_end` selects `FogSettings::the_end`, and anything else
(including pre-login `None`) keeps `fog_for_render_distance`. This reaches a
pixel every frame through the existing `app.rs` call site
(`let desired_fog = self.sim.fog_settings(); ... render.set_fog(desired_fog);`),
which needed no change at all — it already re-applied whatever
`fog_settings()` returned each frame the value changed.

The **frame clear colour is the one half still not wired** — see below.

This pass had already added the presets to `lodestone-render`:

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
  render-distance edge shape rather than vanilla's separate
  `sky_color`/`fog_color` distance blend (`AtmosphericFogEnvironment`'s
  `skyColorMixFactor`), and not attempting the End's actual starfield sky.

  > **Stale, corrected.** This bullet used to end "nothing in this renderer draws
  > a sky dome at all today, overworld included (the 'sky' is only ever the fog
  > colour the frame clears to)". That was true when written and is now false: the
  > overworld sky dome, sun, moon, stars, clouds, a per-fragment
  > horizon-to-zenith gradient, the sunrise/sunset band and void fog all render —
  > see [the sky pass](./sky-and-air-bubbles.md). Issue #96 was filed quoting this
  > sentence as evidence, which is exactly `CLAUDE.md` rule 2: the claim was
  > evidenced when written and nothing about it looked wrong on inspection. The
  > End specifically still has no dome (`DimensionType.Skybox.END` is a different
  > draw — a cube-mapped `end_sky.png`, not the overworld disc), so flat fog
  > remains the right approximation *there*.
- `lodestone_render::fog::VoidFog` — the world-bottom darkening
  (`FogRenderer.computeFogColor`'s quadratic `darkness` term). Consumed by the
  sky pass via `SkyFrame::with_void_fog`; not yet applied to the *distance* fog
  or the frame clear, which are computed in `sim.rs`/`app.rs`.
- `lodestone_render::fog::multiply_gamma` / `scale_gamma` — gamma-space colour
  arithmetic, because vanilla's `ARGB.multiply` is `red(lhs)*red(rhs)/255` on raw
  sRGB bytes and its fog darkening scales `ARGB.redFloat(color)`. Doing either in
  linear space pulls the factor toward 1.0 and washes the result out.
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

**The frame clear colour is now wired too.** `RenderState::set_clear_color`
(`crates/lodestone-shell/src/gpu.rs`) mirrors `set_fog` exactly — it replaces
the private `clear: wgpu::Color` field the constructor seeds from `SKY_COLOR`
— and `app.rs`'s `redraw()` calls it right after `render.set_fog(desired_fog)`,
with the *same* `desired_fog.color`, inside the existing
`if self.applied_fog != Some(desired_fog)` change-detection (so this piggybacks
on that guard at zero extra cost rather than adding a second one). Fog and
clear colour are therefore always one value, per `SKY_COLOR`'s own doc comment
on why a second, independently-drifting copy of the sky colour is exactly how
the horizon ends up banding in a colour the sky never is. The Nether has no sky
dome in vanilla (`"skybox": "none"` in its dimension type) — its "sky" *is* the
fog colour — so this closes the exact gap this section used to describe: the
edge of the loaded Nether world now fades into the correct red fog *and* the
horizon/above-world clear is that same red, not a hard blue wall. `gpu.rs` was
off limits to the task that wrote the original version of this section; it is
not off limits to whoever landed this.

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
  reasoning from `model.wgsl`'s own `lightmap_term` (see
  [light-ramp.md](./light-ramp.md)), a `sky_light_factor` of `0.0` would mean the
  End's sky exposure should contribute **nothing**, leaving every sky-only-lit
  block **pure black** regardless of the overworld's current time of day — the
  *opposite* of "pin to full daylight." (This used to say "at the shader's `0.2`
  ambient floor"; that floor was vanilla-inauthentic and is gone. The End also
  wants a non-zero `AmbientColor`, which we do not model — see light-ramp.md.)

  This is flagged rather than fixed: it would require either decoding the
  `dimension_type` registry entry (not done anywhere in this client — see
  below) or a second special-cased id match, and the correct pinned value is
  genuinely uncertain without a live-server screenshot comparison (getting the
  sign of this backwards is exactly the class of mistake `CLAUDE.md`'s
  validation log warns about). **Do not wire a guess here** — verify against
  a live End first.

### The dimension-type registry decode — landed (#288); `attributes` still is not

**This section previously said "no dimension-type registry decode at all". That is
no longer true and the correction is the point:** `registry_data` is decoded, the
`login`/`respawn` holder id is resolved against it, and `has_skylight`,
`min_y`/`height`/`logical_height`, `coordinate_scale`, `ambient_light`,
`has_ceiling`, `has_fixed_time` and `default_clock` all come off the wire now. The
chunk shape follows the registry rather than `ChunkShape::for_dimension`'s name
match. Full detail in [`registry-data-ingest.md`](./registry-data-ingest.md).

What is still **not** decoded, and is what the remaining items in this doc need:

- the dimension type's **`attributes`** map — `minecraft:visual/fog_color`,
  `visual/sky_color`, `visual/cloud_color`, `visual/cloud_height`,
  `visual/ambient_light_color`, and the audio entries. These are present in the
  captured NBT (see `tests/fixtures/registry_data_dimension_type.hex`, and the
  overworld payload transcribed in `lodestone-core`'s NBT test) and are dropped by
  the decode. **The fog presets above are still hand-written constants**, and
  wiring them to the registry is now a parse rather than a protocol change.
- `skybox` and `cardinal_light`, likewise present and dropped.
- 26.2's `skyDarken` is computed from `EnvironmentAttributes.SKY_LIGHT_LEVEL`
  (`Level.java:741`), i.e. from that same `attributes` map — not from a clock
  directly. So the End's sky-darken item above is blocked on the attributes decode,
  not on the clock work, and the live-End check it asks for is still required.

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

**This bug is fixed, and the two paragraphs above it are stale** — kept because
the diagnosis is still the best description of *how* it failed. `Inner::apply` no
longer holds the player's dimension at all: the Stage 3 vitals collapse moved it
to `lodestone_ecs::session::ServerDimension`, whose fold
(`apply_local_player_state`) handles **`Respawned` as well as `Login`** precisely
because respawn is how a portal trip is reported. Its regression gate is
`crates/lodestone-client/tests/read_model.rs`'s
`respawning_into_another_dimension_updates_the_read_model`, and
`ServerDimension`'s own doc comment names this bug as the reason.

Issue #288 then made the whole read moot for the decisions that matter:
`sky_default_for_dimension` reads `PlayerSnapshot::dimension_type`, folded from
`ClientEvent::DimensionTypeChanged`, which the adapter emits off **both** `login`
and `respawn`. So a portal trip moves the sky-light policy through a path that
never consults the level name.

## How to change it

- New dimension colour → add an sRGB hex constant plus a `FogSettings`
  constructor in `crates/lodestone-render/src/fog.rs`, converted through
  `srgb_u8_to_linear`, never hand-typed as linear (see that function's doc
  comment for why). Add the dimension-id branch in `Sim::fog_settings()`
  (`crates/lodestone-shell/src/sim.rs`) the same way the Nether/End branches
  already read — a `d.namespace() == "minecraft" && d.path() == "..."` match
  *before* the `_ =>` fallthrough, still inside the lava/water priority order.
- New dimension-conditioned render decision → read
  `PlayerSnapshot::dimension_type` (issue #288) and branch on the server's own
  field, the way `mesher.rs::sky_default_for_dimension` now does. Fall back to the
  well-known level-id match only for `dimension_type == None`, i.e. a server that
  sent no `registry_data`. `Sim::fog_settings` has **not** been converted and still
  matches on the level name — the colours it needs live in the dimension type's
  `attributes` map, which this decode drops.
- Gotcha: verify a live gate through an **actual dimension change**, not just a
  fresh login into the target dimension. Both `ServerDimension` and
  `ServerDimensionType` move on `Respawned` now, but this is the trap that made the
  original staleness invisible for a whole pass, and any *new* per-dimension fact
  added without a `Respawned` arm reproduces it exactly.
- The clear colour tracks the fog colour automatically now
  (`RenderState::set_clear_color`, called from `app.rs` with
  `desired_fog.color`) — a new dimension colour added to `fog.rs`/
  `Sim::fog_settings()` per the bullet above reaches the clear colour for free,
  with no separate wiring. Do not add a second colour source that computes the
  clear independently; it must always be fed the same `desired_fog.color`
  `app.rs` already computes for `set_fog`.

## Configuration

None. All colours/distances here are constants derived from the decompiled
26.2 data files (`.cache/mc/26.2/client-src/data/minecraft/{dimension_type,
worldgen/biome}/*.json`), not runtime-configurable.

## Dependencies

- `lodestone-render::fog` — the presets and the sRGB helper, and now
  `Sim::fog_settings()`'s direct call target for the Nether/End branches.
- `lodestone-render::world::SkyDefault` — the sky-light-default policy.
- `lodestone-client::DimensionId`/`ClientHandle::shared_handle` — the (stale,
  see above) dimension identity every consumer here reads, `Sim::fog_settings`
  included.
