# Particle rendering

## What it is

How a decoded particle type becomes a simulated, textured billboard on screen, including block-break debris whose colour and texture derive from the broken block. The shell facade delegates to `particles/events.rs` (event and ambient emission), `particles/lifecycle.rs` (simulation and extraction) and `particles/render.rs` (GPU uploads and draws).

## How it works

### Catalogue

`lodestone-particle`'s `Sheet` names a texture sheet under `textures/particle/*.png`; its identity is the frame sequence, not just pixels (two sheets can share eight textures and differ in playback order). `Behaviour` is a per-type tick, quad-size and layer override shared by every registry type of one reference class. `emit` has one function per class, grouped under `crates/lodestone-particle/src/emit/` by effect family (block and item fragments, combat, social/UI, ambient, magic, water, foliage, fire, special), behind a small `emit.rs` re-export facade. `Particles::spawn_one` (`particles/events.rs`) is the single place mapping a registry name to an emitter call.

Wire path: level-particles decode, `ClientEvent::Particles`, `NetUpdate::Particles` in `sim.rs`, `Particles::spawn_particles`, `spawn_one`. Any type wired into that dispatch renders for any producer (a `/particle` command, datapack, plugin), whether or not the usual in-game trigger is predicted locally. Types the reference only adds client-side (breeding hearts, note chimes, totem flashes) have no packet and need their own trigger (block-action replay, entity synced-state predictor); the dispatch arm alone is not enough.

`ClientEvent::Particles` carries one canonical velocity-scale vector and a `ParticleDistribution`. Scalar-speed protocols supply `[speed; 3]` and `Default`; protocol 777 keeps independent speeds and its distribution. A zero count emits one particle at the exact origin with `offset * speed`, no noise. For positive counts, `Default` draws three standard-normal position offsets then three standard-normal velocities; `Alternative` draws three uniform position offsets and keeps the supplied velocity; `AlternativeWithSpeed` draws three more uniforms to scale it. Uniform offsets are `draw * offset` (not centred or doubled, so negative offsets fall in the negative half). Emitters then add their own randomness. The sampler lives in the shared shell event module for native and browser; its tests use supplied draws and unequal signed axis scales to distinguish draw order, scalar collapse, centring, additive speed noise and position-draw reuse.

The three poplar leaf types share the falling-leaf emitter (fall acceleration `0.07`, side `10.0`, swirl on, flow-away off, size scale `2.0`, initial downward speed `0.021`) and ignore packet velocity; each colour picks one of four frames in definition order (`red_poplar_1` to `_4`, and orange, yellow) from separate sheets with no wire colour. `Sheet::all` includes them, so atlas stitching handles them.

Types with payload (colour, block state, power) decode via `decode_particle_options` (`crates/versions/26.2/src/adapter/chunk.rs`), matched on the fully namespaced name (`"minecraft:dust"`; the stripped path silently decodes nothing). A bare type resolves to `ParticleOptions::None`, which is correct, not a placeholder.

### Break particles

Debris is a camera-facing billboard textured from a random quarter of the block's `#particle` sprite (not necessarily a face texture: `grass_block` declares `block/dirt`), tinted per block state and lit at its cell. `lodestone-particle` emits an opaque `SpriteSource::BlockState(StateId)`; `lodestone-render`'s `block_models.rs` bakes each state's particle UV rect and tint once; `particles/lifecycle.rs` joins the tables; `particles/render.rs` draws.

The shell event module is the generated-state ingress for decoded block-particle options and local break effects. `lodestone_model::BlockStateRef` tags a 26.2 global id `Canonical` and keeps a legacy family's or synchronised extension's number `ProtocolLocal` (a small overlapping number must not become a 26.2 state). The tag travels to the destroy burst through `LevelEventData::BlockState`: adapters classify level event `2001` before the generic `ClientEvent::LevelEvent` loses protocol context, `net.rs` forwards `NetUpdate::BlockDestroyed`, and `sim/net_apply.rs` calls `Particles::destroy_block`. Only `Canonical` ids validate into `lodestone_data::block_states::StateId`, which emitters and `SpriteSource::BlockState` keep until the final atlas index. Out-of-census values drop; protocol-local values are not rendered by this built-in resolver, not coerced.

