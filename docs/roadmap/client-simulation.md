# Client simulation, physics and input

## What this is

The decomposition behind Tiers 1–3's simulation slice: the movement modes not yet
modelled, vitals and their movement/combat consequences, damage, prediction and
reconciliation, input, and the tick/frame seam. The individual items are filed as
sub-issues of epics [#1](https://github.com/matteopolak/lodestone/issues/1) (Tier 1),
[#2](https://github.com/matteopolak/lodestone/issues/2) (Tier 1½),
[#3](https://github.com/matteopolak/lodestone/issues/3) (Tier 2) and
[#4](https://github.com/matteopolak/lodestone/issues/4) (Tier 3); this doc is the
ordering argument and the trap record, not a duplicate of any one issue.

**Not this doc's scope**: rendering/visuals (`client-rendering.md`), server-side
simulation (`server-simulation.md`, `server-entities.md`), the plugin framework
(`plugin-framework.md`), benchmarks (`benchmarks.md`), protocol packet coverage
(`protocol.md`). Riding/vehicles (#11), combat feedback (#12), the missing input verbs
(#16), and view bobbing/damage tilt (#58) are **already filed as their own Tier 1/1½
issues** — this doc tracks and orders them but does not re-file them.

## Why this matters more than it looks like it does

1:1 behavioural parity with vanilla is this repo's stated goal, and movement is the
most player-visible failure surface a client has: a desync reads as rubber-banding,
falling through the floor, or a punch that silently didn't land. Unlike rendering bugs,
a physics or input divergence is adversarial — the **server** is the arbiter, and
`docs/baritone-port.md` §3.2 already measured the bar it enforces: **squared error
above 0.0625 (0.25 blocks) in a single packet's horizontal disagreement, with no
multi-tick accumulator**, replaying our claimed delta through its own
`player.move(MoverType.PLAYER, …)`. That bar is demanding but achievable for a
bit-exact integrator, and unachievable for one fed missing world data or missing
input state — which is exactly the shape most of the gaps below take.

## What is already true, and where this briefing's own framing was stale

Grounding this doc required re-verifying several claims against the current tree
rather than trusting docs or issue text written earlier in the same session — per
this repo's rule 2, a stale claim is indistinguishable from a true one until checked.
Three findings worth recording so they aren't rediscovered:

- **Levitation, Slow Falling and Jump Boost are fully modelled**, with tests
  (`crates/lodestone-physics/src/effect.rs`, `player.rs`). A draft of this briefing's
  own scope list named "levitation, slow falling" among movement modes not yet
  modelled; that was wrong at the time it was checked. What actually remains open in
  the effects family is narrower and more specific — see #193 below.
- **`maybeBackOffFromEdge` is fully landed**, bit-exact against golden traces, with a
  dedicated doc (`docs/edge-back-off.md`). `docs/baritone-port.md` §3.1 still lists it
  as "zero occurrences in the tree" — that finding predates the fix and is now stale
  in that document, not in reality.
- **Issue #31 ("`CollisionView::fluid_at` is unimplemented by both shell adapters")
  appears to already be fixed** — `LiveCollision::fluid_cell_of` reads a real per-state
  amount/falling flag from `BlockModels::fluid`, corroborated by
  `docs/collision-shapes.md`. Commented on the issue with the specific code rather than
  closing it outright, in case there's context this pass missed. **Do not re-file this
  as new scope** — it is either already done or a one-line question, not a gap.
- Conversely, **`docs/baritone-port.md` §3.2's headline claim — "the live view
  implements three of `CollisionView`'s twelve methods" — is now stale in the other
  direction.** Reading `crates/lodestone-shell/src/collision.rs` today shows all twelve
  methods backed by real per-state data (`friction_at`, `speed_factor_at`,
  `jump_factor_at`, `bounce_at`, `stuck_at`, `climbable_at`, `is_water_at`, `is_lava_at`,
  `fluid_at`, `blocks_motion_at`, `is_solid_face_at`, plus `collision_boxes`), matching
  `docs/block-physics-constants.md` and `docs/collision-shapes.md`'s own account of that
  work landing. What remains are the two **residual, self-documented** approximations
  tracked as #216, not a three-of-twelve gap. This is worth restating precisely because
  it means the navigation-plugin blocker that document's §3.2 is built around is smaller
  than that document currently says — a correction for whoever next reads it, not an
  action this doc's scope owns (the plugin framework is out of scope here).

Also confirmed working and *not* re-filed: block-place/break prediction (the `Mining`
and `Placement` predictors in `crates/lodestone-shell/src/sim.rs` carry a real,
monotonic prediction sequence), the teleport-confirmation handshake (the protocol
adapter emits `AcceptTeleportation` automatically alongside every decoded
`TeleportPlayer`, not something `sim.rs` has to remember to do), and container-click
prediction (`docs/container-clicks.md`, already complete).

## The tracks

| item | tier | epic | what |
|---|---|---|---|
| [#191](https://github.com/matteopolak/lodestone/issues/191) | 1 | #1 | Creative/spectator flight as a real physics mode, not a debug free-cam |
| [#193](https://github.com/matteopolak/lodestone/issues/193) | 1 | #1 | `movement_speed` not attribute-driven — Speed/Slowness inert |
| [#194](https://github.com/matteopolak/lodestone/issues/194) | 1 | #1 | `fall_distance` never accumulated |
| [#196](https://github.com/matteopolak/lodestone/issues/196) | 1 | #1 | Wire the built-but-unused `EyeHeightSmoother` into `Sim` |
| [#199](https://github.com/matteopolak/lodestone/issues/199) | 1 | #1 | Bubble columns apply no impulse |
| [#220](https://github.com/matteopolak/lodestone/issues/220) | 1 | #1 | Live gate: the rubber-band threshold and the vertical-disagreement clamp |
| [#11](https://github.com/matteopolak/lodestone/issues/11) | 1 | #1 | Riding and vehicles *(already filed)* |
| [#12](https://github.com/matteopolak/lodestone/issues/12) | 1 | #1 | Combat feel *(already filed)* |
| [#200](https://github.com/matteopolak/lodestone/issues/200) | 1½ | #2 | Sprint never locally gated on food level |
| [#201](https://github.com/matteopolak/lodestone/issues/201) | 1½ | #2 | Auto-jump not implemented |
| [#202](https://github.com/matteopolak/lodestone/issues/202) | 1½ | #2 | Toggle-vs-hold sneak/sprint absent |
| [#203](https://github.com/matteopolak/lodestone/issues/203) | 1½ | #2 | Mouse feel incomplete (invert-Y, scroll sensitivity) |
| [#16](https://github.com/matteopolak/lodestone/issues/16) | 1½ | #2 | Missing input verbs *(already filed)* |
| [#60](https://github.com/matteopolak/lodestone/issues/60) | 1½ | #2 | Air supply *(already filed)* |
| [#31](https://github.com/matteopolak/lodestone/issues/31) | 1½ | #2 | `fluid_at` *(already filed — likely already fixed, see above)* |
| [#206](https://github.com/matteopolak/lodestone/issues/206) | 2 | #3 | Elytra firework-rocket boost not modelled |
| [#208](https://github.com/matteopolak/lodestone/issues/208) | 2 | #3 | Riptide not modelled at all |
| [#210](https://github.com/matteopolak/lodestone/issues/210) | 2 | #3 | Scaffolding's distinct climb rules not modelled |
| [#212](https://github.com/matteopolak/lodestone/issues/212) | 2 | #3 | Freezing (`frozen_ticks`) not tracked |
| [#214](https://github.com/matteopolak/lodestone/issues/214) | 2 | #3 | Lava shallow-vs-deep branch not modelled |
| [#216](https://github.com/matteopolak/lodestone/issues/216) | 2 | #3 | Residual `CollisionView` approximations (`is_solid_face`, `stuck_multiplier`) |
| [#218](https://github.com/matteopolak/lodestone/issues/218) | 2 | #3 | Shared per-tick interpolated-scalar abstraction (refactor) |
| [#219](https://github.com/matteopolak/lodestone/issues/219) | 3 | #4 | Touchscreen/controller input — recorded deferral, not active scope |

## Ordering and dependency edges

Phases are about *what unblocks what*, not raw severity — a Tier 2 item with no
dependencies can go before a Tier 1 item that's blocked on something else.

**Phase A — independent, ready now, highest desync risk.** #191, #193, #194, #196,
#199 touch different files and different modes (flight, attribute folding,
fall-distance accumulation, camera smoothing, water impulses) and can proceed fully in
parallel; none blocks another. #220 (the live rubber-band gate) has no code
dependency either, but doing it **early** is valuable because its result — whether the
vertical-disagreement clamp really is dead code in 26.2 — directly informs how much
the #194 (`fall_distance`) implementer needs to worry about server rejection of a
vertical-only change. Do #220 before or alongside #194, not after.

**Phase B — Tier 1½ input, independent of Phase A.** #200 (food-gated sprint), #201
(auto-jump), #202 (toggle sneak/sprint), #203 (mouse feel) are all self-contained
`lodestone-controller`/`lodestone-shell` changes with no physics-engine dependency.
#200 does depend on the food/saturation value already sitting on the `Vitals`
component (`crates/lodestone-ecs/src/session.rs`) reaching the controller crate — that
plumbing doesn't exist yet and is this issue's actual work, not a blocker on something
else.

**Phase C — Tier 2, mostly independent, two real edges.**

- #210 (scaffolding) and #216 (residual `CollisionView` approximations) touch the same
  trait (`CollisionView::is_climbable` widening for scaffolding; `is_solid_face`
  and `stuck_multiplier` for the approximations) — sequence them together to avoid two
  agents editing the same trait signature in the same window, per this repo's
  shared-checkout hazards.
- #206 (firework boost) builds on the elytra base (`tick_elytra`, already landed) and
  needs to confirm the firework's `acceleration_power` item-data-component is already
  decoded (`docs/item-prototypes.md`'s coverage) before assuming new decode work — check
  before assuming, per this repo's rule 2.
- #208 (riptide) has a real edge onto #12 (combat): riptide's trigger needs the same
  `Interact`/use-item plumbing #12 is already building for the `ATTACK` action.
  Sequence #208 after #12 lands the packet model, or coordinate closely if both are
  in flight — building the packet-and-cooldown model generically now would let riptide
  reuse it rather than duplicate it later.
- #214 (lava shallow/deep) is independent but shares shape with the water `fluid_at`
  work already landed — the fastest path is extending that existing seam rather than
  designing a parallel one.
- #218 (shared tick-scalar abstraction) is explicitly sequenced **after** #58 (view
  bobbing) lands, per its own body: the audit of whether a shared type is warranted
  needs `bobView`/`bobHurt`/`xBob`/`yBob`'s actual advancement rule in hand, not a
  guess at it. Filing it now records the concern; starting it now would be premature.

**Phase D — Tier 3, no dependencies, low priority.** #219 is a recorded decision, not
an implementation task, and blocks nothing.

```
Phase A (parallel):      #191  #193  #194  #196  #199  #220
Phase B (parallel):      #200  #201  #202  #203
Phase C:  #210 ─┬─ #216         #206 (after item-prototypes check)
                └─ (shared trait edit)
                                #208 (after #12's Interact/cooldown model)
                                #214 (extends the water fluid_at seam)
                                #218 (after #58 lands)
Phase D:                 #219
```

## Verification standards this track inherits

Every issue above states its own proof obligation, but the shared rules (from
`CLAUDE.md` and this repo's evidence standard) are:

- **Golden traces come from an independent oracle.** `crates/lodestone-physics/tests/gen_golden.py`
  is a Python re-implementation written from `.cache/mc/26.2/src`, not from this
  crate's Rust — agreement between the two is the whole point, and writing the oracle
  from the Rust under test would make that agreement circular.
- **Run the zero-deletion control before trusting a golden-trace diff.** The checked-in
  `golden_traces.rs` is `cargo fmt`'d; the generator's raw output is single-line.
  Regenerating unmodified and diffing *lines* produces a false 4:1 mismatch every time;
  diff extracted hex literals instead. This has already produced one false alarm in
  this exact test suite (`docs/swimming.md`'s issue #59 section) and would again.
- **A live gate needs `lodestone-testsupport`'s `unique_username`.** Offline mode
  derives the UUID from the username; a dead player from a name collision is held on
  the death screen, which sends no chunks — a silent, total blackout that would
  invalidate any of this track's live gates (#220 above, and the still-owed second half
  of `docs/edge-back-off.md`'s live verification) without an obvious symptom.
- **The creative oracle cannot observe several of these.** The server-side rubber-band
  check (#220) and the sneak-edge back-off are both skipped for `isCreative()`. Use
  `./scripts/live-oracles/survival.sh`, not `creative.sh`, or the result is a
  guaranteed vacuous pass.
- **An absence needs a control proving the detector would have fired.** Every "no
  correction", "no divergence" claim in this batch's issues pairs with a provocation
  case designed to trigger the same detector, per the four-species vacuous-test table
  in `CLAUDE.md`.

## Scale, honestly

Most of the domain this track was scoped to cover turned out to already be built to a
high standard — bit-exact against JVM oracles, with golden traces — or already tracked
by large, correctly-scoped existing issues (#11 riding, #12 combat, #16 input verbs,
#58 view bobbing, #60 air supply). That is why this doc files 18 new issues rather than
padding toward a larger number: the real remaining gaps in client simulation, physics
and input are narrower and more precisely located than the original scope suggested,
and several of them (#193, #194, #196) are places where the fix is already fully
specified in an existing doc and just hasn't been done yet. Inventing narrow issues to
hit a target count would have cost more in false scope than a lower, accurate count
costs in coverage.
