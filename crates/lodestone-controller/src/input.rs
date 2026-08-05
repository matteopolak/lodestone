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

/// Vanilla's double-tap-forward-to-sprint window, in 20 Hz ticks.
///
/// Read from `.cache/mc/26.2/client-src/net/minecraft/client/Options.java:631-640`:
/// the `options.sprintWindow` slider is an `OptionInstance.IntRange(0, 10)` with
/// default `7` (`0` disables double-tap sprint; `LocalPlayer.java:807` arms the
/// timer with this value). Since issue #444 the value is exposed as a settings
/// slider and pushed down each step by the shell; this constant is the **default**
/// [`InputState::sprint_window_ticks`] boots with, so a caller that never calls
/// [`InputState::set_sprint_window_ticks`] keeps vanilla's shipped behaviour.
pub const SPRINT_TRIGGER_WINDOW_TICKS: u8 = 7;

/// Vanilla's minimum food level to *start* a sprint (issue #200):
/// `FoodData.hasEnoughFood()` is `foodLevel > 6.0` (`FoodData.java:92-94`), so
/// exactly `6` does not qualify — the cutoff is strict, not inclusive.
pub const MIN_FOOD_LEVEL_TO_SPRINT: i32 = 6;

/// The set of currently-held actions plus accumulated, not-yet-consumed mouse
/// motion. Cheap to copy; the platform layer owns one.
///
/// `Default` is implemented manually rather than derived since issue #444:
/// the derived value would boot [`Self::sprint_window_ticks`] to `0` (double-tap
/// sprint disabled), which is not vanilla's shipped behaviour.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InputState {
    forward: bool,
    back: bool,
    left: bool,
    right: bool,
    jump: bool,
    sneak: bool,
    sprint: bool,
    /// Sprint latched on by a double-tap of forward, independent of the
    /// sprint key. Mirrors vanilla's persisted `LocalPlayer.isSprinting()`
    /// flag as set by the `sprintTriggerTime` branch of `aiStep`
    /// (`LocalPlayer.java:802-814`): a *fresh* forward press while
    /// [`sprint_trigger_ticks`](Self::sprint_trigger_ticks) is still counting
    /// down latches this on, the same way holding the sprint key drives
    /// `sprint` above. [`movement_intent`] ORs the two together and applies
    /// the existing forward/back/sneak gate to the result, so the latch
    /// supplies the flag without ever bypassing that gate — see that
    /// function's doc comment for the "flag vs. currently effective"
    /// distinction this mirrors.
    ///
    /// Deliberately *not* the same field as `sprint`: unlike the sprint key,
    /// which always reflects live physical key state, this must persist
    /// across the moment forward is briefly released between the two taps,
    /// and must be cleared independently of the key (see `set`) once the
    /// double-tap's effect has run its course. Conflating the two would let
    /// a stale latch resume sprinting on a later, unrelated single tap of
    /// forward.
    sprint_latched: bool,
    /// Ticks remaining in the double-tap window armed by the last *fresh*
    /// forward press (vanilla's `sprintTriggerTime`, `LocalPlayer.java:137`).
    /// Counts down once per [`InputState::tick`] call; a second fresh
    /// forward press while this is still nonzero triggers
    /// [`sprint_latched`](Self::sprint_latched). Sneaking or holding back
    /// cancels a pending window each tick, mirroring
    /// `LocalPlayer.java:798-800` (vanilla also cancels while slowed by item
    /// use — this crate has no such signal to check).
    sprint_trigger_ticks: u8,
    /// Raw physical hold state of the sneak key, tracked **separately** from
    /// [`Self::sneak`] so a toggle-mode press can be told apart from a repeat
    /// (issue #202). Unused while [`Self::toggle_sneak`] is off, where
    /// `sneak` already *is* the physical state and this would be redundant.
    sneak_key_down: bool,
    /// As [`Self::sneak_key_down`], for sprint.
    sprint_key_down: bool,
    /// Vanilla's `key.sneak` toggle option (`Options::toggleCrouch`, issue
    /// #202). A **config** flag, not per-key transient state — see
    /// [`Self::set_toggle_modes`] and [`Self::release_all`], which preserves
    /// it exactly like [`Self::mouse_dx`]/[`Self::mouse_dy`].
    toggle_sneak: bool,
    /// As [`Self::toggle_sneak`], for `key.sprint`/`Options::toggleSprint`.
    toggle_sprint: bool,
    /// Vanilla's `key.attack` toggle option (`Options.java:619-626`, issue
    /// #444), carried through [`Self::set_toggle_modes`] and preserved across
    /// [`Self::release_all`] exactly like [`Self::toggle_sneak`].
    ///
    /// **Not yet consumed by a press edge.** Attack/use flow through
    /// `lodestone-shell`'s `interact.rs`, not through this crate's `Action`
    /// set, so there is no `Action::Attack`/`Action::Use` branch in [`Self::set`]
    /// to hang the toggle on yet. The flag is wired end to end so a consumer
    /// can read it without touching the plumbing again — the same shape
    /// [`Self::sprint_window_ticks`] rides, where the *value* reaches the model
    /// even before every consumer exists.
    toggle_attack: bool,
    /// As [`Self::toggle_attack`], for `key.use`/`Options::toggleUse`
    /// (`Options.java:627-629`).
    toggle_use: bool,
    /// The shell's **one-tick** auto-jump request (issue #444): ORed into the
    /// jump intent by [`movement_intent`], then cleared by [`Self::tick`] at
    /// the end of the same physics tick it was consumed in.
    ///
    /// Deliberately separate from `jump`: the real key's flag is latched until
    /// the platform reports a release, so an auto-jump that wrote `jump` would
    /// leave the player bunny-hopping forever. This is a transient, cleared
    /// every tick, and never preserved across [`Self::release_all`] — unlike
    /// the config flags above, it is per-tick state, not an option.
    auto_jump_requested: bool,
    /// Vanilla's `options.sprintWindow` (`Options.java:631-640`, issue #444) —
    /// how many 20 Hz ticks the double-tap-forward window stays armed. `0`
    /// disables double-tap sprint (`LocalPlayer.java:807` arms
    /// `sprintTriggerTime` with this value). Pushed down once per `step` by
    /// the shell via [`Self::set_sprint_window_ticks`]; boots at
    /// [`SPRINT_TRIGGER_WINDOW_TICKS`].
    sprint_window_ticks: u8,
    /// Accumulated horizontal mouse delta in pixels since last consume.
    pub mouse_dx: f32,
    /// Accumulated vertical mouse delta in pixels since last consume.
    pub mouse_dy: f32,
}