Local destroy bursts resolve the state through `lodestone_data::outline_shapes` and pass each outline box to the emitter; the per-hit mining chip uses the union bounds (an empty outline gives no chip). Collision geometry is wrong here because it is empty for many targetable plants. Packet-driven block particles keep their packet position.

`SpriteSource::Item` carries the generated `lodestone_data::item::Item` enum. Its producers (a local consumable and three fixed built-in types) validate first, and the shell lowers to `Item::registry_id()` only when indexing the baked item-UV table; custom items stay at their registry boundary. The ambient world probe converts the numeric state to `StateId`, dispatches on the typed `Block` and reads properties through that state; unknown results produce no particle.

Tint is not the face tint: the reference uses a separate lookup, and a few blocks disagree (`grass_block`'s particle samples untinted dirt; water tints by biome though its face does not). Everything else inherits the face tint, and deriving it from a quad's `tint_index` breaks exactly those cases. Debris draws in two passes (`Layer::Opaque` before the water pass with depth writes, `Layer::Translucent` after without); the depth write, not draw order, keeps underwater debris from painting over water, since water tests depth without writing it.

Gamma-space light (a linear multiply washes unlit particles to full brightness) and the atlas: particle sheets (flame, smoke, crits) are a separate stitch from the block-model atlas, and binding the wrong bind group still resolves every UV, so a resolved-UV counter cannot prove the right atlas was bound.

## How to change it

**New particle type:**

1. Find its reference class and per-type registration table; several names share a class.
2. Read `assets/minecraft/particles/<name>.json` for the sheet. The sheet stem may differ from the name (`witch` and `instant_effect` sample `spell_N`), and read the frame order from the jar (about half of multi-frame sheets run descending; a wrong order still resolves a real sprite, only a jar-backed atlas gate catches it).
3. Add a `Sheet`/`Behaviour` variant only for a genuinely new tick shape or sequence; read the class's tick, quad-size and light overrides first.
4. With an options payload, add an arm to `decode_particle_options` and, if the emitter needs its fields, a `ParticleOptions` variant to `spawn_one`.
5. Add the `spawn_one` arm.

**Transposition trap:** a subclass overriding one constant of its parent (a gravity, lifetime formula or sign) matches a sibling in sheet, layer, count and behaviour, so copying your own port carries the wrong constant. Diff the two class bodies for the differing number; only a gate predicting both the correct and swapped hypothesis catches it.

**Debug debris:** wrong position or count, check `outline_shapes` (not collision boxes). Wrong tint or texture, check `vanilla_particle_tint_kind` (`crates/lodestone-assets/src/tint.rs`) against the reference tint table and whether the block overrides its particle colour, not just face tint. Nothing drawn, check the frame's unresolved-sprite counter first (silent in pixels, loud there); if zero, compare submitted and uploaded instance counts, since a draw slipped inside a gate for another renderer reports a healthy upload with nothing submitted.

## Configuration

No runtime flags. Sheets, per-state particle UVs and tints are baked from the loaded resource pack at startup; without a pack the demo path uses an untinted synthetic palette.

## Dependencies

- `lodestone-particle` (`ParticleEngine`, `Sheet`, `Behaviour`, `emit`, `SpriteSource`).
- `lodestone-assets` (`bake::BakedModel::particle_uv`, `tint::vanilla_particle_tint_kind`, particle atlas) and `lodestone-render` (`block_models.rs` tables shared with the block mesher).
- `crates/versions/26.2` (`decode_particle_options`, `LEVEL_PARTICLES` and `2001` decodes).
- `lodestone-shell` (`particles/{events,lifecycle,render}.rs`, plus the break-particle emit sites in `interact.rs` and `sim.rs`).
