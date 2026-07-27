//! Input model, kept as pure data + pure functions so the mapping from held
//! keys to a physics [`MovementInput`] and the mouse-look integration are unit
//! testable without a window — and reusable unchanged from the browser.
//!
//! The platform layer (winit in `lodestone-shell`, web-sys in the browser
//! `web/`) only *feeds* this: it maps a physical key to an [`Action`] on
//! press/release and accumulates raw mouse motion. Every decision about what
//! those mean lives here, so both platforms share one implementation.

use lodestone_physics::MovementInput;

/// Logical actions the controller cares about, decoupled from physical key codes
/// so each platform maps its own key type → this and everything below is shared
/// and testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    /// Walk forward (W).
    Forward,
    /// Walk backward (S).
    Back,
    /// Strafe left (A).
    Left,
    /// Strafe right (D).
    Right,
    /// Jump / swim up (Space).
    Jump,
    /// Sneak / descend (Shift).
    Sneak,
    /// Sprint (Ctrl).
    Sprint,
}

/// The set of currently-held actions plus accumulated, not-yet-consumed mouse
/// motion. Cheap to copy; the platform layer owns one.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct InputState {
    forward: bool,
    back: bool,
    left: bool,
    right: bool,
    jump: bool,
    sneak: bool,
    sprint: bool,
    /// Accumulated horizontal mouse delta in pixels since last consume.
    pub mouse_dx: f32,
    /// Accumulated vertical mouse delta in pixels since last consume.
    pub mouse_dy: f32,
}

impl InputState {
    /// Set or clear a held action.
    pub fn set(&mut self, action: Action, held: bool) {
        match action {
            Action::Forward => self.forward = held,
            Action::Back => self.back = held,
            Action::Left => self.left = held,
            Action::Right => self.right = held,
            Action::Jump => self.jump = held,
            Action::Sneak => self.sneak = held,
            Action::Sprint => self.sprint = held,
        }
    }

    /// Accumulate a raw mouse-motion delta (device pixels).
    pub fn add_mouse(&mut self, dx: f32, dy: f32) {
        self.mouse_dx += dx;
        self.mouse_dy += dy;
    }

    /// Take and clear the accumulated mouse motion.
    pub fn take_mouse(&mut self) -> (f32, f32) {
        let out = (self.mouse_dx, self.mouse_dy);
        self.mouse_dx = 0.0;
        self.mouse_dy = 0.0;
        out
    }

    /// Clear all held actions (used when the cursor is released / window loses
    /// focus, so the player doesn't keep walking).
    pub fn release_all(&mut self) {
        let mouse = (self.mouse_dx, self.mouse_dy);
        *self = InputState::default();
        self.mouse_dx = mouse.0;
        self.mouse_dy = mouse.1;
    }

    /// Whether any movement key is held (used to gate sprint etc.).
    #[must_use]
    pub fn any_move(&self) -> bool {
        self.forward || self.back || self.left || self.right
    }

    /// Whether the sprint key is held (raw, ungated — used by free-fly which
    /// isn't subject to the forward-only sprint gate that walking uses).
    #[must_use]
    pub fn sprint_held(&self) -> bool {
        self.sprint
    }
}

/// Map the held-key state to the physics engine's [`MovementInput`].
///
/// Conventions match vanilla's `Input`: `forward` is +1 with W, `strafe` is +1
/// with **left** (A) — the engine's `input_vector` treats +strafe as left, so we
/// must not flip it here. Sprinting only applies while actually moving forward,
/// mirroring `LocalPlayer.aiStep` gating (you can't sprint standing still or
/// while sneaking).
#[must_use]
pub fn movement_intent(state: &InputState) -> MovementInput {
    let forward = f32::from(state.forward) - f32::from(state.back);
    let strafe = f32::from(state.left) - f32::from(state.right);
    let sprint = state.sprint && state.forward && !state.back && !state.sneak;
    MovementInput {
        forward,
        strafe,
        jump: state.jump,
        sneak: state.sneak,
        sprint,
    }
}

/// Pitch is clamped to just under straight up/down, exactly like vanilla
/// (`Mth.clamp(pitch, -90, 90)`), so the camera can never flip over.
pub const PITCH_LIMIT: f32 = 89.999;

