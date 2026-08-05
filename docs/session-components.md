# Session and HUD state as ECS components

## What it is

The scoreboard, tab list, boss bars, menus, session phase, vitals, experience,
the title/action-bar overlays, the HUD effect stack, the respawn counter and the
server-assigned player id — held as `bevy_ecs` components and folded by systems.
This is Stage 3 of [`bevy-migration.md`](./bevy-migration.md), whose stated
payoff was deleting the **double fold** that doc's §1.1 measured.

## The double fold, as it actually was

§1.1 described two different types named `Scoreboard` folding one `ClientEvent`
stream. Traced out, the scoreboard/tab-list/boss-bar family had **three**
implementations, and one of them was already dead:

| implementation | folded where | read by | reached pixels? |
|---|---|---|---|
| `lodestone_client::scoreboard::Scoreboard` | net thread, `Inner::apply` | `ClientHandle::scoreboard()`; `NetClient::sidebar()` → `overlay::sidebar_from` | **no** — `NetClient::sidebar()` had zero callers |
| `lodestone_game::scoreboard::Scoreboard` | driver thread, `Sim::poll_net` on `NetUpdate::ScoreboardEvent` | `Sim::sidebar()` → `crate::scoreboard::sidebar_from` → `app.rs::WindowApp::redraw` | yes |
| `lodestone_game::bossbar::BossBarSet` | nowhere | nothing | no |

They also **disagreed**, which is what makes this a defect rather than an
inefficiency:

- the client's type modelled 3 display slots; the game's models all 19 (the 16
  per-colour sidebars included), so a server using `sidebar.team.red` was
  invisible to the bot API and visible to the HUD;
- a `ScoreUpdate` naming an unknown objective created a bucket for it in the
  client and was dropped in the game crate (`Scoreboard::apply`'s doc comment
  called this out explicitly as "a deliberate divergence … noted so a future
  reconciliation of the two does not 'fix' it blindly");
- the client's `Objective.display_name` was `Option<Text>`; the game's defaults
  to `Text::literal(name)`;
- the client's type had no team decoration at all (`decorate` / `display_name_of`
  / `sidebar_for_color`).

And the player list was worse than duplicated. `Inner::apply`'s only arm was
`PlayerListUpdate`; there was **no `PlayerListRemove` arm**, so a player who left
the server never left `ClientHandle::players()`.
`lodestone_game::tablist::TabList::apply` handles both, and it is now the only
fold. `crates/lodestone-ecs/src/session.rs` carries a regression test
(`a_player_who_leaves_is_removed_from_the_tab_list`).

The resolution: **`lodestone-game`'s aggregates are the components**, folded once
by `NetIngest` systems inside `lodestone-client`. The shell folds nothing.

## How it works

It lives in two `World`s, and the split follows one rule rather than taste:

> The fold lives where the readers are shared. A fold with a single driver-side
> reader stays on the driver.

| half | which `World` | components |
|---|---|---|
| shared-fold | the net thread's, owned by `lodestone_client::state::SharedState` | `SessionScoreboard`, `SessionTabList`, `SessionBossBars`, `SessionMenus` |
| driver | the shell's, owned by `lodestone_shell::sim::Sim` | `Phase`, `Vitals`, `Xp`, `TitleOverlay`, `ActionBarOverlay`, `HudEffects`, `RespawnCount`, `ServerEntityId` |

The shared four had to go to the net thread because `ClientHandle` is a public
API that must work with no shell at all (`examples/`, the bot tier,
`tests/read_model.rs`), and `ClientHandle` cannot reach the driver's `World`. The
driver eight had no duplicate to collapse, so moving them across a thread
boundary would have bought nothing and cost a lock inside the tick.

### The shared half: one fold, one system each

```
net thread → SharedState::apply(event)
  ├─ TimeChanged                     → WorldTime resource            (Stage 0)
  ├─ ingest::handles_event(e)   ─┐
  ├─ session::handles_event(e)  ─┴──→ IngestQueue.push(e); run_schedule(NetIngest)
  └─ everything else                 → Inner::apply (the local-player echo only)
```

Inside `NetIngest`: `IngestSet::Drain` → `IngestSet::Apply`, and within `Apply`
the four session systems are `SessionSet::Fold`, chained:

