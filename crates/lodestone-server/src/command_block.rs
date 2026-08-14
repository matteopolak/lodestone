//! Command blocks (`minecraft:command_block`/`chain_command_block`/
//! `repeating_command_block`) — the data model and pure tick semantics ported
//! from `CommandBlockEntity.java`/`BaseCommandBlock.java`/`CommandBlock.java`,
//! the second half of issue #48's remainder.
//!
//! # What this closes, and what it does not
//!
//! Before this module there was **no command-block block entity at all** —
//! `grep CommandBlock crates/lodestone-server/src` found nothing but the wire
//! packet's own field-decoding struct in `crates/protocol/v770`, whose decode
//! arm discards the payload (`server_protocol.rs`'s `SET_COMMAND_BLOCK` arm is
//! `let _ = decode_full::<SetCommandBlock>(payload);`). This module adds
//! [`BlockEntity::CommandBlock`](crate::block_entities::BlockEntity::CommandBlock)
//! (the persistent data — command text, mode, conditional/auto flags, output
//! tracking, success count) and the pure functions vanilla's own tick
//! (`CommandBlock.tick`) and redstone-edge (`CommandBlock.setPoweredAndUpdate`)
//! logic reduce to, each cited against and unit-tested against the decompiled
//! source.
//!
//! **Both hops named above are now wired**, and a third, narrower gap is
//! disclosed below in its place:
//!
//! 1. **The wire decode.** `SET_COMMAND_BLOCK` now decodes into
//!    `ServerBound::SetCommandBlock` (`crates/protocol/v770/src/server_protocol.rs`)
//!    and `crate::server`'s handler applies it: swaps the block's own type to
//!    match the requested mode (preserving `FACING`), writes `conditional`,
//!    updates the entity's command/track-output/"Always Active" fields, and —
//!    matching `CommandBlockEntity.setAutomatic`'s own inline scheduling —
//!    schedules an immediate run via [`on_automatic_changed`] when turning
//!    "Always Active" on while unpowered. `SET_COMMAND_MINECART` is still
//!    decode-only; no command-block-minecart entity exists to write into.
//! 2. **Scheduling into the tick loop.** `tick.rs`'s due-tick drain now has a
//!    [`TICK_COMMAND_BLOCK`] arm, the same shape as
//!    `crate::redstone_dispenser`'s `TICK_DISPENSER_FIRE` arm it was written
//!    to precede: it calls [`tick`], runs the command through a fresh
//!    `crate::commands::ServerCommands` when the decision says to, folds
//!    `condition_met`/`success_count`/`last_execution` back into the entity,
//!    walks any chain behind it via [`next_chain_position`]/
//!    [`chain_link_present`]/[`chain_link_should_run`], and reschedules for
//!    `Auto` mode exactly as [`tick`]'s own `reschedule` field says to.
//!
//! **What is still open, and is a different hop from the two above:**
//! nothing yet calls [`on_power_changed`] from a real redstone signal.
//! `CommandBlock.neighborChanged` would need to reach into `block_entities`
//! from `crate::random_tick::propagate_and_react`, which today only rewrites
//! the block-*state* string (see that function's own dispenser arm) and has
//! no block-entity handle in scope at all — threading one through is real
//! work in its own right, not a leftover of this pass. So today a command
//! block runs from **"Always Active"** (wired, and the path this module's
//! own tests exercise end to end) or from a `ServerCommands`/RCON caller
//! setting it up directly; an ordinary redstone pulse into an impulse
//! (`minecraft:command_block`) or repeating-but-not-automatic block does
//! nothing yet, because nothing yet calls [`on_power_changed`] in production.
//!
//! # Two field names collide with a Rust keyword's neighbour, on purpose
//!
//! `CommandBlockData::auto` is vanilla's `CommandBlockEntity.auto` (the
//! **"Always Active"** toggle on a repeating command block — run every tick
//! with no redstone at all) and is a completely different thing from
//! [`CommandBlockMode::Auto`] (the *block type* — `repeating_command_block` —
//! which vanilla calls `Mode.AUTO`). A repeating command block that is not
//! "Always Active" still needs redstone to run; the mode name and the toggle
//! name coinciding is vanilla's own naming, not introduced here, and every
//! function below takes them as two separate parameters for exactly this
//! reason.

use lodestone_model::BlockPos;
use uuid::Uuid;

