# Architecture

## What it is

The shape of the whole project: how the crates fit together, why version knowledge is confined to one
crate per protocol family, and the load-bearing constraints — physics parity, memory layout, renderer
budgets, the browser target — that the rest of the tree is built around. Per-subsystem detail lives in
the other docs; this is the map and the reasoning behind it.

## Guiding principles

1. **The library is the product.** The playable game is a thin shell over `lodestone-client`. Anything
   the game can do, a bot can do headlessly.
2. **Version knowledge lives in exactly one place per version.** Above the adapter layer, no code knows
   what protocol it is speaking.
3. **Generated code is cheap; hand-written logic is expensive.** Duplicate the former freely, share the
   latter carefully. This is the organising rule for the version split below.
4. **Parity is proven, not asserted.** Physics and protocol correctness are validated by differential
   testing against real Minecraft, not by reading code and hoping.
5. **Probe, don't assume** — for GPU features, server behaviour, and library APIs alike.

## Crate graph

```
lodestone-macros      proc-macros: #[derive(Encode, Decode, Packet)]
lodestone-core        VarInt, Reader/Writer, error types, bounded reads
lodestone-nbt         NBT + SNBT, zero-copy
lodestone-text        chat components (JSON + NBT forms), legacy § formatting
lodestone-model       THE CANONICAL MODEL. Version-free: events, actions,
                      BlockState handles, ItemStack, entity/world types
lodestone-data        generated per-version tables (collision shapes, hardness,
                      blast/flammability, registries)
lodestone-auth        MSA device-code OAuth, session server, profile keys

crates/protocol/{v47, v340, v735, v770}
                      one crate per protocol family, named for its LOWEST
                      protocol number. Fully self-contained; depends ONLY on
                      version-free shared crates.

lodestone-registry    protocol number -> Box<dyn VersionAdapter>. The only
                      crate that knows which families exist. Two tables:
                      Family (can join) and ServerFamily (can host).
lodestone-net         framing, compression, AES/CFB8, transport trait
lodestone-world       chunk storage, palettes, lighting, snapshots
lodestone-physics     version-free engine + PhysicsProfile
lodestone-entity      entity state, interpolation, attributes
lodestone-client      headless client: connect, event stream, action API
lodestone-server      integrated server (singleplayer + open-to-LAN)
lodestone-worldgen    vanilla-parity generation
lodestone-assets      resource packs, models, blockstates, atlas build
lodestone-render      wgpu renderer
lodestone-shell       the game binary (bin name: `lodestone`)
xtask                 codegen, data fetch, conformance, drift gates
```

## Version modularity

The split rule is **"is it generated data, or is it hand-written logic?"**

| Kind | Where it lives | Deletable? |
|---|---|---|
| Packet structs and IDs | per-version crate | yes, entirely |
| Block/item/entity/particle/sound registries | per-version crate | yes |
| Block-state ID ↔ canonical mapping | per-version crate | yes |
| Chunk and light wire codecs | per-version crate | yes |
| Entity metadata layouts | per-version crate | yes |
| Item/slot serialization | per-version crate | yes |
| Adapter to the canonical model | per-version crate | yes |
| Physics **engine** | shared, version-free | nothing to delete |
| Physics **constants + feature flags** | per-version crate (`PhysicsProfile`) | yes |
| Novel per-version physics logic | per-version crate (hook impl) | yes |
| Worldgen, renderer, UI, netcode | shared, version-free | n/a |

**Isolation rule: a version crate may depend only on version-free shared crates, never on another
version crate.** `cargo xtask check-isolation` derives "is a version crate" structurally from
`crates/protocol/` membership rather than from an allowlist. The one intended aggregation point is
`lodestone-registry`, which opts in via `[package.metadata.lodestone-isolation] role =
"version-registry"` — a structural marker, not a name match, and one that can only downgrade an
*already-optional* edge, so a required registry→version edge or any version→version edge still fails.

