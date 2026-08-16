# The wither boss fight

## What it is

The server-side state for the wither boss fight: the summon-structure block
pattern (soul sand/soil plus three wither skulls), the 220-tick invulnerable
"emerging" phase, the powered-armor arrow/wind-charge immunity below half
health, and the skull-projectile combat rules (flat damage, unconditional
impact blast, the `minecraft:wither` status effect on a landed hit, and an
owner heal on a kill). Lives at `crates/lodestone-server/src/wither.rs` (the
pure port) and `crates/lodestone-server/src/mobs/{wither.rs,wither_pattern.rs}`
(the `MobSim` integration and the structure matcher), ported from 26.2's
decompiled `WitherBoss`/`WitherSkull`/`WitherSkullBlock` under
`.cache/mc/26.2/src/net/minecraft/world/{entity/boss/wither,entity/projectile/hurtingprojectile,level/block}/`.

Every function in `crate::wither` is **pure** — no world, no entity, no
packet. Given inputs, it returns a new value or a
[`wither::WitherEffect`](../crates/lodestone-server/src/wither.rs) for a
caller to perform. This mirrors `docs/dragon-fight.md`'s own shape
deliberately — see that doc for the precedent this change cribbed the split
from.

## How it works

Two halves:

- **`crate::wither`** — the invulnerable-countdown state machine (`220` ticks,
  a `10.0` HP heal every `10` ticks while invulnerable vs. `1.0` HP every `20`
  ticks once active), the emergence blast (`7.0`-power explosion the tick
  invulnerability ends), the powered-armor gate (`health <= max/2` blocks an
  arrow-or-wind-charge hit outright), and the skull's own numbers (`8.0`
  damage with a living owner, a `1.0`-power impact blast on any surface, a
  `5.0` HP owner heal on a kill, and the Normal/Hard wither-effect durations).
