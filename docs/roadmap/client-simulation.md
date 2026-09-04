# Client simulation, physics and input

## What this is

This roadmap covers client movement modes, vitals and their movement/combat effects,
prediction and reconciliation, input, and the tick/frame seam. Rendering, server
simulation, plugins, benchmarks, and wire coverage are covered elsewhere in the roadmap
directory.

Movement is a server-arbitrated surface: a divergence manifests as correction, clipping, or
an interaction that appears not to land. The live horizontal disagreement bound is a squared
error of `0.0625` (0.25 blocks) for one packet; do not rely on accumulated drift to hide an
incorrect integrator.

## Current foundations

- Levitation, slow falling, jump boost, edge back-off, place/break prediction, teleport
  confirmation, and container-click prediction already have implementations and evidence.
- `CollisionView` has concrete state-backed answers across the normal physics surface.
  The remaining work is the `is_solid_face` and `stuck_multiplier` approximations, not a
  wholesale collision adapter.
- Flight, vehicle/riding, combat feedback, core input verbs, and view bobbing are distinct
  workstreams. Keep simulation responsibilities separate from their render presentation.

## Roadmap

| Priority | Feature | Completion condition |
|---|---|---|
| 1 | Creative and spectator flight | Flight is a physics mode with the correct input, collision, and movement packet behaviour rather than debug free-cam. |
| 1 | Attribute-driven movement speed | Speed and slowness feed the movement integrator through session attributes. |
| 1 | Eye-height smoothing | `EyeHeightSmoother` advances in `Sim` and drives the camera consistently. |
| 1 | Bubble-column impulse | Column state produces the appropriate vertical impulse. |
| 1 | Live reconciliation gate | Exercise horizontal correction, vertical disagreement, and sneak-at-a-ledge behaviour against a live server. |
| 1 | Riding, vehicles, and combat feedback | Complete their independent input, simulation, and packet paths. |
| 1½ | Food-gated sprint | Deliver food/saturation from `Vitals` to controller gating. |
| 1½ | Auto-jump | Choose and perform jumps from collision and controller state. |
| 1½ | Toggle input | Support hold and toggle semantics for sneak and sprint. |
| 1½ | Mouse controls | Support invert-Y and scroll sensitivity without changing raw-look invariants. |
| 1½ | Air supply and core input verbs | Maintain the remaining survival/input state and route it to the relevant actions. |
| 2 | Elytra rocket boost | Build on existing elytra motion and read the item acceleration component before adding a new data path. |
| 2 | Riptide | Reuse the generic use-item, interaction, and cooldown model. |
| 2 | Scaffolding climb | Extend climbable collision semantics without splitting the collision model. |
| 2 | Freezing | Track frozen ticks and expose them to simulation and overlay consumers. |
| 2 | Lava depth | Model shallow/deep lava through the existing fluid seam. |
| 2 | Collision refinements | Improve `is_solid_face` and `stuck_multiplier` together. |
| 2 | Interpolated scalars | Consider a shared per-tick interpolation type only after the camera-bob rules are available. |
| 3 | Touchscreen and controller input | Retain as an explicit later platform scope. |

## Dependency and sequencing guidance

Flight, attributes, eye-height smoothing, bubble columns, and reconciliation gates are
independent enough to proceed in parallel. Controller work is likewise independent once
vitals can reach it.

Coordinate scaffolding with collision refinements because both change the same trait surface.
Build rocket boost on the existing elytra path. Sequence riptide after, or jointly with, the
generic interaction and cooldown model. Extend the established water/fluid path for lava;
do not introduce another query family. Delay a shared interpolation abstraction until there
are enough concrete advancement rules to prove its shape.

## Verification standards

- Derive golden traces from an independent oracle, never a second expression of the Rust
  under test. Compare extracted hex literals rather than generator formatting.
- Use a unique live-test username so offline identity reuse cannot turn a failed scenario
  into a dead-player blackout.
- Run live reconciliation and edge-back-off checks in survival mode; creative mode skips
  the server-side constraints those tests need to observe.
- Every absence or no-divergence claim needs a provocation that makes the detector fail.
  Choose inputs on which competing motion rules produce different values.
- Trace each feature through input or packet receipt, simulation, predicted state, packet
  emission, server acceptance, and any visual consumer. A green crate-local test cannot
  prove that chain is connected.

## How to change it

Keep state ownership explicit: controller state selects intent, `Sim` advances local
prediction, session state carries server authority, and rendering consumes results without
altering motion. Add a narrow test at the layer changed, then an end-to-end gate if the
change crosses the client/server seam. Reuse `CollisionView` and the existing fluid queries
for new environmental rules.

## Configuration and dependencies

The track depends on `lodestone-physics`, `lodestone-controller`, shell simulation, ECS
session state, and protocol movement actions. Golden-trace generation and live-oracle
scripts provide external evidence; the live survival oracle requires the configured test
server environment.
