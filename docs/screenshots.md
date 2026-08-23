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
3. for each scene, run its RCON commands, drain the network until the world stops arriving,
   advance the sim clock by a fixed number of ticks, then render one frame through
   `RenderState::render` (plus `HudRenderer::render_with_item_models` when the scene asks for
   a HUD);
4. read the texels back and write a PNG.

**Scenes are data, not code.** One `scripts/screenshot-scenes/<name>.txt` per image; the
file's stem is the PNG's name, and the files are processed in sorted order. A line starting
with `@` is a directive, `#` is a comment, and everything else is an RCON command run
verbatim. Editing a scene therefore costs no recompile of a crate whose test binaries take
minutes.

```text
@size 2560 1440        # framebuffer, and therefore the PNG
@camera 0.0 65.1 10.5  # eye position, world coordinates
@look 0.0 65.1 13.6    # aim at a point…
@yawpitch 180 8        # …or aim explicitly, in the render camera's convention
@fov 50                # vertical FOV in degrees (default 70)
@wait 2000             # WALL-CLOCK ms floor on the network drain (costs no ticks)
@ticks 60              # sim ticks to advance after that drain, before the shot
@hud                   # composite the HUD over the world
@hand                  # draw the first-person hand
@debug                 # also draw the F3 overlay (needs @hud)
```

`LODESTONE_SCENES=02-signs,05-hud` restricts a run to those stems, which is how you iterate
on one image.

### A re-run is byte-identical, and the two settle directives are why

**A capture with no code change reproduces the committed PNG exactly.** Measured: three
consecutive full runs of the same commit produced the same five md5s
(`c3cbb47d…`, `5dfb7917…`, `62d244ef…`, `930e309f…`, `f5410329…`). This document used to say
the opposite — "reproducible but not byte-identical" — and that was not a property of
screenshots, it was three bugs:

| what moved | measured, run to run |
|---|---|
| every animation phase, because the capture tick was whatever the machine managed in a wall-clock window | `02-signs` **78,215–82,094 px** (a sea lantern's animated sprite), `03-block-entities` **77,963–93,798 px** (beacon beam, banner sway, book, conduit), `05-hud` **36,032 px** |
| the world was not finished arriving | `03-block-entities` alone under `LODESTONE_SCENES` drew **38 sections / 15,015 quads** on one run and **36 / 13,991** on the next |
| two item entities the scene created without meaning to | see the campfire gotcha below |

The fix is that the settle is now two directives rather than one, because it was doing two
unrelated jobs:

* **`@wait` is wall clock and it is for the network.** RCON edits have to travel back over
  the socket and be meshed, and that answers to the machine. This phase pumps `Sim::step`
  with `dt = 0`, which runs `Update` (and therefore `poll_net`, `heal_dirty_columns` and the
  mesh drains) while `FrameClock::take_tick` claims **no** tick — so a slow machine costs
  seconds and not phase. It is a *floor*, not the whole wait: the harness keeps draining
  until 40 consecutive frames upload no section, remove none, and see no change in the
  loaded-column count.
* **`@ticks` is sim ticks and it is for the animation.** It runs after the drain with no
  sleeping at all, so the frame is captured at a fixed absolute tick — `JOIN_BASE_TICK` plus
  every earlier scene's `@ticks`. The harness asserts that number rather than trusting it.

The join is the one phase that cannot be tick-free (a client that never ticks never sends a
position), so its cost is variable — measured at 1 to 120 ticks — and is normalised away by
winding the clock up to `JOIN_BASE_TICK` before the first scene.

**The residual, so nobody reads it as a new regression.** In 2 of about 15 runs the
*distant superflat ground* seen past the stage edge in `01-text-displays` (and, through the
same sections, `02-signs`) rendered **untinted**: neutral grey `(166, 167, 168)` where a
correct run gives plains green `(85, 111, 55)`, with **91 fewer quads** in both scenes — the
count moves because a per-position biome tint stops quads merging. It is not an animation
phase (the tick is pinned), not a streaming race the quiescence wait can see (the wrongly
tinted mesh is quiet), and it survives every change in this harness, so it is a **client**
bug rather than a capture one: a player joining that world would see the same grey ground on
those frames. Only the *initial* chunk stream is affected — every block a scene places
afterwards re-meshes correctly — which is what points at the join. If a re-render shows a
grey horizon in those two images, discard it and run again; that is the one difference that
is still noise.

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

* **Build high, not on the oracle's own surface — though the reason has changed.** The
  superflat world's ground is `y=-61`, and this client used to apply vanilla's *non-flat*
  void fog everywhere (a 32-block fade above the world bottom), so a camera down there
  rendered a near-black sky: brightness `0.042` at `y=-57.4`. That is fixed — the level's
  own `is_flat` now reaches the renderer and a flat level's onset is `1.0`, so the oracle's
  surface renders fully lit. Every scene still builds a stage at `y=63`, because the stone
  plate reads better than the oracle's grass and because a stage at the world bottom has no
  horizon behind it.
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
* **The README set renders at 2560x1440, and PNG size scales nowhere near the pixel
  count.** 1440p is 11.1x the pixels of the old 768x432, and the four re-rendered scenes
  grew only **2.7-3.7x** (386 KB total to 1.19 MB) — Minecraft art is flat, low-entropy
  colour, so the compressor absorbs most of the extra resolution. Budget by measuring, not
  by scaling: the whole five-scene set lands near 1.6 MB, well inside what a README should
  carry, so there was no need to fall back to 1080p.
* **Do not post-process the committed PNGs.** A lossless re-encode (PIL `optimize=True`) was
  measured at **0.9%** — 1,214,413 B to 1,203,387 B across the four — and no PNG optimiser
  (`oxipng`, `pngquant`, `optipng`, `zopflipng`, `pngcrush`, ImageMagick) is installed here
  anyway. It is not worth it even where a tool exists: the harness is the generator, so an
  optimised file no longer matches what a re-render produces, and every subsequent run shows
  a spurious diff. Keep the committed bytes exactly what the harness wrote.
* **A few hundred pixels per image come out non-opaque**, and it is worth knowing before
  anyone "fixes" it: the nametag, glowing-sign and XP text passes write their own alpha into
  the framebuffer, so `02-signs`, `04-entities` and `05-hud` each carry 200-530 pixels
  (0.005-0.014% of the frame) at alpha 128-254, clustered exactly on that text. GitHub
  composites those against the page background. It is invisible at this scale and dropping
  the alpha channel would save only ~8%, which is why the images stay RGBA — but a pass that
  starts leaking alpha over a *large* area would show up here first.
* **A block-entity NBT field you leave out is not a default, it is a zero — and for a
  campfire that means the food cooks on the first tick.** `03-block-entities` set
  `Items:[{porkchop},{potato}]` and nothing else, so `CookingTotalTimes` came out
  `[I; 0, 0, 0, 0]`, `CampfireBlockEntity.cookTick` found every slot already past its total,
  and both items were ejected as **real item entities**. Verified over RCON against the
  offending line: the block read back `Items: []`, and a `Cooked Porkchop` and a
  `Baked Potato` were lying on the stage at `(2.47, 64.0, 12.90)` and `(4.71, 64.5, 13.55)`.
  So the scene did not photograph what its own header claimed — an empty campfire, and two
  pieces of litter whose eject velocity and yaw are rolled fresh every run, which was the
  last source of run-to-run pixel noise in the set. Give the block the real
  `CookingTotalTimes:[I;600,600,0,0]` (vanilla's own `CampfireCookingRecipe` time). The
  general form: the harness's `gamerule mob_drops false` / `block_drops false` preamble stops
  the *server* dropping items and does nothing about items a scene's own block entities
  create.
* **Water is the one thing a scene cannot clean up after itself.** A `fill … air` over a
  stage leaves the sources outside it, which flow back in and flood the next scene. The
  harness purges water once per run over a box wider than any stage.
* **Every scene shares one world**, so a scene must build what it needs and must not assume
  its plot is empty. Each file starts with its own `fill … air` and `kill`.
* **A fixed wall-clock settle is a bet on the machine, not a condition on the world.** With
  `LODESTONE_SCENES=03-block-entities` — no earlier scene having paid for the initial stream
  — two runs of the same commit drew 38 sections / 15,015 quads and 36 / 13,991 against a
  three-second `@wait`. The harness now waits for quiet (`QUIET_FRAMES` frames with no
  upload, no removal and no change in the loaded-column count) with `@wait` as a floor, and
  does the same before the first scene so a narrowed run does not start against a
  half-streamed world. It warns and captures anyway at `SETTLE_DEADLINE`, because a capture
  against an unfinished world writes a visibly wrong PNG that the two-statistic control
  catches, while a hang would just look like a slow run.

### Four reports that were the scene, and one that was not

**Nothing is currently held out of these images.** This section is kept because of what
the four false reports have in common, not because anything is still excluded.

Four block-entity subjects were reported as rendering wrongly and pulled from the
block-entity scene. Re-measured against the same oracle, **three of the four were the
scene's own placement**, and each produced a picture that looks exactly like a renderer bug:

| subject | reported as | actually |
|---|---|---|
| `conduit` | a huge translucent blue sheet over the whole stage | `minecraft:conduit`'s default state is `waterlogged=true`, so a bare `setblock` puts a real water source on the stage; the server floods it to a radius-6 diamond, and with the eye 0.125 blocks above that surface the water fills the frame. Vanilla does the same. Place it `waterlogged=false` |
| `skeleton_skull` / `zombie_head` | a plain untextured cube | the camera was square on to `rotation=8`, which is the **back** of the head — segment 0 faces north (`-Z`). The back of a skeleton skull is uniform light grey and of a zombie head uniform green. At `rotation=0` both draw their faces, and so do the creeper, wither-skeleton and player heads |
| the double chest | half a chest each way, "a hole on each side" | `type=left`/`type=right` is only half the placement — `ChestBlock.getConnectedDirection` pairs `LEFT` with `facing.getClockWise()`, so at `facing=south` the scene's `left` looked west and its `right` looked east and neither found a partner. Two orphans, each drawing the seam face it exists to hide |
| `dragon_head` (and `piglin_head`) | draws nothing at all | **real**: `DragonHeadModel`/`PiglinHeadModel` are multi-part rigs unrelated to the shared 8×8×8 `SkullModel` box, and were unported. Now ported, and in the scene |

Three things generalise:

* **An A/B that removes the subject proves the subject is *involved*, not that the subject's
  renderer is at fault.** All four were confirmed that way and three were still the scene.
* **A block's default state is not the state you meant to place.** `waterlogged`, `type`,
  `facing`, `rotation` and `powered` all have defaults, and `setblock` takes them silently.
* **Half the reports were a subject photographed from behind.** A skull at `rotation=8`, a
  chest at `facing=south` with the camera on `-Z`: no face, no latch. Check which way the
  subject is pointing before believing the renderer dropped a texture.

### Two scene descriptions that had stopped matching what they built

Found while characterising the noise, and worth knowing the shape of, because `02-signs`
already had one (it described a dark room and was not roofed):

* `01-text-displays` explained its `y=63` stage by the *non-flat void fog*, which is fixed —
  the same fix this page's first gotcha records. The stage is still right; the reason had
  become composition, not a renderer gap.
* `04-entities` said `difficulty easy` was there "because peaceful removes hostile mobs the
  tick they spawn". Every mob it summons — sheep, wolf, axolotl, fox, allay, cow — is
  passive, so peaceful would leave all of them standing. The command is world setup, not this
  frame's requirement.

Neither was wrong when written and neither is visible in the image, which is exactly why both
survived. When a scene's build changes, re-read its header in the same edit.

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
