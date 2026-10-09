# Client simulation, physics and input

## What it is

The roadmap for client movement modes, vitals and their movement and combat effects, prediction and reconciliation, input, and the tick/frame seam. Rendering, server simulation, plugins, benchmarks and wire coverage have their own roadmaps.

Movement is server-arbitrated: a divergence shows as correction, clipping or an interaction that seems not to land. The live horizontal disagreement bound is a squared error of `0.0625` (0.25 blocks) per packet; accumulated drift must not be relied on to hide a wrong integrator.

## Roadmap

Already implemented with evidence: levitation, slow falling, jump boost, edge back-off, place and break prediction, teleport confirmation, container-click prediction. `CollisionView` has state-backed answers across the physics surface; what remains is the `is_solid_face` and `stuck_multiplier` approximations.

| Priority | Feature | Completion condition |
|---|---|---|
| 1 | Creative and spectator flight | A physics mode with correct input, collision and movement packets, not debug free-cam |
| 1 | Attribute-driven movement speed | Speed and slowness feed the integrator via session attributes |
| 1 | Eye-height smoothing | `EyeHeightSmoother` advances in `Sim` and drives the camera |
| 1 | Bubble-column impulse | Column state gives the vertical impulse |
| 1 | Live reconciliation gate | Horizontal correction, vertical disagreement and sneak-at-a-ledge against a live server |
| 1 | Riding, vehicles, combat feedback | Independent input, simulation and packet paths |
| 1½ | Food-gated sprint | Food and saturation from `Vitals` gate the controller |
| 1½ | Auto-jump | Jumps chosen from collision and controller state |
| 1½ | Toggle input | Hold and toggle semantics for sneak and sprint |
| 1½ | Mouse controls | Invert-Y and scroll sensitivity without touching raw-look invariants |
| 1½ | Air supply and core input verbs | Remaining survival and input state routed to actions |
| 2 | Elytra rocket boost | Build on existing elytra motion; read the item acceleration component first |
| 2 | Riptide | Reuse the use-item, interaction and cooldown model |
| 2 | Scaffolding climb | Extend climbable collision without splitting the model |
| 2 | Freezing | Frozen ticks exposed to simulation and overlays |
| 2 | Lava depth | Shallow and deep lava through the existing fluid seam |
| 2 | Collision refinements | `is_solid_face` and `stuck_multiplier` together |
| 2 | Interpolated scalars | A shared per-tick interpolation type only after camera-bob rules exist |
| 3 | Touchscreen and controller input | Explicit later platform scope |

Sequencing: flight, attributes, eye-height smoothing, bubble columns and reconciliation gates are independent; controller work is independent once vitals reach it. Coordinate scaffolding with collision refinements (same trait surface), build rocket boost on the elytra path, sequence riptide after or with the generic interaction model, extend the fluid path for lava rather than adding a query family, and delay a shared interpolation abstraction until concrete rules prove its shape.

## How to change it

Keep ownership explicit: controller state selects intent, `Sim` advances local prediction, session state carries server authority, rendering consumes without altering motion. Add a narrow test at the changed layer, then an end-to-end gate if it crosses the client/server seam. Reuse `CollisionView` and fluid queries for environmental rules.

Verification standards:
- Derive golden traces from an independent oracle and compare extracted hex literals, not generator formatting.
- Use a unique live-test username so offline identity reuse cannot turn a failed scenario into a dead-player blackout.
- Run live reconciliation and edge-back-off checks in survival; creative skips the constraints they observe.
- Every no-divergence claim needs a provocation that makes the detector fail, on inputs where competing motion rules differ.
- Trace each feature through input or packet receipt, simulation, predicted state, packet emission, server acceptance and visual consumer; a crate-local green test cannot show the chain is connected.

## Dependencies

`lodestone-physics`, `lodestone-controller`, shell simulation, ECS session state and protocol movement actions. Golden traces and live-oracle scripts supply external evidence; the live survival oracle needs the configured test server.
