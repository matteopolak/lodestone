# Screenshots

## What it is

The README's in-game images, and the harness that produces them. Every PNG under
`docs/images/` is this client rendering a live session against the flat creative 26.2
oracle — no mock-ups, no compositing, no editing. `just screenshots` regenerates the whole
set, so the images can be refreshed when the renderer changes instead of drifting into a
record of how the client looked one afternoon.

This is separate from `crates/lodestone-shell/src/screenshot.rs`, which is the in-game
`key.screenshot` keybind. The two share the PNG encoder (`screenshot::encode_png`) and
nothing else: the keybind reads the window's swapchain, this reads a headless target.

## How it works

`crates/lodestone-shell/tests/capture_screenshots.rs` is a live gate in the shape of
`live_sign_text_pixels.rs` that ends at a file rather than at an assertion:

1. join the oracle with `Sim` — the same type `WindowApp` drives — via `Sim::connect_as`,
   and pump `Sim::step` + `drain_removals`/`drain_meshes` the way `app/redraw.rs` does;
2. install the render sources `app/session.rs`'s `install_session_render_sources` and
   `app/redraw.rs` install: the sky pass, the entity light sampler, the shadow ground
   sampler, the time-of-day clock, and every block-entity/display source;
3. for each scene, run its RCON commands, keep pumping for its settle time, then render one
   frame through `RenderState::render` (plus `HudRenderer::render_with_item_models` when the
   scene asks for a HUD);
4. read the texels back and write a PNG.

**Scenes are data, not code.** One `scripts/screenshot-scenes/<name>.txt` per image; the
file's stem is the PNG's name, and the files are processed in sorted order. A line starting
with `@` is a directive, `#` is a comment, and everything else is an RCON command run
verbatim. Editing a scene therefore costs no recompile of a crate whose test binaries take
minutes.

```text
@size 768 432          # framebuffer, and therefore the PNG
@camera 0.0 65.1 10.5  # eye position, world coordinates
@look 0.0 65.1 13.6    # aim at a point…
@yawpitch 180 8        # …or aim explicitly, in the render camera's convention
@fov 50                # vertical FOV in degrees (default 70)
@wait 2000             # milliseconds to keep pumping the sim after the build
@hud                   # composite the HUD over the world
@hand                  # draw the first-person hand
@debug                 # also draw the F3 overlay (needs @hud)
```

`LODESTONE_SCENES=02-signs,05-hud` restricts a run to those stems, which is how you iterate
on one image.

A re-run is **reproducible but not byte-identical**: the world state is pinned (spawn, time,
weather, game rules) but a campfire's flame, a mob's idle sway and a chat line's fade age are
all phase-dependent, so the PNGs differ by a few hundred bytes between runs. That is why
there is no drift gate over them.

### The control

A capture tool's worst failure is a *silent* one — a black frame, or a camera inside a
block, written to `docs/images/` and committed. Draw counters cannot rule that out; this
repo has measured a harness that submitted geometry, reported 59 sections and 15,104 quads,
and read back nothing. So each frame is checked on two pixel statistics before it is
written:

* **distinct colours**, quantised to 5 bits per channel — a frame that never lit is one flat
  colour;
* **the fraction of pixels off the modal colour** — a frame that is *mostly* one thing still
  carries a legible sky gradient and would clear a distinct-colour floor on its own.

The thresholds (64 and 0.25) sit well under every measured scene (175–1009 distinct, 0.75–0.93
off-modal) and well over a degenerate frame.

## How to change it

Add a file under `scripts/screenshot-scenes/`, then run `just screenshots`. Nothing in the
harness needs to know about it. To change what the README shows, edit the table in
`README.md` — the harness writes files and does not touch it.

Gotchas, each of which cost a run:

* **Build high, not on the oracle's own surface.** The superflat world's ground is `y=-61`,
  and this client applies vanilla's *non-flat* void fog everywhere (a 32-block fade above
  the world bottom, `VoidFog::OVERWORLD`), so a camera down there renders a near-black sky.
  Vanilla uses a 1-block snap in a flat world; we do not know the world is flat. Every scene
  therefore builds a stage at `y=63`.
* **26.2 renamed every game rule to snake_case.** `advance_time`, not `doDaylightCycle`;
  `mob_drops`, not `doMobLoot`. The camelCase spellings do not parse at all, and
  `/gamerule` reports that as `Incorrect argument for command`. Ask the server
  (`help gamerule`) rather than a wiki page.
* **A render source has no uninstall, and one `RenderState` serves the whole run.** The
  first-person-hand suppressor was installed only for scenes without `@hand`, which left it
  in place for the `@hand` scene that came after — captured with no hand at all, and nothing
  red anywhere. Install both arms of any such switch, never one.
