//! Right-click block interaction — vanilla's `BlockBehaviour::useWithoutItem`
//! (issue #532).
//!
//! # What was missing
//!
//! The *redstone-driven* half of these blocks has worked since #319:
//! [`crate::redstone_openable`] opens a door when a lever powers it, and
//! `random_tick::react_to_notification` reaches it from the world tick loop. The
//! **hand** half did not exist at all — `apply_use_item_on` had exactly one
//! block-family guard (`is_bed_block`), so a right-click on a door fell through to
//! the placement branch, found the cell non-replaceable, and returned. A player
//! could not open a door, flip a lever or press a button on our own server.
//!
//! That is worse than it sounds, because it makes the whole of #314/#315/#319
//! unreachable by hand: `redstone.rs` would happily propagate a lever's signal if
//! something set it, and nothing could.
//!
//! # The five families, and where each rule comes from
//!
//! | family | vanilla | rule |
//! |---|---|---|
//! | door | `DoorBlock.useWithoutItem` (`:200-210`) | `state.cycle(OPEN)`, both halves |
//! | trapdoor | `TrapDoorBlock.useWithoutItem` (`:86-95`) | `toggle` → cycle `open` |
//! | fence gate | `FenceGateBlock.useWithoutItem` (`:143-159`) | cycle `open`, and re-face toward the player when opening |
//! | lever | `LeverBlock.useWithoutItem` (`:63-76`) → `pull` | cycle `powered` |
//! | button | `ButtonBlock.useWithoutItem` (`:86-95`) → `press` | set `powered = true`, schedule a release |
//! | note block | `NoteBlock.useWithoutItem` (`NoteBlock.java:126`) → `changePitch` | cycle `note`, wrapping `24` back to `0` |
//!
//! **`open` alone, not `open` *and* `powered`.** [`crate::redstone_openable`]'s
//! `with_open_and_powered` writes both, because for the redstone path they move
//! together by definition — `powered` *is* the input. A hand click must leave
//! `powered` alone, or opening a door by hand would make it read as
//! redstone-powered and the next neighbour notification would slam it shut.
//!
//! **Iron cannot be opened by hand.** `BlockSetType`'s first boolean field is
//! `canOpenByHand`, and it is `false` for exactly two sets, `iron` and `gold`
//! (`BlockSetType.java:11-13` for the field order; the `false` values are on the
//! `iron` and `gold` registrations). Only iron has a door and a trapdoor, so
//! `minecraft:iron_door` and `minecraft:iron_trapdoor` are the two blocks a hand
//! click must refuse. Copper doors *can* be opened by hand — copper's own
//! `canOpenByHand` is `true`, which is easy to get backwards because copper is a
//! metal and its `canButtonBeActivatedByArrows` is the `false` one.
//!
//! # What is deliberately not modelled
//!
//! * **The open/close sound.** It is a `LEVEL_EVENT`, and `ServerProtocol` has no
//!   encoder for one at all. Named in #532's own scope list; the block state is
//!   what makes the door usable, the sound is cosmetic.
//! * **Pressure plates.** A collision trigger, not a right-click — it needs the
//!   player-AABB-vs-block work #532 puts out of scope.
//! * **`GameEvent.BLOCK_OPEN`/`BLOCK_CLOSE`.** Sculk sensors are not modelled.
//! * **Vanilla's `isClientSide` split in `LeverBlock`.** The particle half is the
//!   client's; the `pull` half is what a server does, and that is this.

use lodestone_model::BlockPos;

use crate::redstone::with_property;

/// The scheduled-tick kind a pressed button schedules to release itself, in the
/// same `String`-keyed space `crate::redstone`'s `TICK_TORCH`/`TICK_REPEATER`
/// already use. `tick::run_tick_loop`'s drain dispatches on it.
pub const TICK_BUTTON: &str = "lodestone:button_release";