use crate::neighbor_update::Direction;
use crate::redstone::{base_name, direction_from_str, direction_to_str, get_bool_property, get_str_property};
use crate::scheduled_tick::{ScheduledTick, ScheduledTickQueue, TickPriority};

pub const COMMAND_BLOCK: &str = "minecraft:command_block";
pub const CHAIN_COMMAND_BLOCK: &str = "minecraft:chain_command_block";
pub const REPEATING_COMMAND_BLOCK: &str = "minecraft:repeating_command_block";

/// The scheduled-tick kind a `tick.rs` drain would dispatch on, mirroring
/// [`crate::redstone_dispenser::TICK_DISPENSER_FIRE`]'s own naming — see this
/// module's doc for why nothing schedules it yet.
pub const TICK_COMMAND_BLOCK: &str = "command:tick";

#[must_use]
pub fn is_command_block_family(state: &str) -> bool {
    matches!(base_name(state), COMMAND_BLOCK | CHAIN_COMMAND_BLOCK | REPEATING_COMMAND_BLOCK)
}

/// `CommandBlockEntity.Mode` — derived from the **block type**, not stored
/// (`CommandBlockEntity.getMode`): a command block's mode changes the instant
/// its block is swapped for one of the other two, with no separate flag to
/// keep in sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandBlockMode {
    /// `minecraft:chain_command_block` — runs only when chained into from a
    /// predecessor, never on its own redstone edge.
    Sequence,
    /// `minecraft:repeating_command_block` — reschedules itself every tick
    /// while powered or "Always Active".
    Auto,
    /// `minecraft:command_block` — runs once per redstone rising edge.
    /// `CommandBlockEntity.getMode`'s own fallback for anything that is
    /// neither of the other two, matched here.
    Redstone,
}

/// `CommandBlockEntity.getMode`.
#[must_use]
pub fn mode_for_block(state: &str) -> CommandBlockMode {
    match base_name(state) {
        CHAIN_COMMAND_BLOCK => CommandBlockMode::Sequence,
        REPEATING_COMMAND_BLOCK => CommandBlockMode::Auto,
        _ => CommandBlockMode::Redstone,
    }
}

/// `CommandBlock.CONDITIONAL` — the `conditional=true/false` block-state
/// property.
#[must_use]
pub fn is_conditional(state: &str) -> bool {
    get_bool_property(state, "conditional").unwrap_or(false)
}

/// `DirectionalBlock.FACING` on a command block.
#[must_use]
pub fn facing(state: &str) -> Direction {
    get_str_property(state, "facing").map(direction_from_str).unwrap_or(Direction::North)
}

/// The persistent state of one command block — `BaseCommandBlock`'s own
/// fields plus `CommandBlockEntity`'s `powered`/`auto`/`conditionMet`, which
/// `saveAdditional`/`loadAdditional` both fold into the same NBT compound
/// vanilla does. Field-for-field against `BaseCommandBlock.java`, not a
/// paraphrase: `lastExecution`/`updateLastExecution` exist because
/// `performCommand` refuses to run twice in the same game tick
/// (`level.getGameTime() == this.lastExecution`), which matters the moment a
/// chain or a redstone pulse could otherwise re-trigger a block already run
/// this tick.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandBlockData {
    pub command: String,
    pub success_count: i32,
    pub track_output: bool,
    pub last_output: Option<String>,
    /// `CommandBlockEntity.powered` — the redstone state as of the last
    /// [`on_power_changed`] call.
    pub powered: bool,
    /// `CommandBlockEntity.auto` — the "Always Active" toggle. See this
    /// module's doc for why this is *not* [`CommandBlockMode::Auto`].
    pub auto: bool,
    /// `CommandBlockEntity.conditionMet` — the result of the most recent
    /// [`mark_condition_met`] call.
    pub condition_met: bool,
    pub update_last_execution: bool,
    /// `BaseCommandBlock.lastExecution` — the game tick this block last ran,
    /// `None` for "never" (vanilla's `-1`).
    pub last_execution: Option<i64>,
}

impl Default for CommandBlockData {
    fn default() -> Self {
        Self {
            command: String::new(),
            success_count: 0,
            track_output: true,
            last_output: None,
            powered: false,
            auto: false,
            condition_met: false,
            update_last_execution: true,
            last_execution: None,
        }
    }
}

