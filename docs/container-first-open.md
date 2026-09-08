# Container first-open performance

## What it is

The container renderer prewarms its block-entity icon resources during GPU
bring-up and can emit one location-level timing sample for the first drawable
container frame. This keeps one-time asset work out of inventory and chest
opening while preserving an opt-in diagnostic for regressions.

`IconRenderer::prewarm_special` builds the special icon pass before the window
enters normal redraw. The pass owns block-entity meshes, decoded sheets, bind
groups and banner/shield pattern masks, all of which used to be created from
the first frame containing a special item.

`ContainerProfile` is a one-shot diagnostic attached to
`ContainerRenderer`. It is separate from the rolling frame profiler because a
rolling mean cannot identify a single first-open stall.

## How it works

Bring-up attaches the item-model resources to both `HudRenderer` and
`ContainerRenderer`, then calls `prewarm_special_icons` while resource loading
is already expected. A jar-less or model-less run remains detached: the
prewarm method returns without changing the fallback behavior.

The container render path samples these stages on the first non-empty frame:
geometry construction, the player preview, icon upload (including the special
pass fallback), the slot submission, the between-strata hook, and the carried
submission. The log also records vertex counts, special-icon count, attachment
state, whether the special pass was already present, and the font's cached
glyph-run count before and after geometry construction. This makes a timing
location and workload explicit instead of reducing the frame to an average.

## How to change it

Keep the prewarm call after `attach_item_models`, because the special pass needs
the model colour format and the GPU queue. If a new first-use resource is added
to container rendering, add a stage to `container/profile.rs` and mark it at
the production consumer in `container/renderer.rs`. Keep the enabled/disabled
subscriber tests with the profile so a silent or unconditional instrument does
not pass review.

Resource-pack reloads may drop the special pass. The reload path should call
`prewarm_special_icons` after dropping it when the next draw must remain free of
one-time construction.

## Configuration

The profile is disabled by default. Enable it with the existing tracing filter:

```text
RUST_LOG=container_profile=debug
```

The first drawable container sample is emitted at debug level. The startup
prewarm emits a separate `special_prewarm` debug event when the same target is
enabled. If a pack reload or a failed prewarm leaves the pass absent, the
fallback constructor emits `special_lazy_build`, identifying any work that
still happened during redraw.

## Dependencies

The feature uses `ContainerRenderer`, `HudRenderer`, and the shared
`hud::item_icon::IconRenderer`. Resource decoding comes from the shell resource
loaders; GPU uploads and command submission use `wgpu`; timings use the
portable `crate::platform::Instant`.
