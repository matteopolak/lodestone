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

The `terminal` path owns a normal `Sim`, connects it to the requested server, and advances it with the
same fixed-timestep logic as the windowed client. Completed terrain meshes are uploaded to
`RenderState`, whose ordinary world pass draws into `HeadlessTarget`. The resulting RGBA frame is not a
separate map renderer: it is the actual game camera output. The linear offscreen values are
sRGB-encoded, wrapped in an in-memory `image::RgbaImage`, and passed to `ratatui-image`'s primitive
half-block protocol. That protocol packs two vertical pixels into each `▀` cell with independent
true-colour foreground and background. It is forced instead of terminal-specific Kitty, Sixel, or
iTerm2 image protocols so `--surface terminal` has stable Unicode output everywhere.

Ratatui splits the current terminal dimensions into a chat pane on the left, game pane on the right,
and input pane along the bottom. A resize rebuilds the headless target to the game pane's new cell
dimensions. In game focus, `W`, `A`, `S`, `D`, and space feed the shared `InputState`; mouse movement
inside the game pane feeds the same accumulated mouse deltas as the window. Enter or `/` focuses chat,
where Enter sends, Escape cancels, and a leading `/` uses `Sim::send_chat`'s normal command path.
`Ctrl-C` exits. A short release timeout prevents movement sticking on terminal emulators that do not
report key-release events.

## How to change it

- Add a surface name in `Config::from_args`, update `Config::usage`, then add the exhaustive dispatch in
  both `crate::run` and `crate::app::run`.
- Change stream filtering or local commands in `crate::terminal::run_stdio`. Keep `/` server-bound;
  local controls use the reserved `#` namespace so chat cannot be mistaken for a client command.
- Change terminal layout in `crate::terminal::surface_areas`, event mapping in
  `crate::terminal::handle_key`, and image conversion in `crate::terminal::halfblock_protocol`.
- Keep the `ratatui-image` primitive half-block backend free of Chafa and image decoder features. The
  surface starts from raw RGBA bytes, so those dependencies add no capability here.
- Change the rendered scene through `Sim` or `RenderState`, not by teaching the terminal encoder about
  blocks. A second scene implementation would drift from the window.

### Gotchas

The terminal surface requires both stdin and stdout to be TTYs because Ratatui uses raw input and the
alternate screen. Use `--surface stdio` when redirecting either side. Mouse motion is cell-granular,
not pixel-granular, and depends on the terminal emulator delivering mouse-move events.

The Unicode renderer still needs a GPU adapter: "terminal" means no window or swapchain, not software
rendering. The `stdio` surface is the option for a genuinely GPU-free session.

## Configuration

```text
lodestone --surface stdio --host example.org --port 25565
lodestone --surface terminal --host example.org --port 25565
```

- `--surface <window|stdio|terminal>` selects the presentation path. The default remains `window`.
- `--host`, `--port`, and `--protocol` select the server for both terminal surfaces.
- The live terminal size controls the pane and GPU target sizes; resize events take effect while the
  client is running.

## Dependencies

- `lodestone-client` through `NetClient` for network events and outbound chat/command actions.
- `Sim` for the authoritative interactive game state used by the Unicode surface.
- `lodestone-render` and wgpu's headless target for real game frames without a window.
- Ratatui and its Crossterm backend for layout, raw keyboard events, resizing, mouse capture, and safe
  alternate-screen restoration.
- `ratatui-image` for maintained true-colour Unicode half-block conversion, with its Chafa and image
  decoder features disabled; `image` only wraps the in-memory RGBA buffer.
