# Rendering handoff: first-person bow and framed maps

## What it is

This is the 2026-09-01 handoff for two live rendering defects that remain after commit
`62c3d425`: a bow that disappears while charging in first person, and filled maps that still
z-fight or disappear on the DemocracyCraft server. Sign text is now confirmed fixed and should
not be changed as part of this work.

The goal is vanilla-correct behavior derived from the 26.2 Java client, not another visual nudge.
The current checkout is `main`, and `62c3d425` is pushed to `origin/main`.

## Current status

| Area | Live result | Confidence / next boundary |
| --- | --- | --- |
| Sign text and glow | **Fixed**, confirmed by the user | Do not touch `gpu/sign_text.rs` while solving the other two defects |
| Bow mechanics and arrow flight | Working | Do not change use-state or projectile simulation |
| First-person bow pose | Bow disappears as soon as use begins | The Java pre-switch arm translation is missing from Lodestone's BOW chain |
| Visible item-frame map | Still z-fights at some views | Reaches map submission; CPU frustum culling is not the cause |
| Invisible-frame/floating map board | Still disappears with a nonlinear-looking edge | Reaches map submission; CPU frustum culling is not the cause |

## First-person bow: likely regression and exact Java behavior

The current implementation starts the BOW case at its BOW-specific translation:

- `crates/lodestone-render/src/entity.rs`: `first_person_bow_chain`,
  `first_person_bow_matrix`, and `first_person_item_mesh_with_use`
- `crates/lodestone-shell/src/gpu/first_person.rs`: supplies the live use pose

That misses a transform Java applies *before* entering the BOW switch. In
`.cache/mc/26.2/client-src/net/minecraft/client/renderer/ItemInHandRenderer.java`, Java does:

```text
if (!useAnimation.hasCustomArmTransform())
    applyItemArmTransform(arm, inverseArmHeight)

case BOW:
    T(i * -0.2785682, 0.18344387, 0.15731531)
    Rx(-13.935) * Ry(i * 35.3) * Rz(i * -9.785)
    charge shake, forward translation, Z scale, Ry(i * -45)
```

`ItemUseAnimation.BOW` has `customArmTransform=false`, so the complete matrix begins with:

```text
T(i * 0.56, -0.52 + inverseArmHeight * -0.6, -0.72)
```

Lodestone's `first_person_bow_chain(arm, held_ticks)` currently omits that translation and does not
accept `inverse_arm_height`. Thread the equip height into the bow matrix and compose the generic arm
translation before the existing BOW-specific chain. Do not add attack swing while actively using.

The test named `charged_bow_pose_matches_vanillas_item_in_hand_transform` is not a sufficient oracle:
its expected matrix repeats the same incomplete chain, so it passed while the live item disappeared.
Replace or extend it with a test built from Java's full call sequence, including a nontrivial
`inverse_arm_height`, then verify the ordinary idle item path is unchanged.

## Maps: observations and eliminated branches

The user tested a release client launched with only CPU map frustum culling disabled:

```bash
LODESTONE_MAP_DISABLE_FRUSTUM_CULL=1 \
LODESTONE_MAP_DIAG_FILE=/tmp/lodestone-map-frustum-off.log \
RUST_LOG=maps=debug just run
```

Neither the item-frame z-fighting nor the floating-board disappearance improved. Therefore
`framed_map_in_frustum` and its wall-offset AABB are ruled out for these symptoms.

The retained log contains live frames with all upstream boundaries satisfied. A representative
visible `glow_item_frame` had `item=filled_map`, `map_id=47295`, `source=Resolved`,
`in_frustum=true`, `submitted=true`, `instances=61`, and `batches=56`. An invisible-frame board also
had resolved content and was submitted. This rules out absent `MAP_ITEM_DATA`, the entity gather,
the map source, and the CPU broad phase for those observed entities. The unresolved defect is after
mesh preparation: geometry/winding, render state, or depth ordering.

Do not treat `map_in_front_mask` as a reliable per-corner depth oracle yet. The diagnostic compares
corresponding indices, but Lodestone's `Ry(180)` map transform swaps the X ordering relative to the
frame comparison surface. The live mask therefore oscillates partly because the corners are paired
incorrectly, not necessarily because the physical surfaces cross.

## Maps: the important divergence from Java

The relevant Java sources are:

- `.cache/mc/26.2/client-src/net/minecraft/client/renderer/entity/ItemFrameRenderer.java`
- `.cache/mc/26.2/client-src/net/minecraft/client/renderer/MapRenderer.java`

Java's framed-map sequence is:

```text
T(direction * 0.46875)
frame-facing rotations
T(z = 0.4375 visible, 0.5 invisible)
Rz((rotation % 4) * 90)
Rz(180)
S(1 / 128)
T(-64, -64, 0)
T(0, 0, -1)
MapRenderer quad at local z = -0.01
```