/// `ButtonBlock`'s `ticksToStayPressed` for the stone family — `Blocks.java`'s
/// `new ButtonBlock(BlockSetType.STONE, 20, p)`. Also the value for
/// `polished_blackstone_button`, which registers against the **stone** set.
pub const STONE_BUTTON_TICKS: u64 = 20;

/// `ButtonBlock`'s `ticksToStayPressed` for every wooden family (oak, spruce,
/// birch, jungle, acacia, cherry, dark_oak, pale_oak, mangrove, bamboo, crimson,
/// warped) — all registered at `30`.
pub const WOODEN_BUTTON_TICKS: u64 = 30;

/// What a right-click does to a block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandUse {
    /// Every `(position, new state)` the click rewrites. One entry for a
    /// trapdoor/gate/lever/button, **two** for a door (both halves move together).
    pub changes: Vec<(BlockPos, String)>,
    /// A delay in ticks after which `TICK_BUTTON` should fire at `pos` to release
    /// a pressed button, or `None` for the other four families.
    pub release_after: Option<u64>,
}

/// Whether `state` is a block this module knows how to right-click.
///
/// Cheap and total, so `apply_use_item_on` can ask before doing any work.
#[must_use]
pub fn is_hand_usable(state: &str) -> bool {
    crate::redstone_openable::is_openable(state)
        || is_lever(state)
        || is_button(state)
        || is_note_block(state)
}

/// `minecraft:note_block`.
#[must_use]
pub fn is_note_block(state: &str) -> bool {
    base(state) == crate::redstone_note_block::NOTE_BLOCK
}

/// `minecraft:lever`.
#[must_use]
pub fn is_lever(state: &str) -> bool {
    base(state) == "minecraft:lever"
}

/// Any of the fourteen button blocks.
#[must_use]
pub fn is_button(state: &str) -> bool {
    base(state).strip_suffix("_button").is_some_and(|rest| {
        rest.strip_prefix("minecraft:")
            .is_some_and(|name| !name.is_empty())
    })
}

/// `ticksToStayPressed` for a button state — 20 for stone and polished
/// blackstone, 30 for every wooden family.
#[must_use]
pub fn button_release_delay(state: &str) -> u64 {
    match base(state) {
        "minecraft:stone_button" | "minecraft:polished_blackstone_button" => STONE_BUTTON_TICKS,
        _ => WOODEN_BUTTON_TICKS,
    }
}

/// Whether a hand click may open this block — vanilla's
/// `BlockSetType.canOpenByHand`, which is `false` only for the `iron` and `gold`
/// sets. Gold has neither a door nor a trapdoor, so these two names are the whole
/// refusal set.
#[must_use]
pub fn can_open_by_hand(state: &str) -> bool {
    !matches!(base(state), "minecraft:iron_door" | "minecraft:iron_trapdoor")
}

