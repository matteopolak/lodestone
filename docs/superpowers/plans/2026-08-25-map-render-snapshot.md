# Map Render Snapshot Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace per-frame deep clones of every filled map with copy-on-write shared snapshots, skip the map diagnostic while F3 is closed, and re-profile the megaworld before migrating block-entity gathers.

**Architecture:** `MapStore` owns an `Arc<BTreeMap<i32, MapState>>` and each `MapState` owns an `Arc<Vec<u8>>`; incoming patches use `Arc::make_mut`, so an unchanged frame clones two pointers while an update copies only data still observed by an older snapshot. The renderer's map source returns the shared colour payload and `WindowApp::redraw` calls the diagnostic only when F3 is visible. Samply captures always include the symbol sidecar required by the repository's cost-table analyzer.

**Tech Stack:** Rust 2024, `Arc`, `BTreeMap`, Bevy ECS session state, wgpu map textures, Python `unittest`, Samply, the Java megaworld benchmark.

---

### Task 1: Make benchmark flamegraphs self-symbolizing

**Files:**
- Modify: `scripts/client-frame-benchmark.py`
- Modify: `scripts/test-client-frame-benchmark.py`
- Modify: `docs/benchmark-harness.md`

- [ ] **Step 1: Write the failing command-construction test**

Extracting command construction behind a pure helper gives the test a stable seam. Add this test first:

```python
def test_samply_command_requests_presymbolication(self):
    artifact = pathlib.Path("/tmp/profile.json.gz")
    command = MODULE._samply_command(["/tmp/lodestone", "--benchmark", "megaworld"], artifact)
    self.assertEqual(command[:4], ["samply", "record", "--save-only", "--unstable-presymbolicate"])
    self.assertEqual(command[-4:], ["--", "/tmp/lodestone", "--benchmark", "megaworld"])
```

- [ ] **Step 2: Run the focused Python suite and verify RED**

Run: `python3 scripts/test-client-frame-benchmark.py`

Expected: FAIL with `AttributeError: module 'client_frame_benchmark' has no attribute '_samply_command'`.

- [ ] **Step 3: Add the minimal pure helper and use it**

Add beside `_client_command`:

```python
def _samply_command(client: list[str], artifact: pathlib.Path) -> list[str]:
    return [
        "samply",
        "record",
        "--save-only",
        "--unstable-presymbolicate",
        "--output",
        str(artifact),
        "--",
        *client,
    ]
```

Replace the inline list in `run_trial` with:

```python
if samply_artifact is not None:
    command = _samply_command(client, samply_artifact)
else:
    command = client
```

- [ ] **Step 4: Verify GREEN and document the artifact pair**

Run: `python3 scripts/test-client-frame-benchmark.py`

Expected: all tests pass, including `test_samply_command_requests_presymbolication`.

Update `docs/benchmark-harness.md` to say `--samply` emits a `.json.gz` profile
and a sibling `.json.syms.json` sidecar, and show the real baseline artifact:

```bash
python3 scripts/profile-cost-table.py bench-results/profiles/megaworld-closed-20260825-210124.json.gz
```

### Task 2: Make `MapStore` clones copy-on-write snapshots

**Files:**
- Modify: `crates/lodestone-game/src/maps.rs`

- [ ] **Step 1: Write failing structural-sharing and isolation tests**

Add inside `maps.rs`'s existing test module:

```rust
#[test]
fn cloning_a_store_shares_unchanged_map_storage() {
    let mut store = MapStore::default();
    store.apply(&event(7, None, Some(patch(0, 0, 1, 1, 9))));
    let snapshot = store.clone();

    assert!(Arc::ptr_eq(&store.maps, &snapshot.maps));
    assert!(Arc::ptr_eq(
        &store.get(7).unwrap().colors,
        &snapshot.get(7).unwrap().colors,
    ));
}

#[test]
fn a_patch_copies_only_storage_observed_by_an_older_snapshot() {
    let mut store = MapStore::default();
    store.apply(&event(7, None, Some(patch(0, 0, 1, 1, 9))));
    let snapshot = store.clone();

    store.apply(&event(7, None, Some(patch(0, 0, 1, 1, 44))));

    assert_eq!(snapshot.get(7).unwrap().color_at(0, 0), 9);
    assert_eq!(store.get(7).unwrap().color_at(0, 0), 44);
    assert!(!Arc::ptr_eq(&store.maps, &snapshot.maps));
    assert!(!Arc::ptr_eq(
        &store.get(7).unwrap().colors,
        &snapshot.get(7).unwrap().colors,
    ));
}
```

- [ ] **Step 2: Run the focused game test and verify RED**

Run: `RUSTC_WRAPPER= cargo test -p lodestone-game maps::tests::cloning_a_store_shares_unchanged_map_storage -- --exact`

