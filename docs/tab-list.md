# The Tab player-list overlay

## What it is

The list of online players the game shows while Tab is held, ported from vanilla's
`PlayerTabOverlay`: a translucent plate, one striped row per player carrying that
player's display name and a five-band ping icon, split into columns past twenty
players, with the server's header above and footer below when it sent them.

## How it works

Three layers, and the split is deliberate — each answers a question the others
cannot.

| layer | lives in | question |
|---|---|---|
| state | `lodestone_game::tablist::TabList` | who is online, and what does the server say about them |
| projection | `lodestone_shell::tablist::tab_list_view` | what does one frame of the overlay contain |
| geometry | `lodestone_shell::hud::TabPanel` | where does each of those pieces go |

The wire end was never the problem. `PLAYER_INFO_UPDATE` and `PLAYER_INFO_REMOVE`
are decoded by the `v770` adapter (our own server encodes all three sites),
`TAB_LIST` emits `ClientEvent::TabListChanged` and routes through the `SESSION`
router into `TabList::apply`, and `SessionTabList` is what `Sim::tab_list_view`
reads. **The defect was in the draw**: `HudFrame::players` used to be
`Option<&[String]>` — a flat list of pre-formatted `"NAME  30ms"` rows — plus a
`"PLAYERS (n)"` caption vanilla has no equivalent of. Every link was green, so
`cargo xtask connectedness` reported the same output before and after; what the
flattening destroyed was the game mode, the styled display name and the latency
*band*. This is the "fully-connected wire carrying the wrong value" shape, and the
instrument is silent on it rather than wrong.

### The projection — `tablist.rs`

`tab_list_view` does what the fold deliberately does not:

- resolves each entry's `display_name` (else its plain profile name) into **styled
  spans**, so a server that colours a name gets a coloured row —
  `PlayerTabOverlay.getNameForDisplay`;
- applies vanilla's `limit(80)` **after** sorting, so a 200-player server shows
  the alphabetically-and-by-order first 80 rather than 200 rows off the bottom;
- turns the raw latency into one of six sprite ids via `ping_sprite`.

Sort order is `TabList::ordered`, which is `PLAYER_COMPARATOR`: descending
`list_order`, then spectators last, then team name, then profile name
case-insensitively. Team membership belongs to the scoreboard, so the default
omits that key and `ordered_by` takes it from a caller that has one.

Only *listed* players appear (`getListedOnlinePlayers()`). Tab-completion in chat
reads the **unfiltered** set (`getOnlinePlayers()`), which is why the two must not
share a projection — see `docs/chat.md`.

### The geometry — `hud.rs`

`TabPanel::new` is `PlayerTabOverlay.extractRenderState`'s arithmetic. Read it
knowing three things:

- **Vanilla's metrics, not the HUD's.** The overlay draws at `TAB_TEXT_SCALE`
  (`1.0`) with a `TAB_LINE_H` of `9` in the already-`gui_scale`-divided logical
  canvas, exactly like the F3 overlay. The rest of `build_inner` runs at the HUD's
  2× text scale; drawing this overlay there is what "the text is way too big"
  means.
- **The column loop has to be read in Java's own order.**
  `for (cols = 1; rows > 20; rows = (slots + cols - 1) / cols) { cols++; }`
  evaluates condition, body, *then* update, so `cols` is bumped before `rows` is
  recomputed. 20 players stay in one column of 20 (the guard is `> 20`, not
  `>= 20`); 21 become **two columns of 11**, not two columns of 20 with nine empty
  rows of plate hanging below; 41 become three of 14.
- **Slots fill column-major** — `col = i / rows`, `row = i % rows`. A row-major
  reading produces a list that reads across instead of down, and on any list of 20
  or fewer the two are indistinguishable.

Every division is vanilla's **integer** division and is floored for that reason;
`slot_w` in particular is `min(estimate, screenWidth - 50) / cols`, and letting it
stay fractional puts column 1 half a pixel off at most widths.

Colours are transcribed, and the second one is the one that gets guessed wrong:
the plates are `Integer.MIN_VALUE` (`0x80000000`, black at alpha 128) but the
per-row fill is `getBackgroundColor(553648127)` = `0x20FFFFFF` — **white** at
alpha 32. Reading it as another black wash makes the rows read as one flat block
instead of a striped list. A spectator's name is `0x90FFFFFF` rather than opaque
white.

## How to change it, and the gotchas

**There is no player head, and on every server we can host that is vanilla's own
behaviour.** `extractRenderState` gates the 8×8 face on
`showHead = connection.onlineMode()`, which comes from the LOGIN packet's
`onlineMode` field. Our server writes `false` there (`v770`'s
`server_protocol`), so vanilla joined to it draws no head either, and
`TabListView::show_head` being `false` reproduces exactly that layout — the 9 px
is reserved only when the flag is set. Turning heads on for an online-mode server
needs **two** things, and neither is in this subsystem:

1. the client-side decode of `onlineMode` off the LOGIN packet (it is currently
   write-only, in the server encoder);
2. a texture path in the HUD render pass. The HUD has a colour pipeline and one
   GUI-atlas sprite pipeline; a per-player skin is a third, with a bind group per
   distinct texture. `remote_skins.rs` already fetches and decodes the sheets for
   the *world* entity pass, so the data is there — what is missing is the pass.

**Do not fabricate a header or footer.** A vanilla server sends neither unless a
plugin or a datapack sets one, which is why vanilla's own tab list shows neither.
`banner_lines` returns an empty `Vec` for absent, empty and whitespace-only
banners, and the draw skips the whole plate — not just the text — so an absent
banner leaves byte-identical geometry.

**The scoreboard score column is not modelled.** Vanilla's `widthForScore` needs a
display objective on the `PLAYER_LIST` slot, which this overlay is not given;
`TabPanel` therefore hardcodes it to `0`. Adding it means threading the objective
plus a `Scoreboard` in, and the hearts render type brings its own blink-state
machine (`PlayerTabOverlay.HealthState`). The sidebar is a separate surface and is
unaffected — see `overlay.rs`.

**Italics are not modelled.** `decorateName` italicises a spectator's name as well
as dimming it; this font has no italic variant and a fabricated slant would be
worse than the dimming alone.

Adding a row field means touching `TabListRow`, the projection, and the draw. The
layout is a separate type on purpose: the draw constructs one `TabPanel` and draws
from it, and a gate constructs one from the same inputs and measures against it, so
a gate cannot keep passing against an overlay that has moved.

## Configuration

None of its own. It follows the GUI scale through `menu::render::logical_canvas`,
and `MAX_TAB_ROWS` (80) and `TAB_MAX_ROWS_PER_COL` (20) are vanilla constants
rather than settings.

## Dependencies

- `lodestone_game::tablist` for the folded state, and `lodestone_game::text` for
  `translate` resolution in display names and banners.
- `lodestone_ecs::SessionTabList`, the one fold, read by `Sim::tab_list_view`.
- The `gui/sprites/icon/ping_*` sprites out of `lodestone_render::GuiAtlas`, bound
  by `HudRenderer::attach_gui`. With no atlas attached (a jar-less run) the bars
  draw nothing and the rest of the overlay is unaffected.
- `crate::overlay::plain_spans`/`spans_text` for the span vocabulary the rows share
  with the scoreboard sidebar.