/// Vanilla's mouse-sensitivity response curve.
///
/// `MouseHandler.turnPlayer` computes `f = sensitivity·0.6 + 0.2` then
/// `f·f·f·8.0`, and `Entity.turn` multiplies the resulting pixel deltas by
/// `0.15` to get degrees. Folding those together, the degrees-per-pixel factor
/// is `(s·0.6 + 0.2)³ · 8 · 0.15`. The `sensitivity` slider is `0..1`; the
/// vanilla default of `0.5` yields exactly `0.15°`/pixel. The curve is cubic on
/// purpose — it makes low settings fine and high settings fast, and a shell that
/// used a flat multiplier would feel wrong to anyone used to Minecraft.
#[must_use]
pub fn sensitivity_factor(slider: f32) -> f32 {
    let f = slider * 0.6 + 0.2;
    f * f * f * 8.0 * 0.15
}

/// Integrate a mouse delta into yaw/pitch (degrees), returning the new pair.
///
/// * Moving the mouse right increases yaw (turns the view right). In Minecraft's
///   convention yaw increases clockwise from south, and turning right *is* an
///   increase, so `+dx → +yaw`.
/// * Moving the mouse down increases pitch (positive pitch looks down), matching
///   `lodestone_render::Camera`'s "positive pitch looks down".
/// * The raw pixel deltas pass through the vanilla [`sensitivity_factor`] curve.
/// * Yaw wraps to `[-180, 180)`; pitch is clamped to [`PITCH_LIMIT`].
#[must_use]
pub fn apply_look(yaw: f32, pitch: f32, dx: f32, dy: f32, sensitivity: f32) -> (f32, f32) {
    let factor = sensitivity_factor(sensitivity);
    let mut yaw = yaw + dx * factor;
    let mut pitch = pitch + dy * factor;
    // Wrap yaw into [-180, 180).
    yaw = (yaw + 180.0).rem_euclid(360.0) - 180.0;
    pitch = pitch.clamp(-PITCH_LIMIT, PITCH_LIMIT);
    (yaw, pitch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forward_and_back_cancel() {
        let mut s = InputState::default();
        s.set(Action::Forward, true);
        s.set(Action::Back, true);
        let m = movement_intent(&s);
        assert_eq!(m.forward, 0.0);
    }

    #[test]
    fn left_is_positive_strafe() {
        let mut s = InputState::default();
        s.set(Action::Left, true);
        assert_eq!(movement_intent(&s).strafe, 1.0);
        s.set(Action::Left, false);
        s.set(Action::Right, true);
        assert_eq!(movement_intent(&s).strafe, -1.0);
    }

    #[test]
    fn sprint_requires_forward_and_not_sneak() {
        let mut s = InputState::default();
        s.set(Action::Sprint, true);
        assert!(
            !movement_intent(&s).sprint,
            "no sprint while standing still"
        );
        s.set(Action::Forward, true);
        assert!(movement_intent(&s).sprint);
        s.set(Action::Sneak, true);
        assert!(!movement_intent(&s).sprint, "no sprint while sneaking");
    }

    #[test]
    fn look_right_increases_yaw_and_wraps() {
        // With the default 0.5 slider (~0.15°/px), 400 px turns ~60°; from 170
        // that wraps past 180 into the negative range.
        let (yaw, _) = apply_look(170.0, 0.0, 400.0, 0.0, 0.5);
        assert!(yaw < 0.0, "yaw wrapped past 180 to {yaw}");
    }

    #[test]
    fn sensitivity_curve_matches_vanilla_default() {
        // Vanilla's 0.5 slider is exactly 0.15 degrees per pixel.
        assert!((sensitivity_factor(0.5) - 0.15).abs() < 1e-6);
        // The curve is monotonic and cubic: higher slider ⇒ strictly faster.
        assert!(sensitivity_factor(1.0) > sensitivity_factor(0.5));
        assert!(sensitivity_factor(0.0) < sensitivity_factor(0.5));
    }

    #[test]
    fn pitch_is_clamped() {
        let (_, pitch) = apply_look(0.0, 80.0, 0.0, 100000.0, 0.12);
        assert!((pitch - PITCH_LIMIT).abs() < 1e-3);
        let (_, pitch) = apply_look(0.0, -80.0, 0.0, -100000.0, 0.12);
        assert!((pitch + PITCH_LIMIT).abs() < 1e-3);
    }

    #[test]
    fn take_mouse_clears() {
        let mut s = InputState::default();
        s.add_mouse(3.0, -2.0);
        assert_eq!(s.take_mouse(), (3.0, -2.0));
        assert_eq!(s.take_mouse(), (0.0, 0.0));
    }

    #[test]
    fn release_all_stops_walking_keeps_mouse() {
        let mut s = InputState::default();
        s.set(Action::Forward, true);
        s.add_mouse(1.0, 1.0);
        s.release_all();
        assert_eq!(movement_intent(&s).forward, 0.0);
        assert_eq!(s.mouse_dx, 1.0);
    }
}
