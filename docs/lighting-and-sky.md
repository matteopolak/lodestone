# Lighting and sky

## What it is

The client's lighting and atmosphere model: per-corner smooth lighting and ambient occlusion on baked block models, the lightmap curve turning a light byte plus time of day into a shading factor, the day clock feed, fog, the sky pass (disc, sun, moon, stars, clouds) with per-dimension and per-biome tint, and the client's own relight engine that repairs lighting after a block edit without waiting for the server.

## How it works

### Smooth lighting and ambient occlusion

Two meshers exist and only one has the real port. `mesh_simple`/`mesh_greedy` (packed full-cube path, headless) has its own older AO. `mesh_models` (live terrain: stairs, slabs, fences, plants, every tinted or partial block) uses `quad_corner_sample` in `crates/lodestone-render/src/models.rs`.

Per vertex it averages AO (four per-cell shade samples: 1.0 open, 0.2 occluded, never 0.0) and light (the same four cells, except an occluding neighbour's value is replaced by the centre's once the centre is lit above a threshold, so a wall-pressed corner reads dim, not black).

- **Two predicates, not one.** AO shade asks a collision-shape question (full collision cube gives 0.2, with exceptions for glass, ice, mud, soul sand, snow layers); smooth light's substitution asks the rendering-occlusion predicate. Conflating them fails for leaves, slime, honey, spawners and grates (tree canopies fail to darken underneath). The AO census is the jar-dumped `lodestone_data::shade_brightness` (one bit per validated `StateId`), not a hand exception list over collision shapes, since exceptions move states in both directions. `SnapshotModelView` validates raw wire ids at the meshing boundary; an invalid id is conservatively open.
- **Light-ring centre is a per-quad fork.** A quad with a `cullface` samples the cell that face opens into. An unculled quad samples its own cell unless its plane is flush with the block boundary (or the block is a full collision cube), then samples outward. Cross-plant models satisfy neither, so they light from their own cell; getting this wrong blackens plants on one side next to a solid block.
- **AO eligibility** combines the model's `"ambientocclusion"` JSON flag (default true, from the first resolved model of a multipart state) and zero light emission (`lodestone_data::light_props`, validated `StateId` keyed). `BlockModels::ambient_occlusion` combines them; `SnapshotModelView::ambient_occlusion_at` feeds the mesher, so a light-emitting full cube takes flat lighting.
- **Directional shade** is a constant per face, not a diffuse dot product: down 0.5, up 1.0, north/south 0.8, east/west 0.6 (the Nether's darker variant is unported). AO and shade multiply in gamma space (`srgb_to_linear(linear_to_srgb(rgb) * tint * shade)`); linear space washes the result out.
- Partial model faces blend the four corner samples by in-plane extents; full unit faces use nearest-corner byte for byte. Fluids carry no AO. Gap: hidden-diagonal substitution for translucent interior faces.

### The light ramp

The lightmap curve `level / (4 - 3*level)` applies to raw sky and block levels, then the sky half scales by the time-of-day factor (`sky_darken`), the channels combine, and the result is lifted by the inverse-gamma curve at its default 0.5, always in that order. The old linear ramp (`0.2 + 0.8*level`) matched at both endpoints and ran 1.3-1.6x too bright around sky level 4-8, which is why neither a daylight nor a pitch-black gate caught it. The darkest floor is not 0: the combine seeds with the dimension's ambient colour (grey `0x0A0A0A` in the Overworld, so unlit is 0.0935 of daylight).

`SKY_LIGHT_COLOR` is a hue shift: the sky half's tint keyframes white to light blue (`NIGHT_SKY_LIGHT_COLOR = 0xFF7A7AFF`) on the same ticks as darkening, and block light has its own warm `BLOCK_LIGHT_TINT` scaled 1.4, the channels added not `max`ed. `lightmap_color`/`light_color_from_levels` in `lodestone-render` implement it, recoverable from the darkening factor alone (no new uniform lane); the scalar `lightmap_term` remains for GUI and particles. The packed full-cube path (`block.wgsl`, demo and headless gates) keeps the old simple ramp, a scoped gap.

### The day clock

`sky_darken` is all rendering needs from "what time is it", computed once from the server day clock and carried in a spare lane of the group-0 fog uniform (the model shader is at the four-bind-group floor). Terrain and entity shaders read the identical lane, or mobs and blocks disagree about the hour.

The 26.2 clock-sync packet is a map of clocks, usually empty: a full sync at join, one entry on `/time set` or a rate change, and otherwise an empty map about once a second meaning "unchanged". The client holds an anchor (`total_ticks`, `rate`, `at_game_time`) and extrapolates; rate 0.0 is paused. The day clock is resolved from the dimension type's `default_clock`, not a hardcoded holder id (a data pack can reorder, and the End's clock is not holder 0). The keyframes are `730 -> 1.0, 11270 -> 1.0, 13140 -> 0.24, 22860 -> 0.24`, linearly eased and wrapping through tick 0 (`sky_darken_for_time_of_day`).

