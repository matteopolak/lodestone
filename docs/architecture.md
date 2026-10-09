# Architecture

## What it is

The map of the project: how the crates fit together, why protocol knowledge is confined to one crate per family, and the load-bearing constraints (physics parity, memory layout, renderer budgets, the browser target) the rest of the tree is built around. Per-subsystem detail lives in the other docs.

## How it works

### Principles

1. **The library is the product.** The game is a thin shell over `lodestone-client`; anything the game can do, a bot can do headlessly.
2. **Version knowledge lives in one place per version.** Above the adapter layer nothing knows what protocol it speaks.
3. **Generated code is cheap, hand-written logic is expensive.** Duplicate the former, share the latter.
4. **Parity is proven, not asserted.** Physics, protocol and worldgen are differential-tested against real Minecraft.
5. **Probe, don't assume** (GPU features, server behaviour, library APIs).

### Crate graph

```
lodestone-macros      #[derive(Encode, Decode, Packet)]
lodestone-core        VarInt, Reader/Writer, errors, bounded reads
lodestone-nbt         NBT + SNBT, zero-copy
lodestone-text        chat components, legacy section-sign formatting
lodestone-model       THE CANONICAL MODEL: version-free events, actions,
                      BlockState handles, ItemStack, entity/world types
lodestone-data        generated per-version tables
lodestone-auth        MSA device-code OAuth, session server, profile keys

crates/versions/*     one crate per protocol family; depends only on
                      version-free shared crates
lodestone-registry    protocol number -> Box<dyn VersionAdapter>. Only crate
                      that knows which families exist. Two tables: Family
                      (can join) and ServerFamily (can host)
lodestone-net         framing, compression, AES/CFB8, transport trait
lodestone-world       chunk storage, palettes, lighting, snapshots
lodestone-physics     version-free engine + PhysicsProfile
lodestone-entity      entity state, interpolation, attributes
lodestone-client      headless client: connect, event stream, action API
lodestone-server      integrated server (singleplayer + open-to-LAN)
lodestone-worldgen    generation
lodestone-assets      resource packs, models, blockstates, atlas build
lodestone-render      wgpu renderer
lodestone-shell       the game binary (`lodestone`)
xtask                 codegen, data fetch, conformance, drift gates
```

Family folders are named for the era-start Minecraft version (`crates/versions/1.8`), not a protocol number; ask `VersionAdapter::supports`.

### Version modularity

The split rule is whether something is generated data or hand-written logic.

| Kind | Lives in |
|---|---|
| Packet structs and IDs, registries, block-state mapping, chunk/light codecs, metadata layouts, slot serialisation, adapter to the canonical model | per-version crate (deletable) |
| Physics constants + feature flags (`PhysicsProfile`), novel per-version physics hooks | per-version crate |
| Physics engine, worldgen, renderer, UI, netcode | shared, version-free |

- **Isolation.** A version crate may depend only on version-free shared crates. A version-to-version edge is allowed only through `[package.metadata.lodestone-isolation] compatibility-base = "<package>"` (a required normal path dependency). The registry opts in with `role = "version-registry"`. `cargo xtask check-isolation` enforces this.
- **Deletability.** `cargo xtask check-deletable <family>` reports a removal plan: the folder plus every transitive compatibility dependent. It is a structural scan, not a delete-and-build experiment.
- **Feature-forward trap.** A forward such as `live-v1-8 = ["lodestone-registry/v1-8"]` names the feature suffix, not a package, so it is not a dependency edge. Deleting a family leaves a dangling forward that breaks resolution, and the folder name differs from the suffix, so grepping the folder is not enough.
- **Canonical model direction.** The model follows the newest protocol; older adapters translate upward and supply defaults for newer concepts. It grows monotonically, so legacy-only concepts go behind a small extension enum owned by the version crate.
- **Family boundaries** that force a new crate: fixed-point to double positions (47 to 107), the flattening (340 to 393), light split and palettes (404 to 477), long packing (578 to 735), world height (754 to 755), biomes in sections (756 to 757), chat signing (758 to 759), configuration state (763 to 764), NBT text (764 to 765), item components (765 to 766). Smaller deltas use `#[mc(since/until)]`.
- **Cost model.** Codegen covers packet IDs and registry tables, not dispatch or wire-shape migration. A new family costs roughly 900 lines of irreducible per-family knowledge (dispatch/choreography and the chunk codec). `xtask new-version` clones a family and rewrites IDs from `packets.json`, producing the old client with new IDs; shape changes are human work, tracked in an enforced `SHAPE_REVIEW.toml`.
- **Metric.** Track hand-written lines per family; dispatch, not packet structs, is the bulk of a version crate.

### Protocol layer

