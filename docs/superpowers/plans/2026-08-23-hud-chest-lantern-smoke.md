# HUD Chest, Lantern, and Campfire Smoke Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Correct the HUD screenshot's double chest and lantern alcoves, restore Minecraft 26.2's block-entity-driven campfire smoke, and regenerate a visually verified `05-hud.png`.

**Architecture:** Scene-only defects stay in `scripts/screenshot-scenes/05-hud.txt` and gain a hermetic fixture test. Campfire smoke moves out of the generic random ambient block scan: loaded campfire block entities become fixed-tick sources, and `Particles` mirrors the 26.2 roll, burst count, and position distribution before using the existing cosy/signal smoke emitter.

**Tech Stack:** Rust, Lodestone's ECS/simulation and particle crates, vanilla 26.2 block-state data, RCON screenshot scenes, wgpu headless capture, Cargo/just.

---

## File map

- Create `crates/lodestone-shell/tests/hud_scene_fixture.rs`: hermetic invariants for the committed HUD scene.
- Modify `scripts/screenshot-scenes/05-hud.txt`: correct chest halves and construct shaded lantern alcoves.
- Modify `crates/lodestone-shell/src/block_entities.rs`: resolve loaded lit campfires into `(position, signal_fire)` particle sources.
- Modify `crates/lodestone-shell/src/particles.rs`: exact 26.2 campfire block-entity particle tick; remove campfires from the ambient block scan.
- Modify `crates/lodestone-shell/src/sim/render_sources.rs`: gather and run campfire sources on the fixed 20 Hz particle tick.
- Modify `docs/particle-catalogue.md`: document the 26.2 lifecycle split.
- Modify `docs/screenshots.md`: record the fixed scene traps and visual controls.
- Modify `docs/images/05-hud.png`: live regenerated artifact after code and scene tests pass.
- Regenerate `docs/README.md` only if the docs-index gate reports a generated-summary change.

### Task 1: Make the HUD scene structurally correct

**Files:**
- Create: `crates/lodestone-shell/tests/hud_scene_fixture.rs`
- Modify: `scripts/screenshot-scenes/05-hud.txt`

- [ ] **Step 1: Write the failing scene invariant tests**

Create a hermetic test that reads the real committed scene rather than duplicating it into a fixture:

```rust
const HUD_SCENE: &str = include_str!("../../../scripts/screenshot-scenes/05-hud.txt");

fn has_command(command: &str) -> bool {
    HUD_SCENE.lines().map(str::trim).any(|line| line == command)
}

#[test]
fn south_facing_chest_halves_connect_inward() {
    assert!(has_command(
        "setblock -1 64 18 minecraft:chest[facing=south,type=right]"
    ));
    assert!(has_command(
        "setblock 0 64 18 minecraft:chest[facing=south,type=left]"
    ));
}

#[test]
fn lanterns_sit_in_backed_roofed_alcoves() {
    for command in [
        "setblock -8 64 21 minecraft:lantern[hanging=false]",
        "setblock 8 64 21 minecraft:lantern[hanging=false]",
        "fill -9 64 21 -7 65 22 minecraft:stone_bricks hollow",
        "fill 7 64 21 9 65 22 minecraft:stone_bricks hollow",
    ] {
        assert!(has_command(command), "missing HUD scene command: {command}");
    }
}
```

- [ ] **Step 2: Run the fixture tests and verify RED**

Run:

```bash
cargo test -p lodestone-shell --test hud_scene_fixture -- --nocapture
```

Expected: both tests fail because the scene still has outward chest halves, lanterns at `z=22`, and no alcove fills.

- [ ] **Step 3: Apply the minimal scene correction**

In `05-hud.txt`, swap the chest types and move each lantern one block toward the camera. Add the two three-wide, two-deep stone alcove shells shown in the test. Keep the existing wall at `z=22`, which becomes the opaque backing. The shell's top row at `y=65` and side columns at `x=center +/- 1` shade the lantern while leaving the front open.

- [ ] **Step 4: Run the fixture tests and verify GREEN**

Run the same command. Expected: `2 passed; 0 failed`.

- [ ] **Step 5: Commit the scene fix**

Stage only the new test and scene file, verify `git diff --cached --check`, and commit:

```text
fix(screenshots): connect the HUD chest and shade its lanterns
```

