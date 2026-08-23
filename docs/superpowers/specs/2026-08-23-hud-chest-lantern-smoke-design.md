# Design: HUD scene chest, lantern light, and campfire smoke

## What it is

This change fixes three visible defects in `docs/images/05-hud.png`: two orphaned
double-chest halves, lanterns sitting in sky holes without a readable pool of
block light, and a lit campfire with no smoke plume. The chest and lantern defects
belong to the screenshot scene; the smoke defect is a production client lifecycle
bug exposed by that scene.

## How it works

### Double chest

The scene places a south-facing chest at `x=-1` with `type=left` and its neighbour
at `x=0` with `type=right`. For a south-facing chest those connection directions
point outward, so neither half finds the other and each renders its double-chest
seam face. Swap the two `type` values while keeping their positions and facing:
`right` at `x=-1`, `left` at `x=0`.

### Lantern alcoves

The back wall is one block thick at `z=22`, and each lantern replaces one of those
wall blocks. Because the lantern model is mostly transparent, the screenshot sees
the blue sky through a square hole around it. Make the wall two blocks deep behind
the two lantern positions and add a small roof/side enclosure around each recess.
The lantern remains at the front coordinate and the added stone supplies a real
background while reducing skylight inside the recess. Vanilla's authoritative
light update then leaves the lantern's level-15 block light as the visible local
source, proving more than a texture-only correction.

The rest of the stage stays in late-morning daylight. Only the two alcoves become
shaded, so the HUD, entities, banners, chest, and hand retain the existing scene's
composition and colour balance.

### Campfire smoke

Minecraft 26.2 does not create the main campfire plume from the random nearby
block `animateTick` scan. A lit campfire installs
`CampfireBlockEntity::particleTick`; each client tick it rolls an 11% chance and,
on success, calls `CampfireBlock::makeParticles` two or three times. Signal-fire
state selects cosy versus signal smoke.

Lodestone currently puts campfire smoke in `Particles::ambient_tick`, whose
random block probe is capped at +/-8. The HUD campfire is 14 blocks from the
player, so it cannot be selected even though its chunk and block entity are
loaded. Widening the scan would still be probabilistic for the wrong reason and
would increase probes for every ambient block.

Instead, gather loaded lit campfire block entities once per fixed 20 Hz particle
tick, alongside the existing campfire block-entity gather. Pass their position and
`signal_fire` flag into a dedicated particle method that mirrors the 26.2 11%
roll, two-or-three burst, and `makeParticles` position distribution. Remove the
campfire arm from the ambient block scan so a nearby fire cannot emit twice.
Particle simulation remains fixed-rate and independent of rendered frame rate.

## Data flow

```text
loaded chunk block entities
  -> campfire state lookup (`lit`, `signal_fire`)
  -> fixed 20 Hz `Sim::tick_particles`
  -> dedicated campfire particle tick
  -> cosy/signal smoke particles
  -> normal particle extraction and GPU pass
```

The screenshot scene still advances the normal simulation by its deterministic
60 ticks. It does not inject `/particle`; the regenerated PNG therefore verifies
the production producer rather than staging a cosmetic plume.

## Testing

- Add a fixture regression that parses `scripts/screenshot-scenes/05-hud.txt`
  and asserts the south-facing chest halves point toward one another and each
  lantern position has opaque stone backing plus an overhead shade.
- Add a block-entity gather test showing a lit campfire outside the old +/-8 scan
  still becomes a smoke source, while an unlit campfire does not.
- Add a deterministic particle test for the 26.2 roll/burst path and verify the
  resulting particles use `Behaviour::CampfireSmoke` with cosy/signal selection.
- Run focused shell tests, the shell crate suite with `--no-fail-fast`, and the
  repository's canonical health checks.
- Run the live `05-hud` capture, inspect the regenerated PNG, and require all
  three visual outcomes: one connected double chest, shaded lantern alcoves with
  visible local illumination, and at least one smoke particle above the campfire.

## How to change it

Scene composition lives in `scripts/screenshot-scenes/05-hud.txt`; keep chest
state and neighbouring coordinates consistent with `ChestBlock`'s connection
direction, and never replace the only opaque wall layer with a transparent model.
Campfire source discovery belongs with block-entity gathers in
`crates/lodestone-shell/src/block_entities.rs`. Random burst construction belongs
in `crates/lodestone-shell/src/particles.rs`, and its only production call belongs
in the fixed tick path in `sim/render_sources.rs`.

Do not add a screenshot-only `/particle` command, make the plume frame-driven, or
raise the generic ambient scan budget to compensate for a block-entity lifecycle
bug.

## Configuration

`LODESTONE_SCENES=05-hud` restricts `just screenshots` to this scene. The capture
requires the flat creative oracle on ports `25570`/`25571`, a GPU adapter, and the
26.2 vanilla assets under `.cache/mc/26.2`.

## Dependencies

- `scripts/screenshot-scenes/05-hud.txt` and the live screenshot harness.
- `lodestone-world` loaded chunk/block-entity data.
- `lodestone-data` block-state names and properties.
- `lodestone-particle`'s existing cosy/signal smoke emitter and simulation.
- The local Minecraft 26.2 sources for `CampfireBlockEntity::particleTick` and
  `CampfireBlock::makeParticles` as the behavioural oracle.
