# Fluid classification

## What it is

The single answer to **"does this block state carry water (or lava)?"** — shared by
the mesher (which draws the water surface) and by physics (which decides whether
you swim, and whether your eye is submerged).

Before this existed the two disagreed: the mesher classified any `waterlogged=true`
block plus kelp/seagrass/bubble columns as carrying water, while
`CollisionView::is_water` matched an exact `minecraft:water` state id. The visible
result was a player standing *inside rendered water* — waterlogged stairs, a kelp
forest — who could not swim and whose fog, overlay and ambient sounds all said
"dry".

## How it works

```
lodestone_render::BlockModels::fluid(state_id) -> Option<FluidCell>   ← the rule
        │
        ├── crates/lodestone-shell/src/mesher.rs   (FluidSectionView::fluid_at → bake_fluid)
        │
        └── crates/lodestone-shell/src/blocks.rs   vanilla_fluid(atlas, state_id)
                    │
                    └── crates/lodestone-shell/src/collision.rs
                            LiveCollision::fluid_kind → is_water / is_lava
                                    │
                                    └── lodestone_physics::compute_fluid_state
                                            → swim path, fog, overlay, ambient sounds
```

The rule itself lives in one function, `classify_fluid` in
`crates/lodestone-render/src/block_models.rs`, evaluated once per state at asset-load
time and stored in `BlockModels::fluids`. It covers three cases a block-id match
cannot:

1. `minecraft:water` / `minecraft:lava`, whose `level` property gives the amount and
   falling flag.
2. **Any** state with `waterlogged=true` — stairs, slabs, fences, trapdoors,
   chests, …
3. The five classes whose `getFluidState` hardcodes `Fluids.WATER.getSource(false)`
   with no blockstate property at all: `kelp`, `kelp_plant`, `seagrass`,
   `tall_seagrass`, `bubble_column`. A property-driven classifier is structurally
   unable to see these — the list is in `UNCONDITIONAL_WATER_BLOCKS`, extracted from
   the decompiled jar.

Two id spaces, two accessors, one rule:

- `blocks::vanilla_fluid(atlas, state_id)` — delegates to `BlockModels::fluid`. Used
  by `LiveCollision` (live multiplayer world).
- `blocks::demo_fluid(state_id)` — the offline demo palette's own one-line table
  (`id::WATER`). Not a copy of the vanilla rule: the demo palette is a nine-block
  fixture in its own id space with no models, no waterlogging and no lava.
- `ShellClassifier::fluid(state_id)` dispatches between them for callers that hold
  the session's classifier.

### Coarseness

`CollisionView` also has a finer `fluid_at` hook that reports the cell's *level*, so
`compute_fluid_state` can use vanilla's real `amount / 9.0` heights. The shell's
adapters deliberately do **not** implement it: they answer only the coarse
presence booleans, and `compute_fluid_state` then treats a present cell as a full
cell (height `1.0`). That is exact for the fully-submerged common case and matches
the coarseness the rest of the adapter already commits to (full-cube colliders
only). Implementing `fluid_at` from `BlockModels::fluid` — which already carries
`FluidState { amount, falling }` — is the natural next step if surface bobbing or
fluid-push ever matters.

## How to change it

- **A block classifies wrong** → fix `classify_fluid` in
  `crates/lodestone-render/src/block_models.rs`. Do *not* special-case it in
  `collision.rs`; that is the exact drift this structure exists to prevent.
- **A new consumer needs "am I in water"** → read
  `Sim::fluid_state()` (`lodestone_physics::FluidState`), not a fresh boolean. It
  already carries `in_water`, `under_water`, `in_lava`, `under_lava`, computed once
  per physics tick.
- **Gotcha — the surface boundary.** `isEyeInFluid` is inclusive: an eye exactly at
  the fluid top plane counts as submerged (`eye_y <= fluid_top`). Fog, overlay,
  sounds and pose all flip on that one comparison, so it is pinned by
  `eye_exactly_at_the_water_surface_counts_as_submerged` in
  `crates/lodestone-shell/src/collision.rs`.
- **Gotcha — `vanilla_fluid` returns `None` when the atlas has no baked models.**
  Never true for a live session (`BlockResources::try_vanilla` always calls
  `with_models` before the atlas escapes), but a hand-built `BlockAtlas` in a test
  will silently classify everything as dry.

## Configuration

None of its own. It needs the vanilla resource pack that
`BlockResources::load(true)` resolves — `LODESTONE_ASSETS`, else the highest-sorting
complete pack under `<repo>/.cache/mc/<ver>` (a directory holding both `client.jar`
and `generated/reports/blocks.json`). Without it the session falls back to the demo
palette, where `demo_fluid` applies.

## Dependencies

- `lodestone-render` — `BlockModels` (the rule), `BlockAtlas` (carries it),
  `FluidKind`.
- `lodestone-physics` — `CollisionView::is_water` / `is_lava` (the seam),
  `compute_fluid_state` / `FluidState` (the shared per-tick answer).
- `lodestone-assets` — `BlockBaker` and the blockstate/model resolution that
  `BlockModels::build` runs over.

## Tests

`crates/lodestone-shell/src/collision.rs`, all `#[ignore]`d because they need the
real pack — run with `cargo test -p lodestone-shell --lib -- --ignored`:

- `waterlogged_blocks_and_underwater_plants_submerge_the_eye` — the falsifying test,
  with `waterlogged=false` stairs and land plants as controls.
- `lava_submerges_the_eye_and_is_not_water`.
- `eye_exactly_at_the_water_surface_counts_as_submerged`.

The rule itself is unit-tested without a jar in
`crates/lodestone-render/src/block_models.rs`
(`waterlogged_blocks_carry_a_water_source`,
`underwater_plants_carry_water_without_a_waterlogged_property`).
