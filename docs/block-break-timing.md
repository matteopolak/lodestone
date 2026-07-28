# Block-break timing

## What it is

How long the shell takes to mine a block, and how fast the crack overlay fills
while it does. The arithmetic lives in `lodestone-game`'s `mining` module
(`BreakInputs`, `Mining`), the per-block data lives in the version crates behind
`VersionAdapter::block_hardness`, and `lodestone-shell`'s `sim.rs` is the join
between them.

Until `a1eb385` there was no per-block data seam, so the shell fed one fake
constant (`LIVE_DIG_HARDNESS = 0.05`) for every block. That is retired. This doc
records what replaced it and the two traps that make the replacement easy to get
wrong in the direction of *breaking too fast*.

## How it works

Once per physics tick, while the attack button is held on a live server,
`Sim::drive_interaction` calls `Sim::drive_mining`:

1. The crosshair target's **block-state id** is read from the client-owned world
   (`NetClient::block_at`) — the live server's id space, not the offline demo
   world's.
2. `Sim::resolve_block_hardness` asks the version adapter for that state's
   `BlockHardness { hardness, requires_correct_tool }`. The adapter is resolved
   once in `Sim::new` via `lodestone_registry::adapter_for_protocol(config.protocol)`,
   so the shell still names no version crate.
3. `bare_hand_break_inputs` turns that census entry plus the player's own state
   into `BreakInputs`.
4. `Mining::continue_` accumulates `progress_per_tick` and emits
   `STOP_DESTROY_BLOCK` on the tick its progress reaches `1.0`.

`Sim::crack_target` reads `Mining::destroy_stage()` (`progress * 10`, or `-1`
when idle) and returns `None` for `-1`, so the renderer draws no overlay.

### Trap 1 — `correct_tool` is not `requires_correct_tool`

