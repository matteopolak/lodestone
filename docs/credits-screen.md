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
- Text wrapping measures with the fixed-advance font measure (`render::text_px`), not the proportional font, so wrapped lines are slightly narrower than the 256 px column.
- Held-key state (Space, Up, Control) is tracked in `WindowApp` (`credits_space_held`, `credits_up_held`, `ctrl_held`) because the menu key path only sees presses. Left and right Control are not distinguished, so Control counts once.
- Not drawn: the logo, the vignette, the end-sky backdrop (the frame uses the opaque backdrop) and the credits music.

## Configuration

None. The three text files come from the resource pack stack; a pack without them makes the screen skip to the respawn.

## Dependencies

`serde_json` (credits parse), `resources::open_vanilla_pack_stack`, the shell font's legacy colour-code support, `lodestone-client`'s `ClientAction::Respawn`.