impl CommandBlockData {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `BaseCommandBlock.setCommand` — replacing the command always zeroes
    /// the success count, matching the analog-output-signal reset a player
    /// sees immediately after editing one.
    pub fn set_command(&mut self, command: impl Into<String>) {
        self.command = command.into();
        self.success_count = 0;
    }

    /// `BaseCommandBlock.performCommand`'s own same-tick dedup —
    /// `level.getGameTime() == this.lastExecution`. Call *before* actually
    /// running the command; a caller that skips this on a `true` answer
    /// reproduces the one case `performCommand` itself refuses (and reports
    /// as "did not run" to `executeChain`'s own `if (!performCommand())
    /// break`).
    #[must_use]
    pub fn already_ran_this_tick(&self, game_time: i64) -> bool {
        self.last_execution == Some(game_time)
    }

    /// Record that the command was attempted this tick — call after
    /// [`already_ran_this_tick`] answers `false`, regardless of whether the
    /// command itself succeeded (vanilla always advances `lastExecution`,
    /// success or not).
    pub fn record_executed(&mut self, game_time: i64) {
        self.last_execution = if self.update_last_execution { Some(game_time) } else { None };
    }
}

/// `CommandBlockEntity.markConditionMet` — an unconditional block is always
/// "met"; a conditional one inherits its predecessor's last success
/// (`commandBlockEntity.getCommandBlock().getSuccessCount() > 0` on the block
/// directly behind, opposite this block's own facing). `predecessor_succeeded`
/// is `None` when there is no command block immediately behind at all
/// (vanilla's own `!(block instanceof CommandBlock)` branch, which forces
/// `false` rather than skipping the check).
#[must_use]
pub fn mark_condition_met(conditional: bool, predecessor_succeeded: Option<bool>) -> bool {
    if conditional { predecessor_succeeded.unwrap_or(false) } else { true }
}

/// `CommandBlock.setPoweredAndUpdate` — `None` when the redstone state did
/// not actually change (nothing to write). On a rising edge, a block that is
/// "Always Active" or in [`CommandBlockMode::Sequence`] mode is a no-op past
/// the powered-flag update itself: neither ever runs off a plain redstone
/// edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PowerReaction {
    pub new_powered: bool,
    /// Whether this edge should mark the condition met and schedule a 1-tick
    /// execution — vanilla's `markConditionMet()` + `scheduleTick(pos, this, 1)`.
    pub schedule_execution: bool,
}

#[must_use]
pub fn on_power_changed(
    mode: CommandBlockMode,
    was_powered: bool,
    is_powered: bool,
    always_active: bool,
) -> Option<PowerReaction> {
    if is_powered == was_powered {
        return None;
    }
    let schedule_execution =
        is_powered && !always_active && mode != CommandBlockMode::Sequence;
    Some(PowerReaction { new_powered: is_powered, schedule_execution })
}

/// `CommandBlockEntity.setAutomatic` — toggling "Always Active" on, while
/// unpowered and not already active, schedules an immediate run exactly like
/// a rising redstone edge would (unless this is a chain/`Sequence` block,
/// which never self-schedules).
#[must_use]
pub fn on_automatic_changed(mode: CommandBlockMode, was_automatic: bool, automatic: bool, powered: bool) -> bool {
    !was_automatic && automatic && !powered && mode != CommandBlockMode::Sequence
}

/// One [`CommandBlock.tick`] pass, reduced to what it decides rather than
/// what it does — a caller applies [`TickDecision::run`] by actually invoking
/// the command dispatcher (this crate has no dispatcher dependency to call
/// one from; see this module's own doc for why that wiring is not here yet).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TickDecision {
    /// The refreshed `conditionMet` value this tick leaves behind.
    pub condition_met: bool,
    /// Whether the command should actually run this tick.
    pub run: bool,
    /// Whether a conditional block with an unmet condition should have its
    /// success count zeroed (vanilla's `else if (isConditional())
    /// setSuccessCount(0)` — happens even when nothing runs).
    pub zero_success_if_conditional: bool,
    /// `Auto` mode's own self-rescheduling — always `false` for the other two
    /// modes, which never reschedule from inside `tick` itself.
    pub reschedule: bool,
}

