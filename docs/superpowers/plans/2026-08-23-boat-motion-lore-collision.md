# Boat Motion, Lore, Collision, and Dismount Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Smooth locally controlled boats at render cadence without changing 20 Hz physics, stop invisible boat masks hiding riders, render styled item lore, make boats solid to players, and place integrated-server riders safely on dismount.

**Architecture:** Fixed-tick vehicle state records previous/current poses and render code samples them with the shared accumulator alpha. Item lore remains structured `Text` from v770 decode through the game stack and tooltip. Hard entity collision is threaded through the existing movement integrator using a capability separate from crowd pushing, while the integrated server owns dismount placement and position sync.

**Tech Stack:** Rust, Bevy ECS schedules/resources, wgpu render passes, Lodestone's canonical item/text model, protocol v770 network-NBT, `lodestone-physics` AABB collision, Tokio server tests.

---

## File structure

- `crates/lodestone-ecs/src/vehicle.rs`: fixed-tick previous/current controlled-vehicle poses and correction resets.
- `crates/lodestone-shell/src/entities.rs`: controlled-vehicle render-pose sampling shared by boat draw and rider seat.
- `crates/lodestone-shell/src/gpu/entity_passes.rs`: separate visible entity and invisible water-mask batches.
- `crates/lodestone-shell/src/gpu/frame.rs`: draw masks after every visible entity and before translucent water.
- `crates/lodestone-shell/tests/boat_water_mask_pixels.rs`: water suppression plus rider-visibility pixel gate.
- `crates/lodestone-model/src/item.rs`: canonical ordered lore component.
- `crates/protocol/v770/src/adapter/inventory.rs`: preserve lore NBT as `Text`.
- `crates/protocol/v770/tests/item_components.rs`: wire-level lore decode gate.
- `crates/lodestone-game/src/item.rs`: typed lore component and model/game conversion.
- `crates/lodestone-shell/src/container/tooltip.rs`: styled lore lines and tooltip geometry.
- `crates/lodestone-model/src/adapter.rs`: hard-collision capability beside push capability.
- `crates/lodestone-data/src/entity_census.rs` and generated census data: v770 collidable-type lookup.
- `crates/protocol/v770/src/adapter/mod.rs`: expose collision capability through `EntityFacts`.
- `crates/lodestone-physics/src/player.rs`: thread entity colliders through travel.
- `crates/lodestone-physics/tests/entity_collision.rs`: player landing/pass-through/same-vehicle gates.
- `crates/lodestone-shell/src/sim/collide.rs`: include hard colliders in the per-tick neighbourhood.
- `crates/lodestone-shell/src/sim/tests.rs`: shell producer gate for a boat.
- `crates/lodestone-server/src/mobs/vehicles.rs`: pure boat dismount candidate selection.
- `crates/lodestone-server/src/server.rs`: apply and transmit authoritative dismount position.
- `crates/lodestone-server/tests/serve_play.rs`: end-to-end integrated-server dismount wire gate.
- `docs/riding.md`, `docs/entity-push.md`, `docs/item-data-component-decode.md`, `docs/README.md`: behavior and maintenance documentation.

### Task 1: Phase-lock controlled-boat rendering to the fixed-tick accumulator

**Files:**
- Modify: `crates/lodestone-ecs/src/vehicle.rs`
- Modify: `crates/lodestone-ecs/tests/vehicle_authority.rs`
- Modify: `crates/lodestone-shell/src/entities.rs`
- Modify: `crates/lodestone-shell/src/sim/camera.rs`

- [ ] **Step 1: Write failing pose-history tests**

Add a `VehicleRenderPose` value and tests that specify the API before production code exists:

```rust
#[test]
fn a_controlled_vehicle_keeps_the_pose_from_the_start_of_the_last_tick() {
    let mut app = app_with_controlled_boat();
    app.world_mut().run_schedule(GameTick);
    let first = app.world().resource::<ControlledVehicle>().0.clone().unwrap();
    app.world_mut().run_schedule(GameTick);
    let second = app.world().resource::<ControlledVehicle>().0.as_ref().unwrap();
    assert_eq!(second.previous.position, first.current_pose().position);
    assert_eq!(second.previous.yaw, first.current_pose().yaw);
}

#[test]
fn a_vehicle_correction_resets_both_render_endpoints() {
    apply_vehicle_correction(&mut app, corrected_position, corrected_rotation);
    let held = app.world().resource::<ControlledVehicle>().0.as_ref().unwrap();
    assert_eq!(held.previous, held.current_pose());
}
```

