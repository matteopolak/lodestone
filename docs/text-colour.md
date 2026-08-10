# Text colour

## What it is

Server-authored text colour and formatting — the sixteen legacy `§` colours, modern
`TextColor` hex values, and the five format flags — carried from the wire through to the
emitted vertex on **every** surface that draws server text: chat, item names and tooltips, the
scoreboard sidebar, the tab list, the title/subtitle overlay, boss bars, container titles,
nametags, sign text, the kick screen and the server-list MOTD.

## How it works

That list used to name three surfaces. Ten of the seventeen were drawing raw `§` codes as
glyphs; *Every surface, and what each one was doing* below is the full census.

There are three draw vocabularies, and the difference is the whole architecture.

| path | input | applies `§`? | can carry hex? | used by |
|---|---|---|---|---|
| `VanillaFont::draw` / `draw_plain` | plain `&str` | **yes** | no | every string surface: item names, titles, boss bars, tab header/footer, container labels, menus |
| `VanillaFont::draw_legacy` | `&str` known to be `§`-coded | yes | no | chat, the action bar, the held-item name |
| `VanillaFont::draw_spans` | `&[TextSpan]` | n/a — already expanded | **yes** | sidebar, tab entries, MOTD, kick reason |

The first row is the one that changed, and the reason it had to is a single vanilla fact:
**there is no non-decomposing string draw in vanilla to be faithful to.** `Font.drawInBatch`
and `Font.width` both go through `StringDecomposer.iterateFormatted`, which applies legacy
codes at *draw* time — that is exactly why a plugin server can put `§7` in an item name and
have it colour. A "plain" pass that emitted `§` and `7` as glyphs was therefore not the simple
case, it was the wrong case, and the surfaces reaching it were precisely the ones showing raw
codes. `draw` now forwards to `draw_legacy` and `draw_plain` to one unshadowed `legacy_run`;
`VanillaFont::width` forwards to `legacy_width`, because measurement has to agree with the draw
(counting the pair over-measured by two characters per code and pushed every centred line left
of where it drew).

The un-styled `glyph` primitive and the plain `run` are **gone**, not merely bypassed:
`glyph_styled` with a default `GlyphStyle` is byte-identical to the old `glyph` (zero bold
offset, no obfuscation substitution, no italic shear, `has_effect()` false), so nothing was
lost, and leaving them in place would have left the trap reachable.

`§` codes can only name the sixteen legacy colours. `Text::to_legacy_string` therefore
*silently drops* a `TextColor::Rgb`, because `TextColor::legacy_code()` returns `None` for
it and no code is emitted. Anything flattened to a legacy string before drawing has already
lost every hex colour a 1.16+ server sent. `draw_spans` takes `TextColor` itself, so it does
not have that ceiling.

### Expansion is the default, and the raw variant says so in its name

`Text::to_spans` expands `§` codes found inside literal content. The non-expanding pass is
`Text::to_spans_ignoring_legacy_codes`, and it has exactly two legitimate callers —
`to_spans` (its own inner pass) and `to_legacy_string` (which is putting the codes *back* and
must not double-expand them).

This used to be the other way round: a plain `to_spans` did not expand, and
`to_spans_expanding_legacy` was the opt-in. One surface in the whole tree had opted in (the
MOTD), which is what made this a partial-adoption bug rather than a missing feature — the
correct function existed and read as an exotic special case. The old name survives as a
`#[deprecated]` forward.

`crates/lodestone-model/tests/legacy_expansion_guard.rs` enforces the allowlist mechanically
over every `.rs` file under `crates/`, `xtask/` and `web/`. **Prose in a doc comment is not a
guard** — this repo has a measured instance of a doc-comment rule being violated four times
while the comment sat there being correct. Two assertions in that test are load-bearing and
neither is decoration: a floor on files scanned (an audit that prints nothing is a failure to
run, never a pass) and a requirement that every allowlisted file still names the function (a
rename would otherwise empty the guard without failing it).

### `StringDecomposer.iterateFormatted`, clause by clause

Ported from the record, not from a summary of it. Each row is a place where a plausible answer
is wrong.