```rust
pub trait Encode { fn encode(&self, w: &mut Writer, ctx: Ctx) -> Result<()>; }
pub trait Decode<'a>: Sized { fn decode(r: &mut Reader<'a>, ctx: Ctx) -> Result<Self>; }
pub trait Packet { const NAME: &'static str; const STATE: State; const BOUND: Bound; }
```

`Ctx` carries the negotiated protocol version; decoding borrows. The derive uses syn + quote with hand-rolled attribute parsing. Attributes: `varint`, `varlong`, `len(...)`, `fixed(n)`, `angle`, `nbt`, `json`, `uuid_int_array`, `remaining`, `when(expr)`, `tag(varint)`, `bounded(max)`, `since`/`until`.

- **Packet IDs are never hand-written.** They are generated from `packets.json`, keyed by the stable `minecraft:` name.
- **Structural context.** Chunk sub-structures need parameters from the dimension registry (`PaletteKind`, world height, section count). `#[mc(decode_context = "T")]` and `#[mc(decode_with = "path")]` cover that. It is deliberately not extended to `Vec<T>` elements: only chunk data and light update need it, so that loop stays hand-written.

### Physics

The base integrator is about 90% version-stable: gravity 0.08, jump power 0.42, air drag 0.91/0.98, input friction 0.98, sprint constant 0.21600002 are unchanged since 1.8. What varies is which mechanisms exist (elytra 1.9, swimming 1.13, soul speed 1.16, powder snow 1.17, attribute-driven air drag in 26.x). So numbers are shared and the mechanism set is versioned: `lodestone-physics` takes a `PhysicsProfile` (constants, capability bitflags, a `PhysicsHooks` escape hatch).

**Bit-exact parity** requirements:

- Vanilla's sine is a 65536-entry `float` lookup table, not `f32::sin`. The table is checked in and a unit test asserts its FNV-1a hash `3563566116167745249` (matching the JVM on all entries). Use `lodestone_physics::mth`; the standard library diverges at the poles, where fixture inputs sit.
- No FP contraction; Java `double` to `long` truncation (Rust `as i64` matches); collision sweep ordering; step-up and sneak edge-backoff.
- Correctness comes from per-tick golden traces captured from real sessions.

### World storage and memory

A 1.18+ column is 24 sections of 4096 blocks. Naive `u16` storage is 196 KB per column, about 830 MB at render distance 32. The layout rules:

1. **Never allocate per block.** A block state is a `u32` id; behaviour lives in tables indexed by id.
2. **Paletted containers.** Per-section palette plus bit-packed indices; homogeneous sections store a single value and no array. Measured: flat column 6,864 B, realistic terrain 19,264 B, so 77.6 MiB at RD32.
3. **Palette thresholds** (not scaled copies of each other):
   - Block states (4 bits per axis): 0 entries is single-value; 1-4 is a 4-bit linear palette; 5-8 is a hashmap palette at that width; more than 8 is direct (`ceilLog2(registrySize)`, about 15).
   - Biomes (2 bits per axis): 0 is single; 1/2/3 is linear at that width; more than 3 is direct. No floor clamp.
   - Entries never straddle an `i64` (`valuesPerLong = 64/bits`, low bits first). Index order is YZX: `(y << b | z) << b | x`.
4. **Long-array framing is version-specific.** Protocols 770 and later write the packed array with no VarInt length prefix; 769 and earlier prefix it. This is `LongArrayFraming::{Prefixed, FixedSize}` on the container profile, never a hardcoded default. Heightmaps switch at the same boundary (NBT compound up to 1.21.4, typed long-array list from 1.21.5).
5. **Light is the real memory hog.** 2048 B per section per light type across 26 light sections is about 106 KB per column naively, 396 MiB at RD32. `LightData::{Missing, Uniform(u8), Values}` makes a uniform section one byte: measured 9,024 B per column, 36.4 MiB at RD32. Vanilla elides only all-zero light on the wire, so uniform-15 sky light is still sent in full.
6. **Slab recycling.** Bits per entry in {1,2,3,4,5,6,7,8,15} over 4096 entries gives a few size classes that chunk streaming churns constantly; use a size-classed free pool.
7. **Keep the system allocator.** `lodestone-allocbench` against the macOS baseline: mimalloc 94% throughput at 130% RSS, snmalloc 79%/104%, jemalloc 113%/111%; none is both faster and leaner, and each adds a C/C++ toolchain. Pitfalls: cross-thread free inverts the ranking, and `vec![0u8; n]` uses `alloc_zeroed`, which fakes a 4x win.

Library crates must never set `#[global_allocator]`; that is the game binary's decision, behind features.

### Renderer

Vanilla's bottleneck is CPU-side per-section draw submission. Design priorities: compact vertex format, region buffer packing, async meshing over copy-on-write snapshots, greedy meshing, GPU frustum culling, texture arrays, Hi-Z occlusion.