/// `CommandBlock.tick`, `Mode::Sequence`'s branch excluded — a chain block
/// never reaches this tick handler on its own; it only ever runs through the
/// `markConditionMet`/`performCommand` pair [`next_chain_position`]/
/// [`chain_link_present`]/[`chain_link_should_run`] walk a caller through,
/// which vanilla inlines directly into `executeChain` rather than routing
/// through `tick`.
///
/// `was_condition_met` is the value **before** this call — `Auto` mode reads
/// it first and only *then* refreshes via [`mark_condition_met`], so a
/// repeating command block's gate is always one reschedule behind its own
/// predecessor's latest success, exactly as `CommandBlock.tick`'s own
/// ordering produces (`boolean wasConditionMet = commandBlock
/// .wasConditionMet(); if (mode == AUTO) { commandBlock.markConditionMet();
/// … }`).
#[must_use]
pub fn tick(
    mode: CommandBlockMode,
    was_condition_met: bool,
    conditional: bool,
    predecessor_succeeded: Option<bool>,
    powered: bool,
    always_active: bool,
) -> TickDecision {
    match mode {
        CommandBlockMode::Auto => {
            let refreshed = mark_condition_met(conditional, predecessor_succeeded);
            TickDecision {
                condition_met: refreshed,
                run: was_condition_met,
                zero_success_if_conditional: !was_condition_met && conditional,
                reschedule: powered || always_active,
            }
        }
        CommandBlockMode::Redstone => TickDecision {
            condition_met: was_condition_met,
            run: was_condition_met,
            zero_success_if_conditional: !was_condition_met && conditional,
            reschedule: false,
        },
        CommandBlockMode::Sequence => TickDecision {
            condition_met: was_condition_met,
            run: false,
            zero_success_if_conditional: false,
            reschedule: false,
        },
    }
}

/// One hop of `CommandBlock.executeChain`'s walk: the next position to check,
/// stepping in `direction` from `from`.
#[must_use]
pub fn next_chain_position(from: BlockPos, direction: Direction) -> BlockPos {
    direction.relative(from)
}

/// Whether `executeChain`'s walk should continue *stepping into* the block at
/// this position at all — `!state.is(CHAIN_COMMAND_BLOCK) ||
/// commandBlock.getMode() != SEQUENCE` breaks the loop outright (a
/// non-chain block, or a chain block somehow not reporting `Sequence` mode,
/// ends the whole chain, not just this link).
#[must_use]
pub fn chain_link_present(state: &str) -> bool {
    base_name(state) == CHAIN_COMMAND_BLOCK && mode_for_block(state) == CommandBlockMode::Sequence
}

/// Whether a present chain link should actually run this pass —
/// `commandBlock.isPowered() || commandBlock.isAutomatic()`. A chain link
/// that is neither stays in the chain (the walk does not `break`) but simply
/// does not fire, matching vanilla's own `if (isPowered || isAutomatic) { … }`
/// with no `else break`.
#[must_use]
pub fn chain_link_should_run(powered: bool, always_active: bool) -> bool {
    powered || always_active
}

/// The wire ordinal `SET_COMMAND_BLOCK`'s `mode` field carries
/// (`CommandBlockEntity.Mode`'s declaration order — `SEQUENCE=0, AUTO=1,
/// REDSTONE=2`, matched by `ServerGamePacketListenerImpl
/// .handleSetCommandBlock`'s own `switch`) to the block base name the
/// packet's handler swaps in, preserving `FACING`. Falls back to
/// [`COMMAND_BLOCK`] for any other ordinal, matching that `switch`'s own
/// `default` arm.
#[must_use]
pub fn base_name_for_mode_ordinal(mode: i32) -> &'static str {
    match mode {
        0 => CHAIN_COMMAND_BLOCK,
        1 => REPEATING_COMMAND_BLOCK,
        _ => COMMAND_BLOCK,
    }
}

/// Builds a command block's block-state string from its two properties —
/// `CommandBlock.createBlockStateDefinition`'s only two (`FACING`,
/// `CONDITIONAL`). Property order is not significant to this crate's state
/// resolver (`crate::redstone::with_property`'s own doc comment); this order
/// matches this module's own test fixtures.
#[must_use]
pub fn state_with(base: &str, facing: Direction, conditional: bool) -> String {
    format!("{base}[conditional={conditional},facing={}]", direction_to_str(facing))
}

