# Client rendering and UI: remaining visual and audio surface

## What this is

This roadmap covers the player-visible 26.2 client work: scene rendering, GUI and HUD
surfaces, item and entity presentation, camera effects, audio, and text breadth. Client
physics and input, server simulation, protocol coverage, plugins, and benchmarks have
their own roadmaps; [`README.md`](./README.md) indexes them.

The work is ordered by visible impact per unit of effort. A feature is not complete until
live state reaches a draw or audio consumer and a focused gate observes the result.

## Roadmap

### Band 1 — high-impact wiring and scene fundamentals

| Feature | Required outcome |
|---|---|
| Enchantment glint | Propagate `ItemIcon` glint state to a shared shimmer pass instead of hard-coding it off. |
| Hurt flash and screen shake | Drive small screen-space effects from damage and explosion events. |

### Band 2 — ordinary survival play

- Weather: rain, snow, thunder, and their scene-state transitions.
- Block-entity renderers, beginning with chests and signs, then skulls, campfires,
  brewing stands, lecterns, banners, and shields.
- First-person presentation: held block, sprite, and special-item poses; bow and
  crossbow draw poses depend on that held-item path.
- Held-item and third-person view lag: retain smoothed yaw- and pitch-axis history behind
  head turns, apply it to the first-person hand pose and third-person body attachment, and
  leave `Sim::camera` as the unlagged world and interaction origin. Completion requires a
  rapid-turn gate that observes a nonzero, time-decaying hand/body offset and fails for an
  unlagged control.
- Entity layers: wool, skins, overlays, glowing eyes, and species-specific additions.
- HUD and overlays: underwater, fire, air bubbles, attack indicator, held-item name,
  and animated health, hunger, experience, and hotbar feedback.
- Item and text fidelity: leather dye, trims, and bold, italic, underline,
  strikethrough, and obfuscated text rendering.
- Music selection for menu, biome, and situational tracks.

### Band 3 — GUI backbone and visual breadth

- Finish the container-screen family: furnace, anvil, enchanting, brewing, loom,
  smithing, stonecutter, grindstone, cartography, beacon, villager, and horse screens.
- Correct 3D block-item presentation in containers and keep pack-backed screen art rather
  than replacing it with programmatic approximations.
- Add the creative inventory, recipe book, advancements, filled maps, item tints, and
  banner/shield pattern compositing.
- Expand particles into environmental and combat/event catalogues.
- Add ambient loops and locally predicted sound triggers.
- Refine fluid rendering divergences and the crosshair/screen depth rule.

### Band 4 — atmosphere and lower-frequency interaction

- Nausea/confusion field-of-view wobble, portal distortion, freeze and pumpkin overlays,
  plus the state that drives them.
- Spyglass vignette after the first-person item and draw-pose paths exist.
- GUI scale and the complete options hierarchy.
- Skin fidelity: browser-safe remote and selected-account texture fetches, remote cape preference
  and texture propagation, and the remaining cape/elytra texture relationship.
- Remote swing animation, pickup fly-to-player, visible mob equipment, dropped-stack
  count, and the front-facing third-person camera stage.

### Band 5 — completeness and accessibility

- Statistics, world creation, credits/end text, chat settings, and social/reporting UI.
- Rich debug-overlay controls and information layout.
- Sound subtitles and full Unicode text support, including unihex/TTF rasterization and
  bidirectional text.

## Existing foundations

Do not duplicate these capabilities when extending the roadmap:

- GUI sprite atlases, nine-slice borders, proportional bitmap-font advances, gamma-space
  text shadows, title/pause screens, crafting, click prediction, item GUI geometry,
  armor, entity models and animation, dropped items, thrown projectiles, first-person
  arm swing, block-face occlusion, tinted break particles, fog presets, and key bindings.
- The live `mesh_models` path supplies per-corner smooth light and ambient occlusion. Its
  remaining fidelity is face-shape-weighted interpolation for partial quads and applying
  per-state light emission to the model ambient-occlusion eligibility check.
- `SkyRenderer` draws the gradient disc, sunrise band, sun, moon, stars, fast and fancy
  clouds, fog-coloured clear, void fog, and dimension sky mode through the frame path.
  The remaining sky fidelity is decoding the server-supplied dimension sky selection instead of
  using the dimension-name fallback.
- Chat entry and scrollback in `lodestone_shell::chat`, plus rendered status effects,
  boss bars, scoreboards, tab lists, action bars, and item durability bars in the HUD.
- Nameplates consume player-list and custom-name state, honor visibility and team rules, and have
  focused pixel coverage in `nametag_pixels`.
- `Screen::Death` is a backdrop-aware overlay with manual Respawn and Title Screen actions; live
  respawn coverage proves a death waits for the selected action.
- Animated block and item textures through `AnimationMeta`, `AnimTable`, and
  `BlockModels::anim_slot_uniforms`; `animated_block_pixels` is the pixel gate.
- Particle-sheet atlas installation uses the same atlas as the UV table, with
  `sheet_particle_atlas_pixels` guarding the sampled texture.
- Third-person mode cycling from `Sim::cycle_camera_type` through
  `WindowApp::redraw`; only the front-facing stage remains.
- Elytra wings use `ElytraMesh`, the chest-slot draw gate, and an ignored pixel gate. Remaining
  fidelity is the per-state wing pose and optional cape-sheet selection, not missing geometry.
- Sound-event lookup, spatial panning, and distance attenuation. Music, ambient loops,
  subtitles, and locally predicted triggers are separate remaining features.

## Implementation constraints

- The model shader already consumes four bind groups. Check device limits before adding a
  new group; effects should reuse or extend existing bindings where possible.
- Depth is reversed-Z in `[0, 1]`: clear to `lodestone_render::DEPTH_CLEAR` (`0.0`), with
  nearer fragments having greater depth. Preserve this convention in comparisons and bias.
- GUI winding is an agreement with the camera transform: compare
  `sign(det(gui_ortho * gui_item_pose))` with `sign(det(Camera::view_projection()))`.
- Tint and shade multiply in gamma space. Use
  `srgb_to_linear(linear_to_srgb(rgb) * tint * shade)` for new tint paths.
- WGSL belongs in a crate's `src/shaders/` directory and is loaded with `include_str!`.
  Do not embed shader source in Rust.
- `mesh_simple` drives headless rendering while live terrain uses `mesh_models`; model-path
  effects need a model-path gate. First-person gates need a 16:9 viewport because a square
  target can legitimately contain no held-item pixels.

## How to change it

Start at the state producer, trace it through render extraction or audio dispatch, and name
the final consumer before adding fields or decoding packets. For a visual feature, add a
focused pixel gate that reports a bounding box and a negative control; for audio, verify
event selection and spatial parameters independently. Keep related effects on shared paths
(item tint, text layout, particle atlas, and HUD geometry) rather than introducing an
isolated renderer.

## Configuration and dependencies

Rendering depends on `lodestone-render`, assets, and the shell's render loop; audio depends
on asset sound resolution and `lodestone-audio`. Resource packs supply sprites, texture
animation metadata, and sounds. Camera and HUD work consumes `lodestone-client` session
state; account-backed skins depend on the authentication roadmap.