In `entities.rs`, add failing sampling tests:

```rust
#[test]
fn controlled_vehicle_render_pose_uses_frame_alpha_not_interp_clock_time() {
    let pose = sample_vehicle_pose(previous, current, 0.25);
    assert_vec3_close(pose.position, Vec3::new(2.5, 64.0, 0.0));
}

#[test]
fn controlled_vehicle_yaw_takes_the_short_path_across_zero() {
    assert_angle_close(sample_vehicle_pose(pose(359.0), pose(1.0), 0.5).yaw, 0.0);
}
```

- [ ] **Step 2: Run the focused tests and verify RED**

Run:

```bash
cargo test -p lodestone-ecs --test vehicle_authority --no-fail-fast
cargo test -p lodestone-shell entities::tests::controlled_vehicle --lib
```

Expected: compile/test failure because pose history and accumulator-based sampling do not exist.

- [ ] **Step 3: Implement fixed-tick pose history**

Add:

```rust
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VehicleRenderPose {
    pub position: Vec3d,
    pub yaw: f32,
    pub pitch: f32,
}

impl ControlledVehicleState {
    pub fn current_pose(&self) -> VehicleRenderPose {
        VehicleRenderPose {
            position: self.motion.position,
            yaw: self.yaw,
            pitch: self.pitch,
        }
    }
}
```

Store `previous: VehicleRenderPose`. Seed it from the authoritative ECS transform, copy `current_pose()` into it immediately before each fixed vehicle tick, and reset it to the correction pose in `apply_vehicle_moved`.

- [ ] **Step 4: Implement one shared per-frame sample**

Add a shortest-path sampler:

```rust
fn lerp_degrees(from: f32, to: f32, alpha: f32) -> f32 {
    let delta = (to - from + 180.0).rem_euclid(360.0) - 180.0;
    (from + delta * alpha).rem_euclid(360.0)
}

fn sample_vehicle_pose(from: VehicleRenderPose, to: VehicleRenderPose, alpha: f32) -> VehicleRenderPose {
    let alpha = alpha.clamp(0.0, 1.0);
    VehicleRenderPose {
        position: from.position.add(to.position.subtract(from.position).scale(alpha as f64)),
        yaw: lerp_degrees(from.yaw, to.yaw, alpha),
        pitch: from.pitch + (to.pitch - from.pitch) * alpha,
    }
}
```

When the track id matches `ControlledVehicle.server_id`, `extract_entity_draws` and `riding_render_seat` use the sampled pose with `FrameClock::interp_alpha`. Delete `interp_window_for` and its controlled-vehicle one-tick special case; uncontrolled tracks retain `INTERP_WINDOW`.

- [ ] **Step 5: Verify GREEN and frame-rate independence**

Run the two focused commands again, then add a deterministic 20/60/144 Hz loop asserting identical authoritative tick endpoints and monotonic render samples. Expected: all pass.

- [ ] **Step 6: Commit Task 1 paths**

Use the repository's pathspec commit workflow with only the four files above.

### Task 2: Draw invisible boat masks after visible entities

**Files:**
- Modify: `crates/lodestone-shell/src/gpu/entity_passes.rs`
- Modify: `crates/lodestone-shell/src/gpu/frame.rs`
- Modify: `crates/lodestone-shell/tests/boat_water_mask_pixels.rs`

- [ ] **Step 1: Write a failing batch-order test**

Expose a small plan result containing `visible` and `water_masks`, then assert:

```rust
#[test]
fn every_visible_entity_batch_precedes_every_boat_water_mask() {
    let planned = plan_test_entities(&[boat_draw(), rider_draw()]);
    assert!(planned.visible.iter().all(|b| b.model != "boat_water_patch"));
    assert!(planned.water_masks.iter().all(|b| b.model == "boat_water_patch"));
}
```

Extend the pixel fixture with a player limb crossing the patch plane and compare mask-on/mask-off images: rider pixels must be unchanged while interior water pixels differ.

- [ ] **Step 2: Run tests and verify RED**

Run:

```bash
cargo test -p lodestone-shell every_visible_entity_batch_precedes_every_boat_water_mask --lib
cargo test -p lodestone-shell --test boat_water_mask_pixels --no-fail-fast
```