/// One [`TICK_COMMAND_BLOCK`] entry at delay `1` —
/// `CommandBlockEntity::scheduleTick`'s own `level.scheduleTick(pos, block,
/// 1)`, called from `setAutomatic`/`onModeSwitch`. Built through a real queue
/// rather than a struct literal because `ScheduledTick::sub_tick_order` is
/// private — the same idiom `crate::fluid::ticks_after_edit`/
/// `crate::gravity_tick::ticks_after_place` already use, for the same reason.
#[must_use]
pub fn ticks_after_schedule(pos: BlockPos) -> Vec<ScheduledTick<String>> {
    let mut pending: ScheduledTickQueue<String> = ScheduledTickQueue::new();
    pending.schedule((pos.x, pos.y, pos.z), TICK_COMMAND_BLOCK.to_owned(), 1, TickPriority::Normal);
    pending.drain_due(u64::MAX, usize::MAX)
}

/// `Direction.toYRot()` — the yaw a command block's `CommandSourceStack`
/// faces, for `^`-relative coordinates in its own command. Vertical
/// directions have no vanilla y-rotation (`Direction.java`'s own
/// `IllegalStateException`); `0.0` is a documented fallback rather than a
/// port of that panic, since a command block can legally face up or down and
/// this crate's source construction must stay total.
#[must_use]
pub fn yaw_for_facing(facing: Direction) -> f32 {
    match facing {
        Direction::North => 180.0,
        Direction::South => 0.0,
        Direction::West => 90.0,
        Direction::East => -90.0,
        Direction::Up | Direction::Down => 0.0,
    }
}

/// The synthetic [`crate::commands::CommandSource`] identity every command
/// block runs as — `CommandBlockEntity`'s own source has no real player
/// behind it (vanilla's `CommandSource.NULL`/a closeable console source), and
/// [`crate::commands::Effect::SetBlock`]/`Fill` still need *some* uuid to
/// self-target (see `crate::commands::block_commands`' own doc comment on
/// why). The nil uuid can never collide with a real player's, which always
/// comes from `lodestone-auth`'s offline/online resolution.
pub const COMMAND_BLOCK_SOURCE_UUID: Uuid = Uuid::nil();

#[cfg(test)]
mod tests {
    use super::*;

    fn state(base: &str, facing: &str, conditional: bool) -> String {
        format!("{base}[conditional={conditional},facing={facing}]")
    }

    #[test]
    fn mode_is_derived_from_the_block_type_not_stored() {
        assert_eq!(mode_for_block(&state(COMMAND_BLOCK, "north", false)), CommandBlockMode::Redstone);
        assert_eq!(mode_for_block(&state(REPEATING_COMMAND_BLOCK, "north", false)), CommandBlockMode::Auto);
        assert_eq!(mode_for_block(&state(CHAIN_COMMAND_BLOCK, "north", false)), CommandBlockMode::Sequence);
    }

    #[test]
    fn is_command_block_family_recognises_exactly_the_three_blocks() {
        assert!(is_command_block_family(&state(COMMAND_BLOCK, "north", false)));
        assert!(is_command_block_family(&state(CHAIN_COMMAND_BLOCK, "north", false)));
        assert!(is_command_block_family(&state(REPEATING_COMMAND_BLOCK, "north", false)));
        assert!(!is_command_block_family("minecraft:stone"));
    }

    #[test]
    fn set_command_zeroes_the_success_count() {
        let mut data = CommandBlockData { success_count: 7, ..CommandBlockData::new() };
        data.set_command("say hi");
        assert_eq!(data.command, "say hi");
        assert_eq!(data.success_count, 0);
    }

    #[test]
    fn already_ran_this_tick_is_exact_game_time_equality() {
        let mut data = CommandBlockData::new();
        assert!(!data.already_ran_this_tick(100));
        data.record_executed(100);
        assert!(data.already_ran_this_tick(100));
        assert!(!data.already_ran_this_tick(101), "a different tick must not dedup");
    }

    #[test]
    fn record_executed_forgets_the_tick_when_update_last_execution_is_off() {
        let mut data = CommandBlockData { update_last_execution: false, ..CommandBlockData::new() };
        data.record_executed(50);
        assert_eq!(data.last_execution, None);
        assert!(!data.already_ran_this_tick(50), "vanilla's own `-1` sentinel never matches a real tick");
    }