- **`crate::mobs::wither`** — `MobSim`'s integration: `spawn_wither_at`
  (test/summon-command entry point), `try_construct_wither` (the structure-
  detection query, mirroring `try_construct_golem`'s shape exactly),
  `tick_withers` (drives the emergence countdown, heal ticks and a single
  skull-firing schedule — see the module's own doc for why this is one
  schedule rather than vanilla's three independent heads), `damage_wither`
  (the invulnerability/powered-armor gates, removing the wither once health
  reaches zero), and the snapshot/boss-bar producers.

`crate::mobs::wither_pattern` is a second, independent block-pattern matcher
— not a generalisation of `crate::mobs::golem`'s `GolemCell`, because that
enum is closed over the two golem patterns and a wither cell alphabet
(soul sand/soil, wither skull, wither wall skull) does not fit it cleanly.
The *engine* (brute-force search over a `dist × dist × dist` cube and all 24
`(forwards, up)` orientations) is copied rather than shared, the same way
vanilla's own `BlockPatternBuilder` construction is duplicated per block
class rather than factored out.

## What consumes this today

- **The skull's full impact chain reaches the wire.** `mobs::wither::
  tick_withers` fires a skull through `MobSim::spawn_projectile_from` (the
  same funnel every other projectile in this crate uses), and
  `MobSim::resolve_projectile_impacts` (in `mobs/projectiles.rs`) now carries
  a `"wither_skull"` case all the way through: flat (non-speed-scaled)
  damage, the `minecraft:wither` effect on a landed non-lethal hit, an owner
  heal on a kill, and an unconditional impact explosion on any surface.
- **The boss bar reaches the wire with no new call site.** `MobSim::boss_bars`
  (in `mobs/dragon.rs`) now appends `mobs::wither::push_wither_boss_bars`'s
  output to the same `Vec` the dragon's own bars populate, and
  `crate::tick::run_tick_loop` already calls `MobSim::boss_bars` once per
  tick and publishes the result through `LiveMobSource` — the exact path
  `crate::server::sync_boss_bars` diffs against a connection's last-sent set.
  So a wither's bar appears, updates and disappears (its uuid simply stops
  appearing in the list once the wither is removed — the same `REMOVE`
  fallback an entity id vanishing from entity sync uses) with **zero**
  edits to an off-limits file.
- **`tick_withers` is called from `crate::tick::run_tick_loop`** — a
  one-line addition beside the pre-existing `tick_dragons` call, in the
  shared-but-editable `tick.rs`. Without it a spawned wither would be inert
  the same way an un-ticked dragon was before that line landed.

## What does not consume this yet

- **Nothing calls `MobSim::try_construct_wither` in production.** The
  hook belongs beside `MobSim::try_construct_golem`'s own real call site —
  `crate::server`'s block-placement handler, on placing
  `minecraft:wither_skeleton_skull`/`_wall_skull` — which is an off-limits
  file for this change. See this crate's own report (or the sibling
  `try_construct_golem` call site, cited above, as the exact shape to
  mirror) for the hunk a block-placement owner still needs to add.
- **No `MetadataField` for `WitherBoss.DATA_ID_INV`/`DATA_TARGET_A/B/C`**
  (indices 19/16-18 of the committed
  `crates/protocol/v770/tests/support/entity_data_index_jvm.txt` dump, all
  `INT`, all shared with other species at the same crowded indices — see
  `crate::wither`'s own module doc for the full collision census). A wither
  is selector-visible, damageable and boss-bar-tracked without it; only the
  client-side "still emerging" shield/particle visual and the two side
  heads' own aim state are missing.
- **No darken-screen bit on `BossBarSnapshot`.** `WitherBoss`'s own
  `ServerBossEvent` sets `setDarkenScreen(true)`; this crate's
  `BossBarSnapshot` has no carrier for it.

## How to change it

- Each function cites the vanilla symbol it ports by class and method name,
  never a line number — re-verify against the current decompile rather than
  trusting a comment's paraphrase, per this repo's own citation rule.
- The wither is tracked as a plain `HashMap<i32, TrackedWither>` entry, not a
  goal-driven `SimMob` — the same shape `mobs::dragon` uses for the ender
  dragon, and for the identical reason: this codebase's flying-mob AI has no
  aerial pathfinder. **Unlike the dragon, the wither does not move at all**
  (no simplified orbit) — a smaller scope than the dragon's own, disclosed in
  `mobs::wither`'s module doc.
- If you add a new emergence/heal/damage rule, add it to `crate::wither`'s
  own test module as a **magnitude** assertion (predict the exact value, not
  just the sign of a change) — see `invulnerable_countdown_reaches_zero_after_exactly_220_ticks`
  for the shape.
- The wither-effect duration currently always assumes Normal difficulty
  (`crate::wither::wither_effect_ticks`, called with a hardcoded
  `Difficulty::Normal` from `mobs::projectiles::resolve_projectile_hit`) —
  threading a real difficulty through needs widening
  `MobSim::resolve_projectile_impacts`'s signature, which ripples into
  `MobSim::tick`'s own signature, a hot path several hand-rolled test
  schedules construct. Grep for every harness that builds a schedule
  containing `MobSim::tick` before doing this, per this repo's own
  system-widening rule.

## Configuration

None — every constant (`INVULNERABLE_TICKS = 220`, both heal intervals, the
emergence/skull-impact blast powers, the `8.0`/`5.0` skull damage figures,
the `10`/`40`-second wither-effect durations) is a vanilla constant
transcribed as a named `const` with its own doc comment citing the field it
came from.

## Dependencies

- `lodestone_model::{BlockPos, Vec3, Difficulty}` — the only external types
  `crate::wither`/`mobs::wither_pattern` use.
- `lodestone_entity::projectile::Projectile`/`DamageFlags` and
  `lodestone_entity::ai::mob::ProjectileKind::WitherSkull` — the skull's own
  launch/impact plumbing, shared with every other projectile this crate
  spawns.
- Nothing else: no dependency on `crate::dragon` (a sibling boss fight, not a
  shared base), `lodestone-world`, or any `crates/protocol/*` crate.