/// Resolves one right-click into the state changes it produces, or `None` when
/// the click does nothing (an unrecognised block, an iron door, or a button that
/// is already pressed).
///
/// `pos` is the clicked block and `state` its current state. `other_half` is the
/// door's other half as `(position, state)` when there is one — the caller reads
/// it, because this function has no world access; `None` for every other family
/// and for a door whose partner is missing.
///
/// `player_yaw` is used only by the fence-gate arm, to re-face the gate toward the
/// player as vanilla does. `None` (no rotation reported yet) keeps the existing
/// facing, which is the same fallback `placed_block_state` uses.
#[must_use]
pub fn hand_use(
    pos: BlockPos,
    state: &str,
    other_half: Option<(BlockPos, String)>,
    player_yaw: Option<f32>,
) -> Option<HandUse> {
    if is_button(state) {
        // `ButtonBlock.useWithoutItem` returns CONSUME without pressing when the
        // button is already down, so a second click neither re-powers it nor
        // extends its timer.
        if crate::redstone_openable::powered(state) {
            return None;
        }
        return Some(HandUse {
            changes: vec![(pos, with_property(state, "powered", "true"))],
            release_after: Some(button_release_delay(state)),
        });
    }
    if is_lever(state) {
        let now = crate::redstone_openable::powered(state);
        return Some(HandUse {
            changes: vec![(pos, with_property(state, "powered", if now { "false" } else { "true" }))],
            release_after: None,
        });
    }
    if is_note_block(state) {
        // `NoteBlock.useWithoutItem` → `changePitch`: `state.cycle(NOTE)`, wrapping
        // 24 back to 0. The `playNote` half of `changePitch` is not modelled — see
        // `crate::redstone_note_block`'s own module doc for why the pulse sound has
        // no wire path here yet (the same `LEVEL_EVENT`/block-event gap its
        // neighbour-triggered pulse already discloses).
        return crate::redstone_note_block::cycle_note(state)
            .map(|new_state| HandUse { changes: vec![(pos, new_state)], release_after: None });
    }
    if !crate::redstone_openable::is_openable(state) || !can_open_by_hand(state) {
        return None;
    }

    let opening = !is_open(state);
    let mut changes = Vec::with_capacity(2);
    let mut primary = with_property(state, "open", if opening { "true" } else { "false" });
    if crate::redstone_openable::is_fence_gate(state) && opening {
        // `FenceGateBlock.useWithoutItem`'s middle branch: when the gate is
        // opening and its `facing` is the *opposite* of where the player is
        // looking, it swings to face them instead. Skipped when the yaw is
        // unknown, and a no-op when the facings already agree.
        if let Some(yaw) = player_yaw {
            let player_facing = facing_from_yaw(yaw);
            if let Some(current) = property(state, "facing")
                && current == opposite_facing(player_facing)
            {
                primary = with_property(&primary, "facing", player_facing);
            }
        }
    }
    changes.push((pos, primary));

    // A door is two blocks and vanilla moves both: `DoorBlock.setOpen` writes the
    // clicked half, and the other half follows because both halves carry `open`
    // and `DoorBlock.neighborChanged` keeps them in step. Writing only the clicked
    // half leaves a visibly half-open door.
    if crate::redstone_openable::is_door(state)
        && let Some((other_pos, other_state)) = other_half
    {
        changes.push((
            other_pos,
            with_property(&other_state, "open", if opening { "true" } else { "false" }),
        ));
    }

    Some(HandUse {
        changes,
        release_after: None,
    })
}

/// The state a released button returns to — `powered=false`. `tick::run_tick_loop`
/// calls this when a [`TICK_BUTTON`] entry comes due.
///
/// `None` when the button is already unpowered, so a duplicate scheduled tick
/// cannot publish a redundant block update (the same `Option` shape every
/// `run_scheduled_tick` in the redstone families uses).
#[must_use]
pub fn release_button(state: &str) -> Option<String> {
    if !is_button(state) || !crate::redstone_openable::powered(state) {
        return None;
    }
    Some(with_property(state, "powered", "false"))
}

/// `open=true` for a state carrying the property, `false` when it is absent (a
/// bare `minecraft:oak_door` is the default state, which is closed).
fn is_open(state: &str) -> bool {
    property(state, "open") == Some("true")
}

fn base(state: &str) -> &str {
    state.split('[').next().unwrap_or(state)
}

/// Reads one `key=value` out of a block-state string's property list.
fn property<'a>(state: &'a str, key: &str) -> Option<&'a str> {
    let props = state.split_once('[')?.1.strip_suffix(']')?;
    props
        .split(',')
        .filter_map(|pair| pair.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v)
}

/// The horizontal `Direction` a yaw points at, matching vanilla's
/// `Direction.fromYRot` — the same quadrant arithmetic `placed_block_state`
/// already uses for the redstone directional families.
fn facing_from_yaw(yaw: f32) -> &'static str {
    match (((yaw / 90.0) + 0.5).floor() as i32).rem_euclid(4) {
        0 => "south",
        1 => "west",
        2 => "north",
        _ => "east",
    }
}