impl Default for InputState {
    fn default() -> Self {
        Self {
            forward: false,
            back: false,
            left: false,
            right: false,
            jump: false,
            sneak: false,
            sprint: false,
            sprint_latched: false,
            sprint_trigger_ticks: 0,
            sneak_key_down: false,
            sprint_key_down: false,
            toggle_sneak: false,
            toggle_sprint: false,
            toggle_attack: false,
            toggle_use: false,
            auto_jump_requested: false,
            // Vanilla's shipped default — see [`Self::sprint_window_ticks`].
            sprint_window_ticks: SPRINT_TRIGGER_WINDOW_TICKS,
            mouse_dx: 0.0,
            mouse_dy: 0.0,
        }
    }
}

impl InputState {
    /// Sets vanilla's `key.sneak`/`key.sprint`/`key.attack`/`key.use` toggle
    /// options (issues #202/#444 — `Options::toggleCrouch`/`toggleSprint`/
    /// `toggleAttack`/`toggleUse`).
    ///
    /// A config setter rather than a constructor argument: the platform layer
    /// owns one long-lived `InputState` and the option can change mid-session
    /// from the settings screen, so this has to be callable at any time. Safe
    /// to call every tick with the same values — it only ever writes the four
    /// flags, never touches the effective state, so calling it redundantly is
    /// a no-op in every way that matters.
    pub fn set_toggle_modes(
        &mut self,
        toggle_sneak: bool,
        toggle_sprint: bool,
        toggle_attack: bool,
        toggle_use: bool,
    ) {
        self.toggle_sneak = toggle_sneak;
        self.toggle_sprint = toggle_sprint;
        self.toggle_attack = toggle_attack;
        self.toggle_use = toggle_use;
    }

