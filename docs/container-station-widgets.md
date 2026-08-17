# The container station widgets: enchanting, stonecutter, loom

## What it is

The three screens whose `ClientAction::ContainerButtonClick` producer lives in
`crates/lodestone-shell/src/container/{enchant,stonecutter,loom}.rs`: the
enchanting table's three enchant-offer rows, the stonecutter's up-to-32-recipe
scroll grid, and the loom's up-to-32-pattern scroll grid. `ContainerButtonClick`
was encoded by every protocol family with zero shell callers before this — the
same outbound-island shape `ClientAction::SetFlying`/`SetBeaconEffects` were
caught in — and these three modules are its producers.

This doc covers what the three share (a first-refusal click chain, and — for
the two recipe/pattern grids — a client-side mirror of a server-authoritative
computation) rather than repeating each module's own doc, which covers its
own real layout constants and click geometry in full.

## How it works

### The shared shape: predict, then send

All three follow the same discipline `container-cost-screens.md` already
documents for the enchanting table: a hit *is* the send. There is no local
pending state to update first (contrast the beacon's power buttons, which
stage a selection and only send on confirm) — a click that hits a real,
currently-valid offer/recipe/pattern is itself the whole action, pre-validated
client-side the same way vanilla's own menu mirror gates `clickMenuButton`
before it ever reaches the network. None of the three closes the screen on a
successful click; vanilla's own `EnchantmentScreen`/`StonecutterScreen`/
`LoomScreen` all stay open after a selection.

### The shared click chain: first refusal, in `app/lifecycle.rs`

A `MouseInput` press against an open container screen offers the click to
each special-screen surface in turn, before falling through to the ordinary
slot hit-test:

```text
merchant trade rows -> beacon buttons -> enchant offers -> stonecutter grid -> loom grid -> recipe-book panel -> ordinary slot click
```

Each `handle_*_click` (`app/container_input.rs`) returns whether it
**consumed** the click; a later stage in the chain only runs if every earlier
one refused. This matters because none of these buttons occupy real slot
rects, but a screen this contended is exactly where a click path that forgets
to check the earlier stages gets away with it silently — the first-refusal
shape is what lets a new click surface be added without re-deriving the
whole precedence order by hand each time. Only a **press** is offered to any
of the special surfaces; a release still reaches the ordinary
`MenuInput::release` unconditionally, so an in-flight drag that started on a
real slot can still terminate correctly.

### The stonecutter/loom pair: a client-side mirror of a server computation

The enchanting table's three offers are numeric (`container_data` properties
the server already streams), so `enchant::offer_clickable` only needs
arithmetic. The stonecutter and loom are different: their offer *lists*
themselves are server-computed data the client has to reconstruct, not read
off the wire as a count.

- **Stonecutter** (`container::stonecutter::matches`): filters the shell's
  already-loaded `lodestone_game::recipe::RecipeBook` for
  `Recipe::Stonecutting` entries whose ingredient matches the input slot's
  item, sorted by recipe id. This is a client-side re-derivation of
  `lodestone-server`'s own `crate::stonecutting::matches` — both walk the
  same jar-sourced recipe corpus, independently, and agree because the corpus
  agrees, not because one calls the other.
- **Loom** (`container::loom::{is_banner_item, is_dye_item, is_pattern_item,
  selectable_pattern_count}`): the loom's offer list is not recipe data at
  all — it is a small hardcoded table transcribed from
  `tags/banner_pattern/pattern_item/*.json` (ten pattern items, each granting
  exactly one pattern — **not** always its own name; `bordure_indented_banner_pattern`
  grants `curly_border`, `field_masoned_banner_pattern` grants `bricks`) and
  `tags/banner_pattern/no_item_required.json`'s 32-entry base grid. This table
  is duplicated, not shared, between `lodestone-server::loom` and
  `lodestone-shell::container::loom` — there is no crate boundary a shared
  constant could cross here, so both sides transcribe the same jar files
  independently. **If a future pattern item needs a row, re-derive the
  mapping from the jar's own tag JSON on both sides** — do not guess the
  identity mapping most rows happen to follow, and do not update only one
  side.

Both mirrors exist for the same reason vanilla's own client-side menu mirror
does: it lets the grid pre-validate a click's index against the real offer
count before ever sending it, rather than trusting every click blindly and
letting the server silently ignore an out-of-range one.

### Layout: each grid's own real vanilla constants, never shared

