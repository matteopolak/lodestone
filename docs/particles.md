# Particle rendering

## What it is

The particle system: how a decoded particle type becomes a physically-simulated, textured billboard on
screen, and the special case of block-break debris, whose colour and texture are derived from the broken
block itself rather than from a dedicated sprite.

## How it works

### The catalogue shape

`lodestone-particle`'s `Sheet` enum names a physical texture sheet under `textures/particle/*.png` (its
identity is the sheet's own **frame sequence**, not just its pixels — two sheets can share all eight
textures and differ only in playback order, e.g. ascending vs descending); `Behaviour` names a per-type
tick/quad-size/layer override shared across every vanilla particle *class* it corresponds to (several
registry types share one Java class and therefore one `Behaviour`); `emit` holds one function per class,
transcribed from vanilla's own client-side particle package. `Particles::spawn_one`
(`crates/lodestone-shell/src/particles.rs`) is the single place that maps a decoded registry name to an
emitter call.

The wire path is: a clientbound level-particles packet decode → `ClientEvent::Particles` →
`NetUpdate::Particles` in `sim.rs` → `Particles::spawn_particles` → `spawn_one`'s dispatch. Any type
wired into that dispatch renders correctly for **any** producer of that packet (a `/particle` command, a
datapack, a plugin), independent of whether this codebase also predicts that type's usual in-game trigger
locally.

Some particle-carrying types have **no** network path at all — vanilla's own generic add-particle call is
a no-op
on the server and is only ever real on the client, so gameplay code that calls it directly (breeding
hearts, note-block chimes, totem-of-undying flashes) needs its own client-side trigger (a block-action
replay, a per-entity synced-state predictor), not a packet decoder. Wiring the generic dispatch arm is
necessary but not sufficient for those types.

Several registry types carry a real payload beyond position/velocity (a colour, a block state, a power
scalar) via vanilla's `ParticleOptions` codecs. `decode_particle_options`
(`crates/versions/26.2/src/adapter/chunk.rs`) is the shared decoder for these, matched on the
**fully-namespaced** registry name (`"minecraft:dust"`, not `"dust"` — matching the stripped path
silently decodes nothing). A type with no payload resolves to `ParticleOptions::None`, which is correct
for the large majority of registry entries (a bare `SimpleParticleType`), not a placeholder.

### Break particles

Terrain (block-break) debris is a small camera-facing billboard textured from a random quarter of the
broken block's `#particle` sprite (its baked model's particle-icon reference — not necessarily any
face's own texture: `grass_block` declares `#particle` as `block/dirt`), tinted by a per-block-state
colour, and shaded by the light at its cell. Three layers own the parts: `lodestone-particle` emits an
opaque `SpriteSource::BlockState(StateId)` with no atlas and no tint opinion; `lodestone-render`'s
`block_models.rs` bakes each state's particle UV rect and tint once from the jar; `lodestone-shell`'s
`particles.rs` joins the two and builds GPU instances.

The shell is the generated-state ingress for both decoded block-particle options and local break effects.
The version-free `lodestone_model::BlockStateRef` tags a 26.2 global id as `Canonical`, while a legacy
family or synchronized extension keeps its numeric value as `ProtocolLocal`; an overlapping small number
must not accidentally become a 26.2 state just because it fits this build's table. The same tag reaches
the destroy-burst route through `LevelEventData::BlockState`: adapters classify level-event `2001` before
the generic `ClientEvent::LevelEvent` loses its protocol context, `net.rs` forwards it as
`NetUpdate::BlockDestroyed`, and `sim/net_apply.rs` hands it to `Particles::destroy_block` unchanged.
The particle seam validates only `Canonical` ids into `lodestone_data::block_states::StateId`; particle
emitters and `SpriteSource::BlockState` retain that proof, and the shell lowers it to a raw atlas index
only at the final indexed lookup. An out-of-census canonical value drops there; a protocol-local/custom
value is not rendered by this built-in resolver rather than being coerced into a built-in state. A version
or dynamic-registry renderer can dispatch on its `BlockStateRef` source without reconstructing intent
from the raw number.

Item crumbs follow the same ownership rule. `SpriteSource::Item` carries the generated
`lodestone_data::item::Item` enum, not a numeric registry value. The only two producers are a local
consumable after resolving its item name and three fixed built-in particle types; both validate before
emitting. The shell lowers the enum to `Item::registry_id()` only when indexing the baked item-UV table.
A custom or dynamically registered item has no entry in that built-in model census, so it remains at its
import/registry boundary rather than being made into a misleading particle.

The ambient world probe has the same boundary before it chooses a block-specific animation: it converts
the returned numeric state to `StateId`, then dispatches on the typed `Block` and reads properties only
through that validated state. An unknown or custom probe result produces no particle; a built-in block
added in a future registry update cannot accidentally match a misspelled name.

