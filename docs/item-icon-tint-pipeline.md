# Item-icon tint pipeline (issue #171) — scoping

## What it is

Issue #171 asks for the *item-icon/held-item* tint path: potion bottle
liquid colour, spawn-egg background/foreground dots, map marker/border
colours, and leather dye's icon form (armour dye's **worn 3-D model** form
is #17, already done, and out of scope here — see `crates/lodestone-assets/src/equipment.rs`'s
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
(`crates/lodestone-assets/src/item_model.rs:119-124`) already parses
**every** tint kind generically from an item model's `tints` array —
`minecraft:dye`, `minecraft:grass`, `minecraft:potion`, `minecraft:map_color`,
`minecraft:firework`, whatever a pack names — because parsing only reads the
`type` string and an optional constant `default`, never branches on which
kind it is. This was already true before this session; the parsing half was
never the gap.

The gap is **evaluation**: turning a `TintSource` into a live colour for a
concrete stack. `crates/lodestone-render/src/block_models.rs:1147-1154`
(`extruded_sprite_geometry`'s doc, current line numbers — the issue's own
citation of `805-816` has drifted, expected per `CLAUDE.md`'s "tracker lags
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

## Finding 2: potion, map-colour and leather-icon dye share one real blocker

The remaining three (`minecraft:potion`'s liquid colour, `minecraft:map_color`'s
border tint, and leather's dye colour in **icon** form) all need the same
thing evaluation cannot get today: **a typed read of the item stack's own
component data**, and that does not exist for any of the three components
involved.

```
$ grep -rn "dyed_color\|DYED_COLOR\|potion_contents\|POTION_CONTENTS" crates/lodestone-game/src/
(no output)
```

`crates/lodestone-game/src/item.rs`'s `ComponentValue` enum
(`item.rs:52-81`) has typed variants for `Int`, `Bool`, `Str`, `Text`,
`Tool`, `Enchantments` — the handful of components existing game logic
inspects — and everything else, including `minecraft:potion_contents`,
`minecraft:dyed_color` and (per `docs/filled-map-item.md`'s separate finding)
whatever a filled map's own colour would come from, lands as
`ComponentValue::Opaque(Vec<u8>)`: **structurally present** on the stack
(item components decode generically over the wire) but not interpretable —
`ItemComponents::get_int`/`get_str` cannot read an opaque blob, only `Int`/
`Str` variants.

Adding a typed variant (or a component-specific NBT reader) means editing
`crates/lodestone-game/src/item.rs`, which this task's file ownership
assigns to the cost-screens agent, not this one (`crates/lodestone-game/`
is listed off-limits in the briefing). So evaluation was **not** built
speculatively against data this crate cannot yet read — per this session's
other finding for #184, that would be exactly the island `CLAUDE.md` warns
about: individually testable with synthetic data, reaching zero pixels in
play because the real stack's colour is never actually reachable.

**`TintSource.default` is the correct fallback until this lands.** Every
one of the three tint kinds carries a jar-authored `default` (e.g.
`minecraft:potion`'s is `-13083194`, `assets/minecraft/items/potion.json`),
and that default is what a stack with no override should show anyway
(vanilla's own base "no effect" bottle) — so the current untinted-white
behaviour is the wrong default and the fix, once evaluation exists, is
"use `TintSource.default`, then override from the decoded component when
present" — a two-tier fallback, not a full rewrite.

## What would unblock this, in order

1. **Type at least `minecraft:dyed_color`** (`{rgb: int}`, the simplest of
   the three payloads — a single top-level int, unlike potion's nested
   `{potion: <id>, custom_color: optional int}`) as a new `ComponentValue`
   variant or a dedicated accessor in `lodestone-game/src/item.rs`. Not
   this task's file ownership; flagged for the cost-screens agent or a
   dedicated follow-up.
2. **A per-item-variant tint table** in `lodestone-render`, parallel to but
   separate from `BlockModels::tint_palette` — keyed by `(item, layer
   index)` rather than a block's biome-position lookup, since the colour
   here comes from the *stack instance*, not world position. This is
   in-ownership work (`block_models.rs`), gated entirely on (1).
3. **Potion and map-colour** each need their own resolution logic beyond a
   raw int read (potion's `custom_color` overrides a *looked-up* base
   colour from the potion type when absent; map-colour is presently
   unblocked-by-data at all per `docs/filled-map-item.md`), so they are
   real follow-up work even after (1) and (2) land, not automatic
   consequences of them.

## Dependencies

- `crates/lodestone-assets/src/item_model.rs` — `TintSource` parsing
  (already complete, unchanged by this session).
- `crates/lodestone-render/src/block_models.rs` — `extruded_sprite_geometry`,
  where per-instance tint would be applied once (1)-(2) above land.
- `crates/lodestone-game/src/item.rs` — `ComponentValue`, the blocking
  prerequisite; out of this task's ownership.

## Related

- `docs/banner-shield-patterns.md` (#174) — hits the identical
  `ComponentValue::Opaque` wall for `minecraft:banner_patterns`, independent
  discovery, same root cause.
- `docs/filled-map-item.md` (#184) — a *different* kind of missing data (no
  packet decode at all, not just an untyped component), documented
  separately so the two are not conflated.
- Issue #17 — leather armour's **worn** dye, already shipped; this issue's
  leather bullet is the icon form only.