The stonecutter is a 4×3 grid of 16×18 cells (`RECIPES_X = 52, RECIPES_Y = 14`,
twelve visible at once); the loom is a genuinely different shape, a 4×4 grid
of 14×14 cells (`PATTERNS_X = 60, PATTERNS_Y = 13`, sixteen visible at once).
These were derived independently from `StonecutterScreen.java`/
`LoomScreen.java`'s own declared constants — **not** from each other, per
CLAUDE.md's own warning that a derived constant tuned against a sibling
you also ported can cancel a bug in that sibling rather than reproduce
vanilla's own (independent) two numbers. Each module's own test suite pins
its grid arithmetic against the transcribed formula directly.

### Scrolling: wheel-only, wired on both grids; the thumb drag is a disclosed cut

Both grids' scroll math is a direct port of vanilla's own
`mouseScrolled`/`mouseDragged` shared tail
(`startIndex/startRow = (scrollOffs * offscreenRows + 0.5) [* columns]`,
`scrollOffs` clamped `0.0..=1.0`). Only the **wheel** input feeds it:
`WindowApp::scroll_stonecutter`/`WindowApp::scroll_loom`
(`app/container_input.rs`) each advance a persisted `f32` offset
(`WindowApp::stonecutter_scroll`/`loom_scroll`) by one `mouseScrolled` step
per notch, wired into the same `MouseWheel` arm
(`app/lifecycle.rs`) that already handles a bundle's scroll-to-select
highlight — bundle scroll gets first refusal (a bundle slot under the cursor
is a real, hoverable target), and only when it declines does the notch reach
the stonecutter/loom fallback.

**The scrollbar thumb drag is not wired**, on both screens. Vanilla's own
`mouseDragged` for these two screens is not even internally consistent about
its own click-vs-drag geometry (the loom's `mouseClicked` starts a drag from
a track four pixels above where `mouseDragged`'s own continuation computes
against) — reproducing that surface was judged not worth the plumbing when
the wheel alone already reaches every offer past the visible window on both
grids. If a future pass adds it, port the *continuation* formula
(`mouseDragged`'s own `yscr`/`yscr2`), not the click-start rect, since that
is the one that actually decides the resulting `scrollOffs`.

### What is deliberately not drawn

None of these three modules draws a visual grid — no icon per recipe/pattern
cell, no selected/highlighted sprite state. This matches the enchanting
table's own precedent (`container-cost-screens.md`): the click surfaces exist
and pre-validate correctly, but the pixel-level grid art is out of scope for
all three, a disclosed cut rather than a silent one.

## How to change it

- A new click surface in this family follows the same shape: a `hit_test_local`
  taking whatever bounded count/geometry the screen needs, a `button_hit_test`
  wrapper resolving panel origin/scale (`super::layout::panel_origin_with_scale`
  plus `crate::config::calculate_gui_scale`), a `handle_*_click` in
  `app/container_input.rs`, and one more link added to the first-refusal
  chain in `app/lifecycle.rs` — both the click-consumption `let` chain and its
  final `if !consumed_by_*` guard need the new name added to *every* later
  stage, not just its own line.
- If the loom or stonecutter ever need their pattern/recipe *list* itself to
  change, re-verify against the jar source named above — do not assume the
  transcribed table is exhaustive without re-checking it, per CLAUDE.md's own
  "re-verify before routing around 'X doesn't exist yet'" rule.

## Configuration

None — no flags or env vars gate any of this.

## Dependencies

- `crates/lodestone-game/src/{menu.rs,recipe.rs}` — `SpecialLayout`/`Menu::loom`/
  `Menu::stonecutter` and the `RecipeBook` the stonecutter mirror reads.
- `crates/lodestone-shell/src/container/{enchant,stonecutter,loom}.rs` — the
  three click surfaces.
- `crates/lodestone-shell/src/app/container_input.rs` — the `handle_*_click`/
  `scroll_*` producers.
- `crates/lodestone-shell/src/app/lifecycle.rs` — the first-refusal chain and
  the `MouseWheel` fallthrough.
- `crates/lodestone-shell/src/sim/session.rs`'s `send_container_button_click`
  — the actual wire send, shared by all three.
- `lodestone-server`'s `crate::{enchanting, stonecutting, loom}` modules — the
  server-authoritative computations these three mirror or feed numbers from.
  Not a build dependency — the agreement is by construction (the same jar
  source), not by a shared crate.
- [`container-cost-screens.md`](container-cost-screens.md) — the enchanting
  table's own cost-number wiring, and the general anvil/grindstone/smithing/
  enchanting screen machinery this family sits beside.

## Verification

```bash
cargo test -p lodestone-shell --lib --no-fail-fast -- container::enchant:: container::stonecutter:: container::loom::
```
