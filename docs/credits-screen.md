# Credits screen

## What it is

The scrolling end poem and credits roll (`Screen::Credits`) shown when the server announces the win after the player first leaves the End. Its text comes from the active resource pack at runtime; none is stored in this repository.

## How it works

1. The server (`connection_travel::begin_end_exit`, [nether-portals](./nether-portals.md)) sends game event 4. The 26.2 adapter decodes it to `ClientEvent::WinGame` (the parameter is ignored), which `net::forward` turns into `NetUpdate::WinGame`.
2. `Sim::poll_net` latches `won`. `WindowApp::drive_ui_from_session` calls `UiState::show_credits` then `WindowApp::begin_credits`, which loads `CreditsText` (`assets/minecraft/texts/end.txt`, `credits.json`, `postcredits.txt` via `resources::open_vanilla_pack_stack`, so packs can override) into `menu::credits::Credits` held in `MenuNav`. If none of the three files exist it skips to step 5.
3. Each frame `WindowApp::tick_credits` advances `Credits` by wall time and `render::frame_for` builds a `MenuLabel` per visible line (`render::credits_frame`), carrying legacy `§` colour codes and the obfuscation marker.
4. Layout and speed follow the original: 12 px line pitch, 256 px wrapped column, 0.5 px per 20 Hz tick (10 px/s), lines starting 150 px below the canvas. Poem lines substitute `PLAYERNAME` (`Sim::local_player_name`); the obfuscation marker becomes 3 to 6 scrambled glyphs; credits sections are centred yellow headings between `============` rules, titles grey, names white and indented. Space held is 5x, plus 15x per Control key held; Up scrolls backwards. The roll ends at `scroll > lines * 12 + 2 * canvas_height + 24`.
5. Ending (finished, or Escape via `MenuNav::key_credits`) goes through `MenuNav::finish_credits`, returning `MenuAction::FinishCredits`; the app calls `Sim::finish_credits`, which clears the latch and sends `ClientAction::Respawn`, and the server moves the player home. A headless `lodestone-client` with the automatic respawn policy answers `WinGame` at once; the shell uses the manual policy.

## How to change it

- Speeds, pitch and width are constants at the top of `crates/lodestone-shell/src/menu/credits.rs`. Wrapping uses real proportional font metrics (`credits::font_measure`, `VanillaFont::shared`), falling back to a fixed 6 px advance without a font.
- Held keys (Space, Up, left and right Control) are tracked in `WindowApp` (`credits_space_held`, `credits_up_held`, `credits_ctrl_left_held`, `credits_ctrl_right_held`) because the menu key path only sees presses; the two Controls each add to the speed-up.
- Drawn: "LODESTONE" title lettering at `height + 50 - scroll` (`Credits::logo_y`, `MenuFrame::credits_logo_y`) and the edge vignette.
- Backdrop: a roll with a poem uses `MenuBackdrop::EndPortal` (`gpu::end_portal::EndPortalRenderer::new_backdrop`/`draw_backdrop`, owned by `MenuRenderer`, reusing the block pass shader with an identity camera and clip-space quad on a clock started at its first frame). A credits-only roll uses the opaque backdrop plus tiled `menu_background` shifted up by half the scroll (`MenuFrame::background_scroll`). Without portal textures a flat fill shows.
- Vignette: the pack's `misc/credits_vignette` (`resources::CREDITS_VIGNETTE_TEXTURE`) multiplies the screen by `1 - texel` via the sprite shader's negative `VIGNETTE_TINT`; without it `draw_vignette` builds the falloff (about 39% black at the edge, gone a third of the way in) from stepped strips.
- Music: `audio::music::credits_situation` makes the credits track win over menu music.
- 26.3 differs from 26.2 only in the key-event API (shortcut key codes).
- Visual gate: `tests/hud/credits_screen_pixels.rs` (`--ignored`); `LODESTONE_CREDITS_PNG_DIR` dumps PNGs and `LODESTONE_ASSETS=.cache/benchmarks/vanilla-26.3-assets` uses the vanilla pack. The built-in pack's `menu_background` is flat, so background scroll is invisible with it.

## Configuration

None. A pack without the three text files makes the screen skip to the respawn.

## Dependencies

`serde_json`, `resources::open_vanilla_pack_stack`, the shell font's legacy colour-code support, and `lodestone-client`'s `ClientAction::Respawn`.
