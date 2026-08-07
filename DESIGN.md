# Lodestone — Design Plan

A multi-version, library-first Minecraft Java Edition client in Rust (2024 edition) + wgpu.

> **How to read this document.**
>
> §0–§11 and §13–§16 are the **design**: architecture, rationale, and the constraints that shaped
> it. They are stable.
>
> **§12 is the validation log, and it is the most valuable section.** It records every claim that
> was empirically tested during construction — including roughly twenty cases where a confident,
> well-argued belief (frequently mine) turned out to be false, and how it was caught. Several
> entries document the *same* error recurring at a new layer; §12.66, §12.74, §12.88 and §12.92
> are four successive failures of a single metric, each at a different layer and the last one
> merely from going stale. If you are resuming this project, read §12 before writing code — it is
> a list of the specific ways this codebase produces green tests that are not evidence.
>
> **Current scope is narrowed to v770 (protocol 776 / MC 26.2) across four workstreams: packets,
> UI, entities, lighting.** Work explicitly deferred out of that scope — other protocol families,
> WebAssembly, audio, worldgen performance, online-mode auth — is documented for pickup in
> [`HANDOFF.md`](./HANDOFF.md), which is self-contained and does not require reading this file
> first.
>
> **Status of the code:** contrary to the line below (written at design time and left for the
> record), substantial production code exists — ~25 crates, four protocol families, a playable
> shell that renders real generated terrain, and a browser build. Measured protocol coverage for
> v770 is **91/141 clientbound decoded, 89/141 reaching the seam, 49/69 serverbound encoded**
> (`cargo xtask connectedness` — run it, don't quote this line, it will go stale too).
>
> What the codebase has consistently *lacked* is not depth but **connectedness**: subsystems
> verified to a high standard in isolation while nothing consumed them. That is now located
> precisely — packets reach the seam well, but **37 of 66 `ClientEvent` variants have no
> consumer**, so the constraint lives in UI and rendering, not in the protocol crates. See §12.85,
> §12.88 and §12.92.

**Status:** design complete, core risks empirically validated (see §12). No production code written yet.

---

## 0. Verified facts (measured, not assumed)

Everything below was checked on this machine rather than recalled. Several contradicted my priors.

| # | Claim | Result |
|---|---|---|
| 1 | Toolchain | rustc **1.95.0**, edition 2024 OK |
| 2 | Latest MC | **26.2**, protocol **776**, dataVersion 4903, requires Java 25 |
| 3 | wgpu latest | **30.0.0** (not 0.20). `Instance::new` takes by value; `Features` is now `[u64;2]` split into `FeaturesWGPU`/`FeaturesWebGPU`; `VertexState::buffers` is `&[Option<..>]` |
| 4 | `darling` 0.23 + `syn` 3 | **Incompatible** — darling still pins `syn ^2.0.15`. Hand-roll attribute parsing |
| 5 | minecraft-data coverage | `data/pc/` stops at **1.21.11**. No 26.x data at all |
| 6 | Mojang data generator | **Works** on 26.2, emits `blocks.json`, `registries.json`, `commands.json`, and **`packets.json`** (authoritative packet IDs keyed by `minecraft:name`) |
| 7 | 26.2 obfuscation | **Gone.** No mappings in the manifest; `net/minecraft/util/Mth.class` etc. present by real name. Decompiled to 4,849 `.java` files |
| 8 | GPU (Apple M5 / Metal) | `MULTI_DRAW_INDIRECT_COUNT` **unavailable**; `TEXTURE_BINDING_ARRAY`, `PARTIALLY_BOUND_BINDING_ARRAY`, `BUFFER_BINDING_ARRAY`, `SUBGROUP`, `SHADER_INT64`, `EXPERIMENTAL_MESH_SHADER`, `TIMESTAMP_QUERY` **all available** |
| 9 | `Mth.sin` parity | Rust `f64::sin` reproduces Java's 65536-entry LUT **bit-exactly** (FNV-1a `3563566116167745249` on both) |
| 10 | Live server | Vanilla 26.2 server runs in Docker for integration tests |

> Note on #8: published guidance claims `PARTIALLY_BOUND_BINDING_ARRAY` and `BUFFER_BINDING_ARRAY` are unsupported on Metal. On this M5 they are supported. **Always probe capabilities at runtime; never branch on documented backend support.**

---

## 1. Guiding principles

1. **The library is the product.** The playable game is a thin shell over `lodestone-client`. Anything the game can do, a bot can do headlessly.
2. **Version knowledge lives in exactly one place per version.** Above the adapter layer, no code knows what protocol it's speaking.
3. **Generated code is cheap; hand-written logic is expensive.** Duplicate the former freely, share the latter carefully. This is the organising rule for §3.
4. **Parity is proven, not asserted.** Physics and protocol correctness are validated by differential testing against real Minecraft, not by reading code and hoping.
5. **Probe, don't assume** — for GPU features, server behaviour, and library APIs alike.

---

## 2. Crate graph

```
lodestone-macros        proc-macros: #[derive(Encode, Decode, Packet)]
lodestone-core          VarInt, Reader/Writer, error types, bounded reads
lodestone-nbt           NBT + SNBT, zero-copy
lodestone-text          chat components (JSON + NBT forms), legacy § formatting
lodestone-model         ← THE CANONICAL MODEL. Version-free. Events, actions,
                          BlockState handles, ItemStack, entity/world types
lodestone-auth          MSA device-code OAuth, session server, profile keys

protocol/v47  v107  v210  v393  v477  v735  v755  v757  v759  v761  v762  v764  v765  v766  v767  v769  v770
                        ↑ one crate per protocol family, named for its LOWEST
                          protocol number (so 26.2 / proto 776 lives in `v770`,
                          the family spanning 1.21.5–26.2).
                          Fully self-contained; depends ONLY on
                          lodestone-core + lodestone-model.

lodestone-protocol      registry: maps protocol number → Box<dyn VersionAdapter>.
                        The only file that knows which versions exist.
lodestone-net           framing, compression, AES/CFB8, transport trait
lodestone-world         chunk storage, palettes, lighting, lock-free snapshots
lodestone-physics       version-free engine + PhysicsProfile (see §4)
lodestone-entity        entity state, interpolation, attributes
lodestone-client        headless client: connect, event stream, action API
lodestone-server        integrated server (singleplayer + open-to-LAN)
lodestone-worldgen      vanilla-parity generation
lodestone-assets        resource packs, models, blockstates, atlas/array build
lodestone-render        wgpu renderer
lodestone-ui            HUD, inventory screens, chat, debug overlay
lodestone               the game binary
xtask                   codegen, data fetch, decompile, conformance, capture
```

---

## 3. Version modularity — the central design

### 3.1 The split

The rule is **"is it generated data, or is it hand-written logic?"**

| Kind | Where it lives | Deletable? |
|---|---|---|
| Packet structs & IDs | per-version crate | yes, entirely |
| Block/item/entity/particle/sound registries | per-version crate | yes, entirely |
| Block-state ID ↔ canonical mapping | per-version crate | yes, entirely |
| Chunk & light wire codecs | per-version crate | yes, entirely |
| Entity metadata layouts | per-version crate | yes, entirely |
| Item/slot serialization | per-version crate | yes, entirely |
| Adapter to canonical model | per-version crate | yes, entirely |
| **Physics engine** | shared, version-free | nothing to delete |
| **Physics constants + feature flags** | per-version crate (`PhysicsProfile`) | yes |
| **Novel per-version physics logic** | per-version crate (hook impl) | yes |
| Worldgen, renderer, UI, netcode | shared, version-free | n/a |

Generated code is duplicated without guilt — a new version is a `cargo xtask` invocation, and if Mojang retro-patches one version we edit exactly one folder. **This is only viable because the duplication is generated; see §13 for why that distinction is load-bearing.**

### 3.2 Strict isolation rule

**A version crate may depend only on version-free shared crates (`lodestone-core`, `lodestone-model`, `lodestone-world`, `lodestone-physics`, …). Never on another version crate.**

Enforced by `cargo xtask check-isolation`, which derives "is a version crate" **structurally** from `crates/protocol/` membership rather than from an allowlist, and states the *invariant* in its failure message so nobody reaches for a hardcoded exemption later:
1. version → version is always fatal;
2. shared → version breaks deletability — a required edge is fatal, an optional/dev-only edge is surfaced as a warning.

**The one intended aggregation point** is `lodestone-registry` (protocol number → `Box<dyn VersionAdapter>`, one feature per family). It opts in via `[package.metadata.lodestone-isolation] role = "version-registry"` — a **structural marker, not a name match**. That marker's only power is to downgrade an *already-non-fatal optional* edge to info; the exemption arm requires `is_soft`, so a required registry→version edge or any version→version edge still falls through to fatal. An escape hatch that cannot be widened into a way to hide a real violation.

**Deletability is measured, not asserted** — `cargo xtask check-deletable <family>` simulates removal and reports the true fallout:
```
$ cargo xtask check-deletable v47
cleanly deletable: removing the folder plus the 4 manifest line(s) below
leaves every crate building (no code changes, nothing structurally undeletable)
  - Cargo.toml:22                              lodestone-v47 = { path = ... }
  - crates/lodestone-client/Cargo.toml:42      live-v47 = ["lodestone-registry/v47"]
  - crates/lodestone-registry/Cargo.toml:26    lodestone-v47 = { workspace = true, optional = true }
  - crates/lodestone-registry/Cargo.toml:32    v47 = ["dep:lodestone-v47"]
+ 4 registry source lines for a warning-clean removal
```
v770 measures at 7 manifest + 4 source lines; the extra weight is one white-box chunk test in `lodestone-client` that intrinsically names `lodestone_v770::packets::chunk` — coupling the registry abstraction genuinely cannot express, and the reason that test should relocate into `crates/protocol/v770/tests/`.

**A bug the real drill caught that the checker missed.** After deleting the folder and the obvious manifest lines, the build *still* failed:
```
package lodestone-client depends on lodestone-registry with feature v47
but lodestone-registry does not have that feature
```
`live-v47 = ["lodestone-registry/v47"]` names the folder **token**, not the package — so it is not a dependency-graph edge at all, and a graph-plus-package-name check reports "unaffected" while the default build is broken by a dangling feature forward. Cargo validates feature strings at resolve time. Same failure shape as everything else on this project: the check was self-consistent and wrong, and only running the drill for real exposed it. The token match is now bounded so `v47` never matches `v470`.

### 3.3 Families, not versions

From the protocol research, 47→776 collapses into **17 families**. One crate per family, named for its lowest protocol number:

`v47` (1.8.x) · `v107` (1.9.x) · `v210` (1.10–1.12.2) · `v393` (1.13.x) · `v477` (1.14–1.15.2) · `v735` (1.16.x) · `v755` (1.17.x) · `v757` (1.18.x) · `v759` (1.19–1.19.2) · `v761` (1.19.3) · `v762` (1.19.4–1.20.1) · `v764` (1.20.2) · `v765` (1.20.3–4) · `v766` (1.20.5–6) · `v767` (1.21–1.21.3) · `v769` (1.21.4) · `v770` (1.21.5–26.2)

Within a family, small deltas use `#[mc(since/until)]` predicates (§5.3) — that's cheap and readable. Across families, nothing is shared.

The hardest boundaries (each forces a new crate): 47→107 (fixed-point→double, metadata rewrite), 340→393 (flattening), 404→477 (light split, palettes), 578→735 (long packing), 754→755 (world height), 756→757 (biomes in sections), 758→759 (chat signing), 763→764 (configuration state), 764→765 (NBT text), 765→766 (item components).

### 3.4 Canonical model direction

The model is shaped by the **newest** protocol's concepts; older adapters translate **upward** (the ViaVersion insight). Consequences:
- Deleting an old version removes only its adapter.
- Adding a new version that introduces a concept means extending the model once, then older adapters supply a default.
- Client/UI/render code never sees a version number.

**Validated** (§12.4): 1.8.9 and 26.2 velocity packets, with different IDs and encodings, decode to one identical `ClientEvent`.

### 3.5 Where isolation is imperfect (honest caveats)

- **The canonical model itself** is shared and grows monotonically. A concept added for 1.8.9 that nothing else has would linger after deleting v47. Mitigation: model on the newest version; legacy-only concepts go behind a small `LegacyExt` enum owned by the version crate.
- **`lodestone-core` primitives** (VarInt, NBT) are shared — but they're version-free by construction, so there's nothing version-specific to strand.
- **Physics** cannot be duplicated per version (see §4) — it's subtle hand-written code where 6 copies means fixing every bug 6 times.
- **Assets/resource packs** vary by version (texture names, model JSON schema). Handled by a per-version asset profile in the version crate, same pattern as physics.

---

## 4. Physics — parity and modularity

### 4.1 How version-specific is it actually?

Measured against decompiled 26.2 (`LivingEntity.java`):
```
DEFAULT_BASE_GRAVITY   = 0.08     BASE_JUMP_POWER        = 0.42
BASE_HORIZONTAL_AIR_DRAG = 0.91   BASE_VERTICAL_AIR_DRAG = 0.98
INPUT_FRICTION         = 0.98     sprint constant        = 0.21600002F
```
These are **unchanged since 1.8**. The base integrator is ~90% version-stable. What actually changes across versions is *which mechanisms exist* — elytra (1.9), swimming (1.13), soul speed (1.16), powder snow (1.17), and in 26.x an attribute-driven `AIR_DRAG_MODIFIER` / `omnidirectionalAirMover()` that didn't exist before.

So: **the numbers are shared; the mechanism set is versioned.**

### 4.2 Design

`lodestone-physics` is version-free and takes a profile supplied by the version crate:

```rust
pub struct PhysicsProfile {
    pub gravity: f64,              // 0.08
    pub jump_power: f32,           // 0.42
    pub air_drag_h: f32,           // 0.91
    pub air_drag_v: f32,           // 0.98
    pub sprint_speed_bonus: f32,
    pub sneak_multiplier: f32,
    pub step_height: f32,
    pub caps: Caps,                // bitflags: ELYTRA | SWIMMING | SOUL_SPEED |
                                   // POWDER_SNOW | ATTRIBUTE_AIR_DRAG | ...
    pub hooks: &'static dyn PhysicsHooks,  // escape hatch for novel logic
}
```

Genuinely new *logic* (not constants) is implemented as a `PhysicsHooks` impl **inside the version crate**, so it still deletes with the folder. The shared engine stays version-free.

### 4.3 Bit-exact parity

Vanilla uses `Mth.sin` — a 65536-entry `float` LUT built as `(float)Math.sin(i / 10430.378350470453)`. Everything downstream (movement vectors, rotation) depends on it.

**Verified:** Rust reproduces the table and lookup bit-exactly, FNV-1a `3563566116167745249` matching Java on all 65,536 entries plus signed/edge probes.

Robustness plan, because `Math.sin` is an intrinsic that may differ per platform:
1. Generate the table once; **check it into the repo** as a constant.
2. Unit test asserts the runtime-computed table hash matches the checked-in one.
3. If a CI platform's libm disagrees, the checked-in table wins — parity never depends on libm agreement.

Other parity requirements: no FP contraction (Rust default, enforced by lint), Java `double→long` truncation semantics (Rust `as i64` matches), collision sweep ordering, and vanilla's step-up / sneak edge-backoff logic.

### 4.4 Proving it

Copyright constrains us (§11): we do **not** transliterate Mojang's source. Instead we prove equivalence empirically:

- **Golden traces.** A recording harness captures per-tick position/velocity/rotation from a real client-server session across scenarios (walk, sprint-jump, water, ladders, ice, slabs, cobweb, elytra, boats, knockback). Rust must reproduce each trace to the bit.
- Traces are stored per version and per scenario; a failing trace names the exact tick and field that diverged.
- This is a far stronger correctness argument than code review, and doubles as the regression suite.

---

## 5. Protocol layer

### 5.1 Traits

```rust
pub trait Encode { fn encode(&self, w: &mut Writer, ctx: Ctx) -> Result<()>; }
pub trait Decode<'a>: Sized { fn decode(r: &mut Reader<'a>, ctx: Ctx) -> Result<Self>; }
pub trait Packet { const NAME: &'static str; const STATE: State; const BOUND: Bound; }
```

`Ctx` carries the negotiated protocol version. Borrowed decoding (`Decode<'a>`) avoids copying large payloads like chunk data.

### 5.2 Derive macro

Built on **syn 3 + quote, no darling** (darling is syn-2-only — verified). Hand-rolled `parse_nested_meta` also gives us precise error spans, which matters for a DSL this large.

```rust
#[derive(Packet, Encode, Decode, Debug, Clone)]
#[mc(name = "minecraft:entity_velocity", state = Play, bound = Client)]
pub struct EntityVelocity {
    #[mc(varint)] pub entity_id: i32,
    pub velocity: Vel3,
}
```

Attribute surface (drawn from what the real protocol actually needs):
`varint` · `varlong` · `len(varint|u8|i16)` · `fixed(n)` · `angle` · `nbt` · `json` · `uuid_int_array` · `remaining` · `when(expr)` · `tag(varint)` for tagged unions · `bounded(max)` for anti-DoS length limits · `since`/`until` for intra-family deltas.

Packet **IDs are never written by hand.** They come from Mojang's `packets.json`, keyed by the stable `minecraft:` name, generated per version. A new version's ID shuffle is then a zero-effort regeneration.

### 5.3 Intra-family version predicates

**Validated** (§12.2): one struct, `#[mc(since = 107)]` / `#[mc(until = 340)]`, produces correctly distinct wire forms for 47 / 340 / 776 and round-trips at each. Because the version is a runtime value in `Ctx`, a single-version build can const-fold the branches away.

---

## 6. Codegen pipeline (`cargo xtask`)

```
xtask fetch-version <ver>    # manifest → server.jar (SHA-verified)
xtask gen-reports <ver>      # java -DbundlerMainClass=net.minecraft.data.Main --reports
xtask gen-registries <ver>   # registries.json/blocks.json → Rust tables in the version crate
xtask gen-packet-ids <ver>   # packets.json → const ID table
xtask new-version <old> <new># clone version crate, apply ID diff, REPORT SHAPE CHANGES
xtask decompile <ver>        # Vineflower → reference source (never shipped)
xtask conformance            # our packet set vs minecraft-data + Mojang reports
xtask capture <ver>          # proxy-record real sessions → replay corpus
```

`new-version` is the answer to "easy when new versions come out": it copies the previous family crate, rewrites IDs from the authoritative report, and prints a diff of packets whose *shape* changed so a human reviews only the genuine deltas.

**Data sources, in order of authority:**
1. Mojang's own generator (`packets.json`, `registries.json`, `blocks.json`) — authoritative, works for every version ≥1.14 **including 26.x**.
2. Decompiled source — reference for behaviour only.
3. minecraft-data — bootstrap + cross-check for **1.8–1.21.11 only** (it has no 26.x).
4. minecraft.wiki protocol pages — human documentation.

---

## 7. Renderer

Vanilla's bottleneck is CPU-side per-section draw submission. The wins, in ROI order:

1. **Compact vertex format** — 12 bytes vs vanilla's ~32 (quantised position within a 16³ section, texture index, packed light/AO, face index). 2–3× bandwidth cut, low effort.
2. ~~**Region-based multi-draw-indirect** — group ~512 sections per region buffer; 10–50× fewer draw calls.~~ **RETRACTED — see §12.21.** Multi-draw is CPU-emulated as a `for` loop on *both* Metal and WebGPU, our only two targets. It reduces draw calls by exactly zero. Region-based *buffer packing* is still valuable (it's what suballocation and GPU culling want); the *draw-call* win is not available here.
3. **Async meshing** on a rayon pool over copy-on-write section snapshots — the world is never locked while meshing.
4. **Binary greedy meshing** (64-bit column bitmasks) for full cubes; 5–20× fewer quads. Non-cube models (stairs, fences, fluids, cross-plants) bypass merging. AO/light values constrain merges — only merge faces with identical packed light.
5. **GPU frustum culling** in a compute pass over the section list.
6. **Texture array instead of atlas** — fixes mip bleeding and makes animated textures a layer update.
7. **Hi-Z occlusion culling**, two-phase with last-frame-visible, at region granularity for rd ≥ 24.

**Metal constraint (measured):** no `MULTI_DRAW_INDIRECT_COUNT`. So draw submission goes behind a strategy trait with three backends:
- `MdiCount` (Vulkan/DX12) — count stays GPU-side. **The only strategy that is a genuine native win**, and the only one selectable, because `MULTI_DRAW_INDIRECT_COUNT` is the sole public signal that distinguishes native multi-draw from emulation (§12.21).
- `MdiZeroInstance` — fixed draw count, culled sections get `instance_count = 0`. **No longer the macOS default — it was never a Metal win at all** (§12.21). Retained as a manual override and as the strategy to wire in if wgpu ever exposes native base-MDI.
- `PerDraw` — **the actual default on Metal and WebGPU**, and correct there: it submits only *visible* regions, whereas the emulated multi-draw loop submits every region including culled ones.

Lighting stays vanilla-accurate (server-provided light data; matching propagation for singleplayer) — modern effects layer on top optionally without changing the vanilla look.

**Foundation built and probed (`lodestone-render`).**`Instance → Adapter → Device` with a `GpuCapabilities` struct that is **constructible without a GPU**, so every decision that branches on capability is a pure function testable headlessly. Draw-strategy selection is one such pure function, unit-tested across the whole matrix *including* a hypothetical indirect-count adapter we can't run locally — so the Vulkan/DX12 path is validated before we own the hardware. Suballocation is address-ordered first-fit with immediate boundary-tag coalescing over one flat arena, `BTreeMap` free list, live allocations tracked so double/fabricated frees are rejected. Evidence it works end to end: an `#[ignore]`d headless test that draws a triangle and **reads the centre pixel back**.

**Probe on Apple M5 / Metal / wgpu 30 (confirms §0):** `multi_draw_indirect_count = false`, `timestamp_inside_encoders = false`; `indirect_first_instance`, `multi_draw_indirect` (via downlevel `INDIRECT_EXECUTION`, emulated), `texture_binding_array` **and** `nonuniform_binding_array_indexing`, subgroup, int64, experimental mesh shader all `true`. Limits that matter: `max_buffer_size` 4 GiB, `max_bind_groups` 8, **`max_texture_array_layers` 2048**, `max_storage_buffer_binding_size` ~4 GiB. Selected strategy: `mdi-zero-instance`.

> The 2048 layer cap is a real constraint on the "texture array instead of atlas" plan (item 6): the block atlas alone needs **1,233 sprites** before animation frames are counted, so one-sprite-per-layer does not trivially fit. Resolve by measurement, not assumption.

**wgpu 30 API deltas worth recording** (verified against the registry source, not recalled — these silently mislead anyone working from older examples):
- `PipelineLayoutDescriptor.push_constant_ranges` is **gone** → `immediate_size: u32` (push constants are now "immediates").
- `Surface::get_current_texture()` returns a **`CurrentSurfaceTexture` enum** (`Success`/`Suboptimal`/`Timeout`/`Occluded`/`Outdated`/`Lost`/`Validation`), *not* `Result<_, SurfaceError>`. Handle every variant; `Outdated`/`Lost` → reconfigure. Surface loss on macOS is a routine event, not a theoretical one.
- Presentation moved from `SurfaceTexture::present()` to **`Queue::present(tex)`**.
- `RenderPipelineDescriptor.multiview` → `multiview_mask` (also on `RenderPassDescriptor`).
- `request_adapter` returns `Result` (not `Option`); `RequestAdapterOptions` gained `apply_limit_buckets`; `DeviceDescriptor` gained `experimental_features`; `Buffer::get_mapped_range()` returns `Result`; `device.poll` takes `PollType::wait_indefinitely()`; `VertexState.buffers` and `PipelineLayoutDescriptor.bind_group_layouts` are slices of `Option<_>`; `RenderPassColorAttachment` requires `depth_slice`.

**Verified versions:** `wgpu` 30.0.0, `winit` 0.30.13 (optional, `window` feature), `pollster` 1.0.1, `bytemuck` 1.25.2, `glam` 0.33.2.

**More wgpu/glam API corrections (measured, not recalled):** `DepthStencilState::depth_write_enabled` is `Option<bool>` and `depth_compare` is `Option<CompareFunction>`; `SamplerDescriptor::mipmap_filter` takes a distinct `wgpu::MipmapFilterMode`, not `FilterMode`; `PipelineLayoutDescriptor::bind_group_layouts` is `&[Option<&BindGroupLayout>]`. glam 0.33 **deprecates** `Mat4::perspective_rh`/`look_to_rh` — use `glam::camera::rh::proj::directx::perspective` (yields `[0,1]` depth, which is what wgpu/Metal want) and `glam::camera::rh::view::look_to_mat4`. And a debugging trap worth remembering: a buffer's `BindGroupLayoutEntry::visibility` must name **every** stage that reads it — marking a vertex-read buffer `FRAGMENT` compiles fine and fails only at bind time.

> **Gap found: we only ever decompiled `server.jar`.** `.cache/mc/26.2/src/` has no `net/minecraft/client` package, so camera conventions, `GameRenderer`, `LocalPlayer` and the client render pipeline were *unverifiable* — the camera was implemented from documented behaviour with every convention written down as an explicit assumption (RH; +X east/+Y up/+Z south; yaw about +Y, 0 = south, 90 = west; positive pitch looks down; vertical FOV 70°; near 0.05). `client.jar` is being decompiled to `.cache/mc/26.2/client-src/` so these can be reconciled against the source. **This also matters for physics** (client-side prediction) and must be gitignored like the server source.

**Decoupling rule:** the mesher's input is a `SectionView`-style **trait**, not a dependency on `lodestone-world`. This lets meshing be built and tested against synthetic worlds in parallel with world storage, and keeps the renderer usable by anything that can answer "what block is at (x,y,z)".

> **Correction: the neighbourhood is 3×3×3 = 27 sections, not 6.** Face culling alone needs the 6 face-adjacent sections, but **ambient occlusion samples the 3 cells around each vertex corner**, which reach across section *edges and corners* too. Meshing with only 6 neighbours yields correct culling and subtly wrong AO along every section boundary — a much harder artifact to spot than missing faces. Missing neighbours read as empty.

**Vertex format — 8 bytes/vertex** (2× `u32`), 6× smaller than a naive 48-byte `{pos: [f32;3], uv, normal, colour}`:
```
word0:  x[0:6] y[6:12] z[12:18] normal[18:21] ao[21:23] sky[23:27] block[27:31]
word1:  sprite[0:11] u[11:16] v[16:21]           (u,v in tile units)
```
6 bits per axis covers 0..63 (a 16³ section needs 0..=16, leaving headroom for sub-block geometry); normal is one of 6 faces; AO is vanilla's 0..3; sky/block light 4 bits each; 11-bit sprite id = 2048 sprites. **VRAM @ RD32:** 12.5 M quads → **667 MiB packed vs 2,574 MiB naive**; 50 M quads → 2,670 MiB vs 10,299 MiB. That is the difference between fitting in VRAM and not.

**Meshing — measured, single section:**

| world | simple quads | greedy quads | reduction |
|---|---|---|---|
| full solid | 1,536 | 6 | 99.6% |
| flat plain | 768 | 6 | 99.2% |
| cave-like noise | 5,702 | 5,702 | 0% |
| checkerboard (worst case) | 12,288 | 12,288 | 0% |

**Greedy is the default.** It is never *worse* in quad count, because the merge key includes all four AO values and both light channels — which is precisely why it degenerates to simple on noisy terrain rather than producing wrong output. Its cost on those inputs is CPU time, not vertices, so a cheap per-section heuristic (fall back to simple if the first merge pass yields <10% reduction) is the follow-up. `mesh_simple` is kept as the correctness reference and the two are cross-validated.

AO is vanilla's per-vertex rule from the 3 corner neighbours (`side1 && side2 ? 0 : 3-(s1+s2+corner)`), with the quad triangulation diagonal flipping when `ao0+ao2 > ao1+ao3`. **This is not actually what vanilla does** — see the smooth-lighting divergence below.

**Camera reconciled against the real client source** (only reachable after `client.jar` was decompiled; see §7 client-source gap). Held: right-handed Y-up, `rotationYXZ`-derived forward `(−sin y·cos p, −sin p, cos y·cos p)`, vertical FOV default 70, near = `PROJECTION_Z_NEAR = 0.05`. **Two assumptions were wrong:**
- **Far plane is `max(render_distance_chunks·16·4, cloud_range·16)` — 4× render distance in blocks, 2048 at RD32.** A "sensible" fixed 512 clips distant terrain, and the symptom looks like a projection-matrix bug rather than a constant.
- Camera position is `entity.y + eye_height`, standing `DEFAULT_EYE_HEIGHT = 1.62`. The eye offset is load-bearing for raycast parity, so it is not a rendering detail.

FOV-modifying effects (sprint/sneak/underwater ×0.857/death) scale the degrees *before* projection and stay the caller's concern rather than being baked into the camera.

**Divergences from vanilla, deliberately catalogued** (matching where it's correctness, deferring where it's polish):
- **Section visibility matches.** Vanilla's `VisGraph` floods non-opaque cells from every edge cell and marks all face-pairs the region touches as connected — the same thing our union-find 15-pair matrix computes. Deferred: vanilla's sparse-section shortcut (`setAll` when <256 opaque or fully solid, skipping the flood entirely — the *common* case, since most sections are mostly air), multi-source entry accumulation (we keep only the first entry face, so we are slightly conservative — a safe direction to be wrong in), and the distant-section raymarch (skipped as complex and marginal).
- **Smooth lighting genuinely diverges.** Vanilla averages **four** samples per corner — two edge sides, the diagonal corner, *and the centre* — as continuous floats, blending skylight and blocklight identically over the same neighbours, then interpolates those corner values across non-cube faces by `faceShape` weights. The classic integer `3−(s1+s2+corner)` gets the *shape* of concave-corner darkening right and the values wrong.
- **Translucency sorting is a real gap.** Vanilla splits into `SOLID`/`CUTOUT`/`TRANSLUCENT` layers and sorts only TRANSLUCENT back-to-front, re-sorting via `TranslucencyPointOfView`, which quantizes the camera relative to each section to `{−1,0,1}³` and re-sorts **only when that octant changes**. That trigger is not a micro-optimisation — it is what makes the sort affordable at all.
- **Vanilla does no greedy meshing whatsoever.** It emits model quads and relies on face-culling plus layers. Greedy is *our* optimisation layered on top, and it is valid only for full-cube faces. Worth stating loudly, because "vanilla parity" and "greedy meshing" are independent axes and conflating them produces confident, wrong reasoning.

**The most instructive renderer bug so far: air must carry light, or every block face renders black.** A face samples its lighting from the *neighbour* cell it faces into — which for an exposed surface is air. Returning an unlit `Cell::EMPTY` (`sky_light: 0`) is a perfectly valid-looking value that happens to be the wrong one, so every geometry unit test passes and the terrain still renders at 0.2× brightness (measured pixel `[0,51,0]`). This is the archetype of the class: correct mesh, correct pipeline, wrong output, silent. It was only diagnosable because the GPU test reads back **actual pixels** rather than asserting on the mesh.

**Open fork — two vertex formats.** Packed 8-byte `PackedVertex` stores position at 6 bits/axis on the block grid, which is exact for cube corners and far too coarse for baked models on a 1/16 grid that can poke outside the cube. So non-cube geometry uses a wider float `ModelVertex`. This is sound in isolation but collides with the asset pipeline: **all 32,366 states are baked models**, so in the real pipeline there is no block that isn't a model, and the packed path survives only if a predicate can recognise "exactly a full opaque cube" *from the baked model* and route it. That predicate must be derived, never a hardcoded block list — a hardcoded list is a version-specific fact smuggled into a version-free crate. Being resolved by measuring what fraction of baked states, and what fraction of *rendered blocks* in realistic terrain, are full cubes. Collapsing to one format is an acceptable outcome: the whole 667-vs-2,574 MiB argument assumes the fast path carries the bulk, and if it doesn't, a 2× memory cost buying a single code path is the better trade.

**Section visibility — implemented, not just designed.** Each section is flood-filled with union-find to record which of the 15 face-pairs are mutually connected; a BFS walks the section graph from the camera gated by `connects(entry, exit)`, never reversing along an axis, composed with the frustum test. This is what stops the entire underground being submitted while standing on the surface — **frustum culling alone cannot do this**. Pure and headlessly testable.

**Atlas vs texture array — settled by measurement: hybrid.** 1,233 sprites, ~1,147 exactly 16×16, **~2,600 total animation frames**, 42 wider than 16. Since `max_texture_array_layers = 2048`, one-sprite-per-layer **does not fit** once frames are counted, and the 42 wide sprites can't share 16×16 layers anyway. But VRAM is ~2–3 MB either way — **this was never a capacity decision, it is a mip-correctness decision.** So: texture array for the 16×16 majority (each layer gets a clean, independent mip chain), 2D atlas for the wide/animated outliers. For the atlas path, mips are generated **per-sprite with clamped sampling inside each rect**, so no texel ever averages across a sprite border; verified by a red/blue two-sprite test asserting no purple appears at any mip level.

---

## 7.1 Assets and resource packs

**Requirement: full compatibility with vanilla resource packs.** The asset layer speaks Mojang's on-disk format natively, so any pack that works in the real game works here unmodified. Vanilla's own assets are then just the bottom-most pack in the stack — no special-casing, and "use the real textures" and "use a custom pack" are the same code path.

**Acquisition — download, never vendor.** Vanilla assets come from `client.jar` plus the asset index, fetched from Mojang at runtime exactly as a launcher does, into `.cache/`. Nothing is committed. This is both the legally clean position (§11) and the practical one: the repo stays small and each version pulls its own matching assets. *(User has noted licensing is not a concern while private; recording it here so the constraint isn't lost when it stops being private.)*

**Layering.** A pack stack, lowest priority first: `[vanilla client.jar, ...user packs]`. Later packs override earlier ones per-resource, matching vanilla's semantics. Namespaced lookups (`minecraft:block/stone`) resolve to `assets/<ns>/<kind>/<path>`, so non-`minecraft` namespaces work for free.

**What's version-specific.** Path conventions and format numbers drift; the loader must not hardcode them:
- `textures/blocks/` (≤1.12) vs `textures/block/` (1.13+) — the flattening hit assets too.
- `pack.mcmeta` `pack_format` numbers differ per version and gate validity.
- Model/blockstate JSON gained features over time (multipart, `atlases/` in 1.19.3+).

Per §3's rule, the **loader is version-free** and the **conventions come from the version crate** as an asset profile — same shape as `PhysicsProfile`. Dropping a version drops its profile, not the loader.

**Measured facts about vanilla's own assets (26.2, from the real `client.jar`):**
- **`client.jar` has no root `pack.mcmeta`.** Root entries are just `pack.png`, `version.json`, `flightrecorder-config.jfc` plus `assets/ data/ META-INF/ com/ net/`. Vanilla builds its built-in pack programmatically, so the loader must treat "no `pack.mcmeta`" as a valid source, not an error. User packs always have one.
- Vanilla pack metadata comes from `version.json`, whose shape has changed: `pack_version` is now **major/minor pairs** (`resource_major: 88`, `resource_minor: 0`, `data_major: 107`, `data_minor: 1`), not a flat integer. **The resource pack format for 26.2 is 88.**
- `version.json` also carries `protocol_version: 776`, independently confirming the number the network stack targets.
- Content counts: 1,371 `textures/block/*.png`, 2,657 `models/block/*.json`, 1,198 `blockstates/*.json`. Modern flattened singular paths (`block/`, not `blocks/`).
- Per-texture `*.png.mcmeta` files carry **animation** metadata (e.g. `{"animation":{"frametime":2}}`) — unrelated to `pack.mcmeta`, and the input to animated-texture support.

**Resolution layer — built and measured against the real jar.** `BlockStates` (variants + multipart with `When::{Match,And,Or}` and `|`-alternatives) and `ModelResolver` → `ResolvedModel` (parent chains flattened, `#variables` substituted, cycles → `ParentCycle`, depth cap 128). Geometry-only; no GPU types, so it unit-tests headlessly. **Coverage: 1198/1198 blockstates, 8661 model refs, 2657/2657 models resolve — 100%.**

Two findings the jar disproved:
- The 3 in-jar `pack.mcmeta` files are **datapacks**, not resource packs: `data/minecraft/datapacks/{minecart_improvements,redstone_experiments,trade_rebalance}/pack.mcmeta`.
- **Element rotation has two shapes in 26.2**, not one: the classic `{axis, angle, origin, rescale}` and a **Euler `{x, y, z, origin}` triple** (hanging signs, e.g. `template_hanging_sign_rot_3`) whose angles exceed the old ±45 limit. Normalise both into `ElementRotation { origin, angles: [f32;3], rescale }`. Rejecting the Euler form silently cost 12 models / 48 refs.

**Known perf trap (found and fixed):** `ZipSource::read` originally reconstructed a `ZipArchive` per call, re-parsing `client.jar`'s central directory on every read — full-jar resolution took **153 s**. Fixed by parsing once at `open()` and cheap-cloning the archive over a shared `Arc<[u8]>` of the file bytes, which keeps reads **lock-free and parallel** (a `Mutex<ZipArchive>` would fix correctness but serialise the asset loader). **153.33 s → 0.26 s, ~590×**, guarded by an elapsed-time assertion so it can't silently regress. Tradeoff to revisit: the whole archive is held in memory (~39 MB for `client.jar`, but user packs stack and can be far larger) — a memory-mapped variant is the escape hatch.

**Textures — measured distribution (26.2), which invalidates the obvious assumptions:**
- **1,269 block PNGs. Colour types: palette 1076, RGBA 116, RGB 37, grey+alpha 21, grey 19; bit depths 1/2/4/8.** A decoder written for "vanilla is RGBA8" fails on the *majority* of the jar. Palette + `tRNS` and sub-byte depths are mandatory.
- **1,175 are exactly 16×16**; nearly all the rest are 16×N vertical animation strips (16×64 = 4 frames, 16×512 = 32 frames); only ~42 are genuinely wider (mostly 32×32, one 32×1024).
- **176 `*.png.mcmeta`** files (102 under `textures/block/`), 0 malformed. Top-level keys: `animation` 63, `texture` 57, `gui` 44, `villager` 14 — so the parser must accept non-animation sections without erroring.

**Atlas vs texture array — decision: texture array, staged.** ~93% of block textures are exactly 16×16 and the tall ones are 16-wide strips of 16×16 frames, so array layers of a common tile fit vanilla almost perfectly, and per-layer mip chains eliminate atlas mip-bleed outright (binding arrays confirmed available on Metal, §0). Cost: layers share dimensions, so higher-res packs scale to a common tile. Currently implemented as a 2D shelf-packed atlas at native resolution with `layer` already on every sprite, so the switch is not an API break; the renderer will measure both before we commit. Mips are deferred to the renderer — this crate stays GPU-free.

**Full block atlas (measured):** 1,234 textures referenced by the 1,198 blockstates → 1,233 loaded, **0 decode failures**, 1,233 sprites (52 animated), 1024×1024 single layer, 4.0 MiB RGBA, **0.10 s**, byte-identical on rebuild. The single miss is `minecraft:missingno`, vanilla's intentional placeholder.

**Baking — measured: 32,366 / 32,366 block states baked, 100%, 0 failures, 415,364 quads, 4.86 s.** (1,377 states are legitimately empty — air, fluids, block-entity-only — counted as zero-quad successes, not failures.) The full vanilla `FaceBakery` behaviour is reproduced: default UV derivation, element + model rotation, `uvlock` UV recompute, face rotation, winding recalculation, and **rotation-aware `cullface`** — a `cullface: north` on a model rotated 90° about Y must rotate too, or adjacent blocks get holes and z-fighting.

> **Fourth jar-driven correction.** The first bake run scored **94.26%**, with 1,857 `UnresolvedTexture` failures concentrated on glass, redstone wire, ice, slime and honey. Cause: **26.2 introduced an object form for texture values** — `"all": {"sprite": "minecraft:block/glass", "force_translucent": true}` instead of a bare string. 110 models / 163 occurrences, i.e. essentially every translucent block. Supporting it lifted coverage to 100%. This is the fourth time a whole-corpus coverage number has caught a real bug that hand-picked fixtures missed; it is the single highest-value test in the assets crate.

Also measured: the Euler element rotation appears in exactly **one** model in 26.2 (`template_hanging_sign_rot_3`), so it was worth handling but is not widespread.

**Determinism is a hard requirement**: sprite order is sorted by location and face iteration uses a fixed direction order, never `HashMap` iteration order, so a given pack always yields byte-identical atlas bytes, UVs and quad output.

**Pipeline.** pack stack → resource provider (bytes by `ResourceLocation`) → blockstate JSON (variants + multipart) → block model JSON (parent inheritance, `#texture` variable resolution, elements/faces) → baked geometry + stitched atlas / texture array → renderer (§7).

The full block path, end to end:
```
block state id (u32, from chunk packet)
  → [version crate]  block name + properties   (from generated blocks.json: 1196 blocks, states with ids)
  → [assets]         variant selection / multipart evaluation
  → [assets]         ResolvedModel
  → [assets]         baked quads (positions, atlas UVs, cullface, tintindex, shade)
  → [render]         chunk mesh
```
The id → (name, properties) step is **generated per-version data** and so lives in the version crate, behind a version-free `BlockStateRegistry` trait in `lodestone-model`. That keeps `lodestone-assets` entirely version-agnostic while still letting it bake.

The renderer consumes only *baked* output, so the asset layer is independently testable without a GPU — which is how it gets unit tested.

**Beyond blocks (verified: 168 hermetic + 11 real-jar tests).**
- **Entity models are code-only — there is no data path.** Nothing in `.cache/mc/26.2/generated/` or `minecraft-data` exposes mesh geometry; all 267 classes under `net/minecraft/client/model/` are hand-written `LayerDefinition`/`MeshDefinition`/`PartDefinition`. **~130–150 base mob meshes must be hand-ported.** Mitigation that keeps this from being "17 versions × every mob": the version-free *primitive* (`CubeDef`/`PartPose`/`PartDef` → `bake_entity`) lives in `lodestone-assets`, and the per-mob `EntityModelDef` **data** lives in the version crate. Meshes are largely stable across versions, so it is author-once, tweak-per-version.
- **Animated sprites can live in the same immutable atlas, even with `interpolate: true`.** Every physical frame is retained as its own region, so the renderer selects a frame sub-rect and blends N↔N+1 in-shader with both already resident. No per-tick atlas re-upload, no separate dynamic region, no seam. This was a real fork in the renderer's upload strategy and it is now closed.
- **Item models: 1271/1271 resolve (100%)** — `generated` 1231, `block3d` 3, `empty` 37, **`builtin_entity` 0**. That zero is a genuine 26.2 finding: chests/shulkers/banners moved to the new `assets/minecraft/items/*.json` item-definition system, so `builtin/entity` no longer appears under `models/item/`. `builtin/*` parents have no JSON file at all and must be terminal sentinels rather than resolution errors.
- **Pack stacking is proven, not assumed**: texture override, new namespace, model replacement, 3-pack priority with de-duplicated `list()`, and `pack_format` gating (flat exact-match vs inclusive `supported_formats` range).
- **Tint indices across all 32,366 states: exactly 2.** tintindex 0 on 31 blocks, tintindex 1 on 2 (pink_petals, wildflowers). No heavy machinery justified.


---

## 7.2 Memory design

Ordered by **measured impact**, which is not the order people usually reach for. Allocator choice is last because it's worth far less than getting the data layout right.

**The number that drives everything.** A 1.18+ chunk column is 24 sections × 4096 blocks = 98,304 blocks. Stored naively as `u16` that's 196 KB/column; at render distance 32 (4,225 columns) it's **~830 MB of block data alone**. That is the whole ballgame — no allocator can rescue a layout that wrong.

1. **Never allocate per block.** A block state is a `u32` id, not an object. No `Box<dyn Block>`, no per-block structs. Behaviour lives in tables indexed by id.

2. **Paletted containers** (the single biggest win, and free). Per-section palette + bit-packed indices, bits-per-entry sized to the palette — exactly what the wire format already uses, so we need it for protocol parity regardless. Homogeneous sections collapse to a **single-value palette** storing no index array at all, which matters enormously because most sections are pure air or pure stone. Typical terrain lands ~4–5 bpe → ~2 KB/section. That turns the 830 MB above into roughly **100 MB**.

   **Measured (`lodestone-world`, real implementation):** flat-world column **6,864 B**; realistic terrain column **19,264 B** → **77.6 MiB @ RD32**, beating the 100 MB estimate. Full-entropy worst case is 201,408 B/column (811 MiB) — i.e. the naive size, correctly, because genuinely random data is incompressible.

   **Thresholds, read from 26.2 `Strategy.java` (my recollection was right for blocks, wrong for biomes):**
   - *Block states* (bitsPerAxis 4): 0 → single-value; 1–4 → **clamped up to 4-bit** linear palette; 5–8 → hashmap palette at that width; >8 → direct (`ceilLog2(registrySize)`, ≈15).
   - *Biomes* (bitsPerAxis 2, 64 entries): 0 → single; 1→1, 2→2, 3→3 bit linear; >3 → direct. **No floor clamp and a ceiling of 3** — not a scaled copy of the block rule.
   - Entries **never straddle an `i64`**: `valuesPerLong = 64/bits`, low bits first, leftover high bits are padding. (True since 1.16.)
   - Index order is **YZX**: `(y << b | z) << b | x`.

   **Version-specific framing (important, and a trap):** in 26.2 the packed long array is written with `writeFixedSizeLongArray` — **no VarInt length prefix**, the count is derived from bits × entry count. Older protocols *do* prefix it. The bit-packing, thresholds and indexing are structural and shared; only the framing differs, so it is a `LongArrayFraming::{Prefixed, FixedSize}` knob on the container profile, never a hardcoded modern default in the version-free crate.

   **The boundary is 1.21.5 / snapshot 25w07a / protocol 770** (high confidence, three independent lines of evidence: the 26.2 source, the wiki chunk-format page, and a *sibling* break at exactly 1.21.5 in `minecraft-data` where heightmaps switch from an NBT compound to a typed long-array list). So **≤769 → `Prefixed`, ≥770 → `FixedSize`**. Our `v770` family sits exactly on the boundary. Cross-parse safety is explicit: decoding `Prefixed` bytes under a `FixedSize` config is caught by the palette-range check or by trailing bytes — never a silent misparse.

   **Heightmaps are a second knob on the same boundary**: NBT compound ≤1.21.4 vs typed long-array list ≥1.21.5. Packed storage shared, framing at the version seam.

3. **Empty-section elision.** An all-air section stores nothing and is a null pointer in the column.

3b. **Light is the real memory hog, not block states.** 4096 nibbles = 2048 B per section per light type × 2 (sky + block) × **26** light sections (light extends one section beyond the build range, top and bottom) = **~106 KB per column** naively — five times a realistic terrain column's block data, i.e. **~396 MiB @ RD32**. Elision is therefore not an optimisation but a requirement.

   **Measured, with elision (`lodestone-world`):** realistic-terrain light **9,024 B/column → 36.4 MiB @ RD32** (vs 396.1 MiB naive, ~11×). Full totals: realistic terrain **79.2 MiB blocks+biomes + 36.4 MiB light**; full-entropy worst case 813 MiB + 432.5 MiB.

   Representation: `NibbleArray` (2048 B, YZX `y<<8|z<<4|x`, byte-pair packed per vanilla's `DataLayer`), `LightData::{Missing, Uniform(u8), Values}` — a uniform section costs **one byte**. **Wire/storage asymmetry worth knowing:** vanilla only elides all-*zero* light on the wire (`isEmpty()`), so a uniform-15 sky section is still transmitted as a full 0xFF array. We transmit faithfully and store it as a tag.

4. **Slab/pool recycling for section storage.** Bits-per-entry ∈ {1,2,3,4,5,6,7,8,15} over a fixed 4096 entries yields a *small, fixed set of size classes*. Chunk streaming churns these constantly as columns load/unload, which is exactly the pattern a size-classed free pool handles best: recycle `Box<[u64; N]>` per class instead of round-tripping the allocator. This is where a slab genuinely pays.

5. **Bump arena for meshing.** Meshing produces variable-size vertex/index scratch per section on a rayon worker. A per-worker `bumpalo` arena reset after each section makes that allocation-free in steady state (`bumpalo` 3.20.3).

6. **GPU suballocation** — likely worth more than CPU allocator choice. `wgpu` does not suballocate; one buffer per section would be disastrous. Sections suballocate from large per-region buffers via a free-list, which the region-based MDI design (§7) already wants.

7. **Global allocator — MEASURED. Decision: keep the system allocator.** Benchmarked in `crates/lodestone-allocbench` (one binary per allocator, mutually-exclusive features; peak RSS via `/usr/bin/time -l`; workload = deterministic cross-thread producer/consumer of paletted-section arrays 512 B–7.5 KB and mesh buffers 2 KB–300 KB; median of 5).

   | vs. system baseline | throughput (geomean) | mean RSS |
   |---|---|---|
   | mimalloc 0.1.52 | 94% | **130%** |
   | snmalloc-rs 0.7.4 | 79% | 104% |
   | tikv-jemallocator 0.7.0 | **113%** | 111% |

   No candidate is both faster *and* leaner than macOS `libmalloc`, and the wins at the top end are within the measured noise on this machine. Each costs a C/C++ toolchain dep (snmalloc additionally needs **CMake**; jemalloc adds ~22 s to a cold build). **Not justified.** If meshing throughput is later *proven* by profiling to be the bottleneck, `jemalloc` is the only candidate with a consistent edge — revisit then, not before.

   **Two findings worth keeping:**
   - **Cross-thread free inverts the ranking.** Local-free order is `jemalloc > system ≈ mimalloc ≫ snmalloc`; cross-thread free at 8–10 threads is `snmalloc > jemalloc ≈ mimalloc > system`. `snmalloc` is the only allocator that gets *faster* under cross-thread free (~196% of its local-free rate) while everyone else degrades. Benchmarking with same-thread free — the obvious thing to write — would have ranked snmalloc last and produced the opposite conclusion. The §7.2 hypothesis that our access pattern favours snmalloc was directionally right; it just isn't enough to beat the baseline overall.
   - **"Just use mimalloc" is wrong here**: weakest overall, parity throughput at the *highest* RSS.
   - Methodology trap hit and fixed: `vec![0u8; n]` routes to `alloc_zeroed`, letting an allocator skip the memset on fresh OS-zeroed pages — it showed jemalloc at a bogus 4×. Use `with_capacity` + real fill so the benchmark matches how sections and meshes are actually written.

**Rule: library crates must never set `#[global_allocator]`.** That is an application-level decision, and a library that hijacks it breaks every downstream consumer. The allocator is selected in the game binary only, behind features, with the library defaulting to whatever the host chose.

---

## 8. Client, singleplayer, programmability

- **Singleplayer = integrated server over an in-memory transport** implementing the same `Connection` trait as TCP. This is what vanilla does, and it means singleplayer and multiplayer exercise the same code path. Open-to-LAN falls out for free.
- **ECS** (`bevy_ecs` standalone, 0.19) for world/entity state — gives systems, schedules, and natural plugin points for both the renderer and third-party extensions.
- **Programmable API:** `lodestone-client` exposes async connect, a typed event stream, and an action API. Headless by construction — the renderer is a separate crate that observes the same ECS world.
- **Scripting** (later phase): WASM plugin host with a capability-based API, so untrusted automation can't touch the filesystem or network.

---

## 9. Testing

| Layer | Method |
|---|---|
| Packets | proptest round-trip `decode(encode(x)) == x` for every packet, every version |
| Packets | **replay corpus** — proxy-recorded real sessions; decode all, assert byte-identical re-encode |
| Packet IDs | conformance vs Mojang `packets.json` + minecraft-data |
| Physics | **golden traces** from real client-server sessions, bit-exact |
| Worldgen | block-for-block comparison vs real server-generated chunks |
| Renderer | headless wgpu golden images with tolerance |
| Integration | real vanilla servers per tier-1 version in Docker; scripted scenarios |
| Isolation | CI lint: no version crate may reference another |

Tier-1 versions (full CI): **1.8.9, 1.12.2, 1.16.5, 1.20.1, 1.21.x, 26.2**. Others best-effort within their family.

---

## 10. Roadmap

| Phase | Deliverable | Status |
|---|---|---|
| **1. Skeleton** | Workspace, macros, core, model, `v770` crate (protocol 776 / MC 26.2), net stack. **Headless client joins the live 26.2 server, exchanges keep-alives, reads chat.** | ✅ **Done** — verified against a real vanilla server (`Lodestone logged in with entity id 102 / joined the game`), 185 tests green |
| **2. World** | Chunk decode, palettes, lighting, block registry. Headless client has a queryable world. | ✅ **Done — seam closed this session.** 225 live columns decoded with 0 trailing bytes and flat-world layers at the correct Y; the §12.24 white-box gap is fixed — the client owns a `World` behind a `WorldSink`, `ClientEvent::ChunkLoaded` carries only `pos`, and chunk data now crosses the public API (81 columns on 1.8.9, 9 on 26.2 through the playable binary). Remaining: section-granularity `Arc` + a merge op so a block update mutates one section rather than replacing a column (§12.49) |
| **3. Physics** | Engine + profile + golden-trace harness. Bot walks, jumps, collides identically to vanilla. | 🔶 **Engine proven; send path gated, server-acknowledged displacement not yet.** Live gate against real 26.2: 100 ticks, **zero corrective `player_position` packets**, with a permanent negative control proving the server *would* have corrected an impossible move (§12.53). A bot walks 4 blocks through `lodestone-client`'s public API with the displacement now **asserted** (≥3.5 blocks, arrival within 0.5). But that readback is the driver's **optimistic local prediction**, not server-confirmed movement (§12.70) — the remaining gap is a second observer client watching our own entity |
| **4. Second version** | Add `v47` (1.8.9). This *proves* the isolation design under maximum stress — and validates deletion. | ✅ **Done, now at three families.** `v47` (1.8.9) + `v340` (1.12.2) alongside `v770`. Deletability **measured** and continuously checked: v340 = 4 manifest + 2 source lines, v47 = 4 + 4, v770 = 7 + 4. `check-isolation` clean apart from one known `lodestone-client → v770` warning (a white-box chunk test being relocated) |
| **5. Render** | wgpu pipeline, meshing, atlas/array, entities, HUD. Playable. | 🔶 **Live chunk → pixels, verified fail-closed.** A real column from the live 26.2 server meshes and renders: 3008 quads, sky 22.2% / terrain 77.8%, 8.67s. The gate previously passed in **0.00s asserting nothing** (§12.52) — now selects the jar by named version, fails closed in all six sites, and the fail-closed behaviour is itself covered by three hermetic tests in the default suite. Measured: packed:wide 75:25, greedy merge 43–46×. Next: multi-section render loop, culling, draw-call strategy |
| **6. Interaction** | Inventories, containers, crafting, scoreboard, tab list, bossbars, sounds, particles, resource packs. | ⚠️ **Depth without seams** — see the connectedness table below. Click machine matches vanilla on all 10 click types, audio decodes vanilla ogg with three-implementation validation, scoreboard/tab-list types are tested; **none of them receive a packet**. Blocked on dispatch breadth, assigned |
| **7. Singleplayer** | Integrated server + worldgen parity. | ✅ **Reachable — and it is the first Docker-free end-to-end test.** `lodestone-client` connects to `lodestone-server` in-process and receives generated chunks, asserted block-for-block through the client's public API, with two anti-vacuity floors, **not `#[ignore]`d** (§12.63). Worldgen retains bit-exact JVM parity. Remaining: swap the labelled `StandInAdapter` for the real v770 codec once its encoders land |
| **8. Fill in** | Remaining families via `xtask new-version`; scripting host. | |

**Connectedness is now the binding constraint, and it is tracked separately from depth.** Auditing for *seams* rather than for bugs (the §12.34 technique applied to integration) found the same defect four times in one day. None of it fails a test:

| Subsystem | Depth | Connection to the client |
|---|---|---|
| Chunks | 225 live columns, 0 trailing bytes, palette strategies validated | ✅ crosses the API; `BLOCK_UPDATE` + `SECTION_BLOCKS_UPDATE` now dispatch onto section-granularity `Arc` storage |
| Physics | vanilla-exact; live gate shows zero corrective teleports, with a negative control | 🔶 send path asserted through the public API; server-acknowledged displacement still ungated (§12.70) |
| Entities | 117 tests, arrow parity 4.8e-6, 158 types censused | ✅ metadata + attributes cross the API; client attribute fold **0.35 == server's 0.35** |
| Scoreboard / tab list | types well tested | 🔶 all six packets **decode** but `return Ok(Vec::new())` — decoded-and-stranded, so §12.51 is **open**, not closed (§12.74). `impl-model`'s carriers now exist, so the flip is one line per arm |
| Audio | 63 tests, three-implementation Vorbis validation | ✅ `SOUND`, `SOUND_ENTITY`, `LEVEL_EVENT`, `LEVEL_PARTICLES` dispatch |
| Inventory clicks | agrees with vanilla on all 10 click types | 🔶 `OPEN_SCREEN` dispatches; `CONTAINER_SET_CONTENT` / `SET_SLOT` and the serverbound click action still missing |
| Singleplayer (`lodestone-server` + worldgen) | bit-exact JVM parity across ~130k probes | ✅ in-process e2e, block-for-block, no Docker (§12.63) |

**Play clientbound: 45 handled of 141, of which 44 reach a consumer and 1 is decoded-and-stranded (`PLAYER_INFO_REMOVE`). Play serverbound: 16 of 69 (23%).** Those ratios, not the test count, are the honest measure of how much game exists — measured against Mojang's own per-state packet tables, **not** against counts we choose ourselves (§12.66), and counting *arrival at a consumer* rather than mere appearance in the adapter (§12.74). The three legitimate outlets are a `ClientEvent`, the world sink, and a `Directive`; a packet that decodes and discards is better than one ignored, because it proves the codec, but it is not connectedness.

**A classifier for this must follow delegation one level.** Four separate times a hand-rolled counter has reported a false stranding because the arm body is `return decode_sound(payload);` or `return handle_add_entity(payload);` rather than an inline `ClientEvent::`. Each time the contradiction with a directly observed behaviour was the signal that the *check* was wrong, not the code (§12.74). Any future count must resolve `return <fn>(payload)` before judging, and the standing rule holds: **when a check disagrees with something you have directly observed, the check is the suspect.**

**The 96 unhandled clientbound packets are the binding constraint on the whole project.** Ranked by consequence rather than count: stream-breaking (`START_CONFIGURATION`, `CHUNK_BATCH_START`/`FINISHED`, `BUNDLE_DELIMITER`), simulation-affecting (`UPDATE_MOB_EFFECT`/`REMOVE_MOB_EFFECT`, `EXPLODE`, `SET_PASSENGERS`, `MOVE_VEHICLE`), world-integrity (`LIGHT_UPDATE`, `SET_CHUNK_CACHE_CENTER`/`RADIUS`, `BLOCK_ENTITY_DATA`, `BLOCK_EVENT`, `BLOCK_CHANGED_ACK`), player-facing (`PLAYER_CHAT`/`DISGUISED_CHAT`/`DELETE_CHAT`, `TAB_LIST`, `SET_HELD_SLOT`, `SET_EXPERIENCE`, titles, `RESOURCE_PACK_PUSH`/`POP`), then a long cosmetic/debug tail (`DEBUG_*`, `GAME_TEST_*`, `TICKING_*`, `WAYPOINT`, dialogs) that can wait indefinitely. Ordering by consequence matters more than the raw ratio: a client missing `WAYPOINT` is fine, one missing `START_CONFIGURATION` desyncs the stream and misparses everything thereafter.

The lesson is structural rather than a lapse anyone made: **a test count measures depth and is incapable of measuring connectedness.** Each of these subsystems was built to a high standard against real vanilla data, and each was verified in isolation *because* isolation is what makes parallel work possible. The seam is the one thing no single owner's test can cover, so it is the one thing that must be audited centrally and tracked as a ratio — a ratio is falsifiable; "we added some packets" is not.

Consequently the roadmap's ordering changes: **dispatch breadth outranks new subsystem depth.** Wiring `SOUND` connects a finished audio engine; wiring `Move` turns a validated physics engine into a bot that walks; making `lodestone-server` reachable turns a verified worldgen into the feature "singleplayer" *and* gives the project its first end-to-end test that needs no Docker.

**Two capability gaps found by auditing for *unassigned work* rather than for bugs (§12.34), now both assigned:**

| Gap | Why it matters | Owner |
|---|---|---|
| **Online-mode auth + encryption** | No AES-128-CFB8, no RSA step, no session-server call, no `lodestone-auth` crate. Every test server this session is `online-mode=false`, so the branch has **never executed**. Real public servers are all online-mode — against the stated requirement ("compatible with regular vanilla Minecraft servers") we currently connect to nothing outside our own lab. | `impl-net` |
| **Audio** | No `lodestone-audio` crate. `lodestone-assets` resolved the sound *registry* (1968 events, 4843 files) but nothing decodes or plays. | `impl-audio` |

The auditing technique is worth keeping: **scan for capabilities nobody owns, not just for failures in what exists.** Bugs announce themselves through red tests; *absent* subsystems are invisible to every test in the repo, and a fully green workspace says nothing about them.

Phase 4 is deliberately early: the isolation architecture must be stress-tested before there's a lot of code to retrofit.

**Phase 1 retrospective — what the process caught.** Three real bugs were found only because a *real consumer* and *real data* were introduced early rather than late:
1. `lodestone-macros` was tested against its own mock of `Reader`/`Writer`. The codegen compiled against the mock and nothing else — invisible until `v770` became the first real consumer. Fixed by dev-depending on `lodestone-core` and deleting the mock.
2. That fix immediately exposed a second bug: `#[mc(remaining)]` never consumed its bytes — live in the very code path that joined the server.
3. The asset loader assumed `client.jar` has a root `pack.mcmeta`. It does not; running the test against the real jar disproved it, and also corrected the pack format from a guessed 55 to the actual 88.

The generalisable rule: **a test suite that mocks the thing it integrates with can pass indefinitely while being wrong.** Prefer a real dependency and real data even when it's slower.

**Phase 2 retrospective — the rule held again, twice.** Chunk storage passed 62 hermetic tests including hand-built golden bit-packing bytes, and was still wrong in two ways that only the live server could reveal:
1. **Each section carries TWO shorts, not one** — `writeShort(nonEmptyBlockCount); writeShort(fluidCount);` — before the block container. The synthetic model tracked a single non-air count. No amount of self-consistent round-trip testing could surface this; only real bytes could.
2. **Chunk delivery is flow-controlled.** Vanilla sends one batch, emits `chunk_batch_finished`, then *stalls* until the client replies `chunk_batch_received` (a single big-endian `float`). Without that ACK we received **9 chunks**; with it, all **225**. A synthetic transport would have happily delivered everything and hidden the requirement entirely.

The detector that caught #1 was **asserting zero trailing bytes after decode**. A misparse almost always leaves the buffer misaligned, so `ensure_empty` is worth far more than any number of "did it error?" assertions. The detector for a transposed layout was asserting **known block ids at known Y** in the flat world — a YZX-transposed decoder passes every round-trip test and fails that one instantly.

**Live measurements (26.2, flat world, 15×15 view):** 225 chunks in 0.70 s; palette strategies **single 5175 / indirect 225 / direct 0** across 5,400 sections (exactly one indirect terrain section per chunk — single-value elision validated on real data); **11,184 B/chunk** including blocks, biomes, light, heightmaps and block entities → **~47 MB at RD32**, consistent with the synthetic projection and far under the naive baseline.

**Macro gap surfaced by this work — now closed, with a deliberately limited verdict.** `Decode::decode(r, ctx)` threads only `ctx.version: i32`, but chunk sub-structures need *structural* parameters that come from the **dimension registry, not the protocol version**: `PalettedContainer` needs a `PaletteKind`, `Heightmaps` needs world height, `ColumnLight` needs section count.

`lodestone-macros` closed this with `#[mc(decode_context = "T")]` (generates an inherent `T::decode_with(r, ctx, &T)` instead of a `Decode` impl) and `#[mc(decode_with = "path")]` (routes one field through a custom decoder that receives the context). Non-breaking: every existing derived packet is untouched. Rejected alternatives: extending `Ctx` with `&dyn Any` (moves a compile-time error to a runtime downcast failure) and making `Decode` generic over context (breaking change across every version crate).

**The migration was then run against the real packet, and the honest verdict is "adopt, but do not extend."** `v770`'s `LevelChunkWithLight` now uses the derive and the live gate passes unchanged (225 chunks, 0 trailing bytes, identical palette/layer numbers to the pre-migration baseline — so the migration is behaviour-preserving). But:

- The derive **cannot express `Vec<T>` whose element decode needs context**. The `section_count` loop over two *different* `PaletteKind` containers stays a hand-written function that the derive merely calls. That is precisely the case the design was meant to prove, and it isn't proven.
- Net readability is a wash: one linear codec became field order plus four functions.

The reflex here is to go build element-context support into the macro. **That reflex is wrong.** Count the packets that actually need structural context across 17 families: chunk data and light update, essentially — roughly two per family. Machinery for two packets costs more than it saves. So the mechanism stays (it is free and non-breaking), pays off for packets with *many simple* context-dependent scalar fields, and is not forced onto codecs whose bulk is a bespoke loop. **The judgement recorded here is that a modest, honestly-scoped mechanism beat a more powerful one, and that the migration's value was in producing that scoping rather than in the migration itself.**

Secondary finding from the same migration: the generated `decode_with` hardcodes `lodestone_core::Result`, so field codecs speaking `WorldError` needed manual conversion. The blocker was initially reported as an orphan-rule violation — true in `v770`, **false in `lodestone-world`**, where `impl From<WorldError> for lodestone_core::Error` is legal (`impl From<Local> for Foreign` needs one local type and no uncovered type parameters ahead of it). The impl belongs in the crate that owns the error.

---

## 11. Legal

- **No Mojang assets are redistributed.** The client downloads assets from Mojang using the user's own authenticated account, exactly as a launcher does.
- **Decompiled source is a behavioural reference, not a source of code.** We do not transliterate it. Implementations are written originally and proven equivalent by differential testing (§4.4) — which is both legally cleaner and a stronger correctness argument.
- Decompiled output and vendored data stay out of the repo (`.gitignore`d, fetched by `xtask`).
- Study of GPL/AGPL prior art (e.g. Sodium-family renderers) informs *design only*; no code is copied.

---

## 12. Validation log

Spikes written and executed during design. All passed.

**12.1 Derive macro on syn 3** — builds without darling after confirming darling 0.23 pins syn 2.

**12.2 Version-predicated codec** — 5/5 tests:
```
v47_omits_head_yaw_but_keeps_legacy_flag   ok   (4-byte wire form)
v340_has_both_optional_fields              ok
v776_drops_legacy_flag                     ok   (3-byte wire form)
same_struct_yields_different_bytes_per_version ok
```

**12.3 Version deletion** — built with `--no-default-features --features v776`; then physically `rm -rf`'d the `proto-v47` crate and made the 3 documented edits. Tree builds, tests pass.

**12.4 Canonical convergence** — 1.8.9 (id `0x12`) and 26.2 (id `0x5F`) velocity packets decode to one identical `ClientEvent`; the same `ClientAction` encodes to different IDs and different byte lengths per version.

**12.5 GPU capability probe** — Apple M5 / Metal, feature matrix in §0.

**12.6 `Mth.sin` bit-exactness** — **all 65,536 entries bit-identical between a real JVM and Rust `f32`**, compared element-wise.

The original spike recorded an FNV-1a constant (`3563566116167745249`) as the anchor. It proved **unreproducible** — the serialization it used was never recorded, so no later run could recompute it. It has been **deleted**, and the reasoning generalises: *a hash nobody can recompute is worse than no hash at all.* It looks like a regression anchor, which stops anyone from adding a real one, while being incapable of detecting anything.

The replacement is a checked-in JVM dump (`tests/support/sin_reference_jvm.txt`) compared entry-by-entry, which **names the divergent index** on failure — the difference between "something changed" and "entry 40,113 changed". Verified by re-running `SinOracle` in a fresh `eclipse-temurin:25-jdk --rm` container and diffing: byte-identical.

**12.8 Movement bit-exactness vs a real JVM.** A from-scratch Java re-implementation of `Mth` + collision + the air/water player steps (written originally, not transliterated) dumps per-tick position/velocity via `Double.doubleToRawLongBits`. All **9 scenarios agree bit-for-bit** with the Rust golden traces: `free_fall(200) walk_flat(200) sprint_jump(120) ice_slide(200) walk_into_wall(80) slab_step(60) water_sink(120) diagonal_walk(100) analog_strafe(100)`.

This closed a weakness the physics agent flagged itself: the earlier golden harness used a Python oracle, so *both* sides were the same author's re-derivation and agreement proved less than it appeared. The chain is now **JVM == golden == Rust**, in the language whose float semantics we claim to match.

**An honest non-finding worth recording.** The agent hardened the Python oracle's `modifyInput` length from a single cast to per-op rounding to match Java/Rust structurally — then reported that it **could not construct an input where the two forms diverge**, so the mismatch is latent and structural rather than demonstrated. It declined the available flattering narrative ("the JVM oracle caught a Python bug"). That matters: an overstated win would have taught us the harness is more sensitive than it is, and it would have been trusted too far somewhere that counts.

**12.9 The live-chunk "regression" that was never a code change.** The Phase 2 gate went from 225 chunks to **0** with no edit to the decoder, and the change window pointed at a `lodestone-registry` refactor. It was innocent. The chain, established by measurement rather than by reading the refactor:

1. Restarting the container did **not** fix it → not transient server state.
2. A packet-id histogram of the live stream showed 253 play packets that were *perfectly aligned and sane* — 22 `bundle_delimiter` pairs around 22 `add_entity`, `set_chunk_cache_center`, `player_position`, `set_time`, `update_attributes` — and **zero** `level_chunk_with_light`. So the framing, the state machine and the IDs were all correct; the server simply wasn't sending chunks.
3. Dumping payloads: `set_health` = **`0.0`**. The persisted NBT at `world/players/data/<uuid>.dat` confirmed `Health = 0.0` and a `DeathLocation`.

**Root cause: in offline mode the server derives the account UUID from the *username* (`OfflinePlayer:<name>`) and ignores the UUID the client sends.** Every live test used the name `Lodestone`, so all of them shared one persisted player file. A mob killed that player once (`Lodestone was slain by Zombie`), vanilla persisted the dead state, and from then on every rejoin was held on the death screen — which sends no chunks until `client_command(perform_respawn)`. Deleting the file wasn't enough either: another agent's live test recreated the dead state within seconds (`slain by Spider`). Switching the gate to a per-run unique username restored **225 chunks, 0 trailing bytes, in 0.87 s** (versus a 30 s timeout).

Three things worth keeping:
- **A dead player is a silent, total chunk blackout.** Join, keep-alives, entity spawns and entity movement all continue perfectly. Every signal a client normally trusts says "healthy".
- **Shared mutable state arrived through a *username*.** Nothing in the test looked shared — it even generated a fresh `Uuid::new_v4()` per run, which is precisely the mitigation that *appears* to isolate runs and does not, because the server discards it.
- **The `hasClientLoaded()` / `clientLoadedTimeoutTimer` lead was a dead end**, and reading callers proved it: `sendNextChunks` is called unconditionally per tick from `MinecraftServer` and is not gated by it. Confirming a promising hypothesis is *false* was what forced the switch to dumping real payloads, which is what actually solved it.

**12.10 Collision shapes: the community dataset is not good enough, and the game will answer directly.** `blocks.json` contains **no collision geometry at all** — 1,196 blocks / 32,366 states of properties only. Vanilla collision is code-defined (`Block.getCollisionShape`), often neighbour-state-dependent. The obvious fallback, `vendor/minecraft-data/blockCollisionShapes.json`, measured **stale and incomplete for 26.2**: newest pc entry 1.21.11, **92.29% of states reliably covered, 7.71% fallback/suspect, 30 blocks missing by name**. That is the number that makes the decision; a spot check would have said "looks fine."

**The replacement technique matters more than the shapes.** `ShapeOracle.java` boots the real 26.2 server headlessly in Docker (`SharedConstants.tryDetectVersion(); Bootstrap.bootStrap()`) and dumps `getCollisionShape(...).toAabbs()` for **all 32,366 states** — authoritative, version-exact, and immune to third-party lag. **Generalised rule: prefer interrogating the real jar over any community dataset; where minecraft-data is still the practical choice, record why.**

Facts from that dump that are load-bearing and not guessable: **fence/wall collision height is 1.5** (the 0.6 auto-step cannot mount them — a bug that would read as "the pathfinder is broken" rather than "a shape is wrong"); `soul_sand` = 0.875; `cobweb`/`water`/`lava` are **empty**.

**12.11 A version profile that can only carry numbers is a trap.** Two 1.8-vs-modern movement differences are genuinely *different functions*, not coefficients: modern `modifyInputSpeedForSquareMovement` (unit-square projection) vs 1.8's `moveFlying` normalise-by-`max(1, magnitude)`, and the modern fluid branch (swimming pose + falling-adjusted clamp) vs 1.8's single fluid path. A scalar-only `PhysicsProfile` would have silently run modern maths under a 1.8 label — fully configured-looking and wrong.

The seam is therefore **type-level**: `InputModel { UnitSquareProjection, LegacyMoveFlying }` and `FluidModel { Modern, Legacy1_8 }` on the profile, dispatched in `modify_input`/`tick_water`, with the **1.8 arms `unimplemented!()`** so a 1.8 profile fails loudly. No decompiled 1.8 source exists locally, so shipping a plausible-looking 1.8 body would reintroduce exactly the silent-wrong risk the seam exists to remove. It stays a loud stub until a 1.8 JVM oracle can validate it.

Also confirmed from `Blocks.java`: ice and packed_ice friction `0.98F`, **blue_ice `0.989F`** (three tiers, not two). Ladder climb clamp ±`0.15F` widened to double = `0.15000000596046448`. **12/12 movement scenarios bit-identical JVM == golden == Rust.**

**12.12 Lodestone runs in a browser — and the browser immediately exposed a false claim.** Verified first-hand: `scripts/wasm-check.sh` passes for all 11 wasm targets (core, model, world, physics, assets, registry, render, v770, v47, net `--features ws-web`, `lodestone-web`); `cargo test -p lodestone-relay --test live_ws_join -- --ignored` joins the real 26.2 server through the WebSocket relay (**ok, 15.46s**); and serving `web/dist/` in Chrome renders greedy-meshed geometry with the HUD reading `backend: BrowserWebGpu | strategy: MdiZeroInstance`.

**That strategy value falsifies this plan's own claim** that `select_strategy()` "degrades to `PerDraw` on WebGPU by construction." It does not. The probe sets `multi_draw_indirect` from `DownlevelFlags::INDIRECT_EXECUTION` — which means *indirect draws work at all*, not *multi-draw indirect works* (`Features::MULTI_DRAW_INDIRECT`, native-only). One boolean carries two different meanings, and `MdiZeroInstance::record` then calls `multi_draw_indexed_indirect`, which WebGPU cannot perform.

Measured directly in the browser (`[...adapter.features]`): **`indirect-first-instance` IS present** on Chrome/Apple WebGPU, and there is no multi-draw feature of any kind. So both halves of the `indirect` conjunction read true and the selector confidently picks an impossible strategy. Had `indirect-first-instance` been absent we would have landed in `PerDraw` by luck and never learned this.

It doesn't crash today only because the browser app calls `select_strategy` for **display** while drawing through a simpler path — the worst state available: already wrong, silent, and scheduled to detonate on whoever first wires the real strategy, who will then debug the renderer rather than the probe. **Rule: a capability field must be named for exactly the capability it measures**, and every capability claim about a backend we haven't run on is a hypothesis until the backend says otherwise.

Also measured: the unoptimised `.wasm` is **12 MB**; WebGPU masks `adapter.info` for fingerprinting, so the HUD's adapter name is legitimately blank.

**12.13 The full-cube census killed the argument I was going to make for the packed vertex format — and the format survived anyway, for different reasons.** Baking all 32,366 v770 states: **1,377 empty, 30,989 renderable, of which only 2,874 (9.3%) are full-cube geometry and 2,622 (8.5%) are packed-eligible untinted cubes**; 28,115 (90.7%) are non-cube. Worse for the intuition: the two dominant overworld *surfaces* — grass (tinted top → wide path) and water (a fluid) — are **not** packed cubes. "The fast path carries most surfaces" is simply false.

What actually justifies keeping two formats: **volume, not state count** (410/1,196 blocks are full-cube in *every* state, and those are the ones that fill a world — stone, deepslate, dirt, sand), and a **different UV/animation strategy** (packed stores sprite-id + tile coords and animates in-shader; models bake absolute atlas UVs). Entities settle it: they need a third pipeline regardless (own texture sheet, own lighting, no tint, no merge) but **share `ModelVertex`'s layout with baked models**, so the wide path's fixed cost is amortised over two consumers and packed becomes a pure-win specialisation. Priced: **packed 72 B/quad vs wide 136 B/quad — 1.9× per quad, 2.33× per vertex.** The predicate is derived from *baked geometry* (6 axis-aligned unit faces, each self-culled), so no version-specific block list rots the version-free crate.

**12.14 Vanilla's darkest ambient-occlusion corner is 0.4, never black.** Occluded shade is 0.2 and the always-open block in front contributes 1.0, giving `(0.2·3 + 1.0)/4`. The integer `3−(s1+s2+corner)` AO with a hard corner → 0 was wrong. Adopting vanilla's real 4-sample float model (`{edge1, edge2, diagonal, centre}`, with the `smoothBlend` rule replacing a dark *occluding* neighbour by the centre light when the centre is lit) forced the packed vertex from **8 to 12 bytes** — fractional AO does not fit a 2-bit field — so **the packed win over the 48-byte naive baseline drops from 6× to 4×.** Correctness moved the number; the number did not get to move correctness.

**12.15 The `<256` sparse-section shortcut is exact, not merely conservative.** The min-cut of a 16³ grid is **256 cells**, so fewer than 256 opaque blocks *cannot* disconnect opposite faces — `opaque_count < 256 → all()` is provably right rather than a safe guess.

**12.16 `BakedQuad.layer` is the atlas layer, not the render layer.** Routing translucency by it — as an earlier brief instructed — would have been silently wrong. `lodestone-assets` exposes no per-quad render type today, so render-layer classification is a renderer concern. Translucency re-sort reproduces `TranslucencyPointOfView` exactly (per-axis `blockToSectionCoord(camera) − section` clamped to `{−1,0,1}`, re-sort only when the triple changes) and is proven by **pixel readback**: sorted `[128,0,64]` vs unsorted `[64,0,128]` for two overlapping half-alpha quads — asserting on the blended pixel, not on the index array.

**12.17 The `texture_2d_array` path was mis-gated behind bindless.** A `texture_2d_array` sampled `textureSample(t,s,uv,layer)` needs **neither** `TEXTURE_BINDING_ARRAY` **nor** non-uniform indexing; only true bindless (`binding_array<texture_2d>`, per-fragment non-uniform) does. The code demanded the one feature the web lacks in order to use the universally-available path. Split into two orthogonal axes: `recommend_layout(stats, max_layers)` decides physical layout on **fit** alone, `select_binding_model(caps, layout)` decides binding. Also verified: **WebGPU guarantees only `maxTextureArrayLayers = 256`** (2048 measured on Metal), so at 1,233 sprites the web target falls out to Atlas2D by fit — and the 11-bit sprite field is safe to lock because in Atlas2D it indexes a UV lookup table, not an array layer.

**12.18 A green live test that passed on luck: the summon/tick race.** `impl-entity`'s live attribute tests queried an entity selector immediately after `/summon`. They passed for a long time — then failed the moment the oracle server was restarted on a clean world. Root cause: **a freshly summoned entity is not selector-visible until the next server tick**, and the tests had been winning the race only because network round-trip latency happened to straddle a tick. Query-immediately → "No entity was found"; `sleep(2s)` → found, `step_height = 0.6` reads correctly. Fixed with a `wait_for_entity()` poll; now deterministic across 3/3 repeat runs.

Two general lessons. **Timing-dependent is not the same as correct**, and a suite that is green on latency asserts nothing — this is the same failure class as the unreproducible hash constant (§12.8) and the mis-gated capability probe (§12.12): each *looked* like verification. And **environmental variation is a test-quality instrument** — this only surfaced because a coordination nudge forced a restart, not because anything failed on its own. Anything that creates server state (`/summon`, `/kill`, a death) must be polled for, not asserted against immediately.

**12.19 A wrong contract propagates further than wrong code.** `PathWorld::collision_top` was *documented* as returning `0.0..=1.0`. The code was fine; the doc would have led whoever implemented the seam to **clamp fences to 1.0**, making them pass the 0.6 step-up check and producing "the pathfinder routes straight through fences" — a symptom hunted in the navigation crate while the fault sits two crates away in a doc comment, and where the implementer who followed the docs is blameless. Fixed to state *uncapped*, with the authoritative values inline (fence/wall/fence-gate 1.5, soul_sand 0.875, slab 0.5, air/water/lava/cobweb 0.0).

**Seam boundaries settled as a result:** `lodestone-physics` owns the collision *algorithm* and `CollisionView` (plus, now, an uncapped walkable-top accessor); the **version crate** owns the shape *data* and the `base_path_type` node-evaluator classification (open/blocked/water/lava/damage/fence/door/rail) — that is block-registry semantics, **not** collision, and putting it in `CollisionView` would weld navigation policy into the physics seam; `lodestone-entity` owns navigation and consumes both.

**12.20 Attribute application order (authoritative, single source in `lodestone-entity`).**

```
base   = baseValue + Σ amount          // ADD_VALUE
result = base + Σ (base * amount)      // ADD_MULTIPLIED_BASE
result = result * Π (1 + amount)       // ADD_MULTIPLIED_TOTAL
value  = sanitize(result)              // clamp [min,max]; NaN -> min
```

26.2 renamed the operations: `AddValue`(0) = old ADDITION, `AddMultipliedBase`(1) = MULTIPLY_BASE, `AddMultipliedTotal`(2) = MULTIPLY_TOTAL. Modifiers are keyed by a stable `Identifier` (vanilla's per-id constraint), replaced in place, first-insertion order preserved. Within one operation class the maths is order-independent; only the three-class ordering matters. Sprint = `AddMultipliedTotal` 0.3. **The failure mode to watch is folding the two multiply stages into one** — it yields a movement speed a couple of percent wrong under effects, which is effectively unlocalisable after the fact.

**What does *not* ride the attribute seam (settled by pushback, before it was implemented).** `impl-physics` verified at `Player.java:456` that `setSpeed((float) getAttributeValue(MOVEMENT_SPEED))` runs per tick — so sprint is already *inside* the attribute value, and physics must consume `.value()` rather than re-multiplying. But it then rejected the obvious generalisation: **Jump Boost is not a `MOVEMENT_SPEED` modifier** (it is `+0.1·(amp+1)` added to jump velocity in `LivingEntity.getJumpPower`), and **Depth Strider is a boots enchantment** reducing water drag. Only **Speed and Slowness** ride the seam; Jump Boost and Levitation are their own fields. Routing Jump Boost through `MOVEMENT_SPEED` would have produced a wrong jump height presenting as a gravity bug — caught at the seam-design stage rather than after, which is the cheapest possible place to catch it.

Matching discipline on the `f32` cast: physics takes the raw `f64` from `.value()` and performs the `(float)` narrowing **at vanilla's spot inside the tick**, not at the seam. Casting early would be invisible and would break bit-exactness.

**12.21 The renderer's second-biggest planned optimisation does not exist on either of our targets — and my own proposed fix was impossible.** Following §12.12 I instructed `impl-render` to probe `multi_draw_indirect` from `Features::MULTI_DRAW_INDIRECT`. It came back with: **that feature does not exist in wgpu 30.** I verified in the registry source — `grep MULTI_DRAW` over `wgpu-types-30.0.0/src/` yields exactly one constant, `MULTI_DRAW_INDIRECT_COUNT`. Base multi-draw is gated solely on the `INDIRECT_EXECUTION` *downlevel flag*, so the "correct capability" I told it to use was fictional. The field was instead renamed to `indirect_execution`, honestly describing what the flag measures.

Then the much larger finding, which I confirmed line-by-line in the vendored sources rather than accepting:

- **Metal** — `wgpu-hal-30.0.0/src/metal/command.rs:1616`: `for _ in 0..draw_count { encoder.drawIndexedPrimitives_...(); offset += size_of::<DrawIndexedIndirectArgs>(); }`. "Multi-draw" **is** a CPU loop of N draw calls.
- **WebGPU** — `wgpu-30.0.0/src/backend/webgpu.rs:3748`: `for i in 0..count { self.inner.draw_indexed_indirect_with_f64(...) }`. Identical shape.
- Native single-command multi-draw exists only on **Vulkan** and **DX12** (`ExecuteIndirect`).

**Consequence: §7's item 2 ("region-based MDI → 10–50× fewer draw calls") delivers zero draw-call reduction on Metal and WebGPU, which are the only two backends Lodestone actually runs on today.** Worse, `MdiZeroInstance` — documented as "the default on macOS" — was **strictly worse than `PerDraw`** there: the emulated loop issues a draw for every region *including culled ones*, while `PerDraw` issues draws only for visible ones. The plan's optimisation ROI list had a pessimisation sitting at #2 with a 10–50× win written next to it.

Two further traps found in the same read, with opposite failure modes for the same misuse:
- Metal's `draw_indexed_indirect_count` is an **empty `//TODO` stub** — it silently draws nothing.
- WebGPU's `multi_draw_indexed_indirect_count` **panics** with a clear message.

The silent one is the dangerous one: a spurious `MULTI_DRAW_INDIRECT_COUNT` on Metal yields a blank screen with no error.

**The load-bearing structural fact: wgpu 30 exposes no public signal distinguishing native from emulated base multi-draw.** `MULTI_DRAW_INDIRECT_COUNT` is the only honest proxy, so `select_strategy` keys on it alone. A hypothetical Vulkan adapter with native base-MDI but no count feature is undetectable and conservatively gets `PerDraw` — accepted, because the alternative is gating on a hardcoded backend name, which is the exact rule §0 exists to enforce.

Measured after the fix on real M5/Metal: `indirect_first_instance=true, indirect_execution=true, multi_draw_indirect_count=false` → **`per-draw`** (was `mdi-zero-instance`). 121 hermetic + 6 GPU + 1 census pass; clippy clean. Bindless was re-audited under the same suspicion and is **not** affected — `texture_binding_array`/`nonuniform_binding_array_indexing` read real feature bits that WebGPU genuinely lacks, so it honestly reports false there; locked with a test.

Three lessons, and the third is the one I most want to keep:
1. **A capability name is an assertion, and this one was false in a way that inverted a performance decision.** §12.12 called it "scheduled to detonate"; it had in fact already detonated, quietly, as a pessimisation nobody would have profiled.
2. **"Emulated" is not a footnote.** wgpu presents a uniform API over backends that implement it in wildly different ways; a call that *works everywhere* can have wildly different cost, and the API deliberately hides that. Availability and performance are independent questions.
3. **I gave a confidently wrong instruction and the agent refused it with evidence.** Had it complied, we'd have a compile error at best and a plausible-looking wrong probe at worst. The value of the whole review structure is precisely this — an agent that verifies its brief against source instead of executing it. That behaviour should be rewarded loudly, because the alternative failure is silent.

**12.22 The asset crate's browser problem turned out to be already solved — by a seam built before the requirement existed.** I briefed `impl-assets` that `lodestone-assets`' use of `std::fs` was a structural wall for the browser: `fs` is sync, `fetch` is async, so the seam *shape* would have to change and no `cfg` could rescue it. I asked it to check one thing before designing anything — whether the parsers already take bytes. They do, and the wall isn't there:

```
$ grep -rln "fs::" crates/lodestone-assets/src/
crates/lodestone-assets/src/source.rs     # one file
$ grep -rn "fs::" crates/lodestone-assets/src/     # three lines, all native discovery
```

Every parser is already `parse(&[u8])`, and the `ResourceProvider` trait I described as work-to-be-done **already existed** as `ResourceSource` — sync, byte-only, `read`/`list`. The browser path needs **no API change whatsoever**: `fetch` the pack into a `Vec<u8>`, hand it to `ZipSource::from_bytes`, and both `read` *and* `list` work off the in-memory index, so whole-corpus enumeration works in a browser too. `ZipSource::open(path)` is just `fs::read` + `from_bytes`, so native and browser converge one line in.

`impl-assets` also pre-emptively refused the async version of the trait, correctly: making `ResourceSource` async would poison the font loader, model resolver and atlas builder and force every hermetic test async, to solve a problem one pre-fetch already solves. **The right shape is async *acquisition*, sync *access*** — and the sync half was already built.

Three lessons:
1. **Check whether the problem exists before designing the solution.** My brief was a confident, plausible, well-argued description of a wall that wasn't there. One `grep` settled it. The cost of asking first was ~1 minute; the cost of not asking was an async refactor of the crate's entire downstream API.
2. **A byte-oriented seam is portability insurance you get for free.** Nobody designed `ResourceSource` for wasm. It's portable because "parsers take bytes, sources supply bytes" is just good decomposition, and good decomposition pays out against requirements that didn't exist when it was written.
3. **The residual risk is the `Instant::now()` class again:** `DirectorySource`/`ZipSource::open` compile on wasm and die at runtime, which the compile-only tripwire structurally cannot catch. The fix is to feature-gate the fs impls behind a default-on `native` feature so a `default-features = false` build *cannot reference* them — converting a runtime death into a compile error. Held pending evidence of what the browser actually calls.

**12.23 Assets whole-corpus coverage extended to fonts, GUI, sounds and particles** — 220 hermetic + **15** real-jar, clippy clean (verified: all 15 named tests green in 7.28s).

- **Fonts.** Advance width is derived from the **rightmost non-transparent column + 1**, scaled by `declared_height/glyph_height` — *not* from the cell width. Verified live against the real jar (`i=2, l=3, I=4, a=6, !=2, W=6, t=4, .=2`). Getting this wrong silently breaks every centring, wrapping and tooltip-sizing calculation in the game. `space = 4` passing also proves **first-declared-provider-wins** priority ordering, since the space provider must beat the ascii blank cell's advance of 1. Census: 7 font files, providers bitmap=5 / space=1 / reference=7 / **ttf=0 / unihex=0**, 2414 codepoints in the default stack. Seam to `lodestone-text`: assets owns glyph metrics + `advance_bold`; only **bold** changes advance (+1); legacy `§` codes are 2 chars, 0 width.
- **GUI.** 466 sprites → **422 stretch, 44 nine_slice, 0 tile**. `tile` implemented for third-party pack compatibility even though vanilla ships none. Nine-slice geometry proven by an area-sum invariant at multiple target sizes.
- **Sounds — my brief was wrong twice.** Entry type is **`"file" | "event"`**, not `"sound" | "event"` (`Sound.java`'s `Type` enum). And **`sounds.json` is not in `client.jar` at all** — it, like every `.ogg`, lives in the external asset-object store addressed by `asset-index-32.json` (`objects/<sha1[0..2]>/<sha1>`). Real corpus: **1968 events, 8024 entries (7963 file, 61 event-refs), 4843 distinct files.** Chaining is real but **all 61 refs are depth-1 and acyclic**; vanilla ships **no cycle guard**, so a malicious pack would stack-overflow at play time — we bound it with a visited set + depth cap. Faithful subtlety: a `type: event` entry contributes the *referenced* event's total weight to the parent's selection sum.
- **Particles.** 112 files, all carrying textures, every listed sprite resolving to a real texture (0 misses).

**12.24 The client cannot deliver a chunk to anyone, and the lint that pointed at it was one message away from being suppressed.** `impl-shell`, building the playable binary, tried to *use* the client for its actual purpose and found:

```
$ grep -n "packets::chunk" crates/protocol/v770/src/adapter.rs
(no output)
adapter.rs:237:   // Everything else in play is intentionally ignored for now.
lodestone-model/src/event.rs:169:   ChunkLoaded { pos: ChunkPos }   // position only, no block data
```

The `level_chunk_with_light` decoder is real and correct — 225 chunks, zero trailing bytes — but `handle_play` never calls it, and `ClientEvent::ChunkLoaded` structurally cannot carry a chunk. **"Phase 2 complete: live chunk decode" was proven by a white-box test reaching into `lodestone_v770::packets::chunk` directly.** The decoder works; the *client* has never delivered world data to a consumer. Three crates were blocked behind a seam nobody knew was missing.

**The worse half.** There was a standing isolation warning, `lodestone-client -> lodestone-v770 (optional)`, which I read as sloppy test placement and instructed `impl-world` to fix by relocating the test. It was not sloppiness: that test depends on the version crate **precisely because the adapter exposes no way to obtain chunks**. The dependency was load-bearing and was the last remaining pointer at a real architectural hole. Executing my instruction would have turned the isolation report green while leaving the client unable to deliver world data — and the evidence would have been gone.

Three lessons:
1. **A lint suppressed rather than satisfied is worse than the lint.** The warning was doing its job; I misread a true signal as noise and nearly ordered it deleted. Before silencing any lint, establish *why* it fires — "move the file" and "build the missing seam" look identical in the report and are opposites in reality.
2. **Per-crate test suites cannot detect that the crates don't connect.** Every crate here is well tested. The integration gap survived thousands of passing tests because no test ever consumed the public API for its intended purpose. The thing that found it was someone trying to *use* the product.
3. **Beware acceptance gates that reach past the interface.** The 225-chunk test was designed to prove the decoder and did so honestly — but it was recorded on the roadmap as though it proved delivery. **A gate should exercise the seam its phase claims to complete**, or the claim outruns the evidence.

Fix in progress: widen `ClientEvent::ChunkLoaded` to carry decoded, version-free section data; wire `handle_play` to decode and emit; then rewrite the live test to consume the public event stream while still asserting **225 chunks and zero trailing bytes** — a weaker assertion through a nicer API would be a regression. The isolation warning then clears *because the dependency is genuinely gone*.

**12.25 A Cargo feature is a request, not a guarantee.** `lodestone-assets`' `std::fs` usage is confined to native discovery (§12.22), and I instructed `impl-assets` to gate it behind a default-on `native` feature so a wasm build couldn't reference it. `wasm-spike` caught that this **cannot work**, and I verified:

```
$ grep -n "lodestone-assets" crates/lodestone-render/Cargo.toml
crates/lodestone-render/Cargo.toml:37:lodestone-assets.workspace = true    # default features ON
```

`lodestone-render` is in the browser's dependency graph and takes the crate with default features on. **Cargo unifies features per crate across the whole graph, so `web/` setting `default-features = false` is silently overridden.** We would have shipped a gate that reads in every manifest as though it protects us and delivers nothing — a *false* guarantee, which is worse than an acknowledged gap because it stops anyone looking. Corrected to `#[cfg(not(target_arch = "wasm32"))]`, which is immune to unification and turns the runtime death into a compile error. **Rule: for a hard architectural boundary use `cfg(target_arch)`; features are advisory.**

**12.26 The asset pre-fetch is 4.9 MiB, not 39 MiB — measured in a browser at full scale.** I warned that pre-fetching the vanilla pack meant a ~39 MiB download. That is the whole `client.jar`, dominated by JVM bytecode we never fetch. `wasm-spike` measured the real renderable corpus in Chrome:

```
REAL corpus: 4.9 MiB zip, 10967 entries listed (1371 block textures)
  fetch 19 ms · from_bytes 231.3 ms · list 21.8 ms · read 0.20 ms · stone.png OK
```

Central-directory indexing of ~11k entries: **231 ms one-time**. Whole-corpus `list`: **22 ms**. Per-file `read` (zlib-rs inflate): **sub-millisecond**. This **retires the trimmed-pack workstream** and independently confirms that the sync byte-only `ResourceSource` (§12.22) is fast enough at full real scale — the async refactor would have bought nothing measurable.

**12.27 Two renderer bugs found by the first real consumer.**
- **`generate_isolated_mips` panics on any non-power-of-two atlas width.** `texture.rs:293-299` clamps the level rect (`lw = (width >> level).max(1)`) but not the sprite origin (`sx = sprite.x >> level`). For a 160×16 strip at level 6: `lw = 2`, sprite at `x=144` → `sx = 2` → writes column 2 into a 2-wide buffer. **Every mip test in the crate uses a 16×16 single-sprite atlas** — power-of-two *and* one sprite, so `sx` is always 0 and the failure cannot be expressed. Full green coverage over a function that panics on the first realistic input; a suite that only exercises the degenerate case is close to no suite at all.
- **`mesh_greedy` is incompatible with atlas UVs.** Greedy merging produces spans whose tile UVs exceed `[0,1]` and therefore sample neighbouring sprites. This is a genuine design tension, not a patch: greedy meshing is the plan's largest geometry reduction, and atlasing is mandatory because WebGPU guarantees only 256 array layers (§12.14). Resolutions are shader-side tile repeat with an explicit sprite rect, same-sprite-only merge runs, or `texture_2d_array` where layers permit — **to be decided on measured merge factors on real terrain**, and coupled to the packed-vs-wide vertex decision since a per-quad sprite rect changes the byte budget.

**12.28 The version-isolation claim survived its maximum-stress test: two incompatible chunk wire formats, one version-free store, zero leakage.** 1.8.9's chunk format is not a variant of the modern one — it's a different design: a section bitmask, **flat little-endian `u16` arrays** of `(blockId<<4)|meta` with no palette, inline per-section nibble light, a 256-byte 2-D biome footer, and `map_chunk_bulk`, which has no modern equivalent. I ran the live test myself against `lodestone-mc189`:

```
=== LIVE 1.8.9 CHUNK DECODE REPORT ===
columns decoded       : 81   (1 via map_chunk, 8 via map_chunk_bulk)
trailing bytes/column : 0 (ensure_empty passed on every column)
flow control          : none (1.8 has no chunk_batch ACK; all chunks pushed)
flat-world layers y0-3: [y0:112] [y1:48] [y2:48] [y3:32]
test ... ok  (8.14s)
```

Bedrock(112)/dirt(48)/dirt(48)/grass(32) is the exact vanilla 1.8 superflat, and a big-endian or YZX-transposed decoder produces garbage rather than clean bedrock — so this one assertion pins byte order *and* index order simultaneously.

**The headline: no modern concept leaked in and no new seam was needed.** v47 never calls `PalettedContainer::decode`; it builds containers via `from_values` from flat arrays, so the `LongArrayFraming::{Prefixed, FixedSize}` knob — a packed-long concern that exists precisely for this kind of divergence — is **never consulted**. The version-free storage absorbed a fundamentally different wire format unchanged.

Two findings worth keeping:
- **1.8 block data is little-endian**, the one place Minecraft breaks its own big-endian convention. `Reader::u16()` would have scrambled every id *invisibly* — round-trip and "did it error?" tests all pass on scrambled ids. Only the known-block-at-known-Y detector catches it.
- **`minecraft-data` has no authoritative answer here**: `chunkData` is an opaque length-prefixed `buffer` and `map_chunk_bulk.data` a bare `restBuffer`. Packet *ids* have a community oracle; the byte geometry inside does not. Another instance of the standing rule — prefer interrogating the real thing over any dataset.

**Open seam (relayed to `impl-world`): 1.8 biomes are 2-D and our storage is 3-D.** 1.8 sends 256 bytes, one biome per XZ column, constant over Y. `ChunkSection` only offers a 3-D 4×4×4 container, so v47 currently **fabricates** one — down-sampling 16×16→4×4 (discarding 15/16 of the horizontal resolution the server actually sent) and replicating across four Y layers (inventing vertical structure that never existed). It decodes cleanly and is harmless on a flat world, but it is lossy in one direction and fictional in the other. Wants either a column-level 2-D biome store or a constructor accepting a uniform/2-D biome source.

Also settled: v47's live chunk test lives in `crates/protocol/v47/tests/live_chunk.rs` behind a `live-chunk` feature, so deleting the version deletes its live test too. `check-deletable v47` → cleanly deletable, 0 blockers.

**12.29 Two mob-AI beliefs falsified by the real server, both of which would have produced silently-wrong tests.**

- **A 1-block solid wall does not block a mob — it jumps it.** Vanilla jump height ≈1.25 > 1.0. A test asserting "the zombie detours around a stone wall" is asserting something false; the real zombie walked straight over it at constant z. The correct unjumpable obstacle is a **fence**, whose 1.5 collision top defeats both the 0.6 auto-step and the 1.25 jump — while eye-height LOS (~1.74) still passes over it, so the target is *still acquired* and the mob genuinely has to path around. This is the same 1.5 constant that §12.10 established for collision and that `impl-entity` earlier caught being mis-documented as `0.0..=1.0`. **One number, three separate near-misses.**
- **`Invulnerable:1b` makes an entity un-targetable.** Vanilla's `TargetingConditions` rejects invulnerable entities outright, so an "invulnerable lure" is invisible to the AI and the mob just random-strolls. This presented as a *detour test freezing at the origin* — a symptom that points at the pathfinder, while the fault was in the test fixture. Use `NoAI:1b` (stationary but mortal) instead.

Both were found by summoning real mobs and watching what they actually did. Neither is discoverable from source reading alone at reasonable cost, and both produce **plausible-looking green tests** if guessed wrong.

**Live AI divergence, measured rather than eyeballed** (real server zombie vs our `PathFinder`, detour around a fence): real max|z| = **4.46**, ours = **4**, **Δ = 0.46, detour side agreement = true**. Open-ground pursuit: 68 ticks, mean ground speed **≈0.118 blocks/tick** from a `movement_speed` attribute of 0.23 (≈half, after per-tick acceleration and friction). Exact positions are **not** checkable — server-side RNG with an unobservable seed — so the honest claim is invariants (reachability, detour side, deviation bound), stated as such rather than dressed up as position equality.

Also settled: vanilla runs **two** selectors (target selector ticked before goal selector), with target goals on `Flag::TARGET` and movement goals on `MOVE`/`LOOK`/`JUMP`, so they never contend. Goal scheduling is **not** a simple priority queue — flags plus `isInterruptable` decide preemption, and getting it wrong yields mobs that look *almost* right, which nobody reports.

Despawn rules: the two distance gates must **not** be folded — a mob at 40 blocks with a fresh timer is Kept-not-Reset, so it keeps ageing toward the 600-tick random-despawn threshold. Caps 70/10/15/5/20/−1 per category, `mob_cap = max × chunks / 289`, despawn 128 instant / 32 immune.

**12.30 Worldgen is DATA, decisively — and the proof that it's data is better than the claim.** The shape-determining question for Phase 7 is answered: vanilla 26.2 ships its noise router as **963 JSON files** under `data/minecraft/worldgen/` — 35 `density_function`, 63 `noise`, 7 `noise_settings`, 66 `biome`, 226 `configured_feature`, 262 `placed_feature`, 188 `template_pool`, 54 `structure(_set)`, 40 `processor_list`, 4 `configured_carver`. Overworld `final_density` is a DAG of ~28 node types over ~24 named noises. **So the generator is a ~700-line version-free interpreter over per-version JSON, not ~10k lines of ported code** — exactly §3's split, with the engine version-free and the data in the version crate.

**The methodological point is the more valuable one.** "I read the source and the JSON looked complete" is unfalsifiable. Instead: the Rust interpreter reads **disk JSON**, while the oracle evaluates the **running server's live `RandomState` router** (`SharedConstants.tryDetectVersion(); Bootstrap.bootStrap(); VanillaRegistries.createLookup()`). If disk JSON were an incomplete picture, the two would diverge. They agree **34048/34048 = 100.0000%** block-for-block over a whole contiguous 16×16×64 region. A claim that could have failed, and didn't.

Verified by me: `rng_matches_jvm_bit_for_bit`, `noise_matches_jvm_bit_for_bit`, `density_router_matches_jvm`, `noise_router_matches_jvm_over_whole_region` — all green (2.30s for the region test). Layer counts: RNG 663/663, noise 1224/1224, density 5120/5120, region 34048/34048, every one bit-exact via `Double.doubleToRawLongBits`, element-wise, **naming the divergent key** on failure.

**Honest remaining scope, volunteered rather than extracted:** `NoiseChunk` **cell interpolation is not built** — overworld cells are 4×8×4 and vanilla samples `final_density` *only at cell corners* then trilerps, so a bit-exact router point-sampled per block is still **not** vanilla's block field. Also unbuilt: aquifers, surface rules, carvers, features. `WorldgenChunkSource` is labelled precisely as the point-sampled router sign-field rather than "vanilla blocks." That distinction is exactly the kind that would otherwise be discovered *after* three more stages were built on top of it.

**Open seam:** the protocol stack is **decode-only**. There are no client-bound encoders (`join_game`, registry data, `level_chunk_with_light`) or server-bound decoders, so the integrated server cannot serve a real client and Phase 7's acceptance test is blocked. `lodestone-server` defines a `ServerProtocol` seam (mirror of `VersionAdapter`) awaiting an implementation; the hermetic `integrated_memory` test proves the plumbing over `memory_pair()` with a stand-in wire format, clearly labelled as such. Queued behind the §12.24 chunk-emission surgery in the same file.

**12.31 A self-authored oracle can encode your own misunderstanding — and then it agrees with you.** This is the most important testing lesson of the project so far, and it survived a bit-exact three-way cross-validation.

`do_move` set `velocity = resolved` (the collision-clamped distance). Vanilla does **not**: it keeps `deltaMovement == delta` through `collide()`, and only `restituteMovementAfterCollisions` rewrites velocity — that is what zeroes a blocked axis (restitution 0) or reverses it into a bounce. **The two formulations coincide exactly when contact is flush (gap 0), and all 16 existing scenarios happened to be flush contacts.**

So: `MoveOracle.java` (JVM), the golden vectors, and the Rust implementation **all three carried the same shortcut**, all agreed with each other bit-for-bit across 16 scenarios and 12 previously-verified cases, and all three were wrong about real vanilla on any fractional landing. The oracle was not an independent authority — it was a transliteration of the same misunderstanding. The bug surfaced only when `restituteMovementAfterCollisions` was implemented properly and `sprint_jump` (which contains a fractional landing) went red.

**The generalisable rule: a JVM oracle validates the behaviour you chose to model in it, not vanilla's behaviour.** Cross-validation between implementations detects transcription errors; it cannot detect a shared misreading of the source. Defences: derive oracle scenarios from *vanilla's own call graph* rather than from the Rust design, and deliberately include cases (fractional gaps, off-grid contacts) where competing formulations must diverge. **Agreement across ports is weak evidence when the ports share an author.**

Related correction from the same pass: `getBlockSpeedFactor`/`getBlockJumpFactor` must query `blockPosition()` (floor of feet) directly, **not** the `friction_block` `fy+1` derivation, which diverges whenever the y-fraction exceeds 0.5.

Physics now at **55 tests / 21 JVM-bit-identical scenarios** (verified: "All 21 shared scenarios agree bit-for-bit with the real JVM"), adding soul-sand speed factor 0.4, Jump Boost II (+0.2F), honey jump factor 0.5, slime restitution 1.0, and sneak-cancels-bounce (sneak has a **dual** role — it zeroes base restitution *and* vetoes the block-bounce branch). The `f64::from(0.98f32)` float-widening trap in the drag lerp and `getOnPos(0.2)` for block bounciness are both reproduced.

**Knockback: nothing to reproduce.** It is server-authoritative velocity — `set_entity_motion` and attack impulses *write* velocity, then normal integration runs. The integrator already handles whatever velocity it's handed, so there is no separate knockback model. Worth recording because it looked like a workstream and isn't.

Deferred honestly: honey wall-slide and cobweb/powder-snow need a "stuck movement" hook absent from the `CollisionView` seam (to be specced, not guessed); Elytra warrants its own oracle batch; the 1.8 fluid/input arms remain `unimplemented!()` with `should_panic` tests guarding against a silent modern-math run under a 1.8 label.

**12.32 I asserted an unverified fact to an agent, and caught it only by checking afterwards.** While briefing `impl-client` I stated that the §12.24 chunk-delivery seam "has landed — `ClientEvent::ChunkLoaded` now carries real section data, verified through the public API," and told it a world read-model was therefore buildable. I had not checked. It was false:

```
$ grep -n "packets::chunk\|ChunkLoaded" crates/protocol/v770/src/adapter.rs
(no output)
$ grep -n -A4 "ChunkLoaded" crates/lodestone-model/src/event.rs
169:    ChunkLoaded {
170-        /// Chunk position.
171-        pos: ChunkPos,
172-    },
```

Position only; the adapter still never imports the decoder. Had the agent taken it at face value it would have designed a world-query API against an event structurally incapable of supplying blocks — and the resulting code would have looked correct and returned nothing, which is this project's signature failure mode.

Corrected within a minute by an explicit retraction to the agent. Worth recording for three reasons:

1. **The orchestrator is not exempt from the rule.** Four times this session an agent has refused or corrected a confidently wrong instruction of mine (§12.21 fictional wgpu feature, §12.22 non-existent fs wall, §12.25 unenforceable Cargo gate, §12.24 the lint I nearly ordered suppressed). This is the same class, with the difference that no agent caught it — I did, on a post-hoc check. **A brief is not more reliable than the code it describes just because I wrote it.**
2. **Status claims decay fastest.** "X has landed" is the most dangerous sentence in a multi-agent brief, because it's true eventually and false now, and it licenses downstream work that is expensive to unwind. Facts about *design* survive; facts about *progress* need re-verification at the moment of use.
3. **The recovery was worth more than the error cost.** Retracting turned the agent into an *early* reviewer of a seam that hasn't frozen yet — it had just audited the client's event path for payload cost, so its opinion on whether `ChunkLoaded` should carry owned section data, an `Arc`, or a handle into a shared store is the best-informed one available, and it now arrives *before* the shape sets rather than after.

Standing rule added: **when a brief states that another agent's work has landed, verify it in the same breath, or state it as "assigned, not confirmed."**

**12.33 Ruling: chunk data must NOT travel through the event channel. The client owns authoritative world state; events are notifications.**

Four agents were blocked on the shape of the widened `ClientEvent::ChunkLoaded` (§12.24). The obvious fix — put the decoded chunk in the event — is wrong, and the reason is a coupling that only becomes visible when two independently-correct facts are put side by side:

1. `lodestone-world` already has the right home: `World { chunks: HashMap<ChunkPos, LoadedChunk> }` with `load`/`unload`/`get`/`contains`, and `LoadedChunk { column, light, heightmaps, block_entities }` plus `heap_bytes()`.
2. `impl-client` audited the event path and found `Driver::emit()` moves each event into a **bounded** `mpsc::channel(256)`, and that keep-alives are encoded and sent *before* `events.send()` specifically so a stalled consumer cannot drop one.

Put together: the event channel is bounded, so a slow consumer applies **backpressure to the driver**. If chunks flow through it, then *world correctness becomes coupled to consumer liveness* — a consumer that pauses stalls packet processing, and any future decision to drop-on-full (the natural fix for that stall) silently corrupts the world, because a missed `ChunkLoaded` is an unrecoverable hole with no resync. **World state must never be reconstructible only from a lossy or backpressuring stream.** A late-attaching consumer has the same problem: it can never learn about chunks that loaded before it subscribed.

**The shape:**
- The version adapter decodes into a version-free `LoadedChunk` and **applies it to a `World` the client owns** — the adapter gets a world sink in its context rather than returning chunk payloads.
- `ClientEvent::ChunkLoaded { pos }` stays **exactly as it is** — a notification that something changed at `pos`.
- Consumers (renderer, bot read-model, shell) **query the world** rather than accumulating it. Queries are idempotent, replayable and correct regardless of when the consumer attached or how far behind it is.

This is also how the real client works, it keeps the per-event cost at "a couple of ints" (preserving `impl-client`'s no-clone/backpressure analysis), it stores each chunk exactly once instead of once per consumer, and it means `heap_bytes()` gives a single honest number for world memory.

**Cost, stated honestly:** it changes the `VersionAdapter` signature, so all three version crates move together — the largest cross-crate change of the session. Accepted, because the alternative embeds a silent-corruption path in the one crate the whole design calls "the product."

**Note the §12.24 lesson survives intact.** The gate must still be *the client delivering world data through its public API* — now phrased as "connect, and query 225 chunks out of `World` via the client's public surface, with zero trailing bytes on decode." Keeping the strong assertion while changing the mechanism is the point; a weaker assertion through a nicer API would be a regression.

**12.34 Lodestone cannot join any real server, and every live test all session has been blind to it.** Auditing for unassigned capability rather than for bugs, I found that the entire online-mode login path does not exist:

```
$ ls crates/                      # no lodestone-auth, no lodestone-audio
$ ls crates/lodestone-net/src/    # codec connection error lib transport ws_native ws_web
$ grep -rln "cfb8\|Cfb8" crates/ --include=*.rs
(only the EncryptionRequest packet structs in v47/v340/v770 — no cipher anywhere)
$ docker exec lodestone-mc262 grep ^online-mode server.properties
online-mode=false
$ docker exec lodestone-mc189 grep ^online-mode server.properties
online-mode=false
```

The `EncryptionRequest` **struct** is defined and decoded in all three version families, but there is **no AES-128-CFB8 in the codec, no RSA step, no session-server call, and no authentication crate at all**. Every Docker server used all session is `online-mode=false`, so the encryption branch has **never once executed** — in any test, in any crate, at any point.

**This is §12.24's shape again, one level up.** There, thousands of green tests coexisted with a chunk that had never crossed the public API. Here, a fully green multi-version protocol stack coexists with a mandatory code path that has never run — and the blind spot came from the *test environment* being uniformly convenient rather than from any individual test being weak. Every real public vanilla server runs online-mode, so against the user's actual requirement ("compatible with regular vanilla Minecraft servers") the client currently connects to nothing.

**Credit where it's due: the failure is loud, not silent.** `v770/src/adapter.rs:120` decodes the packet and then returns `AdapterError::Unsupported("encryption / online-mode authentication (login hello) is not yet implemented; connect to an offline-mode server")`. That is the correct handling of an unimplemented mandatory path — it is why this was findable at all, and it is the opposite of the plausible-looking stub this project keeps warning about.

**The testing problem, and its answer.** We have no Microsoft credentials, so a full authenticated join cannot be tested end to end. But almost all the risk is in code that *can* be validated against external authorities:
- Minecraft's server-ID hash is a notoriously non-standard SHA-1 — twos-complement, rendered as signed hex with a leading `-` for negative digests. Published vectors exist (`Notch` → `4ed1f46bbe04bc756bcb17c0c7ce3e4632f06a48`, `jeb_` → `-7c9d5b0044c130109a5d7b5fb5c317c02b4e28c1`, `simon` → `88e16a1019277b15d58faf0541e11910eb756f6`). These are **not self-authored**, which is precisely what §12.31 says our oracles otherwise lack.
- AES-128-CFB8 round-trips against standard vectors, and the stream-cipher property that matters here: the cipher is **stateful across the whole connection**, not per packet.
- Against a real `online-mode=true` server, the *failure mode itself* is evidence: reaching "Failed to verify username" proves the RSA/shared-secret/cipher handshake was accepted and only the session-server lookup failed — far deeper validation than never attempting it.

**12.35 `cfg(target_arch)` does not make a wasm filesystem call a compile error — I claimed it did, and the agent disproved it by experiment.** Correcting §12.25's fix, I told `impl-assets`: *"on wasm those paths cannot be referenced, so the runtime death becomes a compile error."* That is false. `impl-assets` tested it rather than accepting it, and I reproduced the test myself:

```
$ printf 'fn _sneaky(){ let _=std::fs::read("x"); }' >> crates/lodestone-assets/src/manager.rs
$ cargo build -p lodestone-assets --target wasm32-unknown-unknown
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 14.83s   ← COMPILES GREEN
```

**`std::fs` compiles for `wasm32-unknown-unknown` and fails only at runtime** — precisely the `Instant::now()` class. So `cfg(not(target_arch = "wasm32"))` genuinely removes the *existing* `DirectorySource`/`ZipSource::open` entry points (a real win: the browser cannot call them), but it does nothing about a *newly added* ungated `fs::read` elsewhere in the crate. My "compile error" guarantee was a third false guarantee stacked on the same question — Cargo feature (§12.25, no protection), then `cfg` (real but partial), each time asserted with more confidence than the evidence supported.

**The agent's fix is better than what I asked for, because it's structural rather than declarative:**
1. All `std::fs` confined to a single wholly-gated file, `src/source_native.rs` (verified: `grep -rln "std::fs::" crates/lodestone-assets/src/` returns exactly that one file).
2. A confinement guard in `scripts/wasm-check.sh`: any `std::fs::` outside `source_native.rs` fails the check, naming file and line.

Verified by me with the same injection:
```
  lodestone-assets                   PASS
  lodestone-assets fs-confinement    FAIL
      crates/lodestone-assets/src/manager.rs:118:fn _sneaky(){ let _=std::fs::read("x"); }
  - lodestone-assets: std::fs used outside the gated source_native.rs
```
Green compile, caught anyway. Reverted after. `lodestone-assets` now at **239 hermetic + 16 real-jar**.

**The general principle, which is the durable part:** when the type system cannot express a constraint, don't pretend it can — **make the constraint checkable and then check it in CI**. Confinement + a grep guard is unglamorous next to "the compiler enforces it," but it is *actually enforced*, and it fails loudly with a file and line. The three-round arc here is the lesson: the desire for the boundary to be free (a feature flag, then a `cfg`) produced two guarantees that read as ironclad in review and delivered partial or zero protection. **A gate that isn't exercised is a comment** — so the guard's own failure path was demonstrated, not assumed.

Also settled: our atlas is a tidy **1024×1024**, so `impl-render`'s NPOT `generate_isolated_mips` panic is not reachable from our own asset output — it remains a real bug for third-party packs, not a live one for the default path.

**12.36 The meshing lifetime constraint: `World::get` must not hand out a borrow that holds a lock.** `impl-render` accepted the §12.33 ruling and then derived a requirement from it that I had not anticipated, and it is the sharpest constraint yet on the world query surface.

Meshing holds a **27-section neighbourhood for the entire duration of a mesh**, and meshing is the slow CPU step. So if `World::get` returns a borrow whose lifetime requires a world lock to be held, **chunk loading serialises behind meshing** — which re-introduces, one layer down, exactly the consumer↔world coupling that §12.33 removed. The ruling would have been defeated by its own implementation.

**Requirement, adopted:** per-chunk `Arc<LoadedChunk>` from `get()`, or an arc-swapped immutable world snapshot. The mesher takes 27 `Arc`s, releases immediately, and meshes off them while loads continue. `ChunkSectionView` borrows *from the Arc-kept chunk*, so there is still no clone of section data. A stale snapshot is explicitly fine — the next `ChunkLoaded { pos }` re-dirties and it re-meshes. This is §2's "lock-free snapshots" arriving as a concrete consequence rather than an aspiration.

Two claims it checked rather than asserted, both already true — worth recording because they mean the ruling costs the renderer nothing:
- **`SectionNeighborhood` is already `[3][3][3]`**, with `cell(x,y,z)` routing via `div_euclid`/`rem_euclid` and returning `Cell::EMPTY` for unloaded slots. The 27-not-6 requirement was designed in from the start, with the module doc explaining why (AO samples land in edge- and corner-adjacent sections).
- **The World→mesher bridge already exists**: `ChunkSectionView::new(&ChunkSection, classifier, light)` implements `SectionView`, and that `&ChunkSection` is exactly what `World::get(pos).column.section(i)` returns. "Mesh from world queries" is **lifetime plumbing, not a redesign.**

It also strengthened the ruling's own justification: a renderer that folded `ChunkLoaded` *payloads* could not work at all, because loading column P dirties P's mesh **and the boundary meshes of its 8 horizontal neighbours** (their edge/corner AO now samples into P). A payload-accumulating consumer would have to reconstruct those neighbours from its own history; a queryable world simply answers. `ChunkLoaded { pos }` as a dirty-region signal is precisely what a mesher wants.

**12.37 The confinement-guard pattern propagated, and the second adopter improved on it.** After §12.35, `impl-render` applied the pattern to its own crate — and its motive is the notable part: it found that `frame.rs` *claimed* the injectable-clock seam "stops `Instant::now()` from silently leaking into new tick code," **and nothing enforced that claim.** A doc comment asserting an invariant is the same failure as a Cargo feature asserting a boundary.

Its version generalises §12.35's: a **hermetic unit test** (`no_wasm_trap_symbols_are_confined`) that runs in the default `cargo test` rather than only in `wasm-check.sh`, scans `src/`, strips comments so docs may still name the symbols, and bans the whole trap family — `Instant::now`, `std::fs::`, `std::thread::spawn`, `tokio::time` — outside allow-listed files. Nice touch: the patterns are assembled by concatenation so the guard's own source never contains the contiguous banned substring and cannot flag itself.

Verified by me with an injection:
```
$ printf 'fn _sneaky() { let _ = std::time::Instant::now(); }' >> src/caps.rs
$ cargo test -p lodestone-render --lib no_wasm_trap_symbols_are_confined
  wasm runtime-trap symbols found outside their confined module
  (these compile green but panic in a browser):
  crates/lodestone-render/src/caps.rs:171:fn _sneaky() { let _ = std::time::Instant::now(); }
  test result: FAILED
```
Reverted; `lodestone-render` now **126 hermetic + 7 GPU**.

Its framing of why both layers exist is the durable part: **`FramePacer`'s injectable `TimeSource` protects the current code (the hazard becomes inexpressible); the guard protects the next edit.** Ideal where achievable, guard as the durability layer underneath — both, not either.

**12.38 Vanilla's own RCON client is fragile in a way that silently breaks tooling — worth knowing project-wide.** `impl-game` hit this driving its click oracle and traced it into the decompiled source: `RconClient` (lines 47–56) opens a fresh `BufferedInputStream` and performs **exactly one `read()` per request**, then closes the socket unless `pktsize == read - 4`.

Consequence: sending the RCON frame as **two** `write_all` calls (length, then body) sometimes delivers the 4-byte length alone into that single `read()`, giving `10 > read` and a **silent socket close after a few commands**. It presents as "RCON randomly stops working mid-test." A Python probe using one `sendall` masked it entirely — so the bug appeared only in the Rust harness and looked like a Rust bug. **Fix: write the entire frame in one call.** Every agent using RCON should assume this.

The click machine itself agrees with vanilla on **all 10 click types**, verified by me against the live creative server:
```
[left-pickup-whole] [right-pickup-half] [right-place-one] [quick-move-shift] [hotbar-swap]
[double-click-collect] [throw-drop-one] [throw-drop-stack] [left-drag-even] [right-drag-one]
=== ITEM-FUL ORACLE PASSED === (5.39s)
```
including the three-phase drags, shift quick-move and double-click-collect — the places a hand-rolled model normally diverges. **No model bug surfaced; both defects found were in the harness**, and only the server could reveal them (the other was an unsolicited join-time `container_set_content` shifting every response by one, which made scenario 1 falsely "diverge" — fixed with a post-join drain so clicks map 1:1 to responses).

This closes the gap `impl-game` volunteered last round: the oracle previously reconciled an *injected phantom* stack against real empty content, because offline survival gives an empty inventory. Now it uses real `/give`-populated stacks. The agent identified the limitation itself, then removed it.

**12.39 Chat findings, and a correct pushback on my "cheap wins" framing.** Live capture of all three clientbound chat packets, decoded version-free with **zero trailing bytes** (the alignment detector):
- **`/say` is command-sourced → `isSystem()` → `disguised_chat`, not `player_chat`** — and its sender name is **`"Rcon"`**, not "Server".
- `/tellraw` → `system_chat`; a signed player message → `player_chat`.
- The captured player UUID is a **version-3 name-based offline UUID**, which is the §12.x offline-mode landmine made directly visible on the wire.
- `Text::from_nbt` correctly resolves a **server-serialised `translate` component** through the built-in table — a real round-trip, not a hermetic one.

**Pushback accepted:** I framed chat as a cheap win. The disguised/system captures were cheap; **`player_chat` (id 65) is the single most complex packet in the game** — global index, per-sender index, optional 256-byte signature, packed signed body with last-seen, optional unsigned content, filter bitset, and a bound chat type. Calling it cheap undersold it, and the estimate would have been wrong in a way that mattered if anything had depended on the schedule.

**Honest limitation, volunteered:** a genuinely *filtered* message can't be reproduced — without Mojang's configured chat-filter service the mask is always `PassThrough`, so `FilterMask::Partial`/`FullyFiltered` stay hermetic-only. Likewise signed-secure chat needs a keyed session. Both correctly graded low confidence rather than papered over.

**12.40 The cell-interpolation gap is closed: 98304/98304 bit-exact over a whole chunk column.** §12.30 recorded an honest limitation that `impl-worldgen` volunteered rather than hid — the noise router was bit-exact, but vanilla samples `final_density` **only at 4×8×4 cell corners and trilerps between them**, so a point-sampled router, however exact, is *not* vanilla's block field. Building surface rules, aquifers and carvers on top of a point-sampled field would have produced a world that was subtly wrong everywhere, with no single test able to say why.

`NoiseChunk` interpolation now exists (`src/density/chunk.rs`, `CELL_WIDTH = 4`, `CELL_HEIGHT = 8`) and is verified per-block against the JVM:

```
$ cargo test -p lodestone-worldgen --test chunk_parity -- --nocapture
interpolated final-density whole-chunk parity: 98304/98304 = 100.0000% bit-exact
```

98,304 = 16 × 16 × 384 — every block in a full chunk column, compared as raw `Double.doubleToRawLongBits`, element-wise, with the harness naming divergent `x,y,z` coordinates on failure rather than reporting a count. Worldgen now stands at **8 tests: 3 hermetic + 5 JVM-parity layers** (rng, noise, density router, whole-region router 34048/34048, whole-chunk interpolated 98304/98304), zero failures.

The sequencing is the lesson worth keeping: the agent reported the interpolation gap *at the moment it could have claimed completion*, and the very next unit of work closed it. A gap disclosed while it is still cheap to fix costs one round; the same gap discovered after surface rules, aquifers and carvers are built on top costs all of them.

**12.41 The online-mode crypto path now works end to end — proven by the *failure* it reaches.** §12.34 found that Lodestone could not join any real server and that the encryption branch had never executed once. It now executes, against a live `online-mode=true` vanilla 26.2 server, and reaches exactly the intended stopping point:

```
$ cargo test -p lodestone-net --test online_handshake -- --ignored --nocapture
server_id=""
post-encryption disconnect reason: {"translate":"multiplayer.disconnect.unverified_username"}
test result: ok. 1 passed
```

**The measurement is the failure, and it is a strong one.** That disconnect arrived **encrypted** and decrypted cleanly. So the server accepted our RSA-wrapped shared secret, matched the verify token we echoed, switched on its cipher, and its AES-128-CFB8 reply round-tripped against ours. The only thing that failed is the session-server ownership lookup, which needs a Microsoft account we don't have. A framing or decrypt error would have meant broken crypto; a clean protocol-level "unverified username" means the crypto is right. **A fully authenticated join remains untested and is not claimed.**

This is the clearest example yet of a principle worth generalising: **when you cannot reach success, choose a failure that discriminates.** "We can't test it without credentials" would have been true and useless; "we reach the one error that can only occur after the cipher is working" converts an untestable feature into a verified one.

**Layering, both pitfalls handled.** Encryption is outermost on the wire — `encode = frame(compress(body))` then encrypt; `feed = decrypt` then buffer. One `Cfb8Cipher` per connection created once at `enable_encryption`, with separate CFB8 feedback registers per direction, key == IV == the 16-byte secret. The switchover is the driver's: write `EncryptionResponse` in cleartext, *then* enable. It lives in the sans-IO `Codec`, so the browser path inherits it free — the same property that has now paid off for assets, transport and framing.

**External vectors, independently confirmed by me** (I quoted them from memory and told the agent to treat them as hypotheses; they verified with Python stdlib, and so did I):
```
Notch  4ed1f46bbe04bc756bcb17c0c7ce3e4632f06a48  OK
jeb_   -7c9d5b0044c130109a5d7b5fb5c317c02b4e28c1  OK      ← the negative case
simon  88e16a1019277b15d58faf0541e11910eb756f6   OK      ← 39 digits, leading zero
```
Minecraft's server-ID hash is a signed SHA-1: `BigInteger.toString(16)` over the raw digest, so negatives get a leading `-` and a leading-zero digest loses a character. A naive hex digest passes `Notch` and fails the other two. Rust uses `num_bigint::BigInt::from_signed_bytes_be(&digest).to_str_radix(16)`, the direct Java analog. CFB8-AES128 checked against NIST SP800-38A F.3.7 via pyca/cryptography. **Every one of these is an authority we did not write** — the §12.31 defence applied correctly.

Notable test choices: a **per-packet-reinit-is-wrong** test that proves statefulness actually matters rather than merely asserting it, and an encryption test that feeds many packets **one byte at a time with compression on**, combining the split-read and cross-packet traps in a single case.

**Verified totals: 45 (34 `lodestone-net` + 8 `lodestone-auth` + 2 integration + 1 doctest), 1 ignored live smoke test.**

**Brief corrections from the agent:** `reqwest` 0.13's TLS feature is **`rustls`**, not `rustls-tls`, and `.form()` now needs an explicit **`form`** feature — both exactly the kind of drift the "verify versions, don't recall them" rule exists for. Dependencies pending promotion to `[workspace.dependencies]`: `aes 0.9`, `cfb8 0.9`, `rsa 0.9` (pins `rand_core 0.6`), `sha1 0.10`, `num-bigint 0.4`, `base64 0.23`, `serde 1`, `reqwest 0.13`.

**12.42 I re-asserted a falsified claim four times, because corrections were recorded but never propagated back into my briefs.** `impl-assets` has now corrected the same sentence of mine **four separate times**: I keep describing vanilla's mipmap downsample as "alpha-weighted." It is not.

`MipmapGenerator.java`'s default path is **`ARGB.meanLinear`** — an *arithmetic* alpha mean `(a1+a2+a3+a4)/4` plus a linear-light RGB mean. **No alpha weighting anywhere.** The anti-black-bleed behaviour I was attributing to weighting is a separate mechanism, **`solidify`**, a BFS flood-fill that propagates colour into fully-transparent texels before downsampling.

The technical error is minor; **the process failure it exposes is not.** Each correction was accepted, verified, and written into §12 — and then I wrote the next brief from memory and reintroduced the same wrong phrase. The plan captured the fact and did nothing to stop me repeating it. **A correction that lands in the record but not in the next instruction has not actually been absorbed.** Given that I ask every agent to treat my briefs as hypotheses, this is the strongest possible argument for why that instruction is necessary — and `impl-assets` checking every time rather than deferring is the only reason the wrong belief never reached the code.

Standing change: **before briefing on a topic with a prior §12 entry, re-read that entry rather than recalling it.** Recall is exactly what failed, four times, on a question I had already been given the right answer to.

**12.43 Two decisive asset numbers, one of which closes an open renderer decision.**

*Atlas census (real 26.2 jar):* **1269 sprites**, 1024×1024, **1 layer**, 52 animated, 1763 physical frames, 5 mip levels, base 4.0 MiB / full pyramid 5.3 MiB, build 1.27 s.

> **1269 sprites ≫ the 256 array layers WebGPU guarantees, so a per-sprite `texture_2d_array` is impossible.** A single 2D atlas is required — and it fits everywhere (1024 px ≤ WebGPU's 8192 and Metal's 16384).

That settles the greedy-vs-atlas branch of §12.28 on portability grounds rather than taste: the texture-array escape hatch was never available. Any solution to greedy meshing's out-of-range UVs must work *within* a single 2D atlas (shader tile-repeat, or same-sprite-only merges).

*Animation census:* **176 `.mcmeta`, 63 animated textures, 668 physical frames**, worst case **32** (`fire_0/1`, `nether_portal`, `water_still/flow`, `soul_fire_0/1`, `respawn_anchor_top`); histogram 2–4→31, 5–8→10, 9–16→6, 17–32→16; 8 explicit frame lists, **3 with unequal per-frame times** (`locator_bar_arrow_*` `[10,4]`, `trial_available` `[10,2,2,2,2,2]`), 23 with `interpolate=true`.

> Worst case is only 32 frames, so **all frames stay resident and blending happens in-shader**. No per-frame atlas re-upload. The decision was made by whoever had the numbers, which is the point of asking for a census instead of an opinion.

**12.44 Awkward resource packs need no special-casing, because vanilla degrades too.** I briefed that non-power-of-two and non-square packs would need handling in the atlas builder. `impl-assets` checked `SpriteLoader.stitch` instead of implementing: vanilla computes `min over sprites of min(lowestOneBit(w), lowestOneBit(h))` and, when that falls below the requested level, **drops the mip count for the entire atlas**, logging a warning per offending texture. Since `2^tz(x) ≤ x`, the `minTexelSize` term never binds, and the whole thing reduces exactly to `effective_levels = min over sprites of max_mip_level`.

**So one 13×7 sprite capping the mip levels of the whole sheet is vanilla-faithful behaviour, not a bug to fix.** Asserted against the independently-transcribed vanilla formula rather than against our own computation — the §12.31 discipline.

A second finding came from a deliberately-wrong assertion: fully-transparent cutout mips are **not** alpha-0 above the base level, because vanilla adds `bias + 0.025` to every texel each level (6→12→18→24), staying below the 0.5 cutoff and therefore invisible. The agent's initial expectation was the wrong authority and the source corrected it. An adversarial pack (13×7, 16×8, 1×1, 1×256, fully-transparent, unequal animation times) is now a committed fixture — the direct answer to the renderer's 100%-green-mip-tests-that-panicked-on-real-input failure. **`lodestone-assets`: 245 hermetic + 17 real-jar.**

**12.45 The crypto win broke the browser build, through a crate nobody named.** §12.41's online-mode encryption landed correctly and `cargo test --workspace` stayed green at **1171 passed / 0 failed**. `wasm-check.sh` did not:

```
  lodestone-net --features ws-web    FAIL
  lodestone-client                   FAIL
  lodestone-web (browser app)        FAIL
error: the wasm*-unknown-unknown targets are not supported by default,
       you may need to enable the "js" feature   --> getrandom-0.2.17/src/lib.rs:346
```

Chain traced, not guessed:
```
$ cargo tree -p lodestone-net --features ws-web --target wasm32-unknown-unknown -i getrandom@0.2.17
getrandom v0.2.17 └── rand_core v0.6.4 ├── rand v0.8.7 → lodestone-net
                                       ├── rsa v0.9.10 → lodestone-net
                                       └── signature v2.2.0 → rsa v0.9.10
$ grep -A1 '^name = "getrandom"' Cargo.lock | grep version
version = "0.2.17"   version = "0.3.4"   version = "0.4.3"
```

**Three `getrandom` majors coexist in one tree.** §16 records the wasm randomness fix as landed and verified — and it is, *for 0.4*, via `uuid`'s `js` feature. `rsa 0.9`'s `rand_core 0.6` pin drags in **0.2**, whose wasm opt-in is a separate mechanism entirely that the 0.4-era fix cannot cover.

**The generalisable shape: nothing anyone edited mentions `getrandom`.** `rsa 0.9` was the correct, obvious choice; the breakage arrives via a crate never named, at a major never chosen, on a target the author had no reason to build. Same family as §12.25 (feature unification silently defeating a gate) and §12.35 (`std::fs` compiling green for wasm) — a boundary that looks enforced and isn't, this time through the dependency graph rather than the type system.

**The guard worked; it just wasn't run.** `wasm-check.sh` named the exact crates at a cost of one command, and `cargo test --workspace` is structurally blind to the whole class. **Standing rule: run `wasm-check.sh` whenever a dependency is added or bumped** — dependency changes are the only way this class enters the tree.

Fix assigned with a preference for reducing duplicate majors (does `rand 0.8` still need to be a *direct* dep?) over the conventional target-gated `getrandom_02 = { package = "getrandom", features = ["js"] }` paper-over.

**12.46 Arrow trajectory is bit-exact against the live server, and the test cannot pass by drift.** Verified by me on `lodestone-entity` (117 lib tests):
```
sim(18) err=4.982e0   sim(19) err=2.479e0   sim(20) err=4.823e-6   sim(21) err=2.454e0   sim(22) err=4.883e0
```
The assertion demands both the razor match **and** that ±1 tick is >0.3 blocks off, so a slowly-drifting integrator fails. This is §12.41's discriminating-failure principle applied to a *success* case: the neighbours are the control.

Findings that would each have been silent near-misses:
- **Throwable and arrow use different operation orders** — throwable `gravity→drag→move` (g=0.03, drag 0.99/0.8), arrow `move→drag→gravity` (g=0.05, drag 0.99/0.6). Folding them into one integrator yields a plausible trajectory that is wrong for one of the two.
- **`tick step N` does not advance entity physics on this server; only `tick sprint N` does.** And a `tick sprint 1` used for entity *registration* silently consumes a tick, presenting as a phantom "+1 tick offset." Same family as the §12.18 summon/tick race — add to the standing live-test hazards.
- Explosion `getSeenPercent` step is `1/(size·2+1)`, so **bigger boxes sample at *more* points** (729 vs 64). The agent's first test asserted the reverse and the counting test caught it.

Brain observability, volunteered honestly: brain working memory is **not NBT-serializable**, so it cannot be read or injected over RCON; and idle stroll is *architecture-agnostic* (a goal-pig and a brain-goat stroll identically), so Brain machinery must be proven hermetically — the live test can only confirm emergent timing. Census: 20 Brain mobs / ~50 Goal mobs / 158 registered types.

**12.47 Audio: validated against three independent Vorbis implementations, and a silence trap that would have made the test worthless.** `lodestone-audio` delivered at **42 tests**, `lewton 0.10.2` (pure Rust) + `cpal 0.18.1` (native-only, `cfg`-gated), sample-driven clock with **no `Instant::now()` anywhere**.

Decode validation deliberately avoided self-comparison per §12.31: **libsndfile encodes** the fixture, **ffmpeg decodes** the golden PCM, **lewton** is under test — `max_abs_diff 3.1e-5`, `rms 1.8e-5` (≈1 LSB from the i16→f32 path). The test has teeth: negated, channel-swapped, and one-frame-shifted goldens are each asserted to **fail** the tolerance.

**The trap worth keeping: a genuinely-silent vanilla ogg gave a worthless all-zeros "match."** Two silent buffers agree perfectly. Guarded with a `peak > 0.3` assertion on the fixture — the audio analogue of §12.31's flush-contact coincidence, where the comparison is satisfied by degeneracy rather than by correctness.

**Correction to my brief: `SoundSource` has 11 buses in 26.2, not 10 — I omitted `UI`.** Parity transcribed with call-site citations: range `max(instanceVolume, 1.0) × attenuationDistance` (default 16); `AL_LINEAR_DISTANCE` rolloff 1.0 ref 0.0 reducing to `gain = max(0, 1 − dist/maxDist)`; MASTER is **not** squared; pitch clamped `[0.5, 2.0]`; only MONO spatialises, stereo plays flat with no downmix.

Honestly graded as *not* parity: **panning geometry**, because vanilla delegates stereo placement to OpenAL-Soft's HRTF. Equal-power panning is documented as an approximation rather than claimed exact — the right call, and the kind of scoping §12.39 rewarded.

The confinement-guard pattern (§12.35, §12.37) has now propagated to a **third** crate, in its strongest form: `Instant::now(` banned crate-wide with an **empty allowlist**, so "audio never touches wall-clock time" is a checked invariant rather than a promise. Both guards were demonstrated failing under injection before being restored.

**12.48 Chunk data crossed the client's public API for the first time — and the ruling is half-applied.** `impl-shell` connected the playable binary to the live 26.2 server and received 9 columns. Genuine milestone. But it also found the code contradicting itself, which I confirmed:

```
$ grep -n -A6 "ChunkLoaded" crates/lodestone-model/src/event.rs
173:    ChunkLoaded {  175: pos: ChunkPos,  179: column: lodestone_world::ChunkColumn,
```
while `driver.rs`'s doc states the column *"is moved straight into the read-model's world store and **not forwarded** through the bounded event channel."* Both cannot be true; the live run proves the enum wins. The store half (the hard half) is built — `ClientHandle` now has `block_at`, `is_chunk_loaded`, `wait_for_chunk`, `player`, `entities` over `Arc<RwLock<Inner>>` — but the "no widening" half has not landed.

**Two seam gaps that block live terrain, reported rather than worked around:**
1. **No bulk read.** The only query is per-block `block_at`. One 27-section meshing neighbourhood through it is **16³ × 27 ≈ 110,000 locked calls per section**.
2. **The read-model drops light** — it stores `ChunkColumn` (block states only), not `LoadedChunk`. Meshing without light walks straight into §7's trap where **a face samples lighting from the air it faces into, so unlit air renders every surface black** — correct mesh, correct pipeline, silent wrong output, caught only by pixel readback.

`impl-shell` declined to build a live-terrain proof over `block_at`, on the grounds that it would be a 110k-call workaround that renders black. **That is the correct call and the behaviour worth reinforcing** — a green "live terrain" screenshot obtained that way would have been this project's signature failure mode dressed as a milestone. It destructures `{ pos, .. }` and ignores the payload, so it survives the fix unmoved.

Resolution dispatched as one coherent change: strip `column`, and expose `chunk(pos) -> Option<Arc<LoadedChunk>>` — `Arc` per §12.36 so no lock-bound borrow escapes, `LoadedChunk` so light comes with it.

**Process cost now measurable:** `impl-shell` reports the `crates/lodestone-*` glob was transiently broken **five times in a single turn** by sibling agents mid-edit. It waited and retried rather than touching others' crates — correct — but it blocks every agent's `cargo` for minutes. Mitigation is to land cross-crate signature changes as one compiling unit rather than a sequence of intermediate states.

**12.49 I specified the wrong `Arc` granularity, and caught it by working through block updates.** The §12.33 ruling landed cleanly — verified myself: `ClientEvent::ChunkLoaded { pos }` narrowed, `world: Arc<RwLock<World>>` behind a `WorldSink` in the client, adapter arity migrated across all three families, `cargo build --workspace` clean. `lodestone-shell` absorbed the change with **zero edits** because it had destructured `{ pos, .. }` in advance — a good demonstration that the tolerant pattern is worth adopting before a known change lands.

The remaining gap was the bulk accessor:
```
$ grep -n "chunks:\|pub fn get" crates/lodestone-world/src/world.rs
 91:    chunks: HashMap<ChunkPos, LoadedChunk>,
121:    pub fn get(&self, pos: ChunkPos) -> Option<&LoadedChunk>     ← lock-bound borrow
```
Exposing that shape would serialise chunk loads behind meshing (§12.36) — the ruling defeated one layer below where it was made. So I specified `HashMap<ChunkPos, Arc<LoadedChunk>>`.

**That was also wrong.** A block update touches one block; with a per-*chunk* `Arc`, applying it means `Arc::make_mut` on the whole `LoadedChunk` — up to 24 sections of paletted storage plus light and heightmaps — **every time any consumer holds a reference**, which during active rendering is essentially always. Block updates are frequent (neighbour breaks, pistons, redstone). That is a per-block cost proportional to a whole column, violating the constraint I had set myself: *a block update must not require rebuilding a column.*

**The `Arc` belongs at section granularity**, `LoadedChunk { sections: Vec<Arc<ChunkSection>>, … }`. Three independent things line up, which is usually the signal the granularity is right:
- **Matches the update unit** — `Arc::make_mut` on exactly one section; paletted sections are small (the layer that took 830 MB naive down to a measured 77.6 MiB), so copy-on-write is bounded rather than column-proportional.
- **Matches what the mesher asks for** — `SectionNeighborhood` is already `[3][3][3]` *of sections* and `ChunkSectionView::new(&ChunkSection, …)` already borrows one section. The mesher clones 27 `Arc`s, drops the lock immediately, meshes off a stable snapshot. No section bytes copied.
- **Matches the invalidation unit** — re-meshing is already per-section, so a stale `Arc` is scoped to exactly what gets re-dirtied.

Open question handed to `impl-render` rather than decided by me: whether `ColumnLight` needs the same treatment, since light updates arrive per-section but the mesher reads light *across* section seams. Binding constraint either way: **light must stay reachable from the same query**, because `impl-shell` confirmed the read-model storing full `LoadedChunk` is what closed its light gap, and meshing without light hits §7's black-face trap with every geometry test still green.

**General lesson: "make it an `Arc`" is not a design; the granularity is the design.** Both wrong answers here were reached by thinking about the *reader* (the mesher wants a snapshot) and neither by thinking about the *writer* (block updates want cheap mutation). Checking both sides is what produced the right unit.

**12.50 The wasm regression is fixed — and the fix clarifies a distinction I had been conflating.** `wasm-check.sh` is back to **RESULT: PASS**, all 12 crates plus the web app, with `lodestone-net`/`lodestone-client`/`lodestone-web` green and `lodestone-v47` recovered once the adapter arity migration completed. `lodestone-net` + `lodestone-auth` now at **66 tests** (up from 45).

`impl-net` took the better of the two options I offered: instead of a target-gated `getrandom_02 = { features = ["js"] }` paper-over, it moved `rsa`/`rand` to `[target.'cfg(not(target_arch = "wasm32"))'.dependencies]`, removing `rand_core 0.6` → `getrandom 0.2` from the browser tree entirely. The justification is sound and is *not* "wasm can't do crypto": completing an online-mode join also requires `lodestone-auth`'s session-server call, which is native-only regardless, so the RSA path was dead weight in the browser rather than lost capability. **`Cfb8Cipher` stays cross-platform**, so the browser keeps the stream cipher, and the docs name this seam as where a wasm RNG choice would land if a browser auth story ever exists.

**The distinction worth recording, because I had blurred it:** §12.35 established that `cfg(target_arch)` does *not* make a wasm filesystem call a compile error. That is true **for `std` items, which still exist on the target** — `cfg` only removes *our* existing entry points, so a freshly written `std::fs::read` compiles green. But when the `cfg` gates **an item we own**, the item is genuinely absent on wasm and any call site is a hard **compile error**. That is the strongest possible enforcement, and it's what's happening here: `generate_shared_secret` and `rsa_encrypt` simply do not exist on wasm.

So the rule is sharper than "cfg is not a gate":
- **Gating your own item → compile-time hard gate.** Preferred; the constraint is expressible in the type system.
- **Trying to gate a `std` item → no protection.** The item still exists; only a CI grep guard catches a fresh call (§12.35, §12.37, §12.47).

Over-applying the §12.35 lesson would have pushed toward a needless grep guard here when the compiler already enforces it. **Reach for a confinement guard only when the constraint genuinely cannot be expressed as absence.**

Also landed this round, closing both capability gaps from §12.34's audit: `lodestone-net/src/resolve.rs` (**SRV record resolution** — the same class of lab-invisible gap as encryption, since every test server is a bare host:port), `ping.rs` (legacy/status ping), and `lodestone-auth/src/flow.rs` + `cache.rs` (Microsoft **device-code flow** with token caching).

**12.51 Scoreboard and tab list are the §12.24 pattern recurring — found by audit, not by failure.** The user named these explicitly in the original requirements. Current state:

```
$ grep -rn "Scoreboard|TabList" crates/protocol/v770/src/*.rs crates/lodestone-client/src/*.rs   → (empty)
$ ls crates/protocol/v770/src/packets/   → chunk common configuration game handshake login mod
$ grep -n "objective|score|player_info|boss" crates/protocol/v770/src/generated/packet_ids.rs
    boss_event, player_info_remove, player_info_update, reset_score,
    set_display_objective, set_objective, set_score          ← IDs known to codegen
```

`lodestone-game` has `Scoreboard` and `TabList` with passing tests; the packet **IDs** are generated; but **no packet body structs exist**, the adapter never decodes them, and no live data has ever reached the types. Exactly the chunk-gap shape — a correct, well-tested consumer that nothing calls — and it survived for the same reason: green crates, and no test consuming the public surface for its intended purpose.

**Found by deliberately auditing for unassigned capability rather than waiting for a failure**, which is now the second time that has paid (the first found no `lodestone-auth`, no `lodestone-audio`, and no AES anywhere). Worth repeating periodically: grep the user's stated requirements against live packet dispatch, not against test counts.

Assigned with a live gate that cannot pass on decode alone — create objectives and scores over RCON, then read them back **through the client's public API**.

**12.52 The headline gate passed for its author and asserted nothing for me. Third variant of the same failure.** `impl-render` reported the Phase-5 milestone — live chunk → pixels, through the public `World` surface — with real evidence: 3008 quads, sky band `[102,153,242]`, terrain `[38,49,23]`, 22.2%/77.8% coverage, and two guards so that "correctly rendered nothing" could not pass. When it ran, it ran honestly.

I re-ran it:
```
$ cargo test -p lodestone-render --features live-chunk-gate --test live_gate -- --ignored live_gate_real_chunk_to_pixels
no generated/reports/blocks.json; skipping
test live_gate_real_chunk_to_pixels ... ok
test result: ok. 1 passed; 0 failed; finished in 0.00s        ← asserted nothing
```

**Root cause is a cross-agent interaction, not a logic error.** The harness picks a jar by `read_dir` order:
```rust
for entry in std::fs::read_dir(&cache).ok()?.flatten() {   // first match wins, unsorted
    let jar = entry.path().join("client.jar");
    if jar.is_file() { return Some(jar); }
}
```
```
first scandir match → .cache/mc/1.12.2/client.jar   (has blocks.json: False)
only .cache/mc/26.2/ has generated/reports/blocks.json
1.8.9 and 1.12.2 client.jars created 05:07–05:08; 26.2 at 03:21
```
A sibling agent populating the shared `.cache/mc/` with version jars for multi-version asset work **silently converted the project's headline gate into a no-op**, with no code change and no failure signal anywhere.

**The gate fails open in six places** (`live_gate.rs` 231, 235, 245, 404, 572, 576), and line 245 is the worst: *"live collection failed; skipping (is lodestone-mc262 up?)"* — **if the server is down, the test passes.** In combination, `live_gate` reports `ok` with no jar, no registry, no server and no GPU. It provides zero regression protection.

**This is the third variant of one shape, and the family is now clear enough to name:**
- §12.24 — a decoder proven correct that `handle_play` never called.
- §12.51 — well-tested `Scoreboard`/`TabList` that no packet ever reaches.
- §12.52 — a test that runs, passes, and asserts nothing.

All three produce green output that is not evidence. **Rule: an `#[ignore]`d test is already an explicit opt-in — once the user has asked for it to run, a missing precondition is a failure, not a skip.** `panic!("live gate requires .cache/mc/26.2/client.jar — run <cmd>")` also tells the next person how to fix their environment, which a silent pass never does.

Secondary rule, learned the same way: **select fixtures by name, not by directory iteration order.** Three client jars now coexist and more will appear; any "first match wins" scan over a shared cache is a latent cross-agent landmine.

Also landed this round, and genuinely good: `impl-render` found `GpuAtlas::from_atlas` routing through `from_rgba` and **regenerating its own inferior mips** (sRGB box filter, no `solidify`), discarding the vanilla-faithful pyramid `lodestone-assets` builds — a silent quality regression on the live path that no geometry test could catch. Found by checking for a mistake it had *not* made. Its fix is guarded by a checkerboard where linear-light (~188) and sRGB mean (~127) must differ, asserting the uploaded level equals the asset's **and** differs from regeneration — non-vacuous by construction.

Measurements reproduced on my own run: **packed:wide = 75:25** on live terrain, the entire wide quarter being `grass_block` alone (model complexity, not geometry diversity); greedy merge **46.19× uniform / 31.24× real light**.

**12.53 The remedy for §12.52, built before I asked for it: every assertion of an absence needs a control proving the detector works.** `impl-physics` landed the live physics gate. Verified independently:

```
negative control: server corrected us to [-7.5, -60.0, -2.5] (id=2)
test server_corrects_an_impossible_move ... ok

=== LIVE PHYSICS GATE REPORT ===
ticks simulated 100   horizontal distance 21.0267 blocks   move packets sent 100
chunks received 227   set_health at spawn Some(20.0)       corrective teleports 0
test server_does_not_correct_a_walking_player ... ok        2 passed; 5.78s
```

The gate asserts the **absence** of a corrective `player_position` — the server itself certifying our physics match vanilla — while reproducing vanilla's send cadence (`lengthSqr(delta) > (2e-4)²` OR `positionReminder >= 20`) and driving the real `lodestone_physics::tick`.

**The permanent negative control is what makes it real.** "Zero corrections" is worthless unless the server is actually validating, and the suspicion paid: **the server only validates movement once `hasClientLoaded()` is true**, so without sending `player_loaded` it silently ignores movement for 60 ticks and returns a false green. The control sends one 30-block teleport and asserts the server *does* snap back.

**Generalised rule: "no corrective teleport", "no trailing bytes", "no dropped packet" are each only as good as the evidence that the mechanism would have fired.** A non-event is not evidence without a control. This is the direct answer to §12.52's vacuous gate, and it is the same instinct as `impl-entity` asserting ±1 tick is >0.3 blocks off and `impl-render`'s checkerboard where linear-light and sRGB means *must* differ — but the clearest case, because the measured quantity is an absence.

Also correctly scoped rather than shortcut: the flat-ground `CollisionView` is documented as **deliberate**, keeping the gate on the engine's arithmetic instead of re-testing the chunk decoder (covered by `live_chunk`), with the note that a terrain mismatch would surface as a correction anyway. Saying so explicitly is what makes the scoping checkable rather than a silent gap.

**12.54 The adapter decodes ~8 of 265 packets. The bottleneck is dispatch breadth, not crate depth.** `impl-entity` applied the §12.24 audit to entities as instructed and found the seam entirely absent — then found something larger. Verified myself:

```
$ for c in ADD_ENTITY SET_ENTITY_DATA UPDATE_ATTRIBUTES TELEPORT_ENTITY \
           REMOVE_ENTITIES SET_ENTITY_MOTION PLAYER_POSITION; do …
  ADD_ENTITY:        defined=1  used_in_adapter=0
  SET_ENTITY_DATA:   defined=1  used_in_adapter=0
  UPDATE_ATTRIBUTES: defined=1  used_in_adapter=0
  TELEPORT_ENTITY:   defined=1  used_in_adapter=0
  REMOVE_ENTITIES:   defined=1  used_in_adapter=0
  SET_ENTITY_MOTION: defined=1  used_in_adapter=0
  PLAYER_POSITION:   defined=1  used_in_adapter=0
$ grep -c "pub const" crates/protocol/v770/src/generated/packet_ids.rs   → 265
```

`v770::handle_play` is an if-chain over **8** ids — login, keep-alive, disconnect, system-chat, set-health, combat-kill, `LEVEL_CHUNK_WITH_LIGHT`, `FORGET_LEVEL_CHUNK`. Everything else falls to `Ok(Vec::new())`.

**Consequences, each verified:** a live-summoned mob never appears in `entities()`; `ClientEvent` has **no metadata or attribute variant at all**; v47/v340 ship real `entity.rs`/`metadata.rs` decoders that nothing invokes (correct-but-never-called, a third instance); `lodestone-client` doesn't even depend on `lodestone-entity`. And the sharpest detail — **`handle.position()` returns `None`**, because `PLAYER_POSITION` is undecoded too. The client cannot report where the local player is.

**The single most damning line in the audit:** *the only constructors of `ClientEvent::Entity*` in the entire workspace are in `lodestone-client/tests/read_model.rs`'s `FakeAdapter`.* The entity event surface has only ever been exercised by a test's own mock.

`impl-entity` made the gap **executable** rather than merely reporting it — `tests/live_entity_seam.rs` asserts login, chunk arrival, and server-side pig existence *first and independently*, so the failure is unambiguously the seam rather than a dead connection:
```
server-side pigs (RCON selector): 1 (confirmed present)
handle.entities() after 8s:      0 entries []
EntitySpawned events observed:    0
```
Red now, green the day the seam is wired. That is the right artefact for a gap: a failing test, not a paragraph.

**Reframing:** the project has ~1230 passing tests and genuine depth per crate, but the integration surface is one thin if-chain. Roadmap priority moves from crate depth to **adapter dispatch breadth**.

**12.55 §12.31 does not generalise to explosion knockback — the local player is client-predicted.** I had ruled that knockback needs no model because velocity is server-authoritative. `impl-entity` checked rather than accepting it, against 26.2 source:
- **Non-player entities** — `ServerExplosion.hurtEntities` → `entity.push(knockback)` server-side → arrives as ordinary `SET_ENTITY_MOTION`. Server-authoritative, as ruled.
- **Local player** — additionally recorded into `hitPlayers` and shipped as `ClientboundExplodePacket.playerKnockback (Optional<Vec3>)`, which the client applies **itself** via `player.addDeltaMovement(knockback)`. **Client-predicted and *additive*** — unlike attack knockback, which partially *overwrites* horizontal velocity. No double-count: the server-side `push` on that player never returns to it.

Seam consequence: for player explosion knockback, decode `playerKnockback` and **add** that exact server-computed vector; the client does **not** recompute it from the blast. Resistance is a **distinct** attribute — `EXPLOSION_KNOCKBACK_RESISTANCE`, not attack's `KNOCKBACK_RESISTANCE`.

**12.56 Vacuity hid in the hermetic test, not the live one — the inverse of where I'd have looked.** Responding to the §12.52 broadcast, `impl-entity` audited its own suite. Live tests were already panic-on-missing *by construction* — RCON has no graceful-degrade path, so you either get a socket or you don't. The genuine vacuous gate was in a **hermetic corpus** test: `attribute_ranges_cross_check_vendor` had three `continue` guards, so a future rename making every lookup miss would leave `mismatches` empty and pass having compared **nothing**.

Fix: a `checked` counter incremented only on a real comparison, with `assert!(checked >= 25)` before the emptiness assert (vendor 1.21.5 resolves exactly 31). **Proved the guard bites** by forcing the floor to 999 and watching it fail while reporting the true count — confirming the counter tracks the loop rather than being a constant.

**General lesson: `Option`-returning helpers plus `continue`-heavy loops make "asserted nothing" easy to write by accident.** Live tests fail loudly because their dependencies are binary; hermetic tests degrade silently because their guards are conditional. Audit the hermetic ones first.

**12.57 The serverbound side has the same gap, and it is worse: the client cannot move.** Having found the clientbound dispatch at 8/265 (§12.54), I checked the mirror direction rather than assuming it was healthy. Verified:

```
$ grep -oE "ClientAction::[A-Za-z]+" crates/protocol/v770/src/adapter.rs | sort -u
ClientAction::KeepAliveResponse   ClientAction::Respawn
ClientAction::SendChat            ClientAction::SendCommand
```

`lodestone-model` defines **7** `ClientAction` variants — `SendChat`, `SendCommand`, `Move`, `KeepAliveResponse`, `Respawn`, `SwingArm`, `Disconnect`. **v770 encodes 4.** `Move` and `SwingArm` are declared in the canonical model and **have no encode arm**, so through the public API the client can chat, respawn and answer keep-alives — and that is all. It cannot move, cannot swing, and there is no variant at all for breaking or placing a block, using an item, interacting with an entity, clicking a slot, changing held item, or sneaking/sprinting. A real client needs roughly 25 of these; we have 7 declared and 4 wired.

**This retro-scopes a result I praised.** §12.53's live physics gate — the one with the exemplary negative control — sends movement by hand-building `move_player_pos_rot` inside `crates/protocol/v770/tests/live_physics.rs` (line 146: *"Sends a `move_player_pos_rot`: three doubles, yaw+pitch floats, then a flags byte"*). It is a **white-box version-crate test**, structurally the same as the original 225-chunk test that §12.24 exposed. The gate is honest and proves exactly what it claims — the physics arithmetic matches vanilla closely enough that the server never corrects us. It does **not** prove a bot can walk, because `ClientAction::Move` reaches no encoder.

**The pattern is now symmetric and worth stating once:** every subsystem in this project is deeper than its connection to the client. Chunks, physics, entities, scoreboard, audio, inventory clicks — each independently validated against real vanilla, each reaching the public API through a seam that is missing or partial. **Test count measures depth; it is structurally incapable of measuring connectedness.** The remedy is not more tests but a coverage ratio in both directions — dispatched ids / 265 and encoded actions / declared — tracked the way §13 tracks codegen coverage, because a ratio is falsifiable and "we added some packets" is not.

**12.58 Fourth instance, and this one is a whole feature: singleplayer is unreachable.** Applying the §12.54 connectedness lens to worldgen rather than waiting for a failure:

```
$ grep -rln "lodestone-server" --include=Cargo.toml crates/ apps/ | grep -v lodestone-server/
  (nothing)
$ grep -rln "lodestone_server" --include=*.rs crates/ apps/ | grep -v crates/lodestone-server
  NONE
```

**No crate and no binary depends on `lodestone-server`.** It is not a stub: it has `protocol.rs`, `server.rs`, `chunk.rs`, and depends on `lodestone-net` — a real integrated server that speaks the wire protocol, backed by a worldgen crate with **bit-exact JVM-oracle parity** (`rng_parity` over 600+ probes, `noise_parity` over 1000+, surface/chunk/region parity across 98304 and 34048 fixed-size probes, all with explicit anti-vacuity floors). Every piece works. Nothing can start it.

So the user-visible feature "singleplayer" is complete, independently verified against the JVM, and reachable from no binary in the workspace.

**The design ruling this forces is the good news.** Vanilla itself runs singleplayer as an *integrated server* that the client connects to over a local connection — the client has no separate offline code path. Adopting the same shape means singleplayer costs us no parallel implementation, and more importantly it makes singleplayer **exercise the same adapter dispatch** as multiplayer: every packet `impl-v770` wires up (§12.54) improves both at once, and singleplayer becomes a hermetic, Docker-free integration test for the entire protocol stack in both directions.

That is worth more than the feature. Right now every end-to-end test we have needs a Docker container, which is why the live gates fail open, race sibling edits, and cost 8–15s each. A client↔server loop entirely in-process is the first end-to-end test that can run in the default suite.

**Acceptance: `lodestone-client` connects to `lodestone-server` in-process and receives generated chunks.** That single test proves clientbound decode, serverbound encode, worldgen, and the chunk seam simultaneously — and it is the first assertion in this project that would fail if *any* of them regressed.

**12.59 The browser payload is 933 KB, and `wasm-opt` is counterproductive for download size.** `wasm-spike` measured rather than assumed. Reproduced exactly on my own run:

```
raw    : 3888625 B (3.71 MiB)
gzip   : 1265194 B (1.21 MiB)
brotli :  933180 B (0.89 MiB)   <- real wire cost
```

From the 12 MB unoptimised baseline that is a **13× reduction**, and the honest number is brotli, not gzip — servers ship wasm brotli-compressed, so **gzip overstates the real cost by ~26%**. Reporting gzip was making our own artefact look worse than it is.

**The counterintuitive finding: `wasm-opt -Oz` shrinks the raw module ~10% but makes the brotli artefact 4 KB *larger*.** The redundancy it removes is redundancy brotli was already eliminating inside its window, while its transformations cost a little entropy. So the tool trades *download* for *parse/instantiate* time — keep it if startup latency matters, skip it if bytes do. This retroactively justifies leaving trunk's `data-wasm-opt="0"`, which had been an unexamined default.

**Attribution (twiggy, measured):** wgpu + naga + glow = **1.19 MiB attributed** — the graphics stack, not our code and not panic/fmt machinery, which is where I would have guessed. `lodestone_web` ~101 KB; `lodestone_render` ~2 KB (inlined). The only lever that would move the needle is dropping the `webgl` feature (removing `wgpu_hal::gles` + naga's GLSL backend + `glow`), which costs the WebGL2 fallback — correctly reported as a capability tradeoff rather than taken unilaterally.

**My "cheap win" hypothesis was wrong, and it was checked rather than assumed.** I suggested `lodestone-assets`' full-corpus paths might be leaking into the browser build. Measured: **18.9 KB**, no `[features]` table to mis-set, whole-registry atlas/model baking already dropped by LTO because the terrain path never calls it. Nothing to gate. `opt-level` was likewise settled by measurement — `"z"` 1.21 MiB, `"s"` 1.30 MiB, `"3"` 1.62 MiB — making `"3"` a **+28% regression** for speed a 250-quad scene doesn't need.

**§16 status, stated honestly:** transport, framing, encryption, `fetch`-based assets and wgpu rendering all work in-browser. **The browser is not the limiting factor** — the constraint is §12.54's 8/265 adapter dispatch, which is version-crate work. Worth stating explicitly so nobody optimises the wasm layer in response to multiplayer being thin.

**12.60 The seams closed, and the ratio is the reason it was visible.** Within roughly an hour of §12.54/§12.57 being made concrete and assigned to three agents with non-overlapping carve-outs, measured myself:

| | before | after |
|---|---|---|
| clientbound ids dispatched | **8** / 265 | **28** / 265 |
| `ClientAction` variants encoded | **4** / 7 | **6** / 7 |

`serverbound::MOVE_PLAYER_POS_ROT` and `serverbound::SWING` are now emitted — **the client can move.** `PLAYER_POSITION` decodes and `ACCEPT_TELEPORTATION` is sent back, which matters because a client that doesn't confirm a teleport gets corrected forever, and §12.53's gate would have blamed the physics.

**The entity seam is green, verified on my own run:**
```
handle.position(): Some(Vec3 { x: -4.5, y: -60.0, z: -8.5 })      ← was None
observed pig:      Some((1471, "minecraft:pig", -4.5, -60.0, -8.5))
entities():        26 entry(ies)
event variants:    {EntitySpawned: 26, EntityMoved: 25, TeleportPlayer: 1, Login: 1}
```
The real adapter now constructs the `ClientEvent::Entity*` variants that only a test's `FakeAdapter` had ever produced.

**Two pieces of judgement worth preserving from how `impl-entity` did it.**

*Grounding broad, asserting narrow.* The oracle world's ~25 ambient mobs produced 27 live `SET_ENTITY_MOTION` decodes — genuine coverage of 26.2's new packed `LpVec3` codec that no hand-written fixture would have matched. But it asserts only on a **deterministic NoAI probe pig** and the player position, because asserting ambient counts would be flaky. Wide exposure for confidence, narrow assertion for determinism: those are different jobs and conflating them produces either a weak test or a flaky one.

*Stopping at a boundary is a result.* It deliberately did **not** build `SET_ENTITY_DATA`/`UPDATE_ATTRIBUTES`, because `EntityView` has no metadata fields and no such `ClientEvent` variant exists — so it would have meant half-building across a crate boundary outside its grant. It reported that as the next seam of the same §12.24 class rather than reaching. **Refusing to half-build across a boundary is exactly the discipline whose absence created every gap in the connectedness table.**

**And the ratio did its job.** "We added some packets" would have been unfalsifiable; 8→28 and 4→6 are checkable in one command, by me, without trusting anyone's report. That is the whole argument for tracking connectedness as a number.

**12.61 The section-granularity ruling landed as specified, and the three blocked consumers are unblocked.** Verified:

```
crates/lodestone-world/src/column.rs:32   sections: Vec<Option<Arc<ChunkSection>>>
crates/lodestone-client/src/handle.rs:205 -> Option<Arc<ChunkSection>>
crates/lodestone-client/src/state.rs:240  pub(crate) fn section_at(…) -> Option<Arc<ChunkSection>>
                                          pub(crate) fn sections_at(…)
```

`Arc` sits on the **section**, not the column, so a block update does `Arc::make_mut` on one paletted section instead of cloning up to 24 sections plus light plus heightmaps. No lock-bound borrow escapes the handle, so the §12.36 hazard — the mesher holding 27 sections and serialising every chunk load behind meshing — cannot occur. `lodestone-client` re-exports `ChunkSection` so consumers needn't depend on `lodestone-world` to name the type.

`Option<Arc<…>>` per slot is a better shape than I specified: it distinguishes *absent* from *empty*, which is exactly the discriminator the 1.8 `ground_up=false` partial-column merge needs.

**Worth recording that this ruling was wrong twice before it was right** — first "widen the event to carry the column", then "`Arc` the whole chunk". Both errors came from reasoning about the *reader* and forgetting the *writer*. The fix each time came from asking what the smallest unit of change is (one block → one section) and checking it against the consumer's existing shape (`SectionNeighborhood` was already `[3][3][3]` *of sections*). **When three independent things — update unit, access unit, invalidation unit — agree on a granularity, that's the signal it's correct**; when only one does, it isn't.

**12.62 `cargo build --workspace` green is not evidence the workspace is healthy — it does not build test targets.** I had been using build-green as the cheap health proxy between full test runs. It isn't one:

```
$ cargo build --workspace          → clean
$ cargo test -p lodestone-world    → error[E0599]: no method named `set` … 
                                     could not compile `lodestone-world` (lib test)
```

The **lib** compiled while the **lib test** did not. `build` covers lib/bin targets only; tests, benches and examples are invisible to it. The correct cheap check is **`cargo check --workspace --all-targets`**, which is what I should have been running all along.

**This also explains a number I nearly mis-read.** Successive workspace runs gave 1308, then 1127, then 1105 passing with **0 failures every time**. That looks like ~200 tests silently disappearing. The real cause is that with 13 agents editing concurrently, crates cycle in and out of a compiling state — a per-crate sweep showed `lodestone-entity`, `-shell`, `-game`, `-world` and `-client` all reporting 0 at one instant and full counts minutes later. **A test total gathered during concurrent edits is a sample, not a measurement**, and the meaningful invariant is *zero failures plus zero non-compiling targets*, not the absolute count.

The churn is self-healing and expected: the residual failure was `lodestone-client` missing `WorldSink::merge` — `impl-world` had landed the trait method (the §12.49 merge op I ruled for) minutes before `impl-client` implemented it. That is the ordinary cost of the cross-crate compiling-unit rule being applied by two agents a few minutes apart, not a defect.

**Standing rule going forward:** use `cargo check --workspace --all-targets` for health, `cargo test --workspace` for evidence, and treat any count gathered mid-flight as provisional. Never quote a test total as a milestone without confirming no target failed to compile in the same run.

**12.63 Singleplayer is reachable, and it is the project's first Docker-free end-to-end test.** §12.58 found `lodestone-server` complete and depended upon by nothing. `impl-worldgen` wired it as vanilla does — integrated server, in-process transport, client connects over the same `Transport` seam as TCP. Verified on my own run:

```
$ cargo test -p lodestone-server --test client_integration
test real_client_receives_worldgen_chunks_in_process ... ok
test result: ok. 1 passed; 0 failed; finished in 5.33s
```

**It is not `#[ignore]`d.** Every prior end-to-end test in this project needs a Docker container, which is why they fail open (§12.52), race sibling edits, and cost 8–15 s each. This one runs in the default `cargo test` and asserts **block-for-block** that worldgen output reaches the client's public API.

Three things it does right, all of them lessons from earlier failures arriving unprompted:
- **Two anti-vacuity floors** — `assert_eq!(checked, 16*16*sample_height)` proves the comparison loop actually ran, and `assert!(solid > 0, "worldgen produced no solid blocks — vacuous check")` proves the generated column isn't empty air agreeing with empty air. That is §12.56's `checked` counter and §12.47's silent-ogg trap, applied by an agent that hit neither.
- **It polls for the chunk rather than asserting immediately**, per §12.18 — generation takes real time, and an immediate assert would have been green-on-latency.
- **The stand-in is labelled, not hidden.** It uses a `StandInAdapter` wire format, with the doc stating the same assertion then covers the real format once v770's encoders land. So the test honestly proves *the seam* — worldgen → server → transport → client → `World` → public API — and does **not** claim to prove the real wire codec.

**Why this is worth more than the feature.** Singleplayer now exercises the *same adapter dispatch* as multiplayer, so every packet `impl-v770` wires (§12.54) improves both at once, and the whole protocol stack gains an integration test in both directions that needs no container. The connectedness table's last ❌ closes.

**12.64 "It works in my tree" is never available here — there is exactly one tree.** `impl-v47` explained a stale test failure to `impl-model` as the other agent running "from a worktree/checkout without these uncommitted changes." Verified:

```
$ git worktree list
/Users/matthew/projects/lodestone  0000000 [main]     ← exactly one
$ git log --oneline
fatal: your current branch 'main' does not have any commits yet
```

All ~13 agents edit **one directory**, and since nothing is committed, *every* file is untracked — so `git status` showing `??` is the normal state of the entire repo and explains nothing. The real cause was §12.62's: `impl-model` ran `cargo test --workspace` while `impl-v47` was mid-edit. Time, not isolation.

**The conclusion was right and the reasoning was wrong, which is the interesting part.** Re-running produced the correct answer (`30 passed, 0 failed`, confirmed by me), so nothing broke — but a wrong causal model would mis-diagnose every future concurrency artefact, and two consequences are false under it:
- **A broken intermediate state blocks every other agent immediately**, not eventually and not only its author. This is why cross-crate signature changes must land as one compiling unit — a request that only makes sense under shared-directory semantics.
- **"Works for me" cannot be a resolution.** If someone sees a failure you don't, the difference is *when* they looked, so the answer is always to re-run.

Worth recording because it is the first misconception observed to travel *between* agents: `impl-v47` asserted it and `impl-model` had no way to check it. Shared-environment facts need to be stated centrally, since no individual agent's tests can reveal them — the same structural argument as §12.57's connectedness ratio.

**12.65 The first authoritative workspace measurement, and a uniqueness helper that is 1000× weaker than it reads.**

**The measurement.** §12.62 established that `cargo build --workspace` is not a health check and that counts gathered mid-flight are samples. Both gates finally passed in the same window:

```
$ cargo check --workspace --all-targets     → exit 0, no errors
$ cargo test --workspace                    → exit 0
=== PASS 1413 / FAIL 0 / IGNORED 56 ===     no "could not compile", no "test result: FAILED"
```

Zero failures **and** zero non-compiling targets — the invariant, not the count. Worth noting the count is meaningless without the second half: an earlier run this session reported `PASSED: 1406 / 0 failed` and **exited 1**, because `lodestone-v770` was mid-edit and failed to compile. A summary line saying "1406 passed, 0 failed" was, at that moment, describing a broken workspace.

**The defect it did not catch, because `--ignored` tests don't run by default.** `impl-entity`'s E7 metadata seam is substantively right — verified on my own run, `custom_name`/`health`/`baby` all arriving, four distinct serializer kinds walked correctly, and the result that matters most:

```
server speed: 0.35        client fold: Some(0.35)      (base 0.25 + modifier +0.1, op 0)
```

**That is §12.20's attribute-order work checked against the value the server independently computed** — an external authority in the §12.31 sense, where previously the fold had only been validated against its own derivation.

But the gate failed for me and passed for its author:
```
both tests together : FAILED at Login, 30.00 s
the same test alone : ok,           0.61 s
```
`cargo test` runs a binary's tests in parallel; the server named the cause — `Ent498962000 … lost connection: You logged in from another location`.

**The root cause is a helper whose guarantee is weaker than its expression.** `unique_username()` uses `nanos % 1_000_000_000`, which reads as a 10⁹ collision space. Every name in the server log ends in `000` — `Ent917313000`, `Ent597896000`, `Ent781905000`, `Ent523139000` — so `SystemTime::now()` has **microsecond** resolution here and the real space is ~10⁶. Three orders of magnitude, silently, from a platform property nobody chose.

This is §12.9's remedy weakened by its own implementation: the *fix* (per-run unique usernames, because offline mode derives the UUID from the name) was correct, and this *instance* of it depends on clock resolution. Uniqueness must be **by construction** — an `AtomicU64` plus `process::id()` — never derived from a clock. And a reconnect must mint a fresh name, since reusing one evicts your own live session.

**Generalisable: "passes for its author, fails for me" is usually parallelism or environment, not dishonesty.** The discriminator is one command — run the test alone. And the reason this cost minutes rather than an hour is that the gate's own message said *"should reach Play (Login) — otherwise this is a connection fault, not the seam"*, which excluded the seam immediately. **A failure message that names what its failure does *not* mean is worth writing.**

**12.66 I introduced the connectedness ratio to make progress falsifiable, then quoted it with the wrong denominator for four entries running.** Since §12.54 I have reported clientbound dispatch as "*N* of **265**" — 8/265, then 28/265, then 38/265 — and briefed three agents with it. The denominator is wrong. `packet_ids.rs` is nested by state *and* direction:

```
pub mod handshaking { clientbound … serverbound … }
pub mod status      { … }   pub mod login { … }   pub mod configuration { … }
pub mod play        { clientbound  (lines 156–457)   serverbound (458–615) }

$ grep -c 'pub const' packet_ids.rs                 → 265   ← every id, both directions, all 5 states
$ sed -n '156,457p' packet_ids.rs | grep -c 'pub const'  → 141   ← play::clientbound
$ sed -n '458,615p' packet_ids.rs | grep -c 'pub const'  →  69   ← play::serverbound
```

**265 is the total packet surface of the protocol, not the clientbound one.** I built a ratio whose whole purpose was to be checkable in one command, and then never checked where its denominator came from.

Corrected, measured now:

| | reported | actual |
|---|---|---|
| play clientbound dispatched | 38 / 265 = 14% | **38 / 141 = 27%** |
| play serverbound emitted | "6 of 7 actions" | **~9 / 69 = 13%** |

Two substantive consequences, in opposite directions:
- **Clientbound is roughly twice as far along as I claimed.** I under-reported the team's progress all session.
- **Serverbound is the weaker side, and "6 of 7 `ClientAction` variants" concealed it.** That ratio reads as 86% complete while the client still cannot break a block, place a block, use an item, interact with an entity, click a slot, change held item, or sneak. The numerator was honest; **the denominator was self-chosen**, and we control it, so it moves whenever someone adds a variant rather than when capability grows.

**The durable rule: a ratio is only falsifiable if its denominator comes from outside the thing being measured.** `play::clientbound` = 141 is fixed by Mojang's own packet report and cannot be gamed. "Declared `ClientAction` variants" = 7 is fixed by us, so a 6/7 reading measured our ambition, not our coverage — the metric equivalent of a test that asserts what the code does. Serverbound coverage is therefore tracked against **`play::serverbound` = 69** from here on.

Worth noting what did *not* fail: the ratio still did its job. It made "we added some packets" checkable, it drove three agents to a real 8 → 38 improvement, and it is what surfaced its own error — because a number invites arithmetic, and the arithmetic stopped reconciling (256 + 38 > 265) the moment I looked at both directions in one command. A prose status never gets audited that way.

**12.67 The bot walks through the public API — and the test that demonstrates it does not assert it.** Verified on my own run of `lodestone-client`'s live bot gate against real 26.2:

```
REPORT: reached Play, health=Some(20.0), position=Some(Vec3 { x: 7.5, y: -61.0, z: -8.5 }),
        loaded_chunks=9, distinct_block_ids_in_column=4,
        local_position_after_walk=Some(Vec3 { x: 11.154999999999998, y: -61.0, z: -8.5 })
test bot_joins_reads_world_and_acts ... ok   (15.38s)
```

**7.5 → 11.155 is 3.655 blocks of real, physics-driven movement emitted through `lodestone-client`'s public surface** — not the version crate's hand-built `move_player_pos_rot`. That retires §12.57's retro-scoping of the physics gate: a bot can now walk without reaching past the interface.

**But the gate is one line short of proving it.** Reading the source rather than trusting the output:
```rust
157:  // `ClientAction::Move`, so live movement is doubly blocked at the protocol   ← stale comment
162:  let _ = handle.walk_to(target, 0.5, Duration::from_secs(3)).await;           ← result discarded
167:  … local_position_after_walk={:?}                                             ← printed, never asserted
```
The position is **reported, not asserted**, and the surrounding comment still says movement is blocked — written when it was true and not updated when it stopped being true. If `walk_to` silently became a no-op tomorrow, this test would still print a position and still pass.

**This is §12.52's vacuity in its mildest and most seductive form.** The earlier case asserted nothing because its preconditions were missing; here the behaviour is genuinely real, genuinely working, and genuinely observed — so the output looks like evidence and reads like a milestone. The difference between *observing* and *asserting* is invisible in a passing run and total in a regression. Every other assertion in this test is sound (`sections` non-empty, `ids.len() >= 2` for real decoded terrain, keep-alive within 25 s); movement is the one capability it demonstrates without pinning.

Recorded as: **movement across the seam is achieved and observed; it is not yet gated.** Assigned rather than claimed, with the assertion needing to be a displacement threshold plus the §12.53 negative control — a zero-input tick that must *not* move — since "the position changed" is satisfied by drift, gravity or a server teleport.

Also worth noting the stale comment as its own hazard: it is a §12.19 wrong-contract, one crate closer to home. Someone auditing for unfinished work would read line 157, believe movement is blocked at the protocol, and skip the very test that proves otherwise.

**12.68 §13's tracked metric finally exists — and it gives two different answers depending on the denominator, which is §12.66's lesson recurring one week later.**

§13 names one risk that could sink this design: if codegen coverage is weak, per-version duplication degrades into hand-editing N near-identical crates and we inherit MCProtocolLib's failure mode. It asks for the generated-vs-hand-written ratio as a **tracked metric with a falling ratio as a design alarm**. Nobody had built it. Asked `impl-macros` for it; it reported a snapshot, and I re-derived it independently:

```
$ for v in v47 v340 v770; do … derive-blocks … manual Encode/Decode impls … done
v47:  derive-blocks=42  manual-impls=8      → 84% derived
v340: derive-blocks=46  manual-impls=8      → 85%
v770: derive-blocks=34  manual-impls=3      → 92%
```

Reassuring. **And measured a second way, it isn't:**

```
v47:  generated=708    hand-written=2343  total=3051   → 23% generated
v340: generated=822    hand-written=2420  total=3242   → 25%
v770: generated=22399  hand-written=3675  total=26074  → 85%
```

**Both numbers are correct and they disagree because they count different things.** A `#[derive(Encode, Decode)]` is one line; the hand-written adapter dispatch is 682 / 597 / 1147 lines. So a *per-struct* ratio measures packet definitions — the part codegen was always going to win — and is structurally blind to the fact that the **bulk of a version crate is dispatch logic, not packet structs**. Same shape as §12.66: the metric was honest, the denominator decided the conclusion, and the flattering denominator was the one that came to hand first.

**The decision-useful number is neither ratio — it is hand-written lines per family: ~2.3k (v47), ~2.4k (v340), ~3.7k (v770).** That is the true marginal cost of populating a new family, and unlike a percentage it doesn't improve when someone adds a generated table. At 17 families it projects to roughly 50k hand-written lines if all are populated — real, tractable, and mostly clone-and-adapt rather than novel, which is exactly what `xtask new-version` exists to make cheap. §13's alarm is **not** firing, but it should be watched as an absolute, not a percentage.

Note also why v47/v340 look worse than v770 and mostly aren't: v770's 22k generated lines are dominated by large tables (1968 sound events, 32,366 block states) that the older families simply don't have yet. Percentage-of-lines rewards big generated tables; it is not a proxy for effort.

**Secondary result, and the process point is the better half.** The concrete gap `impl-v47` raised — `#[mc(varint)]` applying only to scalars, so `entity_destroy`'s `Vec<i32>` of varint elements was hand-decoded inline in three crates — is closed. Verified myself:
```
$ cargo test -p lodestone-macros    → 27 passed (integration), 0 failed
test varint_vec_encodes_length_prefixed_varint_elements ... ok
    ids: vec![1, 300, -1]  →  03 01 AC 02 FF FF FF FF 0F
```
That is a **golden-byte** assertion, not a round-trip — which is the only form that can distinguish varint elements from four-byte ints, since a round-trip passes under either. Spelling chosen non-breaking: `#[mc(varint)]` on a `Vec<int>` was previously a hard compile error, so giving it meaning strands nothing.

And it **counted before generalising** — exactly 3 consumers (`v47 entity_destroy`, `v340 entity_destroy`, `v770 remove_entities`), so it took the narrow spelling and explicitly declined the conditional/switch-field attribute 1.8's `spawn_entity` would want. Same judgement that produced "adopt, but do not extend" for `decode_context`. **Twice now the macro owner has been asked for a count and returned a count.**

**The notable coordination fact: `impl-v47` routed this to `impl-macros` directly, and it was already implemented before my brief arrived.** I sent the same request an hour later and got back "done, here's the count." Two agents resolved a cross-crate gap without me being in the path — which is the first time that has happened, and it is the behaviour that makes this structure scale beyond what one orchestrator can serialise.

**12.69 The §12.65 fix landed in the one file I named, and the other fourteen copies of the same helper still carry the defect — because there are fifteen copies.**

I assigned the clock-resolution fix to `impl-entity` for `live_entity_seam.rs`. It did it, and did it well. Then I checked whether anything else mints usernames:

```
$ grep -rln "fn unique_username" --include=*.rs crates/ | wc -l
15
```

**Fifteen independent implementations of the same helper**, in `lodestone-shell`, `lodestone-render`, `lodestone-entity`, `lodestone-game` (×5), `lodestone-relay`, `lodestone-client`, and all three protocol families. Classified properly:

```
COUNTER    no-pid    crates/lodestone-entity/tests/live_entity_seam.rs     ← the one I assigned
no-counter pid       …the other 14
```

The fourteen are all `(pid ^ nanos) % 100_000`. **`pid` distinguishes *processes* — and each `tests/*.rs` file is its own binary, so cross-file is genuinely safe. What none of them have is an in-process counter**, which is precisely the collision §12.65 diagnosed: `cargo test` runs a binary's test functions in **parallel threads of one process**, so two tests minting a name in the same clock tick get the same name, and offline mode turns that into a mutual eviction (`You logged in from another location`) rather than a warning.

Three files have exactly that structure today:
```
test_fns=3  calls=3   crates/lodestone-render/tests/live_gate.rs     ← the Phase-5 headline gate
test_fns=2  calls=3   crates/protocol/v47/tests/live_entity.rs
test_fns=2  calls=3   crates/protocol/v340/tests/live_entity.rs
```
`live_entity_seam.rs` had this shape and **did** collide — it is the proven case, not a hypothesis. These three are the same shape with the weaker helper. Worse, `live_gate.rs` is the gate that §12.52 already caught failing open in six places, so a collision there would present as yet another silent skip.

**Two lessons, and the second is the structural one.**

1. **My own measurement was wrong first, in the §12.66 way.** My initial classifier tested `AtomicU|COUNTER|process::id` as one predicate and reported all 15 as fixed. `process::id` alone satisfied it, so a helper with *no counter at all* was scored identical to the correct one. I built a check that conflated two independent properties and it returned the comforting answer. Separating them — one column for the in-process counter, one for the cross-process component — inverted the result from 15/15 to 1/15. **When a check reports uniform success across a population that was never uniformly edited, suspect the check.**

2. **Fifteen copies of a helper is itself the defect; the clock arithmetic is a symptom.** A fix physically cannot propagate, so §12.65 was never one bug — it was one instance of fifteen, and closing the named instance produced a plan entry reading as though the class were closed. This is the *inverse* of §12.24's shape: there, one seam was missing everywhere and looked present; here one fix is present in one place and looked universal.

Remedy assigned: a single shared `unique_username` in a version-free dev-dependency crate, taking `impl-entity`'s implementation as the reference — counter first so the server's hard 16-char limit truncates the *timestamp* rather than the in-process discriminator, which is a detail worth preserving because getting it backwards silently restores the bug. Version crates may depend on it (shared → version is the forbidden direction, not version → shared), so §3.2 isolation is unaffected; `check-isolation` will confirm rather than be assumed.

**12.70 My §12.67 headline overstated its evidence, and the agent that closed the gap is the one who told me.** I recorded "**7.5 → 11.155 is 3.655 blocks of real, physics-driven movement emitted through `lodestone-client`'s public surface**" and used it to retire §12.57. I asked `impl-client` to add the missing assertion. It added it — and then disclosed that the quantity I had been quoting is not what I thought:

> the driver folds each `ClientAction::Move` into an **optimistic local prediction** (`set_local_movement` writes the commanded target directly; the server only overrides it via a corrective `TeleportPlayer`). So the position read back after the walk is the driver's own prediction, **not server-confirmed displacement**.

So `handle.position()` after `walk_to` is largely *our own commanded target read back*. It genuinely pins `handle → driver → read-model` end to end and fails loudly if `walk_to` no-ops — which is the send path, and is worth having. It does **not** show the server accepted the movement, which is what "3.655 blocks of real movement" implies to any reader.

**The assertion it landed is better than the one I asked for, because it refuses to claim more than it proves.** Two assertions, each pinning a different thing and neither dressed as the other:
```rust
assert!(to_target <= 0.5,  "... — a no-op walk_to lands here");
assert!(advanced  >= 3.5,  "commanded a 4-block walk but the local prediction only advanced {advanced:.3}");
```
plus a **server-derived** precondition — the pre-move `position` can only have been written by a server `TeleportPlayer`, since nothing else sets it before the first `Move`, so requiring it `Some` proves the server really placed us and a v770 that stopped emitting the placement teleport fails here. The report line now reads `local prediction, not server-confirmed`, and the docs name what the stronger claim would require: **a second observer client watching our own entity**.

**Three things worth keeping.**

1. **This is §12.31's shape at the integration layer.** There, a JVM oracle validated the behaviour we chose to model in it rather than vanilla's, and three implementations agreed because they shared an author's misunderstanding. Here a client asserts against **its own prediction** — reader and writer are the same component, so the loop closes with the server outside it. Agreement between a thing and its own forecast is not evidence, and it looks exactly like evidence.

2. **Server-side proof does exist, and it is elsewhere.** §12.53's gate — 100 ticks, **zero corrective `player_position`**, with a permanent negative control proving a 30-block teleport *does* get snapped back — is the server certifying our movement. That remains valid. What was wrong was my *merging* of the two results: I read the public-API displacement as though it inherited the version-crate gate's server-side authority. Two honest tests, one unwarranted inference between them, and the inference lived in the plan rather than in any code.

3. **The mildest form of vacuity keeps being the durable one.** §12.52 was a gate asserting nothing; §12.67 was a real behaviour observed but unasserted; this is a real, asserted behaviour that measures **one seam less than its wording claims**. Each is harder to see than the last, and each passed review because the output looked like what we wanted.

Recorded as: **movement across the public API is real, asserted, and proven as far as the send path. Server-acknowledged displacement through the public API is not yet gated** — it needs the second-observer test, assigned to `impl-physics`. Note the failure message names what its failure does *not* mean, per §12.65.

**12.71 The critical path cleared, and the consolidation is verifiable in one command.** Authoritative measurement after the largest single round of change this session:

```
$ cargo check --workspace --all-targets   → exit 0, no errors
$ cargo test --workspace                  → PASS 1470  FAIL 0  IGNORED 56
                                            0 "could not compile", 0 "test result: FAILED"
```
Zero failures **and** zero non-compiling targets — the §12.62 invariant, not the count.

**Dispatch, measured against Mojang's own fixed denominators (§12.66):**

| | before | after |
|---|---|---|
| play clientbound dispatched | 38 / 141 (27%) | **46 / 141 (33%)** |
| play serverbound emitted | ~9 / 69 (13%) | **15 / 69 (22%)** |
| `ClientAction` variants declared | 7 | **22** |
| …encoded by v770 | 4 | **21 / 22** |

`ClientAction` now covers block interaction (`BlockAction` → `StartDestroy`/`StopDestroy`/`AbortDestroy`), item use (`UseItemOn`/`UseItem`/`ReleaseUseItem`), entity interaction (`InteractEntity` → `Interact`/`InteractAt`/`Attack`), inventory (`ContainerClick`/`ContainerClose`/`SetCarriedItem`/`SetCreativeModeSlot`/`SwapItemWithOffhand`/`DropSelectedItem`), and `PlayerCommand`/`SetPlayerInput`. **§12.51 is closed** — `SET_OBJECTIVE`, `SET_SCORE`, `RESET_SCORE`, `SET_DISPLAY_OBJECTIVE`, `SET_PLAYER_TEAM`, `BOSS_EVENT` all dispatch, alongside `SOUND`, `SOUND_ENTITY`, `LEVEL_EVENT`, `LEVEL_PARTICLES`, `BLOCK_UPDATE`, `SECTION_BLOCKS_UPDATE` and `OPEN_SCREEN`.

**Note the asymmetry this creates, which is the design working as intended (§3.4):** v770 encodes 21/22, v47 and v340 encode **5/22**. The canonical model is shaped by the newest protocol and older adapters translate upward, so a lag here is expected — but it means a 1.8.9 or 1.12.2 client still cannot break a block. That is now the clearest single gap and it is per-family work, not model work.

**§12.69's remedy landed and is checkable in one command**, which was the explicit requirement:
```
$ grep -rln "fn unique_username" --include=*.rs crates/ | wc -l
1     ← crates/lodestone-testsupport/src/lib.rs   (was 15)
```
The surviving implementation is the right one — `fetch_add` counter **first** in the string so the server's hard 16-character limit truncates the timestamp rather than the in-process discriminator, `pid << 21` mixed into the seconds so two processes starting in the same second still differ. Isolation is unaffected and was *verified* rather than assumed: `check-isolation` passes, and all three families remain **cleanly deletable** (v47 5 manifest lines, v340 4, v770 8).

**And §13's tracked metric is now a command**, reporting both denominators with the caveat inline so the optimistic one can't be quoted alone:
```
$ cargo xtask codegen-ratio
family  derive-blocks  manual-impls  struct-derived  generated-lines  hand-written-lines
v47                42             8            84%              708                2343
v340               46             8            85%              822                2420
v770               34             9            79%            22399                4294
```

**One warning moved rather than disappeared, and it is the §12.24 signal again.** `lodestone-client → lodestone-v770` is **gone** — that dependency was load-bearing precisely because the client could not deliver chunks, and it cleared *because the seam genuinely closed*, which is the outcome §12.24 demanded. In its place: `lodestone-render → lodestone-v770`, from `live_gate.rs` importing `V770Adapter` and `packet_ids::play` directly. Same shape, one crate over: the Phase-5 gate obtains its chunk by **reaching past the client's public API**. Now that `handle.chunk(pos)` and `sections_at` exist, routing it through the public surface would remove the dependency *legitimately*, prove the whole path (live server → client → `World` → mesher → pixels) instead of just the mesher, and close §12.52's fail-open sites in the same edit. Assigned — **not** silenced.

**12.72 The WebGL2 "fallback" was 537 KB of payload that could never have rendered a frame — and we only know because I refused to decide it on audience reasoning.**

The `webgl` feature question looked like a classic tradeoff: 537 KB brotli (786 → 249 KB, **68%** of the entire download) against the fraction of users whose browser lacks WebGPU — and WebGPU still needs flags on desktop Linux Chrome/Edge and Firefox, with Firefox Android unshipped. That framing invites an audience guess, and I nearly took it. Instead I instructed `wasm-spike` to first answer a prior question: **does the WebGL2 path render pixels at all?** §12.12 and §12.21 had both burned us on unexercised backend claims, so a fallback nobody had run was a hypothesis, not a fallback.

**It never rendered a frame.** Verified in Chrome — WebGPU renders, forced-GL panics before frame 0. Cause confirmed by me at source rather than accepted from the report:

```
crates/lodestone-render/src/block.rs:205   label: Some("lodestone-atlas-bgl")
crates/lodestone-render/src/block.rs:225       visibility: wgpu::ShaderStages::VERTEX,
crates/lodestone-render/src/block.rs:227       ty: wgpu::BufferBindingType::Storage { read_only: true },
```

The terrain pipeline binds a **vertex-stage storage buffer** — wgpu's `DownlevelFlags::VERTEX_STORAGE`, which WebGL2 categorically lacks — so `create_bind_group_layout` panics at construction. The feature was **pure cost**: two thirds of the browser download buying a code path that terminates before drawing anything.

**The decision therefore isn't a tradeoff at all, and that's the point.** Had I ruled on reach-versus-bytes, I would have reached a defensible-sounding answer to a question that had already been settled by the pipeline's own requirements — and either outcome would have been wrong in a way no measurement would ever have contradicted, because nobody was going to run the fallback. Re-adding WebGL2 now honestly costs a **downlevel-compatible render path** (no vertex-stage storage), not a feature flag; that's recorded in `web/Cargo.toml` beside the removal so the next person doesn't mistake it for a toggle.

**Generalises the §12.21 rule one step further.** There, availability and performance turned out to be independent questions and a "supported" capability was a CPU loop. Here, availability and *function* are independent: the feature compiled, linked, shipped, and inflated the bundle without ever being able to work. **Before pricing a fallback, run it.** A fallback that has never executed is not a fallback — it is dead weight with a reassuring name, which is the same family as §12.52's gate that passed while asserting nothing and §12.34's encryption branch that had never once executed.

Also landed and verified: browser singleplayer is unblocked — `lodestone-server`'s tokio is now target-split (`crates/lodestone-server/Cargo.toml:27`, *"`wasm32-unknown-unknown` cannot build `mio` (hence tokio's `net`)"*), with the `tokio::spawn`-has-no-browser-runtime hazard handled at the task spawner rather than left latent. And `lodestone-controller` now exists as a real crate (`action.rs`, `input.rs`, `lib.rs`), so the browser input path can be wired **without** forking movement — `wasm-spike` named its own `FlyCamera` as that fork risk and refused to proceed until the shared core existed. Refusing to build the thing that would have created a duplicate is the same judgement as `impl-entity` declining to half-build across a crate boundary.

**12.73 The `walk_to` shape was settled between two agents with me out of the path — the second time this has happened, and the reasoning was better than a ruling from me would have been.** `impl-shell` stated its consumer position: the interactive shell never calls `walk_to` at all, driving `input → lodestone_physics::tick → send_action(Move)` per tick, so a per-tick call must not allocate a future or return an outcome discarded 20×/second — while `walk_to` is a *bot/goal* primitive where `Ok(())`-on-timeout is precisely §12.67's vacuous shape. `impl-client` confirmed both seams had already shipped distinct. Verified myself:

```
crates/lodestone-client/src/handle.rs:402   ) -> Result<WalkOutcome, BotError> {
crates/lodestone-client/src/handle.rs:575   pub enum WalkOutcome {          // Arrived | TimedOut { remaining }
crates/lodestone-client/tests/live_bot.rs:181  assert_eq!(outcome, WalkOutcome::Arrived,
       "walk_to timed out before the local prediction reached the target: {outcome:?} \
        — a no-op or non-stepping walk_to lands here")
```

§12.67's `let _ = handle.walk_to(...)` is gone. The failure message names what would land there, per §12.65. And the test still declares in-line that this is the **local prediction, not server-confirmed displacement** (§12.70) — the claim did not inflate when the assertion was added, which is the failure I'd most expect at this step.

**The durable observation: "one function cannot serve both" was derivable only from the consumer's tick loop**, which no amount of API-design reasoning on my part would have surfaced. Both cross-agent resolutions so far (this and §12.68's varint gap) were closed by the two parties who held the relevant facts. That is the structure paying off in the way that matters — an orchestrator who insists on ruling everything is a serialisation point, and a wrong ruling from the top propagates further than a wrong one from a leaf (§12.32).

**12.74 My dispatch ratio counts packets that decode and throw the result away — so §12.51 is not closed, and I recorded that it was.**

`impl-game` mentioned in passing to `impl-model` that its six scoreboard/team/boss decoders "currently decode-and-drop (no emit) because there is no `ClientEvent` variant to carry them." My ratio had already counted all six. Checked:

```
$ awk '/clientbound::SET_OBJECTIVE/,/^        }/' crates/protocol/v770/src/adapter.rs
        if packet_id == play::clientbound::SET_OBJECTIVE {
            decode_and_validate::<SetObjective>(payload)?;
            return Ok(Vec::new());          ← decoded, validated, discarded
        }
```
All six identical. **Nothing reaches a consumer**, which is precisely the §12.51 defect the work was supposed to fix — a correct, well-tested `Scoreboard`/`TabList` that no packet ever arrives at. The decoders are real and good (zero-trailing plus known-value across every conditional branch, and the comments show real care: *"the display-name tail is present only for add(0)/change(2)… a wrong branch leaves trailing bytes, which ensure_empty rejects"*). But **§12.51 is open, and the plan said closed.**

**This is §12.66 recurring on the numerator instead of the denominator.** There I built a ratio to make progress falsifiable and never checked where its denominator came from (265 is all five states and both directions, not `play::clientbound` = 141). Here the denominator is finally right and the *numerator* counts the wrong event: "appears in `handle_play`" is not "reaches the client." A metric invented specifically to measure **connectedness** was counting **decode coverage**, which is the thing the metric exists to distinguish from.

**And my first attempt to measure it properly was also wrong — the §12.69 shape, third time.** A classifier scanning each `if packet_id == …` block for `ClientEvent::` reported 15 emit / 25 drop, listing `ADD_ENTITY` and `PLAYER_POSITION` as drops. I had personally watched those produce `EntitySpawned: 26, EntityMoved: 25, TeleportPlayer: 1` on a live run, so the check contradicted an observation I trusted more than the check:
```
$ awk '/clientbound::ADD_ENTITY/,/^        }$/' … 
        if packet_id == play::clientbound::ADD_ENTITY { return handle_add_entity(payload); }
```
It delegates. Following one level of delegation inverts the answer to **29 emit / 11 drop**. **When a check disagrees with something you have directly observed, the check is the suspect** — and every time this has happened (15/15 usernames "fixed", the conflated capability predicate, this) the flaw was that the check tested a *proxy* for the property rather than the property.

Of the 11, only **seven are genuinely stranded**: `SET_OBJECTIVE`, `SET_SCORE`, `RESET_SCORE`, `SET_DISPLAY_OBJECTIVE`, `SET_PLAYER_TEAM`, `BOSS_EVENT`, `PLAYER_INFO_REMOVE`. The rest are correct by design and must **not** be "fixed":
- `BLOCK_UPDATE` / `SECTION_BLOCKS_UPDATE` call `world.set_block(...)` — world state deliberately does not travel the event channel (§12.33), because the channel is bounded and a missed update is an unrecoverable hole.
- `DISCONNECT` returns a `Directive::Disconnect`, a different and correct outlet.

**So the honest scoreboard is:** play clientbound **40 handled / 141**, of which **29 reach a consumer** (event, world sink, or directive) and **7 are decoded-and-stranded**. Tracking "handled" alone would have let the stranded seven sit indefinitely behind a rising number — exactly what a connectedness metric is supposed to prevent.

**The fix is cheap and the blocker is already gone**, which is the other reason this was worth catching now: `impl-game` reported the missing carriers as a type-ownership question (its `Scoreboard`/`Team`/`BossBar` live in `lodestone-game`, which `lodestone-model` cannot reference), and `impl-model` had **already landed model-owned minimal carriers** — `ObjectiveUpdate`, `DisplayObjective`, `ScoreUpdate`, `ScoreReset`, `TeamUpdate`, `BossBarUpdate` — and ruled correctly against moving game state down into the model. The flip is one line per arm. Two agents had each done their half; neither knew the other had.

**Standing change to the metric:** report **reaches-a-consumer / 141**, not handled / 141, and count the three legitimate outlets (`ClientEvent`, world sink, `Directive`) rather than grepping for `ClientEvent::`. A packet that decodes and drops is genuinely better than one that is ignored — it proves the codec — but it is **not** connectedness, and only the stricter numerator can tell the two apart.

**12.75 §13's fatal risk materialised on the first use of `xtask new-version` — and the tool diagnosed it correctly, out loud, into a medium with no enforcement.**

A fourth family, **`v735` (1.16.x)**, appeared. `cargo xtask codegen-ratio` reports its hand-written line count as **2795 — byte-identical to v340's 2795**, which is the signature of a clone. Checked file by file:

```
identical=12 differs=5
differs    ./adapter.rs  ./generated/packet_ids.rs  ./generated/entity_types.rs  ./lib.rs  ./packets/metadata.rs
IDENTICAL  ./packets/chunk.rs  ./packets/slot.rs  ./packets/game.rs  ./packets/window.rs
           ./packets/position.rs  ./packets/entity.rs  ./packets/common.rs  … (12)
```

**Every packet body except `metadata.rs` is still 1.12.2's wire shape, carrying 1.16 packet IDs.** 1.12.2 → 1.16 crosses four of §3.3's hardest boundaries at once: flattening (1.13), the light split (1.14), 3-D biomes (1.15), and non-straddling long packing (1.16) — the last being exactly the `LongArrayFraming` seam §7.2 records as "true since 1.16."

**Verified against an external authority rather than my own recall (§12.42).** minecraft-data, which we did not write:
```
1.12.2  map_chunk contains heightmaps: False   biomes: False
1.16.5  map_chunk contains heightmaps: True    biomes: True
```
Two top-level fields that 1.12.2's packet does not have, both positioned before the section data — so a v340 decoder misparses a 1.16 chunk immediately.

**Three failure modes stacked, and the crate looks finished from every angle:**
1. `crates/lodestone-registry/src/lib.rs:73-76` registers `v735` as a **supported version** with no incompleteness marker.
2. It ships a full cloned test suite, including `tests/live_chunk.rs` — which still reads `#[ignore = "requires a live 1.12.2 server on 127.0.0.1:25568"]` and **connects to the 1.12.2 container**. A 1.16 family whose live gate certifies it against a 1.12.2 server passes, and proves nothing. §12.52's vacuity, arriving by clone rather than by `read_dir`.
3. Nothing anywhere records that shape review is outstanding.

**The tool was not at fault, and that is the whole lesson.** `scaffold_new_version` emits a residue list, and its first entry says *verbatim*:

> `review packet structs under crates/protocol/v735/src/packets/ — they are v340's wire shapes; change the ones that differ for protocol 735`

It named the exact defect, in the exact directory, at generation time. **Then the same command wired the family into the registry as supported.** The warning went to stdout and evaporated; the registration persisted to disk. One command emitted a true signal and an opposite fact, and only the fact survived.

**This is the project's signature failure in its purest form yet.** §12.19 was a correct doc comment nobody had to obey; §12.37 a doc asserting an invariant with nothing enforcing it; §12.52 a gate that skipped instead of failing. Here the diagnosis was *perfect* and still worthless, because **a warning is not a constraint**. The repeated lesson — "when the type system cannot express a constraint, make it checkable and check it in CI" (§12.35) — applies to generated residue too: **residue printed is residue lost.**

**Structural fix (assigned, not patched):** `new-version` must **fail closed**, exactly as §12.52 ruled for preconditions.
- Write the residue to disk as a checked artefact (`SHAPE_REVIEW.toml`) in the new crate, one entry per packet whose minecraft-data shape differs between `--from`'s version and the target's — the diff is *computable*, since minecraft-data covers 1.7→1.21.11 and Mojang's report covers ≥1.14, so this need not be a generic "review everything" note.
- The family must not be registerable while entries are undischarged: a test (or `check-isolation`-style gate) fails while any entry lacks `reviewed = true`.
- **Never clone a live test.** A cloned live gate pointing at the source family's server is worse than no test, because it manufactures evidence for the wrong version. `new-version` should refuse to copy `tests/live_*.rs`, or emit them `#[ignore]`d with a `panic!` naming the correct server.

**§13's mitigation is now empirically wrong as written and must be rewritten.** It claims human effort is spent only on packets whose *shape* changed, with `new-version` diffing and reporting them. The reporting exists; the *enforcement* does not, so the actual observed outcome on first use was a family that skipped shape review entirely and presented as complete. Combined with `impl-macros`' independent ruling that **adapter dispatch is not meaningfully derivable** ("ID routing is mechanical; lowering/raising to `ClientEvent`/`ClientAction`, world side effects, registry lookups, teleport replies and chunk-shape state are semantic per-version work"), the honest statement of §13 is: **codegen covers packet IDs and registry tables — the cheap part — and covers neither dispatch nor wire-shape migration, which are the bulk and the risk.** The marginal cost of a family is ~2.3–5.1k hand-written lines of genuinely semantic work, and `new-version`'s value is scaffolding plus a *checklist*, not avoidance of that work.

**12.76 A connection-breaking bug that the whole test suite is structurally blind to — because the blindness is *duration*, not assertion or precondition.**

`CHUNK_BATCH_RECEIVED` has **zero occurrences** in the v770 crate, and `CHUNK_BATCH_START`/`CHUNK_BATCH_FINISHED` are unhandled. Verified in Mojang's source, `.cache/mc/26.2/src/net/minecraft/server/network/PlayerChunkSender.java`:

```java
private static final int MAX_UNACKNOWLEDGED_BATCHES = 10;          // :28
if (this.unacknowledgedBatches < this.maxUnacknowledgedBatches) {  // :51
    ... this.unacknowledgedBatches++;                              // :61
}
public void onChunkBatchReceivedByClient(final float desired) {    // :114
    this.unacknowledgedBatches--;                                  // :115
```

`unacknowledgedBatches` is decremented **only** by the client's ack. Lodestone never sends it, so the server dispatches 10 batches and then gates `sendNextChunks` off **permanently for the session**. Observable symptom: spawn chunks load, then chunk delivery silently stops forever — walking produces void. This is not a cosmetic gap; it makes the client unusable past the spawn area against any server ≥1.20.2.

**Why nothing caught it, and this is the transferable part.** Every live chunk gate we have connects, receives the spawn area, asserts, and disconnects — never staying alive long enough to exhaust 10 batches. The test is *correct*: its assertion is real, its precondition fails closed, it uses a live server. It is simply **shorter than the time-to-failure**.

So this is a third, distinct species of vacuity:
- §12.67 — vacuous **assertion** (`Ok(())` swallowed the outcome)
- §12.52 — vacuous **precondition** (missing fixture skipped instead of failing)
- **§12.76 — vacuous *duration*** (the property held at t=2s and was never probed at t=30s)

The first two are found by reading the test. **This one cannot be** — the source looks exemplary. It is only findable by asking "what does this test prove about the system *later*?", which is a question about the system's state machine, not about the test. **Any property governed by a counter, a quota, a token bucket, a timeout, or a keepalive is invisible to a gate that finishes before the counter saturates.** The audit question to add to the standing list: *does any server-side counter accumulate across the session, and does our gate run long enough to reach its limit?*

Corollary for the regression test: asserting "we send the ack packet" would pass against a stubbed encoder and prove nothing. The property is **"chunk delivery does not stall"**, so assert *that*, over a distance that forces >10 batches, with a negative control confirming the test fails when the ack is suppressed — otherwise we cannot distinguish "no stall" from "never reached batch 11."

Also unhandled and in the same class of stream-breaking: **`START_CONFIGURATION`** (server pushes play → configuration mid-session for resource-pack/datapack reload and `TRANSFER`). `adapter.rs:1546` already routes `ConnectionState::Configuration`, so the state machine can express it; the play handler simply never triggers the transition, and every packet after such a push misparses.

**The audit question immediately found a second instance.** `ServerGamePacketListenerImpl.java:1775-1783`:
```java
this.lastSeenMessages.addPending(signature);
trackedCount = this.lastSeenMessages.trackedMessagesCount();
if (trackedCount > 4096) {
    this.disconnect(Component.translatable("multiplayer.disconnect.too_many_pending_chats"));
}
```
The server pushes every **signed** chat message it sends into a pending list, drained only by the client's `last_seen` acknowledgement. `PLAYER_CHAT` is unhandled, so we never acknowledge, so on any populated server the count climbs monotonically and at **4096 signed messages we are disconnected**. Note the qualifier: only messages with a non-null signature accumulate, so system and disguised chat are exempt — it is *player* chat specifically. The acknowledgement machinery is half-present (`packets/game.rs:68-88` already models `last_seen_offset` plus the 20-bit acknowledged bitset and checksum byte), but nothing maintains it, and `ServerboundChatAckPacket` — the means of acknowledging *without* sending a message — is absent entirely.

Two independent counters, both verified at Mojang's source, both fatal, neither observable from any test we have: **10 batches → chunks stop; 4096 signed messages → kicked.** The first bites in seconds, the second after hours on a busy server, which is exactly why a short gate sees neither. This is now a standing audit obligation, not a one-off finding.

**12.77 The game runs.** First end-to-end evidence that this is a client and not a collection of crates:

```
$ cargo build -p lodestone-shell && ./target/debug/lodestone
INFO starting lodestone config.mode=Window
pos=(0.5,46.0,0.5) facing=north (-Z) mode=walk fps=49 frame=19.13ms
chunks=169 live_cols=0 sections=489 quads=160688 vram=11298KB world=1414KB rss=0MB local world
```

A window opens, a local world generates, and it renders **169 chunks / 489 sections / 160,688 quads at a steady ~48 fps**. `live_cols=0` — that run was singleplayer, so the network path was not exercised.

Two defects observed in the same run, one of which is a §12-class problem rather than a bug:
- `--help` **launches the game** instead of printing usage; flags are parsed after window/world init.
- **`rss=0MB` — the memory gauge reads a flat zero** while `vram` and `world` report correctly (macOS needs `task_info`/`proc_pidinfo`, not the Linux `/proc` path). This matters more than its size: memory efficiency is an explicit user requirement, and §11's whole story (slab chunks, arena, the 77.6 MiB @ RD32 figure) is only defensible if process memory is observable. **A gauge reading zero is worse than no gauge** — it gets glanced at and believed, silently converting "unmeasured" into "fine". Precisely the §12 pattern: a signal that looks like evidence and isn't. Fix it or delete the field; do not leave a zero in a HUD.

**What this does not yet show:** nothing draws the player-facing state. `lodestone-game` models `hud`, `chat`, `tablist`, `scoreboard`, `container`, `menu`, `bossbar`, `effect` — and the renderer draws none of them, exactly as it draws none of the 8 entity models that exist in `lodestone-assets`. The §12.24 shape at UI scale. Scoreboard and tab list were explicit user requests and now have both a model *and* live packet data arriving, so the remaining gap is purely the last mile of drawing.

**12.78 The browser build renders real terrain — verified by screenshot, not by frame counter.** Served `web/dist` and attached DevTools:

```
[status] rendering — backend: BrowserWebGpu | select_strategy(): PerDraw
[assets] atlas: real vanilla pack, 3 blocks → 4 sprites, 32×32 px (deflate+PNG decoded in-browser)
[status] REAL terrain from real server bytes — 16 chunks, 16 sections, 250 greedy quads
[frame]  frame 1380 | 8.18 ms/frame (~122 fps)
```

**The screenshot is the load-bearing evidence, not the frame counter** — a frame counter ticks perfectly happily over a blank canvas, which is exactly the availability-vs-function trap of §12.21 (a "supported" multi-draw that was a CPU loop) and §12.72 (a WebGL2 path that shipped 537 KB and could not draw frame 0). The capture shows greedy-meshed terrain, vanilla stone texturing, correct perspective and sky. Real pixels, real vanilla assets, real server bytes, in a browser, at 122 fps. The user's WebAssembly goal is met at spike level, and the WebGPU-only ruling is vindicated by that `BrowserWebGpu` backend line.

Two defects found in the process:
- **An unhandled `TypeError` on the relay failure path.** `WebSocket … ERR_CONNECTION_REFUSED` is immediately followed by `Uncaught TypeError: Cannot read properties of undefined (reading 'length')`. The render loop survives, which is *worse* than crashing: the page runs on at 122 fps while the network layer is dead, traced only by a console error nobody reads. The browser flavour of every fail-open in §12.52 — the failure must reach the on-page status line, not merely be survivable.
- **`trunk` is not installed**, so `trunk serve` cannot run; `dist/` had to be served by a plain static server. The user asked for trunk specifically. Note the asymmetry this creates: `Trunk.toml` sets COOP/COEP up front (correctly, for future `wasm-bindgen-rayon` threading) and a plain static server does not — so anything later depending on cross-origin isolation will work under trunk and fail mysteriously elsewhere.

Also: **14.9 MB uncompressed `.wasm`** (≈933 KB brotli). Compressed size governs transfer, but *uncompressed* size governs browser compile time and memory, so it is the figure to watch as more of the game lands.

**12.79 Server-confirmed displacement — the physics claim is now end-to-end, not self-referential.** The longest-standing hole in the project is closed:

```
=== LIVE SECOND-OBSERVER GATE REPORT ===
walker entity id (in B) : 25487 (minecraft:player)
walk ticks              : 17
walker own displacement : 3.6550 blocks (local prediction)
OBSERVER displacement   : 3.6550 blocks (server-broadcast, B != A)
parity gap              : 0.0000 blocks (<= 0.5)
negative control drift  : 0.0000 blocks (A idle, B observes)
test observer_confirms_walker_displacement ... ok
```

Two **independent connections** to a real vanilla 26.2 server: client A walks under our physics; client B — a separate client that has never seen A's local state — observes A's entity through the server's own broadcast. Agreement to four decimal places, with a paired negative control (A idle ⇒ B observes zero drift) that distinguishes "the observer tracks A" from "the observer reports whatever we expect."

**Why this is categorically stronger than everything before it.** Every prior movement assertion was self-referential: our simulation agreed with our simulation (§12.67's `walk_to` was explicit that it proved *local prediction* only), and even the 21-scenario JVM parity suite compares our physics to a JVM harness we drive. This gate closes the loop through an **oracle we do not control** — the server's own entity-position broadcast, read by a client that shares no state with the mover. It upgrades the claim from "our physics matches the JVM in isolation" to **"a real vanilla server accepts our movement and tells another player where we are."** That is the claim the whole project rests on, and it was previously untested.

The retry discipline is worth copying: runs where the walk lane was obstructed were **discarded as interference** rather than allowed to pass, and the report prints the discard reason. A live gate on a shared world has real environmental noise, and the honest handling is to detect contamination and re-run, never to loosen the tolerance until it passes.

Note the gate is behind **both** the `live-v770` feature and `#[ignore]`, so the invocation is `cargo test -p lodestone-client --features live-v770 --test live_second_observer -- --ignored`. Without the feature the file compiles to *zero* tests and reports `ok` — I hit exactly that and briefly believed it had run. That is acceptable as a double opt-in (§12.52: an ignore is already opt-in), but **`test result: ok. 0 passed` is indistinguishable from success at a glance**, so the invocation belongs in the docs at every call site.

**12.80 Scope decision: cut from 17 version families to 3–4.** Two independent findings converged on this today, and together they make the original target the wrong plan rather than merely an ambitious one.

- `impl-macros`, asked directly whether adapter *dispatch* could be generated, ruled it **cannot**: "ID routing is mechanical; lowering/raising to `ClientEvent`/`ClientAction`, world side effects, registry lookups, teleport replies and chunk-shape state are semantic per-version work." That is the well-argued "irreducibly hand-written" answer that is worth more than a macro automating the easy 30%.
- §12.75 showed **shape migration** is equally irreducible: `new-version` cloned v340 → v735 correctly and mechanically, and the result was a 1.12.2 client wearing 1.16 packet IDs.

So codegen covers packet IDs and registry tables — the cheap part — and covers **neither** dispatch nor wire-shape migration, which are the bulk *and* the risk. Measured marginal cost per family is **~2.3–5.1k hand-written lines** of genuinely semantic work (`cargo xtask codegen-ratio`). Thirteen further families is therefore ~40k lines of individually-verified work, and it is the least interesting work in the project.

**Recommended target: 26.2 (v770), 1.16.5 (v735), 1.12.2 (v340), 1.8.9 (v47).** Modern, mid, and legacy — enough to demonstrate the multi-version claim the user actually asked for, to exercise every hard boundary (flattening, light split, 3-D biomes, long packing, pre-palette 1.8), and to prove the folder-deletion modularity property. Choosing 1.16.5 over 1.20.1 for the mid slot is pragmatic rather than principled: **v735 is already scaffolded**, so repairing it costs only the shape migration (§12.75) instead of a fresh family, and 1.16.5 is at least as widely deployed. It also happens to be the single most instructive family to get right, since 1.12.2 → 1.16 crosses four hard boundaries at once. `xtask new-version` plus an enforced `SHAPE_REVIEW.toml` remains the documented path for anyone adding more. **Depth on four beats breadth across seventeen**, and the honest framing is that the architecture supports seventeen while the schedule funds four.

**12.81 Cost discipline (user-directed).** Two changes to how this session runs:
- **Verification batched rather than per-report.** Independently re-running every agent claim caught four real problems today (the bogus dispatch metric, v735, the chunk-batch cliff, the chat cliff), so it was not waste — but it does not need to happen per report. Switch to periodic sweeps, reserving deep verification for claims that are load-bearing, or that contradict something directly observed (the standing rule that has never yet been wrong: **when a check disagrees with an observation, the check is the suspect**).
- **Sonnet-class models for implementation.** The remaining work is dominated by high-volume, low-ambiguity breadth: ~87 clientbound arms, ~53 serverbound, ~120 entity model ports. That is an ideal fit. Reserve the expensive reasoning for orchestration and for seams where being wrong is *silent* — which is precisely where every serious defect this session has been found.

**12.82 A fourth species of vacuity: the vacuous *world*. The gate is impeccable; the input cannot exercise the property.**

`impl-world` built a light propagation engine and `impl-v770` gated it against the server's own light. I re-ran it myself:

```
$ cargo test -p lodestone-v770 --features live-chunk --test live_chunk -- --ignored \
      computed_light_matches_server_oracle_on_flat_world
computed sky light differs from server in 0 of 24576 cells
negative control (all-zero light): 5120 cells differ
test result: ok. 1 passed
```

Every rule this project has accumulated is satisfied. It is **live** (real vanilla server, not a fixture). It **fails closed** (missing server → panic, per §12.52). It **counts cells rather than returning a boolean**, so it names the magnitude of a divergence. It has a **paired negative control** proving the comparison can fail (§12.53). By every criterion in §12 it is a model gate.

**It runs on a superflat world with `interior_margin: 0`.**

A superflat world is a stack of uniform horizontal layers under open sky. Every column is identical, so **sky light never has to spread sideways** — the one behaviour that BFS light propagation exists to compute, and the exact behaviour `impl-world` independently flagged as most-likely-wrong and pinned with a hermetic unit test. The gate cannot see caves, overhangs, or horizontal decay under a ledge. `0 of 24576` is a real measurement of a property that is trivially true on this input.

**This is a distinct failure mode from the three already catalogued, and the distinction is where it hides:**

| species | § | where the flaw lives | visible when reading the test? |
|---|---|---|---|
| vacuous **assertion** | 12.67 | the assert (`let _ =`, printed-not-asserted) | **yes** |
| vacuous **precondition** | 12.52 | the setup (missing fixture → skip) | **yes** |
| vacuous **duration** | 12.76 | the test's lifetime vs the system's counters | no |
| vacuous **world** | 12.82 | the *input data* | no |

The first two are found by reading the source. The last two cannot be — the source is exemplary in both cases, and the flaw is a property of what the test was pointed at. §12.76's audit question was *"does a server-side counter accumulate past our test's lifetime?"*; this one's is **"does the input actually contain the structure the code under test exists to handle?"**

Both are questions about the *system*, not the test — which is why a code review, however careful, cannot answer either.

**Corollary, and the reason this is worth a numbered entry:** a passing gate on a degenerate world is worse than no gate, because it terminates the search. Nobody writes the hills-and-caves test after seeing `0 of 24576 cells differ` on a green light gate. Same shape as §12.44's silent-ogg (two silent buffers agree perfectly) and §12.31's flush contacts (two formulations coincide when the gap is zero) — **the comparison is satisfied by degeneracy rather than by correctness.** Assigned: re-run against real generated terrain with a genuine `interior_margin`, and report unmapped-block-state occupancy as a gate-visible number rather than a silent default-to-opaque.

**12.83 The world on screen is not the world we verified. Third island in one day, and the best-evidenced subsystem in the project is the one nobody can see.**

Asked `impl-worldgen` the standing connectedness question — *what actually consumes you?* — rather than asking how the generator was going. Answer:

```
lodestone-worldgen → lodestone-server → (no shipped consumer)
crates/lodestone-shell/src/worldgen.rs   189 lines, sine + hash stand-in
```

**The shell has its own generator.** So §12.77's headline — *"169 chunks / 489 sections / 160,688 quads at ~48 fps, a local world generates and renders"* — was rendering a **placeholder**, and I cited it repeatedly as evidence the pipeline works. The `live_cols=0` in that same HUD line was the tell, and I read it as "singleplayer, so no network" rather than "nothing you verified is involved."

The parity evidence stranded behind that seam is the strongest in the codebase, all bit-exact against a JVM oracle, element-wise, on real fixtures:

```
noise router      34048 / 34048   whole region
final density     98304 / 98304   whole chunk, interpolated (4×8×4 cells, trilerped)
carvers           98304 / 98304   × 2 chunks
surface + aquifer land and ocean profiles
ore features      whole-chunk exact BOTH directions, 3 fixtures / 2 seeds / 2 terrain profiles
                  (2843, 5141, 3619 changed blocks — no missing, no extra)
```

**Three islands in one day is a pattern, not a coincidence** — `lodestone-entity` (9,571 lines, zero dependents), the entity render stack (terminating in its own test), and now worldgen. Each was built to a high standard *because* isolation is what makes parallel work possible; each was invisible to every test in the repo, because a per-crate suite structurally cannot observe that the crates don't connect (§12.57). The remedy that keeps working is not more tests but **asking each owner what consumes them, and treating "nothing" as a defect report rather than a status update.** All three were found by that one question.

**The ruling: direct call now, integrated server later.** Vanilla runs singleplayer as an integrated server, and `impl-entity` has just built a mob tick in `lodestone-server`, so the faithful destination is server-generates → loopback → client-consumes, sharing the multiplayer path. I explicitly deferred it. A two-hop island closed today by a direct call from the shell is worth more than the correct architecture arriving tomorrow, provided the *generator* doesn't have to change when the call site is replaced — which it doesn't. Recorded so the shortcut is a decision with a named successor rather than drift.

**The ore bug `impl-worldgen` caught is the class that matters most here.** Buried ores draw a `nextFloat` inside `shouldSkipAirCheck` before the 6-neighbour air test; short-circuiting them desynchronises the shared RNG stream and **three ore families silently vanish**. A wrong draw *count* is invisible to any test asking "did ores appear" and instantly fatal to whole-chunk parity. This is why the aggregate-statistics gate matters alongside exact-match: exact-match on one chunk catches a wrong draw order, and count bands catch a plausible-but-wrong distribution. Neither catches the other.

**12.84 The self-consistent fixture: a hermetic test built with your own encoder cannot detect your own misreading of the wire format.**

`impl-v47` migrated v735 (protocol 754 / MC 1.16.5) and its hermetic chunk fixtures passed throughout. Then the live gate ran against a real 1.16.5 server:

```
49 × "unexpected end of input"
```

The `map_chunk` decoder was missing the **1.16.2 biomes varint length-prefix** — biomes became a length-prefixed `varint[]` in 1.16.2, where 1.15 and 1.16.1 sent a bare fixed 1024-int array. The fixtures could not possibly have caught it, because they were generated with `PalettedContainer::encode()` — *our own encoder*. Encoder and decoder shared one misunderstanding, so the round trip closed perfectly on bytes no server would ever send.

This is the concrete instance of the general warning that `decode(encode(x)) == x` is weak: **a round trip tests self-consistency, not conformance.** Both halves are written from the same mental model, so a wrong model produces a green test. The failure is not a missing assertion or a degenerate input — the fixture is rich, the assertions are real, and the test is genuinely exercising the code. What's wrong is the *provenance of the expected value*.

The rule this yields, and it generalises past chunk codecs: **an expected value must originate outside the code under test.** Bytes captured from a real server, a JVM oracle's output, a hand-decoded spec example — any of these can falsify a misreading. A fixture minted by our own encoder cannot, by construction. Where a live capture isn't practical, at minimum the fixture should be *checked in as bytes* the first time it's validated against reality, so a later regression in the encoder can't silently move the goalposts for the decoder.

Note how this interacts with §12.82: there, the input was degenerate and the assertion was fine; here, the input is rich and its *authorship* is the flaw. Together they say the same thing from two directions — **the quality of a gate is bounded by where its inputs and expectations came from, and neither is visible when reading the test.**

Also from this migration, worth keeping: the fail-closed `SHAPE_REVIEW.toml` gate did its job — v735 was de-registered until all 62 packet entries were audited. And a latent bug surfaced in the registry's own test, `default_build_has_no_families`, which guarded only `v47`/`v770`; a build enabling only `v340` or `v735` would have failed it. A guard that enumerates rather than generalises rots the moment the set grows, which is the same defect class as §12.20's hardcoded version list.

**12.85 Five islands in one day. The pattern is structural, and the question that finds them costs one sentence.**

Running tally of subsystems built to a high standard that nothing consumed, all found today, none detectable by any test in the repo:

| subsystem | size / evidence quality | what consumed it |
|---|---|---|
| `lodestone-entity` | 9,571 lines; vanilla goals, brains, pathfinding | nothing — every tick call inside `#[cfg(test)]` |
| entity render stack | full instanced pipeline, real Metal-adapter pixel gate | its own test |
| `lodestone-worldgen` | bit-exact vs JVM oracle: 34048/34048 region, 98304/98304 chunk, ores exact both directions | `lodestone-server` only |
| `lodestone-server` | integrated server + mob tick | **nothing** |
| vanilla texture pipeline | `blockstate/model/bake/atlas/tint/mipmap` in `lodestone-assets` | `lodestone-render`, which the shell bypasses |

The last one is the sharpest, because it contradicts an explicit user requirement — *"make it compatible with the vanilla resource packs from the real game… use the real texture pack by default"* — while `crates/lodestone-shell/src/blocks.rs` renders a **hand-authored procedural atlas**. I found it by running the binary, not by reading code.

**Why per-crate testing is structurally blind to this.** Every one of these crates has a green, genuinely rigorous suite. A test in crate A can only observe crate A. Nothing in a per-crate suite can express *"and something ships this"* — that assertion has no home. The integration tests that would catch it are exactly the ones that don't exist yet, because integration is what's missing. So the defect is invisible to the entire testing apparatus **by construction**, not by oversight.

**Why parallel agents amplify it.** Isolation is what makes 25 agents productive: each owns a crate, edits nobody else's, and lands compiling units. That same isolation means **no agent's remit includes the seam between two crates**, so seams are the residue nobody owns. Each agent honestly reports "my crate is complete and tested," and all of them are right.

**The remedy is one question, asked of the owner: *what actually consumes you?*** All five were found that way, in under a minute each, by treating "nothing" as a defect report rather than a status update. It works because the owner always knows — `impl-worldgen` answered *"nothing; the shell has its own 189-line stand-in"* immediately and without prompting. Nobody was hiding anything; nobody had been asked.

Two corollaries worth keeping:

1. **Verify the graph, don't accept the summary.** `impl-worldgen` reported one island; `grep -rln "lodestone-server" --include=Cargo.toml` showed it was two hops, because the server has no dependents either. I had also just congratulated `impl-entity` for "closing" its zero-dependents problem when its mob tick had merely moved into the larger island. The dependency graph is cheap to check and I should check it before praising a connection.

2. **The demo metric was measuring the stand-in.** `169 chunks / 489 sections / 160,688 quads / 78.1% coverage` is real rendering of *placeholder terrain* with *placeholder textures*. Both halves of the thing I was pointing at as evidence were stubs. `live_cols=0` in the same HUD line was the tell twice over, and I read it as "singleplayer, so no network" rather than "nothing you verified is in this picture."

**12.86 The sixth island is the dangerous one: an unconsumed crate whose *duplicate* ships.**

`impl-game` mentioned in passing that `lodestone-client` doesn't depend on `lodestone-game`, and that the client's `Scoreboard`/`BossBar`/`TabList` are reimplementations — the client's own docs say they *"deliberately mirror `lodestone-game`'s fold semantics."* Following that one hop:

```
$ grep -rln "lodestone-game" --include=Cargo.toml crates/ apps/ | grep -v lodestone-game/
(nothing)
```

`lodestone-game` has **zero dependents**, and it holds sixteen modules — `click`, `menu`, `container`, `reconcile`, `recipe`, `effect`, `hud`, `item`, `chat`, `chat_ack`, `player_state`, `progress`, `bossbar`, `scoreboard`, `tablist` — including the click machine that agrees with vanilla on all ten click types and now round-trips through a live server.

**This is categorically worse than islands one through five.** Those were merely unconsumed: a gap, and a gap is visible the moment anyone looks. Here a *second implementation of the same server-state fold* is the one that ships. Two folds, two crates, two agents, agreeing today by careful intent — and diverging the first time one of them fixes a bug the other doesn't. Nothing fails when they diverge; the two just quietly stop describing the same world.

**The manifest caused it, which matters for assigning blame correctly (there is none).** `lodestone-game` declares an *optional* `lodestone-client` dependency for its live tests. From the client's side, depending on `lodestone-game` therefore **looks circular**, so reimplementing looked like the only available move. It wasn't — that dependency belongs in `dev-dependencies` and the cycle then vanishes — but nothing in the tree said so. `impl-client` did the responsible thing under the constraint as it appeared, and documented that it was mirroring. The defect is in the manifest, not the judgement.

**Ruling.** The layering the crates almost already have:

```
lodestone-client   session, protocol, world   →  emits ClientEvent
lodestone-game     folds ClientEvent → game state (menus, scoreboard, tablist, bossbar, hud)
lodestone-shell    drives client, folds through game, renders
```

`lodestone-game` already depends on `lodestone-model`, where `ClientEvent` lives, so it needs nothing from the client to do its job. Three steps, of which 2 and 3 must land together: game's client dep becomes dev-only; shell adds game and switches its reads; client deletes `scoreboard.rs`. Asymmetry decides direction — one duplicated module versus sixteen.

**This also relocated `impl-game`'s own conclusion, instructively.** It had reasoned the inventory read model belonged in `lodestone-client`, *because that is what the shell already reads* — correct given the graph as it stood, and wrong once the graph is fixed. The read model is `ClientMenu`/`Menu`, which already exists and is live-proven. Its instinct not to build a session aggregate no packet reaches was right; the fix was one layer further out than the instinct could see. **A correct inference from a broken graph yields a wrong destination** — which is the general reason these islands are worth fixing rather than routing around.

**The `MenuKind` trap, recorded because it will otherwise be rediscovered as a rendering bug.** Window 0 is `0` result, `1..=4` craft, `5..=8` armour, `9..=35` main, `36..=44` hotbar, `45` offhand. A `Generic{n}` container is `0..n` container, `n..n+27` main, `n+27..n+36` hotbar — **no armour, no offhand, hotbar not at 36**. A consumer assuming a constant offset draws a plausible, wrongly-transposed inventory: it looks like an art bug and gets chased in the renderer.

**12.87 The measured cost of a version family: ~900 irreducible lines, not 2.3–5.1k. My estimate overstated the floor by ~3×.**

`impl-v47` has now migrated a family twice, so I asked for the real number rather than the estimate I'd been quoting. It measured v735 (protocol 754 / 1.16.5) rather than trusting the codegen-ratio line:

| bucket | lines | nature |
|---|---|---|
| generated (`packet_ids` 841 + `entity_types` 123) | 964 | xtask; zero human knowledge |
| hand-written total | 3007 | of which… |
| · doc/comments | 997 | a third of "hand-written" is prose |
| · blank | 181 | |
| · **actual code** | **1829** | the real target |

And within that 1829:

```
adapter.rs    712   dispatch / choreography / lower / raise      IRREDUCIBLE
chunk.rs      191   paletted decode, biomes prefix, 1.14 light split, flattening   IRREDUCIBLE
metadata.rs   211   typed union + per-version type-id table      semi-reducible
hand codecs   ~200  JoinGame/Respawn NBT, slot, position         macro-closable
derived decls ~515  #[derive] + field lists                      mechanical
```

**Genuine irreducible per-version knowledge ≈ 900 code lines.** My "2.3–5.1k hand-written lines per family" figure counted docs, blanks and derived struct declarations — all of which are either prose or mechanical. That matters for scope: at ~900 lines of real knowledge, the four-family stopping point is conservative rather than tight, and a fifth family is a day's work rather than a project.

**The leverage finding, which corrected a plan of mine.** I had assumed the missing NBT *writer* was the blocking gap and that closing it would reduce every family's cost. `impl-v47` split it:

- The **client** residue — `JoinGame`/`Respawn` hand *decode*, ~90 lines in **every** family — needs only a **reader-side `#[mc(nbt)]`** that captures a raw named-NBT span into `Vec<u8>`, wrapping core's existing `read_named_nbt`. The client round-trips raw bytes; **no writer required.** The same attribute closes `slot.rs`'s NBT tail, which already stores raw bytes for exactly this reason.
- The full NBT **writer** is a separate, larger capability needed only by the **encode/server** path — `bulk-encoders` synthesizing dimension codecs for the integrated server. It reduces no client family's cost at all.

So the single highest-leverage change is a small reader-side macro attribute, and the thing I'd been treating as the priority is real but serves a different consumer. Two features I had conflated into one.

Not closable today: position bit-packing (~15 lines/family) is a per-family-local win, and `Slot`'s enum-with-leading-bool discriminant is **not** covered by the shipped `present_if`/`when`, which handles struct-field presence only.

**Also recorded: a real defect found by hardening, not by theory.** v340's single-shot `block_place` was transiently ignored ~50% of the time. The wire shape was verified correct against minecraft-data, so the fault was *choreography*, not encoding — fixed by retry-until-confirmed against a server `testforblock` readback. v735 shared the same latent flake and was hardened prophylactically. A 50%-flaky gate is worse than a failing one: it gets re-run until green and then believed.

**And an honest ceiling worth keeping:** v47's place interaction **cannot** be gated — mc189 is survival with no RCON or console, so the player has nothing to place. Break-only is the maximum, documented in-crate rather than silently absent. Giving that container an RCON channel would unlock it.

**12.7 Live server** — vanilla 26.2 running in Docker, ready as an integration-test target.

---

---

The entries below (**12.19** onward) are the operational record that used to live in
[`CLAUDE.md`](./CLAUDE.md). They were moved here verbatim when that file was trimmed from 990 lines to
roughly 300, because a rule nobody finishes reading is not a control. **`CLAUDE.md` keeps the
imperative; this log keeps the measurement that paid for it.** Nothing was deleted -- each rule still
states itself there, and the incident, sha and count are here.


**12.19 A timing taken while other agents build is not a measurement.**

- **A *timing* taken while other agents build is not a measurement either, and it will be attributed
  to the wrong cause.** Measured: an agent recorded 2.66 ticks/s in debug versus 19.29 in release and
  wrote the build profile into both its test and its doc as the explanation. Re-run on an idle
  machine, the **same unoptimised build** hit 19.29/s. The real variable was concurrent machine load;
  the profile explained nothing. It caught this itself and corrected both in `3380fb0`.

  This is worse than a noisy number, because the *story* survives the correction: "debug is 7×
  slower" is plausible, memorable, and gets quoted downstream. Note
  `docs/plans/worldgen-parity.md`'s risk 3 rests on debug-profile figures for exactly this reason —
  treat any debug-vs-release attribution recorded during a multi-agent session as unproven until
  re-measured quiet.

  What survives contention: a **ratio** between two arms measured in the same run. That agent's
  tick-loop gate asserted a ratio and was never affected. Prefer one; when you need an absolute,
  re-measure on an idle machine and say which it was.

  **But a ratio is only protected if both arms see the same load, and two *sequential* timings do
  not.** Corrected within the hour of writing the paragraph above, which overstated it.
  `sim::tests::extract_particles_does_not_hold_the_world_guard_across_the_per_particle_work` takes
  `small_ns` then `large_ns` and asserts `large_ns < small_ns * HOLD_SCALING_LIMIT` — a ratio, and
  it still failed on committed `main` under four concurrent agents, because a load spike between the
  two calls inflates one arm and not the other. Run alone it passes.

  So the real rule is the *two observations at two different moments* hazard again, one scope
  smaller: it applies **inside a single test**, not just between a test run and a diff. A ratio
  survives contention only when the arms are measured **concurrently**, or when the quantity is a
  count rather than a duration. Counts are immune; sequential durations are not, however you divide
  them. Before reporting a timing-shaped test as a regression, re-run it **alone** — and if you are
  the one writing it, prefer a counter (that gate's companion asserts particle *volume* at both
  ends, which is the part that never flaked).


**12.20 The shared checkout: `git checkout -- <path>` is the same command, narrowed.**

**`git checkout -- <path>` is the same command as `git checkout .`, narrowed — and it is banned
too.** An agent read the ban above as covering only the `.` form and ran
`git checkout -- docs/README.md` to discard a regeneration it did not want to commit. That path
happened to be a generated file, so nothing was lost, but the operation discards *whatever* is in
the working tree for the named path — including another agent's uncommitted edit, with no diff and
no reflog to recover from. There is no safe pathspec for it in a shared checkout. If you have a
working-tree change you do not want in your commit, **just do not name that path** — the pathspec
commit form ignores everything you do not list, which is the whole reason it is the standard here.


**12.21 `docs/README.md` drift is red-`main`-shaped, and reverting it makes it worse.**

- **`docs/README.md` drift is red-`main`-shaped, and reverting it makes it worse.** `cargo test -p
  xtask` fails when the committed index does not match the generator, and the usual cause is a
  *different* agent changing a doc's H1 or `## What it is` summary and not regenerating. That is what
  happened above: the summary change was already **committed**, so reverting the regeneration left
  `main` red and the next agent inherited it. If you find that test red and the drift is not yours,
  **regenerate and commit `docs/README.md` alone** (`cargo xtask docs-index`, or
  `LODESTONE_REGEN=1 cargo test -p xtask docs_index_matches_committed`) — committing a one-file
  regeneration under your own message is correct and expected, not a foreign-line violation. Check
  `git status` first: if the drift is *uncommitted*, its author is mid-flight and will regenerate
  themselves; only a committed drift is yours to fix.


**12.22 Rewriting a shared file wholesale is a fourth way to clobber.**

- **Never rewrite a shared file wholesale — edit the lines you mean.** This is a *fourth* way to
  clobber, and no git command is involved, so none of the rules above catch it: writing a full new
  copy of a file silently discards every concurrent edit in it, and the loser finds out only when
  their own change stops existing. An agent overwrote `sim.rs` this way and destroyed three edits
  another agent had already made there; that agent recovered by re-routing its work through
  `resources.rs` and `app.rs`, but nothing warned either of them. `sim.rs`, `app.rs`, `gpu.rs` and
  `docs/README.md` are the usual victims because everyone needs a line in them. Prefer a targeted
  edit over a rewrite, and **re-read a shared file immediately before writing to it** — not at the
  start of your task, which may be an hour of other agents' commits ago.


**12.23 `cargo fmt` in a shared checkout, and why the cleanup is the damage.**

- **Never run `cargo fmt` (or `rustfmt`) in this checkout.** It rewrites files you do not own, and
  the damage is not the reformatting — it is that your diff becomes inseparable from everyone
  else's, so the *cleanup* is what destroys work. An agent ran `cargo fmt` on `sim.rs`, then tried
  to strip the reformatting by reversing hunks against `HEAD`; the reversal deleted another agent's
  concurrent `particle_atlas`/`particle_sheet_atlas` additions, because new content added since
  `HEAD` is indistinguishable from "collateral formatting" when you diff against `HEAD`. It was
  caught only by a build error naming a method that had stopped existing, and re-applying the patch
  forward recovered it. Format the lines you wrote, by hand.


**12.24 Staging hunks rather than files.**

- **When a shared file already holds someone else's work, stage your hunks, not the file.**
  `git add -p`, or `git diff -- <file> | …` filtered and applied with `git apply --cached`, then
  read `git diff --cached` to confirm the commit contains no foreign lines. This is the working
  practice that let one agent commit into `gpu.rs`, `gpu/stats.rs`, `resources.rs` and
  `docs/README.md` while three other agents held in-flight edits in all four.


**12.25 A red test may be someone else's deliberate neuter.**

- **A red test in this checkout may be someone else's *deliberate* neuter, and no diff can tell you.**
  Every control in this file works by breaking something on purpose and watching a test fail — so at
  any moment another agent's two-minute neuter window looks exactly like a real regression. It
  happened: one agent reported "two `entity::tests::*projectile*` lib tests are red on committed
  `main`", and they were the exact pair another agent's `arrow_NEUTERED` experiment produced. `main`
  was green throughout.
  **The `git diff HEAD` substitute does not save you here**, which is the part worth internalising,
  because that substitute is otherwise excellent (see the entry below). The neuter lived in
  `lodestone-assets` while the failures surfaced in `lodestone-render`, and — more fundamentally —
  a clean diff and a test run are **two observations at two different moments**. Emptiness at 19:31
  says nothing about the tree at 19:33.
  So: before reporting a red `main`, re-run at the **committed sha in an isolated worktree**, which is
  the only observation that excludes concurrent edits by construction. And when *you* neuter
  something, keep the window as short as possible and restore by `cp` from a scratchpad backup with an
  md5 check — never `git checkout`.


**12.26 The scratchpad directory is shared between agents too.**

- **The scratchpad directory is shared between agents too, so the md5 check above is load-bearing.**
  The path is per-*session*, and every agent in a session gets the same one — so a
  `scratch/probe.rs` or `msg.txt` is exactly as contended as a file in the checkout, with none of the
  git-level protections and no diff to show you what happened. Observed: an agent wrote two scripts
  by heredoc and **read back different content than it wrote**, and found a `msg.txt` it had never
  created already sitting there. That nearly had it classify its hunks against the shared *index*
  instead of `HEAD`, which is the one mistake that ships another agent's lines.
  **Use uniquely-named files** (include the issue number or a nonce), write them with the file tools
  rather than shell heredocs, and re-read anything you are about to reason from. A `#[path]` harness
  is the common case here: it compiles whatever is on disk at that instant, so a clean run proves
  nothing about the file you thought you wrote. This is the same "two observations at two different
  moments" failure as the entry above, one directory over.


**12.27 The stale index blob: the most frequently observed hazard, and its cause.**

- **Never leave a stale blob in the shared index.** A `docs/README.md` blob sat staged at `7b506a8`
  while `HEAD` had `3432cb3`; committing the index would have **deleted** a newer agent's index
  bullet. Refreshing one path with `git reset -- <path>` sets that index entry back to `HEAD` and
  leaves the working tree untouched, which is the safe cleanup — but the real fix is never staging in
  the first place (see the pathspec-commit entry).
  **This is the most frequently observed hazard in the file: five instances in one session**, every one a
  *reversal of a commit that had just landed*, armed for the next agent's `git commit` to ship under
  their message. Twice on `container.rs` (632 lines, then 268), three times on `gpu.rs` (115, 59 and 290
  deletions). Every affected agent had used `GIT_INDEX_FILE` correctly and none had run a bare
  `git add` — truthfully.

  **The cause is the cleanup step itself, in the wrong order.** `git reset -- <paths>` sets the *shared*
  index entry to whatever `HEAD` is **at that instant**, creating an entry where there was none. Run it
  *before* `git update-ref`, and it pins the pre-commit blob; `update-ref` then moves `HEAD` forward and
  that entry becomes a staged reversal of the commit you just made. A deletion-only staged diff
  (`0` insertions, N deletions) is the signature.

  So the order is not stylistic:

  ```
  TREE=$(GIT_INDEX_FILE=$priv git write-tree)
  NEW=$(git commit-tree "$TREE" -p "$OLD" -F msg)
  git update-ref refs/heads/main "$NEW" "$OLD"   # HEAD moves FIRST
  git reset -- <paths>                           # then refresh, against the NEW HEAD
  ```


**12.28 `git write-tree` against a missing index writes the EMPTY tree.**

- **`git write-tree` against a missing index writes the EMPTY tree, silently — and that commit
  deletes the entire repository.** This is the worst outcome available from the escape hatch and it
  has already reached `refs/heads/main` once, for a few seconds, before its author caught it in
  `git show --stat` and reverted with a compare-and-swap.

  The trigger is mundane: **shell state does not persist between tool calls.** A private-index path
  built with a `$$` nonce in one invocation is an *empty string* in the next, so `GIT_INDEX_FILE=""`
  and `write-tree` has nothing to write. No error, no warning — a valid commit object whose tree
  contains nothing.

  Three defences, and use all three because each catches a different slip:
  1. **One invocation** for `read-tree` → `add` → `write-tree` → `commit-tree` → `update-ref`. Not
     "one per step, carefully ordered" — the variables do not survive.
  2. **A literal nonce**, not `$$` or `$RANDOM`: `idx-fog-7f3a`, chosen by you and typed out.
  3. **Sanity-check the tree before moving the ref.** `git ls-tree -r "$TREE" --name-only | grep -c ""`
     against a plausible floor is one line and it makes this class impossible:
     ```
     n=$(git ls-tree -r "$TREE" --name-only | grep -c "")
     [ "$n" -gt 1000 ] || { echo "ABORT: tree has only $n files"; exit 1; }
     ```
  And always `git show --stat` your own commit afterwards. That is what caught it.

  And still check `git diff --cached` is empty immediately *before* every commit, because another agent
  may have left one: a count, not an eyeball, and a verdict that depends on the count — an unconditional
  `echo "(clean)"` after the check is its own vacuous control, and that mistake was also made here.


**12.29 The shared index, and why the pathspec commit form is the standard.**

- **The index is shared too: never leave work staged.** Hunk-staging (above) stops *you* shipping
  someone else's lines; it does nothing to stop *them* shipping yours. `git add` writes to the one
  index every agent shares, so any other agent's `git commit` in the gap — however narrow — harvests
  whatever you have staged into **their** commit, under their message. This happened to a whole
  26-file change: the `registry_data` ingest for #288 was staged, verified, and then committed by
  another agent as `a19e5e4 feat(shell): chests reach pixels`. Nothing was lost and nothing foreign
  was shipped, but the change set has no commit that describes it, and a reviewer reading `a19e5e4`
  is misled about what it contains. The same gap cost that work three re-stagings, because a
  concurrent broad `git add` also reset the index for `docs/` twice mid-flight, and a
  `git diff --cached` read one command later was already describing a different index.
  **Use the pathspec form: `git commit -m "…" -- <your paths>`. This is the standard here, not a
  fallback.** It commits exactly those paths and **ignores the index entirely**, which is the only
  property that makes it safe.

  Measured in a throwaway worktree, because the whole point is that it needs no cleanup step:

  | | result |
  |---|---|
  | commit created | yes, `HEAD` moved |
  | contents | only the named path |
  | **index afterwards** | **clean — no `git reset` needed** |
  | working tree | untouched |
  | another file's edits | survived on disk, excluded from the commit |

  That third row is the important one. **`git reset -- <paths>` is the source of every stale-index
  incident in this file** — nine in one session — and it only exists to clean up after the private-index
  route. The pathspec form leaves nothing to clean up, so **do not run `git reset` after it.** Adding
  that step back is how the hazard returns.

  Argument order matters: `git commit -m "msg" -- <paths>`. Put `-m` *before* the `--` or git parses
  the message as a pathspec and silently commits nothing — a probe written the wrong way round here
  reported "index clean" from a commit that never happened, which is a vacuous control on top of a
  no-op.

  **And the pathspec form cannot introduce an untracked file. It fails by committing nothing.**
  `git commit -m "…" -- <paths>` dies with `error: pathspec … did not match any file(s) known to git`
  when any named path is new, so **anything that creates a file needs an explicit `git add <files>`
  first**. Hit independently by **two agents in one session**, both mid-refactor where creating files
  was the whole point.

  The reason it is dangerous rather than merely annoying: with output redirected, the *only* signal was
  that `git rev-parse HEAD` printed **another agent's** sha. A no-op commit in a busy shared checkout
  does not look like a failure — it looks like a successful commit belonging to someone else, and an
  agent that reports that sha publishes a wrong provenance for work that is still uncommitted on disk.

  **What catches it: read your own sha in the same shell invocation as the commit, and `git show --stat`
  it.** Both agents caught it that way and neither would have otherwise. This is the same
  *two-observations-at-two-different-moments* hazard as everywhere else in this file — a `rev-parse` one
  tool call later is a different moment, and in this repo another commit lands in that gap routinely.
  "Stage, verify and commit in one shell invocation" was tried and is **not sufficient** — a single
  invocation is not an atomic transaction. An agent staged six files, asserted
  `git diff --cached --name-only` matched exactly, and then its plain `git commit` swept in **14
  files** belonging to another agent who had run `git add` in the window between the assert and the
  commit. One of those files was captured **mid-keystroke**, so `main` was briefly red from a commit
  whose author never touched the broken file. Review-then-commit cannot be made race-free while the
  index is shared; the fix is not to look harder but to stop consulting the index at all.
  `git add` "to see the diff" is the most expensive way to look — `git diff -- <paths>` shows the
  same thing and touches nothing.

  **The pathspec form commits *working-tree* content, so a path you name carries whatever is in it.**
  It defeats the index race, not the shared checkout. **That is an accepted cost, not a blocker** —
  the repo owner's call: shipping a few of another agent's lines under your message is far cheaper than
  agents stalling on each other, and it is recoverable by reading the diff. So:

  - **Name only paths in your own assigned cluster.** That is what actually prevents this, and it is
    why ownership is assigned per agent up front.
  - `git diff -- <path>` before naming it, so you *know* what is going in. If a foreign edit is there,
    say so in the commit message rather than abandoning the commit.
  - **Do not block on it.** Waiting for another agent to finish is usually the wrong trade, and
    splitting your change to avoid a shared file is worse — it produces two half-commits neither of
    which reaches pixels.
  - The one case still worth avoiding: a file that is **mid-keystroke** rather than merely modified. If
    it does not compile and you did not break it, wait a beat, do not commit it.

  Only reach for the temp-index route below when you need **partial-file** granularity — committing two
  hunks out of a file whose remaining hunks belong to someone else. That is a real need (it happened
  once here) and it is the only thing the pathspec form cannot express.


**12.30 `git pull --rebase --autostash` left a staged deletion of another agent's file.**

- **Never `git pull --rebase`, and never `--autostash`.** The `git stash` ban above is easy to keep
  when you type it; `--autostash` runs one *for* you, on the whole shared tree, silently. An agent ran
  `git pull --rebase --autostash`, the rebase aborted, and it was left with a spurious **staged
  deletion of another agent's brand-new test file** — content intact but the index claiming a removal,
  which the next commit would have shipped. It repaired the index entry by hand. There is also a live
  `stash@{0}: autostash` entry holding a full-tree snapshot, left in place deliberately as someone
  else's safety net: **do not `stash drop` or `stash pop` it.** If you need to move to a newer commit,
  do it in a throwaway `git worktree add --detach`, which touches nothing here.


**12.31 `git commit --amend` orphaned another agent's commit.**

- **Never `git commit --amend`.** It rewrites a commit that other agents have already built on, and in
  a shared checkout the thing it sweeps up is not yours. Measured: an agent amended its own `#299`
  commit and thereby absorbed **another agent's staged-but-uncommitted `feat(chat)` work** into it.
  The content survived — verified byte-identical in `HEAD` afterwards — but the other agent's commit
  is now **orphaned**, so the change set has no commit describing it and a reviewer reading the
  history is misled about what `#299` contains. The same agent then ran a bare `git reset`, which
  unstaged several other agents' in-progress work.

  Both are the shared-index hazards already in this file, reached by a route the file did not name.
  **The fix is the same as everywhere else: pathspec-form commits from the start**
  (`git commit -m "…" -- <paths>`), which ignore the index entirely, so there is never a reason to
  amend. If your last commit was wrong, **land a follow-up commit** — a second commit is cheap and
  honest; rewriting shared history is neither, and `git push --force` to fix the amend would be worse
  still.


**12.32 The private-index escape hatch and its stale tree.**

- **`GIT_INDEX_FILE` + `commit-tree` is the escape hatch, and it has its own trap: a stale tree.**
  When you need partial-file granularity that a pathspec commit cannot express, build the commit in a
  **private** index so the shared one is never touched. But the ref compare-and-swap in
  `git update-ref <new> <old>` protects the *parent*, **not the tree you built**. An agent read a tree,
  two commits landed while it worked, and committing that stale tree onto the fresh parent **reverted
  2,173 lines** of another agent's chest and metadata fixes. It was caught immediately in
  `git show --stat` and repaired, but the lesson is: **read the tree and commit it in one step**, and
  always `git show --stat` your own commit afterwards to confirm it contains only additions you
  intended and no deletions you did not.


**12.33 `git clean` destroyed a plugin crate and a doc that were in no commit.**

- **`git clean` is the worst of the git-level mistakes, because it destroys what nothing can
  recover.** The others discard *modifications* to tracked files, which at least existed in a commit
  once.
  `git clean` deletes **untracked** files — which in this repo means whole new crates, new
  `docs/*.md`, new oracle dumps and new test files, none of which are in any commit or reflog.
  It has already cost real work: an agent ran it while others were mid-flight and destroyed
  `docs/autonomous-navigation.md` outright, plus `crates/plugins/lodestone-autopilot`'s manifest
  and source, leaving only the `LICENSE` behind and the workspace unloadable. The author had to
  rewrite it from nothing. There is **no legitimate use** for it here: build output is already
  gitignored, and "tidying up" a shared checkout is not a thing any single agent has the standing
  to do.
- **Stage explicit *file* paths, never a directory.** `git add docs/` is the same mistake as
  `git add -A`, just narrower — it sweeps up whatever else happens to be in there. This bit me
  personally: `53850ce` swept another agent's then-unfinished `docs/block-break-timing.md` into a
  render commit. Nothing was lost, but the commit contains 169 lines its author never wrote, and a
  reviewer reading that diff would be misled about what the change was. `git add <file>` or
  `git add -p`, always.
- **Read `git diff --cached` before every commit.** Explicit file paths are necessary but not
  sufficient: a *shared* file can already contain someone else's in-flight edit. `0b95b4e` staged
  `docs/README.md` by exact path and still captured another agent's index line pointing at a doc
  that commit did not include — shipping a broken link. Review the staged diff, not just the file
  list.


**12.34 `rtk` is not a transparent proxy.**

- **`rtk` is not a transparent proxy. Do not trust it for evidence — use `/usr/bin/grep` and the
  real `cargo`/`git`.** It is a token-saving filter, and its filtering silently destroys exactly the
  output a search exists to produce. Verified here directly, on one file, one pattern:

  | | output for `ambient_occlusion_at` in `mesher.rs` |
  |---|---|
  | `rtk grep -n` | `usize, y: usize, z: usize) -> bool {` |
  | `/usr/bin/grep -n` | `fn ambient_occlusion_at(&self, x: usize, y: usize, z: usize) -> bool {` |

  **It strips the matched pattern and everything before it on the line** — it deletes the one thing
  you searched for, so you cannot tell a real match from a near-miss, and a symbol looks absent when
  it is present. This is the `| head` trap with no visible pipe: rule 2's whole class of "X doesn't
  exist yet" mistakes can now be manufactured by the search tool itself.

  Also observed by agents, each nearly producing a wrong conclusion: `rtk proxy cargo test` reporting
  **exit 0 while its own output said 7 failed**, and rewriting `-p lodestone-render` into a run that
  executed `lodestone-physics`' tests; and `rtk proxy git diff HEAD -- $LONG_VAR` returning **zero
  hunks while the content plainly differed**, which nearly had an agent conclude its work was already
  committed (single literal paths worked). Exit-code preservation *is* fine for `cargo check`
  failures — measured 101 both through `rtk proxy` and through `~/.cargo/bin/cargo` — so the failure
  is not uniform, which is worse than if it were: it is unpredictable per subcommand.

  Practical rule: `rtk` for reading something you already believe, the real binary for anything a
  conclusion rests on. **Re-read every exit code from a captured file with a program, not from a
  pipeline.**


**12.35 Docker's memory cost is the machine's real ceiling.**

- **Docker's memory cost is the machine's real ceiling, and this entry used to say the opposite of
  what to do.** It previously warned that the machine was shared with an unrelated project whose
  `mht-*`, postgres, valkey and seaweedfs containers must never be pruned. **Checked 2026-08-04:
  none of those containers or images exist**, and the owner confirmed nothing else in Docker matters
  to them. That stale warning had every agent — and me — avoiding Docker maintenance all session
  while the box suffocated.

  What was actually there, measured: the three JVM oracle containers holding **2.75 GB**, the Docker
  VM reserving **7.26 GB**, **22 GB** of build cache, **19 GB** of dangling images, and two dead
  `temurin:8-jdk` containers. Free memory was down to about **87 MB** with load average **91**, and a
  single-crate `cargo check` took **10m44s**. Stopping the oracles, pruning, and quitting Docker
  Desktop recovered roughly **5.5 GB** of RAM — free pages went 5,542 → 360,121 and the compressor
  dropped from 259k to 67k pages.

  So: **Docker is fair game to stop and prune when no live gate needs it.** Prefer stopping the
  oracles over leaving them idle — they are explicitly not repo state and
  `scripts/live-oracles/{creative,survival,terrain}.sh` recreates them. Quitting Docker Desktop
  entirely (`osascript -e 'quit app "Docker"'`) reclaims the VM reservation, which is the single
  largest win available; restart it before any `#[ignore]`d live-oracle gate. Still name targets
  explicitly rather than trusting a filter — Docker's `name=` filter is a *substring* match — and
  still prefer `--rm`. Lodestone containers are `lodestone-*`.

  **Volumes are the one thing to keep hesitating over.** 81 local volumes / 2.18 GB survive here
  untouched; they are cheap, and a volume is the only Docker object that can hold data nothing
  recreates. Prune images and build cache freely; think before pruning volumes.


**12.36 Test-binary memory froze the machine, and `Pages free` is not headroom.**

- **`-j` bounds rustc, not test binaries — and unbounded *test* memory froze the machine.** On
  2026-08-04 the box ran out of memory and had to be force-rebooted while roughly a dozen agents were
  live. It was not their idle footprint: single test binaries in this workspace have been measured at
  **4.8 GB and 5.2 GB RSS**, and with 16 GB total, two or three concurrent `cargo test` runs is the
  whole budget. `-j 4` caps *compile* parallelism and does nothing about a linked test binary's
  runtime footprint, which is why the existing per-agent `-j` guidance did not prevent this.

  So, when many agents are live: **pass `-- --test-threads=2` to `cargo test`** (test threads each
  hold their own fixtures — that is the knob that actually caps peak RSS), **prefer `cargo check`
  when you only need to know it compiles**, and **never run two cargo commands concurrently or
  background one and start another**.

  **`Pages free` is NOT headroom, and a threshold on it is actively harmful.** This entry first said
  "free pages under ~50,000 → wait", and that gate **stalled an agent** which correctly obeyed it:
  macOS deliberately keeps `free` low and reclaims from `inactive`, so a reading of 33,550 free pages
  (~137 MB) sat alongside ~1.1 GB of reclaimable `inactive` and **zero swapouts**. There was no
  pressure at all. Measured minutes later: free 343 MB, inactive 1,146 MB, `vm.swapusage` total
  **0.00M** — the machine had not swapped once since boot.

  The signals that actually mean pressure, in order: **`sysctl -n vm.swapusage` showing non-zero
  `used`**, **`Swapouts` in `vm_stat` climbing**, and **`memory_pressure`'s own "System-wide memory
  free percentage"** (it was 85% during the supposed danger). Compressor *growth* over successive
  readings matters; a single absolute value does not. **Load average is the worst proxy of all** —
  right after a reboot it sat at **31** purely from Spotlight/ML reindexing (`mds_stores`,
  `mediaanalysisd`, `ANECompilerService`) while memory was 85% free.

  And **"wait" must never mean arming a background monitor.** An agent that stops to wait for a
  monitor is marked complete by the harness and its notification is discarded — that is the most
  repeated operational failure in this repo (**nine instances across seven agents in one session**),
  and a memory gate that tells agents to wait without saying *how* manufactures it. If you must
  wait, re-read `vm_stat` a bounded number of times **inside one shell invocation**, or just run the
  cheaper command (`check` instead of `test`, one crate instead of the workspace) and move on.


**12.37 Phantom `cargo check` errors naming another agent's worktree.**

- **A `cargo check` in this checkout can report hundreds of phantom errors naming another agent's
  worktree.** Measured: `cargo check -p lodestone-shell --no-default-features` produced **435 error
  lines**, mostly `couldn't read …/scratchpad/wt-route-9a4c/crates/lodestone-server/assets/worldgen/
  biome/*.json: No such file or directory` — a path inside a *throwaway worktree that was being
  removed while cargo read from it*. The named files exist perfectly well in this checkout, and all
  430 of them are tracked. Re-running minutes later gave **3** errors, all real and all one agent's
  in-flight edit.
  The trap is that this looks exactly like a catastrophic breakage — hundreds of missing data files
  reads as "someone deleted the assets" or "the embed path is wrong", and it names real filenames.

  **The cause, and the rule that prevents it: never point `CARGO_TARGET_DIR` at the shared `target/`
  from a throwaway worktree.** Doing so bakes the worktree's absolute path into a build script's
  output, and when the worktree is removed the shared cache keeps serving those dead paths to
  *everyone else's* build. The agent who did it found and fixed it with
  `cargo clean -p lodestone-server` — **34,735 files, 3.7 GiB**. A throwaway worktree must use its own
  target dir; the cost is one extra build, and the alternative is poisoning every other agent's
  output for as long as it takes someone to notice.

  **A correction worth keeping, because it is a live example of §2.** This entry first recorded that
  `cargo clean -p <crate>` reported `Removed 0 files`, and concluded "the artifact cache was never the
  cause." That was exactly backwards. The clean printed zero because the agent responsible had already
  run it minutes earlier — the cache was the whole cause. Two observations at two different moments,
  and the second one read as evidence about the first. **A no-op result from a repair step is not
  evidence the thing you repaired was healthy** — check whether someone else already fixed it before
  concluding it never needed fixing.

  So: **an error whose path contains `/scratchpad/` or a `wt-` prefix is not about your code.** Ignore
  it, re-run, and remember the general rule this is one more instance of — a check run in a shared
  checkout while a dozen agents edit is a **sample, not a measurement**. Before believing any verdict
  about `main`, re-run at the committed sha in a fresh isolated worktree, and prefer
  `git worktree remove` over leaving worktrees around.


**12.38 Island factories: the event routers and their terminal `_ =>` arms.**

**One specific island factory: `ingest::handles_event`'s routing switch.** A system can be correct,
registered in the right set, in the right order, and unit-tested green — and still never run in
production, because `SharedState::apply` only forwards events the switch lists. A hermetic test that
calls the system directly passes either way, so nothing catches it. This has now hidden working code
**twice in one session** (`EntityDamaged`/`EntityHurtAnimation`, then air supply). When adding an
ingest system, the switch is the first thing to check, not the last.

**Generalise it: every terminal `_ =>` arm in an event router is an island factory, and there are
three.** A `_ => {}` that silently discards is indistinguishable, at the call site, from one that has
nothing left to handle.

| router | carries | missed instance |
|---|---|---|
| `ingest::handles_event` | per-entity ECS state | `EntityDamaged`/`EntityHurtAnimation`, air supply |
| `session::handles_event` | local-player session scalars | — (but see below) |
| `net.rs`'s `forward` | the shell's own `ClientEvent` stream | `BLOCK_EVENT`, so chest lids could never animate |

**`ingest` vs `session` is a real fork and guessing it wrong has cost work twice.** `SharedState::apply`
consults *both*, so an arm added to the wrong one compiles, tests green as a unit, and never runs.
`DimensionTypeChanged` is claimed by `session`, and so is `AbilitiesChanged` — for which both the issue
and the dispatch briefing said `ingest`, where an arm would have produced a fold that never fires.
The rule of thumb that has held: **per-entity state is `ingest`, local-player scalars are `session`**,
and block/world events are neither, travelling the shell stream instead — the chest work needed no
`handles_event` arm at all.

So when a decoded packet reaches no pixels, grep its variant in *every* router before concluding the
decode is wrong, and check the sibling router before adding an arm to the one you thought of first.

**Islands come in both directions.** All of the above are *inbound*. `ClientAction::SetFlying` was the
mirror image: encoded by four protocol adapters with **zero producers** anywhere outside
`crates/protocol/`, so flight was applied locally and the server kicked us with
`multiplayer.disconnect.flying`. Ask what *sends* a serverbound action, not only what consumes a
clientbound one.


**12.39 The two staleness traps that cost real work.**

- **Zero hits in the file a stale note names is not evidence a feature is unwired.** A note said the
  shell didn't consume the chat resolver, citing `chat.rs:88`. Grepping `chat.rs` returned nothing —
  correctly, because the consumer is one layer up in `sim.rs`, at ingest. **Grep for the producer
  across the whole tree, not for the consumer in one named file.**
- **Read the record definition, not a summary of the call site.** `HANDOFF.md` transcribed vanilla's
  `DepthStencilState(…, 1.0F, 10.0F)` as "constant 1.0, slope 10.0". The record is
  `(depthTest, writeDepth, depthBiasScaleFactor, depthBiasConstant)` — i.e. slope 1.0, constant
  10.0. Backwards.


**12.40 What `cargo xtask connectedness` does and does not measure.**

**But know what it measures, because it is silent rather than wrong outside that scope — and it is
narrower than its name suggests.** Measured twice today, each time by an agent I had pointed at it
wrongly:

- **Both defects below are fixed as of `e164d06` (issue #412) — kept because the numbers matter and the
  failure modes recur.** It used to measure `v770` **only**, via a hard
  `if family != "v770" { continue; }`, while its own header claimed "denominators from each family". So
  for months a green number said nothing whatever about three of the four families. With the filter gone,
  the first per-family reading was:

  | family | clientbound decoded | serverbound encoded |
  |---|---|---|
  | family | first reading (kept for the ratio) | measured 2026-08-04 |
  |---|---|---|
  | v47 | 17/74 | **21**/74 |
  | v340 | 16/80 | **22**/80 |
  | v735 | 17/92 | 17/92 |
  | v770 clientbound | 111/141 | **114**/141 |
  | v770 serverbound encoded | 53/69 | **54**/69 |

  **The legacy families decode under a quarter of their clientbound packets**, which nobody knew, because
  the instrument that would have said so was skipping them. Do not assume a legacy family is well covered.
  It now also measures **serverbound decode**, and reports *"not applicable"* rather than a false `0/69`
  for the three families with no `server_protocol.rs` — only `v770` implements `ServerProtocol`. Note
  `serverbound encoded` remains a *client*-side figure: bare token presence in the client adapter, no arm
  and no direction check.

  **Four of the six figures in this table were stale within days, in the table that exists to stop staleness.**
  Re-measured 2026-08-04 during a tracker sweep; the second column above is that reading. The serverbound
  decode figure was the worst: this file said **13/69**, when the truth was **60/69 decoded and 17/69
  connected** — the wrong *axis*, not merely a stale count, and five issue bodies had inherited the same
  error. **Do not quote a number from this table. Run `cargo xtask connectedness` and quote that.** The
  table is here for the *shape* of the finding — legacy families are thin, decode and connectedness are
  different axes — not for its digits.

  **And the instrument has a blind spot it cannot report: a fully-connected wire carrying the wrong value.**
  Issue #323 is the worked example. The server broadcasts `SET_TIME`, the client decodes it and really does
  darken the sky — every link measured, nothing stranded, `connectedness` perfectly green — and the value on
  the wire is **wall-clock elapsed-since-join**, while `tick.rs`'s real tick counter never reaches the
  encoder. So `connectedness` answers "is this packet reaching something", and **cannot** answer "is it
  carrying the right number". That is a distinct failure from the *island* (built, reaches nothing) and from
  the *magnitude* species (right direction, wrong amount): here the plumbing is complete and the source is
  wrong. Only a gate whose expected value originates **outside** our own producer can see it.
- **Our source scanners were silently broken by Rust lifetimes, and it took a UTF-8 panic to notice.**
  `matching_brace` was described as comment-, string- and char-literal-aware. Its "in a char literal" flag
  **never closes on a lifetime** — `&'static str` opens it and nothing shuts it — so from the first
  lifetime in a file, comment detection was disabled for the rest of it. Fixed in all three scanners with
  a lookahead-based `char_literal_span`. Two lessons worth keeping: any coverage number produced before
  that fix, for a file containing a lifetime before a comment, was **unreliable in an unknown direction**;
  and the bug surfaced only as an unrelated-looking crash in new code, never as a wrong answer, which is
  how a scanner bug normally behaves. **A hand-rolled Rust lexer will be wrong about lifetimes** — test
  one against a file with `&'a` before a `//`.
- **Serverbound decode does not live in `lodestone-server` at all** —
  `/usr/bin/grep -rn "serverbound::" crates/lodestone-server/src/` returns **zero hits**. It is in
  `crates/protocol/v770/src/server_protocol.rs:880`, as `State::Play if packet_id ==
  play::serverbound::NAME =>` arms. **This entry previously said `lodestone-server` and quoted
  "5/69 → 8/69"; both were wrong** — there are **10 Play arms**, and `docs/roadmap/protocol.md`'s
  "completely zero" is stale too. Two hand-counted figures in two documents, both stale within a day,
  which is the argument for automating the axis rather than re-counting it.
  Note the count alone is not connectedness: a variant that decodes and lands only in `server.rs`'s
  `ServerBound::Ignored => {}` group is stranded exactly as a clientbound packet would be, so the
  serverbound axis is a **two-file join** across crates, not a one-file scan.
- It does not measure **Rust call graphs** either. Pointed at a *crate-internal* island — an
  implemented type nothing in the workspace constructs — it returns **byte-identical output before and
  after the fix**, which reads as "no change" rather than "not applicable". The agent closing
  `projectile.rs`/`item_entity.rs`'s missing tick drivers hit this and correctly reported the identical
  output as meaningless rather than quoting it.

So: right instrument for "is this clientbound packet reaching anything", wrong one for everything else.
For a crate-internal island, grep for constructors tree-wide plus a test that drives the *registry*
rather than the type. For server decode, grep the packet ids.


**12.41 A control's premise can be false before the feature under test existed.**

**A control's premise can be false before the feature under test ever existed.** This is subtler
than a wrong assertion and it fails in the *safe*-looking direction: the control fires, so the gate
looks rigorous, and what it actually measures is unrelated. Two instances while wiring the sky:

- A control asserted that a sky-less frame "clears uniformly to `SKY_COLOR`". It failed at 3.5%. The
  offenders were at `x221..255 y180..255` in dark browns — the **first-person bare arm**, which the
  hand pass draws whenever `third_person_body_drawn` is false, i.e. always, in first person, with
  nothing installed. The premise had been false since long before the sky existed.
- A HUD gate's rect hardcoded the *with-hotbar* `cluster_top`. `sprite_vitals` stacks upward from a
  **moving** anchor (pulled up only `if frame.hotbar`, again only `if frame.xp`), so the gate
  measured ~20 logical pixels above a row that was drawing perfectly and reported 0 px — a dead
  wiring chain that was not dead.

So: before believing a control, ask **what else already paints here**, and derive layout from the
same expression the draw uses rather than restating a constant. And per *measure by location, never
by frame average* below — both were diagnosed in one step by printing a **bounding box** instead of
a percentage. A gate that reports only a fraction cannot tell a uniform-but-wrong frame from a
localised blob; make failure output say *where*.


**12.42 Shell pipelines that invented a green.**

**A shell pipeline will destroy the evidence you are about to reason from.** Two instances in one
session, both of which produced a confident wrong conclusion:

- **`| head` read as absence.** `grep -rn -A4 0.085 …/world/entity/ | head -24` was flooded by
  `DropChances.java` and showed no hit in `Player.java`, so the swim-descent constants were declared
  unverifiable and an agent was told to distrust them. They are real, at `Player.java:1408`. A
  truncated search is not a negative result — `grep -c`, or narrow the path, before concluding a
  thing does not exist.
- **`| grep | tail` swallowed a non-zero exit.** `cargo test --workspace | grep … | tail -30`
  reported "exit code 0" because that is `tail`'s status, while cargo's own last line was
  `error: 1 target failed:` — and the grep pattern then cut the target name off. This came within
  one command of a commit on a red tree. **Let cargo write its own output to a file and check its
  real exit status**; filter the file afterwards.

- **`| tail` with no `-f` buffers until EOF, so a backgrounded build looks hung.** An agent backgrounded
  `cargo test … | tail -80`, watched the output file stay **empty** for the whole compile, and concluded
  the run had hung — it was compiling normally, and `tail` was simply holding everything until the pipe
  closed. It killed a healthy run. Redirect straight to a file (`> log 2>&1`) and read the file; never put
  a buffering filter between a long build and your only view of it. Same family as the entries above: the
  transform that makes output readable is the transform that lies about it.
- **zsh does not word-split an unquoted `$var`, so a path list in a variable is *one* argument.**
  An audit built as `P="a.rs b.rs …"; git diff --numstat -- $P` printed **nothing** and its companion
  `git diff -- $P | grep -E "<foreign markers>"` printed **none** — both correct answers about an
  empty diff, because git was handed a single nonexistent path with spaces in it. The check whose
  entire job was "prove this commit contains no other agent's lines" returned a green by measuring
  nothing, one command before the commit. Caught only because the empty `numstat` was *also*
  surprising. **Write the paths out, or `set -- a b c` and use `"$@"`** — and treat an audit that
  prints nothing as a failure to run, never as a pass.

The general rule: the transform that makes output readable is also the transform that can invent a
green. When a conclusion depends on what was *not* in the output, re-run without the filter.

**And `rtk` rewrites pipelines, so this reaches controls that have nothing to do with cargo.** A
zero-deletion control on a regenerated data table ran `diff | grep -c '^<'` and reported **0**. The
true figure was about **15,000**; it surfaced only as 20,251 deletions in `git diff --cached`, and
the control had to be redone as a semantic parse (43 statics carrying over with all 30,360 literals
byte-identical). The generator emits one line per tick where the committed file is reflowed to four,
so a line-oriented control was the wrong instrument even before the pipeline ate the count. **Do not
build a control out of a shell pipeline here.** Count with a program that reads the file.


**12.43 The five species of vacuous test, worked.**

The *magnitude* species is new and it is subtle because everything else about the gate is right. The
hurt-overlay gate asserted that silhouette pixels **"moved toward vanilla's overlay red"** and
reported 3440/3440, with a working negative control. It measured **direction, not magnitude** — and
the shader was rendering ~70% red where vanilla renders ~30%, a predicate satisfied identically by
both. Wiring genuinely proven, strength never under test, and a player saw it immediately.

The repair generalises: **predict the value, do not merely assert the sign of the change.** Compute
*both* the correct and the suspected-wrong hypothesis from constants that originate outside the code,
and require the measurement to land on the right one. Here vanilla's overlay green is 0, so the blend
is a pure scaling in gamma space and green retention is `0.698` if right and `0.302` if inverted —
measured `0.6969`, control `0.3057`. A ratio needs no knowledge of the subject's own colours.

The *world* species is the live one here. A colour fix was verified against `--headless` and
measured byte-identical, concluding it was inert. There are two meshers: `--headless` renders
through `mesh_simple`, whose `ao` is corner-occlusion only, while `face_shade`'s per-face constants
live in `mesh_models`, which is what live terrain uses. **The change was verified against the one
scene in the tree that structurally cannot exercise it.**

**A second instance, and it is worse, because the test is end-to-end and looks unimpeachable.**
`c4ad474` added four `ServerBound` variants with four fully-written `apply_*` consumers and updated
only **two** decode arms. Issue #425 later found two of the strandings and missed the other two. One
of those was `CLIENT_COMMAND`, so `apply_client_command`'s `PERFORM_RESPAWN` was unreachable — and
per *Live-server hazards* below, **a dead player is held on the death screen and sent no chunks**, so
any player who died entered a permanent silent chunk blackout with keep-alives still flowing, and the
one packet that would have recovered it was the discarded one. The other was
`SET_CREATIVE_MODE_SLOT`: every creative inventory write from a real client, decoded field by field
and thrown away.

`serve_play.rs::creative_mode_slot_write_lands_in_the_real_inventory` is a genuine end-to-end test
over a real transport through a full login/join — and it runs against **`FakeProtocol`, which has its
own decode arms on invented packet ids 50/51**. So it proved dispatch and consumer against the one
`ServerProtocol` in the tree that structurally *cannot* exercise the production decoder. Nothing about
the test reads wrong; "end-to-end over a real transport" is exactly what you would ask for.

So the audit question is not "is this test integration-level?" but **"which implementation does this
test's transport actually resolve to, and is it the one production uses?"** A test double that is
*complete enough to pass* is the most dangerous kind. `serverbound_wiring.rs` now gates the class
structurally — every `ServerBound` variant must be constructed in non-test code — and it failed at
pristine `HEAD` naming exactly `["ClientCommand", "CreativeModeSlotSet"]`.

Two riders. That gate's own first draft was **half-vacuous**: a *comment* mentioning
`CreativeModeSlotSet` masked one of the two islands, so it now blanks comments, literals and
`#[cfg(test)]` modules before scanning — and it is lifetime-aware by lookahead rather than by the
toggle that silently broke three scanners here (see *Re-verify* above). And the stale claim ran the
**opposite** way from what everyone assumed: all five serverbound issues said "0/N decoded" when the
real figure was **60/69 decoded and 17/69 connected**. Decode was nearly finished; connectedness was
the whole gap. Check which axis is actually short before staffing a decode sweep.

Audit questions: *does any server-side counter accumulate past this gate's lifetime?* and *does the
input actually contain the structure the code under test exists to handle?*


**12.44 A test suite that opened a browser on the owner's desktop.**

**A test that performs an OS-level side effect is a defect with a user-visible symptom, and no health
check in this file can see it — the suite passes.** The owner reported that
`https://login.live.com/oauth20_remoteconnect.srf` kept opening in their browser unprompted. It was
**our test suite**: an accounts-screen unit test feeds a `Prompt` fixture, `nav.pump()` treats the
browser open as an *effect*, and `Command::new("open").spawn()` fires — so to the OS a unit test and
a player pressing **Add account** are indistinguishable. The fixture URL `https://microsoft.com/link`
301s to the device-code endpoint in one hop, which is why the symptom named a flow production has not
used since `c33e325`. It fired once per `cargo test -p lodestone-shell` run, which is constantly.

Two things generalise. **Fork on `#[cfg(test)]` rather than early-returning on `cfg!(test)`**, so the
interception is *assertable* instead of a silent skip — the gate that catches this asserts the
`cfg(test)` arm is the compiled one, and fails if the fork is deleted. And **grep for the effect, not
the feature**: `Command::new("open")` / `xdg-open` / `cmd /C start` tree-wide found a second latent
instance in `menu/telemetry.rs`, which had escaped only because no test activates those two rows.
Fixtures should use RFC 2606 `.invalid` hostnames as a second layer. Chasing this cost two passes,
and the first one's guess — a stale binary — was disprovable from the owner's own bug report that
morning.

**Measure by location, never by frame average.** Averaging a frame once gave G/R ≈ 1.13 and read as
"global gamma"; clustering by *location* revealed two spatially distinct populations, which a global
transform cannot produce. Ask *where*, not *what*.


**12.45 Shaders live in `.wgsl` files: the double-quote trap that bit four times.**

- **Shaders live in `.wgsl` files. Never inline one in Rust again.**
  `crates/lodestone-render/src/shaders/` and `crates/lodestone-shell/src/shaders/`, pulled in with
  `include_str!` — still compile-time, still a `&'static str`, no runtime asset loading. See
  [`docs/shaders.md`](./docs/shaders.md). "Just for a quick test" is not an exception:
  `no_wgsl_is_inlined_in_rust_sources` fails on any `@vertex`/`@fragment` under a crate's `src/`.
  The rule this replaces was *never put a double quote inside a shader, not even in a comment* —
  because a `"` terminated the enclosing Rust raw string and rustc then parsed the remaining WGSL
  and your *prose* as code: `error: prefix 'yet' is unknown`, pointing at English. The errors
  looked nothing like the cause, and it bit **four times**, twice inside comments that were
  themselves warning about the trap. Deleting the trap beat remembering it.
  Two things worth keeping from that history. First, **a `"` in a `.wgsl` comment is now legal and
  inert** — measured, not assumed: one put into `sky_disc.wgsl`'s comment left the suite green,
  while the same `"` in *code* position failed with `expected expression, found "\""`. Write shader
  comments normally. Second, **`cargo check` has never compiled a shader at any feature setting**,
  so before `wgsl_valid` a WGSL syntax error could reach `main` with all three required checks
  green — the only thing that read the WGSL was `create_shader_module`, inside an `#[ignore]`d GPU
  gate. `cargo test --workspace` now runs all 22 shaders through naga's front end in ~0.02s with no
  adapter.


**12.46 A file path in a document is a claim like any other.**

**This citation said `crates/protocol/v770/tests/` until 2026-08-04 and was wrong** — the tables moved
to `lodestone-data` in the #361 extraction and nothing updated the pointer. `v770/tests/` does contain
a `block_hardness_seam.rs`, which is a *different* test, so the stale path looked plausible enough to
survive review. It was caught only when an agent tried to build a `just` recipe from it and found no
such file. A file path in this document is a claim like any other; verify it before relying on it.


**12.47 Never hand-count an entity metadata index.**

**Never hand-count an entity metadata index. Run `EntityDataIndexOracle.java`.** It dumps every
`EntityDataAccessor` in the game sorted by index, so collisions land on adjacent lines. The first
time it was run it immediately found **two shipped bugs**: `Sheep.DATA_WOOL_ID` and
`Horse.DATA_ID_TYPE_VARIANT` were each off by one, both hand counts having missed
`AgeableMob.AGE_LOCKED`. **Every sheep in the game was rendering its default colour** while the
decoder reported a clean parse — invisible precisely because the tests encode with the same
constants they decode with, which is the `decode(encode(x))` trap in its most expensive form, and
because every sheep pixel gate builds its `EntityDraw` *downstream* of the wire.

Indices are reused across classes and **the guard you need depends on which classes collide**:
- Index 8 is `LivingEntity.DATA_LIVING_ENTITY_FLAGS` **and** `AbstractArrow.ID_FLAGS`, both `BYTE`,
  with the arrow's crit bit `0x01` bit-identical to "using item" — living vs **non**-living, so
  `entity_census::is_living` is the right guard.
- Index 15 is `Mob`'s flags (aggressive `0x04`) **and** `ArmorStand.DATA_CLIENT_FLAGS`, whose `0x04`
  is `CLIENT_FLAG_SHOW_ARMS` — and an armour stand *is* a `LivingEntity`, so `is_living` would report
  **every decorative armour stand with arms as an aggressive mob**. That collision is living vs
  living and needs `entity_census::is_mob`. `Display` also claims 15 as a `BYTE`.

So: check the oracle dump for the index, then pick the census column that separates the *actual*
claimants. Assuming the previous collision's guard generalises is how the armour-stand bug would
have shipped.


**12.48 The generated docs index, and the directory it silently omitted.**

This replaced a standing instruction to update the index by hand, and the reason is measured: at 77
commit-touches in 30 days `docs/README.md` was the **single most contended file in the repo** — more
than `sim.rs` — because every feature needed one line in it. It caused real damage in both directions:
a stale staged blob of it would have *deleted* a newer agent's bullet, and `0b95b4e` shipped a broken
link by capturing another agent's in-flight line. Those 77 touches per month now do not exist. A
generated index also cannot drift from the docs, which is the failure this repo's whole §2 is about.
Note that a doc with no usable summary makes the generator **fail loudly naming the file**, rather
than emitting a blank entry.

**But the drift gate proves consistency, not coverage — and it silently omitted a whole directory.**
The generator scanned `docs/`, `docs/roadmap/` and `docs/research/`, and **not `docs/plans/`**. Six
plan documents landed invisible to `docs/README.md`, each one written to satisfy the H1 +
`## What it is` contract that only matters *because* the generator reads it. Nothing failed:
`docs_index_matches_committed` compares the generator against the committed index, and both agreed
the directory did not exist. Fixed in `5bf792c`, but the general shape is worth keeping — **a gate
that compares two things you control cannot tell you that a third thing exists.** Ask of any
drift/parity gate: what is *in scope* for it, and how would I find out if something fell outside?
This is `decode(encode(x)) == x` wearing different clothes.

Two operational notes from the same episode. `read_md_dir_sorted` **errors on a missing directory**,
so deleting `docs/plans/` breaks the generator rather than degrading — same as `roadmap`/`research`,
so it is consistent, not a new trap, but do not create a fourth scanned directory lazily. And a
regenerated `docs/README.md` left sitting in the working tree **was swept into an unrelated agent's
pathspec commit** within minutes, briefly reddening `main` because the index moved without the
generator. If you regenerate, commit the generator change and the index **together and immediately**;
the window is measured in minutes, not hours.

## 13. Prior art, and the one risk that could sink this

Four known strategies, and where Lodestone sits:

| Project | Strategy | Fatal flaw |
|---|---|---|
| **stevenarella** (Rust) | All versions' structs in one 145KB `packet.rs`, per-version ID tables, names like `JoinGame_i8` vs `JoinGame_WorldNames_IsHard` | Struct-name proliferation — the name *is* the version annotation. Hand-written, no codegen |
| **ViaVersion** (Java, MIT) | Translation pipeline: chain `Protocol_N→N+1` steps until the server sees the newest format | No type safety at all — raw `PacketWrapper` byte manipulation. Every packet pays N−M translation hops |
| **MCProtocolLib** (Java) | Per-version duplicated codecs | **Combinatorial explosion.** Historically only supports the last few versions because the duplication is hand-maintained |
| **node-minecraft-protocol** (JS) | Runtime-interpreted ProtoDef JSON schemas | No compiled types; interpretation cost on every packet |
| **azalea** (Rust, MIT) | Derive macros + bevy_ecs, single version at a time | Not multi-version — but the best reference for macro ergonomics and ECS client design |

**Lodestone = MCProtocolLib's structure with codegen as the load-bearing difference.**

This must be stated plainly: **per-version duplication is exactly the pattern that limited MCProtocolLib to a handful of versions.** The design only works because:

1. Duplicated code is **generated**, not hand-written — `xtask new-version` clones a family crate and rewrites IDs from Mojang's authoritative `packets.json`.
2. Mojang now ships that authoritative packet report themselves, so ID churn costs nothing.
3. Human effort is spent only on packets whose *shape* changed, which `new-version` diffs and reports.

**The risk:** if codegen coverage is weak, every new version degrades into hand-editing N near-identical crates, and we inherit MCProtocolLib's failure mode.

**Mitigations:**
- Codegen coverage is a tracked metric — **built and measured (§12.68)**. CI-able in one command. But the useful figure is **hand-written lines per family (~2.3k v47 / ~2.4k v340 / ~3.7k v770)**, *not* a percentage: the per-struct derive ratio reads 84–92% and is structurally blind to the fact that dispatch logic, not packet structs, is the bulk of a version crate. Watch the absolute; a percentage improves whenever someone adds a large generated table.
- Phase 4 adds 1.8.9 deliberately early, under maximum structural stress, before there's much code to retrofit.
- If the ratio can't be held high, the fallback is to merge adjacent families and narrow tier-2 support — a scope decision, not a rewrite.

What Lodestone keeps from each: typed structs (stevenarella/azalea), adapt-upward-to-canonical (ViaVersion), crate-per-version namespacing so no name mangling is needed (`v47::play::clientbound::JoinGame`, not `JoinGame_i8`).

---

## 14. Open decisions

1. **Scripting runtime** — WASM (`wasmtime`, sandboxed) vs Lua (`mlua`, ergonomic). Leaning WASM for untrusted plugins. Deferred to Phase 8.
2. **Worldgen fidelity** — full density-function/noise-router parity for one version first, or approximate generation earlier for playability? Leaning parity-for-latest, validated by chunk diffing.
3. **Tier-2 breadth** — how many of the 17 families to actually populate. Recommend the 6 tier-1 first, then demand-driven.

4. **WebAssembly / browser target** — ✅ **spiked and answered.** See §16.

---

## 15. Session resource hygiene

**This machine is shared with an unrelated project.** At the time of writing Docker holds 13 images, 81 volumes and ~22 GB of build cache belonging to the user's *other* work (`mht-*`, postgres, valkey, seaweedfs…). Only two containers and two images are ours.

**The single irreversible mistake available in this environment is an unfiltered prune.** `docker system prune`, `docker volume prune` and `docker builder prune` would each destroy the user's other project. Never run them. Every cleanup action names its target explicitly.

**Rules for anything spawned (agents are told this in every brief):**
- Name containers `lodestone-<purpose>` so ownership is visible from `docker ps` alone.
- Prefer `docker run --rm` for one-shot work so nothing survives the command.
- Reuse already-pulled images (`eclipse-temurin:25-jdk`, `eclipse-temurin:8-jdk`) rather than pulling new ones.
- Report anything that outlives a command so it lands in the ledger.

**Ledger:** the `spawned_resources` table in the session DB records every long-lived resource, who created it, why, its exact cleanup command, and whether it's currently safe to remove. **Cleanup script:** `files/cleanup.sh` — `--status` (report only), default (containers), `--images`, `--deep`. It removes only `lodestone-*` containers and the two JDK images we pulled, and it prints the *other* project's containers under a "DO NOT TOUCH" heading so the distinction is impossible to miss.

**Deliberately kept:** `.cache/` (843 MB — jars, generated reports, and the decompiled client/server reference; expensive to refetch) and `vendor/minecraft-data` (431 MB). Both gitignored.

**Disk is a live session-survival risk, and `target/` is the thing that grows.** Measured mid-session with 13 agents building concurrently: the host was at **93% full — 30 GiB free of 460 GiB** — with our `target/` at **34 GB**, up from 6.3 GB earlier in the same session. At that trajectory a multi-hour run exhausts the disk, which would break *every* agent at once and is not recoverable by retrying.

Breakdown that made the fix obvious: `target/debug/incremental` **18 GB**, `deps` 13 GB, `build` 167 MB. **`incremental/` is pure regenerable cache** — deleting it does *not* force a dependency rebuild, because dependency rlibs live in `deps/`. Reclaimed it and verified the tree still builds (`lodestone-world`/`-physics`/`-assets` → `Finished dev profile in 10.77s`): **free space 30 → 44 GiB, `target/` 34 → 19 GB.** One `rm` reported "Directory not empty" because an agent was mid-write; a second pass cleared it, and nothing was corrupted.

Preferred over `cargo clean`, which would have cost 13 agents a full cold rebuild. Incremental compilation is left *enabled* — the speed matters more than the space as long as the cache is periodically reclaimed. **Re-check `df -h` and `du -sh target` periodically; reclaim `target/debug/incremental` when free space drops below ~40 GiB.**

**Verification can race a live edit — and nearly produced a false accusation.** I re-ran `wasm-spike`'s relay gate and saw it fail in 0.00s pointing at `127.0.0.1:2`, the dead port from its own negative-control experiment — which its report claimed to have reverted. That looked like an agent asserting a cleanup it hadn't done, the one failure mode that would undermine every report I accept. Checking the *source* before saying anything showed `const MC_SERVER = "127.0.0.1:25565"` — the revert had landed; I had run **while the edit was in flight** and cargo reused a binary built from the intermediate state. Forcing a rebuild: `ok, 15.38s`, matching its claimed 15.39s exactly.

**Rule: when verification contradicts a report, check the source before concluding, and force a rebuild.** With 13 agents editing concurrently, a stale artefact can make an honest agent look dishonest — and that accusation is unrecoverable once made. The same concurrency that makes parallel agents fast makes point-in-time verification unreliable; only the committed file is authoritative.

**Disk profile has shifted and `incremental` is no longer the lever.** This reclaim returned only 3.5 GB (33 → 35 GiB free) against 18 GB earlier. `target/` is now **25 GB of which `debug/deps` is 22 GB** — and unlike `incremental`, that is *not* wholesale deletable: it holds the current rlibs for every dependency, so removing it costs 13 agents a cold rebuild. `cargo sweep --time N` is the obvious tool and is **wrong here**: the artefacts with the oldest mtimes are the stable third-party deps that are still current, so a time-based sweep deletes exactly what's needed.

**Correction to that guidance — `deps` is not uniformly untouchable, and the distinction is own-crate vs third-party.** At 26 GiB free (94% full) with `deps` at 29 GB, measuring rather than accepting my own rule found:

```
$ ls target/debug/deps | wc -l                      → 636,555 files
$ ls | sed -E 's/^lib//; s/-[0-9a-f]{16}\..*$//' | grep ^lodestone | sort | uniq -c | sort -rn
  59614 lodestone        48552 lodestone_assets     37620 lodestone_render
  26772 lodestone_v770   23438 lodestone_net        22195 lodestone_model
$ find . -maxdepth 1 -name '*lodestone*' -mmin +90 -exec stat -f%z {} + | awk '{s+=$1} END {…}'
  reclaimable: 15.3 GB across 282,442 files
```

**Nearly 50,000 artefacts for a single crate.** Every rebuild emits a new content hash and nothing collects the old ones, so with 20+ agents rebuilding for hours, *own-crate* artefacts accumulate one dead hash per rebuild while only the newest is live. That inverts the sweep argument: for **our** crates old mtime means dead, precisely because they are rebuilt constantly; the reasoning that makes time-based sweeping wrong applies only to **third-party** deps, which are stable and current.

Deleting `*lodestone*` older than 90 minutes (never third-party, never outside `deps/`) took free space **26 → 35 GiB**, and `incremental` + `release` + `doc` took it to **37 GiB**, `target/` 33 → 22 GB. `cargo check --workspace --all-targets` clean afterwards in 1m55s. **The risk asymmetry is what justifies it:** a deleted rlib is recoverable — cargo detects the missing output and rebuilds — whereas disk exhaustion breaks every agent simultaneously and cannot be retried. Rule: reclaim `incremental` first, then stale **own-crate** artefacts in `deps` by mtime, and leave third-party artefacts alone.

**The prune hazard is still live and was re-verified**, not recalled: **13 images, 81 volumes, 22.16 GB build cache (19.71 GB reclaimable)**, all belonging to the user's unrelated `mht-*` project (`mht-web`, `mht-api`, `mht-worker`, postgres, valkey, seaweedfs, pgbouncer…). An unfiltered prune would destroy all of it.

**Cleanup harness audited and brought current.** `files/cleanup.sh`'s explicit registry had drifted to 4 containers while **7** were running. The catch-all (`docker ps -aq --filter 'name=lodestone-'`) meant cleanup was never actually unsafe, but the ledger no longer documented ownership — so a stop could have killed a server another agent was mid-test against. Registry now names all seven with owner and purpose: `mc262` :25565, `mc189` :25566, `entity-oracle` :25567/:25575 (impl-entity), `mc1122` :25568, `tw1122` :25569, `creative` :25570/:25571 (impl-game), `mc-online` :25572 (impl-net, the only `online-mode=true` server).

One safety property worth stating because it is not obvious: **Docker's `name=` filter is a substring match, not a prefix match**, so `name=lodestone-` would also catch `my-lodestone-x`. Verified on this host that every container matching `lodestone` is `lodestone-*` and ours, and the caveat is now recorded in the script rather than left as an assumption. Syntax-checked (`bash -n`) and dry-run via `--status` after editing.

**Known leftover:** `.deletion-drill-backup/` — confirmed **gone** (the v47 deletion drill completed and v47 is present and green); now also gitignored so a future drill can't pollute a commit.

**First-commit hygiene — done.** The repo has zero commits, so everything untracked is a commit candidate and stray artifacts were about to be committed as though they were source. Audited and fixed by extending `.gitignore`: downloaded tooling (`/.tooling/` — the `trunk` binary and its tarball), the scratch capture workspace (`/.capture/`, which carries its own `target/`), and root-level debug artifacts from headless and windowed render runs (`/*.png`, `/*.ppm`, `/*.log` — `lodestone-frame.png`, `lodestone-window.png`, `shell-window*.log`). These are *outputs of* tests, not inputs to them.

Verified both directions, because a `.gitignore` that over-matches is worse than one that under-matches — it silently drops source:
```
$ git status --porcelain | grep '^??'
?? .cargo/  .gitignore  Cargo.lock  Cargo.toml  crates/
?? rust-toolchain.toml  scripts/  web/  xtask/          # legitimate source only
$ git status --porcelain --untracked-files=all | grep -c '^??'
547                                                      # real files still visible
$ git check-ignore -q crates/lodestone-worldgen/tests/support/region_jvm.txt   → not ignored
$ git check-ignore -q crates/lodestone-physics/tests/support/sin_reference_jvm.txt → not ignored
```
That last pair matters specifically: the committed JVM reference dumps are the *anchors* for bit-exactness (§12.6), and a pattern that swept them up would have destroyed the parity guarantee while looking like tidiness. Deliberate asymmetry retained: `oracle-java/*.txt` **is** ignored (e.g. the 5.7 MB `shape_java.txt`) because the `.java` sources are tracked and Docker regenerates it, whereas `sin_reference_jvm.txt` is committed because §12.6's whole lesson is that parity must not depend on a regeneration step agreeing.


---

## 16. WebAssembly / browser target

Spiked **early and deliberately**: a wasm constraint found now costs a design tweak; one found after twenty crates depend on it costs a rewrite.

**Verdict: viable, and no architectural rewrite is implied.** Measured against the real tree on `wasm32-unknown-unknown`.

### What the spike overturned

- **I predicted `lodestone-assets` would fail on filesystem access. It does not.** `std::fs` *compiles* fine on wasm and only fails at runtime. Compile-time and runtime feasibility are different questions, and conflating them is easy — this directly limits what a "wasm CI check" can honestly claim.
- **One masking error made ten crates look broken.** Eight of ten were a single `uuid` feature flag away (`uuid` on wasm demands an explicit randomness source). Generalisable: when everything fails *identically*, suspect one shared cause before concluding the architecture is unfit.
- **`Instant::now()` compiles and then traps at runtime** on wasm — the nastiest item on the list precisely because it passes every compile check. `lodestone-render`'s `FrameClock` needs an injectable time source; the `tick_at` seam already exists.

### The one true blocker: networking

**Browsers cannot open raw TCP sockets. Vanilla servers speak only raw TCP. Therefore a browser build can never connect directly to a vanilla server** — it strictly requires a **WebSocket↔TCP relay**. No browser API removes this (WebTransport/WebRTC don't speak to a vanilla TCP listener either). Singleplayer has no such constraint.

The relay is ~150 lines and, critically, **protocol-blind** — because `Codec` is byte-transparent framing, it never parses a packet, so **one relay serves all versions and all servers**. The moment it parses a packet it becomes a per-version component and we'd need seventeen.

### The design paying off

The sans-IO split **held under this pressure, measured rather than asserted**:
- `Codec` is a pure synchronous state machine — reusable in-browser unchanged.
- `Transport` is a marker trait (`AsyncRead + AsyncWrite + Unpin + Send`); a WebSocket stream satisfies it for free, exactly as `DuplexStream` and `TcpStream` do.
- `ClientBuilder::connect_with<T: Transport>` already *is* the injection seam.
- Empirically, tokio's `sync, macros, io-util, rt, time` all compile on wasm (only `net` and `rt-multi-thread` don't) — so `ClientHandle`, `EventStream`, `select!` and `timeout` survive. **The public API does not leak TCP.**

Likewise **`select_strategy()` degrades `mdi-zero-instance` → `PerDraw` on WebGPU by construction**, with no new code, because strategy selection is a pure function of probed capabilities. That is the payoff for a decision made long before the constraint appeared.

### Cost-tagged change list

| # | Change | Cost | Status |
|---|---|---|---|
| 1 | `uuid` wasm randomness feature (`js`) | cheap | ✅ **landed** — verified: world/assets/render build to wasm, native unaffected |
| 2 | Target-gate tokio `net` + `Connection::connect` in `lodestone-net` | moderate | ✅ **landed**, and now mirrored in `lodestone-server` (§12.72), which unblocks **browser singleplayer** |
| 3 | WebSocket `Transport` + WS↔TCP relay | moderate | ✅ **landed** — joins the real 26.2 server through the relay |
| 4 | `spawn_local` shim in `lodestone-client::builder::start()` | cheap | ✅ handled at the task spawner (a browser has no blocking tokio runtime to `tokio::spawn` into) |
| 5 | Injectable clock in `lodestone-render/frame.rs` | cheap | ✅ landed, plus a hermetic guard banning the whole `Instant::now`/`fs`/`thread::spawn`/`tokio::time` family outside allow-listed files |
| 6 | `Trunk.toml` COOP/COEP headers (for future threaded meshing) | cheap | in flight |
| 7 | **Non-bindless atlas fallback** | invasive if late | ✅ settled by fit, not features (§12.17, §12.43): 1269 sprites ≫ WebGPU's guaranteed 256 layers, so Atlas2D is the *only* portable layout and `texture_2d_array` needs neither bindless nor non-uniform indexing |
| 8 | **`webgl` feature removed** | — | ✅ **not a tradeoff** (§12.72): 537 KB brotli (68% of the download) for a path that panics before frame 0 on a vertex-stage storage buffer WebGL2 lacks. Re-adding costs a downlevel-compatible render path, not a flag |

**Browser payload: 933 KB brotli** (raw 3.71 MiB / gzip 1.21 MiB) at the time of the §12.59 measurement, before `webgl` removal took another 537 KB off. Report **brotli** — servers ship wasm brotli-compressed and gzip overstates the real cost by ~26%. `wasm-opt -Oz` shrinks raw ~10% but makes the *brotli* artefact 4 KB larger, so it trades download for parse time; trunk's `data-wasm-opt="0"` is correct for bytes. Attribution: wgpu + naga + glow = 1.19 MiB, i.e. the graphics stack, not our code and not panic/fmt machinery.

### Bank-early constraints

- **Keep the `Transport` seam sacred.** TCP is currently reachable only via `connect` vs `connect_with`. If the integrated server or more of `-client` starts assuming `TcpStream`, we lose "the transport is the only thing that changes." Cost now: zero — it already holds.
- **Design the bindless atlas with a `texture_2d_array` + per-draw-material-index fallback from day one.** WebGPU and WebGL2 have no binding arrays and no non-uniform indexing. Retrofitting a second binding model into a shipped bindless renderer *is* the rewrite scenario this whole approach exists to avoid — and the renderer is being built right now, so the timing is unusually favourable.
- **A wasm check must state what it proves.** It proves compilation; `lodestone-assets` is the standing proof that compiling and working differ on wasm. An overclaiming check gets trusted right up until it's catastrophically wrong.

### Verified tool versions (checked against crates.io, not recalled)

`trunk` **0.21.14** (0.22.0-beta.2 exists) · `wasm-bindgen` **0.2.126** · `wasm-pack` **0.15.0** · `wgpu` **30.0.0** (compiles to wasm) · `getrandom` 0.4.3 · `tokio` 1.53.1.

**12.88 The connectedness metric was one hop short — again, and at a new layer. 37 of 66 `ClientEvent` variants are emitted into the void.**

I asked `impl-model` to audit its own surface with the standing island question, and it produced the first per-variant census of `ClientEvent`/`ClientAction`:

```
ClientEvent:  29 live · 37 write-only · 0 read-only · 0 dead · 0 mock-only
```

**Write-only** = a real adapter constructs it and **nothing outside reads it**. `WeatherChanged`, `GameModeChanged`, `Particles`, `LevelEvent`, `TitleText`, `MobEffectApplied`/`Removed`, `HeldSlotChanged`, `ExperienceChanged`, `BlockDestruction`, `ItemPickup`, `EntityDamaged`, all four title variants, `DifficultyChanged`… decoded, lowered into a canonical event, pushed into the channel, dropped.

**This is the third time the same metric has been wrong, each time one layer further out**, and the progression is the point:
- §12.66 — wrong **denominator** (265 = all five states both directions; play clientbound is 141).
- §12.74 — wrong **numerator**: counted `decode_and_validate::<T>(payload)?; return Ok(Vec::new());` as connected.
- §12.88 — **numerator wrong again, one hop later**: a packet that decodes into an event nobody reads is still decode-and-discard, just with an extra hop of indirection that makes it *look* connected.

The property I actually care about, and have never once measured directly: **does this packet change something a player or a bot can observe?** Every proxy I've reached for has been the cheapest thing adjacent to it. That is not coincidence — it is the standing failure mode, and it is now explicit: *the check tested a proxy for the property instead of the property*, for the fourth time (§12.69's username classifier, §12.74's `ClientEvent::` grep, and twice on this ratio).

**Corrected metric, three columns, none allowed to replace the others:** `decoded` (proves the codec) · `emits` (proves the seam) · `consumed` (proves the feature). Collapsing to one loses information, and I have consistently collapsed to whichever was easiest to compute. Routed to `impl-xtask` mid-build, with the instruction to get the consumer half **from `impl-model`** rather than re-deriving it.

**Important qualifier, so this isn't over-read: write-only is not automatically a defect.** `TitleText` has nothing to draw to; `Particles` has no particle renderer. Those are correctly plumbed and waiting on a UI that doesn't exist — expected staging. What is *not* acceptable is that the number was invisible until someone asked.

**Two genuine defects the same audit surfaced, both connection-affecting:**
- **`ChatAck` is `read-only`** — v770 *encodes* it, nothing *produces* it. That is the §12.76 chat cliff still live: the server pushes every **signed** message into a pending list drained only by our ack, and at **4096** it disconnects us. The fold exists in `lodestone-game::chat_ack`, the wire shape exists in v770, the action variant exists — only the producer is missing. Routed to `impl-client`.
- **`Disconnect` action is `write-only`** — the driver produces it on shutdown and **no adapter encodes it**, so we never send a clean disconnect.

**`ClientAction` encode coverage, per family:** v47 **16/43**, v340 **17/43**, v735 **17/43**, v770 **42/43**. Part of that spread is §3.4 working as designed — the model is shaped by the newest protocol and older adapters translate upward, so `SetPlayerInput`/`EndClientTick`/`ChatAck` correctly have no 1.8.9 encoding. But `BlockAction`, `UseItemOn`/`UseItem` and `InteractEntity` are `~` (partial/lossy) and `ContainerClick` is absent on all three older families — so **a 1.8.9 client still cannot break a block**. Routed to `impl-v47`, with the explicit instruction that the deliverable includes distinguishing *absent by design* from *not done yet*, because a table where those look identical is exactly how v735 shipped registered-and-unreviewed (§12.75).

**12.89 The browser requirement is met end to end, and the verification method is the lesson.**

`trunk` **0.21.14** is installed and `trunk serve` works — confirmed by **loading the page and seeing terrain**, not by observing that a server started:

```
[status] REAL terrain from real server bytes — 16 chunks, 16 sections, 250 greedy quads
         backend: BrowserWebGpu | select_strategy(): PerDraw          ~119–121 fps
[net]    relay probe OK — browser WebSocket → relay → live server
         version.name = "26.2" | {"version":{"name":"26.2","protocol":776}, …}
```

Browser → WS relay → **live vanilla 26.2 server**, round-tripping real status JSON. That closes the user's WebAssembly goal at spike level.

**A browser-specific verification trap worth keeping:** a 2-D `getImageData` readback of the WebGPU canvas returns **all-black** — that is the un-retained drawing buffer, *not* a blank scene. The composited screenshot is the reliable check. Anyone using pixel readback to verify the browser path will otherwise conclude the renderer is broken when it is fine — the inverse of this project's usual failure, and just as misleading.

**COOP/COEP asymmetry measured, not assumed:** `trunk serve` sets both headers and yields `crossOriginIsolated === true`; a plain static server sets neither. Real difference, documented at the call site in `web/README.md`, because anything later depending on cross-origin isolation (threaded meshing via `wasm-bindgen-rayon`) would work under trunk and fail mysteriously elsewhere.

**The relay-failure `TypeError` was a stale artefact, and the diagnosis matters.** The fix was already in `ws_web.rs` — the WebSocket `error` handler takes a bare `Event`, not `ErrorEvent`, specifically so it never reads `undefined.length`. The deployed `dist/` predated it. Rebuilding through trunk deployed it, and with the relay down the failure now reaches the **on-page status line** with no uncaught exception. The previous behaviour — page running at 122 fps while the network layer was dead, traceable only via a console error nobody reads — was the browser flavour of every fail-open in §12.52.

Also worth recording as good practice: the agent **declined to re-state the 933 KB brotli figure**, on the grounds that it had built debug and would not claim a number it did not run. Given §12.6 (an unreproducible hash constant that looked like a regression anchor and could detect nothing), that restraint is exactly right.

**12.90 Scope cut (user-directed): v770 only, four workstreams.**

New direction, superseding §12.80's four-family target: **make the latest version work fully.** v47/v340/v735 stay in the tree — not deleted, not improved. All effort goes to **v770 (protocol 776 / MC 26.2)** across exactly four workstreams: **packets · UI · entities · lighting.** Token and compute economy is an explicit requirement.

**Stood down 9 of 22 active agents**: `impl-v47` (multi-version action encoding — retracted mid-task), `bench-alloc` (settled: system allocator), `wasm-spike` and `impl-net` (browser goal met — trunk 0.21.14, live terrain at ~120 fps through the relay), `impl-audio` (working), `impl-worldgen` (bit-exact and now genuinely on screen), `impl-core`/`impl-model`/`impl-macros` (foundations sufficient). `impl-xtask` narrowed to a v770-only metric, then stops.

**13 keepers, mapped to the four workstreams** so nobody duplicates: packets — `bulk-packets`, `bulk-serverbound`, `bulk-encoders`, `impl-v770`, `impl-client`; UI — `impl-shell`, `impl-game`, `impl-assets`, `impl-render`; entities — `impl-entity`, `bulk-models`, `impl-physics` (narrowed to per-entity-type dimensions only); lighting — `impl-world`, `impl-render`, `impl-v770`.

**The standing rule given to all keepers: prefer connecting something that already exists over building something new.** That is the direct consequence of the island findings (§12.85, §12.86, §12.88) — 29 of 66 `ClientEvent` variants have a real consumer, ~40 of 141 clientbound packets are handled, and essentially none of the UI is drawn despite being fully modelled in `lodestone-game`. Depth is excellent; connectedness is the constraint. Ordering is now a rule rather than a preference.

Secondary benefit worth noting: dropping from 22 to 14 active agents should materially reduce the shared-checkout contention that has repeatedly produced transiently-broken workspaces (§12.62, §12.64) — and it removes the excuse, so a red tree is now someone's fault rather than ambient noise.

**Lighting is unblocked.** `handle.light_at` / `lights_at` / `sections_and_light_at` have landed on `lodestone-client`'s public surface — verified by grep, after several rounds of it being the single named blocker. The remaining lighting work is moving the oracle off superflat (§12.82) and consuming light in the mesher.

**12.91 Handoff committed to the repo, and the resource cut executed.**

User asked that the descoped work be captured "in a plan and handoff in the repo so i can pass it off later." Two documents committed and pushed (`1a7f7b8`):

- **`HANDOFF.md`** (509 lines) — the deferred work, self-contained. One section per area (the three frozen protocol families, WebAssembly/browser, audio, worldgen perf, online-mode auth, the closed allocator question, never-started items), each with *measured* current state, entry points, what remains, and the traps. Plus a consolidated traps section: the four species of vacuous test, the "expected value must originate outside the code under test" rule, absence-needs-a-control, live-server hazards, and the resource-hygiene warnings.
- **`DESIGN.md`** (2,264 lines) — the full design plan, which until now existed **only in session state**. That was a real gap: if this project were handed off, the single most valuable artefact would not have been in the repo. Prefixed with a reading guide flagging that §12 is the highest-value section, and correcting the stale "no production code written yet" line rather than deleting it.

Both verified non-ignored before committing (`git check-ignore`) — a `.gitignore` that over-matches silently drops source, which is the failure mode that would have destroyed the JVM parity anchors.

**Resource cut, which the user explicitly asked for.** Containers went 8 → **3**: stopped `mc189` (1.8.9), `mc1122` and `tw1122` (1.12.2), `mc1165` (1.16.5) — all serving now-frozen families — and `mc-online`, whose online-mode auth work is deferred. Kept `mc262` (v770), `creative` (v770 click/inventory oracles) and `entity-oracle` (v770 attributes). Disk was at **22 GiB free**, below the ~28 GiB threshold; reclaimed `target/debug/incremental` plus own-crate artefacts in `deps` older than 90 minutes → **27 GiB free**, `target/` 32G → 27G. Third-party artefacts untouched.

**Agents 22 → 14 active.** Verified `cargo check --workspace --all-targets` → exit 0 before committing. One transient red during the sweep (`lodestone-client` importing `lodestone_game::chat_ack` before the manifest edit landed) resolved on its own — the §12.86 re-layering completing mid-flight, and the §12.62 rule applying exactly as written: a mid-edit sample is not a measurement.

**Verified `impl-world`'s vacuity guard rather than taking the report** (`118 passed, 0 failed`). `light_exercises_propagation` makes the §12.82 degenerate-world case **fail closed**, and it ships with `propagation_check_ignores_a_secretly_uniform_values_array` — a `Values` array that is *materially* uniform passes a naive "is it `Uniform`?" check, so without that test the guard would itself have been vacuous. The remedy for a species of vacuity, built without reintroducing it.

**12.92 The connectedness metric, finally measured by a tool — and I was wrong a fourth time, in the opposite direction.**

`cargo xtask connectedness` landed and reports, for v770:

```
clientbound decoded 91/141; emits 89/141; decoded-but-stranded 2 [CHUNK_BATCH_START, RESPAWN]
serverbound encoded 49/69; examined 91 arm(s)
```

I had been quoting **"~40 of 141 clientbound"** — in agent briefs and in status to the user. The true figure is **91**. The bulk agents landed roughly fifty packets while my number stood still, because I was carrying a hand-count from §12.74 and never re-derived it.

**Verified independently before believing it**, because a tool I commissioned reporting a flattering number is precisely when to be suspicious:
- Denominators: `141` clientbound / `69` serverbound counted straight out of the `play::{clientbound,serverbound}` modules of `generated/packet_ids.rs` — exact match with the tool.
- Numerator sanity: a naive `grep -oE 'clientbound::[A-Z_0-9]+' | sort -u` gives **98**, against the tool's **91**. The tool is *below* the naive grep, i.e. it discards non-dispatch mentions rather than inflating. Wrong direction for a flattering bug.

**The fourth error in this metric, and the first optimistic one.** §12.66 wrong denominator, §12.74 wrong numerator, §12.88 numerator wrong one hop later — all three pessimistic-or-flattering in ways that tracked whatever was cheapest to count. This one is different in kind: the number was *correct when measured* and simply **went stale while the thing it measured moved**. That is a fifth failure mode to name — not a bad measurement, but a good measurement quoted long after its subject changed. The remedy is the tool existing, which is why it was worth building.

**What it does and does not say.** `emits 89/141` is the decode→seam layer, and it is genuinely strong — only **2** decoded-but-stranded packets (`CHUNK_BATCH_START`, `RESPAWN`). It does **not** contradict §12.88: 37 of 66 `ClientEvent` variants still have no consumer. Both are true. Packets now decode and reach the seam well; **consumption is the binding constraint**, and it sits in UI and rendering, not in the protocol crates.

**Consequence for prioritisation, acted on immediately:** packets are in far better shape than I told the keepers, so relative weight should shift toward **UI, entities and lighting** — the three workstreams that *consume*. I have corrected the baseline with the agents rather than letting a wrong figure keep steering them.

---

**12.93 One stale note produced four misdirected diagnoses — and it was cited as the *shared root cause* of all four.**

`docs/backlog.md` said: "`collect_item_model_parts` keeps only `IconPart::Model`, so an `item/generated` icon never enters `BlockModels::items()`. That is most items in the game."

**It was true when written and false when read.** `9980a96` added `extruded_sprite_geometry` — vanilla's `ItemModelGenerator` transcribed — and `BlockModels::build` inserts the resulting slab into **the same `items` map** the 3-D models go into, under the same key. `sprite_drop_pixels` had been passing since. Two doc comments on `BlockModels::item`/`items()` still said "3-D" and "`None` for a flat sprite"; `docs/dropped-items.md` still had a bullet titled "Flat sprite items are the remaining hole".

That sentence was then copied verbatim into **four** GitHub issues as their common cause. #54 said "**That is probably a shared root cause and #33 should be fixed first**"; #56 said "**#33 is a prerequisite, not a coincidence**". It was a prerequisite for neither. Measured causes:

| issue | claimed cause | real cause |
|---|---|---|
| #33 flat sprite drops | the sprite stream | **already fixed**; the issue itself was stale |
| #50 container block items flat | the sprite stream | `app.rs:1273` calls `render_scaled`, which hardcodes `models: None`/`depth: None` — a fully attached, fully tested model pass that nothing feeds |
| #54 first-person hand empty | the sprite stream | nothing ever told `RenderState` what the local player was holding; `render` takes only `&[EntityDraw]` and the local player is not in it |
| #56 no projectiles | the sprite stream | **no projectile renderer existed at all** |

**Why review could not catch it.** The note names a real function, a real enum variant and a real map, and describes a mechanism that genuinely existed. Grepping the function it names finds the function. Only reading the *whole* function — past the `IconPart::Model` arm to the `IconPart::Sprite` arm 16 lines below — refutes it. This is §12's staleness failure mode with a cost attached: **four diagnoses, three of them pointed at the wrong crate.**

**Two further beliefs in the briefing that measurement refuted:**

- **"`wind_charge` and `fire_charge` are cross-billboard thrown items."** `WIND_CHARGE` and `BREEZE_WIND_CHARGE` use `WindChargeRenderer`, a real cuboid model, and `AbstractWindCharge.getItem()` returns `ItemStack.EMPTY` — there is no sprite to billboard. `fire_charge` *is* one, but as the **item** of the `fireball`/`small_fireball` entities, at scale **3.0** and **0.75** respectively. `eye_of_ender`'s item is `minecraft:ender_eye`, not `minecraft:eye_of_ender`; a table derived from entity names draws nothing for it, silently.
- **"The gap is the consumer, so look for a missing accessor."** There is no missing accessor and there should not be one. Every consumer that wanted "the sprite stream" wanted `BlockModels::item`, which already answers both kinds. Reading the two-*stream* split in `ItemIcon` as a two-*map* split in `BlockModels` is what made a nonexistent accessor plausible.

**A measurement worth keeping, because it looks exactly like a bug.** `applyItemArmTransform` puts the held item 0.56 blocks right of the eye and 0.72 forward, and `hand_projection`'s 70° FOV is *vertical*. On a **square** viewport the item is outside the right edge entirely: measured on a working build, 256×256 → **0** lit pixels, aspect 1.5 → **2722**, 16:9 → **4191**. A pixel gate written on the repo's usual 256×256 target reads "the held item does not render" and sends the next reader after a chain bug that is not there. Vanilla's window is never square; the gate now renders 448×256 and says why.

**The negative control that had to be loosened, and why that is not a weakening.** The projectile gate's edge-on control (identity in place of `camera_orientation`) draws **494** px against the billboard's **3788** — 13%, not the ~6% a 1/16-thick slab's face-to-edge ratio predicts. `ItemModelGenerator` fans one edge quad per boundary texel of the alpha outline, and side-on those quads are the widest thing left. A 10% ceiling **failed on a working build**; 20% keeps a 7.7× separation and keeps the control able to fail.

**12.94 A `Mutex` only excludes code that takes it — and the lock is why nobody looked again.**

`crates/lodestone-fuzz/tests/length_prefix_allocation.rs` measures peak single-allocation size through a `#[global_allocator]` writing to `static PEAK_SINGLE_ALLOC: AtomicUsize`. It flaked. It had **already been fixed once**, with a `MEASUREMENT_LOCK: Mutex<()>` held across each measurement — which is exactly why the flake survived a second look: a lock is the canonical answer to a shared-state race, so seeing one present reads as *already handled*.

**The counter is process-global; the lock is opt-in. Those are not the same scope.** `real_registry_data_fixture_still_decodes_cleanly_after_the_fix`, in the *same file*, never calls `peak_alloc_during` and so never takes the lock. Its fixture read plus `RegistryData::decode` allocate into the shared atomic from a parallel harness thread. Every sibling test in every other binary does too — a `#[global_allocator]` has no way to not observe them. The lock serialises the *measurers* against each other and does nothing about the *allocators*, which are the entire population that matters.

Measured: the new control `a_sibling_threads_allocation_does_not_contaminate_a_measurement` failed against the locked process-global counter with `showed up in this thread's measurement as 48000000 bytes`, and reads **0** after the fix.

**The fix scopes the state rather than serialising the tests** — `thread_local!` `Cell<usize>`, which needs no cooperation from code that has never heard of the file, and lets the suite stay parallel. Three details are load-bearing: `const`-initialised, so reading the cell inside `alloc` cannot itself allocate and recurse; read through `try_with`, so a teardown-time allocation after TLS destruction records nowhere instead of panicking *inside the allocator*; and the `Mutex` deleted rather than kept "for safety", since leaving it would preserve the appearance that the race was addressed.

**The generalisation.** Ask of any lock-based isolation: *what is the set of code paths that mutate this state, and what is the set that takes this lock?* When the first set is "anything that allocates", "anything that logs", "anything that touches this global" — i.e. defined by a language or runtime facility rather than by a module — no lock can cover it and the fix has to move the state instead. This is §12.43's **duration** species (a counter outliving the gate) in its spatial form: **the state outlives the gate's scope sideways, across threads, rather than forwards in time.** Both are unreadable from the test source, because the test is exemplary and the defect is a property of what it was pointed at.

Same session, adjacent: `NullSink` discards writes and therefore **structurally cannot distinguish a refused packet from a wrapped one**, so the v340 overflow gate had to assert through a `RecordingSink` (`AdapterError::Decode` **and** zero `set_block` calls). A test double complete enough to pass is the dangerous kind (§12.43's *world* species). And the two-row control table there is the reason the fix is not half of one: `checked_mul(16)` alone accepts `chunk_x = 1_875_000`, which multiplies to 30,000,000 and fits an `i32` perfectly well while sitting outside `WorldBorder.absoluteMaxSize`. Without the border-pair control, the incomplete fix looked complete.

**12.95 The counter-over-duration rule, with the number that settles it: 585×.**

`CLAUDE.md` says prefer a counter over a duration, and that a timing taken under load is attributed to the wrong cause. This is the cleanest measurement of that this repo has, taken while verifying `main` at `d197d555` in an isolated detached worktree with its own `--target-dir`.

`sim::tests::a_frame_takes_many_short_world_guards_and_no_long_one` asserts that no single `World` guard in a frame approaches a frame's duration. It was the **only** failure in `cargo test --workspace --no-fail-fast` (5048 passed), and it failed on the duration:

| run | longest guard hold | holds counted |
|---|---|---|
| inside the full workspace suite (`--test-threads=2`, six agents building) | **27,028,583 ns** | **45** |
| alone, run 1 | 46,209 ns | **45** |
| alone, run 2 | 47,000 ns | **45** |
| alone, run 3 | 44,708 ns | **45** |

**A 585× spread on a byte-identical binary, while the counter does not move at all.** The gate's *subject* — "does the frame take many short guards rather than one long one" — is a claim about the count, 45, which was correct throughout. The gate's *assertion* is about a duration, which is a property of the machine at that moment.

Three things worth keeping:

- **This is not a flaky test in the usual sense.** The code under test never changed behaviour; the instrument measured the load. A "fix" that widened the bound would have destroyed the gate's ability to detect the real defect it exists for (one long guard instead of many short ones), because the real defect and machine load both move the same number.
- **The procedure that produced the right answer is the repo's own:** re-run a timing-shaped failure **alone**, a bounded number of times, before calling it a regression. Three runs, not one — a single alone-run agreeing with the assertion is consistent with luck.
- **Where a duration is genuinely the subject, report the counter beside it.** Here the counter was already in the failure output, and it is what made the diagnosis take one step instead of a bisect. A gate that prints only the quantity it asserts on gives a reader nothing to cross-check against.

Corollary for §12.19's ratio correction: a ratio of two *sequential* durations is not protected either, and this measurement bounds how badly — a 585× excursion on one arm swamps any ratio whose arms are not measured concurrently.

**12.96 The oracle was frozen, and it answered anyway: `pause-when-empty-seconds` defaults to 60.**

Every live oracle in `scripts/live-oracles/` is driven over RCON with **no player connected**. That is the normal state for a fixture, not an edge case — and vanilla's `pause-when-empty-seconds` defaults to **60**, so after a minute the server pauses the whole world. `gameTime` stops advancing, and because `ServerLevel.tick` calls `blockTicks.tick(getGameTime())`, **no scheduled block tick ever fires again**.

**What makes this a trap rather than an outage is that the rig stays half-alive.** Redstone dust propagates *synchronously*, inside `setBlock` — so a dust probe answers correctly, on a frozen server. Anything with a delay does not: repeaters, comparators, observers, torches, mob spawn cycles. A gate asserting "the signal arrived" passes; a gate asserting *when* reads a stopped clock. Found while oracle-verifying #315/#317, by a falling-sand control that had no business failing.

**It also appears to be the real cause of a rule this repo already wrote down.** `CLAUDE.md` carries "`tick step N` does not advance entity physics; only `tick sprint N` does". The jar sets `runGameElements = !isFrozen || frozenTicksToRun > 0` and gates block ticks on exactly that — **so a paused world and a frozen one present identically**, and a measurement taken on a paused server would produce that conclusion whether or not `tick step` behaves as claimed. The entity-physics half was measured separately (the fall-damage oracle, §12's `NoAI` note) and is not disturbed here. The *scheduled-block-tick* half is a misdiagnosis.

Two things worth generalising:

- **A fixture's default configuration is part of the code under test.** Nothing in our tree was wrong; the oracle was. An expected value sourced from an external oracle is only as good as that oracle being in the state you believe — which is a *third* thing to verify, alongside our code and the expectation.
- **Partial liveness is worse than no liveness.** Had the server refused connections, this would have cost minutes. Because synchronous work kept answering, the frozen clock was invisible to exactly the probes a developer reaches for first. The general form: **when an external system half-works, ask which half you have been reading.**

Fixed in all three scripts — `creative.sh` and `terrain.sh` rewrite the property before start (their `server.properties` persists across runs), `survival.sh` carries it in the heredoc it regenerates every run. The distinction matters: a `sed` in `survival.sh` would be overwritten by its own next line.

**12.97 A brokered patch whose stated justification was the exact mechanism that made it fail.**

I brokered a `tick.rs` patch for #465 that had the scheduled-tick loop re-run the redstone fan-out at the mutated position, and I wrote the reason into the patch: *"because `propagate_and_react` writes only on change."* That sentence is true, and it is why the patch cannot work — **the first run consumes the change**, so the re-run at drain time sees nothing to propagate. The implementing agent measured it at four delay settings: with my inline call, `powered=false` and output dust **0**; without it, `powered=true` at **15**. It landed in the opposite shape — the loop adopts the *schedule* the inline fan-out produced — as `4ee341d`.

The generalisable part is not "I was wrong about a cache". It is the **shape of the error**, which is not visible by reading the patch:

- **A write-on-change optimisation and a re-run are the same fact seen from two directions**, and the justification reads as *support* for the patch when it is a refutation of it. Nothing about the sentence looks wrong; it is a correct premise attached to the wrong conclusion. **When a patch's rationale names a state-consuming mechanism, ask who consumed it first.**
- **A second, independent defect hid behind the same call.** `propagate` notifies the origin's six neighbours and **never the origin**, so a freshly placed diode was asked nothing at all. Vanilla's half is `DiodeBlock.setPlacedBy` at delay **1** — not `getDelay`'s `2d`, which is the most plausible wrong model and the one I would have written. **Placement and signal-change are two different callbacks with two different delays.**
- **The orchestrator's patch is not privileged evidence.** Mine was reasoned from a call-site reading; the agent's came from a rig at four settings. The correct disposition of a brokered patch is *a hypothesis with a proposed diff*, and an agent that reshapes it on measurement is doing the job. Brokered patches should therefore state **what to verify**, not only what to type — this one stated the mechanism and asked for no measurement of it.

The gate that resulted is the shape to copy: it enumerates **six** plausible wrong models (request never reaches the loop; instant flip; delay in redstone ticks `1+d`; off by one; placement delay used for signal changes; delay-on-falling-edge-only) and shows the oracle separating each at **4 of 4** delay settings. The last of those is why the *rising* edge was measured at all — a falling-edge-only model reproduces the falling column perfectly, so the falling column alone proves nothing.

**12.98 The worldgen release baseline: the plan's five structural predictions were all correct, and the gap to sub-ms is 97×.**

Units 1–2 of `docs/plans/worldgen-rewrite.md`. **No release-profile baseline for the composed worldgen pipeline existed anywhere in this repo before this**; every prior number was debug, partial, or against a fixture tree with stages missing. The instrument is `crates/lodestone-worldgen/src/counters.rs` (relaxed atomics behind a default-off `gen-counters` feature) plus new benches in `benches/generation.rs`. Machine: Apple Silicon, seed 42, single thread, embedded 26.2 server data, all ten stages counter-asserted live. Two independent runs, taken with no other CPU-consuming process, agreed to **within 2%** on every figure below, so these are measurements rather than samples.

**The five predictions the counters confirmed — each derived independently, then asserted exactly:**

| claim | plan | measured |
|---|---|---|
| `block_at` calls per chunk fill | 98,304 | **98,304** (`16 × 16 × height`, `height = 384`) |
| pre-ore chunks touched by one cold `column()` | 25 (5×5) | **25** |
| ore RNG walks on a cold column | 9 (3×3) | **9** |
| climate table rows per biome search | 7,594 | **7,594** (58,519,364 / 7,706) |
| `String` allocations per warm column | ~885k | **885,898** (884,736 from `stitch_veg_region` + palette interns) |

D5's "~2.2M squared-distance comparisons per pre-ore chunk" measured **2.37M** (606,335,336 / 256) — within 8%, confirmed. The heap-allocation figure is worth stating separately because it validates the diagnosis rather than merely agreeing with it: a steady-state column performs **905,459** allocations, of which ~885k are those `String`s. **97.7% of all heap traffic on the serve path is one `to_string()` in one loop.**

**The verdict on sub-ms (a goal, not a gate, per the owner's ruling):**

| | measured | target | ratio |
|---|---|---|---|
| C_ss (median of 100 interior chunks, 12×12 sweep) | **96.8 ms** | ≤ 1.0 ms | **97× over** |
| C_cold (first column, fresh region) | **883 ms** | ≤ 8 ms | 110× over |
| vegetation stage alone | **52.8 ms** | ~1 ms decision threshold | 53× over |

**Follow-up correction (U3, `HEAD` at `55909f4`): the table above was taken counters-ON, which this harness's own doc forbids for a timing, and on the nightly whose release profile was broken. Both were re-taken; the figure barely moved, and *that* is the finding.** The row above stands as what was measured then — it is not rewritten — but the number later units are held to is the one below. Re-taken with `gen-counters` **off**, release, on the pinned `nightly-2026-08-07` (rustc `84b36a78a`), same bench binary, same seed 42 / 12×12 / 100-interior definition, machine quiet (`Swapouts` **971378 → 971378, flat across all three runs**; compressor pages *fell* 81642 → 80672; no other cargo invocation):

| run | C_ss | C_cold | vegetation stage | steady-state allocs/column |
|---|---|---|---|---|
| 1 | 101.68 ms | 867.1 ms | 63.42 ms | **905,459** |
| 2 | 97.78 ms | 851.8 ms | 63.77 ms | **905,459** |
| 3 | 95.99 ms | 822.6 ms | 52.28 ms | **905,459** |
| **median** | **97.8 ms** (98× over) | **852 ms** (107× over) | 63.4 ms | 905,459 |

**The expectation going in was that counters-off would measure materially *lower*** — the counters add relaxed atomics inside `block_at` (98,304 calls per chunk fill) and `next_bits`. It did not. 96.8 ms sits *inside* the counters-off spread (95.99–101.68, 5.6% peak-to-peak), so **the counter overhead is below this instrument's noise floor and cannot be resolved by it at all.** Two consequences, and the second is the one that matters:

- **No later unit's acceptance criterion changes.** C_ss moves 96.8 → 97.8 ms and the ratio 97× → 98×; the sub-ms verdict, the structural-waste-vs-parity-floor decomposition, and the per-draw argument below are all untouched. A 1 ms shift on a 97 ms number cannot reach any of them.
- **This bench's *timings* have a precision of roughly ±3%, and its `vegetation stage` figure is far worse than that — 52.28…63.77 ms, a 22% peak-to-peak spread on three runs of the identical binary, with nothing else on the machine.** Meanwhile the allocation counter read **905,459 exactly, three times out of three, to the digit.** That is CLAUDE.md's "prefer a counter over a duration" arriving as a measurement rather than a maxim, on this exact file, within one session: the counter reproduces bit-for-bit and can gate; the duration beside it cannot distinguish a 20% regression from Tuesday. **Do not state a vegetation-stage timing to three significant figures from a single run — and do not build any unit's acceptance criterion on one.** U3's criterion is the String counter for precisely this reason.

One caveat recorded rather than papered over: all three runs (and, as far as can be told, U2's) execute the whole `generation` binary, so ~2 min of other one-shot diagnostics precede C_ss and pre-heat the machine identically in every arm. That makes the arms comparable, which is what was needed here; it does not make any of these numbers a cold-cache figure.

Per-stage share of a served column: vegetation **52.0%**, ore **22.2%**, shape 9.9%, carve 6.8%, surface 3.1%, top_layer 2.1%, materialize 2.0%, aquifer 1.6%, biome 0.3%, intern 0.0%.

**The one number that decides whether the plan's optimism is justified is not any of the above — it is cost per RNG draw.** The vegetation walk draws **11,034** RNG values per column, and the plan is correct that this count is spec-bound and untouchable at parity. But it costs **4,781 ns per draw** — roughly **14,000 CPU cycles to service one random number**. A spec-bound draw whose consequences were evaluated against flat arrays and bitsets should cost tens to hundreds of cycles. So the 97× gap is **not** made of irreducible parity-bound work: it is one to two orders of magnitude of per-draw overhead sitting on top of a draw count nobody proposes to change. That reframes Q3's verdict — sub-ms is out of reach by tuning, and the question is entirely whether U3/U6/U7/U8 delete enough structure. Nothing measured here argues for weakening parity, and the recourse is not needed yet.

**Three method findings, each of which would have cost the next unit time:**

- **A stage-participation counter must sit *below* the stage's no-data early return, not above it.** Placed above, it reports "ore ran once per chunk" for exactly the run where `ore_stage` early-returned on an empty resolver — which is this file's own documented history and the *world* species of vacuous test. Placed below, `stage_entered[top_layer] == 0` is a precise, mechanical detector for "this bench is secretly pointed at the fixture tree". A per-stage *timing* floor, which the bench already had, catches this only probabilistically and only for expensive stages.
- **"Every stage ran exactly once per chunk" is one sentence with a different chunk count per stage**, because each stage has its own dependency radius. For a 12×12 sweep it is 256 (5×5 closure) for fill/surface/carve, 196 (3×3) for ore, and 144 for vegetation/top_layer/intern — asserted, and it held. Stating it as a single "144 of each" would have been wrong for nine of the ten stages and would have failed for the wrong reason.
- **`cargo bench`/`cargo test` in the release profile were already broken workspace-wide on the pinned nightly, and nothing reported it.** `rustc 1.99.0-nightly (da86f4d07 2026-07-24)` hits an **ICE** (`rustc_codegen_ssa/src/mir/operand.rs:291: not immediate`, on `UnsafeCell<MaybeUninit<Notified<Arc<multi_thread::Handle>>>>`) compiling **tokio 1.53.1** at `opt-level=3` whenever dev-dependencies enable `rt-multi-thread`. Confirmed pre-existing and independent of this work: `cargo build --release -p lodestone-server --tests` reproduces it on a graph these units never touch. Every health check in `CLAUDE.md` is a *debug* build, so none of them can see it — the same structural blind spot as the doctest rule, one profile over. Workaround that needs no file change and cannot affect a measurement (the bench never executes tokio): `cargo --config 'profile.release.package.tokio.opt-level=1' bench …`. **If a dated nightly is pinned, pin one that compiles tokio in release** — and note that a `--release` gate would have caught this months earlier than a `--release` benchmark did.

**12.99 Fourteen red `lodestone-server` tests, and not one of them was a bug in the code it tested.**

Cleared in six commits (`a4f43de`…`d5ed901`). The distribution is the finding: **zero production bugs under test, five stale premises, and two genuine production defects that no failing test was pointing at.** Every one of the fourteen was a gate that had stopped measuring its subject, and in four cases the failure message named the wrong culprit with complete confidence. Five entries' worth of method, in the order they cost time.

**A deliberate performance deferral is a silent rewrite of every count-over-N-ticks gate on that loop.** Issue #481 made `run_tick_loop` skip its random-tick pass while `game_tick <= 40` (2.0 s at nominal TPS), so the seeding task can populate the `ChunkStore` first. That pass is the *only* thing in the loop that calls `world.column()`. Three gates drove 12 ticks or a 1.5 s window and asserted on generated-column counts; all three read **0**, and one reported it as *"the store is not in the source path"* — a precise accusation against the wrong subsystem. The deferral is correct and stays; the gates now derive their tick count from the constant, which was made `pub(crate)` **for that reason and documented as such**. The general rule: when you add a warm-up, deferral or debounce to a loop, every gate that counts the loop's side effects over a fixed horizon silently becomes an assertion about your new constant. Grep for gates on that loop before landing it, and make the constant reachable so they can derive rather than restate.

**A tripwire that fires falsifies one claim; the rest of its justification was written by the same reasoning and needs re-checking too.** `outside_border_is_absent_from_the_generated_damage_type_table` fired, and its message named the production change to make. Doing it exposed a *second*, independent error in the same doc comment: it claimed the vanilla JSON "carries no bypass tags, so the derived flags would be all `false` anyway", and the real table records `outside_border` as **`bypasses_armor bypasses_shield bypasses_wolf_armor no_knockback`**. So `DamageFlags::default()` had never been equivalent to the table's answer, and the tripwire — which only ever watched for the entry appearing — could not have caught that half. The original evidence was `grep outside_border crates/lodestone-data/src/damage_types.rs` → empty. The table lives in `crates/lodestone-data/src/generated/damage_types.rs`. **A grep at the wrong path returns the same empty output as a true absence**, which is §12.42's invented green arriving through a path rather than a pipe: when a conclusion rests on zero hits, verify the file you searched contains anything at all.

**A test that has never passed is not a stale expectation — it is an expectation that was never evidence.** Two of the fourteen were born red, and `git log` said so in one command: production and test landed in the *same* commit (`d09c694` for `sleep`, `43e096b` for `world_spawn`) and neither file had been touched since. The `world_spawn` pair is the sharper case, because two sibling tests in one file encoded **mutually contradictory** rules — `plains_origin_chunk_yields_spawn_at_local_8_8` wanted local `(8, 8)`, while `ocean_origin_chunk_moves_the_spawn_to_the_nearest_land` wanted `x = 16, z = 0` for chunk `(1, 0)`, i.e. local `(0, 0)`. Vanilla's `PlayerSpawnFinder.getSpawnPosInChunk` (`:183-190`) scans from `chunkPos.getMinBlockX()`, so the sibling was right and the name was a transcription of the pre-#329 hardcode it replaced. **Before reasoning about which side of a red test is wrong, ask whether it ever passed** — and read its siblings, because a contradiction between two tests is free evidence that neither was checked against the source.

**A degenerate fixture makes the subject's own defect signature appear on a path the subject never runs.** `join_streams_the_view_outward_from_the_players_own_column` reported **123** columns generated before the first encode against a bound of 2, which is exactly what issue #453 undone looks like — generate the view, then encode. It was not. Decomposing the number: `1` (the #329 world-spawn search's `fallback_y` query) `+ 121` (the full ±5-chunk spiral, because an **all-air** fixture has no valid spawn anywhere so every candidate is rejected) `+ 1` (ring 0). The ring loop was untouched and still encodes inside it. The fixture was a *world*-species vacuity in the making (§12.43): it exercised the pathological invalid-origin path, not the one a joining player takes. **Decompose a count regression into named summands before believing the headline** — 123 as an opaque number reads as an ordering bug; `1 + 121 + 1` names the real cause in one line. It also surfaced a real defect no test was accusing: `find_initial_spawn` generated the origin column **twice** (once for `fallback_y`, once as the spiral's first offset), on the join critical path ahead of chunk streaming, invisible behind a `ChunkStore` and a doubling without one.

**A margin control can be promoted from "wrong" to "structurally void" by a change in another crate — and sometimes no correct margin exists.** Issue #463 (`3b65cbf`) replaced `NavigatingMob`'s shared `SplitMix64(0x1234_5678_9ABC_DEF0)` with a per-mob seed of `id as u64`. Two files' premises were built on the old stream and neither was touched by that commit:

| seed | first draw with `next_u64() % 120 == 0` |
|---|---|
| `0x1234_5678_9ABC_DEF0` (pre-#463, shared) | 130 |
| `1` (every mob in a fresh `MobSim`) | **9** |
| `3` | 147 |

`mob_idle_throttle`'s control asserted a mob with no player nearby can *never* stroll, which held only because draw 130 sat past `RandomStrollGoal`'s 100-tick throttle. At draw 9 the stroll fires *inside* the throttle, so **no outcome of the control could distinguish a working throttle from an absent one** — void, not merely wrong, and unreadable from the test source. The fix picks the subject's id deliberately (`set_next_id`) so its first hit is past the throttle again, and a `const _: () = assert!(EXPECTED_FIRST_STROLL_TICK > IDLE_THROTTLE_TICKS)` makes a future void control a **build failure**. Draw indices came from a standalone program over the documented SplitMix64 recurrence that reproduces the old seed's 130 exactly — reproducing the *superseded* value is what validates an oracle as external rather than a transcription of the run (§12.41).

`mob_roster`'s two negative controls are the harder half, because **there was no correct threshold to pick.** They asserted an untempted mob "does not close 3 blocks in 120 ticks", and a ±10-block random stroll can close 3 blocks by coincidence — while strolling toward a player is *legitimate vanilla behaviour*, so every candidate margin is either flaky or wrong. Loosening it to fit the observed 1.49 and 0.795 would have made the gate a mirror. The property actually under test was never a distance: it is **"this mob's movement does not depend on what the player is holding."** Asserted as *bit-identical trajectories* between two arms differing only in the held item, it is exact, tolerance-free, seed-independent, and strictly stronger than 3 blocks — an untempted mob never reads `held_item`, and `TemptGoal::can_use` consumes no RNG, so the streams cannot diverge. **When a margin control fails, ask whether a correct margin exists at all; if the underlying behaviour is legitimately random, stop bounding the outcome and start varying exactly one input.** The headline gate in the same file passed by the same accident and was hardened identically — a control that passes today because of a seed is a control that will fail tomorrow for a reason unrelated to its subject.

**12.100 The 885k Strings are gone (97.7% of the serve path's heap traffic, deleted by one representation change) — and the per-stage attribution says the acceptance criterion they were written for cannot be met by the unit that owns them.**

Unit 3 of `docs/plans/worldgen-rewrite.md`, at `99565a1`. §12.98 measured a steady-state warm column at **905,459 heap allocations**, of which **884,736** were one unconditional `state.to_string()` in `stitch_veg_region`'s `48 × 384 × 48 × 9` loop. Measured after: **20,686**. The counting allocator in `benches/generation.rs` (`measure_allocs`) is the instrument — real `GlobalAlloc` calls, not the hand-bumped `string_allocs` counter, which could have been "satisfied" by deleting a bump call. Swap was byte-identical either side of the run (`used = 1267.38M`, `Swapouts 983444`, unchanged).

The change is small and entirely internal: a per-generator `Arc<StateInterner>` shared by **every** grid, and both storage layers (`DenseBlockGrid`'s local palette, `VegGrid`'s backing map) holding `StateId(u16)` instead of `String`. The serve boundary was deliberately **not** crossed — every one of the 885k allocations was internal, `GeneratedColumn`'s `Vec<String>` palette is O(~50) per column and explicitly allowed by the plan's own budget as output, so touching `ChunkColumn` or the wire encode would have bought zero counter movement at the cost of the "49 × unexpected end of input" risk class. ~200 external call sites compile untouched.

**The determinism worry was real and is answered by construction, not by testing.** `overworld.rs`'s module doc carries a post-mortem on a `RandomState` iteration-order bug that already shipped here once, and interning invites exactly that failure: if id-assignment order reached the served palette it would be world-visible. It cannot, because **a grid's local palette still holds entries in first-write order** — interning changed what a palette entry *is*, not the order it is appended in — so `into_palette_and_blocks` emits a byte-identical `Vec<String>`. That is why the interner is free to be growable and lock-guarded with no determinism cost. `column_is_byte_identical_across_two_independently_constructed_generators` passes, and would be the detector if the property were ever broken. **The corollary is a trap for the next person: do not "optimise" a grid into storing interner ids directly in `blocks`.** It would delete the palette probe and it would put id-assignment order on the wire.

**The finding worth more than the 97.7%: on a warm column, seven of the ten stages allocate exactly zero, because they never run.** Per-stage binning of real allocations (new: the bench allocator reads `counters::current_stage()`) gives vegetation **20,625 (99.7%)**, intern 41, other 19, and **0 for aquifer, shape, biome, surface, materialize, carve and ore** — all cache hits at `column(5,5)` on a warm generator. Three consequences:

- **`steady_state_heap_allocs_per_column` is structurally blind to most of Unit 3's own file cluster.** The plan scopes U3 to `dense_grid.rs`, `carver/`, `feature/mod.rs`, `feature/top_layer.rs` and `overworld.rs`; porting the carver, ore or surface string paths off `String` **cannot move this metric by a single allocation**, only `C_cold`. An agent continuing that cluster on the strength of this counter would measure nothing and conclude wrongly. Ask *which stages a per-column metric can see* before aiming a unit at it.
- **U3's stated acceptance criterion — "the steady-state `String`-allocation counter reads 0" — is not reachable by U3.** The residual 99.7% is the vegetation *placement engine* (`get_state` returning `Option<String>`, the leaf `distance=N` read-and-rewrite, the `waterlogged` fix-up), which the same plan assigns to **U8**, gated behind U3 and U7. The criterion was written against a unit boundary that does not contain the code that satisfies it. Reported rather than met, with the number that proves it.
- **The table is scene-dependent, and says so.** `apply_freeze_top_layer` early-returns for biomes not listing `freeze_top_layer`, so seed 42's interior never exercises `top_layer.rs`'s `with_snowy_true` `format!` or its per-column `to_owned()`s. Those sites are real and this scene cannot see them — the *world* species of vacuous measurement, caught by asking what the input actually contains rather than by reading the bench.

**Two method points, both of which cost or saved time here:**

- **A calibration that asserts what your unit deletes must be *inverted*, not removed, and inverted with both hypotheses named.** `bench_counter_calibration` asserted `string_allocs >= 884_736` and failed correctly the moment interning landed — the ratchet doing its job. Its replacement requires the measurement to land on one of two externally-derived values: pre-U3 `>= 884,736`, or post-U3 interner warmup only (measured **65**, ceiling 1,000). A one-directional "it got better" bound would have been the *magnitude* species of vacuous test, satisfied by any improvement including a broken one. The ceiling is deliberately not `== 65`: the exact count is a property of the worldgen **data**, so pinning it would fail on a data update for no good reason.
- **Before attributing with a feature-gated instrument, prove the feature is neutral for what you are attributing.** Per-stage binning needs `gen-counters` on (without it `current_stage()` is a constant `Stage::Other`), and counters-on is forbidden for *timings* by this harness's own doc. `steady_state_heap_allocs_per_column` reads **20,684 with the feature on and off**, identically (20,686 after a later `palette_names` shadow, which adds exactly two per-grid `Vec`s) — which is what licenses reading the attribution as describing the same program the ratchet measures. Absent that control the split would have been a fact about a different binary.

**One number deliberately not claimed.** The counters-off C_ss for this run came out at **78.2 ms** against §12.98's 97.8 ms median. That is a 20% move on an instrument this very entry measures at ±3% run-to-run with a 22% swing on its vegetation stage, from **one** run, un-interleaved. Per the two-arm rule it is not evidence of a speedup and is recorded here only so a later interleaved measurement is not mistaken for a regression against a number nobody should have trusted. The gate was, and remains, the allocation counter.

**12.101 Vanilla has two trilinear interpolation orders; the one the block field uses is selected by a `cache_all_in_cell` that exists only in code, and the plan told U4 to implement the other one.**

U4 of `docs/plans/worldgen-rewrite.md`, at `4aa7ac85`. `NoiseChunk.NoiseInterpolator` computes its eight corners two ways: `Mth.lerp3` when `fillingCell == true` (**X inner**, then Y, then Z) and the incremental `updateForY` → `updateForX` → `updateForZ` chain when it is false (**Y inner**, then X, then Z). Bilinear interpolation is order-independent algebraically and not in IEEE 754, so those are two different worlds. `density/chunk.rs` implements the first; the plan's U4 row prescribes the second by name ("vanilla's incremental cell walk, `advanceCellX`/`updateForY`"), and so does a reading of `NoiseChunk`'s driver loop, and so does `DensityChunkOracle.java`, which literally calls those methods. **The first one is correct**, because `NoiseChunk`'s constructor (`NoiseChunk.java:157-160`) wraps `cacheAllInCell(add(finalDensity, beardifier))` around the router before anything reads it, and that cache's array is pre-filled inside `selectCellYZ` between `fillingCell = true` and `fillingCell = false` — so every value `getInterpolatedDensity()` returns was produced by `Mth.lerp3`, and the incremental chain is machinery the loop maintains and `final_density` never reads.

- **The marker that decides this is invisible to a data census.** `grep` finds zero occurrences of `minecraft:cache_all_in_cell` in all of 26.2's worldgen JSON; it is applied in Java. So the natural way to enumerate "which markers must the engine implement" — walk the `noise_settings` and `density_function` documents, which is how this repo's own type census was built — structurally cannot see the one marker that changes the arithmetic. An authoritative source answering a *neighbouring* question, again (§12's `registries.json` entry): the data is authoritative about the graph and silent about the wrapping.
- **The wrong choice costs 7,741 blocks per chunk and looks like a tolerance problem.** Measured by swapping the helper and re-running: `chunk_parity` goes from **98304/98304** to **90563/98304 (92.13%)**, every divergence a last-place difference. Terrain still generates and still looks like terrain. A 92% parity number reads as "nearly right, tighten the epsilon" rather than "wrong algorithm", which is exactly why it would have survived. The two nestings are bit-distinguishable at **60,300 of 393,216** blocks over four chunk/seed cases, worst absolute difference `1.78e-15`, so agreement was never plausible as an explanation.
- **The corner-harvest control fired on its first run, and its failure was the useful part.** `interpolation_order.rs` recovers the real corner lattice through the public API by exploiting `lerp(0.0, a, b) == a` exactly — at a cell corner the sampler's own interpolation is the identity, so no private access and no second corner implementation is needed. Rooted at `noise_router.final_density` it reported **178,815 / 393,216** blocks unexplained: `final_density` is `min(squeeze(interpolated(...)), ...)`, so the marker is nested two levels down and the enclosing ops vary *within* a cell. The premise holds only for a root that **is** the marker. Absent the control this would have produced a confident, wrong verdict about which order vanilla uses — the measurement was already running before the premise was checked.
- **The guard for this is inverted, and has to be.** The test asserts the two nestings *differ*, failing if they ever agree everywhere, because at that point it cannot distinguish a correct port from a wrong one; a guard that silently stops discriminating is worse than no guard. Its magnitude bound is **absolute, not in ULPs**: an interpolated density crosses zero, and two values straddling zero are thousands of ULPs apart while being `1e-15` apart, so the healthy case measures 2048 ULPs. What needs catching is a miswritten helper, which differs by O(1).
- **The correction does not remove U4's win, it re-aims it.** Vanilla does hoist the corner work — it hoists it with `Mth.lerp3`. The correct cell walk pre-fills a 4×8×4 = 128-value cell array from eight corners held once per cell, which is the *same arithmetic* the current per-block path performs. So the available win is a **lookup** win, not an arithmetic one: `786,432` corner lookups per chunk (98,304 blocks × 8) collapse to `1,225` corner evaluations (5 × 49 × 5, which independently agrees with vanilla's own slice accounting — `fillSlice` fills 5 × 49 = 245 per X-plane over five planes). The multiply-adds in the lerp are unchanged, which bounds how much of `C_ss` this unit can move and is worth knowing before attributing a disappointing measurement to the implementation.

Full account, including the four hidden semantics a flattened graph must preserve, in `docs/worldgen-density-engine.md`.

**12.102 The flattened density engine: two of the five semantics it had to preserve are unobservable at the geometry the game actually ships, and the counter prediction the issue carried was right per slot and 2× wrong per router.**

U4 of `docs/plans/worldgen-rewrite.md`, at `a4920cc2` and `85ca27df`, following §12.101's correction. `engine/` in `lodestone-worldgen-core` compiles a `Density` tree to a `Vec<Op>` of 16-byte records with `u32` child indices, evaluated against a pooled `Scratch` that holds every cache; `density/chunk.rs` became a façade and the recursive walker was deleted in the cutover commit. The premise, measured rather than estimated: `size_of::<Density>()` is **232 bytes** against 16 for an `Op`, because one variant inlines a `BlendedNoise` and every node pays the widest variant's width.

- **Two of the five semantics cannot be tested at `cell_width = 4`, and a test suite that tries passes while measuring nothing.** `flat_cache` snaps XZ to the **quart** grid — a hardcoded `>> 2 << 2` in vanilla, deliberately *not* `cell_width`. When `cell_width` is also 4, as it is for the overworld, every quart-snapped position and every corner position is a cell corner. At a cell corner all three lerp factors are exactly `0.0`, and `lerp(0.0, a, b) == a` **exactly** — so a nested `interpolated` is the identity whether or not it is transparent, and the X-inner and Y-inner nestings agree bit-for-bit. Nested-`interpolated` transparency and the interpolation order are therefore both value-unobservable at every position the evaluator ever evaluates them, and `chunk_parity` — 98,304 blocks against a real JVM, at `cell_width = 4` — says **nothing** about either. `engine_semantics.rs` uses `cell_width = 8` for those two so the quart grid lands mid-cell. This is the same shape as §12.101's own lesson one level down: the strongest available gate was silent on the rules most likely to break, and the silence looked like coverage.
- **The real router does not exercise nested-`interpolated` transparency at all**, in either direction: none of the compiled `final_density`'s five `interpolated` nodes is nested inside another. A semantic can be load-bearing in the engine and completely unexercised by the shipped data, which means "the parity suite is green" and "this rule is right" are independent facts.
- **A fixture built from `const`/`y_clamped_gradient` structurally cannot expose an interpolation-order difference.** A function of `y` alone has x/z-invariant corners, so every nesting of the lerps agrees exactly. The order test needs a genuinely 3-D input — it instantiates a real `NormalNoise` through an in-memory `Resolver` — and asserts that at least one sampled position is bit-distinguishable (2 of 6) rather than assuming it. The *world* species of vacuous test, caught by asking what the input data can express.
- **The counter prediction was right per slot and 2× wrong per router, and the premise check is what found it.** Issue #490 predicted 786,432 → 6,144 corner lookups from `768 cells × 8`. Correct *per interpolated slot*. The compiled `final_density` contains **five** `interpolated` nodes and enters **two** per block, so the real figures are 1,536 cell fills, **12,288** lookups against a no-hoist hypothesis of 1,572,864, and 2,450 corner evaluations (2 × the 5×49×5 lattice, unchanged by the hoist as designed). The gate's first version asserted "exactly one `interpolated` node" as a premise check before measuring, and that assertion failed on its first run. Without it the file would have asserted 6,144, failed, and been "fixed" by relaxing the number — the same failure mode as widening a tolerance, arriving via a different door. The three unentered nodes are `mul` short-circuiting and `range_choice` branching, **not** transparency: a structural walk applying the evaluator's own transparency rule finds all five reachable, so the obvious explanation was the wrong one.
- **The two hypotheses sit exactly 128× apart, and both are computed from outside the measurement.** 128 is the blocks in a `4 × 8 × 4` cell. A floor ("fewer lookups than before") would have been satisfied by a *single last-cell memo*, which measures 98,304 — 8× worse — because `fill_stage`'s innermost axis is Y and a cell spans only 8 of those, so a one-entry memo is evicted 12,288 times per node per chunk. The counter gate names that number and that diagnosis in its failure message, because the partially-working implementation is the plausible mistake, not the absent one.
- **Neither cache layer subsumes the other, and dropping the wrong one is invisible in the other's counter.** The cell layer (768 octets) deletes the *lookup*; the per-slot layer (1,225 values) keeps the *evaluation* count honest, because adjacent cells share corners. A hoist without the slot layer shows the winning 12,288 lookups while quintupling the expensive half of the work. Two counters, one per layer, and the gate asserts both.
- **D3, measured with both arms in one process: 19,356 allocations per chunk → exactly 0.** `build_aquifer` cloned eight `Density` trees per chunk; the fields are now three `Program`s (`Arc<Graph>` + a root) and five `Arc<Density>`. The control — the literal pre-U4 deep clone, still reachable — is measured *first* and required to exceed 1,000, so the zero cannot be explained by a dead instrument. The first version of that test cloned into `Vec`s, measured 17, and would have needed a 16-allocation container-overhead allowance; switching to fixed-size arrays and pre-allocating the sinks outside the measurement window made the expected value an exact zero. **An allowance you have to explain is a tolerance you will later widen.**
- **Sharing the graph created a lock-contention hazard that the deep clone had been hiding.** The compiled `final_density` carries **708** `Cache2D` nodes inside its point-evaluated leaves, each with a `Mutex`-backed last-value slot, reached on the order of 10^4–10^5 times per chunk from the spline leaves. Per-chunk deep cloning gave each chunk its own cold, uncontended slots; `Arc`-sharing turns them into 708 slots contended by every generating thread, converting a cache that exists to save time into a serialisation point. `Cache2DSlot` now uses `try_lock` and treats contention as a miss — value-invariant, because the memo's key is an exact `(x, z)` over a pure subtree, which is the *same* property that licenses the sharing at all. The fix and the justification are one argument.
- **`Program::compile` must not take the slot count, and finding out why is a construction-order lesson.** `Builder::slot_count` is an over-approximation shared across every tree it built and is only final after the **last** `build` call, so a `compile` that demanded it cannot be called where the trees are assembled. The count belongs to the *scratch*, not the graph.
- **`interval_select` cannot derive its threshold count as `functions.len() - 1`.** `Builder` tolerates a missing `thresholds` array (`unwrap_or_default`), and the tree walker's loop is over `thresholds`, not `functions` — so with `k != n - 1` it performs `k` comparisons and falls back to the last function. A flattened graph that derived `k` would read `n - 1` params, running off the end of that node's payload into whatever the next node pushed. Flattening turns a harmless tolerance in a parser into an out-of-bounds read of a neighbour's data.
- **`cargo check -p <crate> --all-targets` does not see a sibling crate's test targets.** The D3 change passed `check -p lodestone-worldgen --all-targets` while `lodestone-worldgen-core`'s lib-test target did not compile, because two call sites in `engine/graph.rs`'s own unit tests still passed the removed argument. Caught by `cargo test`, not by any `check`. A narrower `-p` is a narrower blind spot, not a faster version of the same check.
- **No speedup is claimed, and the reason is structural rather than caution.** The two arms are different builds of the same symbol, so they cannot be interleaved in one process, and this repo's two-arm rule exists because non-interleaved worldgen timings have been attributed to the wrong cause before. Equality was established instead, at the U6 bar: 786,432 blocks over 8 chunks at 4 seeds, two isolated worktrees, md5-verified-identical harness, `cmp`-clean and md5-identical, with a detector control confirming a single flipped bit is reported. Context only: 1.31 s old / 0.35 s new on that sweep, separate processes.
- **`legacy_random_source` (#486) and `end_islands` were deliberately not landed, and the reason is the island rule, not effort.** There is **no Nether or End generator anywhere in the workspace**, `lodestone-worldgen`'s fixtures contain only `noise_settings/overworld.json`, `end_islands` appears only in a census test, and there is no `NormalNoise` legacy-init path (only a blended-noise-only `PerlinNoise::create_legacy_for_blended_noise`). Both would be individually built, individually tested, and reach zero blocks. #486's own Sequencing section asked whether to land it before U4 or fold it in; the interpreter rewrite it was waiting on is now done, so that half of the question has expired and only the no-consumer half remains. It belongs inside group NE as its first item.

Full account in `docs/worldgen-density-engine.md`.

**12.103 The per-ring join barrier was a workaround for a workaround, and the commit that reverted its removal was right for a reason nobody wrote down: two defects were live and only one of them was the cache.**

U10 of `docs/plans/worldgen-rewrite.md`, at `7ba0176b` and `0a3ede8d`, issue #494. `crate::join_scheduler` replaces the barrier with a **primed sliding window** over the same wire order: in-flight width `2 × available_parallelism`, first top-up to 1, emission strictly in coordinate order. Full account in `docs/join-scheduler.md`.

- **`4307b59`'s message names the cache, and deleting the cache is not sufficient to undo it.** *"Revert per-ring barrier removal — cache contention with 289 concurrent generator calls."* U6 deleted both `Mutex`-guarded FIFO caches, so that clause expired. But the reverted commit's in-flight count was `(2r + 1)²` — it scaled with the **view radius**, so an 8-core machine ran 289 concurrent `spawn_blocking` generator calls, and the structural gate measures the old shape at **279 columns in flight** against the window's 8. A revert message that cites one cause when two are present is the most expensive kind of accurate: the cited cause gets fixed, the fix looks complete, and the second cause returns with it. **Ask what else changed in the commit being reverted, not only what its message blames.**
- **Its stated rationale had become a comment about nothing, and that is what made it look load-bearing.** *"Ring 0 seeds the cache, ring 1's columns hit those cache entries"* described `pre_ore_cache`, deleted in `34202a21`. U6 handed the comment over as a brokered patch rather than editing another unit's crate; without that hand-off the next reader would have found a documented reason to keep a barrier that had none.
- **The acceptance number is 441/361 and it had to be *repeated*, not merely correct.** Three release runs of the 289-column burst through the barrier-free scheduler: `pre_ore_computed = 441`, `post_ore_computed = 361`, hits `5,698` / `2,240`, evictions `0` — all three identical, matching U6's landed figures down to the hit counts. The old cache's signature was over-computation **and** variance (452/452/448, 380/383/372), so a single correct reading proves less than three identical ones: an exact match repeated is the only result that excludes a racing miss.
- **The negative control severs the dependency edge rather than perturbing the scheduler.** What the barrier stood in for is the store's per-entry `OnceLock`: two workers needing the same chunk's same stage join on one computation. Cutting it — a fresh generator per column, so nothing is shared — moved a 3×3's pre-ore from **49 to 225** (9 × 25) and post-ore from 25 to 81. A control that reproduces the *subject's* number is the failure mode to fear here; this one differs by 4.6×.
- **Time-to-first-chunk is a counter, and reading it as one is what let the barrier go without regressing #453.** "Columns generated before the first chunk was encoded" is 1 for the barrier, 1 for the window, and **289** for the pre-#453 flat shape. That third reading is why the window is *primed*: a plain sliding window fills before awaiting its head, so a fast source completes the whole window first and the counter jumps from 1 to `window` — a #453 regression invisible to any wall clock, because the latency is unchanged and only the *count* moves.
- **The first version of the landing gave both `SourceRef` arms the same window, and arithmetic says it cannot help the borrowed one.** Ring cumulative sizes are `1 + 4r(r + 1)`; `r(r + 1)` is always even, so every ring boundary sits at an offset ≡ 1 (mod 8) — precisely where a window-8 batch boundary sits. No batch even straddles a ring, and ring 8's single 64-column blocking batch becomes eight serial ones: the split **adds** barriers. It failed its own "a group spans two rings" gate on the first run, which is the only reason the congruence was noticed rather than shipped as a slowdown. A blocking source has no encode to overlap with, so uniformity across the arms was the wrong goal; the property that has to match is the **wire order**.
- **A wall-clock comparison of two schedulers on this machine changes sign with the arm order.** Non-alternated, the window arm's time-to-first-chunk read 803 ms against the barrier's 471 ms; with identical coordinates and the burst's own load excluded it read *lower in 5 of 6* alternated rounds (mean 441 ms against 462 ms). Full-burst totals were window-lower 4 of 4 alternated and window-*higher* when the barrier always went first. Both arms drift by more than the gap across rounds. Following §12.100, the numbers are recorded and no speedup is claimed — but the specific lesson is sharper than "interleave": **the confound was that the second arm in a round inherits the first arm's thermal and allocator state**, so alternation is not a refinement of a sequential measurement, it is the difference between signal and its opposite.
- **The scratch harness's first version measured two different chunks.** Each arm built its own generator at a coordinate offset by 100 to "keep them independent" — but the arms then generated *different terrain*, and a 2× time-to-first-chunk gap that read as a scheduler regression was one column of a more expensive biome. Independence between arms comes from a fresh generator, never from a different input.

**12.104 Twenty-one percent of worldgen CPU was SipHash, the two containers everyone would have guessed were not it, and the thing that made the biggest swap provably safe was a doc comment a previous unit wrote against exactly this hazard.**

U17 of `docs/plans/worldgen-rewrite.md`, at `d50feba7` (+ a second increment), issue #498, doc `docs/worldgen-fast-hashing.md`. Found by U5's profile (#495), not by the plan. The instrument is `samply` 0.13.1 against the release `benches/generation.rs` binary, `threadCPUDelta`-weighted, with every sample whose **leaf** frame is a hashing symbol attributed to its nearest non-hashing caller — an inverted-caller join the shared `scripts/profile-cost-table.py` does not do. Hash-leaf self time: **21.01% of all CPU**, the second-largest item in the pipeline behind `place_placed_feature::recurse`.

- **Attribution first was the right instruction, because the two candidates a reader would rank highest are both absent.** The brief listed U6's 64 shard `HashMap`s and the density slot caches. The shard maps **never appear in the hash profile at all** — a column takes on the order of 34 store probes, and *sharding a map does not make the map hot*; a structure being concurrent, mutex-free and much-discussed makes it memorable, not expensive. The density caches were already on a private `FxHasher` U4 had added for this exact reason. What the profile actually named: `RegionView::overlay` **39.5%** of the hash time, `StateInterner::ids` **20.8%**, `ocean_floor_wg` **12.8%**, `DenseBlockGrid::index_of` **11.8%** — four point caches, ~85% of it, three keyed by small integers and one by our own block-state strings. `reserve_rehash` at 6.8% was the ore overlay *growing*, not a mis-sized static table.
- **The biggest single swap was safe because U7 had already written the defence, in prose, against this exact hazard.** Changing a hasher changes iteration order, and `overworld/mod.rs`'s module doc carries the post-mortem of a `RandomState` iteration-order bug that shipped here once. `RegionView::overlay` is the one hot map that *is* iterated — and `centre_writes_in_scan_order` sorts by the **full key** over unique keys, with a doc comment saying it does so because *"the `RandomState` trap … is avoided by construction rather than by hoping the map iterates the same way twice."* A later unit's licence to re-hash that map was bought entirely by a defensive comment written before the need existed. The general point: **a doc comment that states an invariant and names the incident behind it is load-bearing evidence for future work, not commentary.** The corollary is that `FxHash` being safe for non-adversarial keys and a map's order being unobservable are **two independent claims**, and only the second one licenses the swap; `hash::fast`'s module doc is written to force that second claim to be made per map.
- **Copying a well-known hasher's shape without checking whether it helps *here* silently lost a property, and it failed in the direction that reads as fine.** The first draft ended `finish()` with `rustc-hash`'s `rotate_left(20)`. `hashbrown` indexes buckets from the **low** bits, and multiplication by an odd constant is a bijection mod `2^n`, so an unrotated multiply makes the low `n` bits a *permutation* of the key's low `n` bits — and `StateId`s are handed out `0, 1, 2, …`, so a `StateId`-keyed table collides **never**. Measured: 4096 sequential keys into 4096 buckets occupy **3931** distinct buckets with the rotation and **4096** without. The trap is that 3931 is *excellent* — a uniformly random hash gives ~2589 — so every plausible spread check passes and the review question "is the hash any good?" answers yes. The test that caught it predicted the exact value (4096) from the algebra rather than asserting the spread was good, which is §12's *magnitude* species arriving as a fix rather than a defect.
- **A repetition count is not evidence the thing ran, and two independent instruments of that failure fired in one session.** The brief asked for `parallel_generation_is_deterministic_and_matches_serial` repeatedly. First attempt: `cargo test -p lodestone-server --lib <bare_name> -- --exact` — `--exact` matches the **full module path**, so twelve consecutive runs reported `0 passed; 545 filtered out` and **exit 0**, twelve green results measuring nothing. Then the *guard* written to catch that class was itself vacuous: `n=$(python3 …)` printing two numbers, followed by `set -- $n`, in **zsh**, which does not word-split an unquoted `$var` — so `$1` was the whole string and the guard reported all 12 runs vacuous when all 12 had genuinely passed. Both directions of wrong, from the same root: **a pass/fail verdict computed by a shell pipeline is not evidence.** Re-counted with a program reading the log files, requiring `1 passed; 0 failed` *and* the test's own name present in the output: 12 of 12 genuine.
- **On a shared checkout, "I only changed my own files" is a different claim from "the arm contained only my files", and only a worktree establishes the second.** The first after-arm byte dump was taken in the main checkout — where a concurrent unit had edited `overworld/decorate.rs` four minutes earlier. It came back `cmp`-clean, and the reasoning that this could only ever have produced a false *failure* (a foreign terrain change breaks equality, it cannot manufacture it) is correct but is not the same as having isolated the change. Re-run in a fresh detached worktree at the baseline sha carrying **only** U17's files, verified by `git status` in that worktree: same dump, same md5 `518283b2719f4e1994016de8e690d51f`. The cheap version of this discipline is to `git status` the worktree and read the file list, not to reason about who is editing what.
- **Absolute times from two `samply` captures on this machine are not comparable, and the control for that is code you did *not* change.** Hash-leaf share read 21.01% before and 10.46% after — but self time for **unchanged** callers moved ×1.77 (`build_surface`), ×1.82 (`StatePredicate::test`) and ×2.25 (`VegGrid::get_local_id`) between the captures, because siblings were compiling during the second. So the difference of two percentages whose numerator *and* denominator both drifted is not a delta, and `21.01% → 10.46%` is recorded here as two descriptions of two captures rather than as a measurement of this change. What *is* sound is **categorical**: the callers that vanished from the attribution table are exactly the maps that were swapped (`StateInterner::id_of`, `DenseBlockGrid::set_id`, `RegionView::get`, the overlay half of `ore_stage`), and every caller that remains is a map that was not. **A named frame disappearing from an attribution is not a duration and does not care how hot the machine was** — prefer that shape to any percentage when the arms are two binaries. Following §12.100 and §12.103, no speedup is claimed.
- **`scripts/profile-cost-table.py` is broken against the installed `samply`, and the failure is a schema move, not a bug in either.** U5 reported it "misbehaving". Cause: `samply` 0.13.1 writes `preprocessedProfileVersion: 55`, in which `funcTable`/`frameTable`/`stackTable`/`stringArray` live on **`threads[i]`**; the script reads a top-level `profile["shared"]` (`RawProfileSharedData`, a later `fxprof-processed-profile` layout) and dies with `KeyError: 'shared'`. Its sidecar join, symbol resolution and `threadCPUDelta` weighting are all correct and were reused verbatim over the per-thread layout. Anyone fixing it needs a version fork, not a rewrite — and the documented profiling workflow is unusable until someone does.
- **~4.7% of CPU was attributed and deliberately left on the table, which is the correct outcome, not an unfinished one.** `ocean_floor_wg` and `surface_diff` live in `overworld/{decorate,fill}.rs` and `feature/mod.rs` (U15 live); the vegetation maps in `feature/vegetation/**` (U8). Every one is a point cache that is never iterated, so every one is a one-line swap — for its owner. Two agents in one file is its own incident class in this repo, and a measured share is not a licence to enter a file.

**12.105 The unit briefed to cut "2.2M ore-path allocations" found the ore engine held 207,671 of them and the surface stage held 3,847,972 — the figure was inclusive of a dependency closure, and the two instruments that agreed on the answer disagreed by design.**

U18 of `docs/plans/worldgen-rewrite.md`, at `22982b99` (instrument at `d44a07a1`, its own regression at `9675ab8d`). Ore-stage allocations **207,671 → 503** over a 3×3 cold sweep (**8,306 → 20 per ore pass**, −99.76%), seed 42, embedded production data, real `GlobalAlloc` calls binned by innermost `Stage`. `counters::rng_draws[Stage::Ore]` reads **992,537 before and after**; 45 dumped columns (5 seeds × 3×3, 8,899,204 bytes) are byte-identical, both arms md5 `a9db7cf741214167db615fa8b9356fa8`. Two causes: `Placement::get_positions` returned a fresh `Vec<BlockPos>` per modifier per attempt (79.96%), now `OrePositions::{None,One,Repeat}` — U8's vegetation shape applied to the engine U8 did not touch; and `do_place`'s two per-blob scratch `Vec`s (9.65% + 10.21%), now `thread_local` free-lists.

- **"Allocations on the X path" is ambiguous between *inclusive* and *self*, and for a stage that drives its own dependency closure the two differ by 18×.** `ore_stage` calls `pre_ore_stage` for its eight neighbours, and each of those runs aquifer → shape → surface → materialize → carve. So a samply subtree, a wall-clock span, or an allocation count taken *around the call* all attribute the surface stage's 3,847,972 allocations to ore. Measured self-time attribution: **surface 92.46%, ore 4.99%, carve 2.44%**, total 4,161,893. `counters`' `StageGuard` attributes to the **innermost** stage — the property its module doc already claimed and the only reason the two are separable at all. The brief's premise survived review because it was arithmetically consistent with a real measurement of a real thing; it was just a measurement of a different question. **Before optimising a share, ask whether the denominator includes work the subject merely triggers.**
- **Two instruments that agree are only evidence if they could have disagreed.** The site table is a *sampled* backtrace aggregation (1 in 64 — symbolising a backtrace against this binary's debuginfo costs milliseconds, and the unsampled version ran over eight minutes without finishing); the cross-check is an *unsampled* allocation-**size** histogram. They agree — 79.96% vs 78.62% for `get_positions` — and the agreement is load-bearing because the size is derivable from the source independently: `size_of::<BlockPos>()` is 12, and **12 is not a multiple of 8**, so a 12-byte allocation cannot be a `Vec<u64>` or a `Vec<f64>`. The 163,272 twelve-byte allocations are therefore *necessarily* single-element `vec![pos]`, which is an exact floor on what the fix had to remove rather than a share. `2048 = 64 · 4 · 8` and `1056 = 33 · 4 · 8` name the `size = 64` and `size = 33` ore configs the same way. A second instrument that reads the same table by a different route proves nothing; this one reads a different quantity.
- **The two controls were measured on a different scene by a different instrument than the attribution, and that is what made their agreement mean something.** Reverting each mechanism separately in the working tree: `get_positions`' `Vec` reinstated → **6,701** warm allocs/pass; scratch reuse disabled → **1,901**; both exit 101 with the diagnostic naming the regime. The **77.9% / 22.1%** split is on the JVM plains-land fixture at 50,920 writes; the attribution's **79.96% / 19.86%** is on embedded production data over a 3×3 sweep. Two scenes, two instruments, agreement within two points.
- **`Vec::resize` only zeroes the elements it ADDS, so a recycled bitset arrives dirty — and the symptom is a silently dropped ore, not a slow one.** A `VisitedBox` buffer returning from a larger blob keeps that blob's set bits in every word the smaller blob reuses; a stale set bit reads as "already tested", so `do_place` **skips** a `try_place_ore` it must perform. `clear()` before `resize()` is the fix, and it is a correctness requirement wearing the costume of tidiness. It is also **invisible to every pre-existing test**, all of which handed `over_spheres` a fresh `Vec` — the hazard is created by the reuse and so cannot pre-date it. `a_recycled_visited_buffer_starts_clear` was run against the `clear()` removed and observed panicking before the clean result was believed. **Whenever a buffer starts being reused, the *new* question is what it carries in, and no existing test can be asking it.**
- **An allocation gate needs a magnitude, not a bound, because a bound is satisfied by any implementation that happens to sit under it.** The absolute figure is 1 warm allocation for 50,920 writes, and the bound is 64 — deliberate headroom, because the residual belongs to `feature/region_view.rs`, a medium this unit does not own and a neighbouring unit was live in. The sharp assertion is `ore_allocations_do_not_scale_with_placement_attempts`, whose failure message computes *both* hypotheses. The first draft of that bound's derivation was also **wrong in a way the measurement caught**: it reasoned "the overlay grows geometrically from empty, so `O(log2 w)` ≈ a dozen", when `RegionView` recycles its map from a thread-local free-list and does not regrow it at all. The prediction was off by an order of magnitude in the *safe* direction and would have been recorded as a derivation.
- **Nine other stages' allocation counts, unchanged to the digit, were the strongest control available and nobody designed them.** Surface 3,847,972, carve 101,651, shape 2,303, biome 784, materialize 444, aquifer 441, other 250, intern 219, vegetation 158 — identical across the before and after runs. A change reaching outside the ore engine would have to move one of them. Per-stage binning was built to *attribute*; it turned out to also *bound blast radius* for free, which is worth remembering the next time a counter looks like it only answers one question.
- **Four candidate crates were offered and the measurement rejected all four, which is a result rather than a refusal.** `smallvec`/`arrayvec` would have removed the same allocations while keeping a length the code no longer needs — the measured shape is *exactly one position, or n copies of one position*, and an enum states that in the type where a `SmallVec` cannot distinguish `Repeat` (recurse n times on the same position) from a fan-out, which is precisely the distinction the RNG-desync hazard turns on. `bumpalo` is for heterogeneous short-lived scratch; after attribution there were exactly **two** shapes with a per-blob high-water mark. `rustc-hash`/`hashbrown` — nothing here needed a map. `memchr` — `split('[')` does survive on this path, but neither `split` allocates, so it appears nowhere in the attribution: it is a CPU question, and U15's prescription (an id-keyed bitset, not a faster scan) is still the right one. **A dependency shortlist is a hypothesis set; attribution is what falsifies it.**
- **A guard written to prevent a vacuous measurement made the default build red, and the fix was a `cfg` rather than a weaker assertion.** `attribution_requires_counters` asserted `counters::enabled()` unconditionally, so `cargo test -p lodestone-worldgen` failed for anyone who had not asked for the `gen-counters` diagnostic — 142 passed / 1 failed on a healthy tree. The guard protects an `#[ignore]`d test that only ever runs when named, so it belongs *inside* that test (where it now is) and its standalone form belongs under `#[cfg(feature = "gen-counters")]`. **A guard against vacuity that fires in configurations the thing it guards cannot run in is not a stricter test, it is a broken one.**
**12.106 The last 30 allocations came out with the free-list U7 had already named — but the gate written to measure them read the right number for the wrong reason twice, and the dense-array "obvious better fix" was refuted by counting.**

U19 of `docs/plans/worldgen-rewrite.md`, closing the two follow-ups #497 (U8) and #498 (U17) each named and left. `feature/region_view.rs` gains a private `scratch` module: a **per-thread** free-list recycling `RegionView::overlay`, `VegGrid::blocks` and `VegGrid::dirty`, cleared on return so a buffer in the list holds no keys, bounded at 4 per shape. A warm 3×3 vegetation pass goes **13 → 0** allocations; a warm served column **87 → 64** (41 output allowance untouched, 16 other, vegetation 30 → 7); vegetation RNG draws read **11,034**, unchanged. Byte-identical over 45 columns / 5 seeds, both dumps md5 `a9db7cf741214167db615fa8b9356fa8`, detector control observed firing. Per §12.100 and §12.103, no speedup is claimed.

- **The measurement that mattered refuted the fix everyone would have reached for, and it refuted it by *counting* rather than by timing.** #498 measured `RegionView::overlay` as the largest hashed map in the pipeline (39.5% of hash time, 8.3% of all CPU) and left a change rule — check for a dense array before reaching for a cheap hasher, as U15 had done for `ocean_floor_wg`. The cheap form of that here is a **presence bitset in front of the map**: 108 KB instead of a 1.77 MB dense value array, and it pays *only* if probes are overwhelmingly misses, which the read-then-fall-through-to-source shape makes look obvious. Counted instead of assumed, over warm columns at seed 42: **230,582 probes / 109,884 hits** at (0,0), 190,818 / 83,868 at (1,0), 177,831 / 71,970 at (0,1) — **~45% of probes hit**, because ~7,000 written cells absorb ~100,000 successful reads, about **15 re-reads per written cell**, as the heightmap scans and `is_adjacent_to_air` walk back over cells decoration has just placed. So the bitset removes ~55% and *taxes* the other 45%; only the full dense array (1.77 MB + 108 KB for the ore view, 3.1 MB + 192 KB for the vegetation grid, per thread) removes the hashing outright. **The cheap approximation of a good idea was defeated by a two-counter measurement that cost one temporary instrumentation and one test run** — and note the direction of the error: the intuition was not slightly off, it was wrong about which half of the distribution dominated.

- **"Warm" is a property of the *lifecycle* the harness reproduces, not of the arm's label, and the gate kept reading 13 after the fix landed.** `vegetation_allocs.rs` held arm 1's grid and arm 2's grid alive at the same time. Production builds one medium per served column and **drops it before building the next** — and that drop is the whole mechanism, because it is what returns the buffer. So arm 2 drew from an *empty* free-list and measured a cold container growth under a warm label, reporting the pre-change number against changed code. The fix is one `drop(cold_grid)` plus an assertion on `scratch_free_list_lengths()`; the lesson is that a steady-state gate has to model the **object lifetime** of the steady state, not just its inputs. The same shape then produced a *second* wrong reading: `allocations_are_geometric_growth_not_per_write`, whose ratio form floored a zero denominator with `max(1)`, **failed against a correct implementation** (`2 → 13, ratio 6.50` against a write ratio of 1.20). A test whose predicate is built around a residual becomes a false alarm the moment the residual reaches zero — the assertion has to be replaced, not widened.

- **The instrument's own bookkeeping was 2 of the residual allocations, and it was flat, which is exactly what made it look like the engine.** With the containers pooled, a warm pass read a stubborn **2** — identical at 2,086, 2,209, 2,211 and 2,499 writes. Flatness reads as "a fixed per-pass cost in the subject", i.e. precisely the profile of something worth chasing in the engine. It was `census::reset()`: `VegCensus::unsupported` is a `BTreeMap<String, usize>`, and clearing it costs a `String` clone plus a tree node on the first unmodelled dispatch of each distinct reason. `OverworldGenerator` never resets the census, so those 2 exist **only inside the measurement**. This is the same species as this file's `seeded_grid` note — 16,384 harness `to_string()`s attributed to the engine — recurring at 1/8000th the magnitude, where it is far harder to notice. **Take deltas across the window; do not reset a counter inside one.**

- **A residual allocation count is not attributable, and the cheap way to make it so is a counter on the *mechanism*, not a profiler on the symptom.** Seven allocations per warm column remain in the vegetation stage. Rather than argue about them, `region_view::scratch_misses()` counts takes that found the free-list empty: it reads **0** across four warm production columns, with a control that drains the free-list and observes it reach 2. That is a categorical answer — *none of the three pooled containers is responsible* — obtained without `samply`, and it hands the next unit a measured starting point instead of a suspect list. The 7 are inferred from the call site to be `vegetation_stage`'s private post-ore copy plus its palette growth, and that inference is recorded **as an inference**.

- **Recycling a `HashMap` is the same hazard as re-hashing one, and the safety argument has to be re-established per consumer rather than inherited.** A recycled map has a different capacity and therefore a different iteration order than a fresh one — the identical failure mode #498's doc was written against, and the one `overworld/mod.rs` records having shipped once when a `RandomState`-ordered map fed the palette. It is safe here for two *separate* reasons, one per consumer: `VegGrid::blocks` is private to its module and never iterated at all, and `RegionView::centre_writes_in_scan_order` sorts by the full key precisely so order cannot be observed. Neither reason covers the other, and neither would cover a third consumer added later.

- **The control worth running was the one aimed at this change, not the one the file already documented.** U7's recorded way to break the seam control is `source_slot` on `/ 16`, which still drives it from 20 crossing rows to **0** (`bbox = None`) — valuable, but as proof the control is still **live** rather than merely still green after an edit. The control that actually exercised U19's own failure mode was removing `map.clear()` from `Overlay::drop`: a recycled buffer carrying the previous column's writes collapses the production seam to `leaves_east=0`, the cross-seam spill gone entirely. **When you add a mechanism, the inherited controls test the old mechanism; write the one that fails if yours is wrong**, and run it before believing the green.

- **No dependency was added, and the reason each candidate lost is worth more than the conclusion.** `smallvec`/`arrayvec` address containers that are *small*; these are recycled *whole* and the `dirty` log reaches ~2,500 entries. `bumpalo` addresses many short-lived heterogeneous allocations; the residual is neither. `memchr` had no scan left after #497's interning. `hashbrown`/`rustc-hash` would have replaced ~30 lines of arithmetic that `hash/fast.rs` records **deliberately** declining to take as a dependency — "a `Cargo.lock` edit in a shared checkout for the same arithmetic" — so taking it would have reversed a documented decision to buy nothing. Evaluating on measurement rather than reputation returned "none of them" for four different reasons.
**12.107 The recorded reason this fix had been filed rather than done — "`Rule::Bandlands` *computes* its name" — was true and irrelevant: the set it computes over is 192 entries drawn from seven, fixed at parse time. And the RNG control the brief specified reads zero for this stage no matter what the code does.**

U21 of `docs/plans/worldgen-rewrite.md`, closing #501. The surface stage carries interned `StateId`s across every seam — the `pre` callback, `Rule::Block`, the clay-band table, the sparse diff. Surface-stage allocations **3,847,972 → 690** over a 3×3 cold sweep at seed 42, embedded production data, real `GlobalAlloc` calls binned by innermost `Stage` (**78,530 → 14 per stage entry**, −99.98%), digit-stable across two runs per arm. **All nine other stages digit-identical.** 45 dumped columns byte-identical, both arms md5 `a9db7cf741214167db615fa8b9356fa8`, detector control observed firing (`differ: char 2000001`). Per §12.100 and §12.103, no speedup is claimed. Details in `docs/worldgen-surface-ids.md`.

- **"It computes the value" and "the value set is unbounded" are independent properties, and the blocker on record conflated them.** #501 named the obvious fix (return a borrowed `&str`) and recorded why it could not work: `SurfaceSystem.getBand` *computes* which terracotta it returns rather than selecting a static one, so there is nothing to borrow. Correct, and it stopped the previous unit. But `getBand` indexes `clayBands`, and reading `generate_bands` rather than `get_band` shows the table is exactly `CLAY_BANDS_LEN` = 192 entries written from **seven** hardcoded literals — the whole value set is known once per world seed and pre-internable into a `Vec<StateId>`, after which `get_band` is a subscript and a `Copy`. **The accessor tells you how a value is selected; only the producer tells you how many values there are.** The general form: when a note says a representation change is blocked by a computed value, go and count the things it can compute. `bandlands()` now *asserts* the 192 and the membership rather than assuming it, so an eighth band block in a future version fails loudly at generator construction and names itself.
- **A control inherited from a sibling stage carries the sibling's instrumentation coverage, not yours — and this one was structurally zero.** The brief required "assert the surface stage's RNG draw count unchanged", with precedent `rng_draws[Ore]` 992,537 and vegetation 11,034 digit-identical. `rng_draws[Surface]` reads **0**, on both arms, because `bump_rng_draw` has exactly one site — `WorldgenRandom::next_bits` — and the surface system's positional draws (`surface_depth`'s `master.at(x,0,z)`, `Cond::VerticalGradient`'s `next_float`) go through a bare backend instead. "Unchanged at 0" would have been reported as evidence and would have been worth nothing. **Before treating a counter's stability as evidence, check the counter is non-zero on the baseline arm.** The live substitutes were carve 352,859 / ore 992,537 / vegetation 40,917 digit-identical — stages that *consume* this one's output — plus byte identity, which fixes values and positions together and is strictly stronger than any count.
- **The nine-stages control passed too well, and the surplus was a hole in the scene.** Removing `Ctx.biome`'s `String` also deletes one allocation per `SurfaceSystem::top_material` call, which is on the **carve** path — so carve was predicted to fall. It read 101,651 on both arms, to the digit. The explanation is that this scene never calls `top_material` at all: no carver exposed dirt beneath a carved grass block in those nine chunks, which the arm-A carve site table independently corroborates (1,585 samples, no `top_material` frame). So the strongest control in the set is, for that one mechanism, **unable to move** — the *world* species, where the input does not contain the structure the code exists to handle. A control that cannot move is not confirming anything, and the way this was caught was predicting the delta first and then having to explain its absence.
- **The JVM parity gate for this exact stage cannot see this change, and only running the control revealed that.** A deliberately mis-classified pre-surface block — the **right** id with the wrong `PreClass`, i.e. a fully-connected wire carrying a wrong value — left `surface_parity` **green** on both fixtures and failed only the composed `overworld_gen.rs` gate ("surface rule barely ran: 59 surface-capped vs 197 stone-capped columns"). The reason is a transport difference: the fixture path classifies from the *string* via `PreState::from_name`, production reads the class off the `BlockKind` the aquifer fill already wrote. Both are correct; they are different code. **Ask which implementation a test's transport resolves to, not whether the test is integration-level** — and note the direction, which is the dangerous one: the *stage-specific* gate was blind and the *composed* gate caught it.
- **The obvious id conversion would have improved the allocation counter and made the program worse, so the gate is on the interner's length rather than only on allocations.** Returning `StateId` from `pre` via `interner.id_of(name)` deletes every `String` — and replaces ~60,000 allocations per chunk with ~60,000 `RwLock` read guards on a table shared by every concurrent generator call, which is precisely the shape `4307b59` was reverted for (cache contention across 289 concurrent calls). The allocation count would have looked like a win. So `nothing_is_interned_during_a_surface_scan` asserts `StateInterner::len()` is unchanged across a scan, and asserts `len() > 1` beforehand so it cannot pass by having nothing to intern. **When the cheap instrument can be satisfied by moving a cost onto an uninstrumented one, gate the mechanism, not the symptom.**
- **The residual was predicted from outside the code and landed exactly, and the second fixture is what made it a prediction rather than a fit.** 690 = 14 × 49 + 4: fourteen allocations per stage entry over 49 entries. `hashbrown`'s capacity series (3, 7, 14, 28 … 14336, 28672) needs exactly **fourteen** doublings to hold a chunk's ~18,200 rewrites at 87.5% load. Confirmed three ways: the sampled backtrace table attributes 100% to `build_surface`'s `FastMap`; the **unsampled** size histogram shows exactly 14 distinct sizes (76 … 557,064 bytes, a doubling series for a 16-byte entry) each occurring exactly 49 times; and the two JVM fixtures both measure **14** despite a **1.39× probe ratio** (60,157 vs 83,472 probes), which is the per-probe claim in one pair of numbers. The 4 remaining 64-byte allocations are below the 1-in-64 sampling resolution and are recorded as **unattributed**, not explained.
- **A class that is derived cannot be typo'd, and the control that separates "derived" from "asserted" is a same-class-different-id swap.** `PreState` carries an air/fluid/stone class beside the id so the scan needs no name; a wrong class is not a crash but a different set of rules firing and a plausible-looking column. Rather than hand-write the class at the use site and assert it, the three `default_*_pre` fields are built by `PreState::from_name`, i.e. by applying `class_of_name` — the surviving string definition — to the very string the settings supplied, and `surface_stage` re-derives all four on every entry under `debug_assertions`. The control that proves this is stronger than a constant-vs-constant check: pairing `default_fluid_pre` with `default_lava`'s string is the **same class, a different id**, and it fired (exit 101). A `class_of_name(&self.default_fluid) == PreClass::Fluid` assertion — which was the first draft — could not have seen it.
- **The measurement inverted the arena argument instead of merely repeating the refusal.** `bumpalo` suits many short-lived *heterogeneous* allocations, and this stage looked like the first plausible candidate in the drive: on paper, hundreds of thousands of small strings. But the conversion *removes* them rather than pooling them, and what is left is **14 allocations of one shape**, so there is nothing for an arena to amortise. `memchr` again appears nowhere in an allocation attribution because `split` does not allocate; `smallvec`/`arrayvec` again have no small container (192 fixed, ~18,200 in the map); `hashbrown`/`rustc-hash` would again reverse `hash/fast.rs`'s own documented refusal. Four candidates, four different reasons, no dependency — the third unit in a row, which is itself worth noting: **the shortlist keeps losing because attribution keeps finding a representation error rather than an allocator problem.**
- **The dense-array follow-up is now sized rather than recommended.** `worldgen-fast-hashing.md` says prefer an array where the key space is dense and bounded, and left `surface_diff` open. The numbers to decide it exist for the first time: the map's final table is **557,064 bytes** for ~18,200 entries, while a dense `16 × 384 × 16` array of `StateId` is **196,608** — smaller, and *one* allocation instead of fourteen, taking the stage 690 → ~49. Deliberately not done: it is a second byte-identity run and a sentinel question ("unchanged" is not "air"), against a residual that is now 0.6% of a scene whose largest term is carve's 101,651. **Carve is now the pipeline's largest allocator** (94.6% of the post-U21 total, 71.8% of it in `CarveEnv::carve_block`), which is where the next unit goes.

**12.108 The owner's "exponentially slower as I walk from spawn" is real and is not a CPU term: per-column cost is flat to a million blocks out, and what grows is the staged store, whose 512-entry ceiling is dead code on the only path the game uses.**

Diagnosis only, no fix landed — the term is in `lodestone-worldgen`, another unit's crate. Instrument: `crates/lodestone-server/tests/walk_distance_curve.rs` (three `#[ignore]`d arms) plus `serve_play.rs`'s `generation_is_anchored_at_the_player_not_at_the_origin`. Details and the brokered patch in `docs/worldgen-store-distance-leak.md`. Measured: a fresh generator per band, six columns per band, chunk 0 … 65,536 — warm cost **0.74–1.09×** of band 0 with **no monotone trend**, and *cheapest* at 1,048,576 blocks. One generator sliding a 17×17 view 100 chunk steps — `store_len` **441 → 2,541**, exactly **21 entries per chunk step**, **0 evictions**, RSS **208.9 → 997.2 MiB**, linear.

- **Two insertion paths, one ceiling check, and the game only ever takes the unchecked one.** `StagedStore::entry()` calls `reclaim()` when its own insert crosses `retention`; `open_view()` inserts fresh slots, bumps `total`, and checks nothing. `OverworldGenerator::column` opens the 5×5 pin **first**, and `COLUMN_CLOSURE_RADIUS = 2` *is* the pre-ore closure — so by the time `pre_ore_stage`/`post_ore_world` call `entry()`, every slot already exists, `inserted` is always false, and **`reclaim()` never runs in a real session**. Not eviction thrash, which is what reading the ceiling check predicts: no reclamation at all. The general form: **when a ceiling is enforced on one insertion path, enumerate the other paths — a pre-creating fast path upstream can make the check unreachable without touching it.**
- **The gate for exactly this bug is green, has its own live detector control, and drives the one call shape production cannot produce.** `unpinned_entries_are_reclaimable_once_the_scope_ends` asserts `len() <= retention + SHARD_COUNT`, asserts `evicted()` rose, and does 500 inserts via **`entry()`**. Production inserts via **`open_view()`**. The *world* species in its purest form — the test source is exemplary and the flaw is in the input — and the audit question that finds it is the one already on record: **which implementation does this test's transport resolve to, and is it the one production uses?** Both reclamation gates in that file share the defect, so the path is green in CI and dead in the game.
- **`store_len = 441` was reported as healthy by U6 and it was; the defect is that it never comes down.** 441 is exactly a 289-column view's 21×21 closure. A correct instantaneous reading became a wrong standing conclusion because nothing ever sampled it **twice** with the player somewhere else. **A high-water counter read once at the moment it is supposed to be high proves nothing about release** — the release path needs a second reading after the scope that justified the first is gone.
- **The order of growth was predicted before it was fitted, which is what makes it a term rather than a correlation.** A `+x` step at view radius `R` exposes `2R+1` new columns whose radius-2 pins add a strip of `2R+5`: at `R = 8`, **21**, and the measured slope is 21. Per-entry memory likewise: 5 pre-ore grids and 3 post-ore grids per 1-D step at ~192 KiB gives `8 × 192 / 5 ≈ 307` KiB against **323** measured. So the claim is **`O(d)` linear in distance travelled, not `O(d²)`** — a distinction no timing could have made, and both constants come from outside the measurement.
- **The control was flatter than the machine's documented noise floor, and that is a fact about the instrument's *shape*, not about the machine getting quieter.** Nine six-column walks at the origin: warm-mean spread **1.01×**, against the 10.8% wall-clock reproducibility and 22% single-stage swing on record. A mean of three adjacent columns inside one tight loop shares thermal and allocator state; a whole-stage timing across a process does not. **Prefer a tight-loop repeat over a cross-run comparison when the effect is per-iteration** — the same lesson §12.103 reached from the opposite direction.
- **Two independent read-only sweeps and the owner all proposed origin-anchored enumeration, and instrumentation killed it.** `ViewTracker::window` is `center ± radius` and `recenter` diffs `next.difference(&self.loaded)`; a recording `ChunkSource` driven through the real `serve_connection` **80,000 blocks** out generates exactly the player's `(2r+1)²` window, and the detector control — anchoring `window()` at `(0, 0)` — was observed failing. **The plausible mechanism named by three independent sources was still wrong**, and the cheap falsification was worth doing first.
- **The leak-to-slowdown chain is inference at its last link and is recorded as such.** 504 KiB per block walked extrapolates to ~4.8 GB at 10,000 blocks on a 17 GB machine, with `IntegratedServer` sharing the client's process, so memory pressure explains a superlinear *feel* from a linear term. Confirmed: the growth, its rate, its mechanism. **Not** confirmed: the machine entering swap and generation slowing as a consequence, which needs a multi-GB run this hardware is on record as being force-rebooted by. **A complete mechanism plus an unproven final step is worth more written down honestly than a chain closed by assertion.**

**12.109 §12.108's leak, fixed by moving one ceiling check onto the other insertion path — and the gate written to catch it reproduced a live server's whole curve digit-for-digit from a `u64` model, while the repo's standard byte-identity dump turned out to be structurally incapable of seeing the change at all.**

Closes #503. `StagedStore::open_view` now checks the retention ceiling **after the whole box is pinned**; `entry()`, `reclaim()` and `StageSlot` are untouched. Sliding a 17×17 view 100 chunk steps in `view_walk_curve`: `store_len` **2,541 → a flat 512**, evictions **0 → 2,029**, RSS **997.2 → 324.6 MiB** and flat from step 60 (+0.3 MiB over 640 blocks, against +315 MiB before) — marginal rate **504 KiB → 0.48 KiB per block walked**. All 13 parity binaries, all four eviction-is-zero gates still at zero, `just health` at 0. Details in `docs/worldgen-store-distance-leak.md`; the change rules in `docs/worldgen-staged-store.md`.

- **The ordering is the entire correctness argument, and it is the part most likely to be "tidied" later.** A reclaim pass skips pinned entries, so *after* the loop this scope's closure is ineligible by construction. **Inside** the loop it is not: at iteration *k* only *k* of the box's 25 entries carry a pin, and the unpinned remainder are typically the **oldest entries in the whole store** — the neighbouring column visited moments ago — so they sort to the *front* of the candidate list and the pass would evict exactly the slots the request is about to compute into. The general form: **when a capacity check is added to a loop that acquires protection incrementally, the check belongs after the loop, and the reason is that the loop's own subject is indistinguishable from the coldest thing in the structure until it is protected.**
- **A hermetic `u64` model and a real embedded-data server sweep agreed on all twelve numbers, which is stronger evidence of faithfulness than any argument about the model.** The new gate reports 441 / 861 / 1,281 / 1,701 / 2,121 / 2,541 with zero evictions before the fix and a flat 512 with 349 / 769 / 1,189 / 1,609 / 2,029 after — identical to `walk_distance_curve.rs`'s live measurements in `lodestone-server`. It costs **0.03 s**, so it runs on every `cargo test`, where the instrument that found the defect is `#[ignore]`d and multi-minute. **When a defect is geometric rather than computational, model the geometry and drop the payload**: the cheap gate then *is* the permanent one, and its agreement with the expensive instrument is the check that it models the right thing. The load-bearing anchor is asserting the join-view closure is exactly 441 — that number holds on both arms, so it is the part that says the model reproduces production rather than a shape too cheap to leak.
- **The repo's standard 45-column byte-identity dump cannot see this change, and it reported the expected md5 anyway.** Both arms produced `a9db7cf741214167db615fa8b9356fa8`, the figure U18, U19 and U21 all recorded — and that agreement is *vacuous here*: the scene is a fresh generator per seed over a 3×3 patch, a 49-entry closure, so it never reaches the 512-entry ceiling on **either** arm and would be identical whatever `open_view` does. The *world* species again, one level up from the bug it was being used to clear. **A byte-identity harness is only evidence about the code path its scene actually enters; inheriting a sibling's known-good hash is inheriting their scene, and a matching hash from a scene that cannot exercise your change is the most reassuring possible null result.** The replacement arm — a 140-column strip whose closure is 720 — differs across arms in exactly the right way: `store_len` 720/evictions 0 against 512/208, `720 − 512 = 208` exactly, and byte-identical output (`6d80318bab2d514416cba1dce0216f52` both).
- **The two arms of the concurrency gate reclaimed on visibly different schedules, and that is what makes it a determinism result rather than a coincidence.** A 21×21 = 441-column burst on 8 threads (closure 625, past the ceiling — deliberately wider than `staged_store_gates.rs`'s R=8 burst, which is sized to evict *nothing*) matched a serial arm on every column while the two evicted **113 and 577** entries respectively. **Ask of any determinism gate whether its two arms actually did different work**; had both evicted the same set, byte-equality would have shown only that the schedules matched. `parallel_generation_is_deterministic_and_matches_serial` also ran 12/12, each run verified by a program to have matched exactly one test (`1 passed`) rather than filtering to none — §12's own recorded vacuity mode for that test.
- **Fixing a defect inverted the polarity of the assertion that documented it.** `view_walk_curve`'s non-degeneracy check was `store_len > 512`: correct while the leak existed, and guaranteed to fail *because* the leak was gone. It is now `store_len + store_evictions > 512`, the form `age_curve` already used, which is the quantity the question "did this walk reach the retention path?" was always about. **A gate that asserts a defect's magnitude to prove it is in scope becomes a gate against the fix.** Look for them by grepping the fixed quantity, not the feature: `store_len` had two callers with opposite intent and no `cargo check` could tell them apart.
- **The one place a fix like this could have changed output is a strong-count branch, and it is worth naming even though it is unreachable.** `decorate.rs` does `Arc::try_unwrap(world).unwrap_or_else(|shared| (*shared).clone())`, whose branch depends on whether the store still holds the value — precisely the thing reclamation changes. It is safe on two independent grounds (the centre is pinned for the whole call, so the count is never 1; and both branches yield identical content), but **"eviction only costs a recompute" is a purity claim, so the audit is to grep for the places where uniqueness is *observable*** — `Arc::make_mut`, `try_unwrap`, `strong_count` — rather than to re-assert the claim.
- **Reclamation added one `Relaxed` atomic load and no lock, which the store's "no shared pool here, ever" rule made a hard requirement** (`4307b59` is the scar: 289 concurrent columns on one `Arc<Mutex>`). The load sits on the insert path behind `fresh_inserts > 0`, so a steady-state view re-opening a resident neighbourhood does not perform it, and an **entry hit is still a `OnceLock` load and no lock at all**. Cost bounded by a counter rather than a timing: 1,989 column visits and 2,029 evictions in 0.03 s release is ~15 µs per column, **0.03%** of a real column. A two-arm wall clock was available and declined — §12.103 records one changing sign with arm order here, and §12.104 records unchanged code moving ×1.8–2.3 between captures.
- **The leak → pressure → slowdown chain is still open, and closing the leak did not close it.** Demonstrating the last link needs the leaking arm carried to multiple GB, which `CLAUDE.md` records force-rebooting this machine; a cheaper wall-clock substitute at ~1 GB, where a 17 GB machine is under no pressure, could only yield a number later attributed to the wrong cause. What *did* change is that the chain's premise is gone: the `O(d)` term no longer exists whether or not pressure was the mechanism the owner felt. **A fix does not need its symptom chain closed when it removes the term the chain starts from** — and saying so is cheaper than an experiment that would prove the machine can be crashed.

**12.110 §12.108's best follow-up lead was a chain of three true facts with a false joint: the 20 Hz block-entity scan really does regenerate whole columns on the tick thread, at 610 per tick past a threshold — and it is flat in distance, because polling a column at 20 Hz does not pin it, which is the prediction this unit got wrong and measured.**

Filed as #504, not fixed — the fix is a gameplay-semantics call (vanilla ticks block entities per *loaded chunk*). Five arms at the end of `chunk_store.rs`'s test module; write-up in `docs/block-entity-tick-distance.md`. The lead read: registry never unloads → scanned at 20 Hz → each hopper probes `world.block_state` → *walking away ages the column out of the LRU* → a ~50 ms cold column every tick. Facts 1–3 hold. Fact 4 does not.

- **Three true facts and a false joint is the hardest shape of stale claim to audit, because every link survives inspection individually.** Each of the first three was verified and each is worth keeping: `remove` exists but is wired only to block *breaking*, so there is genuinely no *chunk*-unload path and the probed set only grows; the scan is genuinely unfiltered; `ChunkStore::block_state` genuinely regenerates on a miss, in the concrete implementation and not a stale trait default. **The defect was in the *conjunction*, and no amount of re-verifying the individual claims would have found it** — only asking "what would have to be true for these three to compose?" and then measuring that. **When a lead is a chain, measure the joint, not the links.**
- **A wrong prediction, kept, and it is the most valuable line in the unit.** The over-capacity arm was written predicting **1** on this argument: `ChunkStore::read` refreshes `last_used` on every hit and `ensure` inserts with the newest stamp, so a position polled at 20 Hz is permanently most-recently-used, and `evict_down_to` takes the *minimum* — so the scan should **pin** what it probes. It measured **12**. The argument omits that the scan runs *once* per tick while the random-tick pass touches **49** columns immediately *after* it, so by the end of a pass the polled column's stamp is the **oldest** in the map. **Frequency of access does not confer LRU residency; it is *recency relative to everything else touched in the interval* that does, and a 50 ms interval is long enough for another loop stage to lap you 49 times.** The general form: **an argument about an LRU that reasons only about the subject's own access rate is incomplete until it counts the competing touches per interval.**
- **The subject's flatness came from headroom, not from any property of polling — and only a deliberately over-capacity arm could tell those apart.** At the default render distance the working set is `289 view + 49 tick area = 338` against a 512 ceiling, so `evicted() == 0` is *guaranteed* and reads as though the mechanism were safe. It is not safe; it is merely unexercised. **"No eviction observed" is a statement about the ceiling's headroom, never about eviction policy**, and a gate that stops there would have blessed a cliff that a user raising render distance to 11 chunks (`23 × 23 = 529 > 512`) walks straight off. **Ask of any cache result: did this configuration reach the bound at all?**
- **The negative control landing on the lead's *own* predicted number is what converted "1" from an absence into a measurement.** `with_capacity(source, 0)` produced exactly **52** cold columns over 52 ticks — one per tick, the lead's own prediction — as a real *configuration* of the shipped type rather than a neuter. Without it the subject is the *assertion* species: an unscanned registry, an unticked hopper and an uncalled closure all also report a low number, and all three read as a pass. **Build the control that reproduces the reported bug before believing the arm that says it is absent**, and prefer a control that is a supported configuration so it cannot rot.
- **The two regimes were predicted from constants and the boundary is a known worst case, so the balloon is a term rather than a surprise.** Below the ceiling every column is generated once for the session (`hoppers`); above it the access pattern is a **cyclic** scan of N positions through an LRU of capacity `C < N`, which is LRU's textbook worst case — by the time the scan returns to a position it has touched every other one, so *every* probe misses (`hoppers × ticks`). Predicted `600 × 52 = 31,200`; measured **31,739**, the 1.7% excess being the tick area churning under the same pressure. **Naming the access pattern (cyclic-over-capacity) predicted a 52× step from outside the measurement; "the cache will thrash" would have predicted only a direction.**
- **The counters were byte-identical across three release runs *and* one debug run in an isolated worktree at the committed sha — 400 and 31,739 every time.** Profile-independence is not a bonus here, it is the argument: a *count* cannot move with optimisation level, so the debug run is a valid re-verification of the release measurement, and the isolated worktree proved the arms do not depend on the sibling's concurrently-landing store fix. **A counter buys you cheap re-verification in configurations a timing would make meaningless** — on a machine on record at 10.8% wall-clock reproducibility, that is the difference between one measurement and four.
- **`INITIAL_RANDOM_TICK_DEFERRAL_TICKS`'s doc claim was wrong in a *second* way, and the file's own gate was clean only because of what it passed in.** The claim that the random-tick pass is "the only thing in this loop that touches `world.column()`" was already known stale via `block_ticks.drain_due`; the block-entity scan is the second counter-example, from tick 1, at a call site *above* the deferred section — evidenced by the control generating on all 52 ticks including the 40 the deferral covers. `chunk_store.rs` repeated the claim in its own `RANDOM_TICK_PASSES` comment, and it was true **only because `drive_tick_loop` passed an empty `BlockEntityHandle`**. The *world* species applied to a comment rather than a test: **a scoping claim inherits the scope of the harness that made it look true, so a comment asserting "X is the only caller" should name the fixture that makes it so.** The deferral is startup smoothing, not a bound on tick-thread generation — it defers one of three callers.
- **The unfiltered scan is real and the 1,608 unmodelled kinds are inert, which are two findings and not one.** `tick_all_with_hopper_lock` collects **every** key at 20 Hz with no filter, so #477's whole population is walked — but only `BlockEntity::Hopper` reaches the world, so 1,608 `Opaque` entries over 1,608 distinct far-flung columns still cost the store nothing (competing hypothesis `49 + 1608`). **"Is it filtered?" and "does the unfiltered remainder cost anything?" have opposite answers here**, and reporting only the first would have overstated the defect by three orders of magnitude. The threshold is in *distinct hopper-bearing chunks* — 174 in production — not in block entities.
- **Two of the five arms deliberately characterise a defect that is not being fixed, and they are labelled as such in the source.** A future fix turns them red *by design*. **An arm that pins current-but-wrong behaviour is only safe if it says so where the failure message will be read**, otherwise the next unit relaxes it and the regression is silent — the mirror image of §12.109's `store_len > 512`, where a gate written to document a defect's magnitude became a gate against its fix.

**12.111 §12.108's cliff, fixed by deriving the store's capacity from the radius it serves — and the issue's headline mechanism was wrong in a way that made the defect *worse* than described: the view is diffed rather than rescanned, so LRU's worst case never applies to it, and what falls out of a too-small store is measurably the **centre** of the view, not its edge.**

Issue #505. `ChunkStore::DEFAULT_CAPACITY` was a bare literal `512` chosen to cover "a typical streamed view", in a different file from the `render_distance` it was chosen for. The shell serves `view_radius = render_distance + 1`, so the streamed square is `(2·(rd+1)+1)²` — 361 at our default 8, **529 at 10**, **729 at vanilla's own default 12**, 4,489 at the slider's maximum of 32. Replaced by `capacity_for_view_radius`: `view_columns(r) + 50`, floored at 512 and capped at 1,275. Gate: `crates/lodestone-server/tests/view_radius_store_capacity.rs`; write-up in `docs/chunk-store.md`.

- **The issue predicted LRU's cyclic-over-capacity worst case on the view, and that term does not exist — `ViewTracker::recenter` *diffs* the window.** It computes `next.difference(&self.loaded)` and generates only what newly entered, so the view is streamed once and incrementally extended, never rescanned. §12.110's 31,739-generation collapse was a cyclic scan of the *block-entity registry*, and inheriting that shape for the view is a wrong-axis reuse of a real measurement — the same failure mode as the coverage figure five issue bodies copied. **A measured worst case belongs to the access pattern that produced it, not to the cache it was measured on.** Restating it for a second consumer of the same cache needs the second consumer's access pattern established first.
- **The real cost is narrower and lands in the worst possible place, and the *order* of the join is what decides that.** `join_view_rings` streams outward from the player's own column, so stamp order is ring order and `evict_down_to`'s minimum is the **innermost** ring. At `render_distance` 10 the old literal dropped 17 columns, and the *measurement* that locates them is the blind spot in the next bullet: with the probe set re-requesting only rings 4 and outward, the unfixed arm reported **zero** regenerations, which is possible only if all 17 lay inside radius 3. Ring order then places them exactly — ring 0 (1 column) plus ring 1 (8) plus 8 of ring 2 — i.e. the player's own column and its two nearest rings, which is what `vitals_tick` probes every 50 ms and what `run_tick_loop`'s random-tick pass covers. **The band's location was established by an arm that failed to see it, not by an arm aimed at it**, which is the only reason it was found at all. **"The cache is too small for the view" says nothing about *which* columns you lose, and the enumeration order of the fill decides it.** Intuition said the outer edge; the outer edge is the freshest thing in the map.
- **The gate's first probe set had a blind spot, it failed in the safe direction, and it was chosen for a *good-looking* reason.** The rig shrinks the render distance and grows it back, so only columns that left the window are re-requested — which makes the shrink radius the gate's sensitivity knob. The first draft used 3, deliberately aligned with `CONCURRENT_TICK_RADIUS` (the 49-column tick area). The `render_distance` 10 row then reported **0 regenerations on the unfixed code** for a store that was genuinely 17 columns short, because rings 0–2 never left a radius-3 window. Dropping to 0 made that row report 92. **When a gate measures "what was evicted", the probe set *is* the instrument's aperture, and aligning it with a production constant is not the same as aligning it with the band under test.** The audit question: what does my probe set structurally fail to touch?
- **The subject radius had to be chosen against the *old ceiling*, not against our own default — and that choice is now a test rather than a comment.** 361 < 512, so an arm at `render_distance` 8 passes before *and* after the fix: the *world* species, invisible in the source. The whole curve, unfixed against fixed, at 512 vs the derivation: rd 8 → 0 / 0; rd 10 → **92** / 0; rd 12 → **451** / 0; rd 19 → 1,595 / 1,077 (past the new cap, by design). `the_default_render_distance_is_under_the_old_ceiling_on_both_arms` asserts the premise so it cannot quietly go stale.
- **The unfixed figures move a few percent between runs and the arithmetic floors do not, so the subject asserts 0 and the control asserts a computed floor — neither asserts an observed number.** `generate_columns_offloaded` fans the re-grow over the blocking pool, so scheduling decides which entry a given miss evicts. This is §12.110's counter-stability result with the opposite sign: a count is *not* automatically reproducible when the thing being counted is a race against an eviction policy. **Reproducibility is a property of the measurement, not of the units it is expressed in.** What is stable is `view_columns(r) − capacity`, which is arithmetic.
- **`#[cfg(test)]` on `ChunkStore::new` is the fix expressed as a type signature.** Every production caller is in `integrated.rs` and every one already had a `view_radius` in scope — the defect was reachable only because a constructor existed that did not ask for one. Removing it from the non-test build means a new call site has to name a capacity or a radius; it cannot reintroduce the literal by accident. **A defect that is "the wrong default was easy to reach" is often better fixed in the API surface than in the default.**
- **The cap's memory cost was measured rather than extrapolated, and the hypothesis that made that worth doing was cheap to test and would have failed expensively.** 512 retained measures 194.8 KiB per column and 1,275 measures 194.4 — flat across 2.5×, so the interpolated rows of the table are sound. But a `HashMap` growing through several rehash thresholds with 192 KiB values in it is a plausible superlinear shape, and the arm cost one `#[ignore]`d test and one `/usr/bin/time -l` run. Peak RSS at the cap is **250.1 MiB** against an 8.1 MiB unretained control, i.e. a 242.0 MiB delta; uncapped, `render_distance` 32 would be **863 MiB** of chunk cache in a process that also holds meshes and a GPU allocator. **The cap is the answer to "is an unbounded derivation a fix?", and it is no** — but a cap with a *measured, documented, gated* degradation band is better than either extreme, and the control that proves the band exists is a supported configuration rather than a neuter.
- **The `289` in this file was wrong twice, in a file whose module doc said `361` three hundred lines earlier, and it survived because it was conservative.** 289 is `(2·8+1)²` — the view for a *view radius* of 8, where 8 is the `render_distance`. Every conclusion drawn from it held with room to spare at either value, which is precisely why nothing looked wrong on inspection. §12.110's derived "174 distinct hopper-bearing chunks in production" inherited it and is **also** wrong in mechanism, not only arithmetic: the view is touched once per column while the registry is scanned every 50 ms, so the view never competes with the 20 Hz set for residency and does not reduce that threshold at all. **A number that appears twice in one file with two different values is a self-contained contradiction that no external source is needed to catch — grep your own file for the quantity before deriving anything new from it.**

**12.112 The first C_ss re-measurement since §12.98 — 97.8 ms → 15.1 ms, a 6.5× improvement that is still 15× over the goal — and it was blocked for hours by a link failure whose obvious diagnosis was wrong, whose plausible fix worked for the wrong reason, and which a two-line control killed.**

21 units of `docs/plans/worldgen-rewrite.md` landed against **counters**, deliberately: this bench's own vegetation stage swung 52.28–63.77 ms across three runs of an identical binary while its allocation counter read 905,459 to the digit, 3 of 3 (§12.98). The consequence nobody stated at the time is that **the headline figure went unmeasured for the entire drive.** Measured at `1798ca3b`, release, `lto = "thin"`, embedded server data, in an isolated worktree with its own target dir:

| | §12.98 baseline | now | target |
|---|---|---|---|
| C_ss (median of 100 interior) | 97.8 ms | **15.14 ms** | ≤ 1.0 ms (goal) |
| C_ss p95 | — | 18.09 ms | — |
| C_cold (first column, fresh) | 852 ms | **267.2 ms** | ≤ 8 ms |
| steady-state heap allocs/column | 905,459 | **64** | 0 + O(1) |

- **Allocation rank and time rank are different orders, and optimising by the first would have chased the wrong stage.** U18 binned allocations by innermost stage and measured **surface at 92.46%, ore at 4.99%** — an 18× gap that set the priority for three subsequent units. By *time*, with all ten stages verified live, the order inverts: **ore 6,566 µs against surface 718 µs**, ore being 9× surface in time while holding a fraction of its allocations. Both measurements are correct and they rank the same ten stages almost oppositely. **An allocation counter is a proxy for time only while allocation is the dominant term; once it is driven to 64 per column it has stopped measuring the thing you care about, and it does not announce that it has stopped.** The counters that guided 21 units are now the wrong instrument for the next one.
- **A fix that works is not a diagnosis.** `cargo bench` failed to link with `could not materialize bitcode object file …liballoca…`; `alloca` is a genuinely non-optional dependency of `criterion 0.8.2`, which made a dependency story credible, and an issue (#528) was filed naming it. `MACOSX_DEPLOYMENT_TARGET=26.0` then made the bench link, exit 0 — apparent confirmation. **Two variables had changed at once**: the env var, and a fresh `CARGO_TARGET_DIR` in a throwaway worktree. The control — fresh target dir, **no** env var — also exited 0, proving the env var irrelevant. The real cause was a poisoned `alloca` build-script output in the shared `target/`, and the fix is `cargo clean -p alloca`, which CLAUDE.md already prescribes for exactly this symptom. Without the control this file would carry a permanent, plausible, wrong workspace-wide deployment-target setting. **When a fix works, ask which of the things you changed did it; "it links now" is compatible with every hypothesis you did not test.**
- **The harness refused to let a counters build contaminate the record, and that guard is why the per-stage split above is quotable at all.** Running `--features gen-counters` printed `REFUSING to record "embedded_stage_ore_us" … inflates a burst by roughly 3×, so the number is not comparable to a clean run` for all eight stages. The same run is what verified the split is not vacuous — its header reads *all ten stages live*, the check the counters-off run explicitly disclaims (*"counters NOT compiled in, so the exactly-once invariant was NOT checked … precisely the condition that let this file measure an ore-free pipeline for as long as it did"*). **Stage participation and stage timing cannot come from the same run, so a trustworthy per-stage number needs two runs and a guard that stops you conflating them.**
- **Two independent instruments agree, which is the only reason the absolute figures are worth stating on a machine whose wall clock reproduces to 10.8%.** This bench projects RD 8 at ~6–7 s from its rd=2/rd=4 arms (24,295 and 21,524 µs/chunk); the mesh-fill harness (`docs/mesh-fill-rate.md`), a different process measuring delivered columns over wall time through the real server, measured the 361-column view filling in **6.3–6.9 s**. Neither was calibrated against the other. **Agreement between two instruments built for different questions is worth more than three runs of either.**