`BlockHardness::requires_correct_tool` is `BlockState.requiresCorrectToolForDrops`:
a property of the **block** ("does this drop nothing unless mined with a suitable
tool?"). `BreakInputs::correct_tool` is `Player.hasCorrectToolForDrops`: a
property of the **held item vs. the block**, and it picks vanilla's `30` (correct)
vs `100` (wrong) speed divider.

Bare-handed they are opposites — an empty hand is the correct tool for exactly
those blocks that demand none:

```rust
correct_tool: !entry.requires_correct_tool
```

Assigning the field straight across compiles, reads like faithful data wiring,
and reintroduces the exact defect the seam exists to fix:

| block    | hardness | `requires_correct_tool` | correct (`!req`) | naive (`req`) |
| -------- | -------- | ----------------------- | ---------------- | ------------- |
| stone    | 1.5      | true                    | **151 ticks**    | 45 — 3.4× fast |
| dirt     | 0.5      | false                   | 15 ticks         | 51            |
| obsidian | 50.0     | true                    | 5000 ticks       | 1500          |
| bedrock  | -1.0     | false                   | never            | never         |

`sim.rs`'s `bare_hand_correct_tool_is_the_negation_of_the_blocks_requirement`
pins both columns, so a "simplification" back to the naive form fails.

### Trap 2 — `submerged` is `eye_in_water`, not `under_water()`

Vanilla's `getDestroySpeed` gates the 5×-slower underwater factor on
`isEyeInFluid(WATER)` **alone**. `FluidState::under_water()` is
`eye_in_water && in_water()` — vanilla's `isUnderWater()`, and the predicate the
*fog* selects on (`Sim::fog_settings`). The two agree in nearly every real pose
but are different functions, so mining reads the raw `fluid_state.eye_in_water`
flag and fog keeps `under_water()`. Do not harmonise them.

### 151, not 150

`ticks_to_break` replays vanilla's accumulate-then-compare loop rather than
dividing, so bare-hand stone is **151 ticks (~8.0 s)**, not the textbook 150.
That is f32 accumulation across 150 additions and it is server-confirmed over
RCON. Do not "correct" it. For the same reason, a 5× slower rate does not give
exactly 5× the ticks — assert on `dig_speed()` when you mean the rate.

### Unknown states refuse to dig

`resolve_block_hardness` returns `None` when no version family is compiled in
(the default, version-free build) or the state id is outside the census. Both
cases abort the dig rather than substituting a number. Guessing a hardness here
is how breaking got too fast the first time, so the seam's "reports unknown,
never a guessed number" contract is carried through to the consumer. The v770
table covers all 32,366 real states, so on a vanilla server this never fires.

## The server interaction, and why it changed

This is not just arithmetic — it changes which branch the *server* takes, so it
was measured live rather than reasoned about.

On `STOP_DESTROY_BLOCK` the server (26.2 `ServerPlayerGameMode`) computes
`f1 = getDestroyProgress * (ticksSpentDestroying + 1)`:

- `f1 >= 0.7` → immediate `destroyAndAck`.
- otherwise → set `hasDelayedDestroy` and finish on the server's own subsequent
  ticks once cumulative progress reaches `1.0`.

The old fixed hardness always took the **delayed** branch, which is why break
*times* were right despite the fake number: the server's own timer drove them.
Real hardness moves the `STOP` to the true completion tick, where for bare-hand
stone `f1 ≈ 0.00667 × 151 ≈ 1.01` — clear of the gate — so it takes the
**immediate** branch. Measured back-to-back on one connection, one block
(bare-hand stone on the survival oracle):

| leg                          | STOP tick | STOP at   | air at   | stop → air |
| ---------------------------- | --------- | --------- | -------- | ---------- |
| before (fixed `0.05`)        | 5         | 0.262 s   | 7.448 s  | 7.186 s    |
| after (census stone, 1.5)    | 151       | 7.913 s   | 8.015 s  | 0.103 s    |

Player-visible break time is unchanged (~8 s); only the mechanism moved.
Delayed-destroy survives as the safety net in the other direction.

## How to change it

- **Per-block data** is version-owned: `crates/protocol/v770/src/hardness.rs`,
  generated from a headless server dump. A new version family implements
  `VersionAdapter::block_hardness` and the shell needs no edit.
- **The input builder** is `bare_hand_break_inputs` in
  `crates/lodestone-shell/src/sim.rs`. Both traps above are documented on it and
  pinned by unit tests in the same file.
- **Held tools are not modelled yet.** `tool_speed`, `mining_efficiency`,
  `haste_amplifier`, `mining_fatigue` and `block_break_speed` are left at
  `BreakInputs::default()`, because `minecraft:tool` is not among the modelled
  item components (only `custom_name`, `damage`, `enchantments`). Digging
  therefore always times as an empty hand, even with a diamond pickaxe selected —
  a pickaxe currently makes stone no faster. That is vanilla-correct *for an empty
  hand*, but it means hard, tool-gated blocks are effectively unmineable in
  practice: obsidian is 5000 ticks (~4 min 10 s) of unbroken holding, where a
  diamond pickaxe would be seconds. Closing this needs the `tool`
  component decoded in the version crate first; `tool_inputs_stay_at_bare_hand_defaults`
  is the reminder in `sim.rs`.
- Haste/Mining Fatigue are separately available from `Sim::hud_effects` and could
  be wired ahead of tools, but were left alone here to keep one seam per change.

## Configuration

| knob | where | effect |
| ---- | ----- | ------ |
| `--protocol <n>` | `Config::protocol` | which family's hardness census is resolved |
| `live` feature | `lodestone-shell/Cargo.toml` | compiles a family into the registry at all; without it `resolve_block_hardness` is always `None` and live digging is refused |

## Dependencies

- `lodestone-game::mining` — `BreakInputs`, `Mining`, `destroy_stage`.
- `lodestone-model::{BlockHardness, VersionAdapter}` — the data seam.
- `lodestone-registry::adapter_for_protocol` — the only route to a version crate.
- `lodestone-physics::FluidState` — `eye_in_water`.
- `crates/protocol/v770/src/hardness.rs` — the 32,366-state census.

## Tests

Hermetic (`cargo test -p lodestone-shell --lib sim::tests`): the divider
negation, the submerged/off-ground factors, bedrock drawing no crack, and stage
ordering across dirt/stone/obsidian. With `--features live`,
`the_registry_seam_feeds_the_same_numbers_the_unit_tests_assume` also checks the
hand-written census constants against the real table through the registry.

Live (oracle on `127.0.0.1:25565`, RCON `:25566`):

```text
cargo test -p lodestone-shell --features live --lib \
    sim::tests::live_bare_hand_stone -- --ignored --nocapture

cargo test -p lodestone-game --features live-mining \
    --test live_mining -- --ignored --nocapture
```

The first is the before/after table above. The second is the older end-to-end
gate that the real `Mining` machine's actions break a real block.