**Deletability is measured, not asserted**: `cargo xtask check-deletable <family>` simulates removal and
reports the true fallout in manifest and source lines.

One trap that a graph-plus-package-name check structurally cannot see: a feature forward such as
`live-v47 = ["lodestone-registry/v47"]` names the folder *token*, not the package, so it is not a
dependency edge at all. Cargo validates feature strings at resolve time, so deleting a family leaves a
dangling feature forward that breaks the default build while the checker reports "unaffected". Token
matching is bounded so `v47` never matches `v470`.

**Canonical model direction.** The model is shaped by the *newest* protocol's concepts; older adapters
translate **upward** (the ViaVersion insight). Deleting an old version removes only its adapter; adding
a new one that introduces a concept means extending the model once and letting older adapters supply a
default. Client, UI and render code never see a version number.

Where isolation is imperfect, honestly: the canonical model itself grows monotonically, so a
legacy-only concept lingers after its family is deleted (mitigation: legacy-only concepts go behind a
small extension enum owned by the version crate). Physics cannot be duplicated per version — it is
subtle hand-written code where N copies means fixing every bug N times. Assets vary by version too, and
are handled by a per-version asset profile, the same shape as `PhysicsProfile`.

**Family boundaries.** 47→776 collapses into 17 families. The hardest boundaries, each of which forces
a new crate: 47→107 (fixed-point→double, metadata rewrite), 340→393 (flattening), 404→477 (light split,
palettes), 578→735 (long packing), 754→755 (world height), 756→757 (biomes in sections), 758→759 (chat
signing), 763→764 (configuration state), 764→765 (NBT text), 765→766 (item components). Within a family,
small deltas use `#[mc(since/until)]` predicates.

**Scope: the architecture supports seventeen families; the schedule funds four** — `v770` (26.2),
`v735` (1.16.5), `v340` (1.12.2), `v47` (1.8.9). Modern, mid and legacy, enough to exercise every hard
boundary above and to prove the folder-deletion property. `xtask new-version` plus an enforced
`SHAPE_REVIEW.toml` remains the documented path for adding more.

That number was chosen from two measurements. Codegen covers packet IDs and registry tables — the cheap
part — and covers **neither dispatch nor wire-shape migration**, which are the bulk *and* the risk: ID
routing is mechanical, but lowering and raising to `ClientEvent`/`ClientAction`, world side effects,
registry lookups, teleport replies and chunk-shape state are semantic per-version work, and
`new-version` cloning one family into another produces a correct, mechanical result that is the old
client wearing the new family's packet IDs. Measured on a real migration, though, the per-family cost is
**~900 lines of genuinely irreducible knowledge**, not the 2.3–5.1k a naive hand-written-lines count
suggests: of 3,007 "hand-written" lines, 997 are prose, 181 blank and ~515 mechanical derive
declarations, leaving dispatch/choreography (~712) and the chunk codec (~191) as the real content. A
fifth family is a day's work rather than a project.

**The risk this design carries, stated plainly:** per-version duplication is exactly the pattern that
limited MCProtocolLib to a handful of versions. It works here only because the duplication is
*generated* — `xtask new-version` clones a family crate and rewrites IDs from Mojang's authoritative
`packets.json`, and human effort goes only into packets whose *shape* changed. If codegen coverage ever
weakens, every new version degrades into hand-editing N near-identical crates. The useful metric is
**hand-written lines per family**, not a derive percentage: the per-struct ratio reads 84–92% and is
structurally blind to the fact that dispatch logic, not packet structs, is the bulk of a version crate.

## Protocol layer

```rust
pub trait Encode { fn encode(&self, w: &mut Writer, ctx: Ctx) -> Result<()>; }
pub trait Decode<'a>: Sized { fn decode(r: &mut Reader<'a>, ctx: Ctx) -> Result<Self>; }
pub trait Packet { const NAME: &'static str; const STATE: State; const BOUND: Bound; }
```

