# Client rendering and UI: remaining visual and audio surface

## What it is

The roadmap for player-visible 26.2 client work: scene rendering, GUI and HUD, item and entity presentation, camera effects, audio and text breadth. Physics, server, protocol, plugins and benchmarks have their own roadmaps ([README](./README.md)). A feature is complete only when live state reaches a draw or audio consumer and a focused gate observes the result.

## Roadmap

Ordered by visible impact per unit of effort.

**Band 1: wiring and scene fundamentals**
- Enchantment glint: propagate `ItemIcon` glint state to a shared shimmer pass.
- Hurt flash and screen shake from damage and explosion events.

**Band 2: ordinary survival play**
- Weather: rain, snow, thunder and their transitions.
- Block-entity renderers: chests and signs first, then skulls, campfires, brewing stands, lecterns, banners, shields.
- First-person presentation of held block, sprite and special items; bow and crossbow draw poses depend on it.
- Held-item and third-person view lag: smoothed yaw/pitch history applied to the hand pose and body attachment, with `Sim::camera` staying the unlagged origin. Needs a rapid-turn gate showing a nonzero, decaying offset and failing for an unlagged control.
- Entity layers (wool, skins, overlays, glowing eyes, species extras); HUD overlays (underwater, fire, air bubbles, attack indicator, held-item name, animated bars); leather dye, trims and bold/italic/underline/strikethrough/obfuscated text; music selection.

**Band 3: GUI backbone and breadth**
- Finish container screens (furnace, anvil, enchanting, brewing, loom, smithing, stonecutter, grindstone, cartography, beacon, villager, horse); keep pack-backed art and correct 3D block items.
- Creative inventory, recipe book, advancements, filled maps, item tints, banner and shield patterns.
- Environmental and combat particles, ambient loops, locally predicted sounds, fluid divergences, the crosshair/screen depth rule.

**Band 4: atmosphere and lower-frequency interaction**
- Nausea wobble, portal distortion, freeze and pumpkin overlays plus driving state; spyglass vignette; GUI scale and the full options hierarchy.
- Skin fidelity (browser-safe remote and account texture fetches, cape preference, cape/elytra relation).
- Remote swing animation, pickup fly-to-player, visible mob equipment, dropped-stack count, the front-facing third-person stage.

**Band 5: completeness and accessibility**
- Statistics, world creation, credits/end text, chat settings, social and reporting UI, richer debug overlay, sound subtitles, full Unicode text (unihex/TTF, bidirectional).

## Existing foundations (do not duplicate)

GUI atlases and nine-slice borders, bitmap font and shadows, title/pause screens, crafting, item GUI geometry, armour, entity models and animation, dropped items, projectiles, first-person swing, tinted break particles, fog presets, key bindings, chat, status effects, boss bars, scoreboards, tab list, action bars, durability bars, nameplates, the death screen, animated block and item textures, `SkyRenderer` (disc, sunrise, sun, moon, stars, clouds, void fog), third-person cycling, elytra geometry, sound lookup with panning and attenuation. Open fidelity gaps among them:

- `mesh_models` lacks face-shape-weighted interpolation for partial quads and per-state emission in its ambient-occlusion eligibility.
- Sky mode still uses the dimension-name fallback rather than the server's dimension sky selection.
- Elytra needs per-state wing pose and optional cape-sheet selection.

## How to change it

Start at the state producer, trace through render extraction or audio dispatch, and name the final consumer before adding fields or decoding packets. For visuals, add a pixel gate reporting a bounding box plus a negative control; for audio, verify event selection and spatial parameters separately. Reuse shared paths (item tint, text layout, particle atlas, HUD geometry).

Constraints (details in [architecture](../architecture.md)): the model shader already uses four bind groups; depth is reversed-Z (`lodestone_render::DEPTH_CLEAR` is `0.0`); GUI winding must match the camera transform's determinant sign; tint and shade multiply in gamma space; WGSL lives in `src/shaders/` via `include_str!`. `mesh_simple` drives headless rendering while live terrain uses `mesh_models`, so model-path effects need a model-path gate, and first-person gates need a 16:9 viewport.

## Configuration and dependencies

`lodestone-render`, assets and the shell render loop; `lodestone-audio` plus asset sound resolution for audio; resource packs for sprites, animation metadata and sounds; `lodestone-client` session state for camera and HUD; the authentication roadmap for account skins.