The tint is **not** the same lookup as a block's face tint — vanilla's `TerrainParticle` constructor
calls a separate virtual method (`colorAsTerrainParticle`), and a couple of blocks deliberately disagree
with their own face tint (`grass_block`'s particle samples untinted dirt, since its `#particle` texture
already *is* dirt; still/flowing water tints its particle by biome colour even though its face tint does
not). Everything else inherits the face tint. Getting this backwards (deriving particle tint from a
quad's own `tint_index`) breaks exactly the cases that matter.

The debris pass draws in two passes matching vanilla's opaque/translucent split (`Layer::Opaque` before
the water pass with depth writes on, `Layer::Translucent` after it with depth writes off) — this is what
keeps underwater debris from painting over the water surface, and the depth **write**, not merely the
draw order, is the mechanism: water tests depth without writing it, so only a particle already in the
depth buffer can be occluded correctly.

Two colour-space gotchas recur here as everywhere else in this renderer: the light term must be applied
in gamma space (a linear multiply washes an unlit particle out toward full brightness), and a particle
sheet (flame, smoke, crits) is a **separate** texture stitch from the block-model atlas — binding the
wrong one's bind group still resolves every UV (nothing reports "unresolved"), it just samples the wrong
image, so a resolved-UV counter alone cannot prove the right atlas was ever bound.

## How to change it

**Adding a new particle type end to end:**

1. Find its vanilla class under vanilla's own client-side particle package and check its per-type
   registration table for
   which class actually renders it — several registry names share one class.
2. Read that type's own `assets/minecraft/particles/<name>.json` for which physical sheet it samples.
   **Never assume the sheet stem matches the registry name** (`witch` and `instant_effect` both sample
   `spell_N`, not their own name) and **read the frame list's actual order out of the jar** — about half
   of vanilla's multi-frame sheets are listed descending, and a wrong order still resolves a real sprite,
   so no test catches it structurally; only a jar-backed atlas gate does.
3. Add a `Sheet`/`Behaviour` variant only if the tick shape or sheet sequence is genuinely new — read the
   class's `tick()`/`getQuadSize()`/`getLightCoords()` overrides before reaching for an existing
   `Behaviour` (does it fit?) or before adding a new one (does an existing one already cover this shape?).
4. If the type carries an options payload, add an arm to `decode_particle_options` plus, if the emitter
   needs the payload's own fields, a new `ParticleOptions` variant threaded through to `spawn_one`.
5. Add the dispatch arm in `spawn_one`.

**The recurring transposition trap**: a vanilla subclass that overrides exactly one constant from its
parent (one differing gravity, one differing lifetime formula, one differing sign) is otherwise
byte-for-byte identical in sheet, layer, count and behaviour to a sibling you may have already ported —
so copying an existing emitter for the "same-looking" new type silently carries over the wrong constant.
When porting a family of near-identical vanilla particle classes, diff the two Java class bodies for the
one differing number rather than reusing your own prior port wholesale; a gate that predicts both the
correct and the swapped-constant hypothesis and requires the measurement to land on one is the only thing
that reliably catches this, since the particle count, sheet and physics all look right either way.

**Debris tint or texture is wrong**: check `vanilla_particle_tint_kind`
(`crates/lodestone-assets/src/tint.rs`) against vanilla's tint-source table, and whether the block
overrides `colorAsTerrainParticle` specifically (not just its face tint). **Debris draws nothing**: check
the particle frame's own unresolved-sprite counter before anything else — an unresolved sprite is silent
in pixels but loud in that counter; if it's zero and particles are still missing, compare the submitted
vs. uploaded instance counts, since a silently-dropped draw call (e.g. one that slipped back inside a
gate meant for an unrelated renderer) reports a healthy uploaded count with nothing actually submitted.

## Configuration

No runtime flags. Every particle sheet, per-state particle UV, and per-state tint is baked from the
loaded resource pack at startup; without a real pack the demo/packed path uses an untinted synthetic
palette (correct, since the demo palette has no colormaps or tinted blocks).

## Dependencies

* `lodestone-particle` — `ParticleEngine`, `Sheet`, `Behaviour`, `emit`, `SpriteSource`.
* `lodestone-assets` — `bake::BakedModel::particle_uv`, `tint::vanilla_particle_tint_kind`, the stitched
  particle atlas.
* `lodestone-render` — `block_models.rs`'s per-state particle UV/tint tables, shared with the ordinary
  block mesher.
* `crates/versions/26.2` — `decode_particle_options`, the `LEVEL_PARTICLES` and block-destroy (`2001`)
  decodes that feed both the generic dispatch and the break-particle path.
* `lodestone-shell` — `particles.rs` (dispatch, tint join, GPU instances) and `interact.rs`/`sim.rs` (the
  break-particle emit sites, including local prediction for the player's own break).