The server sends both clocks about once a second; the shell's `ExtrapolatedServerClock` carries each at its measured rate for at most 1.5 s past the last value, so a frozen server, stopped cycle or changed tick rate paces the sun and clouds correctly. A step backwards or faster than any tick rate counts as a set and keeps the old rate.

### Fog

Two independent ramps combined with `max`:

- **Environmental** (spherical distance) from the dimension or biome `fog_start`/`fog_end`, overridden by water, lava or blindness. Presets: Nether a fixed `10..96` haze; water from the eye to `min(32, view distance)`; lava `0..3`; Overworld and End carry the registered default `0..1024`, a mild mid-field wash.
- **Render-distance** (cylindrical, `max(|xz|, |y|)`, so a valley overhead is not over-fogged). Its span is `clamp(render_distance_blocks / 10, 4, 64)` back from the edge, not a fixed fraction of view distance.

Both are always computed. The mix happens in gamma space (linear mixing pulls toward the fog colour and is worst where the factor is smallest: "too foggy too early"). The sky disc's colour mix is a separate, still-linear path, harmless there. The fog colour is a flat renderer constant (`SKY_COLOR`), not vanilla's render-distance-dependent haze-to-sky mix; fixing it touches about a dozen pixel gates that hardcode the constant.

### The sky pass

A disc, sun, 8-phase moon, stars and a camera-centred cloud plane draw in their own pass before terrain. The pass **clears to the resolved fog colour**, not black: the disc is a finite 512-block plane, so wherever nothing paints (open ocean, unmeshed chunks, beyond the far plane) the clear colour is what the player sees. Whole-pass drawing is gated on the dimension's `skybox` attribute (a colour cannot say "draw no sun"; the Nether once rendered the overworld sun); cloud opacity is gated on the cloud-colour alpha.

- **View-dependent fog colour.** `sky::atmospheric_fog_color` lerps the time-tracked fog toward the sunrise colour by how far the camera faces the sun side (only at render distance 4 or more), then toward the sky colour by `1 - lerp(min(sky fog end, render distance)/32, 0.25, 1)^0.25` (about 0.19 at distance 8, giving a noon savanna horizon of `(177, 209, 255)`, pinned by a unit test). `SkyFrame::with_atmosphere` and `RenderState::atmosphere` build the view from the same camera. Open-air only (`FogSettings::open_air`); the no-sky fallback is view independent.
- Day-clock sampling and colour tracks live in `lodestone-render/src/sky_time.rs`; `sky.rs` re-exports it and owns geometry. The moon uses `lodestone_assets::MoonPhase` resolved by `moon_phase_for_time_of_day`.
- The gradient comes from radially fogging a flat disc; sky end is the render distance in blocks, clamped (wrong clamp stretches it 4x at small distances). Void fog below the build limit is quadratic over 1.0 blocks (flat worlds) or 32.0 (otherwise); the flat case wrong either kills the fade near `y=0` or darkens a superflat surface.
- Per-biome sky tint uses the server's registry-carried biome colours (so a data pack recolour works), found by scanning downward from the eye for the nearest non-empty section.
- **Clouds:** Fast is a flat sampled plane; Fancy is a cached voxel-cell face list reaching the default 128 chunks (a 2052-block disc fading to transparent at 2048). That reach is necessary: the layer is about 120 blocks up, so a level gaze meets it past about 170 blocks. The face list is enumerated on the CPU only when the camera changes cell, uploaded as one packed `u32` per face (`CloudFace::packed`) and expanded in `sky_cloud_fancy.wgsl` (`sky::fancy_cloud_geometry` is the CPU test reference). Both scroll by world age, not time of day: offset `age mod (texture width x 400) x 0.03` blocks, so `/time set` never moves clouds. `SkyRenderer` owns the texture-dependent pipelines as optional `CloudResources`, built once `clouds.png` decodes; without it clouds skip but the disc, sun, moon and stars remain. Required celestial art fails construction.
- Air bubbles are a six-hop chain from entity metadata to the HUD, shown when the eye is underwater **or** air is below max.

### Dimension-conditioned rendering

Sky-light default for an absent neighbour while meshing follows the dimension type's `has_skylight` from server registry data (the End has real sky exposure; a level-name guess rendered it too dark). `DimensionTypeInfo::environment_attributes` retains the decoded attributes; visual fog, sky, cloud and ambient colours are lifted into typed fields that drive the render consumers, and unknown data-pack keys stay available. The End's `sky_light_factor: 0.0` travels the shared lightmap lane, where zero is distinct from the negative "not wired" sentinel.

The connected dimension comes from one accessor (`Sim::dimension`/`ServerDimension`) updated on both `Login` and `Respawned`. A server-initiated portal trip once left the rendering copy stale (too-bright Nether on traversal only); route every dimension-conditioned read through it.

### Client-side relight

