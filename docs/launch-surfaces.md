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

The `terminal` path owns a normal `Sim`, connects it to the requested server, and advances it with the
same fixed-timestep logic as the windowed client. Completed terrain meshes are uploaded to
`RenderState`, whose ordinary world pass draws into `HeadlessTarget`. The resulting RGBA frame is not a
separate map renderer: it is the actual game camera output. The linear offscreen values are
sRGB-encoded, wrapped in an in-memory `image::RgbaImage`, and passed to `ratatui-image`'s primitive
half-block protocol. That protocol packs two vertical pixels into each `▀` cell with independent
true-colour foreground and background. It is forced instead of terminal-specific Kitty, Sixel, or
iTerm2 image protocols so `--surface terminal` has stable Unicode output everywhere.

Ratatui splits the current terminal dimensions into a chat pane on the left, game pane on the right,
and input pane along the bottom. The game pane uses the real render camera and the same local-body,
first-person-hand, and skin sources as the window surface, so `F5` draws the signed-in body and the
first-person arm uses its skin sheet. When the tty reports physical window pixels, the headless target is
sized to the game pane's measured cell width and height; this makes the camera projection correct for
terminals whose cells are not exactly 1:2. The half-block protocol still emits two vertical source
pixels per cell and downsamples the physical frame to the pane's cell size. Terminals that report zero
pixel dimensions use the protocol's 1:2 fallback. A resize rebuilds the headless target to the game
pane's new geometry. In game focus, `W`, `A`, `S`, `D`, space, and the modifier keys feed the shared
`InputState`; number keys select hotbar slots, `F5` changes the camera, and `T`, Enter, or `/` focuses
chat. Enter sends, Escape cancels, and a leading `/` uses `Sim::send_chat`'s normal command path.
Mouse left/right/middle buttons invoke attack, use/place, and pick-item, while the wheel cycles the
hotbar. `Ctrl-C` exits. A short release timeout prevents movement sticking on terminal emulators that
do not report key-release events, and focus loss releases movement and active mouse actions.

The terminal draws its hotbar natively as a compact nine-slot row over the camera pane: terminal cells
cannot preserve the window GUI's pixel-sized item art at a normal TTY resolution. `E` opens a native
inventory grid from the active server menu or the local player menu; a server-opened menu also raises
that grid automatically. Left and right click send ordinary pickup clicks, Shift-left-click sends a
quick move, and `1` through `9` swap the hovered grid slot with that hotbar slot. `E` or Escape closes
the terminal view and, for a server menu, sends the regular close request.

## How to change it

- Add a surface name in `Config::from_args`, update `Config::usage`, then add the exhaustive dispatch in
  both `crate::run` and `crate::app::run`.
- Change stream filtering or local commands in `crate::terminal::run_stdio`. Keep `/` server-bound;
  local controls use the reserved `#` namespace so chat cannot be mistaken for a client command.
- Change terminal layout in `crate::terminal::surface_areas`, keyboard mapping in
  `crate::terminal::key_command`, mouse mapping in `crate::terminal::mouse_event_command`, event
  effects in `crate::terminal::handle_key`/`crate::terminal::handle_mouse`, target geometry in
  `crate::terminal::terminal_pixel_size`/`crate::terminal::render_dimensions`, and image conversion
  in `crate::terminal::halfblock_protocol`. Keep the pure mappers and geometry helpers covered with
  synthetic Crossterm events or measured dimensions so behavior stays deterministic without a TTY.
- Keep the `ratatui-image` primitive half-block backend free of Chafa and image decoder features. The
  surface starts from raw RGBA bytes, so those dependencies add no capability here.
- Change the rendered scene through `Sim` or `RenderState`, not by teaching the terminal encoder about
  blocks. A second scene implementation would drift from the window.
- Change the native terminal inventory in `crate::terminal::terminal_menu`,
  `terminal_inventory_text`, `inventory_slot_at`, and `handle_inventory_mouse`. Keep its click route
  on `ClientHandle::menu_click`; the client-owned menu predictor must produce the changed-slot list and
  state id, never a hand-built container packet.

### Gotchas

The terminal surface requires both stdin and stdout to be TTYs because Ratatui uses raw input and the
alternate screen. Use `--surface stdio` when redirecting either side. Crossterm requests SGR mouse
reporting and all-motion tracking, but terminals still report absolute cell coordinates rather than
raw relative or pixel motion. The client derives deltas between in-game cells, resets the anchor at
pane boundaries and resize, and cannot provide true pointer lock; look therefore remains cell-granular
and depends on the terminal emulator delivering mouse-move/drag events. Mouse input outside the game
pane is ignored so clicks on chat and the status chrome cannot change gameplay.

The physical cell fields come from the terminal's window-size query and are not required by the tty
interface. They are therefore a best-effort aspect correction, not a pixel-perfect display contract;
the fallback remains deterministic and portable.

The terminal hotbar and inventory are textual overlays rather than a downsampled copy of the window GUI.
They deliberately expose item ids/counts and menu-slot indices compactly so their click targets remain
usable on a small terminal. The player skin is visible as a full body only in detached (`F5`) camera
modes; first person correctly exposes only the skinned arm.

Ratatui restores raw mode and the alternate screen through its panic hook. The terminal surface also
disables mouse capture, focus reporting, and keyboard enhancement flags on every normal or unwinding
exit. Keyboard enhancement is terminal-dependent, so release timeouts and focus-loss cleanup remain
necessary fallbacks.

The Unicode renderer still needs a GPU adapter: "terminal" means no window or swapchain, not software
rendering. The `stdio` surface is the option for a genuinely GPU-free session.

## Configuration

```text
lodestone --surface stdio --host example.org --port 25565
lodestone --surface terminal --host example.org --port 25565
```

- `--surface <window|stdio|terminal>` selects the presentation path. The default remains `window`.
- `--host`, `--port`, and `--protocol` select the server for both terminal surfaces.
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
