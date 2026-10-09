# Player simulation

## What it is

The local player's simulated survival systems (hunger, drowning, burning, freezing, swimming, fall damage and death, experience, status effects, eating, creative flight, ledge back-off) plus the ECS component sets behind the player, session/HUD state and every other entity. Survival rules are mostly server-authoritative (`crates/lodestone-server/src`); movement integration and component wiring are client-side (`lodestone-physics`, `lodestone-ecs`, `lodestone-shell`).

## How it works

All timers are tick counts, never wall-clock, because real-clock reads are unavailable in the browser build.

### Hunger

`food.rs` is a pure value type; `PlayerVitals` applies the health consequences. Depletion has three layers: exhaustion accumulates from actions (capped at 40); each tick, exhaustion strictly above 4.0 (`EXHAUSTION_DROP`) is spent, 4.0 subtracted and one saturation lost; only at zero saturation does food level drop, never on Peaceful. The strict test means a fresh spawn sprints 241 blocks, not 200, before the bar moves.

- Costs: sprint 0.1 per block, walking and crouching 0 (charging them invents depletion), break 0.005, attack 0.1. Swim/jump costs are not charged. Eating adds `nutrition * modifier * 2.0` saturation, clamped to the new food level.
- Regen and starvation are one if/else chain sharing a timer: saturated regen (10 ticks, heal `min(sat, 6)/6`), slow regen (80 ticks, heal 1.0, exhaust 6.0, needs food 18 or more), starvation (80 ticks, 1.0 damage, food 0), else reset.
- Starvation gate: `health > 10 || HARD || (health > 1 && NORMAL)`. Easy and Peaceful still starve a player to 10 health; Peaceful's protection is that food never reaches zero.

### Drowning

`PlayerVitals::tick(eye_in_water)` takes 1 air per tick submerged and refills 4 per tick, capped at `MAX_AIR_SUPPLY = 300`. At -20 air it resets to 0 and deals `DROWN_DAMAGE = 2.0` straight to health (no armour). First hit is at 320 ticks, then every 20.

The connection keeps a `PlayerEnvironment` pose and swim flag from input and resident collision/fluid data. The eye height comes from `Pose::eye_height` (standing 1.62, crouching 1.27, swimming/crawling 0.4), never the eased camera eye. Unavailable resident probes defer the air tick. Water Breathing and Conduit Power refill, and Breath of the Nautilus holds air; Respiration, bubble columns and mob drowning are not modelled.

### Burning and freezing

