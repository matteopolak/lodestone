# Main menu

## What it is

The GUI entry point. Running `lodestone` with no connection flags opens on a title
screen instead of dropping straight into the local dev world: Singleplayer /
Multiplayer / Quit, a persisted multiplayer server list with add/edit/delete, and
a per-server status ping showing MOTD, player count, latency and favicon.

It lives entirely under [`crates/lodestone-shell/src/menu/`](../crates/lodestone-shell/src/menu/):

| file | owns |
|---|---|
| `menu.rs` | `Screen` / `UiState` — the screen state machine and every legal edge |
| `menu/nav.rs` | selection, the add/edit form, what a keypress *means* |
| `menu/render.rs` | layout, the frame builder, and a self-contained GPU pipeline |
| `menu/servers.rs` | `ServerEntry` / `ServerList` and the on-disk JSON |
| `menu/status.rs` | background status pings and their cache |

## How it works

### State machine

`UiState` (`menu.rs`) is the authority on which screen shows:

```
MainMenu ──Singleplayer──> Playing
   │
   └─Multiplayer─> ServerList ──Enter──> Connecting ──> Playing
                      │  ↑                    │
                    A/E  │                    └─fail──> Error ──> MainMenu
                      ↓  │
                   ServerEdit
```

Escape unwinds **one level at a time**: from `ServerEdit` to `ServerList`, from
`ServerList` to `MainMenu`, and only from `MainMenu` does it quit. Getting that
wrong means Escape mid-edit exits the game.

Every transition is guarded by its source screen (`open_server_list` only fires
from `MainMenu`, `session_ready` only from `Connecting`), so a late signal from a
torn-down session cannot yank the player out of a menu or back into a world.

### Input

`app.rs` translates winit events into `MenuKey` (`Up`, `Down`, `Enter`, `Escape`,
`Tab`, `Backspace`, `Delete`, `Char`) and hands them to `MenuNav::key`, which
returns a `MenuAction` naming the one side effect the app must perform
(`Singleplayer`, `Connect`, `Quit`, `Reprobe`, `Forget`).

That indirection is the point: **the entire menu is unit-testable with no window,
no GPU and no server.** `MenuAction` is an enum rather than a callback so adding a
variant fails to compile at the `match` in `app.rs` instead of silently doing
nothing.

`Char` is interpreted per screen — a command on the list (`a` add, `e` edit, `d`
delete, `r` refresh) and literal text in the edit form. Get that backwards and
either the list is unusable or the form cannot spell `australia.example.com`.

Mouse hover highlights a row and a left click activates it, hit-tested against
`render::row_rect` so the mouse and keyboard drive one selection. Clicking the
backdrop does nothing — it must not confirm whatever happens to be highlighted.

### Rendering

`MenuRenderer` owns its own shader, pipeline and vertex buffer, following
`EffectsRenderer` and `ContainerRenderer`. This is structural, not stylistic:
`hud.rs` and `container.rs` belong to other agents, and folding a fourth surface
into the HUD's single geometry pass would mean editing their files. The only thing
borrowed is the HUD's **public** bitmap font, `hud::glyph_rows`.

Unlike the HUD overlays this pass **clears** the target, because nothing renders
behind a menu — otherwise the last world frame shows through.

`render::frame_for` is the single place menu *state* becomes menu *content*, and
`render::owns_frame` is the predicate for "this renderer owns the frame and the
keyboard". They are tested against each other on every `Screen` variant, because
two definitions that can disagree is how a screen ends up drawn twice or not at
all. `owns_frame` covers the three menu screens **and** `Screen::Error` (a
disconnect used to leave a frozen world on screen with no explanation), but
deliberately **not** `Screen::Connecting` — that keeps rendering the world so
chunks mesh and upload as they stream in, rather than piling up behind a loading
screen and landing as one spike at login.

`geometry()` is pure (`MenuFrame` + viewport → `Vec<f32>`), which is what lets the
layout be gated by **coverage inside a row's own rect**, with negative controls,
rather than by counting vertices.

### The server list on disk

JSON array of `{name, host, port?}` at `<data dir>/servers.json`:

| platform | data dir |
|---|---|
| macOS | `~/Library/Application Support/lodestone` |
| Windows | `%APPDATA%\lodestone` |
| other | `$XDG_DATA_HOME/lodestone`, else `~/.local/share/lodestone` |

There was **no user-state directory helper anywhere in the workspace** before
this, so `servers.rs` establishes the convention.

Two rules that are load-bearing:

- **An absent `port` is not `Some(25565)`.** Vanilla only performs the
  `_minecraft._tcp` SRV lookup when the user did not pin a port, so collapsing the
  two makes every SRV-only server unreachable. `ServerEntry::port` is `Option`
  end to end, including into `lodestone_net::server_status`.
- **A corrupt or missing file loads as an empty list, never an error.** A bad
  server list must not stop the game from launching. Individual malformed rows are
  skipped so one bad entry does not lose the rest.

Writes happen **eagerly, on every mutation**, not at exit: there is no guaranteed
clean-shutdown hook, and a list that survives only a graceful quit is one that
silently loses the entry the player just added. A failed write is surfaced on the
list screen rather than swallowed.

### Status pings