    #[test]
    fn an_unconditional_block_is_always_met() {
        assert!(mark_condition_met(false, None));
        assert!(mark_condition_met(false, Some(false)));
    }

    #[test]
    fn a_conditional_block_inherits_its_predecessors_last_success() {
        assert!(mark_condition_met(true, Some(true)));
        assert!(!mark_condition_met(true, Some(false)));
        assert!(!mark_condition_met(true, None), "no predecessor at all reads as unmet, not met");
    }

    #[test]
    fn a_rising_edge_schedules_redstone_and_a_not_always_active_repeating_block_but_never_sequence() {
        let redstone = on_power_changed(CommandBlockMode::Redstone, false, true, false)
            .expect("rising edge");
        assert!(redstone.new_powered);
        assert!(redstone.schedule_execution, "an ordinary impulse block must schedule on its own rising edge");

        // `CommandBlock.setPoweredAndUpdate` only excludes `isAutomatic()`
        // (the "Always Active" toggle) and `Mode.SEQUENCE` — **not**
        // `Mode.AUTO` itself. A repeating block that is not "Always Active"
        // schedules off its own redstone edge exactly like an impulse block;
        // it is only the *toggle*, checked separately in
        // `on_automatic_changed`, that makes it self-sufficient.
        let auto_not_always_active =
            on_power_changed(CommandBlockMode::Auto, false, true, false).expect("rising edge");
        assert!(
            auto_not_always_active.schedule_execution,
            "a repeating block with redstone but not \"Always Active\" must still schedule off the edge"
        );

        let auto_always_active =
            on_power_changed(CommandBlockMode::Auto, false, true, true).expect("rising edge");
        assert!(
            !auto_always_active.schedule_execution,
            "\"Always Active\" already keeps it running every tick — the edge adds nothing"
        );

        let sequence = on_power_changed(CommandBlockMode::Sequence, false, true, false).expect("rising edge");
        assert!(!sequence.schedule_execution, "a chain block never runs off its own redstone edge");
    }

    #[test]
    fn a_falling_edge_never_schedules_regardless_of_mode() {
        let out = on_power_changed(CommandBlockMode::Redstone, true, false, false).expect("falling edge");
        assert!(!out.new_powered);
        assert!(!out.schedule_execution);
    }

    #[test]
    fn a_steady_power_state_is_a_no_op() {
        assert_eq!(on_power_changed(CommandBlockMode::Redstone, false, false, false), None);
        assert_eq!(on_power_changed(CommandBlockMode::Redstone, true, true, false), None);
    }

    #[test]
    fn always_active_suppresses_scheduling_on_the_edge_even_for_an_impulse_block() {
        let out = on_power_changed(CommandBlockMode::Redstone, false, true, true).expect("rising edge");
        assert!(!out.schedule_execution, "an always-active block does not additionally schedule off redstone");
    }

    #[test]
    fn turning_always_active_on_while_unpowered_schedules_immediately() {
        assert!(on_automatic_changed(CommandBlockMode::Auto, false, true, false));
        assert!(!on_automatic_changed(CommandBlockMode::Auto, false, true, true), "already powered: the powered edge already covers it");
        assert!(!on_automatic_changed(CommandBlockMode::Auto, true, true, false), "no transition: already on");
        assert!(!on_automatic_changed(CommandBlockMode::Sequence, false, true, false), "a chain block never self-schedules");
    }

    /// The discriminating property of `Auto` mode's own tick: it reads
    /// `was_condition_met` (set by the *previous* cycle) to decide whether to
    /// run, and only refreshes for the *next* cycle afterward — so a
    /// predecessor's very first success is not observed until the reschedule
    /// after this one.
    #[test]
    fn auto_mode_runs_off_the_previous_cycles_condition_and_refreshes_for_the_next() {
        let out = tick(CommandBlockMode::Auto, true, false, None, true, false);
        assert!(out.run, "unconditional: last cycle's `true` carries through to `run`");
        assert!(out.condition_met, "unconditional refresh is always met");
        assert!(out.reschedule, "powered: auto mode keeps rescheduling");

        // Conditional, predecessor now succeeding, but `was_condition_met`
        // (from the previous cycle) is `false` — this pass does not run, and
        // the refreshed value is what the *next* pass will see.
        let out = tick(CommandBlockMode::Auto, false, true, Some(true), true, false);
        assert!(!out.run, "the stale `false` from last cycle governs whether THIS pass runs");
        assert!(out.condition_met, "but the refreshed value (true) is what the next reschedule sees");
    }