fn opposite_facing(facing: &str) -> &'static str {
    match facing {
        "north" => "south",
        "south" => "north",
        "east" => "west",
        _ => "east",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(x: i32, y: i32, z: i32) -> BlockPos {
        BlockPos::new(x, y, z)
    }

    #[test]
    fn a_click_opens_a_closed_door_and_leaves_powered_alone() {
        let lower = "minecraft:oak_door[half=lower,open=false,powered=false]";
        let out = hand_use(
            pos(1, 64, 1),
            lower,
            Some((
                pos(1, 65, 1),
                "minecraft:oak_door[half=upper,open=false,powered=false]".to_string(),
            )),
            None,
        )
        .expect("a wooden door opens by hand");

        assert_eq!(out.changes.len(), 2, "both halves move together");
        assert!(out.changes[0].1.contains("open=true"));
        assert!(out.changes[1].1.contains("open=true"));
        // The load-bearing half: `powered` must not have moved, or the next
        // neighbour notification reads the door as redstone-powered and shuts it.
        assert!(out.changes[0].1.contains("powered=false"));
        assert!(out.changes[1].1.contains("powered=false"));
        assert_eq!(out.release_after, None);
    }

    #[test]
    fn a_second_click_closes_it() {
        let open = "minecraft:oak_door[half=lower,open=true,powered=false]";
        let out = hand_use(pos(1, 64, 1), open, None, None).expect("closes");
        assert!(out.changes[0].1.contains("open=false"));
    }

    #[test]
    fn an_iron_door_refuses_a_hand_click_and_a_copper_one_accepts() {
        assert_eq!(
            hand_use(pos(0, 0, 0), "minecraft:iron_door[open=false]", None, None),
            None,
            "BlockSetType.IRON has canOpenByHand = false"
        );
        assert_eq!(
            hand_use(pos(0, 0, 0), "minecraft:iron_trapdoor[open=false]", None, None),
            None
        );
        assert!(
            hand_use(pos(0, 0, 0), "minecraft:copper_door[open=false]", None, None).is_some(),
            "BlockSetType.COPPER has canOpenByHand = true — its `false` field is \
             canButtonBeActivatedByArrows"
        );
    }

    #[test]
    fn a_lever_cycles_powered_both_ways() {
        let off = "minecraft:lever[face=wall,facing=north,powered=false]";
        let on = hand_use(pos(0, 0, 0), off, None, None).expect("flips");
        assert!(on.changes[0].1.contains("powered=true"));
        let back = hand_use(pos(0, 0, 0), &on.changes[0].1, None, None).expect("flips back");
        assert!(back.changes[0].1.contains("powered=false"));
    }

    #[test]
    fn a_button_presses_once_and_schedules_its_own_release() {
        let up = "minecraft:stone_button[face=wall,facing=north,powered=false]";
        let pressed = hand_use(pos(0, 0, 0), up, None, None).expect("presses");
        assert!(pressed.changes[0].1.contains("powered=true"));
        assert_eq!(
            pressed.release_after,
            Some(STONE_BUTTON_TICKS),
            "a stone button stays pressed 20 ticks"
        );

        // Already pressed: vanilla returns CONSUME without pressing, so the timer
        // is not extended and no block update is produced.
        assert_eq!(hand_use(pos(0, 0, 0), &pressed.changes[0].1, None, None), None);

        let wooden = "minecraft:oak_button[face=wall,facing=north,powered=false]";
        assert_eq!(
            hand_use(pos(0, 0, 0), wooden, None, None)
                .expect("presses")
                .release_after,
            Some(WOODEN_BUTTON_TICKS),
            "a wooden button stays pressed 30 ticks"
        );
    }

    #[test]
    fn release_button_returns_a_pressed_button_and_nothing_else() {
        assert_eq!(
            release_button("minecraft:stone_button[powered=true]").as_deref(),
            Some("minecraft:stone_button[powered=false]")
        );
        assert_eq!(release_button("minecraft:stone_button[powered=false]"), None);
        assert_eq!(release_button("minecraft:lever[powered=true]"), None);
    }

    #[test]
    fn an_opening_fence_gate_swings_to_face_the_player() {
        // Gate faces north; a player looking north is at yaw 180 in Minecraft's
        // convention, so the gate's facing is the opposite of the player's and must
        // swing.
        let gate = "minecraft:oak_fence_gate[facing=north,in_wall=false,open=false,powered=false]";
        let out = hand_use(pos(0, 0, 0), gate, None, Some(0.0)).expect("opens");
        assert!(out.changes[0].1.contains("open=true"));
        assert!(
            out.changes[0].1.contains("facing=south"),
            "a yaw of 0 faces south, and the north-facing gate is its opposite, so it \
             swings: {}",
            out.changes[0].1
        );

        // Unknown yaw keeps the facing rather than guessing.
        let out = hand_use(pos(0, 0, 0), gate, None, None).expect("opens");
        assert!(out.changes[0].1.contains("facing=north"));
    }

    #[test]
    fn closing_a_fence_gate_never_re_faces_it() {
        let gate = "minecraft:oak_fence_gate[facing=north,in_wall=false,open=true,powered=false]";
        let out = hand_use(pos(0, 0, 0), gate, None, Some(0.0)).expect("closes");
        assert!(out.changes[0].1.contains("open=false"));
        assert!(
            out.changes[0].1.contains("facing=north"),
            "vanilla's re-face is inside the opening branch only"
        );
    }

    #[test]
    fn is_hand_usable_covers_the_six_families_and_nothing_else() {
        for yes in [
            "minecraft:oak_door",
            "minecraft:oak_trapdoor",
            "minecraft:oak_fence_gate",
            "minecraft:lever",
            "minecraft:stone_button",
            "minecraft:polished_blackstone_button",
            "minecraft:note_block",
        ] {
            assert!(is_hand_usable(yes), "{yes} must be hand-usable");
        }
        for no in [
            "minecraft:stone",
            "minecraft:air",
            "minecraft:redstone_wire",
            "minecraft:chest",
            "minecraft:oak_planks",
        ] {
            assert!(!is_hand_usable(no), "{no} must not be");
        }
    }

    /// A right-click on a note block advances its pitch by one semitone and
    /// wraps `24` back to `0`, with no `release_after` (unlike a button) and
    /// no second `changes` entry (unlike a door).
    #[test]
    fn a_note_block_click_cycles_its_pitch_and_wraps() {
        let out = hand_use(
            pos(0, 0, 0),
            "minecraft:note_block[instrument=harp,note=0,powered=false]",
            None,
            None,
        )
        .expect("cycles");
        assert_eq!(out.changes.len(), 1);
        assert!(out.changes[0].1.contains("note=1"), "{}", out.changes[0].1);
        assert_eq!(out.release_after, None);

        let out = hand_use(
            pos(0, 0, 0),
            "minecraft:note_block[instrument=harp,note=24,powered=false]",
            None,
            None,
        )
        .expect("wraps");
        assert!(
            out.changes[0].1.contains("note=0"),
            "24 must wrap back to 0: {}",
            out.changes[0].1
        );
    }

    #[test]
    fn button_delay_table_matches_the_jar_registrations() {
        assert_eq!(button_release_delay("minecraft:stone_button"), 20);
        assert_eq!(button_release_delay("minecraft:polished_blackstone_button"), 20);
        for wooden in [
            "minecraft:oak_button",
            "minecraft:spruce_button",
            "minecraft:birch_button",
            "minecraft:jungle_button",
            "minecraft:acacia_button",
            "minecraft:cherry_button",
            "minecraft:dark_oak_button",
            "minecraft:pale_oak_button",
            "minecraft:mangrove_button",
            "minecraft:bamboo_button",
            "minecraft:crimson_button",
            "minecraft:warped_button",
        ] {
            assert_eq!(button_release_delay(wooden), 30, "{wooden}");
        }
    }
}
