# HUD vitals animations

## What it is

Three cosmetic, client-side-only animations on the survival vitals cluster,
ported from vanilla's `Hud` class
(`.cache/mc/26.2/client-src/net/minecraft/client/gui/Hud.java`): the heart row
flashes and jitters around a health change, the hunger row wobbles while
saturation is empty, and a hotbar item "pops" (squashes then settles) when a
stack lands in a slot. Issue #30.

**A fourth item in that issue's original text does not exist in vanilla.**
"XP bar flashes on level-up" was never real: `Player.giveExperienceLevels`
(`Player.java:1561-1574`) only plays `PLAYER_LEVELUP` every 5 levels and draws
nothing. `ContextualBar.extractExperienceLevel`
(`ContextualBar.java:34-40`) and `ExperienceBar.extractRenderState`
(`ExperienceBar.java:35-36`, empty) confirm there is no timer anywhere in the
XP bar's own render state. This is recorded rather than silently dropped
because the issue asked for a citation on every item, and "no citation
exists" is itself the answer for this one — see `CLAUDE.md`'s §2 on stale
issue text.

## How it works

### Why a wall clock, not the server's tick

Every duration below is vanilla's own, in **ticks** (20/second, 50ms each).
Nothing forwards the server's tick counter as far as `hud.rs` — `HudFrame`
carries current-value vitals only, and threading one through would mean
reaching into `sim.rs`/`app.rs` for a feature with no server-visible effect.
`hud/anim.rs`'s `wall_tick` divides a wall-clock `Instant::elapsed` by 50ms and
uses that as the substitute, the same trade `app.rs`'s chat-caret blink
already makes (`chat_caret_visible`, wall time instead of a tick count — "the
caret keeps blinking while the game is paused"). Every state machine
downstream of `wall_tick` is a pure function of the resulting `i64`, so all of
it is unit-tested with literal tick numbers and no timing flakiness; only
`wall_tick` itself touches `Instant`.

`HudRenderer` owns the one wall-clock origin (`anim_start`) and the two pieces
of cross-frame state (`heart_anim: anim::HeartAnim`, `hotbar_pop:
anim::HotbarPop`), computed once per `render_with_item_models` call into a
`HudAnim` snapshot that is threaded down into `HudGeometry::build_inner`,
`sprite_vitals` and `draw_hotbar_items`. `HudGeometry::build`/
`build_with_font`/`build_with_gui` — the pure, jar-less, deterministic entry
points every pre-existing geometry test calls — pass `HudAnim::NONE` (idle),
so none of them grew a wall-clock dependency and all keep drawing
pixel-identically to before this existed.

### Heart row: blink (flash) + critical-health jitter

`hud/anim::HeartAnim` ports `Hud.java:766-781` (`lastHealth`/`displayHealth`/
`healthBlinkTime`/`lastHealthTime`) as a pure `tick(tick, health) -> (blink,
display_health)` state machine:

- **Damage** (health drops): a 20-tick blink window (`Hud.java:768-770`).
- **Heal** (health rises): a 10-tick window (`Hud.java:771-773`), not 20.
- **`blink`** alternates 3-on/3-off inside the window
  (`(blink_until_tick - tick) / 3 % 2 == 1`, `Hud.java:766`) — and is read
  from the *previous* call's window before this call's comparison updates it,
  matching vanilla's own statement order (the read at `:766` runs before the
  reassignment at `:770`/`:773`), so a hit's blink becomes visible starting
  the *following* tick, not the one that registered the change.
- **`display_health`** ("the ghost of health about to be lost") only catches
  up to the current value once 1000ms (20 ticks) have passed with no further
  change (`Hud.java:776-779`).

`sprite_vitals` (`hud.rs`) uses `blink` twice: the **container** background
sprite swaps to `hud/heart/container_blinking` for every heart slot regardless
of that slot's own fill state (`Hud.java:871`), and a **ghost** overlay draws
the pre-damage total in the `_blinking` sprite variant wherever
`halves < display_health` (`Hud.java:882-885`).

Separately, `hud/anim::heart_jitter(tick, container)` ports the critical-health
y-jitter (`Hud.java:863-865`, `currentHealth + absorption <= 4`): every heart
container redraws with a fresh `0..=1`px offset. `HudFrame` does not model
absorption yet, so this gates on health alone — a documented narrowing, not a
silent one.

### Hunger row: the empty-saturation wobble