Expected: compilation fails because `MapStore::maps` and `MapState::colors` are not `Arc` values accepted by `Arc::ptr_eq`.

- [ ] **Step 3: Implement copy-on-write storage**

Import `Arc` and change the fields/defaults:

```rust
use std::{collections::BTreeMap, sync::Arc};

pub struct MapState {
    pub scale: i8,
    pub locked: bool,
    pub colors: Arc<Vec<u8>>,
    pub decorations: Vec<MapDecoration>,
}

impl Default for MapState {
    fn default() -> Self {
        Self {
            scale: 0,
            locked: false,
            colors: Arc::new(vec![0; MAP_SIZE * MAP_SIZE]),
            decorations: Vec::new(),
        }
    }
}

pub struct MapStore {
    maps: Arc<BTreeMap<i32, MapState>>,
}
```

In `MapState::apply_patch`, obtain the writable colour vector once before the loops:

```rust
let colors = Arc::make_mut(&mut self.colors);
// existing bounds loops, assigning through `colors[...]`
```

In `MapStore::apply`, obtain the writable tree before `entry`:

```rust
let state = Arc::make_mut(&mut self.maps).entry(*map_id).or_default();
```

`get`, `len`, `is_empty`, and `ids` continue to work through `Arc` deref without API changes.

- [ ] **Step 4: Run the complete maps test module and verify GREEN**

Run: `RUSTC_WRAPPER= cargo test -p lodestone-game maps::tests -- --nocapture`

Expected: all map tests pass, including both new copy-on-write tests.

### Task 3: Carry shared map pixels through the renderer

**Files:**
- Modify: `crates/lodestone-shell/src/gpu/sources.rs`
- Modify: `crates/lodestone-shell/src/gpu/state.rs`
- Modify: `crates/lodestone-shell/src/gpu/maps.rs`
- Modify: `crates/lodestone-shell/src/sim/render_sources.rs`

- [ ] **Step 1: Write a failing shared-payload contract test**

Add a test module at the end of `gpu/sources.rs`. It constructs the source directly,
so no GPU is required:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

#[test]
    fn map_source_reuses_shared_pixels_between_consumers() {
        let pixels = Arc::new(vec![9; lodestone_game::maps::MAP_SIZE.pow(2)]);
        let captured = Arc::clone(&pixels);
        let source = MapSource(Some(Box::new(move |_| Some(Arc::clone(&captured)))));

        let held = source.picture(None).expect("held map picture");
        let framed = source.picture(None).expect("framed map picture");
        assert!(Arc::ptr_eq(&held, &framed));
        assert!(Arc::ptr_eq(&held, &pixels));
    }
}
```

- [ ] **Step 2: Run the focused test and verify RED**

Run: `RUSTC_WRAPPER= cargo test -p lodestone-shell gpu::sources::tests::map_source_reuses_shared_pixels_between_consumers -- --exact`

Expected: compilation fails because the current `MapSource` closure returns
`Option<Vec<u8>>`, not `Option<Arc<Vec<u8>>>`.

- [ ] **Step 3: Change `MapSource` from owned pixels to shared pixels**

Use one type consistently through setter, source, and renderer:

```rust
pub struct MapSource(
    pub(super) Option<
        Box<dyn Fn(Option<i32>) -> Option<Arc<Vec<u8>>> + Send + Sync>,
    >,
);