* **And the mirror image: a source that is never installed at all.** The entity ground
  shadows shipped with no `ShadowGroundSource` here, so `RenderState::prepare_shadows`
  sampled `None` for every candidate cell, emitted zero vertices, and every mob in every
  capture stood on a shadow-free floor. Neither half of the surrounding wiring objected —
  the option gate (`set_entity_shadows_enabled`) defaults on, the shadow texture loads from
  the vanilla pack, the pass ran and produced nothing. Re-rendering with the source
  installed moved **3,673 px in `04-entities` (3,671 of them darker)** and 811 in `05-hud`,
  all inside the ground band under the entities. This harness is a second, silent
  implementation of `install_session_render_sources`' wiring: when that function grows a
  source, mirror it here in the same commit, because nothing mechanical connects the two.
* **`HudRenderer::new` takes the *raw* (non-sRGB) format and every `attach_*` takes the
  corrected one.** Vanilla's 2-D GUI blending is not colour-managed, so the flat-colour pass
  draws into a raw view of the same texture. Building the whole thing against one format is
  a wgpu validation error at the first `set_pipeline`.
* **The target is `Rgba8UnormSrgb`, unlike the pixel gates' `Rgba8Unorm`.** That is the
  format whose stored bytes are what a player sees, and therefore what belongs in a PNG; a
  linear target would build every pipeline against a linear write and the file would come
  out dark.
* **Water is the one thing a scene cannot clean up after itself.** A `fill … air` over a
  stage leaves the sources outside it, which flow back in and flood the next scene. The
  harness purges water once per run over a box wider than any stage.
* **Every scene shares one world**, so a scene must build what it needs and must not assume
  its plot is empty. Each file starts with its own `fill … air` and `kill`.

### What is deliberately not photographed

A screenshot must not launder a defect. Three block-entity types were once pulled out of the
block-entity scene as broken. Re-measured against the same oracle, **two of the three were
the scene's own placement, not the renderer** — and both misdiagnoses are worth keeping,
because both produce a picture that looks exactly like a render bug:

| subject | reported as | actually |
|---|---|---|
| `conduit` | a huge translucent blue sheet over the whole stage | `minecraft:conduit`'s default state is `waterlogged=true`, so a bare `setblock` puts a real water source on the stage; the server floods it out to a radius-6 diamond, and with the eye 0.125 blocks above that surface the water fills the frame. Vanilla does the same. Place it `waterlogged=false` |
| `skeleton_skull` / `zombie_head` | a plain untextured cube | the camera was square on to `rotation=8`, which is the **back** of the head — segment 0 faces north (`-Z`). The back of a skeleton skull is uniform light grey and of a zombie head uniform green. At `rotation=0` both draw their faces, and so do the creeper, wither-skeleton and player heads |
| `dragon_head` (and `piglin_head`) | draws nothing at all | real: `DragonHeadModel`/`PiglinHeadModel` are multi-part rigs, unrelated to the shared 8×8×8 `SkullModel` box, and were not ported |

The lesson generalises past these two: **an A/B that removes the subject proves the subject
is involved, not that the subject's renderer is at fault.** Both of these were confirmed by
removing the block and watching the artefact go, and both were still the scene.

Two smaller divergences the images do show, honestly, rather than hide: legacy `§` codes in
sign text render as literal section signs (this client deliberately never turns them into
style), and the tab list draws over the boss-bar title, which is vanilla's own order.

### Two deliberate divergences from live-gate practice

* **Fixed usernames**, not `lodestone_testsupport::unique_username`. Offline mode derives
  the account UUID from the name, so a shared name is a shared player file — the hazard
  being that a *dead* player is held on the death screen and is sent no chunks. The
  companions never render anything and the oracle is flat, creative and peaceful, so nothing
  here can kill one; what a unique name would cost is the whole point of the tab-list image,
  since `E0_1k3j9fa2` is not a screenshot of a tab list. The camera client is put in
  creative on join for the same reason.
* **Two `HotbarSlot` fields are left at their defaults.** `enchanted` and `skin` come from
  `hud::item_icon`, which is `pub(crate)` and so unreachable from an integration test. The
  consequence is narrow and stated rather than hidden: a glinting or custom-head stack in a
  captured hotbar would draw without its foil or its face. No scene puts one there.

## Configuration

| | |
|---|---|
| `LODESTONE_SCENES` | comma-separated scene stems to capture; unset captures all |
| oracle | `127.0.0.1:25570` game, `127.0.0.1:25571` RCON, password `lodestone` — matching `scripts/live-oracles/creative.sh` |
| output | `docs/images/<scene stem>.png` |

The harness pins the world spawn to `(0, -60, 0)`, force-loads a 64×64-block box around it,
stops the day/night and weather cycles, sets the time to 2000 (late morning) and suppresses
command feedback, so a re-run is reproducible rather than "whatever the world was doing".

## Dependencies

* the flat creative 26.2 oracle — `just oracle-creative` (`scripts/live-oracles/creative.sh`)
* a wgpu adapter; the harness fails rather than skips without one
* the vanilla assets under `.cache/mc/26.2` (or `LODESTONE_ASSETS`) — without them `Sim`
  takes the demo path and would capture the procedural palette rather than the game
* `--features live`, which compiles `v770` into the registry
* `lodestone_testsupport::RconClient`, `lodestone::screenshot::encode_png`
