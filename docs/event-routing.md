# Event routing — making a mis-routed `ClientEvent` a compile error

## What it is

`lodestone_model::event::route` is a single **exhaustive** table saying which of the
client's event routers claim each `ClientEvent` variant. It exists so that adding a
variant and forgetting to wire it is a **compile error** (`E0004`) rather than a
silent nothing.

That silence is this repo's dominant defect class — `CLAUDE.md` §1's *island*, with
nine confirmed instances. Four of them were the same mechanical failure in the same
two functions.

## Why the old shape could not work

`ClientEvent` is `#[non_exhaustive]`. That attribute means **no crate outside
`lodestone-model` can write an exhaustive match over it** — rustc requires a
wildcard arm, and there is nothing a downstream crate can do about it. So every
consumer ended in a terminal `_ =>`, and:

* `lodestone_ecs::ingest::handles_event` was a `matches!` — new variant ⇒ `false`.
* `lodestone_ecs::session::handles_event` was a `matches!` — new variant ⇒ `false`.
* `lodestone_shell::net::forward` ends in `_ => return Ok(())` — new variant ⇒ dropped.

A new variant therefore compiled with **zero** routing arms anywhere and reached
nothing. `SharedState::apply` (`crates/lodestone-client/src/state.rs`) unions the
first two predicates and otherwise falls through to a legacy echo that handles
`TeleportPlayer` and nothing else, so "not listed" and "deliberately unrouted" were
the same observation.

The attribute is *correct* — it keeps external plugin code from breaking on a new
variant — and it is not removed. Inside the defining crate it simply does not bind,
which is the whole trick: the table lives next to the enum, where the match can be
exhaustive.

## How it works

```rust
pub struct Route {
    pub ingest: bool,            // lodestone_ecs::ingest  — per-entity ECS state
    pub session: bool,           // lodestone_ecs::session — local-player scalars
    pub shell: bool,             // lodestone_shell::net::forward — block/world state
    pub shell_conditional: bool, // that shell arm is guarded (see below)
    pub client: bool,            // consumed inside lodestone-client, not by a router
}

pub fn route(event: &ClientEvent) -> Route { /* exhaustive match, no wildcard */ }
```

Three consumers, each earning its flag differently:

| flag | who enforces it | failure mode if it drifts |
|---|---|---|
| `ingest` | `ingest::handles_event` **is** `route(e).ingest` | none — it is a derivation |
| `session` | `session::handles_event` **is** `route(e).session` | none — it is a derivation |
| `shell` | a `debug_assert!` on `net::forward`'s catch-all | fires in every test and oracle run |
| `shell_conditional` | nothing; it keeps that assert correct | a guarded arm would trip the assert |
| `client` | nothing; documentation | `Route::NOWHERE` would over-report islands |

### Booleans, not an enum

The claims are **not exclusive**, and an enum would have forced a false choice that
loses information on day one:

