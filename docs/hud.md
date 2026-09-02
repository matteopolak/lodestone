# The HUD

## What it is

The game HUD: the bottom-centre vitals cluster (hotbar, XP bar, hearts, hunger, armour, air bubbles,
action bar), the vitals' cosmetic animations, the Tab player-list overlay, the scoreboard sidebar, and
the held-item name tooltip. Unlike the menu screens (see [`ui-framework.md`](./ui-framework.md)), the
HUD is not a widget tree — it is rebuilt fresh from live game state every frame, so its layout is a set
of pure functions of that state rather than a set of persistent objects with their own lifecycle.

## How it works

### Layout: two anchors, everything else derived

Every HUD row lives in **logical-canvas pixels** — the physical framebuffer divided by the effective
integer GUI scale, which is exactly vanilla's own `guiScaledWidth`/`guiScaledHeight` space, so a vanilla
`guiHeight - N` expression transcribes directly with no unit conversion. Two absolute anchors drive
everything: the hotbar sits flush at the bottom with no margin, and the health/hunger row's baseline
(`vitals_line_base`) is `height - 39`. Every other row — armour, air bubbles, the XP bar and its level
number, the action bar, the held-item name — is expressed as an offset from one of those two anchors,
never stacked upward from the row below it. Building the cluster by stacking from a movable base was a
real, already-fixed bug class here: it makes every row's position depend on which other rows are
present (a hotbar vs. no hotbar, survival vs. creative), where vanilla's own anchors take no such
branch. A pixel gate for any HUD row should call the same anchor function the draw uses rather than
restate the offset as a literal — three separate gates once each held their own copy of a margin
constant, and all three broke the same day for the same underlying fix.

One offset in this cluster looks wrong on a single read and is not: the air-bubble row's y is computed
through *three* additive terms in vanilla (a base offset, a conditional subtraction when no vehicle
health row is present, and a rowOffset correction inside the bubble draw itself) that cancel down to a
single flat offset for an unmounted player. Reading only the first step gives the wrong answer; the
full three-step derivation is what the bubble row's position must be reproduced from.

### Vanilla text: the font stack every HUD surface shares

Every string the HUD draws — chat, the F3 overlay, titles, the action bar, the scoreboard, the tab
list, stack counts — renders through vanilla's real proportional font (loaded once from the client jar,
fail-open to a fixed-width fallback when no jar is present) rather than a fixed-advance debug bitmap.
Glyph advances are derived from each glyph's own rasterized coverage, matching vanilla's own
alpha-column measurement, so ink and advance can never disagree. Glyph coverage draws as merged runs of
quads on the HUD's ordinary colour vertex stream — no font atlas, no texture upload, no extra bind
group, which matters because the model shader is already at the GPU's four-bind-group floor (see
`ui-framework.md`).

Bold, italic, underline, strikethrough and the obfuscated (`§k`) style all draw real geometry rather
than being layout-only: bold redraws the same glyph offset in x; italic shears each texel row by an
affine function of its vertical position rather than shearing one whole quad; underline and
strikethrough are fixed-height bars at two different, independently-transcribed y-offsets; obfuscated
swaps in a same-width-class replacement codepoint every draw call from a free-running counter, which is
what makes it read as continuously animated with no timer. The drop shadow is a second full pass at 25%
of the main colour, computed in the same gamma space the HUD's colour convention already uses — taking
that quarter in linear space would produce a visibly lighter grey outline instead of vanilla's near-black
one. Text is measured with the same font it will be drawn with at every layout call site, so a centred
or right-aligned string can never be laid out against a different font than the one that renders it.

### Text scale: one absolute factor per surface, never an ambient multiplier

Each HUD text surface has its own literal scale transcribed from vanilla — the title draws at 4×, the
subtitle at 2×, the action bar and held-item name at 1× (unscaled), the F3 overlay, tab list and
scoreboard sidebar all at 1× in the already GUI-scale-divided logical canvas. There is no shared,
HUD-wide "legibility" multiplier layered on top of any of these — an ad-hoc doubled pitch used to exist
and silently doubled several surfaces beyond their real vanilla size before being deleted entirely.
`guiScale` is vanilla's only general text-size control; there is no per-surface size option for the
title/subtitle/action bar trio, and none should be invented — adding one would be a parity divergence
with nothing on the vanilla side to match. Chat's own scale option (`chatScale`) is real but applies
only to the chat scrollback log in vanilla, never to the chat input line, its caret, or the suggestion
dropdown — those draw outside chat's own scale bracket even in vanilla.

### Vitals animations: blink, jitter, wobble, pop

Four cosmetic client-only animations on the vitals cluster, each a pure function of a wall-clock-derived
tick counter (nothing forwards the server's real tick count this deep into the HUD, so a wall-clock
substitute divided into 50ms steps stands in for it — the same trade the chat caret's blink already
makes):

- **Heart blink**: a health *change* opens a fixed window (20 ticks on damage, 10 on heal) during which
  every heart container swaps to a "blinking" sprite variant and a ghost overlay shows the pre-change
  health total fading back to the real value.
- **Low-health jitter**: every heart container gets a fresh small vertical jitter every frame while
  current health is at or below a fixed low threshold — independent of the blink window above.
- **Hunger wobble**: the hunger row wobbles with a small vertical offset on ticks divisible by an
  interval derived from the current food level, gated on saturation being empty — a level trigger, not
  a change trigger.
- **Hotbar item pop**: a slot whose item identity changed or whose count rose (never decreased) squashes
  and un-squashes over a few ticks, an axis-aligned non-uniform scale about a fixed pivot point that is
  *not* the icon's geometric centre on one axis — getting the pivot wrong is a natural mistake since it
  looks like an off-centre rect rather than an obviously wrong scale. Only the flat sprite icon layer
  squashes; 3-D block icons, durability bars and stack counts all draw undistorted at their normal rect,
  matching vanilla's own pose-stack scoping.

