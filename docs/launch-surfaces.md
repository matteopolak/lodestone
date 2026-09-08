# Launch surfaces

## What it is

Launch surfaces choose where an interactive multiplayer session is presented. `window` is the normal
wgpu window, `stdio` is a GPU-free chat and command stream, and `terminal` draws the real game camera as
true-colour Unicode cells without creating a GUI window.

## How it works

`Config::from_args` maps `--surface window`, `--surface stdio`, and `--surface terminal` onto the shell's
launch `Mode`. This keeps the choice at the process boundary; protocol adapters and `lodestone-client`
do not know which surface consumes their events.

The `stdio` path owns a `NetClient` directly. It prints connection progress, chat, action-bar text, and
disconnects. A background stdin reader turns each non-empty line into the same `ClientAction` produced
by the GUI chat box: a leading `/` sends a command without the slash, and any other line sends chat.
`#quit` is local and closes the surface. EOF also closes it, making the mode useful in pipelines.

Only the window surface leaves stdout available to process tracing. All non-window modes write tracing
selected by `RUST_LOG` to stderr, while `LODESTONE_TRACE` continues to write its separate trace file.

The `terminal` path constructs the same `WindowApp` and calls the same `redraw` pipeline as the window
surface, but gives it a `HeadlessTarget` instead of a swapchain. This means a launch without `--host`
stays on the real main menu (including its buttons and panorama), and an in-game frame includes the
same HUD, hotbar, inventory/container screens, entities, player skin, effects, and overlays. The only
terminal-specific step happens after the frame is complete: its RGBA readback is sRGB-encoded, wrapped
in an in-memory `image::RgbaImage`, and passed to `ratatui-image`'s primitive half-block protocol.
That protocol packs two vertical pixels into each `▀` cell with independent true-colour foreground and
background. It is forced instead of terminal-specific Kitty, Sixel, or iTerm2 image protocols so
`--surface terminal` has stable Unicode output everywhere.

Ratatui still performs its normal previous/current cell diff, so unchanged cells are not serialized.
The Crossterm backend is wrapped in `TerminalFrameWriter`, which collects that diff (including cursor
and style escape sequences) until the backend flush at the end of the draw. The collected bytes are then
written and flushed as one presentation batch; this prevents the stdout line buffer from exposing a large
frame one row at a time. The writer retains its allocation between frames, and it does not change the
cell diff or the rendered pixels.

Ratatui reserves the final row for a small input prompt and rasterizes the complete game frame into the
remaining terminal area. When the tty reports physical window pixels, the headless target is sized to
the measured cell geometry; this makes the camera projection correct for terminals whose cells are not
exactly 1:2. The half-block protocol still emits two vertical source pixels per cell and downsamples the
physical frame to the terminal's cell size. Terminals that report zero pixel dimensions use the
protocol's 1:2 fallback. A resize rebuilds the shared headless target to the new geometry. In game focus,
`W`, `A`, `S`, `D`, space, and the modifier keys feed the shared
`InputState`; number keys select hotbar slots, `F5` changes the camera, and `T`, Enter, or `/` focuses
chat. Enter sends, Escape cancels, and a leading `/` uses `Sim::send_chat`'s normal command path. The
chat log is the one stored by `Sim` and its ages drive the terminal overlay's fade. Chat is the one
intentional presentation exception: its shared GPU pixels are suppressed, then the same text and age
window are composited as readable Ratatui lines at the lower-left of the game frame, so chat is not
duplicated or made unreadable by rasterization. The input line and history use the shared `ChatInput`
edit box and history store as well.
Mouse left/right/middle buttons invoke attack, use/place, and pick-item, while the wheel cycles the
hotbar during play. On a menu or container screen, the same wheel is forwarded to that screen's list,
creative grid, bundle, stonecutter, or loom scroll action; pointer movement updates the shared hover
state and left/right/middle clicks use the shared menu/container hit-testing path. `Escape` pauses
play or closes the active container, matching the window action. `Ctrl-C` exits. A short release
timeout prevents movement sticking on terminal emulators that do not report key-release events, and
focus loss releases movement, active mouse actions, and the stored hover pointer.

The terminal does not draw a substitute hotbar or inventory: those are part of the shared GPU frame and
therefore remain pixel-identical before rasterization. `E` and Escape still use the terminal input
adapter to open and close the same inventory/container state; clicks are delivered to the shared game
state.

## How to change it

- Add a surface name in `Config::from_args`, update `Config::usage`, then add the exhaustive dispatch in
  both `crate::run` and `crate::app::run`.
- Change stream filtering or local commands in `crate::terminal::run_stdio`. Keep `/` server-bound;
  local controls use the reserved `#` namespace so chat cannot be mistaken for a client command.