    #[test]
    fn redstone_mode_never_reschedules_and_runs_off_its_own_scheduled_condition() {
        let out = tick(CommandBlockMode::Redstone, true, true, Some(true), false, false);
        assert!(out.run);
        assert!(!out.reschedule, "an impulse block never self-reschedules from `tick`");
        // `condition_met` is passed through unchanged — `tick`'s redstone arm
        // never calls `markConditionMet` again (only the triggering edge did).
        assert!(out.condition_met);
    }

    #[test]
    fn a_conditional_block_with_an_unmet_condition_zeroes_its_success_count() {
        let out = tick(CommandBlockMode::Redstone, false, true, Some(false), false, false);
        assert!(!out.run);
        assert!(out.zero_success_if_conditional);

        // The unconditional control: nothing to zero, because there is no
        // condition to have failed.
        let out = tick(CommandBlockMode::Redstone, false, false, None, false, false);
        assert!(!out.zero_success_if_conditional, "unconditional blocks have nothing to zero");
    }

    #[test]
    fn sequence_mode_never_runs_from_tick_itself() {
        let out = tick(CommandBlockMode::Sequence, true, false, None, true, true);
        assert!(!out.run, "a chain block only ever runs through next_chain_position's own walk, never its own tick");
        assert!(!out.reschedule);
    }

    #[test]
    fn next_chain_position_steps_in_the_given_direction() {
        let origin = BlockPos::new(0, 64, 0);
        assert_eq!(next_chain_position(origin, Direction::North), BlockPos::new(0, 64, -1));
        assert_eq!(next_chain_position(origin, Direction::Up), BlockPos::new(0, 65, 0));
    }

    #[test]
    fn chain_link_present_requires_both_the_block_and_its_own_sequence_mode() {
        assert!(chain_link_present(&state(CHAIN_COMMAND_BLOCK, "north", false)));
        assert!(!chain_link_present(&state(COMMAND_BLOCK, "north", false)), "not a chain block at all");
        assert!(!chain_link_present("minecraft:air"));
    }

    #[test]
    fn chain_link_should_run_needs_power_or_always_active() {
        assert!(chain_link_should_run(true, false));
        assert!(chain_link_should_run(false, true));
        assert!(!chain_link_should_run(false, false));
    }

    #[test]
    fn base_name_for_mode_ordinal_matches_command_block_entity_mode_declaration_order() {
        assert_eq!(base_name_for_mode_ordinal(0), CHAIN_COMMAND_BLOCK, "0 is SEQUENCE");
        assert_eq!(base_name_for_mode_ordinal(1), REPEATING_COMMAND_BLOCK, "1 is AUTO");
        assert_eq!(base_name_for_mode_ordinal(2), COMMAND_BLOCK, "2 is REDSTONE");
        assert_eq!(base_name_for_mode_ordinal(99), COMMAND_BLOCK, "an unknown ordinal falls back like the switch's default");
    }

    #[test]
    fn state_with_round_trips_through_this_modules_own_readers() {
        let built = state_with(REPEATING_COMMAND_BLOCK, Direction::West, true);
        assert_eq!(mode_for_block(&built), CommandBlockMode::Auto);
        assert_eq!(facing(&built), Direction::West);
        assert!(is_conditional(&built));
    }

    #[test]
    fn ticks_after_schedule_is_one_entry_at_delay_one() {
        let ticks = ticks_after_schedule(BlockPos::new(4, 70, -2));
        assert_eq!(ticks.len(), 1);
        assert_eq!(ticks[0].pos, (4, 70, -2));
        assert_eq!(ticks[0].kind, TICK_COMMAND_BLOCK);
        assert_eq!(ticks[0].trigger_tick, 1);
    }

    #[test]
    fn yaw_for_facing_matches_directions_own_to_y_rot() {
        assert_eq!(yaw_for_facing(Direction::North), 180.0);
        assert_eq!(yaw_for_facing(Direction::South), 0.0);
        assert_eq!(yaw_for_facing(Direction::West), 90.0);
        assert_eq!(yaw_for_facing(Direction::East), -90.0);
    }
}
