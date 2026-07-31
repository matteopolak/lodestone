# The sky pass and the air-bubble row

## What it is

Two features that landed as complete, tested, unreachable modules and were then
wired into the frame:

- **The sky** — a sky disc, sun, moon (8 phases), a star field and a cloud plane,
  drawn before the terrain pass. `crates/lodestone-render/src/sky.rs` is the pure
  half (time-of-day maths and geometry), `sky_pipeline.rs` the GPU half.
- **The air-bubble row** — vanilla's underwater breath meter, ported in
  `crates/lodestone-render/src/air_bubbles.rs` and drawn by the HUD.

They are documented together because they shared a failure mode, not a
subsystem: both were **islands**, individually green and reaching zero pixels.
That is the dominant defect class in this repo (`CLAUDE.md` rule 1), and these
were instances nine and ten.

## How it works

### Sky

`RenderState` holds `sky: Option<SkyRenderer>`, installed via `install_sky` and
fed by `set_time_of_day_source` — the same injected-closure pattern
`set_sky_darken_source` already used, so the renderer never learns about the
network. Both are installed at the **two** connect sites in `app.rs` that install
every other render source.

In `render_inner` the sky runs in its **own pass, before the block pass**, and
the block pass's colour attachment then becomes:

```rust
load: if stats.sky_drawn { wgpu::LoadOp::Load } else { wgpu::LoadOp::Clear(self.clear) }
```

**Conditional on the sky having actually drawn, not on a renderer being
installed.** That distinction is load-bearing: an unconditional `Load` would
leave a headless or pre-install frame with no clear at all, which reads as
smeared history rather than as an obviously-missing sky — a much harder bug to
recognise.

Every sky pipeline sets `depth_stencil: None` and runs with **no depth
attachment**. This is deliberate and worth preserving: our depth is `[0,1]`
DirectX-style rather than vanilla's reversed-Z, so every ported depth comparison
flips sign. Having nothing to flip is worth more than having it right.

### Air bubbles

`airSupply` was decoded nowhere, so the chain is six hops:

`metadata.rs` (`IDX_AIR_SUPPLY = 1`) → `EntityMetadataUpdate::air_supply` →
`Vitals::air` (via `apply_local_player_air_supply`, registered after
`apply_entity_metadata`) → `PlayerSnapshot::air` → `Sim::air` →
`HudFrame::air: Option<(i32, i32, bool)>` → `sprite_vitals`.

The index is **verified, not assumed**: `Entity.java:260` defines
`DATA_SHARED_FLAGS_ID` (index 0) and everything between it and
`DATA_AIR_SUPPLY_ID` at `:268` is `int FLAG_*` constants, not accessors — so air
supply is the next accessor and index 1 is correct. A wrong metadata index reads
a different field entirely and produces plausible nonsense.

The GUI atlas needed **no** work: `GuiAtlas` globs `gui/sprites/**`, so
`hud/air`, `hud/air_empty` and `hud/air_bursting` were already stitched in. A
regression test in `gui_atlas.rs` pins that.

Visibility follows vanilla exactly — `Hud.java:910`:

```java
if (isUnderWater || currentAirSupplyTicks < maxAirSupplyTicks)
```

An **or**, not an and. The row stays visible out of water while air is below max,
which is what makes the gradual refill watchable after surfacing.

## How to change it, and the gotchas

**The `Clear`/`Load` handover is the fragile part of the sky wiring.** If you add
another pass before the block pass, decide explicitly which one owns the clear.
`stats.sky_drawn` is the signal; do not re-derive it from `self.sky.is_some()`.

**`sprite_vitals` lays out relative to a moving anchor.** `row_y` derives from
`cluster_top`, which starts at `b.h - margin` and is pulled up only `if
frame.hotbar` and again only `if frame.xp`. Any test or layout change that
assumes a fixed offset from the bottom will be wrong for some frames — see the
gate note below, where exactly that cost a false negative.