A real server sends no light update for a block you break to the player who broke it, and vanilla's client survives by running the same light engine. Without a local engine every broken block on a real server leaves a pitch-black hole, since an exposed face lights from the neighbour cell it opens into and an opaque cell's stored light is 0.

`lodestone-world`'s `relight` module records old and new state ids from `World::set_block`/`set_blocks`. At drain it skips unchanged states and transitions with identical injected opacity and emission (`Relit::skipped_unchanged` and `skipped_light_equivalent` count them, not solved jobs). Explicit `World::queue_relight` calls are forced (previous state unknown). Batches compare against the pre-batch state, conservatively keeping any light-changing edit at duplicate positions; a prediction's same-state confirmation never removes an earlier light-changing edit.

- Admitted positions group by section and recompute a bounded box (the change's bounding box plus a fixed radius). The outermost shell is held as immovable sources, the interior is recomputed from zero (never from stored values, so a newly blocking cell cannot leave stale brightness), and only changed cells are written back. Sky light also spans the whole open shaft below each change, since uncapping a shaft makes every cell to the floor a full-strength source.
- Each changed cell invalidates meshes within a two-cell sampling radius (faces, edges, corners), separate from the propagation radius. Local writeback aggregates bounds before recording conservative destination keys, and the shell admits them after releasing the write lock via the same helper as packets (see [`meshing.md`](meshing.md)). Every completed write retains its invalidation bounds.
- A relight never overrides the server: `merge_light` drops any pending relight for a patched chunk, so whichever arrives second wins. In singleplayer the relight (about 8 ms) beats the server tick (about 50 ms), making the server's correction a standing cross-check.
- Cost is bounded by a per-drain cell budget, a per-job ceiling (a pathological job such as a shaft hundreds deep is dropped and counted), and a cap on pending positions. The last admitted job may exceed the remaining budget, so 320,000 is not a hard cap.
- Standalone light packets are sparse overwrites: the world store compares each named section with its current value and returns only changed indices to the adapter. An identical packet still cancels pending relight but emits no invalidation. Update `World::merge_light_changed` and the adapter together; never drop explicit-zero sections or treat an absent one as zero. Custom `WorldSink`s that cannot compare keep the conservative default.

## How to change it

- Do not derive the AO census and smooth-light occlusion from one another, nor substitute a hand collision table for the jar-dumped shade table.
- Keep the AO and `light_props` scalar APIs keyed by `StateId`; raw ids are validated at the packet or snapshot boundary. The injected `LightProperties` seam stays raw (registry independent), so adapters select their conservative fallback; add no raw lookup to `lodestone_data::light_props`.
- A fix to "the AO ring" must also consider the per-quad light-position fork.
- Light-ramp gate expectations must write the curve out by hand; importing the production function makes the gate `decode(encode(x))`.
- `sky_darken`'s negative sentinel means "never wired" and reads as full daylight everywhere. Zero is a legitimate dimension factor, so a caller that forgets to install the source renders at noon.
- Never collapse fog's environmental and render-distance ranges into one pair. Water and lava fog deliberately keep their range in the render-distance slot, because call sites compare fog settings structurally and moving it flips which term wins `max`.
- A frame-average pixel check cannot see a ramp, gradient or fog-onset defect (both hypotheses are 0 near and 1 far with similar means). Sample by screen location and print a bounding box.
- A writer bypassing `World::set_block`/`set_blocks` must call the relight queue itself. Keep relight admission matched to the solver's inputs (opacity and emission); geometry changes still need normal block-update mesh invalidation.
- Read the dimension only through `Sim::dimension`; three consumers once grew copies with different stale fallbacks.

## Configuration

- No player options for AO, the light ramp or relight; `AO_OCCLUDED`, `SMOOTH_LIGHT_MIN_CENTRE` and `BRIGHTNESS_FACTOR` are fixed constants. `BRIGHTNESS_FACTOR` is the gamma option's default; a brightness slider would use one of the uniform's two remaining free lanes.
- `Config::render_distance` is the only input to the Overworld fog ramp. Sky, fog and cloud constants come from the registry's dimension attributes and biome data.
- Relight tunables `AFFECTED_RADIUS`, `RELIGHT_CELL_BUDGET`, `RELIGHT_JOB_CEILING`, `PENDING_RELIGHT_CAP` (`lodestone-world`) and the shell's `LIGHT_DIRTY_SECTION_BUDGET` are compile-time; `RUST_LOG=light=debug` prints a per-job signed breakdown.

## Dependencies

- `lodestone_render::light`: the one Rust authority for the lightmap curve and colour combine, duplicated into `model.wgsl`/`entity.wgsl`/`fluid.wgsl` (WGSL has no include). `lodestone_render::models`/`block_models` for corner math and census accessors.
- `lodestone_data::shade_brightness` and `light_props`; `crates/versions/26.2` for `DayClock` and dimension-type decode; `lodestone_world::relight`/`LightProperties`.
- The pinned decompile under `.cache/mc/<version>/client-src` for constants and formulas (reference only).