- Burn counter counts down; damage fires when `remaining % 20 == 0` and the entity is not in lava, so 160 ticks hit exactly 8 times. Contact damage has a 10-tick cooldown (lava's 4.0 lands every half second). Ignition only raises the counter. Fire and soul fire last 160 ticks (1.0 vs 2.0 damage), lava 300. Respawn clears counter and cooldown.
- Fire Resistance refuses the damage, not the counter, so the entity still visibly burns. Fire damage needs both the tick flag and the block flag. Rain extinguishing and mob ignition are not modelled.
- Ladders and scaffolding share one climbable flag, but only a ladder clamps sneaking descent to zero.
- Powder-snow freezing: 0..=140 ticks, +1 inside, -2 outside; fully frozen deals 1.0 every 40 ticks. It is not gated on `!flying`.

**A per-tick "in water/lava/powder-snow/inside-block" check must scan every cell the movement crossed**, the union of pre- and post-move boxes narrowed to cells swept through, or a fast mover tunnels through a one-block layer. Reuse this for any new stuck, submersion or ground check.

### Swimming and sprinting

Sprint is sent over two packets: a state packet every tick and an edge-triggered command that actually flips server-side sprinting. Send both or a "sprinting" swim runs at normal speed. Double-tap-forward uses a 7-tick window aged in the fixed 20 Hz loop, not per frame.

Water movement integrates buoyancy, drag and the jump decision. Depth Strider and movement speed fold through the server-reported `Attributes` component (the same path as Speed, Slowness, Soul Speed and the sprint modifier). Looking down pulls vertical velocity toward the look angle (0.085 when pitched steeply down, else 0.06), gated on looking down, jumping or a submerged head. Lava is a different branch, not retuned water: flat 0.02 input speed plus a quarter-gravity pull, shallow/deep split at fluid height 0.4 (deep halves velocity with no falling adjustment).

The camera eases pose-change eye height half the remaining distance per tick (`EyeHeightSmoother`) while the entity's eye height snaps.

### Fall damage and death

Movement is client-authoritative, so fall tracking is driven by inbound move packets sampling the block below the feet. The first placement movement is not fed to the tracker until the client's empty readiness marker; respawn re-arms that gate.

Damage is `floor((distance + 1e-6 - SAFE_FALL_DISTANCE) * blockModifier * multiplier)`, applied only when positive (safe distance 3.0, multiplier 1.0; modifier 1.0, 0.2 for hay/honey, 0.0 for slime; powder snow never applies it).

- Lethal damage must go through one `publish_health` helper that also sends the combat-kill packet; `set_health(0.0)` alone leaves zero hearts with no death screen. Respawn is symmetric (`encode_respawn` plus reset vitals).
- `cancel` (mid-flight, in water or climbing) zeroes distance only; `reset` (teleport, respawn) also drops the remembered last y. Using `cancel` for a teleport banks phantom distance.
- Lava does not cancel a fall; only water does, and needs two rules (suppress accumulation while submerged, and zero a banked fall on entry).
- Feather Falling, Resistance, vehicles, Slow Falling/Levitation and the dripstone bonus are not modelled.

### Experience

Level cost has three regimes, both seams inclusive: `7 + level*2` below 15, `37 + (level-15)*5` from 15 to 30, `112 + (level-30)*9` from 30. 30 levels cost 1395 points. Orb denominations are greedy change over `[2477, 1237, 617, 307, 149, 73, 37, 17, 7, 3, 1]`, so 100 XP is 4 orbs (73+17+7+3). Awards re-express progress against the new level's cost; underflow at level 0 zeroes progress rather than borrowing. The `SET_EXPERIENCE` wire order is progress, level, total, not declaration order.

Sources wired: mob death (player hit within 100 ticks, not a baby; an animal gives `1 + roll(3)`), ore mining at the block centre (iron, gold, copper and deepslate forms drop none), and furnace smelting on container close (split the recipe key on the first colon only). Orbs merge only when ids are congruent mod 40. Breeding, fishing, trading and bottles are unwired; orbs are not persisted.

### Status effects

The registry in `lodestone-physics::effect` only classifies; no duration, stacking or tick logic lives there. A periodic interval is `25 >> amplitude`, reaching every tick at high amplifiers. The tick count passed in is the remaining duration, so a 210-tick poison first fires at tick 11, and the effect is removed the tick it reaches zero.

| effect | cadence and amount |
|---|---|
| poison | 25 ticks, 1.0 damage, only if health > 1 (cannot kill) |
| wither | 40 ticks, 1.0 damage, no floor (can kill) |
| regeneration | 50 ticks, heal 1.0 if hurt |
| hunger | every tick, `0.005*(amplifier+1)` exhaustion |
| instant health / damage | `4 << amplifier` / `6 << amplifier` |

- Stacking is a hidden-effect chain: a higher amplifier takes over and pushes a shorter current effect onto a queue (its clock still runs); equal amplifier keeps the longer duration; a lower but longer effect is queued, not dropped.
- Splash and lingering impact scale by `1.0 - sqrt(distance_sq)/4.0`. Instant effects scale the amount, timed effects scale duration and drop below 20 ticks remaining.
- Resistance and Absorption overlay the damage pipeline at hit time (Absorption is a nominal `4.0*(amplifier+1)` cushion, not a per-hit-depleting pool). The Speed/Slowness attribute fold and lingering clouds are unwired.
- Wire sync: two encoders put an applied or cleared effect on the wire; only a beacon's periodic grant calls them in production, while `/effect give`/`clear` mutate the registry directly.

### Eating and drinking

Particles are client-only, sounds are server-only broadcast; swapping them is silent both ways. Emission is a conjunction: past 21.875% of the use time and the remaining ticks a multiple of 4. A default 32-tick food emits 6 times (5 particles, 16 on the final bite), not 8 or 24. The eat jiggle is `1 - t^27` on the scaled usage time; a linear curve disagrees by 18x at 90% remaining. The bob opens only in the last 80% of the use, and the eat transform applies after the ordinary item-in-hand transform. Crumb velocity is multiplied, not power-scaled.

### Creative flight and spectator

The connection retains an `Abilities` record. A client may change only `flying`, accepted only while `may_fly`. Game-mode packets and commands update the same record (creative preserves flight, spectator enables it, survival/adventure clear it); speeds survive mode changes. Movement while flying resets the fall tracker.

Flight wraps ordinary travel: capture pre-travel Y velocity, run travel, then overwrite Y with `preTravelY * 0.6` (gravity discarded, no horizontal drag). Speed has four arms: flying 0.05 (doubled when sprinting); not flying, sprinting uses the literal `0.025999999` (not `0.026`, which undershoots sprint-jumps by 30%), walking 0.02. Thirteen sites gate on `!flying` (ground jump, fluid travel, fall reset, block speed factor, climbable, swim/crouch pose, edge back-off, stuck-in-block, bubble-column impulse, fluid push, glide, landing cancel). The toggle is a double-press of space within 7 ticks gated on `mayfly`; the upward impulse is `inputYa * flyingSpeed * 3.0`. Vehicles and the takeoff hop are not modelled.

Spectator movement uses the `ServerGameMode` component, forwarded into `PlayerState::spectator` each tick. The shared dispatcher keeps airborne acceleration and flight damping but bypasses collision, crowd push, block slowdowns and pose fallback; fluids are sampled only for camera effects. The integrated server accepts absolute positions and keeps spectator flight regardless of toggle requests. While a non-spectator's column is still streaming, physics holds position and velocity: an unloaded column has no collision surface, and treating it as ground would cancel creative flight.

### Edge back-off

The sneak-at-a-ledge rule is a desync rule, not a feel rule: the server replays claimed movement and teleports back on more than 0.25 blocks disagreement in one packet, with no accumulator. The gate: not flying, not moving upward, sneaking (the raw key, not the crouch pose) and less than the step height (0.6) above ground. The ground probe tests the whole footprint, inset horizontally and extended downward, so it clears exactly when a move would leave the supporting block.

The candidate delta steps toward zero in 0.05 increments across three loops (X alone, Z alone from the original delta, then X+Z jointly for outside corners). Only the local candidate delta is rewritten; velocity and downstream collision keep the un-backed-off value, so releasing shift mid-hold launches at full speed. World-border collision is the unmodelled term.

The server has two rules that teleport a player back:

- The 0.25-block disagreement check is purely horizontal (the vertical part is zeroed first), so no fall trips it.
- The speed check is three-dimensional: squared claimed travel against a budget of 100 per packet (ten blocks). Free fall converges to 3.92 blocks per tick (`v <- (v - 0.08) * 0.98`), so a fall is exempt from both. On the survival oracle a stale claim of the pre-teleport pose measured a vertical term of 153 blocks.

### Position corrections

A correction can carry a velocity. Protocol 776 resolves X/Y/Z independently (absolute replaces, relative adds), with an optional rotation turning the current velocity first; `TeleportVelocity::resolve` is shared by local-player and remote-entity corrections. Older families keep stop-on-teleport. Explosion knockback is separate and additive via `ClientEvent::Explosion`; families whose packet carries removed-block offsets write canonical air first, and protocols 774 and 776 rely on ordinary block updates.

Every correction opens a transaction: the adapter surfaces the authoritative event, the shell applies pose and velocity, then the driver writes the acknowledgement and an unconditional full movement echo with both contact flags clear (compression bypassed so rotation is included). Between forwarding and adoption, the net action relay rewrites a `Move` to the newest fully absolute target, both when the simulation queues it and when the net loop drains it. After adoption the relay closes its window and the driver generation token drops movement submitted before the correction, keeping movement submitted after. The initial join placement is already folded into the read model, so the driver completes it after the directive batch for headless clients.

A direct entity-velocity packet is a replacement, not an impulse: the net thread folds it into entity state and mirrors it via `NetUpdate::EntityVelocity`; `Sim::step` applies it before its fixed-timestep loop only for the local server entity id. An absolute correction sets current and previous position to the target; relative axes add to the previous position independently.

`crates/lodestone-shell/tests/live/live_edge_back_off_rubber_band.rs` confirms this against the survival oracle: sneaking at a real ledge and an ordinary 151-block fall produce zero adopted `TeleportPlayer`s (`Sim::teleport_count`), contrasted with an RCON `tp` that does register. The fall's largest per-tick delta is predicted as `3.92 * (1 - 0.98^77) = 3.0926` blocks, past the 0.25 threshold and far inside the ten-block budget.

### Local movement tracing

Set `LODESTONE_PHYSICS_TRACE=1` and `RUST_LOG=lodestone_physics=debug` to trace each local tick (id, input/output pose and velocity, collision sweep axis order, clipped shapes, flags, auto-step candidates), timestamp-aligned with the `net_join` target's impulses, corrections and acknowledgements. Scoped to the local-player call, so integrated mob movement does not flood it; disabled in browser builds.

### Component sets

Player, session/HUD and entity state are `bevy_ecs` components across a few `World`s (native/browser sharing, net-thread vs driver-thread ownership).

- **Entity components** use a three-state wrapper: absent (never mentioned), present with `None` (cleared), present with `Some(v)`. A dropped item's texture is sent once at spawn, so a default `None` instead of absence would blank it on the next metadata packet. Ingest indexes entities by network id as they spawn, so a spawn-then-move in one batch resolves. The local player is indexed too (it never receives its own spawn), guarded against eviction, and ending a session clears the whole index or a rejoin duplicates every mob. Render interpolation runs its own schedule (clock advance, animate, fold ingest state, extract draws) in a fixed order its math depends on.
- **Local player components** hold physics state, movement intent, hotbar selection, death state and outbound-movement edge trackers on one entity, advanced input, physics, send (send last). A borrowed collision view cannot be a `'static` resource, so a `CollisionSource` trait lets the implementor own what it borrows. Four driver-pushed values (auto-jump, glider equipped, firework boost, item-use ticks) share one shape: written once per tick, folded in by a physics system.
- **Session components** are one aggregate per server event, split by whether the reader must work with no shell (net thread) or is driver-only. A build-time ambiguity check requires exactly one system per component, since a duplicate schedule registration once blacked out ingest.
- **Player entities** make a connected player a real entity for other connections: an RAII-registered registry, a per-connection tab-list diff and two encoders. A real client silently discards an entity-add for a player uuid it has no player-info for, so the tab-list update must reach the wire before the spawn, from one lock snapshot. Player entity ids use a separate counter from mob ids.

### Chat and social

Inbound player chat decodes, checks the sender's announced signing session (stale or invalid signatures are rejected, replying only to the sender) and publishes to a bounded append-only log each connection drains with its own absolute-sequence cursor. Messages relay as system chat, not signed player chat: nothing lets a peer verify them, so there is no delete-chat, report chain or "Not Secure" indicator. `enforce-secure-profile` gates only unsigned chat before a session is announced; once one exists an unsigned message is always rejected.

The Social Interactions screen lists live tab-list players with a persisted Hide-in-Chat toggle; a hidden sender's signed message is dropped before the local feed (unsigned and system chat always show). Report is permanently inactive and the Microsoft-managed Blocked tab is omitted.

## How to change it

- **New exhaustion producer:** call `add_exhaustion` and guard it on the invulnerable ability, or creative players starve.
- **New ignition, freeze or fall source:** raise or reset the counter through the module's function; a plain overwrite shortens an existing effect.
- **New movement-check predicate:** reuse the swept-segment scan.
- **New discrete server toggle (sprint, flight, glide):** send an edge-triggered command packet, never folded into the per-tick state packet.
- **New XP source:** call `give_points` and send the experience packet. Persist the level/progress/total triple together and read it back on join, or a save's real XP is overwritten with zero.
- **New periodic effect:** give it its own interval and amount; never derive one from another.
- **New consumable:** one row in the consumable table, plus one in the food table if it restores hunger (milk, potions and the ominous bottle are drinkable, not food).
- **New `!flying` gate:** put it at the call site inside the travel or tick function, never a parallel flight-only path.
- **New component on the player, session or entity set:** add it to both the spawn and the reset function, or the previous session leaks forward.
- **Trait method gating a scan or fold:** check every wrapper forwards it; a default lets a non-forwarding wrapper silently take the default in production.
- Regeneration costs food, so a gate reading "the next health packet" may see a heal first. A death or respawn packet sent without its counterpart is worse than neither. Adjacent same-typed wire fields (such as the experience packet) are transposition traps: verify against the packet's write/read, never its constructor.

## Configuration

| knob | default | effect |
|---|---|---|
| `natural_health_regeneration` (game rule) | on | gates the two regen arms only; starvation still applies |
| difficulty | | Peaceful never depletes food; sets the starvation floor |
| `Abilities.flyingSpeed` / `mayfly` | `0.05` / `false` | creative flight speed and toggle gate |
| `enforce-secure-profile` | `false` | rejects unsigned chat before a session is announced |
| `SAFE_FALL_DISTANCE` / multiplier | `3.0` / `1.0` | fall damage |
| `MAX_AIR_SUPPLY` / `DROWN_DAMAGE` | `300` / `2.0` | drowning |
| freeze threshold | `140` | powder-snow ticks |
| sprint trigger window | `7` ticks | double-tap sprint |
| `LODESTONE_PHYSICS_TRACE` | unset | `1`, `true`, `yes` or `on` enables the trace |

Everything else is a game constant, not a runtime option.

## Dependencies

- `crates/lodestone-server`: `food.rs`, `vitals.rs`, `burning.rs`, `mob_effects.rs`, `experience.rs`, `fall.rs`, `players.rs`, `chat_session.rs`, driven from `server/play_loop.rs` and `server/play_dispatch.rs`.
- `lodestone-physics`: tick functions, `PlayerState`, `CollisionView`, edge back-off, swept-segment helpers, effect classifier.
- `lodestone-entity`: the attribute fold. `lodestone-ecs`: component sets and schedules. `lodestone-controller`: held-key input and the sprint window. `lodestone-game`: session aggregates.
- Hosted families `crates/versions/1.7`, `1.8`, `1.9`, `1.13` and `26.2` translate onto their wire formats.
- `docs/keybindings.md`: the eager-persistence rule the social toggle follows.