`Ctx` carries the negotiated protocol version; borrowed decoding avoids copying large payloads. The
derive macro is built on syn + quote with hand-rolled attribute parsing (darling pins syn 2). Attribute
surface: `varint` · `varlong` · `len(...)` · `fixed(n)` · `angle` · `nbt` · `json` · `uuid_int_array` ·
`remaining` · `when(expr)` · `tag(varint)` · `bounded(max)` · `since`/`until`.

**Packet IDs are never written by hand** — they come from Mojang's `packets.json`, keyed by the stable
`minecraft:` name, generated per version, so a version's ID shuffle costs a regeneration.

Some chunk sub-structures need *structural* parameters that come from the dimension registry rather than
the protocol version (`PalettedContainer` needs a `PaletteKind`, `Heightmaps` a world height,
`ColumnLight` a section count). `#[mc(decode_context = "T")]` and `#[mc(decode_with = "path")]` cover
that. The mechanism is deliberately **not** extended to `Vec<T>` whose element decode needs context:
across every family only chunk data and light update need it, and machinery for two packets per family
costs more than it saves. That loop stays a hand-written function the derive calls.

## Physics

Measured against decompiled 26.2, the base integrator is ~90% version-stable — `DEFAULT_BASE_GRAVITY`
0.08, `BASE_JUMP_POWER` 0.42, horizontal/vertical air drag 0.91/0.98, input friction 0.98, sprint
constant 0.21600002 — all unchanged since 1.8. What changes across versions is *which mechanisms exist*
(elytra 1.9, swimming 1.13, soul speed 1.16, powder snow 1.17, attribute-driven air drag in 26.x). So
**the numbers are shared; the mechanism set is versioned**, and `lodestone-physics` takes a
`PhysicsProfile` (constants + capability bitflags + a `PhysicsHooks` escape hatch) supplied by the
version crate.

**Bit-exact parity.** Vanilla's own sine helper is a 65536-entry `float` LUT, not `f32::sin`, and everything
downstream of it (movement vectors, rotation) depends on the exact values. Rust reproduces the table
bit-exactly (FNV-1a `3563566116167745249`, matching the JVM on all 65,536 entries). The table is checked
into the repo and a unit test asserts the runtime-computed hash matches, so parity never depends on
libm agreement across platforms. Substituting the standard library diverges exactly at the poles, which
is where "obvious" fixture inputs sit — use `lodestone_physics::mth`.

Other parity requirements: no FP contraction, Java `double→long` truncation semantics (Rust `as i64`
matches), collision sweep ordering, and vanilla's step-up / sneak edge-backoff logic. Correctness is
proven by golden traces captured per-tick from real sessions rather than by transliterating source.

## World storage and memory

The number that drives everything: a 1.18+ chunk column is 24 sections × 4096 blocks = 98,304 blocks.
Stored naively as `u16` that is 196 KB/column, or ~830 MB at render distance 32. No allocator rescues a
layout that wrong.

1. **Never allocate per block.** A block state is a `u32` id; behaviour lives in tables indexed by id.
2. **Paletted containers.** Per-section palette + bit-packed indices, sized to the palette; homogeneous
   sections collapse to a single-value palette storing no index array at all. Measured: flat-world
   column 6,864 B, realistic terrain column 19,264 B → **77.6 MiB at RD32**. Full-entropy worst case is
   the naive size, correctly — random data is incompressible.
3. **Palette thresholds**, read from 26.2's own strategy table and *not* a scaled copy of each other:
   - *Block states* (bitsPerAxis 4): 0 → single-value; 1–4 → clamped up to a 4-bit linear palette; 5–8 →
     hashmap palette at that width; >8 → direct (`ceilLog2(registrySize)`, ≈15).
   - *Biomes* (bitsPerAxis 2, 64 entries): 0 → single; 1/2/3 → linear at that width; >3 → direct. **No
     floor clamp, ceiling of 3.**
   - Entries never straddle an `i64`: `valuesPerLong = 64/bits`, low bits first, leftover high bits pad.
   - Index order is **YZX**: `(y << b | z) << b | x`.