- **No multi-draw indirect.** On both targets (Metal, WebGPU) it is a CPU-emulated loop, so it saves zero draw calls. `PerDraw` is the default because it submits only visible regions. `MULTI_DRAW_INDIRECT_COUNT` is the only public signal of native support.
- **Vertex format.** Packed cube vertices are two `u32`:
  ```
  word0: x[0:6] y[6:12] z[12:18] normal[18:21] ao[21:23] sky[23:27] block[27:31]
  word1: sprite[0:11] u[11:16] v[16:21]
  ```
  Fractional four-sample AO does not fit the 2-bit field, so the shipped packed vertex is 12 bytes (about 4x smaller than a naive 48-byte layout). Non-cube geometry uses a wider float `ModelVertex`. The packed path applies only where a predicate derived from the baked model recognises an exact full opaque cube; never use a hardcoded block list.
- **The fast path is small.** Of 32,366 baked 26.2 states, 30,989 render and only 2,874 (9.3%) are full cubes. Grass (tinted top) and water are not packed cubes.
- **Section visibility.** Union-find per section records which of the 15 face pairs connect; a BFS from the camera gated by `connects(entry, exit)` and never reversing along an axis, composed with the frustum test, stops the underground being drawn. Sections with fewer than 256 opaque blocks are skipped exactly, since the min-cut of a 16-cubed grid is 256.
- **Meshing neighbourhood is 27 sections, not 6.** AO samples cells across section edges and corners; six neighbours give correct culling and subtly wrong AO at boundaries. Missing neighbours read as empty.
- **Air must carry light.** A face samples light from the neighbour cell it faces, usually air. An unlit empty cell is plausible but wrong and renders terrain at 0.2x brightness while geometry tests pass; only a GPU pixel readback shows it.
- **Deliberate divergences.** Vanilla does no greedy meshing (ours, full-cube faces only). Smooth lighting averages four samples per corner (two edge sides, the diagonal, and the centre) as continuous floats; the integer `3-(s1+s2+corner)` shape is wrong. Translucency sorting is a known gap.

#### Hard renderer constraints

- **4 bind groups.** The model shader uses all four of wgpu's default `max_bind_groups` (camera, atlas, palette, anim); fog lives in the camera uniform. A fifth group validates on hardware reporting 8 and crashes on 4-group adapters. Probe limits at runtime; do not trust documented backend support.
- **Depth is reversed-Z in `[0,1]`.** Near is `1`, far is `0`, depth clears to `lodestone_render::DEPTH_CLEAR` (`0.0`), "nearer" is greater-or-equal, and a bias toward the eye is positive. Reversed-Z spends float precision where it is needed (see [camera-and-view](./camera-and-view.md)); preserve the convention in comparisons and biases.
- **GUI winding is negative.** `sign(det(gui_ortho * gui_item_pose))` must equal `sign(det(Camera::view_projection()))`, which is negative with glam's DirectX RH perspective. Derive the sign from a real camera.
- **Tint and shade multiply in gamma space:** `srgb_to_linear(linear_to_srgb(rgb) * tint * shade)`. Doing it in linear washes the image out, most visibly against dark backgrounds.
- **Browser surface format.** `Surface::get_default_config` picks `formats[0]`: sRGB natively, never sRGB on the WebGPU backend, so a browser image comes out dark. Fix with an sRGB view over the swapchain (`config.view_formats` plus an explicit view format per frame). Nothing in the source looks `cfg`-conditional.
- **`LineList` is one physical pixel**, invisible on HiDPI. Use a screen-space ribbon like `OutlineRenderer`.
- **Re-attach borrowed GPU resources.** A pass holding a bind group on another renderer's atlas or buffer stays valid after that resource is replaced and keeps sampling the dropped one. Resource-pack reload replaces the model atlas view, tint palette and animation buffer; no gate catches a stale borrow because gates build once. Add the re-attach in the same change as the borrow.
- **Blend bytes are not predictable.** On Metal with an sRGB target the effective blend alpha is a repeatable non-trivial function of the fragment alpha byte. Predict exact values only for full alpha and bracket the rest, with one assertion that fails under the wrong pipeline.

### Assets and resource packs

The asset layer reads the game's on-disk pack format natively. The built-in archive ([built-in-resource-pack.md](./built-in-resource-pack.md)) is the bottom pack, with user and server packs above. The loader is version-free; conventions (`textures/blocks/` up to 1.12 vs `textures/block/`, `pack.mcmeta` format numbers, multipart, `atlases/`) come from a per-version asset profile.

Measured facts about 26.2 assets:

- `client.jar` has no root `pack.mcmeta`, and the loader must accept that; pack metadata comes from `version.json` (resource pack format 88).
- Block PNGs come in palette (1,076 of 1,269), RGBA, RGB, grey and grey+alpha forms at bit depths 1/2/4/8, so palette plus `tRNS` and sub-byte depths are mandatory. Most are 16x16 or 16xN animation strips.
- Element rotation has two shapes (`{axis, angle, origin, rescale}` and a Euler `{x, y, z, origin}` beyond the old +-45 limit); texture values may be strings or objects (`{"sprite": ..., "force_translucent": true}`).
- Item models are `assets/minecraft/items/*.json`; `builtin/*` parents have no file and are terminal sentinels.

Design points:

- **Atlas, not texture array.** The reason is mips, not VRAM. The block atlas needs 1,233 sprites before about 2,600 animation frames, beyond a 2048-layer limit (WebGPU guarantees 256). Mips are generated per sprite with clamped sampling, and sprites need a reserved gutter extruded at every mip level because bilinear taps at a sprite edge still reach the neighbour.
- **Mip count** is `min over sprites of max_mip_level`, matching how vanilla drops mips for the whole atlas.
- **Animated sprites** keep every frame as its own region in the immutable atlas; the shader blends N and N+1. No re-upload.
- **`BakedQuad.layer` is the atlas layer, not the render layer.** Translucency classification is a renderer concern.
- **Entity models are code-only.** No data exposes mesh geometry. The primitive (`CubeDef`/`PartPose`/`PartDef` to `bake_entity`) is in `lodestone-assets`; per-mob data is in the version crate.
- **Determinism.** Sprite order is sorted by location and faces iterate in a fixed order, so a pack yields byte-identical atlas bytes, UVs and quads.

Block path:

```
block state id (u32, chunk packet)
  -> [version crate]  name + properties (behind BlockStateRegistry in lodestone-model)
  -> [assets]         variant / multipart evaluation
  -> [assets]         ResolvedModel (parents flattened, #variables substituted)
  -> [assets]         baked quads
  -> [render]         chunk mesh
```

The renderer consumes only baked output, so the asset layer tests without a GPU.

### Client, singleplayer, programmability

- Singleplayer is the integrated server over an in-memory transport implementing the same `Connection` trait as TCP, so both paths share code and open-to-LAN falls out.
- World and entity state are `bevy_ecs` (standalone); the renderer is a separate crate observing the same world.
- `lodestone-client` exposes async connect, a typed event stream and an action API.
- Scripting is a capability-based WASM plugin host; see [`plugin-api.md`](./plugin-api.md).

### Browser target

Browsers cannot open raw TCP, so a browser build needs a WebSocket-to-TCP relay. The relay is protocol-blind (`Codec` is byte-transparent framing), so one relay serves every version; once it parses packets it becomes per-version. Singleplayer needs none.

- `Codec` is a sans-IO state machine, `Transport` is a marker trait, and `connect_with<T: Transport>` is the injection seam. Keep that seam: nothing in the server or client may assume `TcpStream`.
- **A green wasm compile says little.** `std::fs::*` returns `Err(Unsupported)` (degrades), while `Instant::now`, `SystemTime::now`, `thread::spawn` and `thread::scope` trap. `Builder::spawn` and `available_parallelism` return `Err`; classify the call site, since `.expect()` makes a degrading call fatal. `thread::scope` traps because `Scope::spawn` hits an internal `.expect()`.
- `scripts/wasm-check.sh` (and `cargo xtask wasm-check`) bans clock paths across the crates the browser links. It only covers the crates it names; the browser reaches about fifteen.

See [`browser-shell-port.md`](./browser-shell-port.md).

### Testing strategy

| Layer | Method |
|---|---|
| Packets | proptest round-trip plus a replay corpus of proxy-recorded sessions |
| Packet IDs | conformance against `packets.json` + minecraft-data |
| Physics | bit-exact golden traces from real sessions |
| Worldgen | block-for-block comparison with server-generated chunks |
| Renderer | headless wgpu gates that read back pixels |
| Integration | real vanilla servers under Apple `container`, scripted scenarios |
| Isolation | `check-isolation` / `check-deletable` in CI |

A suite that mocks what it integrates with can pass while wrong, and test counts measure depth, not connectedness. Track the seam as a ratio with `cargo xtask connectedness`.

### Legal

- No Mojang assets are redistributed; the client downloads them with the user's own account, and decompiled output stays `.gitignore`d.
- Decompiled source is a behavioural reference only; implementations are original and proven by differential testing.
- GPL/AGPL prior art (Sodium-family renderers, ViaVersion, azalea) informs design only.

## Dependencies

`wgpu`, `glam` (DirectX RH projection, `[0,1]` depth), `bevy_ecs`, `tokio` (only `net` and `rt-multi-thread` fail on wasm), `syn`/`quote`, `bumpalo` (meshing arenas), `trunk` (browser build).
