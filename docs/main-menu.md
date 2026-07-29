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

## The title screen is vanilla's layout, from vanilla's source

`Screen::MainMenu` reproduces `TitleScreen` — the same widgets, in the same
places, drawn with the pack's own `widget/button*` art. Every number came out of
`.cache/mc/26.2/client-src/net/minecraft/client/gui/screens/TitleScreen.java`,
not from memory, and lives in `render::title_slot` with the source line beside
it.

`topPos = height / 4 + 48` (`TitleScreen.java:113`), rows every 24 px:

| # | widget | rect (logical px) | state |
|---|---|---|---|
| 0 | Singleplayer | `W/2-100, topPos, 200×20` | live |
| 1 | Multiplayer | `W/2-100, topPos+24, 200×20` | live |
| 2 | Minecraft Realms | `W/2-100, topPos+48, 200×20` | **disabled** |
| 3 | Friends (icon) | `W/2-34, topPos+72, 20×20` | **disabled** |
| 4 | Language (icon) | `W/2-10, topPos+72, 20×20` | **disabled** |
| 5 | Accessibility (icon) | `W/2+14, topPos+72, 20×20` | **disabled** |
| 6 | Options… | `W/2-100, topPos+96, 98×20` | live |
| 7 | Quit Game | `W/2+2, topPos+96, 98×20` | live |

Plus `LogoRenderer`'s wordmark at `W/2-128, 30` (256×44) with the edition strip
at `W/2-64, 67` (128×14), the version string at `2, H-10` and the copyright line
right-aligned at `W-2, H-10` (`LogoRenderer.java:35-43`,
`TitleScreen.java:110-111,323`).

Two details worth naming because a remembered layout gets them wrong:

- the icon row's x comes from `getHorizontalPosition(n, 3, 20)`
  (`TitleScreen.java:170-173`): `totalWidth = 3*20 + 2*4 = 68`, so
  `W/2 - 34 + (n-1)*24`;
- the Options/Quit pair has a **4 px** gutter (98 at `W/2-100`, 98 at `W/2+2`).
  The pause screen's equivalent pair has an **8 px** one. They are not the same
  layout — see [`pause-menu.md`](./pause-menu.md).

### Why four buttons are present and disabled

Because the alternative reads worse. A button missing from its vanilla position
moves everything below it and the screen stops looking like vanilla's; a
greyed-out button in the right position looks exactly like vanilla with the
feature unavailable, which is a state vanilla itself ships (it disables
Multiplayer and Realms for a banned account, `TitleScreen.java:189-203`).

Each disabled one needs a subsystem this client does not have: Realms is a paid
Mojang-hosted service with an authenticated HTTP API; Friends needs a
Microsoft-account social graph; Language needs a language-selection screen (the
shell loads exactly one table, `en_us.json`); Accessibility needs an
accessibility options screen.

The look is vanilla's own, not invented: `MainButton::enabled() == false` selects
`widget/button_disabled` and recolours the label to `0xFF_A0_A0_A0` — vanilla's
`-6250336` from `AbstractWidget.WithInactiveMessage` (`AbstractWidget.java:318`).

### Disabled buttons and the keyboard, the mouse, and clicks

One index space (`MAIN_BUTTONS` / `PAUSE_BUTTONS`) serves keyboard selection,
mouse hover, hit-testing and the renderer, so they cannot drift. Three rules,
all of them vanilla's:

- **Arrow keys step over a disabled row** (`nav::step_enabled`). Vanilla's
  `AbstractWidget::nextFocusPath` returns `null` for an inactive widget
  (`AbstractWidget.java:152-158`), so Tab never lands on one either.
- **The mouse still hovers one.** Vanilla sets `isHovered` from geometry alone
  and never consults `active` (`AbstractWidget.java:56-62`), and
  `WidgetSprites::get(active, focused)` returns the *disabled* sprite whichever
  way `focused` went (`WidgetSprites.java:19-25`) — so it looks greyed out, not
  highlighted.
- **Enter/click on a disabled row does nothing.** This one is load-bearing rather
  than cosmetic: `app.rs` turns a click into `hover(row)` then `MenuKey::Enter`,
  so if `hover` had *refused* the disabled row the Enter would have activated
  whatever was highlighted before — clicking greyed-out Advancements would have
  disconnected you. `nav::a_disabled_button_is_hoverable_but_cannot_be_activated`
  gates exactly that, with the enabled case as its positive control.

### Buttons are real nine-slice sprites

`widget/button`, `widget/button_highlighted` and `widget/button_disabled` are
sprite-scaling sprites: their sibling `.png.mcmeta` declares `nine_slice` with a
border, and only the middle stretches. **The borders are read from the pack**, by
`GuiAtlas::geometry` → `GuiScaling::geometry`, and are not hardcoded anywhere in
the shell. That matters concretely: in the real 26.2 pack `button` and
`button_highlighted` declare `border: 3` and `button_disabled` declares
`border: 1`. A single hardcoded 3 would draw the disabled button's corners three
times too large at every size.

`render::nine_slice_borders_come_from_the_mcmeta_not_a_constant` pins the 3-vs-1
split against a synthetic pack that repeats those two values.

### Where the textures come from

`resources::load_menu_gui_atlas` stitches `gui/sprites/**` plus the two **loose**
title textures, via the new `GuiAtlas::build_with_extras`. The logo lives at
`textures/gui/title/minecraft.png`, outside the sprite tree, so plain
`GuiAtlas::build` structurally cannot see it.

