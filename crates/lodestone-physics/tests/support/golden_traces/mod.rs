//! AUTO-GENERATED golden movement traces. DO NOT EDIT.
//!
//! Produced by `gen_golden.py`, an independent Python oracle (see that
//! file's header). Each value is an `f64` bit pattern so the Rust test
//! asserts bit-for-bit equality with no decimal-parsing rounding.

/// One tick of ground truth: position then velocity, as raw `f64` bits.
#[derive(Debug)]
pub struct GoldenTick {
    /// Position `(x, y, z)` bits.
    pub pos: [u64; 3],
    /// Velocity `(x, y, z)` bits.
    pub vel: [u64; 3],
}

mod free_fall;
mod walk_flat;
mod walk_speed_ii;
mod sprint_jump;
mod ice_slide;
mod walk_into_wall;
mod slab_step;
mod water_sink;
mod diagonal_walk;
mod analog_strafe;
mod ladder_climb;
mod ladder_sneak_hold;
mod blue_ice_slide;
mod lava_sink;
mod lava_shallow;
mod levitation;
mod slow_falling_water;
mod swim_sprint;
mod swim_look_down_dives;
mod swim_surface_look_up_no_pulldown;
mod swim_surface_look_down_control;
mod soul_sand_walk;
mod jump_boost;
mod honey_jump;
mod slime_bounce;
mod slime_bounce_sneak;
mod elytra_glide_level;
mod elytra_dive;
mod elytra_climb;
mod elytra_diagonal_yaw;
mod water_current_push;
mod sneak_edge_stop;
mod sneak_edge_walk_off;
mod sneak_edge_diagonal;
mod entity_push_shove;
mod entity_push_wide_plateau;
mod entity_push_flush_control;
mod swim_gap_tunnel;
mod swim_gap_blocked_control;
mod crouch_low_corridor;
mod stand_low_corridor_control;
mod crouch_release_stays_crouched;
mod elytra_gap_glide;
mod bubble_column_up;
mod bubble_column_water_control;
mod bubble_column_down;
mod bubble_column_surface_launch;

pub use free_fall::GOLDEN_FREE_FALL;
pub use walk_flat::GOLDEN_WALK_FLAT;
pub use walk_speed_ii::GOLDEN_WALK_SPEED_II;
pub use sprint_jump::GOLDEN_SPRINT_JUMP;
pub use ice_slide::GOLDEN_ICE_SLIDE;
pub use walk_into_wall::GOLDEN_WALK_INTO_WALL;
pub use slab_step::GOLDEN_SLAB_STEP;
pub use water_sink::GOLDEN_WATER_SINK;
pub use diagonal_walk::GOLDEN_DIAGONAL_WALK;
pub use analog_strafe::GOLDEN_ANALOG_STRAFE;
pub use ladder_climb::GOLDEN_LADDER_CLIMB;
pub use ladder_sneak_hold::GOLDEN_LADDER_SNEAK_HOLD;
pub use blue_ice_slide::GOLDEN_BLUE_ICE_SLIDE;
pub use lava_sink::GOLDEN_LAVA_SINK;
pub use lava_shallow::GOLDEN_LAVA_SHALLOW;
pub use levitation::GOLDEN_LEVITATION;
pub use slow_falling_water::GOLDEN_SLOW_FALLING_WATER;
pub use swim_sprint::GOLDEN_SWIM_SPRINT;
pub use swim_look_down_dives::GOLDEN_SWIM_LOOK_DOWN_DIVES;
pub use swim_surface_look_up_no_pulldown::GOLDEN_SWIM_SURFACE_LOOK_UP_NO_PULLDOWN;
pub use swim_surface_look_down_control::GOLDEN_SWIM_SURFACE_LOOK_DOWN_CONTROL;
pub use soul_sand_walk::GOLDEN_SOUL_SAND_WALK;
pub use jump_boost::GOLDEN_JUMP_BOOST;
pub use honey_jump::GOLDEN_HONEY_JUMP;
pub use slime_bounce::GOLDEN_SLIME_BOUNCE;
pub use slime_bounce_sneak::GOLDEN_SLIME_BOUNCE_SNEAK;
pub use elytra_glide_level::GOLDEN_ELYTRA_GLIDE_LEVEL;
pub use elytra_dive::GOLDEN_ELYTRA_DIVE;
pub use elytra_climb::GOLDEN_ELYTRA_CLIMB;
pub use elytra_diagonal_yaw::GOLDEN_ELYTRA_DIAGONAL_YAW;
pub use water_current_push::GOLDEN_WATER_CURRENT_PUSH;
pub use sneak_edge_stop::GOLDEN_SNEAK_EDGE_STOP;
pub use sneak_edge_walk_off::GOLDEN_SNEAK_EDGE_WALK_OFF;
pub use sneak_edge_diagonal::GOLDEN_SNEAK_EDGE_DIAGONAL;
pub use entity_push_shove::GOLDEN_ENTITY_PUSH_SHOVE;
pub use entity_push_wide_plateau::GOLDEN_ENTITY_PUSH_WIDE_PLATEAU;
pub use entity_push_flush_control::GOLDEN_ENTITY_PUSH_FLUSH_CONTROL;
pub use swim_gap_tunnel::GOLDEN_SWIM_GAP_TUNNEL;
pub use swim_gap_blocked_control::GOLDEN_SWIM_GAP_BLOCKED_CONTROL;
pub use crouch_low_corridor::GOLDEN_CROUCH_LOW_CORRIDOR;
pub use stand_low_corridor_control::GOLDEN_STAND_LOW_CORRIDOR_CONTROL;
pub use crouch_release_stays_crouched::GOLDEN_CROUCH_RELEASE_STAYS_CROUCHED;
pub use elytra_gap_glide::GOLDEN_ELYTRA_GAP_GLIDE;
pub use bubble_column_up::GOLDEN_BUBBLE_COLUMN_UP;
pub use bubble_column_water_control::GOLDEN_BUBBLE_COLUMN_WATER_CONTROL;
pub use bubble_column_down::GOLDEN_BUBBLE_COLUMN_DOWN;
pub use bubble_column_surface_launch::GOLDEN_BUBBLE_COLUMN_SURFACE_LAUNCH;