None of these reproduce vanilla's exact RNG sequence for jitter/wobble offsets — only the same
distribution shape, since nobody can screenshot-diff a purely cosmetic jitter against a live server.

**A fifth vanilla vitals animation — the Regeneration effect's travelling heart "wave," a fixed −2px
offset that visits one heart container per tick on a repeating cycle while Regeneration is active — is
not yet implemented.** It is a distinct mechanism from the blink above: it is gated on the *status
effect being present*, not on a health change, and moves nothing during either a damage or a heal event.
A wave gated on a health delta instead of the effect itself is the wrong shape entirely and is the
mistake to avoid if implementing it. Also worth carrying forward if this lands: vanilla's health fill
uses an integer ceiling of the raw health float, not a float comparison — a float-based fill can show an
apparently-empty heart row at fractional health values above zero, which reads as "alive at zero hearts."

### Tab list

The player list shown while Tab is held. Three layers: folded server state (who's online, and what the
server said about them), a per-frame projection (styled display names, sorted and limited to the same
count vanilla shows, ping mapped to one of six sprite bands), and geometry (column layout past twenty
players, following vanilla's own column-growth loop). A player's display name can be colored two
different ways — an explicit server-sent display-name component, or scoreboard team coloring applied to
a player with no explicit name — and both have to be checked, since the team-coloring path is the more
common one in practice (a server that colors names by team rarely also sends an explicit display-name
component). Only *listed* players appear here; chat's tab-completion reads the full online-player set,
which is a different, unfiltered projection over the same underlying roster.

### Scoreboard sidebar

The right-edge panel listing an objective's per-player scores, folded from the server's scoreboard state
into a title-plus-rows structure and then laid out at vanilla's own fixed metrics. Vanilla's vertical
placement formula puts the panel bottom slightly *above* true vertical centre by design (`height/2 +
panelHeight/3`) — porting that as a symmetric centering is a plausible-looking regression. The score
column's spacer between label and number only exists when the row actually has a score to show; a
zero-width score format should not still reserve the gap.

### Held-item name tooltip

The item name that briefly appears centred above the hotbar whenever the selected item's *identity*
changes — not whenever the selected hotbar slot changes. Switching between two slots holding an
identical item does not restart the timer; it only continues counting it down. It holds at full opacity
for its whole duration and only fades in its last ten ticks — there is no fade-in, unlike a naive
reading of "fade" might suggest. Font styling (forced italic for a custom-named item) depends on the
same styled-text draw path documented above under vanilla text. Item rarity coloring and a same-page
creative/spectator vertical offset are both known, narrower gaps, not modelled because the game-mode/
rarity data needed to drive them doesn't reach the HUD frame yet.

## How to change it

- **Derive every offset from a named vanilla anchor or expression, never restate a pixel constant.** A
  restated offset silently drifts from the draw the moment a row's presence or a container's row count
  changes; every layout bug recorded in this cluster's history was exactly this shape.
- **A HUD text surface's scale is an absolute, vanilla-derived literal — never an ambient multiplier
  layered on top of the GUI-scale-divided logical canvas.** If a surface looks 2× too big (or too
  small) at every GUI scale uniformly, suspect a stray multiplier before suspecting the logical-canvas
  math itself.
- **Font metrics and vertical gaps between rows are two different kinds of quantity — don't derive one
  from the other.** A row's distance from its neighbour is a real, independently-transcribed vanilla
  constant, not a function of glyph height.
- **Keep the vanilla font's glyph-ink cache independent of caller state** (tint, screen position, GUI
  scale, italic pose, shadow) — the cache holds only raster-derived data that's the same for every
  caller of a given font/codepoint pair; anything caller-specific belongs to the draw call, not the
  cache entry.
- **A pixel gate for HUD text needs to be a pixel gate, not a string/vertex-count assertion.** A font
  with every glyph correct and every *advance* wrong still passes a content or vertex-count check; the
  defect lives in the spacing between glyphs, which only a rendered-pixel comparison can see.
- **When animating a value that also drives a color transition (e.g. a level-up flash), render the
  "before" state first.** An animation that treats a first-ever value as a rising edge can paint the
  wrong color on the very first frame a gate observes it.

## Configuration

None of this cluster has its own config file. `gui_scale` (via the shared `logical_canvas` divisor) is
the one general control over HUD text and layout size; `chatScale` is the one vanilla option that scales
anything in this cluster beyond that, and only the chat scrollback log. Vanilla exposes no size option
for the title/subtitle/action bar, the F3 overlay, the tab list, or the scoreboard sidebar, and none
should be added.

## Dependencies

- `crates/lodestone-shell/src/hud.rs` and `hud/{anim,vanilla_font,item_icon}.rs` — layout, animation
  state machines, and the vanilla font draw path.
- `crates/lodestone-shell/src/tablist.rs`, `scoreboard.rs` — the tab-list and sidebar projections.
- `lodestone-game` — `tablist::TabList`, `scoreboard::Scoreboard`, `player_state::HeldItemHighlight`,
  the folded state every projection above reads.
- `lodestone-assets::font` — glyph metrics and rasterization, shared with every other text surface in
  the shell.
- `lodestone-model` — `Text`/`TextStyle`, the `§`-coded legacy formatting model.
- The 26.2 jar under `.cache/mc/26.2/{client-src,client.jar}` — behavioral reference only, never
  transliterated.
- [`ui-framework.md`](./ui-framework.md) — the menu-screen widget/layout model this HUD deliberately
  does *not* use.
