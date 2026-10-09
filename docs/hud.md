# The HUD

## What it is

The in-game HUD: vitals cluster (hotbar, XP bar, hearts, hunger, armour, air, action bar), vitals animations, the Tab player list, the scoreboard sidebar, the spectator selector and the held-item name. Unlike menu screens ([`ui-framework.md`](./ui-framework.md)) it is not a widget tree: layout is a pure function of live game state, rebuilt every frame. Toasts share a separate draw pass.

## How it works

### Layout

- Everything is in logical-canvas pixels: the framebuffer divided by the integer GUI scale, the same space the reference client uses.
- Two absolute anchors drive the cluster: the hotbar flush with the bottom, and the health/hunger baseline `vitals_line_base` at `height - 39`. Every other row (armour, air, XP bar and level, action bar, item name) is an offset from one of these, never stacked on the row below, so a row's position does not depend on which other rows exist.
- The air-bubble row looks mis-offset but is a sum of three reference terms that cancel to one flat offset for an unmounted player. Reproduce the full derivation.
- Pixel gates must call the same anchor function the draw uses, not restate a literal.

### Hotbar and game mode

- `lodestone_game::player_state::HotbarSlot` is one of nine slots. `HudState` validates the raw `i32` from `ClientEvent::HeldSlotChanged` once; invalid values keep the current selection. Keep raw integers at protocol and UI boundaries.
- The game mode comes from `Sim::game_mode` (the ECS `ServerGameMode`). `HudFrame::apply_game_mode` hides the hotbar, icons, cooldowns, item label, crosshair and vitals in spectator mode. Do not infer mode from flight abilities.

### Spectator selector

- Stays in `Screen::Playing` with the cursor captured. Pick-item (middle mouse) or a number key opens an unselected nine-slot bar; pressing the selected number again, or pick-item, activates it.
- Root categories list non-spectator players (UUID-sorted) and teams with an eligible member. Seven targets per first page; later pages reserve slot 1 for Previous, show six targets, Next in 8, Close in 9. A teleport emits `MenuAction::TeleportToEntity`.
- `SpectatorMenuState` refreshes the roster during session reconciliation and yields a `SpectatorHotbarView` per draw (`hud/spectator.rs`), using GUI sprites with a procedural fallback. It closes after 5 idle seconds (fading over the last 2). With it closed, the wheel adjusts flight speed by 0.005, clamped to `0..=0.2`.
- Interaction and roster rules: `menu/spectator_menu.rs`; input routing: `app/input.rs`, `app/lifecycle.rs`.

### Text

- All HUD text uses the real proportional font (from the client jar, with a fixed-width fallback when absent). Advances come from each glyph's rasterised coverage. Glyphs draw as merged quad runs on the ordinary colour vertex stream: no atlas, texture or extra bind group, because the model shader is at the four-bind-group floor.
- Bold, italic (per-texel-row shear), underline, strikethrough and obfuscated all emit real geometry. The shadow is a second pass at 25% of the colour, computed in gamma space (linear space gives a visibly lighter outline). Measure with the font that will draw.
- Each surface has its own absolute scale: title 4x, subtitle 2x, action bar, item name, F3, tab list and sidebar 1x of the logical canvas. There is no ambient multiplier and no per-surface size option. The chat scale option applies only to the scrollback log, not the input line, caret or suggestions.

### Vitals animations

Four cosmetic client-only animations, each a pure function of a wall-clock tick counter (50 ms steps). Exact reference RNG sequences are not reproduced, only the distribution.

- **Regeneration wave**: lifts one container 2 px per display tick while regeneration is active; index wraps at `ceil(max health + 5)` half-points. Independent of blink and jitter; derived from the active-effects projection and passed as its own offset, not folded into `HeartAnim`.
- **Heart blink**: a health change opens a window (20 ticks damage, 10 heal) with blinking sprites and a ghost overlay of the old total.
- **Low-health jitter**: per-frame vertical jitter below a threshold.
- **Hunger wobble**: level-triggered (not change-triggered) on ticks divisible by a food-derived interval, when saturation is empty.
- **Hotbar item pop**: when an item's identity changes or its count rises, the flat sprite squashes about a pivot that is not the icon centre on one axis. 3-D icons, durability bars and counts stay undistorted.

Health fill uses an integer ceiling of the raw float, so fractional health never shows an empty row. Projection helpers and atlas draws live in `hud/vitals.rs`; `hud.rs` orchestrates frames.

### Tab list

- Three layers: folded server state, a per-frame projection (styled names, vanilla count limit, ping mapped to six sprite bands), and geometry in `hud/tab_panel.rs` (re-exported as `hud::TabPanel`; extra columns past twenty players).
- Names colour from an explicit display-name component or from scoreboard team colour; check both, the team path is more common. Only listed players show; chat tab-completion reads the unfiltered roster.
- Protocol 5 has no UUID: its rows live in a separate name-keyed map with `profile.id = None`. They render, sort and complete normally, but are absent from social-interaction and spectator-teleport menus and use the default skin. `ClientHandle::players` preserves the absence; handle `None`.

### Scoreboard sidebar and item name

- Sidebar: right edge, title plus rows. Its bottom sits slightly above true centre by design (`height/2 + panelHeight/3`); do not centre symmetrically. The label-to-score spacer exists only when a row has a score.
- Item name: restarts when the selected item's identity changes, not when the slot changes. Full opacity, fading only in the last ten ticks; custom-named items are forced italic. Colour follows built-in rarity unless the custom name sets one.

## How to change it

- Derive offsets from a named anchor, never a restated pixel constant.
- A text surface that is uniformly 2x off at every GUI scale has a stray multiplier; the logical-canvas math is rarely at fault.
- Font metrics and inter-row gaps are different quantities; do not derive one from the other.
- Keep the glyph-ink cache independent of caller state (tint, position, scale, pose, shadow).
- HUD text gates must compare rendered pixels; correct glyphs with wrong advances pass any string or vertex-count check.
- When an animated value also drives a colour transition, render the "before" state first so the first observed value is not treated as a rising edge.

## Configuration

No config file. `gui_scale` (via `logical_canvas`) is the one general size control; the chat scale option affects only the scrollback log. Do not add size options for other surfaces.

## Dependencies

- `crates/lodestone-shell/src/hud.rs` and `hud/{anim,item_icon,spectator,tab_panel,toasts,vanilla_font,vitals}.rs`; `tablist.rs`, `scoreboard.rs`, `effects.rs` in the shell.
- `menu/spectator_menu.rs`, `sim/session.rs` and the app input layers.
- `lodestone-game`: `tablist::TabList`, `scoreboard::Scoreboard`, `player_state::HeldItemHighlight`.
- `lodestone-assets::font` for glyph metrics; `lodestone-model` for `Text`/`TextStyle`.
- [`ui-framework.md`](./ui-framework.md), the menu model this HUD deliberately does not use.
