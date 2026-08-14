# Item-icon tint pipeline (issue #171) — scoping

## What it is

Issue #171 asks for the *item-icon/held-item* tint path: potion bottle
liquid colour, spawn-egg background/foreground dots, map marker/border
colours, and leather dye's icon form (armour dye's **worn 3-D model** form
is already done, a separate issue, and out of scope here — see `crates/lodestone-assets/src/equipment.rs`'s
`only_leather_is_dyeable_and_only_its_base_layer`). This codebase already
does biome (grass/foliage) tint and multiplies tint and shade in gamma
space; this issue is specifically the *data-driven, per-item-stack* tint
sources that are not the biome lookup.

This doc is the scoping the issue's own text asks for — "a scoping comment
establishing what already exists is valuable even before code" — because
the honest state turned out to split three ways: one bullet needs no work
at all, three are blocked on a real, identified prerequisite outside this
task's ownership, and the parsing half already exists and needed no changes.

## What already exists

`TintSource { kind: String, default: Option<i32> }`
(`crates/lodestone-assets/src/item_model.rs`) already parses
**every** tint kind generically from an item model's `tints` array —
`minecraft:dye`, `minecraft:grass`, `minecraft:potion`, `minecraft:map_color`,
`minecraft:firework`, whatever a pack names — because parsing only reads the
`type` string and an optional constant `default`, never branches on which
kind it is. This was already true before this session; the parsing half was
never the gap.