| input | vanilla | why it is easy to get wrong |
|---|---|---|
| `§` + a valid code | consumes both, applies the style | — |
| `§` + an **invalid** code | consumes both, emits **neither**, style untouched | `i++` sits *outside* the `formatting != null` test. Printing the pair, or dropping only the `§`, are the two other plausible answers; ours printed the pair |
| a dangling `§` at end of string | `break` — the `§` is dropped | it is the one case that looks like it should fall through to "ordinary character" |
| a **colour** code | sets the colour and clears all five flags **explicitly** | `Style.applyLegacyFormat`'s `default:` arm assigns `bold = italic = strikethrough = underlined = obfuscated = false` before setting the colour |
| a **format** code (`k l m n o`) | sets one flag, leaves the colour alone | getting this pair backwards makes `§c§lFoo` render in a way that looks almost right |
| `§r` | restores `resetStyle` — the **enclosing component's** style, not `Style.EMPTY` | `applyLegacyFormat` really does return `EMPTY` for `RESET`, but `iterateFormatted` special-cases `RESET` *before* calling it and substitutes its `resetStyle` parameter, which `iterateFormatted(String, int, Style, FormattedCharSink)` seeds with the component's own style |
| `§x§r§r§g§g§b§b` (BungeeCord hex) | **not honoured.** `getByCode('x')` is null, so `§x` vanishes and the six pairs after it read as six ordinary colour codes — the run ends up coloured by the last one | it works on Spigot-family servers, so it looks like a standard |

Two of those land on `TextStyle`'s `Some(false)` vs `None` distinction from a direction its own
docs do not cover. A colour code's cleared flags are `Some(false)` — explicitly off — because
`to_spans`' expansion pass inherits an expanded run's style from the enclosing component, so
`None` would let `{"bold":true,"text":"a§cb"}` inherit bold onto `b` where vanilla turns it
off. `§r`, conversely, stays all-`None`, because all-unspecified plus `TextStyle::inherit`
*is* `resetStyle`: at the root, where there is no enclosing style, it is `Style.EMPTY`; inside
a component it is that component's style. One representation, both cases right.

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

### Every surface, and what each one was doing

The full census. "raw `§`" means the two characters reached a quad as glyphs.

| surface | text arrives as | before | after |
|---|---|---|---|
| chat log | `§` string (`ChatLog`) | `text_legacy` — correct | unchanged |
| action bar | `§` string (`to_legacy_string`) | `text_legacy` — correct | unchanged |
| held-item name highlight | `§` string (`styled_hover_name`) | `text_legacy` — correct | unchanged |
| **item tooltip title** | `§` string (`styled_hover_name`) | `shadowed_label` → plain `run` → **raw `§`**, and `font.width` over-measured the box | decomposed and coloured |
| **title / subtitle overlay** | `§` string (`to_legacy_string`) | `Builder::text` → plain `run` → **raw `§`** | decomposed and coloured |
| **boss bar title** | `String` (`resolve_to_string`) | `Builder::text` → **raw `§`** | decomposed and coloured |
| **tab list header / footer** | `String` (`resolve_to_string`) | `Builder::text` → **raw `§`** | decomposed and coloured |
| **container / screen title** | `String` (`resolve_to_string`) | `Builder::label` → `draw_plain` → **raw `§`** | decomposed and coloured |
| **scoreboard sidebar title, labels, scores** | `Vec<TextSpan>` (`to_spans`) | spans, but unexpanded → **raw `§`** | expanded at the seam |
| **tab list entry names** | `Vec<TextSpan>` (`to_spans`) | spans, but unexpanded → **raw `§`** | expanded at the seam |
| **kick / disconnect reason** | `Vec<TextSpan>` (`to_spans`) | spans, but unexpanded → **raw `§`** | expanded at the seam |
| server-list MOTD | `Vec<TextSpan>` | already opted in | unchanged, new name |
| **entity nametags** | `String` (`effective_name().to_plain_string()`) | `layout_ink_runs` → **raw `§`** | pair consumed; colour still uniform, see below |
| **sign text** | `String` (`SignText::parse`) | `layout_ink_runs` → **raw `§`** | pair consumed; colour still uniform, see below |
| death screen message | `String` (`to_plain_string`) | `Builder::text` → **raw `§`** | decomposed |
| menus, toasts, advancement titles, sound subtitles | `&str` (local corpus) | plain `run` | decomposed; no `§` in practice, but no longer a trap |
| debug overlay | `String` (ours) | plain `run` | decomposed; unaffected in practice |

Ten of those seventeen were wrong, and only two — item names and the scoreboard — were in the
original report. Every one of the ten was fixed by one of three seam changes, not by seventeen
call-site edits: `VanillaFont::draw`/`draw_plain`/`width` decomposing, `Text::to_spans`
expanding, and `nametag::layout_ink_runs` consuming pairs.