4. **Version-specific framing, and it is a trap.** In 26.2 the packed long array is written with no
   VarInt length prefix (the count is derived from bits × entry count); older protocols do prefix it.
   The bit-packing, thresholds and indexing are structural and shared, so this is a
   `LongArrayFraming::{Prefixed, FixedSize}` knob on the container profile — never a hardcoded modern
   default in the version-free crate. The boundary is **≤769 → `Prefixed`, ≥770 → `FixedSize`**, and
   `v770` sits exactly on it. Heightmaps switch at the same boundary (NBT compound ≤1.21.4 vs typed
   long-array list ≥1.21.5).
5. **Light is the real memory hog, not block states.** 4096 nibbles = 2048 B per section per light type
   × 2 × 26 light sections (light extends one section past the build range, top and bottom) = ~106 KB
   per column naively, ~396 MiB at RD32 — five times a realistic column's block data. Elision is a
   requirement, not an optimisation: measured 9,024 B/column → **36.4 MiB at RD32**. `LightData::
   {Missing, Uniform(u8), Values}` makes a uniform section cost one byte. Wire/storage asymmetry worth
   knowing: vanilla only elides all-*zero* light on the wire, so a uniform-15 sky section is still
   transmitted as a full array.
6. **Slab recycling.** Bits-per-entry ∈ {1,2,3,4,5,6,7,8,15} over a fixed 4096 entries yields a small
   fixed set of size classes, and chunk streaming churns them constantly — the pattern a size-classed
   free pool handles best.
7. **Global allocator: keep the system allocator.** Benchmarked in `lodestone-allocbench` — mimalloc
   94% throughput at 130% RSS, snmalloc 79%/104%, jemalloc 113%/111% against the macOS baseline. No
   candidate is both faster and leaner, and each costs a C/C++ toolchain dependency. Two findings worth
   keeping: cross-thread free *inverts* the ranking (snmalloc is the only allocator that gets faster
   under it), so benchmarking with same-thread free — the obvious thing to write — produces the opposite
   conclusion; and `vec![0u8; n]` routes to `alloc_zeroed`, letting an allocator skip the memset on
   fresh OS pages and showing a bogus 4× win.

**Library crates must never set `#[global_allocator]`** — that is an application-level decision, made in
the game binary behind features.

## Renderer

Vanilla's bottleneck is CPU-side per-section draw submission. The design, in ROI order: compact vertex
format, region-based buffer packing, async meshing over copy-on-write snapshots, binary greedy meshing,
GPU frustum culling, texture arrays, Hi-Z occlusion culling.

**Multi-draw indirect is not available to us.** It is CPU-emulated as a `for` loop on *both* Metal and
WebGPU, our only two targets, so it reduces draw calls by exactly zero. `PerDraw` is the correct default
on both, because it submits only *visible* regions where the emulated multi-draw loop submits every
region including culled ones. `MULTI_DRAW_INDIRECT_COUNT` is the sole public signal distinguishing
native multi-draw from emulation. Region-based buffer *packing* is still valuable; the draw-call win is
not.

**Vertex format — 8 bytes/vertex** (two `u32`), 6× smaller than a naive 48-byte layout:

```
word0:  x[0:6] y[6:12] z[12:18] normal[18:21] ao[21:23] sky[23:27] block[27:31]
word1:  sprite[0:11] u[11:16] v[16:21]           (u, v in tile units)
```

At RD32 this is 667 MiB packed against 2,574 MiB naive for 12.5 M quads. Packed positions are exact for
cube corners and too coarse for baked models on a 1/16 grid, so non-cube geometry uses a wider float
`ModelVertex`; the packed path survives only where a predicate can recognise "exactly a full opaque
cube" *from the baked model*, and that predicate must be derived, never a hardcoded block list.