- Change terminal layout in `crate::terminal::terminal_game_area`, keyboard mapping in
  `crate::terminal::key_command`, mouse mapping in `crate::terminal::mouse_event_command`, and
  route UI events through `WindowApp`'s terminal adapter (`terminal_menu_key`,
  `terminal_pointer_moved`, `terminal_pointer_button`, and `terminal_scroll`) rather than mutating
  `Sim` directly. Keep terminal Escape on `WindowApp::terminal_escape` so pause/container close
  semantics stay aligned with the window path. `TerminalSession::enable_input_capabilities` treats
  mouse capture, focus reporting, and keyboard enhancement flags as optional capabilities; extend
  its cleanup state if another terminal mode is added.
  Gameplay effects in `crate::terminal::handle_key`/`crate::terminal::handle_mouse`, target geometry in
  `crate::terminal::terminal_pixel_size`/`crate::terminal::terminal_render_dimensions`, and image conversion
  in `crate::terminal::halfblock_protocol`. Keep the frame boundary in
  `crate::terminal::TerminalFrameWriter`: a direct write from the backend would reintroduce
  partial-frame presentation. Keep the pure mappers and geometry helpers covered with synthetic
  Crossterm events or measured dimensions so behavior stays deterministic without a TTY.
- Keep the `ratatui-image` primitive half-block backend free of Chafa and image decoder features. The
  surface starts from raw RGBA bytes, so those dependencies add no capability here.
- Change the rendered scene through `WindowApp::redraw`, `Sim`, or `RenderState`, not by teaching the
  terminal encoder about blocks or UI. The terminal encoder must remain a final framebuffer adapter.

### Gotchas

The terminal surface requires both stdin and stdout to be TTYs because Ratatui uses raw input and the
alternate screen. Use `--surface stdio` when redirecting either side. Crossterm requests SGR mouse
reporting and all-motion tracking, but terminals still report absolute cell coordinates rather than
raw relative or pixel motion. The client derives deltas between in-game cells, resets the anchor at
pane boundaries, focus changes, and resize, and cannot provide true pointer lock: there is no portable
terminal escape sequence that confines the host OS cursor or warps it back into the terminal. Look
therefore remains cell-granular and depends on the terminal emulator delivering mouse-move/drag
events while its window is under the pointer. Mouse input outside the game pane is ignored so clicks
on chat and the status chrome cannot change gameplay.

The physical cell fields come from the terminal's window-size query and are not required by the tty
interface. They are therefore a best-effort aspect correction, not a pixel-perfect display contract;
the fallback remains deterministic and portable.

The terminal image is necessarily difficult to read at normal cell sizes, but it is the same framebuffer
as the window surface. The player skin is visible as a full body in detached (`F5`) camera modes; first
person correctly exposes only the skinned arm.

When no `--host` is supplied, the terminal starts on the same main-menu screen as a window. Crossterm
cells are converted to framebuffer coordinates and menu/container clicks are handled by `WindowApp`'s
shared hit-testing and navigation. Only chat is an intentional presentation exception: its shared log,
fade timing, history, editor state, span colours, and basic formatting flags are rendered as readable
native terminal text over the game pane.

Ratatui restores raw mode and the alternate screen through its panic hook. The terminal surface also
disables only the mouse capture, focus reporting, and keyboard enhancement capabilities it successfully
enabled, on every normal or unwinding exit. Keyboard enhancement and focus reporting are terminal-
dependent; unsupported terminals continue with basic keyboard input, so release timeouts and
focus-loss cleanup remain necessary fallbacks. OS-level cursor confinement would require a
platform-specific accessibility/automation API and is intentionally not attempted by this portable
surface.

The Unicode renderer still needs a GPU adapter: "terminal" means no window or swapchain, not software
rendering. The `stdio` surface is the option for a genuinely GPU-free session.

## Configuration

```text
lodestone --surface stdio --host example.org --port 25565
lodestone --surface terminal --host example.org --port 25565
```

`terminal` is enabled by the `window` rendering feature and does not require the `multiplayer` feature.
Without a host it can therefore show and navigate the main menu in a singleplayer-only build; the
multiplayer row remains disabled by that build's normal feature flag.

- `--surface <window|stdio|terminal>` selects the presentation path. The default remains `window`.
- `--host`, `--port`, and `--protocol` select the server when a terminal session is requested. Omitting
  `--host` on `terminal` starts the shared main-menu scene; `stdio` remains connection-only.
- `RUST_LOG` tracing goes to stderr for every non-window mode, so it cannot corrupt a surface's stdout
  protocol or alternate-screen output; `LODESTONE_TRACE=<path>` remains a file sink.
- The live terminal size controls the pane and GPU target sizes; when available, the terminal's
  physical window dimensions also control the target aspect. Resize events take effect while the
  client is running. Pixel dimensions reported as zero use the half-block 1:2 fallback.

## Dependencies

- `lodestone-client` through `NetClient` for network events and outbound chat/command actions.
- `Sim` for the authoritative interactive game state used by the Unicode surface.
- `lodestone-render` and wgpu's headless target for real game frames without a window.
- Ratatui and its Crossterm backend for layout, raw keyboard events, resizing, mouse capture, and safe
  alternate-screen restoration.
- `ratatui-image` for maintained true-colour Unicode half-block conversion, with its Chafa and image
  decoder features disabled; `image` only wraps the in-memory RGBA buffer.
