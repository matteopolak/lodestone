# Player simulation

## What it is

The local player's simulated survival systems — hunger, drowning, burning,
freezing/climbing, swimming, fall damage and death, experience, status effects,
eating/drinking, creative flight, and the sneak-at-a-ledge back-off — plus the
ECS component sets that back the player, the session/HUD state, and every other
entity. Survival rules are mostly server-authoritative
(`crates/lodestone-server/src`); movement integration and component wiring live
client-side (`lodestone-physics`, `lodestone-ecs`, `lodestone-shell`).

## How it works

### Hunger

`food.rs` is a pure value type; `PlayerVitals` applies its health
consequences. Depletion is a three-layer buffer: **exhaustion** accumulates
from actions (capped 40.0); each tick, exhaustion **strictly above** `4.0`
(`EXHAUSTION_DROP`) is spent — `4.0` subtracted, one point of **saturation**
lost; only once saturation hits `0.0` does the visible **food level** drop,
never on Peaceful. Because the test is strict, a fresh spawn sprints **241**
blocks, not 200, before the bar first moves. Costs per block/event: sprint
`0.1`, walk/crouch **0** (vanilla's literal `0.0F` multiply — charging it
invents depletion vanilla doesn't have), break a block `0.005`, attack
`0.1`; swim/jump/sprint-jump aren't charged (no wire signal yet). Eating
applies `nutrition * modifier * 2.0` saturation, clamped to the new food
level.

Regen/starvation is **one** if/else chain sharing a timer (can't regen and
starve the same tick): saturated regen (10 ticks, heal `min(sat,6)/6`,
exhausts by the amount spent), slow regen (80 ticks, heal `1.0`, exhaust
`6.0`, needs food ≥ 18), starvation (80 ticks, `1.0` damage, food ≤ 0), else
reset. The gate is `health > 10 || HARD || (health > 1 && NORMAL)` —
**Easy and Peaceful still starve a player down to 10 health**; Peaceful's
real protection is that depletion never reaches zero food there.

### Drowning

`PlayerVitals::tick(eye_in_water)` mirrors vanilla's own base per-tick entity update's water-breath
block: `-1` air/tick submerged, `+4`/tick refill capped at
`MAX_AIR_SUPPLY = 300`; at `<= -20` air resets to `0` and deals `2.0`
(`DROWN_DAMAGE`) straight to health, no armour model. Fully submerged, a
player takes 300 ticks (15s) to empty then 20 more to the first hit —
**320 ticks to the first hit**, then every 20 ticks after, since the reset
re-arms an identical countdown. Submersion is read at the eye
(`feet + 1.62`); lava does not drown (`is_water` is narrower than the
general fluid check). Respiration, water breathing, bubble columns,
i-frames and mob drowning are not modelled.

### Burning

The counter **counts down**; damage fires when `remaining % 20 == 0` **and
the entity is not in lava** — an 8-second (160-tick) burn hits exactly 8
times. Lava deals its own `4.0`/tick instead. Ignition only ever **raises**
the counter, never shortens it, so stepping from lava into fire doesn't put
the lava burn out. Fire/soul fire last 160 ticks, contact damage 1.0 vs
**2.0** for soul fire; lava lasts 300 ticks, contact 4.0. Fire Resistance is
a damage-source check (immune to the *damage*, not the counter) — the
entity still visibly burns, only the hit is refused. The `is_fire` tag
needs both `on_fire` (the tick) and `in_fire` (the block) or resistance
half-works. Rain/water extinguishing and mob ignition are not modelled.

### Climbing and freezing

Scaffolding and ladders share one `is_climbable` flag, but only a ladder
clamps descent to zero while sneaking — sneaking on scaffolding still
descends at the ordinary climb speed. Powder-snow freezing: `frozen_ticks`
(0..=140, `TICKS_REQUIRED_TO_FREEZE`) climbs `+1`/tick inside the block,
falls `-2`/tick outside it; fully frozen, damage (`1.0`) applies every 40
ticks. Freezing is **not** gated on `!flying` — a creative-flying player
drifting through snow still freezes, with none of the stuck-drag slowdown.

**A per-tick "in water/lava/powder-snow/inside-block" check must scan every
integer cell the movement crossed during the tick, not just the post-move
destination** — sampling only the endpoint lets a fast mover tunnel through
a one-block layer within a single tick without ever resting in it. The fix
scans the union of the pre- and post-move bounding boxes, narrowed to cells
actually swept through; reuse this shape for any new stuck/submersion/
ground check.

### Swimming

Vanilla tells the server "sprinting" over two packets: one only gets
**stored**, and a separate command packet actually flips `isSprinting()`
server-side. Both must be sent — the state packet every tick, the command
edge-triggered — or a "sprinting" swim is really normal-speed. Double-tap-
forward sprint uses a 7-tick window that must age inside the fixed 20 Hz
tick loop, not per frame, or the timing goes frame-rate dependent.

Water movement integrates buoyancy, drag and the jump decision. Depth
Strider and `movement_speed` fold through the server-reported `Attributes`
component via a three-stage attribute fold, the same path Speed/Slowness/
Soul Speed and the sprint modifier use — no separate client-side
effect→attribute path exists. Looking down while swimming pulls vertical
velocity toward the look angle (`0.085` if `lookAngleY < -0.2`, else
`0.06`), gated on looking down, jumping, or a submerged head. Lava movement
is a **different branch**, not retuned water: flat `0.02` input speed
regardless of depth plus `-baseGravity/4`; shallow/deep splits at
`fluidHeight <= 0.4`, shallow keeping water's buoyant slow-descent, deep a
flat `scale(0.5)` with no falling adjustment.

The camera jerk on a pose change is vanilla's own `Camera` smoothing
(`eyeHeight += (target - eyeHeight) * 0.5` per tick), separate from the
entity's own eye height, which snaps atomically — an `EyeHeightSmoother`
eases half the remaining distance per tick and is what the camera reads.

### Fall damage and death

Movement is client-authoritative, so the server reads no physics tick for
`fallDistance` — everything is driven off inbound move packets, sampling
the block below the feet. Damage: `floor((distance + 1e-6 -
SAFE_FALL_DISTANCE) * blockModifier * FALL_DAMAGE_MULTIPLIER)`, applied
only when positive (`SAFE_FALL_DISTANCE = 3.0`, multiplier `1.0`, default
block modifier `1.0`, cushioned `0.2` for hay/honey, slime `0.0`, powder
snow never calls the function). Lethal damage must route through one
`publish_health` helper that also sends `player_combat_kill` —
`set_health(0.0)` alone pins the client at zero hearts with no death
screen, since that comes from a separate packet; respawn is symmetric
(`encode_respawn` plus reset vitals), or hearts refill behind a screen
that never closes.

`cancel` (mid-flight — water, climbable) zeroes fall distance only;
`reset` (teleport/respawn) also drops the remembered last y — using
`cancel` for a teleport banks phantom fall distance. **Lava does not
cancel a fall**, only water does ("any fluid cancels" makes a lava dive a
safe landing), and water needs **two** rules: a guard suppressing
accumulation while submerged, and a separate reset zeroing a banked fall
on entry — a guard alone still charges the next dry landing. Feather
Falling, Resistance, vehicles, Slow Falling/Levitation and dripstone's
landing bonus are not modelled.

### Experience

The level-up cost curve has three regimes: `7 + level*2` below 15,
`37 + (level-15)*5` from 15 to 30, `112 + (level-30)*9` from 30 up — both
seams **inclusive**. 30 levels costs 1395 points; the first level costs 7.
Orb denominations are greedy change-making over
`[2477, 1237, 617, 307, 149, 73, 37, 17, 7, 3, 1]`, not a uniform cap, so
orb *count* is player-visible (100 XP becomes 4 orbs: `73+17+7+3`). Awarding
XP re-expresses the progress carry against the **new** level's cost —
leaving it as `progress - 1.0` badly over-levels a big award; underflow at
level 0 zeroes progress/total rather than borrowing. `SET_EXPERIENCE`'s wire
order is **progress, level, total** — not declaration order.

Wired sources: mob death (needs a player hit within 100 ticks, not a baby —
an animal's reward is `1 + roll(3)`, not a flat table), ore mining at the
block centre (six ores — iron/gold/copper and deepslate forms — drop **no**
XP by design), and furnace smelting on container close (split the recipe
key on the *first* colon only). Orbs merge only when ids are congruent
**mod 40**, so consecutively spawned orbs never merge with each other and a
big award scatters into several piles. Breeding, fishing, trading and
bottles o' enchanting are unwired; orbs are not persisted.

### Status effects

The shared registry `lodestone-physics::effect` classifies from — no
duration/stacking/tick logic lives in physics. The periodic interval is a
right-shift, `25 >> amplitude`, reaching **every tick** at high amplifiers
rather than never (poison bottoms out at amplifier 5, wither/regen at 6).
The tick count passed in is the **remaining** duration, so the modulo
counts down — a 210-tick poison first fires at tick 11 — and the effect is
removed the tick duration hits zero, not the tick after. Intervals/effects:
poison 25 ticks, 1.0 damage, **only if health > 1** (cannot kill); wither
40 ticks, 1.0 damage, **no health floor** (can kill); regeneration 50
ticks, heal 1.0 if hurt; hunger every tick, `0.005*(amplifier+1)`
exhaustion; instant health `4 << amplifier`; instant damage
`6 << amplifier`.

Stacking is a hidden-effect chain, not last-write-wins: a higher amplifier
takes over and pushes a shorter current effect onto a hidden queue that
resurfaces later (its clock still runs while queued); equal amplifier keeps
the longer duration; a lower, longer-lasting amplifier is queued rather
than dropped. A splash/lingering impact scales by
`1.0 - sqrt(distance_sq)/4.0` (full at contact, zero at 4 blocks) — instant
effects scale the *amount*, timed effects scale the *duration* and drop
outright under 20 ticks remaining. Resistance and Absorption overlay onto
the damage pipeline at hit time (Absorption's nominal `4.0*(amplifier+1)`
cushion, not vanilla's per-hit-depleting pool). Speed/Slowness's attribute
fold and a lingering potion's own cloud entity are unwired.

**Wire sync**: two encoders put an applied/cleared effect on the wire
(entity id, registry id, amplifier, duration, an ambient/visible/icon/blend
bitset) — decode existed long before either encoder, so an effect could
change health/exhaustion with no HUD icon appearing. Only a beacon's
periodic grant calls it in production; `/effect give`/`clear` still only
mutate the registry directly.

### Eating and drinking

Vanilla splits one method across both sides, each dropping the half it
can't do: **particles are client-only**, **sounds are server-only
broadcast**. Getting this backwards is silent both ways.

The emit cadence is a **conjunction**: past `consumeTicks * 0.21875` of the
use *and* `remaining % 4 == 0`. A default 32-tick food emits 6 times, not 8
(modulo alone) or 24 (fraction alone) — 5 particles per emission, 16 on the
final bite. The eat-transform jiggle is `1 - scaledUsageTime^27` — the
exponent is the animation's whole character; a linear `1-t` disagrees by
18× at 90% remaining. The bob only opens once `scaledUsageTime < 0.8` (the
*last* 80% of the use, since it counts down), and the eat/drink transform
applies **after** the ordinary item-in-hand transform. Crumb velocity must
be **multiplied**, not power-scaled, or the vertical bias comes out ~10×
too fast.

### Creative flight

Two unrelated systems: **creative flight** (`Abilities.flying`,
server-granted, collides with terrain, ordinary air-travel arithmetic with
three modifications) and a developer **free-fly/noclip** camera, since
deleted (superseded by `/gamemode creative` plus real flight).

Flight *wraps* ordinary travel rather than adding a fourth mode: the
pre-travel Y velocity is captured, ordinary travel runs, and the result's Y
is **overwritten** with `preTravelY * 0.6` — gravity that tick is
discarded, not damped, and there's no horizontal drag term despite the
"0.6 looks like drag" intuition. Flying speed has four arms: flying +
not-sprinting uses server `flyingSpeed` (default `0.05`), flying +
sprinting doubles it; **not flying**, sprinting uses the exact literal
`0.025999999` (not `0.026`), walking uses `0.02` — the non-flying sprint
arm was missing for a while, undershooting every sprint-jump by 30%.
Thirteen sites gate on `!flying` (ground-jump, fluid travel branch, fall
distance reset, block speed factor, climbable, swim/crouch pose, edge
back-off, stuck-in-block, bubble-column impulse, fluid push, glide, and
flight cancelling on landing). The toggle is a double-press-space edge in a
7-tick window gated on server `mayfly`; the vertical impulse on toggling up
is `inputYa * flyingSpeed * 3.0`, the raw non-sprint-doubled speed.
Spectator noclip, vehicles and the one-tick takeoff hop are not modelled.

### Edge back-off

The sneak-at-a-ledge rule is a **desync rule, not a feel rule**: the
server replays claimed movement and teleports back if the replay disagrees
by more than 0.25 blocks in one packet, with no accumulator. The gate: not
flying, not moving upward, sneaking (the raw shift key, not the crouch
pose), and "above ground" by less than the step-height attribute (default
`0.6`). The ground probe is a **whole-footprint** test, horizontally inset
but vertically expanded downward at the feet plane — it clears exactly on
the tick a move would leave the supporting block, which is what makes it
look like ledge detection despite never measuring a distance to one.
Vanilla steps the candidate delta toward zero in `0.05` increments across
**three** loops (X alone, Z alone from the original delta, then X+Z
jointly for outside corners); at walking speed the loop always terminates
on its first step, and the joint loop only matters well above walking
speed. Only the local candidate delta is rewritten — velocity and
collision downstream keep the un-backed-off value, so releasing shift
mid-hold launches at full speed. World-border collision is the one
unmodelled term of the block/entity/border triple this check consults.

### Component sets

Player, session/HUD and generic-entity state all live as `bevy_ecs`
components rather than hand-rolled structs, split across a few `World`s for
dependency reasons (native/browser sharing, net-thread vs. driver-thread
ownership).

**Entity components** (non-player entities: position, health, equipment,
item identity, render interpolation) use a three-state wrapper — component
**absent** (never mentioned), present with inner `None` (cleared), or
present with `Some(v)` — because a dropped item's texture is sent once at
spawn and never again, and a default `None` component instead of absence
would blank it on the next metadata packet. Ingest indexes entities by
network id eagerly as they spawn, so a spawn-then-move in one batch still
resolves; the local player is indexed too (vanilla never sends it its own
spawn packet), guarded so spawn/removal never evicts that id and so ending
a session clears the whole index — it used to survive a rejoin,
duplicating every mob under the new session's ids alongside the frozen
old ones. Render-side interpolation runs its own small schedule (clock
advance → animate → fold ingest state → extract draws) in a fixed order
the interpolation math depends on.

**Local player components** hold physics state, movement intent, the
free-fly flag, hotbar selection, death state and outbound-movement edge
trackers on one entity, advanced each tick input → physics → send (send
last, so whatever a later system wrote is what the server is told). A
borrowed collision view can't be a scheduler resource directly (must be
`'static`, no `unsafe`), so a `CollisionSource` trait lets an implementor
own what it borrows from. Four small driver-pushed values (auto-jump,
glider equipped, firework boost, item-use ticks) share one shape: the
driver writes once per tick, a physics system folds it in.

**Session components** replaced three separate scoreboard/tab-list/
boss-bar implementations (one dead, the other two disagreeing on team
decoration and tab-list departure — there was no player-list-removal arm,
so a player who left never left the tab list). Each server event now
folds into one aggregate once, split only by whether the reader must work
with no shell attached (net thread) or is driver-only (session phase,
vitals, XP, overlays). A duplicate schedule registration once caused a
silent, total ingest blackout; a build-time ambiguity check now requires
exactly one system per component.

**Player entities** makes a connected player a real entity other
connections receive: a registry (RAII-registered per connection, so a
dropped connection can't leak a ghost player), a per-connection tab-list
diff, and two encoders. A real client **silently discards** an entity-add
for a player uuid it holds no player-info for, so the tab-list update must
reach the wire before the entity spawn, from one lock snapshot. Player
entity ids use a separate counter than mob ids — unlike vanilla's single
allocator, which matters only once both share an owner.

### Chat and social

**Player chat**'s inbound half decodes chat, checks the sender's announced
signing session (rejecting a stale or invalid signature, replying only to
the sender), and publishes accepted messages to a bounded, append-only log
that every connection drains with its own absolute-sequence cursor — never
a drain-all, which would hand each message to whichever connection's timer
fired first. Messages relay as **system chat**, not a real signed player
chat: this server verifies what it accepts but nothing lets a peer verify
it too, so there's no delete-chat, no report chain, no "Not Secure"
indicator. `enforce-secure-profile` only gates *unsigned* chat before a
session is announced; once one exists, an unsigned message from it is
always rejected regardless of the flag.

The **Social Interactions** screen lists connected players from the live
tab list with a per-player Hide-in-Chat toggle, persisted immediately, and
it is real: a signed message's sender uuid carries through to the local
chat feed, and a hidden sender's message is dropped before it reaches the
feed (unsigned/system chat has no sender key and always shows). The Report
button stays permanently inactive — it needs real signed-chat relay,
which doesn't exist — and vanilla's Microsoft-managed Blocked tab is
omitted entirely rather than built as geometry over nothing.

## How to change it

* **A new exhaustion producer**: call `add_exhaustion` and **guard it on
  the invulnerable-ability check** — forgetting it starves a creative
  player.
* **A new ignition/freeze/fall-distance source**: raise or reset the
  counter through the module's own function (raise, or a `min(0, …)`
  clear) — a plain overwrite silently shortens an existing effect.
* **A new movement-check predicate**: reuse the whole-segment sweep scan
  rather than sampling only the destination cell.
* **Another discrete server-side toggle** (sprint, flight, glide): send an
  edge-triggered command packet, never folded into the per-tick state
  packet — that split is exactly the bug that made sprint-swimming
  silently run at normal speed.
* **A new XP source**: call `give_points` and send the experience packet
  — a mutation with no send is what made the bar invisible for a whole
  session. Persist the level/progress/total triple together; modelling
  the fields without reading them back on join is worse than not
  modelling them (it silently overwrites a save's real XP with zero).
* **Another periodic status effect**: give it its own interval and amount
  — never derive one effect's constant from another's.
* **A new consumable**: one row in the consumable table, plus a separate
  row in the food table if it restores hunger — the lists differ (milk,
  potions, the ominous bottle are drinkable but not food).
* **Adding a `!flying` gate**: put it at the vanilla call site, inside the
  travel/tick function it belongs to — never a parallel flight-only path.
* **A component on the player/session/entity set**: add it to the
  matching spawn *and* reset function in the same change — a component
  added only at spawn leaks the previous session forward, and an
  id-keyed index never cleared on session end duplicates every entity on
  rejoin.
* **A trait method gating a scan or fold**: check every wrapper forwards
  it — a default lets a non-forwarding wrapper compile silently and take
  the default in production while its own narrower tests pass.

### Gotchas across the board

* Regeneration costs food — a healed player is also exhausted, so a gate
  reading "the next health packet" may see a *heal* first once well-fed.
* Poison cannot kill, wither can, though both share one death message.
* The experience packet's wire order, and any adjacent same-typed field
  pair, are transposition traps — verify against the packet's own
  write/read, never its constructor or field order.
* A death or respawn packet omitted where its counterpart is sent is
  *worse* than omitting both — it looks like it worked.
* Every timer here is a **tick count**, never wall-clock time — real-clock
  reads are unavailable on the browser target this code also ships to.

## Configuration

| knob | default | effect |
|---|---|---|
| `natural_health_regeneration` (game rule) | on | gates the two hunger regen arms only — starvation still applies with it off |
| difficulty | — | Peaceful never depletes food; sets the starvation health floor |
| `Abilities.flyingSpeed` | `0.05` | server-set creative flight speed |
| `Abilities.mayfly` | `false` | the flight-toggle gate |
| `enforce-secure-profile` | `false` | rejects unsigned chat before a session is announced |
| `SAFE_FALL_DISTANCE` / multiplier | `3.0` / `1.0` | fall-damage formula constants |
| `MAX_AIR_SUPPLY` / `DROWN_DAMAGE` | `300` / `2.0` | drowning cadence and hit |
| `TICKS_REQUIRED_TO_FREEZE` | `140` | powder-snow freeze threshold |
| sprint trigger window | `7` ticks | double-tap-forward sprint window |

Everything else here is a vanilla constant, not a runtime option.

## Dependencies

* `crates/lodestone-server` — `food.rs`, `vitals.rs`, `burning.rs`,
  `mob_effects.rs`, `experience.rs`, `fall.rs`, `players.rs`,
  `chat_session.rs`, driven from `server.rs`'s per-tick vitals timer and
  packet dispatch.
* `lodestone-physics` — the tick functions, `PlayerState`, `CollisionView`,
  the edge back-off and swept-segment helpers, the effect classifier.
* `lodestone-entity` — the attribute fold (base + modifiers → value, and
  wire-shaped snapshot conversion) Depth Strider and movement speed ride on.
* `lodestone-ecs` — the player, entity and session component sets and their
  tick/ingest schedules.
* `lodestone-controller` — held-key input and the fixed-tick sprint window,
  shared between native and browser.
* `lodestone-game` — the session aggregates (scoreboard, tab list, boss
  bars, menus, active effects) the ECS components wrap.
* `crates/versions/26.2` — the only family implementing `ServerProtocol`,
  hence the only one that can host any server-authoritative system here.
* `docs/keybindings.md` — the eager-persistence rule the social screen's
  toggle follows.