Two measurements bound how much that fast path is worth. Baking all 32,366 v770 states: 1,377 empty,
30,989 renderable, of which only **2,874 (9.3%) are full-cube geometry** and 2,622 packed-eligible
untinted cubes — and the two dominant overworld surfaces, grass (tinted top) and water (a fluid), are
*not* packed cubes, so "the fast path carries most surfaces" is false. Separately, adopting vanilla's
real four-sample float AO forced the packed vertex from 8 to 12 bytes (fractional AO does not fit a
2-bit field), dropping the win over the naive baseline from 6× to 4×. Correctness moved the number.

**Section visibility** flood-fills each section with union-find to record which of the 15 face-pairs are
mutually connected, then BFS-walks the section graph from the camera gated by `connects(entry, exit)`,
never reversing along an axis, composed with the frustum test. Frustum culling alone cannot stop the
entire underground being submitted while standing on the surface. Vanilla's `<256` sparse-section
shortcut is **exact, not merely conservative**: the min-cut of a 16³ grid is 256 cells, so fewer than
256 opaque blocks cannot disconnect opposite faces.

**Meshing neighbourhood is 3×3×3 = 27 sections, not 6.** Face culling alone needs the six face-adjacent
sections, but ambient occlusion samples the three cells around each vertex corner, which reach across
section edges and corners. Meshing with six neighbours yields correct culling and subtly wrong AO along
every boundary — much harder to spot than missing faces. Missing neighbours read as empty.

**Air must carry light, or every block face renders black.** A face samples lighting from the
*neighbour* cell it faces into, which for an exposed surface is air; returning an unlit `Cell::EMPTY` is
a valid-looking value that happens to be the wrong one, so every geometry unit test passes and terrain
renders at 0.2× brightness. This was only diagnosable because a GPU test read back actual pixels.

**Deliberate divergences from vanilla.** Vanilla does no greedy meshing at all — it emits model quads
and relies on face culling plus layers; greedy is *our* optimisation and is valid only for full-cube
faces, so "vanilla parity" and "greedy meshing" are independent axes. Smooth lighting genuinely
diverges: vanilla averages four samples per corner (two edge sides, the diagonal corner, *and the
centre*) as continuous floats, blending skylight and blocklight identically, then interpolates corner
values across non-cube faces by face-shape weights; the classic integer `3−(s1+s2+corner)` gets the
shape of concave-corner darkening right and the values wrong. Translucency sorting is a real gap.

### Hard renderer constraints

- **The model shader is at wgpu's 4-bind-group floor.** wgpu's default `max_bind_groups` is 4 and the
  shader already spends all four (camera / atlas / palette / anim). A 5-group shader validates on an M5
  (which reports 8) and **fails on any 4-group adapter** — a startup crash for other people and never
  for us. Fog was folded into the group-0 camera uniform for this reason. Check the limit, not the
  adapter. This generalises: always probe capabilities at runtime, never branch on documented backend
  support (published guidance says `PARTIALLY_BOUND_BINDING_ARRAY` is unsupported on Metal; it is not).
- **Depth is `[0,1]` DirectX-style, not vanilla's reversed-Z.** Every ported depth comparison and bias
  flips sign: vanilla's `GREATER_THAN_OR_EQUAL` is our `LessEqual`, and a positive vanilla depth bias is
  negative here. The sign flip is the easy half — **precision does not survive the change**, so a ported
  sub-millimetre depth separation is unresolvable at ordinary range. Reversed-Z is not stylistic on
  vanilla's part; it is the arrangement that spends float exponent where the depth range needs it, and
  it is *why* vanilla's constants are the size they are. Prefer a mechanism that does not depend on
  depth over tuning a bias until an artefact goes away at one distance.
- **The GUI winding invariant is negative, not positive.** `sign(det(gui_ortho * gui_item_pose))` must
  *equal* `sign(det(Camera::view_projection()))`, and that sign is negative because glam's DirectX RH
  perspective is itself negative. Derive the front-facing sign from a real camera; do not assert a
  polarity, or you ship an inside-out block that still looks plausibly isometric.
