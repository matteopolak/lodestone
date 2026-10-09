# Launch surfaces

## What it is

Launch surfaces choose where an interactive multiplayer session is presented: `window` (the normal wgpu window), `stdio` (a GPU-free chat and command stream) and `terminal` (the real game camera drawn as true-colour Unicode cells with no GUI window).

## How it works

`Config::from_args` maps `--surface window|stdio|terminal` onto the shell's launch `Mode`; protocol adapters and `lodestone-client` never know which surface consumes their events. Only the window surface leaves stdout to tracing: every other mode writes `RUST_LOG` tracing to stderr, and `LODESTONE_TRACE` keeps its own file.

**stdio.** Owns a `NetClient` directly, printing connection progress, chat, action-bar text and disconnects. A stdin reader turns each non-empty line into the GUI chat box's `ClientAction`: leading `/` sends a command (slash removed), anything else chat. `#quit` is local; EOF also closes, so it works in pipelines.

**terminal.** Builds the same `WindowApp` and calls the same `redraw` pipeline with a `HeadlessTarget` instead of a swapchain, so a launch without `--host` shows the real main menu and an in-game frame includes the full HUD, screens, entities, skin and effects. After the frame, the sRGB-encoded RGBA readback is wrapped in an in-memory `image::RgbaImage` and passed to `ratatui-image`'s forced primitive half-block protocol (two vertical pixels per `▀` cell, independent true-colour fg/bg), chosen over Kitty/Sixel/iTerm2 for stable output everywhere.

- Ratatui's cell diff still skips unchanged cells. The Crossterm backend is wrapped in `TerminalFrameWriter`, which collects the diff (cursor and style escapes included) until the backend flush and writes it as one batch, so the stdout line buffer never shows a frame row by row; it retains its allocation between frames.
- Ratatui reserves the last row for an input prompt and rasterizes the frame into the rest. When the tty reports physical window pixels the target is sized to the measured cell geometry (correct projection for cells that are not 1:2); zero dimensions use the 1:2 fallback. Resize rebuilds the target.
- Input: `W A S D`, space and modifiers feed `InputState`; digits select hotbar slots; `F5` changes camera; `T`, Enter or `/` focuses chat (Enter sends, Escape cancels, `/` uses `Sim::send_chat`). Mouse left/right/middle are attack, use/place and pick-item; the wheel cycles the hotbar, or on a menu/container forwards to that screen's list, creative grid, bundle, stonecutter or loom scroll; pointer movement and clicks use the shared menu hit-testing. Escape pauses or closes the container like the window; `Ctrl-C` exits. A short release timeout prevents stuck movement on terminals without key-release events; focus loss releases movement, mouse actions and hover.
- Chat is the one presentation exception: its GPU pixels are suppressed and `Sim`'s log (with fade ages, the shared `ChatInput` editor and history) is drawn as readable Ratatui text at the lower left. Hotbar and inventory stay in the shared GPU frame; `E` and Escape drive the same inventory state.

## How to change it

- New surface: add the name in `Config::from_args`, update `Config::usage`, add the exhaustive dispatch in `crate::run` and `crate::app::run`.
- stdio filtering or local commands: `crate::terminal::run_stdio`. Keep `/` server-bound and local controls in the reserved `#` namespace.
- Terminal layout is `crate::terminal::terminal_game_area`; keys `key_command`/`handle_key`; mouse `mouse_event_command`/`handle_mouse`; geometry `terminal_pixel_size`/`terminal_render_dimensions`; image conversion `halfblock_protocol`. Route UI events through `WindowApp`'s terminal adapter (`terminal_menu_key`, `terminal_pointer_moved`, `terminal_pointer_button`, `terminal_scroll`, `terminal_escape`) rather than mutating `Sim`. `TerminalSession::enable_input_capabilities` treats mouse capture, focus reporting and keyboard enhancement as optional; extend its cleanup state for any new mode.
- Keep the frame boundary in `TerminalFrameWriter` (a direct backend write reintroduces partial frames). Cover pure mappers and geometry with synthetic Crossterm events so tests need no TTY.
- Keep `ratatui-image` on the primitive half-block backend without Chafa or image-decoder features. Change the scene through `WindowApp::redraw`, `Sim` or `RenderState`, never by teaching the encoder about blocks or UI.

### Gotchas

- `terminal` needs stdin and stdout to be TTYs (raw input, alternate screen); use `stdio` when redirecting. It also still needs a GPU adapter: it means no window or swapchain, not software rendering. `stdio` is the GPU-free option.
- Terminals report absolute cell coordinates, so the client derives deltas between in-game cells and resets the anchor at pane boundaries, focus changes and resize. There is no portable pointer lock, so look is cell-granular; mouse input outside the game pane is ignored.
- Physical cell pixels come from the window-size query and are a best-effort aspect correction.
- Ratatui restores raw mode and the alternate screen through its panic hook; the surface disables only the capabilities it enabled on every exit path. Keyboard enhancement and focus reporting are terminal-dependent, so release timeouts and focus-loss cleanup stay as fallbacks.
- Terminal timing probes use `crate::platform::Instant` so the wasm confinement check covers the whole shell source.
- The image is hard to read at normal cell sizes but is the same framebuffer as the window; the player skin shows in detached (`F5`) cameras, first person shows only the arm.

## Configuration

```text
lodestone --surface stdio --host example.org --port 25565
lodestone --surface terminal --host example.org --port 25565
```

- `--surface <window|stdio|terminal>` (default `window`); `--host`, `--port`, `--protocol` select the server. Omitting `--host` on `terminal` starts the main menu; `stdio` is connection-only.
- `terminal` is enabled by the `window` rendering feature, not `multiplayer`, so a singleplayer-only build can navigate the menu (multiplayer row disabled by its feature flag).
- The live terminal size drives pane and GPU target sizes, with resize applied live.

## Dependencies

`lodestone-client` (`NetClient`), `Sim`, `lodestone-render` with wgpu's headless target, Ratatui with Crossterm, and `ratatui-image` (Chafa and image-decoder features off; `image` only wraps the RGBA buffer).