| system | events | component |
|---|---|---|
| `apply_scoreboard` | `ObjectiveUpdate`, `DisplayObjective`, `ScoreUpdate`, `ScoreReset`, `TeamUpdate` | `SessionScoreboard` |
| `apply_tab_list` | `PlayerListUpdate`, `PlayerListRemove` | `SessionTabList` |
| `apply_boss_bars` | `BossBarUpdate` | `SessionBossBars` |
| `apply_menus` | `ScreenOpened`, `ScreenClosed`, `ContainerContent`, `ContainerSlot`, `ContainerData`, `CursorItemChanged`, `InventorySlotChanged` | `SessionMenus` |

Each system calls the aggregate's own `apply` — the ECS owns state and
scheduling, never the fold logic ([`bevy-migration.md`](./bevy-migration.md) §8).
`SharedState` holds the session `Entity` so a read is `World::get` under a *read*
lock; a `Query` would need `&mut World` and contend with ingest for nothing.

### The driver half

`Sim` reads and writes the eight driver components through accessors, exactly as
Stage 2 does for physics state. The one system is `tick_hud_overlays` in
`GameTick` / `TickSet::Animate`, which replaces three hand-written `tick(1)`
calls in `Sim::step`.

### Read-through, and what the shell no longer holds

```
Sim::sidebar()      → NetClient::scoreboard() → ClientHandle::scoreboard() → SessionScoreboard
Sim::player_rows()  → NetClient::tab_list()   → ClientHandle::tab_list()   → SessionTabList
Sim::boss_bars()    → NetClient::boss_bars()  → ClientHandle::boss_bars()  → SessionBossBars
```

`NetUpdate::TabListEvent` and `NetUpdate::ScoreboardEvent` are deleted along with
their `forward()` arms, so those events no longer even cross the channel.

## What was deleted, field by field

**`crates/lodestone-client/src/scoreboard.rs` — the whole file (388 lines).**
`Objective`, `ScoreEntry`, `Team`, `BossBar`, `Scoreboard` (with
`apply_objective`, `apply_display`, `apply_score`, `apply_score_reset`,
`apply_team`, `add_member`, `remove_member`, `objective`, `objectives`,
`displayed`, `score`, `scores`, `scores_in_slot`, `team`, `team_of`) and
`apply_boss_bar`. `lib.rs`'s `mod scoreboard;` and its `pub use` go with it;
`lodestone-game`'s equivalents are re-exported in their place.

**`Inner` (`state.rs`)** loses `players`, `scoreboard`, `boss_bars`, `menus`, its
hand-written `Default`, and seven `apply` arms (`PlayerListUpdate`,
`ObjectiveUpdate`, `DisplayObjective`, `ScoreUpdate`, `ScoreReset`, `TeamUpdate`,
`BossBarUpdate`) plus the leading `if self.menus.apply(event) { return; }`.

**`Sim` (`sim.rs`)** loses eleven fields and one type:

| deleted | now |
|---|---|
| `phase: SessionPhase` | `Phase` component |
| `tab_list` | client's `SessionTabList` |
| `scoreboard` | client's `SessionScoreboard` |
| `hud_effects` | `HudEffects` component |
| `title` | `TitleOverlay` component |
| `action_bar` | `ActionBarOverlay` component |
| `health` | `Vitals.health` |
| `food` | `Vitals.food` |
| `experience` | `Xp` component |
| `respawn_count` | `RespawnCount` component |
| `local_entity_id` | `ServerEntityId` component |
| `enum SessionPhase` (definition) | `lodestone_ecs::session::SessionPhase`, `pub use`d from `sim.rs` so `app.rs` and the live gates are untouched |

Also deleted: `NetUpdate::{TabListEvent, ScoreboardEvent}`, `NetClient::sidebar()`
(zero callers), and `overlay::{sidebar_from, sidebar_view, MAX_SIDEBAR_LINES}` —
a *second* sidebar projection reachable only through that dead method.

## What deliberately did not move, and why

- **`Sim.chat_log`.** Every push needs `Sim.clock_secs` (the shell's own frame
  clock, which stamps arrivals so the HUD can age lines for the vanilla fade) and
  every read needs it again to compute an age. A component would either carry a
  clock — a second copy of `Sim`'s — or need one passed in on every access. It
  moves with `clock_secs`, in Stage 5.