Expected: the unit test cannot separate batches, and the rider pixels are occluded by the current mask ordering.

- [ ] **Step 3: Separate preparation output**

Return:

```rust
pub(super) struct PreparedEntityBatches {
    pub visible: Vec<EntityDrawBatch>,
    pub water_masks: Vec<EntityDrawBatch>,
}
```

Build masks in their own instance list instead of inserting `boat_water_patch` into material/skin groups. Keep statistics based on visible entities only.

- [ ] **Step 4: Draw the two lists in order**

In `frame.rs`, render all `visible` batches through the entity pipeline, then all `water_masks` through `water_mask_pipeline`, without switching back and forth. Leave translucent terrain after both lists.

- [ ] **Step 5: Verify GREEN and commit Task 2 paths**

Re-run both commands. Expected: rider pixels survive and existing water-mask assertions still pass. Commit only the three Task 2 files.

### Task 3: Preserve lore from the v770 wire into the canonical model

**Files:**
- Modify: `crates/lodestone-model/src/item.rs`
- Modify: `crates/protocol/v770/src/adapter/inventory.rs`
- Modify: `crates/protocol/v770/tests/item_components.rs`

- [ ] **Step 1: Write a failing wire decode test**

Build a component patch containing two network-NBT lore values with different styles, decode the stack, and assert:

```rust
assert_eq!(stack.components.lore.len(), 2);
assert_eq!(stack.components.lore[0].plain(), "First line");
assert_eq!(stack.components.lore[1].plain(), "Second line");
assert_eq!(stack.components.lore[1].style.color, Some(TextColor::Rgb(0x12_34_56)));
```

- [ ] **Step 2: Run and verify RED**

Run `cargo test -p lodestone-v770 --test item_components lore --no-fail-fast`.
Expected: compile failure because `ItemComponents::lore` does not exist.

- [ ] **Step 3: Add model storage and decode**

Add `pub lore: Vec<Text>` to model `ItemComponents`. Replace the discard loop with:

```rust
let mut lore = Vec::with_capacity(lines);
for _ in 0..lines {
    let nbt = read_network_nbt(reader).map_err(dec_err)?;
    lore.push(Text::from_nbt(&nbt));
}
components.lore = lore;
```

Preserve the 256-line guard and default empty value. Update every explicit `ItemComponents` fixture that does not use `..Default::default()`.

- [ ] **Step 4: Verify GREEN and commit Task 3 paths**

Run the focused v770 test and `cargo test -p lodestone-model --lib`. Commit the three paths.

### Task 4: Render styled lore in slot tooltips

**Files:**
- Modify: `crates/lodestone-game/src/item.rs`
- Modify: `crates/lodestone-shell/src/container/tooltip.rs`
- Modify: `docs/item-data-component-decode.md`

- [ ] **Step 1: Write failing conversion and tooltip tests**

Add `LORE_COMPONENT` and specify a typed list:

```rust
ComponentValue::Lore(Vec<Text>)
```

The conversion test asserts model -> game -> model preserves both lines. The tooltip test asserts order `name, lore[0], lore[1], potion/enchantment/book lines, advanced lines`, and asserts lore spans carry default dark-purple italic styling while a child RGB colour survives.

- [ ] **Step 2: Run and verify RED**

Run:

```bash
cargo test -p lodestone-game item::tests::lore --lib
cargo test -p lodestone-shell container::tooltip::tests::lore --lib
```

Expected: compile failure because the typed lore component/accessor is absent.

- [ ] **Step 3: Implement typed conversion and tooltip lines**

Add `LORE_COMPONENT`, `ComponentValue::Lore`, `ItemStack::lore()`, and both conversion arms. Add:

```rust
fn lore_lines(stack: &ItemStack) -> Vec<TooltipLine> {
    stack.lore().iter().map(|line| {
        let styled = Text {
            style: TextStyle {
                color: Some(TextColor::DarkPurple),
                italic: Some(true),
                ..TextStyle::default()
            },
            extra: vec![line.clone()],
            ..Text::default()
        };
        TooltipLine {
            text: styled.to_plain_string(),
            colour: DARK_PURPLE,
            spans: Some(styled.to_spans()),
        }
    }).collect()
}
```

The empty styled parent supplies vanilla's defaults while the authored line remains a child, so explicit colours or an explicit `italic: false` override those defaults through the existing `TextStyle::inherit` path. Do not flatten and reparse the component. Insert these lines immediately after `title_line` and update the stale module comment claiming lore is not decoded.

