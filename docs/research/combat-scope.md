# Scoping: Tier 1 item 7, "combat feel"

## What it is

Headline finding: the shield and the bow are functionally dead in combat, for two
independent, already-diagnosed reasons. `ClientAction::ReleaseUseItem` is encoded by all
four protocol adapters but constructed nowhere in `lodestone-shell` (a serverbound island,
the same shape as the documented `SetFlying` case), so a drawn bow can never fire and a
raised shield can never be intentionally lowered. Separately, `Sim::use_item_live`
short-circuits on any entity target even after a failed interact, so aiming at a hostile
mob — the common case in combat — never reaches the generic use-item path at all. Also
corrects three stale claims in `docs/backlog.md` item 7 (two already-shipped features, one
mechanic that was never real).

Read first: `docs/backlog.md` item 7, `docs/combat.md` (the existing record —
attack/knockback/attack-strength-ticker/hurt-overlay all landed under issues
#12/#72/#98/#121), `docs/view-bobbing.md`. Vanilla source is `.cache/mc/26.2/{src,client-src}`,
all `file:line` below read directly, not from memory.

**Headline finding, not on any existing list**: the shield and the bow are functionally
dead in combat right now, for two independent, high-confidence, zero-ambiguity reasons
(items 1 and 2 below). Everything else in this doc is secondary to that.

**Also headline**: `docs/backlog.md` item 7 is stale on 3 of its 6 named gaps. Two are
already shipped (attack-strength cooldown bar, hurt tint), one was never real (camera
shake — grepped clean, see below). Re-verified per `CLAUDE.md`'s "re-verify before
routing around X doesn't exist" rule, not assumed.

---

## 1. Table: vanilla mechanic → our status

| vanilla mechanic | jar `file:line` | our status | our `file:line` |
|---|---|---|---|
| Attack-strength ticker + delay curve | `Player.java:268,1816-1835` (`attackStrengthTicker++`; delay `= 20/attack_speed`) | **correct** | `crates/lodestone-ecs/src/player.rs` (`AttackStrengthTicker`), `crates/lodestone-shell/src/sim.rs::attack_strength_delay`/`attack_strength_scale` |
| Crosshair cooldown indicator | `Hud.java:448-465` | **correct** (CROSSHAIR variant only; HOTBAR/ready-icon deliberately cut, #121) | `crates/lodestone-shell/src/hud.rs` (`HudFrame::attack_cooldown`) |
| Damage computation, crit/sweep math, knockback amount, i-frames | `Player.java:951-1053`, `LivingEntity.java:1177-1231` | **N/A — correctly not built**: wire `Attack` packet carries only target id, damage is 100% server-authoritative | `docs/combat.md` confirms; verified the packet shape carries no damage field |
| Knockback applied to *local player* on being hit | `Entity.lerpMotion`/`Entity.java:2649-2651` (unconditional velocity replace) | **correct** | `apply_entity_velocity`, `crates/lodestone-ecs/src/ingest.rs` — writes into `PhysicsState.0.velocity` |
| Knockback applied to *remote entities* (incl. mobs we hit) | same | **correct**, generic `Velocity` component, pre-existing | `crates/lodestone-ecs/src/ingest.rs` |
| Knockback-resistance attribute, sprint-knockback bonus (+0.5, `PLAYER_ATTACK_KNOCKBACK` sound) | `LivingEntity.java:1641-1663`, `Player.java:964-969` | **N/A — server-side only**, nothing to build; server sends the final velocity and a broadcast sound either way | — |
| Invulnerability window / "only a stronger hit re-hurts" | `LivingEntity.java:1216-1231` | **N/A — server-side only**, only changes the damage amount sent, no client mechanic needed | — |
| `HurtTime`/`hurtDuration` countdown (drives red overlay + `bobHurt`) | `LivingEntity.java:1873-1876`, `:2044-2049` | **correct**, both event sources wired | `HurtTime` component + `tick_hurt_time`, `crates/lodestone-ecs/src/ingest.rs`/`entity.rs` |
| Per-entity hurt/death red overlay (`hasRedOverlay`) | `LivingEntityRenderer.java:281`, `OverlayTexture.java` | **correct, reaches pixels** (issue #98) — landed inside `812eb67`, documented at `ce6224d` after a staging mishap; **not stale**, verified live in tree | `crates/lodestone-render/src/entity_pipeline.rs` (`EntityInstanceRaw::with_hurt_overlay`), wired from `crates/lodestone-shell/src/entities.rs`/`gpu.rs` |
| — same overlay, `deathTime` half | `LivingEntityRenderer.java:281`'s `\|\| entity.deathTime > 0` | **absent, small** — nothing decodes a death animation, so overlay ends ~10 ticks after the killing blow instead of persisting through the death flop. Documented, not a surprise. | — |
| `bobHurt` — camera roll toward `hurtDir` | `GameRenderer.java:297-317` | **island** — mechanism built and unit-tested (`ViewBob::hurt`, `BobFrame::hurt_roll_degrees`), **zero production callers**, and `Sim::render_camera` hardcodes `damage_tilt_strength = 0.0`. Blocked on `Camera` gaining a 4th (roll) DOF — a real architectural blocker, not a forgotten call. | `crates/lodestone-shell/src/camera_rig.rs:447` (`ViewBob::hurt`, called only from its own tests at `:936`/`:973`); `crates/lodestone-shell/src/sim.rs::Sim::render_camera` (hardcoded `0.0`) |
| Crit condition (`canCriticalAttack`) + `1.5×` damage + `CRIT` particle | `Player.java:972-975,1032-1041`; particle spawn is **client-only local prediction**, `LocalPlayer.java:664-665` — never sent by the server | **absent** — no code anywhere computes the client-side crit condition. Genuinely needs building (not a wiring bug): damage math itself is still server-side, only the particle-trigger condition needs porting. | — |
| Sweep-attack condition + `SWEEP_ATTACK` particle + sound | `Player.java:978,1043-1053,1164-1192` | **unverified, likely partially working already** — `minecraft:sweep_attack` IS a registered particle id (`crates/lodestone-data/src/generated/particle_types.rs:86`) and the generic server→`ClientEvent::Particles`→emitter pipeline is real and wired (fixed as an island in `77cb3a5`/`d26c4e6`). Unlike crit, vanilla's sweep particle is server-sent (`serverLevel.sendParticles`, `Player.java:1191`), so it may already render with no new code. **Not confirmed** — the `count == 0` call encodes a direction vector rather than a spread, which our particle decode may or may not special-case correctly; needs a live-oracle check, not new code, as the first step. | `crates/lodestone-shell/src/net.rs:395-409`,`1438-1445` |
| Shield raise / hold-to-block | `LivingEntity.java:1198-1202` (`applyItemBlocking`), item is `useOnRelease()`-gated | **absent in practice — see finding #1 below**, not because blocking logic needs porting (it's 100% server-side) but because the client cannot currently *hold* a use-item state at all in the situations combat actually happens in | `crates/lodestone-shell/src/sim.rs::Sim::use_item_live` |
| Bow/crossbow draw-then-release fire | `LivingEntity.java:3471-3475,3565-3616` (`updateUsingItem`→`completeUsingItem`→`releaseUsingItem`, gated on `useOnRelease()`) | **absent in practice — see finding #2 below** | `crates/protocol/v770/src/adapter.rs:3951-3960` (encodes `ReleaseUseItem`, zero callers) |
| Attack sounds (weak/strong/crit/sweep/no-damage/knockback) | `Player.java:965,1000,1064,1069,1165` — all `playServerSideSound` → `level().playSound(null,…)`, broadcast | **already correct, no client work needed** — generic server-sound pipeline (`docs/sound-playback.md`) plays any broadcast sound; combat sounds are just broadcast sounds like any other | `crates/lodestone-shell/src/audio.rs::ShellAudio::play_sound` |
| Camera shake on nearby explosions | claimed by original issue #98 | **not a real vanilla mechanic** — grepped `client-src` clean for `[Ss]hake`; only hit is an unrelated item-wobble in `ItemInHandRenderer.java`. `ClientExplosionTracker.java` only spawns particles, holds no camera reference. | `docs/combat.md`'s own "What is deliberately not built here" already says this |

---

## 2. The two headline gaps (not on any existing list, and the top combat-feel priority)

### Finding 1 — `ClientAction::ReleaseUseItem` is a serverbound island: encoded, zero producers

`ClientAction::ReleaseUseItem` is fully defined (`crates/lodestone-model/src/action.rs:86`)
and encoded by **four** protocol adapters — v47, v340, v735, and v770
(`crates/protocol/v770/src/adapter.rs:3951-3960`, `PLAYER_ACTION` action id `5`,
`RELEASE_USE_ITEM`). Grepped for every call site of `ClientAction::ReleaseUseItem`
across the entire repo:

```
$ grep -rln "ReleaseUseItem" --include="*.rs" . | grep -v "protocol/v47\|protocol/v340\|protocol/v735\|protocol/v770\|lodestone-model"
(no output)
```

Every hit is inside the four protocol crates' own adapter/test files, or
`lodestone-model`'s action enum and its own round-trip tests. **Nothing in
`lodestone-shell` ever constructs it.** This is the exact shape `CLAUDE.md` names for
`ClientAction::SetFlying` — a serverbound island, the outbound mirror of the
inbound-island class this repo already tracks nine-plus instances of.

Confirmed in `crates/lodestone-shell/src/app.rs::lifecycle::WindowApp::window_event`: the
mouse-input match has
both `(Attack, Pressed)` and `(Attack, Released)` arms (`begin_attack`/`end_attack`),
but only `(Use, Pressed) => self.sim.use_item()` — **no `(Use, Released)` arm at
all**. The keyboard path (`InputAction::Use` bound to `V`, `app.rs`) is the same:
pressed-only. `ElementState::Released` appears exactly twice in `app.rs` — the menu
click handler and `Attack`.

**Why this matters for combat**: vanilla's `LivingEntity.updateUsingItem`
(`LivingEntity.java:3471-3475`) auto-completes a use **only** when
`!useItem.useOnRelease()`. Food and potions are `useOnRelease() == false` — fixed
duration, auto-completes server-side once the tick count elapses, so eating/drinking
*appears* to work fine even with this bug, which is exactly why it has gone
unnoticed. **Bow, crossbow, and shield are all `useOnRelease() == true`** — they
structurally cannot complete without the explicit `RELEASE_USE_ITEM` packet
(`LivingEntity.java:3602-3616`, `releaseUsingItem`/`stopUsingItem`). Right now:
holding right-click with a bow drawn and releasing **never fires an arrow**, and
raising a shield can never be intentionally lowered by releasing the button (only by
whatever else the server uses to cancel item-use, e.g. taking damage or attacking).

### Finding 2 — `Sim::use_item_live` cannot even *start* a use in the situations combat happens in

Independent of Finding 1, `use_item_live` (`crates/lodestone-shell/src/sim.rs::Sim::use_item_live`)
often sends nothing at all on a right-click:

```rust
if let Some(entity_id) = self.entity_target() {
    self.interact_entity(entity_id);   // sends Interact, unconditionally swings, RETURNS
    return;
}
let Some(hit) = self.target() else { return };   // no block target either -> RETURN, nothing sent
```

Vanilla's `Minecraft.startUseItem` (`.cache/mc/26.2/client-src/…/Minecraft.java:1677-1741`)
does not behave this way. Its `switch (hitResult.getType())` only `return`s early on a
**successful** `case ENTITY`/`case BLOCK` interaction; an entity interact that merely
fails (the common case — most hostile mobs have no special right-click behaviour, so
`gameMode.interact` returns `PASS`) hits an explicit `break;` (`:1708`) and falls
through to the unconditional generic-use call at `:1730`
(`this.gameMode.useItem(this.player, hand)`), which is what actually raises a shield
or starts a bow draw. There is no equivalent fallback here: `entity_target()` returning
`Some` — which it does for **any** living entity within `ENTITY_REACH` (3.0 blocks,
`sim.rs:96`), hostile or not — always short-circuits to `interact_entity` and returns,
and having no target at all (aiming at open air beyond block reach, e.g. at a mob
standing more than 4.5 blocks away with nothing behind it) returns with **zero**
packets sent.

**The practical result**: aiming at a hostile mob — which is the overwhelmingly common
case when you're trying to use a shield or a bow in combat — never reaches the
generic use-item path at all. Aiming at open air (e.g. a mob just out of block reach,
or looking up) also sends nothing. Between this and Finding 1, the shield and the bow
are two of vanilla's five combat tools (melee, bow, crossbow, shield, potions) that do
not currently function as combat tools in this client, in a way invisible to every
existing test because nothing exercises "right-click while aiming at a mob."

**Corollary, worth knowing but out of this issue's scope**: this also blocks eating/
drinking food while aiming at a hostile mob (same `entity_target()` short-circuit) —
not a combat mechanic itself, but it's the same code path and the same fix, and it's
further evidence this is a real, high-traffic bug rather than an edge case.

---

## 3. Prioritised gap list (by what a stranger notices in the first hour)

1. **Fix findings 1 and 2 together.** They are the same subsystem (`use_item_live`/
   the `Use` input arm) and should land as one change:
   - Add `(InputAction::Use, ElementState::Released)` in `app.rs`'s mouse-input match
     (mirror the existing `Attack` pair) and the keyboard equivalent, sending
     `ClientAction::ReleaseUseItem` — needs a small piece of local state (a
     `UsingItem`/held-since-tick flag) since nothing currently tracks whether a use is
     in progress, the same shape `Attacking(bool)` already has for mining.
   - In `use_item_live`, make the entity-target branch fall through to the generic
     use path on anything other than a *successful* interact, instead of always
     returning — and remove the unconditional early `return` on no block target,
     replacing it with the same generic-use call vanilla's fallthrough reaches.
   - **Expected value from outside our code**: a real MC server (the live survival
     oracle already up at `:25565`) — hold right-click with a bow while aiming at a
     mob and confirm an arrow actually leaves the bow after release; equip a shield,
     aim at an attacking mob, and confirm damage is reduced/blocked server-side
     (visible via health not dropping, or dropping less, on a subsequent hit) and
     that releasing the button actually lowers the shield (a second attack lands at
     full damage). This is exactly the kind of "expected value must originate outside
     the code under test" case `CLAUDE.md` asks for — do not derive "did it work" from
     our own client's state.
   - **Negative control that must fail**: before the fix, run the same live sequence
     and confirm the arrow never spawns / the shield attack-reduction never applies —
     i.e. watch it fail, not just assert it would.
   - This is far and away the highest-value item: any survival playthrough that picks
     up a bow (very likely — bows and arrows are trivial to obtain) or crafts a
     shield will hit a silently-dead control within the first hour, and nothing in the
     test suite currently exercises "right-click while aiming at an entity" to catch it.

2. **Crit particles.** Genuinely absent, needs building from scratch — but cheap: the
   condition (`Player.java:1032-1041`) needs `fall_distance > 0` (already tracked,
   `PhysicsState.fall_distance`, `crates/lodestone-physics/src/player.rs:430`),
   `!on_ground`, `!is_sprinting` (sprint state already read elsewhere for the
   sprint-knockback sound condition), `!in_water`/`!on_climbable` (both already
   modeled per `docs/swimming.md`'s existence), and no passenger check needed yet
   (no vehicles land). The natural plug point is `Sim::attack_entity`
   (`sim.rs::Sim::attack_entity`), which already computes `attack_strength_scale` for the ticker
   reset — `fullStrengthAttack = attackStrengthScale > 0.9F` is the other half of
   the condition and is *already computed* there today, just not read for this.
   Emit via the existing `ClientEvent`-adjacent local particle path (whatever
   `d26c4e6`/`particle_atlas` already exposes for block-break debris) at the
   target entity's position — this is purely local prediction, matching vanilla's
   own client-only spawn (`LocalPlayer.java:664-665`), so it needs **no** new wire
   event.
   - **Expected value from outside our code**: `.cache/mc/26.2/client-src`'s
     `LocalPlayer.crit`/`Player.canCriticalAttack` are the spec; a hand-derived
     truth table of (falling, not sprinting, not in water) → crit is the oracle,
     not our own port.
   - **Gate**: a pixel/production gate the same shape as the hurt-overlay one in
     `docs/combat.md` — assert the particle reaches the screen from a real
     `Sim::attack_entity` call with the right physics state, with a negative
     control (grounded attack, or sprinting attack) that must show **zero** crit
     particles.

3. **Verify sweep-attack particle before writing any code for it.** Given the
   generic particle pipeline already exists and `sweep_attack` is a registered
   particle id, the first step is a live-oracle check (attack multiple mobs
   grouped together with a sword, full strength, grounded, not sprinting, slow
   enough movement) to see whether the arc already renders. If it does, this line
   item in `docs/backlog.md` is stale like the cooldown bar and hurt tint were; if
   not, the gap is almost certainly the `count == 0` directional-particle decode
   (`Player.java:1191`'s `sendParticles(SWEEP_ATTACK, x,y,z, 0, dx,0,dz,0)` — the
   `0` count means "one particle, offset vector is a direction not a spread", a
   detail worth checking in the particle decode path before assuming the whole
   particle needs building).

4. **Wire `bobHurt`'s production half**, once someone is willing to give `Camera` a
   roll DOF (the one blocker `docs/view-bobbing.md` names explicitly, with the
   exact fold spec already written: drive `ViewBob::hurt(yaw)` from the local
   player's own `HurtTime`/`EntityHurtAnimation` yaw, pass real
   `damage_tilt_strength` instead of the hardcoded `0.0` in `sim.rs::Sim::render_camera`). Lower
   priority than 1-3: a subtle camera roll on taking damage is real vanilla feel
   but far less noticeable in the first hour than "my bow doesn't shoot" or "no
   crit particles ever, even off a fall attack."
   - **Gate**: `docs/view-bobbing.md` already names
     `the_dropped_roll_is_the_only_disagreement…` as the pattern to follow —
     measure the rendered roll in degrees against `sin(t⁴·π)·14·damageTiltStrength`
     computed from constants in `GameRenderer.java:297-317`, not against our own
     prior output.

5. **`deathTime` overlay persistence.** Cosmetic, small (~10 ticks of the overlay
   cutting off early on a killing blow instead of persisting through the death
   flop animation). Needs the death animation decoded at all, which is a bigger
   prerequisite than this one field — not worth doing standalone; note for
   whoever eventually builds mob death animations.

6. **Correct `docs/backlog.md` item 7 and the Tier 1 issue tracker.** Strike
   "attack-strength cooldown bar" and "hurt tint" (both shipped, #121/#98) and
   "camera shake" (never a real vanilla mechanic) from the item-7 description;
   replace with the two use-item findings above, which are the real remaining
   combat gaps this investigation found. This is a documentation fix, not code,
   but leaving the stale list in place is exactly the failure mode `CLAUDE.md`'s
   "staleness" section warns cost real work before.

---

## 4. Every island found, both directions

- **Inbound-adjacent but really outbound**: `ClientAction::ReleaseUseItem` —
  encoded by 4 protocol adapters (v47, v340, v735, v770), **zero producers**
  anywhere in `lodestone-shell`. Same class as the documented `SetFlying` case.
  See Finding 1.
- **Logic island, not a routing-switch island**: `ViewBob::hurt`/
  `BobFrame::hurt_roll_degrees` — fully implemented and unit-tested in
  `camera_rig.rs`, called only by its own tests (`camera_rig.rs:936,973`), never
  from production. Distinct second problem: even if wired, `Sim::render_camera`
  passes a hardcoded `0.0` for `damage_tilt_strength`
  (`sim.rs::Sim::render_camera`), so wiring the call alone would still show nothing — both
  hops need fixing together. Not a routing-switch bug (no `ingest`/`session`/
  `net.rs` arm involved); this one is a plain "nobody calls the function" island,
  worth naming because it is the same defect class by a different mechanism.
- **Not an island, but structurally equivalent in effect**: Findings 1 and 2
  together mean the shield and bow *item-use* path is reachable from almost no
  real input sequence during combat, even though every individual piece (the
  encoder, the `EntityInteraction` enum, the generic use-item send) is present
  and correct in isolation — the exact "individually built, individually
  tested, zero pixels because nothing calls it right" shape, just expressed as
  "almost nothing calls it under the conditions that matter" rather than a
  literal zero.

---

## 5. Already checked and confirmed correct — do not re-do

- Attack packet send, entity targeting/ray priority (entity before block),
  swing-on-every-click (miss/block/entity) — `docs/combat.md`, re-confirmed
  against `Minecraft.startAttack`/`Minecraft.java:1603-1672`.
- Server-sent knockback applied to the local player's own `PhysicsState.velocity`
  (`ingest.rs::apply_entity_velocity`) — correct, matches `Entity.lerpMotion`'s
  unconditional-replace semantics, `Entity.java:2649-2651`.
- Remote-entity knockback via the generic `Velocity` component — correct, no
  combat-specific work needed.
- Knockback-resistance attribute, sprint-knockback bonus, invulnerability
  window/"only a stronger hit re-hurts" — all 100% server-side per
  `LivingEntity.java:1177-1231,1641-1663` and `Player.java:964-969`; nothing to
  build on our side beyond receiving the resulting velocity/health, which is
  already correct.
- `HurtTime` countdown, both `EntityDamaged` and `EntityHurtAnimation` producers,
  routed correctly through `ingest::handles_event`
  (`crates/lodestone-ecs/src/ingest.rs:129-130`) — **not** stale, re-checked
  directly against the switch, both arms present.
- Per-entity hurt/death red overlay reaching real pixels — **not** stale despite
  the confusing git history (`a4bd15a` built it with zero callers, `812eb67`
  landed the wiring under an unrelated commit message, `ce6224d` is the doc
  pointer) — verified the call chain is intact in the current tree
  (`entities.rs`/`gpu.rs` call `with_hurt_overlay` from real per-frame draw code,
  not just from the crate's own test).
- Attack-strength ticker, delay curve, crosshair cooldown indicator — correct,
  issue #121, including the correct non-constant delay
  (`20.0 / attack_speed` attribute, not a hardcoded number).
- Combat sounds (weak/strong/crit/sweep/no-damage/sprint-knockback) — no
  client-specific work needed; all are ordinary server-broadcast sounds and the
  generic sound pipeline (`docs/sound-playback.md`) already plays any
  broadcast sound including mob hurt/death, so these should already be audible
  whenever the server actually sends them (which it does, since `Player.attack`
  runs its full sound-selection logic server-side unconditionally).
- Camera shake — confirmed **not a real vanilla mechanic at all** (`docs/combat.md`
  already did this work; independently re-grepped `client-src` for `[Ss]hake` and
  got the same one unrelated hit in `ItemInHandRenderer.java`). Do not scope this
  as a gap under any future combat-feel work.
- Damage/crit/sweep math itself, and the wire shape of the `Attack` packet — fully
  server-authoritative by design in both vanilla and this codebase; the client's
  only job for crit/sweep is the **local feedback trigger** (particles/sound), not
  the damage number.

---

## What was run

- `grep`/`/usr/bin/grep` across `.cache/mc/26.2/{src,client-src}` and the whole
  `crates/` tree (never through `rtk`, per `CLAUDE.md`'s warning that it strips
  matched text).
- `git log --oneline --all --grep` for `combat`/`knockback`/`sweep`/`crit` to find
  prior work before assuming anything was unbuilt.
- `git show` on `ce6224d` (the doc-pointer commit) to recover the #98 story that a
  plain `git log --grep '#98'` would have missed (per the exact trap `CLAUDE.md`
  names for that commit).
- `cargo xtask connectedness` (read-only, no build) — v770 clientbound
  109/141 decoded, 0 decoded-but-stranded, serverbound 53/69 encoded; too coarse
  to see the `ReleaseUseItem` gap by itself (it only flags *decode*-side
  stranding, and this is an *encode-side*, zero-producer gap), which is why the
  manual grep for call sites was necessary and is the stronger evidence here.
- No `cargo build`/`test` run — read-only investigation, no edits made, no need
  to compile anything to answer "what calls this."
- Did not launch the game client or drive the live oracle interactively (no RCON
  commands issued) — the live-verification steps in section 3 are recommendations
  for the implementing agent, not things I ran.