- **Vanilla is not colour-managed.** Tint *and* shade multiply in **gamma** space
  (`srgb_to_linear(linear_to_srgb(rgb) * tint * shade)`). Doing it in linear pulls every shade factor
  toward 1.0 and washes the image out. A gamma/linear blend mismatch has a measurable signature: the
  divergence is large against a dark background and ~0 near white, because black and white are the fixed
  points where the two spaces agree.
- **`Surface::get_default_config` is correct natively and wrong in a browser.** It takes
  `get_capabilities().formats[0]`; native `wgpu-core` sorts sRGB formats first, while the WebGPU backend
  never lists an sRGB format at all — a browser canvas structurally cannot be configured with one, and
  linear shader output then reaches the compositor with no EOTF applied, so the whole image comes out
  dark. The fix is an sRGB *view* over the swapchain (`config.view_formats` plus an explicit format on
  every acquired frame's view), with the target reporting that view format. Note this is a `cfg`-free
  way to get different behaviour per target: nothing in the source looks conditional.
- **`PrimitiveTopology::LineList` rasterises at exactly one *physical* pixel**, so on a HiDPI display it
  is effectively invisible and presents as "the feature does nothing". Use a screen-space ribbon, as
  `OutlineRenderer` does.
- **A GPU pass that borrows another renderer's atlas or buffer must be re-attached wherever that
  resource is replaced.** wgpu resources are `Arc`-backed and a bind group holds a strong reference, so
  the pass stays *valid* and keeps sampling the *dropped* atlas while drawing geometry baked against the
  new packing. The resource-pack reload path replaces the model atlas view, the tint palette and the
  animation buffer; no hermetic gate can see a stale borrow, because every gate builds its renderer once
  and never reloads. Add the re-attach in the same commit as the borrow.
- **You cannot predict an exact composited byte through `ALPHA_BLENDING` on this backend.** On Metal
  with an sRGB target the effective blend alpha is a real, repeatable, non-trivial function of the raw
  fragment alpha byte — not the identity, not `linear_to_srgb(a)`, not any single power law. Predict
  exactly what you can (full alpha: submission order alone decides the winner, byte-identical) and
  bracket the rest, including at least one assertion that fails under the wrong pipeline.

## Assets and resource packs

**Requirement: full compatibility with vanilla resource packs.** The asset layer speaks Mojang's on-disk
format natively, and vanilla's own assets are simply the bottom-most pack in the stack — so "use the
real textures" and "use a custom pack" are one code path. Assets are **downloaded, never vendored**:
`xtask fetch-assets` pulls `client.jar` plus the asset index into `.cache/`, exactly as a launcher does.

Per the version-split rule the **loader is version-free** and the **conventions come from the version
crate** as an asset profile: `textures/blocks/` (≤1.12) vs `textures/block/` (1.13+), `pack.mcmeta`
format numbers, and model/blockstate JSON features that arrived over time (multipart, `atlases/`).

Measured facts about 26.2's own assets that contradict the obvious assumptions:

- **`client.jar` has no root `pack.mcmeta`.** Vanilla builds its built-in pack programmatically, so the
  loader must treat its absence as valid, not an error. Pack metadata comes from `version.json`, whose
  `pack_version` is now major/minor pairs — **the resource pack format for 26.2 is 88**.
- **Textures are not all RGBA8.** Of 1,269 block PNGs: palette 1,076, RGBA 116, RGB 37, grey+alpha 21,
  grey 19, at bit depths 1/2/4/8. A decoder written for "vanilla is RGBA8" fails on the *majority* of
  the jar; palette + `tRNS` and sub-byte depths are mandatory. 1,175 are exactly 16×16, most of the rest
  are 16×N vertical animation strips, and only ~42 are genuinely wider.
- **Element rotation has two shapes**, not one: the classic `{axis, angle, origin, rescale}` and a Euler
  `{x, y, z, origin}` triple whose angles exceed the old ±45 limit. Normalise both.
- **Texture values have an object form** in 26.2 — `{"sprite": ..., "force_translucent": true}` as well
  as a bare string. Missing it cost 1,857 bake failures concentrated on every translucent block.
- **Item models use `assets/minecraft/items/*.json`**, so `builtin/entity` no longer appears under
  `models/item/`; `builtin/*` parents have no JSON file and must be terminal sentinels, not errors.

**Atlas vs texture array was never a capacity decision — it is a mip-correctness one.** VRAM is ~2–3 MB
either way. `max_texture_array_layers` is 2048 and the block atlas needs 1,233 sprites before ~2,600
animation frames are counted, so one-sprite-per-layer does not fit; WebGPU guarantees only 256 layers,
which settles it further. The shipped layout is a 2D atlas with mips generated **per-sprite with clamped
sampling inside each rect**, so no texel averages across a sprite border — but note that isolating mip
*generation* per sprite does not isolate *sampling*: a bilinear tap at a sprite's own UV edge still
reaches its neighbour, so the stitched sprites need a reserved gutter, extruded at every mip level.

**`BakedQuad.layer` is the *atlas* layer, not the render layer.** Routing translucency by it is silently
wrong; `lodestone-assets` exposes no per-quad render type, so render-layer classification is a renderer
concern. Relatedly, awkward packs need no special-casing because vanilla degrades too: it computes the
minimum over sprites of `min(lowestOneBit(w), lowestOneBit(h))` and drops the mip count for the *entire*
atlas when that falls short, which reduces exactly to `effective_levels = min over sprites of
max_mip_level`.

Animated sprites live in the same immutable atlas even with `interpolate: true`: every physical frame is
retained as its own region and the renderer blends N↔N+1 in-shader with both already resident. No
per-tick re-upload, no dynamic region, no seam.

**Entity models are code-only — there is no data path.** Nothing in the generated reports or
minecraft-data exposes mesh geometry; every model class is hand-written. The version-free *primitive*
(`CubeDef`/`PartPose`/`PartDef` → `bake_entity`) lives in `lodestone-assets` and the per-mob data lives
in the version crate; meshes are largely stable across versions, so it is author-once, tweak-per-version.

**Determinism is a hard requirement**: sprite order is sorted by location and face iteration uses a
fixed direction order, never hash-map iteration order, so a given pack always yields byte-identical
atlas bytes, UVs and quad output.

The full block path, end to end:

```
block state id (u32, from the chunk packet)
  -> [version crate]  block name + properties
  -> [assets]         variant selection / multipart evaluation
  -> [assets]         ResolvedModel (parent chain flattened, #variables substituted)
  -> [assets]         baked quads (positions, atlas UVs, cullface, tintindex, shade)
  -> [render]         chunk mesh
```

The id → (name, properties) step is generated per-version data and lives in the version crate, behind a
version-free `BlockStateRegistry` trait in `lodestone-model`. That keeps `lodestone-assets` entirely
version-agnostic while still letting it bake, and the renderer consumes only *baked* output, so the
asset layer is testable without a GPU.

## Client, singleplayer, programmability

- **Singleplayer is the integrated server over an in-memory transport** implementing the same
  `Connection` trait as TCP. This is what vanilla does, and it means singleplayer and multiplayer
  exercise the same code path; open-to-LAN falls out for free.
- **ECS** (`bevy_ecs` standalone) for world and entity state — systems, schedules, and natural plugin
  points for both the renderer and third-party extensions.
- **`lodestone-client` exposes async connect, a typed event stream, and an action API**, headless by
  construction. The renderer is a separate crate observing the same ECS world.
- **Scripting** is a WASM plugin host with a capability-based API, so untrusted automation cannot touch
  the filesystem or network. See [`plugin-api.md`](./plugin-api.md).

## Browser target

**Viable, with one true blocker: browsers cannot open raw TCP sockets and vanilla servers speak only raw
TCP.** A browser build therefore requires a **WebSocket↔TCP relay**. No browser API removes this —
WebTransport and WebRTC do not speak to a vanilla TCP listener either. The relay is small and, critically,
**protocol-blind**: because `Codec` is byte-transparent framing it never parses a packet, so one relay
serves all versions and all servers. The moment it parses a packet it becomes a per-version component.
Singleplayer has no such constraint.

The sans-IO split holds under this pressure: `Codec` is a pure synchronous state machine reusable
in-browser unchanged, `Transport` is a marker trait that a WebSocket stream satisfies for free, and
`connect_with<T: Transport>` is already the injection seam. **Keep that seam sacred** — if the
integrated server or more of `-client` starts assuming `TcpStream`, we lose "the transport is the only
thing that changes."

**A green wasm compile carries almost no information about whether the browser runs.** Measured by
compiling to a `cdylib` and executing in a wasm VM: `std::fs::*` returns `Err(Unsupported)` and does
*not* trap, while `Instant::now()`, `SystemTime::now()`, `thread::spawn` and `thread::scope` all trap
outright. So the filesystem family is degradation-class and the clocks are crash-class. Two refinements
that cost real time to learn: `Builder::spawn` and `available_parallelism` return `Err` rather than
trapping, so classify the **call site** and not the API — an `.expect()` makes a degrading call exactly
as fatal as a trapping one — and `thread::scope` is crash-class *despite* being built on a
degradation-class primitive, because `Scope::spawn` reaches `Builder::spawn`'s `Err` through an internal
`.expect()` inside `std`, where no grep of ours will see it.

`scripts/wasm-check.sh` (and its tested port, `cargo xtask wasm-check`) bans the clock paths
mechanically across every crate the browser links — **a rule written in prose is not a rule**, and this
one existed as an accurate doc comment in `lodestone-server` while four sites violated it. Note the
guard only covers the crates it names, and the browser reaches about fifteen.

See [`browser-shell-port.md`](./browser-shell-port.md) for the census and the open work.

## Testing strategy

| Layer | Method |
|---|---|
| Packets | proptest round-trip plus a **replay corpus** of proxy-recorded real sessions |
| Packet IDs | conformance against Mojang `packets.json` + minecraft-data |
| Physics | golden traces from real client-server sessions, bit-exact |
| Worldgen | block-for-block comparison against real server-generated chunks |
| Renderer | headless wgpu gates that read back actual pixels |
| Integration | real vanilla servers under Apple `container`, scripted scenarios |
| Isolation | `cargo xtask check-isolation` / `check-deletable`, in CI |

Two structural lessons behind that table. **A test suite that mocks the thing it integrates with can
pass indefinitely while being wrong** — the derive macro was tested against its own mock of
`Reader`/`Writer` and the bug was invisible until a real version crate became the first consumer.
And **a test count measures depth and is incapable of measuring connectedness**: subsystems get built to
a high standard in isolation precisely *because* isolation is what makes parallel work possible, so the
seam is the one thing no single owner's test covers. Track it as a ratio (`cargo xtask connectedness`),
because a ratio is falsifiable and "we added some packets" is not.

## Legal

- **No Mojang assets are redistributed.** The client downloads assets from Mojang using the user's own
  authenticated account, exactly as a launcher does. Decompiled output and vendored data stay out of the
  repo, `.gitignore`d and fetched by `xtask`.
- **Decompiled source is a behavioural reference, not a source of code.** We do not transliterate it;
  implementations are written originally and proven equivalent by differential testing, which is both
  legally cleaner and a stronger correctness argument.
- **Vanilla record definitions are cited in `docs/` only, never in a `.rs` file.**
- Study of GPL/AGPL prior art (Sodium-family renderers, ViaVersion, azalea) informs *design only*.

## Dependencies

`wgpu` (renderer), `glam` (math, DirectX RH projection yielding `[0,1]` depth), `bevy_ecs` (standalone),
`tokio` (native async; only `net` and `rt-multi-thread` fail to compile for wasm), `syn`/`quote` (derive
macros and the xtask scanners), `bumpalo` (per-worker meshing arenas), `trunk` (browser build). The
system allocator is used deliberately — see above.