Both title textures are hi-res in 26.2 (1024×256 and 512×64) while vanilla
declares them as 256×64 / 128×16 and blits only the top 44 / 14 rows. Everything
below those cuts was **measured fully transparent** (max alpha 0), so drawing the
whole sprite into a 256×64 / 128×16 rect is pixel-identical to vanilla's sub-rect
blit — which is why no sub-rect blit primitive was needed.

`MenuRenderer` binds the atlas **lazily, on its first draw** (`ensure_gui`),
because the upload needs a `Queue` and `MenuRenderer::new`'s call site in
`app.rs` passes only a `Device`. `attach_gui` is public so `app.rs` can hand in a
shared atlas instead; today that means the menu's atlas is a **second stitch**
alongside the HUD's (a few MB), which is the one known duplication here.

### What is *not* reproduced

Named plainly rather than half-done:

- **No panorama.** Vanilla's title background is the rotating `Panorama` cubemap
  plus `PANORAMA_OVERLAY` (`TitleScreen.java:307`, and note
  `TitleScreen.extractBackground` is empty — the panorama *is* the background).
  Ours is a flat dark fill. This needs a cubemap sampler, six textures and a
  spin, and rushing it would look worse than a clean flat colour.
- **No splash text** (the yellow rotating line). `SplashManager` reads
  `texts/splashes.txt` and vanilla draws it rotated ~20°; no rotation exists in
  this pipeline yet.
- **No fade-in**, no tooltips on the disabled buttons, no Realms notification
  overlay, no `TW` test-world button (that one is `IS_RUNNING_IN_IDE`-only
  anyway).

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
- **Sizes in `render.rs` are *logical* GUI pixels**, the same units vanilla's
  `Screen.width`/`height` are in: `MenuRenderer::draw` divides the framebuffer by
  `config::calculate_gui_scale` through `render::logical_canvas` before laying
  anything out. That is why a vanilla constant can be transcribed verbatim.
- **Adding or moving a vanilla widget** is one arm in `render::title_slot` (or
  `pause_slot`) plus a variant in `nav::MainButton`/`PauseButton`. `row_rect`
  resolves the slot, and `app.rs`'s `menu_row_at` calls `row_rect` — so the draw,
  the hover and the click hit-test all move together by construction. Do not add
  a second placement function.
- **Text on the two vanilla screens is mixed-case vanilla text**, through
  `hud::VanillaFont` (real glyphs, proportional advances, the 1 px 25 %-brightness
  shadow). On the remaining row-stack screens text is still **upper-case only** —
  that is what the HUD's 5×7 bitmap font has glyphs for. `glyph_rows` up-cases
  internally, so mixed case there is harmless but pointless.
- **Measure with `Quads::text_width`, never `text_px`, inside a vanilla screen.**
  It picks the proportional or the fixed measure to match whichever font
  `Quads::text` will actually draw with. A centred label measured against the
  other font is off-centre by a factor of ~1.2 and looks like a layout bug.

### Left for polish

Functional first; none of these block use. Items 1 and 2 applied to *every*
screen when this was written; the **title screen** and the **pause screen** now
draw real GUI textures and real vanilla text at a DPI-correct scale (see the
sections at the top of this file), so 1 and 2 are now specifically about the
server list, the edit form, Options and the error screen.

1. **No dirt/panorama backdrop; no button textures on the list/form/options
   screens.** Flat coloured rectangles and the 5×7 bitmap font there.
2. ~~**No DPI scaling.**~~ Fixed: layout happens in a `logical_canvas` divided
   from the framebuffer by `config::calculate_gui_scale`, and `hit_test` divides
   the incoming cursor by the same factor.
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

## Verification

```bash
cargo test -p lodestone-shell --lib menu:: --no-fail-fast
cargo test -p lodestone-shell --lib every_sprite_id -- --ignored --nocapture
cargo test -p lodestone-shell --test menu_button_pixels -- --ignored --nocapture
```

The layout gates are pure (`title_slot`/`pause_slot` asserted against rects
derived by hand from the Java source, not read back out of themselves).
`every_sprite_id_the_vanilla_screens_name_exists_in_the_real_pack` is the island
check for the hardcoded sprite ids — a mistyped id draws *nothing*, and every
synthetic-pack test still passes because it writes the same string it reads.

`tests/menu_button_pixels.rs` is the on-screen gate: it drives the real
`MenuRenderer` through the same `frame_for` → `render` calls `app.rs` makes and
measures the framebuffer. Three orthogonal discriminators off the source art —
`widget/button`'s **bevel** (top row / bottom row), `button_disabled`'s
flatness and 2.5× darkness, and `button_highlighted`'s **white** outer border row
against the other two sprites' black one — plus an executed negative control
(`MenuRenderer::detach_gui`, which must fail the bevel assertion).

> **The readback is in *linear* space, not the file's sRGB.** `GpuAtlas` uploads
> `Rgba8UnormSrgb`, so `textureSample` linearises. `widget/button`'s bevel is
> `167.4 / 84.8 = 1.97` in the file and **4.29** after linearisation — measured
> 4.33. The first version of that gate compared against 1.97 and would have
> accepted a much flatter button. On the real window (an sRGB surface) the values
> are re-encoded on write and display correctly; this is the same path the HUD's
> hearts take, so it is established behaviour, not a bug here.

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