`hud/anim::hunger_wobble(tick, food, saturation, pip)` ports `Hud.java:977-979`
exactly: `saturation <= 0.0 && tick % (food * 3 + 1) == 0` gates a fresh
`-1..=1`px offset; any other tick draws flush. Unlike the heart row this needs
**no cross-frame memory** — it is a pure function of the current tick, food
and saturation, called once per pip in `sprite_vitals`.

`HudFrame::saturation: Option<f32>` is the new field this reads. **It is not
wired from a live server yet** — see [Known gap](#known-gap-saturation-is-not-yet-threaded-from-sim.rs)
below. `None` is treated as "not empty" (flush row), so a caller that has not
wired it through draws exactly as before this field existed.

### Hotbar: the pickup "pop"

`hud/anim::HotbarPop` ports `ItemStack.popTime`
(`ItemStack.java:153,713-714,1047-1052`), set to `5` by `Inventory.add`
whenever a stack merges into or fills a slot (`Inventory.java:220,268`) and
decremented once per tick. Nothing forwards that server-side call site here,
so `HotbarPop::tick` detects the same event client-side: a slot's item
identity changed, or its count rose, versus the previous frame's contents.
A **decrease** (using an item, dropping it) does not pop, matching vanilla —
`Inventory.add` is the only call site that sets `popTime`, and nothing calls
it on removal.

The draw side is `hud/item_icon::draw_item_icon_popped`, a self-contained
sibling of `draw_item_icon` (duplicating its short decorations tail rather
than sharing it, so the container screen's `draw_item_icon_counted` is
untouched by this existing). `pop_squeeze_rect` is the pure transform math,
factored out precisely so it is checkable with no atlas, no sink and no GPU:

```java
// Hud.java:1146-1152
float pop = itemStack.getPopTime() - deltaTracker.getGameTimeDeltaPartialTick(false);
if (pop > 0.0F) {
   float squeeze = 1.0F + pop / 5.0F;
   graphics.pose().pushMatrix();
   graphics.pose().translate(x + 8, y + 12);
   graphics.pose().scale(1.0F / squeeze, (squeeze + 1.0F) / 2.0F);
   graphics.pose().translate(-(x + 8), -(y + 12));
}
```

This is an axis-aligned, non-uniform scale about the **fixed point** `(x + 8,
y + 12)` — not a rect re-centred on that point. `8` happens to be half the
16px icon's width, so for `x` the two are equivalent; `12` is not half its
height, so for `y` they are not, and the first implementation of
`pop_squeeze_rect` got exactly this wrong (see `item_icon.rs`'s
`pop_five_is_a_2x_squeeze_at_the_vanilla_pivot`/
`pivot_scales_with_icon_size_not_just_the_rect` tests for the two predicted
values that separate the wrong hypothesis from the right one). The general
rule used: each edge moves to `pivot + (edge - pivot) * scale`.

**Only the flat sprite icon layer squashes.** A 3-D block-item mini-icon or a
special-renderer (chest) icon draws undistorted at the original square rect —
vanilla's single pose-stack transform covers all three; this is a deliberate,
documented narrowing (most hotbar items are flat sprites), not a
decode-parity claim. The durability bar and stack count also draw unsquashed,
matching vanilla's own `graphics.itemDecorations` call sitting *after* the
pose is popped (`Hud.java:1155-1160`).

### Why the jitter is not vanilla's exact RNG sequence

Vanilla reuses one `RandomSource`, reseeded once per `extractPlayerHealth`
call (`Hud.java:783`, `random.setSeed(tickCount * 312871)`) and consumed
sequentially across heart containers and food pips in a fixed draw order.
Reproducing that exact sequence buys nothing visible — nobody can
screenshot-diff a purely cosmetic jitter against a live server — and
`docs/sky-and-air-bubbles.md` already made the identical trade for the star
field's RNG ("same distribution shape, different exact positions, a visual
choice and not a decode-parity claim"). `hud/anim::jitter` is a small
splitmix64-style mix keyed by `(tick, salt)` instead, independent per
container/pip so two slots at one tick do not correlate.

## How to change it, and the gotchas

- **Derive every gate's layout from the same expression the draw uses.**
  `sprite_vitals`'s `row_y` comes from a moving `cluster_top` (pulled up only
  `if frame.hotbar`, again only `if frame.xp` — see `CLAUDE.md`'s own account
  of the HUD gate that measured 20px above a correctly-drawing row and
  reported 0px). None of the three animations here change that anchor
  formula; they only offset `y` by a jitter/wobble computed *after* `row_y`
  is resolved, exactly the pattern the pre-existing air-bubble row already
  used (`air_row_y = row_y - icon - 1.0`).