- [ ] **Step 4: Verify GREEN and commit Task 4 paths**

Run both focused commands. Commit the two code files and updated component-decoding doc.

### Task 5: Thread hard entity colliders through player movement

**Files:**
- Modify: `crates/lodestone-physics/src/player.rs`
- Modify: `crates/lodestone-physics/tests/entity_collision.rs`

- [ ] **Step 1: Write failing end-to-end movement tests**

Add a falling-player test using a collidable boat box:

```rust
#[test]
fn player_tick_lands_on_a_collidable_boat() {
    let mut boat = NearbyEntity::living(boat_feet, boat_box);
    boat.pushable = false;
    boat.collidable = true;
    tick_among_entities(&mut player, MovementInput::NONE, &Air, &profile, &[boat], PushSelf::LIVING_PLAYER);
    assert_close(player.position.y, boat_box.max_y);
    assert!(player.on_ground);
}
```

Add negative controls for `collidable = false` and `same_vehicle = true`.

- [ ] **Step 2: Run and verify RED**

Run `cargo test -p lodestone-physics --test entity_collision --no-fail-fast`.
Expected: the player falls through because travel never receives entity colliders.

- [ ] **Step 3: Thread the existing collider implementation**

Change the internal travel dispatcher to accept `nearby: &[NearbyEntity]`. Immediately before each `move_entity` call, gather once from the swept player box:

```rust
let mut colliders = Vec::new();
entity_collision_boxes(
    state.dimensions().bounding_box(state.position).expand_towards(state.velocity),
    nearby,
    &mut colliders,
);
move_entity_among_entities(&mut motion, dims, view, profile, ctx, &colliders);
```

Keep `tick()` passing `&[]`; `tick_among_entities()` passes its caller-owned slice. Apply the same slice through air/water/lava/elytra dispatch so every movement path uses one integrator.

- [ ] **Step 4: Verify GREEN and regression equivalence**

Run the entity collision test and `cargo test -p lodestone-physics --test entity_push --no-fail-fast`. Empty-neighbour behavior must remain bit-identical.

- [ ] **Step 5: Commit Task 5 paths**

Commit the two files only.

### Task 6: Classify and supply boat colliders from the shell

**Files:**
- Modify: `crates/lodestone-model/src/adapter.rs`
- Modify: `crates/lodestone-data/src/entity_census.rs`
- Modify: `crates/lodestone-data/src/generated/entity_census.rs`
- Modify: `crates/protocol/v770/src/adapter/mod.rs`
- Modify: `crates/protocol/v770/tests/entity_facts_seam.rs`
- Modify: `crates/lodestone-shell/src/sim/collide.rs`
- Modify: `crates/lodestone-shell/src/sim/tests.rs`

- [ ] **Step 1: Write failing capability and producer tests**

Assert v770 facts mark every boat/chest boat collidable and an ordinary zombie non-collidable. In the shell fixture, spawn an oak boat with `Riding(None)` and assert its `NearbyEntity` has `collidable = true`, `pushable = false`; set the local `Riding` to that id and assert `same_vehicle = true`.

- [ ] **Step 2: Run and verify RED**

Run:

```bash
cargo test -p lodestone-v770 --test entity_facts_seam collidable --no-fail-fast
cargo test -p lodestone-shell tick_nearby_entities_includes_a_boat_collider --lib
```

- [ ] **Step 3: Add the separate capability**

Extend `EntityFacts` with `pub collidable: bool`. Add an O(1) data lookup generated/default-deny for the vanilla `canBeCollidedWith` override set, with this task's production consumer initially exercising boats. Update the v770 adapter and all explicit test adapters.

- [ ] **Step 4: Widen shell gathering without widening push behavior**

Change the filter from `!facts.pushes_players => skip` to `!(facts.pushes_players || facts.collidable) => skip`; then set:

```rust
neighbour.pushable = facts.pushes_players;
neighbour.collidable = facts.collidable;
neighbour.same_vehicle = riding.0 == Some(server_id);
```

Add `MinecraftEntityId` to the query so the ridden-id comparison is exact.

- [ ] **Step 5: Verify GREEN and commit Task 6 paths**

Run both focused commands plus `cargo test -p lodestone-data --test entity_census --no-fail-fast`. Commit only Task 6 paths.