    /// Set this physics tick's auto-jump request (issue #444).
    ///
    /// The shell calls this on a tick its obstacle check fires (auto-jump is
    /// on, the player is on the ground, and collision clipped this tick's
    /// horizontal motion). [`Self::tick`] clears it at the end of the same
    /// schedule, so a stale request can never re-jump a later tick — the
    /// transient half of the pair whose config half is pushed by the shell,
    /// never stored here.
    pub fn set_auto_jump_request(&mut self, request: bool) {
        self.auto_jump_requested = request;
    }

    /// Set vanilla's `options.sprintWindow` (issue #444) — the
    /// double-tap-forward window in 20 Hz ticks. `0` disables double-tap
    /// sprint. Pushed once per `step` by the shell, so a mid-session change
    /// from the settings screen applies on the very next tick.
    pub fn set_sprint_window_ticks(&mut self, ticks: u8) {
        self.sprint_window_ticks = ticks;
    }

    /// Set or clear a held action.
    ///
    /// # Toggle mode (issue #202)
    ///
    /// Sneak and sprint each have a vanilla option that turns the key from
    /// hold-to-activate into press-to-toggle — `ToggleKeyMapping::setDown`
    /// (`ToggleKeyMapping.java:33-40`): a toggle-mode key's *effective* state
    /// (`isDown()`) only changes on a physical **press** edge, where it flips;
    /// a physical **release** does nothing. [`Self::sneak`]/[`Self::sprint`]
    /// are that effective state — the same field every other reader in this
    /// crate (`movement_intent`, the double-tap window) already consumes — so
    /// nothing downstream has to know or care whether toggle mode is on.
    /// [`Self::sneak_key_down`]/[`Self::sprint_key_down`] exist only to
    /// detect the press edge; they track the raw physical key independently
    /// of whatever the toggle has done to the effective flag.
    pub fn set(&mut self, action: Action, held: bool) {
        match action {
            Action::Forward => {
                let fresh_press = held && !self.forward;
                self.forward = held;
                // Arm/trigger the double-tap window, but only on a genuine
                // fresh press with real forward impulse — mirrors vanilla
                // gating this on `canStartSprinting()`'s
                // `input.hasForwardImpulse()` (false if back is also held,
                // since the two cancel) and `!isMovingSlowly()` (sneaking).
                // See `LocalPlayer.java:802-809`.
                if fresh_press && !self.back && !self.sneak {
                    if self.sprint_trigger_ticks > 0 {
                        self.sprint_latched = true;
                    } else {
                        // Armed for the *configured* window (issue #444): 0 is
                        // `LocalPlayer.java:807`'s "double-tap sprint disabled"
                        // value, so a fresh press arms with 0 and the next one
                        // never sees a live window.
                        self.sprint_trigger_ticks = self.sprint_window_ticks;
                    }
                }
                // Releasing forward always ends an active double-tap sprint
                // (vanilla: `shouldStopRunSprinting`'s `!hasForwardImpulse`
                // branch, `LocalPlayer.java:919`) — clear the latch so a
                // later, unrelated forward press doesn't resume sprinting
                // without a fresh trigger.
                if !held {
                    self.sprint_latched = false;
                }
            }
            Action::Back => self.back = held,
            Action::Left => self.left = held,
            Action::Right => self.right = held,
            Action::Jump => self.jump = held,
            Action::Sneak => {
                let fresh_press = held && !self.sneak_key_down;
                self.sneak_key_down = held;
                if self.toggle_sneak {
                    if fresh_press {
                        self.sneak = !self.sneak;
                    }
                } else {
                    self.sneak = held;
                }
            }
            Action::Sprint => {
                let fresh_press = held && !self.sprint_key_down;
                self.sprint_key_down = held;
                if self.toggle_sprint {
                    if fresh_press {
                        self.sprint = !self.sprint;
                    }
                } else {
                    self.sprint = held;
                }
            }
        }
    }