- **The "not animating" case must stay bit-identical.** `HudAnim::NONE` is
  the literal idle value every pre-existing `build`/`build_with_font`/
  `build_with_gui` call site passes; a settled `HeartAnim`/`HotbarPop` (no
  health/hotbar change ever observed) returns the same idle values by
  construction — see `heart_anim_idle_is_bit_identical_across_repeated_calls`
  and `hotbar_pop_settled_case_is_bit_identical`.
- **The very first observation must not read as an event.** Both
  `HeartAnim` and `HotbarPop` special-case their first `tick()` call: without
  it, a fresh connection at full health misreads as an instantaneous heal,
  and a hotbar that already holds items at HUD startup misreads as nine
  simultaneous pickups. This was caught by the tests, not inspection — see
  `heart_anim_first_observation_primes_without_a_false_blink` and
  `hotbar_pop_first_observation_primes_without_a_false_pop`; the latter's
  absence produced a **second-order** bug where a real, later decrease still
  read a nonzero pop left over from the phantom initial one.
- **New `HudAnim` fields go through `build_inner`'s one signature**, not a
  second parameter list — every draw site (`sprite_vitals`,
  `draw_hotbar_items`) already takes `&HudAnim`, so a fourth animation is a
  new field plus a new pure function in `hud/anim.rs`, not new plumbing.
- **`item_icon.rs`'s `draw_item_icon_popped` intentionally duplicates
  `draw_item_icon_counted`'s decorations tail** rather than sharing it. The
  container screen calls `draw_item_icon_counted` directly
  (`container.rs::builder::Builder::item_icon_counted`, off-limits to this change); refactoring that function
  to also serve the popped path would risk its signature, which nothing here
  needs to touch.

## Known gap: saturation is not yet threaded from `sim.rs`

`Vitals::saturation: Option<f32>` (`lodestone-ecs/src/session.rs:174-179`)
already exists and is already populated — the doc comment on that field even
says so ("no reader draws this today... `PlayerSnapshot::saturation` is a
public bot-API field"). `Sim` exposes `health()`/`food()`/`air()`
(`sim.rs::Sim::health`/`food`/`air`) from the same `Vitals` component but no `saturation()`.
Two one-line additions (outside this change's file ownership; flagged for the
orchestrator rather than made here) complete the wiring:

```rust
// sim.rs, immediately after `pub fn food`:
/// Server-reported food saturation, or `None` off a live survival server.
#[must_use]
pub fn saturation(&self) -> Option<f32> {
    self.vitals().saturation
}
```

```rust
// app.rs, alongside the existing `hud_frame.food = food;`:
hud_frame.saturation = self.sim.saturation();
```

Until that lands, `hud_frame.saturation` stays `None` and the hunger wobble
computes correctly but never triggers on a live server — a reported gap, not
a hidden one. `hud/anim::hunger_wobble` and its tests do not depend on this
wiring at all.

## Configuration

No new config, flags or constants. The three animations' timing constants
(`20`/`10`-tick blink windows, the `312871`-seed-equivalent jitter salts, the
`5.0`-tick pop decay) are transcribed from `Hud.java`/`ItemStack.java` and
live as literals in `hud/anim.rs`, matching the existing convention for the
sibling air-bubble/vanilla-font modules (named constants only where a reader
would otherwise have to re-derive a magic number; a single-use vanilla
literal is cited in the doc comment at its one call site instead).

## Dependencies

- `hud/anim.rs` — the three pure/near-pure state machines, no crate
  dependencies beyond `lodestone_assets::ResourceLocation` (for `HotbarPop`'s
  slot-identity comparison) and `std::time::Instant`.
- `hud/item_icon.rs` — `draw_item_icon_popped`/`pop_squeeze_rect`, sharing the
  existing `IconSink`/`IconAssets`/`GuiSpriteQuad` machinery `draw_item_icon`
  already uses.
- `HudRenderer` (`hud.rs`) — owns the wall-clock origin and both cross-frame
  animation states; `HudGeometry::build_inner` threads the resulting
  `HudAnim` snapshot into `sprite_vitals`/`draw_hotbar_items`.
- The vanilla `client.jar` for the `_blinking` heart sprites
  (`hud/heart/{container,full,half}_blinking`) — already present in the GUI
  atlas glob (`gui/sprites/**`), no atlas change needed, the same situation
  the air-bubble row found for its own sprites.
