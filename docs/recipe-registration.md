# Runtime recipe registration

## What it is

The plugin-facing door onto the crafting corpus (issue
[#148](https://github.com/matteopolak/lodestone/issues/148)) — the `Bukkit.addRecipe`
analogue. A plugin registers a shaped/shapeless/cooking recipe from inside its own
`Plugin::build`, and that recipe becomes matchable by the crafting-table screen and
browsable in the recipe-book panel exactly like one of the 1585 vanilla recipes.

This is *not* a new recipe system. The corpus, the matcher, the occupied-cell index and
the recipe-book UI all predate it ([`crafting.md`](./crafting.md)); what did not exist was
any way to add to them. See that doc for how matching itself works — this one is only
about the registration seam.

## How it works

Three layers, bottom up.

**`lodestone_game::recipe`** owns the mechanism. `RecipeBook::register` is the validated
counterpart to `insert`, and `RecipeBook::unregister` is the removal `insert` never had.
`RecipeRegistration` is a validated `(id, recipe, category)` triple; `RecipeRegisterError`
says why one was refused.

The asymmetry between `insert` and `register` is deliberate:

| | `insert` | `register` |
|---|---|---|
| caller | the JSON corpus loader | a plugin |
| existing id | silently replaced | `Err(Duplicate)` |
| `minecraft:` namespace | fine | `Err(ReservedNamespace)` |

A datapack overriding a vanilla recipe by id is a feature. Two plugins claiming one id is
a bug, and it must surface at the registering plugin rather than as a mysteriously wrong
result slot ten minutes later.

**`lodestone_ecs::recipes`** owns the plugin API. `RecipeRegistry` is a `Resource` holding
the authoritative `RecipeBook`, and it solves the load-order problem: a plugin's `build`
runs long *before* `client.jar`'s corpus loads, so registrations are held **pending** and
replayed onto whichever corpus the process later adopts. Registration is therefore
order-independent — before or after the corpus loads gives the same result.

```rust,ignore
impl Plugin for SparkleStickPlugin {
    fn build(&self, app: &mut App) {
        app.add_recipe(RecipeRegistration::new(
            "sparkle:sparkle_stick".parse().unwrap(),
            Recipe::Shapeless(ShapelessRecipe::new(ingredients, result)),
        )).expect("a fresh id in our own namespace");
    }
}
```

`RecipeRegistryExt::add_recipe` installs the resource on demand, so a plugin does not have
to know whether `RecipeRegistryPlugin` was added before it.

**The shell** adopts and caches. `WindowApp::adopt_recipe_corpus` (`app/lifecycle.rs`)
hands `client.jar`'s corpus to the registry at GPU bring-up and takes the merged book
back; `WindowApp::sync_recipe_book` re-clones it per frame **only when
`RecipeRegistry::revision` has moved**, so a mid-session registration reaches the screen
and the steady state costs one `u64` comparison. `WindowApp::recipe_book` is now a cache,
not the authority.

## How to change it

Validation rules belong in `RecipeRegistration::validate`, not in the ECS layer, which is
transport only. Three gotchas:

- **`TagResolver`'s memo is an `RwLock`, and it has to be.** It was a `RefCell` until this
  issue. `RefCell` is `!Sync`, `Sync` propagates through `RecipeBook`, and a `!Sync` type
  cannot be a `bevy_ecs` `Resource` **at all** — so the memo cache of a read-only lookup
  table was, transitively, the entire reason there was no recipe registration API. If you
  are tempted to put a `Cell`/`RefCell` in anything reachable from `RecipeBook`, this is
  the thing that breaks.
- **`unregister` must go through `RecipeBook`, never a caller-side `Vec::remove`.** A stale
  `grid_index` entry does not panic; `match_grid_entry` degrades it to a *missed match*.
  A hand-rolled removal therefore silently breaks an unrelated recipe.
- **The revision gate is load-bearing for frame cost.** Removing it means cloning a
  1585-recipe corpus every frame for a feature most sessions never use.

## Configuration

None. No feature flag, no env var. A jar-less run has an empty vanilla corpus and plugin
recipes still register and still match, which is what lets a plugin's own tests run with
no `client.jar`.

## What is verified, and the controls

`crates/lodestone-shell/tests/plugin_registers_a_recipe.rs` drives the whole seam: compose
`Sim::client_app()`, add a plugin that registers through `add_recipe` and nothing else,
build a `Sim`, adopt a corpus the way the shell does, then run the **real container
geometry** and assert the ghost-preview result draws in the result slot.

The detector is the ghost preview's dim quad (`container/geometry.rs`), whose colour
`[0.05, 0.05, 0.05, 0.55]` occurs at exactly one place in the whole shell — so it is
atlas-free and needs no jar. The assertion is by **location**: the quad's bounding box is
compared against the result slot's own rect, derived from the same
`slot_layout`/`panel_origin`/`calculate_gui_scale` expressions the draw uses. The first
version of that assertion restated the *unscaled* origin and failed at `(748, 216)` against
an expected `(249.3, 72)` — exactly a factor of three, which is why the scale is now
derived rather than written down.

Three controls, all run and observed:

| control | asserts |
|---|---|
| `control_without_the_plugin_the_ghost_preview_never_fires` | no plugin → zero dim quads |
| `control_a_frame_with_no_recipe_book_draws_no_ghost` | the quad comes from the book, not the grid |
| `recipes::tests::control_without_the_registration_the_same_grid_matches_nothing` | the matcher, same grid, no registration |

The stand-in vanilla corpus deliberately contains only a `Recipe::Special`, which is never
grid-matchable, so nothing in it can satisfy the assertion by accident.

## Dependencies

`lodestone_game::recipe` (corpus, matcher, validation) and `bevy_ecs` for the resource.
Nothing version-specific — recipes are `Identifier`-keyed, never numeric ids, so no
protocol family is involved.

## Known gaps

- **Server-side registration is not wired.** `crates/lodestone-server` has no recipe model
  at all (`inventory.rs` drops a `CONTAINER_CLICK` naming the 2×2 grid, because there is
  nothing to resolve a result against), so a recipe registered here is client-side
  prediction and recipe-book UI only. A real server still authorises the craft. Issue #148's
  own body scopes registration to the server tier; that half needs the server to grow a
  recipe model first.
- **No mid-session recipe-book refresh packet.** Vanilla tells clients about a recipe set
  change with `update_recipes`; we neither send nor decode it. A registration made
  mid-session reaches *our* screen (via the revision gate) but a remote vanilla client
  would not learn about it.