- **`PlayerSnapshot`'s vitals — this one has since closed.** Stage 3 left
  `health`, `food`, `saturation`, `xp_*`, `entity_id` and `alive` on the net
  thread beside `Vitals` / `Xp` / `ServerEntityId` on the driver, bounded by
  "the `World` unification". §4.1(c) shipped and they were still duplicated. See
  [the vitals collapse](#the-vitals-collapse) below for what the second blocker
  actually was and how it was resolved.

  What Stage 3 *did* establish and what survived: `position` / `rotation` /
  `on_ground` are the only thing in the client's scalar state that is not a fold
  at all — a **local echo** of our own outbound movement (`set_local_movement`),
  which is why a bot's `look`/`walk` can build on the latest local pose without a
  round trip. That is the genuine prediction-vs-server-view distinction, it is not
  duplicated anywhere, and it is now the *whole* of what is left there.

- **`lodestone_game::player_state::HudState`.** A fourth implementation of the
  vitals fold: complete, unit-tested (`tests/hud.rs`, `tests/hud_snapshot.rs`),
  and with **no production caller**. It is tempting to adopt as the `Vitals`
  component and it is the wrong shape today: `HudState.health` is `f32` defaulting
  to `20.0`, with no "has the server reported this yet" bit. Both live folds carry
  one (`PlayerSnapshot.health_known`, `Sim.health: Option<f32>`) and the HUD
  depends on it — the offline fixture world must draw *no* health bar, not a full
  one. Adopting `HudState` therefore means adding a reported-yet flag to a
  canonical aggregate with its own tests, which is a change to
  `lodestone-game`'s model rather than a migration step. Recorded so the next
  stage does not rediscover it.

## The vitals collapse

The residue Stage 3 named above closed after §4.1(c), and it needed a decision
rather than only a rebase. The full record — the routing table, the two options
and why the second won, and the `ClientHandle::player` lock consequence — is in
[`world-unification.md`](./world-unification.md#the-vitals-collapse-and-the-second-blocker-c-hid).
The short version:

- Stage 3's stated blocker (a component in one `World` is invisible to a system in
  the other) was real and is gone. **The one that remained was
  `SharedState::apply`'s exclusive routing**: `Login`, `HealthChanged`,
  `Respawned` and `Death` each carry vitals *and*
  `dimension`/`game_mode`/`alive`, so claiming one for a `NetIngest` system froze
  `dimension` — the too-bright-Nether bug, reached by traversal.
- **The routing stayed exclusive.** `ServerGameMode`, `ServerDimension` and
  `ServerAlive` joined the component set so that no event carries a field the
  scalar side still owns. The alternative — run both folds — would have kept two
  copies of `dimension` alive, which is precisely the double fold this stage
  exists to delete.
- `Vitals`, `Xp` and `ServerEntityId` moved from the driver half of
  `session.rs` to the **shared** half, because the rule is *the fold lives where
  the readers are shared* and `ClientHandle::health`/`experience_*`/`player` must
  work with no shell attached. `insert_session_components` inserts them; the
  driver half is now `Phase`, the two overlays, `HudEffects`, `RespawnCount` and
  `SessionChat`.
- `PlayerSnapshot` is **derived** from those components, with its public shape
  unchanged — the same intermediate Stage 1 established for `EntityView`. `Vitals`
  gained a `saturation` field so nothing was silently dropped from the bot API.
- `NetUpdate::Health` and `NetUpdate::Experience` are **deleted**, along with their
  `forward` and `Sim::poll_net` arms, exactly as Stage 3 deleted `TabListEvent`
  and `ScoreboardEvent`: with the net thread folding those events into the same
  components the HUD reads, a shell arm would be a second writer.
- `alive` and `Dead` did **not** merge. Two rules, two readers, and one of them
  has a live-gate negative control switch on it.

## How to change it, and the gotchas

- **`drain_ingest_queue` must be registered exactly once per `World`.** This is
  the bug this stage actually shipped and then caught. `SessionPlugin` originally
  registered its own copy "idempotently with `IngestPlugin`" — but `add_systems`
  does **not** deduplicate. Two copies run in sequence: the first fills
  `IngestBatch` from `IngestQueue`, the second clears the batch it just filled and
  appends a now-empty queue. Every `Apply` system then sees zero events: a silent,
  total ingest blackout. It is now `IngestQueuePlugin`, added by both plugins via
  `is_plugin_added`.

  **The instructive part is how it hid.** `SessionPlugin`'s own unit tests were
  green, because they install `SessionPlugin` *alone* and therefore only ever had
  one drain. The configuration production uses — `new_ingest_handle()`, both
  plugins — folded nothing, and the only thing that showed it was a
  `lodestone-shell` test reading through the real shape. Two tests now pin it
  directly (`the_real_both_plugin_world_still_folds_a_scoreboard`,
  `..._an_entity_spawn`), built on the real handle rather than a bare `App`. This
  is the closed-loop failure `CLAUDE.md` describes, in a form that is not about
  pixels at all.

- **Exactly one system may write each session component**, and there is a build
  check for it: `exactly_one_system_writes_each_session_component` initialises
  `NetIngest` with `ScheduleBuildSettings { ambiguity_detection: LogLevel::Error }`.
  Its control, `a_second_unordered_scoreboard_writer_fails_the_ambiguity_check`,
  adds a rogue second writer and requires the build to fail — without it the
  assertion would pass equally against a detector that was switched off. Note
  that `initialize` does **not** rebuild an already-built schedule, so the check
  must run before anything runs the schedule; the test carries that note.

- **`session::handles_event` and `Menus::apply` must stay in step.** The switch
  cannot delegate to `Menus::apply` (it has no `&mut Menus` to hand it), so the
  container family is listed by hand. An arm added to `Menus::apply` and forgotten
  in `handles_event` never reaches the ECS — the event falls through to
  `Inner::apply` and is silently dropped.

- **Reading `boss_bars()` must go through `BossBarSet::iter`**, not the `HashMap`
  behind it: `iter()` walks the `order` vec, which is what makes the on-screen bar
  stack stable frame to frame.

- **`Sim::end_session` *does* clear the tab list, scoreboard, boss bars and
  menus — and this bullet used to say the opposite.** It read: "they are
  components in the *client's* `World`; dropping `net` drops the only route to
  them and every reader falls back to an empty default." That was true and
  evidenced when written, and **§4.1(c) falsified it** — there is one `World`, the
  readers are `Sim::sidebar`/`player_rows`/`boss_bars` off `Sim.local`, and
  dropping `net` drops no route to anything. The previous server's sidebar
  genuinely survived a quit-to-title until `end_session` started calling
  `insert_session_components`. A note asserting that state *cannot* leak is the
  most expensive kind to leave stale, because nothing about it looks wrong on
  inspection.

- **Add a driver component? Add it to `insert_hud_components`. Add a *shared*
  one? Add it to `insert_session_components`.** Both are now the spawn path *and*
  the `end_session` reset path, for the same reason `reset_local_player` is one
  function: a component added to the spawn and forgotten in the reset leaks the
  previous session's value into the next one. Pick the half by asking who reads
  it — if `ClientHandle` must answer for it with no shell attached, it is shared.

- **The live gate changed meaning, on purpose.**
  `crates/lodestone-shell/tests/live_tab_scoreboard_pixels.rs` used to fold
  `NetUpdate::{TabListEvent, ScoreboardEvent}` into its own `TabList` /
  `Scoreboard` — it *reimplemented* the shell's fold rather than reading it, so it
  could have passed with the shell's own path broken. It now reads
  `NetClient::{tab_list, scoreboard}`. Likewise
  `crates/lodestone-game/tests/live_scoreboard.rs` existed to check that the two
  folds agreed; with one fold left, what it now pins is that the *schedule-driven*
  fold agrees with calling `apply` directly on the same events — i.e. that
  `SessionPlugin`'s registration reaches the components. That is a sharper
  assertion than the one it replaced.

## Configuration

None. No feature flags, no env vars.

## Dependencies

- `lodestone-ecs` → **`lodestone-game`** is new (default features): the session
  component set *is* `scoreboard::Scoreboard`, `tablist::TabList`,
  `bossbar::BossBarSet`, `menus::Menus`, `effect::ActiveEffects` and
  `player_state::{TitleState, ActionBar}`. wasm-safe — `lodestone-client` already
  depended on it with default features and is in `scripts/wasm-check.sh`'s crate
  list. Still never a version crate.
- `lodestone-client` → unchanged set; `state.rs`/`handle.rs` now name
  `lodestone_ecs::session` and hand out `lodestone-game`'s aggregates.
- `lodestone-shell` → unchanged set; `sim.rs` re-exports
  `lodestone_ecs::SessionPhase`, and `overlay.rs` speaks `BossBarSet`.
