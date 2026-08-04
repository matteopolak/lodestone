# Text colour

## What it is

Server-authored text colour and formatting — the sixteen legacy `§` colours, modern
`TextColor` hex values, and the five format flags — carried from the wire through to the
emitted vertex on every surface that draws a chat component: chat, the scoreboard sidebar,
and the server-list MOTD.

## How it works

There are two draw vocabularies, and the difference is the whole architecture.

| path | input | can carry hex? | used by |
|---|---|---|---|
| `VanillaFont::draw_legacy` | `&str` with `§` codes | **no** | chat |
| `VanillaFont::draw_spans` | `&[TextSpan]` | **yes** | sidebar, MOTD |

`§` codes can only name the sixteen legacy colours. `Text::to_legacy_string` therefore
*silently drops* a `TextColor::Rgb`, because `TextColor::legacy_code()` returns `None` for
it and no code is emitted. Anything flattened to a legacy string before drawing has already
lost every hex colour a 1.16+ server sent. `draw_spans` takes `TextColor` itself, so it does
not have that ceiling.

### The colour bridge

`TextColor::rgb() -> u32` (`crates/lodestone-model/src/text.rs`) is the **single** source of
truth for the sixteen named RGB values, transcribed from vanilla's `TextColor.java:18-33`
(26.2). `hud::legacy_rgb` and `hud::vanilla_font::text_color_rgb` both delegate to it, so the
`§`-carrying and `TextColor`-carrying paths cannot disagree about what "gold" means.

Before this existed, the only route from a model colour to a pixel colour was
`TextColor → legacy_code() → char → a §-keyed table private to hud.rs`. That is why hex died
and why there were two transcriptions of sixteen hex constants.

> **Do not look for these values in `ChatFormatting`.** In 26.2 that enum's constructor is
> `ChatFormatting(final char code)` and carries no colour at all. The obvious place to check
> is empty, and its emptiness reads as "vanilla has no table" rather than "the table moved".

### Per surface

**Chat** already worked for named colours and still does: `ChatLog` flattens to a legacy
string (`lodestone-game/src/chat.rs`), `hud.rs`'s `wrap_legacy` wraps it treating code pairs
as zero-width, and `Builder::text_legacy` → `VanillaFont::draw_legacy` → `legacy_run` colours
each run. **Hex colours are still lost in chat**, because that pipeline is a `String`; see
*How to change it*.

**Scoreboard sidebar** — `crates/lodestone-shell/src/scoreboard.rs`. `lodestone-game`'s fold
already preserved style all the way (its own test asserts `style.color == Some(Aqua)`
survives `resolve`), and even *adds* team colour in `Scoreboard::decorate`. The shell then
called `resolve(...).to_plain_string()`, throwing all of it away one layer above a HUD that
had no way to accept it. It now calls `to_spans()`, `overlay::Sidebar`/`SidebarLine` carry
`Vec<TextSpan>`, and `hud.rs` draws with `Builder::text_spans`. `NumberFormat::Styled` was
matched as `Styled(_)` next to `Default` — the server's colour bound to a wildcard and
dropped — and now becomes a coloured span.

**Server-list MOTD** — `crates/lodestone-net/src/status.rs`. Colour was destroyed *twice*
here: a hand-rolled flattener read only `text`/`translate`/`extra` and never looked at
`color`, then a `strip_formatting` pass deleted every `§` pair. Both are gone;
`description` now goes through `Text::from_json(...).to_spans_expanding_legacy()`.
`ServerStatus` gained `motd_spans` alongside the plain `motd`, and the plain string is
*derived* from the spans so the two cannot disagree.

`to_spans_expanding_legacy` matters because MOTDs mix both conventions, routinely in one
field: a bare `§`-coded string, a component tree with `color` keys, or a component tree whose
`text` values *also* contain `§` codes because the server built the string with a legacy
formatter and wrapped it in JSON. A parser that understood only component `color` would
render the codes as literal glyphs.

Wrapping still happens on the plain string. `restyle_wrapped`
(`crates/lodestone-shell/src/menu/render.rs`) re-attaches per-character styles to
`wrap_measured`'s output, so there is exactly **one** word-wrap implementation — vanilla's
MOTD wrap has several non-obvious rules (per-paragraph line state, a blank line is a line, an
over-wide word starts a line rather than overflowing) and each exists because it was a bug
once. It works because a wrapped line's characters are a subsequence of the source in order.

### Colour space — the one thing not to get wrong