**The jar-less path was fixed alongside each.** `hud::item_icon::ColourStream::text` and
`text_w` (the fixed-advance 5×7 debug font, used when there is no `client.jar`) now consume
`§` pairs too, and `menu::render::measure::text_px` delegates to `text_w` so the jar-less
measure and draw cannot disagree. Without that, a jar-less run would show `§7` where the real
font shows grey.

**Nametags and sign text consume the pair but do not apply the colour.** Both callers paint
every `LocalRect` in one uniform colour supplied at the draw site (nametags white, sign lines
the sign's dye), so a per-run colour would need `LocalRect` to carry one and the vertex buffers
to split per run. Consuming the pair is the half that has to be right either way: a dropped
colour reads as plain text, an emitted pair reads as a bug.

### Hex, and why fixing the renderer was never going to be enough

Everything above is about the sixteen colours that **have** a `§` code. `TextColor::Rgb` has
none, and that is not a detail — it is what makes the producer, rather than the renderer, the
place a hex colour dies. `Text::to_legacy_string` can carry the sixteen through a `String`
because the font layer applies codes at draw time; it cannot carry hex at all, and
`to_plain_string` carries neither. So a surface whose producer flattens is hex-blind *however*
correct the draw is, and no amount of work in `VanillaFont` could recover it.

Six surfaces were in that state after the `§` census above, and all six now carry
`Vec<TextSpan>` from the producer down:

| surface | producer, before | now |
|---|---|---|
| title / subtitle overlay | `Sim::title_overlay` → `to_legacy_string` | `to_spans`, `HudFrame::title` is spans |
| action bar | `Sim::action_bar_overlay` → `to_legacy_string` | `to_spans`, `HudFrame::action_bar` is spans |
| boss bar title | `overlay::boss_bars_from` → `resolve_to_string` | `resolve(..).to_spans()`, `BossBarView::title` is spans |
| tab list header / footer | `tablist::banner_lines` → `to_plain_string` | `to_spans` then `overlay::spans_lines`, `TabListView::header`/`footer` are `Vec<Vec<TextSpan>>` |

The draws moved with them, from the `Builder::text`/`text_width` pair to
`text_spans`/`spans_width` — the pair the scoreboard sidebar already used. **Using one of each
is a real bug and a quiet one**: the measure decides where a centred line starts, so a mismatch
shifts every glyph in `x` rather than failing.

`overlay::spans_lines` is the one new piece of logic. A server writes a multi-line tab-list
banner as literal `\n` *inside* one component, so the split has to happen over spans; splitting
a flattened string would have been simpler and would have thrown away exactly the colour the
change exists to keep. It carries each run's style across the break, so a coloured banner stays
coloured on both lines.

**Chat is still hex-blind, and it is the one that needs real work.** `ChatLog` flattens with
`to_legacy_string`, and `hud.rs`'s wrapping (`wrap_legacy` / `wrap_legacy_with`, plus
`ChatWrapCache`, plus `strip_legacy` for `options.chat.color == false`) all operate on `String`.
Span-aware wrapping means: a `wrap_spans` that breaks a `Vec<TextSpan>` on word boundaries while
carrying style across each break (`spans_lines` is the newline-only ancestor of it), a
`ChatWrapCache` keyed on something other than a `&str`, a span-aware `strip_colour`, and
`ChatLog` keeping spans rather than a legacy string. That is a piece of work in its own right,
not plumbing, and it is why chat was scoped out of the change above rather than done badly.

The discriminating input for any of this is **a colour with no legacy equivalent**. A named
colour cannot separate a working span path from a legacy fallback, because the fallback gets it
right. `tests/text_colour.rs`'s producer gate uses six pairwise-distinct hex values, one per
surface, so a surface drawing another surface's text also fails.

### Per surface, in detail

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
`description` now goes through `Text::from_json(...).to_spans()`.
`ServerStatus` gained `motd_spans` alongside the plain `motd`, and the plain string is
*derived* from the spans so the two cannot disagree.

Expansion matters because MOTDs mix both conventions, routinely in one
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

**Disconnect / kick screen** — the fold, not the draw. `Sim::poll_net`'s
`NetUpdate::Disconnected` arm did
`self.resolve_text(&reason).to_legacy_string()` and then
`format!("disconnected: {reason}")`, so the styled tree became a `String` two layers
above any renderer. Nothing downstream *could* draw a span — every screen on that
path was innocent. Three changes:

* `SessionPhase::Ended` carries a `SessionEnd { kind, reason: Text }`
  (`lodestone-ecs/src/session.rs`) instead of a formatted string, so the reason
  stays a tree from the wire to `error_frame`.