The final physical separation contributed by the last two operations is `1.01 / 128`. Java submits
the map through `RenderTypes.text(texture)` while the frame model uses `submitWithZOffset`.

Lodestone instead draws the map through a model-pipeline variant with alpha cutout, back-face
culling, depth write, and this pose in `crates/lodestone-shell/src/gpu/maps.rs`:

```text
item_frame_space * T(content_lift) * Rz(quarter_turn) * Ry(PI) * T(map_renderer_depth)
```

The `Ry(PI)` substitution was introduced to make the model-pipeline quad face outward and compensate
for its V direction. It is not Java's `Rz(PI)`, and the pipeline semantics are also not Java's text
render type. This combined geometry/pipeline divergence is now more suspicious than the numeric depth
bias. The last change reduced the map slope bias from `-2` to `-1`; hermetic Metal tests passed, but
the live server defects did not change.

Before adding another offset, reproduce Java more literally: determine `RenderTypes.text` cull,
depth-test, depth-write, blend, and polygon-offset state; then test an equivalent Lodestone map
pipeline with Java's exact vertex order and `Rz(180)` transform. A no-cull text-like pipeline is a
cleaner experiment than preserving `Ry(180)` and compensating elsewhere.

## Next experiments, in order

Run these live one at a time, not combined:

```bash
LODESTONE_MAP_DISABLE_BACKFACE_CULL=1 \
LODESTONE_MAP_DIAG_FILE=/tmp/lodestone-map-no-cull.log \
RUST_LOG=maps=debug just run

LODESTONE_MAP_DISABLE_DEPTH=1 \
LODESTONE_MAP_DIAG_FILE=/tmp/lodestone-map-no-depth.log \
RUST_LOG=maps=debug just run
```

Interpretation:

1. If no-cull restores either map, the quad's winding/front-face convention is wrong. Port Java's
   geometry and text-render culling semantics as one change.
2. If no-depth restores it, the map reaches rasterization but loses against the frame/wall/depth
   buffer. Compare Java's text render state and frame `submitWithZOffset`; do not add arbitrary
   world-space nudges.
3. If neither restores it, capture a GPU frame or add an offscreen pixel gate using the *live pose and
   pipeline*. At that point inspect alpha/cutout, bind-group selection, draw ordering, and whether a
   later pass overwrites the map.

The existing `crates/lodestone-shell/tests/framed_map_pixels.rs` Metal gates pass for every wall,
oblique views, and large-coordinate FOV 30/64/110. They do not reproduce the live bug, so another
similar synthetic camera sweep is not useful until the gate matches the live pipeline/pose failure.

## Block-entity report: keep it separate until identified

The user also reported that many “block entities” are absent and show no F3+B hitbox. Do not assume
this is the map bug without identifying a concrete object and its protocol type:

- chests, signs, and other true block entities never receive F3+B entity hitboxes;
- item frames are protocol entities, not block entities;
- Java's entity-hitbox renderer skips invisible entities, so an invisible item frame intentionally has
  no F3+B box.

Once an example is known, first decide whether it should come from the block-entity store or
`EntityDraw`; only the latter shares the framed-map gather path.

## Verification already performed

Against `62c3d425`:

- focused sign test passed and the user confirmed the live sign fix;
- two map diagnostic-switch tests passed;
- the bow matrix test passed, but is an invalid live oracle for the missing generic arm transform;
- `cargo check -p lodestone-shell --all-targets` passed;
- three Metal `framed_map_pixels` tests passed;
- the release live test proved `LODESTONE_MAP_DISABLE_FRUSTUM_CULL=1` does not affect either map defect.

## How to change it

Keep bow work scoped to the first-person transform; arrow physics is already correct. Keep map work
scoped to `gpu/maps.rs`, its render-pipeline state in `lodestone-render`, and the live-reproducing pixel
gate. Update `docs/first-person-held-item.md` or `docs/filled-map-rendering.md` when behavior changes.

This repository uses one shared `main` checkout. Follow `AGENTS.md`: use `apply_patch`, do not run
`cargo fmt`, `git add -A`, stash/reset/clean, or create a feature worktree. Commit explicit files and
push ordinary follow-up commits; never amend or force-push.

## Configuration and dependencies

The three map diagnostic switches are native-only, read once at process start, and accept any
non-empty value other than `0`: `LODESTONE_MAP_DISABLE_FRUSTUM_CULL`,
`LODESTONE_MAP_DISABLE_BACKFACE_CULL`, and `LODESTONE_MAP_DISABLE_DEPTH`.
`LODESTONE_MAP_TRACE_ENTITY=<id>` pins the log observer; `LODESTONE_MAP_DIAG_FILE=<absolute path>`
retains transition logs.

The bow path depends on resource-pack `DisplayTransform`s and the shell's interpolated equip height.
The framed-map path depends on `SessionMaps`, `MapRenderCache`, `EntityDraw`, `ModelPipeline`, and the
frame body's separately rendered block model.