**Vanilla is not colour-managed.** A text colour is written to the framebuffer as the sRGB
byte it is: `text_color_rgb` divides by 255 with **no** transfer function. The drop shadow is
`ARGB.scaleRGB(color, 0.25F)` (`Font.PreparedTextBuilder.getShadowColor`), a quarter taken in
**gamma** space, which is what `vanilla_font::shadow_of` does.

Doing either in linear space is the plausible mistake and it is measurable, which is why the
gate discriminates rather than merely accepting a plausible colour:

| quantity | gamma (correct) | linear (wrong) |
|---|---|---|
| gold's `G/R` | `170/255 = 0.6667` | `srgb_to_linear(0.6667) = 0.4019` |
| white's shadow | `0.25` | `linear_to_srgb(0.25) = 0.5372` |

The linear shadow is more than twice as bright — a visible grey outline instead of vanilla's
near-black one.

## How to change it, and the gotchas

- **Prefer `text_spans` over `text_legacy` for anything that starts as a `Text`.** Flattening
  to `§` first is lossy at the call site in a way nothing warns you about.
- **The gate measures vertices, not pixels, deliberately.** `HudGeometry::verts` is flat
  `[x, y, r, g, b, a]` and is the last point where a colour is a number the test chose. On
  this Metal backend, through `ALPHA_BLENDING` with an `Rgba8UnormSrgb` target, the effective
  blend alpha is a repeatable but non-trivial function of the raw fragment alpha — neither the
  identity, nor `linear_to_srgb(a)`, nor any single power law — so an exact-byte prediction
  downstream of the blend cannot be stated honestly. Measuring upstream also means the gate
  runs in the plain `cargo test --workspace`, with no adapter and no `client.jar`.
- **Expected values must not come from `TextColor::rgb()`.** The gate hardcodes the sixteen
  values from the jar. Building the expectation from the code under test makes it
  `decode(encode(x))`, satisfied by any self-consistent misunderstanding — including all
  sixteen being wrong together.
- **Filter ink by `alpha == 1.0`.** The sidebar panel behind the text is a translucent black
  rect; without the filter its vertices are indistinguishable from black text. No named colour
  can be mistaken for a *shadow*, because a shadow is a quarter and none of
  `0x00`/`0x55`/`0xAA`/`0xFF` is a quarter of another (`0xAA/4 = 0x2A`, `0xFF/4 = 0x3F`).
  Black is its own shadow, harmless for a presence test.
- **Test inheritance with a genuinely nested `Text`.** `TextStyle`'s `None` means *inherit* and
  `Some(false)` means *explicitly off*; collapsing them looks correct on flat messages and
  corrupts nested ones. A suite of flat single-colour strings is blind to this by construction
   — the test source looks exemplary and the flaw is in the input data.
- **Bold changes advance width** (`+1` per glyph, `Font::advance_bold`). `VanillaFont::spans_width`
  takes the route `Font::legacy_width`'s own doc prescribes for structured components: decompose
  to `(codepoint, bold)` and call `advance_bold`. Italic shears in place; underline,
  strikethrough and obfuscated leave the pen alone.
- **Chat hex is still lost**, at `Text::to_legacy_string`. Fixing it means either routing chat
  through spans — which needs span-aware *wrapping*, currently `wrap_legacy` over a `String` —
  or teaching the legacy string the BungeeCord `§x§r§r§g§g§b§b` hex form, which `legacy_width`
  would already treat as zero-width and `strip_legacy` would already strip. The second is much
  the cheaper change.
- **A jar-less run still colours text.** `Builder::text_spans` and `Quads::text_spans` both
  have a fixed-advance fallback that keeps per-span colour, matching `text_legacy`'s.

## Configuration

- `options.chat.color` (`false` strips colour from chat via `hud::strip_legacy`). It has no
  effect on the sidebar or MOTD, matching vanilla, where the option is chat-specific.
- No env vars, no feature flags. The vanilla font is jar-sourced and optional
  (`VanillaFont::shared()`); every path degrades to the fixed-advance debug font.

## Dependencies

- `lodestone-model` — `Text`, `TextSpan`, `TextStyle`, `TextColor`, `to_spans`,
  `to_spans_expanding_legacy`, `from_legacy`, `from_json`, `from_nbt`. Owns the colour table.
- `lodestone-net` — status-ping decode; **now depends on `lodestone-model`**, because a
  `description` is a chat component and parsing one is component parsing.
- `lodestone-game` — `text::resolve` (translate lowering, style-preserving), the scoreboard
  fold, `ChatLog`.
- `lodestone-assets` — `Font`/`RasterFont` advances and glyph rasters from the real jar.
- `crates/lodestone-shell/tests/text_colour.rs` — the gate, with its executed negative control.