* `Login` → `ingest` (entity id + `EntityIndex` entry) **and** `session` (game mode,
  dimension, alive) **and** `shell` (`NetUpdate::LoggedIn`) **and** `client` (the
  driver's `player_loaded` latch). Four disjoint writes off one event.
* `EntityPassengersChanged` → `ingest` (the `Passengers`/`Vehicle` component pair)
  **and** `session` (the local player's own `Riding` scalar).

Asserted, not asserted-in-prose, by
`event::route_tests::one_event_can_be_claimed_by_two_routers_at_once`.

### Why `forward` is not exhaustive

Making `net::forward` an exhaustive match would put a ~100-arm `match` into
`net.rs`, a permanently contended choke-point file. It keeps its catch-all and gains
one `debug_assert!(!route(&event).must_forward(), …)` instead — so a shell-routed
variant with no arm fails **loudly** in every debug test and oracle run rather than
quietly costing a chest lid.

`must_forward()` is `shell && !shell_conditional`. Two arms in `forward` are
*guarded* and so must be excluded, or the assert would fire constantly on correct
traffic:

* `LevelEvent` — the arm matches the literal sub-event `2001` (block-break effect).
* `EntitySpawned` — the arm is guarded on `lightning_bolt`, to count flashes.

Both are properties of `net.rs` today, not of the events. If either becomes
unconditional, clear the flag and the assert gets stricter for free.

## The convention

It lives as a comment **directly above the match**, because that is where the
decision is actually made:

* **per-entity state** → `ingest`
* **local-player scalars** → `session`
* **block and world state** → `shell` (it travels the shell's own `NetUpdate`
  stream and needs no `handles_event` arm at all — the chest-lid work needed none)

Guessing the `ingest`/`session` fork wrong has cost work twice:
`DimensionTypeChanged` and `AbilitiesChanged` were both briefed as `ingest` and both
belong to `session`. An arm in the wrong router compiles, unit-tests green, and
never runs.

**Two clarifications the twenty-four-variant decode sweep forced, both of which look like exceptions and
are not:**

* **`DebugEntityValue` names an entity and is `session`.** A debug feed is keyed by
  *subscription* and outlives the entity's ECS row, so folding it as a component
  would resurrect rows the client has already dropped. It is session state *about*
  an entity, not entity state. The convention is about what owns the lifetime, not
  about which nouns appear in the packet.
* **A registry-order table is `session`, even though `BiomeRegistryNames` is
  `shell`.** `BiomeRegistryNames` predates the session-fold convention and is read
  through a shell-owned cell; `EnchantmentRegistryNames` folds into
  `SessionRegistryOrder` instead, because a `shell` route obliges an unconditional
  arm in `net::forward` (or its `debug_assert!` fires) and a session component
  reaches the same consumer without one. **Do not copy `BiomeRegistryNames` as the
  pattern for the next registry table.**

## The trade, stated plainly

Adding a `ClientEvent` variant now costs **one mandatory one-line arm** in `route`,
and in exchange it **cannot** island silently. Before, it cost nothing and risked
silence. `Route::NOWHERE` is still a legal answer — but it has to be typed on
purpose, with a reason beside it, and that is the entire difference between a
decision and the defect.

## How to change it

**Adding a variant.** Write the arm. `cargo check -p lodestone-model` will refuse to
compile until you do:

```
error[E0004]: non-exhaustive patterns: `&ClientEvent::Foo { .. }` not covered
    --> crates/lodestone-model/src/event.rs:2087:11
help: ensure that all possible cases are being handled by adding a match arm with a
      wildcard pattern or an explicit pattern as shown
```

**Do not take rustc's advice.** The suggested `_ => todo!()` — and its friendlier
cousin `_ => Route::NOWHERE` — restore exactly the wildcard that
`#[non_exhaustive]` forces everywhere else, delete the guarantee in one line, and
leave a green tree behind. `route_tests::route_has_no_catch_all_arm` reads this
file's own source and fails if one appears; it carries its own positive control on
both spellings.

**Flipping a flag is not wiring.** The flag only decides who gets *asked*. A router
that is asked but has no system for the event drops it exactly as silently as an
unrouted one. Write the system, then the flag, in one commit. The runtime half of
the guarantee is `ingest::tests::handles_event_covers_exactly_the_variants_with_a_system`,
which feeds one of every claimed variant through the real schedule: the table proves
the decision was made, that test proves the system exists.

**Changing an existing route changes runtime behaviour.** Do it in its own
reviewable commit, not as a drive-by while landing something else.

## Islands: variants this table found reaching nothing

**26 of 132** variants are `Route::NOWHERE`. Most are simply decoded ahead of a
consumer, which is a normal state for a from-scratch client.

> **`EntityLeashed` left the list when a leashed mob's rope was wired.** The
> variant carries a wire entity id (the holder), not a scalar, so
> it is per-entity `ingest` state — `ingest::apply_entity_leash` folds it into
> a `Leashed` component, and `player::push_leash_lines` (`ExtractSet::Debug`)
> turns that into a world-space line each frame. The consumer sat one layer
> below the decode the whole time: `v770`'s adapter already emitted this event
> from `SET_ENTITY_LINK`, and nothing in the ECS claimed it until now.
> `crates/lodestone-server/src/protocol.rs`'s own `EntitySnapshot::leash_link`
> and `ServerProtocol::encode_set_entity_link` are the *server*-side half of
> the same issue and are outside this table's scope (it only covers
> `ClientEvent`, the clientbound-decode side).

> **`EntityStatus` left the list when mob death animations were wired.** It is worth
> reading as a template for a *partially* claimed event: the variant carries Mojang's
> raw per-entity-type event byte, roughly forty codes, and exactly **one** of them
> (`EntityEvent.DEATH`, byte 3) now has a consumer — `ingest::apply_entity_status`
> folds it into a `DeathTime` component, and `ingest::tick_death_time` counts it up
> the way `LivingEntity.tickDeath` does. The other codes are particle and sound
> effects with no subsystem here to receive them, and they are dropped **by that
> system** rather than by this table, which is the right split: `route` answers "is
> anything *asked*", and asking for an event you only partly handle is not
> over-reporting. The reverse — leaving the whole variant stranded because most of
> its codes have no home — is what kept the death animation invisible while every
> formula behind it was built and unit-tested.

> **`VehicleMoved` left the list when the client became authoritative over the
> vehicle it rides.** It is worth reading as a template for the "no entity id, so
> it must be a session scalar" mistake: the packet carries only a position and a
> rotation, but what it *writes* is the vehicle's own `Position`/`Rotation`
> components, so it is `ingest` — the subject comes from `session::Riding`, the way
> the seat pin already resolves one. Ask what a fold writes, not what the packet
> names.

> **The numerator did not move when the decode sweep added twenty-four variants**, and
> that is the useful reading of this line rather than a coincidence: the
> twenty-three clientbound packets that had no decode arm at all — plus the
> enchantment registry order — now decode *and* fold, all of them into `session`
> components (`SessionStatistics`, `SessionDebugFeeds`, `SessionServerInfo`,
> `SessionWaypoints`, `SessionRegistryOrder`, `SessionRecipeBook`,
> `SessionTrades`). Twenty-four new islands would have read as "29 of 131" too if
> the numerator had been carried forward instead of recomputed — which is exactly
> the failure the paragraph below describes.

> **On these two numbers, because both have been wrong in the record.** This line
> read "38 of 98" until the world-state sweep below, and the numerator was right
> while the **denominator was stale** — variants had been added since it was
> written, so the fraction understated the total and nothing flagged it. A
> dispatch briefing in the same session quoted "41 of 98", which was wrong in
> both halves.
>
> Count them, do not carry them forward. Both figures are mechanical from
> `route()`'s own source: the denominator is the `ClientEvent` variant count
> (which equals the number of variants `route()` names, since the match is
> exhaustive with no catch-all — that equality is itself worth asserting), and the
> numerator is the variants whose arm's right-hand side is exactly
> `Route::NOWHERE`. Note the distinction from `..Route::NOWHERE`, which is a
> struct-update spread inside an arm that sets other flags and is *not* an island.

**Twelve variants were different — a fold existed or was cheap, and nothing fed
it.** Three were found by the original pass; the world-state sweep
found nine more. All twelve are fixed:

| variant | the fold that used to never run | now routed to | fixed by |
|---|---|---|---|
| `BlockDestruction` | `lodestone_game::mining::BlockDestructionOverlays::apply` — other players' crack overlay. No caller outside its own file and tests. | `session` → `lodestone_ecs::session::SessionBlockDestruction`, folded by `apply_block_destruction` | the routing commit that closed this table's own note |
| `HeldSlotChanged` | `lodestone_game::player_state::HudState::apply`'s `select_slot` arm — server-driven hotbar selection (`/item`, creative). No caller outside its own file and tests. | `session` → `crate::player::SelectedSlot`, folded by `apply_local_player_state` | same |
| `DifficultyChanged` | the same `HudState::apply`. | `session` → `lodestone_ecs::session::ServerDifficulty`, folded by `apply_local_player_state` | same |
| `TabListChanged` | `lodestone_game::tablist::TabList::apply`'s header/footer arm — **and** `session::apply_tab_list` was already registered. Nothing changed but the flag. | `session` → `SessionTabList` | the world-state sweep |
| the six `WorldBorder*` | none — new `lodestone_game::worldborder::WorldBorder` | `session` → `SessionWorldBorder`, folded by `apply_world_border` | same |
| `SpawnPositionChanged` | none — new `lodestone_game::levelstate::SpawnPoint` | `session` → `SessionSpawnPoint`, folded by `apply_spawn_point` | same |
| `GameRulesChanged` | none — new `lodestone_game::levelstate::GameRuleValues` | `session` → `SessionGameRules`, folded by `apply_game_rules` | same |
| `RecipeBookSettingsChanged` | **the packet had no decode at all** — a new variant, not an un-stranded one | `session` → `SessionRecipeBookSettings`, folded by `apply_recipe_book_settings` | same |
| `MapItemData` | **no decode at all** — id 51 registered, nothing else | `session` → `SessionMaps`, folded by `apply_maps`. Keyed on **map id**, not on an entity: one map can be held by several players and hung in several frames at once | the map/advancement wire landing |
| `AdvancementsUpdated` | **no decode at all** — id 130 registered, nothing else | `session` → `SessionAdvancements`, folded by `apply_advancements` | same |

### One step earlier in the pipeline: id registered, never decoded

`RecipeBookSettingsChanged` is a different defect from the other eleven and worth
separating, because this table cannot see the difference. The others were *decoded
and unrouted*. This packet had **no decode arm in
`crates/protocol/v770/src/adapter/` at all** — only a registered packet id, which
proves nothing except that the id is known. `cargo xtask connectedness` is the
instrument for that axis, not this one.

It was worth doing because the *outbound* half already existed:
`ClientAction::SetRecipeBookSettings` has been encoded by the adapters for some
time, so the client could tell the server its recipe-book state and could never be
told it back. The round trip was half-open.

**Four sibling packets remain undecoded and are deliberately not attempted here**,
because the cost is not in the packets:

| packet | blocked on |
|---|---|
| `RECIPE_BOOK_ADD` (74) | the full `RecipeDisplay`+`SlotDisplay` tree, plus `holderSet<Item>`, the `recipe_book_category` registry, and `OPTIONAL_VAR_INT` |
| `PLACE_GHOST_RECIPE` (63) | the same tree (small arm on top of it) |
| `UPDATE_RECIPES` (133) | the same tree, less obviously — via `SelectableRecipe.noRecipeCodec()`'s `SlotDisplay` in the stonecutter set |
| `RECIPE_BOOK_REMOVE` (75) | nothing technically — but see below |

The shared prerequisite is a recursive `SlotDisplay` decoder (**11 registry-dispatched
variants**, including `item_stack` carrying a `DataComponentPatch` with a field order
*different* from `ItemStack.OPTIONAL_STREAM_CODEC`, and `smithing_trim` carrying a
`Holder<TrimPattern>` whose `0` discriminator means an inline definition follows
containing a full chat `Component`) plus a `RecipeDisplay` dispatcher (5 variants).
**None of it exists in `crates/protocol/` or `crates/lodestone-model/` today — not one
line.** Measured: 4 grep hits for `SlotDisplay`/`RecipeDisplay`, all prose in doc
comments. Realistically 400–600 lines of codec and model types. Recursion is unbounded
on the wire (`composite` of `with_remainder` of `dyed` of …) and vanilla does not bound
it, so a depth cap is required.

`RECIPE_BOOK_REMOVE` is cheap to decode — a VarInt count and N VarInts, codec
`StreamCodec<ByteBuf, _>`, no registry — and **useless on its own**, because the
`RecipeDisplayId` → recipe mapping arrives only in `RECIPE_BOOK_ADD`. Decoding it
alone would produce integers nothing can resolve.

**And there is a design blocker ahead of the codec that is easy to miss.**
`RecipeUnlockState::unlock`/`remove` (`lodestone-game/src/recipe.rs`) key on
`Identifier`. The wire carries `RecipeDisplayId`, a *server-session-assigned integer
index*, and a `RecipeDisplay` contains no recipe id at all — only slot displays, from
which at best an item stack can be resolved. So decoding `RECIPE_BOOK_ADD` does **not**
by itself let anything call `unlock`: either the event carries the index plus a
resolved result and something owns the index→`Identifier` map, or `RecipeUnlockState`
gains an index-keyed path. That decision belongs with whoever owns the recipe book, and
it is why "the consumer is already built" is only half true — the *toast renderer* is
built and `hud.rs`/`app.rs` are wired, but the unlock key type does not match the wire.

### The world-state sweep's nine, and what the router fork cost

All nine are `session`, and the rule of thumb held without exception: **none is
per-entity**, so none is `ingest`. They are scalars scoped to the world this
session is connected to — the same category as `DimensionTypeChanged` and
`AbilitiesChanged`, both of which cost work by being guessed as `ingest` first.

`TabListChanged` is the cheapest instance of this whole defect class that the repo
has produced: the fold arm existed, the system was registered, the event decoded
correctly and was unit-tested, and `route()` simply never asked. A one-line flag
change made it work. Nothing else in the tree needed touching.

**The world border needs a clock, and picking the wrong one is a live trap.**
`apply_world_border` reads `FrameClock`, not `WorldTime`. `WorldTime` is the
*server's* clock and the server can freeze it with the `advance_time` game rule —
a frozen clock must not freeze a border animation. `FrameClock::secs` is monotonic
wall time, the same clock the chat fade reads. The fold itself takes no clock (that
would fork the `apply(&ClientEvent) -> bool` convention every aggregate uses); it
records the resize unstamped and the *system* stamps it, idempotently, so a later
border packet cannot restart an interpolation already in flight.

**On `GameRulesChanged`, two things that are easy to get backwards.** It is *not*
the planned typed registry — that is a server-side 59-rule table and is unbuilt;
this is a client-side raw-string table with typed accessors. And vanilla's
`GAME_RULE_VALUES` is **request/response, not broadcast** (its only send site is
`sendGameRuleValues()`, reachable solely via
`ServerboundClientCommandPacket.REQUEST_GAMERULE_VALUES`), so nothing pushes rule
changes to clients, an unreported rule is the *normal* case, and every accessor
returns `Option`. A caller that treats `None` as `false` erases exactly that
distinction — which is why the fold keeps them apart and a test asserts it at both
ends.

`HudState` was superseded by the `lodestone-ecs` session components (see
`session.rs`'s note on `Vitals`: `HudState` has no "unreported" bit, so Stage 3 did
not adopt it), which is *why* it had no caller for these two — but the events they
fold were never re-homed onto a component, so they reached nothing at all. The fix
was new session components (`SelectedSlot` already existed for the local-input
half and gained a second, server-authoritative writer; `ServerDifficulty` is new),
**not** reviving `HudState::apply` — that function stays dead code.

`BlockDestruction` is a different shape from the other two: it is about *other
players'* blocks, which reads at first like "block/world state" and a candidate
for `shell` (the way the chest-lid `BlockEvent` is, needing no `handles_event` arm
at all). It is routed `session` instead, because
`BlockDestructionOverlays` is a per-*session* collection keyed by the breaking
entity's id — the same shape as `SessionBossBars`/`SessionTabList` just above it in
`route()`, not a world-geometry fact the mesher owns.

**Routing is not the same as drawing.** `HeldSlotChanged` reaches pixels for free —
`lodestone_shell::sim::Sim::selected_slot()` already reads `SelectedSlot` and
`app.rs`'s hotbar highlight already calls it, so wiring the fold was the whole fix.
`BlockDestruction` and `DifficultyChanged` did not, at the time this table was
written: nothing in the shell read `SessionBlockDestruction` or `ServerDifficulty`,
and for `BlockDestruction` specifically, the renderer's `CrackTarget`/`CrackPipeline`
(`lodestone_shell::gpu`) only ever drew *one* target — the local player's own dig.
Both were tracked as separate follow-up issues, closed as follows:

* **`DifficultyChanged`** — `Sim::difficulty()` now reads `ServerDifficulty`
  the same way `selected_slot()` reads `SelectedSlot`, and `app.rs` folds it into
  `DebugStats::difficulty` each frame. It shows as a `DIFFICULTY <NAME>[ (LOCKED)]`
  line on the F3 overlay (`hud.rs`'s `DebugStats::lines`); see
  [`vanilla-hud-text.md`](./vanilla-hud-text.md).
* **`BlockDestruction`** — `CrackPipeline`/`RenderState::render_with_crack`
  now take `cracks: &[CrackTarget]` instead of `Option<CrackTarget>`, so the
  render pass itself can paint any number of simultaneous crack overlays (proved
  by `lodestone-shell/tests/crack_multi_target_pixels.rs`, two targets at two
  screen positions in one call). **Now fully wired.**
  `lodestone_game::mining::BlockDestructionOverlays` gained `iter()`, enumerating
  every active `(BlockPos, u8)` entry with no position known in advance —
  `stage_at`'s single-position probe (`Sim::block_destruction_stage_at`) could not
  serve the per-frame loop, which does not know a position to ask about. The
  gather itself is `lodestone_shell::gpu::gather_crack_targets` (pure, version/
  `Sim`-agnostic: local target + `overlays.iter()` + a `resolve(BlockPos) ->
  Option<u32>` callback), unit-tested directly against a real
  `BlockDestructionOverlays` fold in `gpu/outline.rs`'s `gather_tests`, and proved
  reaching pixels for two different breaking entities in
  `lodestone-shell/tests/crack_live_gather_pixels.rs` (through the live gather,
  not a hand-built `Vec<CrackTarget>` — the distinction
  `crack_multi_target_pixels.rs` cannot draw). `Sim`'s per-frame call
  (`Sim::crack_targets`, wiring `crack_target()` + the local/live `resolve`
  split into `gather_crack_targets`) and `app.rs`'s one-line call site are the
  brokered choke-point patch that lands alongside this doc update.

The remaining **27**, listed for the record and not as a defect claim:

`Ping`, `BlockChangedAck`, `ChunkCacheCenterChanged`, `ChunkCacheRadiusChanged`,
`SimulationDistanceChanged`, `EntityLeashed`,
`ItemCooldown`, `PlayerRotationSet`, `CameraSet`, `BookOpened`, `SoundStopped`,
`PlayerCombatEntered`, `PlayerCombatEnded`, `SignEditorOpened`,
`AdvancementsTabSelected`, `ProjectilePowerChanged`, `MountScreenOpened`,
`TransferRequested`, `CookieRequested`, `CookieStored`, `ResourcePackPushed`,
`ResourcePackPopped`, `CustomPayload`, `ServerDataReceived`, `PongReceived`,
`ChatMessageDeleted`, `PlayerLookAt`.

One of those is worth a second look by whoever owns the relevant subsystem, and is
flagged rather than changed here:

* **`Ping`** is a clientbound ping the vanilla client answers with `pong`. Nothing
  consumes it and no `ClientAction` producer exists, which is the *outbound* island
  shape `ClientAction::SetFlying` had. `PongReceived` is likewise unconsumed.

`SimulationDistanceChanged` is deliberately left stranded and is now doing a second
job: it is the **negative control** for the world-state folds
(`lodestone_client::state`'s
`a_still_stranded_world_scalar_reaches_none_of_the_new_components`). It is the same
shape and subsystem family as the nine that were wired, so if any of those folds
started matching too broadly it would be the first to be caught. That gate asserts
`route(&SimulationDistanceChanged).is_island()` as an explicit **premise check**
before relying on it — so whoever eventually wires this variant gets a clear failure
naming the control rather than a silently vacuous one.

### What "wired" does and does not mean here

**Routing is not drawing, and four of the nine reach no pixels yet.** What they now
have is a folded, tested, resettable component that a renderer can read — which is
the half that was missing. Specifically still outstanding, all brokered because they
live in choke-point files:

* **Tab-list header/footer** need a `hud.rs`/`app.rs` change to draw above and below
  `hud_frame.players`. Note a stale comment in `tablist.rs` claims header/footer are
  "read downstream by `hud.rs`'s snapshot" — grepped, and **no reader exists**. The
  comment was true of an earlier design.
* **The world border** needs its warning overlay and the border wall itself
  (`docs/plans/world-state.md` §B2).
* **The compass** — `lodestone_render::item_render` lists `minecraft:compass` among
  range properties "deliberately unsourced because the datum genuinely is not
  decoded". It *was* decoded; it reached nothing. `SessionSpawnPoint` is now the
  source, and `item_render.rs`'s `unsourced_properties_read_as_unset` test currently pins the property at `0.0`
  with "must be unset" — that pin is what to change.
* **Game rules** have no client consumer yet; `immediate_respawn` (skip the death
  screen) is the most visible candidate.

## What this table does **not** do

* It does not measure whether a claimed router has a system. That is
  `handles_event`'s coverage test.
* It does not know about the version adapters. Chunk payloads reach the world
  through `lodestone_world::WorldSink` directly, so `ChunkLoaded`/`ChunkUnloaded`
  are marked `client` as signals; the heavy data never travels the event stream.
* It does not cover the **serverbound** direction. `ClientAction` has the mirror
  problem (`SetFlying` was encoded by four adapters with zero producers) and no
  equivalent table.
* It is not `cargo xtask connectedness`, which measures clientbound decode → event
  wiring and nothing else.

## Configuration

None. No features, no env vars.

## Dependencies

* `crates/lodestone-model/src/event.rs` — `Route`, `route`, and the table.
* `crates/lodestone-ecs/src/ingest.rs`, `session.rs` — `handles_event`, one line each.
* `crates/lodestone-shell/src/net.rs` — the `debug_assert!` in `forward`'s catch-all.
* `crates/lodestone-client/src/state.rs` — `SharedState::apply`, which unions the two
  predicates. Unchanged by this work; it now consults the table transitively.

The layering is deliberately inverted: the leaf model crate names its consumers.
That is the accepted cost of the one property nothing else can buy — a compile
error.