### Task 2: Discover campfire smoke from block entities

**Files:**
- Modify: `crates/lodestone-shell/src/block_entities.rs`

- [ ] **Step 1: Write the failing state/source tests**

Add a focused `campfire_smoke_tests` module beside the existing campfire helpers. Find real 26.2 state ids by scanning `block_states::STATE_COUNT`, never by hardcoding ids. Assert:

```rust
#[test]
fn only_lit_campfires_become_smoke_sources_and_distance_is_not_the_old_eight_blocks() {
    let lit = campfire_state_with(&[("lit", "true"), ("signal_fire", "false")]);
    let signal = campfire_state_with(&[("lit", "true"), ("signal_fire", "true")]);
    let unlit = campfire_state_with(&[("lit", "false")]);

    assert_eq!(campfire_smoke_source([0, 64, 18], lit), Some(([0, 64, 18], false)));
    assert_eq!(campfire_smoke_source([0, 64, 18], signal), Some(([0, 64, 18], true)));
    assert_eq!(campfire_smoke_source([0, 64, 18], unlit), None);
}
```

The `z=18` position is deliberately 14 blocks from the HUD player at `z=4`; the pure conversion has no +/-8 ambient-scan cutoff.

- [ ] **Step 2: Run the test and verify RED**

Run:

```bash
cargo test -p lodestone-shell --lib campfire_smoke_tests -- --nocapture
```

Expected: compilation/test failure because `campfire_smoke_source` does not exist.

- [ ] **Step 3: Implement source conversion and loaded-world gather**

Add:

```rust
fn campfire_smoke_source(pos: [i32; 3], state_id: u32) -> Option<([i32; 3], bool)> {
    let name = lodestone_data::block_states::block_name(state_id)?;
    if name != "minecraft:campfire" && name != "minecraft:soul_campfire" {
        return None;
    }
    let props = lodestone_data::block_states::properties(state_id)?;
    let prop = |key: &str| props.iter().find(|(name, _)| *name == key).map(|(_, v)| *v);
    (prop("lit") == Some("true"))
        .then_some((pos, prop("signal_fire") == Some("true")))
}
```

Then add `pub fn campfire_smoke_sources(handle: &SharedHandle, eye: Vec3) -> Vec<([i32; 3], bool)>`. Reuse `campfire_candidates` so source discovery walks loaded block entities, not every block in a volume; map each candidate through `campfire_smoke_source`, sort by position, and return an empty vector before login.

- [ ] **Step 4: Verify source tests GREEN**

Run the focused test. Expected: all `campfire_smoke_tests` pass.

- [ ] **Step 5: Commit source discovery**

Commit only `block_entities.rs`:

```text
fix(particles): discover smoke from lit campfire block entities
```

### Task 3: Tick campfire smoke with 26.2 semantics

**Files:**
- Modify: `crates/lodestone-shell/src/particles.rs`
- Modify: `crates/lodestone-shell/src/sim/render_sources.rs`

- [ ] **Step 1: Write the failing deterministic burst tests**

Add a test module after the ambient emitter implementation. Replace the test instance's private engine with `ParticleEngine::seeded(4096)`; that seed's first Java `nextFloat()` is below `0.11`, so the first tick must emit. Assert that one cosy source creates 2 or 3 `Behaviour::CampfireSmoke` particles whose lifetimes are in `80..130`, and one signal source creates 2 or 3 whose lifetimes are in `280..330`.

Also assert all emitted positions are horizontally within `1/3` block of the campfire centre and vertically in `[y, y + 2]`, matching `CampfireBlock.makeParticles`.

- [ ] **Step 2: Run the particle test and verify RED**

Run:

```bash
cargo test -p lodestone-shell --lib campfire_block_entity_particle_tests -- --nocapture
```

Expected: compilation/test failure because the dedicated block-entity tick is absent.

- [ ] **Step 3: Implement the exact fixed-tick burst**

Add `Particles::campfire_block_entity_tick(&[([i32; 3], bool)])`. For each source:

```rust
if self.engine.rng().next_f32() < 0.11 {
    let count = self.engine.rng().next_i32_bound(2) + 2;
    for _ in 0..count {
        // Draw every random value before borrowing the engine for emit.
        // x/z: centre +/- nextDouble()/3; y: block y + two nextDouble() values.
        emit::campfire_smoke(&mut self.engine, x, y, z, 0.0, 0.07, 0.0, signal);
    }
}
```