The gap is **evaluation**: turning a `TintSource` into a live colour for a
concrete stack. `extruded_sprite_geometry`'s doc comment
(`crates/lodestone-render/src/block_models.rs` — the issue's own
line citation has since drifted, expected per `CLAUDE.md`'s "tracker lags
the tree") states outright that every baked icon quad is **untinted**, by
deliberate narrowing: a per-item tint index would have to live in a
separate table from `BlockModels::tint_palette` (the *block* biome
palette), and nothing builds or reads that separate table today. This is
still true — confirmed by re-reading the current source, not assumed from
the issue text.

## Finding 1: spawn eggs need no tint work at all

The issue's own scope list includes "spawn-egg background/foreground dot
colours per mob type", carried over from versions where spawn eggs really
were grey-plus-two-tint-indices. **Checked against the real 26.2 jar, this
is no longer true and nothing needs building:**

```
$ cat assets/minecraft/items/pig_spawn_egg.json
{"model": {"type": "minecraft:model", "model": "minecraft:item/pig_spawn_egg"}}
$ cat assets/minecraft/models/item/pig_spawn_egg.json
{"parent": "minecraft:item/generated", "textures": {"layer0": "minecraft:item/pig_spawn_egg"}}
```

No `tints` array anywhere in either file — every spawn egg's two-tone
texture (`item/pig_spawn_egg.png`, etc.) is a **pre-coloured, per-mob PNG**,
not a grey mask tinted at render time. It already renders correctly through
the ordinary `IconPart::Sprite` path this codebase already draws — untinted
is *correct* here, not a gap. Checked for a representative sample (pig); the
`template_spawn_egg` model this issue's text implied does not exist under
`models/item/` at all, confirming each egg item owns its own baked-colour
model rather than sharing one with tint parameters.

## Finding 2 (superseded in part): dye and potion are now typed; map-colour is not

The remaining three (`minecraft:potion`'s liquid colour, `minecraft:map_color`'s
border tint, and leather's dye colour in **icon** form) all needed the same
thing evaluation could not get when this doc was written: **a typed read of
the item stack's own component data**. Two of the three now have it:

- `minecraft:dyed_color` is typed end to end — `lodestone_model::ItemComponents
  ::dyed_color`, decoded in `crates/protocol/v770/src/adapter/inventory.rs`,
  reaches `lodestone_game::item::ItemStack` via `DYED_COLOR_COMPONENT` and
  `.dyed_color()`, and `lodestone_assets::item_tint::resolve`'s `dye` arm reads
  it live.
- `minecraft:potion_contents` is typed the same way, but as an **effective**
  field rather than a raw one: `crates/protocol/v770/src/adapter/inventory.rs`
  decodes the component's potion/custom-colour/custom-effects and folds them
  through `lodestone_data::potion::potion_color` (a port of `Potion.calculate`
  / `PotionContents.getColorOr` / `getColorOptional`, with the potion's own
  built-in effect list from `lodestone_data::potion`/`crates/lodestone-data/
  src/generated/{potions,potion_effects,mob_effect_colors}.rs`) into
  `lodestone_model::ItemComponents::potion_color: Option<u32>` — an
  already-mixed opaque ARGB, not the raw component. It reaches
  `lodestone_game::item::ItemStack` via `POTION_COLOR_COMPONENT` and
  `.potion_color()`, and `item_tint::resolve`'s `potion` arm reads it live,
  reporting `TintProvenance::Component`.
- `minecraft:map_color` is still untyped — this doc's original finding stands
  for it, and for `docs/filled-map-item.md`'s separate "no packet decode at
  all" gap.

```
$ grep -rn "dyed_color\|DYED_COLOR\|potion_contents\|POTION_CONTENTS\|potion_color\|POTION_COLOR" crates/lodestone-game/src/item.rs
(DYED_COLOR_COMPONENT, dyed_color(), set_dyed_color(), POTION_COLOR_COMPONENT, potion_color(), set_potion_color() all present)
```

**The remaining blocker moved, it did not close.** Both resolvers are correct
and unit-tested against a stack built with real components, but the *only*
call site in the shell that resolves an item icon's tint for drawing —
`lodestone_shell::hud::item_icon::sprite_layer_tint` — still evaluates every
icon against `ItemTintContext::default()` (no stack in hand at all), for
every tint source including `dye`. So a decoded dyed leather item and a
decoded potion both carry the right colour all the way to
`lodestone_game::item::ItemStack`, and neither one is drawn correctly yet:
the gap this doc originally named at the *component-typing* layer is now at
the *draw call site* layer instead, and it is one gap for both sources, not
two.

**`TintSource.default` is still the correct fallback until that lands.**
Every one of the three tint kinds carries a jar-authored `default` (e.g.
`minecraft:potion`'s is `-13083194`, `assets/minecraft/items/potion.json`),
and that default is what a stack with no override should show anyway
(vanilla's own base "no effect" bottle) — so the current untinted-white
behaviour is the wrong default when nothing else is available, and it is
exactly what `item_tint::resolve` still falls back to whenever
`ItemTintContext::default()` reaches it.

## What would unblock this, in order

1. ~~Type at least `minecraft:dyed_color`~~ **Done** (`lodestone-game/src/
   item.rs`'s `DYED_COLOR_COMPONENT`), and `minecraft:potion_contents` is now
   typed the same way (`POTION_COLOR_COMPONENT`, this session). Only
   `minecraft:map_color` remains untyped.
2. **Thread a real stack through `sprite_layer_tint`** (and whatever calls
   it) instead of `ItemTintContext::default()`. This is now the sole blocker
   for both `dye` and `potion` reaching a drawn pixel — a per-item-variant
   tint table in `lodestone-render`, parallel to but separate from
   `BlockModels::tint_palette` (keyed by `(item, layer index)` rather than a
   block's biome-position lookup, since the colour here comes from the
   *stack instance*, not world position), is one way to carry it through to
   the GPU-facing code; a narrower fix that only threads `ItemTintContext`
   to the existing per-icon draw call is another. Either lands in
   `lodestone-render`/`lodestone-shell`'s render code, not in this doc's
   original file-ownership boundary.
3. **Map-colour** still needs its own resolution logic beyond a raw int read
   (its base colour is presently unblocked-by-data at all per
   `docs/filled-map-item.md`), so it remains real follow-up work independent
   of (1) and (2).

## Dependencies

- `crates/lodestone-assets/src/item_model.rs` — `TintSource` parsing
  (already complete, unchanged by this session).
- `crates/lodestone-render/src/block_models.rs` — `extruded_sprite_geometry`,
  where per-instance tint would be applied once (1)-(2) above land.
- `crates/lodestone-game/src/item.rs` — `ComponentValue`, the blocking
  prerequisite; out of this task's ownership.

## Related

- `docs/banner-shield-patterns.md` — hits the identical
  `ComponentValue::Opaque` wall for `minecraft:banner_patterns`, independent
  discovery, same root cause.
- `docs/filled-map-item.md` — a *different* kind of missing data (no
  packet decode at all, not just an untyped component), documented
  separately so the two are not conflated.
- Leather armour's **worn** dye, already shipped, is a separate issue; this issue's
  leather bullet is the icon form only.
