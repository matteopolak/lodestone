# Client rendering and UI: the remaining visual and audio surface

## What this is

The decomposition of everything the player *sees or hears* that is not yet at 1:1 parity
with vanilla 26.2: block entity renderers, sky and weather, smooth lighting, particles, the
remaining GUI screens, HUD elements, camera/post effects, item and entity visuals, audio, and
text rendering breadth. Client physics/prediction/input, server-side anything, the plugin
framework, and benchmarks are covered by the other docs in this directory
([`../roadmap/README.md`](./README.md) lists them). This doc does not repeat their scope.

Every item below is a GitHub issue, filed as a sub-issue of the tier epic it belongs to
([#1](https://github.com/matteopolak/lodestone/issues/1) Tier 1,
[#2](https://github.com/matteopolak/lodestone/issues/2) Tier 1½,
[#3](https://github.com/matteopolak/lodestone/issues/3) Tier 2,
[#4](https://github.com/matteopolak/lodestone/issues/4) Tier 3) and on the
[project board](https://github.com/users/matteopolak/projects/7). [`../backlog.md`](../backlog.md)
remains the tier-definition and trap record; when it and the tracker disagree, the tracker is
newer.

## Why this exists, and the two rules that shaped every issue below

1. **Nothing is done until something on screen changes.** This subsystem has produced more
   confirmed islands than any other in the repo — decoded data with no render consumer
   (nametags, mob equipment, pickup fly-to-player), or a bool/field that exists but every call
   site hardcodes it away (enchantment glint). Every issue below either names what will
   consume the work, or — where the answer is "an existing call site, once one line changes"
   — says so explicitly, because that is the cheapest and easiest class of fix to mistake for
   something bigger.
2. **Re-verify before routing around "X doesn't exist yet."** This pass re-grepped the whole
   tree for every claim in the original briefing rather than trusting it, and found real
   staleness — see "Corrections found while grounding this roadmap" below. One stale sentence
   about item-drop rendering was previously copied into four issues as a shared root cause
   and misdirected three of them; the lesson generalizes past that one incident.

## Ordering rationale

Ordered by **how much it changes the screen (or the speaker) per unit of effort**, the same
principle [`../backlog.md`](../backlog.md) already uses. Five bands, each internally ordered
the same way:

### Band 1 — biggest tell, cheapest fix

The "not Minecraft" tells a first-time viewer would name first, several of which are a
missing consumer for data or plumbing that already exists.

| issue | item | why it's here |
|---|---|---|
| [#22](https://github.com/matteopolak/lodestone/issues/22) | Smooth lighting / AO on the model path | Flat block light + directional shade is the single most recognisable non-vanilla tell now that geometry is right. |
| [#96](https://github.com/matteopolak/lodestone/issues/96) | Sky dome: gradient, sunrise/sunset tint, void fog | The sky is a flat clear colour today, full stop — every outdoor frame is affected, and the fix is "add a shader," not new state. |
| [#100](https://github.com/matteopolak/lodestone/issues/100) | Nametags, with occlusion | `custom_name`/`custom_name_visible` are already decoded every tick and consumed by nothing — a pure wiring fix, and the first thing a second player looks for. |
| [#103](https://github.com/matteopolak/lodestone/issues/103) | Death screen | Currently the player dies and nothing happens — reads as a crash, not a design choice, to anyone who doesn't know the internals. Reuses existing button/screen machinery. |
| [#130](https://github.com/matteopolak/lodestone/issues/130) | Enchantment glint | The bool field already exists on `ItemIcon`; every call site hardcodes it to `false`. Cheapest genuinely-visible fix in the whole list once the shimmer shader itself is written once. |
| [#45](https://github.com/matteopolak/lodestone/issues/45) | Sheet particle atlas bug | A live bug, not a gap: flame/smoke/crit particles currently sample block-atlas texels at particle-atlas coordinates. |
| [#98](https://github.com/matteopolak/lodestone/issues/98) | Hurt flash and screen shake | Cheap screen-space effects with an obvious trigger (damage/explosion events), high "did something happen" signal. |

### Band 2 — core to "a stranger plays survival for an hour"

The rest of Tier 1 and the highest-value Tier 1½ items — everything a full session of
ordinary survival play would surface.

| issue | item |
|---|---|
| [#24](https://github.com/matteopolak/lodestone/issues/24) | Sun, moon, stars, clouds |
| [#25](https://github.com/matteopolak/lodestone/issues/25) | Weather: rain, snow, thunder |
| [#23](https://github.com/matteopolak/lodestone/issues/23) | Block entity renderers (chest and sign first, per its own scope note; skulls/campfires/brewing-stands/lecterns added by comment) |
| [#58](https://github.com/matteopolak/lodestone/issues/58) | View bobbing, damage tilt, view lag |
| [#54](https://github.com/matteopolak/lodestone/issues/54) | First-person held item (block/sprite/special poses) |
| [#57](https://github.com/matteopolak/lodestone/issues/57) | Bow/crossbow draw pose — lands after #54 |
| [#53](https://github.com/matteopolak/lodestone/issues/53) | Mob render layers (sheep wool first; wolf/creeper/golem/llama/horse/mooshroom/snow-golem/shulker/villager/glowing-eyes family) |
| [#108](https://github.com/matteopolak/lodestone/issues/108) | Underwater overlay — the submerged flag already exists, this is a draw-only fix |
| [#60](https://github.com/matteopolak/lodestone/issues/60) | Air-supply tracking + bubble row |
| [#30](https://github.com/matteopolak/lodestone/issues/30) | HUD animations (hearts/hunger/XP/hotbar-pop) |
| [#17](https://github.com/matteopolak/lodestone/issues/17) | Armour leather dye + trims |
| [#112](https://github.com/matteopolak/lodestone/issues/112) | Fire overlay |
| [#117](https://github.com/matteopolak/lodestone/issues/117) | Text styling draw (bold/italic/underline/strikethrough/obfuscated) |
| [#121](https://github.com/matteopolak/lodestone/issues/121) | Attack indicator |
| [#126](https://github.com/matteopolak/lodestone/issues/126) | Held-item name tooltip |
| [#135](https://github.com/matteopolak/lodestone/issues/135) | Music: menu, biome, situational tracks |

### Band 3 — the container/GUI backbone and the visual long tail

High total effort, individually well-scoped, and mostly independent of each other. This is
where Tier 2 lives.

| issue | item |
|---|---|
| [#28](https://github.com/matteopolak/lodestone/issues/28) | Container screens: the whole family (furnace/anvil/enchanting/brewing/loom/smithing/stonecutter/grindstone/cartography/beacon/villager/horse) |
| [#50](https://github.com/matteopolak/lodestone/issues/50) | Block items render flat in container screens (bug) |
| [#51](https://github.com/matteopolak/lodestone/issues/51) | Container screens drawn programmatically, not from pack sprites |
| [#158](https://github.com/matteopolak/lodestone/issues/158) | Creative inventory screen and category tabs |
| [#171](https://github.com/matteopolak/lodestone/issues/171) | Generic item tint pipeline (potion/spawn-egg/map colours) |
| [#174](https://github.com/matteopolak/lodestone/issues/174) | Banner and shield pattern compositing (coordinate with #23) |
| [#178](https://github.com/matteopolak/lodestone/issues/178) | Particle catalogue: ambient/environmental (soul, portal, enchant, drip, campfire smoke, end_rod, sculk, gust, sonic_boom) |
| [#182](https://github.com/matteopolak/lodestone/issues/182) | Particle catalogue: combat/event (totem, explosion, firework, note, heart, villager mood, redstone dust, witch) |
| [#184](https://github.com/matteopolak/lodestone/issues/184) | Filled map item rendering |
| [#163](https://github.com/matteopolak/lodestone/issues/163) | Recipe book UI |
| [#167](https://github.com/matteopolak/lodestone/issues/167) | Advancements screen |
| [#183](https://github.com/matteopolak/lodestone/issues/183) | Ambient sound loops + client-predicted local sound triggers |
| [#49](https://github.com/matteopolak/lodestone/issues/49) | 26.2's real `sky_darken` timeline (dusk/dawn ramp only — plateaus already correct, explicitly low priority per its own issue) |
| [#18](https://github.com/matteopolak/lodestone/issues/18) | Five remaining `FluidRenderer` divergences |
| [#71](https://github.com/matteopolak/lodestone/issues/71) | Settle the crosshair-behind-screen question (research-only, low urgency) |

### Band 4 — atmosphere and immersion polish

Real, player-visible, but lower frequency or lower stakes than the bands above.

| issue | item |
|---|---|
| [#144](https://github.com/matteopolak/lodestone/issues/144) | Nausea / confusion FOV wobble |
| [#149](https://github.com/matteopolak/lodestone/issues/149) | Portal distortion |
| [#154](https://github.com/matteopolak/lodestone/issues/154) | Spyglass screen-space vignette — depends on #54/#57 |
| [#139](https://github.com/matteopolak/lodestone/issues/139) | Freeze overlay + the `freeze_ticks` mechanic (state does not exist at all yet, not just the overlay) |
| [#32](https://github.com/matteopolak/lodestone/issues/32) | Vanilla settings menu (GUI scale) |
| [#55](https://github.com/matteopolak/lodestone/issues/55) | Full Options screen tree |
| [#62](https://github.com/matteopolak/lodestone/issues/62) | Player skins (blocked on Tier 3 auth; elytra needs its own `ElytraModel`/`WingsLayer`, added by comment) |
| [#10](https://github.com/matteopolak/lodestone/issues/10) | Remote/mob swing animation (`EntityAnimation` unconsumed) |
| [#29](https://github.com/matteopolak/lodestone/issues/29) | Islands: pickup fly-to-player, mob equipment, drop stack count |
| [#186](https://github.com/matteopolak/lodestone/issues/186) | Third-person front-facing camera stage (back-view already landed) |
| [#185](https://github.com/matteopolak/lodestone/issues/185) | Pumpkin overlay |

### Band 5 — completeness

Tier 3: invisible in a screenshot, visible the moment a real player looks for it.

| issue | item |
|---|---|
| [#188](https://github.com/matteopolak/lodestone/issues/188) | Statistics screen |
| [#189](https://github.com/matteopolak/lodestone/issues/189) | Social interactions / player reporting — blocked on secure chat signing |
| [#190](https://github.com/matteopolak/lodestone/issues/190) | World creation screen (singleplayer already works via defaults without it) |
| [#192](https://github.com/matteopolak/lodestone/issues/192) | Credits / end-poem screen |
| [#195](https://github.com/matteopolak/lodestone/issues/195) | Chat settings screen (chat itself already works) |
| [#197](https://github.com/matteopolak/lodestone/issues/197) | F3 debug overlay parity (two-column layout, F3+B hitboxes, F3+G chunk borders, light-level pie) |
| [#198](https://github.com/matteopolak/lodestone/issues/198) | Sound subtitle captions (accessibility) |
| [#187](https://github.com/matteopolak/lodestone/issues/187) | Full Unicode text: unihex/TTF rasterization, bidi |

## What already exists — do not re-flag these

Confirmed landed by re-grepping the tree, not by trusting a doc. Listed because every one of
these is plausible to mistake for missing from a summary description alone:

- Vanilla GUI sprite atlas with nine-slice borders, real `ascii.png` font with proportional
  advances and gamma-space shadow, title/pause screens, container screens + click predictor,
  crafting with the real recipe corpus, item GUI geometry, armour at vanilla's four
  inflations, entity models/animations, dropped items, thrown projectiles, first-person arm
  with swing, per-face block occlusion, break particles with tint, day/night `sky_darken`
  (with a known 26.2-timeline divergence, #49), Nether/End fog colour presets, and
  keybindings — all as described in the original briefing.
- **Chat itself**: real text entry and scrollback (`crates/lodestone-shell/src/chat.rs`,
  `hud.rs:491-522`), a first-class `Screen::Chat` sub-mode. Only the *settings* screen is
  missing (#195).
- **Status effect icons, boss bar, scoreboard sidebar, tab list, and the action bar** are all
  **drawn today**, not merely modelled as ECS components: `hud.rs` has real draw calls for
  each (`effects.rs`; `hud.rs:705-709` boss bars; `hud.rs:730` scoreboard; `hud.rs:753` tab
  list; `hud.rs:294,669` action bar, pixel-gated). `docs/session-components.md`'s framing —
  "the scoreboard, tab list, boss bars and menus as ECS components" — describes the *data*
  layer accurately but reads, out of context, like these might not reach the screen. They do.
- **Item durability bars** are drawn (`crates/lodestone-shell/src/hud/item_icon.rs:161-170`,
  a real hue-lerped bar shared by the hotbar and container screens) — genuinely easy to miss
  with a shallow grep, which is exactly how it ended up on a "still needed" list once already.
- **Animated block/item textures** are real frame-cycling, not frame-0-only: `.mcmeta`
  animation metadata is fully parsed (`crates/lodestone-assets/src/texture.rs:167-224`) into a
  runtime `AnimTable` sampled every tick into a GPU uniform
  (`crates/lodestone-render/src/block_models.rs:934`), pixel-verified by
  `tests/animated_block_pixels.rs`.
- **Third-person camera switching** is landed and wired every frame
  (`crates/lodestone-shell/src/camera_rig.rs:103`, F5 → `Sim::toggle_third_person` →
  `render.set_third_person_body_source` in `app.rs::WindowApp::redraw`). Only vanilla's third
  (front-facing) F5 stage is missing — filed narrowly as #186, not as a full
  "third-person perspectives" rewrite.
- **The generic sound-event engine, and spatial/directional audio**, are both real: any named
  vanilla sound event resolves against the actual `sounds.json` at runtime
  (`crates/lodestone-assets/src/sound.rs`, `crates/lodestone-sound/src/driver.rs`), and panning
  + distance attenuation are implemented (`crates/lodestone-audio/src/spatial.rs`), not
  stubbed — an equal-power approximation of vanilla's HRTF, acknowledged as such in-repo. What
  is missing is *music*, *ambient loops*, *subtitle captions*, and *client-predicted local
  triggers* — each filed as its own issue above, not "audio" as one undifferentiated gap.

## Corrections found while grounding this roadmap

Per the task's own warning that a stale claim here has previously misdirected four issues at
once, everything above was re-verified against current source rather than against the
original briefing text. Two things in the briefing itself were wrong:

1. **"Animated textures (frame 0 only today)"** — false. Real cycling animation exists and is
   pixel-tested; see above. Whatever this claim was true of has since been fixed and the note
   was not updated.
2. **"Item durability bars"**, **"effect icons"**, **"boss bars, scoreboard, tab list"** listed
   under "still needed" — all four already render today (see above). Only the elements *not*
   listed there that this pass separately confirmed missing (subtitles, attack indicator,
   held-item tooltip, F3 richness) are genuine gaps.
3. **"Camera and post: … third-person perspectives"** listed as needing work — true only in a
   narrow sense (the front-facing F5 stage). The camera-mode switch itself is landed and
   currently wired; an earlier line inside `docs/third-person-player-body.md` itself claims a
   stale `None` for the render-state wiring, which is also now corrected by this pass — the
   doc's own later "Wired" section and the current `app.rs::WindowApp::redraw` call site already
   contradicted its own earlier sentence before this roadmap pass even started.

## Traps that apply across this whole area

Repeated here because they are easy to lose track of once work is split across ~50 issues —
each is attached to its specific issue too, but they recur:

- **The model shader is at wgpu's 4-bind-group floor** (camera/atlas/palette/anim). A fifth
  bind group validates on an 8-group adapter and is a startup crash on a 4-group one. Check
  the *limit*, not the adapter, before adding a group for any new effect (glint, tint,
  overlays).
- **Depth is `[0,1]` DirectX-style, not vanilla's reversed-Z.** Every ported depth comparison
  and bias flips sign.
- **The GUI winding invariant is negative**, derived from `Camera::view_projection()`'s own
  sign — do not assert a polarity from a screenshot that happens to look right.
- **Vanilla is not colour-managed**: tint and shade multiply in gamma space. Every new tint
  path filed above (glint, potion/spawn-egg tint, banner patterns, fire/hurt/underwater
  overlays) must follow `srgb_to_linear(linear_to_srgb(rgb) * tint * shade)`, not the linear
  form, or the result washes out — this has already gone wrong once per `CLAUDE.md`'s own
  validation log.
- **Never put a double quote inside a WGSL shader**, not even in a comment — the shaders live
  in Rust `r"…"` raw strings and a `"` ends the string early. Use backticks.
- **There are two meshers**: `--headless` renders through `mesh_simple`, live terrain through
  `mesh_models`. Any gate for a model-path effect (AO, glint, tint) verified only against
  `--headless` may be validated against the one scene that structurally cannot exercise it.
- **A square viewport draws 0 held-item pixels on a working build.** Any first-person gate
  (held item, spyglass vignette, view bobbing) written at the repo's usual 256×256 GPU-gate
  target will read as "does not render" even when it does — use a 16:9 target for these.
- **Nothing is done until a pixel changes**, with a negative control that must fail the same
  assertion. Several of the issues above are wiring fixes for data that already exists
  (nametags, glint, durability — before this pass corrected the last one) precisely because
  this defect class is the dominant one in this subsystem.

## Issue count and epic distribution

**34 new issues** filed by this pass, plus **25 pre-existing issues** already open in this
area (the ones named in the five bands above: #10, #17, #18, #22, #23, #24, #25, #28, #29,
#30, #32, #45, #49, #50, #51, #53, #54, #55, #57, #58, #60, #62, #71 — 23 distinct numbers,
two of which — #50 and #51 — are themselves already split off #28's family) — **59 issues**
total spanning this decomposition:

| epic | pre-existing | new | total |
|---|---|---|---|
| Tier 1 (#1) | #22, #23, #24, #25, #45, #54, #57, #58 (8) | #96, #98, #100, #103 (4) | 12 |
| Tier 1½ (#2) | #10, #17, #18, #29, #30, #53, #60 (7) | #108, #112, #117, #121, #126, #130, #135 (7) | 14 |
| Tier 2 (#3) | #28, #49, #50, #51, #71 (5) | #139, #144, #149, #154, #158, #163, #167, #171, #174, #178, #182, #183, #184 (13) | 18 |
| Tier 3 (#4) | #32, #55, #62 (3) | #185, #186, #187, #188, #189, #190, #192, #195, #197, #198 (10) | 13 |

That lands inside the ~40-70 issue guideline for the area as a whole, weighted toward Tier 2
(the container/GUI backbone and the visual long tail — inherently the largest single bucket)
and Tier 1½ (individually cheap, collectively "a renderer" vs. "the game").