Remove `campfire` and `soul_campfire` from `animate_block`; that scan continues to own torches, portals, gateways, and end rods only.

- [ ] **Step 4: Wire sources into the production tick**

At the start of `Sim::tick_particles`, gather an owned source vector through the connected session's `SharedHandle`, before taking the particle or world guards. Call `campfire_block_entity_tick` once in both the live-collision and fallback branches, immediately after the generic ambient tick. Do not call it per rendered frame.

- [ ] **Step 5: Run the focused tests and verify GREEN**

Run:

```bash
cargo test -p lodestone-shell --lib campfire_block_entity_particle_tests -- --nocapture
cargo test -p lodestone-shell --lib campfire_smoke_tests -- --nocapture
```

Expected: both focused groups pass with no duplicate ambient campfire producer.

- [ ] **Step 6: Commit the lifecycle fix**

Commit only `particles.rs` and `sim/render_sources.rs`:

```text
fix(particles): tick campfire smoke from its block entity
```

### Task 4: Document and capture the corrected scene

**Files:**
- Modify: `docs/particle-catalogue.md`
- Modify: `docs/screenshots.md`
- Modify: `docs/images/05-hud.png`
- Possibly regenerate: `docs/README.md`

- [ ] **Step 1: Update subsystem documentation**

Correct `particle-catalogue.md` so `ambient_tick` no longer claims lit campfires. Add the 26.2 split: the main plume is a block-entity tick with an 11% two-or-three burst; only the campfire's lava flecks/crackle remain on block `animateTick`.

Update `screenshots.md`'s double-chest entry to say the scene now swaps the halves, and record the transparent-model-in-a-one-block-wall trap plus the shaded-alcove solution. Note that `05-hud` deliberately places a campfire beyond the old +/-8 ambient range so its visible smoke is a lifecycle regression control.

- [ ] **Step 2: Run documentation and focused code gates**

```bash
cargo test -p lodestone-shell --test hud_scene_fixture -- --nocapture
cargo test -p lodestone-shell --lib campfire -- --nocapture
cargo test -p xtask --lib docs_index_matches_committed -- --nocapture --test-threads=1
```

If the docs-index test reports drift, run `cargo xtask docs-index`, inspect the exact `docs/README.md` diff, and include only the generated change.

- [ ] **Step 3: Start the creative oracle and capture only `05-hud`**

Run the oracle in an attached foreground terminal:

```bash
./scripts/live-oracles/creative.sh
```

Then run:

```bash
LODESTONE_SCENES=05-hud just screenshots
```

Expected: `docs/images/05-hud.png` is regenerated from the real client against the 26.2 oracle.

- [ ] **Step 4: Inspect the PNG at original resolution**

Require all three visible controls before accepting the file:

- the center chest is one connected double chest with no exposed seam faces;
- both rear lanterns have opaque stone backing and warm local light visible against shaded alcove faces;
- at least one large smoke sprite is visible above the lit campfire.

If deterministic seed/tick phase produces no smoke despite the production path being correct, change the scene's explicit `@ticks` count to a measured deterministic tick that contains a plume; do not inject `/particle` or alter production probabilities.

- [ ] **Step 5: Commit docs and artifact**

Commit explicit paths only:

```text
docs: recapture the corrected HUD scene
```

### Task 5: Repository verification

**Files:** none unless a real regression is found.

- [ ] **Step 1: Run the shell crate without fail-fast**

```bash
cargo test -p lodestone-shell --no-fail-fast
```

Expected: zero failures across every shell target; GPU/live ignored tests remain explicitly reported as ignored.

- [ ] **Step 2: Run the canonical health gates in the foreground**

```bash
just check
just check-all
just check-seam
just test
```

Expected: every command exits 0. Capture full logs rather than inferring success from truncated output.

- [ ] **Step 3: Run wasm verification because the shell tick path changed**

```bash
NO_COLOR=true just wasm-check
```

Expected: all compile/confinement rows and the Trunk browser build pass.

- [ ] **Step 4: Verify repository hygiene**

Require `git diff --cached --name-only | wc -l` to be zero and `git status --short` to contain no uncommitted task files. Re-grep for `campfire_block_entity_tick`, both corrected chest commands, and the two alcove commands as marker checks.
