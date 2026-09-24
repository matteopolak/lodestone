//! Opt-in diagnostics for one local player's movement tick.
//!
//! Set `LODESTONE_PHYSICS_TRACE=1` before launching the client to enable the
//! `lodestone_physics` target. The ECS client wraps only its local-player tick
//! in [`with_local_player`], so the detailed sweep logs do not turn into a
//! server-wide stream when an integrated server is running in the same
//! process. The trace records the input and output pose, every axis in the
//! swept collision order, clipped distances and step candidates.

use std::cell::Cell;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::geometry::{Aabb, Axis, Vec3d};
use crate::player::{MovementInput, PlayerState};

static NEXT_TRACE_ID: AtomicU64 = AtomicU64::new(1);
static TRACE_ENABLED: OnceLock<bool> = OnceLock::new();

thread_local! {
    static ACTIVE_TRACE_ID: Cell<Option<u64>> = const { Cell::new(None) };
}

#[cfg(not(target_arch = "wasm32"))]
fn configured() -> bool {
    *TRACE_ENABLED.get_or_init(|| {
        std::env::var("LODESTONE_PHYSICS_TRACE").is_ok_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
    })
}

#[cfg(target_arch = "wasm32")]
fn configured() -> bool {
    false
}

fn active_id() -> Option<u64> {
    ACTIVE_TRACE_ID.with(Cell::get)
}

/// Runs a local-player tick inside a trace scope when the opt-in environment
/// flag is enabled. The guard restores any enclosing scope, which keeps nested
/// physics calls from changing the correlation id.
pub fn with_local_player<R>(f: impl FnOnce() -> R) -> R {
    if !configured() {
        return f();
    }

    let id = NEXT_TRACE_ID.fetch_add(1, Ordering::Relaxed);
    let previous = ACTIVE_TRACE_ID.with(|slot| slot.replace(Some(id)));
    let result = f();
    ACTIVE_TRACE_ID.with(|slot| slot.set(previous));
    result
}

pub(crate) fn player_tick_start(state: &PlayerState, input: MovementInput) {
    let Some(trace_id) = active_id() else {
        return;
    };
    tracing::debug!(
        target: "lodestone_physics",
        trace_id,
        phase = "tick_start",
        position = ?state.position,
        velocity = ?state.velocity,
        yaw = state.yaw,
        pitch = state.pitch,
        on_ground = state.on_ground,
        horizontal_collision = state.horizontal_collision,
        pose = ?state.pose,
        input = ?input,
    );
}

pub(crate) fn player_tick_end(state: &PlayerState) {
    let Some(trace_id) = active_id() else {
        return;
    };
    tracing::debug!(
        target: "lodestone_physics",
        trace_id,
        phase = "tick_end",
        position = ?state.position,
        velocity = ?state.velocity,
        yaw = state.yaw,
        pitch = state.pitch,
        on_ground = state.on_ground,
        horizontal_collision = state.horizontal_collision,
        pose = ?state.pose,
    );
}

pub(crate) fn entity_move(
    position_before: Vec3d,
    velocity_before: Vec3d,
    on_ground_before: bool,
    horizontal_collision_before: bool,
    bounding_box: Aabb,
    move_delta: Vec3d,
    resolved: Vec3d,
    position_after: Vec3d,
    velocity_after: Vec3d,
    x_collision: bool,
    z_collision: bool,
    vertical_collision: bool,
    vertical_collision_below: bool,
    on_ground_after: bool,
) {
    let Some(trace_id) = active_id() else {
        return;
    };
    tracing::debug!(
        target: "lodestone_physics",
        trace_id,
        phase = "entity_move",
        position_before = ?position_before,
        velocity_before = ?velocity_before,
        on_ground_before,
        horizontal_collision_before,
        bounding_box = ?bounding_box,
        move_delta = ?move_delta,
        resolved = ?resolved,
        position_after = ?position_after,
        velocity_after = ?velocity_after,
        x_collision,
        z_collision,
        vertical_collision,
        vertical_collision_below,
        on_ground_after,
    );
}

pub(crate) fn collision_sweep_start(
    movement: Vec3d,
    bounding_box: Aabb,
    shape_count: usize,
    order: [Axis; 3],
) {
    let Some(trace_id) = active_id() else {
        return;
    };
    tracing::debug!(
        target: "lodestone_physics",
        trace_id,
        phase = "collision_sweep_start",
        movement = ?movement,
        bounding_box = ?bounding_box,
        shape_count,
        axis_order = ?order,
    );
}

pub(crate) fn collision_axis(
    axis: Axis,
    movement: f64,
    moving: Aabb,
    clipped: f64,
) {
    let Some(trace_id) = active_id() else {
        return;
    };
    tracing::debug!(
        target: "lodestone_physics",
        trace_id,
        phase = "collision_axis",
        axis = ?axis,
        movement,
        clipped,
        moving = ?moving,
        blocked = movement != clipped,
    );
}

pub(crate) fn collision_shape_clip(axis: Axis, shape: Aabb, before: f64, after: f64) {
    let Some(trace_id) = active_id() else {
        return;
    };
    tracing::debug!(
        target: "lodestone_physics",
        trace_id,
        phase = "collision_shape_clip",
        axis = ?axis,
        before,
        after,
        shape = ?shape,
    );
}

pub(crate) fn collision_result(
    movement: Vec3d,
    resolved: Vec3d,
    x_collision: bool,
    y_collision: bool,
    z_collision: bool,
    on_ground_after: bool,
    step_height: f32,
) {
    let Some(trace_id) = active_id() else {
        return;
    };
    tracing::debug!(
        target: "lodestone_physics",
        trace_id,
        phase = "collision_result",
        movement = ?movement,
        resolved = ?resolved,
        x_collision,
        y_collision,
        z_collision,
        on_ground_after,
        step_height,
    );
}

pub(crate) fn step_candidate(
    candidate: f32,
    step: Vec3d,
    base: Vec3d,
    accepted: bool,
) {
    let Some(trace_id) = active_id() else {
        return;
    };
    tracing::debug!(
        target: "lodestone_physics",
        trace_id,
        phase = "step_candidate",
        candidate,
        step = ?step,
        base = ?base,
        accepted,
    );
}