impl MapSource {
    pub(super) fn picture(&self, id: Option<i32>) -> Option<Arc<Vec<u8>>> {
        self.0.as_ref().and_then(|f| f(id))
    }
}
```

Update `RenderState::set_map_source` to accept the same return type. In
`Sim::map_source`, return:

```rust
Some(Arc::clone(&store.get(id)?.colors))
```

In both map draw paths pass `colors.as_slice()` into `map_texture_bind_group`.

- [ ] **Step 4: Verify the shell and version seam compile**

Run:

```bash
RUSTC_WRAPPER= cargo check -p lodestone-shell --all-targets
RUSTC_WRAPPER= cargo check -p lodestone-shell --no-default-features
```

Expected: both commands exit 0 with no map-source type mismatch.

### Task 4: Do not gather map diagnostics while F3 is closed

**Files:**
- Modify: `crates/lodestone-shell/src/app/redraw.rs`
- Modify: `crates/lodestone-shell/src/app/tests.rs`

- [ ] **Step 1: Write a failing gate test**

Add this pure seam to the test imports only after the test establishes the desired behavior:

```rust
#[test]
fn closed_f3_does_not_call_the_map_debug_gather() {
    let calls = std::cell::Cell::new(0);
    let hidden = super::redraw::map_debug_when_visible(false, || {
        calls.set(calls.get() + 1);
        Some((12, 0.5))
    });
    assert_eq!(hidden, None);
    assert_eq!(calls.get(), 0);

    let visible = super::redraw::map_debug_when_visible(true, || {
        calls.set(calls.get() + 1);
        Some((12, 0.5))
    });
    assert_eq!(visible, Some((12, 0.5)));
    assert_eq!(calls.get(), 1);
}
```

- [ ] **Step 2: Run the exact app test and verify RED**

Run: `RUSTC_WRAPPER= cargo test -p lodestone-shell app::tests::closed_f3_does_not_call_the_map_debug_gather -- --exact`

Expected: compilation fails because `map_debug_when_visible` does not exist.

- [ ] **Step 3: Add the minimal gate and use it at the real call site**

Add near the existing redraw helpers:

```rust
pub(super) fn map_debug_when_visible<T>(
    show_debug: bool,
    gather: impl FnOnce() -> Option<T>,
) -> Option<T> {
    show_debug.then(gather).flatten()
}
```

Replace the unconditional gather with:

```rust
hud_frame.map_debug = map_debug_when_visible(self.show_debug, || self.sim.map_debug());
```

- [ ] **Step 4: Run the exact test and the app test module**

Run:

```bash
RUSTC_WRAPPER= cargo test -p lodestone-shell app::tests::closed_f3_does_not_call_the_map_debug_gather -- --exact
RUSTC_WRAPPER= cargo test -p lodestone-shell app::tests -- --nocapture
```

Expected: the exact test passes; the app module completes with zero failures.

### Task 5: Document, verify, and measure the new bottleneck order

**Files:**
- Modify: `docs/filled-map-item.md`
- Modify: `docs/client-frame-performance-2026-08-25.md`
- Modify: `docs/README.md` (generated)

- [ ] **Step 1: Update feature and performance documentation**

In `docs/filled-map-item.md`, document the copy-on-write `MapStore`, shared renderer payload, mutation cost, and the rule that new consumers clone the snapshot/colour `Arc` rather than the pixel vector.

In `docs/client-frame-performance-2026-08-25.md`, record the presymbolicated baseline shares from the design spec and reserve a before/after table with the actual comparable-run values produced below. Do not compare Samply frame times to non-Samply times.

- [ ] **Step 2: Run focused and workspace checks in the foreground**

Run:

```bash
python3 scripts/test-client-frame-benchmark.py
python3 scripts/test-profile-cost-table.py
RUSTC_WRAPPER= cargo test -p lodestone-game maps::tests -- --nocapture
RUSTC_WRAPPER= cargo check -p lodestone-shell --all-targets
RUSTC_WRAPPER= cargo check -p lodestone-shell --no-default-features
```

Expected: every command exits 0. Report explicitly if the broader shell test suite is not run because of its documented duration.

- [ ] **Step 3: Regenerate and verify the docs index**

Run: `RUSTC_WRAPPER= cargo run -p xtask -- docs-index`

Then run: `RUSTC_WRAPPER= cargo test -p xtask docs_index_matches_committed -- --exact`

Expected: the generator updates `docs/README.md` and the exact gate passes.

- [ ] **Step 4: Run comparable closed/open megaworld trials**

Run in the foreground on the hardware-selected built-in fullscreen display:

```bash
python3 scripts/client-frame-benchmark.py --workload megaworld --trials 3 --debug-overlay closed
python3 scripts/client-frame-benchmark.py --workload megaworld --trials 3 --debug-overlay open
```

Expected: all trials validate fullscreen 3024 x 1898 and append comparable JSONL records. Compare `prepare`, `world.prepare_buffers`, frame percentiles, `world_encode_submit`, GPU timestamps, RSS, and trial spread against the pre-change records at the same display/workload.

- [ ] **Step 5: Record and analyze a new Samply flamegraph**

Run:

```bash
python3 scripts/client-frame-benchmark.py --workload megaworld --debug-overlay closed --samply
PROFILE_PATH=$(ls -t bench-results/profiles/megaworld-closed-*.json.gz | head -1)
python3 scripts/profile-cost-table.py "$PROFILE_PATH" --top 80
```

Expected: the capture has a `.json.syms.json` sidecar; `Sim::maps`, `BTreeMap::clone_subtree`, and `MapState::clone` leave the top table or fall below 1% each. Use the new profile to decide whether the next plan starts with the unified block-entity gather, occlusion-generation churn, relight dirty epochs, or another newly exposed subtree.

- [ ] **Step 6: Commit exact paths only**

Before each pathspec commit, require `git diff --cached --name-only | wc -l` to print `0`. Inspect every named path, then commit only the files changed by this plan using exact file pathspecs; add the two new untracked docs explicitly first. Never stage a directory.