    /// Advance the double-tap-sprint timer by one 20 Hz physics tick.
    ///
    /// Mirrors `LocalPlayer.aiStep`'s per-tick handling of `sprintTriggerTime`
    /// (`LocalPlayer.java:764-766` for the countdown, `:798-800` for the
    /// sneak/back cancel). This crate touches no clock (see the
    /// `no_wasm_trap_symbols_are_confined` guard below), so the platform
    /// layer must call this once per fixed tick rather than this type ever
    /// reaching for `Instant::now()`.
    pub fn tick(&mut self) {
        if self.sprint_trigger_ticks > 0 {
            self.sprint_trigger_ticks -= 1;
        }
        if self.sneak || self.back {
            self.sprint_trigger_ticks = 0;
            self.sprint_latched = false;
        }
        // Auto-jump requests are one-shot: the intent system consumed it
        // this tick (it runs before this), so clear it for the next one. A
        // request that survived here would re-jump on an unrelated later tick.
        self.auto_jump_requested = false;
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
    ///
    /// Clearing `sneak`/`sprint` to `false` here is correct in toggle mode too
    /// — it mirrors `KeyMapping.releaseAll()` calling `release()` on every
    /// mapping, and `ToggleKeyMapping.release()`'s `reset()` sets `isDown`
    /// `false` unconditionally regardless of toggle mode
    /// (`ToggleKeyMapping.java:42-49`). [`Self::toggle_sneak`]/
    /// [`Self::toggle_sprint`]/[`Self::toggle_attack`]/[`Self::toggle_use`]
    /// and [`Self::sprint_window_ticks`] are preserved across the reset like
    /// [`Self::mouse_dx`]/[`Self::mouse_dy`] are: they are the *options*, not
    /// per-key transient state, and losing them here would silently revert a
    /// player's toggle-mode choice or sprint-window setting the next time the
    /// cursor is released. [`Self::auto_jump_requested`] is deliberately *not*
    /// preserved — it is one-tick state, reset by [`Self::default`] like every
    /// held key.
    pub fn release_all(&mut self) {
        let mouse = (self.mouse_dx, self.mouse_dy);
        let options = (
            self.toggle_sneak,
            self.toggle_sprint,
            self.toggle_attack,
            self.toggle_use,
            self.sprint_window_ticks,
        );
        *self = InputState::default();
        self.mouse_dx = mouse.0;
        self.mouse_dy = mouse.1;
        self.toggle_sneak = options.0;
        self.toggle_sprint = options.1;
        self.toggle_attack = options.2;
        self.toggle_use = options.3;
        self.sprint_window_ticks = options.4;
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
/// while sneaking) — this must not flip to an unconditional check here, since
/// the sprint *flag* (raw key **or** [`InputState::tick`]'s double-tap latch)
/// and sprint being *currently effective* are deliberately separate: the flag
/// says sprint was requested, this gate says whether it's allowed to apply
/// right now, exactly like vanilla's `isSprinting()` vs `canStartSprinting()`.
///
/// Auto-jump surfaces through this same function: the shell decides *when* to
/// ask (grounded, blocked ahead, moving) and sets
/// [`InputState::auto_jump_requested`]; this crate only forwards it, and
/// [`InputState::tick`] clears it so a stale request can never re-fire on an
/// unrelated later tick.
#[must_use]
pub fn movement_intent(state: &InputState) -> MovementInput {
    let forward = f32::from(state.forward) - f32::from(state.back);
    let strafe = f32::from(state.left) - f32::from(state.right);
    let sprint_requested = state.sprint || state.sprint_latched;
    let sprint = sprint_requested && state.forward && !state.back && !state.sneak;
    MovementInput {
        forward,
        strafe,
        jump: state.jump || state.auto_jump_requested,
        sneak: state.sneak,
        sprint,
    }
}

/// [`movement_intent`], plus vanilla's food-level sprint gate (issue #200).
///
/// `LocalPlayer.canStartSprinting` requires `isSprintingPossible`, whose
/// non-passenger branch is `hasEnoughFoodToDoExhaustiveManoeuvres()` —
/// `foodData.hasEnoughFood() || abilities.mayfly`
/// (`Player.java:1592-1594`), and `hasEnoughFood()` is `foodLevel > 6.0`
/// (`FoodData.java:92-94`). So: sprint is allowed on empty/low food only while
/// flight is permitted (creative/spectator), and otherwise cuts out at food
/// level 6 and below, not just at 0.
///
/// A second function rather than a parameter on [`movement_intent`] itself:
/// this crate holds no food or ability state (that is server-reported session
/// data a layer up, in `lodestone-ecs`), so the gate has to be a `bool` the
/// caller computes — and `movement_intent`'s existing signature is called
/// directly by `lodestone-shell`'s `sim.rs`, which this crate does not own and
/// must not silently break. The real production path
/// (`ecs::compute_movement_intent` → `ecs::swim_adjusted_intent`) is what
/// needs to move onto this one; see that module.
#[must_use]
pub fn movement_intent_with_food(state: &InputState, sprint_allowed_by_food: bool) -> MovementInput {
    let mut intent = movement_intent(state);
    if !sprint_allowed_by_food {
        intent.sprint = false;
    }
    intent
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
///
/// **No invert option** — see [`apply_look_inverted`] (issue #203), added
/// alongside rather than as a parameter here because this exact signature is
/// called directly by `lodestone-shell`'s `sim.rs`, which this crate does not
/// own and must not silently break.
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

/// [`apply_look`], plus vanilla's `invertXMouse`/`invertYMouse` options
/// (issue #203).
///
/// `MouseHandler.turnPlayer` computes `xo`/`yo` (the raw delta already scaled
/// by sensitivity) and negates *those* right before `Entity.turn`:
/// `player.turn(invertMouseX ? -xo : xo, invertMouseY ? -yo : yo)`
/// (`MouseHandler.java:372-374`) — negation is the last step, after the
/// sensitivity curve, not before it. This negates `dx`/`dy` first and lets
/// [`apply_look`] apply the (unsigned) [`sensitivity_factor`] afterwards, but
/// the two orders agree numerically: the curve multiplies by a positive
/// scalar derived from the sensitivity slider alone, with no dependence on
/// `dx`/`dy`'s sign, so negating before or after that multiplication is the
/// same real number either way.
#[must_use]
pub fn apply_look_inverted(
    yaw: f32,
    pitch: f32,
    dx: f32,
    dy: f32,
    sensitivity: f32,
    invert_x: bool,
    invert_y: bool,
) -> (f32, f32) {
    let dx = if invert_x { -dx } else { dx };
    let dy = if invert_y { -dy } else { dy };
    apply_look(yaw, pitch, dx, dy, sensitivity)
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

    /// #200: vanilla's cutoff is `foodLevel > 6` (`FoodData.java:93`), not `> 0`
    /// — a player at exactly 6 must not be able to start a sprint.
    #[test]
    fn sprint_is_gated_on_food_above_six() {
        let mut s = InputState::default();
        s.set(Action::Sprint, true);
        s.set(Action::Forward, true);

        // Predict both hypotheses at the boundary rather than merely asserting
        // "sprint stops somewhere": vanilla's own cutoff is 6, so 7 must still
        // sprint and 6 must not, not just "some value near there".
        assert!(
            movement_intent_with_food(&s, true).sprint,
            "sprint is otherwise requested and effective; food must not veto it when allowed"
        );
        assert!(
            !movement_intent_with_food(&s, false).sprint,
            "the caller has already resolved food <= 6 to `false`; the gate must take it"
        );
    }

    /// The gate must only ever *remove* sprint, never grant it back — it is a
    /// further AND, not an override of the existing forward/sneak gate.
    #[test]
    fn the_food_gate_cannot_grant_sprint_the_other_gates_refused() {
        let mut s = InputState::default();
        s.set(Action::Sprint, true);
        s.set(Action::Sneak, true); // already vetoes sprint on its own
        assert!(
            !movement_intent_with_food(&s, true).sprint,
            "food alone must not overrule the sneak veto"
        );
    }

    #[test]
    fn double_tap_forward_starts_sprint_without_sprint_key() {
        let mut s = InputState::default();
        s.set(Action::Forward, true);
        assert!(!movement_intent(&s).sprint, "first tap alone doesn't sprint");
        s.set(Action::Forward, false);
        s.tick();
        s.set(Action::Forward, true);
        assert!(
            movement_intent(&s).sprint,
            "second fresh press within the window should latch sprint on"
        );
    }

    #[test]
    fn holding_forward_without_a_second_tap_never_sprints() {
        let mut s = InputState::default();
        s.set(Action::Forward, true);
        for _ in 0..(SPRINT_TRIGGER_WINDOW_TICKS as u32 + 5) {
            s.tick();
            assert!(
                !movement_intent(&s).sprint,
                "a single held press must not auto-sprint"
            );
        }
    }

    #[test]
    fn second_tap_after_window_expires_does_not_sprint() {
        let mut s = InputState::default();
        s.set(Action::Forward, true);
        s.set(Action::Forward, false);
        // Let the window fully expire before the second tap.
        for _ in 0..=(SPRINT_TRIGGER_WINDOW_TICKS as u32) {
            s.tick();
        }
        s.set(Action::Forward, true);
        assert!(
            !movement_intent(&s).sprint,
            "a stale double-tap window must not trigger sprint"
        );
    }

    #[test]
    fn sneaking_cancels_a_pending_double_tap_window() {
        let mut s = InputState::default();
        s.set(Action::Forward, true);
        s.set(Action::Forward, false);
        s.set(Action::Sneak, true);
        s.tick(); // sneak held during this tick cancels the pending window
        s.set(Action::Sneak, false);
        s.set(Action::Forward, true);
        assert!(
            !movement_intent(&s).sprint,
            "sneaking between taps should cancel the pending window"
        );
    }

    #[test]
    fn releasing_forward_clears_the_latch_for_later_unrelated_taps() {
        let mut s = InputState::default();
        // A genuine double tap latches sprint on.
        s.set(Action::Forward, true);
        s.set(Action::Forward, false);
        s.tick();
        s.set(Action::Forward, true);
        assert!(movement_intent(&s).sprint);
        // Release forward (stops the effective sprint) and let plenty of
        // ticks pass, then a single unrelated later tap must not inherit the
        // old latch.
        s.set(Action::Forward, false);
        for _ in 0..20 {
            s.tick();
        }
        s.set(Action::Forward, true);
        assert!(
            !movement_intent(&s).sprint,
            "a stale latch must not resume sprint on an unrelated later tap"
        );
    }

    #[test]
    fn sprint_key_and_double_tap_do_not_fight() {
        let mut s = InputState::default();
        // Holding the sprint key already sprints while moving forward...
        s.set(Action::Sprint, true);
        s.set(Action::Forward, true);
        assert!(movement_intent(&s).sprint);
        // ...and a double-tap on top of that is a harmless no-op, not a
        // conflict: both paths just set the same effective flag.
        s.set(Action::Forward, false);
        s.tick();
        s.set(Action::Forward, true);
        assert!(movement_intent(&s).sprint);
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

    /// #203: `apply_look_inverted(.., false, false)` must be numerically
    /// identical to plain [`apply_look`] — the option's *absence* changing
    /// nothing is the control every "inversion" test below leans on.
    #[test]
    fn uninverted_matches_apply_look_exactly() {
        let plain = apply_look(12.0, -3.0, 40.0, -15.0, 0.7);
        let inverted_off = apply_look_inverted(12.0, -3.0, 40.0, -15.0, 0.7, false, false);
        assert_eq!(plain, inverted_off);
    }

    /// Predicts the exact resulting yaw/pitch from negating the deltas by
    /// hand, rather than merely asserting the direction flipped — a test that
    /// only checked the sign could pass for a wrong magnitude too.
    #[test]
    fn inverting_x_negates_the_yaw_delta_exactly() {
        let (yaw, pitch) = apply_look(0.0, 0.0, 40.0, 0.0, 0.5);
        let (yaw_inv, pitch_inv) = apply_look_inverted(0.0, 0.0, 40.0, 0.0, 0.5, true, false);
        assert_eq!(pitch, pitch_inv, "x-invert must not touch pitch");
        assert!((yaw_inv - (-yaw)).abs() < 1e-6, "expected {}, got {yaw_inv}", -yaw);
    }

    #[test]
    fn inverting_y_negates_the_pitch_delta_exactly() {
        let (yaw, pitch) = apply_look(0.0, 0.0, 0.0, 20.0, 0.5);
        let (yaw_inv, pitch_inv) = apply_look_inverted(0.0, 0.0, 0.0, 20.0, 0.5, false, true);
        assert_eq!(yaw, yaw_inv, "y-invert must not touch yaw");
        assert!((pitch_inv - (-pitch)).abs() < 1e-6, "expected {}, got {pitch_inv}", -pitch);
    }

    #[test]
    fn both_axes_invert_independently_and_simultaneously() {
        let (yaw, pitch) = apply_look(0.0, 0.0, 40.0, 20.0, 0.5);
        let (yaw_inv, pitch_inv) = apply_look_inverted(0.0, 0.0, 40.0, 20.0, 0.5, true, true);
        assert!((yaw_inv - (-yaw)).abs() < 1e-6);
        assert!((pitch_inv - (-pitch)).abs() < 1e-6);
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

    // -- toggle sneak/sprint (issue #202) ------------------------------------

    #[test]
    fn hold_mode_is_the_default_and_matches_the_old_behaviour() {
        let mut s = InputState::default();
        s.set(Action::Sneak, true);
        assert!(movement_intent(&s).sneak, "held while the key is down");
        s.set(Action::Sneak, false);
        assert!(!movement_intent(&s).sneak, "released the moment the key is up");
    }

    #[test]
    fn toggle_mode_flips_on_press_and_ignores_release() {
        let mut s = InputState::default();
        s.set_toggle_modes(true, false, false, false);

        s.set(Action::Sneak, true); // press: flips on
        assert!(movement_intent(&s).sneak, "a press must toggle sneak on");
        s.set(Action::Sneak, false); // release: must NOT clear it
        assert!(
            movement_intent(&s).sneak,
            "releasing a toggled key must not un-sneak — that is hold-mode behaviour"
        );
        s.set(Action::Sneak, true); // a second press flips it back off
        assert!(!movement_intent(&s).sneak, "a second press must toggle it back off");
    }

    #[test]
    fn toggle_sneak_and_toggle_sprint_are_independent() {
        let mut s = InputState::default();
        s.set_toggle_modes(true, false, false, false);
        s.set(Action::Sprint, true);
        assert!(
            !movement_intent(&s).sneak,
            "toggling sneak's mode must not affect the sprint key's own mode"
        );
        // Sprint (still hold mode) requires forward too, per the existing gate.
        s.set(Action::Forward, true);
        assert!(movement_intent(&s).sprint, "sprint held normally while its own toggle is off");
        s.set(Action::Sprint, false);
        assert!(!movement_intent(&s).sprint, "…and releases normally too");
    }

    #[test]
    fn a_key_repeat_does_not_toggle_again() {
        // The platform may report `set(action, true)` more than once for one
        // physical press (key-repeat events); only a genuine press *edge*
        // (transition from up to down) may flip the toggle, mirroring
        // `ToggleKeyMapping.setDown`'s guard on `down` alone would be wrong —
        // vanilla relies on the OS not re-delivering `GLFW_PRESS`, but this
        // layer is not allowed to assume that of every platform.
        let mut s = InputState::default();
        s.set_toggle_modes(false, true, false, false);
        s.set(Action::Sprint, true);
        s.set(Action::Forward, true);
        assert!(movement_intent(&s).sprint, "first press toggles sprint on");
        s.set(Action::Sprint, true); // repeat, not a fresh press
        assert!(
            movement_intent(&s).sprint,
            "a repeated `true` with no release between must not toggle it back off"
        );
    }

    #[test]
    fn release_all_clears_toggled_sneak_but_keeps_the_toggle_option() {
        let mut s = InputState::default();
        s.set_toggle_modes(true, true, false, false);
        s.set(Action::Sneak, true);
        assert!(movement_intent(&s).sneak, "precondition: toggled on");

        s.release_all();
        assert!(
            !movement_intent(&s).sneak,
            "release_all must clear the toggled state, like vanilla's releaseAll"
        );

        // But a later press must still toggle, not hold — the *option* must
        // have survived the reset.
        s.set(Action::Sneak, true);
        assert!(movement_intent(&s).sneak);
        s.set(Action::Sneak, false);
        assert!(
            movement_intent(&s).sneak,
            "toggle mode must still be in effect after release_all — the option was not lost"
        );
    }
}