`StatusCache` (`menu/status.rs`) keeps one `StatusSlot` per **dialable address**
(`host:effective_port`), so renaming or reordering the list does not scramble
results. `refresh` is idempotent — calling it every frame does not spawn a thread
per frame — and `pump()` must be called once per frame to move finished probes
into slots; nothing else drains the channel.

Each probe runs `lodestone_net::server_status` on its own detached thread with its
own single-threaded tokio runtime. Nothing joins them, so a slow DNS lookup can
never stall shutdown.

> **`lodestone_net::ping` and `lodestone_net::resolve` had no consumer anywhere in
> the workspace before this.** They were complete, unit-tested and exported — and
> dead. The only code in the tree that pinged a server,
> `lodestone-game/tests/live_server.rs`, hand-rolled the whole status handshake
> over a raw `TcpStream` rather than calling them. `menu/status.rs` is the first
> caller.

The real probe is the **default** (`StatusCache::new`), not something a caller
must remember to install, and
`status::tests::the_default_cache_actually_uses_the_network` gates that by
requiring a real transport error from a port nothing listens on. A constructor
that returned a do-nothing cache is exactly how a finished subsystem reaches zero
pixels.

### Favicons

The status JSON carries the favicon as a `data:image/png;base64,…` URI;
`lodestone_net::decode_favicon` validates the PNG magic and hands back bytes.
`render::favicon_mosaic` decodes them with `lodestone_assets::Image` and
box-filters to a 16×16 grid of coloured cells, drawn as quads on the pipeline that
is already there — no texture, no second bind group. At the 32 px row icon size
each cell is two screen pixels.

`FaviconCache` memoises the decode by address. Without it, `frame_for` would
inflate every visible server's PNG **every frame**.

## How to change it

- **Adding a screen:** a `Screen` variant, an arm in `MenuNav::key`, a branch in
  `render::frame_for`, and the variant added to `render::owns_frame`. The
  agreement test will tell you if you forget the last one.
- **Adding an action:** a `MenuAction` variant. The `match` in
  `WindowApp::apply_menu_action` is exhaustive on purpose.
- **Sizes in `render.rs` are physical pixels.** There is no DPI scaling, so on a
  2× display the menu draws at half the apparent size of the equivalent vanilla
  screen.
- **Text is upper-case only** — that is what the HUD's bitmap font has glyphs for.
  `glyph_rows` up-cases internally, so mixed case is harmless but pointless.

### Left for polish

Functional first; none of these block use.

1. **No dirt/panorama backdrop, no button textures, no rounded frames.** Flat
   coloured rectangles and a bitmap font.
2. **No DPI scaling.** Physical pixels throughout; small on a Retina display.
3. **No scrolling in the server list.** Rows are laid out centred and unbounded, so
   past roughly a dozen servers they run off the viewport. Row rects are already
   computed by one function (`row_rect`), which is where a scroll offset goes.
4. **No caret positioning, selection, or clipboard in the edit form.** Typing
   appends and Backspace removes from the end; there are no arrow keys within a
   field and no paste.
5. **No reordering** of the server list (no move up/down), and no delete
   confirmation — `d`/`Delete` removes immediately.
6. **No mouse wheel scroll and no drag.** Hover and click only.
7. **Favicon is a 16×16 mosaic of quads, not a sampled texture.** Recognisable,
   not sharp. Swapping in a texture is a change to `favicon_mosaic` plus one bind
   group.
8. **No settings screen** (video, controls, sensitivity, render distance). Those
   remain CLI flags.
9. **Singleplayer enters the shell's local worldgen world, not an integrated
   server.** `app::launch_singleplayer` is staged and returns
   `LaunchError::NoServerProtocol` until a versioned `ServerProtocol` exists for
   `lodestone-server` to serve in-process. The menu's Singleplayer button
   deliberately drives the working worldgen path rather than the staged launcher.
10. **No "Direct Connect"** — every multiplayer target must be saved as an entry
    first.
11. **No automatic re-ping.** Statuses are probed when the list opens and on `r`;
    there is no periodic refresh and no timeout badge distinct from an error.
12. **`--host 127.0.0.1` spelled out explicitly lands on the menu**, because
    `requested_a_connection` compares against `Config::default()` rather than
    reading a "flag was seen" bit. The clean fix is one `Option`-shaped field in
    `config.rs`, which was outside this change's file scope. `--live` is the flag
    every script and gate actually uses.
13. **No keyboard focus ring or hover cursor change**, and no sound on
    select/confirm.

## Configuration

- `LODESTONE_DATA_DIR` overrides the platform data directory (and is how tests
  stay off the developer's real `servers.json`).
- `status::STATUS_PROTOCOL` (776) is advertised in the status handshake. Vanilla
  ignores it in the status state, but a proxy may use it to pick a backend.
- `--live`, `--host`, `--port` bypass the menu and connect immediately, exactly as
  before.

## Dependencies

- `lodestone-net` — `server_status` (SRV resolution + status exchange + MOTD /
  player / favicon decode). **This dependency edge is new**; see the note above.
- `lodestone-assets` — `Image::decode_png` for favicons.
- `crate::hud::glyph_rows` — the bitmap font (public API only).
- `wgpu` — one pipeline, one dynamic vertex buffer.
- `tokio` — a current-thread runtime per probe.
- `serde_json` — the server list's on-disk form (hand-built through `Value`; the
  shell depends on `serde_json` but not on `serde`, so a derive would add a
  dependency).