* `MenuNotice` gained `spans: Vec<TextSpan>` beside its plain `text`, and
  `draw`'s notice block uses `restyle_wrapped` + `Builder::text_spans` when they are
  present — the same two functions the MOTD already used. A notice with no spans
  (every one the shell authors itself) takes the flat `colour` path unchanged.
* The `"disconnected: "` prefix is gone, because it was ours.
  `DisconnectedScreen` puts its `title` in a separate `StringWidget` *above* the
  reason's `MultiLineTextWidget`; it never glues it on. `SessionEndKind` is what
  picks that title, per vanilla: `disconnect.lost` ("Connection Lost") for a
  server-sent disconnect (`ClientCommonPacketListenerImpl.onDisconnect`) and
  `connect.failed` ("Failed to connect to the server") for a client-side failure
  (`ClientHandshakePacketListenerImpl.onDisconnect`, `ConnectScreen`).

`SessionEndKind` is also why a client-side failure is no longer logged as though it
were a kick: `NetUpdate::Error`'s arm `tracing::error!`s the cause where the
disconnect arm does not need to, because a disconnect carries text the player sees
and a failure carries a Rust error nobody was printing.

**The corpus had a blind spot here and it is worth remembering the shape.** A
`Text` with a nested `extra` and an **empty root `text`** — the ordinary shape of a
server's kick message — was covered on the MOTD path
(`motd_keeps_colour_from_json_and_from_legacy_codes`) and on the v770 server-encode
path (`nested_components_survive_the_nbt_encoding`), and *every* disconnect fixture
in `sim/tests.rs` was a flat single component. So the parser was demonstrably fine
and the consumer was demonstrably untested, which is exactly the combination that
reads as covered. `a_kick_reason_keeps_the_server_s_colours_through_frame_and_draw`
now drives that shape with a root colour, a child overriding it, and a second child
inheriting it — three readings of inheritance give three different span lists, so
one colour could not have discriminated.

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
- **Never add a "plain" glyph or string path back to `VanillaFont`.** It reads as an
  optimisation for strings that cannot carry a code, and there is no such string here: any
  `String` that came off the wire can. That belief is what produced this whole class of bug,
  and it was written down as a doc comment on `glyph` (*"`run` draws plain `&str`s that can
  never carry a `§` code"*) at the moment five surfaces were feeding it exactly such strings.
- **A new surface should take `Vec<TextSpan>` from `Text::to_spans`, not a `String`.** Every
  `String` hop through `to_legacy_string` or `to_plain_string` loses something: the first loses
  hex, the second loses everything. The remaining `String`-carrying HUD fields
  (`title`, `action_bar`, `held_item`) are the ones still paying that; converting them means
  changing `sim/session.rs`'s projection, which is where the `to_legacy_string` calls are.
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
- **Chat hex is still lost**, at `Text::to_legacy_string`. The fix is to route chat through
  spans, which needs span-aware *wrapping* — `wrap_legacy` currently works over a `String`.
  **Do not instead teach the legacy string the BungeeCord `§x§r§r§g§g§b§b` form**, which reads
  as the cheaper change and would be wrong: vanilla 26.2 does not honour that dialect
  (`ChatFormatting.getByCode('x')` is null), so a client that did would disagree with vanilla
  on every such string. An earlier version of this doc recommended exactly that.
- **A jar-less run still colours text.** `Builder::text_spans` and `Quads::text_spans` both
  have a fixed-advance fallback that keeps per-span colour, matching `text_legacy`'s.

## Configuration

- `options.chat.color` (`false` strips colour from chat via `hud::strip_legacy`). It has no
  effect on the sidebar or MOTD, matching vanilla, where the option is chat-specific.
- No env vars, no feature flags. The vanilla font is jar-sourced and optional
  (`VanillaFont::shared()`); every path degrades to the fixed-advance debug font.

## Dependencies

- `lodestone-model` — `Text`, `TextSpan`, `TextStyle`, `TextColor`, `LEGACY_PREFIX`, `to_spans`
  (expanding), `to_spans_ignoring_legacy_codes` (allowlisted), `from_legacy`, `from_json`,
  `from_nbt`. Owns the colour table and the `StringDecomposer` port.
- `lodestone-net` — status-ping decode; **now depends on `lodestone-model`**, because a
  `description` is a chat component and parsing one is component parsing.
- `lodestone-game` — `text::resolve` (translate lowering, style-preserving), the scoreboard
  fold, `ChatLog`.
- `lodestone-assets` — `Font`/`RasterFont` advances and glyph rasters from the real jar.
- `crates/lodestone-shell/tests/text_colour.rs` — the gate, with its executed negative control.