### Task 7: Place integrated-server riders at a safe dismount position

**Files:**
- Modify: `crates/lodestone-server/src/mobs/vehicles.rs`
- Modify: `crates/lodestone-server/src/server.rs`
- Modify: `crates/lodestone-server/tests/serve_play.rs`

- [ ] **Step 1: Write failing pure placement tests**

Define a resolver input containing boat transform/dimensions, player dimensions, and a collision predicate. Add tests for clear flat ground, first-side obstruction selecting the next candidate, water/fallback, and complete obstruction. Every accepted result must satisfy support below and no overlap with block or boat AABBs.

- [ ] **Step 2: Write a failing dispatch/wire test**

Extend `sneaking_dismounts_a_boat_on_the_wire`: set `player_pos`, mount a boat on known ground, send `shift: true`, then assert the response contains both empty `SET_PASSENGERS` and a position packet at the resolver's output; assert the tracked `player_pos` matches.

- [ ] **Step 3: Run and verify RED**

Run:

```bash
cargo test -p lodestone-server mobs::vehicles::tests::dismount --lib
cargo test -p lodestone-server --test serve_play sneaking_dismounts_a_boat_on_the_wire --no-fail-fast
```

Expected: current code only clears passengers and leaves the player position unchanged.

- [ ] **Step 4: Implement the pure resolver**

Use the boat's yaw and combined horizontal half-width to build vanilla-ordered escape candidates around the hull. For each candidate, test standing then crouching player boxes against the world collision view and boat box, require a supporting top surface when not in water, and return the first valid `(position, pose)`. Keep the resolver free of connection/protocol state.

- [ ] **Step 5: Apply server authority and sync**

Before clearing the rider, read the boat transform. After `dismount_rider`, resolve using `source.get()` collision, update `*player_pos`, and send:

```rust
apply(conn, state, proto.encode_set_passengers(vehicle_id, &[])).await?;
apply(conn, state, proto.encode_teleport(x, y, z, yaw, pitch)).await?;
```

Use the current `player_rot` or boat yaw fallback without changing it as part of dismount.

- [ ] **Step 6: Verify GREEN and commit Task 7 paths**

Re-run both focused commands. Commit only the three Task 7 files.

### Task 8: Documentation and proportionate verification

**Files:**
- Modify: `docs/riding.md`
- Modify: `docs/entity-push.md`
- Modify: `docs/item-data-component-decode.md`
- Modify: `docs/README.md`

- [ ] **Step 1: Update subsystem documentation**

Record accumulator-based controlled-vehicle interpolation, mask ordering, hard boat collision, integrated-server dismount authority, lore storage/render ordering, change points, configuration (`none`), and dependencies. Remove the stale `riding.md` claim that dismount needs no code and the stale tooltip claim that lore is unavailable.

- [ ] **Step 2: Run formatting checks without global formatters**

Run `git diff --check -- <all touched paths>` and manually correct only changed lines. Do not run `cargo fmt` or `rustfmt` in the shared checkout.

- [ ] **Step 3: Run focused suites in the foreground**

Run:

```bash
cargo test -p lodestone-ecs --test vehicle_authority --no-fail-fast
cargo test -p lodestone-v770 --test item_components --no-fail-fast
cargo test -p lodestone-v770 --test entity_facts_seam --no-fail-fast
cargo test -p lodestone-game --lib
cargo test -p lodestone-physics --test entity_collision --no-fail-fast
cargo test -p lodestone-physics --test entity_push --no-fail-fast
cargo test -p lodestone-shell --lib
cargo test -p lodestone-shell --test boat_water_mask_pixels --no-fail-fast
cargo test -p lodestone-server --test serve_play sneaking_dismounts_a_boat_on_the_wire --no-fail-fast
```

Expected: every command completes with zero failures; report any command not completed.

- [ ] **Step 4: Run repository health checks proportionate to touched seams**

Run in the foreground:

```bash
just check
just check-all
just check-seam
just wasm-check
```

Run `just test` only if the remaining foreground time allows the full workspace suite; otherwise report it explicitly as not run, per repository rules.

- [ ] **Step 5: Commit docs and perform final marker checks**

Commit only the four documentation paths. Re-grep for `sample_vehicle_pose`, `boat_water_patch`, `LORE_COMPONENT`, `collidable`, and the dismount resolver. Verify the shared index has zero staged paths and inspect every task commit with `git show --stat`.