**The bubble `wobble` argument is always `false` today.** Vanilla samples
`tickCount % 2 == 0` (plus a second RNG coin flip) for a 0–1px jitter on a fully
empty row's last bubble. No per-frame tick parity is piped into `HudFrame`, so
this is deliberately unwired rather than approximated. Purely cosmetic.

**Deliberate sky omissions, so nobody reads them as bugs:** clouds are vanilla's
flat "fast" mode, not the 3-D voxel-extruded fancy mode; there is no
below-horizon dark disc; there is no sunrise/sunset tint fan (#96);
`sky_color_for_time_of_day` is a labelled **approximation** because 26.2's
`SKY_COLOR` is a biome-blended keyframe track with no classic-era formula to
port; and the star field uses splitmix64 rather than Java's RNG — same
distribution shape, different exact positions, a visual choice and not a
decode-parity claim.

## The gates, and what they cost to get right

Both features have pixel gates driving the real shell path, and **both gates were
wrong before they were right** — in ways worth recording, because each is a
general trap rather than a one-off.

`crates/lodestone-shell/tests/sky_pixels.rs` first asserted that a sky-less frame
clears *uniformly* to `SKY_COLOR`. It failed at 3.5%. A location report put the
offending pixels at `x221..255 y180..255` in dark browns: the **first-person bare
arm**, which `gpu.rs`'s hand pass draws whenever `third_person_body_drawn` is
false — i.e. always, in first person, with nothing installed. The control's
premise was false before the sky existed. The gate now measures inside the sky's
own screen rect, and `arm_is_what_we_excluded` pins the reason so the excluded
rows are a measurement rather than a magic number.

`crates/lodestone-shell/tests/air_bubble_pixels.rs` failed twice. First it
reported 0 px everywhere and looked like a dead chain — the rect had hardcoded
the *with-hotbar* `cluster_top`, so it was measuring ~20 logical pixels above a
row that was drawing perfectly. Then its control asserted that leaving the water
hides the row immediately; `Hud.java:910` says otherwise. The controls now
isolate vanilla's two disjuncts separately: full air + dry draws nothing, full
air + **underwater** still draws — which is what makes `eye_in_water`
demonstrably load-bearing rather than incidentally satisfied.

Both lessons reduce to the same rule, and it applies to gates as much as to bugs:
**ask where, not just how much.** A percentage cannot distinguish a
uniform-but-wrong frame from a localised blob, and this repo has a documented
case (`DESIGN.md` §12) of a frame average producing a confident wrong conclusion
that clustering by location immediately overturned. Both gates now print a
bounding box on failure for exactly that reason.

Measured, on this machine:

| gate | subject | control(s) |
|---|---|---|
| sky | 100% of the sky rect differs from `SKY_COLOR` | 0.0% with no sky installed |
| sky (day/night) | 97.6% near-black at midnight | 1.1% at noon |
| bubbles | 524 px underwater at 150/300 air | 0 px full-air-dry, **760 px full-air-wet**, 0 px `air: None` |

## Configuration

None. The sky reads the same day clock the rest of the renderer does — there is
no second clock — and `day_sky_color` is fed the renderer's existing clear colour
so wiring the sky in did not change how noon looks. Air supply is entirely
server-driven.

## Dependencies

- `lodestone-assets` — `CelestialAtlas` (sun + 8 moon phases stitched by the same
  `AtlasBuilder` every other atlas uses) and `load_cloud_texture`.
- `lodestone-render` — `sky`, `sky_pipeline`, `air_bubbles`, and
  `Camera::sky_view_projection` (translation-stripped, with a test whose negative
  control proves ordinary `view_projection` *is* translation-sensitive).
- `crates/protocol/v770` — `metadata.rs` decodes `airSupply`.
- `lodestone-ecs` / `lodestone-client` — `Vitals::air` and `PlayerSnapshot::air`.
  Note `ingest::handles_event` must list the event or `SharedState::apply` never
  forwards it in production, regardless of what a hermetic test shows. That trap
  hid working code twice in one session.
