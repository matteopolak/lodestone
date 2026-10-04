# Credits screen

## What it is

The scrolling end poem and credits roll (`Screen::Credits`) that the client shows when the server announces the win after the player first leaves the End. Its text is read from the active resource pack at runtime; nothing of it is stored in this repository.

## How it works

Chain, packet to pixels:

1. The server (`connection_travel::begin_end_exit`, see [`nether-portals.md`](./nether-portals.md)) sends game event 4. The 26.2 adapter decodes it to `ClientEvent::WinGame` (the parameter is ignored, as the real client ignores it), which `net::forward` turns into `NetUpdate::WinGame`.
2. `Sim::poll_net` latches `won`. `WindowApp::drive_ui_from_session` calls `UiState::show_credits` and then `WindowApp::begin_credits`, which loads `CreditsText` (`assets/minecraft/texts/end.txt`, `credits.json`, `postcredits.txt` through `resources::open_vanilla_pack_stack`, so a user pack can override them) and lays it out as `menu::credits::Credits`, held in `MenuNav`. If none of the three files exist, `begin_credits` skips straight to step 5.
3. Each frame `WindowApp::tick_credits` advances `Credits` by wall time and `render::frame_for` builds one `MenuLabel` per visible line (`render::credits_frame`). The labels carry legacy `§` colour codes and the obfuscation marker; the font draws them.
4. Layout and speed follow the original screen: 12 px line pitch, a 256 px wrapped column, 0.5 px per 20 Hz tick (10 px/s), lines start 150 px below the canvas. Poem lines substitute `PLAYERNAME` (from the tab list via `Sim::local_player_name`); the two speakers' colours are the codes in the text; the obfuscation marker becomes 3 to 6 scrambled glyphs; credits sections are centred yellow headings between `============` rules, titles grey, names white and indented. Space held is 5x, plus 15x per Control key held; Up held scrolls backwards. The roll ends when `scroll > lines * 12 + 2 * canvas_height + 24`.
5. Ending (scroll finished, or Escape via `MenuNav::key_credits`) goes through `MenuNav::finish_credits`, which closes the screen and returns `MenuAction::FinishCredits`; the app calls `Sim::finish_credits`, which clears the latch and sends `ClientAction::Respawn` (the perform-respawn client command). The server then moves the player home.

A headless client (`lodestone-client` with the default automatic respawn policy) answers `WinGame` with the respawn at once; the shell uses the manual policy and answers when the screen closes.

## How to change it

- Speeds, line pitch and column width are constants at the top of `crates/lodestone-shell/src/menu/credits.rs`.
- Text wraps with the real proportional font metrics (`credits::font_measure`, backed by `VanillaFont::shared`) at the 256 px column, so line breaks match the original; without a loaded font it falls back to the fixed 6 px advance.
- Held-key state (Space, Up, left and right Control) is tracked in `WindowApp` (`credits_space_held`, `credits_up_held`, `credits_ctrl_left_held`, `credits_ctrl_right_held`) because the menu key path only sees presses. The two Control keys are separate and each adds to the speed-up, as in the original.
- Drawn: the wordmark plus edition strip at `height + 50 - scroll` (`Credits::logo_y`, `MenuFrame::credits_logo_y`) and the edge vignette.
- Backdrop: a roll with a poem uses `MenuBackdrop::EndPortal`, the moving end-portal star field filling the screen (`gpu::end_portal::EndPortalRenderer::new_backdrop` / `draw_backdrop`, owned by `MenuRenderer`; it reuses the block pass's shader with an identity camera and a clip-space quad, and runs on a clock started at its first frame). A credits-only roll uses the opaque backdrop plus the tiled `menu_background` shifted up by half the scroll (`MenuFrame::background_scroll`). Without the portal textures the flat fill shows.
- Vignette: the pack's `misc/credits_vignette` texture (a loose menu-atlas texture, `resources::CREDITS_VIGNETTE_TEXTURE`) multiplies the screen by `1 - texel`; the sprite shader does this for the negative `VIGNETTE_TINT`. When the pack lacks the texture (the built-in one does), `draw_vignette` builds the falloff (about 39% black at the edge, gone a third of the way in) from stepped strips.
- Music: `audio::music::credits_situation` names the credits track as the screen music, so it wins over menu music while the roll is up.
- 26.3 differs from 26.2 only in the key-event API (shortcut key codes); the win-game flow, packets and credits-seen handling are identical.
- Visual gate: `tests/hud/credits_screen_pixels.rs` (`--ignored`) renders the roll headlessly; set `LODESTONE_CREDITS_PNG_DIR` to dump PNGs and `LODESTONE_ASSETS=.cache/benchmarks/vanilla-26.3-assets` to run against the vanilla pack. The built-in pack's `menu_background` is a flat colour, so the tiled background's scroll is not visible in it.

## Configuration

None. The three text files come from the resource pack stack; a pack without them makes the screen skip to the respawn.

## Dependencies

`serde_json` (credits parse), `resources::open_vanilla_pack_stack`, the shell font's legacy colour-code support, `lodestone-client`'s `ClientAction::Respawn`.
